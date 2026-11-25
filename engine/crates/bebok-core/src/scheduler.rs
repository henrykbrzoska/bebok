//! Minimal task scheduler: persist schedules in the `schedules` table of the
//! Bebok database, compute next-run times, and tick every 30 s to fire due tasks.

use chrono::{DateTime, Datelike, TimeDelta, Utc};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduledTask {
    pub id: String,
    pub name: String,
    /// `"interval_mins"` | `"daily"` | `"weekly"`.
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_mins: Option<u64>,
    /// Hour in UTC (0–23). Used by `daily` and `weekly`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hour: Option<u8>,
    /// Minute (0–59). Used by `daily` and `weekly`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub minute: Option<u8>,
    /// ISO weekday (1 = Mon … 7 = Sun). Used by `weekly`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weekday: Option<u8>,
    pub prompt: String,
    pub directory: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub last_status: String,
    pub next_run_at: String,
}

fn default_true() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Persistence (`schedules` table in the Bebok database, see `db.rs`)
// ---------------------------------------------------------------------------

const COLUMNS: &str = "id, name, kind, interval_mins, hour, minute, weekday, prompt, \
     directory, agent, enabled, last_run_at, last_status, next_run_at";

/// Pre-SQLite layout: one JSON file per task under `<data dir>/bebok/scheduled_tasks/`.
fn legacy_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("bebok")
        .join("scheduled_tasks")
}

fn open_store() -> Result<Connection, String> {
    let conn = crate::db::open_db()?;
    import_legacy_json(&conn)?;
    Ok(conn)
}

/// One-time import of the old JSON files. The folder is renamed afterwards,
/// so deleted schedules cannot come back on the next start.
fn import_legacy_json(conn: &Connection) -> Result<(), String> {
    let dir = legacy_dir();
    if !dir.is_dir() {
        return Ok(());
    }
    let mut tasks = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().and_then(|e| e.to_str()) == Some("json")
                && let Ok(bytes) = std::fs::read(&p)
                && let Ok(task) = serde_json::from_slice::<ScheduledTask>(&bytes)
            {
                tasks.push(task);
            }
        }
    }
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    for task in &tasks {
        let exists: bool = tx
            .query_row("SELECT 1 FROM schedules WHERE id = ?1", [&task.id], |_| {
                Ok(true)
            })
            .optional()
            .map_err(|e| e.to_string())?
            .unwrap_or(false);
        if !exists {
            write_task(&tx, task).map_err(|e| e.to_string())?;
        }
    }
    tx.commit().map_err(|e| e.to_string())?;
    let target = dir.with_file_name("scheduled_tasks.migrated");
    if let Err(e) = std::fs::rename(&dir, &target) {
        tracing::warn!("could not retire legacy scheduled_tasks folder: {e}");
    } else {
        tracing::info!(
            count = tasks.len(),
            "imported legacy scheduled tasks into SQLite"
        );
    }
    Ok(())
}

fn read_task(row: &Row<'_>) -> rusqlite::Result<ScheduledTask> {
    Ok(ScheduledTask {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        interval_mins: row
            .get::<_, Option<i64>>(3)?
            .and_then(|v| u64::try_from(v).ok()),
        hour: row
            .get::<_, Option<i64>>(4)?
            .and_then(|v| u8::try_from(v).ok()),
        minute: row
            .get::<_, Option<i64>>(5)?
            .and_then(|v| u8::try_from(v).ok()),
        weekday: row
            .get::<_, Option<i64>>(6)?
            .and_then(|v| u8::try_from(v).ok()),
        prompt: row.get(7)?,
        directory: row.get(8)?,
        agent: row.get(9)?,
        enabled: row.get::<_, i64>(10)? != 0,
        last_run_at: row.get(11)?,
        last_status: row.get(12)?,
        next_run_at: row.get(13)?,
    })
}

fn load_all_in(conn: &Connection) -> rusqlite::Result<Vec<ScheduledTask>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM schedules ORDER BY name, id"
    ))?;
    let rows = stmt.query_map([], read_task)?;
    rows.collect()
}

/// Upsert one task.
fn write_task(conn: &Connection, task: &ScheduledTask) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO schedules (id, name, kind, interval_mins, hour, minute, weekday,
             prompt, directory, agent, enabled, last_run_at, last_status, next_run_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(id) DO UPDATE SET
             name = excluded.name,
             kind = excluded.kind,
             interval_mins = excluded.interval_mins,
             hour = excluded.hour,
             minute = excluded.minute,
             weekday = excluded.weekday,
             prompt = excluded.prompt,
             directory = excluded.directory,
             agent = excluded.agent,
             enabled = excluded.enabled,
             last_run_at = excluded.last_run_at,
             last_status = excluded.last_status,
             next_run_at = excluded.next_run_at",
        params![
            task.id,
            task.name,
            task.kind,
            task.interval_mins.map(|v| v as i64),
            task.hour.map(i64::from),
            task.minute.map(i64::from),
            task.weekday.map(i64::from),
            task.prompt,
            task.directory,
            task.agent,
            task.enabled as i64,
            task.last_run_at,
            task.last_status,
            task.next_run_at,
        ],
    )?;
    Ok(())
}

pub fn load_all() -> Vec<ScheduledTask> {
    match open_store().and_then(|c| load_all_in(&c).map_err(|e| e.to_string())) {
        Ok(tasks) => tasks,
        Err(e) => {
            tracing::error!("loading scheduled tasks failed: {e}");
            Vec::new()
        }
    }
}

pub fn save(task: &ScheduledTask) -> Result<(), String> {
    let conn = open_store()?;
    write_task(&conn, task).map_err(|e| e.to_string())
}

pub fn delete(id: &str) -> Result<(), String> {
    let conn = open_store()?;
    conn.execute("DELETE FROM schedules WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Startup recovery: a task still marked `running` belongs to a process that
/// died mid-turn, so mark it `failed`. Returns the number of rows changed.
pub fn recover_interrupted() -> Result<usize, String> {
    let conn = open_store()?;
    conn.execute(
        "UPDATE schedules SET last_status = 'failed' WHERE last_status = 'running'",
        [],
    )
    .map_err(|e| e.to_string())
}

/// If `task.next_run_at` is not in the future (or cannot be parsed), move it
/// to the next occurrence after `now`. Returns `true` when it was changed.
/// Used when a disabled task is re-enabled so it does not fire immediately.
pub fn refresh_next_if_past(task: &mut ScheduledTask, now: DateTime<Utc>) -> bool {
    let past = match DateTime::parse_from_rfc3339(&task.next_run_at) {
        Ok(dt) => dt.with_timezone(&Utc) <= now,
        Err(_) => true,
    };
    if past {
        task.next_run_at = compute_next(task, now).to_rfc3339();
    }
    past
}

// ---------------------------------------------------------------------------
// Next-run computation
// ---------------------------------------------------------------------------

pub fn compute_next(task: &ScheduledTask, after: DateTime<Utc>) -> DateTime<Utc> {
    match task.kind.as_str() {
        "interval_mins" => {
            let mins = task.interval_mins.unwrap_or(60);
            after + TimeDelta::minutes(mins as i64)
        }
        "daily" => {
            let h = task.hour.unwrap_or(0) as u32;
            let m = task.minute.unwrap_or(0) as u32;
            let candidate = after.date_naive().and_hms_opt(h, m, 0).unwrap();
            let candidate_utc: DateTime<Utc> = DateTime::from_naive_utc_and_offset(candidate, Utc);
            if candidate_utc <= after {
                candidate_utc + TimeDelta::days(1)
            } else {
                candidate_utc
            }
        }
        "weekly" => {
            let h = task.hour.unwrap_or(0) as u32;
            let m = task.minute.unwrap_or(0) as u32;
            let target_wd = task.weekday.unwrap_or(1); // 1 = Mon
            let after_naive = after.date_naive();
            let after_wd = after_naive.weekday().num_days_from_monday() + 1; // 1-based
            let mut days_ahead = (target_wd as i32 - after_wd as i32).rem_euclid(7);
            if days_ahead == 0 {
                // Same weekday: check if the time is still ahead today.
                let today_candidate = after_naive.and_hms_opt(h, m, 0).unwrap();
                let today_utc: DateTime<Utc> =
                    DateTime::from_naive_utc_and_offset(today_candidate, Utc);
                if today_utc <= after {
                    days_ahead = 7;
                } else {
                    return today_utc;
                }
            }
            let candidate =
                after_naive.and_hms_opt(h, m, 0).unwrap() + TimeDelta::days(days_ahead as i64);
            DateTime::from_naive_utc_and_offset(candidate, Utc)
        }
        _ => after + TimeDelta::hours(1), // fallback
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Timelike;

    fn sample(id: &str) -> ScheduledTask {
        ScheduledTask {
            id: id.into(),
            name: format!("task {id}"),
            kind: "interval_mins".into(),
            interval_mins: Some(15),
            hour: None,
            minute: None,
            weekday: None,
            prompt: "run".into(),
            directory: "/tmp/p".into(),
            agent: Some("code".into()),
            enabled: true,
            last_run_at: None,
            last_status: "pending".into(),
            next_run_at: "2000-01-01T00:00:00+00:00".into(),
        }
    }

    #[test]
    fn save_load_delete_roundtrip() {
        let conn = crate::db::open_memory().unwrap();
        write_task(&conn, &sample("a")).unwrap();
        let mut b = sample("b");
        b.enabled = false;
        b.hour = Some(9);
        write_task(&conn, &b).unwrap();

        let all = load_all_in(&conn).unwrap();
        assert_eq!(all.len(), 2);
        let got_b = all.iter().find(|t| t.id == "b").unwrap();
        assert!(!got_b.enabled);
        assert_eq!(got_b.hour, Some(9));
        assert_eq!(got_b.interval_mins, Some(15));

        // upsert, not duplicate
        b.name = "renamed".into();
        write_task(&conn, &b).unwrap();
        assert_eq!(load_all_in(&conn).unwrap().len(), 2);

        conn.execute("DELETE FROM schedules WHERE id = ?1", ["a"])
            .unwrap();
        assert_eq!(load_all_in(&conn).unwrap().len(), 1);
    }

    #[test]
    fn refresh_moves_past_next_run_forward() {
        let now = Utc::now();
        let mut t = sample("x");
        assert!(refresh_next_if_past(&mut t, now));
        let next = DateTime::parse_from_rfc3339(&t.next_run_at).unwrap();
        assert_eq!(next.with_timezone(&Utc), now + TimeDelta::minutes(15));

        // future next_run_at is left alone
        let future = (now + TimeDelta::hours(3)).to_rfc3339();
        t.next_run_at = future.clone();
        assert!(!refresh_next_if_past(&mut t, now));
        assert_eq!(t.next_run_at, future);
    }

    #[test]
    fn daily_next_run_is_tomorrow_when_hour_passed() {
        // Now is 14:00 UTC; daily at 10:00 UTC → next = tomorrow 10:00.
        let now = Utc::now();
        let today_10 = now.date_naive().and_hms_opt(10, 0, 0).unwrap();
        let today_10_utc: DateTime<Utc> = DateTime::from_naive_utc_and_offset(today_10, Utc);

        let task = ScheduledTask {
            id: "test".into(),
            name: "test".into(),
            kind: "daily".into(),
            interval_mins: None,
            hour: Some(10),
            minute: Some(0),
            weekday: None,
            prompt: "hello".into(),
            directory: "/tmp".into(),
            agent: None,
            enabled: true,
            last_run_at: None,
            last_status: String::new(),
            next_run_at: today_10_utc.to_rfc3339(),
        };

        let next = compute_next(&task, now);
        // If 10:00 today already passed, next must be tomorrow.
        if now > today_10_utc {
            assert_eq!(next.date_naive(), (now + TimeDelta::days(1)).date_naive());
            assert_eq!(next.hour(), 10);
            assert_eq!(next.minute(), 0);
        }
    }
}
