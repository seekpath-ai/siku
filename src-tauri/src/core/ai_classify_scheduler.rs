//! Scheduler for unattended AI classification rules.
//!
//! Deliberately a separate loop from [`crate::core::cron_scheduler`]: that one
//! fires *agent turns* (it needs a chat session and a prompt), while a
//! classification rule runs the library pipeline directly. Both share the cron
//! expression matcher, so the schedule syntax is identical.
//!
//! Firing rules do not hold the pipeline's `RunGuard` themselves — the inner
//! `run_draft` does — so a rule that fires while the user is mid-classification
//! simply logs and skips instead of interleaving two runs.

use std::collections::HashMap;

use chrono::Local;
use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};

/// How often enabled rules are checked. Minimum cron granularity is one minute,
/// so a 30s tick bounds the firing delay without busy-looping.
const TICK_SECS: u64 = 30;

pub async fn run(
    db: SqlitePool,
    app: AppHandle,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(TICK_SECS));
    // rule id -> minute key of the last fire, so a rule fires once per minute.
    let mut fired: HashMap<String, String> = HashMap::new();

    loop {
        tokio::select! {
            _ = tick.tick() => {},
            _ = shutdown.recv() => {
                tracing::info!("ai-classify rule scheduler received shutdown signal");
                break;
            }
        }

        let rules = match crate::core::ai_classify::rules_due(&db).await {
            Ok(r) => r,
            Err(e) => {
                tracing::error!(error = %e, "ai-classify rule lookup failed");
                continue;
            }
        };
        if rules.is_empty() {
            continue;
        }

        let now = Local::now();
        let minute_key = now.format("%Y-%m-%d %H:%M").to_string();
        for rule in rules {
            if !crate::core::cron_scheduler::matches(&now, &rule.cron) {
                continue;
            }
            if fired.get(&rule.id).as_deref() == Some(&minute_key) {
                continue;
            }
            fired.insert(rule.id.clone(), minute_key.clone());
            tracing::info!(rule_id = %rule.id, name = %rule.name, "ai-classify rule firing");

            let app2 = app.clone();
            let db2 = db.clone();
            let rule_id = rule.id.clone();
            tokio::spawn(async move {
                crate::core::ai_classify::run_rule(app2.clone(), db2, rule_id.clone()).await;

                // Tell the user what the unattended run did (or why it did
                // nothing) — a silent background writer is not acceptable.
                let result: Option<String> = sqlx::query_scalar(
                    "SELECT last_result FROM ai_classify_rules WHERE id = ?",
                )
                .bind(&rule_id)
                .fetch_optional(&app2.state::<crate::AppState>().db)
                .await
                .ok()
                .flatten();
                use tauri_plugin_notification::NotificationExt;
                let body = result.unwrap_or_else(|| "自动分类已执行".to_string());
                if let Err(e) = app2
                    .notification()
                    .builder()
                    .title("AI 自动分类")
                    .body(body)
                    .show()
                {
                    tracing::warn!(error = %e, "rule notification failed");
                }
            });
        }

        // Keep the de-dup map from growing without bound.
        if fired.len() > 256 {
            fired.clear();
        }
    }
}
