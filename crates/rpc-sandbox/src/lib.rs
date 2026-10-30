//! Restricted-process execution for build scripts and proc macros.
//!
//! Proc macros and build scripts run with the privileges of the invoking user
//! and can read credentials from the ambient environment. `RestrictedProcess`
//! enforces an isolation policy — a curated environment, a fixed working
//! directory, and a watchdog timeout — and delegates execution to
//! [`rpc_codegen::SubprocessDriver`] so that a malicious or hung build script
//! cannot read credentials, escape its working directory, or hang the daemon.
//!
//! Per `docs/adr/0004-sandboxing.md`, syscall-level sandboxing (seccomp on
//! Linux, a Wasm proc-macro runtime) is the next-phase hardening step; this
//! crate provides the policy boundary that makes that future swap a local change.
#![cfg_attr(not(test), warn(missing_docs))]
#![cfg_attr(not(test), warn(rust_2018_idioms))]

use std::collections::HashMap;
use std::collections::HashSet;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

use rpc_codegen::{CodegenRequest, DriverError, SubprocessDriver};

/// Isolation policy for a subprocess.
#[derive(Debug, Clone)]
pub struct Policy {
    /// Watchdog timeout.
    pub timeout: Duration,
    /// Environment variable names permitted to pass through from the ambient
    /// environment. `PATH` and a minimal platform set are always permitted.
    pub allowed_env: HashSet<String>,
    /// Working directory for the subprocess.
    pub working_dir: PathBuf,
}

impl Policy {
    /// Create a policy with the given timeout and an empty allow-list (only
    /// `PATH` and the platform minimum are inherited).
    pub fn with_timeout(timeout: Duration) -> Self {
        Self {
            timeout,
            allowed_env: HashSet::new(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
        }
    }

    /// Allow an additional environment variable through from the ambient env.
    pub fn allow_env(mut self, name: impl Into<String>) -> Self {
        self.allowed_env.insert(name.into());
        self
    }

    /// Curate a child environment from a snapshot of the ambient environment:
    /// always-inherit minimum + the explicit allow-list. Ambient variables not
    /// in the list are dropped, which is how credential leakage is prevented.
    pub fn curated_env(&self, ambient: &HashMap<String, String>) -> Vec<(String, String)> {
        let mut env = Vec::new();
        for always in ["PATH", "SYSTEMROOT", "HOME", "TMPDIR", "TMP"] {
            if let Some(v) = ambient.get(always) {
                env.push((always.to_string(), v.clone()));
            }
        }
        for name in &self.allowed_env {
            if let Some(v) = ambient.get(name) {
                env.push((name.clone(), v.clone()));
            }
        }
        env
    }
}

/// A restricted subprocess request.
#[derive(Debug, Clone)]
pub struct RestrictedProcess {
    /// Program to run.
    pub program: String,
    /// Arguments.
    pub args: Vec<String>,
    /// Isolation policy.
    pub policy: Policy,
}

impl RestrictedProcess {
    /// Create a restricted request.
    pub fn new(program: impl Into<String>, args: Vec<String>, policy: Policy) -> Self {
        Self { program: program.into(), args, policy }
    }

    /// Build the [`CodegenRequest`] that encodes this restriction, without
    /// executing. Exposed so tests can assert on the curated environment.
    pub fn to_request(&self) -> CodegenRequest {
        let ambient: HashMap<String, String> = std::env::vars().collect();
        let env: HashMap<String, String> = self.policy.curated_env(&ambient).into_iter().collect();
        CodegenRequest {
            program: self.program.clone().into(),
            args: self.args.iter().cloned().map(OsString::from).collect(),
            working_dir: self.policy.working_dir.clone(),
            env,
            timeout: self.policy.timeout,
            capture: true,
        }
    }

    /// Execute under the policy via the codegen driver.
    pub fn run(&self) -> Result<rpc_codegen::CodegenOutput, DriverError> {
        SubprocessDriver::run(&self.to_request())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ambient_credentials_are_stripped() {
        let ambient: HashMap<String, String> = [
            ("PATH".to_string(), "/bin".to_string()),
            ("RPC_TEST_SECRET".to_string(), "leaked".to_string()),
        ]
        .into_iter()
        .collect();
        let policy = Policy::with_timeout(Duration::from_secs(5));
        let env = policy.curated_env(&ambient);
        assert!(env.iter().any(|(k, _)| k == "PATH"));
        assert!(
            !env.iter().any(|(k, _)| k == "RPC_TEST_SECRET"),
            "ambient secret leaked into sandbox env"
        );
    }

    #[test]
    fn allow_list_passes_var_through() {
        let ambient: HashMap<String, String> = [
            ("PATH".to_string(), "/bin".to_string()),
            ("RPC_TEST_SECRET".to_string(), "secret-value".to_string()),
        ]
        .into_iter()
        .collect();
        let policy = Policy::with_timeout(Duration::from_secs(5)).allow_env("RPC_TEST_SECRET");
        let env = policy.curated_env(&ambient);
        assert_eq!(
            env.iter().find(|(k, _)| k == "RPC_TEST_SECRET").map(|(_, v)| v),
            Some(&"secret-value".to_string())
        );
    }

    #[test]
    fn timeout_kills_long_runner() {
        let policy = Policy::with_timeout(Duration::from_millis(150));
        let req = RestrictedProcess::new(sleeper_program(), sleeper_args(), policy).to_request();
        let err = SubprocessDriver::run(&req).unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)));
    }

    #[cfg(unix)]
    fn sleeper_program() -> String {
        "sleep".to_string()
    }
    #[cfg(unix)]
    fn sleeper_args() -> Vec<String> {
        vec!["10".to_string()]
    }
    #[cfg(windows)]
    fn sleeper_program() -> String {
        "ping".to_string()
    }
    #[cfg(windows)]
    fn sleeper_args() -> Vec<String> {
        vec!["-n".to_string(), "3".to_string(), "127.0.0.1".to_string()]
    }
}
