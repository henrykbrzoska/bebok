//! `ask_user` tool: a blocking, multiple-choice question to the human.
//!
//! The model calls `ask_user` when a request is ambiguous and a wrong guess
//! would cost real work. The call suspends the turn (no polling): the tool
//! registers a oneshot under a fresh request id, publishes `question.asked`,
//! and waits for `POST /session/{id}/question/{requestID}` (or the turn's
//! abort token, or [`QUESTION_TIMEOUT`]). The answers become the tool output.
//!
//! The pending registry is process-wide (like `SshRegistry`) rather than on
//! `SessionState`, so the route can answer without touching the store.
//!
//! Lives in `bebok-core` (not `bebok-tools`) for the same reason as the code
//! index tools: it needs `InstanceStore` for the event bus and session state,
//! and `core -> tools -> mcp -> core` must not cycle.

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, OnceLock, Weak};
use std::time::Duration;

use async_trait::async_trait;
use bebok_tools::{Tool, ToolCtx, ToolOutput};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use tokio::sync::oneshot;
use uuid::Uuid;

use crate::event::{Event, EventBus};
use crate::store::InstanceStore;

/// Tool name as advertised to the model and whitelisted in presets.
pub const QUESTION_TOOL: &str = "ask_user";

/// Longest a turn waits for an answer before the tool fails (the model then
/// has to decide on its own, so the failure is reported as a tool error).
pub const QUESTION_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Maximum questions per `ask_user` call.
pub const MAX_QUESTIONS: usize = 4;
/// Minimum / maximum options per question.
pub const MIN_OPTIONS: usize = 2;
pub const MAX_OPTIONS: usize = 4;

/// Sub-agents never receive `ask_user`: a child has no human in front of it
/// (its answers would land in the orchestrator's transcript, not the user's).
/// Only the main-thread session advertises the tool.
pub fn withheld_from(is_subagent: bool) -> bool {
    is_subagent
}

// ---------------------------------------------------------------------------
// Arguments
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct QuestionOption {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Question {
    /// Stable key; the answer object is keyed by it.
    pub id: String,
    pub question: String,
    /// Short label shown as a chip (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub header: Option<String>,
    pub options: Vec<QuestionOption>,
    #[serde(default, rename = "multiSelect")]
    pub multi_select: bool,
}

#[derive(Debug, Deserialize)]
struct QuestionArgs {
    questions: Vec<Question>,
}

/// Validate and parse the tool arguments. Rejects anything the UI could not
/// render: empty ids/text, duplicate ids, too many questions, or an option
/// count outside `MIN_OPTIONS..=MAX_OPTIONS`.
pub fn parse_questions(args: Value) -> Result<Vec<Question>, String> {
    let parsed: QuestionArgs =
        serde_json::from_value(args).map_err(|e| format!("invalid ask_user arguments: {e}"))?;
    let questions = parsed.questions;
    if questions.is_empty() {
        return Err("ask_user needs at least one question".into());
    }
    if questions.len() > MAX_QUESTIONS {
        return Err(format!(
            "ask_user takes at most {MAX_QUESTIONS} questions per call, got {}",
            questions.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for q in &questions {
        let id = q.id.trim();
        if id.is_empty() {
            return Err("every question needs a non-empty id".into());
        }
        if !seen.insert(id.to_string()) {
            return Err(format!("duplicate question id '{id}'"));
        }
        if q.question.trim().is_empty() {
            return Err(format!("question '{id}' has empty text"));
        }
        if q.options.len() < MIN_OPTIONS || q.options.len() > MAX_OPTIONS {
            return Err(format!(
                "question '{id}' needs {MIN_OPTIONS}-{MAX_OPTIONS} options, got {}",
                q.options.len()
            ));
        }
        if q.options.iter().any(|o| o.label.trim().is_empty()) {
            return Err(format!("question '{id}' has an option with an empty label"));
        }
    }
    Ok(questions)
}

/// Validate a client answer object: `{ <id>: string | string[] }`.
pub fn parse_answers(value: &Value) -> Result<Map<String, Value>, String> {
    let obj = value
        .as_object()
        .ok_or_else(|| "answers must be a JSON object keyed by question id".to_string())?;
    for (id, v) in obj {
        let ok = match v {
            Value::String(_) => true,
            Value::Array(items) => items.iter().all(Value::is_string),
            _ => false,
        };
        if !ok {
            return Err(format!("answer for '{id}' must be a string or an array of strings"));
        }
    }
    Ok(obj.clone())
}

// ---------------------------------------------------------------------------
// Pending registry (process-wide)
// ---------------------------------------------------------------------------

type Answers = Map<String, Value>;

struct PendingQuestion {
    session_id: String,
    sender: oneshot::Sender<Answers>,
    /// `{requestID, sessionID, messageIndex, questions}` as published.
    properties: Value,
}

/// Why an answer was refused.
#[derive(Debug, PartialEq, Eq)]
pub enum AnswerError {
    /// No such pending question for this session (already answered, timed
    /// out, aborted, or the id belongs to another session).
    NotFound,
    /// The answer body does not match the question.
    Invalid(String),
}

fn registry() -> MutexGuard<'static, HashMap<String, PendingQuestion>> {
    static REGISTRY: OnceLock<Mutex<HashMap<String, PendingQuestion>>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| Mutex::new(HashMap::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Pending questions of one session, for clients that reconnected after
/// missing the `question.asked` event.
pub fn pending_questions(session_id: &str) -> Vec<Value> {
    registry()
        .values()
        .filter(|p| p.session_id == session_id)
        .map(|p| p.properties.clone())
        .collect()
}

/// Deliver an answer to a pending question. The first answer wins; the entry
/// is removed so a second POST gets [`AnswerError::NotFound`].
pub fn answer_question(
    session_id: &str,
    request_id: &str,
    answers: &Value,
) -> Result<(), AnswerError> {
    let answers = parse_answers(answers).map_err(AnswerError::Invalid)?;
    let mut pending = registry();
    let entry = match pending.get(request_id) {
        Some(p) if p.session_id == session_id => p,
        _ => return Err(AnswerError::NotFound),
    };
    let known: Vec<&str> = entry
        .properties
        .get("questions")
        .and_then(Value::as_array)
        .map(|qs| {
            qs.iter()
                .filter_map(|q| q.get("id")?.as_str())
                .collect::<Vec<&str>>()
        })
        .unwrap_or_default();
    if let Some(unknown) = answers.keys().find(|k| !known.contains(&k.as_str())) {
        return Err(AnswerError::Invalid(format!("unknown question id '{unknown}'")));
    }
    let Some(entry) = pending.remove(request_id) else {
        return Err(AnswerError::NotFound);
    };
    // The receiver is gone when the turn already ended; the answer is dropped.
    let _ = entry.sender.send(answers);
    Ok(())
}

// ---------------------------------------------------------------------------
// Tool
// ---------------------------------------------------------------------------

/// The `ask_user` tool. Holds a weak store reference (the store owns the
/// instance, whose registry owns this tool) for the event bus and session.
pub struct AskUserTool {
    store: Weak<InstanceStore>,
}

impl AskUserTool {
    pub fn new(store: Weak<InstanceStore>) -> Self {
        Self { store }
    }
}

enum Outcome {
    Answered(Answers),
    Aborted,
    TimedOut,
}

fn error_output(message: &str) -> ToolOutput {
    ToolOutput::new(
        serde_json::to_string(&json!({ "ok": false, "error": message })).unwrap_or_default(),
        QUESTION_TOOL,
    )
}

#[async_trait]
impl Tool for AskUserTool {
    fn name(&self) -> &str {
        QUESTION_TOOL
    }

    fn description(&self) -> &str {
        "Ask the user one to four multiple-choice questions and wait for the answers. \
         Use it when the request is ambiguous and a wrong guess would cost real work. \
         Each question has 2-4 options; the user can also type their own answer. \
         Returns {ok, answers} where answers maps each question id to a string or \
         string array."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_QUESTIONS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {
                                "type": "string",
                                "description": "Stable key; the answer is keyed by it"
                            },
                            "question": {
                                "type": "string",
                                "description": "The full question, ending with a question mark"
                            },
                            "header": {
                                "type": "string",
                                "description": "Short chip label (about 12 characters)"
                            },
                            "options": {
                                "type": "array",
                                "minItems": MIN_OPTIONS,
                                "maxItems": MAX_OPTIONS,
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string" },
                                        "description": { "type": "string" }
                                    },
                                    "required": ["label"]
                                }
                            },
                            "multiSelect": {
                                "type": "boolean",
                                "description": "Allow more than one option"
                            }
                        },
                        "required": ["id", "question", "options"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    /// Read-only: asking changes no project state, so it never raises a
    /// permission popup (the question popup is its own consent surface).
    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let questions = match parse_questions(args) {
            Ok(q) => q,
            Err(e) => return error_output(&e),
        };
        let Some(store) = self.store.upgrade() else {
            return error_output("ask_user: the engine is shutting down");
        };
        let session = match Uuid::parse_str(&ctx.session_id) {
            Ok(id) => match store.open_session(id).await {
                Ok(s) => s,
                Err(e) => return error_output(&format!("ask_user: {e}")),
            },
            Err(_) => return error_output("ask_user: invalid session id"),
        };
        // The assistant message this call belongs to is the last one written
        // before tools run, so the UI can attach the question to it.
        let message_index = session.messages_snapshot().await.len().saturating_sub(1);

        let request_id = Uuid::new_v4().to_string();
        let questions_value = serde_json::to_value(&questions).unwrap_or(Value::Null);
        let properties = json!({
            "requestID": request_id,
            "sessionID": ctx.session_id,
            "messageIndex": message_index,
            "questions": questions_value,
        });
        let (tx, rx) = oneshot::channel();
        registry().insert(
            request_id.clone(),
            PendingQuestion {
                session_id: ctx.session_id.clone(),
                sender: tx,
                properties: properties.clone(),
            },
        );

        let bus: EventBus = store.bus();
        bus.publish(Event::question_asked(
            session.directory(),
            &ctx.session_id,
            &request_id,
            message_index,
            properties["questions"].clone(),
        ));

        let outcome = tokio::select! {
            biased;
            _ = ctx.abort.cancelled() => Outcome::Aborted,
            answer = rx => match answer {
                Ok(answers) => Outcome::Answered(answers),
                Err(_) => Outcome::Aborted,
            },
            _ = tokio::time::sleep(QUESTION_TIMEOUT) => Outcome::TimedOut,
        };
        // Drop the entry if still pending (timeout / abort); a delivered
        // answer has already removed it.
        registry().remove(&request_id);

        let (label, answers) = match &outcome {
            Outcome::Answered(a) => ("answered", Some(a.clone())),
            Outcome::Aborted => ("aborted", None),
            Outcome::TimedOut => ("timeout", None),
        };
        bus.publish(Event::question_resolved(
            session.directory(),
            &ctx.session_id,
            &request_id,
            message_index,
            label,
            answers.clone().map(Value::Object),
        ));

        match outcome {
            Outcome::Answered(answers) => ToolOutput::new(
                serde_json::to_string(&json!({ "ok": true, "answers": answers }))
                    .unwrap_or_default(),
                QUESTION_TOOL,
            ),
            Outcome::Aborted => ToolOutput::new("ask_user: aborted", QUESTION_TOOL),
            Outcome::TimedOut => error_output(&format!(
                "ask_user: no answer within {} minutes; decide without the user or stop",
                QUESTION_TIMEOUT.as_secs() / 60
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn two_option_question(id: &str) -> Value {
        json!({
            "id": id,
            "question": "Which?",
            "options": [{"label": "a"}, {"label": "b"}]
        })
    }

    #[test]
    fn accepts_a_well_formed_call() {
        let qs = parse_questions(json!({
            "questions": [two_option_question("db"), {
                "id": "scope",
                "question": "Where?",
                "header": "Scope",
                "multiSelect": true,
                "options": [{"label": "api", "description": "server only"}, {"label": "ui"}]
            }]
        }))
        .unwrap();
        assert_eq!(qs.len(), 2);
        assert!(qs[1].multi_select);
        assert_eq!(qs[1].options[0].description.as_deref(), Some("server only"));
    }

    #[test]
    fn rejects_out_of_range_shapes() {
        assert!(parse_questions(json!({"questions": []})).is_err());
        let five: Vec<Value> = (0..5).map(|i| two_option_question(&format!("q{i}"))).collect();
        assert!(parse_questions(json!({"questions": five})).is_err());
        let one_opt = json!({"id": "x", "question": "?", "options": [{"label": "only"}]});
        assert!(parse_questions(json!({"questions": [one_opt]})).is_err());
        let dup = vec![two_option_question("same"), two_option_question("same")];
        assert!(parse_questions(json!({"questions": dup})).is_err());
        let blank = json!({"id": " ", "question": "?", "options": [{"label": "a"}, {"label": "b"}]});
        assert!(parse_questions(json!({"questions": [blank]})).is_err());
    }

    #[test]
    fn answers_must_be_strings_or_string_arrays() {
        assert!(parse_answers(&json!({"db": "postgres", "scope": ["api", "ui"]})).is_ok());
        assert!(parse_answers(&json!({"db": 3})).is_err());
        assert!(parse_answers(&json!({"scope": ["api", 1]})).is_err());
        assert!(parse_answers(&json!(["db"])).is_err());
    }

    #[test]
    fn main_session_only() {
        assert!(withheld_from(true));
        assert!(!withheld_from(false));
    }

    #[tokio::test]
    async fn answer_is_delivered_once_to_the_owning_session_only() {
        let request_id = Uuid::new_v4().to_string();
        let (tx, rx) = oneshot::channel();
        let properties = json!({
            "requestID": request_id,
            "questions": [{"id": "db", "question": "Which?", "options": []}],
        });
        registry().insert(
            request_id.clone(),
            PendingQuestion {
                session_id: "session-a".into(),
                sender: tx,
                properties,
            },
        );
        assert_eq!(pending_questions("session-a").len(), 1);

        let body = json!({"db": "postgres"});
        assert_eq!(
            answer_question("session-b", &request_id, &body),
            Err(AnswerError::NotFound)
        );
        assert!(matches!(
            answer_question("session-a", &request_id, &json!({"nope": "x"})),
            Err(AnswerError::Invalid(_))
        ));
        assert_eq!(answer_question("session-a", &request_id, &body), Ok(()));
        assert_eq!(
            answer_question("session-a", &request_id, &body),
            Err(AnswerError::NotFound)
        );
        let got = rx.await.unwrap();
        assert_eq!(got.get("db"), Some(&json!("postgres")));
        assert!(pending_questions("session-a")
            .iter()
            .all(|p| p["requestID"] != json!(request_id)));
    }
}
