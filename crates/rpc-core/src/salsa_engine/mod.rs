//! `RaApEngine` — real-time Rust pre-compiler diagnostics backed by rust-analyzer's
//! `ra_ap_*` pipeline (`ra_ap_parser` → HIR → `ra_ap_ide_diagnostics`).
//!
//! This is the production-grade engine for the "sub-100ms diagnostics" goal: every
//! query flows through the true `ra_ap_ide` analysis world (`Analysis` +
//! `full_diagnostics`), not a mock. It implements the `IncrementalEngine` trait so
//! CLI/API/MCP/ACP bindings are unchanged.
//!
//! **Note on performance.** `Analysis::from_single_file` rebuilds the analysis
//! world (parser + HIR + diagnostics) on every `query`. This is *correct* — it
//! produces genuine ra_ap diagnostics — but *not fast* (typically far above
//! 100ms). The sub-100ms production engine (F2) reuses an incremental
//! `AnalysisHost` update instead of rebuilding per query; tracked as F2 work,
//! not a correctness defect.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{
    Arc as StdArc,
    atomic::{AtomicU64, Ordering},
};

use triomphe::Arc as TriompheArc;

use rpc_diagnostics::{Diagnostic, Severity as WireSeverity, TextSpan};

use crate::engine::{ApplyError, ChangePack, IncrementalEngine, Revision};
use crate::types::{Query, QueryKind, QueryResult};

use ra_ap_ide::Analysis;
use ra_ap_ide_db::{Severity as RaSeverity, assists::AssistResolveStrategy};
use ra_ap_ide_diagnostics::DiagnosticsConfig;
use ra_ap_paths::AbsPathBuf;

/// Produce real rust-analyzer diagnostics for a single source text.
///
/// `Analysis::from_single_file` builds a self-contained ra_ap world (parser +
/// HIR + crate graph with std) and returns a file id we can query.
fn diags_for_text(text: &str, file: &str) -> Vec<Diagnostic> {
    let proc_macro_cwd = TriompheArc::new(AbsPathBuf::try_from("X:/").unwrap());
    let (analysis, file_id) = Analysis::from_single_file(text.to_string(), proc_macro_cwd);
    let config = DiagnosticsConfig::test_sample();
    analysis
        .full_diagnostics(&config, AssistResolveStrategy::None, file_id)
        .unwrap_or_default()
        .into_iter()
        .filter(|d| d.severity != RaSeverity::Allow)
        .map(|d| map_diagnostic(d, file))
        .collect()
}

/// Translate a rust-analyzer `Diagnostic` into the wire `rpc_diagnostics::Diagnostic`.
fn map_diagnostic(d: ra_ap_ide_diagnostics::Diagnostic, file: &str) -> Diagnostic {
    let severity = match d.severity {
        RaSeverity::Error => WireSeverity::Error,
        RaSeverity::Warning | RaSeverity::WeakWarning => WireSeverity::Warning,
        // `Allow`-severity diagnostics are filtered before this function is called,
        // but the match must stay exhaustive.
        RaSeverity::Allow => WireSeverity::Warning,
    };
    // ra_ap spans are byte offsets inside the file text.
    let start = u32::from(d.range.range.start());
    let end = u32::from(d.range.range.end());
    Diagnostic {
        schema_version: crate::SCHEMA_VERSION,
        code: d.code.as_str().to_string(),
        severity,
        message: d.message,
        spans: vec![TextSpan::new(file.to_string(), start, end)],
        suggestions: Vec::new(),
    }
}

/// Production engine: real-time Rust pre-compiler diagnostics backed by `ra_ap`.
///
/// Owns a tracked file set behind a `Mutex` and a monotonic revision counter.
/// Diagnostics are re-derived per query through `Analysis::from_single_file`
/// (see the module docs on the performance caveat).
pub struct RaApEngine {
    files: std::sync::Mutex<HashMap<PathBuf, String>>,
    revision: StdArc<AtomicU64>,
}

impl RaApEngine {
    /// Create a fresh ra_ap engine (opt-in via the `salsa` feature).
    pub fn new() -> Self {
        Self {
            files: std::sync::Mutex::new(HashMap::new()),
            revision: StdArc::new(AtomicU64::new(0)),
        }
    }
}

impl Default for RaApEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl IncrementalEngine for RaApEngine {
    fn apply_changes(&self, pack: ChangePack) -> Result<Revision, ApplyError> {
        let mut files = self.files.lock().unwrap();
        files.clear();
        for change in pack.changes {
            files.insert(change.path, change.content);
        }
        self.revision.fetch_add(1, Ordering::Relaxed);
        Ok(self.revision.load(Ordering::Relaxed))
    }

    fn revision(&self) -> Revision {
        self.revision.load(Ordering::Relaxed)
    }

    fn engine_version(&self) -> &str {
        "ra_ap"
    }

    fn cancel(&self) {
        // Prototype engine does not track in-flight work.
    }

    fn query(&self, q: Query) -> QueryResult {
        match q.kind {
            QueryKind::Diagnose { path } => {
                let text = {
                    let files = self.files.lock().unwrap();
                    files.get(&path).cloned().unwrap_or_default()
                };
                let file = path.to_string_lossy().to_string();
                let diags = diags_for_text(&text, &file);
                QueryResult::Diagnoses { path, diagnostics: diags, revision: self.revision() }
            }
            QueryKind::ParseSummary { path } => {
                let text = {
                    let files = self.files.lock().unwrap();
                    files.get(&path).cloned().unwrap_or_default()
                };
                QueryResult::Summary {
                    path,
                    line_count: text.lines().count() as u64,
                    byte_len: text.len() as u64,
                    revision: self.revision(),
                }
            }
            QueryKind::EngineInfo => QueryResult::Engine {
                version: format!("ra_ap-{}", env!("CARGO_PKG_VERSION")),
                revision: self.revision(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Query, QueryKind, QueryResult};
    use std::path::PathBuf;

    fn fix(path: &str, content: &str) -> crate::engine::Change {
        crate::engine::Change::new(PathBuf::from(path), content)
    }

    fn diag_q(path: &str) -> Query {
        Query {
            kind: QueryKind::Diagnose { path: PathBuf::from(path) },
            priority: 0,
            timeout_ms: 100,
            cancel_token: None,
        }
    }

    /// End-to-end: feed a file with a syntax error, verify the engine returns
    /// real ra_ap diagnostics (not a mock).
    #[test]
    fn ra_ap_diagnostic_round_trip() {
        let engine = RaApEngine::new();
        engine.apply_changes(ChangePack::new(vec![fix("/src/main.rs", "fn =() {")])).unwrap();
        let result = engine.query(diag_q("/src/main.rs"));
        match result {
            QueryResult::Diagnoses { diagnostics, .. } => {
                assert!(
                    !diagnostics.is_empty(),
                    "expected real diagnostics for a broken file, got none",
                );
                for d in &diagnostics {
                    assert!(!d.message.is_empty(), "diagnostic message must not be empty");
                }
            }
            other => panic!("expected Diagnoses, got {other:?}"),
        }
    }

    /// A well-formed file produces no diagnostics.
    #[test]
    fn ra_ap_clean_file_has_no_diagnostics() {
        let engine = RaApEngine::new();
        engine.apply_changes(ChangePack::new(vec![fix("/src/main.rs", "fn main() {}")])).unwrap();
        let result = engine.query(diag_q("/src/main.rs"));
        match result {
            QueryResult::Diagnoses { diagnostics, .. } => {
                assert!(
                    diagnostics.is_empty(),
                    "clean file should produce no diagnostics, got: {diagnostics:?}",
                );
            }
            other => panic!("expected Diagnoses, got {other:?}"),
        }
    }

    /// Time a single diagnose query; report ms via stderr.
    #[test]
    fn ra_ap_query_timing() {
        let engine = RaApEngine::new();
        engine.apply_changes(ChangePack::new(vec![fix("/src/main.rs", "fn main() {}")])).unwrap();
        let start = std::time::Instant::now();
        engine.query(diag_q("/src/main.rs"));
        let elapsed = start.elapsed();
        eprintln!(
            "RAAP_QUERY_MS={} ({})",
            elapsed.as_millis(),
            if elapsed.as_millis() < 100 { "FAST" } else { "SLOW" }
        );
    }

    /// The salsa feature must actually be active when these tests run.
    #[test]
    fn salsa_feature_is_on() {
        assert!(cfg!(feature = "salsa"), "salsa feature not enabled for tests");
    }
}
