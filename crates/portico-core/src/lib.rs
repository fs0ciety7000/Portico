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

pub mod engine;
pub mod error;
pub mod events;
pub mod forward;
pub mod hosts;
pub mod ids;
pub mod keys;
pub mod logs;
pub mod search;
pub mod secrets;
pub mod sftp;
pub mod snippets;
pub mod ssh_config;
pub mod store;

pub use engine::{Engine, EngineConfig};
pub use error::{Error, ErrorSummary, Result};
pub use events::EngineEvent;
