//! Email verification codes for login/registration, plus the SMTP sender.
//!
//! Codes live in memory only: a relay restart invalidates outstanding codes
//! and the user simply requests a new one. Codes are the sole credential
//! (passwordless accounts), so they are hardened accordingly: 8 digits,
//! 10-minute TTL, single-use, 5 verify attempts per code, one code per email
//! per minute, and a per-email daily send cap against mail bombing.

use rand::Rng;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

const CODE_TTL_SECS: i64 = 10 * 60;
const RESEND_INTERVAL_SECS: i64 = 60;
const MAX_ATTEMPTS: u32 = 5;
/// Per-email daily send cap (UTC day). Blunting mail-bombing abuse; way above
/// any legitimate login cadence.
const DAILY_SEND_CAP: u32 = 20;

struct CodeEntry {
    code_hash: String,
    expires_at: i64,
    attempts: u32,
    last_sent_at: i64,
}

#[derive(Default)]
pub struct VerificationCodes {
    inner: Mutex<HashMap<String, CodeEntry>>,
    /// email -> (UTC day, sends so far that day). Separate from `inner` so a
    /// re-issued code never resets the daily counter.
    daily: Mutex<HashMap<String, (String, u32)>>,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

/// Normalize an email for identity purposes: trim + lowercase. Applied at
/// registration AND login so case differences never split an account.
pub fn normalize_email(email: &str) -> String {
    email.trim().to_lowercase()
}

/// Minimal structural check: exactly one `@`, non-empty local part, and a
/// dotted domain. Deliberately not RFC-complete — the code round-trip is the
/// real proof of ownership.
pub fn is_valid_email(email: &str) -> bool {
    let (local, domain) = match email.split_once('@') {
        Some((l, d)) if !l.contains('@') && !d.contains('@') => (l, d),
        _ => return false,
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.chars().any(|c| c.is_whitespace())
}

fn hash_code(code: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(code.as_bytes());
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, h.finalize())
}

impl VerificationCodes {
    pub fn new() -> Self {
        Self::default()
    }

    /// Issue a fresh code for `email`, replacing any previous one. Fails when
    /// the resend interval has not elapsed or the daily send cap is reached.
    pub fn issue(&self, email: &str) -> Result<String, String> {
        let mut map = self.inner.lock().unwrap();
        let ts = now();
        if let Some(prev) = map.get(email) {
            let wait = RESEND_INTERVAL_SECS - (ts - prev.last_sent_at);
            if wait > 0 {
                return Err(format!("发送太频繁，请 {wait} 秒后重试"));
            }
        }
        {
            let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
            let mut daily = self.daily.lock().unwrap();
            let entry = daily.entry(email.to_string()).or_insert_with(|| (day.clone(), 0));
            if entry.0 != day {
                *entry = (day, 0);
            }
            if entry.1 >= DAILY_SEND_CAP {
                return Err("今日验证码发送次数已达上限，请明天再试".to_string());
            }
            entry.1 += 1;
        }
        // 8 digits: the code is the sole credential for passwordless
        // accounts, so it gets one more order of magnitude over the 6-digit
        // convention (5 attempts / 10^8 space / 10-minute TTL).
        let code = format!("{:08}", rand::thread_rng().gen_range(0..100_000_000u64));
        map.insert(
            email.to_string(),
            CodeEntry {
                code_hash: hash_code(&code),
                expires_at: ts + CODE_TTL_SECS,
                attempts: 0,
                last_sent_at: ts,
            },
        );
        Ok(code)
    }

    /// Verify and consume a code. Every failure (expired, wrong, too many
    /// attempts) is a generic message so the endpoint does not leak which
    /// codes exist.
    pub fn verify(&self, email: &str, code: &str) -> Result<(), String> {
        let mut map = self.inner.lock().unwrap();
        let invalid = "验证码错误或已过期".to_string();
        let entry = map.get_mut(email).ok_or_else(|| invalid.clone())?;
        if entry.expires_at < now() {
            map.remove(email);
            return Err(invalid);
        }
        entry.attempts += 1;
        if entry.attempts > MAX_ATTEMPTS {
            map.remove(email);
            return Err("尝试次数过多，请重新获取验证码".to_string());
        }
        if entry.code_hash != hash_code(code) {
            return Err(invalid);
        }
        map.remove(email);
        Ok(())
    }
}

/// Mask an email for display in conflict prompts: `15***@qq.com`.
pub fn mask_email(email: &str) -> String {
    match email.split_once('@') {
        Some((local, domain)) => {
            let keep: String = local.chars().take(2).collect();
            format!("{keep}***@{domain}")
        }
        None => "***".to_string(),
    }
}

/// SMTP sender for transactional mail (verification codes). Configured via
/// env: `SMTP_HOST` / `SMTP_PORT` (465) / `SMTP_USER` / `SMTP_PASS` /
/// `SMTP_FROM` (defaults to SMTP_USER). When unconfigured the relay logs a
/// warning and registration falls back to the legacy codeless flow — a
/// self-hosted relay without mail must not lock registrations out.
pub struct Mailer {
    host: String,
    port: u16,
    user: String,
    pass: String,
    from: String,
}

impl Mailer {
    pub fn from_env() -> Option<Self> {
        let host = std::env::var("SMTP_HOST").ok().filter(|v| !v.is_empty());
        let user = std::env::var("SMTP_USER").ok().filter(|v| !v.is_empty());
        let pass = std::env::var("SMTP_PASS").ok().filter(|v| !v.is_empty());
        let (host, user, pass) = match (host, user, pass) {
            (Some(h), Some(u), Some(p)) => (h, u, p),
            (None, None, None) => return None,
            _ => {
                tracing::warn!("SMTP 配置不完整（SMTP_HOST/SMTP_USER/SMTP_PASS 需同时配置），注册验证码不可用");
                return None;
            }
        };
        let port = std::env::var("SMTP_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(465);
        let from = std::env::var("SMTP_FROM")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| user.clone());
        info!(host = %host, port, from = %from, "SMTP mailer configured");
        Some(Self { host, port, user, pass, from })
    }

    pub async fn send_code(&self, to: &str, code: &str) -> anyhow::Result<()> {
        use lettre::message::header::ContentType;
        use lettre::transport::smtp::authentication::Credentials;
        use lettre::transport::smtp::client::{Tls, TlsParameters};
        use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};

        let tls = TlsParameters::new(self.host.clone())
            .map_err(|e| anyhow::anyhow!("TLS 参数构建失败: {e}"))?;

        let message = Message::builder()
            .from(self.from.parse()?)
            .to(to.parse()?)
            .subject("思库验证码")
            .header(ContentType::TEXT_PLAIN)
            .body(format!(
                "你的思库验证码：{code}\n\n10 分钟内有效，可用于登录或注册。若非本人操作，请忽略本邮件。"
            ))?;
        let transport = AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&self.host)
            .port(self.port)
            .tls(Tls::Wrapper(tls))
            .credentials(Credentials::new(self.user.clone(), self.pass.clone()))
            .build();
        transport.send(message).await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_normalization_and_validation() {
        assert_eq!(normalize_email("  User@Example.COM "), "user@example.com");
        assert!(is_valid_email("a@b.com"));
        assert!(is_valid_email("first.last+tag@sub.domain.cn"));
        assert!(!is_valid_email("plain"));
        assert!(!is_valid_email("a@b"));
        assert!(!is_valid_email("@b.com"));
        assert!(!is_valid_email("a@.com"));
        assert!(!is_valid_email("a b@c.com"));
        assert!(!is_valid_email("a@@b.com"));
    }

    #[test]
    fn code_issue_verify_consume() {
        let codes = VerificationCodes::new();
        let code = codes.issue("a@b.com").unwrap();
        assert_eq!(code.len(), 8);
        codes.verify("a@b.com", &code).unwrap();
        // Consumed: a second verify fails.
        assert!(codes.verify("a@b.com", &code).is_err());
    }

    #[test]
    fn daily_send_cap_blocks_bombing() {
        let codes = VerificationCodes::new();
        let day = chrono::Utc::now().format("%Y-%m-%d").to_string();
        codes
            .daily
            .lock()
            .unwrap()
            .insert("a@b.com".to_string(), (day, DAILY_SEND_CAP));
        let err = codes.issue("a@b.com").unwrap_err();
        assert!(err.contains("已达上限"));
        // A different email is unaffected.
        assert!(codes.issue("c@d.com").is_ok());
    }

    #[test]
    fn email_masking() {
        assert_eq!(mask_email("1589335815@qq.com"), "15***@qq.com");
        assert_eq!(mask_email("a@b.com"), "a***@b.com");
        assert_eq!(mask_email("no-at"), "***");
    }

    #[test]
    fn code_wrong_and_attempts_cap() {
        let codes = VerificationCodes::new();
        let code = codes.issue("a@b.com").unwrap();
        let wrong = if code == "000000" { "000001" } else { "000000" };
        for _ in 0..MAX_ATTEMPTS {
            assert!(codes.verify("a@b.com", wrong).is_err());
        }
        // Attempts exhausted: even the right code is now rejected.
        assert!(codes.verify("a@b.com", &code).is_err());
    }

    #[test]
    fn resend_interval_enforced() {
        let codes = VerificationCodes::new();
        codes.issue("a@b.com").unwrap();
        let err = codes.issue("a@b.com").unwrap_err();
        assert!(err.contains("秒后重试"));
        // A different email is unaffected.
        assert!(codes.issue("c@d.com").is_ok());
    }
}
