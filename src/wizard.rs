//! SeanPak installer wizard (TesselGUI Design E — the Rail layout).
//!
//! When a built SeanPak installer (this executable plus a payload overlay)
//! is run with no arguments, this wizard opens instead of the packager GUI.
//! A fixed left rail carries the app's identity (monogram tile, name,
//! version, publisher), the four steps (Review, Options, Installing,
//! Done — current step accented, completed steps checked), and the disk
//! space the install needs and finds. The content area holds four pages:
//!
//! 1. **Review** — every install decision in a gridded table
//!    (Item / Value / Change). Values come from the package's overlay
//!    manifest and its version 3+ install-options block (defaults for
//!    older packages). A row whose Change column offers an action routes
//!    to the Options page. The page also runs the shared writability
//!    pre-flight (`install::location_verdict`): a location this per-user
//!    installer cannot write is flagged in its row, its Change action
//!    becomes the one-click switch to the per-user folder, and Install
//!    stays disabled until the location is usable.
//! 2. **Options** — edit the install location (folder picker or typed),
//!    toggle the desktop / Start Menu / PATH rows and — when the
//!    package offers it — "Start <App> at login", edit the Start Menu
//!    folder and the file associations.
//! 3. **Progress** — the shared install operation runs on a worker
//!    thread with live per-file progress; Cancel asks it to stop, the
//!    operation rolls back, and the wizard lands on the Finish page in
//!    its honest "cancelled, nothing kept" state.
//! 4. **Finish** — what was installed, where, and the receipt path; or,
//!    when the install failed, the plain-language reason and the fact
//!    that every change was rolled back. On success, a package that
//!    offers it shows a "Launch <App>" checkbox (checked per the
//!    package's default); closing with it checked starts the app.
//!
//! The module also hosts [`confirm_uninstall`]: the small native
//! confirmation window an interactive uninstall opens — what will be
//! removed, plus the opt-in data-deletion checkbox — in the same
//! visual language.
//!
//! Install itself is `crate::install::perform_install_with` — the exact
//! operation the CLI's `--install` runs — including shortcuts, file
//! associations, PATH entry, upgrade/repair detection, and the install
//! receipt, all journaled so uninstall (or a failed install's rollback)
//! removes precisely what was applied.
//!
//! Windows-only: on other platforms `InstallWizard::run` returns an error.

use std::path::PathBuf;

use crate::overlay::{FileRecordHeader, InstallOptions, OverlayManifestHeader};

/// Builds the Rail review rows (Item / Value / Change) for a package.
/// Pure and platform-independent so the table content is unit-tested.
/// (Disk space is not a row: the rail reports it, under the step list.)
///
/// When the install location fails the shared writability pre-flight
/// (`location_writable` false), the location row says so in its Value
/// and its Change action becomes the one-click offer to switch to the
/// per-user folder instead of the usual route to the Options page.
pub fn review_rows(
    manifest: &OverlayManifestHeader,
    options: &InstallOptions,
    records: &[FileRecordHeader],
    install_dir_display: &str,
    location_writable: bool,
) -> Vec<[String; 3]> {
    let primary = records
        .get(manifest.primary_exe_index as usize)
        .map(|r| r.relative_path());
    let components = match (records.len(), primary) {
        (0, _) => "No files".to_string(),
        (1, Some(p)) => p,
        (n, Some(p)) => format!("{p} + {} more files", n - 1),
        (n, None) => format!("{n} files"),
    };
    let start_menu = options.start_menu_folder();
    let start_menu = if start_menu.is_empty() {
        "None".to_string()
    } else {
        start_menu
    };
    let associations = options.file_associations();
    let associations = if associations.is_empty() {
        "None".to_string()
    } else {
        associations
    };
    let yes_no = |on: bool| if on { "Yes" } else { "No" }.to_string();
    let location_row = if location_writable {
        [
            "Install location".to_string(),
            install_dir_display.to_string(),
            "Browse…".to_string(),
        ]
    } else {
        [
            "Install location".to_string(),
            format!("{install_dir_display} — needs administrator access"),
            "Use per-user folder".to_string(),
        ]
    };
    let mut rows = vec![
        location_row,
        ["Components".to_string(), components, "Choose…".to_string()],
        [
            "Start Menu folder".to_string(),
            start_menu,
            "Edit".to_string(),
        ],
        [
            "Desktop shortcut".to_string(),
            yes_no(options.wants_desktop_shortcut()),
            "Toggle".to_string(),
        ],
        [
            "File associations".to_string(),
            associations,
            "Choose…".to_string(),
        ],
        [
            "PATH entry".to_string(),
            yes_no(options.wants_path_entry()),
            "Toggle".to_string(),
        ],
    ];
    // The start-at-login row exists only when the package offers the
    // option; it stays last.
    if options.offers_start_at_login() {
        rows.push([
            "Start at login".to_string(),
            yes_no(options.start_at_login_chosen()),
            "Toggle".to_string(),
        ]);
    }
    rows
}

/// The install directory proposed by a package: its recorded install
/// location, or `C:\Program Files\<AppName>` when none was recorded.
pub fn default_install_dir(options: &InstallOptions, app_name: &str) -> PathBuf {
    let recorded = options.install_location();
    if recorded.trim().is_empty() {
        PathBuf::from(format!("C:\\Program Files\\{app_name}"))
    } else {
        PathBuf::from(recorded)
    }
}

/// Filesystem/registry-safe fragment of an app name (mirrors the folder
/// sanitizer in `main.rs`).
pub fn sanitize_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "App".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Registry path of the ARP entry the wizard writes for `app_name`
/// (under HKEY_CURRENT_USER).
pub fn uninstall_key_path(app_name: &str) -> String {
    format!(
        "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\SeanPak-{}",
        sanitize_name(app_name)
    )
}

/// Removes the ARP entry for `app_name` (Windows only; no-op elsewhere).
#[cfg(windows)]
pub fn delete_uninstall_entry(app_name: &str) {
    use windows_sys::Win32::System::Registry::{RegDeleteKeyW, HKEY_CURRENT_USER};
    let wide: Vec<u16> = uninstall_key_path(app_name)
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    unsafe {
        RegDeleteKeyW(HKEY_CURRENT_USER, wide.as_ptr());
    }
}

/// Removes the ARP entry for `app_name` (Windows only; no-op elsewhere).
#[cfg(not(windows))]
pub fn delete_uninstall_entry(_app_name: &str) {}

/// The uninstall confirmation window, in the wizard's visual language:
/// names what uninstall removes and — when the install recorded data
/// directories — offers a checkbox (default UNCHECKED) to delete the
/// app's data files too, listing the exact folders and warning that
/// deleted data cannot be recovered.
///
/// Returns `Some(purge_data)` when the user confirms (the checkbox
/// state), or `None` when the window is cancelled or closed — the
/// caller must then remove nothing. The window is a view over the
/// shared uninstall operation ([`crate::uninstall::perform_uninstall`]);
/// it decides nothing else.
///
/// Off Windows there is no native window; the function proceeds with
/// the safe default (uninstall, keep data).
#[cfg(windows)]
pub fn confirm_uninstall(app_name: &str, data_dirs: &[PathBuf]) -> Option<bool> {
    imp::confirm_uninstall(app_name, data_dirs)
}

/// Off Windows there is no native window; proceeds with the safe
/// default (uninstall, keep data). See the Windows form.
#[cfg(not(windows))]
pub fn confirm_uninstall(_app_name: &str, _data_dirs: &[PathBuf]) -> Option<bool> {
    Some(false)
}

/// The Rail-layout installer wizard for one overlay package.
pub struct InstallWizard {
    manifest: OverlayManifestHeader,
    records: Vec<FileRecordHeader>,
    options: InstallOptions,
    exe_path: PathBuf,
}

impl InstallWizard {
    pub fn new(
        manifest: OverlayManifestHeader,
        records: Vec<FileRecordHeader>,
        options: InstallOptions,
        exe_path: PathBuf,
    ) -> Self {
        Self {
            manifest,
            records,
            options,
            exe_path,
        }
    }
}

#[cfg(windows)]
impl InstallWizard {
    /// Opens the wizard window and runs its event-driven message loop.
    pub fn run(&mut self) -> Result<(), u32> {
        imp::run(
            self.manifest,
            std::mem::take(&mut self.records),
            self.options,
            self.exe_path.clone(),
        )
    }
}

#[cfg(not(windows))]
impl InstallWizard {
    /// The wizard is a native Win32 window; other platforms get an error.
    pub fn run(&mut self) -> Result<(), u32> {
        // Touch the package data so the type stays fully exercised (and
        // inspectable by tests/tools) on platforms without the window.
        let _ = (
            self.manifest.app_name(),
            self.records.len(),
            self.options.install_location(),
            self.exe_path.display().to_string(),
        );
        Err(1)
    }
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows_sys::Win32::UI::Controls::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use super::{default_install_dir, review_rows, InstallOptions};
    use crate::install::{InstallOutcome, ProgressEvent};
    use crate::overlay::{FileRecordHeader, OverlayManifestHeader};
    use crate::scanner::format_bytes;

    // TesselGUI rev2 GitHub Dark palette (COLORREF is 0x00BBGGRR).
    const COLOR_BG: u32 = 0x0017_110D; // #0D1117
    const COLOR_SURFACE: u32 = 0x001D_1612; // #12161D
    const COLOR_RAIL: u32 = 0x0009_0401; // #010409 (the rail, darkest panel)
    const COLOR_DEEP: u32 = 0x003C_2615; // #15263C (info box fill)
    const COLOR_FG: u32 = 0x00FC_F6F0; // #F0F6FC
    const COLOR_MUTED: u32 = 0x00A1_9891; // #9198A1
    const COLOR_BLUE: u32 = 0x00F7_812F; // #2F81F7
    const COLOR_GREEN: u32 = 0x0037_7F1A; // #1A7F37
    const COLOR_OK: u32 = 0x0050_B93F; // #3FB950 (completed-step check)

    // The rail: fixed left panel, its step list, and its disk block.
    const RAIL_W: i32 = 150;
    const ID_RAIL_BG: usize = 340;
    const ID_LBL_APPNAME: usize = 341;
    const ID_LBL_APPVER: usize = 342;
    const ID_LBL_APPPUB: usize = 343;
    const ID_LBL_DISKCAP: usize = 344;
    const ID_LBL_DISKNEED: usize = 345;
    const ID_LBL_DISKFREE: usize = 346;
    const STEP_NAMES: [&str; 4] = ["Review", "Options", "Installing", "Done"];
    const ID_STEP_ACCENTS: [usize; 4] = [350, 351, 352, 353];
    const ID_STEP_NUMS: [usize; 4] = [354, 355, 356, 357];
    const ID_STEP_NAMES: [usize; 4] = [358, 359, 360, 361];

    const ID_TILE: usize = 400;
    const ID_LBL_HEADER: usize = 401;
    const ID_LBL_STEP: usize = 402;
    const ID_LBL_INFO: usize = 403;
    const ID_LBL_HINT: usize = 404;
    const ID_LBL_STATUS: usize = 405;
    const ID_LBL_SUBTITLE: usize = 406;
    const ID_LIST: usize = 410;
    const ID_BTN_CANCEL: usize = 301;
    const ID_BTN_INSTALL: usize = 302;
    const ID_BTN_BACK: usize = 303;
    // Options page (page 2).
    const ID_LBL_LOC: usize = 420;
    const ID_EDT_LOCATION: usize = 421;
    const ID_BTN_BROWSE: usize = 422;
    const ID_CHK_DESKTOP: usize = 430;
    const ID_CHK_STARTMENU: usize = 431;
    const ID_CHK_PATH: usize = 432;
    const ID_LBL_SMFOLDER: usize = 433;
    const ID_EDT_SMFOLDER: usize = 434;
    const ID_LBL_ASSOC: usize = 435;
    const ID_EDT_ASSOC: usize = 436;
    const ID_LBL_OPTNOTE: usize = 437;
    const ID_CHK_LOGIN: usize = 438;
    // Progress page (page 3).
    const ID_LBL_PROGFILE: usize = 440;
    const ID_PROGRESS: usize = 441;
    // Finish page (page 4).
    const ID_LBL_FINISH: usize = 450;
    const ID_CHK_LAUNCH: usize = 451;
    // Uninstall confirmation window (its own window + state).
    const ID_LBL_CONFIRM_TITLE: usize = 460;
    const ID_LBL_CONFIRM_BODY: usize = 461;
    const ID_CHK_PURGE: usize = 462;
    const ID_LBL_CONFIRM_DIRS: usize = 463;
    const ID_LBL_CONFIRM_WARN: usize = 464;
    const ID_BTN_CONFIRM: usize = 465;
    const ID_BTN_CONFIRM_CANCEL: usize = 466;

    const OPTION_CONTROLS: [usize; 12] = [
        ID_LBL_LOC,
        ID_EDT_LOCATION,
        ID_BTN_BROWSE,
        ID_CHK_DESKTOP,
        ID_CHK_STARTMENU,
        ID_CHK_PATH,
        ID_CHK_LOGIN,
        ID_LBL_SMFOLDER,
        ID_EDT_SMFOLDER,
        ID_LBL_ASSOC,
        ID_EDT_ASSOC,
        ID_LBL_OPTNOTE,
    ];

    // Worker-thread → UI-thread messages (posted, never sent, so the
    // worker never blocks on the UI).
    const WM_APP_PROGRESS: u32 = 0x8000 + 11; // WM_APP + 11
    const WM_APP_DONE: u32 = 0x8000 + 12;

    const VK_ESCAPE_KEY: usize = 0x1B;
    static WIDE_SANS: &[u16] = &[
        b'S' as u16,
        b'e' as u16,
        b'g' as u16,
        b'o' as u16,
        b'e' as u16,
        b' ' as u16,
        b'U' as u16,
        b'I' as u16,
        0,
    ];
    const BM_GETCHECK_MSG: u32 = 0x00F0;
    const BM_SETCHECK_MSG: u32 = 0x00F1;
    const BST_CHECKED_STATE: usize = 1;
    const PBM_SETRANGE32_MSG: u32 = 0x0400 + 6; // WM_USER + 6
    const PBM_SETPOS_MSG: u32 = 0x0400 + 2;

    // Static-control styles (windows-sys keeps these behind the
    // SystemServices feature, which this crate does not enable).
    const SS_CENTER: u32 = 0x0001;
    const SS_RIGHT: u32 = 0x0002;
    const SS_ETCHEDHORZ: u32 = 0x0010;
    const SS_ETCHEDVERT: u32 = 0x0011;
    // Edit / checkbox styles, likewise defined locally.
    const ES_AUTOHSCROLL: u32 = 0x0080;
    const BS_AUTOCHECKBOX: u32 = 0x0003;

    // NM_DBLCLK is (-3); NMHDR.code is unsigned, so compare as i32.
    const NM_DBLCLK_I32: i32 = -3;

    // EN_CHANGE notification code (high word of WM_COMMAND's wparam).
    const EN_CHANGE_CODE: usize = 0x0300;

    const READY_INFO_TEXT: &str = "Ready to install. Nothing is downloaded and nothing is sent anywhere — this installer works fully offline and can be undone from Windows Settings → Apps.";
    const UNWRITABLE_INFO_TEXT: &str = "That install folder needs administrator access, and this installer runs per user — it cannot use it. Switch to the per-user folder with the Install location row's Change action (no administrator rights needed), or choose a different location on the Options page.";

    struct WizardState {
        manifest: OverlayManifestHeader,
        records: Vec<FileRecordHeader>,
        options: InstallOptions,
        exe_path: PathBuf,
        install_dir: PathBuf,
        /// Latest writability pre-flight verdict for `install_dir`
        /// (see `refresh_location_state`); starts optimistic and is
        /// recomputed whenever the Review/Options pages are shown or
        /// the location changes.
        location_ok: bool,
        /// Current page: 1 Review, 2 Options, 3 Progress, 4 Finish.
        page: u8,
        installing: bool,
        installed: bool,
        /// Set by the progress page's Cancel button; the install
        /// operation polls it between files and rolls back when set.
        cancel: Arc<AtomicBool>,
        /// Latest worker progress (files done, file count, current file).
        progress: Arc<Mutex<(usize, usize, String)>>,
        /// The finished install's outcome, handed from the worker thread
        /// to the UI thread (taken when the Finish page is shown).
        finish: Option<Result<InstallOutcome, String>>,
        /// The primary executable's installed path, set when the Finish
        /// page is armed with a working "Launch <App>" checkbox (the
        /// package offers it and the install succeeded).
        launch_path: Option<PathBuf>,
    }

    static STATE: Mutex<Option<WizardState>> = Mutex::new(None);
    static MAIN_HWND: Mutex<usize> = Mutex::new(0);
    static mut BG_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut SURFACE_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut GREEN_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut RAIL_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut DEEP_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut HEADER_FONT: HFONT = std::ptr::null_mut();
    static mut TILE_FONT: HFONT = std::ptr::null_mut();
    static mut NAME_FONT: HFONT = std::ptr::null_mut();

    pub fn run(
        manifest: OverlayManifestHeader,
        records: Vec<FileRecordHeader>,
        options: InstallOptions,
        exe_path: PathBuf,
    ) -> Result<(), u32> {
        let install_dir = default_install_dir(&options, &manifest.app_name());
        if let Ok(mut guard) = STATE.lock() {
            *guard = Some(WizardState {
                manifest,
                records,
                options,
                exe_path,
                install_dir,
                location_ok: true,
                page: 1,
                installing: false,
                installed: false,
                cancel: Arc::new(AtomicBool::new(false)),
                progress: Arc::new(Mutex::new((0, 0, String::new()))),
                finish: None,
                launch_path: None,
            });
        }
        unsafe {
            let init = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_LISTVIEW_CLASSES | ICC_PROGRESS_CLASS,
            };
            InitCommonControlsEx(&init);

            let instance =
                windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
            let class_name: Vec<u16> = "SeanPakWizardWindowClass\0".encode_utf16().collect();
            let app_name = STATE
                .lock()
                .ok()
                .and_then(|g| g.as_ref().map(|s| s.manifest.app_name()))
                .unwrap_or_else(|| "App".to_string());
            let window_title: Vec<u16> = format!("{app_name} Setup — SeanPak\0")
                .encode_utf16()
                .collect();

            BG_BRUSH = CreateSolidBrush(COLOR_BG);
            SURFACE_BRUSH = CreateSolidBrush(COLOR_SURFACE);
            GREEN_BRUSH = CreateSolidBrush(COLOR_GREEN);
            RAIL_BRUSH = CreateSolidBrush(COLOR_RAIL);
            DEEP_BRUSH = CreateSolidBrush(COLOR_DEEP);
            NAME_FONT = CreateFontW(
                -15,
                0,
                0,
                0,
                FW_SEMIBOLD as i32,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                WIDE_SANS.as_ptr(),
            );
            HEADER_FONT = CreateFontW(
                -20,
                0,
                0,
                0,
                FW_SEMIBOLD as i32,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                WIDE_SANS.as_ptr(),
            );
            TILE_FONT = CreateFontW(
                -26,
                0,
                0,
                0,
                FW_BOLD as i32,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                WIDE_SANS.as_ptr(),
            );

            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: LoadIconW(std::ptr::null_mut(), IDI_APPLICATION),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: BG_BRUSH,
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            if RegisterClassExW(&wc) == 0 {
                let err = GetLastError();
                if err != 1410 {
                    // 1410 = class already registered: not fatal.
                    crate::diag::log("wizard", &format!("RegisterClassExW failed: error {err}"));
                    return Err(err);
                }
            }
            crate::diag::log("wizard", "window class registered");

            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                window_title.as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                880,
                580,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                let err = GetLastError();
                crate::diag::log("wizard", &format!("CreateWindowExW failed: error {err}"));
                return Err(err);
            }
            crate::diag::log("wizard", "window created");
            if let Ok(mut g) = MAIN_HWND.lock() {
                *g = hwnd as usize;
            }

            create_controls(hwnd, instance);
            ShowWindow(hwnd, SW_SHOWDEFAULT);
            UpdateWindow(hwnd);

            crate::diag::log("wizard", "message loop entered");
            let mut msg: MSG = std::mem::zeroed();
            loop {
                let result = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if result == 0 {
                    break;
                }
                if result == -1 {
                    let err = GetLastError();
                    crate::diag::log("wizard", &format!("GetMessageW failed: error {err}"));
                    return Err(err);
                }
                // IsDialogMessageW gives the pages sane Tab order and
                // Enter/Esc handling; unhandled keys fall through.
                if IsDialogMessageW(hwnd, &msg) == 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            crate::diag::log("wizard", "message loop exited cleanly");
            Ok(())
        }
    }

    unsafe fn create_controls(parent: HWND, instance: HINSTANCE) {
        let static_class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let button_class: Vec<u16> = "BUTTON\0".encode_utf16().collect();
        let list_class: Vec<u16> = "SysListView32\0".encode_utf16().collect();
        let edit_class: Vec<u16> = "EDIT\0".encode_utf16().collect();
        let progress_class: Vec<u16> = "msctls_progress32\0".encode_utf16().collect();

        let (app_name, app_version, app_publisher) = STATE
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref().map(|s| {
                    (
                        s.manifest.app_name(),
                        s.manifest.app_version(),
                        s.manifest.publisher(),
                    )
                })
            })
            .unwrap_or_else(|| ("App".to_string(), String::new(), String::new()));

        let mk =
            |class: &[u16], text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: usize| {
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                unsafe {
                    CreateWindowExW(
                        0,
                        class.as_ptr(),
                        wide.as_ptr(),
                        WS_CHILD | WS_VISIBLE | style,
                        x,
                        y,
                        w,
                        h,
                        parent,
                        id as HMENU,
                        instance,
                        std::ptr::null_mut(),
                    )
                }
            };

        // The rail (Design E): a fixed left panel in the darkest
        // surface color. Its background is an empty static painted
        // with the rail brush (see WM_CTLCOLORSTATIC); everything else
        // on the rail is a static laid over it. The rail carries the
        // app's identity, the step list, and the disk figures — the
        // content area to its right holds the pages.
        mk(&static_class, "", 0, 0, 0, RAIL_W, 542, ID_RAIL_BG);
        mk(&static_class, "", SS_ETCHEDVERT, RAIL_W - 1, 0, 2, 542, 0);

        // Identity: the packaged app's monogram tile (app-green), then
        // the app name, version, and publisher. No external assets —
        // the tile is a static whose initial is drawn from the name.
        let monogram = app_name
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect::<String>())
            .unwrap_or_else(|| "?".to_string());
        let tile = mk(&static_class, &monogram, SS_CENTER, 20, 16, 48, 48, ID_TILE);
        SendMessageW(tile, WM_SETFONT, TILE_FONT as WPARAM, 1);
        let name_lbl = mk(&static_class, &app_name, 0, 20, 76, 118, 22, ID_LBL_APPNAME);
        SendMessageW(name_lbl, WM_SETFONT, NAME_FONT as WPARAM, 1);
        mk(
            &static_class,
            &format!("Version {app_version}"),
            0,
            20,
            100,
            118,
            18,
            ID_LBL_APPVER,
        );
        mk(
            &static_class,
            &app_publisher,
            0,
            20,
            118,
            118,
            18,
            ID_LBL_APPPUB,
        );

        // Steps: an accent bar at the rail's left edge (green only for
        // the current step), the step's number — a green check once
        // the step is complete — and its name. Texts/colors refresh in
        // show_page / WM_CTLCOLORSTATIC from the state's page.
        for i in 0..4 {
            let y = 158 + (i as i32) * 40;
            mk(&static_class, "", 0, 0, y + 2, 4, 26, ID_STEP_ACCENTS[i]);
            mk(
                &static_class,
                &(i + 1).to_string(),
                SS_CENTER,
                16,
                y + 3,
                24,
                24,
                ID_STEP_NUMS[i],
            );
            mk(
                &static_class,
                STEP_NAMES[i],
                0,
                48,
                y + 5,
                96,
                20,
                ID_STEP_NAMES[i],
            );
        }

        // Disk figures, pinned to the rail's bottom.
        mk(&static_class, "", SS_ETCHEDHORZ, 16, 452, 118, 2, 0);
        mk(
            &static_class,
            "DISK SPACE",
            0,
            16,
            462,
            118,
            14,
            ID_LBL_DISKCAP,
        );
        mk(&static_class, "", 0, 16, 480, 118, 18, ID_LBL_DISKNEED);
        mk(&static_class, "", 0, 16, 500, 118, 18, ID_LBL_DISKFREE);
        set_disk_labels(parent);

        // Content header: page title left, step counter right, a
        // subtitle under the title, then the separator.
        let header = mk(&static_class, "Review", 0, 174, 14, 360, 26, ID_LBL_HEADER);
        SendMessageW(header, WM_SETFONT, HEADER_FONT as WPARAM, 1);
        mk(
            &static_class,
            "Step 1 of 4",
            SS_RIGHT,
            548,
            18,
            300,
            22,
            ID_LBL_STEP,
        );
        mk(&static_class, "", 0, 174, 44, 674, 20, ID_LBL_SUBTITLE);
        mk(&static_class, "", SS_ETCHEDHORZ, 174, 70, 674, 2, 0);

        let list = mk(
            &list_class,
            "",
            WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL,
            174,
            82,
            674,
            272,
            ID_LIST,
        );
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            (LVS_EX_GRIDLINES | LVS_EX_FULLROWSELECT) as WPARAM,
            (LVS_EX_GRIDLINES | LVS_EX_FULLROWSELECT) as LPARAM,
        );
        SendMessageW(list, LVM_SETBKCOLOR, 0, COLOR_SURFACE as LPARAM);
        SendMessageW(list, LVM_SETTEXTBKCOLOR, 0, COLOR_SURFACE as LPARAM);
        SendMessageW(list, LVM_SETTEXTCOLOR, 0, COLOR_FG as LPARAM);
        fill_list(list);

        mk(
            &static_class,
            READY_INFO_TEXT,
            0,
            174,
            366,
            674,
            46,
            ID_LBL_INFO,
        );
        mk(&static_class, "", 0, 174, 458, 674, 22, ID_LBL_STATUS);
        mk(&static_class, "", SS_ETCHEDHORZ, RAIL_W, 486, 714, 2, 0);
        mk(
            &static_class,
            "↑↓ navigate · Enter edit · Ctrl+K palette",
            0,
            174,
            502,
            420,
            20,
            ID_LBL_HINT,
        );
        mk(
            &button_class,
            "Back",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            448,
            496,
            120,
            30,
            ID_BTN_BACK,
        );
        mk(
            &button_class,
            "Cancel",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            580,
            496,
            120,
            30,
            ID_BTN_CANCEL,
        );
        mk(
            &button_class,
            "Install",
            WS_TABSTOP | (BS_DEFPUSHBUTTON as u32),
            712,
            496,
            136,
            30,
            ID_BTN_INSTALL,
        );

        // ---- Page 2: Options (hidden until shown) ----
        mk(
            &static_class,
            "Install location",
            0,
            174,
            88,
            400,
            20,
            ID_LBL_LOC,
        );
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
            174,
            112,
            548,
            26,
            ID_EDT_LOCATION,
        );
        mk(
            &button_class,
            "Browse…",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            730,
            112,
            118,
            26,
            ID_BTN_BROWSE,
        );
        mk(
            &button_class,
            "Desktop shortcut",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            174,
            154,
            500,
            24,
            ID_CHK_DESKTOP,
        );
        mk(
            &button_class,
            "Start Menu shortcuts",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            174,
            184,
            500,
            24,
            ID_CHK_STARTMENU,
        );
        mk(
            &button_class,
            "Add the install folder to PATH",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            174,
            214,
            500,
            24,
            ID_CHK_PATH,
        );
        // Shown (and labelled with the app name) only when the package
        // offers the start-at-login option — see show_page/fill.
        mk(
            &button_class,
            "Start at login",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            174,
            244,
            500,
            24,
            ID_CHK_LOGIN,
        );
        mk(
            &static_class,
            "Start Menu folder",
            0,
            174,
            280,
            400,
            20,
            ID_LBL_SMFOLDER,
        );
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
            174,
            304,
            420,
            26,
            ID_EDT_SMFOLDER,
        );
        mk(
            &static_class,
            "File associations (comma-separated, for example .txt, .md)",
            0,
            174,
            340,
            674,
            20,
            ID_LBL_ASSOC,
        );
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
            174,
            364,
            420,
            26,
            ID_EDT_ASSOC,
        );
        mk(
            &static_class,
            "Shortcuts, associations, the PATH entry, and the login entry are created for your user account only — no administrator rights needed — and uninstall removes exactly what install created.",
            0, 174, 404, 674, 48, ID_LBL_OPTNOTE,
        );

        // ---- Page 3: Progress ----
        mk(
            &static_class,
            "Waiting to start…",
            0,
            174,
            196,
            674,
            22,
            ID_LBL_PROGFILE,
        );
        mk(
            &progress_class,
            "",
            WS_TABSTOP,
            174,
            228,
            674,
            28,
            ID_PROGRESS,
        );

        // ---- Page 4: Finish ----
        mk(&static_class, "", 0, 174, 82, 674, 330, ID_LBL_FINISH);
        // The launch checkbox sits below the Finish text; its label
        // and state are set when a successful install arms it (and it
        // stays hidden otherwise) — see on_done_message / show_page.
        mk(
            &button_class,
            "Launch",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            174,
            424,
            674,
            24,
            ID_CHK_LAUNCH,
        );

        show_page(parent, 1);
    }

    /// Shows one wizard page (1 Review, 2 Options, 3 Progress,
    /// 4 Finish): control visibility, step label, and footer buttons.
    unsafe fn show_page(hwnd: HWND, page: u8) {
        if let Ok(mut g) = STATE.lock() {
            if let Some(s) = g.as_mut() {
                s.page = page;
            }
        }
        let show = |id: usize, visible: bool| {
            let ctrl = GetDlgItem(hwnd, id as i32);
            if !ctrl.is_null() {
                ShowWindow(ctrl, if visible { SW_SHOW } else { SW_HIDE });
            }
        };
        // Review group.
        show(ID_LIST, page == 1);
        show(ID_LBL_INFO, page == 1);
        // Options group.
        for id in OPTION_CONTROLS {
            show(id, page == 2);
        }
        // The login checkbox joins page 2 only when the package offers
        // the option.
        let login_offered = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.options.offers_start_at_login()))
            .unwrap_or(false);
        show(ID_CHK_LOGIN, page == 2 && login_offered);
        // Progress group.
        show(ID_LBL_PROGFILE, page == 3);
        show(ID_PROGRESS, page == 3);
        // Finish group.
        show(ID_LBL_FINISH, page == 4);
        // The launch checkbox joins page 4 only when a successful
        // install armed it.
        let launch_armed = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.launch_path.is_some()))
            .unwrap_or(false);
        show(ID_CHK_LAUNCH, page == 4 && launch_armed);
        // The status line serves pages 1–2 (transient messages).
        show(ID_LBL_STATUS, page == 1 || page == 2);
        // Footer buttons per page.
        show(ID_BTN_BACK, page == 2);
        show(ID_BTN_CANCEL, page == 1 || page == 3);
        show(ID_BTN_INSTALL, page != 3);
        set_text(
            hwnd,
            ID_BTN_CANCEL,
            if page == 3 {
                "Cancel install"
            } else {
                "Cancel"
            },
        );
        set_text(
            hwnd,
            ID_BTN_INSTALL,
            if page == 4 { "Close" } else { "Install" },
        );
        set_text(
            hwnd,
            ID_LBL_STEP,
            match page {
                2 => "Step 2 of 4",
                3 => "Step 3 of 4",
                4 => "Step 4 of 4",
                _ => "Step 1 of 4",
            },
        );
        set_text(
            hwnd,
            ID_LBL_HEADER,
            match page {
                2 => "Options",
                3 => "Installing",
                4 => "Finish",
                _ => "Review",
            },
        );
        set_text(
            hwnd,
            ID_LBL_SUBTITLE,
            match page {
                2 => "Change any option — install uses exactly these choices.",
                3 => "Installing — cancelling rolls back every change.",
                4 => "The install has finished.",
                _ => "Check the install options — change anything before you install.",
            },
        );
        update_rail(hwnd, page);
        if page == 2 {
            fill_options_controls(hwnd);
        }
        if page == 1 || page == 2 {
            refresh_location_state(hwnd);
        }
        let focus_id = match page {
            1 => ID_LIST,
            2 => ID_EDT_LOCATION,
            4 => ID_BTN_INSTALL,
            _ => ID_BTN_CANCEL,
        };
        let focus = GetDlgItem(hwnd, focus_id as i32);
        if !focus.is_null() {
            SetFocus(focus);
        }
    }

    /// Refreshes the rail's step list for `page`: steps before it get
    /// a green check in place of their number, the current step keeps
    /// its number (its accent bar and green fill come from
    /// WM_CTLCOLORSTATIC), later steps stay numbered and dim. The
    /// statics are invalidated so the color handler repaints them.
    unsafe fn update_rail(hwnd: HWND, page: u8) {
        for i in 0..4usize {
            let step = (i + 1) as u8;
            if page > step {
                set_text(hwnd, ID_STEP_NUMS[i], "\u{2713}");
            } else {
                set_text(hwnd, ID_STEP_NUMS[i], &step.to_string());
            }
            for id in [ID_STEP_ACCENTS[i], ID_STEP_NUMS[i], ID_STEP_NAMES[i]] {
                let ctrl = GetDlgItem(hwnd, id as i32);
                if !ctrl.is_null() {
                    InvalidateRect(ctrl, std::ptr::null(), 1);
                }
            }
        }
    }

    /// Refreshes the rail's disk block: what the package needs, and
    /// what the current install location's drive reports free. Called
    /// at creation and whenever the location verdict refreshes (the
    /// free figure follows the chosen folder's drive).
    unsafe fn set_disk_labels(hwnd: HWND) {
        let snapshot = STATE.lock().ok().and_then(|g| {
            g.as_ref()
                .map(|s| (s.manifest.total_uncompressed_bytes, s.install_dir.clone()))
        });
        let Some((needed, dir)) = snapshot else {
            return;
        };
        set_text(
            hwnd,
            ID_LBL_DISKNEED,
            &format!("{} needed", format_bytes(needed)),
        );
        let free_text = match free_bytes_for(&dir) {
            Some(free) => format!("{} free", format_bytes(free)),
            None => String::new(),
        };
        set_text(hwnd, ID_LBL_DISKFREE, &free_text);
    }

    /// Runs the shared writability verdict for the current install
    /// directory and brings the page furniture in line with it: the
    /// state flag (a Review list refill when the verdict changed, so
    /// the location row's marker and Change action update), the
    /// explanation lines, and the Install button — disabled while the
    /// location cannot be used, so an install never starts into a
    /// folder that would only fail midway. (The operation's own
    /// pre-flight remains the backstop if permissions change between
    /// pages.) Called when the Review/Options pages are shown and
    /// whenever the location changes.
    unsafe fn refresh_location_state(hwnd: HWND) {
        let snapshot = STATE.lock().ok().and_then(|g| {
            g.as_ref().map(|s| {
                (
                    s.install_dir.clone(),
                    s.manifest.app_name(),
                    s.options.install_location(),
                    s.location_ok,
                    s.page,
                )
            })
        });
        let Some((dir, app_name, recorded, was_ok, page)) = snapshot else {
            return;
        };
        let (ok, fallback) = match crate::install::location_verdict(&dir, &app_name, &recorded) {
            crate::install::LocationVerdict::Writable => (true, None),
            crate::install::LocationVerdict::NotWritable { fallback, .. } => {
                (false, Some(fallback))
            }
        };
        if let Ok(mut g) = STATE.lock() {
            if let Some(s) = g.as_mut() {
                s.location_ok = ok;
            }
        }
        if ok != was_ok && page == 1 {
            let list = GetDlgItem(hwnd, ID_LIST as i32);
            if !list.is_null() {
                fill_list(list);
            }
        }
        if ok {
            set_text(hwnd, ID_LBL_INFO, READY_INFO_TEXT);
            set_text(hwnd, ID_LBL_STATUS, "");
        } else if let Some(fallback) = &fallback {
            set_text(hwnd, ID_LBL_INFO, UNWRITABLE_INFO_TEXT);
            set_text(
                hwnd,
                ID_LBL_STATUS,
                &format!("Per-user folder: {}", fallback.display()),
            );
        }
        // On the Finish page the same button is "Close" and must stay
        // enabled; on Progress it is hidden. Elsewhere it follows the
        // verdict.
        set_enabled(hwnd, ID_BTN_INSTALL, ok || page >= 3);
        // The rail's free-space figure follows the location's drive.
        set_disk_labels(hwnd);
    }

    /// The Options page location edit changed: keep the state's install
    /// directory in sync and re-run the verdict, so the explanation and
    /// the Install button track what the user is typing.
    unsafe fn on_location_edited(hwnd: HWND) {
        let text = get_text(hwnd, ID_EDT_LOCATION);
        if let Ok(mut g) = STATE.lock() {
            if let Some(s) = g.as_mut() {
                let trimmed = text.trim();
                if !trimmed.is_empty() {
                    s.install_dir = PathBuf::from(trimmed);
                }
            }
        }
        refresh_location_state(hwnd);
    }

    /// Writes fixed-size UTF-8 into an install-options field.
    fn set_fixed_str(field: &mut [u8], value: &str) {
        for b in field.iter_mut() {
            *b = 0;
        }
        let bytes = value.as_bytes();
        let len = bytes.len().min(field.len().saturating_sub(1));
        field[..len].copy_from_slice(&bytes[..len]);
    }

    unsafe fn get_text(hwnd: HWND, id: usize) -> String {
        let ctrl = GetDlgItem(hwnd, id as i32);
        if ctrl.is_null() {
            return String::new();
        }
        let len = GetWindowTextLengthW(ctrl);
        if len <= 0 {
            return String::new();
        }
        let mut buf = vec![0u16; len as usize + 1];
        let got = GetWindowTextW(ctrl, buf.as_mut_ptr(), buf.len() as i32);
        if got <= 0 {
            return String::new();
        }
        buf.truncate(got as usize);
        String::from_utf16_lossy(&buf)
    }

    unsafe fn set_check(hwnd: HWND, id: usize, on: bool) {
        let ctrl = GetDlgItem(hwnd, id as i32);
        if !ctrl.is_null() {
            SendMessageW(
                ctrl,
                BM_SETCHECK_MSG,
                if on { BST_CHECKED_STATE } else { 0 },
                0,
            );
        }
    }

    unsafe fn get_check(hwnd: HWND, id: usize) -> bool {
        let ctrl = GetDlgItem(hwnd, id as i32);
        !ctrl.is_null() && SendMessageW(ctrl, BM_GETCHECK_MSG, 0, 0) as usize == BST_CHECKED_STATE
    }

    /// Page 2 controls ← current state (called when the page opens).
    unsafe fn fill_options_controls(hwnd: HWND) {
        let snapshot = STATE.lock().ok().and_then(|g| {
            g.as_ref()
                .map(|s| (s.install_dir.clone(), s.options, s.manifest.app_name()))
        });
        let Some((dir, options, app_name)) = snapshot else {
            return;
        };
        set_text(hwnd, ID_EDT_LOCATION, &dir.to_string_lossy());
        set_check(hwnd, ID_CHK_DESKTOP, options.wants_desktop_shortcut());
        set_check(hwnd, ID_CHK_STARTMENU, options.wants_start_menu_shortcut());
        set_check(hwnd, ID_CHK_PATH, options.wants_path_entry());
        set_text(hwnd, ID_CHK_LOGIN, &format!("Start {app_name} at login"));
        set_check(hwnd, ID_CHK_LOGIN, options.start_at_login_default_checked());
        set_text(hwnd, ID_EDT_SMFOLDER, &options.start_menu_folder());
        set_text(hwnd, ID_EDT_ASSOC, &options.file_associations());
    }

    /// Page 2 controls → state (called when leaving the page to
    /// install, so the shared operation receives the edited options).
    unsafe fn harvest_options(hwnd: HWND) {
        let location = get_text(hwnd, ID_EDT_LOCATION);
        let desktop = get_check(hwnd, ID_CHK_DESKTOP);
        let start_menu = get_check(hwnd, ID_CHK_STARTMENU);
        let path_entry = get_check(hwnd, ID_CHK_PATH);
        let login = get_check(hwnd, ID_CHK_LOGIN);
        let sm_folder = get_text(hwnd, ID_EDT_SMFOLDER);
        let associations = get_text(hwnd, ID_EDT_ASSOC);
        if let Ok(mut g) = STATE.lock() {
            if let Some(s) = g.as_mut() {
                if !location.trim().is_empty() {
                    s.install_dir = PathBuf::from(location.trim());
                }
                s.options.desktop_shortcut = desktop as u8;
                s.options.start_menu_shortcut = start_menu as u8;
                s.options.path_entry = path_entry as u8;
                // The login choice edits the option's default in place
                // (like the other toggles), so the shared operation
                // sees the user's final choice. Only when offered —
                // the checkbox is hidden otherwise and its state is
                // meaningless.
                if s.options.offers_start_at_login() {
                    s.options.start_at_login_default = login as u8;
                }
                set_fixed_str(&mut s.options.start_menu_folder_utf8, sm_folder.trim());
                set_fixed_str(&mut s.options.file_associations_utf8, associations.trim());
            }
        }
    }

    /// (Re)builds the review rows in the list from the current state.
    unsafe fn fill_list(list: HWND) {
        // Columns are created once; on refresh only the items are rebuilt.
        static COLUMNS_DONE: Mutex<bool> = Mutex::new(false);
        let mut columns_done = COLUMNS_DONE.lock().map(|g| *g).unwrap_or(false);
        if !columns_done {
            for (idx, (title, width)) in [("Item", 210), ("Value", 320), ("Change", 124)]
                .iter()
                .enumerate()
            {
                let mut wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
                let col = LVCOLUMNW {
                    mask: LVCF_TEXT | LVCF_WIDTH,
                    fmt: 0,
                    cx: *width,
                    pszText: wide.as_mut_ptr(),
                    cchTextMax: 0,
                    iSubItem: idx as i32,
                    iImage: 0,
                    iOrder: idx as i32,
                    cxMin: 0,
                    cxDefault: 0,
                    cxIdeal: 0,
                };
                SendMessageW(list, LVM_INSERTCOLUMNW, idx, &col as *const _ as LPARAM);
            }
            if let Ok(mut g) = COLUMNS_DONE.lock() {
                *g = true;
            }
            columns_done = true;
        }
        let _ = columns_done;
        SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);

        let rows = STATE.lock().ok().and_then(|g| {
            g.as_ref().map(|s| {
                review_rows(
                    &s.manifest,
                    &s.options,
                    &s.records,
                    &s.install_dir.to_string_lossy(),
                    s.location_ok,
                )
            })
        });
        let rows = match rows {
            Some(r) => r,
            None => return,
        };
        for (row_idx, row) in rows.iter().enumerate() {
            for (col_idx, cell) in row.iter().enumerate() {
                let mut wide: Vec<u16> = cell.encode_utf16().chain(std::iter::once(0)).collect();
                let item = LVITEMW {
                    mask: LVIF_TEXT,
                    iItem: row_idx as i32,
                    iSubItem: col_idx as i32,
                    state: 0,
                    stateMask: 0,
                    pszText: wide.as_mut_ptr(),
                    cchTextMax: 0,
                    iImage: 0,
                    lParam: 0,
                    iIndent: 0,
                    iGroupId: 0,
                    cColumns: 0,
                    puColumns: std::ptr::null_mut(),
                    piColFmt: std::ptr::null_mut(),
                    iGroup: 0,
                };
                if col_idx == 0 {
                    SendMessageW(list, LVM_INSERTITEMW, 0, &item as *const _ as LPARAM);
                } else {
                    SendMessageW(list, LVM_SETITEMTEXTW, row_idx, &item as *const _ as LPARAM);
                }
            }
        }
    }

    fn free_bytes_for(path: &Path) -> Option<u64> {
        let mut probe = path.to_path_buf();
        loop {
            if probe.exists() {
                break;
            }
            if !probe.pop() {
                return None;
            }
        }
        use std::os::windows::ffi::OsStrExt;
        let wide: Vec<u16> = probe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut free_avail: u64 = 0;
        let mut total: u64 = 0;
        let mut total_free: u64 = 0;
        let ok = unsafe {
            GetDiskFreeSpaceExW(wide.as_ptr(), &mut free_avail, &mut total, &mut total_free)
        };
        if ok != 0 {
            Some(total_free)
        } else {
            None
        }
    }

    unsafe fn set_text(hwnd: HWND, id: usize, text: &str) {
        let ctrl = GetDlgItem(hwnd, id as i32);
        if !ctrl.is_null() {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            SetWindowTextW(ctrl, wide.as_ptr());
        }
    }

    unsafe fn set_enabled(hwnd: HWND, id: usize, enabled: bool) {
        let ctrl = GetDlgItem(hwnd, id as i32);
        if !ctrl.is_null() {
            EnableWindow(ctrl, if enabled { 1 } else { 0 });
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CTLCOLORSTATIC => {
                let hdc = wparam as HDC;
                let ctrl = lparam as HWND;
                let id = GetWindowLongW(ctrl, GWL_ID) as usize;
                let (installed, page) = STATE
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().map(|s| (s.installed, s.page)))
                    .unwrap_or((false, 1));
                if id == ID_TILE {
                    // The monogram tile: app-green block, light initial.
                    SetBkMode(hdc, OPAQUE as i32);
                    SetBkColor(hdc, COLOR_GREEN);
                    SetTextColor(hdc, COLOR_FG);
                    return GREEN_BRUSH as LRESULT;
                }
                if id == ID_RAIL_BG {
                    // The rail's own background: solid darkest panel.
                    SetBkMode(hdc, OPAQUE as i32);
                    SetBkColor(hdc, COLOR_RAIL);
                    return RAIL_BRUSH as LRESULT;
                }
                if let Some(i) = ID_STEP_ACCENTS.iter().position(|a| *a == id) {
                    // Accent bar: green for the current step, rail
                    // color (invisible) for the rest.
                    SetBkMode(hdc, OPAQUE as i32);
                    if page == (i + 1) as u8 {
                        SetBkColor(hdc, COLOR_GREEN);
                        return GREEN_BRUSH as LRESULT;
                    }
                    SetBkColor(hdc, COLOR_RAIL);
                    return RAIL_BRUSH as LRESULT;
                }
                if let Some(i) = ID_STEP_NUMS.iter().position(|a| *a == id) {
                    // Step number: green-filled while current, a green
                    // check once complete, dim while still ahead.
                    let step = (i + 1) as u8;
                    if page == step {
                        SetBkMode(hdc, OPAQUE as i32);
                        SetBkColor(hdc, COLOR_GREEN);
                        SetTextColor(hdc, COLOR_FG);
                        return GREEN_BRUSH as LRESULT;
                    }
                    SetBkMode(hdc, TRANSPARENT as i32);
                    SetTextColor(hdc, if page > step { COLOR_OK } else { COLOR_MUTED });
                    return RAIL_BRUSH as LRESULT;
                }
                if let Some(i) = ID_STEP_NAMES.iter().position(|a| *a == id) {
                    // Step name: bright for the current and completed
                    // steps, dim for the ones still ahead.
                    SetBkMode(hdc, TRANSPARENT as i32);
                    SetTextColor(
                        hdc,
                        if page >= (i + 1) as u8 {
                            COLOR_FG
                        } else {
                            COLOR_MUTED
                        },
                    );
                    return RAIL_BRUSH as LRESULT;
                }
                if id == ID_LBL_APPNAME || id == ID_LBL_DISKNEED {
                    SetBkMode(hdc, TRANSPARENT as i32);
                    SetTextColor(hdc, COLOR_FG);
                    return RAIL_BRUSH as LRESULT;
                }
                if id == ID_LBL_APPVER
                    || id == ID_LBL_APPPUB
                    || id == ID_LBL_DISKCAP
                    || id == ID_LBL_DISKFREE
                {
                    SetBkMode(hdc, TRANSPARENT as i32);
                    SetTextColor(hdc, COLOR_MUTED);
                    return RAIL_BRUSH as LRESULT;
                }
                if id == ID_LBL_INFO {
                    // The info box: light text on the deep-blue fill.
                    SetBkMode(hdc, OPAQUE as i32);
                    SetBkColor(hdc, COLOR_DEEP);
                    SetTextColor(hdc, COLOR_FG);
                    return DEEP_BRUSH as LRESULT;
                }
                SetBkMode(hdc, TRANSPARENT as i32);
                if (id == ID_LBL_STATUS || id == ID_LBL_FINISH) && installed {
                    // Success green, always paired with the status text.
                    SetTextColor(hdc, COLOR_GREEN);
                } else if id == ID_LBL_STEP
                    || id == ID_LBL_HINT
                    || id == ID_LBL_STATUS
                    || id == ID_LBL_SUBTITLE
                {
                    SetTextColor(hdc, COLOR_MUTED);
                } else if id == ID_LBL_OPTNOTE {
                    SetTextColor(hdc, COLOR_BLUE);
                } else {
                    SetTextColor(hdc, COLOR_FG);
                }
                BG_BRUSH as LRESULT
            }
            WM_COMMAND => {
                let control_id = wparam & 0xFFFF;
                let notify_code = (wparam >> 16) & 0xFFFF;
                if control_id == ID_EDT_LOCATION && notify_code == EN_CHANGE_CODE {
                    on_location_edited(hwnd);
                }
                match control_id {
                    ID_BTN_CANCEL => on_cancel_click(hwnd),
                    ID_BTN_BACK => {
                        harvest_options(hwnd);
                        let list = GetDlgItem(hwnd, ID_LIST as i32);
                        fill_list(list);
                        show_page(hwnd, 1);
                    }
                    ID_BTN_BROWSE => {
                        if let Some(folder) = crate::native_dialog::pick_folder() {
                            set_text(hwnd, ID_EDT_LOCATION, &folder.to_string_lossy());
                        }
                    }
                    ID_BTN_INSTALL => on_install_click(hwnd),
                    _ => {}
                }
                0
            }
            WM_NOTIFY => {
                let hdr = lparam as *const NMHDR;
                if !hdr.is_null() && (*hdr).idFrom == ID_LIST && (*hdr).code as i32 == NM_DBLCLK_I32
                {
                    on_row_activate(hwnd);
                }
                0
            }
            m if m == WM_APP_PROGRESS => {
                on_progress_message(hwnd);
                0
            }
            m if m == WM_APP_DONE => {
                on_done_message(hwnd);
                0
            }
            WM_KEYDOWN => {
                if wparam == VK_ESCAPE_KEY {
                    on_cancel_click(hwnd);
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    /// Cancel means different things per page: close the wizard
    /// (Review), or ask the running install to stop and roll back
    /// (Progress). On the Finish page there is no Cancel button.
    unsafe fn on_cancel_click(hwnd: HWND) {
        let (page, cancel) = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| (s.page, s.cancel.clone())))
            .unwrap_or((1, Arc::new(AtomicBool::new(false))));
        match page {
            3 => {
                cancel.store(true, Ordering::SeqCst);
                set_enabled(hwnd, ID_BTN_CANCEL, false);
                set_text(
                    hwnd,
                    ID_LBL_PROGFILE,
                    "Cancelling — rolling back every change…",
                );
            }
            2 => {
                harvest_options(hwnd);
                let list = GetDlgItem(hwnd, ID_LIST as i32);
                fill_list(list);
                show_page(hwnd, 1);
            }
            _ => {
                DestroyWindow(hwnd);
            }
        }
    }

    /// Progress message from the worker: refresh the bar + file label.
    /// The shared progress slot holds (current index, file count,
    /// current relative path) exactly as the install reported it.
    unsafe fn on_progress_message(hwnd: HWND) {
        let snapshot = STATE.lock().ok().and_then(|g| {
            g.as_ref()
                .and_then(|s| s.progress.lock().ok().map(|p| (p.0, p.1, p.2.clone())))
        });
        let Some((index, count, current)) = snapshot else {
            return;
        };
        let bar = GetDlgItem(hwnd, ID_PROGRESS as i32);
        if !bar.is_null() && count > 0 {
            // Permille of files completed keeps the range small and
            // exact for any package size.
            let pos = (index as i64 * 1000) / count as i64;
            SendMessageW(bar, PBM_SETPOS_MSG, pos as WPARAM, 0);
        }
        if !current.is_empty() {
            set_text(
                hwnd,
                ID_LBL_PROGFILE,
                &format!(
                    "Installing file {} of {} — {current}",
                    (index + 1).min(count.max(1)),
                    count.max(1)
                ),
            );
        }
    }

    /// Completion message from the worker: land on the Finish page.
    unsafe fn on_done_message(hwnd: HWND) {
        let finish = STATE.lock().ok().and_then(|mut g| {
            g.as_mut().and_then(|s| {
                s.installing = false;
                s.finish.take()
            })
        });
        let Some(finish) = finish else {
            return;
        };
        match &finish {
            Ok(outcome) => {
                if let Ok(mut g) = STATE.lock() {
                    if let Some(s) = g.as_mut() {
                        s.installed = true;
                    }
                }
                crate::diag::log(
                    "wizard",
                    &format!(
                        "install ok: {} files to {}",
                        outcome.files_written,
                        outcome.dest.display()
                    ),
                );
                let (app_name, app_version) = STATE
                    .lock()
                    .ok()
                    .and_then(|g| {
                        g.as_ref()
                            .map(|s| (s.manifest.app_name(), s.manifest.app_version()))
                    })
                    .unwrap_or_else(|| ("App".to_string(), String::new()));
                let mut text = format!(
                    "Installed {} {} ({}) to {}.\r\n\r\n{} files written.",
                    app_name,
                    app_version,
                    outcome.mode,
                    outcome.dest.display(),
                    outcome.files_written
                );
                if outcome.restored_files > 0 {
                    text += &format!(
                        " {} existing file(s) were missing or changed and were restored.",
                        outcome.restored_files
                    );
                }
                text += &format!(
                    "\r\nReceipt: {}\r\nShortcuts created: {} · File associations applied: {} · PATH entry: {} · Start at login: {}",
                    outcome.receipt_path.display(),
                    outcome.shortcuts_created,
                    outcome.associations_applied,
                    if outcome.path_entry_added { "added" } else { "not added" },
                    if outcome.start_at_login_applied { "on" } else { "off" },
                );
                text += "\r\n\r\nRemove it any time from Windows Settings \u{2192} Apps, or by running Uninstall.exe in the install folder.";
                set_text(hwnd, ID_LBL_FINISH, &text);
                // Arm the launch checkbox when the package offers it
                // and the install produced the primary executable:
                // labelled for the app, checked per the package's
                // recorded default. Closing with it checked starts the
                // app (see on_install_click).
                let launch = STATE.lock().ok().and_then(|g| {
                    g.as_ref().and_then(|s| {
                        if !s.options.offers_launch_after_install() {
                            return None;
                        }
                        let record = s.records.get(s.manifest.primary_exe_index as usize)?;
                        let rel = record.relative_path();
                        if rel.is_empty() {
                            return None;
                        }
                        Some((
                            outcome.dest.join(rel.replace('\\', "/")),
                            s.options.launch_after_install_default_checked(),
                            s.manifest.app_name(),
                        ))
                    })
                });
                if let Some((path, checked, app)) = launch {
                    if let Ok(mut g) = STATE.lock() {
                        if let Some(s) = g.as_mut() {
                            s.launch_path = Some(path);
                        }
                    }
                    set_text(hwnd, ID_CHK_LAUNCH, &format!("Launch {app}"));
                    set_check(hwnd, ID_CHK_LAUNCH, checked);
                }
            }
            Err(e) => {
                crate::diag::log("wizard", &format!("install failed: {e}"));
                let text = if e.starts_with("Install cancelled") {
                    format!("Install cancelled.\r\n\r\n{e}")
                } else {
                    format!(
                        "Install failed.\r\n\r\n{e}\r\n\r\nEvery change was rolled back; nothing was left half-installed."
                    )
                };
                set_text(hwnd, ID_LBL_FINISH, &text);
            }
        }
        show_page(hwnd, 4);
    }

    unsafe fn on_row_activate(hwnd: HWND) {
        let (busy, installed, page, login_offered) = STATE
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref().map(|s| {
                    (
                        s.installing,
                        s.installed,
                        s.page,
                        s.options.offers_start_at_login(),
                    )
                })
            })
            .unwrap_or((true, true, 1, false));
        if busy || installed || page != 1 {
            return;
        }
        let list = GetDlgItem(hwnd, ID_LIST as i32);
        let selected = SendMessageW(
            list,
            LVM_GETNEXTITEM,
            -1isize as WPARAM,
            LVNI_SELECTED as LPARAM,
        );
        if selected < 0 {
            return;
        }
        // The location row is special while its folder is unwritable:
        // its Change action is the one-click switch to the per-user
        // folder, applied in place (no trip to the Options page).
        if selected == 0 {
            let swap = STATE.lock().ok().and_then(|mut g| {
                g.as_mut().and_then(|s| {
                    if s.location_ok {
                        None
                    } else {
                        let fallback = crate::install::per_user_fallback_dir(
                            &s.manifest.app_name(),
                            &s.options.install_location(),
                        );
                        s.install_dir = fallback.clone();
                        Some(fallback)
                    }
                })
            });
            if let Some(fallback) = swap {
                fill_list(list);
                refresh_location_state(hwnd);
                set_text(
                    hwnd,
                    ID_LBL_STATUS,
                    &format!(
                        "Install location switched to the per-user folder: {}",
                        fallback.display()
                    ),
                );
                return;
            }
        }
        // Rows with a real Change action route to the Options page:
        // 0 location, 2 Start Menu folder, 3 desktop shortcut,
        // 4 associations, 5 PATH, and 6 Start at login when the
        // package offers it. Components (1) is informational.
        let login_row: Option<isize> = if login_offered { Some(6) } else { None };
        match selected {
            0 | 2 | 3 | 4 | 5 => show_page(hwnd, 2),
            sel if Some(sel) == login_row => show_page(hwnd, 2),
            _ => set_text(
                hwnd,
                ID_LBL_STATUS,
                "That row is informational — the package decides its components.",
            ),
        }
    }

    unsafe fn on_install_click(hwnd: HWND) {
        let (page, installed, installing) = match STATE.lock() {
            Ok(g) => match g.as_ref() {
                Some(s) => (s.page, s.installed, s.installing),
                None => return,
            },
            Err(_) => return,
        };
        if page == 4 || installed {
            // The footer button reads "Close" on the Finish page. The
            // launch checkbox is the user's last choice on that page:
            // honor it on the way out. A launch failure is a warning
            // shown in place — the install itself already succeeded,
            // so it never turns into an install failure.
            if page == 4 && get_check(hwnd, ID_CHK_LAUNCH) {
                let target = STATE
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().and_then(|s| s.launch_path.clone()));
                if let Some(path) = target {
                    if let Err(e) = crate::install::launch_program(&path) {
                        set_text(
                            hwnd,
                            ID_LBL_FINISH,
                            &format!(
                                "Installed, but the program could not be started.\r\n\r\n{e}\r\n\r\nYou can start it from the install folder instead."
                            ),
                        );
                        set_check(hwnd, ID_CHK_LAUNCH, false);
                        return;
                    }
                }
            }
            DestroyWindow(hwnd);
            return;
        }
        if installing || page == 3 {
            return;
        }
        if page == 2 {
            // Leaving Options for the install: take the edited values.
            harvest_options(hwnd);
        }
        // Backstop: never start into an unwritable location. The
        // Install button is disabled in that state, but if any path
        // reaches here anyway (permissions changed after the last
        // refresh), land back on the Review page, which explains the
        // problem and offers the per-user folder.
        let snapshot = STATE.lock().ok().and_then(|g| {
            g.as_ref().map(|s| {
                (
                    s.install_dir.clone(),
                    s.manifest.app_name(),
                    s.options.install_location(),
                )
            })
        });
        if let Some((dir, app_name, recorded)) = snapshot {
            if let crate::install::LocationVerdict::NotWritable { .. } =
                crate::install::location_verdict(&dir, &app_name, &recorded)
            {
                show_page(hwnd, 1);
                return;
            }
        }
        start_install(hwnd);
    }

    /// Moves to the Progress page and runs the shared install
    /// operation on a worker thread. Progress and completion are
    /// posted back to the UI thread (WM_APP_PROGRESS / WM_APP_DONE);
    /// the Cancel button sets the shared flag the operation polls.
    unsafe fn start_install(hwnd: HWND) {
        let (exe_path, install_dir, options, cancel, progress) = match STATE.lock() {
            Ok(mut g) => match g.as_mut() {
                Some(s) => {
                    s.installing = true;
                    s.cancel.store(false, Ordering::SeqCst);
                    if let Ok(mut p) = s.progress.lock() {
                        *p = (0, s.records.len(), String::new());
                    }
                    (
                        s.exe_path.clone(),
                        s.install_dir.clone(),
                        s.options,
                        s.cancel.clone(),
                        s.progress.clone(),
                    )
                }
                None => return,
            },
            Err(_) => return,
        };
        show_page(hwnd, 3);
        set_text(hwnd, ID_LBL_PROGFILE, "Preparing…");
        let bar = GetDlgItem(hwnd, ID_PROGRESS as i32);
        if !bar.is_null() {
            SendMessageW(bar, PBM_SETRANGE32_MSG, 0, 1000);
            SendMessageW(bar, PBM_SETPOS_MSG, 0, 0);
        }
        let hwnd_value = hwnd as usize;
        std::thread::spawn(move || {
            let request = crate::install::InstallRequest {
                options_override: Some(options),
                force: false,
                repair: false,
                start_at_login: None,
            };
            let progress_slot = progress.clone();
            let mut on_progress = move |ev: &ProgressEvent| {
                if let Ok(mut p) = progress_slot.lock() {
                    *p = (ev.file_index, ev.file_count, ev.rel_path.clone());
                }
                unsafe {
                    PostMessageW(hwnd_value as HWND, WM_APP_PROGRESS, 0, 0);
                }
            };
            let result = crate::install::perform_install_with(
                &exe_path,
                &install_dir,
                &request,
                Some(&mut on_progress),
                Some(&cancel),
            );
            if let Ok(mut g) = STATE.lock() {
                if let Some(s) = g.as_mut() {
                    s.finish = Some(result);
                }
            }
            unsafe {
                PostMessageW(hwnd_value as HWND, WM_APP_DONE, 0, 0);
            }
        });
    }

    // ---- Uninstall confirmation window --------------------------------
    //
    // A small standalone window in the wizard's palette and language
    // (its own class and state — the wizard's STATE belongs to
    // installs). See [`super::confirm_uninstall`] for the contract.

    struct ConfirmState {
        app_name: String,
        data_dirs: Vec<String>,
        /// None until the user confirms; Cancel/close leaves it None.
        result: Option<bool>,
    }

    static CONFIRM_STATE: Mutex<Option<ConfirmState>> = Mutex::new(None);

    pub fn confirm_uninstall(app_name: &str, data_dirs: &[PathBuf]) -> Option<bool> {
        if let Ok(mut g) = CONFIRM_STATE.lock() {
            *g = Some(ConfirmState {
                app_name: app_name.to_string(),
                data_dirs: data_dirs
                    .iter()
                    .map(|d| d.to_string_lossy().into_owned())
                    .collect(),
                result: None,
            });
        }
        unsafe {
            // The palette statics are normally created by the wizard's
            // run(); an uninstall confirmation runs in a process where
            // the wizard never opened, so create them on demand.
            if BG_BRUSH.is_null() {
                BG_BRUSH = CreateSolidBrush(COLOR_BG);
            }
            if HEADER_FONT.is_null() {
                HEADER_FONT = CreateFontW(
                    -20,
                    0,
                    0,
                    0,
                    FW_SEMIBOLD as i32,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    0,
                    WIDE_SANS.as_ptr(),
                );
            }
            let instance =
                windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
            let class_name: Vec<u16> = "SeanPakUninstallWindowClass\0".encode_utf16().collect();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(confirm_wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: LoadIconW(std::ptr::null_mut(), IDI_APPLICATION),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: BG_BRUSH,
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            if RegisterClassExW(&wc) == 0 {
                let err = GetLastError();
                if err != 1410 {
                    // 1410 = class already registered: not fatal.
                    crate::diag::log(
                        "wizard",
                        &format!("uninstall confirm RegisterClassExW failed: error {err}"),
                    );
                    return None;
                }
            }
            let title: Vec<u16> = format!("Uninstall {app_name}\0").encode_utf16().collect();
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                640,
                400,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                crate::diag::log("wizard", "uninstall confirm window creation failed");
                return None;
            }
            create_confirm_controls(hwnd, instance);
            ShowWindow(hwnd, SW_SHOWDEFAULT);
            UpdateWindow(hwnd);
            let mut msg: MSG = std::mem::zeroed();
            loop {
                let result = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if result <= 0 {
                    break;
                }
                if IsDialogMessageW(hwnd, &msg) == 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
        }
        CONFIRM_STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|s| s.result))
    }

    unsafe fn create_confirm_controls(parent: HWND, instance: HINSTANCE) {
        let static_class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let button_class: Vec<u16> = "BUTTON\0".encode_utf16().collect();
        let (app_name, data_dirs) = CONFIRM_STATE
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|s| (s.app_name.clone(), s.data_dirs.clone()))
            })
            .unwrap_or_else(|| ("App".to_string(), Vec::new()));
        let mk =
            |class: &[u16], text: &str, style: u32, x: i32, y: i32, w: i32, h: i32, id: usize| {
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                unsafe {
                    CreateWindowExW(
                        0,
                        class.as_ptr(),
                        wide.as_ptr(),
                        WS_CHILD | WS_VISIBLE | style,
                        x,
                        y,
                        w,
                        h,
                        parent,
                        id as HMENU,
                        instance,
                        std::ptr::null_mut(),
                    )
                }
            };
        let title = mk(
            &static_class,
            &format!("Uninstall {app_name}"),
            0,
            16,
            14,
            590,
            26,
            ID_LBL_CONFIRM_TITLE,
        );
        SendMessageW(title, WM_SETFONT, HEADER_FONT as WPARAM, 1);
        mk(&static_class, "", SS_ETCHEDHORZ, 0, 48, 624, 2, 0);
        let body = if data_dirs.is_empty() {
            format!(
                "This removes {app_name}'s program files and the entries its install created (shortcuts, file associations, PATH and login entries)."
            )
        } else {
            format!(
                "This removes {app_name}'s program files and the entries its install created (shortcuts, file associations, PATH and login entries). Its data files are kept unless you tick the box below."
            )
        };
        mk(
            &static_class,
            &body,
            0,
            16,
            60,
            590,
            44,
            ID_LBL_CONFIRM_BODY,
        );
        if !data_dirs.is_empty() {
            mk(
                &button_class,
                &format!("Also delete {app_name}'s data files"),
                WS_TABSTOP | BS_AUTOCHECKBOX,
                16,
                116,
                590,
                24,
                ID_CHK_PURGE,
            );
            mk(
                &static_class,
                &data_dirs.join("\r\n"),
                0,
                32,
                144,
                560,
                80,
                ID_LBL_CONFIRM_DIRS,
            );
            mk(
                &static_class,
                "Deleted data cannot be recovered. If you might reinstall later, leave the box unticked — a reinstall finds the data again.",
                0,
                16,
                232,
                590,
                40,
                ID_LBL_CONFIRM_WARN,
            );
        }
        mk(
            &button_class,
            "Cancel",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            470,
            310,
            120,
            30,
            ID_BTN_CONFIRM_CANCEL,
        );
        mk(
            &button_class,
            "Uninstall",
            WS_TABSTOP | (BS_DEFPUSHBUTTON as u32),
            338,
            310,
            120,
            30,
            ID_BTN_CONFIRM,
        );
    }

    unsafe extern "system" fn confirm_wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CTLCOLORSTATIC => {
                let hdc = wparam as HDC;
                let ctrl = lparam as HWND;
                let id = GetWindowLongW(ctrl, GWL_ID) as usize;
                SetBkMode(hdc, TRANSPARENT as i32);
                if id == ID_LBL_CONFIRM_WARN || id == ID_LBL_CONFIRM_BODY {
                    SetTextColor(hdc, COLOR_BLUE);
                } else if id == ID_LBL_CONFIRM_DIRS {
                    SetTextColor(hdc, COLOR_MUTED);
                } else {
                    SetTextColor(hdc, COLOR_FG);
                }
                BG_BRUSH as LRESULT
            }
            WM_COMMAND => {
                match wparam & 0xFFFF {
                    ID_BTN_CONFIRM => {
                        let purge = get_check(hwnd, ID_CHK_PURGE);
                        if let Ok(mut g) = CONFIRM_STATE.lock() {
                            if let Some(s) = g.as_mut() {
                                s.result = Some(purge);
                            }
                        }
                        DestroyWindow(hwnd);
                    }
                    ID_BTN_CONFIRM_CANCEL => {
                        DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                0
            }
            WM_KEYDOWN => {
                if wparam == VK_ESCAPE_KEY {
                    DestroyWindow(hwnd);
                    return 0;
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{FileRecordHeader, OVERLAY_MAGIC};

    fn set_fixed(field: &mut [u8], value: &str) {
        let bytes = value.as_bytes();
        let len = bytes.len().min(field.len() - 1);
        field[..len].copy_from_slice(&bytes[..len]);
    }

    fn manifest_with(files: &[&str], total: u64) -> (OverlayManifestHeader, Vec<FileRecordHeader>) {
        let mut records = Vec::new();
        for name in files {
            let mut record = FileRecordHeader {
                file_size: 100,
                ..Default::default()
            };
            set_fixed(&mut record.relative_path_utf8, name);
            records.push(record);
        }
        let mut manifest = OverlayManifestHeader {
            magic: *OVERLAY_MAGIC,
            version: 3,
            file_count: records.len() as u32,
            primary_exe_index: 0,
            total_uncompressed_bytes: total,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };
        set_fixed(&mut manifest.app_name_utf8, "Test App");
        set_fixed(&mut manifest.app_version_utf8, "1.0.0");
        set_fixed(&mut manifest.publisher_utf8, "Tester");
        (manifest, records)
    }

    #[test]
    fn review_rows_reflect_options_and_records() {
        let (manifest, records) = manifest_with(&["app.exe", "data.bin"], 18_400_000);
        let mut options = InstallOptions::defaults_for("Test App");
        options.desktop_shortcut = 0;
        options.path_entry = 1;
        set_fixed(&mut options.file_associations_utf8, ".txt");
        let rows = review_rows(&manifest, &options, &records, "C:\\Apps\\Test App", true);
        // Six rows: disk space lives in the rail, not the table.
        assert_eq!(rows.len(), 6);
        assert_eq!(rows[0][0], "Install location");
        assert_eq!(rows[0][1], "C:\\Apps\\Test App");
        assert_eq!(rows[0][2], "Browse…");
        assert_eq!(rows[1][1], "app.exe + 1 more files");
        assert_eq!(rows[2][1], "Test App");
        assert_eq!(rows[3][1], "No");
        assert_eq!(rows[4][1], ".txt");
        assert_eq!(rows[5][1], "Yes");
    }

    #[test]
    fn review_rows_without_files() {
        let (manifest, records) = manifest_with(&[], 0);
        let options = InstallOptions::defaults_for("Empty");
        let rows = review_rows(&manifest, &options, &records, "D:\\Empty", true);
        assert_eq!(rows[1][1], "No files");
        assert_eq!(rows[4][1], "None");
    }

    #[test]
    fn review_rows_include_start_at_login_only_when_offered() {
        let (manifest, records) = manifest_with(&["app.exe"], 100);
        let mut options = InstallOptions::defaults_for("Test App");
        options.start_at_login = 1;
        options.start_at_login_default = 1;
        let rows = review_rows(&manifest, &options, &records, "C:\\Apps\\Test App", true);
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[6][0], "Start at login");
        assert_eq!(rows[6][1], "Yes");
        assert_eq!(rows[6][2], "Toggle");
        // Offered but off reads "No".
        options.start_at_login_default = 0;
        let rows = review_rows(&manifest, &options, &records, "C:\\Apps\\Test App", true);
        assert_eq!(rows[6][1], "No");
    }

    #[test]
    fn review_rows_flag_an_unwritable_location() {
        let (manifest, records) = manifest_with(&["app.exe"], 100);
        let options = InstallOptions::defaults_for("Test App");
        let rows = review_rows(
            &manifest,
            &options,
            &records,
            "C:\\Program Files\\Test App",
            false,
        );
        assert_eq!(
            rows[0][1],
            "C:\\Program Files\\Test App — needs administrator access"
        );
        assert_eq!(rows[0][2], "Use per-user folder");
        // Every other row is untouched by the verdict.
        assert_eq!(rows[1][1], "app.exe");
        assert_eq!(rows[5][0], "PATH entry");
    }

    #[test]
    fn install_dir_and_key_defaults() {
        let options = InstallOptions::defaults_for("My App");
        assert_eq!(
            default_install_dir(&options, "My App"),
            PathBuf::from("C:\\Program Files\\My App")
        );
        assert_eq!(
            uninstall_key_path("My App"),
            "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\SeanPak-My App"
        );
        assert_eq!(sanitize_name("a/b:c"), "a_b_c");
    }
}
