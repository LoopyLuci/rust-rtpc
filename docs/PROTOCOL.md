# Wire protocol (line-based JSON)

`rpc-daemon` transports every request and response as **one JSON object per
line**. This is the single wire the CLI, API, MCP and ACP bindings all build
on, so a new binding only needs to implement the `DaemonService` trait — the
wire layout is already handled by `transport::run`.

## Request

```
{"op":"<op-name>","args":{...}}
```

- `op` — the operation name (default `"health"` if missing).
- `args` — the operation arguments (default `{}` if missing).

Example:

```json
{"op":"apply","args":{"path":"src/main.rs","content":"fn main() { println!(); }"}}
{"op":"query","args":{"kind":"diagnose","path":"src/main.rs"}}
{"op":"poll","args":{"id":"12345"}}
{"op":"cancel","args":{"id":"12345"}}
```

## Response

Each response is one JSON object on its own line:

- success: `{"result": {...}}`
- failure: `{"error": "..."}`

```json
{"result":{"revision":1}}
{"result":{"id":"1"}}
{"result":{"path":"src/main.rs","diagnostics":[...],"revision":1}}
{"error":"engine: something went wrong"}
```

## Operations

| op | args | result |
|---|---|---|
| `health` | — | `DaemonHealth` (`status`, `revision`, `queued`, `uptime_secs`) |
| `capabilities` | — | `CapabilitySet` (`protocol`, `engine`, `features`) |
| `apply` | `path`, `content` | `{"result":{"revision":N}}` |
| `query` | `Query` (flattened) | `{"result":{"id":"..."}}` |
| `poll` | `id` (QueryId) | `{"result": QueryResult}` |
| `cancel` | `id` | `{"result": null}` (or `{"error": ...}`) |
| `tools` | — | list of `ToolArg`/`ToolResult` descriptors |
| `capabilities` | — | feature / protocol advertisement |

## The `Query` type (flattened)

`Query.kind` is `#[serde(flatten)]` and `priority`, `timeout_ms`, `cancel_token`
are `#[serde(default)]`. This keeps the wire JSON a single tagged object:

```json
{"kind":"diagnose","path":"src/main.rs"}
{"kind":"parse_summary","path":"src/main.rs"}
{"kind":"engine_info"}
```

`Query.kind` is a `QueryKind` enum:

- `Diagnose { path: PathBuf }`
- `ParseSummary { path: PathBuf }`
- `EngineInfo`

The `QueryResult` variants:

- `Diagnoses { path, diagnostics: Vec<Diagnostic>, revision }`
- `Summary { path, line_count, byte_len, revision }`
- `Engine { version, revision }`
- `NotFound`

## Key protocol invariants

- `QueryId::new` returns a value (not `Result`).
- EOF returns `Ok(())` from the transport reader.
- `Change` / `ChangePack` do **not** derive `Deserialize` (recursive `Change`),
  so the transport builds them manually.
- The `apply` operation wraps a single change in a `ChangePack` and returns the
  new revision.

## Example session

```bash
cargo run -p rpc-daemon -- --listen 127.0.0.1:3457
```

Then in another terminal:

```bash
# health
rpc-cli --daemon 127.0.0.1:3457 health

# apply a file
rpc-cli --daemon 127.0.0.1:3457 apply --path src/main.rs --content 'fn main() { println!(); }' --arg-json ''

# query diagnostics
rpc-cli --daemon 127.0.0.1:3457 query --json '{"kind":"diagnose","path":"src/main.rs"}'
# → {"result":{"id":"1"}}

# poll for the result
rpc-cli --daemon 127.0.0.1:3457 poll --id 1
# → {"result":{"path":"src/main.rs","diagnostics":[...],"revision":1}}
```
