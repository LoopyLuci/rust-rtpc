//! `taskr` — a small, dependency-free CLI used as a hot-patch / pre-compile
//! target by the spikes and the live pre-compiler.
//!
//! It models a tiny in-memory task list with a shared global `STATE`. The
//! shared mutable state is exactly what makes state-preservation across a
//! function-level hot patch observable, which is the pass criterion for
//! Spike C.

mod store;

use crate::store::STATE;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("add") => {
            if let Some(name) = args.get(2) {
                store::push(name);
                println!("added: {name}");
            } else {
                eprintln!("usage: taskr add <name>");
            }
        }
        Some("list") => {
            for (i, t) in store::all().iter().enumerate() {
                println!("{i}: {t}");
            }
        }
        Some("clear") => {
            store::clear();
            println!("cleared");
        }
        _ => eprintln!("usage: taskr <add|list|clear> [name]"),
    }
    // Print the running count so state preservation is observable.
    println!("count = {}", STATE.count());
}
