//! Project-wide text search for the Explorer "Find in Files" dialog
//! (Ctrl/Cmd+Shift+F, IntelliJ-style).
//!
//! A gitignore-aware walk over the project with a literal / whole-word /
//! regex matcher and an optional file mask (`*.rs, *.ts`). When the caller
//! already knows which files are most relevant (the `bebok-index` plugin's
//! ranking) they are passed in `priority` and searched first, so a capped
//! result set still shows the best files.

use std::collections::HashSet;
use std::path::Path;

use regex::{Regex, RegexBuilder};

/// Every project file (gitignore-aware, `.git` / `node_modules` / `target` /
/// `.bebok` skipped) as project-relative `/` paths, sorted - the Explorer
/// "Go to File" dialog (Ctrl/Cmd+Shift+N) fuzzy-matches over this list.
/// The flag is `true` when `max_files` cut the walk short.
pub fn list_files(root: &Path, max_files: usize) -> (Vec<String>, bool) {
    let (files, truncated) = super::goto_definition::source_files(root, &[], max_files.max(1));
    let mut out: Vec<String> = files.into_iter().map(|(_, rel)| rel).collect();
    out.sort_unstable();
    (out, truncated)
}

/// Search options (mirrors the dialog toggles).
#[derive(Debug, Clone, Default)]
pub struct TextSearchOptions {
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// Comma/space separated globs over the file *name* (`*.rs, *.ts`);
    /// empty = all files. A leading `!` excludes.
    pub file_mask: String,
    /// Max matches returned.
    pub limit: usize,
    /// Max files walked.
    pub max_files: usize,
}

/// One matching line.
#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct TextHit {
    pub path: String,
    /// 1-based line.
    pub line: usize,
    /// 1-based column (in characters) of the first match on the line.
    pub column: usize,
    /// Match length in characters.
    pub length: usize,
    /// The line, trimmed to 300 chars.
    pub text: String,
}

#[derive(Debug, serde::Serialize)]
pub struct TextSearchResponse {
    pub results: Vec<TextHit>,
    /// Distinct files with at least one match.
    pub files_matched: usize,
    pub files_searched: usize,
    /// True when `limit` or `max_files` cut the search short.
    pub truncated: bool,
}

/// Compile the matcher; `Err` carries a user-facing message.
pub fn build_matcher(query: &str, opts: &TextSearchOptions) -> Result<Regex, String> {
    let body = if opts.regex {
        query.to_string()
    } else {
        regex::escape(query)
    };
    let body = if opts.whole_word {
        format!(r"\b(?:{body})\b")
    } else {
        body
    };
    RegexBuilder::new(&body)
        .case_insensitive(!opts.case_sensitive)
        .size_limit(1 << 20)
        .build()
        .map_err(|e| format!("invalid pattern: {e}"))
}

/// Parsed file mask: include and exclude glob sets.
struct Mask {
    include: Option<globset::GlobSet>,
    exclude: Option<globset::GlobSet>,
}

impl Mask {
    fn parse(mask: &str) -> Result<Self, String> {
        let mut inc = globset::GlobSetBuilder::new();
        let mut exc = globset::GlobSetBuilder::new();
        let (mut n_inc, mut n_exc) = (0, 0);
        for part in mask.split([',', ' ', ';']).filter(|p| !p.is_empty()) {
            let (neg, pat) = match part.strip_prefix('!') {
                Some(p) => (true, p),
                None => (false, part),
            };
            let glob = globset::GlobBuilder::new(pat)
                .case_insensitive(true)
                .build()
                .map_err(|e| format!("invalid file mask '{part}': {e}"))?;
            if neg {
                exc.add(glob);
                n_exc += 1;
            } else {
                inc.add(glob);
                n_inc += 1;
            }
        }
        let build = |b: globset::GlobSetBuilder, n: usize| -> Result<_, String> {
            if n == 0 {
                Ok(None)
            } else {
                b.build().map(Some).map_err(|e| e.to_string())
            }
        };
        Ok(Self {
            include: build(inc, n_inc)?,
            exclude: build(exc, n_exc)?,
        })
    }

    fn accepts(&self, rel: &str) -> bool {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let hit = |set: &globset::GlobSet| set.is_match(name) || set.is_match(rel);
        self.include.as_ref().is_none_or(hit) && !self.exclude.as_ref().is_some_and(hit)
    }
}

/// Search the project. `priority` = project-relative paths to search first
/// (e.g. the code index ranking); unknown / ignored ones are skipped.
pub fn search_text(
    root: &Path,
    query: &str,
    opts: &TextSearchOptions,
    priority: &[String],
) -> Result<TextSearchResponse, String> {
    if query.is_empty() {
        return Err("query must not be empty".into());
    }
    let re = build_matcher(query, opts)?;
    let mask = Mask::parse(&opts.file_mask)?;
    let (files, walk_truncated) =
        super::goto_definition::source_files(root, &[], opts.max_files.max(1));

    // Priority files first (only those the walk also saw: respects .gitignore).
    let known: HashSet<&str> = files.iter().map(|(_, r)| r.as_str()).collect();
    let mut order: Vec<&str> = Vec::with_capacity(files.len());
    let mut seen: HashSet<&str> = HashSet::new();
    for p in priority {
        let p = p.as_str();
        if known.contains(p) && seen.insert(p) {
            order.push(p);
        }
    }
    for (_, r) in &files {
        if seen.insert(r.as_str()) {
            order.push(r.as_str());
        }
    }

    let mut resp = TextSearchResponse {
        results: Vec::new(),
        files_matched: 0,
        files_searched: 0,
        truncated: walk_truncated,
    };
    let limit = opts.limit.max(1);
    'files: for rel in order {
        if !mask.accepts(rel) {
            continue;
        }
        let Ok(bytes) = std::fs::read(root.join(rel)) else {
            continue;
        };
        // Skip binaries and huge files (minified bundles, dumps).
        if bytes.len() > 4 * 1024 * 1024 || bytes.iter().take(8192).any(|b| *b == 0) {
            continue;
        }
        let Ok(text) = String::from_utf8(bytes) else {
            continue;
        };
        resp.files_searched += 1;
        let mut file_hit = false;
        for (i, line) in text.lines().enumerate() {
            let Some(m) = re.find(line) else {
                continue;
            };
            if !file_hit {
                file_hit = true;
                resp.files_matched += 1;
            }
            resp.results.push(TextHit {
                path: rel.to_string(),
                line: i + 1,
                column: line[..m.start()].chars().count() + 1,
                length: m.as_str().chars().count(),
                text: line.chars().take(300).collect(),
            });
            if resp.results.len() >= limit {
                resp.truncated = true;
                break 'files;
            }
        }
    }
    Ok(resp)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            let p = std::env::temp_dir().join(format!("bebok-psearch-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn write(root: &Path, rel: &str, body: &str) {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    fn opts() -> TextSearchOptions {
        TextSearchOptions {
            limit: 100,
            max_files: 1000,
            ..Default::default()
        }
    }

    #[test]
    fn list_files_is_sorted_and_skips_ignored_dirs() {
        let tmp = TempDir::new();
        write(&tmp.0, "src/b.rs", "");
        write(&tmp.0, "a.txt", "");
        write(&tmp.0, "node_modules/x/index.js", "");
        write(&tmp.0, ".bebok/config.json", "{}");
        let (files, truncated) = list_files(&tmp.0, 100);
        assert_eq!(files, vec!["a.txt".to_string(), "src/b.rs".to_string()]);
        assert!(!truncated);
        let (files, truncated) = list_files(&tmp.0, 1);
        assert_eq!(files.len(), 1);
        assert!(truncated);
    }

    #[test]
    fn literal_case_word_regex_and_mask() {
        let d = TempDir::new();
        write(&d.0, "a.rs", "let foo = 1;\nlet foobar = Foo;\n");
        write(&d.0, "b.ts", "const foo = 'x';\n");
        write(&d.0, "bin.dat", "foo\0\0");

        let r = search_text(&d.0, "foo", &opts(), &[]).unwrap();
        assert_eq!(r.results.len(), 3, "{r:?}"); // binary skipped, one hit per line
        assert_eq!(r.files_matched, 2);

        let mut o = opts();
        o.case_sensitive = true;
        o.whole_word = true;
        let r = search_text(&d.0, "Foo", &o, &[]).unwrap();
        assert_eq!(r.results.len(), 1);
        assert_eq!((r.results[0].line, r.results[0].column), (2, 14));

        let mut o = opts();
        o.file_mask = "*.ts".into();
        let r = search_text(&d.0, "foo", &o, &[]).unwrap();
        assert_eq!(r.results.len(), 1);
        assert_eq!(r.results[0].path, "b.ts");

        let mut o = opts();
        o.file_mask = "!*.ts".into();
        o.regex = true;
        let r = search_text(&d.0, r"foo\w+", &o, &[]).unwrap();
        assert_eq!(r.results.len(), 1);
        assert_eq!(r.results[0].length, 6);

        o.regex = true;
        assert!(search_text(&d.0, "(", &o, &[]).is_err());
    }

    #[test]
    fn priority_files_come_first_and_limit_truncates() {
        let d = TempDir::new();
        write(&d.0, "a.txt", "needle\n");
        write(&d.0, "z.txt", "needle\n");
        let mut o = opts();
        o.limit = 1;
        let r = search_text(&d.0, "needle", &o, &["z.txt".into(), "nope.txt".into()]).unwrap();
        assert_eq!(r.results[0].path, "z.txt");
        assert!(r.truncated);
    }
}
