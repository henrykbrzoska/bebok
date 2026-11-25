//! `ask_user` prompt section: tells the main-thread model when to stop and
//! ask instead of guessing. Injected only when the tool is actually
//! advertised for the request, so the text never promises a tool the model
//! cannot call (sub-agents never get it; see `question_tool::withheld_from`).

/// Heading and body of the section. Kept as one constant so the marker the
/// tests look for stays in one place.
pub const QUESTION_SECTION: &str = "## Asking the user (ask_user)\n\
Use the `ask_user` tool when the request is ambiguous and a wrong guess would \
cost real work (edits to the wrong files, a wrong design choice, a destructive \
or irreversible step). Ask instead of guessing.\n\
- Ask at most 4 questions per call, each with 2-4 options. The user can always \
type their own answer, so do not add an \"Other\" option.\n\
- Do not ask what you can settle by reading the code or running a search.\n\
- Do not ask for permission for routine steps the user already requested.\n\
- The result is `{\"ok\": true, \"answers\": {<question id>: string | string[]}}`. \
Use the answers as given. If the result is an error (no answer in time), decide \
on your own and say what you assumed.";

/// The section when `ask_user` is advertised for this request, else `None`.
pub fn question_section(tool_advertised: bool) -> Option<&'static str> {
    tool_advertised.then_some(QUESTION_SECTION)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn section_only_when_the_tool_is_advertised() {
        assert!(question_section(false).is_none());
        let section = question_section(true).expect("advertised tool gets the section");
        assert!(section.contains("Asking the user"));
        assert!(section.contains("ask_user"));
    }
}
