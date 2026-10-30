//! rpc-mcp — Model Context Protocol adapter for the real-time pre-compiler.
//!
//! A stdio JSON-RPC 2.0 server that maps MCP methods (`initialize`,
//! `tools/list`, `tools/call`) to the `DaemonService` contract. Any
//! MCP client (Claude Desktop, Claude Code, etc.) can drive the
//! daemon through the standard line-protocol transport.

use std::sync::Arc;

use serde_json::{Value, json};

use rpc_core::engine::MockEngine;
pub use rpc_daemon::service::{Daemon, DaemonService};

/// Protocol version advertised via `initialize`.
pub const MCP_VERSION: &str = "2024-11-05";

/// The daemon-backed MCP tools (maps to `DaemonService` ops).
pub fn tool_names() -> Vec<&'static str> {
    vec!["apply", "query", "poll", "cancel", "health", "capabilities", "tools"]
}

/// Build an in-process daemon for embedded / self-test use.
pub fn build_daemon() -> Daemon {
    Daemon::new(Arc::new(MockEngine::new()), |reg| {
        reg.builtin_engine_info();
    })
}

/// JSON-RPC 2.0 message identifier (integer or string per spec).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum RpcId {
    Num(i64),
    Str(String),
}

/// A minimal in-process MCP server: holds a `Daemon` and handles
/// JSON-RPC messages by translating them to `DaemonService` calls.
pub struct McpServer {
    daemon: Arc<Daemon>,
}

impl McpServer {
    pub fn new(daemon: Arc<Daemon>) -> Self {
        Self { daemon }
    }

    /// Handle a single JSON-RPC request and return the response (or None for notifications).
    pub fn handle(&self, req: Value) -> Option<Value> {
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
        let id = req.get("id").cloned();

        let result = match method {
            "initialize" => Some(json!({
                "protocolVersion": MCP_VERSION,
                "capabilities": {
                    "tools": { "listChanged": false },
                    "prompts": null,
                    "resources": {}
                },
                "serverInfo": {
                    "name": "rpc-daemon-mcp",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })),
            "notifications/initialized" => return None,
            "ping" => Some(json!({})),
            "tools/list" => Some(json!({
                "tools": tool_names().into_iter().map(|name| json!({
                    "name": name,
                    "description": format!("rpc-daemon MCP tool: {}", name),
                    "inputSchema": { "type": "object" }
                })).collect::<Vec<Value>>()
            })),
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args_obj = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                let res = self.call_tool(name, args_obj);
                Some(json!({
                    "content": [
                        { "type": "text", "text": serde_json::to_string(&res).unwrap_or_default() }
                    ],
                    "isError": res.as_object().map(|o| o.get("error").is_some()).unwrap_or(false)
                }))
            }
            other => Some(json!({"error": {"message": format!("unknown MCP method: {}", other)}})),
        };

        Some(json!({
            "jsonrpc": "2.0",
            "id": id.unwrap_or(Value::Null),
            "result": result.unwrap_or_else(|| json!({"error": {"message": "internal error"}}))
        }))
    }

    /// Forward an MCP `tools/call` request to the underlying `DaemonService`.
    fn call_tool(&self, name: &str, args: Value) -> Value {
        match name {
            "health" => {
                let h = self.daemon.health();
                json!({"status": serde_json::to_string(&h.status).unwrap(),
                       "revision": h.revision.unwrap_or(0),
                       "queued": h.queued.unwrap_or(0),
                       "uptime": h.uptime_secs})
            }
            "capabilities" => {
                json!({"protocol": self.daemon.capabilities().protocol,
                       "engine": self.daemon.capabilities().engine,
                       "features": self.daemon.capabilities().features})
            }
            "tools" => {
                json!({"tools": self.daemon.tools().all()
                    .iter().map(|t| t.name()).collect::<Vec<_>>()})
            }
            "apply" => {
                let path = args.get("path").and_then(|v| v.as_str()).unwrap_or("");
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                let pack =
                    rpc_core::ChangePack::new(vec![rpc_core::engine::Change::new(path, content)]);
                match self.daemon.apply(pack) {
                    Ok(rev) => json!({"revision": rev}),
                    Err(e) => json!({"error": e.to_string()}),
                }
            }
            "query" => {
                let q: rpc_core::types::Query = match serde_json::from_value(args) {
                    Ok(q) => q,
                    Err(_) => return json!({"error": "invalid query JSON"}),
                };
                match self.daemon.query(q) {
                    Ok(id) => json!({"id": id.to_string()}),
                    Err(e) => json!({"error": e.to_string()}),
                }
            }
            "poll" => {
                let id_str = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let qid = rpc_core::types::QueryId::new(id_str);
                match self.daemon.poll(&qid) {
                    Ok(r) => json!({"result": serde_json::to_string(&r).unwrap_or("{}".into())}),
                    Err(e) => json!({"error": e.to_string()}),
                }
            }
            "cancel" => {
                let id_str = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let qid = rpc_core::types::QueryId::new(id_str);
                match self.daemon.cancel(&qid) {
                    Ok(()) => json!({"cancelled": true}),
                    Err(e) => json!({"error": e.to_string()}),
                }
            }
            _ => json!({"error": format!("unknown MCP tool: {}", name)}),
        }
    }
}

/// Self-test: drive the MCP server through a `tools/list` and `tools/call` (apply)
/// cycle over an in-process daemon, verifying the adapter round-trips correctly.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn mcp_round_trip() {
        let daemon = Arc::new(build_daemon());
        let server = McpServer::new(Arc::clone(&daemon));

        // initialize handshake (id=0)
        let init_req = json!({"jsonrpc":"2.0","id":0,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}},"clientInfo":{"name":"test","version":"0.1.0"}});
        let init_resp = server.handle(init_req).expect("initialize should respond");
        assert!(init_resp.get("id").is_some());
        assert!(init_resp.get("result").is_some());
        assert_eq!(
            init_resp.get("result").unwrap().get("protocolVersion"),
            Some(&json!("2024-11-05"))
        );

        // initialize notification (no id -> None response)
        let init_notif = json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}});
        assert!(server.handle(init_notif).is_none());

        // tools/list
        let tools_req = json!({"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}});
        let tools_resp = server.handle(tools_req).expect("tools/list response");
        assert!(tools_resp.get("result").unwrap().get("tools").is_some());

        // tools/call apply
        let apply_req = json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"apply","arguments":{"path":"src/main.rs","content":"fn main() {}"}}});
        let apply_resp = server.handle(apply_req).expect("tools/call apply response");
        assert!(
            apply_resp.get("result").unwrap()["content"].get(0).unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("revision")
        );

        // health via tools/call
        let health_req = json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"health","arguments":{}}});
        let health_resp = server.handle(health_req).expect("tools/call health response");
        assert!(health_resp.get("result").is_some());
    }
}
