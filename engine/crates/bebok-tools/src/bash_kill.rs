//! `bash_kill` (F9-14): stop a background process started by
//! `bash { background: true }`.
//!
//! Resolves the target by registry `id` (preferred) or OS `pid`, then kills
//! the whole process tree through the [`ProcessRegistry`] and reports the
//! recorded exit status.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::processes::{ProcessInfo, ProcessRegistry};
use crate::tool::{Tool, ToolCtx, ToolOutput};

pub struct BashKill;

#[async_trait]
impl Tool for BashKill {
    fn name(&self) -> &str {
        "bash_kill"
    }

    fn description(&self) -> &str {
        "Stop a background process started by `bash` with background:true. Kills the whole \
         process tree (the shell and every child it spawned) and returns the exit status. \
         Identify it by the id from the bash result (preferred) or by pid."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "id": {
                    "type": "string",
                    "description": "Process id returned by `bash` with background:true."
                },
                "pid": {
                    "type": "integer",
                    "description": "OS pid of the background process (alternative to id).",
                    "minimum": 1
                }
            }
        })
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> ToolOutput {
        let registry = ProcessRegistry::global();
        let id = args.get("id").and_then(Value::as_str).map(str::trim);
        let pid = args.get("pid").and_then(Value::as_u64);

        let target: Option<ProcessInfo> = match (id, pid) {
            (Some(id), _) if !id.is_empty() => registry.get(id),
            (_, Some(pid)) => registry.find_by_pid(pid as u32).or_else(|| {
                registry
                    .list_all()
                    .into_iter()
                    .rev()
                    .find(|p| p.pid as u64 == pid)
            }),
            _ => {
                return ToolOutput::new(
                    "error: provide 'id' (from the bash background result) or 'pid'",
                    "bash_kill",
                );
            }
        };
        let Some(target) = target else {
            let what = match (id, pid) {
                (Some(id), _) if !id.is_empty() => format!("id={id}"),
                (_, Some(pid)) => format!("pid={pid}"),
                _ => String::new(),
            };
            return ToolOutput::new(
                format!(
                    "error: no background process with {what} (only processes started by `bash` with background:true can be stopped)"
                ),
                "bash_kill",
            );
        };

        let already_exited = !target.is_running();
        match registry.kill(&target.id).await {
            Ok(info) => {
                let code = info
                    .exit_code
                    .map(|c| c.to_string())
                    .unwrap_or_else(|| "signal/terminated".to_string());
                let text = if already_exited {
                    format!(
                        "process {} (pid {}) had already exited (exit code {code}): {}",
                        info.id, info.pid, info.command
                    )
                } else if info.is_running() {
                    format!(
                        "sent kill to process {} (pid {}) but it has not exited yet: {}",
                        info.id, info.pid, info.command
                    )
                } else {
                    format!(
                        "stopped process {} (pid {}, exit code {code}): {}",
                        info.id, info.pid, info.command
                    )
                };
                ToolOutput::new(text, format!("bash_kill {}", info.id))
                    .with_structured(serde_json::to_value(&info).unwrap_or(Value::Null))
            }
            Err(e) => ToolOutput::new(format!("error: {e}"), "bash_kill"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::BashKill;
    use crate::processes::ProcessRegistry;
    use crate::tool::{Tool, tool_ctx};
    use tokio_util::sync::CancellationToken;

    fn ctx() -> crate::tool::ToolCtx {
        tool_ctx(
            std::env::temp_dir(),
            "kill-session".into(),
            CancellationToken::new(),
        )
    }

    #[tokio::test]
    async fn missing_arguments_and_unknown_ids_are_errors() {
        let out = BashKill.execute(ctx(), serde_json::json!({})).await;
        assert!(out.text.starts_with("error:"), "{}", out.text);
        let out = BashKill
            .execute(ctx(), serde_json::json!({"id": "does-not-exist"}))
            .await;
        assert!(out.text.contains("no background process"), "{}", out.text);
        let out = BashKill
            .execute(ctx(), serde_json::json!({"pid": 4_000_000_000u64}))
            .await;
        assert!(out.text.contains("no background process"), "{}", out.text);
    }

    #[tokio::test]
    async fn kills_a_background_process_by_id_and_by_pid() {
        let root = std::env::temp_dir().join(format!("bebok-bash-kill-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let long = if cfg!(windows) {
            "ping -n 30 127.0.0.1"
        } else {
            "sleep 30"
        };
        let registry = ProcessRegistry::global();

        let a = registry
            .spawn_background("kill-session", &root, long)
            .await
            .unwrap();
        let out = BashKill
            .execute(ctx(), serde_json::json!({"id": a.id}))
            .await;
        assert!(out.text.starts_with("stopped process"), "{}", out.text);
        let structured = out.structured.unwrap();
        assert_eq!(structured["status"], "exited");
        assert_eq!(structured["id"], a.id);

        let b = registry
            .spawn_background("kill-session", &root, long)
            .await
            .unwrap();
        let out = BashKill
            .execute(ctx(), serde_json::json!({"pid": b.pid}))
            .await;
        assert!(out.text.starts_with("stopped process"), "{}", out.text);
        assert!(!registry.get(&b.id).unwrap().is_running());

        // Second kill reports the recorded exit.
        let out = BashKill
            .execute(ctx(), serde_json::json!({"id": b.id}))
            .await;
        assert!(out.text.contains("had already exited"), "{}", out.text);
        let _ = std::fs::remove_dir_all(&root);
    }
}
