//! Faza 1: `code_search` — full-text search over the per-instance code index.
//!
//! Search runs through the injected backend (`ToolCtx.index`):
//! * `Some` → `query_index` → hits as `path (score …)` lines, capped at 50;
//! * `None` (backend not attached, disabled or unavailable — e.g. `BEBOK_NO_INDEX`)
//!   → graceful degradation message:
//!   `code_search unavailable: code index is disabled (backend missing; BEBOK_NO_INDEX or an index plugin required)`.
//!
//! The tool never falls back to a filesystem scan: a code search without an
//! index is either disabled by policy or points at an unbuilt index, and a
//! plain directory scan cannot satisfy "index search" semantics (it would
//! return unindexed/unscoped content). `grep` remains the tool for raw
//! filesystem search.
//!
//! Read-only (`is_read_only() == true`): allowed by default, whitelisted for
//! the `ask` / `plan` presets like `grep`, default safety category `safe`.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// A code-index search backend, as seen by the `code_search` tool.
///
/// Kept in `bebok-tools` so the tool does not depend on `bebok-core`
/// (dependency direction is `core -> tools`). `bebok-core` implements it
/// as an adapter over its [`bebok_core::index::CodeIndexBackend`].
#[async_trait]
pub trait CodeIndexQuery: Send + Sync {
    /// Run a full-text query over the index.
    async fn query(
        &self,
        q: &str,
        extension: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CodeIndexHit>, CodeIndexError>;

    /// `true` when the index is usable (built and enabled).
    fn is_available(&self) -> bool;
}

/// One hit returned by [`CodeIndexQuery`].
#[derive(Debug, Clone)]
pub struct CodeIndexHit {
    pub path: String,
    pub score: f32,
}

/// Failures surfaced by [`CodeIndexQuery::query`].
#[derive(Debug, thiserror::Error)]
pub enum CodeIndexError {
    /// The index is disabled (policy / build switch, e.g. `BEBOK_NO_INDEX`).
    #[error("code index is disabled")]
    Disabled,
    /// The index exists but has not been built yet.
    #[error("index not built yet")]
    NotBuilt,
    /// Any other backend failure.
    #[error("index error: {0}")]
    Backend(String),
}

/// Full-text search over the indexed project files.
pub struct CodeSearch {
    index: Option<Arc<dyn CodeIndexQuery>>,
}

const MAX_RESULTS: usize = 50;

impl CodeSearch {
    /// Build the tool with an optional index backend.
    ///
    /// `index`: injected from `core` (`Some` when an index plugin is
    /// attached and enabled, `None` when the backend slot is empty /
    /// disabled / the tool runs without an instance). `None` ⇒ the
    /// tool answers with a graceful message instead of failing or panicking.
    pub fn new(index: Option<Arc<dyn CodeIndexQuery>>) -> Self {
        Self { index }
    }

    /// Build the tool without an index backend (tests / no-instance runs).
    pub fn without_index() -> Self {
        Self { index: None }
    }
}

#[async_trait]
impl Tool for CodeSearch {
    fn name(&self) -> &str {
        "code_search"
    }

    fn description(&self) -> &str {
        "Full-text search over the project's indexed files."
    }

    fn parameters_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "Full-text query (words, identifiers, phrases)."
                },
                "extension": {
                    "type": "string",
                    "description": "Optional file-extension filter, e.g. \"rs\" or \".ts\"."
                },
                "limit": {
                    "type": "integer",
                    "description": "Max hits (default 20, capped at 50)."
                }
            },
            "required": ["query"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(query) = args
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|q| !q.is_empty())
        else {
            return ToolOutput::new("error: missing required parameter 'query'", "code_search");
        };
        let extension = args
            .get("extension")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|e| !e.is_empty());
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .map(|l| (l as usize).clamp(1, MAX_RESULTS))
            .unwrap_or(20);

        // The live backend comes from `ToolCtx.index` (fresh per call, fed
        // from the instance's currently attached backend), so a plugin
        // toggle hot-swap takes effect without re-registering the tool.
        // `self.index` is only the construction-time fallback (tests /
        // no-instance runs).
        let index = ctx.index.as_ref().or(self.index.as_ref());
        let Some(index) = index else {
            return ToolOutput::new(
                "code_search unavailable: code index is disabled (backend missing; \
                 BEBOK_NO_INDEX or an index plugin required)",
                "code_search",
            );
        };
        if !index.is_available() {
            return ToolOutput::new(
                "code_search unavailable: code index is disabled (backend missing; \
                 BEBOK_NO_INDEX or an index plugin required)",
                "code_search",
            );
        }

        let query_owned = query.to_string();
        let ext_owned = extension.map(str::to_string);
        let index_ref: Arc<dyn CodeIndexQuery> = index.clone();
        let outcome = query_index(index_ref, query_owned, ext_owned, limit).await;
        match outcome {
            Ok((hits, _)) => {
                let mut out = String::new();
                for hit in hits.iter().take(limit) {
                    out.push_str(&format!("{} (score {:.2})\n", hit.path, hit.score));
                }
                if out.is_empty() {
                    out = "(no matches)".to_string();
                }
                ToolOutput::new(out, format!("code_search {query}"))
            }
            Err(CodeIndexError::Disabled) => ToolOutput::new(
                "code_search unavailable: code index is disabled (backend missing; \
                 BEBOK_NO_INDEX or an index plugin required)",
                "code_search",
            ),
            Err(CodeIndexError::NotBuilt) => ToolOutput::new(
                "code_search unavailable: index not built yet (run 'ensure code index' or wait for indexing to finish)",
                "code_search",
            ),
            Err(CodeIndexError::Backend(reason)) => {
                ToolOutput::new(format!("code_search error: {reason}"), "code_search")
            }
        }
    }
}

/// Query the backend for `root`. Returns hits or a short reason.
async fn query_index(
    index: Arc<dyn CodeIndexQuery>,
    query: String,
    extension: Option<String>,
    limit: usize,
) -> Result<(Vec<CodeIndexHit>, usize), CodeIndexError> {
    let limit = limit.clamp(1, MAX_RESULTS);
    let hits = index.query(&query, extension.as_deref(), limit).await?;
    let n = hits.len();
    Ok((hits, n))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{CodeIndexError, CodeIndexHit, CodeIndexQuery, CodeSearch};
    use crate::tool::Tool;

    struct MockIndex {
        available: bool,
        hits: Vec<CodeIndexHit>,
    }

    #[async_trait::async_trait]
    impl CodeIndexQuery for MockIndex {
        async fn query(
            &self,
            _q: &str,
            _extension: Option<&str>,
            limit: usize,
        ) -> Result<Vec<CodeIndexHit>, CodeIndexError> {
            Ok(self.hits.iter().take(limit).cloned().collect())
        }

        fn is_available(&self) -> bool {
            self.available
        }
    }

    fn hits() -> Vec<CodeIndexHit> {
        vec![
            CodeIndexHit {
                path: "src/main.rs".to_string(),
                score: 1.2,
            },
            CodeIndexHit {
                path: "src/lib.rs".to_string(),
                score: 0.9,
            },
        ]
    }

    #[tokio::test]
    async fn returns_hits_when_index_available() {
        let tool = CodeSearch::new(Some(Arc::new(MockIndex {
            available: true,
            hits: hits(),
        })));
        let out = tool
            .execute(
                crate::tool::tool_ctx(
                    std::path::PathBuf::from("/root"),
                    "sess".to_string(),
                    tokio_util::sync::CancellationToken::new(),
                ),
                serde_json::json!({ "query": "fn main", "limit": 10 }),
            )
            .await;
        assert!(out.text.contains("src/main.rs"), "got: {}", out.text);
        assert!(out.text.contains("score"), "got: {}", out.text);
    }

    #[tokio::test]
    async fn caps_results_at_limit() {
        let big: Vec<CodeIndexHit> = (0..100)
            .map(|i| CodeIndexHit {
                path: format!("src/{i}.rs"),
                score: 1.0,
            })
            .collect();
        let tool = CodeSearch::new(Some(Arc::new(MockIndex {
            available: true,
            hits: big,
        })));
        let out = tool
            .execute(
                crate::tool::tool_ctx(
                    std::path::PathBuf::from("/root"),
                    "sess".to_string(),
                    tokio_util::sync::CancellationToken::new(),
                ),
                serde_json::json!({ "query": "q", "limit": 5 }),
            )
            .await;
        let count = out.text.lines().count();
        assert_eq!(count, 5, "got: {}", out.text);
    }

    #[tokio::test]
    async fn no_matches_when_index_empty() {
        let tool = CodeSearch::new(Some(Arc::new(MockIndex {
            available: true,
            hits: vec![],
        })));
        let out = tool
            .execute(
                crate::tool::tool_ctx(
                    std::path::PathBuf::from("/root"),
                    "sess".to_string(),
                    tokio_util::sync::CancellationToken::new(),
                ),
                serde_json::json!({ "query": "q" }),
            )
            .await;
        assert!(out.text.contains("(no matches)"), "got: {}", out.text);
    }

    #[tokio::test]
    async fn graceful_when_index_disabled() {
        let tool = CodeSearch::new(Some(Arc::new(MockIndex {
            available: false,
            hits: hits(),
        })));
        let out = tool
            .execute(
                crate::tool::tool_ctx(
                    std::path::PathBuf::from("/root"),
                    "sess".to_string(),
                    tokio_util::sync::CancellationToken::new(),
                ),
                serde_json::json!({ "query": "q" }),
            )
            .await;
        assert!(
            out.text.contains("code_search unavailable"),
            "got: {}",
            out.text
        );
    }

    #[tokio::test]
    async fn graceful_when_no_backend() {
        let tool = CodeSearch::without_index();
        let out = tool
            .execute(
                crate::tool::tool_ctx(
                    std::path::PathBuf::from("/root"),
                    "sess".to_string(),
                    tokio_util::sync::CancellationToken::new(),
                ),
                serde_json::json!({ "query": "q" }),
            )
            .await;
        assert!(
            out.text.contains("code_search unavailable"),
            "got: {}",
            out.text
        );
    }

    #[tokio::test]
    async fn rejects_missing_query() {
        let tool = CodeSearch::without_index();
        let out = tool
            .execute(
                crate::tool::tool_ctx(
                    std::path::PathBuf::from("/root"),
                    "sess".to_string(),
                    tokio_util::sync::CancellationToken::new(),
                ),
                serde_json::json!({}),
            )
            .await;
        assert!(
            out.text.contains("missing required parameter 'query'"),
            "got: {}",
            out.text
        );
    }

    /// Hot-swap: the per-call `ToolCtx.index` wins over the
    /// construction-time backend, so a plugin toggle takes effect without
    /// re-registering the tool (live ctx set, stale `self.index` unset).
    #[tokio::test]
    async fn ctx_index_wins_over_construction_time_backend() {
        let tool = CodeSearch::without_index();
        let mut ctx = crate::tool::tool_ctx(
            std::path::PathBuf::from("/root"),
            "sess".to_string(),
            tokio_util::sync::CancellationToken::new(),
        );
        ctx.index = Some(Arc::new(MockIndex {
            available: true,
            hits: hits(),
        }));
        let out = tool
            .execute(ctx, serde_json::json!({ "query": "fn main" }))
            .await;
        assert!(out.text.contains("src/main.rs"), "got: {}", out.text);
    }

    /// Hot-stop: a parked per-call ctx degrades gracefully even when the
    /// tool was built with a live backend.
    #[tokio::test]
    async fn parked_ctx_index_degrades_despite_live_self_index() {
        let tool = CodeSearch::new(Some(Arc::new(MockIndex {
            available: true,
            hits: hits(),
        })));
        let mut ctx = crate::tool::tool_ctx(
            std::path::PathBuf::from("/root"),
            "sess".to_string(),
            tokio_util::sync::CancellationToken::new(),
        );
        ctx.index = Some(Arc::new(MockIndex {
            available: false,
            hits: vec![],
        }));
        let out = tool.execute(ctx, serde_json::json!({ "query": "q" })).await;
        assert!(
            out.text.contains("code_search unavailable"),
            "got: {}",
            out.text
        );
    }
}
