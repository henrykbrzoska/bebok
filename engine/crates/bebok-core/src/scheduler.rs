//! Minimal task scheduler: persist schedules on disk, compute next-run times,
//! and tick every 30 s to fire due tasks.

use chrono::{DateTime, Datelike, TimeDelta, Utc};
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
// Persistence (JSON per task in data dir)
// ---------------------------------------------------------------------------

fn data_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("bebok")
        .join("scheduled_tasks")
}

fn task_path(id: &str) -> PathBuf {
    data_dir().join(format!("{id}.json"))
}

pub fn load_all() -> Vec<ScheduledTask> {
    let dir = data_dir();
    if !dir.is_dir() {
        return Vec::new();
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
    tasks
}

pub fn save(task: &ScheduledTask) -> Result<(), String> {
    let dir = data_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let bytes = serde_json::to_vec_pretty(task).map_err(|e| e.to_string())?;
    std::fs::write(task_path(&task.id), bytes).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn delete(id: &str) -> Result<(), String> {
    let p = task_path(id);
    if p.exists() {
        std::fs::remove_file(p).map_err(|e| e.to_string())?;
    }
    Ok(())
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
