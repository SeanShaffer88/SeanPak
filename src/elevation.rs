//! The user-invoked elevation offer (v4.8.0) — the one exception to
//! SeanPak's per-user rule, picked by SEAN as the hybrid option.
//!
//! Per-user stays the default and the recommended path: installs never
//! *require* administrator rights, the embedded manifest stays
//! `asInvoker`, and nothing in this module runs unless the user asks
//! for it. The offer exists for the one wall a per-user installer
//! cannot climb: a chosen location (the classic `C:\Program Files`
//! default some packages record) that this account cannot write.
//!
//! When the shared writability verdict ([`crate::install::location_verdict`])
//! fails and this process is not elevated, the surfaces offer
//! **Restart as administrator** beside the existing **Use per-user
//! folder** remedy:
//!
//! - The wizard's Review page (install and Modify modes) shows the
//!   button; the interactive uninstall confirmation shows it too.
//!   Clicking it relaunches this same executable elevated
//!   (ShellExecuteEx with the `runas` verb — Windows shows the UAC
//!   prompt) with the original arguments preserved. This process
//!   exits only after a successful hand-off; if the user declines
//!   the prompt, the window stays open and says so in plain words.
//! - Headless runs never elevate silently: the JSON refusal gains
//!   `"elevationOffered"`, and self-elevation happens only behind
//!   the explicit `--elevate` flag, which relaunches elevated,
//!   waits for the child, and propagates its exit code.
//!
//! The loop guard is the internal [`ELEVATED_RELAUNCH_ARG`] marker:
//! a relaunched instance carries it, and an instance that has it
//! (or is already elevated) and *still* cannot write never offers
//! the restart again — it refuses plainly, exactly as before.
//!
//! The decision itself is a pure function ([`decide_elevation`]) so
//! the whole matrix is unit-tested on any host; only the token check
//! and the ShellExecute relaunch are Windows-only, and their runtime
//! behavior is Unverified until SEAN's field pass.

use std::path::{Path, PathBuf};

/// Internal marker argument carried by an instance that was started
/// by an elevation relaunch. Never documented for users; its only
/// job is to stop the offer looping when elevation did not make the
/// location writable.
pub const ELEVATED_RELAUNCH_ARG: &str = "--elevated-relaunch";

/// The explicit command-line flag that authorizes a headless run to
/// relaunch itself elevated at an unwritable location. Without it,
/// headless behavior is exactly what it has always been.
pub const ELEVATE_FLAG: &str = "--elevate";

/// Windows error returned by ShellExecuteEx when the user declines
/// the UAC prompt (ERROR_CANCELLED).
#[cfg(windows)]
const ERROR_CANCELLED_UAC: u32 = 1223;

/// The operation an elevation decision belongs to. The decision is
/// the same for all three today; the mode is carried so call sites
/// (and the log) name what was being attempted, and so a future
/// mode-specific rule has somewhere honest to live.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationMode {
    Install,
    Modify,
    Uninstall,
}

impl ElevationMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ElevationMode::Install => "install",
            ElevationMode::Modify => "modify",
            ElevationMode::Uninstall => "uninstall",
        }
    }
}

/// What a surface should do with a writability verdict.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ElevationDecision {
    /// The location is writable — proceed as always.
    Proceed,
    /// Offer **Restart as administrator** beside the per-user remedy.
    OfferRestart,
    /// The location is unwritable and no (further) restart may be
    /// offered — refuse plainly with the existing report/exit shape.
    Refuse,
}

/// The pure decision every surface shares, factored out of the
/// Windows calls so the matrix is host-testable:
///
/// - writable → [`ElevationDecision::Proceed`], whatever the token;
/// - unwritable, not elevated, no relaunch marker →
///   [`ElevationDecision::OfferRestart`];
/// - unwritable and (already elevated or carrying the marker) →
///   [`ElevationDecision::Refuse`] — an elevated instance that still
///   cannot write has a problem administrator rights will not fix,
///   and re-offering would loop the UAC prompt.
pub fn decide_elevation(
    location_writable: bool,
    is_elevated: bool,
    has_relaunch_marker: bool,
    _mode: ElevationMode,
) -> ElevationDecision {
    if location_writable {
        return ElevationDecision::Proceed;
    }
    if is_elevated || has_relaunch_marker {
        ElevationDecision::Refuse
    } else {
        ElevationDecision::OfferRestart
    }
}

/// True when the **Restart as administrator** remedy should be
/// offered for this verdict.
pub fn elevation_offered(
    location_writable: bool,
    is_elevated: bool,
    has_relaunch_marker: bool,
    mode: ElevationMode,
) -> bool {
    decide_elevation(location_writable, is_elevated, has_relaunch_marker, mode)
        == ElevationDecision::OfferRestart
}

/// The value of the JSON `elevationOffered` field for a run:
/// `None` when the location is writable (the field is omitted —
/// it makes no sense on a success or a healthy plan), otherwise
/// `Some(true)` when the restart was offered and `Some(false)`
/// when the loop guard withheld it.
pub fn elevation_offered_field(
    location_writable: bool,
    is_elevated: bool,
    has_relaunch_marker: bool,
    mode: ElevationMode,
) -> Option<bool> {
    if location_writable {
        None
    } else {
        Some(elevation_offered(
            location_writable,
            is_elevated,
            has_relaunch_marker,
            mode,
        ))
    }
}

/// Whether an argument list carries the relaunch marker.
pub fn has_relaunch_marker(args: &[String]) -> bool {
    args.iter().any(|a| a == ELEVATED_RELAUNCH_ARG)
}

/// Whether an argument list carries the explicit `--elevate` flag.
pub fn has_elevate_flag(args: &[String]) -> bool {
    args.iter().any(|a| a == ELEVATE_FLAG)
}

/// Builds the argument list for an elevated relaunch: the original
/// arguments, preserved in order, with the relaunch marker present
/// exactly once (any markers the original already carried are
/// collapsed into the single trailing one).
pub fn build_relaunch_args(original_args: &[String]) -> Vec<String> {
    let mut args: Vec<String> = original_args
        .iter()
        .filter(|a| a.as_str() != ELEVATED_RELAUNCH_ARG)
        .cloned()
        .collect();
    args.push(ELEVATED_RELAUNCH_ARG.to_string());
    args
}

/// Quotes one argument the way a Windows command line needs it:
/// plain arguments pass through; anything containing whitespace or
/// a quote is wrapped in double quotes with backslash escaping
/// (the `CommandLineToArgvW` rules the child will parse with).
pub fn quote_arg(arg: &str) -> String {
    if !arg.is_empty()
        && !arg
            .chars()
            .any(|c| c.is_whitespace() || c == '"' || c == '\'')
    {
        return arg.to_string();
    }
    let mut out = String::with_capacity(arg.len() + 2);
    out.push('"');
    let mut backslashes = 0usize;
    for c in arg.chars() {
        match c {
            '\\' => backslashes += 1,
            '"' => {
                for _ in 0..backslashes * 2 + 1 {
                    out.push('\\');
                }
                out.push('"');
                backslashes = 0;
            }
            _ => {
                for _ in 0..backslashes {
                    out.push('\\');
                }
                backslashes = 0;
                out.push(c);
            }
        }
    }
    for _ in 0..backslashes * 2 {
        out.push('\\');
    }
    out.push('"');
    out
}

/// The `lpParameters` string for an elevated relaunch of a process
/// whose original arguments were `original_args`.
pub fn relaunch_parameters(original_args: &[String]) -> String {
    build_relaunch_args(original_args)
        .iter()
        .map(|a| quote_arg(a))
        .collect::<Vec<_>>()
        .join(" ")
}

/// This process's own arguments (everything after the executable
/// name), as a relaunch should preserve them.
pub fn current_process_args() -> Vec<String> {
    std::env::args().skip(1).collect()
}

/// Why an elevated relaunch did not hand off.
#[derive(Debug, Clone, PartialEq)]
pub enum RelaunchError {
    /// The user declined the UAC prompt — not an error in the
    /// workflow sense: the caller stays open (GUI) or reports the
    /// plain refusal (CLI).
    Declined,
    /// The shell refused or failed the launch (Windows error code).
    Failed(u32),
    /// Elevation is a Windows-only mechanism.
    Unsupported,
}

impl RelaunchError {
    /// Plain-language account for status lines and error reports.
    pub fn describe(&self) -> String {
        match self {
            RelaunchError::Declined => {
                "Windows did not give administrator permission (the prompt was declined)"
                    .to_string()
            }
            RelaunchError::Failed(code) => {
                format!("Windows could not restart this program as administrator (error {code})")
            }
            RelaunchError::Unsupported => {
                "restarting as administrator is only available on Windows".to_string()
            }
        }
    }
}

/// Whether this process's token is elevated (an administrator token
/// with full privileges — the token a `runas` relaunch of a standard
/// user's prompt, or an already-admin process, runs with).
#[cfg(windows)]
pub fn is_process_elevated() -> bool {
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::Security::{
        GetTokenInformation, TokenElevation, TOKEN_ELEVATION, TOKEN_QUERY,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return false;
        }
        let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
        let mut returned: u32 = 0;
        let ok = GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut TOKEN_ELEVATION as *mut core::ffi::c_void,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned,
        );
        CloseHandle(token);
        ok != 0 && elevation.TokenIsElevated != 0
    }
}

/// Off Windows there is no token to ask; the honest answer for the
/// decision function is "not elevated".
#[cfg(not(windows))]
pub fn is_process_elevated() -> bool {
    false
}

#[cfg(windows)]
fn shell_execute_runas(
    exe: &Path,
    parameters: &str,
    wait: bool,
) -> Result<Option<u32>, RelaunchError> {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, WaitForSingleObject, INFINITE,
    };
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW, SHELLEXECUTEINFOW_0,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let verb = wide("runas");
    let file = wide(&exe.to_string_lossy());
    let params = wide(parameters);
    let directory = exe
        .parent()
        .map(|p| wide(&p.to_string_lossy()))
        .unwrap_or_default();
    let mut info = SHELLEXECUTEINFOW {
        cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
        fMask: SEE_MASK_NOCLOSEPROCESS,
        hwnd: std::ptr::null_mut(),
        lpVerb: verb.as_ptr(),
        lpFile: file.as_ptr(),
        lpParameters: params.as_ptr(),
        lpDirectory: if directory.is_empty() {
            std::ptr::null()
        } else {
            directory.as_ptr()
        },
        nShow: SW_SHOWNORMAL,
        hInstApp: std::ptr::null_mut(),
        lpIDList: std::ptr::null_mut(),
        lpClass: std::ptr::null(),
        hkeyClass: std::ptr::null_mut(),
        dwHotKey: 0,
        Anonymous: SHELLEXECUTEINFOW_0 {
            hIcon: std::ptr::null_mut(),
        },
        hProcess: std::ptr::null_mut(),
    };
    let launched = unsafe { ShellExecuteExW(&mut info) };
    if launched == 0 {
        let code = unsafe { GetLastError() };
        return Err(if code == ERROR_CANCELLED_UAC {
            RelaunchError::Declined
        } else {
            RelaunchError::Failed(code)
        });
    }
    let process = info.hProcess;
    if process.is_null() {
        return Ok(None);
    }
    let exit_code = if wait {
        unsafe {
            WaitForSingleObject(process, INFINITE);
            let mut code: u32 = 0;
            if GetExitCodeProcess(process, &mut code) != 0 {
                Some(code)
            } else {
                None
            }
        }
    } else {
        None
    };
    unsafe {
        CloseHandle(process);
    }
    Ok(exit_code)
}

/// Relaunches `exe` elevated with the original arguments preserved
/// (plus the marker, exactly once) and returns as soon as the shell
/// hands off. The caller exits only on `Ok` — a declined prompt is
/// the caller's cue to stay open and say so.
#[cfg(windows)]
pub fn relaunch_elevated(exe: &Path, original_args: &[String]) -> Result<(), RelaunchError> {
    shell_execute_runas(exe, &relaunch_parameters(original_args), false).map(|_| ())
}

/// The headless `--elevate` form: relaunch elevated, wait for the
/// elevated child to finish, and return its exit code so the caller
/// can propagate it unchanged.
#[cfg(windows)]
pub fn relaunch_elevated_wait(exe: &Path, original_args: &[String]) -> Result<i32, RelaunchError> {
    match shell_execute_runas(exe, &relaunch_parameters(original_args), true)? {
        Some(code) => Ok(code as i32),
        None => Err(RelaunchError::Failed(0)),
    }
}

/// Off Windows the relaunch itself cannot exist; the decision logic
/// above is the portable half and is what the host gate exercises.
#[cfg(not(windows))]
pub fn relaunch_elevated(_exe: &Path, _original_args: &[String]) -> Result<(), RelaunchError> {
    Err(RelaunchError::Unsupported)
}

/// See the Windows form.
#[cfg(not(windows))]
pub fn relaunch_elevated_wait(
    _exe: &Path,
    _original_args: &[String],
) -> Result<i32, RelaunchError> {
    Err(RelaunchError::Unsupported)
}

/// The executable an elevated relaunch starts: always this process's
/// own path, never a searched name.
pub fn own_exe() -> Result<PathBuf, RelaunchError> {
    std::env::current_exe().map_err(|_| RelaunchError::Unsupported)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MODES: [ElevationMode; 3] = [
        ElevationMode::Install,
        ElevationMode::Modify,
        ElevationMode::Uninstall,
    ];

    #[test]
    fn writable_locations_always_proceed() {
        for mode in MODES {
            for elevated in [false, true] {
                for marker in [false, true] {
                    assert_eq!(
                        decide_elevation(true, elevated, marker, mode),
                        ElevationDecision::Proceed,
                        "mode {mode:?} elevated {elevated} marker {marker}"
                    );
                    assert!(!elevation_offered(true, elevated, marker, mode));
                    // The JSON field is absent where it makes no
                    // sense (a writable plan / a success).
                    assert_eq!(elevation_offered_field(true, elevated, marker, mode), None);
                }
            }
        }
    }

    #[test]
    fn unwritable_and_unelevated_offers_the_restart_in_every_mode() {
        for mode in MODES {
            assert_eq!(
                decide_elevation(false, false, false, mode),
                ElevationDecision::OfferRestart,
                "mode {mode:?}"
            );
            assert!(elevation_offered(false, false, false, mode));
            assert_eq!(
                elevation_offered_field(false, false, false, mode),
                Some(true)
            );
        }
    }

    #[test]
    fn elevated_or_marked_instances_never_re_offer() {
        for mode in MODES {
            // Already elevated: administrator rights are not the
            // missing piece, so the refusal stays plain.
            assert_eq!(
                decide_elevation(false, true, false, mode),
                ElevationDecision::Refuse
            );
            // The loop guard: a relaunched instance that still
            // cannot write never prompts again.
            assert_eq!(
                decide_elevation(false, false, true, mode),
                ElevationDecision::Refuse
            );
            assert_eq!(
                decide_elevation(false, true, true, mode),
                ElevationDecision::Refuse
            );
            assert_eq!(
                elevation_offered_field(false, true, false, mode),
                Some(false)
            );
            assert_eq!(
                elevation_offered_field(false, false, true, mode),
                Some(false)
            );
        }
    }

    #[test]
    fn relaunch_args_preserve_the_originals_and_add_the_marker_once() {
        // Bare wizard: no original arguments, just the marker.
        assert_eq!(
            build_relaunch_args(&[]),
            vec![ELEVATED_RELAUNCH_ARG.to_string()]
        );
        // Modify wizard.
        assert_eq!(
            build_relaunch_args(&["--modify".to_string()]),
            vec!["--modify".to_string(), ELEVATED_RELAUNCH_ARG.to_string()]
        );
        // A headless install with an answer file and the explicit
        // --elevate flag: every argument survives, in order.
        let original = vec![
            "--install".to_string(),
            "--answers".to_string(),
            "C:\\Setup\\answers.json".to_string(),
            ELEVATE_FLAG.to_string(),
            "--json".to_string(),
        ];
        let built = build_relaunch_args(&original);
        assert_eq!(built.len(), original.len() + 1);
        assert_eq!(&built[..original.len()], &original[..]);
        assert_eq!(
            built.last().map(|s| s.as_str()),
            Some(ELEVATED_RELAUNCH_ARG)
        );
        // A list that already carries the marker keeps exactly one.
        let mut with_marker = original.clone();
        with_marker.push(ELEVATED_RELAUNCH_ARG.to_string());
        with_marker.insert(0, ELEVATED_RELAUNCH_ARG.to_string());
        let built = build_relaunch_args(&with_marker);
        assert_eq!(
            built
                .iter()
                .filter(|a| a.as_str() == ELEVATED_RELAUNCH_ARG)
                .count(),
            1
        );
        assert!(has_relaunch_marker(&built));
        assert!(has_elevate_flag(&built));
    }

    #[test]
    fn relaunch_parameters_quote_paths_with_spaces() {
        let params = relaunch_parameters(&[
            "--install".to_string(),
            "--to".to_string(),
            "C:\\Program Files\\My App".to_string(),
        ]);
        assert_eq!(
            params,
            "--install --to \"C:\\Program Files\\My App\" --elevated-relaunch"
        );
        assert_eq!(quote_arg("plain"), "plain");
        assert_eq!(quote_arg("--json"), "--json");
        assert_eq!(quote_arg("a b"), "\"a b\"");
        assert_eq!(quote_arg(""), "\"\"");
        // A trailing backslash inside quotes is doubled so it
        // cannot escape the closing quote.
        assert_eq!(quote_arg("C:\\My App\\"), "\"C:\\My App\\\\\"");
    }

    #[test]
    fn marker_and_flag_detection_is_exact() {
        assert!(has_relaunch_marker(&[
            "--modify".to_string(),
            ELEVATED_RELAUNCH_ARG.to_string()
        ]));
        assert!(!has_relaunch_marker(&["--modify".to_string()]));
        assert!(!has_relaunch_marker(&[]));
        assert!(has_elevate_flag(&[ELEVATE_FLAG.to_string()]));
        assert!(!has_elevate_flag(&["--elevated-relaunch".to_string()]));
    }
}
