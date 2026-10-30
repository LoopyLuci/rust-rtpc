//! The incremental-engine trait + `MockEngine` reference implementation.
//!
//! This crate defines the single boundary between the agent-facing capabilities
//! and the underlying memoization engine: the `IncrementalEngine` trait. Two
//! concrete engines ship today:
//!
//!   - `MockEngine` — a deterministic, synchronous reference engine for tests.
//!   - `SalsaEngine` (opt-in) — a real Salsa 0.28 engine with persistence.
//!
//! `apply_changes` (mutating), `query` (reading), and `cancel` are the only
//! operations. The values passed to and returned from these methods live in
//! [`crate::types`]: [`Query`], [`QueryResult`], [`QueryId`]. This keeps the
//! trait transport-agnostic while letting the wire bindings (daemon, cli,
//! api, mcp, acp) share one protocol shape.
//!
//! The engine methods take `&self`: a daemon owns its engine behind an
//! `Arc<dyn IncrementalEngine>` and all clients share that same `&Daemon`,
//! so mutable state lives behind a `Mutex` inside the reference engine.

#![cfg_attr(not(test), warn(missing_docs))]

use std::path::PathBuf;
use std::sync::{
    Mutex,
    atomic::{AtomicU64, Ordering},
};

use crate::Diagnostic;
use crate::types::{Query, QueryKind, QueryResult};

/// A monotonic revision number: one greater than the previous after a change
/// pack is applied.
pub type Revision = u64;

/// A single file-content update written to the engine by a client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    /// The virtual path of the file within the compiled workspace.
    pub path: PathBuf,
    /// The new content of the file (atomically replaced in one revision).
    pub content: String,
}

impl Change {
    /// Create a new file update from a path and a `String` content.
    pub fn new(path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        Self { path: path.into(), content: content.into() }
    }
}

/// A set of changes applied atomically (transactional within the pack). The
/// rules guaranteed by the engine: within a pack, the last write to a path wins
/// for query evaluation, and a failed pack does not change the revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangePack {
    /// The file changes in the pack. The last write to a path wins for query
    /// evaluation.
    pub changes: Vec<Change>,
}

impl ChangePack {
    /// Create a new change pack from a sequence of file updates.
    pub fn new(changes: Vec<Change>) -> Self {
        Self { changes }
    }

    /// Whether the pack contains any changes (a no-op pack).
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// A change annotation produced during query evaluation so a client sees
/// *why* a result is stale. Real engines (salsa) may override with richer
/// payloads in later phases.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Annotations {
    /// The file path (if known) that triggered invalidation.
    pub path: Option<PathBuf>,
}

/// The incremental engine: the single boundary between agent-facing
/// capabilities and the underlying memoization engine.
///
/// Implementations must be `Send` + `Sync` so clients may call them
/// concurrently. The trait methods all take `&self` (interior mutability is
/// the engine's responsibility); a daemon holds an `Arc<dyn IncrementalEngine>`
/// and every wire binding shares the same `&Daemon`.
pub trait IncrementalEngine: Send + Sync {
    /// Apply a change pack, invalidating any cache affected by the changed
    /// files and bumping the revision. Returns `Err` on a partially invalid
    /// pack; otherwise `Ok(new_revision)`.
    fn apply_changes(&self, pack: ChangePack) -> Result<Revision, crate::engine::ApplyError>;

    /// The current revision — the number of packs applied so far.
    fn revision(&self) -> Revision;

    /// Request cancellation of in-flight work. Best-effort: engines that track
    /// work may honor tokens; engines that do not simply no-op.
    fn cancel(&self) {
        // Default: no-op. Engines that track work may override.
    }

    /// The human-readable engine version (e.g. "mock" / "salsa-0.28.5").
    fn engine_version(&self) -> &str;

    /// Evaluate a read-only query on the current revision and return its
    /// derived result. Deterministic: identical input produces identical
    /// output. The engine must not observe or require the transport.
    fn query(&self, q: Query) -> QueryResult;
}

/// Error from `apply_changes`.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// The pack violated a deterministic input invariant (a path escapes the
    /// workspace, or a duplicate write to two different contents).
    #[error("invalid pack: {0}")]
    InvalidPack(String),
    /// An internal engine error.
    #[error("engine error: {0}")]
    Engine(#[from] std::io::Error),
}

/// A deterministic, non-incremental in-memory engine. Useful for tests and as
/// the reference truth against which a salsa engine is compared. Stores its
/// derived state behind a `Mutex` so the reference engine can be shared as
/// `Arc<dyn IncrementalEngine>` and called immutably.
#[derive(Debug)]
pub struct MockEngine {
    revision: AtomicU64,
    contents: Mutex<std::collections::HashMap<PathBuf, String>>,
    diagnostics: Mutex<std::collections::HashMap<PathBuf, Vec<Diagnostic>>>,
}

impl MockEngine {
    /// Creates an empty mock engine at revision 0.
    pub fn new() -> Self {
        Self {
            revision: AtomicU64::new(0),
            contents: Mutex::new(std::collections::HashMap::new()),
            diagnostics: Mutex::new(std::collections::HashMap::new()),
        }
    }
}

impl Default for MockEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalEngine for MockEngine {
    fn apply_changes(&self, pack: ChangePack) -> Result<Revision, crate::engine::ApplyError> {
        if pack.is_empty() {
            return Ok(self.revision());
        }
        // Non-incremental: the whole workspace is re-derived on every pack.
        // Deterministic, and baseline the salsa engine must match.
        let mut contents = self.contents.lock().unwrap();
        let mut diagnostics = self.diagnostics.lock().unwrap();
        contents.clear();
        diagnostics.clear();
        for c in &pack.changes {
            let p = c.path.clone();
            contents.insert(p.clone(), c.content.clone());
            let mut diags = Vec::new();
            for (i, line) in c.content.lines().enumerate() {
                if !line.contains('=') || line.contains(['{', '}', '[', ']']) {
                    diags.push(Diagnostic {
                        schema_version: crate::SCHEMA_VERSION,
                        severity: crate::Severity::Warning,
                        code: String::new(),
                        message: format!("reference parse note on line {}", i + 1),
                        spans: vec![crate::TextSpan::new(p.display().to_string(), 0, 0)],
                        suggestions: vec![],
                    });
                }
            }
            diagnostics.insert(p, diags);
        }
        self.revision.fetch_add(1, Ordering::Relaxed);
        Ok(self.revision.load(Ordering::Relaxed))
    }

    fn revision(&self) -> Revision {
        self.revision.load(Ordering::Relaxed)
    }

    fn cancel(&self) {
        // Reference engine has no in-flight work to cancel.
    }

    fn engine_version(&self) -> &str {
        "mock"
    }

    fn query(&self, q: Query) -> QueryResult {
        match q.kind {
            QueryKind::Diagnose { path } => {
                let diagnostics =
                    self.diagnostics.lock().unwrap().get(&path).cloned().unwrap_or_default();
                QueryResult::Diagnoses { path, diagnostics, revision: self.revision() }
            }
            QueryKind::ParseSummary { path } => match self.contents.lock().unwrap().get(&path) {
                Some(content) => QueryResult::Summary {
                    path,
                    line_count: content.lines().count() as u64,
                    byte_len: content.len() as u64,
                    revision: self.revision(),
                },
                None => QueryResult::NotFound,
            },
            QueryKind::EngineInfo => QueryResult::Engine {
                version: self.engine_version().to_string(),
                revision: self.revision(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_engine_smoke() {
        let eng = MockEngine::new();
        assert_eq!(eng.revision(), 0);

        let pack = ChangePack::new(vec![Change::new("src/main.rs", "fn main() {}\n")]);
        let rev = eng.apply_changes(pack).unwrap();
        assert_eq!(rev, 1);
        assert_eq!(eng.revision(), 1);

        // A second pack bumps revision again.
        let pack2 = ChangePack::new(vec![Change::new("src/main.rs", "fn main() { let hi = 1; }")]);
        assert_eq!(eng.apply_changes(pack2).unwrap(), 2);

        // Query evaluates on the current revision.
        let q = Query::diagnose("src/main.rs");
        let res = eng.query(q);
        match res {
            QueryResult::Diagnoses { diagnostics, revision, .. } => {
                assert_eq!(revision, 2);
                assert!(!diagnostics.is_empty());
            }
            _ => panic!("expected Diagnoses, got: {:?}", res),
        }

        // Query the parse-summary for a file the engine actually holds.
        let summary = eng.query(Query::parse_summary("src/main.rs"));
        assert!(matches!(summary, QueryResult::Summary { .. }));
    }

    #[test]
    fn empty_pack_does_not_bump() {
        let eng = MockEngine::new();
        let rev = eng.apply_changes(ChangePack::new(vec![]));
        assert!(rev.unwrap() == 0);
    }
}
