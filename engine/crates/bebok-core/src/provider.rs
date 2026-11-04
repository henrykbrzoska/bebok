//! Provider factory (Factory): build a provider client for a `provider/model`
//! string. Lives in core (not the server) so core-side features — e.g. the
//! sub-agent `task` tool — can build providers without depending on the
//! server layer.
//!
//! Selection is by model prefix (`zai/...`, `openai/...`, `anthropic/...`,
//! `xai/...`, `deepseek/...`, `google/...`, `openrouter/...`, `ollama/...` or
//! any provider name from config). The API key comes from the provider spec
//! (`api_key` field or the provider env var) with a final fallback to the
//! resolved `config.api_key`.
//!
//! Two shapes of provider are built here:
//! - HTTP (`openai` / `anthropic`) — delegated to [`bebok_llm::build_provider`];
//! - `kind: "cli"` — an agentic CLI driven as a transport, built by
//!   `bebok-cli-agents`. That needs the project root (the CLI's `current_dir`),
//!   which is why [`build_provider_for`] takes a [`ProviderCtx`].

use std::path::{Path, PathBuf};
use std::sync::Arc;

use bebok_llm::{Provider, ProviderKind};

use crate::config::{ResolvedConfig, provider_from_model};
use crate::error::CoreError;

/// Everything a provider may need beyond the config: today only where to run.
///
/// - [`ProviderCtx::none`] preserves the pre-CLI behaviour (HTTP providers only);
/// - [`ProviderCtx::root`] is what every real turn uses.
#[derive(Debug, Clone, Default)]
pub struct ProviderCtx {
    pub project_root: Option<PathBuf>,
}

impl ProviderCtx {
    /// No project root: CLI providers cannot be built from here.
    pub fn none() -> Self {
        Self::default()
    }

    /// Run CLI providers in `root`.
    pub fn root(root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: Some(root.into()),
        }
    }

    pub fn project_root(&self) -> Option<&Path> {
        self.project_root.as_deref()
    }
}

/// Build the provider for a model (see module docs for selection rules).
///
/// Kept for call sites that only ever see HTTP providers; equivalent to
/// [`build_provider_for`] with [`ProviderCtx::none`].
pub fn build_provider(
    config: &ResolvedConfig,
    model: &str,
) -> Result<Arc<dyn Provider>, CoreError> {
    build_provider_for(config, model, &ProviderCtx::none())
}

/// Build the provider for a model, telling CLI providers where to run.
pub fn build_provider_for(
    config: &ResolvedConfig,
    model: &str,
    ctx: &ProviderCtx,
) -> Result<Arc<dyn Provider>, CoreError> {
    let name = provider_from_model(model);
    let spec = config
        .provider_spec(&name)
        .ok_or_else(|| CoreError::ProviderConfig(format!("unknown provider '{name}'")))?;

    if spec.kind == ProviderKind::Cli {
        // A CLI runs in a project directory; without one there is nothing
        // coherent to point it at, and guessing a cwd would let it read or
        // edit the wrong tree. Failing loudly beats a surprise.
        let root = ctx.project_root().ok_or_else(|| {
            CoreError::ProviderConfig(format!(
                "provider '{name}' is a CLI provider but no project root was supplied"
            ))
        })?;
        return bebok_cli_agents::build_provider(&spec, Some(root.to_path_buf()))
            .map_err(CoreError::ProviderConfig);
    }

    bebok_llm::build_provider(
        &spec,
        config.api_key.clone().filter(|s| !s.trim().is_empty()),
    )
    .map_err(CoreError::ProviderConfig)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctx_none_has_no_root_and_root_sets_one() {
        assert!(ProviderCtx::none().project_root().is_none());
        let ctx = ProviderCtx::root("/tmp/proj");
        assert_eq!(ctx.project_root(), Some(Path::new("/tmp/proj")));
    }
}
