//! `?directory=` query shared by directory-keyed endpoints.
//!
//! `PUT /config` additionally accepts `?scope=global|project` (which layer to
//! write) and `?replace=true` (replace the whole file instead of merging).

use serde::Deserialize;

/// `?directory=` query for endpoints that require it (agents/mcp/config).
/// `PUT /config` additionally accepts `?scope=global|project` (which layer to
/// write) and `?replace=true` (replace the whole file instead of merging).
#[derive(Deserialize)]
pub struct DirectoryQuery {
    pub directory: String,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub replace: Option<bool>,
}
