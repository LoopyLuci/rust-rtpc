# Engine and `IncrementalEngine` trait

`rpc-core` defines `IncrementalEngine` — the single boundary between
agent-facing capabilities and the underlying memoization engine. Every
concrete engine implements this trait so that the CLI / API / MCP / ACP
bindings are unchanged.

## The trait

```rust
pub trait IncrementalEngine: Send + Sync {
    /// Apply a change pack, invalidating any cache affected by the changed
    /// files, and bump the revision. Returns the new revision.
    fn apply_changes(&self, pack: ChangePack) -> Result<Revision, ApplyError>;

    /// The current revision (number of packs applied so far).
    fn revision(&self) -> Revision;

    /// Request cancellation of in-flight work. Best-effort; engines that
    /// track no work simply no-op (default).
    fn cancel(&self) { /* default: no-op */ }

    /// Human-readable engine version (e.g. "mock", "ra_ap-0.1.0").
    fn engine_version(&self) -> &str;

    /// Evaluate a read-only query and return the derived result.
    fn query(&self, q: Query) -> QueryResult;
}
```

All methods take `&self`; mutable state lives behind a `Mutex` inside each
engine. A daemon owns its engine as `Arc<dyn IncrementalEngine>` and every
binding shares the same instance.

## Types

### `Change` and `ChangePack`

A `ChangePack` is applied atomically within a revision; the last write to a
path wins for query evaluation.

```rust
pub struct Change {
    pub path: PathBuf,
    pub content: String,
}

pub struct ChangePack {
    pub changes: Vec<Change>,
}

impl ChangePack {
    pub fn new(changes: Vec<Change>) -> Self;
    pub fn is_empty(&self) -> bool;
}
```

`Change`/`ChangePack` deliberately **do not derive `Deserialize`** (they embed
`Vec<Change>`), so the transport parses them manually rather than relying on
serde.

### `Query` / `QueryResult`

- `Query` has `#[serde(flatten)] pub kind: QueryKind` plus `#[serde(default)]`
  on `priority`, `timeout_ms`, `cancel_token`.
- `QueryResult` has `Diagnoses`, `Summary`, `Engine`, `NotFound`.

## `MockEngine`

The deterministic, in-memory reference engine used by tests. It stores files
and diagnostics behind a `Mutex` and returns them on `query`. It is the
reference truth that the Salsa engine is compared against.

## `SalsaEngine` (the `salsa` feature)

The production real-time engine. It wraps [`ra_ap_ide`] (rust-analyzer's
analysis pipeline) behind `IncrementalEngine`:

- owns a tracked file set (`Mutex<HashMap<PathBuf, String>>`) and a monotonic
  `revision` (`Arc<AtomicU64>`),
- `apply_changes` updates the file set and bumps the revision,
- `query(Diagnose(path))` re-derives diagnostics with
  `Analysis::from_single_file` + `Analysis::full_diagnostics`, mapping each
  `ra_ap_ide_diagnostics::Diagnostic` to `rpc_diagnostics::Diagnostic`,
- `engine_version` returns `"ra_ap"`.

```rust
use ra_ap_ide::Analysis;
use ra_ap_ide_db::{assists::AssistResolveStrategy, Severity as RaSeverity};
use ra_ap_ide_diagnostics::{Diagnostic as RaDiag, DiagnosticsConfig};
use ra_ap_paths::AbsPathBuf;

fn diags_for_text(text: &str) -> Vec<rpc_diagnostics::Diagnostic> {
    let proc_macro_cwd = triomphe::Arc::new(AbsPathBuf::try_from("X:/").unwrap());
    let (analysis, file_id) =
        Analysis::from_single_file(text.to_string(), proc_macro_cwd);
    let config = DiagnosticsConfig::test_sample();
    analysis
        .full_diagnostics(&config, AssistResolveStrategy::None, file_id)
        .unwrap_or_default()
        .into_iter()
        .map(|d: RaDiag| map_diagnostic(d))
        .collect()
}
```

> **Performance note** — the prototype rebuilds the ra_ap world (parser +
> HIR + diagnostics) on every `query`. It is *correct* but typically far above
> the 100 ms target. The sub-100 ms production engine (F2) reuses an
> incremental `AnalysisHost` update instead of rebuilding. Tracked as F2 work,
> not a correctness defect.

[`ra_ap_ide`]: https://crates.io/crates/ra_ap_ide
