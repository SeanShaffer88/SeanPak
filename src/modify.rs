//! The modify operation (v7) — the single implementation behind the
//! wizard's Modify mode, the ARP entry's Modify button, and the
//! `--modify` command.
//!
//! [`perform_modify`] re-opens an existing install (located by its
//! receipt) and reconciles it with new choices: option changes
//! (shortcuts, file associations, PATH entry, start-at-login, the
//! launch default) and component selection changes (add / remove
//! optional parts). Files of newly selected components are extracted,
//! files of deselected components are deleted, the system artifacts
//! are removed and re-applied per the new choices, and the receipt is
//! rewritten to describe the result.
//!
//! Safety: before anything changes, the rollback journal snapshots
//! the receipt and every file the reconcile will delete or overwrite
//! (the same [`TransactionRollbackLog`] installs use). If any step
//! fails — or the run is cancelled — the journal restores the files
//! and the pre-modify artifacts are re-applied best-effort, so a
//! failed modify leaves the previous install state in place.
//!
//! Modify runs no install hooks: hooks belong to the install event
//! (a repair runs the post phase; a modify is neither). The receipt
//! keeps the original install's hook records.

use std::collections::HashSet;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

use crate::hash::sha256_hex;
use crate::install::{ensure_location_writable, location_verdict, LocationVerdict, ProgressEvent};
use crate::overlay::{
    copy_str_to_fixed_pub, ExtractControl, FileRecordHeader, InstallOptions, OverlayReader,
    PowerSpec,
};
use crate::receipt::{read_receipt, InstallReceipt, PinRecord, ReceiptFile};
use crate::rollback::TransactionRollbackLog;
use crate::sysinstall::{apply_artifacts, plan_artifacts, remove_artifacts};

/// Caller choices for a modify run.
#[derive(Debug, Clone, Default)]
pub struct ModifyRequest {
    /// The full new install options (the wizard's harvested Options
    /// page, or the CLI's receipt state with flag deltas applied).
    /// `None` = keep the receipt's recorded choices.
    pub options_override: Option<InstallOptions>,
    /// Explicit start-at-login choice for this run.
    pub start_at_login: Option<bool>,
    /// Explicit full component selection; `None` = keep the previous
    /// install's selection (the receipt's choices are the default,
    /// as on upgrade).
    pub components: Option<Vec<String>>,
    /// Component ids to leave out (the CLI's `--without`).
    pub without_components: Vec<String>,
}

/// The option state a modify starts from and ends at, for plans and
/// reports.
#[derive(Debug, Clone, PartialEq)]
pub struct ModifyOptionSummary {
    pub desktop_shortcut: bool,
    pub start_menu_shortcut: bool,
    pub start_menu_folder: String,
    pub path_entry: bool,
    pub file_associations: Vec<String>,
    pub start_at_login: bool,
    pub launch_after_install_default: bool,
}

fn summarize(options: &InstallOptions) -> ModifyOptionSummary {
    ModifyOptionSummary {
        desktop_shortcut: options.wants_desktop_shortcut(),
        start_menu_shortcut: options.wants_start_menu_shortcut(),
        start_menu_folder: options.start_menu_folder(),
        path_entry: options.wants_path_entry(),
        file_associations: crate::sysinstall::parse_extensions(&options.file_associations()),
        start_at_login: options.start_at_login_chosen(),
        launch_after_install_default: options.launch_after_install_default_checked(),
    }
}

/// What a modify would do, computed without side effects (the
/// `--modify --dry-run` report).
#[derive(Debug, Clone)]
pub struct ModifyPlan {
    pub app_name: String,
    pub installed_version: String,
    pub package_version: String,
    pub dest: PathBuf,
    pub components_added: Vec<String>,
    pub components_removed: Vec<String>,
    pub components_kept: Vec<String>,
    /// (relative path, size) of payloads the modify would write.
    pub files_added: Vec<(String, u64)>,
    /// Relative paths the modify would delete.
    pub files_removed: Vec<String>,
    pub options_before: ModifyOptionSummary,
    pub options_after: ModifyOptionSummary,
    pub location_writable: bool,
    pub suggested_location: Option<PathBuf>,
}

/// What a modify did.
#[derive(Debug)]
pub struct ModifyOutcome {
    pub app_name: String,
    /// The package's version, now recorded in the receipt.
    pub version: String,
    pub previous_version: Option<String>,
    pub dest: PathBuf,
    pub files_added: usize,
    pub files_removed: usize,
    pub components_before: Vec<String>,
    pub components_after: Vec<String>,
    pub receipt_path: PathBuf,
    pub shortcuts_created: usize,
    pub associations_applied: usize,
    pub path_entry_added: bool,
    pub start_at_login_applied: bool,
    pub registry_writes_applied: usize,
    pub env_writes_applied: usize,
    pub pins: Vec<PinRecord>,
}

/// The install options an existing install is currently living with:
/// the package's recorded options with the choice fields replaced by
/// the receipt's record of what was actually applied. Shared by the
/// CLI (which applies flag deltas on top) and the wizard's Modify
/// mode (which seeds its Options page from exactly this).
pub fn options_from_receipt(
    package_options: &InstallOptions,
    receipt: &InstallReceipt,
) -> InstallOptions {
    let mut options = *package_options;
    options.desktop_shortcut = receipt.desktop_shortcut as u8;
    options.start_menu_shortcut = receipt.start_menu_shortcut as u8;
    copy_str_to_fixed_pub(
        &mut options.start_menu_folder_utf8,
        &receipt.start_menu_folder,
    );
    options.path_entry = receipt.path_entry as u8;
    let joined = receipt
        .file_associations
        .iter()
        .map(|e| format!(".{e}"))
        .collect::<Vec<_>>()
        .join(",");
    copy_str_to_fixed_pub(&mut options.file_associations_utf8, &joined);
    options.launch_after_install = receipt.launch_after_install_offered as u8;
    options.launch_after_install_default = receipt.launch_after_install_default as u8;
    if receipt.start_at_login {
        // The receipt proves the entry was applied; keep the option
        // offered so the user can turn it off again.
        options.start_at_login = 1;
    }
    options.start_at_login_default = receipt.start_at_login as u8;
    options
}

/// Everything the plan and the operation agree on, computed before
/// anything is touched.
struct Prepared {
    manifest: crate::overlay::OverlayManifestHeader,
    records: Vec<FileRecordHeader>,
    power: PowerSpec,
    /// The package's recorded options, unmodified — the base the
    /// pre-modify state is reconstructed from for rollback.
    package_options: InstallOptions,
    receipt: InstallReceipt,
    options: InstallOptions,
    selection: Vec<String>,
    /// Record indexes to (re)write: newly selected components' files
    /// plus selected files whose bytes differ from the receipt's.
    added_indices: Vec<usize>,
    /// Receipt files to delete (deselected, or gone from the package).
    removed_files: Vec<ReceiptFile>,
    /// The new receipt's file list, in record order.
    final_files: Vec<ReceiptFile>,
}

fn prepare(exe_path: &Path, dest: &Path, request: &ModifyRequest) -> Result<Prepared, String> {
    let (manifest, records, package_options, power) =
        crate::install::read_own_package_full(exe_path)?;
    let receipt = read_receipt(dest)
        .filter(|r| r.app == manifest.app_name())
        .ok_or_else(|| {
            format!(
                "no SeanPak install of {} found at {} (no install receipt there) — nothing to modify",
                manifest.app_name(),
                dest.display()
            )
        })?;
    let mut options = request
        .options_override
        .unwrap_or_else(|| options_from_receipt(&package_options, &receipt));
    if let Some(choice) = request.start_at_login {
        options.start_at_login = 1;
        options.start_at_login_default = choice as u8;
    }
    let primary_rel = records
        .get(manifest.primary_exe_index as usize)
        .map(|r| r.relative_path());
    let selection = power.resolve_selection(
        request.components.as_deref(),
        &request.without_components,
        Some((
            receipt.components.as_slice(),
            receipt.components_known.as_slice(),
        )),
        &records,
        primary_rel.as_deref(),
    )?;
    let hashes = payload_hashes(exe_path, &records)?;
    let selected_paths: HashSet<String> = records
        .iter()
        .filter(|r| {
            selection
                .iter()
                .any(|id| id == &power.component_id_for_file(&r.relative_path()))
        })
        .map(|r| r.relative_path())
        .collect();
    let mut added_indices = Vec::new();
    let mut final_files = Vec::new();
    for (idx, record) in records.iter().enumerate() {
        let rel = record.relative_path();
        if !selected_paths.contains(&rel) {
            continue;
        }
        let kept = receipt
            .files
            .iter()
            .find(|f| f.path == rel && f.sha256 == hashes[idx]);
        match kept {
            Some(entry) => final_files.push(entry.clone()),
            None => {
                added_indices.push(idx);
                final_files.push(ReceiptFile {
                    path: rel,
                    size: record.file_size,
                    sha256: hashes[idx].clone(),
                });
            }
        }
    }
    let removed_files: Vec<ReceiptFile> = receipt
        .files
        .iter()
        .filter(|f| !final_files.iter().any(|n| n.path == f.path))
        .cloned()
        .collect();
    Ok(Prepared {
        manifest,
        records,
        power,
        package_options,
        receipt,
        options,
        selection,
        added_indices,
        removed_files,
        final_files,
    })
}

/// Computes what a modify would do. See [`ModifyPlan`].
pub fn plan_modify(
    exe_path: &Path,
    dest: &Path,
    request: &ModifyRequest,
) -> Result<ModifyPlan, String> {
    let prepared = prepare(exe_path, dest, request)?;
    let before_ids = &prepared.receipt.components;
    let components_added: Vec<String> = prepared
        .selection
        .iter()
        .filter(|id| !before_ids.contains(id))
        .cloned()
        .collect();
    let components_removed: Vec<String> = before_ids
        .iter()
        .filter(|id| !prepared.selection.contains(id))
        .cloned()
        .collect();
    let components_kept: Vec<String> = prepared
        .selection
        .iter()
        .filter(|id| before_ids.contains(id))
        .cloned()
        .collect();
    let (location_writable, suggested_location) = match location_verdict(
        dest,
        &prepared.manifest.app_name(),
        &prepared.options.install_location(),
    ) {
        LocationVerdict::Writable => (true, None),
        LocationVerdict::NotWritable { fallback, .. } => (false, Some(fallback)),
    };
    let old_options = options_from_receipt(&prepared.package_options, &prepared.receipt);
    Ok(ModifyPlan {
        app_name: prepared.manifest.app_name(),
        installed_version: prepared.receipt.version.clone(),
        package_version: prepared.manifest.app_version(),
        dest: dest.to_path_buf(),
        components_added,
        components_removed,
        components_kept,
        files_added: prepared
            .added_indices
            .iter()
            .map(|&idx| {
                (
                    prepared.records[idx].relative_path(),
                    prepared.records[idx].file_size,
                )
            })
            .collect(),
        files_removed: prepared
            .removed_files
            .iter()
            .map(|f| f.path.clone())
            .collect(),
        options_before: summarize(&old_options),
        options_after: summarize(&prepared.options),
        location_writable,
        suggested_location,
    })
}

/// The modify operation described in the module docs. On any failure
/// the destination is rolled back to the pre-modify state before the
/// error is returned.
pub fn perform_modify(
    exe_path: &Path,
    dest: &Path,
    request: &ModifyRequest,
    mut progress: Option<&mut dyn FnMut(&ProgressEvent)>,
    cancel: Option<&AtomicBool>,
) -> Result<ModifyOutcome, String> {
    let prepared = prepare(exe_path, dest, request)?;
    let Prepared {
        manifest,
        records,
        power,
        package_options,
        receipt,
        options,
        selection,
        added_indices,
        removed_files,
        final_files,
    } = prepared;
    let app_name = manifest.app_name();

    // The destination must be writable by this per-user process, as
    // with install — refused before anything changes.
    ensure_location_writable(dest, &app_name, &options.install_location())?;

    // Snapshot phase: the receipt, every file about to be deleted,
    // and every target about to be overwritten.
    let mut tx =
        TransactionRollbackLog::new().map_err(|e| format!("cannot start rollback journal: {e}"))?;
    for file in &removed_files {
        let target = dest.join(file.path.replace('\\', "/"));
        tx.snapshot_file_before_write(&target)
            .map_err(|e| format!("cannot snapshot {}: {e}", target.display()))?;
    }
    for &idx in &added_indices {
        let target = dest.join(records[idx].relative_path().replace('\\', "/"));
        tx.snapshot_file_before_write(&target)
            .map_err(|e| format!("cannot snapshot {}: {e}", target.display()))?;
    }
    let receipt_file = crate::receipt::receipt_path(dest);
    tx.snapshot_file_before_write(&receipt_file)
        .map_err(|e| format!("cannot snapshot receipt: {e}"))?;

    // The rollback path for everything after the old artifacts come
    // off: re-apply the pre-modify artifacts best-effort, then let
    // the journal restore files + receipt.
    let old_options = options_from_receipt(&package_options, &receipt);
    let old_selection = receipt.components.clone();
    let primary_rel = records
        .get(manifest.primary_exe_index as usize)
        .map(|r| r.relative_path());
    let rollback_artifacts = |applied_new: Option<&crate::receipt::AppliedArtifacts>,
                              tx: &mut TransactionRollbackLog| {
        if let Some(applied) = applied_new {
            remove_artifacts(applied);
        }
        let plan = plan_artifacts(
            &old_options,
            &app_name,
            dest,
            primary_rel.as_deref(),
            &power,
            Some(&old_selection),
        );
        if let Ok(restored) = apply_artifacts(&plan, &app_name, dest) {
            // The restored journal is discarded: it describes the
            // same artifacts the (restored) old receipt records.
            let _ = restored;
            let _ = crate::writes::apply_writes(&power);
        }
        tx.rollback();
    };

    // 1. Remove the old system artifacts (shortcuts, associations,
    //    PATH, login entry, journaled writes restore their priors).
    remove_artifacts(&receipt.artifacts);

    // 2. Extract the added files.
    let added_paths: HashSet<String> = added_indices
        .iter()
        .map(|&idx| records[idx].relative_path())
        .collect();
    let added_count = added_indices.len();
    let total_bytes: u64 = added_indices
        .iter()
        .map(|&idx| records[idx].file_size)
        .sum();
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
                    file_count: added_count,
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
        match File::open(exe_path) {
            Ok(file) => OverlayReader::extract_selected_with_control(
                file,
                dest,
                &|record: &FileRecordHeader| added_paths.contains(&record.relative_path()),
                &mut control,
            ),
            Err(e) => Err(e),
        }
    };
    if let Err(e) = extract_result {
        rollback_artifacts(None, &mut tx);
        if e.kind() == std::io::ErrorKind::Interrupted {
            return Err(
                "Modify cancelled; every change was rolled back and the previous install state was kept."
                    .to_string(),
            );
        }
        return Err(format!("extraction failed: {e}"));
    }
    if cancel
        .map(|c| c.load(AtomicOrdering::Relaxed))
        .unwrap_or(false)
    {
        rollback_artifacts(None, &mut tx);
        return Err(
            "Modify cancelled; every change was rolled back and the previous install state was kept."
                .to_string(),
        );
    }

    // 3. Delete the removed files, pruning now-empty payload folders.
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for file in &removed_files {
        let rel = file.path.replace('\\', "/");
        if rel.is_empty() || rel.contains("..") {
            continue;
        }
        let target = dest.join(&rel);
        if target.is_file() {
            let _ = std::fs::remove_file(&target);
        }
        let mut dir = target.parent().map(|p| p.to_path_buf());
        while let Some(d) = dir {
            if d == dest {
                break;
            }
            subdirs.push(d.clone());
            dir = d.parent().map(|p| p.to_path_buf());
        }
    }
    subdirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));
    subdirs.dedup();
    for dir in &subdirs {
        let _ = std::fs::remove_dir(dir); // only succeeds when empty
    }

    // 4. Apply the new artifacts, then the journaled writes.
    let plan = plan_artifacts(
        &options,
        &app_name,
        dest,
        primary_rel.as_deref(),
        &power,
        Some(&selection),
    );
    let mut applied = match apply_artifacts(&plan, &app_name, dest) {
        Ok(applied) => applied,
        Err(e) => {
            rollback_artifacts(None, &mut tx);
            return Err(e);
        }
    };
    match crate::writes::apply_writes(&power) {
        Ok((reg_records, env_records)) => {
            applied.registry_writes = reg_records;
            applied.env_writes = env_records;
        }
        Err(e) => {
            let journal = applied.clone();
            rollback_artifacts(Some(&journal), &mut tx);
            return Err(e);
        }
    }

    // 5. Refresh the ARP entry (an install made before v7 gains its
    //    ModifyPath here).
    if let Err(e) = crate::install::refresh_uninstall_entry(&manifest, dest) {
        let journal = applied.clone();
        rollback_artifacts(Some(&journal), &mut tx);
        return Err(e);
    }

    // 6. The new receipt: same install, new truth.
    let shortcuts_created = applied.shortcuts.len();
    let associations_applied = applied.associations.len();
    let path_entry_added = applied.path_segment_added.is_some();
    let start_at_login_applied = applied.start_at_login.is_some();
    let registry_writes_applied = applied.registry_writes.len();
    let env_writes_applied = applied.env_writes.len();
    let pins = applied.pins.clone();
    let new_receipt = InstallReceipt {
        app: app_name.clone(),
        version: manifest.app_version(),
        publisher: manifest.publisher(),
        install_path: dest.to_string_lossy().into_owned(),
        installed_at_unix: InstallReceipt::now_unix(),
        mode: "modify".to_string(),
        previous_version: if receipt.version != manifest.app_version() {
            Some(receipt.version.clone())
        } else {
            receipt.previous_version.clone()
        },
        files: final_files,
        desktop_shortcut: options.wants_desktop_shortcut(),
        start_menu_shortcut: options.wants_start_menu_shortcut(),
        start_menu_folder: options.start_menu_folder(),
        path_entry: options.wants_path_entry(),
        file_associations: crate::sysinstall::parse_extensions(&options.file_associations()),
        launch_after_install_offered: options.offers_launch_after_install(),
        launch_after_install_default: options.launch_after_install_default_checked(),
        start_at_login: options.start_at_login_chosen(),
        purge_data_default: receipt.purge_data_default,
        data_dirs: crate::uninstall::resolve_data_dirs(&options.data_dirs(), dest)
            .iter()
            .map(|p| p.to_string_lossy().into_owned())
            .collect(),
        components: selection.clone(),
        components_known: power.part_ids(power.has_core_files(&records)),
        hooks: receipt.hooks.clone(),
        artifacts: applied,
    };
    let receipt_path = match new_receipt.write_to(dest) {
        Ok(path) => path,
        Err(e) => {
            let journal = new_receipt.artifacts.clone();
            rollback_artifacts(Some(&journal), &mut tx);
            return Err(format!("cannot write install receipt: {e}"));
        }
    };
    crate::diag::record(
        "modify",
        vec![
            ("app", crate::manifest::jstr(app_name.clone())),
            (
                "dest",
                crate::manifest::jstr(dest.to_string_lossy().into_owned()),
            ),
            (
                "filesAdded",
                crate::manifest::JsonValue::Number(added_indices.len() as f64),
            ),
            (
                "filesRemoved",
                crate::manifest::JsonValue::Number(removed_files.len() as f64),
            ),
            (
                "components",
                crate::manifest::JsonValue::Array(
                    selection
                        .iter()
                        .map(|c| crate::manifest::jstr(c.clone()))
                        .collect(),
                ),
            ),
        ],
    );
    let outcome = ModifyOutcome {
        app_name: app_name.clone(),
        version: manifest.app_version(),
        previous_version: new_receipt.previous_version.clone(),
        dest: dest.to_path_buf(),
        files_added: added_indices.len(),
        files_removed: removed_files.len(),
        components_before: receipt.components.clone(),
        components_after: selection,
        receipt_path,
        shortcuts_created,
        associations_applied,
        path_entry_added,
        start_at_login_applied,
        registry_writes_applied,
        env_writes_applied,
        pins,
    };
    tx.commit()
        .map_err(|e| format!("cannot commit modify journal: {e}"))?;
    Ok(outcome)
}

/// SHA-256 of every payload's *original* bytes, in record order (the
/// same computation the install operation makes, shared shape).
fn payload_hashes(exe_path: &Path, records: &[FileRecordHeader]) -> Result<Vec<String>, String> {
    let mut file =
        File::open(exe_path).map_err(|e| format!("cannot open {}: {e}", exe_path.display()))?;
    let stream_len = std::io::Seek::seek(&mut file, std::io::SeekFrom::End(0))
        .map_err(|e| format!("cannot read package: {e}"))?;
    let (_, parsed_records, _, _, payload_digests) =
        OverlayReader::read_package_with_integrity(&mut file)
            .map_err(|e| format!("cannot read package: {e}"))?
            .ok_or_else(|| "this executable carries no SeanPak overlay".to_string())?;
    if parsed_records.len() != records.len()
        || parsed_records.iter().zip(records).any(|(fresh, planned)| {
            fresh.relative_path() != planned.relative_path()
                || fresh.file_size != planned.file_size
                || fresh.payload_offset != planned.payload_offset
                || fresh.compression() != planned.compression()
        })
    {
        return Err("package record table changed during modify planning".to_string());
    }
    let mut hashes = Vec::with_capacity(records.len());
    for idx in 0..records.len() {
        let bytes = OverlayReader::read_payload(&mut file, records, idx, stream_len)
            .map_err(|e| format!("cannot read payload {}: {e}", records[idx].relative_path()))?;
        if let Some(expected) = payload_digests.as_ref().and_then(|digests| digests.get(idx)) {
            if crate::hash::sha256_digest(&bytes) != *expected {
                return Err(format!(
                    "payload integrity check failed for {}",
                    records[idx].relative_path()
                ));
            }
        }
        hashes.push(sha256_hex(&bytes));
    }
    Ok(hashes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AppManifest;

    fn unique_temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "seanpak_modify_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ))
    }

    /// Packs a two-component fixture: app.exe (core) + extras.dll
    /// (extras, default off), through the real pack pipeline.
    fn pack_fixture(root: &Path) -> PathBuf {
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.exe"), b"fake-pe-bytes").unwrap();
        std::fs::write(src.join("extras.dll"), b"extras-payload").unwrap();
        let manifest = AppManifest::from_json(
            r#"{
                "appName": "Modify App",
                "version": "1.0.0",
                "publisher": "Tester",
                "primaryExe": "app.exe",
                "components": [
                    {"id": "extras", "name": "Extras", "files": ["extras.dll"], "defaultSelected": false}
                ]
            }"#,
        )
        .unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xBBu8; 5000]).unwrap();
        let out = root.join("out");
        let outcome = crate::pack::pack(&manifest, &src, &out, &stub, false).unwrap();
        outcome.setup.path
    }

    #[test]
    fn modify_rejects_corrupted_v7_payload_hashes() {
        let root = unique_temp("integrity");
        std::fs::create_dir_all(&root).unwrap();
        let setup = pack_fixture(&root);
        let (_, records, _, _, digests) =
            OverlayReader::read_package_with_integrity(std::fs::File::open(&setup).unwrap())
                .unwrap()
                .unwrap();
        assert!(digests.is_some());
        let (offset, path) = {
            let record = records
                .iter()
                .find(|r| r.file_size > 0 && r.compression() == crate::codec::CODEC_STORED)
                .unwrap();
            (record.payload_offset as usize, record.relative_path())
        };
        let mut image = std::fs::read(&setup).unwrap();
        image[offset] ^= 1;
        std::fs::write(&setup, image).unwrap();

        let error = payload_hashes(&setup, &records).unwrap_err();
        assert!(error.contains(&format!("payload integrity check failed for {path}")));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn modify_adds_and_removes_components_and_rewrites_the_receipt() {
        let root = unique_temp("cycle");
        std::fs::create_dir_all(&root).unwrap();
        let setup = pack_fixture(&root);
        let dest = root.join("installed");

        // Install with defaults: extras stays out.
        crate::install::perform_install(&setup, &dest).unwrap();
        assert!(dest.join("app.exe").exists());
        assert!(!dest.join("extras.dll").exists());
        let before = read_receipt(&dest).unwrap();
        assert_eq!(before.components, vec!["core".to_string()]);

        // Plan: adding extras shows up as an add, nothing removed.
        let add = ModifyRequest {
            components: Some(vec!["core".to_string(), "extras".to_string()]),
            ..Default::default()
        };
        let plan = plan_modify(&setup, &dest, &add).unwrap();
        assert_eq!(plan.components_added, vec!["extras".to_string()]);
        assert!(plan.components_removed.is_empty());
        assert_eq!(plan.files_added.len(), 1);
        assert_eq!(plan.files_added[0].0, "extras.dll");

        // Apply: the file lands, the receipt follows.
        let outcome = perform_modify(&setup, &dest, &add, None, None).unwrap();
        assert_eq!(outcome.files_added, 1);
        assert_eq!(outcome.files_removed, 0);
        assert!(dest.join("extras.dll").exists());
        let after = read_receipt(&dest).unwrap();
        assert_eq!(after.mode, "modify");
        assert_eq!(
            after.components,
            vec!["core".to_string(), "extras".to_string()]
        );
        assert_eq!(after.files.len(), 2);

        // Modify back down: extras leaves again.
        let remove = ModifyRequest {
            components: Some(vec!["core".to_string()]),
            ..Default::default()
        };
        let outcome = perform_modify(&setup, &dest, &remove, None, None).unwrap();
        assert_eq!(outcome.files_removed, 1);
        assert!(!dest.join("extras.dll").exists());
        let final_receipt = read_receipt(&dest).unwrap();
        assert_eq!(final_receipt.components, vec!["core".to_string()]);
        assert_eq!(final_receipt.files.len(), 1);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn modify_without_an_install_is_a_plain_refusal() {
        let root = unique_temp("missing");
        std::fs::create_dir_all(&root).unwrap();
        let setup = pack_fixture(&root);
        let dest = root.join("never-installed");
        let err = perform_modify(&setup, &dest, &ModifyRequest::default(), None, None).unwrap_err();
        assert!(err.contains("nothing to modify"), "{err}");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn modify_flips_an_option_in_the_receipt() {
        let root = unique_temp("options");
        std::fs::create_dir_all(&root).unwrap();
        let setup = pack_fixture(&root);
        let dest = root.join("installed");
        crate::install::perform_install(&setup, &dest).unwrap();
        let before = read_receipt(&dest).unwrap();
        assert!(before.desktop_shortcut);

        let mut new_options = options_from_receipt(
            &crate::install::read_own_package_full(&setup).unwrap().2,
            &before,
        );
        new_options.desktop_shortcut = 0;
        let request = ModifyRequest {
            options_override: Some(new_options),
            ..Default::default()
        };
        let plan = plan_modify(&setup, &dest, &request).unwrap();
        assert!(plan.options_before.desktop_shortcut);
        assert!(!plan.options_after.desktop_shortcut);
        perform_modify(&setup, &dest, &request, None, None).unwrap();
        let after = read_receipt(&dest).unwrap();
        assert!(!after.desktop_shortcut);
        let _ = std::fs::remove_dir_all(&root);
    }
}
