use async_trait::async_trait;
use serde_json::{Value, json};

use crate::tool::{Tool, ToolCtx, ToolOutput};

/// Show a unified diff between two files (or a file and a string).
///
/// Portable replacement for `diff -u` via the `bash` tool. Implemented with
/// Myers' O(ND) algorithm, so large files stay fast.
pub struct Diff;

#[async_trait]
impl Tool for Diff {
    fn name(&self) -> &str {
        "diff"
    }

    fn description(&self) -> &str {
        "Compare two files (or a file against given text) and return a unified diff. Portable alternative to `diff -u` / `git diff` via bash."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path_a": {
                    "type": "string",
                    "description": "First file (the 'old' side), relative to the project root."
                },
                "path_b": {
                    "type": "string",
                    "description": "Second file (the 'new' side), relative to the project root. Omit when using `content_b`."
                },
                "content_b": {
                    "type": "string",
                    "description": "Text to compare against `path_a` instead of a second file."
                },
                "context": {
                    "type": "integer",
                    "description": "Lines of surrounding context to include. Defaults to 3."
                }
            },
            "required": ["path_a"]
        })
    }

    fn is_read_only(&self) -> bool {
        true
    }

    async fn execute(&self, ctx: ToolCtx, args: Value) -> ToolOutput {
        let Some(path_a) = args.get("path_a").and_then(|v| v.as_str()) else {
            return ToolOutput::new("error: missing required parameter 'path_a'", "diff");
        };
        let path_b = args.get("path_b").and_then(|v| v.as_str());
        let content_b = args.get("content_b").and_then(|v| v.as_str());
        let context = args
            .get("context")
            .and_then(|v| v.as_u64())
            .unwrap_or(3)
            .min(64) as usize;

        if path_b.is_none() && content_b.is_none() {
            return ToolOutput::new("error: provide either 'path_b' or 'content_b'", "diff");
        }

        let text_a = match tokio::fs::read_to_string(ctx.root.join(path_a)).await {
            Ok(t) => t,
            Err(e) => {
                return ToolOutput::new(format!("error: failed to read {path_a}: {e}"), "diff");
            }
        };

        let (label_b, text_b) = match (path_b, content_b) {
            (Some(p), _) => match tokio::fs::read_to_string(ctx.root.join(p)).await {
                Ok(t) => (p.to_string(), t),
                Err(e) => {
                    return ToolOutput::new(format!("error: failed to read {p}: {e}"), "diff");
                }
            },
            (None, Some(c)) => ("(given content)".to_string(), c.to_string()),
            (None, None) => unreachable!("checked above"),
        };

        let a: Vec<&str> = text_a.lines().collect();
        let b: Vec<&str> = text_b.lines().collect();

        let edits = myers(&a, &b);
        let body = unified(&edits, path_a, &label_b, context);

        ToolOutput::new(body, format!("diff {path_a} {label_b}"))
    }
}

/// A single edit in the diff script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op<'a> {
    Keep(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

/// Myers' greedy diff (O(ND)) producing a keep/delete/insert script.
fn myers<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<Op<'a>> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let max = (n + m) as usize;
    if max == 0 {
        return Vec::new();
    }
    let offset = max as isize;
    let mut v = vec![0isize; 2 * max + 1];
    let mut trace: Vec<Vec<isize>> = Vec::new();

    for d in 0..=max as isize {
        // Snapshot `v` *before* this round so backtracking can rewind.
        trace.push(v.clone());
        let mut k = -d;
        while k <= d {
            let idx = (k + offset) as usize;
            let mut x = if k == -d || (k != d && v[idx - 1] < v[idx + 1]) {
                v[idx + 1]
            } else {
                v[idx - 1] + 1
            };
            let mut y = x - k;
            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }
            v[idx] = x;
            if x >= n && y >= m {
                return backtrack(&trace, a, b, d, offset);
            }
            k += 2;
        }
    }
    Vec::new()
}

/// Walk the recorded `trace` backwards to materialize the edit script.
fn backtrack<'a>(
    trace: &[Vec<isize>],
    a: &[&'a str],
    b: &[&'a str],
    d: isize,
    offset: isize,
) -> Vec<Op<'a>> {
    let mut x = a.len() as isize;
    let mut y = b.len() as isize;
    let mut out: Vec<Op<'a>> = Vec::new();

    for dd in (0..=d).rev() {
        let v = &trace[dd as usize];
        let k = x - y;
        let idx = (k + offset) as usize;
        let prev_k = if k == -dd || (k != dd && v[idx - 1] < v[idx + 1]) {
            k + 1
        } else {
            k - 1
        };
        let prev_x = v[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;

        // Diagonal (matching) moves.
        while x > prev_x && y > prev_y {
            out.push(Op::Keep(a[(x - 1) as usize]));
            x -= 1;
            y -= 1;
        }
        // Exactly one of the two coordinates moved past the diagonal:
        // `x == prev_x` means a line was inserted, otherwise one was deleted.
        if dd > 0 {
            if x == prev_x {
                out.push(Op::Insert(b[(y - 1) as usize]));
            } else {
                out.push(Op::Delete(a[(x - 1) as usize]));
            }
        }
        x = prev_x;
        y = prev_y;
    }

    out.reverse();
    out
}

/// Render an edit script as a unified diff with `context` lines of context.
fn unified(ops: &[Op<'_>], label_a: &str, label_b: &str, context: usize) -> String {
    let changed: Vec<usize> = ops
        .iter()
        .enumerate()
        .filter(|(_, op)| !matches!(op, Op::Keep(_)))
        .map(|(i, _)| i)
        .collect();

    if changed.is_empty() {
        return format!("{label_a} and {label_b} are identical");
    }

    // Group nearby changes into hunks.
    let mut groups: Vec<(usize, usize)> = Vec::new();
    let mut start = changed[0].saturating_sub(context);
    let mut end = changed[0];
    for &c in &changed[1..] {
        if c > end + context + 1 {
            groups.push((start, end));
            start = c.saturating_sub(context);
        }
        end = c;
    }
    groups.push((start, end));

    let mut out = String::new();
    out.push_str(&format!("--- {label_a}\n"));
    out.push_str(&format!("+++ {label_b}\n"));

    for (mut from, mut to) in groups {
        to = (to + context).min(ops.len() - 1);
        from = from.min(to);

        // Count old/new lines consumed before the hunk.
        let (old_before, new_before) =
            ops[..from]
                .iter()
                .fold((0usize, 0usize), |(o, n), op| match op {
                    Op::Keep(_) => (o + 1, n + 1),
                    Op::Delete(_) => (o + 1, n),
                    Op::Insert(_) => (o, n + 1),
                });

        let slice = &ops[from..=to];
        let old_count = slice
            .iter()
            .filter(|op| matches!(op, Op::Keep(_) | Op::Delete(_)))
            .count();
        let new_count = slice
            .iter()
            .filter(|op| matches!(op, Op::Keep(_) | Op::Insert(_)))
            .count();

        // Unified diff uses the line *before* the range when the range is empty.
        let old_start = if old_count == 0 { old_before } else { old_before + 1 };
        let new_start = if new_count == 0 { new_before } else { new_before + 1 };

        out.push_str(&format!(
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@\n"
        ));
        for op in slice {
            match op {
                Op::Keep(line) => out.push_str(&format!(" {line}\n")),
                Op::Delete(line) => out.push_str(&format!("-{line}\n")),
                Op::Insert(line) => out.push_str(&format!("+{line}\n")),
            }
        }
    }

    out.trim_end().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ops<'a>(a: &[&'a str], b: &[&'a str]) -> Vec<Op<'a>> {
        myers(a, b)
    }

    #[test]
    fn identical_inputs_produce_only_keeps() {
        let result = ops(&["a", "b"], &["a", "b"]);
        assert_eq!(
            result,
            vec![Op::Keep("a"), Op::Keep("b")],
            "identical sequences must be all keeps"
        );
    }

    #[test]
    fn round_trips_a_through_the_script() {
        // Applying the script to `a` must reconstruct `b`.
        let a = ["one", "two", "three", "four"];
        let b = ["one", "2", "three", "four", "five"];
        let edited: Vec<&str> = ops(&a, &b)
            .into_iter()
            .filter_map(|op| match op {
                Op::Keep(l) | Op::Insert(l) => Some(l),
                Op::Delete(_) => None,
            })
            .collect();
        assert_eq!(edited, b.to_vec());
    }

    #[test]
    fn round_trips_via_deletes_too() {
        let a = ["a", "b", "c", "d"];
        let b = ["a", "d"];
        let kept: Vec<&str> = ops(&a, &b)
            .into_iter()
            .filter_map(|op| match op {
                Op::Keep(l) | Op::Insert(l) => Some(l),
                Op::Delete(_) => None,
            })
            .collect();
        assert_eq!(kept, b.to_vec());
    }

    #[test]
    fn pure_insert_and_pure_delete() {
        let ins: Vec<Op<'_>> = ops(&[], &["x", "y"]);
        assert_eq!(ins, vec![Op::Insert("x"), Op::Insert("y")]);

        let del: Vec<Op<'_>> = ops(&["x", "y"], &[]);
        assert_eq!(del, vec![Op::Delete("x"), Op::Delete("y")]);
    }

    #[test]
    fn unified_header_counts_match_body() {
        let a = ["a", "b", "c"];
        let b = ["a", "x", "c"];
        let text = unified(&ops(&a, &b), "a.txt", "b.txt", 3);
        assert!(text.contains("--- a.txt"));
        assert!(text.contains("+++ b.txt"));
        assert!(text.contains("@@ -1,3 +1,3 @@"), "got:\n{text}");
        assert!(text.contains("-b\n"));
        assert!(text.contains("+x\n"));
    }

    #[test]
    fn unified_is_patch_parseable() {
        // Header counts must equal the number of marked lines in the body.
        let a = ["one", "two", "three", "four", "five"];
        let b = ["one", "TWO", "three", "four", "FIVE", "six"];
        let text = unified(&ops(&a, &b), "a", "b", 1);
        for header in text.lines().filter(|l| l.starts_with("@@")) {
            let parts: Vec<&str> = header.split_whitespace().collect();
            let old_count: usize = parts[1].split(',').nth(1).unwrap().parse().unwrap();
            let new_count: usize = parts[2].split(',').nth(1).unwrap().parse().unwrap();
            // Recount the body lines that follow this header.
            let body: Vec<&str> = text
                .lines()
                .skip_while(|l| *l != header)
                .skip(1)
                .take_while(|l| !l.starts_with("@@"))
                .collect();
            let old_seen = body
                .iter()
                .filter(|l| l.starts_with(' ') || l.starts_with('-'))
                .count();
            let new_seen = body
                .iter()
                .filter(|l| l.starts_with(' ') || l.starts_with('+'))
                .count();
            assert_eq!(old_count, old_seen, "old count mismatch in {text}");
            assert_eq!(new_count, new_seen, "new count mismatch in {text}");
        }
    }

    #[test]
    fn identical_files_are_reported_as_such() {
        let text = unified(&ops(&["a"], &["a"]), "a.txt", "b.txt", 3);
        assert!(text.contains("identical"), "got: {text}");
    }
}
