# Architecture

Rust Real-Time Pre-Compiler (RTPC) is a **persistent, query-driven
compilation daemon** with an integrated API, CLI, MCP and ACP. The defining
property is that every client capability is expressed as a single
`DaemonService` trait, and every binding (CLI, API, MCP, ACP) reduces to that
same trait. No binding touches engine state directly.

## High-level shape

```
        ┌──────────────────────────────────────────────────────────────┐
        │                      AGENT / BINDING                          │
        │  RPC-CLI  │  RPC-API  │  RPC-MCP  │  RPC-ACP  │  raw TCP     │
        └───────────────┬──────────────────────────┬───────────────────┘
                        │                          │
                        ▼                          ▼
        ┌──────────────────────────────────────────────────────────────┐
        │                      RPC-DAEMON                             │
        │  ┌──────────────────────┐    ┌──────────────────────┐       │
        │  │  DaemonService (trait) │    │  transport (line   │       │
        │  │  Daemon (owns engine) │    │  JSON, one per line)│      │
        │  │  ToolRegistry        │    └──────────────────────┘       │
        │  └──────────┬───────────┘                                  │
        │             │ Arc<dyn IncrementalEngine>                    │
        │        ┌────┴────────┐                                     │
        │        │  MockEngine  │  (or  SalsaEngine  w/ --features  │
        │        └───────────────┘       salsa → ra_ap_ide)         │
        └──────────────────────────────────────────────────────────────┘
```

## Component responsibilities

| Crate | Responsibility |
|---|---|
| `rpc-core` | `IncrementalEngine` trait (`apply_changes`, `query`, `cancel`, `revision`); `Change`/`ChangePack` wire types; the `Query`/`QueryResult` domain. |
| `rpc-vfs` | Transactional virtual file system: hashing, coalescing, revision bumps. |
| `rpc-diagnostics` | Diagnostic schema v1 (golden-file serialized, JSON). |
| `rpc-codegen` | Subprocess codegen driver + watchdog sleeper (Windows `ping -n 3`, Unix `sleep 10`). |
| `rpc-sandbox` | Restricted-process policy layer. |
| `rpc-cache` | Caching layer (stub). |
| `rpc-frontend` | Frontend surface (stub). |
| `rpc-daemon` | `DaemonService` trait; `Daemon` + `ToolRegistry`; `transport` (line-based JSON); `main.rs` entry points. |
| `rpc-cli` | CLI driver: `--daemon ADDR CMD`, `--stdio`, `--self-test`. |
| `rpc-api` | `RpcApi` ergonomic in-process handle over `DaemonService`. |
| `rpc-mcp` | MCP JSON-RPC 2.0 stdio server bridging `initialize`/`ping`/`tools/list`/`tools/call` → `DaemonService`. |
| `rpc-acp` | ACP adapter for `agent/instructions` / `agent/status` / `agent/response` / `agent/proposal`. |
| `fixtures/sample-workspace` | Taskr sample workspace + shared `STATE`. |

## Data flow

1. **Client applies a file.** The agent sends `apply { path, content }` over the
   line-based JSON transport. The daemon's `Daemon` builds a `ChangePack` and
   calls `IncrementalEngine::apply_changes(pack)`:
   - updates the tracked file set (via the VFS),
   - bumps the revision (`AtomicU64`),
   - returns the new `Revision`.
2. **Client queries.** The agent sends `query { kind: "diagnose", path }`. The
   daemon calls `IncrementalEngine::query(q)`:
   - `MockEngine` returns pre-computed diagnostics from its in-memory store.
   - `SalsaEngine` re-derives diagnostics by running `Analysis::from_single_file`
     + `Analysis::full_diagnostics` and maps them to `rpc_diagnostics::Diagnostic`.
3. **Client polls.** `poll { id }` returns the `QueryResult` retained for a
   running query.

## Engine trait

```rust
pub trait IncrementalEngine: Send + Sync {
    fn apply_changes(&self, pack: ChangePack) -> Result<Revision, ApplyError>;
    fn revision(&self) -> Revision;
    fn cancel(&self);          // default: no-op
    fn engine_version(&self) -> &str;
    fn query(&self, q: Query) -> QueryResult;
}
```

All methods take `&self`; mutable state is interior-mutated behind `Mutex` by
each concrete engine. A daemon holds `Arc<dyn IncrementalEngine>`, so every
binding shares the same engine instance.

## The daemon contract

`DaemonService` (in `rpc-daemon::service`) is the single agent-facing contract:

```
apply      (ChangePack) → Revision
query      (Query)      → QueryId
poll       (&QueryId)   → QueryResult
cancel     (&QueryId)   → Result<(), ServiceError>
health     ()           → DaemonHealth
capabilities()          → CapabilitySet
```

`ToolRegistry` further allows arbitrary tools (e.g. `engine_info`) to be wired
through the same trait — the CLI, API, MCP and ACP all expose tools uniformly.

## Daemon lifecycle

`rpc-daemon` is a **library** (not just a binary). `main.rs` provides two
entry points:

- `--one-shot CMD [args...]` — build a `Daemon` over a (mock or salsa) engine,
  run a single control command, print the result as JSON. Used by tests and
  build scripts.
- `--listen <addr>` — serve `DaemonService` over a TCP stream using the
  line-based transport. Loops `listener.incoming()` so multiple connections
  are each handled on their own thread. Prints `listener.local_addr()` after
  binding so clients know the real port (use `--listen 0` for a random port).

The daemon owns its engine behind `Arc<dyn IncrementalEngine>`; the bindings
(shared across the network) never touch the engine directly.
