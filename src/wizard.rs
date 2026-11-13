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
//! One v4.8.0 addition shares the same pages: the **elevation
//! offer**. When the verdict fails and this process is not elevated,
//! the Review page also offers **Restart as administrator** (see
//! [`crate::elevation`]) beside the per-user switch, with the honest
//! note that shortcuts and settings will then belong to the
//! administrator account Windows elevates with. The relaunch
//! preserves the original arguments (and an applied preset's path);
//! declining the UAC prompt leaves the wizard open with a plain
//! note. An instance carrying the `--elevated-relaunch` marker
//! never re-offers.
//!
//! Two v7 additions share the same pages. **Modify mode**
//! (`InstallWizard::modify`, reached from a bare `--modify` or the
//! ARP entry's Modify button) re-opens the wizard on an existing
//! install: the pages seed from the install's receipt (its current
//! options, component selection, and login entry), the location is
//! fixed, and the primary button — "Apply changes" — runs
//! `crate::modify::perform_modify`. And the **answer-file offer**: an
//! `answers.json` found beside the Setup.exe is offered on the Review
//! page (Apply preset / Ignore) and applied only on the user's click.
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

use crate::overlay::{FileRecordHeader, InstallOptions, OverlayManifestHeader, PowerSpec};

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
    power: &PowerSpec,
    selection: &[String],
) -> Vec<[String; 3]> {
    let primary = records
        .get(manifest.primary_exe_index as usize)
        .map(|r| r.relative_path());
    // With declared components (v6) the Components row reports the
    // selection ("2 of 3 parts"); required parts then get their own
    // informational rows below it. Without components it keeps the
    // plain file summary.
    let components = if power.components.is_empty() {
        match (records.len(), primary) {
            (0, _) => "No files".to_string(),
            (1, Some(p)) => p,
            (n, Some(p)) => format!("{p} + {} more files", n - 1),
            (n, None) => format!("{n} files"),
        }
    } else {
        let total = power.part_ids(power.has_core_files(records)).len();
        format!("{} of {total} parts", selection.len())
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
    ];
    // Required components (v6) are informational rows: they are
    // always installed, so there is nothing to change.
    for component in power.components.iter().filter(|c| c.required) {
        rows.push([
            format!("Part: {}", component.name),
            "Included (required)".to_string(),
            String::new(),
        ]);
    }
    rows.extend([
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
    ]);
    // Journaled writes (v6) summarize as count rows when the package
    // declares any — informational; the receipt records the details.
    if !power.registry_writes.is_empty() {
        rows.push([
            "Registry writes".to_string(),
            format!("{} values", power.registry_writes.len()),
            String::new(),
        ]);
    }
    if !power.env_writes.is_empty() {
        rows.push([
            "Environment variables".to_string(),
            format!("{} variables", power.env_writes.len()),
            String::new(),
        ]);
    }
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

/// How the uninstall confirmation window ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UninstallConfirmation {
    /// Cancelled or closed — the caller must remove nothing.
    Cancelled,
    /// Confirmed; `purge_data` is the data checkbox's final state.
    Proceed { purge_data: bool },
    /// The user picked **Restart as administrator** and the elevated
    /// instance was handed off successfully — this process removes
    /// nothing and exits; the elevated copy runs the whole
    /// confirmation again under the administrator token.
    RelaunchedElevated,
}

/// The uninstall confirmation window, in the wizard's visual language:
/// names what uninstall removes and — when the install recorded data
/// directories — offers a checkbox to delete the app's data files
/// too, listing the exact folders and warning that deleted data
/// cannot be recovered. The checkbox *starts* in the state
/// `purge_default` carries (v7: the install receipt's recorded
/// default, set at install time by an answer file's
/// `purgeDataOnUninstall`); it is unchecked for every install that
/// did not opt in, and the user always makes the final call here.
///
/// When `offer_elevation` is set (v4.8.0: the install folder is not
/// writable by this account and the elevation decision offers the
/// restart — see [`crate::elevation`]), the window also shows a
/// **Restart as administrator** button with the honest note that
/// the uninstall would then run as the administrator account.
/// Declining the UAC prompt keeps the window open with a plain
/// note; a successful hand-off returns
/// [`UninstallConfirmation::RelaunchedElevated`].
///
/// The window is a view over the shared uninstall operation
/// ([`crate::uninstall::perform_uninstall`]); it decides nothing
/// else.
///
/// Off Windows there is no native window; the function proceeds with
/// the safe default (uninstall, keep data).
#[cfg(windows)]
pub fn confirm_uninstall(
    app_name: &str,
    data_dirs: &[PathBuf],
    purge_default: bool,
    offer_elevation: bool,
) -> UninstallConfirmation {
    imp::confirm_uninstall(app_name, data_dirs, purge_default, offer_elevation)
}

/// Off Windows there is no native window; proceeds with the safe
/// default (uninstall, keep data). See the Windows form.
#[cfg(not(windows))]
pub fn confirm_uninstall(
    _app_name: &str,
    _data_dirs: &[PathBuf],
    _purge_default: bool,
    _offer_elevation: bool,
) -> UninstallConfirmation {
    UninstallConfirmation::Proceed { purge_data: false }
}

/// The Rail-layout installer wizard for one overlay package.
pub struct InstallWizard {
    manifest: OverlayManifestHeader,
    records: Vec<FileRecordHeader>,
    options: InstallOptions,
    exe_path: PathBuf,
    power: PowerSpec,
    /// Modify mode (v7): the existing install's folder. The wizard
    /// then seeds its pages from that install's receipt and its
    /// primary button runs the shared modify operation instead of an
    /// install.
    modify_dir: Option<PathBuf>,
    /// An answer file to apply before the first page shows (v4.8.0):
    /// set only for an elevated relaunch whose unelevated instance
    /// had already applied the preset on the user's click — the
    /// choices are the user's, carried across the hand-off, never a
    /// silent first-run application.
    preset_answers_path: Option<PathBuf>,
    /// The install location the unelevated instance was showing
    /// when the user clicked Restart as administrator (v4.8.0): the
    /// elevated instance opens on the folder the user actually
    /// chose, not the package default. Install mode only.
    relaunch_location: Option<PathBuf>,
}

impl InstallWizard {
    pub fn new(
        manifest: OverlayManifestHeader,
        records: Vec<FileRecordHeader>,
        options: InstallOptions,
        exe_path: PathBuf,
        power: PowerSpec,
    ) -> Self {
        Self {
            manifest,
            records,
            options,
            exe_path,
            power,
            modify_dir: None,
            preset_answers_path: None,
            relaunch_location: None,
        }
    }

    /// Carries an already-applied answer file across an elevation
    /// relaunch (see the field docs). The wizard applies it before
    /// the Review page shows, exactly as the user's earlier Apply
    /// click did in the unelevated instance.
    pub fn with_preset_answers(mut self, path: PathBuf) -> Self {
        self.preset_answers_path = Some(path);
        self
    }

    /// Carries the chosen install location across an elevation
    /// relaunch (see the field docs).
    pub fn with_relaunch_location(mut self, dir: PathBuf) -> Self {
        self.relaunch_location = Some(dir);
        self
    }

    /// The wizard in Modify mode for the install at `dest` (its
    /// receipt must be there — the caller checks first and reports
    /// the plain refusal when it is not).
    pub fn modify(
        manifest: OverlayManifestHeader,
        records: Vec<FileRecordHeader>,
        options: InstallOptions,
        exe_path: PathBuf,
        power: PowerSpec,
        dest: PathBuf,
    ) -> Self {
        Self {
            manifest,
            records,
            options,
            exe_path,
            power,
            modify_dir: Some(dest),
            preset_answers_path: None,
            relaunch_location: None,
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
            std::mem::take(&mut self.power),
            self.modify_dir.clone(),
            self.preset_answers_path.clone(),
            self.relaunch_location.clone(),
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
            self.power.components.len(),
            self.modify_dir.as_ref().map(|d| d.display().to_string()),
            self.preset_answers_path
                .as_ref()
                .map(|d| d.display().to_string()),
            self.relaunch_location
                .as_ref()
                .map(|d| d.display().to_string()),
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
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        EnableWindow, IsWindowEnabled, SetFocus,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use super::{default_install_dir, review_rows, InstallOptions};
    use crate::install::{InstallOutcome, ProgressEvent};
    use crate::overlay::{FileRecordHeader, OverlayManifestHeader, PowerSpec};
    use crate::scanner::format_bytes;

    // Colors come from the shared TesselGUI 1.4.0 theme module —
    // this window keeps no private palette. (The rail takes the
    // theme's background token; the info box its accent token.)
    use crate::theme;

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

    // Options-page component checkboxes (v6): a section label plus up
    // to MAX_COMPONENT_BOXES checkboxes, one per OPTIONAL component,
    // created only when the package declares any. Packages with more
    // optional parts than boxes keep the extra parts at their current
    // selection (the CLI's --components covers every part).
    const ID_LBL_COMPONENTS: usize = 470;
    const ID_CHK_COMPONENT_BASE: usize = 471;
    const MAX_COMPONENT_BOXES: usize = 6;
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
    // The uninstall confirmation's elevation offer (v4.8.0): a note
    // line plus the Restart as administrator button, created only
    // when the caller says the offer applies.
    const ID_LBL_CONFIRM_NOTE: usize = 467;
    const ID_BTN_CONFIRM_ELEVATE: usize = 468;

    // The Review page's answer-file offer (v7): a label plus Apply /
    // Ignore, shown only while a valid beside-the-exe answers.json is
    // waiting for the user's decision (or, for an invalid one, the
    // label alone explains why it was not used).
    const ID_LBL_ANSWERS: usize = 480;
    const ID_BTN_ANSWERS_APPLY: usize = 481;
    const ID_BTN_ANSWERS_IGNORE: usize = 482;
    // The Review page's Restart as administrator button (v4.8.0):
    // sits in the footer row's free left slot (where the keyboard
    // hint normally rests) and is shown only while the elevation
    // offer applies.
    const ID_BTN_RESTART_ADMIN: usize = 483;

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
    const PBM_SETBARCOLOR_MSG: u32 = 0x0400 + 9; // WM_USER + 9
    const PBM_SETBKCOLOR_MSG: u32 = 0x2001;
    // Owner-draw button style (all buttons and checkboxes draw via
    // the theme painters), the click notification the owner-drawn
    // checkboxes toggle on, and the dialog message that keeps Enter
    // activating the primary button now that it no longer carries
    // the default-pushbutton style.
    const BS_OWNERDRAW: u32 = 0x000B;
    const BN_CLICKED_CODE: usize = 0;
    const DM_SETDEFID_MSG: u32 = 0x0400 + 1; // WM_USER + 1

    // Static-control styles (windows-sys keeps these behind the
    // SystemServices feature, which this crate does not enable).
    const SS_CENTER: u32 = 0x0001;
    const SS_RIGHT: u32 = 0x0002;
    const SS_ETCHEDHORZ: u32 = 0x0010;
    const SS_ETCHEDVERT: u32 = 0x0011;
    // Edit styles, likewise defined locally.
    const ES_AUTOHSCROLL: u32 = 0x0080;

    // NM_DBLCLK is (-3); NMHDR.code is unsigned, so compare as i32.
    const NM_DBLCLK_I32: i32 = -3;

    // EN_CHANGE notification code (high word of WM_COMMAND's wparam).
    const EN_CHANGE_CODE: usize = 0x0300;

    const READY_INFO_TEXT: &str = "Ready to install. Nothing is downloaded and nothing is sent anywhere — this installer works fully offline and can be undone from Windows Settings → Apps.";
    const UNWRITABLE_INFO_TEXT: &str = "That install folder needs administrator access, and this installer runs per user — it cannot use it. Switch to the per-user folder with the Install location row's Change action (no administrator rights needed), or choose a different location on the Options page.";
    // The same wall, with the v4.8.0 restart offer in play: both
    // remedies named, the per-user one recommended, and the honest
    // consequence of elevating — shortcuts and settings land under
    // the administrator account Windows elevates with.
    const UNWRITABLE_OFFER_INFO_TEXT: &str = "That folder needs administrator access; this installer runs per user. Recommended: the Install location row switches to the per-user folder in one click — no administrator rights needed. Or restart as administrator (below): Windows will ask permission, and shortcuts and settings will then belong to the administrator account, not yours.";

    struct WizardState {
        manifest: OverlayManifestHeader,
        records: Vec<FileRecordHeader>,
        options: InstallOptions,
        exe_path: PathBuf,
        install_dir: PathBuf,
        /// The package's v6 power block (empty for older packages).
        power: PowerSpec,
        /// The component selection the install will use (v6): starts
        /// at the resolved default (previous install's choices on an
        /// upgrade, else manifest defaults); the Options page's
        /// component checkboxes edit it.
        component_selection: Vec<String>,
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
        /// Modify mode (v7): the wizard re-opened on the install in
        /// `install_dir`; pages are seeded from its receipt and the
        /// primary button applies a modify, not an install.
        modify: bool,
        /// The finished modify's outcome (Modify mode's `finish`).
        modify_finish: Option<Result<crate::modify::ModifyOutcome, String>>,
        /// A valid beside-the-exe answer file waiting for the user's
        /// Apply / Ignore decision on the Review page (install mode
        /// only; never applied silently).
        answers: Option<crate::answers::Answers>,
        /// Why a beside-the-exe answer file was NOT offered (it
        /// exists but did not validate) — shown on the Review page.
        answers_note: Option<String>,
        /// The offer was answered (applied or ignored): the offer
        /// controls hide and the choice is final for this run.
        answers_decided: bool,
        /// The uninstall data-purge default an applied preset chose;
        /// recorded in the receipt by the install operation.
        purge_data_default: bool,
        /// The v4.8.0 elevation offer currently applies (the location
        /// verdict failed, this process is not elevated, and this
        /// instance carries no relaunch marker) — the Restart as
        /// administrator button shows and the offer's info text is
        /// in force. Recomputed by `refresh_location_state`.
        elevation_offer: bool,
        /// This process was itself started by an elevation relaunch
        /// (its arguments carry the marker): it never re-offers.
        relaunch_marker: bool,
        /// The answer file the user applied in this instance (the
        /// beside-the-exe preset's path), so an elevation relaunch
        /// can carry the applied preset across the hand-off.
        answers_applied_path: Option<PathBuf>,
    }

    static STATE: Mutex<Option<WizardState>> = Mutex::new(None);
    static MAIN_HWND: Mutex<usize> = Mutex::new(0);
    static mut HEADER_FONT: HFONT = std::ptr::null_mut();
    static mut TILE_FONT: HFONT = std::ptr::null_mut();
    static mut NAME_FONT: HFONT = std::ptr::null_mut();

    #[allow(clippy::too_many_arguments)]
    pub fn run(
        manifest: OverlayManifestHeader,
        records: Vec<FileRecordHeader>,
        options: InstallOptions,
        exe_path: PathBuf,
        power: PowerSpec,
        modify_dir: Option<PathBuf>,
        preset_answers_path: Option<PathBuf>,
        relaunch_location: Option<PathBuf>,
    ) -> Result<(), u32> {
        let modify = modify_dir.is_some();
        let mut options = options;
        let mut install_dir = default_install_dir(&options, &manifest.app_name());
        if let Some(dir) = &modify_dir {
            // Modify mode: the pages show the install's CURRENT state
            // — its receipt's recorded choices, not the package
            // defaults — and the location is that install's folder.
            install_dir = dir.clone();
            if let Some(receipt) =
                crate::receipt::read_receipt(dir).filter(|r| r.app == manifest.app_name())
            {
                options = crate::modify::options_from_receipt(&options, &receipt);
            }
        }
        // The beside-the-exe answer file (install mode only): parsed
        // now, offered on the Review page, applied only if the user
        // clicks Apply. An invalid file is explained, never used.
        //
        // The one exception is a preset carried across an elevation
        // relaunch (v4.8.0): the user already clicked Apply in the
        // unelevated instance, so the preset lands before the first
        // page — its options and location here, its component
        // selection with the starting selection below, its purge
        // default in the state.
        let mut preset_applied: Option<crate::answers::Answers> = None;
        let mut carried_preset_path: Option<PathBuf> = None;
        let (answers_offer, answers_note) = if modify {
            (None, None)
        } else if let Some(path) = &preset_answers_path {
            match crate::answers::load_answers(path) {
                Ok(parsed) => {
                    parsed.apply_to_options(&mut options);
                    if let Some(location) = &parsed.install_location {
                        install_dir = PathBuf::from(location);
                    }
                    carried_preset_path = Some(path.clone());
                    preset_applied = Some(parsed);
                    (None, None)
                }
                Err(errors) => (None, Some(errors.join("; "))),
            }
        } else {
            let candidate = exe_path
                .parent()
                .map(|p| p.join(crate::answers::ANSWERS_FILE_NAME));
            match candidate {
                Some(path) if path.is_file() => match crate::answers::load_answers(&path) {
                    Ok(parsed) => (Some(parsed), None),
                    Err(errors) => (None, Some(errors.join("; "))),
                },
                _ => (None, None),
            }
        };
        // The location the user was actually looking at when they
        // clicked Restart as administrator wins over both the
        // package default and a carried preset's location (it is
        // the newest statement of the same choice).
        if let Some(dir) = &relaunch_location {
            if !modify {
                install_dir = dir.clone();
            }
        }
        // The starting component selection (v6): the previous
        // install's choices when its receipt is at the destination,
        // else the manifest defaults — the same resolution the shared
        // operation performs, so the Review page shows the truth.
        let component_selection = {
            let previous_receipt =
                crate::receipt::read_receipt(&install_dir).filter(|r| r.app == manifest.app_name());
            let previous = previous_receipt
                .as_ref()
                .map(|r| (r.components.as_slice(), r.components_known.as_slice()));
            let primary_rel = records
                .get(manifest.primary_exe_index as usize)
                .map(|r| r.relative_path());
            power
                .resolve_selection(None, &[], previous, &records, primary_rel.as_deref())
                .unwrap_or_else(|_| {
                    power.default_selection(previous, power.has_core_files(&records))
                })
        };
        // A carried preset's component selection replaces the
        // starting selection, resolved exactly as an Apply click
        // resolves it; an unresolvable one keeps the default.
        let component_selection = match &preset_applied {
            Some(parsed)
                if parsed.components.is_some() || !parsed.without_components.is_empty() =>
            {
                let primary_rel = records
                    .get(manifest.primary_exe_index as usize)
                    .map(|r| r.relative_path());
                match power.resolve_selection(
                    parsed.components.as_deref(),
                    &parsed.without_components,
                    None,
                    &records,
                    primary_rel.as_deref(),
                ) {
                    Ok(selection) => selection,
                    Err(_) => component_selection,
                }
            }
            _ => component_selection,
        };
        let preset_purge_default = preset_applied
            .as_ref()
            .map(|p| p.purge_data_on_uninstall)
            .unwrap_or(false);
        if let Ok(mut guard) = STATE.lock() {
            *guard = Some(WizardState {
                manifest,
                records,
                options,
                exe_path,
                install_dir,
                power,
                component_selection,
                location_ok: true,
                page: 1,
                installing: false,
                installed: false,
                cancel: Arc::new(AtomicBool::new(false)),
                progress: Arc::new(Mutex::new((0, 0, String::new()))),
                finish: None,
                launch_path: None,
                modify,
                modify_finish: None,
                answers: answers_offer,
                answers_note,
                answers_decided: carried_preset_path.is_some(),
                purge_data_default: preset_purge_default,
                elevation_offer: false,
                relaunch_marker: crate::elevation::has_relaunch_marker(
                    &crate::elevation::current_process_args(),
                ),
                answers_applied_path: carried_preset_path,
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
            let window_title: Vec<u16> = if modify {
                format!("{app_name} Modify — SeanPak\0")
            } else {
                format!("{app_name} Setup — SeanPak\0")
            }
            .encode_utf16()
            .collect();

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
                hbrBackground: theme::brush(theme::BACKGROUND),
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
            // The primary button is owner-drawn now (it painted via
            // the theme), so name it the dialog default explicitly —
            // Enter keeps activating it from anywhere on the page.
            SendMessageW(hwnd, DM_SETDEFID_MSG, ID_BTN_INSTALL, 0);
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
        // No LVS_EX_GRIDLINES: the native gridline color is a fixed
        // light system color. The theme's custom draw (WM_NOTIFY)
        // paints selection and gridlines in token colors instead.
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            (LVS_EX_FULLROWSELECT) as WPARAM,
            (LVS_EX_FULLROWSELECT) as LPARAM,
        );
        SendMessageW(list, LVM_SETBKCOLOR, 0, theme::SURFACE as LPARAM);
        SendMessageW(list, LVM_SETTEXTBKCOLOR, 0, theme::SURFACE as LPARAM);
        SendMessageW(list, LVM_SETTEXTCOLOR, 0, theme::FOREGROUND as LPARAM);
        fill_list(list);

        mk(
            &static_class,
            READY_INFO_TEXT,
            0,
            174,
            366,
            674,
            58,
            ID_LBL_INFO,
        );
        // The answer-file offer row (v7): sits between the info box
        // and the status line; show_page decides its visibility and
        // text from the state's offer.
        mk(&static_class, "", 0, 174, 432, 398, 22, ID_LBL_ANSWERS);
        mk(
            &button_class,
            "Apply preset",
            WS_TABSTOP | BS_OWNERDRAW,
            584,
            430,
            120,
            26,
            ID_BTN_ANSWERS_APPLY,
        );
        mk(
            &button_class,
            "Ignore",
            WS_TABSTOP | BS_OWNERDRAW,
            712,
            430,
            120,
            26,
            ID_BTN_ANSWERS_IGNORE,
        );
        mk(&static_class, "", 0, 174, 462, 674, 22, ID_LBL_STATUS);
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
        // The elevation offer's button (v4.8.0): the footer row's
        // left slot, where the keyboard hint rests — the hint hides
        // while the button shows (refresh_location_state settles
        // both). Starts hidden; most installs never need it.
        let restart_btn = mk(
            &button_class,
            "Restart as administrator",
            WS_TABSTOP | BS_OWNERDRAW,
            174,
            496,
            262,
            30,
            ID_BTN_RESTART_ADMIN,
        );
        ShowWindow(restart_btn, SW_HIDE);
        mk(
            &button_class,
            "Back",
            WS_TABSTOP | BS_OWNERDRAW,
            448,
            496,
            120,
            30,
            ID_BTN_BACK,
        );
        mk(
            &button_class,
            "Cancel",
            WS_TABSTOP | BS_OWNERDRAW,
            580,
            496,
            120,
            30,
            ID_BTN_CANCEL,
        );
        mk(
            &button_class,
            "Install",
            WS_TABSTOP | BS_OWNERDRAW,
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
            WS_TABSTOP | BS_OWNERDRAW,
            730,
            112,
            118,
            26,
            ID_BTN_BROWSE,
        );
        mk(
            &button_class,
            "Desktop shortcut",
            WS_TABSTOP | BS_OWNERDRAW,
            174,
            154,
            500,
            24,
            ID_CHK_DESKTOP,
        );
        mk(
            &button_class,
            "Start Menu shortcuts",
            WS_TABSTOP | BS_OWNERDRAW,
            174,
            184,
            500,
            24,
            ID_CHK_STARTMENU,
        );
        mk(
            &button_class,
            "Add the install folder to PATH",
            WS_TABSTOP | BS_OWNERDRAW,
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
            WS_TABSTOP | BS_OWNERDRAW,
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
        // Component checkboxes (v6): created only when the package
        // declares OPTIONAL components (labels and check states are
        // set by fill_options_controls). The block takes the note's
        // space, so show_page hides the note when components show.
        let optional_components = STATE
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|s| s.power.components.iter().filter(|c| !c.required).count())
            })
            .unwrap_or(0);
        if optional_components > 0 {
            mk(
                &static_class,
                "Components — optional parts of this package",
                0,
                174,
                396,
                674,
                20,
                ID_LBL_COMPONENTS,
            );
            for i in 0..MAX_COMPONENT_BOXES.min(optional_components) {
                let x = 174 + (i % 2) as i32 * 338;
                let y = 418 + (i / 2) as i32 * 22;
                mk(
                    &button_class,
                    "Component",
                    WS_TABSTOP | BS_OWNERDRAW,
                    x,
                    y,
                    330,
                    22,
                    ID_CHK_COMPONENT_BASE + i,
                );
            }
        }

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
        let bar = mk(
            &progress_class,
            "",
            WS_TABSTOP,
            174,
            228,
            674,
            28,
            ID_PROGRESS,
        );
        // The progress role color on the surface token (honored
        // wherever the control paints with message colors).
        SendMessageW(bar, PBM_SETBARCOLOR_MSG, 0, theme::PROGRESS as LPARAM);
        SendMessageW(bar, PBM_SETBKCOLOR_MSG, 0, theme::SURFACE as LPARAM);

        // ---- Page 4: Finish ----
        mk(&static_class, "", 0, 174, 82, 674, 330, ID_LBL_FINISH);
        // The launch checkbox sits below the Finish text; its label
        // and state are set when a successful install arms it (and it
        // stays hidden otherwise) — see on_done_message / show_page.
        mk(
            &button_class,
            "Launch",
            WS_TABSTOP | BS_OWNERDRAW,
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
        // The Restart as administrator button defaults to hidden;
        // refresh_location_state (called below for pages 1–2) shows
        // it on the Review page while the elevation offer applies.
        show(ID_BTN_RESTART_ADMIN, false);
        // The answer-file offer (v7): the label shows while an offer
        // is pending or an invalid file needs explaining; the buttons
        // only while a valid preset awaits the user's decision.
        let (offer_pending, answers_note) = STATE
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref().map(|s| {
                    (
                        s.answers.is_some() && !s.answers_decided,
                        s.answers_note.clone(),
                    )
                })
            })
            .unwrap_or((false, None));
        show(
            ID_LBL_ANSWERS,
            page == 1 && (offer_pending || answers_note.is_some()),
        );
        show(ID_BTN_ANSWERS_APPLY, page == 1 && offer_pending);
        show(ID_BTN_ANSWERS_IGNORE, page == 1 && offer_pending);
        if page == 1 {
            if offer_pending {
                set_text(
                    hwnd,
                    ID_LBL_ANSWERS,
                    "A preset file (answers.json) was found beside this installer.",
                );
            } else if let Some(note) = &answers_note {
                set_text(
                    hwnd,
                    ID_LBL_ANSWERS,
                    &format!("The answers.json beside this installer is invalid and was not used: {note}"),
                );
            }
        }
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
        // Component checkboxes (v6) join page 2 when the package has
        // optional components — and take the options note's space.
        let optional_components = STATE
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref()
                    .map(|s| s.power.components.iter().filter(|c| !c.required).count())
            })
            .unwrap_or(0);
        show(ID_LBL_COMPONENTS, page == 2 && optional_components > 0);
        for i in 0..MAX_COMPONENT_BOXES {
            show(
                ID_CHK_COMPONENT_BASE + i,
                page == 2 && i < optional_components,
            );
        }
        if optional_components > 0 {
            show(ID_LBL_OPTNOTE, false);
        }
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
        let modify = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.modify))
            .unwrap_or(false);
        set_text(
            hwnd,
            ID_BTN_INSTALL,
            if page == 4 {
                "Close"
            } else if modify {
                "Apply changes"
            } else {
                "Install"
            },
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
                3 if modify => "Modifying",
                3 => "Installing",
                4 => "Finish",
                _ => "Review",
            },
        );
        set_text(
            hwnd,
            ID_LBL_SUBTITLE,
            match page {
                2 if modify => {
                    "The current install's choices — change any option, then apply. The location is fixed: Modify works on the install that is already there."
                }
                2 => "Change any option — install uses exactly these choices.",
                3 if modify => "Applying changes — cancelling rolls back to the current state.",
                3 => "Installing — cancelling rolls back every change.",
                4 if modify => "The changes have been applied.",
                4 => "The install has finished.",
                _ if modify => {
                    "This app is already installed here — the rows show its current state. Change parts or options, then apply."
                }
                _ => "Check the install options — change anything before you install.",
            },
        );
        update_rail(hwnd, page);
        if page == 2 {
            fill_options_controls(hwnd);
            // Modify never moves an install: the location edit and
            // its Browse button are disabled (the operation refuses
            // a different destination by construction).
            set_enabled(hwnd, ID_EDT_LOCATION, !modify);
            set_enabled(hwnd, ID_BTN_BROWSE, !modify);
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
                    s.modify,
                    s.relaunch_marker,
                )
            })
        });
        let Some((dir, app_name, recorded, was_ok, page, modify, marker)) = snapshot else {
            return;
        };
        let (ok, fallback) = match crate::install::location_verdict(&dir, &app_name, &recorded) {
            crate::install::LocationVerdict::Writable => (true, None),
            crate::install::LocationVerdict::NotWritable { fallback, .. } => {
                (false, Some(fallback))
            }
        };
        // The elevation offer (v4.8.0): the shared decision — offer
        // the restart only while the location is unwritable, this
        // process is unelevated, and this instance is not itself a
        // relaunch. The button lives on the Review page; the
        // keyboard hint yields its slot while the button shows.
        let mode = if modify {
            crate::elevation::ElevationMode::Modify
        } else {
            crate::elevation::ElevationMode::Install
        };
        let offer = crate::elevation::elevation_offered(
            ok,
            crate::elevation::is_process_elevated(),
            marker,
            mode,
        );
        if let Ok(mut g) = STATE.lock() {
            if let Some(s) = g.as_mut() {
                s.location_ok = ok;
                s.elevation_offer = offer;
            }
        }
        {
            let btn = GetDlgItem(hwnd, ID_BTN_RESTART_ADMIN as i32);
            if !btn.is_null() {
                ShowWindow(btn, if offer && page == 1 { SW_SHOW } else { SW_HIDE });
            }
            let hint = GetDlgItem(hwnd, ID_LBL_HINT as i32);
            if !hint.is_null() {
                ShowWindow(hint, if offer && page == 1 { SW_HIDE } else { SW_SHOW });
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
            set_text(
                hwnd,
                ID_LBL_INFO,
                if offer {
                    UNWRITABLE_OFFER_INFO_TEXT
                } else {
                    UNWRITABLE_INFO_TEXT
                },
            );
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
        // Component checkboxes (v6): one per optional component, in
        // manifest order — the name plus the packed size it adds.
        let components = STATE.lock().ok().and_then(|g| {
            g.as_ref().map(|s| {
                (
                    s.power
                        .components
                        .iter()
                        .filter(|c| !c.required)
                        .map(|c| (c.id.clone(), c.name.clone(), c.packed_size))
                        .collect::<Vec<_>>(),
                    s.component_selection.clone(),
                )
            })
        });
        if let Some((optional, selection)) = components {
            for (i, (id, name, packed_size)) in
                optional.iter().enumerate().take(MAX_COMPONENT_BOXES)
            {
                set_text(
                    hwnd,
                    ID_CHK_COMPONENT_BASE + i,
                    &format!("{name} ({})", format_bytes(*packed_size)),
                );
                set_check(hwnd, ID_CHK_COMPONENT_BASE + i, selection.contains(id));
            }
        }
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
        let component_checks: Vec<bool> = (0..MAX_COMPONENT_BOXES)
            .map(|i| get_check(hwnd, ID_CHK_COMPONENT_BASE + i))
            .collect();
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
                // Component selection (v6): optional parts follow
                // their checkboxes; parts beyond the visible boxes
                // keep their state, and required parts and `core`
                // are never removed here (the operation enforces
                // them regardless). The result is stored in canonical
                // part order.
                if !s.power.components.is_empty() {
                    let optional_ids: Vec<String> = s
                        .power
                        .components
                        .iter()
                        .filter(|c| !c.required)
                        .map(|c| c.id.clone())
                        .collect();
                    let mut selection = s.component_selection.clone();
                    for (i, id) in optional_ids.iter().enumerate().take(MAX_COMPONENT_BOXES) {
                        if component_checks.get(i).copied().unwrap_or(true) {
                            if !selection.contains(id) {
                                selection.push(id.clone());
                            }
                        } else {
                            selection.retain(|x| x != id);
                        }
                    }
                    let include_core = s.power.has_core_files(&s.records);
                    s.component_selection = s
                        .power
                        .part_ids(include_core)
                        .into_iter()
                        .filter(|id| selection.contains(id))
                        .collect();
                }
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
            // The header is painted by a subclass on the header
            // control itself, in theme colors (owner-draw never
            // painted — see theme::subclass_list_header).
            theme::subclass_list_header(list);
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
                    &s.power,
                    &s.component_selection,
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
                    SetBkColor(hdc, theme::ACTION);
                    SetTextColor(hdc, theme::ACCENT_FOREGROUND);
                    return theme::brush(theme::ACTION) as LRESULT;
                }
                if id == ID_RAIL_BG {
                    // The rail's own background: solid darkest panel.
                    SetBkMode(hdc, OPAQUE as i32);
                    SetBkColor(hdc, theme::BACKGROUND);
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                if let Some(i) = ID_STEP_ACCENTS.iter().position(|a| *a == id) {
                    // Accent bar: green for the current step, rail
                    // color (invisible) for the rest.
                    SetBkMode(hdc, OPAQUE as i32);
                    if page == (i + 1) as u8 {
                        SetBkColor(hdc, theme::ACTION);
                        return theme::brush(theme::ACTION) as LRESULT;
                    }
                    SetBkColor(hdc, theme::BACKGROUND);
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                if let Some(i) = ID_STEP_NUMS.iter().position(|a| *a == id) {
                    // Step number: green-filled while current, a green
                    // check once complete, dim while still ahead.
                    let step = (i + 1) as u8;
                    if page == step {
                        SetBkMode(hdc, OPAQUE as i32);
                        SetBkColor(hdc, theme::ACTION);
                        SetTextColor(hdc, theme::ACTION_FOREGROUND);
                        return theme::brush(theme::ACTION) as LRESULT;
                    }
                    SetBkMode(hdc, TRANSPARENT as i32);
                    SetTextColor(
                        hdc,
                        if page > step {
                            theme::SUCCESS
                        } else {
                            theme::MUTED
                        },
                    );
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                if let Some(i) = ID_STEP_NAMES.iter().position(|a| *a == id) {
                    // Step name: bright for the current and completed
                    // steps, dim for the ones still ahead.
                    SetBkMode(hdc, TRANSPARENT as i32);
                    SetTextColor(
                        hdc,
                        if page >= (i + 1) as u8 {
                            theme::FOREGROUND
                        } else {
                            theme::MUTED
                        },
                    );
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                if id == ID_LBL_APPNAME || id == ID_LBL_DISKNEED {
                    SetBkMode(hdc, TRANSPARENT as i32);
                    SetTextColor(hdc, theme::FOREGROUND);
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                if id == ID_LBL_APPVER
                    || id == ID_LBL_APPPUB
                    || id == ID_LBL_DISKCAP
                    || id == ID_LBL_DISKFREE
                {
                    SetBkMode(hdc, TRANSPARENT as i32);
                    SetTextColor(hdc, theme::MUTED);
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                if id == ID_LBL_INFO {
                    // The info box: light text on the deep-blue fill.
                    SetBkMode(hdc, OPAQUE as i32);
                    SetBkColor(hdc, theme::ACCENT);
                    SetTextColor(hdc, theme::FOREGROUND);
                    return theme::brush(theme::ACCENT) as LRESULT;
                }
                SetBkMode(hdc, TRANSPARENT as i32);
                if (id == ID_LBL_STATUS || id == ID_LBL_FINISH) && installed {
                    // Success green, always paired with the status text.
                    SetTextColor(hdc, theme::SUCCESS);
                } else if id == ID_LBL_STEP
                    || id == ID_LBL_HINT
                    || id == ID_LBL_STATUS
                    || id == ID_LBL_SUBTITLE
                {
                    SetTextColor(hdc, theme::MUTED);
                } else if id == ID_LBL_OPTNOTE {
                    SetTextColor(hdc, theme::INFO);
                } else {
                    SetTextColor(hdc, theme::FOREGROUND);
                }
                theme::brush(theme::BACKGROUND) as LRESULT
            }
            WM_CTLCOLOREDIT => {
                let hdc = wparam as HDC;
                let ctrl = lparam as HWND;
                SetBkMode(hdc, OPAQUE as i32);
                if IsWindowEnabled(ctrl) == 0 {
                    // The location edit in Modify mode: fixed, and
                    // drawn as disabled rather than merely inert.
                    SetBkColor(hdc, theme::DISABLED_FILL);
                    SetTextColor(hdc, theme::DISABLED_FOREGROUND);
                    theme::brush(theme::DISABLED_FILL) as LRESULT
                } else {
                    SetBkColor(hdc, theme::SURFACE);
                    SetTextColor(hdc, theme::FOREGROUND);
                    theme::brush(theme::SURFACE) as LRESULT
                }
            }
            WM_DRAWITEM => {
                let dis = lparam as *const DRAWITEMSTRUCT;
                if dis.is_null() {
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                let dis = &*dis;
                // No header arm: the review list's header is painted
                // by its subclass (theme::subclass_list_header).
                match dis.CtlID as usize {
                    ID_BTN_INSTALL => theme::draw_push_button(dis, true),
                    ID_BTN_BACK
                    | ID_BTN_CANCEL
                    | ID_BTN_BROWSE
                    | ID_BTN_ANSWERS_APPLY
                    | ID_BTN_ANSWERS_IGNORE
                    | ID_BTN_RESTART_ADMIN => theme::draw_push_button(dis, false),
                    ID_CHK_DESKTOP | ID_CHK_STARTMENU | ID_CHK_PATH | ID_CHK_LOGIN
                    | ID_CHK_LAUNCH => theme::draw_checkbox(dis),
                    id if (ID_CHK_COMPONENT_BASE..ID_CHK_COMPONENT_BASE + MAX_COMPONENT_BOXES)
                        .contains(&id) =>
                    {
                        theme::draw_checkbox(dis)
                    }
                    _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
                }
                1
            }
            WM_COMMAND => {
                let control_id = wparam & 0xFFFF;
                let notify_code = (wparam >> 16) & 0xFFFF;
                // Owner-drawn checkboxes do not toggle themselves —
                // a click flips the stored check state here; the
                // pages harvest the states exactly as before.
                if notify_code == BN_CLICKED_CODE
                    && (matches!(
                        control_id,
                        ID_CHK_DESKTOP
                            | ID_CHK_STARTMENU
                            | ID_CHK_PATH
                            | ID_CHK_LOGIN
                            | ID_CHK_LAUNCH
                    ) || (ID_CHK_COMPONENT_BASE..ID_CHK_COMPONENT_BASE + MAX_COMPONENT_BOXES)
                        .contains(&control_id))
                {
                    let now = get_check(hwnd, control_id);
                    set_check(hwnd, control_id, !now);
                }
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
                    ID_BTN_RESTART_ADMIN => on_restart_admin_click(hwnd),
                    ID_BTN_ANSWERS_APPLY => on_answers_apply(hwnd),
                    ID_BTN_ANSWERS_IGNORE => {
                        if let Ok(mut g) = STATE.lock() {
                            if let Some(s) = g.as_mut() {
                                s.answers_decided = true;
                            }
                        }
                        show_page(hwnd, 1);
                        set_text(
                            hwnd,
                            ID_LBL_STATUS,
                            "Preset ignored — the package defaults stay in charge.",
                        );
                    }
                    _ => {}
                }
                0
            }
            WM_NOTIFY => {
                let hdr = lparam as *const NMHDR;
                if !hdr.is_null() && (*hdr).idFrom == ID_LIST {
                    // Theme custom draw (selection fill, gridlines).
                    if let Some(result) = theme::listview_custom_draw(lparam, true) {
                        return result;
                    }
                    if (*hdr).code as i32 == NM_DBLCLK_I32 {
                        on_row_activate(hwnd);
                    }
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
        // Modify mode has its own outcome slot and Finish text.
        let is_modify = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().map(|s| s.modify))
            .unwrap_or(false);
        if is_modify {
            let finish = STATE.lock().ok().and_then(|mut g| {
                g.as_mut().and_then(|s| {
                    s.installing = false;
                    s.modify_finish.take()
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
                            "modify ok: {} added, {} removed at {}",
                            outcome.files_added,
                            outcome.files_removed,
                            outcome.dest.display()
                        ),
                    );
                    let text = format!(
                        "Modified {} {} at {}.\r\n\r\n{} file(s) added, {} removed.\r\nParts now installed: {}\r\nReceipt: {}\r\n\r\nRemove it any time from Windows Settings \u{2192} Apps, or by running Uninstall.exe in the install folder.",
                        outcome.app_name,
                        outcome.version,
                        outcome.dest.display(),
                        outcome.files_added,
                        outcome.files_removed,
                        outcome.components_after.join(", "),
                        outcome.receipt_path.display(),
                    );
                    set_text(hwnd, ID_LBL_FINISH, &text);
                }
                Err(e) => {
                    crate::diag::log("wizard", &format!("modify failed: {e}"));
                    let text = if e.starts_with("Modify cancelled") {
                        format!("Modify cancelled.\r\n\r\n{e}")
                    } else {
                        format!(
                            "Modify failed.\r\n\r\n{e}\r\n\r\nEvery change was rolled back; the previous install state was kept."
                        )
                    };
                    set_text(hwnd, ID_LBL_FINISH, &text);
                }
            }
            show_page(hwnd, 4);
            return;
        }
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
        // Rows dispatch on their Item text, not their position: v6
        // packages insert required-part rows and the writes summaries
        // between the fixed rows, so positions shift. Rows with a
        // real Change action route to the Options page; the rest are
        // informational and explain themselves in the status line.
        let item = STATE.lock().ok().and_then(|g| {
            g.as_ref().and_then(|s| {
                let rows = review_rows(
                    &s.manifest,
                    &s.options,
                    &s.records,
                    &s.install_dir.to_string_lossy(),
                    s.location_ok,
                    &s.power,
                    &s.component_selection,
                );
                rows.get(selected as usize).map(|row| row[0].clone())
            })
        });
        match item.as_deref() {
            Some(
                "Install location"
                | "Components"
                | "Start Menu folder"
                | "Desktop shortcut"
                | "File associations"
                | "PATH entry"
                | "Start at login",
            ) => show_page(hwnd, 2),
            Some(name) if name.starts_with("Part: ") => set_text(
                hwnd,
                ID_LBL_STATUS,
                "That part is required — it is always installed. Optional parts are chosen on the Options page.",
            ),
            Some("Registry writes") | Some("Environment variables") => set_text(
                hwnd,
                ID_LBL_STATUS,
                "The package declares these writes. They are applied on install and recorded in the receipt; uninstall restores what was there.",
            ),
            _ => set_text(
                hwnd,
                ID_LBL_STATUS,
                "That row is informational — there is nothing to change.",
            ),
        }
    }

    /// The Review page's **Restart as administrator** button
    /// (v4.8.0): relaunch this same executable elevated with the
    /// original arguments preserved — bare wizard, `--modify`, and
    /// an applied preset's `--answers <path>` — plus the relaunch
    /// marker, plus (install mode) the location currently chosen,
    /// carried as `--to` so the elevated instance opens on the
    /// folder the user actually picked. This window closes only
    /// after a successful hand-off. If Windows declines (the UAC
    /// prompt was refused) or the launch fails, the wizard stays
    /// open, says so in plain words on the status line, and the
    /// per-user offer stays exactly as it was — no error page.
    unsafe fn on_restart_admin_click(hwnd: HWND) {
        let snapshot = STATE.lock().ok().and_then(|g| {
            g.as_ref().map(|s| {
                (
                    s.exe_path.clone(),
                    s.answers_applied_path.clone(),
                    s.modify,
                    s.install_dir.clone(),
                )
            })
        });
        let Some((exe_path, applied_preset, modify, install_dir)) = snapshot else {
            return;
        };
        let mut base_args = crate::elevation::current_process_args();
        if base_args.is_empty() && modify {
            base_args.push("--modify".to_string());
        }
        if let Some(path) = &applied_preset {
            if !base_args.iter().any(|a| a == "--answers") {
                base_args.push("--answers".to_string());
                base_args.push(path.to_string_lossy().into_owned());
            }
        }
        if !modify && !base_args.iter().any(|a| a == "--to") {
            base_args.push("--to".to_string());
            base_args.push(install_dir.to_string_lossy().into_owned());
        }
        crate::diag::log(
            "wizard",
            &format!(
                "elevation relaunch requested ({} mode)",
                if modify { "modify" } else { "install" }
            ),
        );
        match crate::elevation::relaunch_elevated(&exe_path, &base_args) {
            Ok(()) => {
                crate::diag::log("wizard", "elevation relaunch handed off; exiting");
                DestroyWindow(hwnd);
            }
            Err(e) => {
                crate::diag::log("wizard", &format!("elevation relaunch: {}", e.describe()));
                set_text(
                    hwnd,
                    ID_LBL_STATUS,
                    &format!(
                        "{} — nothing was changed. The per-user folder above is still available.",
                        e.describe()
                    ),
                );
            }
        }
    }

    /// The Review page's Apply-preset button: the beside-the-exe
    /// answer file's choices land on the wizard state exactly as if
    /// the user had made them on the Options page — options, the
    /// location (when the preset names one), the component
    /// selection, and the uninstall data-purge default the receipt
    /// will record. Nothing here runs without this click: the offer
    /// is the whole point.
    unsafe fn on_answers_apply(hwnd: HWND) {
        let answers = STATE
            .lock()
            .ok()
            .and_then(|g| g.as_ref().and_then(|s| s.answers.clone()));
        let Some(answers) = answers else {
            return;
        };
        let mut selection_error: Option<String> = None;
        if let Ok(mut g) = STATE.lock() {
            if let Some(s) = g.as_mut() {
                answers.apply_to_options(&mut s.options);
                if let Some(location) = &answers.install_location {
                    s.install_dir = PathBuf::from(location);
                }
                if answers.components.is_some() || !answers.without_components.is_empty() {
                    let primary_rel = s
                        .records
                        .get(s.manifest.primary_exe_index as usize)
                        .map(|r| r.relative_path());
                    match s.power.resolve_selection(
                        answers.components.as_deref(),
                        &answers.without_components,
                        None,
                        &s.records,
                        primary_rel.as_deref(),
                    ) {
                        Ok(selection) => s.component_selection = selection,
                        Err(e) => selection_error = Some(e),
                    }
                }
                s.purge_data_default = answers.purge_data_on_uninstall;
                s.answers_decided = true;
                // Remember which file was applied, so an elevation
                // relaunch can carry the preset across the hand-off
                // (the elevated instance applies it before page 1 —
                // the user's click here is its authorization).
                s.answers_applied_path = s
                    .exe_path
                    .parent()
                    .map(|p| p.join(crate::answers::ANSWERS_FILE_NAME));
            }
        }
        let list = GetDlgItem(hwnd, ID_LIST as i32);
        if !list.is_null() {
            fill_list(list);
        }
        show_page(hwnd, 1);
        match &selection_error {
            Some(e) => set_text(
                hwnd,
                ID_LBL_STATUS,
                &format!("Preset applied, except its component selection: {e}"),
            ),
            None => set_text(
                hwnd,
                ID_LBL_STATUS,
                "Preset applied — the Review rows and the Options page now show its choices.",
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
        let (
            exe_path,
            install_dir,
            options,
            cancel,
            progress,
            component_selection,
            modify,
            purge_data_default,
        ) = match STATE.lock() {
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
                        s.component_selection.clone(),
                        s.modify,
                        s.purge_data_default,
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
            let progress_slot = progress.clone();
            let mut on_progress = move |ev: &ProgressEvent| {
                if let Ok(mut p) = progress_slot.lock() {
                    *p = (ev.file_index, ev.file_count, ev.rel_path.clone());
                }
                unsafe {
                    PostMessageW(hwnd_value as HWND, WM_APP_PROGRESS, 0, 0);
                }
            };
            if modify {
                // Modify mode: the shared modify operation reconciles
                // the existing install with the page's choices; its
                // outcome lands in the modify slot on_done_message
                // reads.
                let request = crate::modify::ModifyRequest {
                    options_override: Some(options),
                    start_at_login: None,
                    components: Some(component_selection),
                    without_components: Vec::new(),
                };
                let result = crate::modify::perform_modify(
                    &exe_path,
                    &install_dir,
                    &request,
                    Some(&mut on_progress),
                    Some(&cancel),
                );
                if let Ok(mut g) = STATE.lock() {
                    if let Some(s) = g.as_mut() {
                        s.modify_finish = Some(result);
                    }
                }
            } else {
                let request = crate::install::InstallRequest {
                    options_override: Some(options),
                    force: false,
                    repair: false,
                    start_at_login: None,
                    components: Some(component_selection),
                    without_components: Vec::new(),
                    purge_data_default,
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
        /// The data-purge checkbox's starting state (the receipt's
        /// recorded default; unchecked unless an answer file opted
        /// in at install time).
        purge_default: bool,
        /// The v4.8.0 elevation offer applies (the install folder is
        /// unwritable by this account, unelevated, no marker): the
        /// window shows the Restart as administrator button.
        offer_elevation: bool,
        /// None until the window ends with a decision; Cancel/close
        /// leaves it None (reported as Cancelled).
        result: Option<super::UninstallConfirmation>,
    }

    static CONFIRM_STATE: Mutex<Option<ConfirmState>> = Mutex::new(None);

    pub fn confirm_uninstall(
        app_name: &str,
        data_dirs: &[PathBuf],
        purge_default: bool,
        offer_elevation: bool,
    ) -> super::UninstallConfirmation {
        if let Ok(mut g) = CONFIRM_STATE.lock() {
            *g = Some(ConfirmState {
                app_name: app_name.to_string(),
                data_dirs: data_dirs
                    .iter()
                    .map(|d| d.to_string_lossy().into_owned())
                    .collect(),
                purge_default,
                offer_elevation,
                result: None,
            });
        }
        unsafe {
            // The header font is normally created by the wizard's
            // run(); an uninstall confirmation runs in a process where
            // the wizard never opened, so create it on demand. (The
            // palette needs no such care — theme brushes are created
            // lazily by the theme module itself.)
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
                hbrBackground: theme::brush(theme::BACKGROUND),
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
                    return super::UninstallConfirmation::Cancelled;
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
                return super::UninstallConfirmation::Cancelled;
            }
            create_confirm_controls(hwnd, instance);
            SendMessageW(hwnd, DM_SETDEFID_MSG, ID_BTN_CONFIRM, 0);
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
            .unwrap_or(super::UninstallConfirmation::Cancelled)
    }

    unsafe fn create_confirm_controls(parent: HWND, instance: HINSTANCE) {
        let static_class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let button_class: Vec<u16> = "BUTTON\0".encode_utf16().collect();
        let (app_name, data_dirs, purge_default, offer_elevation) = CONFIRM_STATE
            .lock()
            .ok()
            .and_then(|g| {
                g.as_ref().map(|s| {
                    (
                        s.app_name.clone(),
                        s.data_dirs.clone(),
                        s.purge_default,
                        s.offer_elevation,
                    )
                })
            })
            .unwrap_or_else(|| ("App".to_string(), Vec::new(), false, false));
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
            let purge_box = mk(
                &button_class,
                &format!("Also delete {app_name}'s data files"),
                WS_TABSTOP | BS_OWNERDRAW,
                16,
                116,
                590,
                24,
                ID_CHK_PURGE,
            );
            // The checkbox starts in the receipt-recorded state; the
            // user still decides here, every time.
            if purge_default {
                SendMessageW(purge_box, BM_SETCHECK_MSG, BST_CHECKED_STATE, 0);
            }
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
        if offer_elevation {
            // The elevation offer (v4.8.0): the honest note plus the
            // restart button in the footer's left slot. A declined
            // prompt rewrites the note in place (confirm_wnd_proc)
            // and the window stays open.
            mk(
                &static_class,
                "This app's folder needs administrator access, and this uninstaller runs as your account. You can restart as administrator: Windows will ask for permission, and the uninstall will then run as the administrator account.",
                0,
                16,
                262,
                590,
                40,
                ID_LBL_CONFIRM_NOTE,
            );
            mk(
                &button_class,
                "Restart as administrator",
                WS_TABSTOP | BS_OWNERDRAW,
                16,
                310,
                300,
                30,
                ID_BTN_CONFIRM_ELEVATE,
            );
        }
        mk(
            &button_class,
            "Cancel",
            WS_TABSTOP | BS_OWNERDRAW,
            470,
            310,
            120,
            30,
            ID_BTN_CONFIRM_CANCEL,
        );
        mk(
            &button_class,
            "Uninstall",
            WS_TABSTOP | BS_OWNERDRAW,
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
                if id == ID_LBL_CONFIRM_WARN {
                    SetTextColor(hdc, theme::WARNING);
                } else if id == ID_LBL_CONFIRM_BODY {
                    SetTextColor(hdc, theme::INFO);
                } else if id == ID_LBL_CONFIRM_DIRS {
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
                    ID_BTN_CONFIRM => theme::draw_push_button(dis, true),
                    ID_BTN_CONFIRM_CANCEL | ID_BTN_CONFIRM_ELEVATE => {
                        theme::draw_push_button(dis, false)
                    }
                    ID_CHK_PURGE => theme::draw_checkbox(dis),
                    _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
                }
                1
            }
            WM_COMMAND => {
                // The owner-drawn purge checkbox toggles here (see
                // the wizard's WM_COMMAND for the same pattern).
                if (wparam >> 16) & 0xFFFF == BN_CLICKED_CODE && wparam & 0xFFFF == ID_CHK_PURGE {
                    let now = get_check(hwnd, ID_CHK_PURGE);
                    set_check(hwnd, ID_CHK_PURGE, !now);
                }
                match wparam & 0xFFFF {
                    ID_BTN_CONFIRM => {
                        let purge = get_check(hwnd, ID_CHK_PURGE);
                        if let Ok(mut g) = CONFIRM_STATE.lock() {
                            if let Some(s) = g.as_mut() {
                                s.result = Some(super::UninstallConfirmation::Proceed {
                                    purge_data: purge,
                                });
                            }
                        }
                        DestroyWindow(hwnd);
                    }
                    ID_BTN_CONFIRM_ELEVATE => {
                        // Restart as administrator (v4.8.0): relaunch
                        // this same executable elevated with the
                        // original arguments preserved (the ARP
                        // string's --uninstall --to <dir> included)
                        // plus the marker. The window closes only
                        // after a successful hand-off; a declined
                        // prompt rewrites the note and stays open.
                        let args = crate::elevation::current_process_args();
                        let outcome = match crate::elevation::own_exe() {
                            Ok(e) => crate::elevation::relaunch_elevated(&e, &args),
                            Err(err) => Err(err),
                        };
                        match outcome {
                            Ok(()) => {
                                if let Ok(mut g) = CONFIRM_STATE.lock() {
                                    if let Some(s) = g.as_mut() {
                                        s.result =
                                            Some(super::UninstallConfirmation::RelaunchedElevated);
                                    }
                                }
                                DestroyWindow(hwnd);
                            }
                            Err(why) => {
                                set_text(
                                    hwnd,
                                    ID_LBL_CONFIRM_NOTE,
                                    &format!(
                                        "{} — nothing was removed. You can cancel, or go ahead with Uninstall as your own account.",
                                        why.describe()
                                    ),
                                );
                            }
                        }
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
        let rows = review_rows(
            &manifest,
            &options,
            &records,
            "C:\\Apps\\Test App",
            true,
            &PowerSpec::default(),
            &[],
        );
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
        let rows = review_rows(
            &manifest,
            &options,
            &records,
            "D:\\Empty",
            true,
            &PowerSpec::default(),
            &[],
        );
        assert_eq!(rows[1][1], "No files");
        assert_eq!(rows[4][1], "None");
    }

    #[test]
    fn review_rows_include_start_at_login_only_when_offered() {
        let (manifest, records) = manifest_with(&["app.exe"], 100);
        let mut options = InstallOptions::defaults_for("Test App");
        options.start_at_login = 1;
        options.start_at_login_default = 1;
        let rows = review_rows(
            &manifest,
            &options,
            &records,
            "C:\\Apps\\Test App",
            true,
            &PowerSpec::default(),
            &[],
        );
        assert_eq!(rows.len(), 7);
        assert_eq!(rows[6][0], "Start at login");
        assert_eq!(rows[6][1], "Yes");
        assert_eq!(rows[6][2], "Toggle");
        // Offered but off reads "No".
        options.start_at_login_default = 0;
        let rows = review_rows(
            &manifest,
            &options,
            &records,
            "C:\\Apps\\Test App",
            true,
            &PowerSpec::default(),
            &[],
        );
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
            &PowerSpec::default(),
            &[],
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
    fn review_rows_show_power_parts_and_write_summaries() {
        use crate::overlay::{ComponentSpec, EnvWriteSpec, RegValueKind, RegWriteSpec};
        let (manifest, records) = manifest_with(&["app.exe", "extras/big.bin"], 300);
        let options = InstallOptions::defaults_for("Test App");
        let power = PowerSpec {
            components: vec![
                ComponentSpec {
                    id: "extras".to_string(),
                    name: "Extras".to_string(),
                    description: String::new(),
                    files: vec!["extras/big.bin".to_string()],
                    required: false,
                    default_selected: false,
                    packed_size: 10,
                },
                ComponentSpec {
                    id: "engine".to_string(),
                    name: "Engine".to_string(),
                    description: String::new(),
                    files: vec![],
                    required: true,
                    default_selected: true,
                    packed_size: 0,
                },
            ],
            registry_writes: vec![RegWriteSpec {
                key: "Software\\Test App".to_string(),
                name: "Mode".to_string(),
                kind: RegValueKind::Dword,
                value: "31".to_string(),
            }],
            env_writes: vec![EnvWriteSpec {
                name: "TEST_APP_HOME".to_string(),
                value: "C:\\Test".to_string(),
            }],
            ..Default::default()
        };
        let selection = vec!["core".to_string(), "engine".to_string()];
        let rows = review_rows(
            &manifest,
            &options,
            &records,
            "C:\\Apps\\Test App",
            true,
            &power,
            &selection,
        );
        // location, Components, required part, SM folder, desktop,
        // associations, PATH, registry, environment.
        assert_eq!(rows.len(), 9);
        assert_eq!(rows[1][1], "2 of 3 parts");
        assert_eq!(rows[2][0], "Part: Engine");
        assert_eq!(rows[2][1], "Included (required)");
        assert_eq!(rows[2][2], "");
        assert_eq!(rows[7][0], "Registry writes");
        assert_eq!(rows[7][1], "1 values");
        assert_eq!(rows[8][0], "Environment variables");
        assert_eq!(rows[8][1], "1 variables");
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
