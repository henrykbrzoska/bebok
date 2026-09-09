//! InstanceStore (SPEC §3.10): per-directory runtime state.
//!
//! One engine process serves multiple working directories. Runtime state
//! (sessions, config, tool registry) is keyed by normalized path; sessions are
//! looked up globally by id. Nothing session-critical lives only in RAM: the
//! disk journal is authoritative, RAM is a cache.

pub mod instance;
pub mod instance_store;
pub mod lifecycle;
pub mod session_state;

pub use instance::Instance;
pub use instance_store::InstanceStore;
pub use session_state::SessionState;
