//! Usage statistics (F7-5): digest persisted sessions once, aggregate per
//! request.
//!
//! Two stages so the expensive part can be cached by the server:
//!
//! 1. [`scan`] walks `<data>/instances/*/sessions/*` once and reduces every
//!    session to a small [`SessionDigest`] (timestamps, per-call token
//!    counts, tool names, compaction markers). Message bodies (text, images,
//!    tool output) are dropped immediately, so the digest of a whole data
//!    directory stays tiny compared to the transcripts.
//! 2. [`aggregate`] applies the request filter (project directory, time
//!    range) to a digest list and produces the [`Stats`] payload: totals,
//!    breakdowns (model / provider / agent / project / day), top sessions,
//!    tool usage and the average context size at compaction.
//!
//! Everything here is pure and synchronous; no dates crate is pulled in
//! (UTC civil-date arithmetic is a few lines, see `days_from_civil`).

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::Serialize;
use uuid::Uuid;

use crate::session::persist;
use crate::session::{Message, Part, Role, Session, ToolState};
use crate::util::normalize_path;

/// Prefix of the compaction marker appended by `POST /session/{id}/compact`
/// (`[Context compacted: from X to Y tokens (...)]`).
const COMPACTION_MARKER_PREFIX: &str = "[Context compacted: from ";

/// Number of days covered by `by_day`.
pub const DAYS_WINDOW: usize = 30;

/// Number of sessions listed in `top_sessions`.
pub const TOP_SESSIONS: usize = 10;

const DAY_MS: i64 = 86_400_000;

// ---------------------------------------------------------------------------
// Digest
// ---------------------------------------------------------------------------

/// One LLM call (one `Usage` part).
#[derive(Debug, Clone, PartialEq)]
pub struct CallDigest {
    /// `created_at` of the assistant message carrying the usage part.
    pub at: i64,
    /// Full model id as run (`provider/model`).
    pub model: String,
    /// Agent preset that produced the message (message meta, else session).
    pub agent: String,
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    /// `None` when the model had no pricing (cost "unknown").
    pub cost: Option<f64>,
}

/// One tool invocation (one `Tool` part).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDigest {
    pub at: i64,
    pub name: String,
    pub errored: bool,
}

/// Everything the stats need to know about one session.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionDigest {
    pub id: Uuid,
    pub directory: String,
    pub agent: String,
    pub title: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
    /// `created_at` of every user prompt (summary/marker notes excluded).
    pub prompts: Vec<i64>,
    pub calls: Vec<CallDigest>,
    pub tools: Vec<ToolDigest>,
    /// `(at, context_before)` for every compaction marker in the transcript.
    pub compactions: Vec<(i64, u64)>,
}

/// Reduce a session + transcript to its digest.
pub fn digest_session(session: &Session, messages: &[Message]) -> SessionDigest {
    let mut d = SessionDigest {
        id: session.id,
        directory: session.directory.clone(),
        agent: session.agent.clone(),
        title: session.title.clone().or_else(|| session.alias.clone()),
        created_at: session.created_at,
        updated_at: session.updated_at,
        prompts: Vec::new(),
        calls: Vec::new(),
        tools: Vec::new(),
        compactions: Vec::new(),
    };
    for m in messages {
        let at = if m.meta.created_at > 0 {
            m.meta.created_at
        } else {
            session.created_at
        };
        match m.role {
            Role::User => {
                let text = m.text_content();
                if let Some(before) = parse_compaction_marker(&text) {
                    d.compactions.push((at, before));
                } else if !text.starts_with("[summary of messages") {
                    d.prompts.push(at);
                }
            }
            Role::Assistant => {
                let agent = m
                    .meta
                    .agent
                    .clone()
                    .unwrap_or_else(|| session.agent.clone());
                let model = m
                    .meta
                    .model
                    .clone()
                    .or_else(|| session.model.clone())
                    .unwrap_or_else(|| "unknown".to_string());
                for p in &m.parts {
                    match p {
                        Part::Usage {
                            input_tokens,
                            output_tokens,
                            cost,
                            cache_read_input_tokens,
                            cache_creation_input_tokens,
                        } => d.calls.push(CallDigest {
                            at,
                            model: model.clone(),
                            agent: agent.clone(),
                            input: *input_tokens,
                            output: *output_tokens,
                            cache_read: cache_read_input_tokens.unwrap_or(0),
                            cache_write: cache_creation_input_tokens.unwrap_or(0),
                            cost: *cost,
                        }),
                        Part::Tool { name, state, .. } => d.tools.push(ToolDigest {
                            at,
                            name: name.clone(),
                            errored: matches!(state, ToolState::Error { .. }),
                        }),
                        _ => {}
                    }
                }
            }
        }
    }
    d
}

/// Extract the "from" figure of a compaction marker.
fn parse_compaction_marker(text: &str) -> Option<u64> {
    let rest = text.trim_start().strip_prefix(COMPACTION_MARKER_PREFIX)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// Walk the data directory once and digest every persisted session.
///
/// Same layout as `persist::repair`: `<root>/instances/<hash>/sessions/<id>/`
/// with `session.json` + `msg-NNNNNN.json`. Unreadable sessions or messages
/// are skipped (a crash mid-write never breaks the stats page).
pub fn scan(root: &Path) -> Vec<SessionDigest> {
    let mut out = Vec::new();
    let Ok(instances) = std::fs::read_dir(root.join("instances")) else {
        return out;
    };
    for inst in instances.flatten() {
        let Ok(sessions) = std::fs::read_dir(persist::sessions_dir(&inst.path())) else {
            continue;
        };
        for entry in sessions.flatten() {
            let dir = entry.path();
            if !dir.is_dir() {
                continue;
            }
            let Some(session) = std::fs::read_to_string(persist::session_meta_path(&dir))
                .ok()
                .and_then(|t| serde_json::from_str::<Session>(&t).ok())
            else {
                continue;
            };
            let mut digest = digest_session(&session, &load_transcript(&dir));
            // Older sessions persisted a canonicalised (`\\?\C:\...`)
            // directory; group them with the plain spelling every other code
            // path uses, so one project is one row and `?directory=` (which
            // is normalised the same way) matches them (E2E B5).
            digest.directory = normalize_path(Path::new(&digest.directory));
            out.push(digest);
        }
    }
    out
}

/// Load `msg-*.json` files of one session in index order.
fn load_transcript(dir: &Path) -> Vec<Message> {
    let Ok(files) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut paths: Vec<_> = files
        .flatten()
        .map(|f| f.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("msg-") && n.ends_with(".json"))
        })
        .collect();
    paths.sort();
    paths
        .iter()
        .filter_map(|p| persist::load_message(p))
        .collect()
}

// ---------------------------------------------------------------------------
// Filter + aggregate
// ---------------------------------------------------------------------------

/// Request filter. `directory` must already be normalized
/// (`util::normalize_path`); bounds are inclusive epoch milliseconds.
#[derive(Debug, Clone, Default)]
pub struct StatsFilter {
    pub directory: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
}

impl StatsFilter {
    fn in_range(&self, at: i64) -> bool {
        self.from.is_none_or(|f| at >= f) && self.to.is_none_or(|t| at <= t)
    }

    fn matches_directory(&self, directory: &str) -> bool {
        match &self.directory {
            None => true,
            Some(dir) => {
                directory == dir || crate::git::is_worktree_of(Path::new(directory), Path::new(dir))
            }
        }
    }
}

/// Token/cost totals shared by every breakdown row.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Totals {
    pub sessions: usize,
    /// User prompts.
    pub turns: usize,
    /// LLM round-trips (usage parts).
    pub llm_calls: usize,
    pub tool_calls: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    /// Sum of the calls with known pricing; `None` when no call had a price.
    pub cost: Option<f64>,
    /// Calls whose model had no pricing entry.
    pub cost_unknown_calls: usize,
}

impl Totals {
    fn add_call(&mut self, c: &CallDigest) {
        self.llm_calls += 1;
        self.input_tokens += c.input;
        self.output_tokens += c.output;
        self.cache_read_tokens += c.cache_read;
        self.cache_write_tokens += c.cache_write;
        match c.cost {
            Some(v) => self.cost = Some(self.cost.unwrap_or(0.0) + v),
            None => self.cost_unknown_calls += 1,
        }
    }

    /// All tokens the provider touched (sort key for "top by tokens").
    pub fn total_tokens(&self) -> u64 {
        self.input_tokens + self.output_tokens + self.cache_read_tokens + self.cache_write_tokens
    }
}

/// One breakdown row (model / provider / agent / project).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Bucket {
    pub key: String,
    #[serde(flatten)]
    pub totals: Totals,
}

/// One day of the `by_day` series (UTC).
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DayBucket {
    /// `YYYY-MM-DD`.
    pub day: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub cost: Option<f64>,
    pub llm_calls: usize,
    pub tool_calls: usize,
}

/// One row of `top_sessions`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub title: Option<String>,
    pub directory: String,
    pub agent: String,
    pub updated_at: i64,
    #[serde(flatten)]
    pub totals: Totals,
}

/// One row of `tools`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolRow {
    pub name: String,
    pub calls: usize,
    pub errors: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Compaction {
    pub count: usize,
    /// Mean context size (tokens) right before compaction; `None` if none.
    pub avg_context_before: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RangeInfo {
    pub directory: Option<String>,
    pub from: Option<i64>,
    pub to: Option<i64>,
    /// Number of digested sessions before filtering (all projects).
    pub scanned_sessions: usize,
}

/// `GET /stats` payload.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Stats {
    pub range: RangeInfo,
    pub totals: Totals,
    pub by_model: Vec<Bucket>,
    pub by_provider: Vec<Bucket>,
    pub by_agent: Vec<Bucket>,
    pub by_project: Vec<Bucket>,
    pub by_day: Vec<DayBucket>,
    pub top_sessions: Vec<SessionRow>,
    pub tools: Vec<ToolRow>,
    pub compaction: Compaction,
}

/// Provider part of a `provider/model` id (`unknown` when absent).
pub fn provider_of(model: &str) -> &str {
    match model.split_once('/') {
        Some((p, _)) if !p.is_empty() => p,
        _ => "unknown",
    }
}

/// Accumulates keyed buckets while remembering which sessions touched them.
#[derive(Default)]
struct Grouped {
    rows: HashMap<String, (Totals, HashSet<Uuid>)>,
}

impl Grouped {
    fn entry(&mut self, key: &str, session: Uuid) -> &mut Totals {
        let (totals, ids) = self.rows.entry(key.to_string()).or_default();
        if ids.insert(session) {
            totals.sessions += 1;
        }
        totals
    }

    fn finish(self) -> Vec<Bucket> {
        let mut rows: Vec<Bucket> = self
            .rows
            .into_iter()
            .map(|(key, (totals, _))| Bucket { key, totals })
            .collect();
        rows.sort_by(|a, b| {
            b.totals
                .total_tokens()
                .cmp(&a.totals.total_tokens())
                .then_with(|| a.key.cmp(&b.key))
        });
        rows
    }
}

/// Aggregate digests under a filter. `now_ms` anchors the 30-day series
/// when the filter has no upper bound.
pub fn aggregate(digests: &[SessionDigest], filter: &StatsFilter, now_ms: i64) -> Stats {
    let mut totals = Totals::default();
    let mut by_model = Grouped::default();
    let mut by_provider = Grouped::default();
    let mut by_agent = Grouped::default();
    let mut by_project = Grouped::default();
    let mut tools: HashMap<String, ToolRow> = HashMap::new();
    let mut top: Vec<SessionRow> = Vec::new();
    let mut compactions: Vec<u64> = Vec::new();

    // Day series: DAYS_WINDOW days ending on the day of `to` (or today).
    let end_day = day_index(filter.to.unwrap_or(now_ms));
    let start_day = end_day - (DAYS_WINDOW as i64 - 1);
    let mut days: Vec<DayBucket> = (0..DAYS_WINDOW as i64)
        .map(|i| DayBucket {
            day: format_day(start_day + i),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cost: None,
            llm_calls: 0,
            tool_calls: 0,
        })
        .collect();

    for s in digests {
        if !filter.matches_directory(&s.directory) {
            continue;
        }
        let prompts = s.prompts.iter().filter(|&&at| filter.in_range(at)).count();
        let calls: Vec<&CallDigest> = s.calls.iter().filter(|c| filter.in_range(c.at)).collect();
        let tool_calls: Vec<&ToolDigest> =
            s.tools.iter().filter(|t| filter.in_range(t.at)).collect();
        let comps: Vec<u64> = s
            .compactions
            .iter()
            .filter(|(at, _)| filter.in_range(*at))
            .map(|(_, before)| *before)
            .collect();
        let active = filter.in_range(s.created_at)
            || prompts > 0
            || !calls.is_empty()
            || !tool_calls.is_empty()
            || !comps.is_empty();
        if !active {
            continue;
        }

        totals.sessions += 1;
        totals.turns += prompts;
        totals.tool_calls += tool_calls.len();
        by_project.entry(&s.directory, s.id).turns += prompts;
        by_project.entry(&s.directory, s.id).tool_calls += tool_calls.len();
        by_agent.entry(&s.agent, s.id).turns += prompts;

        let mut session_totals = Totals {
            sessions: 1,
            turns: prompts,
            tool_calls: tool_calls.len(),
            ..Totals::default()
        };
        for c in &calls {
            totals.add_call(c);
            session_totals.add_call(c);
            by_model.entry(&c.model, s.id).add_call(c);
            by_provider.entry(provider_of(&c.model), s.id).add_call(c);
            by_agent.entry(&c.agent, s.id).add_call(c);
            by_project.entry(&s.directory, s.id).add_call(c);
            if let Some(d) = day_slot(&mut days, start_day, c.at) {
                d.llm_calls += 1;
                d.input_tokens += c.input;
                d.output_tokens += c.output;
                d.cache_read_tokens += c.cache_read;
                d.cache_write_tokens += c.cache_write;
                if let Some(v) = c.cost {
                    d.cost = Some(d.cost.unwrap_or(0.0) + v);
                }
            }
        }
        for t in &tool_calls {
            let row = tools.entry(t.name.clone()).or_insert_with(|| ToolRow {
                name: t.name.clone(),
                calls: 0,
                errors: 0,
            });
            row.calls += 1;
            if t.errored {
                row.errors += 1;
            }
            if let Some(d) = day_slot(&mut days, start_day, t.at) {
                d.tool_calls += 1;
            }
        }
        compactions.extend(comps);
        top.push(SessionRow {
            id: s.id.to_string(),
            title: s.title.clone(),
            directory: s.directory.clone(),
            agent: s.agent.clone(),
            updated_at: s.updated_at,
            totals: session_totals,
        });
    }

    top.sort_by(|a, b| {
        b.totals
            .total_tokens()
            .cmp(&a.totals.total_tokens())
            .then_with(|| b.updated_at.cmp(&a.updated_at))
    });
    top.truncate(TOP_SESSIONS);

    let mut tool_rows: Vec<ToolRow> = tools.into_values().collect();
    tool_rows.sort_by(|a, b| b.calls.cmp(&a.calls).then_with(|| a.name.cmp(&b.name)));

    let compaction = Compaction {
        count: compactions.len(),
        avg_context_before: if compactions.is_empty() {
            None
        } else {
            Some(compactions.iter().sum::<u64>() as f64 / compactions.len() as f64)
        },
    };

    Stats {
        range: RangeInfo {
            directory: filter.directory.clone(),
            from: filter.from,
            to: filter.to,
            scanned_sessions: digests.len(),
        },
        totals,
        by_model: by_model.finish(),
        by_provider: by_provider.finish(),
        by_agent: by_agent.finish(),
        by_project: by_project.finish(),
        by_day: days,
        top_sessions: top,
        tools: tool_rows,
        compaction,
    }
}

// ---------------------------------------------------------------------------
// UTC civil-date helpers (no chrono)
// ---------------------------------------------------------------------------

/// Days since the Unix epoch for a UTC timestamp in ms (floor division).
fn day_index(ms: i64) -> i64 {
    ms.div_euclid(DAY_MS)
}

/// The `by_day` bucket for a timestamp, if it falls inside the window.
fn day_slot(days: &mut [DayBucket], start_day: i64, at: i64) -> Option<&mut DayBucket> {
    let idx = day_index(at) - start_day;
    if (0..DAYS_WINDOW as i64).contains(&idx) {
        days.get_mut(idx as usize)
    } else {
        None
    }
}

/// `YYYY-MM-DD` of a day index.
fn format_day(days: i64) -> String {
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Howard Hinnant's `civil_from_days` (proleptic Gregorian, UTC).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Inverse of `civil_from_days`.
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parse a query timestamp into epoch milliseconds.
///
/// Accepts plain epoch milliseconds, `YYYY-MM-DD` (start of that UTC day) and
/// RFC 3339 `YYYY-MM-DDTHH:MM[:SS[.fff]][Z|±HH:MM]`. Returns `None` for
/// anything else.
pub fn parse_timestamp(text: &str) -> Option<i64> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().all(|c| c.is_ascii_digit()) {
        return text.parse().ok();
    }
    let (date, rest) = match text.split_once(['T', ' ']) {
        Some((d, r)) => (d, Some(r)),
        None => (text, None),
    };
    let mut parts = date.splitn(3, '-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let mut ms = days_from_civil(y, m, d) * DAY_MS;
    let Some(rest) = rest else {
        return Some(ms);
    };
    // Split the zone suffix off the time.
    let (time, offset_ms) = if let Some(t) = rest.strip_suffix('Z') {
        (t, 0)
    } else if let Some(pos) = rest.rfind(['+', '-']) {
        let (t, z) = rest.split_at(pos);
        let sign = if z.starts_with('-') { -1 } else { 1 };
        let (zh, zm) = z[1..].split_once(':').unwrap_or((&z[1..], "0"));
        let zh: i64 = zh.parse().ok()?;
        let zm: i64 = zm.parse().ok()?;
        (t, sign * (zh * 3_600_000 + zm * 60_000))
    } else {
        (rest, 0)
    };
    let mut hms = time.splitn(3, ':');
    let h: i64 = hms.next()?.parse().ok()?;
    let mi: i64 = hms.next()?.parse().ok()?;
    let s: f64 = hms
        .next()
        .map(|s| s.parse())
        .transpose()
        .ok()?
        .unwrap_or(0.0);
    if !(0..24).contains(&h) || !(0..60).contains(&mi) || !(0.0..61.0).contains(&s) {
        return None;
    }
    ms += h * 3_600_000 + mi * 60_000 + (s * 1000.0) as i64 - offset_ms;
    Some(ms)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::UsageTotals;

    const DAY: i64 = DAY_MS;
    /// 2026-09-10T00:00:00Z as a fixed "now" for deterministic day buckets.
    const NOW: i64 = 1_788_998_400_000;

    fn user_at(text: &str, at: i64) -> Message {
        let mut m = Message::user(text);
        m.meta.created_at = at;
        m
    }

    fn call_at(
        agent: &str,
        model: &str,
        at: i64,
        input: u64,
        output: u64,
        cache_read: Option<u64>,
        cost: Option<f64>,
        tools: &[(&str, bool)],
    ) -> Message {
        let mut m = Message::assistant_with(agent, model);
        m.meta.created_at = at;
        for (i, (name, errored)) in tools.iter().enumerate() {
            m.add_tool_call(format!("t{i}"), name.to_string(), serde_json::json!({}));
            if *errored {
                m.mark_tool_error(&format!("t{i}"), "boom".to_string());
            } else {
                m.mark_tool_completed(&format!("t{i}"), "ok".into(), "ok".into(), None);
            }
        }
        m.set_usage(UsageTotals {
            input_tokens: input,
            output_tokens: output,
            cost,
            cache_read_input_tokens: cache_read,
            cache_creation_input_tokens: None,
        });
        m
    }

    fn session(dir: &str, agent: &str, created_at: i64) -> Session {
        let mut s = Session::new(dir, agent);
        s.created_at = created_at;
        s.updated_at = created_at;
        s.title = Some(format!("{agent} in {dir}"));
        s
    }

    /// Three sessions across two projects, two providers, one compaction.
    fn fixtures() -> Vec<SessionDigest> {
        // Session A: today, project P1, anthropic + a tool error.
        let a = session("/p1", "code", NOW - DAY / 2);
        let a_msgs = vec![
            user_at("hello", NOW - DAY / 2),
            call_at(
                "code",
                "anthropic/claude-sonnet",
                NOW - DAY / 2 + 1000,
                1000,
                200,
                Some(500),
                Some(0.01),
                &[("read", false), ("bash", true)],
            ),
            user_at("more", NOW - DAY / 2 + 2000),
            call_at(
                "code",
                "anthropic/claude-sonnet",
                NOW - DAY / 2 + 3000,
                2000,
                300,
                Some(1500),
                Some(0.02),
                &[("read", false)],
            ),
        ];
        // Session B: 10 days ago, project P2, openai with unknown pricing,
        // agent override on the message, compaction marker.
        let b = session("/p2", "plan", NOW - 10 * DAY);
        let b_msgs = vec![
            user_at("[summary of messages 0..3]", NOW - 10 * DAY),
            user_at("go", NOW - 10 * DAY + 1000),
            call_at(
                "reviewer",
                "openai/gpt-x",
                NOW - 10 * DAY + 2000,
                4000,
                100,
                None,
                None,
                &[("grep", false)],
            ),
            user_at(
                "[Context compacted: from 120000 to 30000 tokens (12 earlier messages summarized)]",
                NOW - 10 * DAY + 3000,
            ),
        ];
        // Session C: 45 days ago, project P1, outside the day window.
        let c = session("/p1", "code", NOW - 45 * DAY);
        let c_msgs = vec![
            user_at("old", NOW - 45 * DAY),
            call_at(
                "code",
                "openai/gpt-x",
                NOW - 45 * DAY + 1000,
                10,
                5,
                None,
                Some(0.001),
                &[],
            ),
            user_at(
                "[Context compacted: from 80000 to 20000 tokens (4 earlier messages summarized)]",
                NOW - 45 * DAY + 2000,
            ),
        ];
        vec![
            digest_session(&a, &a_msgs),
            digest_session(&b, &b_msgs),
            digest_session(&c, &c_msgs),
        ]
    }

    #[test]
    fn digest_keeps_only_what_the_stats_need() {
        let d = &fixtures()[1];
        assert_eq!(d.directory, "/p2");
        assert_eq!(d.prompts.len(), 1, "summary + marker are not prompts");
        assert_eq!(d.calls.len(), 1);
        assert_eq!(
            d.calls[0].agent, "reviewer",
            "message meta wins over session agent"
        );
        assert_eq!(d.calls[0].model, "openai/gpt-x");
        assert_eq!(d.calls[0].cost, None);
        assert_eq!(d.tools.len(), 1);
        assert_eq!(d.compactions, vec![(NOW - 10 * DAY + 3000, 120_000)]);
    }

    #[test]
    fn totals_cover_every_project_when_unfiltered() {
        let stats = aggregate(&fixtures(), &StatsFilter::default(), NOW);
        let t = &stats.totals;
        assert_eq!(t.sessions, 3);
        assert_eq!(t.turns, 4);
        assert_eq!(t.llm_calls, 4);
        assert_eq!(t.tool_calls, 4);
        assert_eq!(t.input_tokens, 7010);
        assert_eq!(t.output_tokens, 605);
        assert_eq!(t.cache_read_tokens, 2000);
        assert_eq!(t.cache_write_tokens, 0);
        assert!((t.cost.unwrap() - 0.031).abs() < 1e-9);
        assert_eq!(t.cost_unknown_calls, 1);
        assert_eq!(stats.range.scanned_sessions, 3);
    }

    #[test]
    fn breakdowns_are_sorted_by_tokens_and_count_distinct_sessions() {
        let stats = aggregate(&fixtures(), &StatsFilter::default(), NOW);
        let models: Vec<&str> = stats.by_model.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(models, vec!["anthropic/claude-sonnet", "openai/gpt-x"]);
        assert_eq!(stats.by_model[0].totals.sessions, 1);
        assert_eq!(stats.by_model[1].totals.sessions, 2);
        assert_eq!(stats.by_model[1].totals.cost_unknown_calls, 1);

        let providers: Vec<&str> = stats.by_provider.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(providers, vec!["anthropic", "openai"]);

        let agents: Vec<&str> = stats.by_agent.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(agents, vec!["code", "reviewer", "plan"]);
        // "plan" owns session B's prompt but its call ran as "reviewer".
        assert_eq!(stats.by_agent[2].totals.turns, 1);
        assert_eq!(stats.by_agent[2].totals.llm_calls, 0);

        let projects: Vec<&str> = stats.by_project.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(projects, vec!["/p1", "/p2"]);
        assert_eq!(stats.by_project[0].totals.sessions, 2);
        assert_eq!(stats.by_project[0].totals.tool_calls, 3);
    }

    #[test]
    fn directory_filter_keeps_one_project() {
        let filter = StatsFilter {
            directory: Some("/p2".to_string()),
            ..Default::default()
        };
        let stats = aggregate(&fixtures(), &filter, NOW);
        assert_eq!(stats.totals.sessions, 1);
        assert_eq!(stats.totals.input_tokens, 4000);
        assert_eq!(stats.by_project.len(), 1);
        assert_eq!(stats.range.directory.as_deref(), Some("/p2"));
    }

    #[test]
    fn time_range_filters_calls_and_sessions() {
        let filter = StatsFilter {
            from: Some(NOW - 7 * DAY),
            to: Some(NOW),
            ..Default::default()
        };
        let stats = aggregate(&fixtures(), &filter, NOW);
        assert_eq!(
            stats.totals.sessions, 1,
            "only session A is active in the last 7 days"
        );
        assert_eq!(stats.totals.llm_calls, 2);
        assert_eq!(stats.totals.cost_unknown_calls, 0);
        assert_eq!(stats.compaction.count, 0);
        assert_eq!(stats.compaction.avg_context_before, None);
    }

    #[test]
    fn day_series_is_thirty_contiguous_days_ending_today() {
        let stats = aggregate(&fixtures(), &StatsFilter::default(), NOW);
        assert_eq!(stats.by_day.len(), DAYS_WINDOW);
        assert_eq!(stats.by_day.last().unwrap().day, "2026-09-10");
        assert_eq!(stats.by_day.first().unwrap().day, "2026-08-12");
        // Session A ran yesterday (NOW - 12h is 2026-09-09 UTC).
        let yesterday = &stats.by_day[DAYS_WINDOW - 2];
        assert_eq!(yesterday.day, "2026-09-09");
        assert_eq!(yesterday.llm_calls, 2);
        assert_eq!(yesterday.input_tokens, 3000);
        assert_eq!(yesterday.tool_calls, 3);
        // Session B is 10 days back; session C (45 days) falls off the window
        // but still counts in the totals.
        let ten_back = &stats.by_day[DAYS_WINDOW - 11];
        assert_eq!(ten_back.llm_calls, 1);
        assert_eq!(ten_back.cost, None);
        let in_window: usize = stats.by_day.iter().map(|d| d.llm_calls).sum();
        assert_eq!(in_window, 3);
        assert_eq!(stats.totals.llm_calls, 4);
    }

    #[test]
    fn top_sessions_and_tools_are_ranked() {
        let stats = aggregate(&fixtures(), &StatsFilter::default(), NOW);
        assert_eq!(stats.top_sessions.len(), 3);
        // A: 1000+200+500 + 2000+300+1500 = 5500 > B: 4100 > C: 15.
        assert_eq!(stats.top_sessions[0].directory, "/p1");
        assert_eq!(stats.top_sessions[0].totals.total_tokens(), 5500);
        assert_eq!(stats.top_sessions[1].directory, "/p2");
        assert_eq!(stats.top_sessions[2].totals.total_tokens(), 15);
        assert!(
            stats.top_sessions[0]
                .title
                .as_deref()
                .unwrap()
                .starts_with("code in")
        );

        let tools: Vec<(&str, usize, usize)> = stats
            .tools
            .iter()
            .map(|t| (t.name.as_str(), t.calls, t.errors))
            .collect();
        assert_eq!(tools, vec![("read", 2, 0), ("bash", 1, 1), ("grep", 1, 0)]);
    }

    #[test]
    fn compaction_average_uses_the_marker_before_figure() {
        let stats = aggregate(&fixtures(), &StatsFilter::default(), NOW);
        assert_eq!(stats.compaction.count, 2);
        assert_eq!(stats.compaction.avg_context_before, Some(100_000.0));
    }

    #[test]
    fn empty_corpus_yields_zeroes_and_a_full_day_axis() {
        let stats = aggregate(&[], &StatsFilter::default(), NOW);
        assert_eq!(stats.totals, Totals::default());
        assert_eq!(stats.by_day.len(), DAYS_WINDOW);
        assert!(stats.by_model.is_empty());
        assert!(stats.top_sessions.is_empty());
        assert_eq!(stats.compaction.avg_context_before, None);
    }

    #[test]
    fn provider_prefix_is_extracted() {
        assert_eq!(provider_of("openai/gpt-x"), "openai");
        assert_eq!(provider_of("bare-model"), "unknown");
        assert_eq!(provider_of("/x"), "unknown");
    }

    #[test]
    fn timestamps_parse_in_all_supported_shapes() {
        assert_eq!(parse_timestamp("1788998400000"), Some(NOW));
        assert_eq!(parse_timestamp("2026-09-10"), Some(NOW));
        assert_eq!(parse_timestamp("2026-09-10T00:00:00Z"), Some(NOW));
        assert_eq!(parse_timestamp("2026-09-10T00:00:00.000Z"), Some(NOW));
        assert_eq!(parse_timestamp("2026-09-10T02:00:00+02:00"), Some(NOW));
        assert_eq!(parse_timestamp("2026-09-09T22:30-01:30"), Some(NOW));
        assert_eq!(parse_timestamp("1970-01-01"), Some(0));
        assert_eq!(parse_timestamp("nope"), None);
        assert_eq!(parse_timestamp("2026-13-01"), None);
        assert_eq!(parse_timestamp(""), None);
    }

    #[test]
    fn civil_date_round_trips() {
        for days in [-1, 0, 1, 19_000, 20_707, 100_000] {
            let (y, m, d) = civil_from_days(days);
            assert_eq!(days_from_civil(y, m, d), days);
        }
        assert_eq!(format_day(0), "1970-01-01");
        assert_eq!(format_day(-1), "1969-12-31");
        assert_eq!(format_day(day_index(NOW)), "2026-09-10");
    }

    #[test]
    fn scan_reads_persisted_sessions_from_disk() {
        let root = std::env::temp_dir().join(format!("bebok-stats-{}", Uuid::new_v4()));
        let s = session("/p1", "code", NOW);
        let inst = persist::instance_dir(&root, &s.directory);
        let dir = persist::session_dir(&inst, s.id);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            persist::session_meta_path(&dir),
            serde_json::to_vec(&s).unwrap(),
        )
        .unwrap();
        let msgs = [
            user_at("hi", NOW),
            call_at(
                "code",
                "openai/gpt-x",
                NOW + 1,
                7,
                3,
                None,
                Some(0.5),
                &[("read", false)],
            ),
        ];
        for (i, m) in msgs.iter().enumerate() {
            std::fs::write(
                persist::message_path(&dir, i),
                serde_json::to_vec(m).unwrap(),
            )
            .unwrap();
        }
        // A corrupt sibling session must be skipped, not abort the scan.
        let bad = persist::session_dir(&inst, Uuid::new_v4());
        std::fs::create_dir_all(&bad).unwrap();
        std::fs::write(persist::session_meta_path(&bad), b"{not json").unwrap();

        let digests = scan(&root);
        assert_eq!(digests.len(), 1);
        assert_eq!(digests[0].id, s.id);
        assert_eq!(digests[0].calls.len(), 1);
        assert_eq!(digests[0].calls[0].input, 7);
        assert_eq!(digests[0].tools[0].name, "read");
        assert_eq!(scan(&root.join("missing")), Vec::new());
        let _ = std::fs::remove_dir_all(&root);
    }

    /// E2E B5: a session persisted with a `\?\`-prefixed (canonicalised)
    /// directory must land in the same project bucket as the plain path.
    #[cfg(windows)]
    #[test]
    fn scan_normalises_verbatim_directory_keys() {
        const PLAIN_DIR: &str = r"C:\projects\nope-e2e";
        const VERBATIM_DIR: &str = r"\\?\C:\projects\nope-e2e";
        let root = std::env::temp_dir().join(format!("bebok-stats-{}", Uuid::new_v4()));
        let plain = session(PLAIN_DIR, "code", NOW);
        let verbatim = session(VERBATIM_DIR, "code", NOW + 1);
        for s in [&plain, &verbatim] {
            let dir = persist::session_dir(&persist::instance_dir(&root, &s.directory), s.id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                persist::session_meta_path(&dir),
                serde_json::to_vec(s).unwrap(),
            )
            .unwrap();
            std::fs::write(
                persist::message_path(&dir, 0),
                serde_json::to_vec(&user_at("hi", s.created_at)).unwrap(),
            )
            .unwrap();
        }

        let digests = scan(&root);
        assert_eq!(digests.len(), 2);
        assert!(digests.iter().all(|d| d.directory == PLAIN_DIR), "{digests:?}");

        let stats = aggregate(&digests, &StatsFilter::default(), NOW);
        assert_eq!(stats.by_project.len(), 1, "one project row, not two");
        assert_eq!(stats.by_project[0].totals.sessions, 2);

        let filter = StatsFilter {
            directory: Some(normalize_path(Path::new(PLAIN_DIR))),
            ..Default::default()
        };
        assert_eq!(aggregate(&digests, &filter, NOW).totals.sessions, 2);
        let _ = std::fs::remove_dir_all(&root);
    }
}
