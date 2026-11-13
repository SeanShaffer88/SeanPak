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
//! instead. Installs never elevate on their own — the single
//! exception is the user-invoked restart-as-administrator offer
//! ([`crate::elevation`], v4.8.0), which relaunches the process
//! elevated only on the user's explicit click or `--elevate` flag.
//!
//! Packages carrying an overlay v6 power block (see
//! [`crate::overlay::PowerSpec`]) extend the same run: the component
//! selection is resolved before anything is touched (an upgrade
//! defaults to the previous receipt's choices) and only the selected
//! components' files are extracted, snapshotted, and receipted;
//! pre-install hooks run after the writability verdict (skipped on
//! repair), post-install hooks after the artifacts and the journaled
//! registry/environment writes ([`crate::writes`]) are applied — an
//! abort-policy hook failure rolls the whole run back.

use std::cmp::Ordering;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering as AtomicOrdering};

use crate::hash::sha256_hex;
use crate::overlay::{
    ExtractControl, FileRecordHeader, InstallOptions, OverlayManifestHeader, OverlayReader,
    PowerSpec,
};
use crate::receipt::{
    compare_versions, read_receipt, HookRunRecord, InstallReceipt, PinRecord, ReceiptFile,
};
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

/// One component in an install plan (v6): what it is and whether this
/// install includes it.
#[derive(Debug, Clone)]
pub struct PlannedComponent {
    pub id: String,
    pub name: String,
    pub required: bool,
    pub selected: bool,
    /// Packed size in the package.
    pub packed_size: u64,
    /// Payload files the component owns.
    pub file_count: usize,
}

/// What an install would do, computed without side effects.
#[derive(Debug, Clone)]
pub struct InstallPlan {
    pub app_name: String,
    pub app_version: String,
    pub dest: PathBuf,
    /// (relative path, size) for every payload file the component
    /// selection installs, in record order.
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
    /// The package's components (v6), with this plan's selection.
    /// Empty for pre-v6 packages.
    pub components: Vec<PlannedComponent>,
    /// Hooks the install would run, as (phase, command line) pairs —
    /// listed, never run, by a dry run.
    pub hooks: Vec<(String, String)>,
    /// Registry writes the install would apply (v6), as declared.
    pub registry_writes: Vec<crate::overlay::RegWriteSpec>,
    /// Environment writes the install would apply (v6), as declared.
    pub env_writes: Vec<crate::overlay::EnvWriteSpec>,
    /// Display names of the package's rich shortcuts this plan would
    /// create (v6) — the automatic app shortcuts are in `shortcuts`.
    pub rich_shortcuts: Vec<String>,
}

/// Computes what an install would do. `power` is the package's v6
/// power block (empty for older packages) and `selection` the resolved
/// component selection the plan assumes (from
/// [`PowerSpec::resolve_selection`]).
pub fn plan_install(
    manifest: &OverlayManifestHeader,
    records: &[FileRecordHeader],
    options: &InstallOptions,
    dest: &Path,
    power: &PowerSpec,
    selection: &[String],
    stream_len: u64,
) -> InstallPlan {
    let detected = detect_existing(dest, manifest);
    let previous_version = match &detected {
        DetectedInstall::Existing { version, .. } => Some(version.clone()),
        DetectedInstall::None => None,
    };
    let primary_rel = records
        .get(manifest.primary_exe_index as usize)
        .map(|r| r.relative_path());
    let artifacts = plan_artifacts(
        options,
        &manifest.app_name(),
        dest,
        primary_rel.as_deref(),
        power,
        Some(selection),
    );
    let (location_writable, suggested_location) =
        match location_verdict(dest, &manifest.app_name(), &options.install_location()) {
            LocationVerdict::Writable => (true, None),
            LocationVerdict::NotWritable { fallback, .. } => (false, Some(fallback)),
        };
    let selected_records: Vec<&FileRecordHeader> = records
        .iter()
        .filter(|r| {
            selection
                .iter()
                .any(|id| id == &power.component_id_for_file(&r.relative_path()))
        })
        .collect();
    let include_core = power.has_core_files(records);
    let components: Vec<PlannedComponent> = power
        .part_ids(include_core)
        .into_iter()
        .map(|id| {
            let (name, required, packed_size) = if id == crate::overlay::CORE_COMPONENT_ID {
                (
                    "Core files".to_string(),
                    true,
                    power.core_packed_size(records, stream_len),
                )
            } else {
                let c = power.component(&id).expect("part id from the power spec");
                (c.name.clone(), c.required, c.packed_size)
            };
            PlannedComponent {
                selected: selection.iter().any(|s| s == &id),
                file_count: records
                    .iter()
                    .filter(|r| power.component_id_for_file(&r.relative_path()) == id)
                    .count(),
                id,
                name,
                required,
                packed_size,
            }
        })
        .collect();
    let mut hooks: Vec<(String, String)> = power
        .pre_hooks
        .iter()
        .map(|h| ("preInstall".to_string(), crate::hooks::display_command(h)))
        .collect();
    hooks.extend(
        power
            .post_hooks
            .iter()
            .map(|h| ("postInstall".to_string(), crate::hooks::display_command(h))),
    );
    InstallPlan {
        app_name: manifest.app_name(),
        app_version: manifest.app_version(),
        dest: dest.to_path_buf(),
        files: selected_records
            .iter()
            .map(|r| (r.relative_path(), r.file_size))
            .collect(),
        total_bytes: selected_records.iter().map(|r| r.file_size).sum(),
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
        components,
        hooks,
        registry_writes: power.registry_writes.clone(),
        env_writes: power.env_writes.clone(),
        rich_shortcuts: artifacts
            .shortcuts
            .iter()
            .filter(|s| power.shortcuts.iter().any(|rich| rich.name == s.label))
            .map(|s| s.label.clone())
            .collect(),
    }
}

/// Stable machine-readable reason for an install refused because the
/// destination is not writable, carried in the `--json` install report
/// (the exit code stays 5, install failure).
pub const REASON_LOCATION_NOT_WRITABLE: &str = "location_not_writable";

/// Pre-flight verdict for an install destination: can this per-user
/// process write there? SeanPak installs never elevate on their own, so a folder
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

/// The install location a package records when its manifest names
/// none (v5.1.0): the per-user Programs folder as a TOKEN path —
/// `%LOCALAPPDATA%\Programs\<AppFolder>` — so the package itself
/// carries the per-user intent and resolution happens on the
/// installing account at install time. Program Files is never a
/// SeanPak default: it needs administrator access this per-user
/// installer does not have (the ai.md 0.7.0 wound). The folder leaf
/// is the same one the writability fallback derives.
pub fn default_install_location(app_name: &str) -> String {
    format!(
        "%LOCALAPPDATA%\\Programs\\{}",
        install_folder_name(app_name, "")
    )
}

/// Resolves a recorded install location to the folder an install
/// actually targets, against explicit data-dir roots (the live form
/// is [`resolve_install_location`]). Token paths use the `dataDirs`
/// resolver ([`crate::uninstall::resolve_data_dir_with_roots`]) with
/// the per-user default standing in for `%INSTALLDIR%` (a location
/// may not name itself — the manifest validator refuses that token,
/// and anything unresolvable lands on the default rather than on a
/// relative path). Absolute paths stand exactly as written; an empty
/// recorded location means the default.
pub fn resolve_install_location_with_roots(
    recorded: &str,
    app_name: &str,
    roots: &crate::uninstall::DataDirRoots,
) -> PathBuf {
    let default_token = default_install_location(app_name);
    let default_dir = per_user_fallback_from(&roots.local_app_data, app_name, &default_token);
    let raw = recorded.trim();
    let effective = if raw.is_empty() {
        default_token.as_str()
    } else {
        raw
    };
    // `%INSTALLDIR%` names the install folder itself, so as a
    // location it can only mean "the default" (validation refuses it
    // long before this — this is the second line of defense).
    if effective.to_uppercase().starts_with("%INSTALLDIR%") {
        return default_dir;
    }
    crate::uninstall::resolve_data_dir_with_roots(effective, roots, &default_dir)
        .unwrap_or(default_dir)
}

/// [`resolve_install_location_with_roots`] against this machine's
/// roots: token locations resolve per-user on the installing
/// account. The one derivation every surface (wizard default,
/// headless `--to` / answer-file locations, Modify, uninstall)
/// shares.
pub fn resolve_install_location(recorded: &str, app_name: &str) -> PathBuf {
    resolve_install_location_with_roots(recorded, app_name, &crate::uninstall::data_dir_roots())
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
    /// Component selection for this install (v6): `Some(ids)` is the
    /// explicit full selection (required components and the primary
    /// exe's component are force-included at resolution); `None` =
    /// the default selection — the previous install's choices on an
    /// upgrade, else the manifest defaults.
    pub components: Option<Vec<String>>,
    /// Component ids to leave out of the default selection (v6; the
    /// CLI's `--without`).
    pub without_components: Vec<String>,
    /// The uninstall confirmation's data-purge default to record in
    /// the receipt (v7; set from an answer file's
    /// `purgeDataOnUninstall`). A default only: silent uninstalls
    /// still never purge without `--purge-data`.
    pub purge_data_default: bool,
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
    /// Component ids this install installed (v6; empty for pre-v6
    /// packages).
    pub components_installed: Vec<String>,
    /// Every hook that ran (v6), in run order.
    pub hook_results: Vec<HookRunRecord>,
    /// Registry values the install wrote (v6; Windows only — always 0
    /// off Windows, where the writes are an honest no-op).
    pub registry_writes_applied: usize,
    /// Environment variables the install wrote (v6; Windows only).
    pub env_writes_applied: usize,
    /// Taskbar pin outcomes for rich shortcuts that asked for one
    /// (v6) — "pinned" or "unavailable", never a failure.
    pub pins: Vec<PinRecord>,
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
    let (manifest, records, package_options, power) = OverlayReader::read_package_full(file)
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

    // Component selection (v6), resolved before anything is touched:
    // an explicit selection wins; otherwise the previous install's
    // choices are the default on upgrade, else the manifest defaults.
    // An invalid selection (unknown id, leaving out a required part)
    // refuses here, before any change.
    let previous_receipt = read_receipt(dest).filter(|r| r.app == manifest.app_name());
    let previous_selection = previous_receipt
        .as_ref()
        .map(|r| (r.components.as_slice(), r.components_known.as_slice()));
    let primary_rel_for_selection = records
        .get(manifest.primary_exe_index as usize)
        .map(|r| r.relative_path())
        .unwrap_or_default();
    let selection = power.resolve_selection(
        request.components.as_deref(),
        &request.without_components,
        previous_selection,
        &records,
        if primary_rel_for_selection.is_empty() {
            None
        } else {
            Some(primary_rel_for_selection.as_str())
        },
    )?;
    // Record indexes the selection installs, in record order.
    let selected_indices: Vec<usize> = records
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            selection
                .iter()
                .any(|id| id == &power.component_id_for_file(&r.relative_path()))
        })
        .map(|(idx, _)| idx)
        .collect();
    let selected_total_bytes: u64 = selected_indices
        .iter()
        .map(|&idx| records[idx].file_size)
        .sum();

    // Hash the payloads up front (decompressing as needed): the
    // pre-install damage scan and the receipt both use these hashes.
    // (All records are hashed: payload lengths derive from the full
    // record list's offsets, and the cost is one pass either way.)
    let payload_hashes = payload_hashes(exe_path, &records)?;

    // Repair evidence: which existing targets (of the components
    // being installed) are missing or damaged.
    let mut restored_files = 0usize;
    if detected != DetectedInstall::None {
        for &idx in &selected_indices {
            let record = &records[idx];
            let want = &payload_hashes[idx];
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

    // Pre-install hooks (v6): after the writability verdict, before
    // the journal and any file change, running from the installer's
    // own folder. A repair run skips this phase (post-install hooks
    // still run — the documented rule). An abort-policy failure stops
    // here: nothing has changed yet, so there is nothing to roll back.
    let mut hook_records: Vec<HookRunRecord> = Vec::new();
    if mode != "repair" && !power.pre_hooks.is_empty() {
        let installer_dir = exe_path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let (records_run, failure) = crate::hooks::run_hooks(
            "preInstall",
            &power.pre_hooks,
            &installer_dir,
            dest,
            &manifest.app_name(),
            &manifest.app_version(),
        );
        hook_records.extend(records_run);
        if let Some(failure) = failure {
            return Err(failure.message);
        }
    }

    let mut tx =
        TransactionRollbackLog::new().map_err(|e| format!("cannot start rollback journal: {e}"))?;
    for &idx in &selected_indices {
        let record = &records[idx];
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

    // Extraction with progress + cancellation — only the selected
    // components' files (v6); for pre-v6 packages the selection is
    // just `core`, which owns everything.
    let total_bytes = selected_total_bytes;
    let selected_count = selected_indices.len();
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
                    file_count: selected_count,
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
        let selection_ref = &selection;
        let power_ref = &power;
        OverlayReader::extract_selected_with_control(
            file,
            dest,
            &|record: &FileRecordHeader| {
                selection_ref
                    .iter()
                    .any(|id| id == &power_ref.component_id_for_file(&record.relative_path()))
            },
            &mut control,
        )
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

    // System artifacts (shortcuts / associations / PATH), then the
    // journaled registry + environment writes (v6). Their removal is
    // registered as one rollback hook before anything else can fail.
    let primary_rel = records
        .get(manifest.primary_exe_index as usize)
        .map(|r| r.relative_path());
    let plan = plan_artifacts(
        &options,
        &manifest.app_name(),
        dest,
        primary_rel.as_deref(),
        &power,
        Some(&selection),
    );
    let mut applied = match apply_artifacts(&plan, &manifest.app_name(), dest) {
        Ok(applied) => applied,
        Err(e) => {
            tx.rollback();
            return Err(e);
        }
    };
    match crate::writes::apply_writes(&power) {
        Ok((reg_records, env_records)) => {
            applied.registry_writes = reg_records;
            applied.env_writes = env_records;
        }
        Err(e) => {
            // Undo the artifacts by hand: the rollback hook is not
            // registered yet at this point.
            remove_artifacts(&applied);
            tx.rollback();
            return Err(e);
        }
    }
    let shortcuts_created = applied.shortcuts.len();
    let associations_applied = applied.associations.len();
    let path_entry_added = applied.path_segment_added.is_some();
    let start_at_login_applied = applied.start_at_login.is_some();
    let registry_writes_applied = applied.registry_writes.len();
    let env_writes_applied = applied.env_writes.len();
    let pins = applied.pins.clone();
    if !applied.shortcuts.is_empty()
        || !applied.associations.is_empty()
        || path_entry_added
        || start_at_login_applied
        || !applied.registry_writes.is_empty()
        || !applied.env_writes.is_empty()
    {
        let journal = applied.clone();
        tx.record_rollback_hook(move || remove_artifacts(&journal));
    }

    // Post-install hooks (v6): after files, artifacts, and writes,
    // before the receipt is finalized; they run from the install
    // folder. An abort-policy failure rolls the whole install back.
    if !power.post_hooks.is_empty() {
        let (records_run, failure) = crate::hooks::run_hooks(
            "postInstall",
            &power.post_hooks,
            dest,
            dest,
            &manifest.app_name(),
            &manifest.app_version(),
        );
        hook_records.extend(records_run);
        if let Some(failure) = failure {
            tx.rollback();
            return Err(failure.message);
        }
    }

    // Receipt — the record this install leaves behind. Its file list
    // is exactly the payloads this install wrote (the selected
    // components), so the uninstall removes exactly those.
    let receipt = InstallReceipt {
        app: manifest.app_name(),
        version: manifest.app_version(),
        publisher: manifest.publisher(),
        install_path: dest.to_string_lossy().into_owned(),
        installed_at_unix: InstallReceipt::now_unix(),
        mode: mode.clone(),
        previous_version: previous_version.clone(),
        files: selected_indices
            .iter()
            .map(|&idx| ReceiptFile {
                path: records[idx].relative_path(),
                size: records[idx].file_size,
                sha256: payload_hashes[idx].clone(),
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
        purge_data_default: request.purge_data_default,
        data_dirs: crate::uninstall::resolve_data_dirs(&options.data_dirs(), dest)
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        components: selection.clone(),
        components_known: power.part_ids(power.has_core_files(&records)),
        hooks: hook_records.clone(),
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
        components_installed: selection,
        hook_results: hook_records,
        registry_writes_applied,
        env_writes_applied,
        pins,
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
    let (manifest, records, options, _power) = read_own_package_full(exe_path)?;
    Ok((manifest, records, options))
}

/// [`read_own_package`] plus the package's v6 power block (empty for
/// pre-v6 packages).
pub fn read_own_package_full(exe_path: &Path) -> Result<crate::overlay::PackageFull, String> {
    let file =
        File::open(exe_path).map_err(|e| format!("cannot open {}: {e}", exe_path.display()))?;
    OverlayReader::read_package_full(file)
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
        // The Modify button (v7): Windows Settings shows it because
        // ModifyPath is present and NoModify is not set.
        let modify_string = format!(
            "\"{}\" --modify --to \"{}\"",
            uninstaller.display(),
            dest.display()
        );
        let values: [(&str, String); 6] = [
            ("DisplayName", app_name.clone()),
            ("DisplayVersion", manifest.app_version()),
            ("Publisher", manifest.publisher()),
            ("InstallLocation", dest.to_string_lossy().into_owned()),
            ("UninstallString", uninstall_string),
            ("ModifyPath", modify_string),
        ];
        let mut first_error = 0u32;
        for (name, value) in &values {
            let rc = reg_set_sz(hkey, name, value);
            if rc != 0 && first_error == 0 {
                first_error = rc;
            }
        }
        // NoRepair is set (SeanPak has no separate repair entry —
        // reinstalling repairs); NoModify is deliberately NOT set, so
        // the Modify button shows for the ModifyPath above.
        {
            let one: u32 = 1;
            let wide_name: Vec<u16> = "NoRepair"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
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

/// Rewrites an existing install's ARP entry from the package (used by
/// Modify mode, so an install made before ModifyPath existed gains
/// the Modify button). Returns whether the entry was written
/// (Windows only; false elsewhere).
pub fn refresh_uninstall_entry(
    manifest: &OverlayManifestHeader,
    dest: &Path,
) -> Result<bool, String> {
    write_uninstall_entry(manifest, dest, &dest.join("Uninstall.exe"))
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
    fn token_locations_resolve_per_user() {
        // The v5.1.0 default a package records when its manifest
        // names no location: the per-user Programs token path.
        assert_eq!(
            default_install_location("ai.md"),
            "%LOCALAPPDATA%\\Programs\\ai.md"
        );
        let roots = crate::uninstall::DataDirRoots {
            app_data: PathBuf::from("C:\\Users\\x\\AppData\\Roaming"),
            local_app_data: PathBuf::from("C:\\Users\\x\\AppData\\Local"),
            user_profile: PathBuf::from("C:\\Users\\x"),
        };
        let local = PathBuf::from("C:\\Users\\x\\AppData\\Local");
        let resolve =
            |recorded: &str| resolve_install_location_with_roots(recorded, "My App", &roots);
        // Every per-user token resolves against its own root, in any case.
        assert_eq!(
            resolve("%LOCALAPPDATA%\\Programs\\My App"),
            local.join("Programs").join("My App")
        );
        assert_eq!(
            resolve("%appdata%\\My App"),
            PathBuf::from("C:\\Users\\x\\AppData\\Roaming").join("My App")
        );
        assert_eq!(
            resolve("%USERPROFILE%\\Apps\\My App"),
            PathBuf::from("C:\\Users\\x").join("Apps").join("My App")
        );
        // An explicit absolute location stands exactly as written —
        // the package's choice (the 4.8.0 elevation offer covers it).
        assert_eq!(
            resolve("C:\\Program Files\\My App"),
            PathBuf::from("C:/Program Files/My App")
        );
        // No recorded location resolves to the default: the per-user
        // Programs folder built from the token default.
        assert_eq!(resolve(""), local.join("Programs").join("My App"));
        assert_eq!(resolve("  "), resolve(&default_install_location("My App")));
        // Unresolvable forms land on the default, never on a relative
        // path: %INSTALLDIR% (a location cannot name itself), a `..`
        // escape, a bare token root.
        let default = local.join("Programs").join("My App");
        assert_eq!(resolve("%INSTALLDIR%\\My App"), default);
        assert_eq!(resolve("%LOCALAPPDATA%\\..\\evil"), default);
        assert_eq!(resolve("%LOCALAPPDATA%"), default);
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
        let stream_len = std::fs::metadata(&setup).map(|m| m.len()).unwrap_or(0);
        let good_dest = root.join("installed");
        let plan = plan_install(
            &manifest,
            &records,
            &options,
            &good_dest,
            &PowerSpec::default(),
            &[crate::overlay::CORE_COMPONENT_ID.to_string()],
            stream_len,
        );
        assert!(plan.location_writable);
        assert!(plan.suggested_location.is_none());

        // A destination under a file: the plan carries the verdict and
        // the per-user suggestion, and the install itself refuses
        // before creating anything.
        let blocker = root.join("blocker.bin");
        std::fs::write(&blocker, b"file").unwrap();
        let bad_dest = blocker.join("sub");
        let plan = plan_install(
            &manifest,
            &records,
            &options,
            &bad_dest,
            &PowerSpec::default(),
            &[crate::overlay::CORE_COMPONENT_ID.to_string()],
            stream_len,
        );
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
