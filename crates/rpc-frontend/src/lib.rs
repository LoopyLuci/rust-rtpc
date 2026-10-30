//! rpc-frontend crate for the real-time Rust pre-compiler.
//!
//! This is the workspace scaffold entry. The full implementation is added
//! incrementally, crate by crate, driven by PLAN.md.
//!
//! Until the full implementation lands this module exposes only its version
//! constant and a self-test so the workspace compiles and is testable from day
//! one.
#![cfg_attr(not(test), warn(missing_docs))]
#![cfg_attr(not(test), warn(rust_2018_idioms, unused_crate_dependencies))]

/// The semantic version of this crate's public API surface.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_compiles() {
        // Verifies the crate is wired into the workspace and is testable.
        assert!(!super::VERSION.is_empty());
    }
}
