//! One-shot import of the file-based config layers into the config DB
//! (`config_kv`, `config_docs`, `config_import_state`, `config_secrets`).
//!
//! Layers:
//! - global  (`~/.config/bebok/config.json`) -> scope `global`; every key,
//!   including `mcp` and `skills`.
//! - project (`<dir>/.bebok/config.json`)    -> scope `project:<dir>`; every key
//!   except `mcp` (ignored: MCP is global-only). `skills` keeps only the
//!   per-skill disables (`false`), so a project can turn a skill off.
//!
//! Rules:
//! - Idempotent: a layer whose raw-file sha256 equals
//!   `config_import_state.source_sha` is a no-op.
//! - Both files are parsed before anything is written, so a broken file aborts
//!   the whole import with nothing changed.
//! - A file is retired (renamed to `config.json.bak`, never overwritten) only
//!   once the DB holds exactly its content.
//! - Secrets: `sha256(salt || name || secret)` + `last4` go to `config_secrets`.
//!   The plaintext value stays in `config_kv`, because the runtime still reads
//!   provider keys from there; the hash row is a fingerprint and a display aid.
//! - Conflicts (a key already in the DB with a different value) are resolved by
//!   the file: the import wins. The conflict list is summarised by the Claude
//!   CLI when it is installed (best effort), otherwise by a plain code summary.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use super::secrets::{fingerprint, last4, make_salt};
use super::store::{GLOBAL_SCOPE, project_scope};

const CLAUDE_SUMMARY_TIMEOUT: Duration = Duration::from_secs(90);

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A top-level key that already had a different value in the DB.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Conflict {
    pub scope: String,
    pub key: String,
}

/// Outcome of one `import_to_db` run.
#[derive(Debug, Default, Serialize)]
pub struct ImportReport {
    /// Scopes whose content was written to the DB by this run.
    pub scopes: Vec<String>,
    /// Top-level keys written across the imported scopes.
    pub keys: usize,
    /// Secret fingerprints recorded across the imported scopes.
    pub secrets: usize,
    /// Scopes already matching their file (no-op).
    pub unchanged: Vec<String>,
    /// Original `config.json` paths that no longer exist after the run.
    pub deleted_files: Vec<String>,
    /// Backup copies left behind by the retired files.
    pub backups: Vec<String>,
    /// Project entries dropped on purpose (e.g. project `mcp`).
    pub skipped: Vec<String>,
    pub conflicts: Vec<Conflict>,
}

/// Result of a conflict summary, with its source made explicit.
#[derive(Debug, Clone, Serialize)]
pub struct ConflictSummary {
    /// `claude-cli` when the Claude CLI produced it, `code` otherwise.
    pub source: &'static str,
    pub text: String,
}

struct SecretDraft {
    kind: &'static str,
    name: String,
    env_var: String,
    secret: String,
}

#[derive(Default)]
struct ScopeImport {
    changed: bool,
    keys: usize,
    secrets: usize,
    conflicts: Vec<String>,
}

fn db_source_sha(conn: &Connection, scope: &str) -> Result<Option<String>, String> {
    conn.query_row(
        "SELECT source_sha FROM config_import_state WHERE scope=?1",
        [scope],
        |r| r.get(0),
    )
    .optional()
    .map_err(|e| e.to_string())
}

fn non_empty_secret(v: Option<&Value>) -> Option<String> {
    v.and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(str::to_string)
}

/// Secrets of one layer: top-level `api_key` and each `providers[].api_key`.
fn collect_secrets(value: &Value) -> Vec<SecretDraft> {
    let mut out = Vec::new();
    if let Some(secret) = non_empty_secret(value.get("api_key")) {
        out.push(SecretDraft {
            kind: "api_key",
            name: "default".into(),
            env_var: String::new(),
            secret,
        });
    }
    for provider in value
        .get("providers")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(name) = provider.get("name").and_then(Value::as_str) else {
            continue;
        };
        let Some(secret) = non_empty_secret(provider.get("api_key")) else {
            continue;
        };
        out.push(SecretDraft {
            kind: "provider_api_key",
            name: name.to_string(),
            env_var: format!("{}_API_KEY", name.to_ascii_uppercase().replace('-', "_")),
            secret,
        });
    }
    out
}

/// Upsert one secret fingerprint. An unchanged secret keeps its salt; a changed
/// one gets a fresh salt and `rotated_at`.
fn upsert_secret(conn: &Connection, scope: &str, s: &SecretDraft, now: i64) -> Result<(), String> {
    let existing: Option<(Vec<u8>, Vec<u8>, Option<i64>)> = conn
        .query_row(
            "SELECT salt, digest, deleted_at FROM config_secrets WHERE scope=?1 AND kind=?2 AND name=?3",
            params![scope, s.kind, s.name],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(|e| e.to_string())?;
    if let Some((salt, digest, deleted_at)) = &existing
        && deleted_at.is_none()
        && fingerprint(salt, &s.name, &s.secret) == *digest
    {
        return Ok(());
    }
    let salt = make_salt();
    let digest = fingerprint(&salt, &s.name, &s.secret);
    let last = last4(&s.secret);
    match existing {
        None => conn.execute(
            "INSERT INTO config_secrets(scope,kind,name,env_var,hash_algo,salt,digest,last4,storage,created_at,updated_at)
             VALUES(?1,?2,?3,?4,'sha256',?5,?6,?7,'config_kv',?8,?8)",
            params![scope, s.kind, s.name, s.env_var, salt, digest, last, now],
        ),
        Some(_) => conn.execute(
            "UPDATE config_secrets SET env_var=?4, hash_algo='sha256', salt=?5, digest=?6, last4=?7,
                 storage='config_kv', rotated_at=?8, updated_at=?8, deleted_at=NULL
             WHERE scope=?1 AND kind=?2 AND name=?3",
            params![scope, s.kind, s.name, s.env_var, salt, digest, last, now],
        ),
    }
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Import one scope in a single transaction. No-op when the DB already holds
/// a source with the same sha.
fn import_scope(
    conn: &Connection,
    scope: &str,
    source_path: &str,
    source_sha: &str,
    content: &str,
    secrets: &[SecretDraft],
) -> Result<ScopeImport, String> {
    if db_source_sha(conn, scope)?.as_deref() == Some(source_sha) {
        return Ok(ScopeImport::default());
    }
    let v: Value = super::jsonc::parse(content).map_err(|e| e.to_string())?;
    let obj = v.as_object().ok_or("config must be object")?;
    let now = crate::util::now_ms();
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let mut conflicts = Vec::new();
    for (k, val) in obj {
        let old: Option<String> = tx
            .query_row(
                "SELECT value_json FROM config_kv WHERE scope=?1 AND key=?2",
                params![scope, k],
                |r| r.get(0),
            )
            .optional()
            .map_err(|e| e.to_string())?;
        if let Some(old) = old
            && serde_json::from_str::<Value>(&old).ok().as_ref() != Some(val)
        {
            conflicts.push(k.clone());
        }
        let vj = serde_json::to_string(val).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO config_kv(scope,key,value_json,revision,updated_at) VALUES(?1,?2,?3,1,?4)
             ON CONFLICT(scope,key) DO UPDATE SET value_json=excluded.value_json, revision=revision+1, updated_at=excluded.updated_at",
            params![scope, k, vj, now],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO config_history(scope,key,old_value,new_value,ts) VALUES(?1,?2,NULL,?3,?4)",
            params![scope, k, vj, now],
        )
        .map_err(|e| e.to_string())?;
    }
    tx.execute(
        "INSERT INTO config_docs(scope,raw_jsonc,imported_at,dirty) VALUES(?1,?2,?3,0)
         ON CONFLICT(scope) DO UPDATE SET raw_jsonc=excluded.raw_jsonc, imported_at=excluded.imported_at, dirty=0",
        params![scope, content, now],
    )
    .map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO config_import_state(scope,source_path,source_sha,imported_at) VALUES(?1,?2,?3,?4)
         ON CONFLICT(scope) DO UPDATE SET source_path=excluded.source_path, source_sha=excluded.source_sha, imported_at=excluded.imported_at",
        params![scope, source_path, source_sha, now],
    )
    .map_err(|e| e.to_string())?;
    for s in secrets {
        upsert_secret(&tx, scope, s, now)?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(ScopeImport {
        changed: true,
        keys: obj.len(),
        secrets: secrets.len(),
        conflicts,
    })
}

/// Read and parse one layer file. `None` when the file does not exist.
fn read_layer(path: &Path) -> Result<Option<(String, Value)>, String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("read {}: {e}", path.display())),
    };
    let value =
        super::jsonc::parse(&raw).map_err(|e| format!("invalid {}: {e}", path.display()))?;
    if !value.is_object() {
        return Err(format!("{} must be a JSON object", path.display()));
    }
    Ok(Some((raw, value)))
}

/// Project layer as stored in the DB: everything except `mcp`; `skills` keeps
/// only the disabled entries. Dropped entries are recorded in `skipped`.
fn project_db_content(value: &Value, skipped: &mut Vec<String>) -> Map<String, Value> {
    let mut out = Map::new();
    let Some(obj) = value.as_object() else {
        return out;
    };
    for (k, v) in obj {
        match k.as_str() {
            "mcp" => skipped.push("mcp (project MCP is ignored; MCP is global-only)".into()),
            "skills" => match v.as_object() {
                Some(toggles) => {
                    let disabled: Map<String, Value> = toggles
                        .iter()
                        .filter(|(_, on)| on.as_bool() == Some(false))
                        .map(|(name, on)| (name.clone(), on.clone()))
                        .collect();
                    let dropped = toggles.len() - disabled.len();
                    if dropped > 0 {
                        skipped.push(format!(
                            "skills: {dropped} non-disabling entr{} ignored",
                            if dropped == 1 { "y" } else { "ies" }
                        ));
                    }
                    if !disabled.is_empty() {
                        out.insert(k.clone(), Value::Object(disabled));
                    }
                }
                None => skipped.push("skills (not an object) ignored".into()),
            },
            _ => {
                out.insert(k.clone(), v.clone());
            }
        }
    }
    out
}

fn record(report: &mut ImportReport, scope: &str, res: ScopeImport) {
    if !res.changed {
        report.unchanged.push(scope.to_string());
        return;
    }
    report.scopes.push(scope.to_string());
    report.keys += res.keys;
    report.secrets += res.secrets;
    report
        .conflicts
        .extend(res.conflicts.into_iter().map(|key| Conflict {
            scope: scope.to_string(),
            key,
        }));
}

/// Rename `config.json` to `config.json.bak` (or a timestamped name if a backup
/// already exists). Returns the backup path.
fn retire_file(path: &Path) -> Result<PathBuf, String> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.json".into());
    let mut backup = path.with_file_name(format!("{name}.bak"));
    if backup.exists() {
        backup = path.with_file_name(format!("{name}.bak.{}", crate::util::now_ms()));
    }
    std::fs::rename(path, &backup).map_err(|e| format!("retire {}: {e}", path.display()))?;
    Ok(backup)
}

/// Import the global and project config files into the DB. Synchronous: call
/// it from `spawn_blocking`. `conn` must come from `config::store::open_config_db`.
pub fn import_to_db(
    conn: &Connection,
    project_dir: &Path,
    global_path: &Path,
    project_path: &Path,
) -> Result<ImportReport, String> {
    let mut report = ImportReport::default();
    // Parse both layers before writing anything.
    let global = read_layer(global_path)?;
    let project = read_layer(project_path)?;

    // (file, scope, sha of the raw file) of every layer present on disk.
    let mut present: Vec<(&Path, String, String)> = Vec::new();

    if let Some((raw, value)) = &global {
        let sha = sha_hex(raw.as_bytes());
        let res = import_scope(
            conn,
            GLOBAL_SCOPE,
            &global_path.to_string_lossy(),
            &sha,
            raw,
            &collect_secrets(value),
        )?;
        record(&mut report, GLOBAL_SCOPE, res);
        present.push((global_path, GLOBAL_SCOPE.to_string(), sha));
    }

    if let Some((raw, value)) = &project {
        let scope = project_scope(project_dir);
        let mut skipped = Vec::new();
        let filtered = project_db_content(value, &mut skipped);
        let content = serde_json::to_string(&Value::Object(filtered)).map_err(|e| e.to_string())?;
        let sha = sha_hex(raw.as_bytes());
        let res = import_scope(
            conn,
            &scope,
            &project_path.to_string_lossy(),
            &sha,
            &content,
            &collect_secrets(value),
        )?;
        report.skipped.extend(skipped);
        record(&mut report, &scope, res);
        present.push((project_path, scope, sha));
    }

    // Retire a file only when the DB holds exactly its content.
    for (path, scope, sha) in present {
        if db_source_sha(conn, &scope)?.as_deref() != Some(sha.as_str()) {
            continue;
        }
        let backup = retire_file(path)?;
        report
            .deleted_files
            .push(path.to_string_lossy().into_owned());
        report.backups.push(backup.to_string_lossy().into_owned());
    }
    Ok(report)
}

/// Locate the Claude CLI (`claude`, env override or `PATH`). `None` when absent.
pub fn detect_claude() -> Option<PathBuf> {
    let backend = bebok_cli_agents::backend_for("claude")?;
    bebok_cli_agents::discover_binary(backend.as_ref())
}

fn code_summary(conflicts: &[Conflict]) -> ConflictSummary {
    let list = conflicts
        .iter()
        .map(|c| format!("{}:{}", c.scope, c.key))
        .collect::<Vec<_>>()
        .join(", ");
    ConflictSummary {
        source: "code",
        text: format!(
            "{} key(s) overwritten by file values: {list}",
            conflicts.len()
        ),
    }
}

/// Run the Claude CLI once in print mode over the conflict key names. Only key
/// names are sent: never values, because provider keys are among them.
async fn claude_summary(bin: &Path, conflicts: &[Conflict]) -> Result<String, String> {
    let mut prompt = String::from(
        "Summarize these config key overwrites in at most 5 short bullet points. \
         Only key names are given, no values; do not guess values.\n",
    );
    for c in conflicts {
        prompt.push_str(&format!("- {}: {}\n", c.scope, c.key));
    }
    let dir = std::env::temp_dir().join("bebok-config-import");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut cmd = tokio::process::Command::new(bin);
    cmd.arg("-p")
        .arg(&prompt)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(CLAUDE_SUMMARY_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!("exit status {}", out.status));
    }
    let text = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if text.is_empty() {
        return Err("empty output".into());
    }
    Ok(text)
}

/// Summarise conflicts: the Claude CLI when available (best effort), else a
/// code summary. Never fails and never changes the merge result.
pub async fn summarize_conflicts(
    conflicts: &[Conflict],
    claude: Option<&Path>,
) -> Option<ConflictSummary> {
    if conflicts.is_empty() {
        return None;
    }
    let Some(bin) = claude else {
        tracing::info!(
            "config import: claude-cli not found; merged {} conflict(s) by code (file wins)",
            conflicts.len()
        );
        return Some(code_summary(conflicts));
    };
    tracing::info!(
        "config import: claude-cli at {}; summarizing {} conflict(s)",
        bin.display(),
        conflicts.len()
    );
    match claude_summary(bin, conflicts).await {
        Ok(text) => Some(ConflictSummary {
            source: "claude-cli",
            text,
        }),
        Err(e) => {
            tracing::warn!("config import: claude-cli summary failed ({e}); using code summary");
            Some(code_summary(conflicts))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("bebok-import-{tag}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn import_is_idempotent_and_retires_files() {
        let root = temp_dir("idem");
        let project = root.join("proj");
        std::fs::create_dir_all(project.join(".bebok")).unwrap();
        let global_path = root.join("global.json");
        let project_path = project.join(".bebok").join("config.json");
        std::fs::write(
            &global_path,
            r#"{ "model": "zai/a", "api_key": "sk-global-1234", "skills": { "x": true, "y": true },
                 "mcp": { "servers": [] } }"#,
        )
        .unwrap();
        std::fs::write(
            &project_path,
            r#"{ "model": "zai/b", "skills": { "x": false, "y": true },
                 "mcp": { "servers": [{ "name": "p" }] } }"#,
        )
        .unwrap();

        let conn = crate::db::open_memory().unwrap();
        let first = import_to_db(&conn, &project, &global_path, &project_path).unwrap();
        assert_eq!(first.scopes.len(), 2);
        assert_eq!(first.deleted_files.len(), 2);
        assert!(first.skipped.iter().any(|s| s.starts_with("mcp")));
        assert_eq!(first.conflicts, Vec::new(), "fresh DB has no conflicts");
        assert!(!global_path.exists() && !project_path.exists());
        assert!(global_path.with_file_name("global.json.bak").exists());

        // Project skills keep only the disable; project mcp is dropped.
        let proj_skills: String = conn
            .query_row(
                "SELECT value_json FROM config_kv WHERE scope=?1 AND key='skills'",
                [project_scope(&project)],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(proj_skills, r#"{"x":false}"#);
        let proj_mcp: Option<String> = conn
            .query_row(
                "SELECT value_json FROM config_kv WHERE scope=?1 AND key='mcp'",
                [project_scope(&project)],
                |r| r.get(0),
            )
            .optional()
            .unwrap();
        assert!(proj_mcp.is_none());
        let secrets: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM config_secrets WHERE last4='1234'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(secrets, 1);

        // Second run: nothing to import, nothing to retire.
        let second = import_to_db(&conn, &project, &global_path, &project_path).unwrap();
        assert!(second.scopes.is_empty());
        assert!(second.deleted_files.is_empty());
        assert_eq!(second.keys, 0);

        // A changed file with a conflicting key is imported and reported.
        std::fs::write(&global_path, r#"{ "model": "zai/c" }"#).unwrap();
        let third = import_to_db(&conn, &project, &global_path, &project_path).unwrap();
        assert_eq!(third.scopes, vec![GLOBAL_SCOPE.to_string()]);
        assert_eq!(
            third.conflicts,
            vec![Conflict {
                scope: GLOBAL_SCOPE.into(),
                key: "model".into()
            }]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn broken_file_writes_nothing() {
        let root = temp_dir("broken");
        let global_path = root.join("global.json");
        let project_path = root.join("proj").join(".bebok").join("config.json");
        std::fs::write(&global_path, "{ not json").unwrap();
        let conn = crate::db::open_memory().unwrap();
        assert!(import_to_db(&conn, &root, &global_path, &project_path).is_err());
        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM config_kv", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 0);
        assert!(global_path.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
