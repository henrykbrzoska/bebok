pub const MIGRATION_CONFIG: &str = r#"
CREATE TABLE IF NOT EXISTS config_kv (
    scope TEXT NOT NULL,
    key TEXT NOT NULL,
    value_json TEXT NOT NULL,
    revision INTEGER NOT NULL DEFAULT 1,
    updated_at INTEGER NOT NULL,
    PRIMARY KEY (scope, key)
);
CREATE TABLE IF NOT EXISTS config_docs (
    scope TEXT PRIMARY KEY,
    raw_jsonc TEXT NOT NULL DEFAULT '{}',
    exported_at INTEGER,
    imported_at INTEGER,
    dirty INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE IF NOT EXISTS config_import_state (
    scope TEXT PRIMARY KEY,
    source_path TEXT NOT NULL,
    source_sha TEXT NOT NULL,
    imported_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS config_history (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scope TEXT NOT NULL,
    key TEXT NOT NULL,
    old_value TEXT,
    new_value TEXT,
    ts INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS config_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS config_secrets (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    scope TEXT NOT NULL,
    kind TEXT NOT NULL,
    name TEXT NOT NULL,
    env_var TEXT NOT NULL,
    hash_algo TEXT NOT NULL DEFAULT 'sha256',
    salt BLOB NOT NULL,
    digest BLOB NOT NULL,
    last4 TEXT NOT NULL DEFAULT '',
    storage TEXT NOT NULL DEFAULT 'env',
    secret_enc BLOB,
    nonce BLOB,
    created_at INTEGER NOT NULL,
    rotated_at INTEGER,
    updated_at INTEGER NOT NULL,
    deleted_at INTEGER,
    UNIQUE(scope,kind,name)
);
"#;
