//! Install hooks (v6 power bundle): the pre/post install commands a
//! manifest declares, run by the shared install operation.
//!
//! Execution points (see `crate::install`): pre-install hooks run
//! after the destination passes the writability pre-flight and before
//! any file is extracted; post-install hooks run after files,
//! shortcuts, and writes are applied, before the receipt is written.
//! A repair run executes only the post-install phase (documented in
//! the manifest schema). Uninstall hooks do not exist yet — a future
//! slice may add them.
//!
//! Each run is recorded (a [`HookRunRecord`] in the receipt, plus a
//! `seanpak.log` line): phase, command, exit code, failure policy, and
//! outcome. A hook that fails with the `abort` policy fails the whole
//! install, which then rolls back SeanPak's own changes — a hook's
//! OWN changes are never rolled back (a command can do anything; the
//! schema text says so plainly). With `continue`, the failure is a
//! recorded warning and the install proceeds.
//!
//! Hooks run with the install context in their environment:
//! `SEANPAK_INSTALL_DIR`, `SEANPAK_APP_NAME`, `SEANPAK_APP_VERSION`.
//! There is deliberately no timeout: a hook is an install step the
//! package author chose, and installs already run under the user's
//! control (the wizard's Cancel covers extraction; a hung hook is the
//! author's bug, surfaced by the still-running install).

use std::path::Path;

use crate::manifest::{jstr, JsonValue};
use crate::overlay::{HookFailurePolicy, HookSpec};
use crate::receipt::HookRunRecord;

/// A hook failure that aborts the install: the failed run's record
/// plus the plain-language message the install reports.
#[derive(Debug)]
pub struct HookFailure {
    pub record: HookRunRecord,
    pub message: String,
}

/// The command line as a user would read it (command + args, space-
/// joined) — for messages, logs, and the dry-run report.
pub fn display_command(hook: &HookSpec) -> String {
    if hook.args.is_empty() {
        hook.command.clone()
    } else {
        format!("{} {}", hook.command, hook.args.join(" "))
    }
}

/// Runs one phase of hooks in order, in `cwd`, stopping at the first
/// `abort`-policy failure. Returns every run's record (including the
/// failing one) and the failure, when one aborted the phase.
pub fn run_hooks(
    phase: &str,
    hooks: &[HookSpec],
    cwd: &Path,
    install_dir: &Path,
    app_name: &str,
    app_version: &str,
) -> (Vec<HookRunRecord>, Option<HookFailure>) {
    let mut records: Vec<HookRunRecord> = Vec::new();
    for hook in hooks {
        let shown = display_command(hook);
        let (exit_code, succeeded, spawn_error) = {
            let mut cmd = std::process::Command::new(&hook.command);
            cmd.args(&hook.args)
                .current_dir(cwd)
                .env("SEANPAK_INSTALL_DIR", install_dir.as_os_str())
                .env("SEANPAK_APP_NAME", app_name)
                .env("SEANPAK_APP_VERSION", app_version);
            match cmd.status() {
                Ok(status) => (status.code().map(|c| c as i64), status.success(), None),
                Err(e) => (None, false, Some(e.to_string())),
            }
        };
        let record = HookRunRecord {
            phase: phase.to_string(),
            command: hook.command.clone(),
            args: hook.args.clone(),
            exit_code,
            on_failure: hook.on_failure.as_str().to_string(),
            succeeded,
        };
        crate::diag::record(
            "hook-run",
            vec![
                ("phase", jstr(phase)),
                ("command", jstr(shown.clone())),
                (
                    "exitCode",
                    exit_code
                        .map(|n| JsonValue::Number(n as f64))
                        .unwrap_or(JsonValue::Null),
                ),
                ("onFailure", jstr(hook.on_failure.as_str())),
                ("succeeded", JsonValue::Bool(succeeded)),
            ],
        );
        records.push(record.clone());
        if !succeeded {
            let detail = match (&spawn_error, exit_code) {
                (Some(e), _) => format!("could not start: {e}"),
                (None, Some(code)) => format!("exit code {code}"),
                (None, None) => {
                    "the process was killed before it could report an exit code".to_string()
                }
            };
            match hook.on_failure {
                HookFailurePolicy::Abort => {
                    return (
                        records,
                        Some(HookFailure {
                            record,
                            message: format!(
                                "The {phase} command \"{shown}\" failed ({detail}). \
                                 The install was stopped and every change SeanPak made was rolled back; \
                                 anything the command itself changed before failing was not undone."
                            ),
                        }),
                    );
                }
                HookFailurePolicy::Continue => {
                    crate::diag::record(
                        "hook-warning",
                        vec![
                            ("phase", jstr(phase)),
                            ("command", jstr(shown)),
                            ("detail", jstr(detail)),
                        ],
                    );
                }
            }
        }
    }
    (records, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::HookFailurePolicy;

    fn hook(command: &str, on_failure: HookFailurePolicy) -> HookSpec {
        HookSpec {
            command: command.to_string(),
            args: Vec::new(),
            on_failure,
        }
    }

    #[cfg(unix)]
    fn ok_command() -> &'static str {
        "true"
    }

    #[cfg(unix)]
    fn failing_command() -> &'static str {
        "false"
    }

    #[cfg(windows)]
    fn with_args(command: &str, ok: bool, on_failure: HookFailurePolicy) -> HookSpec {
        let _ = command;
        HookSpec {
            command: "cmd".to_string(),
            args: vec![
                "/C".to_string(),
                if ok { "exit 0" } else { "exit 3" }.to_string(),
            ],
            on_failure,
        }
    }

    #[test]
    fn successful_hooks_all_record() {
        let cwd = std::env::temp_dir();
        #[cfg(unix)]
        let hooks = vec![hook(ok_command(), HookFailurePolicy::Abort)];
        #[cfg(windows)]
        let hooks = vec![with_args("", true, HookFailurePolicy::Abort)];
        let (records, failure) = run_hooks("postInstall", &hooks, &cwd, &cwd, "Test App", "1.0");
        assert!(failure.is_none());
        assert_eq!(records.len(), 1);
        assert!(records[0].succeeded);
        assert_eq!(records[0].exit_code, Some(0));
    }

    #[test]
    fn abort_failure_stops_the_phase_and_reports() {
        let cwd = std::env::temp_dir();
        #[cfg(unix)]
        let hooks = vec![
            hook(failing_command(), HookFailurePolicy::Abort),
            hook(ok_command(), HookFailurePolicy::Abort),
        ];
        #[cfg(windows)]
        let hooks = vec![
            with_args("", false, HookFailurePolicy::Abort),
            with_args("", true, HookFailurePolicy::Abort),
        ];
        let (records, failure) = run_hooks("preInstall", &hooks, &cwd, &cwd, "Test App", "1.0");
        // The second hook never ran.
        assert_eq!(records.len(), 1);
        let failure = failure.expect("abort policy must surface the failure");
        assert!(failure.record.exit_code.is_some());
        assert!(!failure.record.succeeded);
        assert!(
            failure.message.contains("rolled back"),
            "{}",
            failure.message
        );
    }

    #[test]
    fn continue_failure_records_a_warning_and_proceeds() {
        let cwd = std::env::temp_dir();
        #[cfg(unix)]
        let hooks = vec![
            hook(failing_command(), HookFailurePolicy::Continue),
            hook(ok_command(), HookFailurePolicy::Abort),
        ];
        #[cfg(windows)]
        let hooks = vec![
            with_args("", false, HookFailurePolicy::Continue),
            with_args("", true, HookFailurePolicy::Abort),
        ];
        let (records, failure) = run_hooks("postInstall", &hooks, &cwd, &cwd, "Test App", "1.0");
        assert!(failure.is_none());
        assert_eq!(records.len(), 2);
        assert!(!records[0].succeeded);
        assert!(records[1].succeeded);
    }

    #[test]
    fn missing_command_is_a_failure_with_no_exit_code() {
        let cwd = std::env::temp_dir();
        let hooks = vec![hook(
            "seanpak-no-such-command-4d2f",
            HookFailurePolicy::Abort,
        )];
        let (records, failure) = run_hooks("preInstall", &hooks, &cwd, &cwd, "Test App", "1.0");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].exit_code, None);
        assert!(failure.is_some());
    }
}
