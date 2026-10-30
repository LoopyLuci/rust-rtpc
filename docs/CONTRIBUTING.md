# Contributing to RTPC

This guide covers the development workflow, verification gates, and the
checklist for adding new crates or features.

## Prerequisites

- **Rust 1.98 stable** (see `rust-toolchain.toml`; components rustfmt + clippy).
- **X: drive enforcement** — all paths, env vars, builds and toolchains live on
  `X:`. `.cargo/config.toml` sets `CARGO_TARGET_DIR = X:/cargo-target`.
  `C:` stays at 90% full and is never touched.
- **git** with your user configured (`git config user.name` / `user.email`).

## The F1 gate (run before claiming anything done)

```
1. cargo build --workspace --all-targets   # exit 0
2. cargo fmt --all -- --check               # FMT_OK (nothing to format)
3. cargo clippy --workspace --all-targets -- -D warnings   # 0 errors
4. cargo test --workspace                   # all crates green
```

Stop at the first failure.

## Development workflow

1. Create a feature branch: `git checkout -b feat/<name>`.
2. Make your changes in the relevant crate(s) under `crates/`.
3. Run the F1 gate locally (all four commands above).
4. If adding a new crate, update the workspace `members` list and the
   `docs/CONTRIBUTING.md`-style module-add checklist below.
5. Re-run the F1 gate, then commit.

## Adding a new crate

- Add it to the `members` list in the root `Cargo.toml`.
- Add `[dependencies.<name>]` and the feature entry (if gated) in `[features]`.
- Document the crate in `docs/ARCHITECTURE.md` and `README.md`.
- The crate must pass the F1 gate on its own (`cargo build -p <crate>`,
  `cargo clippy -p <crate> --all-targets -- -D warnings`, `cargo test -p <crate>`).

## Module / crate wiring checklist

- **Workspace manifest** — explicit `members` list required; `[workspace.package]`
  for shared metadata (`version`, `license`, `edition`, `repository`, `rust-version`).
- **serde** — pinned to `default-features = false, features = ["derive", "std"]`
  in the workspace (not the default `derive` + `std`).
- **Recursive `Deserialize`** — types embedding `Vec<T>` (like `Change`) must NOT
  derive `Deserialize`; parse manually.
- **Cloning** — `ServiceError` and `ApplyError` must **NOT** implement `Clone`
  (they contain `io::Error`).
- **`QueryId`** — implements `Hash` (used as a map key / in `Query`).
- **Line-based JSON** — newline after every request and response; EOF returns
  `Ok(())`; the daemon's `serve()` loops `listener.incoming()`.
- **Sleeper strategy** — Windows `ping -n 3`, Unix `sleep 10`; GNU `timeout`
  breaks under git-bash.

## Clippy and format

- `-D warnings` is applied; clippy warnings are errors.
- `rustfmt.toml` is narrow to stable-only options (no nightly-only options).
- Format: `cargo fmt --all -- --check` must print nothing.

## Tests

- `cargo test --workspace` runs the whole crate suite, including `rpc-cli`
  self-test and `rpc-api` doctest.
- Engine-specific tests live in `#[cfg(test)] mod tests` inside each crate and
  in `crates/rpc-core/src/salsa_engine/mod.rs` (the ra_ap backend tests run
  behind the `salsa` feature).

## Adding a binding (MCP / ACP / HTTP)

The pattern for every new binding is:

1. Implement `DaemonService` (or wrap a `Daemon`).
2. Pick the wire format (MCP JSON-RPC 2.0, ACP proposals, HTTP, etc.).
3. Run the F1 gate. The binding is done when it is reachable and verified.

## Code style

- `rustfmt` (stable-only) for formatting.
- `clippy --workspace --all-targets -- -D warnings` for linting (0 errors).
- `unused_crate_dependencies` and `missing_docs` are warnings (they fire on
  non-test builds); keep doc comments on public items.

## License

`MIT OR Apache-2.0` (see root `Cargo.toml`).
