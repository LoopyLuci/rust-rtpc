//! Domain types of the agent-facing service protocol.
//!
//! This module pins the *contract* between the daemon and every agent-facing
//! client (CLI, HTTP API, MCP, ACP). All types are plain and serializable so a
//! client in any language can adopt the same schema.
//!
//! The wire surface is `apply`, `query`, `poll`, `cancel`, `health` and
//! `capabilities`; those map directly onto the methods of the
//! [`DaemonService`] trait (see the rpc-daemon crate).

use std::path::PathBuf;

use crate::Diagnostic;

use serde::{Deserialize, Serialize};

/// Monotonic revision counter for a compiled workspace.
pub type Revision = u64;

/// An identifier for a submitted query. Allocated monotonically by the daemon
/// and returned to clients for the `query` / `poll` / `cancel` choreography.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct QueryId {
    /// The opaque identifier string.
    pub id: String,
}

impl QueryId {
    /// Builds a new `QueryId` from any type that converts into a `String`.
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

impl std::fmt::Display for QueryId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.id)
    }
}

/// A query submitted to the daemon. The `kind` is the only typed field; the
/// rest of the payload is transport and policy (priority, timeout,
/// cancellation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    /// The query kind, flattened into this struct so the wire JSON is a single
    /// tagged object: `{"kind":"diagnose","path":"...","priority":0,...}`.
    /// Without flatten the `kind` tag would nest itself twice.
    #[serde(flatten)]
    pub kind: QueryKind,
    /// Schedulers and the daemon use this to order competing work.
    #[serde(default)]
    pub priority: u8,
    /// Best-effort request deadline in milliseconds.
    #[serde(default)]
    pub timeout_ms: u64,
    /// Opaque token supplied by a caller to make cancellation unique. The
    /// daemon may ignore tokens it does not understand.
    #[serde(default)]
    pub cancel_token: Option<String>,
}

impl Query {
    /// Builds a `Query` that returns the diagnostics for a virtual file.
    pub fn diagnose(path: impl Into<PathBuf>) -> Self {
        Self {
            kind: QueryKind::Diagnose { path: path.into() },
            priority: 0,
            timeout_ms: 100,
            cancel_token: None,
        }
    }

    /// Builds a `Query` that returns a light parse summary for a virtual file.
    pub fn parse_summary(path: impl Into<PathBuf>) -> Self {
        Self {
            kind: QueryKind::ParseSummary { path: path.into() },
            priority: 0,
            timeout_ms: 100,
            cancel_token: None,
        }
    }

    /// Builds a `Query` that reports engine metadata.
    pub fn engine_info() -> Self {
        Self { kind: QueryKind::EngineInfo, priority: 0, timeout_ms: 100, cancel_token: None }
    }

    /// Returns the scheduling priority of this query.
    pub fn priority(&self) -> u8 {
        self.priority
    }

    /// Returns the cancellation token of this query, if any.
    pub fn cancel_token(&self) -> &Option<String> {
        &self.cancel_token
    }
}

/// The query kind determines which derived result the daemon returns.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum QueryKind {
    /// Trace and return the diagnostics for the virtual file at `path`.
    Diagnose {
        /// Virtual file path within the compiled workspace.
        path: PathBuf,
    },
    /// Return a light parse summary for the virtual file at `path`.
    ParseSummary {
        /// Virtual file path within the compiled workspace.
        path: PathBuf,
    },
    /// Report static engine metadata (version, revision).
    EngineInfo,
}

/// The daemon's evaluation result for a query. This is the only read-facing
/// response shape, so a client need not understand the engine's internals.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryResult {
    /// Diagnostics for one virtual file, re-derived after a revision bump.
    Diagnoses {
        /// File that was queried.
        path: PathBuf,
        /// Diagnostics derived for `path`.
        diagnostics: Vec<Diagnostic>,
        /// Revision the diagnostics were derived from.
        revision: Revision,
    },
    /// A light parse summary for one virtual file.
    Summary {
        /// File that was queried.
        path: PathBuf,
        /// Number of lines in the file.
        line_count: u64,
        /// Byte length of the file.
        byte_len: u64,
        /// Revision the summary was derived from.
        revision: Revision,
    },
    /// Engine metadata: version and revision.
    Engine {
        /// Engine version string (e.g. "mock", "salsa-0.28.5").
        version: String,
        /// Current revision.
        revision: Revision,
    },
    /// The queried path could not be resolved.
    NotFound,
}

impl QueryResult {
    /// Counts diagnostics attached to this result, treating every severity
    /// except `Note` and `Help` as a problem. Useful for "did this query
    /// surface errors" checks in agents.
    pub fn severity_summary(&self) -> u32 {
        let mut n = 0u32;
        if let QueryResult::Diagnoses { diagnostics, .. } = self {
            for d in diagnostics {
                if d.severity == crate::Severity::Error || d.severity == crate::Severity::Warning {
                    n += 1;
                }
            }
        }
        n
    }
}

/// Daemon runtime status for `health`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DaemonStatus {
    /// Not yet accepting queries.
    Starting,
    /// Fully operational; serving queries.
    Running,
    /// Gracefully shutting down; no new work accepted.
    Stopping,
    /// Fatal error; the daemon must be restarted.
    Errored,
}

/// Runtime health report for `health`. A client uses this to decide whether to
/// keep talking to the daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonHealth {
    /// Current runtime status.
    pub status: DaemonStatus,
    /// Revision of the engine, if known.
    pub revision: Option<Revision>,
    /// Number of queries waiting to be scheduled.
    pub queued: Option<u32>,
    /// Seconds since the daemon started, rounded down.
    pub uptime_secs: u64,
}

impl DaemonHealth {
    /// Builds a `DaemonHealth` for a running daemon.
    pub fn running(revision: Option<Revision>, queued: Option<u32>) -> Self {
        Self { status: DaemonStatus::Running, revision, queued, uptime_secs: 0 }
    }
}

/// Capability advertisement for `capabilities`. A client can introspect the
/// protocol version and the features the daemon supports.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilitySet {
    /// Protocol version this daemon speaks.
    pub protocol: String,
    /// Engine variant in use (e.g. "mock", "salsa-0.28.5").
    pub engine: String,
    /// Feature flags the daemon supports beyond the protocol minimum.
    pub features: Vec<String>,
}
