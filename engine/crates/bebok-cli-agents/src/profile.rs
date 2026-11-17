//! Managed CLI profile: the files Bebok generates for an agentic CLI.
//!
//! Instead of handing the CLI Bebok's rules as prompt text, a profile turns
//! them into the CLI's own configuration (`settings.json`, MCP config, system
//! prompt file). The CLI then enforces them itself.
//!
//! Three steps, kept apart on purpose:
//!
//! 1. **Render** ([`ProfileWriter::render`], implemented per backend): a pure
//!    function from `(spec, request, probe, dir)` to [`RenderedProfile`]. No
//!    disk writes, so the output is golden-testable.
//! 2. **Validate** ([`validate_settings`]): the merged settings must not
//!    contain a permission bypass or drop Bebok's own hook entries.
//! 3. **Sync** ([`sync_profile`]): write the files atomically (tmp + rename),
//!    but only when the content hash in `profile.lock.json` differs from the
//!    freshly rendered one. An unchanged profile costs one file read per turn.
//!
//! The profile lives outside the repository (see [`default_profile_dir`]), so
//! it never shows in `git status`, and it holds no secrets: a turn token goes
//! through the environment, never into these files.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use crate::backend::{CliBackend, CliError, CliProbe, CliRequest, ProfileDir};
use crate::spec::CliSpec;

/// Name of the lock file inside a profile directory.
pub const LOCK_FILE: &str = "profile.lock.json";
/// Bump when the lock's own layout changes (not when rendered content does).
const LOCK_SCHEMA: u64 = 1;

/// One generated file, relative to the profile directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProfileFile {
    pub name: String,
    pub contents: String,
}

/// What a backend renders for one turn. Nothing here touches the disk.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderedProfile {
    /// Files to write into the profile directory.
    pub files: Vec<ProfileFile>,
    /// Extra argv, appended after the backend's own argv. Paths are absolute.
    pub args: Vec<String>,
    /// Extra environment for the child process.
    pub env: Vec<(String, String)>,
    /// The CLI flags `args` relies on, recorded in the lock so a flag change
    /// alone triggers a regeneration.
    pub flags: Vec<String>,
    /// CLI version the render was made against, recorded in the lock.
    pub version: Option<String>,
}

/// Inputs to one render.
#[derive(Debug, Clone, Copy)]
pub struct ProfileInput<'a> {
    pub spec: &'a CliSpec,
    pub req: &'a CliRequest,
    pub probe: &'a CliProbe,
    /// Absolute directory the files will be written to.
    pub dir: &'a Path,
}

/// A backend's profile generator.
pub trait ProfileWriter: Send + Sync {
    /// Render the profile for one turn. Must be deterministic for equal inputs.
    fn render(&self, input: &ProfileInput<'_>) -> Result<RenderedProfile, CliError>;
}

impl ProfileDir {
    /// The directory the profile lives in: the explicit one, or the default
    /// for this project and backend.
    pub fn resolve(&self, project: &Path, backend_id: &str) -> Result<PathBuf, CliError> {
        match &self.dir {
            Some(dir) => Ok(dir.clone()),
            None => default_profile_dir(project, backend_id),
        }
    }
}

/// `<user data dir>/bebok/cli-profiles/<hash of project>/<backend>`.
///
/// Keyed by the project path, so two providers of the same backend in one
/// project share a directory; the lock keeps them correct, not separate.
pub fn default_profile_dir(project: &Path, backend_id: &str) -> Result<PathBuf, CliError> {
    let data = dirs::data_dir().ok_or_else(|| {
        CliError::Scratch("no user data directory; cannot place the CLI profile".into())
    })?;
    Ok(data
        .join("bebok")
        .join("cli-profiles")
        .join(project_key(project))
        .join(backend_id))
}

fn project_key(project: &Path) -> String {
    let digest = Sha256::digest(project.to_string_lossy().as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

/// `<project>/.bebok/cli/<backend>/<file>`: the project's own layer.
pub fn project_layer_path(project: &Path, backend_id: &str, file: &str) -> PathBuf {
    project
        .join(".bebok")
        .join("cli")
        .join(backend_id)
        .join(file)
}

/// Read one project layer file, or `None` when the project does not have it.
pub fn read_project_layer(
    project: &Path,
    backend_id: &str,
    file: &str,
) -> Result<Option<String>, CliError> {
    let path = project_layer_path(project, backend_id, file);
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(Some(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(CliError::Config(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// The system prompt text for a profile: the turn's own prompt, then the
/// project's appended text. `None` when both are empty.
pub fn system_text(req_system: Option<&str>, project_append: Option<&str>) -> Option<String> {
    let parts: Vec<&str> = [req_system, project_append]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

/// Deep-merge `patch` into `target`: objects merge key by key, anything else
/// (including arrays and `null`) replaces the value.
pub fn deep_merge(target: &mut Value, patch: &Value) {
    if let (Value::Object(into), Value::Object(from)) = (&mut *target, patch) {
        for (key, value) in from {
            match into.get_mut(key) {
                Some(slot) => deep_merge(slot, value),
                None => {
                    into.insert(key.clone(), value.clone());
                }
            }
        }
    } else {
        *target = patch.clone();
    }
}

/// Check a merged `settings.json` against Bebok's rules.
///
/// * `permissions.defaultMode = bypassPermissions` is never allowed. Bebok's
///   YOLO mode maps to `acceptEdits` instead.
/// * Every hook entry in `base` (Bebok's own) must survive the overrides.
///   Overrides may add hooks, not remove Bebok's.
pub fn validate_settings(base: &Value, merged: &Value) -> Result<(), CliError> {
    if merged
        .pointer("/permissions/defaultMode")
        .and_then(Value::as_str)
        == Some("bypassPermissions")
    {
        return Err(CliError::Config(
            "profile settings may not set permissions.defaultMode to bypassPermissions; \
             Bebok maps YOLO to acceptEdits"
                .into(),
        ));
    }

    if let Some(base_hooks) = base.get("hooks").and_then(Value::as_object) {
        let merged_hooks = merged.get("hooks").and_then(Value::as_object);
        for (event, entries) in base_hooks {
            for entry in entries.as_array().into_iter().flatten() {
                let kept = merged_hooks
                    .and_then(|hooks| hooks.get(event))
                    .and_then(Value::as_array)
                    .is_some_and(|list| list.contains(entry));
                if !kept {
                    return Err(CliError::Config(format!(
                        "profile overrides may not remove Bebok's {event} hook"
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Pretty JSON with a trailing newline, the form every generated file uses.
pub fn to_json_file(value: &Value) -> Result<String, CliError> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|e| CliError::Scratch(format!("cannot serialise profile file: {e}")))?;
    text.push('\n');
    Ok(text)
}

/// Stable digest over everything a render decides: version, flags, argv, env
/// and every file's name and bytes. Equal renders give equal digests.
pub fn content_hash(rendered: &RenderedProfile) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"bebok-profile-v1\n");
    hasher.update(format!("version={}\n", rendered.version.as_deref().unwrap_or("")).as_bytes());
    let mut flags = rendered.flags.clone();
    flags.sort();
    flags.dedup();
    hasher.update(format!("flags={}\n", flags.join(",")).as_bytes());
    hasher.update(format!("args={}\n", rendered.args.join("\u{1f}")).as_bytes());
    for (key, value) in &rendered.env {
        hasher.update(format!("env={key}={value}\n").as_bytes());
    }
    let mut files: Vec<&ProfileFile> = rendered.files.iter().collect();
    files.sort_by(|a, b| a.name.cmp(&b.name));
    for file in files {
        hasher.update(format!("file={} {}\n", file.name, file.contents.len()).as_bytes());
        hasher.update(file.contents.as_bytes());
        hasher.update(b"\n");
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Write `rendered` into `dir`, unless the lock already records the same
/// content hash and every file is still present. Returns whether files were
/// written. An empty render touches nothing.
pub fn sync_profile(dir: &Path, rendered: &RenderedProfile) -> Result<bool, CliError> {
    if rendered.files.is_empty() {
        return Ok(false);
    }
    let hash = content_hash(rendered);
    if lock_matches(dir, &hash, rendered) {
        return Ok(false);
    }

    std::fs::create_dir_all(dir)
        .map_err(|e| CliError::Scratch(format!("{}: {e}", dir.display())))?;
    for file in &rendered.files {
        write_atomic(&dir.join(&file.name), &file.contents)?;
    }

    let mut flags = rendered.flags.clone();
    flags.sort();
    flags.dedup();
    let mut names: Vec<&str> = rendered.files.iter().map(|f| f.name.as_str()).collect();
    names.sort_unstable();
    let lock = json!({
        "schema": LOCK_SCHEMA,
        "generator": "bebok-cli-agents",
        "contentHash": hash,
        "version": rendered.version,
        "flags": flags,
        "files": names,
    });
    write_atomic(&dir.join(LOCK_FILE), &to_json_file(&lock)?)?;
    Ok(true)
}

fn lock_matches(dir: &Path, hash: &str, rendered: &RenderedProfile) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join(LOCK_FILE)) else {
        return false;
    };
    let Ok(lock) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    lock.get("contentHash").and_then(Value::as_str) == Some(hash)
        && rendered
            .files
            .iter()
            .all(|file| dir.join(&file.name).is_file())
}

/// Write via a uniquely named sibling and rename over the target, so a reader
/// (the CLI starting up) sees either the old file or the new one, never a
/// half-written one.
fn write_atomic(path: &Path, contents: &str) -> Result<(), CliError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("profile");
    let tmp = path.with_file_name(format!(
        "{name}.{}.{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&tmp, contents)
        .map_err(|e| CliError::Scratch(format!("{}: {e}", tmp.display())))?;
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        CliError::Scratch(format!("{}: {e}", path.display()))
    })
}

/// What the profile would contain for a project, without writing anything.
/// Shown in Settings; carries no tokens (the turn token is added per turn and
/// is not part of a render).
#[derive(Debug, Clone, Serialize)]
pub struct ProfilePreview {
    pub dir: String,
    pub files: Vec<ProfileFile>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub flags: Vec<String>,
    pub version: Option<String>,
}

/// Render the profile `backend` would use for `project` under `spec`, given a
/// probe of the installed binary. The turn-specific system prompt is absent,
/// so the preview shows the profile's own content only.
pub fn preview_profile(
    backend: &dyn CliBackend,
    spec: &CliSpec,
    project: &Path,
    probe: &CliProbe,
) -> Result<ProfilePreview, CliError> {
    let writer = backend
        .profile_writer()
        .ok_or_else(|| CliError::Config(format!("{} has no managed profile", backend.label())))?;
    let dir = ProfileDir::default().resolve(project, backend.id())?;
    let req = CliRequest {
        prompt: String::new(),
        blocks: Vec::new(),
        system: None,
        model: None,
        cwd: project.to_path_buf(),
        resume_key: None,
        write: spec.allow_write,
        max_turns: None,
        timeout: spec.timeout(),
        extra_env: Vec::new(),
    };
    let rendered = writer.render(&ProfileInput {
        spec,
        req: &req,
        probe,
        dir: &dir,
    })?;
    Ok(ProfilePreview {
        dir: dir.display().to_string(),
        files: rendered.files,
        args: rendered.args,
        env: rendered.env,
        flags: rendered.flags,
        version: rendered.version,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// A fresh directory under the system temp dir, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "bebok-profile-test-{}-{}-{}",
                std::process::id(),
                tag,
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn sample(contents: &str) -> RenderedProfile {
        RenderedProfile {
            files: vec![
                ProfileFile {
                    name: "settings.json".into(),
                    contents: contents.into(),
                },
                ProfileFile {
                    name: "system.md".into(),
                    contents: "be terse\n".into(),
                },
            ],
            args: vec!["--settings".into(), "/x/settings.json".into()],
            env: vec![("CLAUDE_CONFIG_DIR".into(), "/x".into())],
            flags: vec!["--settings".into()],
            version: Some("2.1.325 (Claude Code)".into()),
        }
    }

    #[test]
    fn deep_merge_merges_objects_and_replaces_everything_else() {
        let mut target = json!({ "env": { "A": "1", "B": "2" }, "list": [1, 2], "keep": true });
        deep_merge(
            &mut target,
            &json!({ "env": { "B": "9", "C": "3" }, "list": [7], "keep": null }),
        );
        assert_eq!(
            target,
            json!({ "env": { "A": "1", "B": "9", "C": "3" }, "list": [7], "keep": null })
        );
    }

    #[test]
    fn bypass_permissions_is_rejected_after_merge() {
        let base = json!({ "permissions": { "defaultMode": "acceptEdits" } });
        let mut merged = base.clone();
        deep_merge(
            &mut merged,
            &json!({ "permissions": { "defaultMode": "bypassPermissions" } }),
        );
        let err = validate_settings(&base, &merged).unwrap_err();
        assert!(
            matches!(err, CliError::Config(ref m) if m.contains("bypassPermissions")),
            "{err:?}"
        );
        assert!(validate_settings(&base, &base).is_ok());
    }

    #[test]
    fn overrides_may_not_remove_bebok_hooks() {
        let entry = json!({ "matcher": "*", "hooks": [{ "type": "command", "command": "bebok cli-hook pre" }] });
        let base = json!({ "hooks": { "PreToolUse": [entry.clone()] } });

        // Arrays replace on merge, so an override that lists only its own
        // PreToolUse entry silently drops Bebok's. The check must catch that.
        let mut replaced = base.clone();
        deep_merge(
            &mut replaced,
            &json!({ "hooks": { "PreToolUse": [{ "matcher": "x", "hooks": [] }] } }),
        );
        assert!(validate_settings(&base, &replaced).is_err());

        // Keeping Bebok's entry next to the user's own is fine.
        let mut kept = base.clone();
        deep_merge(
            &mut kept,
            &json!({ "hooks": { "PreToolUse": [entry.clone(), { "matcher": "x", "hooks": [] }] } }),
        );
        assert!(validate_settings(&base, &kept).is_ok());

        let mut removed = base.clone();
        deep_merge(&mut removed, &json!({ "hooks": null }));
        assert!(validate_settings(&base, &removed).is_err());
    }

    #[test]
    fn hash_is_stable_and_follows_every_input() {
        let a = sample("{}\n");
        assert_eq!(content_hash(&a), content_hash(&sample("{}\n")));
        assert_ne!(content_hash(&a), content_hash(&sample("{\"x\":1}\n")));

        let mut other_version = sample("{}\n");
        other_version.version = Some("2.2.0".into());
        assert_ne!(content_hash(&a), content_hash(&other_version));

        let mut other_flags = sample("{}\n");
        other_flags.flags.push("--strict-mcp-config".into());
        assert_ne!(content_hash(&a), content_hash(&other_flags));
    }

    #[test]
    fn hash_ignores_file_order() {
        let a = sample("{}\n");
        let mut b = a.clone();
        b.files.reverse();
        assert_eq!(content_hash(&a), content_hash(&b));
    }

    #[test]
    fn sync_writes_files_and_a_lock_then_skips_when_unchanged() {
        let tmp = TempDir::new("sync");
        let dir = tmp.0.join("profile");
        let rendered = sample("{\"a\":1}\n");

        assert!(
            sync_profile(&dir, &rendered).unwrap(),
            "first render writes"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("settings.json")).unwrap(),
            "{\"a\":1}\n"
        );
        let lock: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join(LOCK_FILE)).unwrap()).unwrap();
        assert_eq!(lock["contentHash"], content_hash(&rendered));
        assert_eq!(lock["version"], "2.1.325 (Claude Code)");
        assert_eq!(lock["flags"], json!(["--settings"]));

        let before = std::fs::metadata(dir.join("settings.json"))
            .unwrap()
            .modified()
            .unwrap();
        std::thread::sleep(Duration::from_millis(20));
        assert!(
            !sync_profile(&dir, &rendered).unwrap(),
            "same hash: no rewrite"
        );
        let after = std::fs::metadata(dir.join("settings.json"))
            .unwrap()
            .modified()
            .unwrap();
        assert_eq!(before, after);

        // A changed input rewrites.
        assert!(sync_profile(&dir, &sample("{\"a\":2}\n")).unwrap());
        assert_eq!(
            std::fs::read_to_string(dir.join("settings.json")).unwrap(),
            "{\"a\":2}\n"
        );
    }

    #[test]
    fn sync_repairs_a_deleted_file_even_when_the_hash_matches() {
        let tmp = TempDir::new("repair");
        let dir = tmp.0.join("profile");
        let rendered = sample("{}\n");
        sync_profile(&dir, &rendered).unwrap();
        std::fs::remove_file(dir.join("system.md")).unwrap();
        assert!(sync_profile(&dir, &rendered).unwrap());
        assert!(dir.join("system.md").is_file());
    }

    #[test]
    fn empty_render_touches_nothing() {
        let tmp = TempDir::new("empty");
        let dir = tmp.0.join("never");
        assert!(!sync_profile(&dir, &RenderedProfile::default()).unwrap());
        assert!(!dir.exists());
    }

    #[test]
    fn atomic_write_leaves_no_temporary_files_behind() {
        let tmp = TempDir::new("atomic");
        sync_profile(&tmp.0, &sample("{}\n")).unwrap();
        let leftovers: Vec<String> = std::fs::read_dir(&tmp.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn project_layer_is_optional_and_read_verbatim() {
        let tmp = TempDir::new("layer");
        assert_eq!(
            read_project_layer(&tmp.0, "claude", "settings.json").unwrap(),
            None
        );
        let path = project_layer_path(&tmp.0, "claude", "settings.json");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{\"env\":{}}").unwrap();
        assert_eq!(
            read_project_layer(&tmp.0, "claude", "settings.json")
                .unwrap()
                .as_deref(),
            Some("{\"env\":{}}")
        );
    }

    #[test]
    fn system_text_puts_the_turn_prompt_first_and_drops_blanks() {
        assert_eq!(system_text(None, None), None);
        assert_eq!(system_text(Some("  "), Some("\n")), None);
        assert_eq!(system_text(Some("turn"), None).as_deref(), Some("turn"));
        assert_eq!(
            system_text(Some("turn\n"), Some(" project ")).as_deref(),
            Some("turn\n\nproject")
        );
    }

    #[test]
    fn default_profile_dir_is_outside_the_project_and_per_backend() {
        let project = Path::new("/work/some-project");
        let dir = default_profile_dir(project, "claude").unwrap();
        assert!(!dir.starts_with(project), "{}", dir.display());
        assert!(dir.ends_with("claude"));
        assert!(dir.to_string_lossy().contains("cli-profiles"));
        assert_ne!(
            default_profile_dir(Path::new("/work/other"), "claude").unwrap(),
            dir
        );
    }

    #[test]
    fn explicit_profile_dir_wins_over_the_default() {
        let explicit = ProfileDir {
            dir: Some(PathBuf::from("/custom/profile")),
        };
        assert_eq!(
            explicit.resolve(Path::new("/p"), "claude").unwrap(),
            PathBuf::from("/custom/profile")
        );
    }
}
