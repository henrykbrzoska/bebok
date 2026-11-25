//! Git routes (WP-GIT / F6-14, F6-15): `GET /projects/{id}/git` and
//! `POST /projects/{id}/git/worktree/remove`.
//!
//! Same shape as `routes::projects`: thin handlers that resolve the project id
//! through `bebok_core::config::projects` (the `global` config scope in the
//! store's `bebok.db`)
//! and hand the registered path to `bebok_core::git`. Both inherit the
//! capability-token layer from `routes::build_api_router`.
//!
//! Worktree removal is deliberately a separate, explicit call - it is never a
//! side effect of `DELETE /session/{id}` - and only ever touches a path that
//! `bebok_core::git::validate_worktree_removal` confirms sits below the
//! project's own `.bebok/worktrees/`.

use std::path::{Path as FsPath, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::response::IntoResponse;
use serde::Deserialize;

use bebok_core::config::projects::{self, ProjectEntry, ProjectsError};
use bebok_core::event::Event;
use bebok_core::git::{self, GitError};

use crate::error::ApiError;
use crate::state::AppState;

fn map_projects_err(e: ProjectsError) -> axum::response::Response {
    match e {
        ProjectsError::Invalid(m) => ApiError::bad_request(m),
        ProjectsError::NotFound => ApiError::not_found("unknown project"),
        ProjectsError::Io(m) => ApiError::internal(m),
    }
    .into_response()
}

/// `GitError` -> HTTP: caller mistakes are 400 (bad path/branch, not a
/// repository); a failing or missing `git` is 500 with git's own message.
pub fn map_git_err(e: GitError) -> axum::response::Response {
    match e {
        GitError::InvalidInput(m) | GitError::NotRepo(m) => ApiError::bad_request(m),
        GitError::AuthFailed(m) => ApiError::unauthorized(m),
        GitError::Unavailable => ApiError::internal(e.to_string()),
        GitError::Failed(_) | GitError::Io(_) => ApiError::internal(e.to_string()),
    }
    .into_response()
}

/// `POST /projects/{id}/git/worktree/remove` body.
#[derive(Deserialize)]
pub struct RemoveWorktreeBody {
    /// Absolute worktree path, as reported by `DELETE /session/{id}`
    /// (`worktree_path`) or the session's `directory`.
    pub path: String,
}

/// `GET /projects/{id}/git` -> repository probe for the registered path.
/// A non-repo (or a host without `git`) answers `{ "is_repo": false, ... }`
/// with the other fields `null`, never an error; only an unknown id is 404.
pub async fn project_git(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let info = git::inspect(FsPath::new(&entry.path)).await;
    let mut value = serde_json::to_value(info).unwrap_or_default();
    value["project_id"] = serde_json::json!(entry.id);
    value["path"] = serde_json::json!(entry.path);
    value["worktrees_dir"] =
        serde_json::json!(git::worktrees_dir(FsPath::new(&entry.path)).to_string_lossy());
    Ok(Json(value))
}

/// `GET /projects/{id}/git/log` query: `?limit=&skip=&path=`.
#[derive(Deserialize)]
pub struct GitLogQuery {
    pub limit: Option<usize>,
    pub skip: Option<usize>,
    pub path: Option<String>,
}

/// `GET /projects/{id}/git/show` query: `?rev=`.
#[derive(Deserialize)]
pub struct GitShowQuery {
    pub rev: String,
}

/// `GET /projects/{id}/git/status` -> branch, ahead/behind, per-file rows.
pub async fn git_status(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let status = git::status(FsPath::new(&entry.path))
        .await
        .map_err(map_git_err)?;
    Ok(Json(serde_json::to_value(status).unwrap_or_default()))
}

/// `GET /projects/{id}/git/log` -> commit history (newest first).
pub async fn git_log(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<GitLogQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let limit = query.limit.unwrap_or(50).clamp(1, 200);
    let skip = query.skip.unwrap_or(0);
    let entries = git::log(FsPath::new(&entry.path), limit, skip, query.path.as_deref())
        .await
        .map_err(map_git_err)?;
    Ok(Json(serde_json::json!({
        "entries": entries,
        "has_more": entries.len() == limit,
    })))
}

/// `GET /projects/{id}/git/branches` -> local branches with tracking info.
pub async fn git_branches(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let branches = git::branches(FsPath::new(&entry.path))
        .await
        .map_err(map_git_err)?;
    let current = branches.iter().find(|b| b.current).map(|b| &b.name);
    Ok(Json(serde_json::json!({
        "current": current,
        "branches": branches,
    })))
}

/// `GET /projects/{id}/git/show?rev=` -> one commit with diff.
pub async fn git_show(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<GitShowQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let shown = git::show(FsPath::new(&entry.path), query.rev.trim())
        .await
        .map_err(map_git_err)?;
    Ok(Json(serde_json::to_value(shown).unwrap_or_default()))
}

/// Publish `git.changed` after a successful mutation so the client panel
/// can refresh. Best effort: a failed inspect still emits with nulls.
async fn emit(state: &AppState, entry: &ProjectEntry, op: &str) {
    let info = git::inspect(FsPath::new(&entry.path)).await;
    state.store.bus().publish(Event::git_changed(
        &entry.path,
        op,
        info.branch.as_deref(),
        info.dirty_count,
    ));
}

/// `POST /projects/{id}/git/stage` body.
#[derive(Deserialize)]
pub struct PathsBody {
    pub paths: Vec<String>,
}

/// `POST /projects/{id}/git/commit` body.
#[derive(Deserialize)]
pub struct CommitBody {
    pub message: String,
    pub amend: Option<bool>,
}

/// `POST /projects/{id}/git/reset` body. `hard` requires `confirm: true`.
#[derive(Deserialize)]
pub struct ResetBody {
    pub mode: git::ResetMode,
    pub rev: String,
    pub confirm: Option<bool>,
}

/// `POST /projects/{id}/git/stash/push` body.
#[derive(Deserialize)]
pub struct StashPushBody {
    pub message: Option<String>,
}

/// `POST /projects/{id}/git/stash/{pop|apply|drop}` body.
#[derive(Deserialize)]
pub struct StashIndexBody {
    pub index: usize,
}

/// `POST /projects/{id}/git/checkout` body. `force` requires `confirm: true`.
#[derive(Deserialize)]
pub struct CheckoutBody {
    pub target: String,
    pub force: Option<bool>,
    pub confirm: Option<bool>,
}

/// `POST /projects/{id}/git/branch` body.
#[derive(Deserialize)]
pub struct BranchBody {
    pub name: String,
    pub base: Option<String>,
}

/// `DELETE /projects/{id}/git/branch` query. `force` requires `confirm: true`.
#[derive(Deserialize)]
pub struct BranchDeleteQuery {
    pub name: String,
    pub force: Option<bool>,
    pub confirm: Option<bool>,
}

/// `POST /projects/{id}/git/branch/rename` body.
#[derive(Deserialize)]
pub struct RenameBody {
    pub from: String,
    pub to: String,
}

/// `POST /projects/{id}/git/cherry-pick` body.
#[derive(Deserialize)]
pub struct RevBody {
    pub rev: String,
}

fn need_confirm(what: &str) -> axum::response::Response {
    ApiError::bad_request(format!("{what} requires confirm: true")).into_response()
}

/// `POST /projects/{id}/git/stage` -> `git add`.
pub async fn git_stage(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PathsBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::stage(FsPath::new(&entry.path), &body.paths)
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "stage").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/unstage` -> `git reset -- <paths>`.
pub async fn git_unstage(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<PathsBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::unstage(FsPath::new(&entry.path), &body.paths)
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "unstage").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/commit` -> `git commit`; answers the new hash.
pub async fn git_commit(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CommitBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let hash = git::commit(
        FsPath::new(&entry.path),
        &body.message,
        body.amend.unwrap_or(false),
    )
    .await
    .map_err(map_git_err)?;
    emit(&state, &entry, "commit").await;
    Ok(Json(serde_json::json!({ "hash": hash })))
}

/// `POST /projects/{id}/git/reset` -> `git reset --<mode>`.
pub async fn git_reset(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ResetBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    if body.mode == git::ResetMode::Hard && !body.confirm.unwrap_or(false) {
        return Err(need_confirm("reset --hard"));
    }
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::reset(FsPath::new(&entry.path), body.mode, &body.rev)
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "reset").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /projects/{id}/git/stash` -> stash entries.
pub async fn git_stash_list(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let stashes = git::stash_list(FsPath::new(&entry.path))
        .await
        .map_err(map_git_err)?;
    Ok(Json(serde_json::json!({ "stashes": stashes })))
}

/// `POST /projects/{id}/git/stash/push`.
pub async fn git_stash_push(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<StashPushBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::stash_push(FsPath::new(&entry.path), body.message.as_deref())
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "stash").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/stash/{pop|apply|drop}`. `drop` needs `confirm`.
pub async fn git_stash_action(
    State(state): State<AppState>,
    Path(params): Path<(String, String)>,
    Json(body): Json<StashIndexBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let (id, action) = params;
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let dir = FsPath::new(&entry.path);
    match action.as_str() {
        "pop" => git::stash_pop(dir, body.index).await.map_err(map_git_err)?,
        "apply" => git::stash_apply(dir, body.index)
            .await
            .map_err(map_git_err)?,
        "drop" => git::stash_drop(dir, body.index)
            .await
            .map_err(map_git_err)?,
        _ => return Err(ApiError::bad_request("unknown stash action").into_response()),
    }
    emit(&state, &entry, "stash").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/checkout`.
pub async fn git_checkout(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CheckoutBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    if body.force.unwrap_or(false) && !body.confirm.unwrap_or(false) {
        return Err(need_confirm("checkout --force"));
    }
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::checkout(
        FsPath::new(&entry.path),
        &body.target,
        body.force.unwrap_or(false),
    )
    .await
    .map_err(map_git_err)?;
    emit(&state, &entry, "checkout").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/branch` -> `git branch <name> [base]`.
pub async fn git_branch_create(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<BranchBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::branch_create(FsPath::new(&entry.path), &body.name, body.base.as_deref())
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "branch").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `DELETE /projects/{id}/git/branch?name=&force=&confirm=`.
pub async fn git_branch_delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<BranchDeleteQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    if query.force.unwrap_or(false) && !query.confirm.unwrap_or(false) {
        return Err(need_confirm("branch -D"));
    }
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::branch_delete(
        FsPath::new(&entry.path),
        &query.name,
        query.force.unwrap_or(false),
    )
    .await
    .map_err(map_git_err)?;
    emit(&state, &entry, "branch").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/branch/rename`.
pub async fn git_branch_rename(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RenameBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::branch_rename(FsPath::new(&entry.path), &body.from, &body.to)
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "branch").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/cherry-pick`.
pub async fn git_cherry_pick(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RevBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::cherry_pick(FsPath::new(&entry.path), &body.rev)
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "cherry-pick").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/cherry-pick/abort`.
pub async fn git_cherry_pick_abort(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::cherry_pick_abort(FsPath::new(&entry.path))
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "cherry-pick").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/cherry-pick/continue`.
pub async fn git_cherry_pick_continue(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::cherry_pick_continue(FsPath::new(&entry.path))
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "cherry-pick").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /projects/{id}/git/conflicts` -> unresolved merge paths.
pub async fn git_conflicts(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let conflicts = git::conflicts(FsPath::new(&entry.path))
        .await
        .map_err(map_git_err)?;
    Ok(Json(serde_json::json!({ "conflicts": conflicts })))
}

/// `POST /projects/{id}/git/worktree/remove` -> `git worktree remove` for a
/// path strictly below `<project>/.bebok/worktrees/` (anything else is 400).
pub async fn remove_worktree(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<RemoveWorktreeBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let removed = remove_worktree_in(&state.store.config_db_path(), &id, &body.path).await?;
    Ok(Json(serde_json::json!({
        "removed": true,
        "path": removed.to_string_lossy(),
    })))
}

/// Testable core of [`remove_worktree`]: explicit config **database** path
/// instead of the store's own.
pub async fn remove_worktree_in(
    config_path: &FsPath,
    id: &str,
    path: &str,
) -> Result<PathBuf, axum::response::Response> {
    let entry = resolve(config_path, id)?;
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err(ApiError::bad_request("path is required").into_response());
    }
    git::remove_worktree(FsPath::new(&entry.path), FsPath::new(trimmed))
        .await
        .map_err(map_git_err)
}

// ---------------------------------------------------------------------------
// Phase 3: remotes / sync / config / clone / tools / forge
// ---------------------------------------------------------------------------
//
// Same thin shape as the handlers above: resolve the id, hand a *validated*
// argument vector to `bebok_core::git`, publish `git.changed` so the panel
// refreshes. Every git-specific rejection (bad remote name/URL/key/refspec)
// is a `GitError::InvalidInput` -> 400; credential failures are
// `GitError::AuthFailed` -> 401, so the GUI can prompt for a login instead of
// showing a generic error.

/// `POST /projects/{id}/git/remotes/set-url` body.
#[derive(Deserialize)]
pub struct SetRemoteUrlBody {
    pub remote: String,
    pub url: String,
}

/// `POST /projects/{id}/git/sync/fetch` body.
#[derive(Deserialize)]
pub struct SyncFetchBody {
    pub remote: String,
}

/// `POST /projects/{id}/git/sync/pull` body.
#[derive(Deserialize)]
pub struct SyncPullBody {
    pub remote: String,
    pub refspec: Option<String>,
}

/// `POST /projects/{id}/git/sync/push` body.
#[derive(Deserialize)]
pub struct SyncPushBody {
    pub remote: String,
    pub refspec: String,
}

/// `GET /projects/{id}/git/config` query: `?key=`.
#[derive(Deserialize)]
pub struct GitConfigQuery {
    pub key: String,
}

/// `POST /projects/{id}/git/config` body. `scope: "global"` requires
/// `confirm: true` (it edits the user's own git configuration).
#[derive(Deserialize)]
pub struct GitConfigSetBody {
    pub key: String,
    pub value: String,
    pub scope: git::ConfigScope,
    pub confirm: Option<bool>,
}

/// `POST /projects/{id}/git/clone` body.
#[derive(Deserialize)]
pub struct CloneBody {
    pub source: String,
    pub dest: String,
    pub branch: Option<String>,
}

/// `POST /projects/{id}/git/forge` body.
#[derive(Deserialize)]
pub struct ForgeBody {
    pub tool: String,
    pub args: Vec<String>,
}

/// Cap on `workspace` output returned to the client (characters, so a
/// multi-byte message is never cut in half).
const FORGE_OUTPUT_CAP: usize = 500_000;

/// `GET /projects/{id}/git/remotes` -> remotes with their fetch URL.
pub async fn git_remotes(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let remotes = git::remotes_list(FsPath::new(&entry.path))
        .await
        .map_err(map_git_err)?;
    Ok(Json(serde_json::json!({ "remotes": remotes })))
}

/// `POST /projects/{id}/git/remotes/set-url` -> `git remote set-url`.
pub async fn git_remote_set_url(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SetRemoteUrlBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::remotes_set_remote_url(FsPath::new(&entry.path), &body.remote, &body.url)
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "remote-set-url").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/sync/fetch` -> `git fetch <remote>`.
pub async fn git_sync_fetch(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SyncFetchBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::remotes_fetch(FsPath::new(&entry.path), &body.remote)
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "fetch").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/sync/pull` -> `git pull <remote> [refspec]`.
pub async fn git_sync_pull(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SyncPullBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::remotes_pull(
        FsPath::new(&entry.path),
        &body.remote,
        body.refspec.as_deref(),
    )
    .await
    .map_err(map_git_err)?;
    emit(&state, &entry, "pull").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/sync/push` -> `git push <remote> <refspec>`.
pub async fn git_sync_push(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<SyncPushBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::remotes_push(FsPath::new(&entry.path), &body.remote, &body.refspec)
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "push").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `GET /projects/{id}/git/config?key=` -> `{ key, value }`. An allowed key
/// that simply is not set answers `value: null` (200), not an error, so the
/// client can render an empty field; a disallowed key is a 400.
pub async fn git_config_get(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Query(query): Query<GitConfigQuery>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    let entry = resolve(&state.store.config_db_path(), &id)?;
    match git::config_get(FsPath::new(&entry.path), &query.key).await {
        Ok(value) => Ok(Json(
            serde_json::json!({ "key": query.key, "value": value }),
        )),
        Err(GitError::InvalidInput(m)) if m == "key not set" => {
            Ok(Json(serde_json::json!({ "key": query.key, "value": null })))
        }
        Err(e) => Err(map_git_err(e)),
    }
}

/// `POST /projects/{id}/git/config` -> `git config --local|--global k v`.
/// Writing the *global* scope touches the user's own git config, so it needs
/// an explicit `confirm: true`.
pub async fn git_config_set(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<GitConfigSetBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    if body.scope == git::ConfigScope::Global && !body.confirm.unwrap_or(false) {
        return Err(need_confirm("config set --global"));
    }
    let entry = resolve(&state.store.config_db_path(), &id)?;
    git::config_set(FsPath::new(&entry.path), &body.key, &body.value, body.scope)
        .await
        .map_err(map_git_err)?;
    emit(&state, &entry, "config").await;
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// `POST /projects/{id}/git/clone` -> `git clone`, then registers the new
/// checkout as a project so the client can switch to it straight away.
pub async fn git_clone(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<CloneBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    resolve(&state.store.config_db_path(), &id)?;
    let cloned = git::clone(&body.source, &body.dest, body.branch.as_deref())
        .await
        .map_err(map_git_err)?;
    // `git::clone` runs from the temp dir, so a relative destination has to be
    // resolved against it before the (absolute-path) project registry sees it.
    let dest = FsPath::new(&cloned);
    let absolute = if dest.is_absolute() {
        dest.to_path_buf()
    } else {
        std::env::temp_dir().join(dest)
    };
    let absolute = absolute.canonicalize().unwrap_or(absolute);
    let (entry, _created) = projects::add_at(
        &state.store.config_db_path(),
        &absolute.to_string_lossy(),
        None,
    )
    .map_err(map_projects_err)?;
    emit(&state, &entry, "clone").await;
    Ok(Json(serde_json::json!({
        "ok": true,
        "project_id": entry.id,
        "path": entry.path,
    })))
}

/// True when `<binary> --version` answers successfully within 10 s.
///
/// Mirrors `bebok_core::git::run`: no window on Windows, no stdin (a CLI that
/// decides to prompt would otherwise hang), hard timeout, `kill_on_drop`.
async fn probe_binary(binary: &str) -> bool {
    let mut cmd = tokio::process::Command::new(binary);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("GIT_TERMINAL_PROMPT", "0")
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        // Inherent `creation_flags` on the tokio command; importing the std
        // `CommandExt` trait here would be an unused-import failure on Unix.
        cmd.creation_flags(0x0800_0000);
    }
    let child = match cmd.spawn() {
        Ok(child) => child,
        Err(_) => return false,
    };
    matches!(
        tokio::time::timeout(Duration::from_secs(10), child.wait_with_output()).await,
        Ok(Ok(out)) if out.status.success()
    )
}

/// `GET /projects/{id}/git/tools` -> which forge CLIs are installed. `git`
/// itself is always `true` (the whole route family shells out to it).
pub async fn git_tools(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    resolve(&state.store.config_db_path(), &id)?;
    let (gh, glab) = tokio::join!(probe_binary("gh"), probe_binary("glab"));
    let git_bin = git::git_available_tools().iter().any(|t| t == "git");
    Ok(Json(serde_json::json!({
        "tools": { "git": git_bin, "gh": gh, "glab": glab },
    })))
}

/// `POST /projects/{id}/git/forge` -> one allowlisted `gh` / `glab` read
/// command; answers its trimmed stdout (capped at [`FORGE_OUTPUT_CAP`]).
pub async fn git_forge(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(body): Json<ForgeBody>,
) -> Result<Json<serde_json::Value>, axum::response::Response> {
    if body.tool != "gh" && body.tool != "glab" {
        return Err(ApiError::bad_request("tool must be 'gh' or 'glab'").into_response());
    }
    let entry = resolve(&state.store.config_db_path(), &id)?;
    let dir = FsPath::new(&entry.path);
    let output = if body.tool == "gh" {
        git::forge_gh(dir, &body.args).await
    } else {
        git::forge_glab(dir, &body.args).await
    }
    .map_err(map_git_err)?;
    emit(&state, &entry, "forge").await;
    let mut output = output;
    if output.chars().count() > FORGE_OUTPUT_CAP {
        output = output.chars().take(FORGE_OUTPUT_CAP).collect();
    }
    Ok(Json(serde_json::json!({ "ok": true, "output": output })))
}

fn resolve(config_path: &FsPath, id: &str) -> Result<ProjectEntry, axum::response::Response> {
    projects::find_at(config_path, id).map_err(map_projects_err)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::http::StatusCode;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn make_state(base: &std::path::Path) -> crate::state::AppState {
        crate::state::AppState {
            store: bebok_core::InstanceStore::with_data_dir(base.join("data")),
            #[cfg(not(target_os = "android"))]
            ptys: Arc::new(bebok_pty::PtyManager::new()),
            debug: Arc::new(bebok_core::DebugLog::new(base.join("debug.log"))),
            llm_trace: Arc::new(bebok_core::LlmTrace::new(2)),
            remote_extensions: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            command_queue: Arc::new(tokio::sync::Mutex::new(crate::state::CommandRegistry::new())),
            batches: Arc::new(bebok_core::BatchStore::new()),
        }
    }

    /// Same raw-socket pattern as `routes::projects`'s HTTP tests. The state
    /// comes back so a test can register a project in its config database
    /// before issuing requests.
    async fn serve_with(
        base: std::path::PathBuf,
    ) -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
        crate::state::AppState,
    ) {
        std::fs::create_dir_all(&base).unwrap();
        let state = make_state(&base);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let router_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                crate::routes::build_api_router().with_state(router_state),
            )
            .await
            .unwrap();
        });
        (address, server, state)
    }

    async fn serve() -> (
        std::net::SocketAddr,
        tokio::task::JoinHandle<()>,
        std::path::PathBuf,
    ) {
        let base = std::env::temp_dir().join(format!("bebok-git-http-{}", uuid::Uuid::new_v4()));
        let (address, server, _state) = serve_with(base.clone()).await;
        (address, server, base)
    }

    /// Test-only `git <args>` in `cwd`; panics on failure.
    fn git_cmd(cwd: &std::path::Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .expect("git spawns");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// `git init` + identity + one commit, so `HEAD` and `git remote -v` work.
    fn init_repo(dir: &std::path::Path) {
        std::fs::create_dir_all(dir).unwrap();
        git_cmd(dir, &["init", "-q"]);
        git_cmd(dir, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git_cmd(dir, &["config", "user.email", "bebok@example.com"]);
        git_cmd(dir, &["config", "user.name", "Bebok Test"]);
        git_cmd(dir, &["config", "commit.gpgsign", "false"]);
        std::fs::write(dir.join("README.md"), "hello\n").unwrap();
        git_cmd(dir, &["add", "README.md"]);
        git_cmd(dir, &["commit", "-q", "-m", "init"]);
    }

    /// One authenticated request against the raw test socket.
    async fn authed(address: std::net::SocketAddr, method: &str, path: &str, body: &str) -> String {
        let auth = crate::auth::token();
        let request = if body.is_empty() {
            format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nConnection: close\r\n\r\n"
            )
        } else {
            format!(
                "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
        };
        raw(address, &request).await
    }

    async fn raw(address: std::net::SocketAddr, request: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        String::from_utf8_lossy(&response).to_string()
    }

    #[tokio::test]
    async fn git_routes_require_the_capability_token() {
        let (address, server, base) = serve().await;
        let get = raw(
            address,
            "GET /projects/some-id/git HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
        )
        .await;
        assert!(
            get.starts_with("HTTP/1.1 401") || get.starts_with("HTTP/1.1 403"),
            "{get}"
        );
        let body = r#"{"path":"x"}"#;
        let post = raw(
            address,
            &format!(
                "POST /projects/some-id/git/worktree/remove HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            ),
        )
        .await;
        assert!(
            post.starts_with("HTTP/1.1 401") || post.starts_with("HTTP/1.1 403"),
            "{post}"
        );
        // Phase 3 mutation routes inherit the same token layer.
        for (path, body) in [
            (
                "/projects/some-id/git/remotes/set-url",
                r#"{"remote":"origin","url":"https://github.com/acme/x.git"}"#,
            ),
            ("/projects/some-id/git/sync/fetch", r#"{"remote":"origin"}"#),
            (
                "/projects/some-id/git/sync/pull",
                r#"{"remote":"origin","refspec":null}"#,
            ),
            (
                "/projects/some-id/git/sync/push",
                r#"{"remote":"origin","refspec":"main"}"#,
            ),
            (
                "/projects/some-id/git/config",
                r#"{"key":"user.name","value":"x","scope":"local"}"#,
            ),
            (
                "/projects/some-id/git/clone",
                r#"{"source":"https://github.com/acme/x.git","dest":"x","branch":null}"#,
            ),
            (
                "/projects/some-id/git/forge",
                r#"{"tool":"gh","args":["repo","view"]}"#,
            ),
        ] {
            let response = raw(
                address,
                &format!(
                    "POST {path} HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                ),
            )
            .await;
            assert!(
                response.starts_with("HTTP/1.1 401") || response.starts_with("HTTP/1.1 403"),
                "{path}: {response}"
            );
        }
        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }

    #[tokio::test]
    async fn unknown_project_id_is_404_over_http() {
        let (address, server, base) = serve().await;
        let auth = crate::auth::token();
        for path in [
            "/projects/no-such-project/git",
            "/projects/no-such-project/git/status",
            "/projects/no-such-project/git/log",
            "/projects/no-such-project/git/branches",
            "/projects/no-such-project/git/show?rev=HEAD",
            "/projects/no-such-project/git/stash",
            "/projects/no-such-project/git/conflicts",
            "/projects/no-such-project/git/remotes",
            "/projects/no-such-project/git/config?key=user.name",
            "/projects/no-such-project/git/tools",
        ] {
            let response = raw(
                address,
                &format!(
                    "GET {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {auth}\r\nConnection: close\r\n\r\n"
                ),
            )
            .await;
            assert!(response.starts_with("HTTP/1.1 404"), "{path}: {response}");
        }
        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }

    /// The removal endpoint refuses anything outside the project's own
    /// `.bebok/worktrees/` before `git` is ever invoked (no git needed).
    #[tokio::test]
    async fn worktree_removal_refuses_paths_outside_the_worktrees_dir() {
        let base = std::env::temp_dir().join(format!("bebok-git-remove-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        let other = base.join("other");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        // The registry now lives in a config database; same temp location.
        let config = base.join("bebok.db");
        let (entry, _) =
            bebok_core::config::projects::add_at(&config, project.to_str().unwrap(), None).unwrap();

        for bad in [
            other.to_string_lossy().to_string(),
            project.to_string_lossy().to_string(),
            bebok_core::git::worktrees_dir(&project)
                .to_string_lossy()
                .to_string(),
            "relative/path".to_string(),
            String::new(),
        ] {
            let err = super::remove_worktree_in(&config, &entry.id, &bad)
                .await
                .expect_err(&format!("must refuse {bad:?}"));
            assert_eq!(err.status(), StatusCode::BAD_REQUEST, "{bad:?}");
        }
        assert!(
            other.exists(),
            "nothing outside the worktrees dir was touched"
        );

        // Unknown project -> 404 before any path validation.
        let err = super::remove_worktree_in(&config, "nope", other.to_str().unwrap())
            .await
            .expect_err("unknown project");
        assert_eq!(err.status(), StatusCode::NOT_FOUND);

        let _ = std::fs::remove_dir_all(base);
    }

    /// `GET /projects/{id}/git/tools` answers 200 with `tools.git == true`
    /// (`git` is implied by the route family, not probed).
    #[tokio::test]
    async fn git_tools_reports_git_available() {
        let base = std::env::temp_dir().join(format!("bebok-git-tools-{}", uuid::Uuid::new_v4()));
        let project = base.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let (address, server, state) = serve_with(base.join("engine")).await;
        let config = state.store.config_db_path();
        let (entry, _) =
            bebok_core::config::projects::add_at(&config, project.to_str().unwrap(), None).unwrap();

        let response = authed(
            address,
            "GET",
            &format!("/projects/{}/git/tools", entry.id),
            "",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let body = response.split("\r\n\r\n").nth(1).unwrap_or("");
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["tools"]["git"], serde_json::json!(true), "{json}");

        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }

    /// `GET /projects/{id}/git/remotes` on a fresh repository answers 200 with
    /// an empty list.
    #[tokio::test]
    async fn git_remotes_lists_an_empty_set_for_a_fresh_repo() {
        if !std::process::Command::new("git")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            eprintln!("skipping: git not on PATH");
            return;
        }
        let base = std::env::temp_dir().join(format!("bebok-git-remotes-{}", uuid::Uuid::new_v4()));
        let repo = base.join("repo");
        init_repo(&repo);
        let (address, server, state) = serve_with(base.join("engine")).await;
        let config = state.store.config_db_path();
        let (entry, _) =
            bebok_core::config::projects::add_at(&config, repo.to_str().unwrap(), None).unwrap();

        let response = authed(
            address,
            "GET",
            &format!("/projects/{}/git/remotes", entry.id),
            "",
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"), "{response}");
        let body = response.split("\r\n\r\n").nth(1).unwrap_or("");
        let json: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(json["remotes"], serde_json::json!([]), "{json}");

        server.abort();
        let _ = std::fs::remove_dir_all(base);
    }
}
