//! Explorer routes: `GET /fs/tree`, `GET /fs/file`, `PUT /fs/file`.

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
        Ok(()) => Ok(Json(serde_json::json!({ "path": rel, "saved": true }))),
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
