use sha2::Digest as _;
use sqlx::SqlitePool;

use crate::core::vault_service::get_current_vault_id;

/// Iteration count for the lock password hash. This is a UI privacy gate,
/// not a cryptographic vault — a fixed-cost KDF deters casual guessing
/// without dedicated password-cracking hardware.
pub const LOCK_HASH_ROUNDS: u32 = 100_000;

/// Hash a lock password with the given salt (hex-encoded). The digest input
/// is the UTF-8 bytes of `salt_hex + ":" + password`, fed through
/// `LOCK_HASH_ROUNDS` rounds of SHA-256 (each round hashes the previous
/// digest), hex-encoded at the end.
pub fn hash_lock_password(salt_hex: &str, password: &str) -> String {
    let mut digest = sha2::Sha256::digest(format!("{salt_hex}:{password}").as_bytes());
    for _ in 1..LOCK_HASH_ROUNDS {
        digest = sha2::Sha256::digest(digest);
    }
    hex::encode(digest)
}

/// Whether the current vault row carries a lock password hash.
pub async fn has_lock_password(db: &SqlitePool) -> Result<bool, String> {
    let vault_id = get_current_vault_id(db).await?;
    let hash: Option<Option<String>> = sqlx::query_scalar("SELECT lock_hash FROM vaults WHERE id = ?")
        .bind(&vault_id)
        .fetch_optional(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(hash.flatten().is_some())
}

/// Set the global lock password. Written to ALL vault rows (one password for
/// the whole app), so it survives vault switching and syncs via CRDT.
/// Refuses when the current vault already has a password — there is no reset
/// flow (confirmed product decision).
pub async fn set_lock_password(db: &SqlitePool, password: &str) -> Result<(), String> {
    if password.is_empty() {
        return Err("锁密码不能为空".to_string());
    }
    if has_lock_password(db).await? {
        return Err("已设置过锁密码".to_string());
    }
    let salt_hex = hex::encode(rand::random::<[u8; 16]>());
    let hash = hash_lock_password(&salt_hex, password);
    sqlx::query("UPDATE vaults SET lock_hash = ?, lock_salt = ?")
        .bind(&hash)
        .bind(&salt_hex)
        .execute(db)
        .await
        .map_err(|e| format!("db: {e}"))?;
    Ok(())
}

/// Verify a candidate password against the current vault row.
/// Returns Ok(false) when no password is set or it does not match.
pub async fn verify_lock_password(db: &SqlitePool, password: &str) -> Result<bool, String> {
    let vault_id = get_current_vault_id(db).await?;
    let row: Option<(Option<String>, Option<String>)> =
        sqlx::query_as("SELECT lock_hash, lock_salt FROM vaults WHERE id = ?")
            .bind(&vault_id)
            .fetch_optional(db)
            .await
            .map_err(|e| format!("db: {e}"))?;
    match row {
        Some((Some(hash), Some(salt))) => Ok(hash_lock_password(&salt, password) == hash),
        _ => Ok(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_and_password_sensitive() {
        let salt = "0123456789abcdef0123456789abcdef";
        let h1 = hash_lock_password(salt, "hunter2");
        let h2 = hash_lock_password(salt, "hunter2");
        assert_eq!(h1, h2, "same salt + password must hash identically");
        assert_eq!(h1.len(), 64, "hex-encoded sha256");
        let h3 = hash_lock_password(salt, "hunter3");
        assert_ne!(h1, h3, "different password must hash differently");
    }

    async fn fresh_db(dir: &std::path::Path) -> SqlitePool {
        let _ = std::fs::remove_dir_all(dir);
        std::fs::create_dir_all(dir).unwrap();
        let db = crate::core::db::tests::connect_with_crsqlite(&dir.join("lock.db"))
            .await
            .unwrap();
        sqlx::query(crate::core::db::SCHEMA_INIT_SQL)
            .execute(&db)
            .await
            .unwrap();
        crate::core::db::register_crr_tables(&db, crate::core::db::CORE_SYNC_TABLES)
            .await
            .unwrap();
        crate::core::vault_service::ensure_defaults(&db).await.unwrap();
        db
    }

    #[tokio::test]
    async fn set_password_once_and_verify() {
        let dir = std::env::temp_dir().join(format!(
            "siku-lock-svc-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = fresh_db(&dir).await;

        assert!(!has_lock_password(&db).await.unwrap());
        assert!(!verify_lock_password(&db, "pw").await.unwrap());

        set_lock_password(&db, "pw").await.unwrap();
        assert!(has_lock_password(&db).await.unwrap());
        assert!(verify_lock_password(&db, "pw").await.unwrap());
        assert!(!verify_lock_password(&db, "wrong").await.unwrap());

        // Second set is rejected; empty password is rejected.
        assert!(set_lock_password(&db, "pw2").await.is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn set_password_rejects_empty() {
        let dir = std::env::temp_dir().join(format!(
            "siku-lock-empty-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let db = fresh_db(&dir).await;
        assert!(set_lock_password(&db, "").await.is_err());
        assert!(!has_lock_password(&db).await.unwrap());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
