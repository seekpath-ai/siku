use std::collections::{BTreeSet, HashMap};
use chrono::{Datelike, Local, Timelike};
use sqlx::SqlitePool;
use tauri::{AppHandle, Manager};

use crate::core::models::CronJob;

/// Parse a cron field into the set of matching values.
fn parse_field(field: &str, min: u32, max: u32) -> Result<Vec<u32>, String> {
    let mut set = BTreeSet::new();
    for part in field.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        if part == "*" {
            for v in min..=max {
                set.insert(v);
            }
            continue;
        }
        if let Some((base, step)) = part.split_once('/') {
            let step: u32 = step.parse().map_err(|_| format!("bad step: {step}"))?;
            if step == 0 {
                return Err("step cannot be 0".to_string());
            }
            let (lo, hi) = if base == "*" {
                (min, max)
            } else if let Some((a, b)) = base.split_once('-') {
                (
                    a.parse().map_err(|_| format!("bad range: {base}"))?,
                    b.parse().map_err(|_| format!("bad range: {base}"))?,
                )
            } else {
                let v: u32 = base.parse().map_err(|_| format!("bad value: {base}"))?;
                (v, v)
            };
            let mut v = lo;
            while v <= hi {
                set.insert(v);
                v += step;
            }
            continue;
        }
        if let Some((a, b)) = part.split_once('-') {
            let lo: u32 = a.parse().map_err(|_| format!("bad range: {part}"))?;
            let hi: u32 = b.parse().map_err(|_| format!("bad range: {part}"))?;
            for v in lo..=hi {
                set.insert(v);
            }
            continue;
        }
        let v: u32 = part.parse().map_err(|_| format!("bad value: {part}"))?;
        set.insert(v);
    }
    Ok(set.into_iter().collect())
}

/// Parse a 5-field cron expression (minute hour day-of-month month day-of-week).
fn parse_cron(
    expr: &str,
) -> Result<(Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>), String> {
    let parts: Vec<&str> = expr.split_whitespace().collect();
    if parts.len() != 5 {
        return Err("cron must have 5 fields: minute hour day-of-month month day-of-week".to_string());
    }
    Ok((
        parse_field(parts[0], 0, 59)?,
        parse_field(parts[1], 0, 23)?,
        parse_field(parts[2], 1, 31)?,
        parse_field(parts[3], 1, 12)?,
        parse_field(parts[4], 0, 7)?,
    ))
}

/// Validate a cron expression (5 fields, supported syntax).
pub fn validate(expr: &str) -> Result<(), String> {
    parse_cron(expr).map(|_| ())
}

fn matches(now: &chrono::DateTime<Local>, expr: &str) -> bool {
    let Ok((mins, hours, doms, months, dows)) = parse_cron(expr) else {
        return false;
    };
    let mut dow = now.weekday().num_days_from_sunday();
    if dow == 7 {
        dow = 0;
    }
    mins.contains(&(now.minute() as u32))
        && hours.contains(&(now.hour() as u32))
        && doms.contains(&(now.day() as u32))
        && months.contains(&(now.month() as u32))
        && dows.contains(&(dow as u32))
}

/// Scheduler loop: every 20s, fire due cron jobs as agent turns.
pub async fn run(
    db: SqlitePool,
    app: AppHandle,
    mut shutdown: tokio::sync::broadcast::Receiver<()>,
) {
    let mut tick = tokio::time::interval(std::time::Duration::from_secs(20));
    // job id -> last fired minute key, to avoid duplicate fires within a minute.
    let mut fired: HashMap<String, String> = HashMap::new();

    loop {
        tokio::select! {
            _ = tick.tick() => {},
            _ = shutdown.recv() => {
                tracing::info!("cron scheduler received shutdown signal");
                break;
            }
        }

        let jobs: Vec<CronJob> = match sqlx::query_as::<_, CronJob>(
            "SELECT id, session_id, cron, prompt, recurring, created_at, updated_at FROM cron_jobs",
        )
        .fetch_all(&db)
        .await
        {
            Ok(j) => j,
            Err(e) => {
                tracing::error!(error = %e, "cron scheduler db error");
                continue;
            }
        };

        let now = Local::now();
        let minute_key = now.format("%Y-%m-%d %H:%M").to_string();

        for job in jobs {
            if !matches(&now, &job.cron) {
                continue;
            }
            if fired.get(&job.id).as_deref() == Some(&minute_key) {
                continue;
            }
            fired.insert(job.id.clone(), minute_key.clone());

            let app2 = app.clone();
            let sid = job.session_id.clone();
            let prompt = job.prompt.clone();
            tokio::spawn(async move {
                let state = app2.state::<crate::AppState>();
                let msg = format!("⏰ 定时任务：{prompt}");
                if let Err(e) =
                    crate::commands::agent::run_agent_turn(&state, &app2, sid.clone(), msg, None).await
                {
                    tracing::error!(session_id = %sid, error = %e, "cron fire failed");
                }
            });

            if !job.recurring {
                let _ = sqlx::query("DELETE FROM cron_jobs WHERE id = ?")
                    .bind(&job.id)
                    .execute(&db)
                    .await;
            }
        }
    }
}
