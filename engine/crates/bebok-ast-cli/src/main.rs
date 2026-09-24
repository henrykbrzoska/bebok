mod query;
mod rust_lang;
mod ts_lang;
mod walk;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct AstRequest {
    root: String,
    kind: String,
    #[serde(default)]
    filters: Value,
    #[serde(default = "default_limit")]
    limit: usize,
}

fn default_limit() -> usize {
    20
}

#[derive(Debug, Serialize)]
struct AstResult {
    path: String,
    line: usize,
    column: usize,
    kind: String,
    name: String,
    snippet: String,
}

#[derive(Debug, Serialize)]
struct AstResponse {
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    results: Option<Vec<AstResult>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    parsed_files: usize,
    duration_ms: u128,
}

fn main() {
    tracing_subscriber::fmt()
        .with_target(false)
        .with_writer(std::io::stderr)
        .init();

    let mut input = String::new();
    if let Err(e) = std::io::Read::read_to_string(&mut std::io::stdin(), &mut input) {
        let resp = AstResponse {
            ok: false,
            results: None,
            error: Some(format!("failed to read stdin: {e}")),
            parsed_files: 0,
            duration_ms: 0,
        };
        println!("{}", serde_json::to_string(&resp).unwrap());
        return;
    }

    let req: AstRequest = match serde_json::from_str(&input) {
        Ok(r) => r,
        Err(e) => {
            let resp = AstResponse {
                ok: false,
                results: None,
                error: Some(format!("invalid request: {e}")),
                parsed_files: 0,
                duration_ms: 0,
            };
            println!("{}", serde_json::to_string(&resp).unwrap());
            return;
        }
    };

    let root = std::path::PathBuf::from(&req.root);
    if !root.is_dir() {
        let resp = AstResponse {
            ok: false,
            results: None,
            error: Some(format!("root is not a directory: {}", req.root)),
            parsed_files: 0,
            duration_ms: 0,
        };
        println!("{}", serde_json::to_string(&resp).unwrap());
        return;
    }

    let start = std::time::Instant::now();

    let filters = query::Filters::from_value(&req.filters);
    let limit = req.limit;

    // Collect files matching the kind filter.
    let exts = filters.exts_for_kind(&req.kind);
    let files = walk::collect_files(&root, &exts, 500);

    let mut results = Vec::new();
    let mut parsed_files = 0usize;

    let rust_lang = rust_lang::RustLang::new().unwrap();
    let ts_lang = ts_lang::TsLang::new().unwrap();

    for path in &files {
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let lang: Option<&dyn query::LanguageHandler> = match ext {
            "rs" => Some(&rust_lang),
            "ts" | "tsx" => Some(&ts_lang),
            _ => None,
        };

        let Some(handler) = lang else {
            continue;
        };

        if !handler.supports_kind(&req.kind) {
            continue;
        }

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };

        parsed_files += 1;
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let matches = handler.query(&content, &req.kind, &filters);
        for m in matches {
            results.push(AstResult {
                path: rel.clone(),
                line: m.line,
                column: m.column,
                kind: m.kind,
                name: m.name,
                snippet: m.snippet,
            });
            if results.len() >= limit {
                break;
            }
        }
        if results.len() >= limit {
            break;
        }
    }

    let duration_ms = start.elapsed().as_millis();

    let resp = AstResponse {
        ok: true,
        results: Some(results),
        error: None,
        parsed_files,
        duration_ms,
    };
    println!("{}", serde_json::to_string(&resp).unwrap());
}
