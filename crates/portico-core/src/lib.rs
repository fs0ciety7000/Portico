//! # portico-core
//!
//! The UI-agnostic business engine of Portico. Both the TUI and the GUI are thin
//! clients of this crate: they send commands to an [`Engine`] and render the
//! [`EngineEvent`]s it emits.
//!
//! Layering (lower modules never depend on higher ones):
//!
//! ```text
//! engine ─┬─ session ─┬─ transport (russh)   ← Phase 1b
//!         │           ├─ sftp                ← Phase 2
//!         │           └─ forward             ← Phase 2
//!         ├─ keys / secrets / snippets
//!         └─ hosts ── ssh_config, search, store
//! ```
//!
//! Hard rules (see `CLAUDE.md`): no UI dependency, no `unwrap`/`expect`/`panic!`
//! outside tests, every fallible API returns [`Result`], secrets are wrapped in
//! [`secrecy`] types and never logged.

mod agent;
pub mod agent_socket;
mod copy_id;
pub mod effective;
pub mod engine;
pub mod error;
pub mod events;
pub mod forward;
pub mod hardening;
pub mod hosts;
pub mod ids;
pub mod import;
pub mod keys;
pub mod known_hosts;
pub mod logs;
pub mod managed_hosts;
pub mod recent;
pub mod recording;
pub mod search;
pub mod secrets;
pub mod session;
pub mod settings;
pub mod sftp;
pub mod snippets;
mod socks;
pub mod ssh_config;
pub mod store;
pub mod update;
pub mod vault;
#[cfg(windows)]
mod winfs;

pub use agent::{AgentCertificate, AgentKey};
pub use copy_id::CopyIdOutcome;
pub use engine::{Engine, EngineConfig};
pub use error::{Error, ErrorSummary, Result};
pub use events::EngineEvent;
