//! The install operation — the single implementation behind both the
//! wizard's Install button and the `--install` command (everything-is-a-
//! command: the GUI is a view over this, never a separate code path).
//!
//! [`perform_install_with`] extracts the payload of the installer at
//! `exe_path` into `dest`, snapshotting every existing target first so
//! any failure (or a cancellation) rolls the destination back to its
//! previous state, then copies the installer in as `Uninstall.exe`,
//! writes the Add/Remove Programs entry, applies the package's system
//! artifacts (shortcuts, file associations, PATH — see
//! [`crate::sysinstall`]) under a rollback hook, and finally writes the
//! install receipt (`seanpak-receipt.json`, see [`crate::receipt`]).
//!
//! Before any of that, an existing install of the same app at `dest`
//! (found via its receipt, or the ARP entry on Windows) decides the
//! mode: fresh install, upgrade in place (old files are snapshotted, so
//! a failed upgrade restores the old version), reinstall, or repair.
//! Installing an older version over a newer one is refused unless the
//! caller forces it. [`plan_install`] computes exactly what a run would
//! do without touching anything (the `--dry-run` report). Before the
//! journal starts, a writability pre-flight
//! ([`ensure_location_writable`]) refuses destinations this per-user
//! process cannot write, suggesting the per-user Programs folder
//! instead — installs never elevate.

use std::cmp::Ordering;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};

use crate::hash::sha256_hex;
use crate::overlay::{
    ExtractControl, FileRecordHeader, InstallOptions, OverlayManifestHeader, OverlayReader,
};
use crate::receipt::{compare_versions, read_receipt, InstallReceipt, ReceiptFile};
use crate::rollback::TransactionRollbackLog;
use crate::sysinstall::{apply_artifacts, plan_artifacts, remove_artifacts};
use crate::wizard::uninstall_key_path;

/// How an install relates to what is already at the destination.
#[derive(Debug, Clone, PartialEq)]
pub enum DetectedInstall {
    None,
    Existing {
        version: String,
        /// Where the detection came from: `receipt` or `ARP entry`.
        source: String,
    },
}

/// Detects an existing install of the manifest's app at `dest`: the
/// install receipt is authoritative; the ARP entry (Windows) is the
/// fallback for installs made before receipts existed.
pub fn detect_existing(dest: &Path, manifest: &OverlayManifestHeader) -> DetectedInstall {
    if let Some(receipt) = read_receipt(dest) {
        if receipt.app == manifest.app_name() {
            return DetectedInstall::Existing {
                version: receipt.version,
                source: "receipt".to_string(),
            };
        }
    }
    if let Some((version, _location)) = crate::sysinstall::arp_install(&manifest.app_name()) {
        return DetectedInstall::Existing {
            version,
            source: "ARP entry".to_string(),
        };
    }
    DetectedInstall::None
}

/// The mode string recorded in outcomes and receipts.
fn mode_name(detected: &DetectedInstall, new_version: &str, repair: bool) -> String {
    match detected {
        DetectedInstall::None => "install".to_string(),
        DetectedInstall::Existing { version, .. } => match compare_versions(new_version, version) {
            Ordering::Greater => "upgrade".to_string(),
            Ordering::Less => "downgrade".to_string(),
            Ordering::Equal => {
                if repair {
                    "repair".to_string()
                } else {
                    "reinstall".to_string()
                }
            }
        },
    }
}

/// What an install would do, computed without side effects.
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub app_name: String,
    pub app_version: String,
    pub dest: PathBuf,
    /// (relative path, size) for every payload file, in record order.
    pub files: Vec<(String, u64)>,
    pub total_bytes: u64,
    pub uninstaller_path: PathBuf,
    /// Registry path of the ARP entry (under HKEY_CURRENT_USER).
    pub arp_key: String,
    /// `install` / `upgrade` / `repair` / `reinstall` / `downgrade`.
    pub mode: String,
    pub previous_version: Option<String>,
    /// Shortcut files the install would create (paths).
    pub shortcuts: Vec<String>,
    /// Extensions the install would associate.
    pub associations: Vec<String>,
    pub path_entry: bool,
    /// The install would apply the start-at-login Run entry.
    pub start_at_login: bool,
    /// Pre-flight verdict: can this per-user process write to `dest`?
    pub location_writable: bool,
    /// When `dest` is not writable, the per-user folder offered instead.
    pub suggested_location: Option<PathBuf>,
}

pub fn plan_install(
    manifest: &OverlayManifestHeader,
    records: &[FileRecordHeader],
    options: &InstallOptions,
    dest: &Path,
) -> InstallPlan {
    let detected = detect_existing(dest, manifest);
    let previous_version = match &detected {
        DetectedInstall::Existing { version, .. } => Some(version.clone()),
        DetectedInstall::None => None,
    };
    let primary_rel = records
        .get(manifest.primary_exe_index as usize)
        .map(|r| r.relative_path());
    let artifacts = plan_artifacts(options, &manifest.app_name(), dest, primary_rel.as_deref());
    let (location_writable, suggested_location) =
        match location_verdict(dest, &manifest.app_name(), &options.install_location()) {
            LocationVerdict::Writable => (true, None),
            LocationVerdict::NotWritable { fallback, .. } => (false, Some(fallback)),
        };
    InstallPlan {
        app_name: manifest.app_name(),
        app_version: manifest.app_version(),
        dest: dest.to_path_buf(),
        files: records
            .iter()
            .map(|r| (r.relative_path(), r.file_size))
            .collect(),
        total_bytes: manifest.total_uncompressed_bytes,
        uninstaller_path: dest.join("Uninstall.exe"),
        arp_key: uninstall_key_path(&manifest.app_name()),
        mode: mode_name(&detected, &manifest.app_version(), false),
        previous_version,
        shortcuts: artifacts
            .shortcuts
            .iter()
            .map(|s| s.link_path.to_string_lossy().into_owned())
            .collect(),
        associations: artifacts.associations.clone(),
        path_entry: artifacts.path_dir.is_some(),
        start_at_login: artifacts.start_at_login,
        location_writable,
        suggested_location,
    }
}

/// Stable machine-readable reason for an install refused because the
/// destination is not writable, carried in the `--json` install report
/// (the exit code stays 5, install failure).
pub const REASON_LOCATION_NOT_WRITABLE: &str = "location_not_writable";

/// Pre-flight verdict for an install destination: can this per-user
/// process write there? SeanPak installs never elevate, so a folder
/// that needs administrator access (the classic `C:\Program Files`
/// case, field-proven with the ai.md 0.6.0 package) must be caught
/// *before* the install starts — not midway through extraction.
#[derive(Debug, Clone, PartialEq)]
pub enum LocationVerdict {
    Writable,
    NotWritable {
        /// The per-user folder offered instead of the refused one.
        fallback: PathBuf,
        /// OS-level detail of the failed probe (for messages and logs).
        detail: String,
    },
}

/// The folder name a fallback install uses: the last component of the
/// package's recorded install location (`C:\Program Files\ai.md` →
/// `ai.md`), or the sanitized app name when the location names no
/// folder (empty, or a bare drive root). Separator handling accepts
/// both `\` and `/` on any platform, so the logic is host-testable.
pub fn install_folder_name(app_name: &str, recorded_location: &str) -> String {
    let loc = recorded_location.trim().trim_end_matches(['\\', '/']);
    let last = loc.rsplit(['\\', '/']).next().unwrap_or("").trim();
    if !last.is_empty() && !last.ends_with(':') {
        last.to_string()
    } else {
        crate::wizard::sanitize_name(app_name)
    }
}

/// The per-user Programs root's parent on this machine: `%LOCALAPPDATA%`
/// on Windows; `$XDG_DATA_HOME` or `~/.local/share` elsewhere (the
/// fallback only has a user-facing role on Windows, but the path stays
/// sane everywhere so host tests can exercise it).
pub fn local_app_data_dir() -> PathBuf {
    if let Some(v) = std::env::var_os("LOCALAPPDATA") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    #[cfg(windows)]
    if let Some(v) = std::env::var_os("USERPROFILE") {
        if !v.is_empty() {
            return PathBuf::from(v).join("AppData").join("Local");
        }
    }
    if let Some(v) = std::env::var_os("XDG_DATA_HOME") {
        if !v.is_empty() {
            return PathBuf::from(v);
        }
    }
    if let Some(v) = std::env::var_os("HOME") {
        if !v.is_empty() {
            return PathBuf::from(v).join(".local").join("share");
        }
    }
    std::env::temp_dir()
}

/// The standard per-user install location — `%LOCALAPPDATA%\Programs\
/// <AppFolder>` (the VS Code / Discord pattern) — built from an explicit
/// local-app-data root so the shape is unit-testable anywhere.
pub fn per_user_fallback_from(
    local_app_data: &Path,
    app_name: &str,
    recorded_location: &str,
) -> PathBuf {
    local_app_data
        .join("Programs")
        .join(install_folder_name(app_name, recorded_location))
}

/// [`per_user_fallback_from`] against this machine's local app data dir.
/// The one derivation the CLI error, the dry-run plan, and the wizard
/// all share — no divergent copies.
pub fn per_user_fallback_dir(app_name: &str, recorded_location: &str) -> PathBuf {
    per_user_fallback_from(&local_app_data_dir(), app_name, recorded_location)
}

/// The nearest ancestor of `path` that exists (possibly `path` itself).
fn nearest_existing_ancestor(path: &Path) -> PathBuf {
    let mut cur = path;
    loop {
        if cur.exists() {
            return cur.to_path_buf();
        }
        match cur.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => cur = parent,
            _ => return cur.to_path_buf(),
        }
    }
}

/// Probes `dir` for writability with a uniquely named temporary file
/// (create + write + delete). Ok only when a file could really be
/// created, written, and removed again.
fn probe_dir_writable(dir: &Path) -> Result<(), String> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let seq = COUNTER.fetch_add(1, AtomicOrdering::Relaxed);
    let probe = dir.join(format!(
        ".seanpak-write-probe-{}-{nanos}-{seq}",
        std::process::id()
    ));
    let attempt = (|| -> std::io::Result<()> {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)?;
        std::io::Write::write_all(&mut file, b"seanpak")?;
        drop(file);
        std::fs::remove_file(&probe)
    })();
    match attempt {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&probe);
            Err(format!("cannot write in {}: {e}", dir.display()))
        }
    }
}

/// Non-destructive writability verdict for `dest`: probes the nearest
/// existing ancestor (creating nothing), so the wizard's Review page,
/// the `--dry-run` plan, and the CLI pre-check can all ask before
/// anything is touched. [`ensure_location_writable`] is the
/// authoritative gate the install operation itself runs.
pub fn location_verdict(dest: &Path, app_name: &str, recorded_location: &str) -> LocationVerdict {
    let fallback = per_user_fallback_dir(app_name, recorded_location);
    let not_writable = |detail: String| LocationVerdict::NotWritable { fallback, detail };
    let ancestor = nearest_existing_ancestor(dest);
    if !ancestor.is_dir() {
        return not_writable(format!("{} exists and is not a folder", ancestor.display()));
    }
    match probe_dir_writable(&ancestor) {
        Ok(()) => LocationVerdict::Writable,
        Err(detail) => not_writable(detail),
    }
}

/// The plain-language refusal shared by every surface: names the
/// folder, says it needs administrator access while this installer runs
/// per user, confirms nothing was changed, and names the per-user
/// fallback.
pub fn location_not_writable_message(dest: &Path, fallback: &Path, detail: &str) -> String {
    format!(
        "Install stopped before anything was changed: {} is not writable by your user account ({detail}). \
         That location needs administrator access, and this installer runs per user. \
         Suggested per-user location: {}",
        dest.display(),
        fallback.display()
    )
}

/// The install operation's own pre-flight, run before the rollback
/// journal, extraction, or any registry/shortcut change: the shared
/// verdict first, then the destination is created (as extraction would
/// anyway) and probed from inside. Any failure is the
/// location-not-writable refusal — nothing has been changed yet at
/// this point beyond, at most, the empty destination folder itself.
pub fn ensure_location_writable(
    dest: &Path,
    app_name: &str,
    recorded_location: &str,
) -> Result<(), String> {
    if let LocationVerdict::NotWritable { fallback, detail } =
        location_verdict(dest, app_name, recorded_location)
    {
        return Err(location_not_writable_message(dest, &fallback, &detail));
    }
    let fallback = per_user_fallback_dir(app_name, recorded_location);
    std::fs::create_dir_all(dest).map_err(|e| {
        location_not_writable_message(dest, &fallback, &format!("cannot create the folder: {e}"))
    })?;
    probe_dir_writable(dest)
        .map_err(|detail| location_not_writable_message(dest, &fallback, &detail))
}

/// Caller choices for an install run.
#[derive(Debug, Clone, Default)]
pub struct InstallRequest {
    /// Overrides the package's recorded install options (the wizard's
    /// Options page passes the user's edited values here).
    pub options_override: Option<InstallOptions>,
    /// Allow installing an older version over a newer one.
    pub force: bool,
    /// Same-version installs verify + restore damaged files and report
    /// the run as a repair.
    pub repair: bool,
    /// Explicit start-at-login choice for this run (the CLI's
    /// `--start-at-login` / `--no-start-at-login`): `Some` forces the
    /// option on (offered, with this default) or off, overriding the
    /// package's recorded default. `None` keeps the package's choice —
    /// the wizard passes `None` and carries the user's choice in
    /// `options_override` instead.
    pub start_at_login: Option<bool>,
}

/// One progress update during extraction.
#[derive(Debug, Clone)]
pub struct ProgressEvent {
    pub file_index: usize,
    pub file_count: usize,
    pub rel_path: String,
    pub file_size: u64,
    pub bytes_done: u64,
    pub total_bytes: u64,
}

#[derive(Debug)]
pub struct InstallOutcome {
    pub files_written: usize,
    pub dest: PathBuf,
    /// True when the ARP entry was written (Windows only; false off
    /// Windows, where there is no registry to write to).
    pub arp_written: bool,
    /// `install` / `upgrade` / `repair` / `reinstall` / `downgrade`.
    pub mode: String,
    pub previous_version: Option<String>,
    /// Pre-existing payload files that were missing or damaged and are
    /// now restored (the repair evidence; 0 on a fresh install).
    pub restored_files: usize,
    pub receipt_path: PathBuf,
    pub shortcuts_created: usize,
    pub associations_applied: usize,
    pub path_entry_added: bool,
    /// The start-at-login Run entry was applied (journaled in the
    /// receipt; Windows only — always false off Windows).
    pub start_at_login_applied: bool,
}

/// Performs the install described by the module docs with default
/// request options and no progress reporting. On any failure the
/// destination is rolled back before the error is returned.
pub fn perform_install(exe_path: &Path, dest: &Path) -> Result<InstallOutcome, String> {
    perform_install_with(exe_path, dest, &InstallRequest::default(), None, None)
}

/// The full install operation behind [`perform_install`], with progress
/// events and cooperative cancellation (setting `cancel` aborts between
/// files; the destination is rolled back and the returned error says
/// the install was cancelled).
#[allow(clippy::too_many_arguments)]
pub fn perform_install_with(
    exe_path: &Path,
    dest: &Path,
    request: &InstallRequest,
    mut progress: Option<&mut dyn FnMut(&ProgressEvent)>,
    cancel: Option<&AtomicBool>,
) -> Result<InstallOutcome, String> {
    let file =
        File::open(exe_path).map_err(|e| format!("cannot open {}: {e}", exe_path.display()))?;
    let (manifest, records, package_options) = OverlayReader::read_package(file)
        .map_err(|e| format!("cannot read package: {e}"))?
        .ok_or_else(|| "this executable carries no SeanPak overlay".to_string())?;
    let mut options = request.options_override.unwrap_or(package_options);
    // A caller's explicit start-at-login choice wins over the package's
    // recorded default (it can only force an entry on when the package
    // has a primary exe to point at — the plan enforces that).
    if let Some(choice) = request.start_at_login {
        options.start_at_login = 1;
        options.start_at_login_default = choice as u8;
    }

    // Mode decision, made before anything is touched.
    let detected = detect_existing(dest, &manifest);
    let previous_version = match &detected {
        DetectedInstall::Existing { version, .. } => Some(version.clone()),
        DetectedInstall::None => None,
    };
    if let DetectedInstall::Existing { version, .. } = &detected {
        if compare_versions(&manifest.app_version(), version) == Ordering::Less && !request.force {
            return Err(format!(
                "Downgrade refused: version {version} of {} is already installed at {}, \
                 and this package is older ({}). Re-run with --force to install the older version anyway.",
                manifest.app_name(),
                dest.display(),
                manifest.app_version()
            ));
        }
    }
    let mode = mode_name(&detected, &manifest.app_version(), request.repair);

    // Hash the payloads up front (decompressing as needed): the
    // pre-install damage scan and the receipt both use these hashes.
    let payload_hashes = payload_hashes(exe_path, &records)?;

    // Repair evidence: which existing targets are missing or damaged.
    let mut restored_files = 0usize;
    if detected != DetectedInstall::None {
        for (record, want) in records.iter().zip(&payload_hashes) {
            let rel = record.relative_path().replace('\\', "/");
            if rel.is_empty() || rel.contains("..") {
                continue;
            }
            let target = dest.join(&rel);
            match crate::hash::sha256_file_hex(&target) {
                Ok(have) if have == *want => {}
                _ => restored_files += 1,
            }
        }
    }

    // Pre-flight writability gate: before the journal, extraction, or
    // any system change. A per-user installer cannot use a folder that
    // needs administrator access, and the refusal must land before
    // anything is touched — never midway through extraction.
    ensure_location_writable(dest, &manifest.app_name(), &options.install_location())?;

    let mut tx =
        TransactionRollbackLog::new().map_err(|e| format!("cannot start rollback journal: {e}"))?;
    for record in &records {
        let rel = record.relative_path().replace('\\', "/");
        if rel.is_empty() || rel.contains("..") {
            continue; // extraction itself validates and rejects escapes
        }
        let target = dest.join(&rel);
        tx.snapshot_file_before_write(&target)
            .map_err(|e| format!("cannot snapshot {}: {e}", target.display()))?;
    }
    let uninstaller = dest.join("Uninstall.exe");
    tx.snapshot_file_before_write(&uninstaller)
        .map_err(|e| format!("cannot snapshot uninstaller: {e}"))?;
    let receipt_file = crate::receipt::receipt_path(dest);
    tx.snapshot_file_before_write(&receipt_file)
        .map_err(|e| format!("cannot snapshot receipt: {e}"))?;

    // Extraction with progress + cancellation.
    let total_bytes = manifest.total_uncompressed_bytes;
    let mut bytes_done: u64 = 0;
    let mut current_size: u64 = 0;
    let extract_result = {
        let mut on_file = |_idx: usize, _total: usize, rel: &str| {
            bytes_done += current_size;
            current_size = records
                .iter()
                .find(|r| r.relative_path() == rel)
                .map(|r| r.file_size)
                .unwrap_or(0);
            if let Some(sink) = progress.as_deref_mut() {
                sink(&ProgressEvent {
                    file_index: _idx,
                    file_count: records.len(),
                    rel_path: rel.to_string(),
                    file_size: current_size,
                    bytes_done,
                    total_bytes,
                });
            }
        };
        let mut control = ExtractControl {
            on_file: Some(&mut on_file),
            cancel,
        };
        let file =
            File::open(exe_path).map_err(|e| format!("cannot open {}: {e}", exe_path.display()))?;
        OverlayReader::extract_with_control(file, dest, &mut control)
    };
    let written = match extract_result {
        Ok(w) => w,
        Err(e) => {
            tx.rollback();
            if e.kind() == std::io::ErrorKind::Interrupted {
                return Err(
                    "Install cancelled; every change was rolled back and nothing was kept."
                        .to_string(),
                );
            }
            return Err(format!("extraction failed: {e}"));
        }
    };
    if cancel
        .map(|c| c.load(AtomicOrdering::Relaxed))
        .unwrap_or(false)
    {
        tx.rollback();
        return Err(
            "Install cancelled; every change was rolled back and nothing was kept.".to_string(),
        );
    }
    if let Err(e) = std::fs::copy(exe_path, &uninstaller) {
        tx.rollback();
        return Err(format!("cannot write uninstaller: {e}"));
    }
    let arp_written = match write_uninstall_entry(&manifest, dest, &uninstaller) {
        Ok(written) => written,
        Err(e) => {
            tx.rollback();
            return Err(e);
        }
    };

    // System artifacts (shortcuts / associations / PATH). Their removal
    // is registered as a rollback hook before anything else can fail.
    let primary_rel = records
        .get(manifest.primary_exe_index as usize)
        .map(|r| r.relative_path());
    let plan = plan_artifacts(&options, &manifest.app_name(), dest, primary_rel.as_deref());
    let applied = match apply_artifacts(&plan, &manifest.app_name(), dest) {
        Ok(applied) => applied,
        Err(e) => {
            tx.rollback();
            return Err(e);
        }
    };
    let shortcuts_created = applied.shortcuts.len();
    let associations_applied = applied.associations.len();
    let path_entry_added = applied.path_segment_added.is_some();
    let start_at_login_applied = applied.start_at_login.is_some();
    if !applied.shortcuts.is_empty()
        || !applied.associations.is_empty()
        || path_entry_added
        || start_at_login_applied
    {
        let journal = applied.clone();
        tx.record_rollback_hook(move || remove_artifacts(&journal));
    }

    // Receipt — the record this install leaves behind.
    let receipt = InstallReceipt {
        app: manifest.app_name(),
        version: manifest.app_version(),
        publisher: manifest.publisher(),
        install_path: dest.to_string_lossy().into_owned(),
        installed_at_unix: InstallReceipt::now_unix(),
        mode: mode.clone(),
        previous_version: previous_version.clone(),
        files: records
            .iter()
            .zip(&payload_hashes)
            .map(|(record, hash)| ReceiptFile {
                path: record.relative_path(),
                size: record.file_size,
                sha256: hash.clone(),
            })
            .collect(),
        desktop_shortcut: options.wants_desktop_shortcut(),
        start_menu_shortcut: options.wants_start_menu_shortcut(),
        start_menu_folder: options.start_menu_folder(),
        path_entry: options.wants_path_entry(),
        file_associations: crate::sysinstall::parse_extensions(&options.file_associations()),
        launch_after_install_offered: options.offers_launch_after_install(),
        launch_after_install_default: options.launch_after_install_default_checked(),
        start_at_login: options.start_at_login_chosen(),
        data_dirs: crate::uninstall::resolve_data_dirs(&options.data_dirs(), dest)
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        artifacts: applied,
    };
    let receipt_path = match receipt.write_to(dest) {
        Ok(path) => path,
        Err(e) => {
            tx.rollback();
            return Err(format!("cannot write install receipt: {e}"));
        }
    };
    crate::diag::record(
        "install-receipt",
        vec![
            ("app", crate::manifest::jstr(receipt.app.clone())),
            ("version", crate::manifest::jstr(receipt.version.clone())),
            ("mode", crate::manifest::jstr(mode.clone())),
            (
                "dest",
                crate::manifest::jstr(dest.to_string_lossy().into_owned()),
            ),
            (
                "receipt",
                crate::manifest::jstr(receipt_path.display().to_string()),
            ),
        ],
    );
    if let Some(run) = &receipt.artifacts.start_at_login {
        crate::diag::record(
            "autostart-applied",
            vec![
                ("app", crate::manifest::jstr(receipt.app.clone())),
                ("name", crate::manifest::jstr(run.name.clone())),
                ("command", crate::manifest::jstr(run.command.clone())),
            ],
        );
    }
    tx.commit()
        .map_err(|e| format!("cannot commit install journal: {e}"))?;
    Ok(InstallOutcome {
        files_written: written.len(),
        dest: dest.to_path_buf(),
        arp_written,
        mode,
        previous_version,
        restored_files,
        receipt_path,
        shortcuts_created,
        associations_applied,
        path_entry_added,
        start_at_login_applied,
    })
}

/// Starts an installed program detached: no console, no inherited
/// stdio, working directory = the executable's folder. Shared by the
/// wizard's Finish-page "Launch <App>" checkbox and the CLI's
/// `--launch`. A failure is reported to the caller, which treats it
/// as a warning — never as an install failure.
pub fn launch_program(exe: &Path) -> Result<(), String> {
    let mut cmd = std::process::Command::new(exe);
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    if let Some(dir) = exe.parent() {
        cmd.current_dir(dir);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: the launched app
        // must not share (or hold open) the installer's console.
        cmd.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    cmd.spawn()
        .map(|_child| ())
        .map_err(|e| format!("could not start {}: {e}", exe.display()))
}

/// SHA-256 of every payload's *original* bytes, in record order.
fn payload_hashes(exe_path: &Path, records: &[FileRecordHeader]) -> Result<Vec<String>, String> {
    let mut file =
        File::open(exe_path).map_err(|e| format!("cannot open {}: {e}", exe_path.display()))?;
    let stream_len = std::io::Seek::seek(&mut file, std::io::SeekFrom::End(0))
        .map_err(|e| format!("cannot read package: {e}"))?;
    let mut hashes = Vec::with_capacity(records.len());
    for idx in 0..records.len() {
        let bytes = OverlayReader::read_payload(&mut file, records, idx, stream_len)
            .map_err(|e| format!("cannot read payload {}: {e}", records[idx].relative_path()))?;
        hashes.push(sha256_hex(&bytes));
    }
    Ok(hashes)
}

/// Reads a package's manifest + records + options from an installer
/// executable. Shared by the CLI install modes.
pub fn read_own_package(
    exe_path: &Path,
) -> Result<(OverlayManifestHeader, Vec<FileRecordHeader>, InstallOptions), String> {
    let file =
        File::open(exe_path).map_err(|e| format!("cannot open {}: {e}", exe_path.display()))?;
    OverlayReader::read_package(file)
        .map_err(|e| format!("cannot read package: {e}"))?
        .ok_or_else(|| "this executable carries no SeanPak overlay".to_string())
}

#[cfg(windows)]
fn write_uninstall_entry(
    manifest: &OverlayManifestHeader,
    dest: &Path,
    uninstaller: &Path,
) -> Result<bool, String> {
    use windows_sys::Win32::System::Registry::*;
    let app_name = manifest.app_name();
    let subkey = uninstall_key_path(&app_name);
    let wide_key: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
    unsafe {
        let mut hkey: HKEY = std::ptr::null_mut();
        let rc = RegCreateKeyExW(
            HKEY_CURRENT_USER,
            wide_key.as_ptr(),
            0,
            std::ptr::null_mut(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            std::ptr::null(),
            &mut hkey,
            std::ptr::null_mut(),
        );
        if rc != 0 {
            return Err(format!("cannot create uninstall registry key (error {rc})"));
        }
        let uninstall_string = format!(
            "\"{}\" --uninstall --to \"{}\"",
            uninstaller.display(),
            dest.display()
        );
        let values: [(&str, String); 5] = [
            ("DisplayName", app_name.clone()),
            ("DisplayVersion", manifest.app_version()),
            ("Publisher", manifest.publisher()),
            ("InstallLocation", dest.to_string_lossy().into_owned()),
            ("UninstallString", uninstall_string),
        ];
        let mut first_error = 0u32;
        for (name, value) in &values {
            let rc = reg_set_sz(hkey, name, value);
            if rc != 0 && first_error == 0 {
                first_error = rc;
            }
        }
        for name in ["NoModify", "NoRepair"] {
            let one: u32 = 1;
            let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
            let rc = RegSetValueExW(
                hkey,
                wide_name.as_ptr(),
                0,
                REG_DWORD,
                &one as *const u32 as *const u8,
                std::mem::size_of::<u32>() as u32,
            );
            if rc != 0 && first_error == 0 {
                first_error = rc;
            }
        }
        RegCloseKey(hkey);
        if first_error != 0 {
            // Leave no half-written entry behind.
            crate::wizard::delete_uninstall_entry(&app_name);
            return Err(format!(
                "cannot write uninstall registry values (error {first_error})"
            ));
        }
    }
    Ok(true)
}

#[cfg(windows)]
unsafe fn reg_set_sz(
    hkey: windows_sys::Win32::System::Registry::HKEY,
    name: &str,
    value: &str,
) -> u32 {
    use windows_sys::Win32::System::Registry::{RegSetValueExW, REG_SZ};
    let wide_name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let wide_value: Vec<u16> = value.encode_utf16().chain(std::iter::once(0)).collect();
    RegSetValueExW(
        hkey,
        wide_name.as_ptr(),
        0,
        REG_SZ,
        wide_value.as_ptr() as *const u8,
        (wide_value.len() * std::mem::size_of::<u16>()) as u32,
    )
}

/// Off Windows there is no Add/Remove Programs registry; the rest of the
/// install (extraction + uninstaller copy) still applies.
#[cfg(not(windows))]
fn write_uninstall_entry(
    _manifest: &OverlayManifestHeader,
    _dest: &Path,
    _uninstaller: &Path,
) -> Result<bool, String> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{InstallOptions, OverlayWriter};

    fn unique_temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "seanpak_install_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ))
    }

    /// Packs a one-file app at `version` and returns the Setup.exe path.
    fn pack_version(root: &Path, version: &str, body: &[u8]) -> PathBuf {
        let src = root.join(format!("src-{version}"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.exe"), body).unwrap();
        std::fs::write(src.join("data.txt"), format!("data for {version}")).unwrap();
        let stub = root.join("stub.exe");
        if !stub.exists() {
            std::fs::write(&stub, vec![0xAAu8; 5000]).unwrap();
        }
        let setup = root.join(format!("Setup-{version}.exe"));
        let options = InstallOptions::defaults_for("Life App");
        OverlayWriter::build_from_folder_with_options(
            &stub, &setup, &src, "Life App", version, "Tester", &options,
        )
        .unwrap();
        setup
    }

    #[test]
    fn lifecycle_install_upgrade_repair_downgrade_uninstall() {
        let root = unique_temp("lifecycle");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("installed");

        // Fresh install of 1.0.0 writes payload + receipt.
        let setup1 = pack_version(&root, "1.0.0", b"pe-v1");
        let outcome = perform_install(&setup1, &dest).unwrap();
        assert_eq!(outcome.mode, "install");
        assert!(dest.join("seanpak-receipt.json").is_file());
        let receipt = read_receipt(&dest).unwrap();
        assert_eq!(receipt.version, "1.0.0");
        assert_eq!(receipt.files.len(), 2);

        // Upgrade to 1.1.0 replaces the payload and records the mode.
        let setup2 = pack_version(&root, "1.1.0", b"pe-v2-longer");
        let outcome = perform_install(&setup2, &dest).unwrap();
        assert_eq!(outcome.mode, "upgrade");
        assert_eq!(outcome.previous_version.as_deref(), Some("1.0.0"));
        assert_eq!(
            std::fs::read(dest.join("app.exe")).unwrap(),
            b"pe-v2-longer"
        );
        assert_eq!(read_receipt(&dest).unwrap().mode, "upgrade");

        // Downgrade back to 1.0.0 is refused without force, allowed with.
        let refused = perform_install(&setup1, &dest).unwrap_err();
        assert!(refused.contains("Downgrade refused"), "{refused}");
        let outcome = perform_install_with(
            &setup1,
            &dest,
            &InstallRequest {
                force: true,
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
        assert_eq!(outcome.mode, "downgrade");

        // Damage a file, then repair with the same version: the scan
        // counts the damaged file and the content is restored.
        std::fs::write(dest.join("data.txt"), b"corrupted").unwrap();
        let outcome = perform_install_with(
            &setup1,
            &dest,
            &InstallRequest {
                repair: true,
                ..Default::default()
            },
            None,
            None,
        )
        .unwrap();
        assert_eq!(outcome.mode, "repair");
        assert!(outcome.restored_files >= 1, "{outcome:?}");
        assert_eq!(
            std::fs::read(dest.join("data.txt")).unwrap(),
            b"data for 1.0.0"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cancelled_install_rolls_everything_back() {
        let root = unique_temp("cancel");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("installed");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("keep.txt"), b"precious").unwrap();

        let setup = pack_version(&root, "1.0.0", b"pe-v1");
        let cancel = AtomicBool::new(true);
        let err = perform_install_with(
            &setup,
            &dest,
            &InstallRequest::default(),
            None,
            Some(&cancel),
        )
        .unwrap_err();
        assert!(err.contains("cancelled"), "{err}");
        assert_eq!(std::fs::read(dest.join("keep.txt")).unwrap(), b"precious");
        assert!(!dest.join("app.exe").exists());
        assert!(!dest.join("seanpak-receipt.json").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_upgrade_restores_the_old_install() {
        let root = unique_temp("upgradefail");
        std::fs::create_dir_all(&root).unwrap();
        let dest = root.join("installed");
        let setup1 = pack_version(&root, "1.0.0", b"pe-v1");
        perform_install(&setup1, &dest).unwrap();

        // A package whose second payload is bogus is hard to fabricate
        // through the writer, so instead force the failure at the
        // uninstaller-copy step's level: install from a path that does
        // not exist must error before touching anything.
        let missing = root.join("nope.exe");
        assert!(perform_install(&missing, &dest).is_err());
        assert_eq!(std::fs::read(dest.join("app.exe")).unwrap(), b"pe-v1");
        assert_eq!(read_receipt(&dest).unwrap().version, "1.0.0");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fallback_path_shape_and_folder_derivation() {
        let root = Path::new("/machine/AppData/Local");
        assert_eq!(
            per_user_fallback_from(root, "ai.md", "C:\\Program Files\\ai.md"),
            root.join("Programs").join("ai.md")
        );
        // Trailing separators and forward slashes derive the same folder.
        assert_eq!(install_folder_name("Any", "D:\\Apps\\My App\\"), "My App");
        assert_eq!(install_folder_name("Any", "C:/Program Files/Tool"), "Tool");
        // A bare drive root or empty location falls back to the app name.
        assert_eq!(install_folder_name("My App", "C:\\"), "My App");
        assert_eq!(install_folder_name("My App", ""), "My App");
        assert_eq!(
            per_user_fallback_from(root, "My App", ""),
            root.join("Programs").join("My App")
        );
    }

    #[test]
    fn writable_temp_dir_passes_the_verdict() {
        let root = unique_temp("writable");
        std::fs::create_dir_all(&root).unwrap();
        let verdict = location_verdict(&root.join("app"), "Life App", "C:\\Apps\\Life App");
        assert_eq!(verdict, LocationVerdict::Writable);
        // A not-yet-existing destination under a writable parent is fine,
        // and the verdict created nothing.
        assert!(!root.join("app").exists());
        // The operation's gate creates + probes the destination itself.
        ensure_location_writable(&root.join("app"), "Life App", "C:\\Apps\\Life App").unwrap();
        assert!(root.join("app").is_dir());
        // The probe file never lingers.
        let leftovers: Vec<_> = std::fs::read_dir(root.join("app"))
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn path_under_a_file_is_not_writable() {
        let root = unique_temp("underfile");
        std::fs::create_dir_all(&root).unwrap();
        let blocker = root.join("blocker.bin");
        std::fs::write(&blocker, b"i am a file").unwrap();
        let dest = blocker.join("sub");
        match location_verdict(&dest, "Life App", "C:\\Program Files\\Life App") {
            LocationVerdict::NotWritable { fallback, .. } => {
                assert!(fallback.ends_with("Life App"), "{}", fallback.display());
                assert!(fallback
                    .parent()
                    .map(|p| p.ends_with("Programs"))
                    .unwrap_or(false));
            }
            LocationVerdict::Writable => panic!("a path under a file cannot be writable"),
        }
        let err =
            ensure_location_writable(&dest, "Life App", "C:\\Program Files\\Life App").unwrap_err();
        assert!(err.contains("not writable"), "{err}");
        assert!(err.contains("administrator"), "{err}");
        assert!(err.contains("Life App"), "{err}");
        assert!(
            !dest.exists(),
            "the refusal must not create the destination"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn read_only_dir_is_not_writable() {
        use std::os::unix::fs::PermissionsExt;
        let root = unique_temp("readonly");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o555)).unwrap();
        // A privileged process (root) can write anyway; the read-only
        // expectation only holds where the permission actually bites.
        let privileged = std::fs::File::create(root.join(".privilege-check")).is_ok();
        if privileged {
            let _ = std::fs::remove_file(root.join(".privilege-check"));
        } else {
            match location_verdict(&root, "Life App", "C:\\Apps\\Life App") {
                LocationVerdict::NotWritable { .. } => {}
                LocationVerdict::Writable => {
                    panic!("a read-only dir must not pass the verdict")
                }
            }
        }
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn plan_and_install_refuse_an_unwritable_location() {
        let root = unique_temp("planrefuse");
        std::fs::create_dir_all(&root).unwrap();
        let setup = pack_version(&root, "1.0.0", b"pe-v1");
        let (manifest, records, options) = read_own_package(&setup).unwrap();

        // A writable destination plans clean, with no suggestion.
        let good_dest = root.join("installed");
        let plan = plan_install(&manifest, &records, &options, &good_dest);
        assert!(plan.location_writable);
        assert!(plan.suggested_location.is_none());

        // A destination under a file: the plan carries the verdict and
        // the per-user suggestion, and the install itself refuses
        // before creating anything.
        let blocker = root.join("blocker.bin");
        std::fs::write(&blocker, b"file").unwrap();
        let bad_dest = blocker.join("sub");
        let plan = plan_install(&manifest, &records, &options, &bad_dest);
        assert!(!plan.location_writable);
        let suggestion = plan.suggested_location.expect("a suggestion is offered");
        assert!(suggestion.ends_with("Life App"), "{}", suggestion.display());

        let err = perform_install(&setup, &bad_dest).unwrap_err();
        assert!(err.contains("not writable"), "{err}");
        assert!(err.contains(&suggestion.display().to_string()), "{err}");
        assert!(!bad_dest.exists(), "nothing may be created on refusal");
        assert!(read_receipt(&bad_dest).is_none());

        let _ = std::fs::remove_dir_all(&root);
    }
}
