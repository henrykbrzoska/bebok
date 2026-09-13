//! Browser viewer routes (WP-BROWSER2 / F7-6).
//!
//! The Bebok "browser viewer" window mirrors the agent's browser and lets the
//! user drive it by hand. Everything goes through the very same tool objects
//! the model uses (`browser_open`, `browser_click`, `browser_type`, …) so
//! behaviour, validation and permission rules are identical:
//!
//! * `GET  /session/{id}/browser`            -> state (`open`, `headed`, url, title, display)
//! * `GET  /session/{id}/browser/frame`      -> one JPEG frame now (also keeps the live
//!   `browser.frame` SSE stream on for a while in non-viewer display modes)
//! * `POST /session/{id}/browser/{action}`   -> `navigate` `{url}`, `back`, `forward`,
//!   `reload`, `click` `{x,y}|{selector}`, `type` `{text, selector?, submit?, clear?}`,
//!   `screenshot`, `close`
//!
//! Permission: each action is evaluated by the instance's permission engine
//! exactly like the corresponding tool call (`Deny` -> 403). An `Ask` verdict
//! is *not* re-asked: the request itself is the user's explicit decision for
//! this one call (it is never cached as a session decision).

use axum::Json;
use axum::extract::{Path, State};
use axum::response::IntoResponse;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use bebok_core::permission::Verdict;
use bebok_tools::browser::{BrowserDriver, BrowserSettings, HistoryAction};
use bebok_tools::{ToolCtx, ToolOutput};

use crate::error::{ApiError, err_response};
use crate::state::AppState;

/// Actions accepted by `POST /session/{id}/browser/{action}`.
pub const ACTIONS: &[&str] = &[
    "navigate",
    "back",
    "forward",
    "reload",
    "click",
    "type",
    "screenshot",
    "close",
];

/// Selector used by `type` when the body names none: whatever the user
/// focused by clicking in the viewer.
pub const FOCUSED_SELECTOR: &str = ":focus";

/// Map a viewer action to the tool it is gated and executed as.
///
/// History actions have no tool of their own; they are gated as
/// `browser_open` (navigation) with a descriptive argument so a
/// `browser_open(*)` rule covers them.
pub fn tool_for(action: &str) -> Option<&'static str> {
    match action {
        "navigate" | "back" | "forward" | "reload" => Some("browser_open"),
        "click" => Some("browser_click"),
        "type" => Some("browser_type"),
        "screenshot" => Some("browser_screenshot"),
        _ => None,
    }
}

/// Tool arguments for an action (normalises the viewer's loose body).
pub fn tool_args(action: &str, body: &Value) -> Value {
    let get = |k: &str| body.get(k).cloned();
    match action {
        "navigate" => {
            let mut a = json!({ "url": get("url").unwrap_or(Value::Null) });
            if let Some(w) = get("wait_ms") {
                a["wait_ms"] = w;
            }
            a
        }
        "back" | "forward" | "reload" => json!({ "history": action }),
        "click" => {
            let mut a = json!({});
            for k in ["selector", "x", "y", "wait_ms"] {
                if let Some(v) = get(k) {
                    a[k] = v;
                }
            }
            a
        }
        "type" => {
            let mut a = json!({
                "selector": get("selector")
                    .filter(|s| s.as_str().is_some_and(|s| !s.trim().is_empty()))
                    .unwrap_or_else(|| Value::String(FOCUSED_SELECTOR.to_string())),
                "text": get("text").unwrap_or(Value::Null),
            });
            for k in ["submit", "clear"] {
                if let Some(v) = get(k) {
                    a[k] = v;
                }
            }
            a
        }
        "screenshot" => {
            let mut a = json!({});
            for k in ["full_page", "format"] {
                if let Some(v) = get(k) {
                    a[k] = v;
                }
            }
            a
        }
        _ => json!({}),
    }
}

fn history_action(action: &str) -> Option<HistoryAction> {
    match action {
        "back" => Some(HistoryAction::Back),
        "forward" => Some(HistoryAction::Forward),
        "reload" => Some(HistoryAction::Reload),
        _ => None,
    }
}

/// `GET /session/{id}/browser` -> viewer state.
pub async fn browser_state(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, axum::response::Response> {
    let session = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;
    let instance = state
        .store
        .get_or_create_instance(session.directory())
        .await
        .map_err(|e| err_response(&e))?;
    let settings = BrowserSettings::from_config(&instance.config_snapshot().browser);
    let driver = BrowserDriver::global();
    let sid = id.to_string();
    let info = driver.info(&sid).await;
    Ok(Json(json!({
        "sessionID": sid,
        "directory": session.directory(),
        "display": settings.display.as_str(),
        "open": info.is_some(),
        "headed": info.as_ref().map(|i| i.headed),
        "url": info.as_ref().map(|i| i.url.clone()).unwrap_or_default(),
        "title": info.as_ref().map(|i| i.title.clone()).unwrap_or_default(),
        "running": session.is_running(),
        "streaming": driver.is_streaming(&sid),
    })))
}

/// `GET /session/{id}/browser/frame` -> one frame now (404 without a browser).
pub async fn browser_frame(
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, axum::response::Response> {
    state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;
    let driver = BrowserDriver::global();
    match driver.capture_frame(&id.to_string()).await {
        Ok(frame) => Ok(Json(serde_json::to_value(frame).unwrap_or(Value::Null))),
        Err(e) if e.contains("no page is open") => Err(ApiError::not_found(e).into_response()),
        Err(e) => Err(ApiError::internal(e).into_response()),
    }
}

/// `POST /session/{id}/browser/{action}` -> drive the session's browser.
pub async fn browser_action(
    State(state): State<AppState>,
    Path((id, action)): Path<(Uuid, String)>,
    body: Option<Json<Value>>,
) -> Result<Json<Value>, axum::response::Response> {
    let body = body.map(|Json(v)| v).unwrap_or(json!({}));
    if !ACTIONS.contains(&action.as_str()) {
        return Err(ApiError::bad_request(format!(
            "unknown browser action '{action}' (expected one of {})",
            ACTIONS.join(", ")
        ))
        .into_response());
    }
    let session = state
        .store
        .open_session(id)
        .await
        .map_err(|e| err_response(&e))?;
    let sid = id.to_string();
    let driver = BrowserDriver::global();

    if action == "close" {
        let closed = driver.close(&sid).await;
        state.store.bus().publish(
            bebok_core::event::Event::new("browser.closed", session.directory(), &sid)
                .with_properties(json!({ "closed": closed })),
        );
        return Ok(Json(json!({ "sessionID": sid, "closed": closed })));
    }

    let instance = state
        .store
        .get_or_create_instance(session.directory())
        .await
        .map_err(|e| err_response(&e))?;
    let tool_name = tool_for(&action).expect("every non-close action maps to a tool");
    let args = tool_args(&action, &body);

    // Same gate as the model's calls (agent overrides do not apply: the user
    // is acting directly, not through an agent preset).
    let read_only = instance
        .tools
        .get(tool_name)
        .map(|t| t.is_read_only_for(&args))
        .unwrap_or(false);
    let evaluation = instance
        .permission
        .evaluate(None, tool_name, &args, read_only);
    if evaluation.verdict == Verdict::Deny {
        return Err(ApiError::forbidden(format!(
            "{tool_name} is denied by permission rule '{}'",
            evaluation.pattern
        ))
        .into_response());
    }

    if let Some(history) = history_action(&action) {
        let (url, title) = driver.navigate_history(&sid, history).await.map_err(|e| {
            if e.contains("no page is open") {
                ApiError::not_found(e).into_response()
            } else {
                ApiError::bad_request(e).into_response()
            }
        })?;
        return Ok(Json(json!({
            "sessionID": sid,
            "action": action,
            "ok": true,
            "url": url,
            "title": title,
        })));
    }

    let Some(tool) = instance.tools.get(tool_name) else {
        return Err(
            ApiError::internal(format!("tool {tool_name} is not registered")).into_response(),
        );
    };
    let ctx = ToolCtx {
        root: instance.root.clone(),
        session_id: sid.clone(),
        abort: CancellationToken::new(),
    };
    let output = tool.execute(ctx, args).await;
    Ok(Json(output_json(&sid, &action, output)))
}

/// Shape a tool result for the viewer: `ok` is false when the tool reported
/// an `error:` (argument or browser failure); the model-facing text is kept.
pub fn output_json(session_id: &str, action: &str, output: ToolOutput) -> Value {
    let ok = !output.text.starts_with("error:") && output.text != "aborted";
    let mut v = json!({
        "sessionID": session_id,
        "action": action,
        "ok": ok,
        "text": output.text,
    });
    if let Some(s) = output.structured {
        if let Some(url) = s.get("url") {
            v["url"] = url.clone();
        }
        if let Some(title) = s.get("title") {
            v["title"] = title.clone();
        }
        v["structured"] = s;
    }
    if let Some(img) = output.image {
        v["image"] = json!({ "media_type": img.media_type, "data": img.data });
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_action_except_close_maps_to_a_browser_tool() {
        for a in ACTIONS {
            if *a == "close" {
                assert!(tool_for(a).is_none());
            } else {
                let t = tool_for(a).unwrap();
                assert!(t.starts_with("browser_"), "{a} -> {t}");
                assert!(bebok_tools::browser::TOOL_NAMES.contains(&t), "{a} -> {t}");
            }
        }
        assert!(tool_for("explode").is_none());
    }

    #[test]
    fn navigate_and_history_are_gated_as_browser_open() {
        assert_eq!(tool_for("navigate"), Some("browser_open"));
        assert_eq!(tool_for("back"), Some("browser_open"));
        assert_eq!(tool_for("reload"), Some("browser_open"));
        assert_eq!(
            tool_args(
                "navigate",
                &json!({ "url": "https://example.com", "junk": 1 })
            ),
            json!({ "url": "https://example.com" })
        );
        assert_eq!(tool_args("back", &json!({})), json!({ "history": "back" }));
        assert_eq!(history_action("forward"), Some(HistoryAction::Forward));
        assert_eq!(history_action("navigate"), None);
    }

    #[test]
    fn click_keeps_only_known_keys() {
        assert_eq!(
            tool_args(
                "click",
                &json!({ "x": 10, "y": 20.5, "selector": null, "evil": true })
            ),
            json!({ "x": 10, "y": 20.5, "selector": null })
        );
    }

    #[test]
    fn type_defaults_to_the_focused_element() {
        let a = tool_args("type", &json!({ "text": "hi", "submit": true }));
        assert_eq!(a["selector"], FOCUSED_SELECTOR);
        assert_eq!(a["text"], "hi");
        assert_eq!(a["submit"], true);
        assert!(a.get("clear").is_none());
        let a = tool_args("type", &json!({ "text": "x", "selector": "  " }));
        assert_eq!(a["selector"], FOCUSED_SELECTOR);
        let a = tool_args("type", &json!({ "text": "x", "selector": "#q" }));
        assert_eq!(a["selector"], "#q");
    }

    #[test]
    fn output_json_flags_tool_errors() {
        let out = ToolOutput::new("error: no element matches selector", "browser_click");
        let v = output_json("s", "click", out);
        assert_eq!(v["ok"], false);
        assert_eq!(v["action"], "click");

        let out = ToolOutput::new("Opened https://x/", "browser_open")
            .with_structured(json!({ "url": "https://x/", "title": "X" }));
        let v = output_json("s", "navigate", out);
        assert_eq!(v["ok"], true);
        assert_eq!(v["url"], "https://x/");
        assert_eq!(v["title"], "X");
        assert_eq!(v["structured"]["url"], "https://x/");

        let out =
            ToolOutput::new("Screenshot", "browser_screenshot").with_image("image/png", "AAAA");
        let v = output_json("s", "screenshot", out);
        assert_eq!(v["image"]["media_type"], "image/png");
        assert_eq!(v["image"]["data"], "AAAA");
    }

    /// Route-level: an unknown action is a 400 before any session lookup, a
    /// missing session a 404, and `/browser/frame` for a session without a
    /// browser a 404 - all through the real router (token layer included).
    #[tokio::test]
    async fn routes_reject_unknown_actions_and_missing_browsers() {
        use axum::body::Body;
        use axum::http::{Request, StatusCode};
        use tower::ServiceExt as _;

        let app = crate::auth::tests::test_app();
        let auth = format!("Bearer {}", crate::auth::token());
        let dir = crate::auth::tests::temp_project();

        // Create a session to address.
        let res = app
            .clone()
            .oneshot(
                Request::post("/session")
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "directory": dir }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = res.status();
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "{}",
            String::from_utf8_lossy(&bytes)
        );
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let sid = v["sessionID"].as_str().unwrap().to_string();

        let res = app
            .clone()
            .oneshot(
                Request::post(format!("/session/{sid}/browser/explode"))
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);

        let res = app
            .clone()
            .oneshot(
                Request::get(format!("/session/{sid}/browser/frame"))
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::NOT_FOUND);

        let res = app
            .clone()
            .oneshot(
                Request::get(format!("/session/{sid}/browser"))
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["open"], false);
        assert_eq!(v["display"], "headed");

        // `close` on a session without a browser is a harmless no-op.
        let res = app
            .clone()
            .oneshot(
                Request::post(format!("/session/{sid}/browser/close"))
                    .header("authorization", &auth)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);

        // A `type` on a session without a page is reported by the tool itself
        // (ok:false, "call browser_open first"), not as a transport error.
        let res = app
            .clone()
            .oneshot(
                Request::post(format!("/session/{sid}/browser/type"))
                    .header("authorization", &auth)
                    .header("content-type", "application/json")
                    .body(Body::from(json!({ "text": "hi" }).to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
            .await
            .unwrap();
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["ok"], false);
        assert!(v["text"].as_str().unwrap().contains("browser_open first"));

        // Missing token -> 401 like every other route.
        let res = app
            .oneshot(
                Request::get(format!("/session/{sid}/browser"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }
}
