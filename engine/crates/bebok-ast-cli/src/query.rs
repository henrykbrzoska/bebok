use regex::Regex;
use serde_json::Value;

/// Filters extracted from the request JSON.
#[derive(Debug, Clone, Default)]
pub struct Filters {
    pub trait_name: Option<String>,
    pub derive: Option<String>,
    pub return_type: Option<String>,
    pub annotation: Option<String>,
    pub name_regex: Option<Regex>,
    pub path_regex: Option<Regex>,
    pub ext: Option<String>,
}

impl Filters {
    pub fn from_value(v: &Value) -> Self {
        let obj = v.as_object();
        let get_str = |key: &str| -> Option<String> {
            obj?.get(key)
                .and_then(|v| v.as_str())
                .map(str::to_string)
        };
        let get_re = |key: &str| -> Option<Regex> {
            let s = get_str(key)?;
            Regex::new(&s).ok()
        };

        Self {
            trait_name: get_str("trait"),
            derive: get_str("derive"),
            return_type: get_str("return_type"),
            annotation: get_str("annotation"),
            name_regex: get_re("name_regex"),
            path_regex: get_re("path_regex"),
            ext: get_str("ext"),
        }
    }

    /// Returns the file extensions relevant for a given kind.
    pub fn exts_for_kind(&self, kind: &str) -> Vec<String> {
        // If an explicit ext filter is set, use it.
        if let Some(ref ext) = self.ext {
            return vec![ext.clone()];
        }
        // Otherwise: Rust kinds only need .rs; TS kinds only .ts/.tsx.
        match kind {
            "const" | "static" | "mod" | "use" | "trait" => vec!["rs".to_string()],
            "impl" | "struct" | "fn" | "enum" | "type_alias" | "test" => {
                vec!["rs".to_string(), "ts".to_string(), "tsx".to_string()]
            }
            _ => vec!["rs".to_string(), "ts".to_string(), "tsx".to_string()],
        }
    }

    pub fn matches_name(&self, name: &str) -> bool {
        match &self.name_regex {
            Some(re) => re.is_match(name),
            None => true,
        }
    }

    pub fn matches_path(&self, path: &str) -> bool {
        match &self.path_regex {
            Some(re) => re.is_match(path),
            None => true,
        }
    }

    pub fn matches_annotation(&self, attrs: &str) -> bool {
        match &self.annotation {
            Some(a) => attrs.contains(a.as_str()),
            None => true,
        }
    }

    pub fn matches_derive(&self, attrs: &str) -> bool {
        match &self.derive {
            Some(d) => attrs.contains(d.as_str()),
            None => true,
        }
    }

    pub fn matches_return_type(&self, ret: &str) -> bool {
        match &self.return_type {
            Some(rt) => ret.contains(rt.as_str()),
            None => true,
        }
    }

    pub fn matches_trait(&self, trait_name: &str) -> bool {
        match &self.trait_name {
            Some(t) => trait_name.contains(t.as_str()),
            None => true,
        }
    }
}

/// A single query match result.
#[derive(Debug, Clone)]
pub struct MatchResult {
    pub line: usize,
    pub column: usize,
    pub kind: String,
    pub name: String,
    pub snippet: String,
}

/// Language-specific handler.
pub trait LanguageHandler {
    fn supports_kind(&self, kind: &str) -> bool;
    fn query(&self, content: &str, kind: &str, filters: &Filters) -> Vec<MatchResult>;
}
