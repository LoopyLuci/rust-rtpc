# Rust Real-Time Pre-Compiler

> A persistent, query-driven Rust compilation daemon with an integrated API,
> CLI, MCP and ACP, exposing a single `DaemonService` contract.

Rust Real-Time Pre-Compiler (RTPC) is a production-grade, real-time Rust
pre-compiler: a **query-driven compilation daemon** that maintains a live
semantic model of the workspace, applies source changes through a
transactional virtual file system, and drives a fast semantic front-end for
diagnostics and incremental checking. Agents interact with it over a single,
transport-agnostic `DaemonService` trait — so the same server powers a CLI,
an HTTP API, an MCP server and an ACP (agent-control-protocol) adapter.

The design targets **sub-100 ms feedback cycles** by isolating `rustc` and the
codegen back-end in a subprocess with jump-table hot-patching, and by keeping
the daemon persistent and query-driven rather than spawning `rustc` on every
keystroke.

---

## Features

- **F1 verification gate** — every change must pass
  `build → fmt → clippy → test` before it is considered done.
- **Core domain + engine trait** — `rpc-core` defines `IncrementalEngine`
  (`apply_changes`, `query`, `cancel`, `revision`) plus the `Query` /
  `QueryResult` wire domain.
- **Production engine backend** — `SalsaEngine` wraps `ra_ap_ide` (the
  rust-analyzer analysis pipeline) behind the `IncrementalEngine` trait.
- **Daemon as a library** — `rpc-daemon` exposes `DaemonService` via `Daemon`
  and `ToolRegistry`; `main.rs` is only entry points (`--one-shot`, `--listen`).
- **Line-based JSON transport** — one request per line, one response per line;
  the wire that CLI / API / MCP / ACP all build on.
- **Bindings** — `rpc-cli` (TCP/stdio/self-test), `rpc-api` (in-process),
  `rpc-mcp` (MCP JSON-RPC 2.0), `rpc-acp` (ACP proposals).
- **Sub-100 ms target** — isolated `rustc` / Cranelift subprocess with jump-table
  hot-patching; `ra_ap_ide` diagnostics available behind the `salsa` feature.

---

## Quick start

```bash
# 1. Build the whole workspace (X: drive enforced; CARGO_TARGET_DIR is set)
cargo build --workspace --all-targets

# 2. Format (stable-only rustfmt)
cargo fmt --all -- --check        # must print nothing

# 3. Clippy (warnings are errors)
cargo clippy --workspace --all-targets -- -D warnings

# 4. All tests (including rpc-cli self-test and rpc-api doctest)
cargo test --workspace

# 5. Run the F1 gate end-to-end
cargo build --workspace --all-targets && \
cargo fmt --all -- --check && \
cargo clippy --workspace --all-targets -- -D warnings && \
cargo test --workspace
```

> **Environment** — `CARGO_TARGET_DIR` is `X:/cargo-target` (set in
> `.cargo/config.toml`). Rust `1.98` stable is required (see
> `rust-toolchain.toml`).

### Run the daemon

```bash
# Listen on a TCP port
cargo run -p rpc-daemon -- --listen 127.0.0.1:3457

# One-shot control command (benchmark / script)
cargo run -p rpc-daemon -- --one-shot apply src/main.rs 'fn main() {}'
```

### Talk to the daemon

```bash
# TCP (line-based JSON)
rpc-cli --daemon 127.0.0.1:3457 health
rpc-cli --daemon 127.0.0.1:3457 tools
rpc-cli --daemon 127.0.0.1:3457 apply --path src/main.rs --content 'fn main() {}' --arg-json ''
rpc-cli --daemon 127.0.0.1:3457 query --json '{"kind":"diagnose","path":"src/main.rs"}'
rpc-cli --daemon 127.0.0.1:3457 poll --id 1
```

---

## Project layout

```
Cargo.toml                 # workspace manifest (13 members, explicit members)
rust-toolchain.toml         # stable channel, rustfmt + clippy components
.rustfmt.toml                # stable-only formatting options
.cargo/config.toml          # CARGO_TARGET_DIR = X:/cargo-target
IDEA.txt                    # original project brief (design reference)

crates/
  rpc-core/                 # IncrementalEngine trait + query wire domain + salsa_engine (ra_ap backend)
  rpc-vfs/                  # transactional virtual file system (hashing, coalescing, revision)
  rpc-diagnostics/          # diagnostic schema v1 (golden-file serialized)
  rpc-codegen/              # subprocess codegen driver + watchdog sleeper
  rpc-sandbox/              # restricted-process policy layer
  rpc-cache/                # caching layer (stub)
  rpc-frontend/             # frontend surface (stub)
  rpc-daemon/               # DaemonService trait + Daemon + transport (line JSON) + main
  rpc-cli/                  # CLI driver (--daemon, --stdio, --self-test)
  rpc-api/                  # RpcApi ergonomic in-process handle
  rpc-mcp/                  # MCP JSON-RPC 2.0 stdio server
  rpc-acp/                  # ACP adapter (agent/instructions / status / response / proposal)
  fixtures/sample-workspace/ # taskr sample workspace + shared STATE
```

---

## Transport protocol

`rpc-daemon` speaks **line-based JSON**: one JSON request per line, one JSON
response per line. This is the single wire that all bindings share.

```json
// request
{"op":"apply","args":{"path":"src/main.rs","content":"fn main() {}"}}
{"op":"query","args":{"kind":"diagnose","path":"src/main.rs"}}
{"op":"poll","args":{"id":"1"}}
{"op":"cancel","args":{"id":"1"}}

// response
{"result":{"revision":1}}
{"result":{"id":"1"}}
{"result":{"path":"src/main.rs","diagnostics":[...],"revision":1}}
```

- `rpc-core::types::Query` is `#[serde(flatten)]` on `kind` + `#[serde(default)]`
  on `priority` / `timeout_ms` / `cancel_token`, so clean wire JSON is
  `{"kind":"diagnose","path":"src/main.rs"}`.
- `QueryId::new` returns a value (not `Result`); EOF returns `Ok(())`.
- `Change` / `ChangePack` deliberately **do not** derive `Deserialize` (they
  embed `Vec<Change>`), so transport parses them manually.

---

## Bindings

| Binding | Crate | Description |
|---|---|---|
| CLI | `rpc-cli` | TCP / stdio / self-test driver |
| API | `rpc-api` | Ergonomic in-process handle over `DaemonService` |
| MCP | `rpc-mcp` | MCP JSON-RPC 2.0 stdio server (tools/list / tools/call) |
| ACP | `rpc-acp` | ACP adapter (`agent/instructions` / `agent/status` / `agent/response` / `agent/proposal`) |

Every binding reduces to the `DaemonService` trait; none of them touch engine
state directly.

---

## Engine backends

`rpc-core` defines `IncrementalEngine`. Two implementations ship:

- **`MockEngine`** — deterministic in-memory reference engine (used by tests
  and the default one-shot daemon).
- **`SalsaEngine`** (`salsa` feature) — the production real-time engine. It
  wraps [`ra_ap_ide`] (rust-analyzer's analysis pipeline) behind
  `IncrementalEngine` so CLI / API / MCP / ACP bindings are unchanged. It owns an
  `AnalysisHost` (interior-mutated via `Mutex`) plus a tracked file set, applies
  `ChangePack` updates the set and bumps a revision, and re-derives diagnostics
  per query via `Analysis::from_single_file` + `Analysis::full_diagnostics`.

> **Performance note** — the prototype rebuilds the ra_ap world from text on
> every query (correct, but typically far above 100 ms). The sub-100 ms
> production engine (F2) reuses an incremental `AnalysisHost` update. This is
> tracked as F2 work, not a correctness defect.

### Feature flags (rpc-core)

| Feature | Description |
|---|---|
| `salsa` | Enables `SalsaEngine`, `ra_ap_ide`, `ra_ap_ide_db`, `ra_ap_ide_diagnostics`, `ra_ap_paths`, `triomphe` |

---

## Contributing

1. **Run the F1 gate before claiming anything done**:
   `cargo build --workspace --all-targets`
   `cargo fmt --all -- --check`
   `cargo clippy --workspace --all-targets -- -D warnings`
   `cargo test --workspace`
2. **Follow the existing conventions** — see `docs/CONTRIBUTING.md` for the
   full development workflow, module-add checklist and clippy/format rules.
3. **New crates go in `crates/`** and must be added to the workspace `members`
   list in the root `Cargo.toml`.
4. **A crate not exercised end-to-end is not done** — implement the real path,
   then verify with the gate.

### Module-add checklist

- Add to `members` in the root `Cargo.toml`.
- Add `[dependencies.foo]` (with `optional = true` where safe) and the feature
  entry in `[features]` if it should be gated.
- Re-run the F1 gate; clippy must report 0 errors.

---

## License

`MIT OR Apache-2.0` (see workspace `license` in `Cargo.toml`).
