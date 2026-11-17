//! Explorer routes: `GET /fs/tree`, `GET/PUT/POST /fs/file`, `POST /fs/dir`,
//! `DELETE /fs/path`, `POST /fs/rename`, `GET /fs/definition`, `GET /fs/search`,
//! `POST /fs/complete`.

use axum::Json;
use axum::extract::{Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;

use crate::error::{ApiError, err_response};
use crate::state::AppState;

/// `GET /fs/tree?directory=&path=` and `GET /fs/file?directory=&path=` query.
#[derive(Deserialize)]
pub struct FsQuery {
    pub directory: String,
    #[serde(default)]
    pub path: Option<String>,
    /// When `true` (or `1`), read the file as raw bytes, base64-encode the
    /// content and return `binary: true` + `media_type` in the response.
    #[serde(default)]
    pub binary: bool,
}

fn is_project_config_path(root: &std::path::Path, rel: &str) -> bool {
    let portable = rel.replace('\\', "/");
    // `write_file_text` creates missing files, so canonicalization alone is
    // insufficient when config.json does not exist yet. Normalize the
    // validated relative path and compare it to the protected path first.
    let mut normalized = std::path::PathBuf::new();
    for component in std::path::Path::new(&portable).components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::Normal(part) => normalized.push(part),
            _ => return false,
        }
    }
    let config_rel = std::path::Path::new(".bebok").join("config.json");
    if normalized == config_rel
        || (cfg!(windows)
            && normalized
                .to_string_lossy()
                .eq_ignore_ascii_case(&config_rel.to_string_lossy()))
    {
        return true;
    }

    let target = root.join(portable);
    let config_path = bebok_core::config::project_config_path(root);
    matches!(
        (
            std::fs::canonicalize(target),
            std::fs::canonicalize(config_path),
        ),
        (Ok(target), Ok(config_path)) if target == config_path
    )
}

/// Map a file extension to a MIME media type for binary responses.
fn media_type_from_path(rel: &str) -> &'static str {
    let ext = std::path::Path::new(rel)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "ico" => "image/x-icon",
        "bmp" => "image/bmp",
        "tiff" | "tif" => "image/tiff",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "pdf" => "application/pdf",
        "json" => "application/json",
        "xml" => "application/xml",
        "zip" => "application/zip",
        "gz" | "gzip" => "application/gzip",
        "wasm" => "application/wasm",
        "txt" | "md" | "rs" | "ts" | "js" | "py" | "go" | "java" | "c" | "cpp" | "h" | "css"
        | "html" | "toml" | "yaml" | "yml" | "sh" | "bat" | "cmd" | "ps1" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// `GET /fs/tree?directory=&path=` -> immediate children of `path` (lazy,
/// gitignore-aware). The client expands a directory by requesting its path.
pub async fn fs_tree(
    State(state): State<AppState>,
    Query(q): Query<FsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let rel = q.path.as_deref().unwrap_or(".");
    bebok_core::explorer::validate_rel(&instance.root, rel)
        .map_err(|e| ApiError::bad_request(e).into_response())?;
    let entries = bebok_core::explorer::list_children(&instance.root, rel);
    Ok(Json(serde_json::json!({ "path": rel, "entries": entries })))
}

/// `GET /fs/file?directory=&path=` -> file content (for the viewer/diffs).
/// When `?binary=true` (or `?binary=1`), the file is read as raw bytes,
/// base64-encoded, and returned with `media_type` metadata.  When the text
/// read fails with a UTF-8 error (binary file accessed without the flag),
/// the handler automatically falls back to binary mode.
pub async fn fs_file(
    State(state): State<AppState>,
    Query(q): Query<FsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let rel = q.path.as_deref().unwrap_or(".");
    bebok_core::explorer::validate_rel(&instance.root, rel)
        .map_err(|e| ApiError::bad_request(e).into_response())?;
    // GET /config is the only HTTP surface for config contents; it redacts
    // credentials. The generic file viewer must not expose the raw file.
    if is_project_config_path(&instance.root, rel) {
        return Err(
            ApiError::forbidden("use /config to inspect project configuration").into_response(),
        );
    }

    if q.binary {
        read_binary(&instance.root, rel)
    } else {
        match bebok_core::explorer::read_file_text(&instance.root, rel) {
            Ok(text) => Ok(Json(serde_json::json!({ "path": rel, "content": text }))),
            // Non-UTF8 text read failure (e.g. binary file accessed without
            // the binary flag): silently fall back to binary mode so images
            // and other assets always load regardless of the client path.
            Err(e) if is_likely_binary_error(&e) => read_binary(&instance.root, rel),
            Err(e) => Err(ApiError::bad_request(e.to_string()).into_response()),
        }
    }
}

/// Read a file as raw bytes, base64-encode it, and return with media type.
fn read_binary(
    root: &std::path::Path,
    rel: &str,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    use base64::Engine as _;
    let bytes = bebok_core::explorer::read_file_bytes(root, rel)
        .map_err(|e| ApiError::bad_request(e).into_response())?;
    let media_type = media_type_from_path(rel);
    let encoded = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(Json(serde_json::json!({
        "path": rel,
        "content": encoded,
        "binary": true,
        "media_type": media_type,
    })))
}

/// Heuristic: a text-read error that looks like a UTF-8 / binary mismatch
/// is worth auto-falling back to binary mode.
fn is_likely_binary_error(err: &str) -> bool {
    let lower = err.to_ascii_lowercase();
    lower.contains("utf-8") || lower.contains("utf8") || lower.contains("invalid")
}

/// `PUT /fs/file?directory=&path=` -> save file content (explorer edit mode).
pub async fn fs_file_write(
    State(state): State<AppState>,
    Query(q): Query<FsQuery>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let rel = q.path.as_deref().unwrap_or(".");
    if is_project_config_path(&instance.root, rel) {
        return Err(
            ApiError::forbidden("use /config to modify project configuration").into_response(),
        );
    }
    let content = body
        .get("content")
        .and_then(|c| c.as_str())
        .ok_or_else(|| ApiError::bad_request("missing 'content'").into_response())?;
    match bebok_core::explorer::write_file_text(&instance.root, rel, content) {
        Ok(()) => {
            instance.symbols.invalidate(&rel.replace('\\', "/"));
            Ok(Json(serde_json::json!({ "path": rel, "saved": true })))
        }
        Err(e) => Err(ApiError::bad_request(e.to_string()).into_response()),
    }
}

/// `POST /fs/file?directory=&path=` -> create an empty file (explorer "New
/// file"). Parents are created as needed; refuses existing targets.
pub async fn fs_file_create(
    State(state): State<AppState>,
    Query(q): Query<FsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let rel = q.path.as_deref().unwrap_or(".");
    if is_project_config_path(&instance.root, rel) {
        return Err(
            ApiError::forbidden("use /config to modify project configuration").into_response(),
        );
    }
    match bebok_core::explorer::create_file(&instance.root, rel) {
        Ok(()) => Ok(Json(serde_json::json!({ "path": rel, "created": true }))),
        Err(e) => Err(ApiError::bad_request(e.to_string()).into_response()),
    }
}

/// `POST /fs/dir?directory=&path=` -> create a directory (explorer "New
/// folder"). Parents are created as needed; refuses existing targets.
pub async fn fs_dir_create(
    State(state): State<AppState>,
    Query(q): Query<FsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let rel = q.path.as_deref().unwrap_or(".");
    match bebok_core::explorer::create_dir(&instance.root, rel) {
        Ok(()) => Ok(Json(serde_json::json!({ "path": rel, "created": true }))),
        Err(e) => Err(ApiError::bad_request(e.to_string()).into_response()),
    }
}

/// `DELETE /fs/path?directory=&path=[&recursive=true]` -> delete a file or
/// directory (explorer "Delete"). Non-empty directories need `recursive`.
pub async fn fs_path_delete(
    State(state): State<AppState>,
    Query(q): Query<FsDeleteQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let rel = q.path.as_deref().unwrap_or(".");
    if is_project_config_path(&instance.root, rel) {
        return Err(
            ApiError::forbidden("use /config to modify project configuration").into_response(),
        );
    }
    match bebok_core::explorer::delete_path(&instance.root, rel, q.recursive()) {
        Ok(()) => Ok(Json(serde_json::json!({ "path": rel, "deleted": true }))),
        Err(e) => Err(ApiError::bad_request(e.to_string()).into_response()),
    }
}

/// `DELETE /fs/path` query: `recursive` accepts `true`/`1`.
#[derive(Deserialize)]
pub struct FsDeleteQuery {
    pub directory: String,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub recursive: Option<String>,
}

impl FsDeleteQuery {
    fn recursive(&self) -> bool {
        matches!(self.recursive.as_deref(), Some("true") | Some("1"))
    }
}

/// `POST /fs/rename?directory=` with `{ from, to }` -> atomic rename / move
/// (explorer "Rename"). Both sides must stay inside the project root.
pub async fn fs_rename(
    State(state): State<AppState>,
    Query(q): Query<FsQuery>,
    Json(body): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let from = body
        .get("from")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("missing 'from'").into_response())?;
    let to = body
        .get("to")
        .and_then(|v| v.as_str())
        .ok_or_else(|| ApiError::bad_request("missing 'to'").into_response())?;
    if is_project_config_path(&instance.root, from) || is_project_config_path(&instance.root, to) {
        return Err(
            ApiError::forbidden("use /config to modify project configuration").into_response(),
        );
    }
    match bebok_core::explorer::rename_path(&instance.root, from, to) {
        Ok(()) => Ok(Json(
            serde_json::json!({ "from": from, "to": to, "renamed": true }),
        )),
        Err(e) => Err(ApiError::bad_request(e.to_string()).into_response()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn file_route_rejects_windows_traversal_over_http() {
        let base = std::env::temp_dir().join(format!("bebok-fs-http-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        std::fs::create_dir_all(&root).unwrap();
        let state = AppState {
            store: bebok_core::InstanceStore::with_data_dir(base.join("data")),
            #[cfg(not(target_os = "android"))]
            ptys: Arc::new(bebok_pty::PtyManager::new()),
            debug: Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
            llm_trace: Arc::new(bebok_core::LlmTrace::new(2)),
            remote_extensions: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            command_queue: Arc::new(tokio::sync::Mutex::new(crate::state::CommandRegistry::new())),
            batches: Arc::new(bebok_core::BatchStore::new()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                crate::routes::build_api_router().with_state(state),
            )
            .await
            .unwrap();
        });

        let directory = root
            .to_string_lossy()
            .replace('\\', "%5C")
            .replace(':', "%3A")
            .replace('/', "%2F");
        // The API router carries the capability-token layer (F0-5), so the raw
        // request must present the engine token - otherwise this would assert
        // the traversal guard while really only observing a 401.
        let auth = crate::auth::token();
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "GET /fs/file?directory={directory}&path=..%5C..%5CWindows%5Cwin.ini HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nConnection: close\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 400"), "{response}");
        assert!(!response.contains("[fonts]"));
        server.abort();
        std::fs::remove_dir_all(base).unwrap();
    }

    #[tokio::test]
    async fn file_route_rejects_project_config_write_over_http() {
        let base =
            std::env::temp_dir().join(format!("bebok-fs-config-http-{}", uuid::Uuid::new_v4()));
        let root = base.join("root");
        let config = root.join(".bebok/config.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let original = "{\n  \"model\": \"openai/gpt-4o\"\n}\n";
        std::fs::write(&config, original).unwrap();
        let state = AppState {
            store: bebok_core::InstanceStore::with_data_dir(base.join("data")),
            #[cfg(not(target_os = "android"))]
            ptys: Arc::new(bebok_pty::PtyManager::new()),
            debug: Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
            llm_trace: Arc::new(bebok_core::LlmTrace::new(2)),
            remote_extensions: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            command_queue: Arc::new(tokio::sync::Mutex::new(crate::state::CommandRegistry::new())),
            batches: Arc::new(bebok_core::BatchStore::new()),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                crate::routes::build_api_router().with_state(state),
            )
            .await
            .unwrap();
        });

        let directory = root
            .to_string_lossy()
            .replace('\\', "%5C")
            .replace(':', "%3A")
            .replace('/', "%2F");
        let auth = crate::auth::token();
        let request_body = r#"{"content":"mutated"}"#;
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "PUT /fs/file?directory={directory}&path=.%5C.bebok%5C.%5Cconfig.json HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{request_body}",
            request_body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert_eq!(std::fs::read_to_string(&config).unwrap(), original);

        // The protected file must remain protected even when it is absent and
        // the generic writer would otherwise create it.
        std::fs::remove_file(&config).unwrap();
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "PUT /fs/file?directory={directory}&path=.%5C.bebok%5C.%5Cconfig.json HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{request_body}",
            request_body.len()
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(response.starts_with("HTTP/1.1 403"), "{response}");
        assert!(!config.exists());
        server.abort();
        std::fs::remove_dir_all(base).unwrap();
    }
}

/// `GET /fs/definition?directory=&path=&symbol=` query.
#[derive(Deserialize)]
pub struct FsDefinitionQuery {
    pub directory: String,
    /// File the Ctrl/Cmd+click happened in (picks the language family and
    /// is searched first).
    #[serde(default)]
    pub path: Option<String>,
    pub symbol: String,
}

/// `GET /fs/definition` -> definition sites of `symbol` (Explorer editor
/// Ctrl/Cmd+click, tree-sitter based: Rust, TS/JS, PHP, Go, Python).
pub async fn fs_definition(
    State(state): State<AppState>,
    Query(q): Query<FsDefinitionQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    if let Some(rel) = q.path.as_deref() {
        bebok_core::explorer::validate_rel(&instance.root, rel)
            .map_err(|e| ApiError::bad_request(e).into_response())?;
    }
    let root = instance.root.clone();
    let resp = tokio::task::spawn_blocking(move || {
        bebok_core::agent::goto_definition::find_definitions(
            &root,
            q.path.as_deref(),
            &q.symbol,
            50,
            20_000,
        )
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()).into_response())?;
    Ok(Json(serde_json::to_value(resp).unwrap_or_default()))
}

/// `GET /fs/search` query (Explorer "Find in Files", Ctrl/Cmd+Shift+F).
#[derive(Deserialize)]
pub struct FsSearchQuery {
    pub directory: String,
    pub q: String,
    #[serde(default)]
    pub case: bool,
    #[serde(default)]
    pub word: bool,
    #[serde(default)]
    pub regex: bool,
    /// File-name globs, `*.rs, !*.min.js`.
    #[serde(default)]
    pub mask: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /fs/search` -> `{ text: {results, files_matched, files_searched,
/// truncated}, symbols: [...], indexed: bool }`. Text hits come from a
/// gitignore-aware scan whose file order follows the `bebok-index` ranking
/// when the plugin is available; symbols are tree-sitter definitions whose
/// name contains the query (only for identifier-like, non-regex queries).
pub async fn fs_search(
    State(state): State<AppState>,
    Query(q): Query<FsSearchQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let root = instance.root.clone();
    let limit = q.limit.unwrap_or(500).clamp(1, 2000);
    let priority = if q.regex {
        Vec::new()
    } else {
        bebok_core::agent::code_index_tools::index_ranked_paths(&root, &q.q, 200).await
    };
    let indexed = !priority.is_empty();
    let opts = bebok_core::agent::project_search::TextSearchOptions {
        case_sensitive: q.case,
        whole_word: q.word,
        regex: q.regex,
        file_mask: q.mask.clone(),
        limit,
        max_files: 50_000,
    };
    let query = q.q.clone();
    let want_symbols = !q.regex;
    let (text, symbols) = tokio::task::spawn_blocking(move || {
        let text = bebok_core::agent::project_search::search_text(&root, &query, &opts, &priority);
        let symbols = if want_symbols {
            bebok_core::agent::goto_definition::find_symbols(&root, &query, 100, 20_000)
        } else {
            Vec::new()
        };
        (text, symbols)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()).into_response())?;
    let text = text.map_err(|e| ApiError::bad_request(e).into_response())?;
    Ok(Json(serde_json::json!({
        "text": text,
        "symbols": symbols,
        "indexed": indexed,
    })))
}

/// `GET /fs/files?directory=` query (Explorer "Go to File", Ctrl/Cmd+Shift+N).
#[derive(Deserialize)]
pub struct FsFilesQuery {
    pub directory: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /fs/files` -> `{ files: [rel, ...], truncated }`: every project file
/// (gitignore-aware), sorted. The client fuzzy-matches names over it.
pub async fn fs_files(
    State(state): State<AppState>,
    Query(q): Query<FsFilesQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let root = instance.root.clone();
    let limit = q.limit.unwrap_or(50_000).clamp(1, 200_000);
    let (files, truncated) = tokio::task::spawn_blocking(move || {
        bebok_core::agent::project_search::list_files(&root, limit)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()).into_response())?;
    Ok(Json(
        serde_json::json!({ "files": files, "truncated": truncated }),
    ))
}

/// `GET /fs/symbols?directory=&q=` query (Explorer "Go to Class / Symbol").
#[derive(Deserialize)]
pub struct FsSymbolsQuery {
    pub directory: String,
    pub q: String,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// `GET /fs/symbols` -> `{ symbols: [...] }`: definitions whose name holds
/// `q` as a case-insensitive subsequence, served from the per-instance
/// symbol cache (no full re-parse per keystroke, unlike `/fs/search`).
pub async fn fs_symbols(
    State(state): State<AppState>,
    Query(q): Query<FsSymbolsQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    let root = instance.root.clone();
    let index = instance.symbols.clone();
    let limit = q.limit.unwrap_or(300).clamp(1, 2000);
    let symbols = tokio::task::spawn_blocking(move || {
        index.refresh(&root);
        index.fuzzy(&q.q, limit)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()).into_response())?;
    Ok(Json(serde_json::json!({ "symbols": symbols })))
}

/// `POST /fs/complete?directory=` query (the buffer travels in the body).
#[derive(Deserialize)]
pub struct FsCompleteQuery {
    pub directory: String,
}

/// `POST /fs/complete` -> ranked completion items for an unsaved buffer
/// (Explorer editor autocomplete, no AI). Body: `{ path, text, offset, limit? }`.
pub async fn fs_complete(
    State(state): State<AppState>,
    Query(q): Query<FsCompleteQuery>,
    Json(body): Json<bebok_core::agent::complete::CompleteRequest>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let instance = state
        .store
        .get_or_create_instance(&q.directory)
        .await
        .map_err(|e| err_response(&e))?;
    bebok_core::explorer::validate_rel(&instance.root, &body.path)
        .map_err(|e| ApiError::bad_request(e).into_response())?;
    let root = instance.root.clone();
    let symbols = instance.symbols.clone();
    let resp = tokio::task::spawn_blocking(move || {
        bebok_core::agent::complete::complete(&root, &symbols, &body)
    })
    .await
    .map_err(|e| ApiError::internal(e.to_string()).into_response())?;
    Ok(Json(serde_json::to_value(resp).unwrap_or_default()))
}
