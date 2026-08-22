use crate::db::Db;
use anyhow::Context;
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claims {
    pub sub: String,      // user_id (also the room id)
    pub device_id: String,
    pub exp: usize,
    pub jti: String,      // token id, used for revocation bookkeeping
}

#[derive(Debug, Clone)]
pub struct Auth {
    secret: Arc<String>,
}

impl Auth {
    pub fn new(secret: impl Into<String>) -> Self {
        Self {
            secret: Arc::new(secret.into()),
        }
    }

    pub fn validate(&self, token: &str) -> anyhow::Result<Claims> {
        let key = DecodingKey::from_secret(self.secret.as_bytes());
        let mut validation = Validation::new(Algorithm::HS256);
        validation.validate_aud = false;
        let token_data = decode::<Claims>(token, &key, &validation)?;
        Ok(token_data.claims)
    }

    /// Issue a device access token (7 days).
    pub fn issue_device_token(&self, user_id: &str, device_id: &str) -> anyhow::Result<String> {
        let exp = Utc::now()
            .checked_add_signed(Duration::days(7))
            .expect("valid exp")
            .timestamp() as usize;
        let claims = Claims {
            sub: user_id.to_string(),
            device_id: device_id.to_string(),
            exp,
            jti: Uuid::new_v4().to_string(),
        };
        let key = EncodingKey::from_secret(self.secret.as_bytes());
        Ok(encode(&Header::new(Algorithm::HS256), &claims, &key)?)
    }

    /// Register a new account. Returns Err when the email is taken.
    pub fn register(&self, db: &Db, email: &str, password: &str) -> anyhow::Result<String> {
        if db.get_user_by_email(email).is_some() {
            anyhow::bail!("email already registered");
        }
        let password_hash = hash_password(password)?;
        let user_id = Uuid::new_v4().to_string();
        // Account-level sync key: generated once, distributed to every device
        // at login so mailbox messages can be decrypted without pairing.
        let mut sync_key = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut sync_key);
        use base64::Engine as _;
        let sync_key_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(sync_key);
        db.create_user(&user_id, email, &password_hash, &sync_key_b64)?;
        Ok(user_id)
    }

    /// Verify credentials; returns the user id on success.
    pub fn login(&self, db: &Db, email: &str, password: &str) -> anyhow::Result<String> {
        let user = db
            .get_user_by_email(email)
            .context("invalid email or password")?;
        verify_password(password, &user.password_hash)?;
        Ok(user.id)
    }

    /// Fetch the account sync key for a user (after login). Lazily generates
    /// one for legacy accounts that predate the sync_key column.
    pub fn sync_key(&self, db: &Db, user_id: &str) -> anyhow::Result<String> {
        let key = db.get_user(user_id).context("user not found")?.sync_key;
        if !key.is_empty() {
            return Ok(key);
        }
        let mut fresh = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut fresh);
        use base64::Engine as _;
        let fresh_b64 = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(fresh);
        db.set_user_sync_key(user_id, &fresh_b64)?;
        Ok(fresh_b64)
    }
}

/// Password hashing: PBKDF2-like iterated SHA-256 with a 16-byte random salt.
///
/// NOTE: This is a pragmatic choice to keep the service dependency-free; for
/// production hardening swap in argon2id (see docs/production-sync-plan.md).
const HASH_ITERATIONS: u32 = 100_000;

pub fn hash_password(password: &str) -> anyhow::Result<String> {
    let mut salt = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut salt);
    let hash = iterated_hash(password.as_bytes(), &salt, HASH_ITERATIONS);
    let salt_b64 = base64_encode(&salt);
    let hash_b64 = base64_encode(&hash);
    Ok(format!("sha256${HASH_ITERATIONS}${salt_b64}${hash_b64}"))
}

pub fn verify_password(password: &str, stored: &str) -> anyhow::Result<()> {
    let parts: Vec<&str> = stored.split('$').collect();
    if parts.len() != 4 || parts[0] != "sha256" {
        anyhow::bail!("unsupported password hash format");
    }
    let iterations: u32 = parts[1].parse().context("bad iterations")?;
    let salt = base64_decode(parts[2]).context("bad salt")?;
    let expected = base64_decode(parts[3]).context("bad hash")?;
    let actual = iterated_hash(password.as_bytes(), &salt, iterations);
    if actual.as_slice() == expected.as_slice() {
        Ok(())
    } else {
        anyhow::bail!("invalid email or password")
    }
}

fn iterated_hash(password: &[u8], salt: &[u8], iterations: u32) -> Vec<u8> {
    let mut state = Sha256::new();
    state.update(salt);
    state.update(password);
    let mut digest = state.finalize().to_vec();
    for _ in 1..iterations {
        let mut h = Sha256::new();
        h.update(&digest);
        digest = h.finalize().to_vec();
    }
    digest
}

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn base64_decode(s: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(s)
        .map_err(|e| anyhow::anyhow!("base64: {e}"))
}

/// PoC helper: issue a token for arbitrary (user, device) without an account.
#[allow(dead_code)]
pub fn issue_test_token(auth: &Auth, user_id: &str, device_id: &str) -> anyhow::Result<String> {
    auth.issue_device_token(user_id, device_id)
}
