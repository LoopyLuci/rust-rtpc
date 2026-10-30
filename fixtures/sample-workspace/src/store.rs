//! Shared mutable state for the `taskr` sample.
//!
//! `STATE` is a global `State` instance. `push`/`clear`/`all` mutate it, and
//! `count` reads it. This is deliberately simple so that a function-level hot
//! patch of `push` (or of the formatting loop in `main`) is observable without
//! restarting the process.

use std::sync::atomic::{AtomicUsize, Ordering};

/// A global, shared in-memory task list.
pub struct State {
    /// Tasks currently stored, in insertion order.
    tasks: std::sync::Mutex<Vec<String>>,
    /// Total number of operations performed against the state.
    ops: AtomicUsize,
}

impl State {
    /// Create a fresh, empty state.
    pub const fn new() -> Self {
        Self { tasks: std::sync::Mutex::new(Vec::new()), ops: AtomicUsize::new(0) }
    }

    /// Append a task and bump the operation counter.
    pub fn push(&self, name: &str) {
        self.tasks.lock().unwrap().push(name.to_string());
        self.ops.fetch_add(1, Ordering::SeqCst);
    }

    /// Remove all tasks and bump the operation counter.
    pub fn clear(&self) {
        self.tasks.lock().unwrap().clear();
        self.ops.fetch_add(1, Ordering::SeqCst);
    }

    /// Return a snapshot of the current tasks.
    pub fn all(&self) -> Vec<String> {
        self.ops.fetch_add(1, Ordering::SeqCst);
        self.tasks.lock().unwrap().clone()
    }

    /// Number of operations performed so far.
    pub fn count(&self) -> usize {
        self.ops.load(Ordering::SeqCst)
    }
}

/// The single shared state instance accessed by `main`.
pub static STATE: State = State::new();

/// Append `name` to the shared task list.
pub fn push(name: &str) {
    STATE.push(name);
}

/// Remove every stored task.
pub fn clear() {
    STATE.clear();
}

/// Snapshot of the current tasks.
pub fn all() -> Vec<String> {
    STATE.all()
}
