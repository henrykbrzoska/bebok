use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// `ask_user` (1.8): a structured questionnaire for the moments a model would
/// otherwise guess - one or more numbered questions, each single- or
/// multi-select with lettered options, always with a free-text "your own
/// answer" slot appended by the client. The tool does not block: it ends the
/// turn (`structured.awaitUser`, see the turn loop) and the client posts the
/// answers back as the next user message in the compact form
/// `1A, 2BC, 3E(their own words)`.
pub struct AskUser;

/// Upper bounds keep the card readable and the prompt small.
pub const MAX_QUESTIONS: usize = 8;
pub const MAX_OPTIONS: usize = 12;

#[derive(Debug, Deserialize)]
struct Args {
    questions: Vec<QuestionArg>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Debug, Deserialize)]
struct QuestionArg {
    text: String,
    #[serde(default)]
    multi: bool,
    #[serde(default)]
    options: Vec<String>,
    /// Off only for questions where a free answer makes no sense.
    #[serde(default = "default_true")]
    allow_custom: bool,
}

fn default_true() -> bool {
    true
}

/// Letters for options: A..Z (12 max, so never beyond L).
pub fn option_letter(index: usize) -> char {
    (b'A' + index as u8) as char
}

#[async_trait]
impl Tool for AskUser {
    fn name(&self) -> &str {
        "ask_user"
    }

    fn description(&self) -> &str {
        "Ask the user to choose between options when a decision is unclear (which fields, which approach, which of several files). \
         Renders a numbered questionnaire; each question is single-select or multi-select with lettered options and the user can \
         always add their own answer. The turn ENDS after this call - the user's choices arrive as the next user message, \
         in the form `1A, 2BC, 3E(their own text)`. Ask everything you need in one call (up to 8 questions); do not ask what you can decide yourself."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "title": {
                    "type": "string",
                    "description": "Optional short heading, e.g. 'New user form - a few details'."
                },
                "questions": {
                    "type": "array",
                    "minItems": 1,
                    "maxItems": MAX_QUESTIONS,
                    "items": {
                        "type": "object",
                        "properties": {
                            "text": { "type": "string", "description": "The question, one clear sentence." },
                            "multi": { "type": "boolean", "description": "true = the user may pick several options (checkboxes); false = exactly one (radio). Default false." },
                            "options": {
                                "type": "array",
                                "maxItems": MAX_OPTIONS,
                                "items": { "type": "string" },
                                "description": "Concrete choices, short (a few words each). The client appends a free-text option automatically."
                            },
                            "allow_custom": { "type": "boolean", "description": "Default true. Set false only when a free answer makes no sense." }
                        },
                        "required": ["text"]
                    }
                }
            },
            "required": ["questions"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, _ctx: ToolCtx, args: Value) -> ToolOutput {
        let parsed: Args = match serde_json::from_value(args) {
            Ok(a) => a,
            Err(e) => return ToolOutput::new(format!("invalid arguments: {e}"), "ask_user"),
        };
        if parsed.questions.is_empty() {
            return ToolOutput::new(
                "invalid arguments: at least one question is required",
                "ask_user",
            );
        }
        if parsed.questions.len() > MAX_QUESTIONS {
            return ToolOutput::new(
                format!("invalid arguments: at most {MAX_QUESTIONS} questions per call"),
                "ask_user",
            );
        }
        let mut questions = Vec::with_capacity(parsed.questions.len());
        let mut summary = String::new();
        if let Some(title) = parsed.title.as_deref().filter(|t| !t.trim().is_empty()) {
            summary.push_str(title.trim());
            summary.push('\n');
        }
        for (i, q) in parsed.questions.iter().enumerate() {
            let options: Vec<String> = q
                .options
                .iter()
                .map(|o| o.trim().to_string())
                .filter(|o| !o.is_empty())
                .take(MAX_OPTIONS)
                .collect();
            summary.push_str(&format!(
                "{}. {}{}\n",
                i + 1,
                q.text.trim(),
                if q.multi { " (multi)" } else { "" }
            ));
            for (j, o) in options.iter().enumerate() {
                summary.push_str(&format!("   {}) {}\n", option_letter(j), o));
            }
            if q.allow_custom {
                summary.push_str(&format!(
                    "   {}) <own answer>\n",
                    option_letter(options.len())
                ));
            }
            questions.push(json!({
                "number": i + 1,
                "text": q.text.trim(),
                "multi": q.multi,
                "options": options,
                "allowCustom": q.allow_custom,
            }));
        }
        let text = format!(
            "Questionnaire shown to the user. The turn ends here; their answers arrive as the next user message \
             as `<question number><letters>` items, e.g. `1A, 2BC` (a trailing `(…)` is their own free-text answer).\n{summary}"
        );
        ToolOutput::new(text, "ask_user").with_structured(json!({
            "awaitUser": true,
            "survey": { "title": parsed.title, "questions": questions },
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn ctx() -> ToolCtx {
        ToolCtx {
            root: std::env::temp_dir(),
            session_id: "s".into(),
            abort: CancellationToken::new(),
        }
    }

    #[tokio::test]
    async fn renders_numbered_questions_and_flags_the_turn_end() {
        let out = AskUser
            .execute(
                ctx(),
                json!({
                    "title": "User form",
                    "questions": [
                        { "text": "Which fields?", "multi": true, "options": ["name", "email", "phone"] },
                        { "text": "Auth?", "options": ["password", "magic link"], "allow_custom": false }
                    ]
                }),
            )
            .await;
        let s = out.structured.expect("structured");
        assert_eq!(s["awaitUser"], true);
        assert_eq!(s["survey"]["questions"].as_array().unwrap().len(), 2);
        assert_eq!(s["survey"]["questions"][0]["multi"], true);
        assert_eq!(s["survey"]["questions"][1]["allowCustom"], false);
        assert!(out.text.contains("1. Which fields? (multi)"));
        assert!(out.text.contains("   D) <own answer>"));
        assert!(!out.text.contains("   C) <own answer>"));
    }

    #[tokio::test]
    async fn rejects_missing_questions() {
        let out = AskUser.execute(ctx(), json!({ "questions": [] })).await;
        assert!(out.structured.is_none());
        assert!(out.text.contains("at least one question"));
    }
}
