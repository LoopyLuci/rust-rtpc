//! Driver for the agentic pre-compiler framework. `rpc-cli` speaks the
//! line-based JSON protocol that `rpc-daemon` owns ([`transport::run`]).
//! It is one of the four official bindings (CLI / API / MCP / ACP) and is the
//! primary way an agent drives a running daemon.
//!
//! # Usage
//!
//! ```
//! # Use the compiled binary, not this doc example.
//! # 1. self-test (in-process)
//! rpc-cli --self-test
//! # 2. health + capabilities over TCP
//! rpc-cli --daemon 127.0.0.1:0 health
//! # 3. list tools
//! rpc-cli --daemon 127.0.0.1:0 tools
//! # 4. apply a file
//! rpc-cli --daemon 127.0.0.1:0 apply --path src/main.rs --content 'fn main() {}' --arg-json ''
//! # 5. run a query and wait for its result
//! rpc-cli --daemon 127.0.0.1:0 query --json '{"kind":"diagnose","path":"src/main.rs"}'
//! rpc-cli --daemon 127.0.0.1:0 poll --id 1
//! ```
//!
//! Pass `--stdio` to drive the daemon from stdin (one JSON request per line,
//! one JSON response per line) — handy for shell pipelines.
//!
//! The wire protocol is one JSON object per line:
//!   - request: `{"op":"<name>","args":{...}}`
//!   - response: `{"result":{...}}` or `{"error":"..."}`

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use rpc_daemon::service::{Daemon, DaemonService, EngineInfo as EngineInfoTool};
use rpc_daemon::transport;
use rpc_daemon::{MockEngine, QueryId};

use rpc_core::{
    ChangePack,
    types::{Query, QueryResult},
};

/// Construct an in-process daemon used by stdio/legacy modes.
fn daemon() -> Daemon {
    let engine = Arc::new(MockEngine::new());
    Daemon::new(engine, |reg| {
        reg.insert(EngineInfoTool);
    })
}

/// Main entry point.
fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("rpc-cli v{}\n\n{}", env!("CARGO_PKG_VERSION"), USAGE);
        std::process::exit(2);
    }

    match args[1].as_str() {
        "--self-test" => self_test(),
        "--stdio" => stdio_mode(),
        "--daemon" => {
            if args.len() < 4 {
                eprintln!("rpc-cli: --daemon ADDR CMD  (got {})\n{1}", args.len(), USAGE);
                std::process::exit(2);
            }
            let addr = args.get(2).expect("addr");
            let cmd = &args[3..];
            daemon_connect(addr, cmd);
        }
        _ => daemon_mode(&args),
    }
}

fn self_test() {
    // In-process smoke test: build a daemon, drive it through the service
    // trait, then through the line protocol.
    let daemon = daemon();

    // health / capabilities
    let h = daemon.health();
    assert_eq!(h.status, rpc_core::types::DaemonStatus::Running);
    let cap = daemon.capabilities();
    assert_eq!(cap.protocol, "rpc-daemon/1");

    // apply + query + poll a cycle
    let pack =
        ChangePack::new(vec![rpc_core::engine::Change::new("src/main.rs", "fn main() {}\n")]);
    let rev = daemon.apply(pack).expect("apply");
    assert_eq!(rev, 1);

    let qid = daemon.query(Query::diagnose("src/main.rs")).expect("query");
    let result = match daemon.poll(&qid) {
        Ok(QueryResult::Diagnoses { diagnostics, .. }) => diagnostics,
        Ok(other) => panic!("expected Diagnoses, got {other:?}"),
        Err(e) => panic!("poll failed: {e}"),
    };
    assert!(!result.is_empty());

    // Transmit one request/response over the line protocol.
    let req = serde_json::json!({ "op": "health", "args": {} });
    let resp = dispatch(&daemon, &req);
    assert!(resp.get("result").is_some());

    println!("rpc-cli self-test OK");
}

fn stdio_mode() {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = std::io::BufWriter::new(stdout);
    let mut lines = BufReader::new(stdin).lines();

    while let Some(Ok(line)) = lines.next() {
        let resp = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(v) => dispatch(&daemon(), &v),
            Err(_) => serde_json::json!({ "error": "invalid request" }),
        };
        let _ = writeln!(out, "{}", serde_json::to_string(&resp).unwrap());
        let _ = out.flush();
    }
}

fn daemon_mode(args: &[String]) {
    // args[1..] = <op> [args...]
    let op = args[1].clone();
    let rest = &args[2..];

    // Launch a daemon on 127.0.0.1:0 in a background thread, then drive it
    // from the foreground thread over the line protocol.
    let (addr, _handle) = {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind daemon");
        let addr = listener.local_addr().expect("local addr");
        let daemon = daemon();

        std::thread::spawn(move || {
            // One accepted connection at a time.
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = transport::run(&daemon, &mut stream);
            }
        });

        (addr, std::thread::sleep(std::time::Duration::from_millis(150)))
    };

    // Connect to the daemon and exchange one request/response.
    let mut stream = TcpStream::connect(addr).expect("client connect");
    let req = build_req(&op, rest);
    let req = match serde_json::to_string(&req) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rpc-cli: serialize request: {e}");
            std::process::exit(1);
        }
    };
    // Send the request followed by a newline (the transport reads one JSON
    // line at a time, so the request must be newline-terminated).
    std::io::Write::write_all(&mut stream, req.as_bytes()).unwrap();
    std::io::Write::write_all(&mut stream, b"\n").unwrap();
    let mut response = [0u8; 65536];
    let n = std::io::Read::read(&mut stream, &mut response).unwrap_or(0);
    let resp_text = String::from_utf8_lossy(&response[..n]);
    let resp: serde_json::Value = serde_json::from_str(&resp_text).unwrap_or_else(|e| {
        eprintln!("rpc-cli: malformed response: {e}");
        serde_json::json!({ "error": "malformed response" })
    });

    if let Some(err) = resp.get("error") {
        eprintln!("rpc-cli error: {}", err);
        std::process::exit(1);
    } else {
        println!("{}", resp);
    }

    // Close the client side so the daemon's run() returns.
    drop(stream);
}

/// Drive an EXTERNAL daemon at `addr` with the given command.
fn daemon_connect(addr: &str, cmd: &[String]) {
    let mut stream = TcpStream::connect(addr).expect("client connect");
    let op = cmd.first().map(|s| s.as_str()).unwrap_or("health");
    let rest = &cmd[1..];
    let req = build_req(op, rest);
    let req = match serde_json::to_string(&req) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("rpc-cli: serialize request: {e}");
            std::process::exit(1);
        }
    };
    // Send the request followed by a newline (the transport reads one JSON
    // line at a time, so the request must be newline-terminated).
    std::io::Write::write_all(&mut stream, req.as_bytes()).unwrap();
    std::io::Write::write_all(&mut stream, b"\n").unwrap();

    let mut response = [0u8; 65536];
    let n = std::io::Read::read(&mut stream, &mut response).unwrap_or(0);
    let resp_text = String::from_utf8_lossy(&response[..n]);
    let resp: serde_json::Value = serde_json::from_str(&resp_text).unwrap_or_else(|e| {
        eprintln!("rpc-cli: malformed response: {e}");
        serde_json::json!({ "error": "malformed response" })
    });

    if let Some(err) = resp.get("error") {
        eprintln!("rpc-cli error: {err}");
        std::process::exit(1);
    } else {
        println!("{}", resp);
    }
}

fn build_req(op: &str, args: &[String]) -> serde_json::Value {
    match op {
        "health" | "tools" => serde_json::json!({ "op": op, "args": {} }),
        "apply" => {
            let path = args
                .iter()
                .find(|a| a.starts_with("--path="))
                .and_then(|a| a.strip_prefix("--path=").map(String::from))
                .unwrap_or_else(|| {
                    eprintln!("rpc-cli: apply requires --path <path>");
                    std::process::exit(2);
                });
            let content = args
                .iter()
                .find(|a| a.starts_with("--content="))
                .and_then(|a| a.strip_prefix("--content=").map(String::from))
                .unwrap_or_else(|| {
                    eprintln!("rpc-cli: apply requires --content <content>");
                    std::process::exit(2);
                });
            serde_json::json!({ "op": op, "args": { "path": path, "content": content } })
        }
        "query" => {
            let q = args
                .iter()
                .find(|a| a.starts_with("--json="))
                .and_then(|a| a.strip_prefix("--json=").map(String::from))
                .unwrap_or_else(|| {
                    eprintln!("rpc-cli: query requires --json <query-json>");
                    std::process::exit(2);
                });
            let q: Query = serde_json::from_str(&q).unwrap_or_else(|e| {
                eprintln!("rpc-cli: invalid query JSON: {e}");
                std::process::exit(2);
            });
            serde_json::json!({ "op": op, "args": serde_json::to_value(&q).unwrap() })
        }
        "poll" | "cancel" => {
            let id = args
                .iter()
                .find(|a| a.starts_with("--id="))
                .and_then(|a| a.strip_prefix("--id=").map(String::from))
                .unwrap_or_else(|| {
                    eprintln!("rpc-cli: {op} requires --id <query-id>");
                    std::process::exit(2);
                });
            serde_json::json!({ "op": op, "args": { "id": id } })
        }
        _ => serde_json::json!({ "op": op, "args": {} }),
    }
}

fn dispatch(daemon: &Daemon, req: &serde_json::Value) -> serde_json::Value {
    let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("health");
    let args = req.get("args").cloned().unwrap_or_default();
    match op {
        "health" => {
            let h = daemon.health();
            serde_json::json!({ "result": { "status": format!("{:?}", h.status) } })
        }
        "tools" => serde_json::json!({
            "result": {
                "tools": daemon
                    .tools()
                    .all()
                    .iter()
                    .map(|t| t.name().to_string())
                    .collect::<Vec<_>>()
            }
        }),
        "apply" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let pack = ChangePack::new(vec![rpc_core::engine::Change::new(path, content)]);
            daemon
                .apply(pack)
                .map(|rev| serde_json::json!({ "result": { "revision": rev } }))
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }))
        }
        "query" => {
            let q: Query = match serde_json::from_value(args) {
                Ok(q) => q,
                Err(_) => return serde_json::json!({ "error": "invalid query" }),
            };
            daemon
                .query(q)
                .map(|id| serde_json::json!({ "result": { "id": id.to_string() } }))
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }))
        }
        "poll" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let qid = QueryId::new(id.to_string());
            daemon
                .poll(&qid)
                .map(|r| serde_json::json!({ "result": r }))
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }))
        }
        "cancel" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let qid = QueryId::new(id.to_string());
            daemon
                .cancel(&qid)
                .map(|_| serde_json::json!({ "result": { "ok": true } }))
                .unwrap_or_else(|e| serde_json::json!({ "error": e.to_string() }))
        }
        other => serde_json::json!({ "error": format!("unknown op {other}") }),
    }
}

const USAGE: &str = "rpc-cli v{version} - drive the real-time Rust pre-compiler daemon.

Usage:
  rpc-cli --self-test
  rpc-cli --daemon 127.0.0.1:0 <op> [<arg>...]
  rpc-cli --stdio

Operators:
  health      daemon health
  tools       list registered tools
  apply       apply a change pack (--path <p> --content <c>)
  query       submit a query (--json <query-json>)
  poll        fetch a queued query result (--id <id>)
  cancel      cancel a query (--id <id>)
";
