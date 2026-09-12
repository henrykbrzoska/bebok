//! Message part model (SPEC §3.4).

pub mod persist;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;

use crate::util::now_ms;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    Image {
        media_type: String,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    Thinking {
        text: String,
    },
    Tool {
        id: String,
        name: String,
        state: ToolState,
    },
    Usage {
        input_tokens: u64,
        output_tokens: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cost: Option<f64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_read_input_tokens: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache_creation_input_tokens: Option<u64>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ToolState {
    Pending {
        input: Value,
    },
    Running {
        input: Value,
        started_at: i64,
    },
    Completed {
        input: Value,
        output: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        structured: Option<Value>,
    },
    Error {
        input: Value,
        error: String,
    },
}

impl ToolState {
    pub fn input(&self) -> &Value {
        match self {
            ToolState::Pending { input }
            | ToolState::Running { input, .. }
            | ToolState::Completed { input, .. }
            | ToolState::Error { input, .. } => input,
        }
    }

    /// Extra structured payload (e.g. task metadata) attached to the completed tool call.
    pub fn structured(&self) -> Option<&Value> {
        match self {
            ToolState::Completed { structured, .. } => structured.as_ref(),
            _ => None,
        }
    }

    /// True once the tool reached a closed state (completed/error).
    pub fn is_closed(&self) -> bool {
        matches!(self, ToolState::Completed { .. } | ToolState::Error { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MessageMeta {
    #[serde(default)]
    pub created_at: i64,
    /// The agent preset used to produce this message (assistant messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    /// The model used to produce this message (assistant messages only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub id: Uuid,
    pub role: Role,
    #[serde(default)]
    pub parts: Vec<Part>,
    #[serde(default)]
    pub meta: MessageMeta,
}

impl Message {
    pub fn new(role: Role) -> Self {
        Self {
            id: Uuid::new_v4(),
            role,
            parts: Vec::new(),
            meta: MessageMeta {
                created_at: now_ms(),
                agent: None,
                model: None,
            },
        }
    }

    pub fn user(text: impl Into<String>) -> Self {
        let mut m = Self::new(Role::User);
        m.parts.push(Part::Text { text: text.into() });
        m
    }

    /// A user message carrying text plus pre-built image parts.
    pub fn user_with_images(text: impl Into<String>, images: Vec<Part>) -> Self {
        let mut m = Self::new(Role::User);
        m.parts.push(Part::Text { text: text.into() });
        for img in images {
            if matches!(img, Part::Image { .. }) {
                m.parts.push(img);
            }
        }
        m
    }

    /// An assistant message tagged with the agent + model that produced it.
    pub fn assistant_with(agent: &str, model: &str) -> Self {
        let mut m = Self::new(Role::Assistant);
        m.meta.agent = Some(agent.to_string());
        m.meta.model = Some(model.to_string());
        m
    }

    /// A compaction summary message: a single user-role text part tagged
    /// `[summary of messages 0..N]` (SPEC §3.9).
    pub fn summary(text: impl Into<String>) -> Self {
        Self::user(text)
    }

    /// Append a text delta to the trailing Text part (or start a new one).
    pub fn append_text(&mut self, delta: &str) {
        if let Some(Part::Text { text }) = self.parts.last_mut() {
            text.push_str(delta);
        } else {
            self.parts.push(Part::Text {
                text: delta.to_string(),
            });
        }
    }

    /// Append a thinking delta to the trailing Thinking part (or start a new one).
    pub fn append_thinking(&mut self, delta: &str) {
        if let Some(Part::Thinking { text }) = self.parts.last_mut() {
            text.push_str(delta);
        } else {
            self.parts.push(Part::Thinking {
                text: delta.to_string(),
            });
        }
    }

    pub fn add_tool_call(&mut self, id: String, name: String, input: Value) {
        self.parts.push(Part::Tool {
            id,
            name,
            state: ToolState::Pending { input },
        });
    }

    pub fn set_usage(&mut self, usage: crate::session::UsageTotals) {
        self.parts.push(Part::Usage {
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cost: usage.cost,
            cache_read_input_tokens: usage.cache_read_input_tokens,
            cache_creation_input_tokens: usage.cache_creation_input_tokens,
        });
    }

    pub fn text_content(&self) -> String {
        let mut out = String::new();
        for p in &self.parts {
            if let Part::Text { text } = p {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(text);
            }
        }
        out
    }

    /// Image parts as `(media_type, base64_data, name)`.
    pub fn image_parts(&self) -> Vec<(String, String, Option<String>)> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Image {
                    media_type,
                    data,
                    name,
                } => Some((media_type.clone(), data.clone(), name.clone())),
                _ => None,
            })
            .collect()
    }

    /// Tool parts with a pending (not yet executed) state.
    pub fn pending_tool_calls(&self) -> Vec<(String, String, Value)> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Tool {
                    id,
                    name,
                    state: ToolState::Pending { input },
                } => Some((id.clone(), name.clone(), input.clone())),
                _ => None,
            })
            .collect()
    }

    /// Index of the Tool part with the given id.
    pub fn tool_part_index(&self, id: &str) -> Option<usize> {
        self.parts
            .iter()
            .position(|p| matches!(p, Part::Tool { id: pid, .. } if pid == id))
    }

    /// Mark a tool part as running.
    pub fn mark_tool_running(&mut self, id: &str, started_at: i64) -> bool {
        let Some(idx) = self.tool_part_index(id) else {
            return false;
        };
        let Part::Tool { name, state, .. } = &mut self.parts[idx] else {
            return false;
        };
        let ToolState::Pending { input } = state else {
            return false;
        };
        let input = input.clone();
        let name = name.clone();
        self.parts[idx] = Part::Tool {
            id: id.to_string(),
            name,
            state: ToolState::Running { input, started_at },
        };
        true
    }

    /// Mark a tool part as completed.
    pub fn mark_tool_completed(
        &mut self,
        id: &str,
        output: String,
        title: String,
        structured: Option<Value>,
    ) -> bool {
        self.transition_tool_state(id, |state| {
            let input = state.input().clone();
            ToolState::Completed {
                input,
                output,
                title,
                structured,
            }
        })
    }

    /// Mark a tool part as failed.
    pub fn mark_tool_error(&mut self, id: &str, error: String) -> bool {
        self.transition_tool_state(id, |state| {
            let input = state.input().clone();
            ToolState::Error { input, error }
        })
    }

    /// Replace the state of the tool part with the given id.
    fn transition_tool_state<F>(&mut self, id: &str, f: F) -> bool
    where
        F: FnOnce(&ToolState) -> ToolState,
    {
        let Some(idx) = self.tool_part_index(id) else {
            return false;
        };
        let Part::Tool { state, .. } = &mut self.parts[idx] else {
            return false;
        };
        *state = f(state);
        true
    }

    /// True when the message has no parts yet.
    pub fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UsageTotals {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_input_tokens: Option<u64>,
}

impl UsageTotals {
    pub fn add(
        &mut self,
        input: u64,
        output: u64,
        cost: Option<f64>,
        cache_read: Option<u64>,
        cache_write: Option<u64>,
    ) {
        self.input_tokens += input;
        self.output_tokens += output;
        if let Some(c) = cost {
            self.cost = Some(self.cost.unwrap_or(0.0) + c);
        }
        if let Some(r) = cache_read {
            self.cache_read_input_tokens = Some(self.cache_read_input_tokens.unwrap_or(0) + r);
        }
        if let Some(w) = cache_write {
            self.cache_creation_input_tokens =
                Some(self.cache_creation_input_tokens.unwrap_or(0) + w);
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub id: Uuid,
    /// Normalized directory the session is bound to (instance key).
    pub directory: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Human-readable name assigned by the orchestrator (e.g. `auth-flow-audit`).
    /// Separate from `title` which is prompt-derived; `alias` is set once at
    /// spawn time and never overwritten by the title heuristic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,
    pub agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<(Uuid, usize)>,
    pub created_at: i64,
    pub updated_at: i64,
    #[serde(default)]
    pub usage: UsageTotals,
    /// Tokens the provider read for the *last* LLM call of the most recent
    /// turn (input + cache read + cache write): the live size of the context
    /// window in use. Unlike `usage`, this is not cumulative. `None` until
    /// the first turn completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_used: Option<u64>,
    /// Model that produced `context_used`. The context window is resolved
    /// live from the model catalog at response time (never persisted), so a
    /// catalog update takes effect without a migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share: Option<Value>,
}

impl Session {
    pub fn new(directory: impl Into<String>, agent: impl Into<String>) -> Self {
        let now = now_ms();
        Self {
            id: Uuid::new_v4(),
            directory: directory.into(),
            title: None,
            alias: None,
            agent: agent.into(),
            model: None,
            parent: None,
            created_at: now,
            updated_at: now,
            usage: UsageTotals::default(),
            context_used: None,
            context_model: None,
            share: None,
        }
    }

    pub fn touch(&mut self) {
        self.updated_at = now_ms();
    }
}

#[cfg(test)]
mod image_tests {
    use super::*;

    #[test]
    fn image_part_serde_round_trip() {
        let mut m = Message::user("look");
        m.parts.push(Part::Image {
            media_type: "image/png".to_string(),
            data: "aGVsbG8=".to_string(),
            name: Some("shot.png".to_string()),
        });
        let v = serde_json::to_value(&m).unwrap();
        assert_eq!(v["parts"][1]["type"], "image");
        assert_eq!(v["parts"][1]["media_type"], "image/png");
        assert_eq!(v["parts"][1]["data"], "aGVsbG8=");
        let back: Message = serde_json::from_value(v).unwrap();
        assert_eq!(back.parts.len(), 2);
        // text_content ignores images.
        assert_eq!(back.text_content(), "look");
        assert_eq!(
            back.image_parts(),
            vec![(
                "image/png".to_string(),
                "aGVsbG8=".to_string(),
                Some("shot.png".to_string())
            )]
        );
        // Legacy payload without name still deserializes.
        let legacy = serde_json::json!({
            "id": "00000000-0000-0000-0000-000000000001",
            "role": "user",
            "parts": [{"type": "image", "media_type": "image/jpeg", "data": "eA=="}],
        });
        let legacy_msg: Message = serde_json::from_value(legacy).unwrap();
        assert_eq!(
            legacy_msg.image_parts(),
            vec![("image/jpeg".to_string(), "eA==".to_string(), None)]
        );
    }

    #[test]
    fn user_with_images_keeps_only_image_parts() {
        let m = Message::user_with_images(
            "hi",
            vec![
                Part::Text {
                    text: "nope".to_string(),
                },
                Part::Image {
                    media_type: "image/png".to_string(),
                    data: "eA==".to_string(),
                    name: None,
                },
            ],
        );
        assert_eq!(m.parts.len(), 2);
        assert!(matches!(m.parts[0], Part::Text { .. }));
        assert!(matches!(m.parts[1], Part::Image { .. }));
        assert_eq!(m.text_content(), "hi");
    }
}
