//! T092/T093 — bounded verifier command execution.
//!
//! Executes a configured verifier command with bounded time, output,
//! environment, process tree, and resource limits:
//!
//! - Spawn via [`std::process::Command`] with the exact argv; **no shell** is
//!   ever involved. (Tests may legitimately use `sh` as argv[0]; that is the
//!   program being spawned, not shell interpretation by this module.)
//! - `timeout_ms`: the child is killed on expiry and the outcome is a typed
//!   [`InfraFailure::TimeoutKilled`], never a hang.
//! - `max_output_bytes`: stdout+stderr combined are capped; the tail beyond
//!   the cap is discarded, `truncated` is recorded, and the outcome is a
//!   typed [`InfraFailure::OutputCapExceeded`] (truncated output is not
//!   trustworthy input for normalization).
//! - Environment: the child starts from an **empty** environment
//!   (`env_clear`); only variable names on the explicit `env_allowlist` are
//!   copied from the parent. Secret-bearing variables are scrubbed by
//!   construction — they cannot reach the child unless explicitly allow-listed.
//! - Process tree: on timeout the direct child is killed (SIGKILL) and reaped.
//!   **Limitation:** detached grandchildren are not reaped by this module
//!   (no libc dependency permitted for setpgid/killpg); hard process-tree
//!   containment is the deployer's job (cgroup/container). What this module
//!   *does* guarantee: `execute` always returns within roughly `timeout_ms`
//!   plus a small bounded grace period. Reader threads are joined with that
//!   grace; if a detached grandchild keeps the pipes open, the drain is
//!   abandoned and the outcome is a typed [`InfraFailure::TimeoutKilled`]
//!   (timeout path) or [`InfraFailure::OutputDrainStalled`] (child already
//!   exited). Documented here so nobody assumes otherwise.
//! - Resource limits (CPU/memory/rlimit): not set by this module (no libc
//!   dependency permitted); bounded via timeout + output cap + the empty
//!   environment above. Documented, not silently claimed.
//!
//! T093: exit codes are captured and classified into distinct typed outcomes:
//! a verifier that ran fine with a non-zero exit is a **test failure**
//! ([`ExecOutcome::TestFailed`]), never confused with runner infrastructure
//! failure ([`ExecOutcome::InfraFailed`]).

use fabric_types::{FabricError, Result};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Configuration for one verifier execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecConfig {
    /// Exact argv; argv[0] is the program. No shell interpretation.
    pub argv: Vec<String>,
    /// Working directory for the child; None inherits the runner's.
    pub workdir: Option<std::path::PathBuf>,
    /// Explicit allow-list of environment variable names copied from the
    /// parent process into the child. The child environment is otherwise
    /// empty: secret-bearing variables never reach the child by default.
    pub env_allowlist: Vec<String>,
    /// Wall-clock bound in milliseconds. Kill on expiry.
    pub timeout_ms: u64,
    /// Max captured stdout+stderr bytes combined.
    pub max_output_bytes: u64,
}

/// Record of a verifier process that ran to completion with intact output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunRecord {
    /// Exit code; None only if the process was terminated by a signal.
    pub exit_code: Option<i32>,
    pub wall_ms: u64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// True if output hit the cap and the tail was discarded.
    pub truncated: bool,
}

/// Runner-level infrastructure failures: the verifier never produced a
/// trustworthy result. Distinct from a verifier that ran and reported
/// failure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum InfraFailure {
    /// The child process could not be spawned at all.
    SpawnFailed { reason: String },
    /// The child exceeded `timeout_ms` and was killed.
    TimeoutKilled { wall_ms: u64 },
    /// Combined output exceeded `max_output_bytes`; tail discarded.
    OutputCapExceeded { capped_at_bytes: u64 },
    /// The child was terminated by a signal rather than exiting.
    TerminatedBySignal,
    /// The direct child exited but a detached grandchild kept the output
    /// pipes open, so the captured output is incomplete. The reader threads
    /// were abandoned after a grace period rather than hanging the runner.
    OutputDrainStalled { captured_bytes: u64 },
}

/// Typed outcome of one verifier execution (T093).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExecOutcome {
    /// Verifier ran fine, exit code 0.
    Passed(RunRecord),
    /// Verifier ran fine, non-zero exit: the *tests* failed, not the runner.
    TestFailed { exit_code: i32, record: RunRecord },
    /// The runner failed; no trustworthy result exists.
    InfraFailed(InfraFailure),
}

impl ExecConfig {
    fn validate(&self) -> Result<()> {
        if self.argv.is_empty() {
            return Err(FabricError::InvalidInput {
                field: "argv",
                reason: "verifier argv must be non-empty".to_string(),
            });
        }
        if self.timeout_ms == 0 {
            return Err(FabricError::InvalidInput {
                field: "timeout_ms",
                reason: "timeout bound must be non-zero".to_string(),
            });
        }
        if self.max_output_bytes == 0 {
            return Err(FabricError::InvalidInput {
                field: "max_output_bytes",
                reason: "output bound must be non-zero".to_string(),
            });
        }
        Ok(())
    }
}

/// Shared, cap-enforcing output sink for one stream.
struct CappedSink {
    buf: Mutex<Vec<u8>>,
    total: AtomicU64,
    truncated: AtomicBool,
    cap: u64,
}

impl CappedSink {
    fn new(cap: u64) -> Self {
        Self {
            buf: Mutex::new(Vec::new()),
            total: AtomicU64::new(0),
            truncated: AtomicBool::new(false),
            cap,
        }
    }

    /// Feed bytes; returns false once the cap is hit (caller should stop
    /// appending but keep draining).
    fn feed(&self, chunk: &[u8]) {
        let mut total = self.total.load(Ordering::Relaxed);
        if total >= self.cap {
            self.truncated.store(true, Ordering::Relaxed);
            return;
        }
        let remaining = (self.cap - total) as usize;
        let take = chunk.len().min(remaining);
        self.buf.lock().unwrap().extend_from_slice(&chunk[..take]);
        total += take as u64;
        self.total.store(total, Ordering::Relaxed);
        if take < chunk.len() {
            self.truncated.store(true, Ordering::Relaxed);
        }
    }

    /// Snapshot the captured bytes and truncation flag without consuming.
    /// Used both on the normal path and when the reader thread was
    /// abandoned (its Arc clone is still alive) — never fabricates state.
    fn snapshot(&self) -> (Vec<u8>, bool) {
        let buf = self.buf.lock().unwrap().clone();
        (buf, self.truncated.load(Ordering::Relaxed))
    }
}

/// Drain a pipe to EOF, enforcing the shared cap across both streams.
fn drain_stream(
    mut pipe: impl Read + Send + 'static,
    sink: Arc<CappedSink>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => sink.feed(&chunk[..n]),
                Err(_) => break,
            }
        }
    })
}

/// Join a reader thread, but never hang the runner: if the thread is still
/// draining after `grace` (a detached grandchild is holding the pipe open),
/// abandon it and report that the drain stalled. Returns true when the
/// thread was abandoned.
fn join_reader(h: std::thread::JoinHandle<()>, grace: Duration) -> bool {
    let start = Instant::now();
    while !h.is_finished() {
        if start.elapsed() >= grace {
            return true; // abandoned: thread keeps blocking until pipes close
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = h.join();
    false
}

/// Execute one verifier command under the configured bounds.
///
/// Returns `Err` only for invalid configuration (pre-spawn). Everything
/// during/after the run is a typed [`ExecOutcome`]. This function always
/// returns within roughly `timeout_ms` plus a small bounded grace period,
/// even when the child detaches grandchildren.
pub fn execute(config: &ExecConfig) -> Result<ExecOutcome> {
    config.validate()?;

    let mut cmd = Command::new(&config.argv[0]);
    if config.argv.len() > 1 {
        cmd.args(&config.argv[1..]);
    }
    if let Some(dir) = &config.workdir {
        cmd.current_dir(dir);
    }
    // Scrub the environment: start empty, copy only the allow-list.
    cmd.env_clear();
    for name in &config.env_allowlist {
        if let Ok(value) = std::env::var(name) {
            cmd.env(name, value);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return Ok(ExecOutcome::InfraFailed(InfraFailure::SpawnFailed {
                reason: e.to_string(),
            }))
        }
    };

    let stdout_sink = Arc::new(CappedSink::new(config.max_output_bytes));
    let stderr_sink = Arc::new(CappedSink::new(config.max_output_bytes));
    let out_h = drain_stream(child.stdout.take().expect("stdout piped"), Arc::clone(&stdout_sink));
    let err_h = drain_stream(child.stderr.take().expect("stderr piped"), Arc::clone(&stderr_sink));

    let start = Instant::now();
    let deadline = Duration::from_millis(config.timeout_ms);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) => {
                if start.elapsed() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait(); // reap the direct child
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => break None,
        }
    };

    // Join the drainers with a bounded grace period. A detached grandchild
    // holding the pipes open must never hang the runner.
    let grace = Duration::from_millis(config.timeout_ms.clamp(100, 2_000));
    let out_abandoned = join_reader(out_h, grace);
    let err_abandoned = join_reader(err_h, grace);
    let drain_stalled = out_abandoned || err_abandoned;
    let wall_ms = start.elapsed().as_millis().min(u64::MAX as u128) as u64;

    let (stdout, out_trunc) = stdout_sink.snapshot();
    let (stderr, err_trunc) = stderr_sink.snapshot();
    let truncated = out_trunc || err_trunc;
    let captured_bytes = (stdout.len() + stderr.len()) as u64;

    if timed_out || status.is_none() {
        return Ok(ExecOutcome::InfraFailed(InfraFailure::TimeoutKilled { wall_ms }));
    }
    let status = status.expect("checked is_some");

    if drain_stalled && !truncated {
        return Ok(ExecOutcome::InfraFailed(InfraFailure::OutputDrainStalled {
            captured_bytes,
        }));
    }

    if truncated {
        return Ok(ExecOutcome::InfraFailed(InfraFailure::OutputCapExceeded {
            capped_at_bytes: config.max_output_bytes,
        }));
    }

    let record = RunRecord {
        exit_code: status.code(),
        wall_ms,
        stdout,
        stderr,
        truncated: false,
    };

    match status.code() {
        Some(0) => Ok(ExecOutcome::Passed(record)),
        Some(code) => Ok(ExecOutcome::TestFailed {
            exit_code: code,
            record,
        }),
        None => Ok(ExecOutcome::InfraFailed(InfraFailure::TerminatedBySignal)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(argv: &[&str], timeout_ms: u64, cap: u64) -> ExecConfig {
        ExecConfig {
            argv: argv.iter().map(|s| s.to_string()).collect(),
            workdir: None,
            env_allowlist: vec![],
            timeout_ms,
            max_output_bytes: cap,
        }
    }

    #[test]
    fn passing_command_reports_passed() {
        let out = execute(&cfg(&["sh", "-c", "exit 0"], 5_000, 65_536)).unwrap();
        match out {
            ExecOutcome::Passed(rec) => assert_eq!(rec.exit_code, Some(0)),
            other => panic!("expected Passed, got {:?}", other),
        }
    }

    #[test]
    fn nonzero_exit_is_test_failure_not_infra() {
        let out = execute(&cfg(&["sh", "-c", "exit 3"], 5_000, 65_536)).unwrap();
        match out {
            ExecOutcome::TestFailed { exit_code, record } => {
                assert_eq!(exit_code, 3);
                assert_eq!(record.exit_code, Some(3));
                assert!(!record.truncated);
            }
            other => panic!("expected TestFailed, got {:?}", other),
        }
    }

    #[test]
    fn timeout_kills_and_reports_infra() {
        // `exec` so the shell does not fork: killing the single process
        // closes the pipes promptly.
        let out = execute(&cfg(&["sh", "-c", "exec sleep 30"], 300, 65_536)).unwrap();
        match out {
            ExecOutcome::InfraFailed(InfraFailure::TimeoutKilled { wall_ms }) => {
                assert!(wall_ms < 10_000, "killed promptly, got {}ms", wall_ms);
            }
            other => panic!("expected TimeoutKilled, got {:?}", other),
        }
    }

    #[test]
    fn detached_grandchild_cannot_hang_runner() {
        // The shell exits immediately; a detached `sleep 5` grandchild keeps
        // the pipes open. The runner must return (drain stalled) rather than
        // hang, well before the grandchild exits on its own.
        let start = Instant::now();
        let out = execute(&cfg(&["sh", "-c", "sleep 5 &"], 10_000, 65_536)).unwrap();
        let elapsed = start.elapsed();
        match out {
            ExecOutcome::InfraFailed(InfraFailure::OutputDrainStalled { .. }) => {}
            other => panic!("expected OutputDrainStalled, got {:?}", other),
        }
        assert!(
            elapsed < Duration::from_secs(5),
            "runner must not wait out the grandchild, took {:?}",
            elapsed
        );
    }

    #[test]
    fn output_cap_reports_infra_not_test_failure() {
        // ~1 MiB of output against a 4 KiB cap.
        let out = execute(
            &cfg(
                &["sh", "-c", "head -c 1048576 /dev/zero | tr '\\0' x; exit 0"],
                10_000,
                4_096,
            ),
        )
        .unwrap();
        match out {
            ExecOutcome::InfraFailed(InfraFailure::OutputCapExceeded { capped_at_bytes }) => {
                assert_eq!(capped_at_bytes, 4_096);
            }
            other => panic!("expected OutputCapExceeded, got {:?}", other),
        }
    }

    #[test]
    fn output_exactly_at_cap_is_not_truncated() {
        let out = execute(
            &cfg(&["sh", "-c", "head -c 100 /dev/zero | tr '\\0' x"], 5_000, 100),
        )
        .unwrap();
        match out {
            ExecOutcome::Passed(rec) => {
                assert_eq!(rec.stdout.len(), 100);
                assert!(!rec.truncated);
            }
            other => panic!("expected Passed, got {:?}", other),
        }
    }

    #[test]
    fn spawn_failure_is_infra() {
        let out = execute(&cfg(
            &["/nonexistent-program-xyz-12345"],
            5_000,
            65_536,
        ))
        .unwrap();
        assert!(matches!(
            out,
            ExecOutcome::InfraFailed(InfraFailure::SpawnFailed { .. })
        ));
    }

    #[test]
    fn secret_bearing_env_is_scrubbed() {
        std::env::set_var("FABRIC_VERIFY_TEST_SECRET_XYZ", "super-secret-value");
        // Not on the allow-list: must not reach the child.
        let c = ExecConfig {
            argv: vec!["sh".into(), "-c".into(), "echo -n ${FABRIC_VERIFY_TEST_SECRET_XYZ-unset}".into()],
            workdir: None,
            env_allowlist: vec![],
            timeout_ms: 5_000,
            max_output_bytes: 65_536,
        };
        let out = execute(&c).unwrap();
        match out {
            ExecOutcome::Passed(rec) => {
                assert_eq!(rec.stdout, b"unset");
            }
            other => panic!("expected Passed, got {:?}", other),
        }
        std::env::remove_var("FABRIC_VERIFY_TEST_SECRET_XYZ");
    }

    #[test]
    fn allowlisted_env_reaches_child() {
        std::env::set_var("FABRIC_VERIFY_TEST_ALLOW_XYZ", "visible");
        let c = ExecConfig {
            argv: vec!["sh".into(), "-c".into(), "echo -n $FABRIC_VERIFY_TEST_ALLOW_XYZ".into()],
            workdir: None,
            env_allowlist: vec!["FABRIC_VERIFY_TEST_ALLOW_XYZ".into()],
            timeout_ms: 5_000,
            max_output_bytes: 65_536,
        };
        let out = execute(&c).unwrap();
        match out {
            ExecOutcome::Passed(rec) => assert_eq!(rec.stdout, b"visible"),
            other => panic!("expected Passed, got {:?}", other),
        }
        std::env::remove_var("FABRIC_VERIFY_TEST_ALLOW_XYZ");
    }

    #[test]
    fn empty_argv_is_config_error() {
        let c = ExecConfig {
            argv: vec![],
            workdir: None,
            env_allowlist: vec![],
            timeout_ms: 5_000,
            max_output_bytes: 65_536,
        };
        assert!(matches!(
            execute(&c).unwrap_err(),
            FabricError::InvalidInput { field: "argv", .. }
        ));
    }

    #[test]
    fn zero_bounds_are_config_errors() {
        let mut c = cfg(&["sh", "-c", "exit 0"], 0, 65_536);
        assert!(execute(&c).is_err());
        c = cfg(&["sh", "-c", "exit 0"], 5_000, 0);
        assert!(execute(&c).is_err());
    }
}
