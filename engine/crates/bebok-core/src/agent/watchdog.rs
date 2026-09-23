//! Orchestrator watchdog: monitors child agents for loops/wandering and
//! triggers restarts.
//!
//! The loop runs per child (spawned by `run_child` in `delegation.rs`),
//! ticks every `watchdog_secs`, and decides via [`should_restart`] whether
//! to abort + respawn the child in the SAME session (history is kept so the
//! model sees it looped; the prompt gains a "change strategy" note).
//! A restart never takes a new `SlotGate` slot — the child keeps its own.
//! After `max_restarts` the caller errors to the orchestrator ("child X got
//! stuck N times, take over yourself") instead of restarting.
//!
//! The loop signals its decision through an `mpsc` channel rather than
//! aborting directly: `run_child` owns the full `ChildSpec` needed to
//! recreate the child session.

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Decision about whether to restart a child task.
pub struct WatchdogDecision {
    /// Whether to restart.
    pub should_restart: bool,
    /// Reason for restart if any (None = no restart).
    pub reason: Option<String>,
    /// Restart count after this decision (0 if no restart).
    pub restart_count: u32,
}

/// Determine whether a child should be restarted based on the current
/// verdict.
///
/// - `restarts >= max_restarts` → `None` (limit reached, caller must error
///   to the orchestrator instead of restarting).
/// - `looping` / `wandering` verdicts → `Some(reason)`.
/// - `snapshot_unchanged` (no progress for the whole interval) → `Some`.
/// - `ok` without silence → `None`.
pub fn should_restart(
    verdict: &str,
    snapshot_unchanged: bool,
    restarts: u32,
    max_restarts: u32,
) -> Option<String> {
    if restarts >= max_restarts {
        return None; // Caller errors to the orchestrator.
    }
    if verdict == "looping" || verdict == "wandering" {
        return Some(format!(
            "child verdict: {verdict} — restarting with adjusted prompt"
        ));
    }
    if snapshot_unchanged {
        return Some("child silent — no progress for watchdog interval — restarting".to_string());
    }
    None
}

/// Snapshot the loop compares across ticks: tool-call count + step count.
/// (Full `analyze_supervision` needs private `delegation` internals; the
/// count pair is the cheap, stable proxy. A caller that already computed a
/// verdict passes it in — see `run_child`.)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ProgressSnapshot {
    pub tool_calls: usize,
    pub steps: usize,
}

/// Watchdog loop for one child task. Ticks every `watchdog_secs`, asks
/// `snapshot_fn` for the current progress, and sends a restart reason
/// through `tx` when [`should_restart`] fires. Exits when `done` is
/// cancelled (child finished) or after signalling one restart (the parent
/// respawns the child together with a fresh loop).
pub async fn watchdog_loop<F>(
    mut snapshot_fn: F,
    done: CancellationToken,
    tx: mpsc::Sender<String>,
    max_restarts: u32,
    restarts: u32,
    watchdog_secs: u64,
) where
    F: FnMut() -> (ProgressSnapshot, String) + Send + 'static,
{
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(watchdog_secs.max(1)));
    // First tick fires immediately — skip it so the child gets a full
    // interval before its first supervision.
    interval.tick().await;
    let mut prev: Option<ProgressSnapshot> = None;

    loop {
        interval.tick().await;
        if done.is_cancelled() {
            break;
        }
        let (snap, verdict) = snapshot_fn();
        let unchanged = prev.is_some_and(|p| p == snap);
        prev = Some(snap);
        if let Some(reason) = should_restart(&verdict, unchanged, restarts, max_restarts) {
            // Best effort: the parent may already be gone.
            let _ = tx.send(reason).await;
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn looping_triggers_restart() {
        assert_eq!(
            should_restart("looping", false, 0, 2),
            Some("child verdict: looping — restarting with adjusted prompt".to_string())
        );
    }

    #[test]
    fn wandering_triggers_restart() {
        assert_eq!(
            should_restart("wandering", false, 0, 2),
            Some("child verdict: wandering — restarting with adjusted prompt".to_string())
        );
    }

    #[test]
    fn ok_without_silence_no_restart() {
        assert_eq!(should_restart("ok", false, 0, 2), None);
    }

    #[test]
    fn silent_triggers_restart() {
        assert_eq!(
            should_restart("ok", true, 0, 2),
            Some("child silent — no progress for watchdog interval — restarting".to_string())
        );
    }

    #[test]
    fn limit_reached_no_restart() {
        // At max_restarts, should_restart returns None (caller must error).
        assert_eq!(should_restart("looping", false, 2, 2), None);
    }

    #[test]
    fn below_limit_allows_restart() {
        assert!(should_restart("looping", false, 1, 2).is_some());
    }

    #[tokio::test]
    async fn loop_signals_silent_child() {
        let (tx, mut rx) = mpsc::channel(1);
        let done = CancellationToken::new();
        let done2 = done.clone();
        let handle = tokio::spawn(watchdog_loop(
            || {
                (
                    ProgressSnapshot {
                        tool_calls: 3,
                        steps: 5,
                    },
                    "ok".to_string(),
                )
            },
            done2,
            tx,
            2,
            0,
            1,
        ));
        // Two ticks at 1s: first establishes the snapshot, second sees it
        // unchanged → restart signal.
        let reason = tokio::time::timeout(std::time::Duration::from_secs(5), rx.recv())
            .await
            .expect("watchdog must signal within 5s")
            .expect("channel must stay open");
        assert!(reason.contains("silent"), "unexpected reason: {reason}");
        done.cancel();
        handle.await.expect("loop must exit cleanly");
    }
}
