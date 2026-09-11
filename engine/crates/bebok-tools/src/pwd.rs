use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Print the project root (working directory).
///
/// Portable alternative to `pwd` (POSIX) / `pwd` aka `Get-Location`
/// (PowerShell): the `bash` tool runs a different shell per OS, so the model
/// would have to guess the syntax. This tool returns `ctx.root` directly and
/// behaves identically on Windows, macOS and Linux.
pub struct Pwd;

#[async_trait]
impl Tool for Pwd {
    fn name(&self) -> &str {
        "pwd"
    }

    fn description(&self) -> &str {
        "Print the project root (working directory). Portable alternative to running `pwd` via bash — works identically on Windows, macOS and Linux."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, _args: Value) -> ToolOutput {
        ToolOutput::new(ctx.root.to_string_lossy().to_string(), "pwd")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn pwd_returns_the_project_root() {
        let root = std::env::temp_dir().join("bebok-pwd-test");
        let ctx = ToolCtx {
            root: root.clone(),
            session_id: "pwd-test".to_string(),
            abort: CancellationToken::new(),
        };
        let out = Pwd.execute(ctx, serde_json::json!({})).await;
        assert_eq!(out.text, root.to_string_lossy().to_string());
        assert!(Pwd.is_read_only());
    }
}
