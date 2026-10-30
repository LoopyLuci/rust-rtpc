//! Line-based service transport: one JSON request per line, one JSON response
//! per line.
//!
//! This is the wire framing the TCP listener uses. It is deliberately simple so
//! HTTP, MCP and ACP can all build on the same dispatch loop. The JSON shape of
//! a request is:
//!
//! ```json
//! {"op": "apply", "args": {"path": "src/main.rs", "content": "fn main() {}"}}
//! ```
//!
//! and the shape of a response is:
//!
//! ```json
//! {"result": {"revision": 1}}
//! ```
//!
//! Errors are reported as `{"error": "..."}`. No bytes higher than this layer
//! transit the wire.

use std::io::{BufRead, BufReader, Write};

use crate::service::{Daemon, DaemonService};
use rpc_core::{Change, ChangePack};

/// Start serving `daemon` over a TCP stream. Reads lines from `stream` and
/// replies to each line. Returns when the stream reaches EOF.
pub fn run(
    daemon: &crate::service::Daemon,
    stream: &mut std::net::TcpStream,
) -> std::io::Result<()> {
    loop {
        let mut line = String::new();
        {
            let mut reader = BufReader::new(&mut *stream);
            let n = reader.read_line(&mut line)?;
            if n == 0 {
                break Ok(());
            }
        }
        if line.trim().is_empty() {
            continue;
        }
        let resp = dispatch(daemon, &line);
        writeln!(stream, "{}", serde_json::to_string(&resp).unwrap())?;
        stream.flush()?;
    }
}

fn dispatch(daemon: &Daemon, line: &str) -> serde_json::Value {
    let req: serde_json::Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return error("invalid request".to_string()),
    };
    let op = req.get("op").and_then(|v| v.as_str()).unwrap_or("health");
    let args = req.get("args").cloned().unwrap_or(serde_json::Value::Object(Default::default()));
    match op {
        "apply" => {
            let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            let pack = ChangePack::new(vec![Change::new(path, content)]);
            daemon
                .apply(pack)
                .map(|rev| ok_json(serde_json::json!({ "revision": rev })))
                .unwrap_or_else(|e| error(e.to_string()))
        }
        "query" => {
            let q = match serde_json::from_value(args) {
                Ok(q) => q,
                Err(_) => return error("invalid query".to_string()),
            };
            daemon
                .query(q)
                .map(|id| ok_json(serde_json::json!({ "id": id.to_string() })))
                .unwrap_or_else(|e| error(e.to_string()))
        }
        "poll" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let qid = crate::protocol::QueryId::new(id);
            daemon
                .poll(&qid)
                .map(|r| ok_json(serde_json::json!({ "result": r })))
                .unwrap_or_else(|e| error(e.to_string()))
        }
        "cancel" => {
            let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
            let qid = crate::protocol::QueryId::new(id);
            daemon
                .cancel(&qid)
                .map(|_| ok_json(serde_json::json!({ "cancelled": true })))
                .unwrap_or_else(|e| error(e.to_string()))
        }
        "health" => ok_json(serde_json::json!(daemon.health())),
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
        "capabilities" => ok_json(serde_json::json!(daemon.capabilities())),
        other => error(format!("unknown operation: {other}")),
    }
}

fn ok_json(v: serde_json::Value) -> serde_json::Value {
    serde_json::json!({ "result": v })
}

fn error(msg: String) -> serde_json::Value {
    serde_json::json!({ "error": msg })
}
