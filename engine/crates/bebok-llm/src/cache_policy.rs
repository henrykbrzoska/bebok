//! Explicit Anthropic prompt-cache breakpoint placement.

use serde_json::Value;

/// Conservative approximation of Anthropic's largest active-model minimum
/// (4,096 tokens at roughly four UTF-8 bytes per token).
pub const MIN_CACHE_PREFIX_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CachePolicy {
    enabled: bool,
    min_prefix_bytes: usize,
}

impl CachePolicy {
    pub fn for_model(model: &str) -> Self {
        Self {
            enabled: crate::ModelCatalog::global()
                .get(model)
                .supports_prompt_cache,
            min_prefix_bytes: MIN_CACHE_PREFIX_BYTES,
        }
    }

    pub fn disabled() -> Self {
        Self {
            enabled: false,
            min_prefix_bytes: MIN_CACHE_PREFIX_BYTES,
        }
    }

    pub fn system_value(self, system: &str) -> Value {
        if self.enabled && system.len() >= self.min_prefix_bytes {
            serde_json::json!([{
                "type": "text",
                "text": system,
                "cache_control": { "type": "ephemeral" }
            }])
        } else {
            Value::String(system.to_string())
        }
    }

    /// Mark the last block of stable conversation history. The newest message
    /// is the request-specific suffix and is deliberately excluded.
    pub fn apply_to_messages(self, system: &str, messages: &mut [Value]) {
        if !self.enabled || messages.len() < 2 {
            return;
        }
        let stable_end = messages.len() - 1;
        let stable_bytes = system.len()
            + messages[..stable_end]
                .iter()
                .map(|message| message.to_string().len())
                .sum::<usize>();
        if stable_bytes < self.min_prefix_bytes {
            return;
        }
        for message in messages[..stable_end].iter_mut().rev() {
            let Some(blocks) = message.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            if let Some(Value::Object(block)) = blocks.last_mut() {
                block.insert(
                    "cache_control".to_string(),
                    serde_json::json!({ "type": "ephemeral" }),
                );
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_policy_does_not_mark_blocks() {
        let mut messages = vec![
            serde_json::json!({"role":"user","content":[{"type":"text","text":"x"}]}),
            serde_json::json!({"role":"user","content":[{"type":"text","text":"new"}]}),
        ];
        CachePolicy::disabled().apply_to_messages(&"s".repeat(20_000), &mut messages);
        assert!(messages[0]["content"][0].get("cache_control").is_none());
    }
}
