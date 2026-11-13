//! SeanPak installer wizard (TesselGUI Design C — the ledger review).
//!
//! When a built SeanPak installer (this executable plus a payload overlay)
//! is run with no arguments, this wizard opens instead of the packager GUI.
//! Four pages in the Design C language:
//!
//! 1. **Review** — every install decision in a gridded table
//!    (Item / Value / Change). Values come from the package's overlay
//!    manifest and its version 3+ install-options block (defaults for
//!    older packages). A row whose Change column offers an action routes
//!    to the Options page.
//! 2. **Options** — edit the install location (folder picker or typed),
//!    toggle the desktop / Start Menu / PATH rows, edit the Start Menu
//!    folder and the file associations.
//! 3. **Progress** — the shared install operation runs on a worker
//!    thread with live per-file progress; Cancel asks it to stop, the
//!    operation rolls back, and the wizard lands on the Finish page in
//!    its honest "cancelled, nothing kept" state.
//! 4. **Finish** — what was installed, where, and the receipt path; or,
//!    when the install failed, the plain-language reason and the fact
//!    that every change was rolled back.
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
use crate::scanner::format_bytes;

/// Builds the Design C review rows (Item / Value / Change) for a package.
/// Pure and platform-independent so the table content is unit-tested.
pub fn review_rows(
    manifest: &OverlayManifestHeader,
    options: &InstallOptions,
    records: &[FileRecordHeader],
    install_dir_display: &str,
    free_bytes: Option<u64>,
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
    let disk = match free_bytes {
        Some(free) => format!(
            "{} of {} free",
            format_bytes(manifest.total_uncompressed_bytes),
            format_bytes(free)
        ),
        None => format!(
            "{} required",
            format_bytes(manifest.total_uncompressed_bytes)
        ),
    };
    let yes_no = |on: bool| if on { "Yes" } else { "No" }.to_string();
    vec![
        [
            "Install location".to_string(),
            install_dir_display.to_string(),
            "Browse…".to_string(),
        ],
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
        ["Disk space".to_string(), disk, "—".to_string()],
    ]
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

/// The Design C installer wizard for one overlay package.
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

    // TesselGUI rev2 GitHub Dark palette (COLORREF is 0x00BBGGRR).
    const COLOR_BG: u32 = 0x0017_110D; // #0D1117
    const COLOR_SURFACE: u32 = 0x001D_1612; // #12161D
    const COLOR_FG: u32 = 0x00FC_F6F0; // #F0F6FC
    const COLOR_MUTED: u32 = 0x00A1_9891; // #9198A1
    const COLOR_BLUE: u32 = 0x00F7_812F; // #2F81F7
    const COLOR_GREEN: u32 = 0x0037_7F1A; // #1A7F37

    const ID_TILE: usize = 400;
    const ID_LBL_HEADER: usize = 401;
    const ID_LBL_STEP: usize = 402;
    const ID_LBL_INFO: usize = 403;
    const ID_LBL_HINT: usize = 404;
    const ID_LBL_STATUS: usize = 405;
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
    // Progress page (page 3).
    const ID_LBL_PROGFILE: usize = 440;
    const ID_PROGRESS: usize = 441;
    // Finish page (page 4).
    const ID_LBL_FINISH: usize = 450;

    const OPTION_CONTROLS: [usize; 11] = [
        ID_LBL_LOC,
        ID_EDT_LOCATION,
        ID_BTN_BROWSE,
        ID_CHK_DESKTOP,
        ID_CHK_STARTMENU,
        ID_CHK_PATH,
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
    // Edit / checkbox styles, likewise defined locally.
    const ES_AUTOHSCROLL: u32 = 0x0080;
    const BS_AUTOCHECKBOX: u32 = 0x0003;

    // NM_DBLCLK is (-3); NMHDR.code is unsigned, so compare as i32.
    const NM_DBLCLK_I32: i32 = -3;

    struct WizardState {
        manifest: OverlayManifestHeader,
        records: Vec<FileRecordHeader>,
        options: InstallOptions,
        exe_path: PathBuf,
        install_dir: PathBuf,
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
    }

    static STATE: Mutex<Option<WizardState>> = Mutex::new(None);
    static MAIN_HWND: Mutex<usize> = Mutex::new(0);
    static mut BG_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut SURFACE_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut GREEN_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut HEADER_FONT: HFONT = std::ptr::null_mut();
    static mut TILE_FONT: HFONT = std::ptr::null_mut();

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
                page: 1,
                installing: false,
                installed: false,
                cancel: Arc::new(AtomicBool::new(false)),
                progress: Arc::new(Mutex::new((0, 0, String::new()))),
                finish: None,
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

        let (app_name, app_version) = STATE
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|s| (s.manifest.app_name(), s.manifest.app_version()))
            })
            .unwrap_or_else(|| ("App".to_string(), String::new()));

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

        // Header: the packaged app's monogram tile (app-green), then
        // the app name/version. No external assets — the tile is a
        // static control whose initial is drawn from the app name.
        let monogram = app_name
            .chars()
            .find(|c| c.is_alphanumeric())
            .map(|c| c.to_uppercase().collect::<String>())
            .unwrap_or_else(|| "?".to_string());
        let tile = mk(&static_class, &monogram, SS_CENTER, 16, 8, 40, 40, ID_TILE);
        SendMessageW(tile, WM_SETFONT, TILE_FONT as WPARAM, 1);
        let header = mk(
            &static_class,
            &format!("{app_name} {app_version}"),
            0,
            68,
            14,
            468,
            26,
            ID_LBL_HEADER,
        );
        SendMessageW(header, WM_SETFONT, HEADER_FONT as WPARAM, 1);
        mk(
            &static_class,
            "Step 1 of 4 — Review",
            SS_RIGHT,
            548,
            18,
            300,
            22,
            ID_LBL_STEP,
        );
        mk(&static_class, "", SS_ETCHEDHORZ, 0, 48, 864, 2, 0);

        let list = mk(
            &list_class,
            "",
            WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL,
            16,
            60,
            832,
            296,
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
            "Ready to install. Nothing is downloaded and nothing is sent anywhere — this installer works fully offline and can be undone from Windows Settings → Apps.",
            0, 16, 368, 832, 44, ID_LBL_INFO,
        );
        mk(&static_class, "", 0, 16, 424, 832, 22, ID_LBL_STATUS);
        mk(&static_class, "", SS_ETCHEDHORZ, 0, 486, 864, 2, 0);
        mk(
            &static_class,
            "↑↓ navigate · Enter edit · Ctrl+K palette",
            0,
            16,
            502,
            480,
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
            16,
            64,
            400,
            20,
            ID_LBL_LOC,
        );
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
            16,
            88,
            700,
            26,
            ID_EDT_LOCATION,
        );
        mk(
            &button_class,
            "Browse…",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            724,
            88,
            124,
            26,
            ID_BTN_BROWSE,
        );
        mk(
            &button_class,
            "Desktop shortcut",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            16,
            132,
            400,
            24,
            ID_CHK_DESKTOP,
        );
        mk(
            &button_class,
            "Start Menu shortcuts",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            16,
            162,
            400,
            24,
            ID_CHK_STARTMENU,
        );
        mk(
            &button_class,
            "Add the install folder to PATH",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            16,
            192,
            400,
            24,
            ID_CHK_PATH,
        );
        mk(
            &static_class,
            "Start Menu folder",
            0,
            16,
            228,
            400,
            20,
            ID_LBL_SMFOLDER,
        );
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
            16,
            252,
            400,
            26,
            ID_EDT_SMFOLDER,
        );
        mk(
            &static_class,
            "File associations (comma-separated, for example .txt, .md)",
            0,
            16,
            292,
            700,
            20,
            ID_LBL_ASSOC,
        );
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
            16,
            316,
            400,
            26,
            ID_EDT_ASSOC,
        );
        mk(
            &static_class,
            "Shortcuts, associations, and the PATH entry are created for your user account only — no administrator rights needed — and uninstall removes exactly what install created.",
            0, 16, 360, 832, 44, ID_LBL_OPTNOTE,
        );

        // ---- Page 3: Progress ----
        mk(
            &static_class,
            "Waiting to start…",
            0,
            16,
            150,
            832,
            22,
            ID_LBL_PROGFILE,
        );
        mk(
            &progress_class,
            "",
            WS_TABSTOP,
            16,
            184,
            832,
            28,
            ID_PROGRESS,
        );

        // ---- Page 4: Finish ----
        mk(&static_class, "", 0, 16, 64, 832, 380, ID_LBL_FINISH);

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
        // Progress group.
        show(ID_LBL_PROGFILE, page == 3);
        show(ID_PROGRESS, page == 3);
        // Finish group.
        show(ID_LBL_FINISH, page == 4);
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
                2 => "Step 2 of 4 — Options",
                3 => "Step 3 of 4 — Installing",
                4 => "Step 4 of 4 — Finish",
                _ => "Step 1 of 4 — Review",
            },
        );
        if page == 2 {
            fill_options_controls(hwnd);
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
        let snapshot = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| (s.install_dir.clone(), s.options)));
        let Some((dir, options)) = snapshot else {
            return;
        };
        set_text(hwnd, ID_EDT_LOCATION, &dir.to_string_lossy());
        set_check(hwnd, ID_CHK_DESKTOP, options.wants_desktop_shortcut());
        set_check(hwnd, ID_CHK_STARTMENU, options.wants_start_menu_shortcut());
        set_check(hwnd, ID_CHK_PATH, options.wants_path_entry());
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
            for (idx, (title, width)) in [("Item", 250), ("Value", 400), ("Change", 160)]
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
                let free = free_bytes_for(&s.install_dir);
                review_rows(
                    &s.manifest,
                    &s.options,
                    &s.records,
                    &s.install_dir.to_string_lossy(),
                    free,
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
                let installed = STATE
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().map(|s| s.installed))
                    .unwrap_or(false);
                if id == ID_TILE {
                    // The monogram tile: app-green block, light initial.
                    SetBkMode(hdc, OPAQUE as i32);
                    SetBkColor(hdc, COLOR_GREEN);
                    SetTextColor(hdc, COLOR_FG);
                    return GREEN_BRUSH as LRESULT;
                }
                SetBkMode(hdc, TRANSPARENT as i32);
                if (id == ID_LBL_STATUS || id == ID_LBL_FINISH) && installed {
                    // Success green, always paired with the status text.
                    SetTextColor(hdc, COLOR_GREEN);
                } else if id == ID_LBL_STEP || id == ID_LBL_HINT || id == ID_LBL_STATUS {
                    SetTextColor(hdc, COLOR_MUTED);
                } else if id == ID_LBL_INFO || id == ID_LBL_OPTNOTE {
                    SetTextColor(hdc, COLOR_BLUE);
                } else {
                    SetTextColor(hdc, COLOR_FG);
                }
                BG_BRUSH as LRESULT
            }
            WM_COMMAND => {
                let control_id = wparam & 0xFFFF;
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
                    "\r\nReceipt: {}\r\nShortcuts created: {} · File associations applied: {} · PATH entry: {}",
                    outcome.receipt_path.display(),
                    outcome.shortcuts_created,
                    outcome.associations_applied,
                    if outcome.path_entry_added { "added" } else { "not added" },
                );
                text += "\r\n\r\nRemove it any time from Windows Settings \u{2192} Apps, or by running Uninstall.exe in the install folder.";
                set_text(hwnd, ID_LBL_FINISH, &text);
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
        let (busy, installed, page) = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| (s.installing, s.installed, s.page)))
            .unwrap_or((true, true, 1));
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
        // Rows with a real Change action route to the Options page:
        // 0 location, 2 Start Menu folder, 3 desktop shortcut,
        // 4 associations, 5 PATH. Components (1) and Disk space (6)
        // are informational.
        match selected {
            0 | 2 | 3 | 4 | 5 => show_page(hwnd, 2),
            _ => set_text(
                hwnd,
                ID_LBL_STATUS,
                "That row is informational — the package decides its components, and the disk row reports free space.",
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
            // The footer button reads "Close" on the Finish page.
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
        let rows = review_rows(
            &manifest,
            &options,
            &records,
            "C:\\Apps\\Test App",
            Some(212_000_000_000),
        );
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[0][0], "Install location");
        assert_eq!(rows[0][1], "C:\\Apps\\Test App");
        assert_eq!(rows[1][1], "app.exe + 1 more files");
        assert_eq!(rows[2][1], "Test App");
        assert_eq!(rows[3][1], "No");
        assert_eq!(rows[4][1], ".txt");
        assert_eq!(rows[5][1], "Yes");
        assert!(rows[6][1].contains("free"));
    }

    #[test]
    fn review_rows_without_free_space_or_files() {
        let (manifest, records) = manifest_with(&[], 0);
        let options = InstallOptions::defaults_for("Empty");
        let rows = review_rows(&manifest, &options, &records, "D:\\Empty", None);
        assert_eq!(rows[1][1], "No files");
        assert!(rows[6][1].contains("required"));
        assert_eq!(rows[4][1], "None");
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
