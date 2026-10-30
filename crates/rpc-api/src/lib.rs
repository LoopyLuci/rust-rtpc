//! `rpc-api` — an ergonomic, in-process API binding over the daemon's
//! `DaemonService` contract. Every client (CLI, HTTP API, MCP, ACP) reduces to
//! the eight operations on that trait; `RpcApi` simply re-wraps it with a
//! synchronous, error-transformed surface that agents call directly.
//!
//! Because the daemon owns the engine as `Arc<dyn IncrementalEngine>`, nothing
//! about the wire (TCP, MCP, ACP) leaks into the API surface.

#![cfg_attr(not(test), warn(missing_docs))]
#![cfg_attr(not(test), warn(rust_2018_idioms, unused_crate_dependencies))]

use rpc_daemon::QueryId as RpcQueryId;
use rpc_daemon::service::{Daemon, DaemonService, ServiceError};

/// Ergonomic in-process handle over a daemon. Construct it from any
/// `Daemon` (mock or Salsa). All mutation is through `&self`; the daemon
/// remains the single owner of engine state.
///
/// # Example
///
/// ```
/// use rpc_daemon::service::Daemon;
/// use rpc_daemon::service::EngineInfo as EngineInfoTool;
/// use rpc_daemon::MockEngine;
/// use rpc_api::RpcApi;
/// use std::sync::Arc;
/// # fn main() -> Result<(), Box<dyn std::error::Error>> {
/// let engine = MockEngine::new();
/// let daemon = Daemon::new(Arc::new(engine), |reg| {
///     reg.insert(EngineInfoTool);
/// });
/// let api = RpcApi::new(daemon);
/// assert_eq!(api.health().status, rpc_core::types::DaemonStatus::Running);
/// let ids = api.query(rpc_core::types::Query::diagnose("src/main.rs"))?;
/// let result = api.poll(&ids)?;
/// # Ok(()) }
/// ```
pub struct RpcApi {
    daemon: Daemon,
}

impl RpcApi {
    /// Wrap an existing `Daemon`.
    pub fn new(daemon: Daemon) -> Self {
        Self { daemon }
    }

    /// Returns the daemon for callers that need the raw trait or `EngineHandle`.
    pub fn into_inner(self) -> Daemon {
        self.daemon
    }

    /// Apply a change pack atomically.
    pub fn apply(&self, pack: rpc_core::ChangePack) -> Result<rpc_core::Revision, ServiceError> {
        self.daemon.apply(pack)
    }

    /// Enqueue a query; returns the query id.
    pub fn query(&self, q: rpc_core::types::Query) -> Result<RpcQueryId, ServiceError> {
        self.daemon.query(q)
    }

    /// Fetch the result of a queued query.
    pub fn poll(&self, id: &RpcQueryId) -> Result<rpc_core::types::QueryResult, ServiceError> {
        self.daemon.poll(id)
    }

    /// Cancel a queued query.
    pub fn cancel(&self, id: &RpcQueryId) -> Result<(), ServiceError> {
        self.daemon.cancel(id)
    }

    /// Daemon health.
    pub fn health(&self) -> rpc_daemon::DaemonHealth {
        self.daemon.health()
    }

    /// Daemon capability set.
    pub fn capabilities(&self) -> rpc_daemon::CapabilitySet {
        self.daemon.capabilities()
    }

    /// The underlying engine handle.
    pub fn engine(&self) -> rpc_daemon::service::EngineHandle {
        self.daemon.engine()
    }

    /// The tool registry.
    pub fn tools(&self) -> &rpc_daemon::ToolRegistry {
        self.daemon.tools()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rpc_core::engine::MockEngine;
    use rpc_daemon::service::EngineInfo as EngineInfoTool;
    use std::sync::Arc;

    #[test]
    fn api_routes_to_daemon() {
        let daemon = Daemon::new(Arc::new(MockEngine::new()), |reg| {
            reg.insert(EngineInfoTool);
        });
        let api = RpcApi::new(daemon);

        let h = api.health();
        assert_eq!(h.status, rpc_core::types::DaemonStatus::Running);
        let cap = api.capabilities();
        assert_eq!(cap.protocol, "rpc-daemon/1");

        let pack =
            rpc_core::ChangePack::new(vec![rpc_core::Change::new("src/main.rs", "fn main() {}\n")]);
        let rev = api.apply(pack).expect("apply");
        assert_eq!(rev, 1);
    }
}
