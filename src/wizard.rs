//! SeanPak installer wizard (TesselGUI Design C — the ledger review).
//!
//! When a built SeanPak installer (this executable plus a payload overlay)
//! is run with no arguments, this wizard opens instead of the packager GUI:
//! a single review page listing every install decision in a gridded table
//! (Item / Value / Change), with Cancel and a green Install button in the
//! footer. Values come from the package's overlay manifest and its
//! version 3 install-options block (defaults for older packages).
//!
//! What Install does in this slice: extracts the payload into the chosen
//! install location (each existing target is snapshotted first, so a
//! failure rolls back), copies this executable next to the payload as
//! `Uninstall.exe`, and writes the Add/Remove Programs (ARP) entry whose
//! uninstall string runs that copy with `--uninstall`. Creating shortcuts,
//! file associations, and PATH entries is NOT wired yet — the review table
//! shows the recorded choices, and those rows are applied by a later slice.
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
    use std::sync::Mutex;

    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    use windows_sys::Win32::UI::Controls::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use super::{default_install_dir, review_rows, InstallOptions};
    use crate::overlay::{FileRecordHeader, OverlayManifestHeader};

    // TesselGUI rev2 GitHub Dark palette (COLORREF is 0x00BBGGRR).
    const COLOR_BG: u32 = 0x0017_110D; // #0D1117
    const COLOR_SURFACE: u32 = 0x001D_1612; // #12161D
    const COLOR_FG: u32 = 0x00FC_F6F0; // #F0F6FC
    const COLOR_MUTED: u32 = 0x00A1_9891; // #9198A1
    const COLOR_BLUE: u32 = 0x00F7_812F; // #2F81F7
    const COLOR_GREEN: u32 = 0x0037_7F1A; // #1A7F37

    const ID_LBL_HEADER: usize = 401;
    const ID_LBL_STEP: usize = 402;
    const ID_LBL_INFO: usize = 403;
    const ID_LBL_HINT: usize = 404;
    const ID_LBL_STATUS: usize = 405;
    const ID_LIST: usize = 410;
    const ID_BTN_CANCEL: usize = 301;
    const ID_BTN_INSTALL: usize = 302;

    // Static-control styles (windows-sys keeps these behind the
    // SystemServices feature, which this crate does not enable).
    const SS_RIGHT: u32 = 0x0002;
    const SS_ETCHEDHORZ: u32 = 0x0010;

    // NM_DBLCLK is (-3); NMHDR.code is unsigned, so compare as i32.
    const NM_DBLCLK_I32: i32 = -3;

    struct WizardState {
        manifest: OverlayManifestHeader,
        records: Vec<FileRecordHeader>,
        options: InstallOptions,
        exe_path: PathBuf,
        install_dir: PathBuf,
        installing: bool,
        installed: bool,
    }

    static STATE: Mutex<Option<WizardState>> = Mutex::new(None);
    static mut BG_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut SURFACE_BRUSH: HBRUSH = std::ptr::null_mut();

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
                installing: false,
                installed: false,
            });
        }
        unsafe {
            let init = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_LISTVIEW_CLASSES,
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
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            crate::diag::log("wizard", "message loop exited cleanly");
            Ok(())
        }
    }

    unsafe fn create_controls(parent: HWND, instance: HINSTANCE) {
        let static_class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let button_class: Vec<u16> = "BUTTON\0".encode_utf16().collect();
        let list_class: Vec<u16> = "SysListView32\0".encode_utf16().collect();

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

        mk(
            &static_class,
            &format!("{app_name} {app_version}"),
            0,
            16,
            14,
            520,
            26,
            ID_LBL_HEADER,
        );
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
                SetBkMode(hdc, TRANSPARENT as i32);
                let installed = STATE
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().map(|s| s.installed))
                    .unwrap_or(false);
                if id == ID_LBL_STATUS && installed {
                    // Success green, always paired with the status text.
                    SetTextColor(hdc, COLOR_GREEN);
                } else if id == ID_LBL_STEP || id == ID_LBL_HINT || id == ID_LBL_STATUS {
                    SetTextColor(hdc, COLOR_MUTED);
                } else if id == ID_LBL_INFO {
                    SetTextColor(hdc, COLOR_BLUE);
                } else {
                    SetTextColor(hdc, COLOR_FG);
                }
                BG_BRUSH as LRESULT
            }
            WM_COMMAND => {
                let control_id = wparam & 0xFFFF;
                match control_id {
                    ID_BTN_CANCEL => {
                        let busy = STATE
                            .lock()
                            .ok()
                            .and_then(|g| g.as_ref().map(|s| s.installing))
                            .unwrap_or(false);
                        if !busy {
                            DestroyWindow(hwnd);
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
            WM_DESTROY => {
                PostQuitMessage(0);
                0
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }

    unsafe fn on_row_activate(hwnd: HWND) {
        let (busy, installed) = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| (s.installing, s.installed)))
            .unwrap_or((true, true));
        if busy || installed {
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
        if selected == 0 {
            // Install location is the one row this slice can edit.
            if let Some(folder) = crate::native_dialog::pick_folder() {
                if let Ok(mut g) = STATE.lock() {
                    if let Some(s) = g.as_mut() {
                        s.install_dir = folder;
                    }
                }
                fill_list(list);
                set_text(hwnd, ID_LBL_STATUS, "Install location updated.");
            }
        } else {
            set_text(
                hwnd,
                ID_LBL_STATUS,
                "Only the install location can be changed in this build; the other recorded choices are applied by the next SeanPak slice.",
            );
        }
    }

    unsafe fn on_install_click(hwnd: HWND) {
        let (installed, installing, exe_path, install_dir) = match STATE.lock() {
            Ok(g) => match g.as_ref() {
                Some(s) => (
                    s.installed,
                    s.installing,
                    s.exe_path.clone(),
                    s.install_dir.clone(),
                ),
                None => return,
            },
            Err(_) => return,
        };
        if installed {
            DestroyWindow(hwnd);
            return;
        }
        if installing {
            return;
        }
        if let Ok(mut g) = STATE.lock() {
            if let Some(s) = g.as_mut() {
                s.installing = true;
            }
        }
        set_enabled(hwnd, ID_BTN_INSTALL, false);
        set_enabled(hwnd, ID_BTN_CANCEL, false);
        set_enabled(hwnd, ID_LIST, false);
        set_text(hwnd, ID_LBL_STATUS, "Installing…");

        let hwnd_value = hwnd as usize;
        std::thread::spawn(move || {
            let result = perform_install(&exe_path, &install_dir);
            let hwnd = hwnd_value as HWND;
            unsafe {
                match result {
                    Ok(summary) => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(s) = g.as_mut() {
                                s.installing = false;
                                s.installed = true;
                            }
                        }
                        crate::diag::log("wizard", &format!("install ok: {summary}"));
                        set_text(hwnd, ID_LBL_STEP, "Step 4 of 4 — Finish");
                        set_text(hwnd, ID_LBL_STATUS, &summary);
                        set_text(hwnd, ID_BTN_INSTALL, "Finish");
                        set_enabled(hwnd, ID_BTN_INSTALL, true);
                    }
                    Err(e) => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(s) = g.as_mut() {
                                s.installing = false;
                            }
                        }
                        crate::diag::log("wizard", &format!("install failed: {e}"));
                        set_text(
                            hwnd,
                            ID_LBL_STATUS,
                            &format!(
                                "Install failed: {e} Changes were rolled back; nothing was left half-installed."
                            ),
                        );
                        set_enabled(hwnd, ID_BTN_INSTALL, true);
                        set_enabled(hwnd, ID_BTN_CANCEL, true);
                        set_enabled(hwnd, ID_LIST, true);
                    }
                }
            }
        });
    }

    /// Runs the shared install operation (see `crate::install`) and
    /// renders its outcome for the wizard's status line.
    fn perform_install(exe_path: &Path, dest: &Path) -> Result<String, String> {
        let outcome = crate::install::perform_install(exe_path, dest)?;
        Ok(format!(
            "Installed {} files to {}. Remove it any time from Windows Settings \u{2192} Apps.",
            outcome.files_written,
            outcome.dest.display()
        ))
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
