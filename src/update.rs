//! Silent auto-update (v5.1.0): when a Setup wizard starts and the
//! SAME app is already installed at the package's target location,
//! there is nothing to ask — the wizard's page flow is replaced by a
//! minimal update window that runs the ordinary upgrade-in-place
//! through the shared install operation
//! ([`crate::install::perform_install_with`], journal and rollback
//! included) and closes itself.
//!
//! The decision is one pure function, [`decide_update`]: the install
//! receipt at the target location (`seanpak-receipt.json`, the same
//! identity the upgrade logic uses — the receipt's app name must
//! match the package's) plus a version comparison. An older or equal
//! installed version takes the silent path; a NEWER installed
//! version is not an update at all — the caller falls through to the
//! normal wizard, whose install run makes the existing plain
//! downgrade refusal; no receipt, or a receipt for a different app,
//! is a first run and also gets the normal wizard.
//!
//! If the app is running, the update first asks its processes to
//! close (WM_CLOSE to their top-level windows) and waits a few
//! seconds. A process that refuses earns the flow's ONE permitted
//! interruption — a plain "<App> is still running." choice between
//! **Close it and update** and **Cancel**. After a successful update
//! the app's primary executable is relaunched if it had been
//! running. Everything process- and window-shaped is Windows-only
//! and Unverified (see the 5.1.0 changelog); the decision and the
//! path matching underneath it are host-tested.

use std::cmp::Ordering;
use std::path::Path;

use crate::overlay::OverlayManifestHeader;
use crate::receipt::{compare_versions, read_receipt};

/// What a starting installer should do about an existing install.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateDecision {
    /// Nothing of this app is installed at the target: the normal
    /// first-run wizard.
    FirstRun,
    /// The same app is installed and its version is older than or
    /// equal to the package's: update silently.
    SilentUpdate { previous_version: String },
    /// The installed version is NEWER than the package: not an
    /// update — the normal flow (and its downgrade refusal) stands.
    RefuseDowngrade { installed_version: String },
}

/// The whole silent-update decision, pure: an installed version (the
/// caller supplies the receipt's, already matched to this app) and
/// the package's version. `None` installed means no receipt for this
/// app at the target.
pub fn decide_update(installed_version: Option<&str>, package_version: &str) -> UpdateDecision {
    match installed_version {
        None => UpdateDecision::FirstRun,
        Some(installed) => match compare_versions(package_version, installed) {
            Ordering::Less => UpdateDecision::RefuseDowngrade {
                installed_version: installed.to_string(),
            },
            _ => UpdateDecision::SilentUpdate {
                previous_version: installed.to_string(),
            },
        },
    }
}

/// [`decide_update`] fed from the install receipt at `dest`: the
/// receipt decides (same app identity as the upgrade logic — its
/// `app` must equal the package's app name). A receipt for a
/// different app, or none, is a first run.
pub fn decide_for_install(dest: &Path, manifest: &OverlayManifestHeader) -> UpdateDecision {
    let installed = read_receipt(dest)
        .filter(|r| r.app == manifest.app_name())
        .map(|r| r.version);
    decide_update(installed.as_deref(), &manifest.app_version())
}

/// Whether a process image path lives inside `folder`, compared the
/// Windows way (case-insensitive, either separator, on a path
/// boundary) as pure string work so it is host-testable. A process
/// whose image IS the folder path itself does not count (a folder is
/// not an executable); anything strictly below it does.
pub fn image_path_in_folder(image: &str, folder: &str) -> bool {
    let norm = |s: &str| s.replace('\\', "/").trim_end_matches('/').to_lowercase();
    let (image, folder) = (norm(image), norm(folder));
    !folder.is_empty()
        && image.len() > folder.len()
        && image.starts_with(&folder)
        && image.as_bytes()[folder.len()] == b'/'
}

/// How a silent-update run ended, for the caller (the wizard entry
/// point in `main.rs`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SilentUpdateRun {
    /// The update window ran the flow to an end (updated, failed and
    /// shown, or cancelled at the still-running choice): exit with
    /// this code; the wizard does not open.
    Completed(i32),
    /// The full installer should open instead — off Windows (there
    /// is no window to run), or the user picked **Open the full
    /// installer** on the update window's failure state.
    OpenFullInstaller,
}

/// Runs the silent update for the package at `exe_path` into `dest`
/// (already decided [`UpdateDecision::SilentUpdate`] by the caller).
/// `primary_rel` is the package's primary-executable path relative
/// to `dest`, used for the was-running relaunch.
#[cfg(windows)]
pub fn run_silent_update(
    manifest: &OverlayManifestHeader,
    exe_path: &Path,
    dest: &Path,
    primary_rel: Option<&str>,
) -> SilentUpdateRun {
    imp::run(manifest, exe_path, dest, primary_rel)
}

/// Off Windows there is no update window; the caller falls through
/// to the normal wizard (which itself reports the platform error).
#[cfg(not(windows))]
pub fn run_silent_update(
    _manifest: &OverlayManifestHeader,
    _exe_path: &Path,
    _dest: &Path,
    _primary_rel: Option<&str>,
) -> SilentUpdateRun {
    SilentUpdateRun::OpenFullInstaller
}

#[cfg(windows)]
mod imp {
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};

    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    use windows_sys::Win32::UI::Controls::DRAWITEMSTRUCT;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use super::SilentUpdateRun;
    use crate::install::{perform_install_with, InstallRequest, ProgressEvent};
    use crate::overlay::OverlayManifestHeader;
    use crate::theme;

    // K32EnumProcesses lives in kernel32; windows-sys keeps it
    // behind the ProcessStatus feature this crate does not enable,
    // so it is declared here (the native_dialog.rs pattern).
    #[link(name = "kernel32")]
    extern "system" {
        fn K32EnumProcesses(lpidProcess: *mut u32, cb: u32, lpcbNeeded: *mut u32) -> i32;
    }

    const STILL_ACTIVE: u32 = 259;
    const CLOSE_WAIT: Duration = Duration::from_secs(5);
    const FORCE_WAIT: Duration = Duration::from_secs(3);
    const AUTO_CLOSE_MS: u32 = 5000;

    const ID_LBL_STATUS: usize = 10;
    const ID_PROGRESS: usize = 11;
    const ID_LBL_DETAIL: usize = 12;
    const ID_BTN_CLOSE_IT: usize = 13;
    const ID_BTN_STILL_CANCEL: usize = 14;
    const ID_BTN_CLOSE: usize = 15;
    const ID_BTN_WIZARD: usize = 16;

    const WM_APP_PROGRESS: u32 = 0x8000 + 21;
    const WM_APP_STILL_RUNNING: u32 = 0x8000 + 22;
    const WM_APP_DONE: u32 = 0x8000 + 23;
    const TIMER_AUTO_CLOSE: usize = 1;

    const PBM_SETRANGE32_MSG: u32 = 0x0400 + 6;
    const PBM_SETPOS_MSG: u32 = 0x0400 + 2;
    const PBM_SETBARCOLOR_MSG: u32 = 0x0400 + 9;
    const PBM_SETBKCOLOR_MSG: u32 = 0x2001;
    const BS_OWNERDRAW: u32 = 0x000B;
    const DM_SETDEFID_MSG: u32 = 0x0400 + 1;

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

    /// What the worker thread hands the window at the end.
    #[derive(Debug)]
    enum WorkerOutcome {
        Updated,
        Failed(String),
        /// The user cancelled at the still-running choice; nothing
        /// was changed.
        Abandoned,
    }

    struct UpdateState {
        app_name: String,
        app_version: String,
        /// (bytes done, total bytes) from the install operation.
        progress: Arc<Mutex<(u64, u64)>>,
        outcome: Arc<Mutex<Option<WorkerOutcome>>>,
        /// UI → worker: the user picked "Close it and update".
        force_close: Arc<AtomicBool>,
        /// UI → worker: the user picked Cancel at the still-running
        /// choice.
        abandon: Arc<AtomicBool>,
        /// Set by the UI thread when the failure state's "Open the
        /// full installer" was picked.
        open_wizard: bool,
        /// The process exit code for a completed run: 1 when the
        /// update failed (the error was shown in the window), else 0
        /// (updated, or cancelled before anything changed).
        exit_code: i32,
        /// The window's phase, for close-button routing:
        /// 0 updating, 1 still-running choice shown, 2 finished.
        phase: u8,
    }

    static STATE: Mutex<Option<UpdateState>> = Mutex::new(None);
    static MAIN_HWND: Mutex<usize> = Mutex::new(0);
    static mut HEADER_FONT: HFONT = std::ptr::null_mut();
    static mut BODY_FONT: HFONT = std::ptr::null_mut();

    /// One running process whose executable lives in the install
    /// folder.
    struct RunningProcess {
        pid: u32,
        image: String,
    }

    fn process_image(pid: u32) -> Option<String> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return None;
            }
            let mut buf = vec![0u16; 1024];
            let mut size = buf.len() as u32;
            let ok = QueryFullProcessImageNameW(handle, 0, buf.as_mut_ptr(), &mut size);
            CloseHandle(handle);
            if ok == 0 {
                return None;
            }
            Some(String::from_utf16_lossy(&buf[..size as usize]))
        }
    }

    fn process_alive(pid: u32) -> bool {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if handle.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = GetExitCodeProcess(handle, &mut code);
            CloseHandle(handle);
            ok != 0 && code == STILL_ACTIVE
        }
    }

    /// Every process (other than this installer itself) whose
    /// executable lives in `dest`.
    fn processes_in_folder(dest: &Path) -> Vec<RunningProcess> {
        let folder = dest.to_string_lossy().into_owned();
        let own_pid = std::process::id();
        let mut pids = vec![0u32; 2048];
        let mut needed: u32 = 0;
        let ok = unsafe {
            K32EnumProcesses(
                pids.as_mut_ptr(),
                (pids.len() * std::mem::size_of::<u32>()) as u32,
                &mut needed,
            )
        };
        if ok == 0 {
            return Vec::new();
        }
        let count = (needed as usize / std::mem::size_of::<u32>()).min(pids.len());
        let mut found = Vec::new();
        for &pid in &pids[..count] {
            if pid == 0 || pid == own_pid {
                continue;
            }
            if let Some(image) = process_image(pid) {
                if super::image_path_in_folder(&image, &folder) {
                    found.push(RunningProcess { pid, image });
                }
            }
        }
        found
    }

    struct CloseSearch {
        pids: Vec<u32>,
    }

    unsafe extern "system" fn enum_close_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
        let search = &*(lparam as *const CloseSearch);
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);
        if search.pids.contains(&pid) {
            PostMessageW(hwnd, WM_CLOSE, 0, 0);
        }
        1
    }

    /// Asks every top-level window of the given processes to close —
    /// a request, never a kill: the app gets its normal close path
    /// (save prompts included).
    fn ask_to_close(processes: &[RunningProcess]) {
        if processes.is_empty() {
            return;
        }
        let search = CloseSearch {
            pids: processes.iter().map(|p| p.pid).collect(),
        };
        unsafe {
            EnumWindows(
                Some(enum_close_proc),
                &search as *const CloseSearch as LPARAM,
            );
        }
    }

    fn terminate(pid: u32) {
        unsafe {
            let handle = OpenProcess(
                PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION,
                0,
                pid,
            );
            if !handle.is_null() {
                TerminateProcess(handle, 1);
                CloseHandle(handle);
            }
        }
    }

    fn wait_until_gone(pids: &[u32], budget: Duration) {
        let start = Instant::now();
        while start.elapsed() < budget {
            if pids.iter().all(|&pid| !process_alive(pid)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }

    fn post(msg: u32) {
        let hwnd = *MAIN_HWND.lock().unwrap_or_else(|e| e.into_inner()) as HWND;
        if !hwnd.is_null() {
            unsafe {
                PostMessageW(hwnd, msg, 0, 0);
            }
        }
    }

    /// The worker: close-the-app dance, the shared install operation
    /// with progress, the relaunch. Every ending posts WM_APP_DONE
    /// with the outcome stored in the shared state.
    #[allow(clippy::too_many_arguments)]
    fn worker(
        exe_path: PathBuf,
        dest: PathBuf,
        primary_rel: Option<String>,
        progress: Arc<Mutex<(u64, u64)>>,
        outcome: Arc<Mutex<Option<WorkerOutcome>>>,
        force_close: Arc<AtomicBool>,
        abandon: Arc<AtomicBool>,
    ) {
        let finish = |result: WorkerOutcome| {
            if let Ok(mut g) = outcome.lock() {
                *g = Some(result);
            }
            post(WM_APP_DONE);
        };

        // 1. If the app is running, ask it to close and wait.
        let running = processes_in_folder(&dest);
        let primary_path = primary_rel
            .as_ref()
            .map(|rel| dest.join(rel.replace('\\', "/")));
        let primary_running = match &primary_path {
            Some(path) => {
                let target = path.to_string_lossy().into_owned();
                running
                    .iter()
                    .any(|p| p.image.eq_ignore_ascii_case(&target))
            }
            None => false,
        };
        if !running.is_empty() {
            ask_to_close(&running);
            wait_until_gone(
                &running.iter().map(|p| p.pid).collect::<Vec<_>>(),
                CLOSE_WAIT,
            );
            let mut remaining: Vec<u32> = running
                .iter()
                .map(|p| p.pid)
                .filter(|&pid| process_alive(pid))
                .collect();
            if !remaining.is_empty() {
                // The flow's one permitted interruption.
                post(WM_APP_STILL_RUNNING);
                loop {
                    if abandon.load(AtomicOrdering::Relaxed) {
                        finish(WorkerOutcome::Abandoned);
                        return;
                    }
                    if force_close.load(AtomicOrdering::Relaxed) {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                for &pid in &remaining {
                    terminate(pid);
                }
                wait_until_gone(&remaining, FORCE_WAIT);
                remaining.retain(|&pid| process_alive(pid));
                if !remaining.is_empty() {
                    finish(WorkerOutcome::Failed(
                        "The program is still running and could not be closed, so the update stopped before anything was changed. Close it yourself, then run this installer again."
                            .to_string(),
                    ));
                    return;
                }
            }
        }

        // 2. The update itself: the shared install operation, whose
        // journal rolls everything back on any failure.
        let progress_sink = progress.clone();
        let mut on_progress = move |ev: &ProgressEvent| {
            if let Ok(mut g) = progress_sink.lock() {
                *g = (ev.bytes_done, ev.total_bytes);
            }
            post(WM_APP_PROGRESS);
        };
        let result = perform_install_with(
            &exe_path,
            &dest,
            &InstallRequest::default(),
            Some(&mut on_progress),
            None,
        );

        match result {
            Ok(_) => {
                // 3. Relaunch the app if it had been running.
                if primary_running {
                    if let Some(path) = &primary_path {
                        let _ = crate::install::launch_program(path);
                    }
                }
                finish(WorkerOutcome::Updated);
            }
            Err(e) => finish(WorkerOutcome::Failed(e)),
        }
    }

    pub fn run(
        manifest: &OverlayManifestHeader,
        exe_path: &Path,
        dest: &Path,
        primary_rel: Option<&str>,
    ) -> SilentUpdateRun {
        let progress = Arc::new(Mutex::new((0u64, 0u64)));
        let outcome: Arc<Mutex<Option<WorkerOutcome>>> = Arc::new(Mutex::new(None));
        let force_close = Arc::new(AtomicBool::new(false));
        let abandon = Arc::new(AtomicBool::new(false));
        if let Ok(mut g) = STATE.lock() {
            *g = Some(UpdateState {
                app_name: manifest.app_name(),
                app_version: manifest.app_version(),
                progress: progress.clone(),
                outcome: outcome.clone(),
                force_close: force_close.clone(),
                abandon: abandon.clone(),
                open_wizard: false,
                exit_code: 0,
                phase: 0,
            });
        }
        unsafe {
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
                BODY_FONT = CreateFontW(
                    -15,
                    0,
                    0,
                    0,
                    FW_NORMAL as i32,
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
            let class_name: Vec<u16> = "SeanPakUpdateWindowClass\0".encode_utf16().collect();
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: LoadIconW(std::ptr::null_mut(), IDI_APPLICATION),
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: theme::brush(theme::BACKGROUND),
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: std::ptr::null_mut(),
            };
            if RegisterClassExW(&wc) == 0 {
                let err = GetLastError();
                if err != 1410 {
                    crate::diag::log("update", &format!("RegisterClassExW failed: error {err}"));
                    return SilentUpdateRun::OpenFullInstaller;
                }
            }
            let app_name = manifest.app_name();
            let title: Vec<u16> = format!("{app_name} Setup\0").encode_utf16().collect();
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                560,
                250,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                crate::diag::log("update", "window creation failed");
                return SilentUpdateRun::OpenFullInstaller;
            }
            if let Ok(mut g) = MAIN_HWND.lock() {
                *g = hwnd as usize;
            }
            create_controls(hwnd, instance);
            ShowWindow(hwnd, SW_SHOWDEFAULT);
            UpdateWindow(hwnd);

            // The worker runs the close-dance + install off the UI
            // thread; the window only paints what it reports.
            let worker_args = (
                exe_path.to_path_buf(),
                dest.to_path_buf(),
                primary_rel.map(|r| r.to_string()),
                progress,
                outcome,
                force_close,
                abandon,
            );
            std::thread::spawn(move || {
                let (exe, dest, primary, progress, outcome, force, abandon) = worker_args;
                worker(exe, dest, primary, progress, outcome, force, abandon);
            });

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
        let (open_wizard, exit_code) = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| (s.open_wizard, s.exit_code)))
            .unwrap_or((false, 0));
        if open_wizard {
            SilentUpdateRun::OpenFullInstaller
        } else {
            SilentUpdateRun::Completed(exit_code)
        }
    }

    unsafe fn create_controls(parent: HWND, instance: HINSTANCE) {
        let static_class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let button_class: Vec<u16> = "BUTTON\0".encode_utf16().collect();
        let progress_class: Vec<u16> = "msctls_progress32\0".encode_utf16().collect();
        let app_name = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.app_name.clone()))
            .unwrap_or_else(|| "App".to_string());
        let mk = |class: &[u16],
                  text: &str,
                  style: u32,
                  x: i32,
                  y: i32,
                  w: i32,
                  h: i32,
                  id: usize|
         -> HWND {
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
        let status = mk(
            &static_class,
            &format!("Updating {app_name}…"),
            0,
            20,
            18,
            500,
            28,
            ID_LBL_STATUS,
        );
        SendMessageW(status, WM_SETFONT, HEADER_FONT as WPARAM, 1);
        let bar = mk(&progress_class, "", 0, 20, 62, 500, 18, ID_PROGRESS);
        SendMessageW(bar, PBM_SETRANGE32_MSG, 0, 1000);
        SendMessageW(bar, PBM_SETBARCOLOR_MSG, 0, theme::PROGRESS as LPARAM);
        SendMessageW(bar, PBM_SETBKCOLOR_MSG, 0, theme::SURFACE as LPARAM);
        let detail = mk(&static_class, "", 0, 20, 92, 500, 56, ID_LBL_DETAIL);
        SendMessageW(detail, WM_SETFONT, BODY_FONT as WPARAM, 1);
        // The still-running choice and the end-state buttons exist
        // from the start but stay hidden until their phase.
        for (text, id, x, w) in [
            ("Close it and update", ID_BTN_CLOSE_IT, 20, 190),
            ("Cancel", ID_BTN_STILL_CANCEL, 222, 110),
            ("Open the full installer", ID_BTN_WIZARD, 178, 190),
            ("Close", ID_BTN_CLOSE, 380, 140),
        ] {
            let btn = mk(
                &button_class,
                text,
                WS_TABSTOP | BS_OWNERDRAW,
                x,
                164,
                w,
                32,
                id,
            );
            SendMessageW(btn, WM_SETFONT, BODY_FONT as WPARAM, 1);
            ShowWindow(btn, SW_HIDE);
        }
        SendMessageW(parent, DM_SETDEFID_MSG, ID_BTN_CLOSE_IT, 0);
    }

    unsafe fn set_text(hwnd: HWND, id: usize, text: &str) {
        let ctrl = GetDlgItem(hwnd, id as i32);
        if !ctrl.is_null() {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            SetWindowTextW(ctrl, wide.as_ptr());
        }
    }

    unsafe fn show(hwnd: HWND, id: usize, visible: bool) {
        let ctrl = GetDlgItem(hwnd, id as i32);
        if !ctrl.is_null() {
            ShowWindow(ctrl, if visible { SW_SHOW } else { SW_HIDE });
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
                if id == ID_LBL_DETAIL {
                    SetTextColor(hdc, theme::MUTED);
                } else {
                    SetTextColor(hdc, theme::FOREGROUND);
                }
                theme::brush(theme::BACKGROUND) as LRESULT
            }
            WM_DRAWITEM => {
                let dis = lparam as *const DRAWITEMSTRUCT;
                if dis.is_null() {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                let dis = &*dis;
                match dis.CtlID as usize {
                    ID_BTN_CLOSE_IT => theme::draw_push_button(dis, true),
                    ID_BTN_STILL_CANCEL | ID_BTN_CLOSE | ID_BTN_WIZARD => {
                        theme::draw_push_button(dis, false)
                    }
                    _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
                }
                1
            }
            WM_APP_PROGRESS => {
                let (done, total) = STATE
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().and_then(|s| s.progress.lock().ok().map(|p| *p)))
                    .unwrap_or((0, 0));
                let bar = GetDlgItem(hwnd, ID_PROGRESS as i32);
                if !bar.is_null() && total > 0 {
                    let pos = ((done.saturating_mul(1000)) / total.max(1)) as usize;
                    SendMessageW(bar, PBM_SETPOS_MSG, pos.min(1000), 0);
                }
                0
            }
            WM_APP_STILL_RUNNING => {
                if let Ok(mut g) = STATE.lock() {
                    if let Some(s) = g.as_mut() {
                        s.phase = 1;
                    }
                }
                let app_name = STATE
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().map(|s| s.app_name.clone()))
                    .unwrap_or_else(|| "The program".to_string());
                set_text(
                    hwnd,
                    ID_LBL_STATUS,
                    &format!("{app_name} is still running."),
                );
                set_text(
                    hwnd,
                    ID_LBL_DETAIL,
                    &format!(
                        "The update needs {app_name} to close first. Close it yourself, or let the installer close it for you."
                    ),
                );
                show(hwnd, ID_BTN_CLOSE_IT, true);
                show(hwnd, ID_BTN_STILL_CANCEL, true);
                0
            }
            WM_APP_DONE => {
                let (result, app_version) = {
                    let guard = STATE.lock();
                    match guard {
                        Ok(g) => match g.as_ref() {
                            Some(s) => {
                                let taken = s.outcome.lock().ok().and_then(|mut o| o.take());
                                (taken, s.app_version.clone())
                            }
                            None => (None, String::new()),
                        },
                        Err(_) => (None, String::new()),
                    }
                };
                match result {
                    Some(WorkerOutcome::Updated) => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(s) = g.as_mut() {
                                s.phase = 2;
                            }
                        }
                        set_text(hwnd, ID_LBL_STATUS, &format!("Updated to {app_version}."));
                        set_text(hwnd, ID_LBL_DETAIL, "");
                        let bar = GetDlgItem(hwnd, ID_PROGRESS as i32);
                        if !bar.is_null() {
                            SendMessageW(bar, PBM_SETPOS_MSG, 1000, 0);
                        }
                        show(hwnd, ID_BTN_CLOSE, true);
                        SetTimer(hwnd, TIMER_AUTO_CLOSE, AUTO_CLOSE_MS, None);
                    }
                    Some(WorkerOutcome::Failed(message)) => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(s) = g.as_mut() {
                                s.phase = 2;
                                s.exit_code = 1;
                            }
                        }
                        set_text(hwnd, ID_LBL_STATUS, "The update did not finish.");
                        set_text(hwnd, ID_LBL_DETAIL, &message);
                        show(hwnd, ID_BTN_WIZARD, true);
                        show(hwnd, ID_BTN_CLOSE, true);
                    }
                    Some(WorkerOutcome::Abandoned) | None => {
                        DestroyWindow(hwnd);
                    }
                }
                0
            }
            WM_TIMER => {
                if wparam == TIMER_AUTO_CLOSE {
                    KillTimer(hwnd, TIMER_AUTO_CLOSE);
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_COMMAND => {
                match wparam & 0xFFFF {
                    ID_BTN_CLOSE_IT => {
                        if let Ok(g) = STATE.lock() {
                            if let Some(s) = g.as_ref() {
                                s.force_close.store(true, AtomicOrdering::Relaxed);
                            }
                        }
                        let app_name = STATE
                            .lock()
                            .ok()
                            .and_then(|g| g.as_ref().map(|s| s.app_name.clone()))
                            .unwrap_or_else(|| "App".to_string());
                        show(hwnd, ID_BTN_CLOSE_IT, false);
                        show(hwnd, ID_BTN_STILL_CANCEL, false);
                        set_text(hwnd, ID_LBL_STATUS, &format!("Updating {app_name}…"));
                        set_text(hwnd, ID_LBL_DETAIL, "");
                    }
                    ID_BTN_STILL_CANCEL => {
                        if let Ok(g) = STATE.lock() {
                            if let Some(s) = g.as_ref() {
                                s.abandon.store(true, AtomicOrdering::Relaxed);
                            }
                        }
                        // The worker notices and posts DONE
                        // (Abandoned), which closes the window.
                        show(hwnd, ID_BTN_CLOSE_IT, false);
                        show(hwnd, ID_BTN_STILL_CANCEL, false);
                    }
                    ID_BTN_CLOSE => {
                        DestroyWindow(hwnd);
                    }
                    ID_BTN_WIZARD => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(s) = g.as_mut() {
                                s.open_wizard = true;
                            }
                        }
                        DestroyWindow(hwnd);
                    }
                    _ => {}
                }
                0
            }
            WM_CLOSE => {
                // Never orphan the worker mid-update: the window
                // stays until the flow ends. In the still-running
                // phase, closing means Cancel.
                let phase = STATE
                    .lock()
                    .ok()
                    .and_then(|g| g.as_ref().map(|s| s.phase))
                    .unwrap_or(0);
                match phase {
                    2 => {
                        DestroyWindow(hwnd);
                        0
                    }
                    1 => {
                        if let Ok(g) = STATE.lock() {
                            if let Some(s) = g.as_ref() {
                                s.abandon.store(true, AtomicOrdering::Relaxed);
                            }
                        }
                        show(hwnd, ID_BTN_CLOSE_IT, false);
                        show(hwnd, ID_BTN_STILL_CANCEL, false);
                        0
                    }
                    _ => 0,
                }
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

    #[test]
    fn update_decision_covers_version_orderings() {
        // No receipt for this app: first run.
        assert_eq!(decide_update(None, "2.0.0"), UpdateDecision::FirstRun);
        // Older installed: silent update.
        assert_eq!(
            decide_update(Some("1.0.0"), "2.0.0"),
            UpdateDecision::SilentUpdate {
                previous_version: "1.0.0".to_string()
            }
        );
        // Same version: still the silent path (a reinstall/refresh).
        assert_eq!(
            decide_update(Some("2.0.0"), "2.0.0"),
            UpdateDecision::SilentUpdate {
                previous_version: "2.0.0".to_string()
            }
        );
        // Newer installed: not an update — the downgrade refusal
        // stands in the normal flow.
        assert_eq!(
            decide_update(Some("2.0.1"), "2.0.0"),
            UpdateDecision::RefuseDowngrade {
                installed_version: "2.0.1".to_string()
            }
        );
        // Pre-release ordering follows compare_versions: a release
        // package over a pre-release install is an update; the
        // reverse is a downgrade.
        assert!(matches!(
            decide_update(Some("2.0.0-beta"), "2.0.0"),
            UpdateDecision::SilentUpdate { .. }
        ));
        assert!(matches!(
            decide_update(Some("2.0.0"), "2.0.0-beta"),
            UpdateDecision::RefuseDowngrade { .. }
        ));
    }

    #[test]
    fn folder_matching_is_boundary_aware_and_case_insensitive() {
        let dir = "C:\\Users\\x\\AppData\\Local\\Programs\\My App";
        assert!(image_path_in_folder(
            "C:\\Users\\x\\AppData\\Local\\Programs\\My App\\app.exe",
            dir
        ));
        assert!(image_path_in_folder(
            "c:/users/x/appdata/local/programs/my app/sub/tool.exe",
            dir
        ));
        // A sibling folder sharing a prefix is NOT inside.
        assert!(!image_path_in_folder(
            "C:\\Users\\x\\AppData\\Local\\Programs\\My App2\\app.exe",
            dir
        ));
        // The folder itself and unrelated paths are not inside.
        assert!(!image_path_in_folder(dir, dir));
        assert!(!image_path_in_folder("C:\\Windows\\app.exe", dir));
        assert!(!image_path_in_folder("C:\\Apps\\app.exe", ""));
    }

    #[test]
    fn receipt_for_another_app_is_a_first_run() {
        use crate::overlay::OverlayManifestHeader;
        use crate::receipt::RECEIPT_FILE_NAME;

        fn header(app: &str, version: &str) -> OverlayManifestHeader {
            let mut h = OverlayManifestHeader {
                magic: [0; 16],
                version: 0,
                file_count: 0,
                primary_exe_index: 0,
                total_uncompressed_bytes: 0,
                app_name_utf8: [0; 128],
                app_version_utf8: [0; 32],
                publisher_utf8: [0; 128],
            };
            h.app_name_utf8[..app.len()].copy_from_slice(app.as_bytes());
            h.app_version_utf8[..version.len()].copy_from_slice(version.as_bytes());
            h
        }

        let root =
            std::env::temp_dir().join(format!("seanpak-update-decision-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let manifest = header("My App", "2.0.0");

        // No receipt at all: first run.
        assert_eq!(
            decide_for_install(&root, &manifest),
            UpdateDecision::FirstRun
        );
        // A receipt for a DIFFERENT app at this folder: first run —
        // identity is the receipt's app name, not the folder.
        std::fs::write(
            root.join(RECEIPT_FILE_NAME),
            r#"{"app":"Other App","version":"1.0.0"}"#,
        )
        .unwrap();
        assert_eq!(
            decide_for_install(&root, &manifest),
            UpdateDecision::FirstRun
        );
        // The same app, older: silent update. Newer: refusal path.
        std::fs::write(
            root.join(RECEIPT_FILE_NAME),
            r#"{"app":"My App","version":"1.5.0"}"#,
        )
        .unwrap();
        assert_eq!(
            decide_for_install(&root, &manifest),
            UpdateDecision::SilentUpdate {
                previous_version: "1.5.0".to_string()
            }
        );
        std::fs::write(
            root.join(RECEIPT_FILE_NAME),
            r#"{"app":"My App","version":"3.0.0"}"#,
        )
        .unwrap();
        assert_eq!(
            decide_for_install(&root, &manifest),
            UpdateDecision::RefuseDowngrade {
                installed_version: "3.0.0".to_string()
            }
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
