//! 支付宝「电脑网站支付」（alipay.trade.page.pay）集成。
//!
//! 未配置 ALIPAY_* 环境变量时整个在线支付优雅降级为现有的线下转账流程：
//! 下单不再返回 pay_url，/pay/:id 页面提示走线下转账。

use anyhow::Context;
use base64::Engine as _;
use rsa::pkcs1v15::{Signature, SigningKey, VerifyingKey};
use rsa::sha2::Sha256;
use rsa::signature::{SignatureEncoding, Signer, Verifier};
use rsa::{RsaPrivateKey, RsaPublicKey};
use std::collections::BTreeMap;
use tracing::warn;

/// 支付宝接口配置。三个密钥变量缺一即视为未配置（返回 None）。
pub struct AlipayConfig {
    pub app_id: String,
    /// 应用私钥（下单签名用）。
    pub private_key: RsaPrivateKey,
    /// 支付宝公钥（异步通知验签用）。
    pub alipay_public_key: RsaPublicKey,
    /// 网关地址；默认正式环境，沙箱用 ALIPAY_GATEWAY 覆盖。
    pub gateway: String,
}

impl AlipayConfig {
    /// 从环境变量加载：ALIPAY_APP_ID / ALIPAY_PRIVATE_KEY / ALIPAY_PUBLIC_KEY
    /// 缺一即 None；ALIPAY_GATEWAY 缺省为正式网关。密钥解析失败同样降级为
    /// None（记 warn），不让一个配置错误拖垮整个 relay。
    pub fn from_env() -> Option<Self> {
        let app_id = std::env::var("ALIPAY_APP_ID").ok().filter(|v| !v.is_empty());
        let private_key = std::env::var("ALIPAY_PRIVATE_KEY").ok().filter(|v| !v.is_empty());
        let public_key = std::env::var("ALIPAY_PUBLIC_KEY").ok().filter(|v| !v.is_empty());
        let (Some(app_id), Some(private_key), Some(public_key)) = (app_id, private_key, public_key)
        else {
            return None;
        };
        let gateway = std::env::var("ALIPAY_GATEWAY")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| "https://openapi.alipay.com/gateway.do".to_string());
        match (parse_private_key(&private_key), parse_public_key(&public_key)) {
            (Ok(private_key), Ok(alipay_public_key)) => Some(Self {
                app_id,
                private_key,
                alipay_public_key,
                gateway,
            }),
            (priv_res, pub_res) => {
                if let Err(e) = priv_res {
                    warn!(error = %e, "ALIPAY_PRIVATE_KEY 解析失败，在线支付已禁用");
                }
                if let Err(e) = pub_res {
                    warn!(error = %e, "ALIPAY_PUBLIC_KEY 解析失败，在线支付已禁用");
                }
                None
            }
        }
    }
}

/// 支付宝密钥工具产出的私钥可能是 PEM 全文或去头尾的 base64，格式可能是
/// PKCS8 或 PKCS1，四种组合都接受。
pub fn parse_private_key(raw: &str) -> anyhow::Result<RsaPrivateKey> {
    use rsa::pkcs1::DecodeRsaPrivateKey;
    use rsa::pkcs8::DecodePrivateKey;
    let trimmed = raw.trim();
    if trimmed.contains("BEGIN") {
        return RsaPrivateKey::from_pkcs8_pem(trimmed)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(trimmed))
            .context("私钥不是有效的 PKCS8/PKCS1 PEM");
    }
    let der = base64::engine::general_purpose::STANDARD
        .decode(trimmed.split_whitespace().collect::<String>())
        .context("私钥 base64 解码失败")?;
    RsaPrivateKey::from_pkcs8_der(&der)
        .or_else(|_| RsaPrivateKey::from_pkcs1_der(&der))
        .context("私钥不是有效的 PKCS8/PKCS1 DER")
}

/// 支付宝公钥同理：PEM 或裸 base64，X.509（PUBLIC KEY）或 PKCS1
///（RSA PUBLIC KEY）都接受。
pub fn parse_public_key(raw: &str) -> anyhow::Result<RsaPublicKey> {
    use rsa::pkcs1::DecodeRsaPublicKey;
    use rsa::pkcs8::DecodePublicKey;
    let trimmed = raw.trim();
    if trimmed.contains("BEGIN") {
        return RsaPublicKey::from_public_key_pem(trimmed)
            .or_else(|_| RsaPublicKey::from_pkcs1_pem(trimmed))
            .context("公钥不是有效的 X.509/PKCS1 PEM");
    }
    let der = base64::engine::general_purpose::STANDARD
        .decode(trimmed.split_whitespace().collect::<String>())
        .context("公钥 base64 解码失败")?;
    RsaPublicKey::from_public_key_der(&der)
        .or_else(|_| RsaPublicKey::from_pkcs1_der(&der))
        .context("公钥不是有效的 X.509/PKCS1 DER")
}

/// 待签名串：参数按 key 字典序拼 `k=v&...`，排除 sign / sign_type，跳过空值
///（支付宝 API 调用的标准拼法；异步通知验签同样排除 sign_type）。
/// BTreeMap 保证字典序。
fn canonical(params: &BTreeMap<String, String>) -> String {
    params
        .iter()
        .filter(|(k, v)| k.as_str() != "sign" && k.as_str() != "sign_type" && !v.is_empty())
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// 页面跳转类请求（alipay.trade.page.pay）的待签名串：与 API 调用不同，
/// **sign_type 参与签名**，仅排除 sign 本身（网关报错时回显的验签字符串
/// 即按此规则生成）。
fn canonical_page_redirect(params: &BTreeMap<String, String>) -> String {
    params
        .iter()
        .filter(|(k, v)| k.as_str() != "sign" && !v.is_empty())
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// RSA2（SHA256withRSA）签名，返回 base64。
pub fn rsa2_sign(params: &BTreeMap<String, String>, key: &RsaPrivateKey) -> String {
    let signing_key = SigningKey::<Sha256>::new(key.clone());
    let sig = signing_key.sign(canonical(params).as_bytes());
    base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
}

/// 页面跳转类请求的 RSA2 签名（sign_type 参与签名）。
fn rsa2_sign_page_redirect(params: &BTreeMap<String, String>, key: &RsaPrivateKey) -> String {
    let signing_key = SigningKey::<Sha256>::new(key.clone());
    let sig = signing_key.sign(canonical_page_redirect(params).as_bytes());
    base64::engine::general_purpose::STANDARD.encode(sig.to_bytes())
}

/// RSA2 验签（异步通知用支付宝公钥）。
pub fn rsa2_verify(params: &BTreeMap<String, String>, sign_b64: &str, key: &RsaPublicKey) -> bool {
    let Ok(sig_bytes) = base64::engine::general_purpose::STANDARD.decode(sign_b64) else {
        return false;
    };
    let Ok(sig) = Signature::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    VerifyingKey::<Sha256>::new(key.clone())
        .verify(canonical(params).as_bytes(), &sig)
        .is_ok()
}

/// 页面跳转类请求的 RSA2 验签（sign_type 参与）。仅测试与调试用。
pub fn rsa2_verify_page_redirect(params: &BTreeMap<String, String>, sign_b64: &str, key: &RsaPublicKey) -> bool {
    let Ok(sig_bytes) = base64::engine::general_purpose::STANDARD.decode(sign_b64) else {
        return false;
    };
    let Ok(sig) = Signature::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    VerifyingKey::<Sha256>::new(key.clone())
        .verify(canonical_page_redirect(params).as_bytes(), &sig)
        .is_ok()
}

/// 北京时间 yyyy-MM-dd HH:mm:ss（支付宝要求东八区）。
fn beijing_now() -> String {
    let tz = chrono::FixedOffset::east_opt(8 * 3600).unwrap();
    chrono::Utc::now()
        .with_timezone(&tz)
        .format("%Y-%m-%d %H:%M:%S")
        .to_string()
}

/// 构造支付宝收银台参数（含 sign）。notify_url / return_url 由 public_base
///（RELAY_PUBLIC_BASE）拼出，必须公网可达。
pub fn page_pay_params(
    order_id: &str,
    plan_id: &str,
    amount_cny: f64,
    cfg: &AlipayConfig,
    public_base: &str,
) -> BTreeMap<String, String> {
    let base = public_base.trim_end_matches('/');
    let biz_content = serde_json::json!({
        "out_trade_no": order_id,
        "total_amount": format!("{amount_cny:.2}"),
        "subject": format!("思库云端存储扩容-{plan_id}"),
        "product_code": "FAST_INSTANT_TRADE_PAY",
        "timeout_express": "30m",
    })
    .to_string();
    let mut params = BTreeMap::new();
    params.insert("app_id".to_string(), cfg.app_id.clone());
    params.insert("method".to_string(), "alipay.trade.page.pay".to_string());
    params.insert("charset".to_string(), "utf-8".to_string());
    params.insert("sign_type".to_string(), "RSA2".to_string());
    params.insert("version".to_string(), "1.0".to_string());
    params.insert("timestamp".to_string(), beijing_now());
    params.insert("notify_url".to_string(), format!("{base}/api/pay/alipay/notify"));
    params.insert("return_url".to_string(), format!("{base}/pay/{order_id}"));
    params.insert("biz_content".to_string(), biz_content);
    let sign = rsa2_sign_page_redirect(&params, &cfg.private_key);
    params.insert("sign".to_string(), sign);
    params
}

/// 支付宝收银台跳转 URL（GET 形式）。
pub fn page_pay_url(
    order_id: &str,
    plan_id: &str,
    amount_cny: f64,
    cfg: &AlipayConfig,
    public_base: &str,
) -> String {
    let params = page_pay_params(order_id, plan_id, amount_cny, cfg, public_base);
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params.iter())
        .finish();
    format!("{}?{}", cfg.gateway, query)
}
