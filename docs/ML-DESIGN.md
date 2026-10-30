# ML-DESIGN.md — Purpose-Built AI/ML Models for the Real-Time Rust Pre-Compiler

> Analysis of every crate's hot paths and failure modes, mapped to concrete,
> trainable ML models that improve **efficiency, effectiveness, stability,
> speed, and performance**. Each entry names the exact file/line it plugs into,
> the model family, the data source the project already generates, the
> integration mechanism, and the expected gain.

## How the project works today (ground truth)

The daemon wraps rust-analyzer's `ra_ap_*` pipeline behind the
`IncrementalEngine` trait and exposes it over line-based JSON to four bindings
(CLI / API / MCP / ACP).

- **The bottleneck.** `crates/rpc-core/src/salsa_engine/mod.rs:123-131` —
  `RaApEngine::query(Diagnose(path))` calls `Analysis::from_single_file`
  (rebuilds parser + HIR + crate graph) on **every** query. The module doc
  (`mod.rs:9-14`) and `docs/ENGINE.md:107-111` both admit this is "correct but
  far above 100 ms" — the documented **F2** perf problem.
- **No suggestions.** `map_diagnostic` (`mod.rs:53-72`) emits
  `suggestions: Vec::new()` — diagnostics are reported but never actionable.
- **No cache.** `crates/rpc-cache/src/lib.rs` is an empty stub (only `VERSION`).
- **Flat priority, no scheduler.** `Query.priority` is a `u8`
  (`crates/rpc-core/src/types.rs:52-53`); `DaemonService::query` (`service.rs:168-176`)
  runs every query synchronously to completion. `timeout_ms` is hardcoded to
  `100` (`types.rs:69`).
- **Manual sandbox env.** `rpc-sandbox` `Policy.allowed_env` is hand-curated
  (`rpc-sandbox/src/lib.rs:26-44`).
- **Two codegen backends, no selection.** `rpc-codegen` supports rustc
  (default) and Cranelift (fast debug) but the choice is manual.
- **Signals the system already logs.** Per-query timing (`mod.rs:211-223`
  emits `RAAP_QUERY_MS`), revision deltas, diagnostic codes/severity/messages,
  VFS content hashes, subprocess exit codes/timeouts.

These gaps are exactly where purpose-built models earn their keep.

---

## Tier 1 — Direct perf & stability (highest ROI)

### M1. Change-Impact Scope Predictor (attacks the F2 bottleneck)
- **Plugs into:** `salsa_engine/mod.rs:123-131` (`query`) + `apply_changes` (`:100-109`).
- **Problem:** the engine rebuilds the *whole* ra_ap world per query even when
  one line changed.
- **Model:** a learned change-impact model. Input features: changed file paths,
  revision delta size, per-file byte-diff, dependency-graph edges touched,
  diagnostic density of each file. Output: the set of files/diagnostics that
  are actually invalidated, so the engine re-derives only those and serves the
  rest from cache. A small GNN over the crate graph or a transformer over the
  file-diff sequence.
- **Data source (already generated):** during validation the system computes
  both the full-rebuild diagnostic set and the incremental subset — that
  `(ChangePack → invalidated set)` pairing is the training label, free.
- **Integration:** in-process ONNX (`ort`/`tract`) for the tiny model; a
  `ScopePredictor` consulted before `query`.
- **Gain:** the direct path to the sub-100 ms goal — potentially 10–50× fewer
  full rebuilds.

### M2. Query Latency Predictor / Dynamic Timeout Tuner
- **Plugs into:** `types.rs:54-56` (`timeout_ms`) + `service.rs:168-176` (`query`).
- **Problem:** a fixed `100 ms` budget causes wasted timeouts (work discarded,
  retried) or wasted patience (queries that would finish in 20 ms are killed at
  100 ms).
- **Model:** gradient-boosted regression (or small MLP) predicting per-query
  latency from (file byte length, revision delta, diagnostic density, file
  change history, backend).
- **Data source:** the existing `RAAP_QUERY_MS` timing hook (`mod.rs:211-223`)
  + query features — a self-labeling feedback loop.
- **Integration:** in-process ONNX; `query` sets a dynamic `timeout_ms` and
  pre-empts queries predicted to exceed the budget.
- **Gain:** fewer timeouts, higher throughput, no daemon stalls.

### M3. Prefetch / Cache-Warm Predictor
- **Plugs into:** `rpc-cache` (currently a stub) + `service.rs:168-176`.
- **Problem:** diagnostics are computed on-demand only; the agent waits for each
  cold read.
- **Model:** next-item prediction over the sequence of `(path, revision)`
  queries (GRU or a 2-layer transformer). Predicts the top-N files likely
  queried next; a background thread pre-computes their diagnostics into the
  cache.
- **Data source:** query history (the daemon already assigns `QueryId`s and
  stores results in `service.rs:129`).
- **Integration:** background task + cache lookup in `query`.
- **Gain:** perceived latency collapses — many reads become cache hits.

---

## Tier 2 — Agent effectiveness

### M4. Diagnostic Relevance Ranker
- **Plugs into:** `mod.rs:46-49` (the `.filter()` before `map_diagnostic`).
- **Problem:** ra_ap emits many diagnostics; irrelevant noise wastes agent
  context and reasoning time.
- **Model:** distilled text classifier (DistilBERT / 6-layer transformer)
  scoring each diagnostic's relevance to the current task + file context.
- **Data source:** severity × actionability × whether an agent later fixed it
  (learnable from fix-order / git history).
- **Integration:** in-process ONNX; only the top-k reach the wire.
- **Gain:** smaller agent context, faster agent decisions (effective speed).

### M5. Codefix / Suggestion Generator
- **Plugs into:** `mod.rs:69-70` (`suggestions: Vec::new()`).
- **Problem:** diagnostics are reported but never actionable.
- **Model:** a fine-tuned code-edit model (T5-small or CodeGen-350M) mapping
  `(diagnostic code, message, source around the span)` → a `replacement`
  string.
- **Data source:** rustc diagnostic → gold-fix pairs (publicly available),
  augmented with the project's own diagnostics.
- **Integration:** sidecar `llama-server` (for a larger model) or in-process
  ONNX (T5-small). Fills `Suggestion { description, span, replacement }`.
- **Gain:** agents get machine-actionable fixes immediately.

### M6. Diagnostic Code Classifier / Router
- **Plugs into:** `mod.rs:54-60` (`map_diagnostic` severity match).
- **Problem:** diagnostic codes (`E0xxx`) span syntax/type/borrow/lifetime/dep;
  each category benefits from a different fixer.
- **Model:** lightweight classifier (logistic regression / small NN) mapping
  `(code, message, span, context)` → category + recommended fix strategy.
- **Data source:** rustc diagnostic taxonomy + the project's own codes.
- **Integration:** attaches a `category` + `fix_strategy` to each wire
  `Diagnostic`.
- **Gain:** routes diagnostics to the right fixer (M5) and ranker (M4).

### M7. Diagnostic Message Rewriter
- **Plugs into:** `mod.rs:68` (`message: d.message`).
- **Problem:** ra_ap/rustc messages are terse; agents (and LLMs) read faster
  with clearer phrasing.
- **Model:** small seq2seq (T5-small) fine-tuned to rewrite terse compiler
  messages into clearer, context-aware explanations.
- **Integration:** in-process ONNX; rewrites `message` before emit.
- **Gain:** faster agent comprehension.

---

## Tier 3 — Stability & security

### M8. Subprocess Backend Selector
- **Plugs into:** `rpc-codegen/src/lib.rs:99-152` (`SubprocessDriver::run`).
- **Problem:** rustc (default) vs Cranelift (fast debug) is chosen manually.
- **Model:** classifier predicting the right backend from (opt level, dev vs
  release, file size, dependency count, whether it's a check vs a build).
- **Data source:** past `(request features → backend → latency/success)`.
- **Gain:** faster debug builds, fewer unexpectedly slow builds.

### M9. Sandbox Policy Learner
- **Plugs into:** `rpc-sandbox/src/lib.rs:56-69` (`curated_env`).
- **Problem:** the env allow-list is hand-curated; too strict breaks builds,
  too loose leaks credentials.
- **Model:** per-program-family classifier of "which env var did the subprocess
  actually read AND did the run succeed" → learns the minimal safe allow-list.
- **Data source:** observed subprocess runs (env read + exit code).
- **Gain:** strengthens the credential-leak guard (see
  `local-first-privacy-layer` skill) without breaking builds.

### M10. Stability / Degradation Guard
- **Plugs into:** `service.rs:192-194` (`health`) + `DaemonStatus` (`types.rs:172-181`).
- **Problem:** rustc ICEs, daemon stalls, and subprocess timeouts need early
  detection.
- **Model:** sliding-window anomaly classifier over `(latency, exit codes,
  timeout rate, query mix)` → a degradation score. `health` reports a new
  `Degraded` status so an agent can take evasive action (retry a different
  backend, cancel, restart the subprocess).
- **Gain:** proactive stability, not reactive.

---

## Tier 4 — Meta

### M11. Query-Scheduler Bandit
- **Plugs into:** `service.rs:168-176` + `Daemon.queued` (`service.rs:131`).
- **Problem:** `priority` is a flat `u8`; there is no real scheduler.
- **Model:** contextual multi-armed bandit (LinUCB / small NN) that learns to
  order, defer, or reject queued queries by (priority, predicted latency from
  M2, agent value).
- **Gain:** higher-value queries served first; the queue becomes intelligent.

---

## Integration strategy (how models run)

Two mechanisms, chosen per model:

1. **In-process ONNX** (`ort` = libonnxruntime, or `tract` = pure Rust) for the
   small models (M1, M2, M3, M4, M6, M7, M8, M9, M10). Tiny models run at
   single-digit ms on CPU — no sidecar, no IPC, no daemon stall. This is the
   right call for latency-critical paths.
2. **Sidecar `llama-server`** (via the existing `rpc-codegen` restricted-process
   path, or a new MCP tool) for the larger generative model (M5 codefix). The
   daemon already has the subprocess + watchdog machinery to manage it.

## Host constraints (this machine)

- Windows 10, AMD RX 7900 XTX (gfx1100), WDDM 2.7. WSL2 ROCm **hangs** on
  Win10 — training must be **Windows-native PyTorch** (`torch 2.11.0+rocm7.13.0`,
  venv `.venv-rocm`). Small ONNX models train fine on CPU too.
- Inference: ONNX Runtime CPU is fine for the tiny models; `llama.cpp` (GGUF,
  ROCm/CUDA) for the codefix LLM.
- Privacy: none of these models ever see secrets — they operate on compiler
  diagnostics and query metadata, and the sandbox (M9) keeps ambient creds out.

## Recommended build order

1. **M1 + M3 + rpc-cache** — the perf trio that makes the daemon actually
   fast (attacks F2, warms the cache). Highest ROI.
2. **M2** — dynamic timeouts; cheap, immediate stability win.
3. **M6 + M5** — make diagnostics actionable (the agent's whole value prop).
4. **M4 + M7** — reduce noise, improve clarity.
5. **M8 + M9 + M10** — stability & security hardening.
6. **M11** — the scheduler bandit, once enough query data exists.

Each model is **self-labeling**: the system already logs the signals (timing,
diagnostics, exit codes) needed to train the next model. Start with M1/M3/M2 and
the data pipeline pays for the rest.
