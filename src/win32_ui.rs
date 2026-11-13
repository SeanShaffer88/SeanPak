//! Pure Native Win32 UI & Event-Driven Message Pump with the SeanPak
//! TesselGUI rev2 (GitHub Dark) theme
//! Event-driven message loop: no polling, the window only works when the user acts.
//!
//! Windows-only: on other platforms `NativeWin32App::run` returns an error.

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    static IS_RUNNING: AtomicBool = AtomicBool::new(false);

    /// Folder chosen in the GUI (Browse button or the command-line argument).
    static SELECTED_FOLDER: Mutex<Option<PathBuf>> = Mutex::new(None);

    const ID_BTN_BROWSE: usize = 101;
    const ID_BTN_BUILD: usize = 102;
    const ID_BTN_ELEVATE: usize = 103;
    const ID_LBL_STATUS: usize = 201;
    const ID_LBL_PATH: usize = 202;
    const ID_LBL_TITLE: usize = 203;
    const ID_LBL_SUBTITLE: usize = 204;

    // TesselGUI rev2 GitHub Dark palette (COLORREF is 0x00BBGGRR)
    const COLOR_BG_DARK: u32 = 0x0017110D; // BGR for #0D1117 (Near-black blue-gray)
    const COLOR_TEXT_LIGHT: u32 = 0x00FCF6F0; // BGR for #F0F6FC (Foreground)
    const COLOR_TEXT_MUTED: u32 = 0x00A19891; // BGR for #9198A1 (Muted gray)
    const COLOR_ACCENT_GREEN: u32 = 0x00377F1A; // BGR for #1A7F37 (GitHub green)

    static mut DARK_BRUSH: HBRUSH = std::ptr::null_mut();

    pub struct NativeWin32App {
        pub target_folder: Option<PathBuf>,
    }

    impl NativeWin32App {
        pub fn new(initial_folder: Option<PathBuf>) -> Self {
            Self {
                target_folder: initial_folder,
            }
        }

        /// Spawns the native Win32 window and runs the non-polling event loop.
        pub fn run(&mut self) -> Result<(), u32> {
            if let Ok(mut guard) = SELECTED_FOLDER.lock() {
                *guard = self.target_folder.clone();
            }
            unsafe {
                let instance =
                    windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
                let class_name: Vec<u16> = "SeanPakWindowClass\0".encode_utf16().collect();
                let window_title: Vec<u16> = format!(
                    "SeanPak {} - Native Ultra-Lightweight Packager\0",
                    env!("CARGO_PKG_VERSION")
                )
                .encode_utf16()
                .collect();

                DARK_BRUSH = CreateSolidBrush(COLOR_BG_DARK);

                let icon = LoadIconW(instance, 1 as _);
                let fallback_icon = if !icon.is_null() {
                    icon
                } else {
                    LoadIconW(std::ptr::null_mut(), IDI_APPLICATION)
                };

                let wc = WNDCLASSEXW {
                    cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                    style: CS_HREDRAW | CS_VREDRAW,
                    lpfnWndProc: Some(wnd_proc),
                    cbClsExtra: 0,
                    cbWndExtra: 0,
                    hInstance: instance,
                    hIcon: fallback_icon,
                    hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                    hbrBackground: DARK_BRUSH,
                    lpszMenuName: std::ptr::null(),
                    lpszClassName: class_name.as_ptr(),
                    hIconSm: fallback_icon,
                };

                if RegisterClassExW(&wc) == 0 {
                    let err = GetLastError();
                    // 1410 = class already registered: not fatal, window
                    // creation can still proceed with the existing class.
                    if err != 1410 {
                        crate::diag::log("gui", &format!("RegisterClassExW failed: error {err}"));
                        return Err(err);
                    }
                }
                crate::diag::log("gui", "window class registered");

                let hwnd = CreateWindowExW(
                    0,
                    class_name.as_ptr(),
                    window_title.as_ptr(),
                    WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX,
                    CW_USEDEFAULT,
                    CW_USEDEFAULT,
                    740,
                    500,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    instance,
                    std::ptr::null_mut(),
                );

                if hwnd.is_null() {
                    let err = GetLastError();
                    crate::diag::log("gui", &format!("CreateWindowExW failed: error {err}"));
                    return Err(err);
                }
                crate::diag::log("gui", "main window created");

                // Create UI Controls
                create_controls(hwnd, instance);

                ShowWindow(hwnd, SW_SHOWDEFAULT);
                UpdateWindow(hwnd);

                IS_RUNNING.store(true, Ordering::SeqCst);

                // Pure event-driven message pump (0.00% CPU when idle).
                // GetMessageW returns -1 on error, 0 on WM_QUIT, >0 for a
                // message; a bare `> 0` loop would turn an error into a
                // silent exit, so all three outcomes are handled explicitly.
                crate::diag::log("gui", "message loop entered");
                let mut msg: MSG = std::mem::zeroed();
                loop {
                    let result = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                    if result == 0 {
                        break; // WM_QUIT
                    }
                    if result == -1 {
                        let err = GetLastError();
                        crate::diag::log("gui", &format!("GetMessageW failed: error {err}"));
                        return Err(err);
                    }
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
                crate::diag::log("gui", "message loop exited cleanly");

                if !DARK_BRUSH.is_null() {
                    DeleteObject(DARK_BRUSH as _);
                }

                Ok(())
            }
        }
    }

    unsafe fn create_controls(parent: HWND, instance: HINSTANCE) {
        let button_class: Vec<u16> = "BUTTON\0".encode_utf16().collect();
        let static_class: Vec<u16> = "STATIC\0".encode_utf16().collect();

        // App Header: SeanPak Banner
        let text_title: Vec<u16> = "SEANPAK [NATIVE SECTOR-ALIGNED PE OVERLAY]\0"
            .encode_utf16()
            .collect();
        CreateWindowExW(
            0,
            static_class.as_ptr(),
            text_title.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            30,
            24,
            660,
            26,
            parent,
            ID_LBL_TITLE as HMENU,
            instance,
            std::ptr::null_mut(),
        );

        let text_subtitle: Vec<u16> =
            "Zero-Overhead Windows Distribution | 0.00% Idle CPU | < 8 MB Resident RAM\0"
                .encode_utf16()
                .collect();
        CreateWindowExW(
            0,
            static_class.as_ptr(),
            text_subtitle.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            30,
            54,
            660,
            20,
            parent,
            ID_LBL_SUBTITLE as HMENU,
            instance,
            std::ptr::null_mut(),
        );

        // Target Folder selection label
        let path_label = match SELECTED_FOLDER.lock().ok().and_then(|g| g.clone()) {
            Some(folder) => format!("Selected: {}\0", folder.display()),
            None => "Target Directory: (Select source folder with your binaries)\0".to_string(),
        };
        let text_path: Vec<u16> = path_label.encode_utf16().collect();
        CreateWindowExW(
            0,
            static_class.as_ptr(),
            text_path.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            30,
            110,
            480,
            28,
            parent,
            ID_LBL_PATH as HMENU,
            instance,
            std::ptr::null_mut(),
        );

        // Browse button
        let text_browse: Vec<u16> = "Browse Folder...\0".encode_utf16().collect();
        CreateWindowExW(
            0,
            button_class.as_ptr(),
            text_browse.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | (BS_PUSHBUTTON as u32),
            520,
            104,
            170,
            34,
            parent,
            ID_BTN_BROWSE as HMENU,
            instance,
            std::ptr::null_mut(),
        );

        // Build Package button
        let text_build: Vec<u16> = "Build SeanPak Executable\0".encode_utf16().collect();
        CreateWindowExW(
            0,
            button_class.as_ptr(),
            text_build.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | (BS_DEFPUSHBUTTON as u32),
            30,
            360,
            280,
            44,
            parent,
            ID_BTN_BUILD as HMENU,
            instance,
            std::ptr::null_mut(),
        );

        // UAC Self-Elevation button
        let text_elevate: Vec<u16> = "Elevate via UAC (runas)\0".encode_utf16().collect();
        CreateWindowExW(
            0,
            button_class.as_ptr(),
            text_elevate.as_ptr(),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | (BS_PUSHBUTTON as u32),
            330,
            360,
            240,
            44,
            parent,
            ID_BTN_ELEVATE as HMENU,
            instance,
            std::ptr::null_mut(),
        );

        // Status label
        let text_status: Vec<u16> =
            "Status: Idle | Restart Manager Active | 4096-Byte Sector Boundary Enforcement\0"
                .encode_utf16()
                .collect();
        CreateWindowExW(
            0,
            static_class.as_ptr(),
            text_status.as_ptr(),
            WS_CHILD | WS_VISIBLE,
            30,
            426,
            670,
            22,
            parent,
            ID_LBL_STATUS as HMENU,
            instance,
            std::ptr::null_mut(),
        );
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
                let ctrl_hwnd = lparam as HWND;
                let id = GetWindowLongW(ctrl_hwnd, GWL_ID) as usize;

                SetBkMode(hdc, TRANSPARENT as i32);
                if id == ID_LBL_TITLE {
                    SetTextColor(hdc, COLOR_ACCENT_GREEN);
                } else if id == ID_LBL_SUBTITLE || id == ID_LBL_STATUS {
                    SetTextColor(hdc, COLOR_TEXT_MUTED);
                } else {
                    SetTextColor(hdc, COLOR_TEXT_LIGHT);
                }
                DARK_BRUSH as LRESULT
            }
            WM_COMMAND => {
                let control_id = wparam & 0xFFFF;
                match control_id {
                    ID_BTN_BROWSE => {
                        if let Some(folder) = crate::native_dialog::pick_folder() {
                            if let Ok(mut guard) = SELECTED_FOLDER.lock() {
                                *guard = Some(folder.clone());
                            }
                            let text = format!("Selected: {}\0", folder.display());
                            let wide: Vec<u16> = text.encode_utf16().collect();
                            let lbl_hwnd = GetDlgItem(hwnd, ID_LBL_PATH as i32);
                            SetWindowTextW(lbl_hwnd, wide.as_ptr());
                        }
                    }
                    ID_BTN_BUILD => {
                        start_build(hwnd);
                    }
                    ID_BTN_ELEVATE => {
                        self_elevate();
                    }
                    _ => {}
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

    pub fn self_elevate() -> bool {
        unsafe {
            use windows_sys::Win32::UI::Shell::ShellExecuteExW;
            use windows_sys::Win32::UI::Shell::SHELLEXECUTEINFOW;

            let exe_path = match std::env::current_exe() {
                Ok(p) => p,
                Err(_) => return false,
            };

            let verb: Vec<u16> = "runas\0".encode_utf16().collect();
            let file: Vec<u16> = exe_path
                .to_string_lossy()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();

            let mut sei: SHELLEXECUTEINFOW = std::mem::zeroed();
            sei.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
            sei.lpVerb = verb.as_ptr();
            sei.lpFile = file.as_ptr();
            sei.nShow = SW_SHOWDEFAULT;

            let ok = ShellExecuteExW(&mut sei);
            if ok != 0 {
                std::process::exit(0);
            }
            false
        }
    }

    fn set_status(hwnd: HWND, text: &str) {
        unsafe {
            let status_hwnd = GetDlgItem(hwnd, ID_LBL_STATUS as i32);
            if !status_hwnd.is_null() {
                let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
                SetWindowTextW(status_hwnd, wide.as_ptr());
            }
        }
    }

    /// Runs a real packaging build for the selected folder. The save dialog runs
    /// on the UI thread; the (potentially long) build itself runs on a worker
    /// thread and reports back through the status label, so the window stays
    /// responsive while files stream.
    fn start_build(hwnd: HWND) {
        let folder = SELECTED_FOLDER.lock().ok().and_then(|guard| guard.clone());
        let folder = match folder {
            Some(f) => f,
            None => {
                set_status(hwnd, "Select a source folder first (Browse Folder...).");
                return;
            }
        };

        let app_stem = folder
            .file_name()
            .map(|s| s.to_string_lossy().replace(' ', ""))
            .unwrap_or_else(|| "App".to_string());
        let default_name = format!("{app_stem}-Setup.exe");
        let output = match crate::native_dialog::pick_save_file(&default_name) {
            Some(path) => path,
            None => return, // user cancelled the save dialog
        };

        set_status(hwnd, "Packaging payload into sector-aligned PE overlay...");
        let hwnd_value = hwnd as usize;
        std::thread::spawn(move || {
            let message = build_package(&folder, &output);
            let hwnd = hwnd_value as HWND;
            set_status(hwnd, &message);
        });
    }

    fn build_package(folder: &Path, output: &Path) -> String {
        use crate::overlay::OverlayWriter;
        use crate::rollback::TransactionRollbackLog;

        let stub = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => return format!("Build failed: cannot locate own executable: {e}"),
        };
        let mut tx = match TransactionRollbackLog::new() {
            Ok(tx) => tx,
            Err(e) => return format!("Build failed: rollback log: {e}"),
        };
        if let Err(e) = tx.snapshot_file_before_write(output) {
            return format!("Build failed: snapshot: {e}");
        }
        let app_name = folder
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "MyApplication".to_string());
        match OverlayWriter::build_from_folder(
            &stub,
            output,
            folder,
            &app_name,
            "1.0.0.0",
            "SeanPak Native",
        ) {
            Ok(bytes) => {
                let _ = tx.commit();
                format!(
                    "Done: {} ({} bytes) - double-click it or run it to extract.",
                    output.display(),
                    bytes
                )
            }
            Err(e) => {
                tx.rollback();
                format!("Build failed: {e}")
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{self_elevate, NativeWin32App};

#[cfg(not(windows))]
pub struct NativeWin32App {
    pub target_folder: Option<std::path::PathBuf>,
}

#[cfg(not(windows))]
impl NativeWin32App {
    pub fn new(initial_folder: Option<std::path::PathBuf>) -> Self {
        Self {
            target_folder: initial_folder,
        }
    }

    pub fn run(&mut self) -> Result<(), u32> {
        Err(1)
    }
}

#[cfg(not(windows))]
pub fn self_elevate() -> bool {
    false
}
