//! Startup diagnostics.
//!
//! SeanPak ships as a GUI-subsystem binary, which has no console: a
//! failure during startup used to vanish without a trace (the reported
//! "launches and exits" bug). This module gives the program a voice:
//!
//! - [`log`] appends timestamped lines to `seanpak.log` next to the
//!   executable (falling back to the temp directory), best-effort and
//!   allocation-light, so the exact startup step that fails is recorded.
//! - [`error_box`] surfaces a fatal startup error in a dialog the user can
//!   actually see (Windows only).
//! - [`attach_parent_console`] reconnects a GUI-subsystem process to the
//!   terminal it was launched from, so CLI modes (`--help`, `--batch`, ...)
//!   print where the user expects (Windows only).

use std::io::Write;
use std::path::PathBuf;
use std::sync::OnceLock;

static LOG_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Where diagnostics are written: `seanpak.log` beside the executable,
/// or in the system temp directory when the exe path is unavailable.
pub fn log_path() -> PathBuf {
    LOG_PATH
        .get_or_init(|| {
            if let Ok(exe) = std::env::current_exe() {
                if let Some(dir) = exe.parent() {
                    return dir.join("seanpak.log");
                }
            }
            std::env::temp_dir().join("seanpak.log")
        })
        .clone()
}

/// Formats seconds since the Unix epoch as `YYYY-MM-DD HH:MM:SSZ` (UTC).
/// Pure arithmetic (civil-from-days), unit-tested; avoids a date crate.
fn format_utc(epoch_secs: u64) -> String {
    let days = (epoch_secs / 86_400) as i64;
    let secs = epoch_secs % 86_400;
    let (hour, minute, second) = (secs / 3600, (secs % 3600) / 60, secs % 60);

    // Howard Hinnant's civil-from-days algorithm.
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if month <= 2 { y + 1 } else { y };

    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}:{second:02}Z")
}

fn now_stamp() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format_utc(secs)
}

/// Appends one line to the diagnostics log. Never fails the caller:
/// diagnostics are best-effort by design.
pub fn log(event: &str, detail: &str) {
    let line = format!("{} [{}] {}\n", now_stamp(), event, detail);
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Appends a structured operation record to the log: one JSON object per
/// line (after the timestamp and `[record]` tag), so a local AI can read
/// the log back and confirm what an operation actually did — which
/// command ran, its outcome, and the artifacts involved.
pub fn record(event: &str, fields: Vec<(&str, crate::manifest::JsonValue)>) {
    let mut pairs: Vec<(String, crate::manifest::JsonValue)> =
        vec![("event".to_string(), crate::manifest::jstr(event))];
    for (key, value) in fields {
        pairs.push((key.to_string(), value));
    }
    log(
        "record",
        &crate::manifest::JsonValue::Object(pairs).render(),
    );
}

/// Shows a modal error dialog (Windows). No-op elsewhere: non-Windows
/// builds keep the message in the log and on stderr via the caller.
#[cfg(windows)]
pub fn error_box(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONERROR, MB_OK};
    let wide_title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let wide_text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            wide_text.as_ptr(),
            wide_title.as_ptr(),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(not(windows))]
pub fn error_box(_title: &str, _text: &str) {}

/// Attaches the process to its parent terminal and re-points the standard
/// handles at it. Must run before any `println!`/`eprintln!` in CLI modes.
/// No-op when there is no parent console or on other platforms.
#[cfg(windows)]
pub fn attach_parent_console() {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::Console::{
        AttachConsole, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_INPUT_HANDLE,
        STD_OUTPUT_HANDLE,
    };
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return; // no parent console (e.g. launched from Explorer)
        }
        if let Ok(out) = std::fs::OpenOptions::new().write(true).open("CONOUT$") {
            let handle = out.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            let _ = SetStdHandle(STD_OUTPUT_HANDLE, handle);
            let _ = SetStdHandle(STD_ERROR_HANDLE, handle);
            std::mem::forget(out); // the console handle must outlive stdio
        }
        if let Ok(input) = std::fs::File::open("CONIN$") {
            let handle = input.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
            let _ = SetStdHandle(STD_INPUT_HANDLE, handle);
            std::mem::forget(input);
        }
    }
}

#[cfg(not(windows))]
pub fn attach_parent_console() {}

#[cfg(test)]
mod tests {
    use super::format_utc;

    #[test]
    fn utc_format_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01 00:00:00Z");
        assert_eq!(format_utc(1_761_062_400), "2025-10-21 16:00:00Z");
        assert_eq!(format_utc(1_761_116_923), "2025-10-22 07:08:43Z");
        assert_eq!(format_utc(951_825_600), "2000-02-29 12:00:00Z"); // leap day
    }
}
