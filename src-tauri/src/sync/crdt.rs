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
/// migration. `notes.current_vault_id` is a device-local UI preference (which
/// vault the user is looking at) — syncing it makes devices fight over the
/// active vault via LWW.
pub fn is_non_syncable_setting_key(key: &str) -> bool {
    key == "app_settings"
        || key.starts_with("account.")
        || key == crate::core::vault_service::CURRENT_VAULT_KEY
}

/// Columns that must never leave this device. `chat_sessions.working_dir` is
/// an absolute filesystem path: syncing it plants a foreign machine's path
/// (e.g. `C:\Users\<other-user>\...`) into every peer, where it does not
/// exist — every sandboxed tool call then fails with os error 3. The column
/// stays device-local; peers run the session without a sandbox root until the
/// user picks a directory locally.
fn non_syncable_column(table: &str, cid: &str) -> bool {
    table == "chat_sessions" && cid == "working_dir"
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
        // Advance the export watermark for EVERY row in range — including
        // filtered-out ones (disabled optional tables, non-syncable settings).
        // Otherwise a range containing only filtered rows leaves
        // `to_db_version` stuck and every push re-scans the same rows.
        let db_version: i64 = row.try_get("db_version").unwrap_or_default();
        max_db_version = max_db_version.max(db_version);
        let table: String = row.try_get("table").unwrap_or_default();
        if !table_sync_enabled(&table) {
            continue;
        }
        let cid: String = row.try_get::<String, _>("cid").unwrap_or_default();
        if non_syncable_column(&table, &cid) {
            continue;
        }
        let pk: Vec<u8> = row.try_get("pk").unwrap_or_default();
        if setting_row_non_syncable(&table, &pk) {
            continue;
        }
        changes.push(CrsqlChange {
            table,
            pk,
            cid,
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

/// Whether the local `updated_at` is strictly newer than the peer's. Both
/// sides write RFC3339 (seconds historically, milliseconds since
/// 2026-08-31), and mixed-precision strings do NOT order lexicographically
/// at a second boundary ('.' < 'Z', so "...:00.500Z" sorts before "...:00Z"
/// despite being later) — parse before comparing. Unparseable values fall
/// back to lexicographic order. Equal timestamps return false so the change
/// is applied and cr-sqlite's col_version merge decides.
fn local_ts_is_newer(local_ts: &str, peer_ts: &str) -> bool {
    match (
        chrono::DateTime::parse_from_rfc3339(local_ts),
        chrono::DateTime::parse_from_rfc3339(peer_ts),
    ) {
        (Ok(local), Ok(peer)) => local > peer,
        _ => local_ts > peer_ts,
    }
}

/// Apply a changeset from a peer into the local `crsql_changes` table.
/// Returns the number of changes actually written (received minus the ones
/// skipped by the sync-scope / last-write-wins / non-syncable-column filters).
#[instrument(skip(db, msg))]
pub async fn apply_changes(db: &SqlitePool, msg: &ChangesetMessage) -> Result<u64> {
    // Last-write-wins by wall clock: CR-SQLite's default conflict rule is
    // "higher col_version wins", and col_version counts edits per site — a
    // peer that edited a row fewer times (but later) would lose to an older
    // edit. When both sides carry `updated_at`, rows whose local timestamp is
    // strictly newer than the peer's are dropped entirely before the
    // extension's merge logic runs. Equal timestamps do NOT skip: the change
    // goes through cr-sqlite's col_version merge, so a same-second peer edit
    // is not silently lost. Delete markers (cid "-1") carry no timestamp and
    // are always applied.
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
    Ok((msg.changes.len() - skipped) as u64)
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
                if local_ts_is_newer(&local_ts, peer_ts) {
                    skipped += group.len();
                    continue;
                }
            }
        }
        for change in group {
            // Defense in depth: peers running older builds may still send
            // device-local columns (e.g. chat_sessions.working_dir).
            if non_syncable_column(table, &change.cid) {
                skipped += 1;
                continue;
            }
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

// ── Full snapshot sync (legacy wire format) ────────────────────────────────
//
// `crsql_changes` only records writes made *after* a table became a CRR, but
// CR-SQLite backfills initial changes for pre-existing rows at registration,
// so a full-history changeset (db_version 0 → now) covers the whole library —
// including tombstones. Senders therefore ship a full changeset instead of
// row INSERTs (see `engine::sync_once_inner` / `deliver_full_snapshot_mailbox`);
// the export/apply functions below remain for BACKWARD COMPATIBILITY with
// pre-changeset peers. `apply_full_snapshot` filters out rows this device has
// already deleted (tombstones in crsql_changes) so an old peer's snapshot can
// no longer resurrect them.

/// Column list for a table, from `PRAGMA table_info`.
#[allow(dead_code)] // legacy snapshot export (tests + reference for the wire format)
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
#[allow(dead_code)] // legacy snapshot export (tests + reference for the wire format)
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
/// No longer sent by current builds (senders ship a full-history changeset);
/// kept for tests and as the reference for the legacy wire format that
/// `apply_full_snapshot` still accepts from old peers.
#[allow(dead_code)]
#[instrument(skip(db))]
pub async fn export_full_snapshot(db: &SqlitePool, tables: &[&str]) -> Result<Vec<String>> {
    let mut statements = Vec::new();
    for table in tables {
        let cols = table_columns(db, table).await?;
        // Drop device-local columns (e.g. chat_sessions.working_dir) from the
        // projection; `SELECT *` column order matches PRAGMA table_info.
        let keep: Vec<usize> = cols
            .iter()
            .enumerate()
            .filter(|(_, c)| !non_syncable_column(table, c))
            .map(|(i, _)| i)
            .collect();
        if keep.is_empty() {
            continue;
        }
        let rows = sqlx::query(&format!("SELECT * FROM \"{table}\""))
            .fetch_all(db)
            .await
            .with_context(|| format!("select full snapshot {table}"))?;
        let col_list = keep
            .iter()
            .map(|&i| format!("\"{}\"", cols[i]))
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
            let values = keep
                .iter()
                .map(|&i| row_value(row, i))
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
/// Returns the number of rows actually inserted (INSERT OR IGNORE rows that
/// hit existing data — and rows dropped by the tombstone/sync-scope filters —
/// do not count).
#[instrument(skip(db, statements))]
pub async fn apply_full_snapshot(db: &SqlitePool, statements: &[String]) -> Result<u64> {
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
    let (saw_papers, applied) = apply_result?;
    drop(conn);

    // Fill the device-local creators overlay for freshly arrived papers
    // (papers that already have creators rows are left untouched).
    if saw_papers {
        if let Err(e) = crate::core::paper_service::rebuild_creators_where_missing(db).await {
            tracing::warn!(error = %e, "rebuild creators after snapshot failed");
        }
    }

    info!(count = statements.len(), applied, "applied full snapshot");
    Ok(applied)
}

/// Inner part of [`apply_full_snapshot`] on an FK-disabled connection.
/// Returns (whether any `papers` rows were part of the snapshot, rows
/// actually inserted).
async fn apply_full_snapshot_inner(
    conn: &mut sqlx::SqliteConnection,
    statements: &[String],
) -> Result<(bool, u64)> {
    let mut saw_papers = false;
    let mut applied = 0u64;
    let mut tx = conn.begin().await.context("begin snapshot tx")?;
    // Lazily-filled per-table caches for the tombstone filter: pk column
    // names, and the decoded pks that carry a delete marker (cid "-1").
    let mut pk_cols_cache: std::collections::HashMap<String, Vec<String>> = Default::default();
    let mut tombstone_cache: std::collections::HashMap<
        String,
        std::collections::HashSet<Vec<String>>,
    > = Default::default();
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
            // Tombstone filter: a row this device deleted must not be
            // resurrected by a peer's legacy snapshot. INSERT OR IGNORE would
            // re-insert it as a LOCAL write (firing the CRR triggers with
            // this device's site_id), breaking delete-wins and broadcasting
            // the zombie row back to every peer. Skip statements whose pk
            // already has a delete marker in crsql_changes.
            if snapshot_row_is_tombstoned(
                &mut tx,
                &mut pk_cols_cache,
                &mut tombstone_cache,
                table,
                stmt,
            )
            .await?
            {
                continue;
            }
            if table == "papers" {
                saw_papers = true;
            }
        }
        let res = sqlx::query(stmt)
            .execute(&mut *tx)
            .await
            .with_context(|| format!("apply snapshot statement: {}", &stmt[..stmt.len().min(160)]))?;
        applied += res.rows_affected();
    }
    tx.commit().await.context("commit snapshot tx")?;
    Ok((saw_papers, applied))
}

/// Whether the row a legacy snapshot statement inserts is known-deleted
/// locally, i.e. a delete marker (cid "-1") for its pk exists in
/// `crsql_changes`. Parse/lookup failures are non-fatal: the statement is
/// let through to the merge rather than dropped.
async fn snapshot_row_is_tombstoned(
    conn: &mut sqlx::SqliteConnection,
    pk_cols_cache: &mut std::collections::HashMap<String, Vec<String>>,
    tombstone_cache: &mut std::collections::HashMap<String, std::collections::HashSet<Vec<String>>>,
    table: &str,
    stmt: &str,
) -> Result<bool> {
    if !pk_cols_cache.contains_key(table) {
        let cols: Vec<String> = sqlx::query_scalar(
            "SELECT name FROM pragma_table_info(?) WHERE pk > 0 ORDER BY pk",
        )
        .bind(table)
        .fetch_all(&mut *conn)
        .await?;
        pk_cols_cache.insert(table.to_string(), cols);
    }
    let pk_cols = &pk_cols_cache[table];
    if pk_cols.is_empty() {
        return Ok(false);
    }
    let Some((cols, values)) = parse_snapshot_statement(stmt) else {
        return Ok(false);
    };
    let mut pk_values = Vec::with_capacity(pk_cols.len());
    for pk_col in pk_cols {
        let Some(idx) = cols.iter().position(|c| c == pk_col) else {
            return Ok(false);
        };
        let Some(v) = values.get(idx) else {
            return Ok(false);
        };
        pk_values.push(v.clone());
    }
    if !tombstone_cache.contains_key(table) {
        let rows: Vec<Vec<u8>> = sqlx::query_scalar(
            "SELECT pk FROM crsql_changes WHERE \"table\" = ? AND cid = '-1'",
        )
        .bind(table)
        .fetch_all(&mut *conn)
        .await
        .context("read tombstones for snapshot filter")?;
        let set = rows.iter().filter_map(|pk| decode_pk(pk)).collect();
        tombstone_cache.insert(table.to_string(), set);
    }
    Ok(tombstone_cache[table].contains(&pk_values))
}

/// Split a legacy snapshot statement into its column names and value
/// literals, normalized to the same string form [`decode_pk`] produces
/// (integers as decimal text, blobs as `X'<lowercase hex>'`, text raw).
/// Format: `INSERT OR IGNORE INTO "t" ("c1", ...) VALUES (v1, ...)`.
fn parse_snapshot_statement(stmt: &str) -> Option<(Vec<String>, Vec<String>)> {
    let cols_open = stmt.find('(')?;
    let cols_close = matching_paren(stmt, cols_open)?;
    let cols = stmt[cols_open + 1..cols_close]
        .split(',')
        .map(|c| c.trim().trim_matches('"').to_string())
        .collect();
    let values_part = stmt[cols_close + 1..]
        .trim()
        .strip_prefix("VALUES")?
        .trim()
        .strip_prefix('(')?
        .strip_suffix(')')?;
    Some((cols, split_sql_literals(values_part)))
}

/// Find the `)` matching the `(` at byte index `open`, skipping single-quoted
/// SQL strings (with '' escapes).
fn matching_paren(s: &str, open: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    let mut depth = 0usize;
    let mut i = open;
    while i < bytes.len() {
        match bytes[i] {
            b'\'' => {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == b'\'' {
                        if bytes.get(i + 1) == Some(&b'\'') {
                            i += 2;
                            continue;
                        }
                        break;
                    }
                    i += 1;
                }
            }
            b'(' => depth += 1,
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
        i += 1;
    }
    None
}

/// Split a SQL VALUES literal list on top-level commas (respecting quoted
/// strings) and normalize each literal to [`decode_pk`]'s string form.
fn split_sql_literals(values: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = values.as_bytes();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        if bytes[i] == b'\'' {
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\'' {
                    if bytes.get(i + 1) == Some(&b'\'') {
                        i += 2;
                        continue;
                    }
                    break;
                }
                i += 1;
            }
        } else if bytes[i] == b',' {
            out.push(normalize_sql_literal(&values[start..i]));
            start = i + 1;
        }
        i += 1;
    }
    out.push(normalize_sql_literal(&values[start..]));
    out
}

/// Normalize one SQL literal (as produced by `row_value` in snapshot export)
/// to the string form [`decode_pk`] yields for the same value. NULL maps to
/// a sentinel that can never match a decoded pk (pks are never NULL).
fn normalize_sql_literal(tok: &str) -> String {
    let t = tok.trim();
    if t.eq_ignore_ascii_case("null") {
        return "\u{0}NULL".to_string();
    }
    if t.len() >= 2 && t[..1].eq_ignore_ascii_case("x") && t[1..].starts_with('\'') && t.ends_with('\'') {
        return format!("X'{}", t[2..t.len() - 1].to_lowercase());
    }
    if t.len() >= 2 && t.starts_with('\'') && t.ends_with('\'') {
        return t[1..t.len() - 1].replace("''", "'");
    }
    if let Ok(n) = t.parse::<i64>() {
        return n.to_string();
    }
    t.to_string()
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

    /// `notes.current_vault_id` is a device-local UI preference (which vault
    /// the user is looking at). Syncing it makes devices fight over the
    /// active vault via LWW — switching vaults on one device would yank every
    /// other device's notes view to a different vault. It must be filtered
    /// from changesets like the secret settings rows.
    #[tokio::test]
    async fn current_vault_setting_never_leaves_device() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-crdt-vault-setting-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;

        let db = connect_with_crsqlite(&dir.join("a.db")).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db).await?;
        register_crr_tables(&db, CORE_SYNC_TABLES).await?;
        register_crr_tables(&db, OPTIONAL_SYNC_TABLES).await?;

        // A syncable setting (should export) + the device-local vault pointer
        // (must NOT export) + a secret key (must NOT export).
        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("theme")
            .bind("dark")
            .bind("2026-01-01T00:00:00Z")
            .execute(&db)
            .await?;
        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)")
            .bind(crate::core::vault_service::CURRENT_VAULT_KEY)
            .bind("00000000-0000-0000-0000-000000000001")
            .bind("2026-01-01T00:00:00Z")
            .execute(&db)
            .await?;
        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("account.sync_key")
            .bind("secret")
            .bind("2026-01-01T00:00:00Z")
            .execute(&db)
            .await?;

        let changes = export_changes_since(&db, 0).await?;
        let settings_keys: Vec<String> = changes
            .changes
            .iter()
            .filter(|c| c.table == "settings")
            .filter_map(|c| decode_pk(&c.pk).and_then(|v| v.into_iter().next()))
            .collect();
        assert!(
            settings_keys.contains(&"theme".to_string()),
            "ordinary settings must still sync: {settings_keys:?}"
        );
        assert!(
            !settings_keys
                .contains(&crate::core::vault_service::CURRENT_VAULT_KEY.to_string()),
            "current-vault pointer must never leave the device: {settings_keys:?}"
        );
        assert!(
            !settings_keys.contains(&"account.sync_key".to_string()),
            "account.* secrets must never leave the device"
        );

        sqlx::query("SELECT crsql_finalize()").execute(&db).await?;
        db.close().await;
        Ok(())
    }

    /// `chat_sessions.working_dir` is an absolute path local to the device
    /// that created the session. It must be stripped from changesets AND
    /// snapshots — otherwise peers receive a path that only exists on the
    /// originating machine (e.g. `C:\Users\<other-user>\...`), and every
    /// sandboxed tool call there fails with os error 3. The apply side also
    /// rejects the column as defense in depth against older peers.
    #[tokio::test]
    async fn working_dir_never_leaves_device() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-crdt-wd-test-{}-{}",
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
            "INSERT INTO chat_sessions (id, title, working_dir, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind("s1")
        .bind("Chat One")
        .bind(r"C:\Users\other-machine\vault")
        .bind("2026-01-01T00:00:00Z")
        .bind("2026-01-01T00:00:00Z")
        .execute(&db_a)
        .await?;

        // Incremental export: the row syncs, the working_dir column does not.
        let changes = export_changes_since(&db_a, 0).await?;
        assert!(!changes.changes.is_empty(), "should export the session row");
        assert!(
            changes
                .changes
                .iter()
                .all(|c| !(c.table == "chat_sessions" && c.cid == "working_dir")),
            "working_dir must be stripped from the changeset"
        );

        // Snapshot export: no statement may carry the column at all.
        let statements = export_full_snapshot(&db_a, &["chat_sessions"]).await?;
        assert!(!statements.is_empty(), "snapshot should contain the row");
        assert!(
            statements.iter().all(|s| !s.contains("working_dir")),
            "working_dir must be stripped from the snapshot"
        );

        // Apply the stripped changeset: row arrives with working_dir NULL.
        let db_b_path = dir.join("b.db");
        let db_b = connect_with_crsqlite(&db_b_path).await?;
        sqlx::query(SCHEMA_INIT_SQL).execute(&db_b).await?;
        register_crr_tables(&db_b, CORE_SYNC_TABLES).await?;
        register_crr_tables(&db_b, OPTIONAL_SYNC_TABLES).await?;
        apply_changes(&db_b, &changes).await?;
        let wd: (Option<String>,) =
            sqlx::query_as("SELECT working_dir FROM chat_sessions WHERE id = 's1'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(wd.0, None, "applied session must not carry working_dir");

        // Defense in depth: even a hand-crafted changeset from an old peer
        // that DOES include working_dir must not write the column.
        // pk blob for id "s1": [ncols=1][text=0x0B][len=2]['s']['1'].
        let forged = ChangesetMessage {
            from_db_version: 0,
            to_db_version: 1,
            changes: vec![CrsqlChange {
                table: "chat_sessions".into(),
                pk: vec![1, 0x0B, 2, b's', b'1'],
                cid: "working_dir".into(),
                val: Some(r"C:\Users\other-machine\vault".into()),
                col_version: 99,
                db_version: 1,
                site_id: vec![9, 9, 9, 9],
                cl: 1,
                seq: 0,
            }],
        };
        apply_changes(&db_b, &forged).await?;
        let wd: (Option<String>,) =
            sqlx::query_as("SELECT working_dir FROM chat_sessions WHERE id = 's1'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(wd.0, None, "forged working_dir change must be rejected");

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// Regression: rows filtered out of the export (here a non-syncable
    /// settings row) must still advance the export watermark, otherwise every
    /// push re-scans the same range forever.
    #[tokio::test]
    async fn export_watermark_advances_past_filtered_rows() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-crdt-watermark-test-{}-{}",
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

        // `app_settings` embeds API keys and must never be synced.
        sqlx::query("INSERT INTO settings (key, value, updated_at) VALUES (?, ?, ?)")
            .bind("app_settings")
            .bind("{\"default_llm\":{\"api_key\":\"secret\"}}")
            .bind("2026-01-01T00:00:00Z")
            .execute(&db_a)
            .await?;

        let changes = export_changes_since(&db_a, 0).await?;
        assert!(
            changes.changes.is_empty(),
            "non-syncable settings row must be filtered out"
        );
        assert!(
            changes.to_db_version > 0,
            "watermark must still advance past filtered rows, got {}",
            changes.to_db_version
        );

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        db_a.close().await;
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

    #[test]
    fn local_ts_is_newer_compares_parsed_instants() {
        // Millis beats the same second's second-precision form.
        assert!(local_ts_is_newer("2026-06-16T10:30:00.500Z", "2026-06-16T10:30:00Z"));
        assert!(!local_ts_is_newer("2026-06-16T10:30:00Z", "2026-06-16T10:30:00.500Z"));
        // Equal timestamps are NOT "newer" — the change goes to the merge.
        assert!(!local_ts_is_newer("2026-06-16T10:30:00Z", "2026-06-16T10:30:00Z"));
        assert!(!local_ts_is_newer("2026-06-16T10:30:00.000Z", "2026-06-16T10:30:00Z"));
        // Ordinary ordering still holds across precisions.
        assert!(local_ts_is_newer("2026-06-16T10:30:01Z", "2026-06-16T10:30:00.999Z"));
        assert!(!local_ts_is_newer("2026-06-16T10:29:59Z", "2026-06-16T10:30:00Z"));
    }

    /// Equal `updated_at` on both sides must NOT drop the peer's change
    /// (the LWW pre-filter is strictly-greater): the merge falls through to
    /// cr-sqlite's col_version rule, which deterministically picks the side
    /// with more edits. Regression guard for the old `>=` comparison that
    /// silently discarded same-timestamp peer edits.
    #[tokio::test]
    async fn equal_updated_at_lets_col_version_merge_decide() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-lww-eq-test-{}-{}",
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

        let ts = "2026-01-01T00:00:00Z";
        sqlx::query(
            "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
             VALUES ('n1', 1, 'original', 'body', 'body', '[]', '[]', ?, ?)",
        )
        .bind(ts)
        .bind(ts)
        .execute(&db_a)
        .await?;
        let changes = export_changes_since(&db_a, 0).await?;
        apply_changes(&db_b, &changes).await?;

        // Both sides edit the title with the SAME updated_at. A edits twice
        // (title col_version 3), B once (col_version 2) — cr-sqlite's merge
        // must deterministically pick A.
        sqlx::query("UPDATE notes SET title = 'A-edit-1', updated_at = ? WHERE id = 'n1'")
            .bind(ts)
            .execute(&db_a)
            .await?;
        sqlx::query("UPDATE notes SET title = 'A-edit-2', updated_at = ? WHERE id = 'n1'")
            .bind(ts)
            .execute(&db_a)
            .await?;
        sqlx::query("UPDATE notes SET title = 'B-edit', updated_at = ? WHERE id = 'n1'")
            .bind(ts)
            .execute(&db_b)
            .await?;

        let changes = export_changes_since(&db_a, 0).await?;
        apply_changes(&db_b, &changes).await?;

        let title: (String,) =
            sqlx::query_as("SELECT title FROM notes WHERE id = 'n1'")
                .fetch_one(&db_b)
                .await?;
        assert_eq!(
            title.0, "A-edit-2",
            "same-timestamp peer change must reach the merge (higher col_version wins)"
        );

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// A legacy full snapshot (INSERT OR IGNORE statements from a
    /// pre-changeset peer) must not resurrect a row this device deleted, and
    /// the skipped row must not be recorded as a LOCAL change (which would
    /// re-broadcast the zombie to every peer).
    #[tokio::test]
    async fn legacy_snapshot_does_not_resurrect_deleted_row() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-snap-tombstone-test-{}-{}",
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

        let ts = "2026-01-01T00:00:00Z";
        for (id, title) in [("n1", "Note One"), ("n2", "Note Two")] {
            sqlx::query(
                "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
                 VALUES (?, 1, ?, 'body', 'body', '[]', '[]', ?, ?)",
            )
            .bind(id)
            .bind(title)
            .bind(ts)
            .bind(ts)
            .execute(&db_a)
            .await?;
        }
        // B receives both notes via the normal changeset path...
        let changes = export_changes_since(&db_a, 0).await?;
        apply_changes(&db_b, &changes).await?;
        // ...then deletes n1 (delete-wins tombstone with B's site_id).
        sqlx::query("DELETE FROM notes WHERE id = 'n1'")
            .execute(&db_b)
            .await?;
        let b_site: (Vec<u8>,) = sqlx::query_as("SELECT crsql_site_id()")
            .fetch_one(&db_b)
            .await?;
        let b_site_rows_before: (i64,) =
            sqlx::query_as("SELECT count(*) FROM crsql_changes WHERE site_id = ?")
                .bind(&b_site.0)
                .fetch_one(&db_b)
                .await?;

        // A (never saw the delete) ships a legacy full snapshot.
        let statements = export_full_snapshot(&db_a, &["notes"]).await?;
        assert!(
            statements.iter().any(|s| s.contains("n1")),
            "test snapshot must contain the deleted row"
        );
        apply_full_snapshot(&db_b, &statements).await?;

        let n1: (i64,) = sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'n1'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(n1.0, 0, "deleted row must not be resurrected by a snapshot");
        let n2: (i64,) = sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'n2'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(n2.0, 1, "live rows in the snapshot must still apply");
        let b_site_rows_after: (i64,) =
            sqlx::query_as("SELECT count(*) FROM crsql_changes WHERE site_id = ?")
                .bind(&b_site.0)
                .fetch_one(&db_b)
                .await?;
        assert_eq!(
            b_site_rows_after, b_site_rows_before,
            "applying the snapshot must not record local (B-site) changes"
        );

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }

    /// The full-history changeset (what replaced the legacy snapshot on the
    /// send side) must also respect a local delete: re-applying the sender's
    /// whole history goes through the cr-sqlite merge, where the tombstone's
    /// higher causal length wins.
    #[tokio::test]
    async fn full_changeset_does_not_resurrect_deleted_row() -> anyhow::Result<()> {
        let dir = std::env::temp_dir().join(format!(
            "siku-changeset-tombstone-test-{}-{}",
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

        let ts = "2026-01-01T00:00:00Z";
        for (id, title) in [("n1", "Note One"), ("n2", "Note Two")] {
            sqlx::query(
                "INSERT INTO notes (id, vault_id, title, content, content_plain, tags, aliases, created_at, updated_at) \
                 VALUES (?, 1, ?, 'body', 'body', '[]', '[]', ?, ?)",
            )
            .bind(id)
            .bind(title)
            .bind(ts)
            .bind(ts)
            .execute(&db_a)
            .await?;
        }
        let changes = export_changes_since(&db_a, 0).await?;
        apply_changes(&db_b, &changes).await?;
        sqlx::query("DELETE FROM notes WHERE id = 'n1'")
            .execute(&db_b)
            .await?;
        let b_site: (Vec<u8>,) = sqlx::query_as("SELECT crsql_site_id()")
            .fetch_one(&db_b)
            .await?;
        let b_site_rows_before: (i64,) =
            sqlx::query_as("SELECT count(*) FROM crsql_changes WHERE site_id = ?")
                .bind(&b_site.0)
                .fetch_one(&db_b)
                .await?;

        // A re-exports its FULL history (db_version 0 → now) — A never saw
        // the delete, so the changeset carries n1's insert again.
        let full = export_changes_since(&db_a, 0).await?;
        apply_changes(&db_b, &full).await?;

        let n1: (i64,) = sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'n1'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(n1.0, 0, "deleted row must not be resurrected by a full changeset");
        let n2: (i64,) = sqlx::query_as("SELECT count(*) FROM notes WHERE id = 'n2'")
            .fetch_one(&db_b)
            .await?;
        assert_eq!(n2.0, 1);
        let b_site_rows_after: (i64,) =
            sqlx::query_as("SELECT count(*) FROM crsql_changes WHERE site_id = ?")
                .bind(&b_site.0)
                .fetch_one(&db_b)
                .await?;
        assert_eq!(
            b_site_rows_after, b_site_rows_before,
            "re-applying the full history must not record local (B-site) changes"
        );

        sqlx::query("SELECT crsql_finalize()").execute(&db_a).await?;
        sqlx::query("SELECT crsql_finalize()").execute(&db_b).await?;
        db_a.close().await;
        db_b.close().await;
        Ok(())
    }
}
