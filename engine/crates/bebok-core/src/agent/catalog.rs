//! Agent catalog: built-ins + file presets + hot-reload watcher.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;

use super::preset::Agent;
use crate::event::{Event, EventBus};

/// Summary of an agent preset exposed to the GUI (`GET /agent`).
#[derive(Debug, Clone, Serialize)]
pub struct AgentInfo {
    pub name: String,
    pub description: Option<String>,
    pub model: Option<String>,
    pub builtin: bool,
    pub source: Option<String>,
}

/// A resolved catalog of agents for one instance: built-ins + file presets
/// from `~/.config/bebok/agent/*.md` and `<project>/.bebok/agent/*.md`
/// (project overrides global, both override built-ins by name).
#[derive(Debug, Clone, Default)]
pub struct AgentCatalog {
    builtins: Vec<Agent>,
    custom: BTreeMap<String, Agent>,
}

impl AgentCatalog {
    /// Load built-ins + file presets for a project directory.
    pub fn load(project: &Path) -> Self {
        let mut catalog = Self {
            builtins: Agent::builtins(),
            custom: BTreeMap::new(),
        };
        if let Some(dir) = dirs::config_dir().map(|d| d.join("bebok").join("agent")) {
            catalog.load_dir(&dir);
        }
        catalog.load_dir(&project.join(".bebok").join("agent"));
        catalog
    }

    fn load_dir(&mut self, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().map(|e| e != "md").unwrap_or(true) {
                continue;
            }
            if let Some(agent) = Agent::from_file(&path) {
                self.custom.insert(agent.name.clone(), agent);
            }
        }
    }

    /// Resolve an agent by name. Unknown names fall back to `code`.
    pub fn resolve(&self, name: &str) -> Agent {
        if let Some(a) = self.custom.get(name) {
            return a.clone();
        }
        if let Some(a) = self.builtins.iter().find(|a| a.name == name) {
            return a.clone();
        }
        tracing::warn!("unknown agent '{name}', falling back to 'code'");
        Agent::code()
    }

    pub fn contains(&self, name: &str) -> bool {
        self.custom.contains_key(name) || self.builtins.iter().any(|a| a.name == name)
    }

    /// All agents (custom first, then built-ins), as GUI summaries.
    pub fn list(&self) -> Vec<AgentInfo> {
        let mut out: Vec<AgentInfo> = self
            .custom
            .values()
            .map(|a| AgentInfo {
                name: a.name.clone(),
                description: a.description.clone(),
                model: a.model.clone(),
                builtin: false,
                source: a.source.as_ref().map(|p| p.display().to_string()),
            })
            .collect();
        for a in &self.builtins {
            // A custom file can shadow a built-in name.
            if self.custom.contains_key(&a.name) {
                continue;
            }
            out.push(AgentInfo {
                name: a.name.clone(),
                description: a.description.clone(),
                model: a.model.clone(),
                builtin: true,
                source: None,
            });
        }
        out
    }
}

/// Spawn a filesystem watcher that reloads the agent catalog and emits
/// `agent.list.changed` whenever an agent file changes (debounced).
///
/// Watches `~/.config/bebok/agent` and `<project>/.bebok/agent`. The returned
/// task lives for the process lifetime (matching the engine's "hot reload"
/// semantics); it keeps the `notify` watcher alive internally.
pub fn spawn_agent_watcher(
    project: PathBuf,
    catalog: Arc<std::sync::RwLock<AgentCatalog>>,
    bus: EventBus,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    use notify::{RecursiveMode, Watcher};

    let global_dir = dirs::config_dir().map(|d| d.join("bebok").join("agent"));
    let project_dir = project.join(".bebok").join("agent");
    // The project agent dir is created eagerly so it can be watched.
    if !project_dir.exists() {
        std::fs::create_dir_all(&project_dir)?;
    }

    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res
            && (ev.kind.is_create() || ev.kind.is_modify() || ev.kind.is_remove())
        {
            let _ = tx.send(());
        }
    })
    .map_err(|e| std::io::Error::other(e.to_string()))?;

    for dir in [global_dir.as_deref(), Some(project_dir.as_path())]
        .into_iter()
        .flatten()
    {
        if dir.exists() {
            let _ = watcher.watch(dir, RecursiveMode::NonRecursive);
        }
    }

    Ok(tokio::spawn(async move {
        let _watcher = watcher;
        loop {
            if rx.recv().await.is_none() {
                break;
            }
            // Debounce a burst of editor saves into a single reload.
            loop {
                tokio::select! {
                    _ = rx.recv() => continue,
                    _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => break,
                }
            }
            {
                let mut guard = catalog.write().unwrap();
                *guard = AgentCatalog::load(&project);
            }
            bus.publish(Event::new(
                "agent.list.changed",
                &project.to_string_lossy(),
                "",
            ));
        }
    }))
}
