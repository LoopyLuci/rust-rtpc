//! Entry point for `rpc-daemon`.
//!
//! Two modes:
//!   - `--one-shot CMD` — build a local `Daemon` over the mock engine and run a
//!     single control command. Prints the result as JSON (for tests and build
//!     scripts to consume).
//!   - `--listen <addr>` — serve `DaemonService` over a TCP stream using the
//!     line-based transport in `transport.rs`.

use std::process::ExitCode;
use std::sync::Arc;

use rpc_core::engine::MockEngine;
use rpc_daemon::service::{Daemon, DaemonService};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("usage: rpc-daemon --one-shot CMD [args...] | --listen <addr>");
        return ExitCode::from(2);
    }
    match args[1].as_str() {
        "--version" | "-v" => {
            println!("rpc-daemon {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "--one-shot" => one_shot(&args[2..]),
        "--listen" => serve(&args[2..]),
        other => {
            eprintln!("unknown mode: {other}\nusage: --one-shot CMD [args...] | --listen <addr>");
            ExitCode::from(2)
        }
    }
}

fn one_shot(args: &[String]) -> ExitCode {
    let mut it = args.iter().peekable();
    let cmd = it.next().expect("no command");

    // A daemon that owns the engine plus the agent-first `engine_info` tool.
    let daemon = Daemon::new(Arc::new(MockEngine::new()), |reg| {
        reg.builtin_engine_info();
    });

    match cmd.as_str() {
        "apply" => {
            let path = it.next().expect("apply <path>");
            let content = it.next().expect("apply <path> <content>");
            let pack = rpc_core::ChangePack::new(vec![rpc_core::Change::new(path, content)]);
            match daemon.apply(pack) {
                Ok(rev) => {
                    println!("{rev}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("apply: {e}");
                    ExitCode::from(1)
                }
            }
        }
        "query" => {
            let kind = it.next().expect("query <kind>");
            let q = match kind.as_str() {
                "engine_info" => rpc_daemon::protocol::Query::engine_info(),
                _ => rpc_core::types::Query::diagnose(it.next().expect("query <kind> <path>")),
            };
            match daemon.query(q) {
                Ok(id) => {
                    println!("{id}");
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("query: {e}");
                    ExitCode::from(1)
                }
            }
        }
        "poll" => {
            let id = it.next().expect("poll <id>");
            match daemon.poll(&rpc_daemon::protocol::QueryId::new(id)) {
                Ok(_r) => {
                    // The result was streamed to the configured output in the
                    // service layer.
                    ExitCode::SUCCESS
                }
                Err(e) => {
                    eprintln!("poll: {e}");
                    ExitCode::from(1)
                }
            }
        }
        "health" => {
            let _ = serde_json::to_writer(std::io::stdout(), &daemon.health());
            println!();
            ExitCode::SUCCESS
        }
        "tools" => {
            let names: Vec<String> =
                daemon.tools().all().iter().map(|t| t.name().to_string()).collect();
            println!("{}", serde_json::to_string(&names).unwrap());
            ExitCode::SUCCESS
        }
        other => {
            eprintln!(
                "unknown one-shot command: {other}\nknown: apply, query, poll, health, tools"
            );
            ExitCode::from(2)
        }
    }
}

fn serve(addr: &[String]) -> ExitCode {
    let addr = addr.first().expect("--listen <addr>");
    let daemon = Arc::new(Daemon::new(Arc::new(MockEngine::new()), |reg| {
        reg.builtin_engine_info();
    }));
    let listener = match std::net::TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("bind {addr}: {e}");
            return ExitCode::from(1);
        }
    };
    let addr = listener.local_addr().expect("bound listener");
    eprintln!("rpc-daemon listening on {}", addr);
    loop {
        match listener.accept() {
            Ok((mut sock, _)) => {
                let daemon = Arc::clone(&daemon);
                std::thread::spawn(move || {
                    let _ = rpc_daemon::transport::run(&daemon, &mut sock);
                });
            }
            Err(e) => {
                eprintln!("accept: {e}");
            }
        }
    }
}

/// Must be public so `cargo test` can reach the one-shot logic under a
/// deterministic path. Kept in a separate crate (main) per workspace rules.
#[cfg(test)]
mod tests {}
