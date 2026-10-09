//! T036: Copilot integration boundary.
//!
//! GitHub Copilot is supported **only** through its documented interfaces:
//! the `gh` CLI and the official `gh-copilot` extension. There is no
//! documented non-interactive completions API, and this module deliberately
//! does not touch IDE plugin token flows or `api.githubcopilot.com`.
//!
//! [`probe_copilot_support`] checks at runtime — never assumes — whether a
//! headless integration is possible, and returns an explicit [`CopilotSupport`]
//! value. When headless automation is unavailable the result is
//! [`CopilotSupport::Unsupported`] carrying a concrete, actionable reason.

use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// T036: result of probing for a supported headless Copilot interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopilotSupport {
    /// A supported headless interface was found at runtime.
    Supported {
        /// Which interface, e.g. `gh copilot --json`.
        interface: String,
        /// Version string of the backing tool.
        version: String,
    },
    /// No supported headless interface is available. The reason is always
    /// populated with what was checked and what was missing.
    Unsupported {
        reason: String,
    },
}

impl CopilotSupport {
    pub fn is_supported(&self) -> bool {
        matches!(self, Self::Supported { .. })
    }
}

/// Configuration for the Copilot probe.
#[derive(Debug, Clone)]
pub struct CopilotProbe {
    /// `gh` binary to invoke. Defaults to `gh`; override in tests or when the
    /// binary lives elsewhere. The `FABRIC_COPILOT_GH_BIN` env var also
    /// overrides it (checked first) without being logged.
    pub gh_binary: String,
    /// Per-command timeout.
    pub timeout: Duration,
}

impl Default for CopilotProbe {
    fn default() -> Self {
        Self {
            gh_binary: "gh".to_string(),
            timeout: Duration::from_secs(10),
        }
    }
}

impl CopilotProbe {
    pub fn new(gh_binary: impl Into<String>, timeout: Duration) -> Self {
        Self {
            gh_binary: gh_binary.into(),
            timeout,
        }
    }

    /// Resolve the effective `gh` binary: env override wins, then config.
    /// Only the binary *path* is used; no secret values are ever read.
    fn effective_binary(&self) -> String {
        std::env::var("FABRIC_COPILOT_GH_BIN")
            .ok()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or_else(|| self.gh_binary.clone())
    }

    /// Probe for a supported headless Copilot interface.
    ///
    /// Checks, in order:
    /// 1. the `gh` CLI runs at all,
    /// 2. the official copilot extension is installed,
    /// 3. `gh copilot --help` documents a non-interactive mode.
    ///
    /// Step 3 is the headless gate: `gh copilot suggest`/`explain` are
    /// interactive commands, so without a documented non-interactive mode the
    /// boundary reports [`CopilotSupport::Unsupported`].
    pub fn probe(&self) -> CopilotSupport {
        let bin = self.effective_binary();

        let version_out = match run_bounded(&bin, &["--version"], self.timeout) {
            Some(o) if o.exit_ok => o.stdout,
            _ => {
                return CopilotSupport::Unsupported {
                    reason: format!(
                        "gh CLI not runnable as `{bin}`: Copilot integration requires the GitHub CLI"
                    ),
                }
            }
        };
        let version = version_out
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .to_string();

        let ext_out = match run_bounded(&bin, &["extension", "list"], self.timeout) {
            Some(o) if o.exit_ok => o.stdout.to_lowercase(),
            _ => {
                return CopilotSupport::Unsupported {
                    reason: format!(
                        "gh CLI present ({version}) but `gh extension list` failed; \
                         cannot verify the copilot extension"
                    ),
                }
            }
        };
        if !ext_out.contains("copilot") {
            return CopilotSupport::Unsupported {
                reason: format!(
                    "gh CLI present ({version}) but the copilot extension is not installed; \
                     install with `gh extension install github/gh-copilot`"
                ),
            };
        }

        // Headless gate: look for a documented non-interactive mode.
        let help = run_bounded(&bin, &["copilot", "--help"], self.timeout)
            .map(|o| o.stdout.to_lowercase())
            .unwrap_or_default();
        let non_interactive_markers = ["non-interactive", "noninteractive", "--json", "--silent", "--yes", "-y"];
        if let Some(marker) = non_interactive_markers
            .iter()
            .find(|m| help.contains(*m))
        {
            return CopilotSupport::Supported {
                interface: format!("gh copilot ({marker} mode documented)"),
                version,
            };
        }

        CopilotSupport::Unsupported {
            reason: format!(
                "gh CLI present ({version}) with the copilot extension installed, but no \
                 supported headless interface exists: `gh copilot` commands require an \
                 interactive terminal and GitHub documents no non-interactive completions API; \
                 IDE plugin token flows and api.githubcopilot.com are intentionally not used"
            ),
        }
    }
}

/// Probe with default configuration.
pub fn probe_copilot_support() -> CopilotSupport {
    CopilotProbe::default().probe()
}

/// Outcome of one bounded subprocess invocation.
struct BoundedOutput {
    stdout: String,
    exit_ok: bool,
}

/// Run `cmd args` with stdout piped, stdin nulled, stderr discarded, and a
/// hard timeout. Returns `None` when the binary cannot start.
fn run_bounded(cmd: &str, args: &[&str], timeout: Duration) -> Option<BoundedOutput> {
    let mut child = Command::new(cmd)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let mut buf = String::new();
                // Best effort: output may be truncated on read error; still usable.
                let _ = stdout.read_to_string(&mut buf);
                return Some(BoundedOutput {
                    stdout: buf,
                    exit_ok: status.success(),
                });
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Some(BoundedOutput {
                        stdout: String::new(),
                        exit_ok: false,
                    });
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => {
                let _ = child.kill();
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a fake `gh` script that answers --version / extension list /
    /// copilot --help from canned responses, and point the probe at it.
    fn fake_gh(dir: &std::path::Path, version: &str, extensions: &str, help: &str) -> String {
        let path = dir.join("gh");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh").unwrap();
        writeln!(f, "case \"$1 $2\" in").unwrap();
        writeln!(f, "\"--version \") echo {version} ;;").unwrap();
        writeln!(f, "\"extension list\") printf '%s' \"{extensions}\" ;;").unwrap();
        writeln!(f, "\"copilot --help\") printf '%s' \"{help}\" ;;").unwrap();
        writeln!(f, "esac").unwrap();
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&path).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&path, p).unwrap();
        }
        path.to_string_lossy().into_owned()
    }

    fn tmpdir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("fabric-copilot-test-{name}"));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    /// Probe an explicit binary path. Asserts the env override is unset so
    /// tests stay hermetic under parallel execution.
    fn probe_bin(bin: &str) -> CopilotSupport {
        assert!(
            std::env::var_os("FABRIC_COPILOT_GH_BIN").is_none(),
            "test pollution: FABRIC_COPILOT_GH_BIN is set"
        );
        CopilotProbe::new(bin, Duration::from_secs(5)).probe()
    }

    #[test]
    fn missing_binary_is_unsupported_with_reason() {
        let probe = CopilotProbe::new("/nonexistent/gh-binary-xyz", Duration::from_secs(2));
        match probe.probe() {
            CopilotSupport::Unsupported { reason } => {
                assert!(reason.contains("not runnable"), "{reason}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn missing_extension_is_unsupported_with_reason() {
        let d = tmpdir("noext");
        let bin = fake_gh(&d, "gh version 2.75.0", "owner/ext-one", "usage: gh copilot");
        let r = probe_bin(&bin);
        match r {
            CopilotSupport::Unsupported { reason } => {
                assert!(reason.contains("copilot extension is not installed"), "{reason}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn extension_without_headless_mode_is_unsupported() {
        let d = tmpdir("noheadless");
        let bin = fake_gh(
            &d,
            "gh version 2.75.0",
            "github/gh-copilot",
            "usage: gh copilot <suggest|explain> (interactive)",
        );
        let r = probe_bin(&bin);
        match r {
            CopilotSupport::Unsupported { reason } => {
                assert!(reason.contains("no supported headless interface"), "{reason}");
                assert!(reason.contains("interactive terminal"), "{reason}");
            }
            other => panic!("expected Unsupported, got {other:?}"),
        }
    }

    #[test]
    fn documented_non_interactive_mode_is_supported() {
        let d = tmpdir("headless");
        let bin = fake_gh(
            &d,
            "gh version 2.99.0",
            "github/gh-copilot",
            "flags: --json output as JSON for non-interactive use",
        );
        let r = probe_bin(&bin);
        match r {
            CopilotSupport::Supported { interface, version } => {
                assert!(interface.contains("gh copilot"), "{interface}");
                assert!(version.contains("2.99.0"), "{version}");
            }
            other => panic!("expected Supported, got {other:?}"),
        }
    }

    #[test]
    fn hanging_binary_hits_timeout() {
        let d = tmpdir("hang");
        let path = d.join("gh");
        let mut f = std::fs::File::create(&path).unwrap();
        writeln!(f, "#!/bin/sh\nsleep 30").unwrap();
        drop(f);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut p = std::fs::metadata(&path).unwrap().permissions();
            p.set_mode(0o755);
            std::fs::set_permissions(&path, p).unwrap();
        }
        let probe = CopilotProbe::new(path.to_string_lossy().into_owned(), Duration::from_millis(300));
        // Must return (Unsupported: gh not runnable / timed out), not hang.
        let start = Instant::now();
        let r = probe.probe();
        assert!(start.elapsed() < Duration::from_secs(10), "probe hung");
        assert!(!r.is_supported());
    }
}
