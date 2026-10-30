//! rpc-core — the incremental engine trait + query types for the real-time
//! Rust pre-compiler.
//! This crate defines the boundary every agent-facing capability (CLI, API,
//! MCP, ACP) is built on top of:
//!
//!   - [`IncrementalEngine`] — the trait (engine implementations implement
//!     `apply_changes`, `query`, `cancel`, `revision`).
//!   - [`Change`]/[`ChangePack`] — the client-facing mutation surface.
//!   - [`types`] — the JSON-shaped protocol domain (`Query`, `QueryResult`,
//!     `QueryId`, health/capability types) shared by the daemon and its four
//!     bindings.
//!
//! Two engine implementations ship:
//!   - `MockEngine` — deterministic, synchronous reference engine (F2).
//!   - `SalsaEngine` — real Salsa 0.28 engine, opt-in (`salsa` feature).

#![cfg_attr(not(test), warn(missing_docs))]
#![cfg_attr(not(test), warn(rust_2018_idioms, unused_crate_dependencies))]

pub use crate::engine::IncrementalEngine;
pub use crate::engine::{Change, ChangePack, Revision};

pub use rpc_diagnostics::{
    self as diagnostic, Diagnostic, SCHEMA_VERSION, Severity, Suggestion, TextSpan,
};

pub mod engine;
pub mod types;

#[cfg(feature = "salsa")]
pub mod salsa_engine;
