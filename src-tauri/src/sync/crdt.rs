use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sqlx::{Acquire, Row, SqlitePool};
use tracing::{info, instrument};

/// Settings-table rows that must never leave this device: `app_settings`
/// embeds LLM API keys (legacy `default_llm` block), and legacy `account.*`
/// rows carry the account token / E2E sync key. Syncing them would leak
/// credentials to every paired device and let last-writer-wins clobber
/// per-device secrets. Account state now lives in `device_settings`, which is
/// device-local by design; this guard also covers rows written before the
/// migration.
pub fn is_non_syncable_setting_key(key: &str) -> bool {
    key == "app_settings" || key.starts_with("account.")
}

/// Whether the optional sync tables (chat_sessions, chat_messages, settings)
/// are currently enabled. Read from the device-local settings cache so the
/// runtime toggle in `set_sync_config` takes effect immediately.
pub fn optional_tables_enabled() -> bool {
    crate::core::settings_service::cached_device_settings().sync_optional_data
}

/// Whether a table's changes may be synced right now.
fn table_sync_enabled(table: &str) -> bool {
    if crate::core::db::OPTIONAL_SYNC_TABLES.contains(&table) {
        return optional_tables_enabled();
    }
    true
}

/// Check whether a `settings`-table row (identified by its decoded pk) must
/// be excluded from sync.
fn setting_row_non_syncable(table: &str, pk: &[u8]) -> bool {
    if table != "settings" {
        return false;
    }
    match decode_pk(pk) {
        Some(values) => values.iter().any(|v| is_non_syncable_setting_key(v)),
        None => false, // undecodable pk: let the merge machinery decide
    }
}

/// A single CR-SQLite change row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CrsqlChange {
    pub table: String,
    pub pk: Vec<u8>,
    pub cid: String,
    pub val: Option<String>,
    pub col_version: i64,
    pub db_version: i64,
    pub site_id: Vec<u8>,
    pub cl: i64,
    pub seq: i64,
}

/// Changeset payload exchanged with a peer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangesetMessage {
    pub changes: Vec<CrsqlChange>,
    pub from_db_version: i64,
    pub to_db_version: i64,
}

/// Export changes from `crsql_changes` since the given db_version (exclusive).
#[instrument(skip(db))]
pub async fn export_changes_since(db: &SqlitePool, since_db_version: i64) -> Result<ChangesetMessage> {
    let rows = sqlx::query(
        r#"SELECT "table", "pk", "cid", CAST("val" AS TEXT) AS "val", "col_version", "db_version", "site_id", "cl", "seq"
           FROM crsql_changes
           WHERE "db_version" > ?
           ORDER BY "db_version", "seq""#,
    )
    .bind(since_db_version)
    .fetch_all(db)
    .await
    .context("export crsql_changes")?;

    let mut changes = Vec::with_capacity(rows.len());
    let mut max_db_version = since_db_version;

    for row in rows {
        let table: String = row.try_get("table").unwrap_or_default();
        if !table_sync_enabled(&table) {
            continue;
        }
        let pk: Vec<u8> = row.try_get("pk").unwrap_or_default();
        if setting_row_non_syncable(&table, &pk) {
            continue;
        }
        let db_version: i64 = row.try_get("db_version").unwrap_or_default();
        max_db_version = max_db_version.max(db_version);
        changes.push(CrsqlChange {
            table,
            pk,
            cid: row.try_get::<String, _>("cid").unwrap_or_default(),
            val: row.try_get::<Option<String>, _>("val").ok().flatten(),
            col_version: row.try_get::<i64, _>("col_version").unwrap_or_default(),
            db_version,
            site_id: row.try_get::<Vec<u8>, _>("site_id").unwrap_or_default(),
            cl: row.try_get::<i64, _>("cl").unwrap_or_default(),
            seq: row.try_get::<i64, _>("seq").unwrap_or_default(),
        });
    }

    info!(count = changes.len(), max_db_version, "exported changes");
    Ok(ChangesetMessage {
        changes,
        from_db_version: since_db_version,
        to_db_version: max_db_version,
    })
}

/// Decode a CR-SQLite pk blob into text column values.
/// Wire format: `[column_count:1][type:1][value...]...`; text type is `0x0B`
/// with a 1-byte length prefix, integer `0x01` (8 bytes BE), float `0x03`,
/// blob `0x05` (length + bytes).
fn decode_pk(pk: &[u8]) -> Option<Vec<String>> {
    let ncols = *pk.first()? as usize;
    let mut i = 1usize;
    let mut values = Vec::with_capacity(ncols);
    for _ in 0..ncols {
        let ty = *pk.get(i)?;
        i += 1;
        match ty {
            0x0B => {
                let len = *pk.get(i)? as usize;
                i += 1;
                let s = std::str::from_utf8(pk.get(i..i + len)?).ok()?;
                values.push(s.to_string());
                i += len;
            }
            0x01 => {
                let bytes: [u8; 8] = pk.get(i..i + 8)?.try_into().ok()?;
                i += 8;
                values.push(i64::from_be_bytes(bytes).to_string());
            }
            0x03 => {
                let bytes: [u8; 8] = pk.get(i..i + 8)?.try_into().ok()?;
                i += 8;
                values.push(f64::from_be_bytes(bytes).to_string());
            }
            0x05 => {
                let len = *pk.get(i)? as usize;
                i += 1;
                values.push(format!("X'{}'", hex::encode(pk.get(i..i + len)?)));
                i += len;
            }
            _ => return None,
        }
    }
    if values.is_empty() {
        None
    } else {
        Some(values)
    }
}

/// Read the local `updated_at` of the row identified by `pk`, if the table
/// has that column and the row exists. Runs on the caller's connection (an
/// active transaction owns it), never on the pool — the pool may be size 1.
async fn local_updated_at(
    conn: &mut sqlx::SqliteConnection,
    table: &str,
    pk: &[u8],
) -> Result<Option<String>> {
    let cols: Vec<String> = sqlx::query_scalar(
        "SELECT name FROM pragma_table_info(?) WHERE pk > 0 ORDER BY pk",
    )
    .bind(table)
    .fetch_all(&mut *conn)
    .await?;
    if cols.is_empty() {
        return Ok(None);
    }
    let Some(values) = decode_pk(pk) else {
        return Ok(None);
    };
    if values.len() != cols.len() {
        return Ok(None);
    }
    let mut sql = format!("SELECT \"updated_at\" FROM \"{table}\" WHERE ");
    for (i, col) in cols.iter().enumerate() {
        if i > 0 {
            sql.push_str(" AND ");
        }
        sql.push('"');
        sql.push_str(col);
        sql.push_str("\" = ?");
    }
    sql.push_str(" LIMIT 1");
    let mut q = sqlx::query_as::<_, (Option<String>,)>(&sql);
    for v in &values {
        q = q.bind(v);
    }
    let row: Option<(Option<String>,)> = q.fetch_optional(&mut *conn).await.ok().flatten();
    Ok(row.and_then(|(ts,)| ts))
}

/// Apply a changeset from a peer into the local `crsql_changes` table.
#[instrument(skip(db, msg))]
pub async fn apply_changes(db: &SqlitePool, msg: &ChangesetMessage) -> Result<()> {
    // Last-write-wins by wall clock: CR-SQLite's default conflict rule is
    // "higher col_version wins", and col_version counts edits per site — a
    // peer that edited a row fewer times (but later) would lose to an older
    // edit. When both sides carry `updated_at`, rows whose local timestamp is
    // newer than the peer's are dropped entirely before the extension's
    // merge logic runs. Delete markers (cid "-1") carry no timestamp and are
    // always applied.
    let mut groups: std::collections::BTreeMap<(String, Vec<u8>), Vec<&CrsqlChange>> =
        std::collections::BTreeMap::new();
    for c in &msg.changes {
        groups
            .entry((c.table.clone(), c.pk.clone()))
            .or_default()
            .push(c);
    }

    // Apply with FK enforcement off. CRR tables no longer declare REFERENCES
    // (CR-SQLite forbids checked FKs on them), so this is a defensive no-op
    // today — it keeps apply robust if a future table adds constraints, and
    // costs nothing. FK checks are re-enabled before the connection returns
    // to the pool. (The PRAGMA must run outside the transaction; it is a
    // no-op inside one.)
    let mut conn = db.acquire().await.context("acquire apply conn")?;
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .context("disable foreign keys for apply")?;
    let apply_result = apply_changes_inner(&mut conn, &groups).await;
    // Always re-enable, even on failure, so the pooled connection stays sane.
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .context("re-enable foreign keys after apply")?;
    let (skipped, paper_ids, deleted_paper_ids) = apply_result?;
    drop(conn);

    // `creators` is a device-local overlay (not synced): rebuild it from the
    // papers JSON columns for the rows this changeset touched.
    if !paper_ids.is_empty() || !deleted_paper_ids.is_empty() {
        if let Err(e) = crate::core::paper_service::rebuild_creators_for_papers(
            db,
            &paper_ids,
            &deleted_paper_ids,
        )
        .await
        {
            tracing::warn!(error = %e, "rebuild creators after changeset failed");
        }
    }

    info!(count = msg.changes.len(), skipped, "applied changes");
    Ok(())
}

/// Inner part of [`apply_changes`], running on an FK-disabled connection.
/// Returns (skipped_count, upserted paper ids, deleted paper ids).
async fn apply_changes_inner(
    conn: &mut sqlx::SqliteConnection,
    groups: &std::collections::BTreeMap<(String, Vec<u8>), Vec<&CrsqlChange>>,
) -> Result<(usize, Vec<String>, Vec<String>)> {
    let mut tx = conn.begin().await.context("begin apply changes tx")?;
    let mut skipped = 0usize;
    let mut paper_ids: Vec<String> = Vec::new();
    let mut deleted_paper_ids: Vec<String> = Vec::new();
    for ((table, pk), group) in groups {
        // Tables that are not currently enabled (optional sync off, or
        // non-syncable settings rows) must not be applied either.
        if !table_sync_enabled(table) || setting_row_non_syncable(table, pk) {
            skipped += group.len();
            continue;
        }
        let peer_ts = group
            .iter()
            .find(|c| c.cid == "updated_at")
            .and_then(|c| c.val.as_deref());
        if let Some(peer_ts) = peer_ts {
            if let Some(local_ts) = local_updated_at(&mut *tx, table, pk).await? {
                if local_ts.as_str() >= peer_ts {
                    skipped += group.len();
                    continue;
                }
            }
        }
        for change in group {
            sqlx::query(
                r#"INSERT INTO crsql_changes
                   ("table", "pk", "cid", "val", "col_version", "db_version", "site_id", "cl", "seq")
                   VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)"#,
            )
            .bind(&change.table)
            .bind(&change.pk)
            .bind(&change.cid)
            .bind(&change.val)
            .bind(change.col_version)
            .bind(change.db_version)
            .bind(&change.site_id)
            .bind(change.cl)
            .bind(change.seq)
            .execute(&mut *tx)
            .await
            .with_context(|| {
                format!(
                    "insert change table={} cid={} pk_len={} val={:?} db_version={} seq={}",
                    change.table,
                    change.cid,
                    change.pk.len(),
                    change.val,
                    change.db_version,
                    change.seq
                )
            })?;
        }
        // Track papers rows actually applied so the device-local creators
        // overlay can be rebuilt afterwards. cid "-1" marks a row deletion.
        if table == "papers" {
            if let Some(id) = decode_pk(pk).and_then(|v| v.into_iter().next()) {
                if group.iter().any(|c| c.cid == "-1") {
                    deleted_paper_ids.push(id);
                } else {
                    paper_ids.push(id);
                }
            }
        }
    }
    tx.commit().await.context("commit apply changes tx")?;
    Ok((skipped, paper_ids, deleted_paper_ids))
}

/// Get the current CR-SQLite database version.
#[allow(dead_code)] // kept for diagnostics/debugging
#[instrument(skip(db))]
pub async fn current_db_version(db: &SqlitePool) -> Result<i64> {
    let row: (i64,) = sqlx::query_as("SELECT crsql_db_version()")
        .fetch_one(db)
        .await
        .context("get crsql_db_version")?;
    Ok(row.0)
}

// ── Full snapshot sync ─────────────────────────────────────────────────────
//
// `crsql_changes` only records writes made *after* a table became a CRR.
// Pre-existing rows (created before the first launch with sync enabled) never
// appear there, so a brand-new device would receive nothing over the
// incremental path. On first connection each side sends a full snapshot
// (one INSERT OR REPLACE statement per row) which is idempotent; afterwards
// the incremental changesets keep both sides in sync.

/// Column list for a table, from `PRAGMA table_info`.
async fn table_columns(db: &SqlitePool, table: &str) -> Result<Vec<String>> {
    let rows = sqlx::query(&format!("PRAGMA table_info(\"{table}\")"))
        .fetch_all(db)
        .await
        .with_context(|| format!("pragma table_info {table}"))?;
    let mut cols = Vec::with_capacity(rows.len());
    for row in &rows {
        cols.push(row.try_get::<String, _>("name")?);
    }
    Ok(cols)
}

/// Serialize one row value as a SQL literal, trying the storage classes in
/// order (TEXT → INTEGER → REAL → BLOB → NULL).
fn row_value(row: &sqlx::sqlite::SqliteRow, i: usize) -> String {
    use sqlx::Row as _;
    if let Ok(Some(s)) = row.try_get::<Option<String>, _>(i) {
        return format!("'{}'", s.replace('\'', "''"));
    }
    if let Ok(Some(n)) = row.try_get::<Option<i64>, _>(i) {
        return n.to_string();
    }
    if let Ok(Some(f)) = row.try_get::<Option<f64>, _>(i) {
        if f.fract() == 0.0 && f.is_finite() {
            return format!("{f:.0}");
        }
        return f.to_string();
    }
    if let Ok(Some(b)) = row.try_get::<Option<Vec<u8>>, _>(i) {
        return format!("X'{}'", hex::encode(b));
    }
    "NULL".to_string()
}

/// Dump the sync-relevant tables as idempotent `INSERT OR REPLACE` statements.
#[instrument(skip(db))]
pub async fn export_full_snapshot(db: &SqlitePool, tables: &[&str]) -> Result<Vec<String>> {
    let mut statements = Vec::new();
    for table in tables {
        let cols = table_columns(db, table).await?;
        if cols.is_empty() {
            continue;
        }
        let rows = sqlx::query(&format!("SELECT * FROM \"{table}\""))
            .fetch_all(db)
            .await
            .with_context(|| format!("select full snapshot {table}"))?;
        let col_list = cols
            .iter()
            .map(|c| format!("\"{c}\""))
            .collect::<Vec<_>>()
            .join(", ");
        for row in &rows {
            // Skip non-syncable settings rows (secrets) in snapshots too.
            if *table == "settings" {
                if let Ok(Some(key)) = row.try_get::<Option<String>, _>("key") {
                    if is_non_syncable_setting_key(&key) {
                        continue;
                    }
                }
            }
            let values = cols
                .iter()
                .enumerate()
                .map(|(i, _)| row_value(row, i))
                .collect::<Vec<_>>()
                .join(", ");
            statements.push(format!(
                "INSERT OR IGNORE INTO \"{table}\" ({col_list}) VALUES ({values})"
            ));
        }
    }
    info!(statements = statements.len(), "exported full snapshot");
    Ok(statements)
}

/// Extract the table name from a snapshot statement
/// (`INSERT OR IGNORE INTO "table" (...)`).
fn statement_table(stmt: &str) -> Option<&str> {
    let rest = stmt.split_once("INTO \"")?.1;
    rest.split('"').next()
}

/// Extract the `key` literal of a `settings` snapshot statement (first column
/// of the VALUES list). Returns None when the statement is not a settings row
/// or cannot be parsed; the caller then falls back to letting it through.
fn settings_statement_key(stmt: &str) -> Option<String> {
    let start = stmt.find("VALUES (")? + "VALUES (".len();
    let rest = &stmt[start..];
    if !rest.starts_with('\'') {
        return None;
    }
    let mut out = String::new();
    let mut chars = rest[1..].chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\'' {
            if chars.peek() == Some(&'\'') {
                chars.next();
                out.push('\'');
                continue;
            }
            break;
        }
        out.push(c);
    }
    Some(out)
}

/// Apply a full snapshot: execute the statements inside one transaction.
/// `statements` is skipped from instrument fields: a full snapshot can hold
/// thousands of data-bearing SQL rows and must never be logged verbatim.
#[instrument(skip(db, statements))]
pub async fn apply_full_snapshot(db: &SqlitePool, statements: &[String]) -> Result<()> {
    // Same defensive FK-off as apply_changes (see there for the rationale);
    // CRR tables declare no REFERENCES, so this is a no-op safeguard.
    let mut conn = db.acquire().await.context("acquire snapshot conn")?;
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .context("disable foreign keys for snapshot")?;
    let apply_result = apply_full_snapshot_inner(&mut conn, statements).await;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .context("re-enable foreign keys after snapshot")?;
    let saw_papers = apply_result?;
    drop(conn);

    // Fill the device-local creators overlay for freshly arrived papers
    // (papers that already have creators rows are left untouched).
    if saw_papers {
        if let Err(e) = crate::core::paper_service::rebuild_creators_where_missing(db).await {
            tracing::warn!(error = %e, "rebuild creators after snapshot failed");
        }
    }

    info!(count = statements.len(), "applied full snapshot");
    Ok(())
}

/// Inner part of [`apply_full_snapshot`] on an FK-disabled connection.
/// Returns whether any `papers` rows were part of the snapshot.
async fn apply_full_snapshot_inner(
    conn: &mut sqlx::SqliteConnection,
    statements: &[String],
) -> Result<bool> {
    let mut saw_papers = false;
    let mut tx = conn.begin().await.context("begin snapshot tx")?;
    for stmt in statements {
        // Defense in depth: never apply rows for disabled optional tables or
        // non-syncable settings keys, even if the peer sends them.
        if let Some(table) = statement_table(stmt) {
            if !table_sync_enabled(table) {
                continue;
            }
            if table == "settings" {
                if let Some(key) = settings_statement_key(stmt) {
                    if is_non_syncable_setting_key(&key) {
                        continue;
                    }
                }
            }
            if table == "papers" {
                saw_papers = true;
            }
        }
        sqlx::query(stmt)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("apply snapshot statement: {}", &stmt[..stmt.len().min(160)]))?;
    }
    tx.commit().await.context("commit snapshot tx")?;
    Ok(saw_papers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::{
        register_crr_tables, tests::connect_with_crsqlite, CORE_SYNC_TABLES, OPTIONAL_SYNC_TABLES,
        SCHEMA_INIT_SQL,
    };

    #[tokio::test]
    async fn export_and_apply_changes_round_trip() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!("siku-crdt-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a_path = dir.join("a.db");
        let db_a = connect_with_crsqlite(&db_a_path).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_a).await?;
        register_crr_tables(&db_a, CORE_SYNC_TABLES).await?;

        // Insert a paper, note and annotation.
        sqlx::query(
            "INSERT INTO papers (id, title, created_at, updated_at, imported_at) \
             VALUES (?, ?, ?, ?, ?)"
        )
        .bind("p1")
        .bind("Paper One")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)"
        )
        .bind("n1")
        .bind(1i64)
        .bind("Note One")
        .bind("Hello **world**")
        .bind("Hello world")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;
        sqlx::query(
            "INSERT INTO annotations (id, paper_id, page, type, rect, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?)"
        )
        .bind("a1")
        .bind("p1")
        .bind(1i64)
        .bind("highlight")
        .bind("[0,0,1,1]")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;

        let changes = export_changes_since(&db_a, 0).await?;
        assert!(!changes.changes.is_empty(), "should export changes");

        let db_b_path = dir.join("b.db");
        let db_b = connect_with_crsqlite(&db_b_path).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_b).await?;
        register_crr_tables(&db_b, CORE_SYNC_TABLES).await?;

        apply_changes(&db_b, &changes).await?;

        let note_count: (i64,) = sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'n1'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(note_count.0, 1);
        let paper_count: (i64,) = sqlx::query_as("SELECT count(*) FROM papers WHERE id = 'p1'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(paper_count.0, 1);
        let annotation_count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM annotations WHERE id = 'a1'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(annotation_count.0, 1);

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn export_and_apply_optional_changes_round_trip() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-crdt-optional-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a_path = dir.join("a.db");
        let db_a = connect_with_crsqlite(&db_a_path).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_a).await?;
        register_crr_tables(&db_a, CORE_SYNC_TABLES).await?;
        register_crr_tables(&db_a, OPTIONAL_SYNC_TABLES).await?;

        sqlx::query(
            "INSERT INTO chat_sessions (id, title, created_at, updated_at) \
             VALUES (?, ?, ?, ?)"
        )
        .bind("s1")
        .bind("Chat One")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;
        sqlx::query(
            "INSERT INTO chat_messages (id, session_id, role, content, created_at) \
             VALUES (?, ?, ?, ?, ?)"
        )
        .bind("m1")
        .bind("s1")
        .bind("user")
        .bind("Hello")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;
        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("theme")
            .bind("dark")
            .bind("2026-01-01T00:00:00Z")
            .execute(&db_a)
            .await?;

        let changes = export_changes_since(&db_a, 0).await?;
        assert!(!changes.changes.is_empty(), "should export optional changes");

        let db_b_path = dir.join("b.db");
        let db_b = connect_with_crsqlite(&db_b_path).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_b).await?;
        register_crr_tables(&db_b, CORE_SYNC_TABLES).await?;
        register_crr_tables(&db_b, OPTIONAL_SYNC_TABLES).await?;

        apply_changes(&db_b, &changes).await?;

        let session_count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM chat_sessions WHERE id = 's1'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(session_count.0, 1);
        let message_count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM chat_messages WHERE id = 'm1'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(message_count.0, 1);
        let setting_value: (String,) =
            sqlx::query_as("SELECT value FROM settings WHERE key = 'theme'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(setting_value.0, "dark");

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// Full snapshot must carry pre-CRR rows (which never appear in
    /// crsql_changes) and must be idempotent when applied twice.
    #[tokio::test]
    async fn full_snapshot_carries_existing_rows() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-snapshot-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a_path = dir.join("a.db");
        let db_a = connect_with_crsqlite(&db_a_path).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_a).await?;

        // Legacy row: exists BEFORE the tables are registered as CRRs, so
        // crsql_changes never records it (real historical-data scenario).
        sqlx::query(
            "INSERT INTO notes (id, title, content, content_plain, tags, aliases,              is_favorite, is_folder, parent_id, created_at, updated_at)              VALUES ('legacy-1', 'Old Note', 'body', 'body', '[]', '[]', 0, 0, NULL, ?, ?)",
        )
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;

        register_crr_tables(&db_a, CORE_SYNC_TABLES).await?;

        // CR-SQLite generates initial changes for pre-existing rows when the
        // table becomes a CRR, so the incremental export DOES carry them.
        let changes = export_changes_since(&db_a, 0).await?;
        assert!(
            changes.changes.iter().any(|c| c.table == "notes"),
            "CRR registration should emit initial changes for legacy rows"
        );

        // Full snapshot must include it.
        let statements = export_full_snapshot(&db_a, CORE_SYNC_TABLES).await?;
        assert!(
            statements.iter().any(|s| s.contains("legacy-1") && s.contains("INSERT OR IGNORE INTO \"notes\"")),
            "snapshot should carry the legacy note"
        );

        // Apply to a fresh db; twice = idempotent.
        let db_b_path = dir.join("b.db");
        let db_b = connect_with_crsqlite(&db_b_path).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_b).await?;
        register_crr_tables(&db_b, CORE_SYNC_TABLES).await?;
        apply_full_snapshot(&db_b, &statements).await?;
        apply_full_snapshot(&db_b, &statements).await?;

        let count: (i64,) =
            sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'legacy-1'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(count.0, 1);
        let title: (String,) =
            sqlx::query_as("SELECT title FROM notes WHERE id = 'legacy-1'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(title.0, "Old Note");

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// A peer change carrying an older updated_at must NOT overwrite a newer
    /// local edit, even when the peer's col_version is higher. Regression for
    /// "new data clobbered by old data" (CR-SQLite default LWW compares edit
    /// counts, not wall-clock time).
    #[tokio::test]
    async fn newer_local_edit_wins_over_stale_peer_change() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-lww-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a = connect_with_crsqlite(&dir.join("a.db")).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_a).await?;
        register_crr_tables(&db_a, CORE_SYNC_TABLES).await?;
        let db_b = connect_with_crsqlite(&dir.join("b.db")).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_b).await?;
        register_crr_tables(&db_b, CORE_SYNC_TABLES).await?;

        // A creates the note at T1.
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('n1', 1, 'A原始标题', 'body', 'body', '[]', '[]', ?, ?)",
        )
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;
        let changes = export_changes_since(&db_a, 0).await?;
        apply_changes(&db_b, &changes).await?;

        // B edits at T2 (newer). B's col_version for the title is 1, same as
        // A's, so the stock LWW rule would decide by site_id — random.
        sqlx::query("UPDATE notes SET title = 'B新标题', updated_at = ? WHERE id = 'n1'")
            .bind("2026-08-15T12:00:00Z")
            .execute(&db_b)
            .await?;
        let changes = export_changes_since(&db_b, 0).await?;
        apply_changes(&db_a, &changes).await?;

        let title: (String,) =
            sqlx::query_as("SELECT title FROM notes WHERE id = 'n1'")
                .fetch_one(&db_a)
                .await?;
        assert_eq!(title.0, "B新标题", "newer wall-clock edit must win");

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// A full snapshot must initialize missing rows but never overwrite rows
    /// that already exist locally (the merge happens through changesets).
    #[tokio::test]
    async fn snapshot_does_not_overwrite_existing_rows() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-snapshot-keep-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        // B has a newer local row.
        let db_b = connect_with_crsqlite(&dir.join("b.db")).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_b).await?;
        register_crr_tables(&db_b, CORE_SYNC_TABLES).await?;
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('n1', 1, 'B新标题', 'body', 'body', '[]', '[]', ?, ?)",
        )
        .bind("2026-08-15T12:00:00Z")
        .bind("2026-08-15T12:00:00Z")
        .execute(&db_b)
        .await?;

        // A has a stale row and sends a snapshot.
        let db_a = connect_with_crsqlite(&dir.join("a.db")).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_a).await?;
        register_crr_tables(&db_a, CORE_SYNC_TABLES).await?;
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('n1', 1, 'A旧标题', 'body', 'body', '[]', '[]', ?, ?)",
        )
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;
        let statements = export_full_snapshot(&db_a, CORE_SYNC_TABLES).await?;
        apply_full_snapshot(&db_b, &statements).await?;

        let title: (String,) =
            sqlx::query_as("SELECT title FROM notes WHERE id = 'n1'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(title.0, "B新标题", "snapshot must not clobber local rows");

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// Organization tables (vaults, tags, collections, composite-PK junction
    /// tables, note_versions, note_links) must round-trip through changesets
    /// even though the receiving db is empty and change groups apply in
    /// alphabetical table order — children before their parents (apply runs
    /// with FK enforcement off; convergence settles the references). The
    /// device-local creators overlay must be rebuilt from the synced papers
    /// JSON columns.
    #[tokio::test]
    async fn org_tables_round_trip_with_creators_rebuild() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-org-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db_a = connect_with_crsqlite(&dir.join("a.db")).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_a).await?;
        register_crr_tables(&db_a, CORE_SYNC_TABLES).await?;

        let ts = "2026-01-01T00:00:00Z";
        // Parents first (FK is enforced locally).
        sqlx::query("INSERT INTO papers (id, title, authors, created_at, updated_at, imported_at) \
             VALUES ('p1', 'Paper One', '[\"Alice\"]', ?, ?, ?)")
            .bind(ts).bind(ts).bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO papers (id, title, created_at, updated_at, imported_at) \
             VALUES ('p2', 'Paper Two', ?, ?, ?)")
            .bind(ts).bind(ts).bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO vaults (id, name, created_at, updated_at) VALUES ('v1', 'Vault One', ?, ?)")
            .bind(ts).bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO notes (id, vault_id, title, created_at, updated_at) VALUES ('n1', 'v1', 'Note One', ?, ?)")
            .bind(ts).bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO notes (id, vault_id, title, created_at, updated_at) VALUES ('n2', 'v1', 'Note Two', ?, ?)")
            .bind(ts).bind(ts).execute(&db_a).await?;
        // Children of the above, including self-referencing rows.
        sqlx::query("INSERT INTO note_versions (id, note_id, title, content, edited_by, created_at) \
             VALUES ('nv1', 'n1', 'Note One', 'body', 'agent', ?)")
            .bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO note_links (source_id, target_id, context, created_at) VALUES ('n1', 'n2', 'ctx', ?)")
            .bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO tags (id, name, color, parent_id, created_at) VALUES ('t1', 'root', '#fff', NULL, ?)")
            .bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO tags (id, name, color, parent_id, created_at) VALUES ('t2', 'child', '#fff', 't1', ?)")
            .bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO paper_tags (paper_id, tag_id) VALUES ('p1', 't2')")
            .execute(&db_a).await?;
        sqlx::query("INSERT INTO collections (id, name, parent_id, sort_order, created_at) VALUES ('c1', 'Root', NULL, 0, ?)")
            .bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO collections (id, name, parent_id, sort_order, created_at) VALUES ('c2', 'Child', 'c1', 0, ?)")
            .bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO paper_collections (paper_id, collection_id) VALUES ('p1', 'c2')")
            .execute(&db_a).await?;
        sqlx::query("INSERT INTO related_papers (paper_id, related_id, created_at) VALUES ('p1', 'p2', ?)")
            .bind(ts).execute(&db_a).await?;
        sqlx::query("INSERT INTO bookmarks (id, title, route, params_json, created_at) VALUES ('b1', 'Lib', '/library', '{}', ?)")
            .bind(ts).execute(&db_a).await?;

        let changes = export_changes_since(&db_a, 0).await?;

        let db_b = connect_with_crsqlite(&dir.join("b.db")).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_b).await?;
        register_crr_tables(&db_b, CORE_SYNC_TABLES).await?;
        apply_changes(&db_b, &changes).await?;

        for table in [
            "vaults",
            "note_versions",
            "note_links",
            "tags",
            "paper_tags",
            "collections",
            "paper_collections",
            "related_papers",
            "bookmarks",
        ] {
            let (count,): (i64,) =
                sqlx::query_as(&format!("SELECT count(*) FROM \"{table}\""))
                    .fetch_one(&db_b)
                    .await?;
            let expected = if table == "tags" || table == "collections" { 2 } else { 1 };
            assert_eq!(count, expected, "{table} rows should have synced");
        }

        // The creators overlay is rebuilt from the synced papers.authors JSON.
        let (creators_count,): (i64,) =
            sqlx::query_as("SELECT count(*) FROM creators WHERE paper_id = 'p1' AND name = 'Alice'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(creators_count, 1, "creators overlay should be rebuilt");

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }
}
