//! The daemon's service: the single agent-facing contract.
//!
//! Every client (CLI, HTTP API, MCP, ACP) talks to the daemon through
//! [`DaemonService`]. The concrete engine is an `Arc<dyn IncrementalEngine>`
//! from `rpc-core`, so swapping the engine (e.g. mock → Salsa) changes nothing
//! in the contract.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use rpc_core::ChangePack;
use rpc_core::IncrementalEngine;
use rpc_core::types::{CapabilitySet, DaemonHealth, Query, QueryId, QueryResult, Revision};

/// The single error type returned by the daemon, and translated by each wire
/// binding into its own wire format.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// The daemon rejected a request for a deterministic reason.
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    /// The requested query is not known.
    #[error("unknown query id: {0}")]
    UnknownQuery(QueryId),
    /// The engine rejected a change pack.
    #[error("engine: {0}")]
    Engine(#[from] rpc_core::engine::ApplyError),
    /// An internal error.
    #[error("internal: {0}")]
    Internal(String),
}

/// A callable tool. Tools let an agent ask the daemon to do something beyond
/// passive compilation (e.g. inspect state, run a command in a sandbox).
pub trait Tool: Send + Sync {
    /// Stable, lower-case tool name, e.g. `"engine_info"`.
    fn name(&self) -> &str;
    /// Human-readable description. Deprecated in favour of agent prompts.
    fn description(&self) -> &str {
        ""
    }
    /// The argument schema. For a real tool this would be JSON-Schema-like;
    /// here the arguments are carried verbatim as a JSON value.
    fn args(&self) -> Vec<ToolArg> {
        vec![]
    }
    /// Execute the tool, returning `ToolResult` (serialisable to JSON).
    fn run(&self, daemon: &Daemon, args: serde_json::Value) -> ToolResult;
}

/// A tool's single argument.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolArg {
    /// The argument name, e.g. `"path"`.
    pub name: String,
    /// The JSON type of the argument (`string`, `array`, `object`, ...).
    pub kind: String,
    /// Human-readable description of the argument.
    pub description: String,
}

/// A tool's result: anything serialisable to JSON.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolResult {
    /// The tool name this result belongs to.
    pub name: String,
    /// Whether the tool ran successfully.
    pub ok: bool,
    /// The output of the tool (serialisable to JSON).
    pub output: serde_json::Value,
    /// An error message when `ok` is false.
    pub error: Option<String>,
}

/// Registry of named tools. Shared by the daemon and the tool-loading code.
#[derive(Default)]
pub struct ToolRegistry {
    inner: Vec<Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// Register a tool in the registry.
    pub fn insert(&mut self, tool: impl Tool + 'static) {
        self.inner.push(Arc::new(tool));
    }

    /// Return the tool with `name`, if any.
    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.inner.iter().find(|t| t.name() == name)
    }

    /// Return all registered tools.
    pub fn all(&self) -> Vec<Arc<dyn Tool>> {
        self.inner.clone()
    }
}

/// The incremental engine in use, surfaced through the daemon.
pub struct EngineHandle {
    engine: Arc<dyn IncrementalEngine>,
}

impl EngineHandle {
    /// Create a new handle into the daemon's engine for introspection.
    pub fn new(engine: Arc<dyn IncrementalEngine>) -> Self {
        Self { engine }
    }

    /// Return the crate version of this daemon, for client logging.
    pub fn engine_version(&self) -> String {
        env!("CARGO_PKG_VERSION").to_string()
    }

    /// Return the current engine revision for introspection in logs and
    /// client UIs.
    pub fn revision(&self) -> Revision {
        self.engine.revision()
    }
}

/// The daemon: owns the engine, the tools and the query store, and exposes the
/// `DaemonService` contract. A daemon is `Send` + `Sync` because its pieces
/// (`Arc`, `AtomicU` and `IncrementalEngine`) are.
pub struct Daemon {
    engine: Arc<dyn IncrementalEngine>,
    tools: ToolRegistry,
    queries: std::sync::Mutex<std::collections::HashMap<QueryId, QueryResult>>,
    next_id: AtomicU64,
    queued: AtomicU32,
}

impl Daemon {
    /// Create a daemon over `engine` with `tool_builders` installed.
    pub fn new<F>(engine: Arc<dyn IncrementalEngine>, mut builders: F) -> Self
    where
        F: FnMut(&mut ToolRegistry),
    {
        let mut registry = ToolRegistry::default();
        builders(&mut registry);
        Self {
            engine,
            tools: registry,
            queries: std::sync::Mutex::new(std::collections::HashMap::new()),
            next_id: AtomicU64::new(1),
            queued: AtomicU32::new(0),
        }
    }

    /// Return the engine handle for introspection.
    pub fn engine(&self) -> EngineHandle {
        EngineHandle { engine: Arc::clone(&self.engine) }
    }
}

impl DaemonService for Daemon {
    fn apply(&self, pack: ChangePack) -> Result<Revision, ServiceError> {
        if pack.is_empty() {
            return Ok(self.engine.revision());
        }
        let rev = self.engine.apply_changes(pack)?;
        // A successful revision sweep clears the query store.
        self.clear_queries();
        Ok(rev)
    }

    fn query(&self, q: Query) -> Result<QueryId, ServiceError> {
        // Enqueue: bump the queued counter (scheduler-relevant).
        self.queued.fetch_add(1, Ordering::Relaxed);
        let id = QueryId::new(self.next_id.fetch_add(1, Ordering::Relaxed).to_string());
        let result = self.engine.query(q);
        self.store(&id, result);
        self.queued.fetch_sub(1, Ordering::Relaxed);
        Ok(id)
    }

    fn poll(&self, qid: &QueryId) -> Result<QueryResult, ServiceError> {
        self.queries
            .lock()
            .unwrap()
            .get(qid)
            .cloned()
            .ok_or_else(|| ServiceError::UnknownQuery(qid.clone()))
    }

    fn cancel(&self, _qid: &QueryId) -> Result<(), ServiceError> {
        // Reference engine has no in-flight work to cancel.
        Ok(())
    }

    fn health(&self) -> DaemonHealth {
        DaemonHealth::running(Some(self.engine.revision()), Some(0))
    }

    fn capabilities(&self) -> CapabilitySet {
        CapabilitySet {
            protocol: "rpc-daemon/1".to_string(),
            engine: self.engine_version(),
            features: vec!["query".to_string(), "cancel".to_string()],
        }
    }
}

impl Daemon {
    /// The tool registry owned by this daemon.
    pub fn tools(&self) -> &ToolRegistry {
        &self.tools
    }

    fn store(&self, qid: &QueryId, result: QueryResult) {
        self.queries.lock().unwrap().insert(qid.clone(), result);
    }

    fn clear_queries(&self) {
        self.queries.lock().unwrap().clear();
    }

    fn engine_version(&self) -> String {
        self.engine.engine_version().to_string()
    }
}

/// A local in-process daemon. Alias for `Daemon`.
pub type LocalDaemon = Daemon;

/// The single service contract every client binds to. All methods are
/// synchronous for the reference and Salsa engines; the daemon's queue model
/// (dispatch → poll) is deliberately identical to the protocol so a client
/// never needs to understand the engine.
pub trait DaemonService: Send + Sync {
    /// Apply a change pack; returns the new revision.
    fn apply(&self, pack: ChangePack) -> Result<Revision, ServiceError>;
    /// Submit a query; returns its id immediately.
    fn query(&self, q: Query) -> Result<QueryId, ServiceError>;
    /// Fetch the derived result of a submitted query.
    fn poll(&self, qid: &QueryId) -> Result<QueryResult, ServiceError>;
    /// Cancel a running query.
    fn cancel(&self, qid: &QueryId) -> Result<(), ServiceError>;
    /// Runtime health report.
    fn health(&self) -> DaemonHealth;
    /// Capability advertisement.
    fn capabilities(&self) -> CapabilitySet;
}

impl ToolRegistry {
    /// Registers the built-in read-only `engine_info` tool. Agents use this to
    /// learn the daemon's version and revision.
    pub fn builtin_engine_info(&mut self) -> &mut Self {
        self.insert(EngineInfo);
        self
    }
}

/// Built-in tool: returns the engine version and revision. This is the hello
/// world of an agent talking to the daemon.
#[derive(Debug)]
pub struct EngineInfo;

impl Tool for EngineInfo {
    fn name(&self) -> &str {
        "engine_info"
    }

    fn description(&self) -> &str {
        "Return engine version and current revision."
    }

    fn run(&self, daemon: &Daemon, _args: serde_json::Value) -> ToolResult {
        let h = EngineHandle { engine: Arc::clone(&daemon.engine) };
        ToolResult {
            name: self.name().to_string(),
            ok: true,
            output: serde_json::json!({
                "engine": h.engine_version(),
                "revision": h.revision(),
            }),
            error: None,
        }
    }
}
