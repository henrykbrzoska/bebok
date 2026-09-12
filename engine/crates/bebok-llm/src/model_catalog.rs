//! Model capabilities and prices sourced from the vendored models.dev snapshot.

use std::collections::HashMap;
use std::path::Path;
use std::sync::OnceLock;

use serde::Deserialize;

const SNAPSHOT: &str = include_str!(concat!(env!("OUT_DIR"), "/models.dev.json"));

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub supports_images: bool,
    pub supports_tools: bool,
    pub supports_thinking: bool,
    pub supports_prompt_cache: bool,
    pub context_window: u64,
    pub max_output: u64,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            // Unknown and custom models remain usable. The provider is the
            // authority when no catalog record exists.
            supports_images: true,
            supports_tools: true,
            supports_thinking: false,
            supports_prompt_cache: false,
            context_window: 64_000,
            max_output: 8_192,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPricing {
    pub input: f64,
    pub output: f64,
    pub cache_read: f64,
    pub cache_write: f64,
}

#[derive(Debug, Clone)]
struct ModelEntry {
    capabilities: ModelCapabilities,
    pricing: Option<ModelPricing>,
}

#[derive(Debug, Clone)]
pub struct ModelCatalog {
    models: HashMap<String, ModelEntry>,
    providers: HashMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct Snapshot {
    providers: HashMap<String, SnapshotProvider>,
}

#[derive(Deserialize)]
struct SnapshotProvider {
    #[serde(default)]
    models: HashMap<String, SnapshotModel>,
}

#[derive(Deserialize)]
struct SnapshotModel {
    #[serde(default)]
    modalities: Modalities,
    #[serde(default)]
    tool_call: bool,
    #[serde(default)]
    reasoning: bool,
    #[serde(default)]
    prompt_cache: bool,
    #[serde(default = "default_context")]
    context: u64,
    #[serde(default = "default_output")]
    output: u64,
    #[serde(default)]
    cost: Option<SnapshotCost>,
}

#[derive(Default, Deserialize)]
struct Modalities {
    #[serde(default)]
    input: Vec<String>,
}

#[derive(Deserialize)]
struct SnapshotCost {
    input: f64,
    output: f64,
    #[serde(default)]
    cache_read: f64,
    #[serde(default)]
    cache_write: f64,
}

const fn default_context() -> u64 {
    64_000
}
const fn default_output() -> u64 {
    8_192
}

impl ModelCatalog {
    pub fn global() -> &'static Self {
        static CATALOG: OnceLock<ModelCatalog> = OnceLock::new();
        CATALOG.get_or_init(|| {
            let mut catalog = Self::from_json(SNAPSHOT).expect("vendored models.dev.json is valid");
            if let Some(path) = std::env::var_os("BEBOK_MODEL_CATALOG")
                && let Ok(json) = std::fs::read_to_string(path)
                && let Ok(local) = Self::from_json(&json)
            {
                catalog.merge(local);
            }
            catalog
        })
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        let snapshot: Snapshot = serde_json::from_str(json)?;
        let mut models = HashMap::new();
        let mut providers = HashMap::new();
        for (provider, entry) in snapshot.providers {
            let provider = provider.to_ascii_lowercase();
            let mut names: Vec<String> = entry.models.keys().cloned().collect();
            names.sort();
            for (model, item) in entry.models {
                let key = format!("{provider}/{}", model.to_ascii_lowercase());
                models.insert(key, item.into());
            }
            providers.insert(provider, names);
        }
        Ok(Self { models, providers })
    }

    pub fn with_override(path: impl AsRef<Path>) -> Result<Self, Box<dyn std::error::Error>> {
        let mut catalog = Self::from_json(SNAPSHOT)?;
        catalog.merge(Self::from_json(&std::fs::read_to_string(path)?)?);
        Ok(catalog)
    }

    pub fn get(&self, model: &str) -> ModelCapabilities {
        self.entry(model)
            .map(|entry| entry.capabilities)
            .unwrap_or_default()
    }

    pub fn pricing(&self, model: &str) -> Option<ModelPricing> {
        self.entry(model).and_then(|entry| entry.pricing)
    }

    pub fn provider_models(&self, provider: &str) -> Vec<String> {
        self.providers
            .get(&provider.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    fn entry(&self, model: &str) -> Option<&ModelEntry> {
        let key = model.to_ascii_lowercase();
        if let Some(entry) = self.models.get(&key) {
            return Some(entry);
        }
        // Unprefixed model ids are accepted when they identify one snapshot
        // entry uniquely. This preserves existing callers such as cost.rs.
        let suffix = format!("/{key}");
        let mut matches = self
            .models
            .iter()
            .filter(|(id, _)| id.ends_with(&suffix) && id.matches('/').count() == 1);
        let first = matches.next().map(|(_, entry)| entry)?;
        matches.next().is_none().then_some(first)
    }

    fn merge(&mut self, other: Self) {
        self.models.extend(other.models);
        for (provider, models) in other.providers {
            let current = self.providers.entry(provider).or_default();
            for model in models {
                if !current.contains(&model) {
                    current.push(model);
                }
            }
        }
    }
}

impl From<SnapshotModel> for ModelEntry {
    fn from(value: SnapshotModel) -> Self {
        Self {
            capabilities: ModelCapabilities {
                supports_images: value.modalities.input.iter().any(|item| item == "image"),
                supports_tools: value.tool_call,
                supports_thinking: value.reasoning,
                supports_prompt_cache: value.prompt_cache,
                context_window: value.context,
                max_output: value.output,
            },
            pricing: value.cost.map(|cost| ModelPricing {
                input: cost.input,
                output: cost.output,
                cache_read: cost.cache_read,
                cache_write: cost.cache_write,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_models_have_populated_capabilities() {
        let catalog = ModelCatalog::global();
        let gpt = catalog.get("openai/gpt-4.1");
        assert!(gpt.supports_images && gpt.supports_tools && gpt.supports_prompt_cache);
        assert_eq!(gpt.context_window, 1_048_576);
        assert_eq!(gpt.max_output, 32_768);
        let price = catalog.pricing("openai/gpt-4.1").unwrap();
        assert_eq!(price.input, 2.0);
        assert_eq!(price.output, 8.0);

        let deepseek = catalog.get("deepseek/deepseek-chat");
        assert!(!deepseek.supports_images);
        assert!(deepseek.supports_tools);
    }

    #[test]
    fn unknown_model_uses_sensible_fallback() {
        let capabilities = ModelCatalog::global().get("custom/unknown-model");
        assert_eq!(capabilities, ModelCapabilities::default());
        assert!(capabilities.context_window > 0 && capabilities.max_output > 0);
        assert!(
            ModelCatalog::global()
                .pricing("custom/unknown-model")
                .is_none()
        );
    }

    #[test]
    fn local_override_replaces_snapshot_entry() {
        let local = ModelCatalog::from_json(r#"{"providers":{"openai":{"models":{"gpt-4.1":{"modalities":{"input":["text"]},"context":42,"output":7}}}}}"#).unwrap();
        let mut catalog = ModelCatalog::from_json(SNAPSHOT).unwrap();
        catalog.merge(local);
        let capabilities = catalog.get("openai/gpt-4.1");
        assert!(!capabilities.supports_images);
        assert_eq!(capabilities.context_window, 42);
        assert_eq!(capabilities.max_output, 7);
    }
}
