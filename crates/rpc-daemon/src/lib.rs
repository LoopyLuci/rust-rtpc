//! Agentic daemon for the real-time Rust pre-compiler.
//!
//! This crate owns the query engine and exposes the single agent-facing
//! contract on which every client is built: the [`DaemonService`] trait. The
//! four protocol bindings — CLI, HTTP API, MCP, and ACP — all speak `DaemonService`
//! over a channel shaped like [`rpc_core::types`].
//!
//! The daemon owns:
//!   - the incremental engine (mock or Salsa),
//!   - the tool registry,
//!   - a tiny scheduler (priority + coalescing + cancellation),
//!   - durability (optional, D1-layer) if a persistent store is attached.
//!
//! ## Layout
//!
//! ```text
//! rpc-daemon
//! ├─ service.rs   DaemonService trait + Daemon + tool registry
//! ├─ lib.rs       library entry
//! └─ src/main.rs  binary: one-shot CLI or TCP server
//! ```
//!
//! ## Example: in-process daemon used by the protocol bindings
//!
//! ```rust,no_run
//! use rpc_daemon::service::{Daemon, DaemonService};
//! use rpc_daemon::service::EngineInfo as EngineInfoTool;
//! use rpc_core::engine::MockEngine;
//!
//! // `engine` is any `IncrementalEngine`, e.g. `MockEngine` or `SalsaEngine`.
//! let engine = std::sync::Arc::new(MockEngine::new());
//! let daemon = Daemon::new(engine, |reg| {
//!     reg.insert(EngineInfoTool);
//! });
//!
//! // Clients talk to the daemon through the trait, not the concrete type.
//! let health = daemon.health();
//! let cap = daemon.capabilities();
//! ```
//!
//! # Errors
//!
//! [`ServiceError`] is the only error type the daemon returns; all four
//! protocol bindings translate it to their own wire format.

#![cfg_attr(not(test), warn(missing_docs))]
#![cfg_attr(not(test), warn(rust_2018_idioms, unused_crate_dependencies))]

pub mod service;
pub mod transport;

pub use service::{Daemon, DaemonService, LocalDaemon, ServiceError, Tool, ToolRegistry};

/// Re-exports shared protocol types from `rpc-core`.
pub use protocol::{CapabilitySet, DaemonHealth, Query, QueryId, QueryResult, Revision};

/// Re-exports the reference engine for convenience.
pub use rpc_core::engine::MockEngine;

/// Re-export of the service protocol domain from `rpc-core`.
pub mod protocol {
    /// The JSON-shaped request/response types the four bindings share, one
    /// module per crate so `rpc-daemon` keeps a single source of truth.
    pub use rpc_core::types::{CapabilitySet, DaemonHealth, Query, QueryId, QueryResult, Revision};
}
