//! Codegen execution, isolated behind a subprocess with a watchdog.
//!
//! The pre-compiler never links the compiler or a code generator into the
//! daemon process. Codegen is driven by [`SubprocessDriver`], which spawns a
//! subprocess (the default backend is `rustc`, with Cranelift as a fast
//! debug-codegen path), enforces a per-task watchdog timeout, and returns
//! structured output. An internal-compiler-error in `rustc` kills only the
//! subprocess, not the daemon.
#![cfg_attr(not(test), warn(missing_docs))]
#![cfg_attr(not(test), warn(rust_2018_idioms))]

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use thiserror::Error;

/// A backend codegen invocation the daemon requests.
#[derive(Debug, Clone)]
pub struct CodegenRequest {
    /// Program to run, e.g. `rustc`.
    pub program: OsString,
    /// Arguments passed to the program.
    pub args: Vec<OsString>,
    /// Working directory for the subprocess.
    pub working_dir: PathBuf,
    /// Environment overrides. `PATH` is always inherited; callers may set
    /// additional vars (e.g. `RUSTC_WRAPPER`) here.
    pub env: HashMap<String, String>,
    /// Hard watchdog timeout. The driver returns [`DriverError::Timeout`] if
    /// the process has not exited by then.
    pub timeout: Duration,
    /// When `true`, capture stdout/stderr instead of inheriting the console.
    pub capture: bool,
}

impl CodegenRequest {
    /// Build a capturing request that inherits the current working directory and
    /// a one-minute watchdog.
    pub fn capture(program: impl Into<OsString>, args: Vec<OsString>) -> Self {
        Self {
            program: program.into(),
            args,
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            env: HashMap::new(),
            timeout: Duration::from_secs(60),
            capture: true,
        }
    }
}

/// Output of a codegen subprocess.
#[derive(Debug)]
pub struct CodegenOutput {
    /// Process exit code, if it terminated.
    pub exit_code: Option<i32>,
    /// `true` if the watchdog killed the process.
    pub timed_out: bool,
    /// Captured stdout (empty when `capture` was false).
    pub stdout: Vec<u8>,
    /// Captured stderr (empty when `capture` was false).
    pub stderr: Vec<u8>,
}

impl CodegenOutput {
    /// `true` if the subprocess reported success.
    pub fn success(&self) -> bool {
        !self.timed_out && self.exit_code == Some(0)
    }
}

/// Errors raised by the driver itself (distinct from a non-zero codegen exit).
#[derive(Debug, Error)]
pub enum DriverError {
    /// The subprocess could not be spawned or reaped.
    #[error("codegen subprocess error: {0}")]
    Io(#[from] std::io::Error),
    /// The watchdog fired and the process was killed.
    #[error("codegen subprocess timed out after {0:?}")]
    Timeout(Duration),
}

fn read_all(mut r: impl Read) -> Vec<u8> {
    let mut buf = Vec::new();
    let _ = r.read_to_end(&mut buf);
    buf
}

/// Drives an isolated codegen subprocess with a watchdog timeout.
pub struct SubprocessDriver;

impl SubprocessDriver {
    /// Spawn the request, enforce the watchdog, and return the captured output.
    pub fn run(req: &CodegenRequest) -> Result<CodegenOutput, DriverError> {
        let mut child = if req.capture {
            Command::new(&req.program)
                .args(&req.args)
                .current_dir(&req.working_dir)
                .envs(&req.env)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?
        } else {
            Command::new(&req.program)
                .args(&req.args)
                .current_dir(&req.working_dir)
                .envs(&req.env)
                .spawn()?
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (tx_out, rx_out) = mpsc::channel::<Vec<u8>>();
        let (tx_err, rx_err) = mpsc::channel::<Vec<u8>>();
        if let Some(so) = stdout {
            thread::spawn(move || {
                let b = read_all(so);
                let _ = tx_out.send(b);
            });
        }
        if let Some(se) = stderr {
            thread::spawn(move || {
                let b = read_all(se);
                let _ = tx_err.send(b);
            });
        }

        let deadline = Instant::now() + req.timeout;
        let exit_code = loop {
            match child.try_wait()? {
                Some(status) => break status.code(),
                None => {
                    if Instant::now() >= deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(DriverError::Timeout(req.timeout));
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            }
        };

        // The process exited. Drain the captured streams (they are already EOF).
        let stdout = rx_out.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        let stderr = rx_err.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
        Ok(CodegenOutput { exit_code, timed_out: false, stdout, stderr })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rustc_version_runs() {
        let req = CodegenRequest::capture("rustc", vec!["--version".into()]);
        let out = SubprocessDriver::run(&req).expect("rustc --version should run");
        assert!(out.success());
        assert!(out.stdout.starts_with(b"rustc"));
    }

    #[test]
    fn bad_flag_reports_failure() {
        let req = CodegenRequest::capture("rustc", vec!["--this-flag-does-not-exist".into()]);
        let out = SubprocessDriver::run(&req).expect("spawn should succeed");
        assert!(!out.success());
        assert!(out.exit_code.is_some_and(|c| c != 0));
    }

    fn sleeper() -> CodegenRequest {
        let mut req = CodegenRequest::capture(sleeper_program(), sleeper_args());
        req.timeout = Duration::from_millis(150);
        req
    }

    #[test]
    fn timeout_kills_long_runner() {
        let err = SubprocessDriver::run(&sleeper()).unwrap_err();
        assert!(matches!(err, DriverError::Timeout(_)), "got {err:?}");
    }

    #[cfg(unix)]
    fn sleeper_program() -> OsString {
        "sleep".into()
    }
    #[cfg(unix)]
    fn sleeper_args() -> Vec<OsString> {
        vec!["10".into()]
    }
    #[cfg(windows)]
    fn sleeper_program() -> OsString {
        "ping".into()
    }
    #[cfg(windows)]
    fn sleeper_args() -> Vec<OsString> {
        vec!["-n".into(), "3".into(), "127.0.0.1".into()]
    }
}
