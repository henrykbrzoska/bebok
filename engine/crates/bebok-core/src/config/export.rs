use super::jsonc::JsoncDocument;
use rusqlite::Connection;

pub fn export_scope(conn: &Connection, scope: &str) -> Result<String, String> {
    let raw: Option<String> = conn
        .query_row(
            "SELECT raw_jsonc FROM config_docs WHERE scope=?1",
            [scope],
            |r| r.get(0),
        )
        .ok();
    let raw = raw.unwrap_or_else(|| "{}".to_string());
    let doc = JsoncDocument::parse(&raw).map_err(|e| e.to_string())?;
    // Rebuild from kv for dirty scopes
    let dirty: i64 = conn
        .query_row(
            "SELECT dirty FROM config_docs WHERE scope=?1",
            [scope],
            |r| r.get(0),
        )
        .unwrap_or(0);
    if dirty == 0 {
        return Ok(raw);
    }
    // dirty: reconstruct by applying kv onto raw
    let mut out = doc.raw().to_string();
    let mut stmt = conn
        .prepare("SELECT key,value_json FROM config_kv WHERE scope=?1")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([scope], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })
        .map_err(|e| e.to_string())?;
    for row in rows {
        let (k, vj) = row.map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_str(&vj).map_err(|e| e.to_string())?;
        let cur = JsoncDocument::parse(&out).map_err(|e| e.to_string())?;
        // handle removal: value_json == null and we want to remove key
        // For now, if value is JSON null we treat as removal if key exists
        // Use with_set for non-null, with_remove for null sentinel
        if v.is_null() {
            out = cur.with_remove(&k);
        } else {
            out = cur.with_set(&k, &v);
        }
    }
    Ok(out)
}

pub fn mark_exported(conn: &Connection, scope: &str, raw: &str) -> Result<(), String> {
    let now = crate::util::now_ms();
    conn.execute(
        "INSERT INTO config_docs(scope,raw_jsonc,exported_at,dirty) VALUES(?1,?2,?3,0)
         ON CONFLICT(scope) DO UPDATE SET raw_jsonc=excluded.raw_jsonc, exported_at=excluded.exported_at, dirty=0",
        rusqlite::params![scope, raw, now],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
