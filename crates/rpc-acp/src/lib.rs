//! rpc-acp — Agent Communication Protocol adapter for the daemon.
//!
//! Translates ACP-style agent messages (`agent/instructions`, `agent/status`,
//! `agent/response`, `agent/proposal`) into `DaemonService` operations, so an
//! ACP agent can drive compilation, query, and tool calls without the CLI.

use std::sync::Arc;

use serde_json::{Value, json};

use rpc_core::engine::MockEngine;
pub use rpc_daemon::service::{Daemon, DaemonService};

/// Build an in-process daemon for embedded / self-test use.
pub fn build_daemon() -> Daemon {
    Daemon::new(Arc::new(MockEngine::new()), |reg| {
        reg.builtin_engine_info();
    })
}

/// ACP adapter holding a daemon reference.
pub struct AcpAdapter {
    daemon: Arc<Daemon>,
}

impl AcpAdapter {
    pub fn new(daemon: Arc<Daemon>) -> Self {
        Self { daemon }
    }

    /// Handle an ACP agent message and return the response.
    pub fn handle(&self, msg: Value) -> Option<Value> {
        let msg_type = msg.get("type").and_then(|v| v.as_str()).unwrap_or("");
        let payload = msg.get("payload").cloned().unwrap_or_else(|| json!({}));
        let id = msg.get("id").cloned();
        let result = match msg_type {
            "agent/instructions" => {
                json!({
                    "description": "Agent-facing real-time Rust pre-compiler",
                    "capabilities": {
                        "protocol": self.daemon.capabilities().protocol,
                        "engine": self.daemon.capabilities().engine,
                        "features": self.daemon.capabilities().features
                    }
                })
            }
            "agent/status" => {
                let h = self.daemon.health();
                json!({"status": serde_json::to_string(&h.status).unwrap(), "revision": h.revision.unwrap_or(0), "uptime": h.uptime_secs})
            }
            "agent/response" => {
                // Echo the payload back with daemon health overlay for now.
                json!({"ack": true, "payload": payload, "daemon": {"revision": self.daemon.engine().revision()}})
            }
            "agent/proposal" => {
                // Proposals are translated as queries to the daemon.
                let q: rpc_core::types::Query = serde_json::from_value(payload)
                    .unwrap_or_else(|_| rpc_core::types::Query::engine_info());
                match self.daemon.query(q) {
                    Ok(qid) => json!({"proposal_id": qid.to_string()}),
                    Err(e) => json!({"error": e.to_string()}),
                }
            }
            other => json!({"error": format!("unknown ACP message type: {}", other)}),
        };
        Some(if let Some(id) = id {
            json!({"type": "agent/response", "payload": result, "id": id})
        } else {
            json!({"type": "agent/response", "payload": result})
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acp_round_trip() {
        let daemon = Arc::new(build_daemon());
        let adapter = AcpAdapter::new(Arc::clone(&daemon));

        let instructions = adapter
            .handle(json!({"type":"agent/instructions","payload":{}}))
            .expect("agent/instructions response");
        assert!(instructions.get("payload").unwrap()["capabilities"].get("protocol").is_some());

        let status = adapter
            .handle(json!({"type":"agent/status","payload":{}}))
            .expect("agent/status response");
        assert!(status.get("payload").unwrap()["status"].as_str().is_some());

        let proposal = adapter
            .handle(json!({"type":"agent/proposal","payload":{"kind":"engine_info"}}))
            .expect("agent/proposal response");
        assert!(proposal.get("payload").unwrap()["proposal_id"].as_str().is_some());
    }
}
