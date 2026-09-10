//! Plugin host + event-observer API (pluginowalność).
//!
//! Plugins are in-process, async observers that hook into engine events
//! (`EventBus` fan-out) and into explicit lifecycle hook points in the agent
//! loop. There is **no dynamic loading** here: a "plugin" is any object that
//! implements [`BebokPlugin`] and is registered on the [`PluginHost`] by the
//! embedding application (the server binary, a test, a future cdylib loader).
//!
//! Plugins can:
//!
//! - observe every bus event through the observer pipe (nothing lost: the
//!   [`PluginHost`] keeps a permanent bus subscriber that never lags), and
//! - run synchronous filters (mutate & veto) at defined [`Hook`] points in the
//!   agent lifecycle (before the LLM request, before/after a tool call, at the
//!   end of a turn, after a permission decision).
//!
//! Plugins resolve in registration order for every hook. A plugin that mutates
//! state at a hook is responsible for keeping the change consistent with the
//! rest of the engine (the disk journal stays authoritative).

use std::fmt;
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::event::{Event, EventBus};

/// A hook point in the agent lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Hook(pub &'static str);

impl Hook {
    /// Before the provider request is sent to the LLM. Payload: [`RequestHook`].
    pub const BEFORE_REQUEST: Hook = Hook("before.request");
    /// Before a tool executes (after the permission gate allowed it). Payload:
    /// [`ToolCallHook`] — `allowed = false` vetoes the call.
    pub const BEFORE_TOOL: Hook = Hook("before.tool");
    /// After a tool completed (success or error). Payload: [`ToolResultHook`].
    pub const AFTER_TOOL: Hook = Hook("after.tool");
    /// After a turn finished (final answer persisted or error). Payload:
    /// [`TurnHook`].
    pub const TURN_END: Hook = Hook("turn.end");
    /// After a permission decision was made (ask resolved / cache hit).
    /// Payload: [`PermissionHook`].
    pub const PERMISSION_RESOLVED: Hook = Hook("permission.resolved");
}

impl fmt::Display for Hook {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Payload for [`Hook::BEFORE_REQUEST`]: the provider request a plugin may
/// inspect and mutate (append system text, ...).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestHook {
    pub model: String,
    pub system: String,
    pub messages: Vec<RequestMessage>,
}

/// One provider-request message, mirrored for plugins (role + text + counts).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestMessage {
    pub role: String,
    pub text: String,
    pub tool_calls: usize,
    pub tool_results: usize,
}

impl RequestHook {
    pub fn new(
        model: impl Into<String>,
        system: impl Into<String>,
        messages: Vec<RequestMessage>,
    ) -> Self {
        Self {
            model: model.into(),
            system: system.into(),
            messages,
        }
    }

    /// Append extra system-prompt text (used by "inject into prompt" plugins).
    pub fn push_system(&mut self, text: &str) {
        if !self.system.is_empty() {
            self.system.push('\n');
        }
        self.system.push_str(text);
    }
}

/// Payload for [`Hook::BEFORE_TOOL`]: veto gate before execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallHook {
    pub tool: String,
    pub input: serde_json::Value,
    pub allowed: bool,
}

impl ToolCallHook {
    pub fn deny(&mut self) {
        self.allowed = false;
    }
}

/// Payload for [`Hook::AFTER_TOOL`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResultHook {
    pub tool: String,
    pub ok: bool,
    pub output: String,
}

/// Payload for [`Hook::TURN_END`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnHook {
    pub ok: bool,
    pub messages: usize,
}

/// Payload for [`Hook::PERMISSION_RESOLVED`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionHook {
    pub tool: String,
    pub decision: String, // "allow" | "deny"
    pub pattern: String,
}

/// Result of running one plugin at one hook.
#[derive(Debug)]
pub enum HookResult {
    /// No changes; continue with the next plugin.
    Continue,
    /// The payload was mutated; keep going.
    Changed,
    /// Short-circuit: stop running further plugins for this hook (optional
    /// message is logged).
    Stop(Option<String>),
}

/// A plugin: event observer + typed hook filters.
///
/// `on_event` is the observer side (observe-only; cannot veto). `on_hook`
/// receives the hook point and a JSON snapshot of the typed payload; override
/// it to mutate (return `Changed`) or veto (return `Stop`). Payload types are
/// decoupled through JSON so every plugin only has to know the payloads it
/// actually cares about.
#[async_trait]
pub trait BebokPlugin: Send + Sync {
    /// Human-readable plugin id (e.g. `"telemetry"`, `"guard"`).
    fn name(&self) -> &str;

    /// Observe one event from the bus. Default: no-op.
    async fn on_event(&self, _event: &Event) {}

    /// Run at one hook. Default: no-op (`Continue`).
    async fn on_hook(&self, _hook: Hook, _payload: &mut serde_json::Value) -> HookResult {
        HookResult::Continue
    }
}

/// Shared host state behind [`PluginHost`].
struct PluginInner {
    plugins: RwLock<Vec<Arc<dyn BebokPlugin>>>,
    /// The attached bus (guarded; attach is idempotent).
    bus: RwLock<Option<EventBus>>,
    dispatch: tokio::sync::mpsc::UnboundedSender<Event>,
}

/// The plugin host: owns plugins, fans events out to them, runs hooks.
///
/// Event-observer semantics: [`PluginHost::attach`] installs a *permanent* bus
/// subscriber (created before the host can race publishes), so registered
/// plugins observe every event the engine publishes — nothing is lost to a
/// lagging/absent SSE client.
#[derive(Clone)]
pub struct PluginHost {
    inner: Arc<PluginInner>,
}

impl Default for PluginHost {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginHost {
    pub fn new() -> Self {
        let (dispatch, mut rx) = tokio::sync::mpsc::unbounded_channel::<Event>();
        let inner = Arc::new(PluginInner {
            plugins: RwLock::new(Vec::new()),
            bus: RwLock::new(None),
            dispatch,
        });
        let inner2 = inner.clone();
        // Dedicated dispatcher: plugin observers run off the hot publish path.
        tokio::spawn(async move {
            while let Some(event) = rx.recv().await {
                let plugins = inner2.plugins.read().await.clone();
                for p in plugins {
                    p.on_event(&event).await;
                }
            }
        });
        Self { inner }
    }

    /// The process-wide host. The server attaches the engine bus and registers
    /// built-in plugins here at startup; the agent loop reads hooks from it, so
    /// turn code stays decoupled from the plugin wiring.
    pub fn global() -> PluginHost {
        static GLOBAL: OnceLock<PluginHost> = OnceLock::new();
        GLOBAL.get_or_init(PluginHost::new).clone()
    }

    /// Attach the engine bus. Every event published on it is forwarded to each
    /// plugin, in order, off the publish path. Idempotent (first bus wins).
    pub async fn attach(&self, bus: &EventBus) {
        let mut slot = self.inner.bus.write().await;
        if slot.is_some() {
            return;
        }
        *slot = Some(bus.clone());
        let mut rx = bus.subscribe();
        let dispatch = self.inner.dispatch.clone();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(event) => {
                        if dispatch.send(event).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                        tracing::warn!("plugin event pipe lagged, dropped {n} events");
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Register a plugin (appended; hooks resolve in registration order).
    /// Registering the same name again replaces the previous plugin.
    pub async fn register(&self, plugin: Arc<dyn BebokPlugin>) {
        let mut plugins = self.inner.plugins.write().await;
        if let Some(pos) = plugins.iter().position(|p| p.name() == plugin.name()) {
            tracing::warn!(
                "plugin '{}' already registered; replacing it",
                plugin.name()
            );
            plugins[pos] = plugin;
            return;
        }
        plugins.push(plugin);
    }

    /// Remove a registered plugin by id. Returns true when it was present.
    pub async fn unregister(&self, name: &str) -> bool {
        let mut plugins = self.inner.plugins.write().await;
        let before = plugins.len();
        plugins.retain(|p| p.name() != name);
        plugins.len() != before
    }

    /// Ids of the registered plugins, in resolution order.
    pub async fn names(&self) -> Vec<String> {
        self.inner
            .plugins
            .read()
            .await
            .iter()
            .map(|p| p.name().to_string())
            .collect()
    }

    /// True when at least one plugin is registered (cheap no-op fast path).
    pub async fn has_plugins(&self) -> bool {
        !self.inner.plugins.read().await.is_empty()
    }

    /// Run every plugin at `hook`, in registration order, stopping early on a
    /// `Stop`. `payload` is (de)serialized through JSON per plugin so each
    /// plugin sees exactly the typed payload it knows. Plugin errors never
    /// abort the turn — they are logged and skipped.
    pub async fn run_hook<T>(&self, hook: Hook, payload: &mut T)
    where
        T: Send + Clone + Serialize + serde::de::DeserializeOwned + 'static,
    {
        let plugins = self.inner.plugins.read().await.clone();
        for p in plugins {
            let mut value = match serde_json::to_value(payload.clone()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            match p.on_hook(hook, &mut value).await {
                HookResult::Continue => {}
                HookResult::Changed => {
                    if let Ok(decoded) = serde_json::from_value::<T>(value) {
                        *payload = decoded;
                    }
                }
                HookResult::Stop(message) => {
                    if let Some(m) = message {
                        tracing::warn!("plugin '{}' stopped hook {hook}: {m}", p.name());
                    }
                    break;
                }
            }
        }
    }
}

/// List the hook points the engine currently exposes (for tooling/docs).
pub fn hook_names() -> Vec<&'static str> {
    vec![
        Hook::BEFORE_REQUEST.0,
        Hook::BEFORE_TOOL.0,
        Hook::AFTER_TOOL.0,
        Hook::TURN_END.0,
        Hook::PERMISSION_RESOLVED.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Spy {
        events: AtomicUsize,
        veto: bool,
    }

    #[async_trait::async_trait]
    impl BebokPlugin for Spy {
        fn name(&self) -> &str {
            "spy"
        }
        async fn on_event(&self, _event: &Event) {
            self.events.fetch_add(1, Ordering::SeqCst);
        }
        async fn on_hook(&self, hook: Hook, payload: &mut serde_json::Value) -> HookResult {
            if hook == Hook::BEFORE_TOOL {
                if let Some(serde_json::Value::Bool(allowed)) = payload.get_mut("allowed") {
                    if self.veto && *allowed {
                        *allowed = false;
                        return HookResult::Changed;
                    }
                }
            }
            HookResult::Continue
        }
    }

    #[tokio::test]
    async fn observer_gets_bus_events() {
        let host = PluginHost::new();
        let spy = Arc::new(Spy {
            events: AtomicUsize::new(0),
            veto: false,
        });
        host.register(spy.clone()).await;

        let bus = EventBus::default();
        host.attach(&bus).await;
        // Wait for the bridge task to subscribe before publishing.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        for _ in 0..3 {
            bus.publish(Event::new("session.updated", "/tmp", "s1"));
        }
        // Give the dispatcher a moment to fan out.
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert_eq!(spy.events.load(Ordering::SeqCst), 3);

        assert!(host.names().await.contains(&"spy".to_string()));
        assert!(host.unregister("spy").await);
        assert!(!host.names().await.contains(&"spy".to_string()));
    }

    #[tokio::test]
    async fn hook_can_veto_tool_call() {
        let host = PluginHost::new();
        let spy = Arc::new(Spy {
            events: AtomicUsize::new(0),
            veto: true,
        });
        host.register(spy).await;

        let mut payload = ToolCallHook {
            tool: "bash".into(),
            input: serde_json::json!({ "command": "rm -rf /" }),
            allowed: true,
        };
        host.run_hook(Hook::BEFORE_TOOL, &mut payload).await;
        assert!(!payload.allowed, "plugin must veto the call");
    }

    #[tokio::test]
    async fn hook_can_mutate_system_prompt() {
        let host = PluginHost::new();
        struct SysInject;
        #[async_trait::async_trait]
        impl BebokPlugin for SysInject {
            fn name(&self) -> &str {
                "sysinject"
            }
            async fn on_hook(&self, hook: Hook, payload: &mut serde_json::Value) -> HookResult {
                if hook == Hook::BEFORE_REQUEST {
                    if let Some(serde_json::Value::String(sys)) = payload.get_mut("system") {
                        if !sys.contains("plugin note") {
                            sys.push_str("\nplugin note: injected");
                            return HookResult::Changed;
                        }
                    }
                }
                HookResult::Continue
            }
        }
        host.register(Arc::new(SysInject)).await;
        let mut payload = RequestHook::new("m", "base", vec![]);
        host.run_hook(Hook::BEFORE_REQUEST, &mut payload).await;
        assert!(payload.system.contains("plugin note: injected"));
    }
}
