# Architecture Decision Record: daemon as a library

- **Status**: Accepted
- **Date**: 2026-10-30
- **Replaces**: (none — initial ADR)
- **Helpful for**: understanding why `rpc-daemon` is a library and not just a
  binary, and how the CLI/API/MCP/ACP bindings stay consistent.

## Context

RTPC is a persistent, query-driven compilation daemon exposing agent-facing
capabilities (apply, query, poll, cancel, health, capabilities, tools). Four
bindings (CLI, API, MCP, ACP) need to drive the daemon. The engine itself is a
separate concern (`IncrementalEngine`, `MockEngine`, `SalsaEngine`).

## Decision

`rpc-daemon` is a **library**, not just a binary:

- `main.rs` provides only two entry points (`--one-shot CMD [args...]` and
  `--listen <addr>`). It constructs a `Daemon` over a (mock or salsa) engine,
  then either runs a single control command or serves the line-based JSON
  transport.
- The library surface (`rpc_daemon::service`) exposes `DaemonService` (a trait),
  `Daemon`, `ToolRegistry`, `EngineHandle`, `LocalDaemon`.
- Every binding (CLI, API, MCP, ACP) wraps the `DaemonService` trait; none of
  them touch engine state directly.

## Consequences

- **Consistency** — all bindings speak the same contract, so adding a new
  binding (e.g. HTTP) only requires a new adapter, not a new engine.
- **Testability** — the daemon and its engine are unit-testable in-process.
  `RpcApi` is an ergonomic in-process handle for exactly this reason.
- **Lifecycle separation** — `main.rs` owns the OS-level entry points
  (TCP listen, one-shot command); the library owns the service logic.
- **No duplicated state** — the engine is owned by `Daemon`, shared behind
  `Arc<dyn IncrementalEngine>` across all bindings.

## Verification

- F1 gate: `cargo build --workspace --all-targets`, `cargo fmt --all -- --check`,
  `cargo clippy --workspace --all-targets -- -D warnings`, `cargo test --workspace`.
- The daemon is exercised end-to-end via `rpc-cli --daemon <addr>` over TCP.
