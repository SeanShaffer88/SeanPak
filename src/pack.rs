//! The headless packaging pipeline — SeanPak's primary workflow.
//!
//! [`pack`] takes an [`AppManifest`] plus a files directory and produces
//! exactly the release shape, deterministically named from the manifest:
//! `<AppName>-v<version>-x64-Setup.exe` (native overlay installer),
//! `<AppName>-v<version>-x64-Portable.zip` (zero-install archive of the
//! same files), and `SHA256SUMS.txt` covering both. Existing outputs that
//! differ are never overwritten without `force`.
//!
//! [`verify_package`] proves a built Setup.exe before publishing: the
//! overlay must be readable, the embedded manifest sane, and every
//! recorded payload present, in bounds, and readable.
//!
//! The ZIP writer is hand-rolled (stored, no compression) for the same
//! dependency reason as everything else in this crate.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use crate::hash::{crc32, sha256_file_hex};
use crate::manifest::AppManifest;
use crate::overlay::{BuildControl, OverlayReader, OverlayWriter};
use crate::scanner::ScannedFolder;

#[derive(Debug)]
pub enum PackError {
    /// A manifest-listed file is missing, or the file set is unusable.
    BadFiles(String),
    /// An existing output differs and `force` was not given.
    RefuseOverwrite(String),
    /// The caller cancelled via the cancel flag passed to
    /// [`pack_with_hooks`]. Any outputs the cancelled run placed that
    /// did not exist before the run have already been removed; files
    /// that existed before the run are never touched.
    Cancelled,
    Io(std::io::Error),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::BadFiles(m) => write!(f, "{m}"),
            PackError::RefuseOverwrite(m) => write!(f, "{m}"),
            PackError::Cancelled => write!(f, "pack cancelled by user"),
            PackError::Io(e) => write!(f, "{e}"),
        }
    }
}

impl From<std::io::Error> for PackError {
    fn from(e: std::io::Error) -> Self {
        PackError::Io(e)
    }
}

#[derive(Debug, Clone)]
pub struct AssetInfo {
    pub name: String,
    pub path: PathBuf,
    pub size: u64,
    pub sha256: String,
}

/// One component of a packed release (v6), with the packed size the
/// packer's own codec produced for its files.
#[derive(Debug, Clone)]
pub struct ComponentInfo {
    pub id: String,
    pub name: String,
    pub required: bool,
    pub packed_size: u64,
    pub file_count: usize,
}

#[derive(Debug)]
pub struct PackOutcome {
    pub setup: AssetInfo,
    pub portable: AssetInfo,
    pub sums_path: PathBuf,
    pub stub_bytes: u64,
    pub file_count: usize,
    pub total_payload_bytes: u64,
    /// The package's components (v6): `core` first when the package
    /// has unclaimed files, then the manifest's components in order.
    pub components: Vec<ComponentInfo>,
}

/// One per-file progress update from [`pack_with_hooks`] and
/// [`verify_package_with`] — the same shape as the install
/// operation's `ProgressEvent`.
#[derive(Debug, Clone)]
pub struct PackProgress {
    pub file_index: usize,
    pub file_count: usize,
    pub rel_path: String,
}

/// One release packaging run. See the module docs for the contract.
pub fn pack(
    manifest: &AppManifest,
    files_dir: &Path,
    out_dir: &Path,
    stub_path: &Path,
    force: bool,
) -> Result<PackOutcome, PackError> {
    pack_with_hooks(manifest, files_dir, out_dir, stub_path, force, None, None)
}

/// Like [`pack`], with opt-in per-file progress and cooperative
/// cancellation — the hooks the Builder studio's worker threads use.
/// Cancellation is checked between files (scan, overlay build,
/// portable zip) and between the placement steps; a cancelled run
/// returns [`PackError::Cancelled`] and removes any of the three
/// release files it placed that did not exist before the run (files
/// that existed before the run are never deleted). Callers that pass
/// no hooks — the CLI always does — get exactly [`pack`]'s behavior.
#[allow(clippy::too_many_arguments)]
pub fn pack_with_hooks(
    manifest: &AppManifest,
    files_dir: &Path,
    out_dir: &Path,
    stub_path: &Path,
    force: bool,
    mut progress: Option<&mut dyn FnMut(&PackProgress)>,
    cancel: Option<&AtomicBool>,
) -> Result<PackOutcome, PackError> {
    let cancelled = || crate::scanner::is_cancelled(cancel);
    // One local sink both build phases report through (passing the
    // raw `Option<&mut dyn FnMut>` parameter to two callees tangles
    // the trait-object lifetimes; a closure reborrow does not).
    let mut report_progress = |event: &PackProgress| {
        if let Some(sink) = progress.as_deref_mut() {
            sink(event);
        }
    };
    let scanned = match ScannedFolder::scan_with(files_dir, cancel) {
        Ok(scanned) => scanned,
        Err(_) if cancelled() => return Err(PackError::Cancelled),
        Err(e) => return Err(PackError::Io(e)),
    };
    if scanned.files.is_empty() {
        return Err(PackError::BadFiles(format!(
            "no files found in {}",
            files_dir.display()
        )));
    }

    // Resolve the file list: the manifest's explicit list when given,
    // otherwise the whole scanned folder.
    let mut chosen: Vec<(PathBuf, String)> = Vec::new();
    if manifest.files.is_empty() {
        for f in &scanned.files {
            chosen.push((f.absolute_path.clone(), f.relative_path.clone()));
        }
    } else {
        for wanted in &manifest.files {
            match scanned
                .files
                .iter()
                .find(|f| f.relative_path.replace('\\', "/") == *wanted)
            {
                Some(f) => chosen.push((f.absolute_path.clone(), f.relative_path.clone())),
                None => {
                    return Err(PackError::BadFiles(format!(
                        "manifest lists \"{wanted}\" but it is not in {}",
                        files_dir.display()
                    )))
                }
            }
        }
    }

    let primary_rel = match &manifest.primary_exe {
        Some(p) => {
            let normalized = p.replace('\\', "/");
            if !chosen
                .iter()
                .any(|(_, rel)| rel.replace('\\', "/") == normalized)
            {
                return Err(PackError::BadFiles(format!(
                    "primaryExe \"{p}\" is not among the packaged files"
                )));
            }
            Some(normalized)
        }
        None => scanned
            .suggested_primary_exe
            .as_ref()
            .map(|p| p.replace('\\', "/")),
    };

    let files_arg: Vec<(PathBuf, String, bool)> = chosen
        .iter()
        .map(|(abs, rel)| {
            let is_primary = primary_rel.as_deref() == Some(rel.replace('\\', "/").as_str());
            (abs.clone(), rel.clone(), is_primary)
        })
        .collect();

    // v6 power block: the manifest's components / hooks / writes /
    // shortcuts, embedded in the package. Component files and
    // shortcut targets/icons must be among the packaged files — a
    // component or shortcut pointing at a file that is not packed
    // would install broken, so the pack refuses here.
    let power = manifest.to_power_spec();
    {
        let chosen_rels: Vec<String> = chosen
            .iter()
            .map(|(_, rel)| rel.replace('\\', "/"))
            .collect();
        for component in &power.components {
            for file in &component.files {
                if !chosen_rels.iter().any(|rel| rel == file) {
                    return Err(PackError::BadFiles(format!(
                        "component \"{}\" lists \"{file}\" but it is not among the packaged files",
                        component.id
                    )));
                }
            }
        }
        for shortcut in &power.shortcuts {
            if !chosen_rels.iter().any(|rel| rel == &shortcut.target) {
                return Err(PackError::BadFiles(format!(
                    "shortcut \"{}\" targets \"{}\" but it is not among the packaged files",
                    shortcut.name, shortcut.target
                )));
            }
            if let Some(icon) = &shortcut.icon {
                if !chosen_rels.iter().any(|rel| rel == icon) {
                    return Err(PackError::BadFiles(format!(
                        "shortcut \"{}\" uses icon \"{icon}\" but it is not among the packaged files",
                        shortcut.name
                    )));
                }
            }
        }
    }
    let total_payload_bytes: u64 = files_arg
        .iter()
        .map(|(abs, _, _)| std::fs::metadata(abs).map(|m| m.len()).unwrap_or(0))
        .sum();

    // Build into a private temp folder first: until every asset exists
    // and the overwrite policy has passed, the output directory is
    // untouched.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let temp_dir =
        std::env::temp_dir().join(format!("seanpak_pack_{}_{}", std::process::id(), nanos));
    std::fs::create_dir_all(&temp_dir)?;
    let cleanup = |dir: &Path| {
        let _ = std::fs::remove_dir_all(dir);
    };

    let setup_name = manifest.setup_file_name();
    let portable_name = manifest.portable_file_name();
    let temp_setup = temp_dir.join(&setup_name);
    let temp_portable = temp_dir.join(&portable_name);

    // Which of the three release files already exist in the output
    // folder. A cancelled run removes only outputs it placed itself;
    // anything that was there before the run is never deleted.
    let setup_dest = out_dir.join(&setup_name);
    let portable_dest = out_dir.join(&portable_name);
    let sums_dest = out_dir.join("SHA256SUMS.txt");
    let existed_before = [
        setup_dest.is_file(),
        portable_dest.is_file(),
        sums_dest.is_file(),
    ];

    let options = manifest.to_install_options();
    let mut packed_power: Option<crate::overlay::PowerSpec> = None;
    let build_result = {
        let mut build_progress = |idx: usize, total: usize, rel: &str| {
            report_progress(&PackProgress {
                file_index: idx,
                file_count: total,
                rel_path: rel.to_string(),
            });
        };
        let mut control = BuildControl {
            on_file: Some(&mut build_progress),
            cancel,
        };
        OverlayWriter::build_overlay_with_power_ctl(
            stub_path,
            &temp_setup,
            files_dir,
            &files_arg,
            &manifest.app_name,
            &manifest.version,
            &manifest.publisher,
            &options,
            &power,
            &mut control,
        )
        .map(|(_, computed)| computed)
    };
    let build_result = match build_result {
        Ok(computed) => {
            packed_power = Some(computed);
            write_portable_zip_ctl(
                &files_arg,
                &temp_portable,
                cancel,
                Some(&mut report_progress),
            )
        }
        Err(e) => Err(e),
    };
    if let Err(e) = build_result {
        cleanup(&temp_dir);
        if cancelled() {
            return Err(PackError::Cancelled);
        }
        return Err(PackError::Io(e));
    }
    let packed_power = packed_power.unwrap_or(power);
    let components: Vec<ComponentInfo> = {
        // `core` first (when it owns files), sized from the records
        // the writer just produced — recompute from the temp Setup.
        let mut infos: Vec<ComponentInfo> = Vec::new();
        let stream_len = std::fs::metadata(&temp_setup).map(|m| m.len()).unwrap_or(0);
        if let Ok(Some((_, records, _, _))) =
            std::fs::File::open(&temp_setup).and_then(OverlayReader::read_package_full)
        {
            if packed_power.has_core_files(&records) {
                infos.push(ComponentInfo {
                    id: crate::overlay::CORE_COMPONENT_ID.to_string(),
                    name: "Core files".to_string(),
                    required: true,
                    packed_size: packed_power.core_packed_size(&records, stream_len),
                    file_count: records
                        .iter()
                        .filter(|r| {
                            packed_power.component_id_for_file(&r.relative_path())
                                == crate::overlay::CORE_COMPONENT_ID
                        })
                        .count(),
                });
            }
        }
        for component in &packed_power.components {
            infos.push(ComponentInfo {
                id: component.id.clone(),
                name: component.name.clone(),
                required: component.required,
                packed_size: component.packed_size,
                file_count: component.files.len(),
            });
        }
        infos
    };

    let stub_bytes = std::fs::metadata(stub_path)?.len();
    std::fs::create_dir_all(out_dir)?;

    // Overwrite policy per asset: identical existing file is kept as-is;
    // a differing one refuses the whole run unless forced.
    let place = |temp: &Path, name: &str| -> Result<AssetInfo, PackError> {
        let dest = out_dir.join(name);
        let new_hash = sha256_file_hex(temp)?;
        let new_size = std::fs::metadata(temp)?.len();
        if dest.is_file() {
            let existing_hash = sha256_file_hex(&dest)?;
            if existing_hash == new_hash {
                return Ok(AssetInfo {
                    name: name.to_string(),
                    path: dest,
                    size: new_size,
                    sha256: new_hash,
                });
            }
            if !force {
                return Err(PackError::RefuseOverwrite(format!(
                    "{} already exists with different contents; re-run with --force to overwrite it",
                    dest.display()
                )));
            }
        }
        std::fs::copy(temp, &dest)?;
        Ok(AssetInfo {
            name: name.to_string(),
            path: dest,
            size: new_size,
            sha256: new_hash,
        })
    };

    // Outputs this run has placed that did not exist before it: a
    // cancellation from here on removes exactly these, so a cancelled
    // build never leaves a partial release set behind.
    let mut placed_fresh: Vec<PathBuf> = Vec::new();
    let remove_fresh = |placed: &[PathBuf]| {
        for path in placed {
            let _ = std::fs::remove_file(path);
        }
    };

    if cancelled() {
        cleanup(&temp_dir);
        return Err(PackError::Cancelled);
    }
    let setup_info = match place(&temp_setup, &setup_name) {
        Ok(info) => info,
        Err(e) => {
            cleanup(&temp_dir);
            return Err(e);
        }
    };
    if !existed_before[0] {
        placed_fresh.push(setup_dest.clone());
    }
    if cancelled() {
        remove_fresh(&placed_fresh);
        cleanup(&temp_dir);
        return Err(PackError::Cancelled);
    }
    let portable_info = match place(&temp_portable, &portable_name) {
        Ok(info) => info,
        Err(e) => {
            cleanup(&temp_dir);
            return Err(e);
        }
    };
    if !existed_before[1] {
        placed_fresh.push(portable_dest.clone());
    }
    if cancelled() {
        remove_fresh(&placed_fresh);
        cleanup(&temp_dir);
        return Err(PackError::Cancelled);
    }

    let sums_path = out_dir.join("SHA256SUMS.txt");
    let sums = format!(
        "{}  {}\n{}  {}\n",
        setup_info.sha256, setup_info.name, portable_info.sha256, portable_info.name
    );
    std::fs::write(&sums_path, sums)?;
    cleanup(&temp_dir);

    Ok(PackOutcome {
        setup: setup_info,
        portable: portable_info,
        sums_path,
        stub_bytes,
        file_count: files_arg.len(),
        total_payload_bytes,
        components,
    })
}

/// Writes a stored (uncompressed) ZIP of `files` — the zero-install
/// portable archive. Entries are sorted by path and stamped with a fixed
/// 1980-01-01 date so identical inputs give identical bytes.
pub fn write_portable_zip(
    files: &[(PathBuf, String, bool)],
    out_path: &Path,
) -> std::io::Result<()> {
    write_portable_zip_ctl(files, out_path, None, None)
}

/// Like [`write_portable_zip`], with opt-in per-file progress +
/// cancellation (checked between entries; see [`pack_with_hooks`]).
pub fn write_portable_zip_ctl(
    files: &[(PathBuf, String, bool)],
    out_path: &Path,
    cancel: Option<&AtomicBool>,
    mut progress: Option<&mut dyn FnMut(&PackProgress)>,
) -> std::io::Result<()> {
    let mut entries: Vec<&(PathBuf, String, bool)> = files.iter().collect();
    entries.sort_by_key(|entry| entry.1.replace('\\', "/"));

    let mut out = std::fs::File::create(out_path)?;
    let mut central: Vec<(String, u32, u64, u64)> = Vec::new(); // name, crc, size, local offset
    let mut offset: u64 = 0;

    const DOS_TIME: u16 = 0;
    const DOS_DATE: u16 = 0x21; // 1980-01-01

    for (idx, (abs, rel, _)) in entries.iter().enumerate() {
        if crate::scanner::is_cancelled(cancel) {
            return Err(crate::scanner::cancelled_error("pack"));
        }
        let name = rel.replace('\\', "/");
        if let Some(sink) = progress.as_deref_mut() {
            sink(&PackProgress {
                file_index: idx,
                file_count: entries.len(),
                rel_path: name.clone(),
            });
        }
        let data = std::fs::read(abs)?;
        let crc = crc32(&data);
        let size = data.len() as u64;
        let name_bytes = name.as_bytes();

        let mut header = Vec::with_capacity(30 + name_bytes.len());
        header.extend_from_slice(&0x0403_4b50u32.to_le_bytes()); // local file header
        header.extend_from_slice(&20u16.to_le_bytes()); // version needed
        header.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 name flag
        header.extend_from_slice(&0u16.to_le_bytes()); // method: stored
        header.extend_from_slice(&DOS_TIME.to_le_bytes());
        header.extend_from_slice(&DOS_DATE.to_le_bytes());
        header.extend_from_slice(&crc.to_le_bytes());
        header.extend_from_slice(&(size as u32).to_le_bytes());
        header.extend_from_slice(&(size as u32).to_le_bytes());
        header.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        header.extend_from_slice(&0u16.to_le_bytes()); // extra length
        header.extend_from_slice(name_bytes);
        out.write_all(&header)?;
        out.write_all(&data)?;
        central.push((name, crc, size, offset));
        offset += header.len() as u64 + size;
    }

    let cd_start = offset;
    for (name, crc, size, local_offset) in &central {
        let name_bytes = name.as_bytes();
        let mut record = Vec::with_capacity(46 + name_bytes.len());
        record.extend_from_slice(&0x0201_4b50u32.to_le_bytes()); // central directory
        record.extend_from_slice(&20u16.to_le_bytes()); // version made by
        record.extend_from_slice(&20u16.to_le_bytes()); // version needed
        record.extend_from_slice(&0x0800u16.to_le_bytes());
        record.extend_from_slice(&0u16.to_le_bytes());
        record.extend_from_slice(&DOS_TIME.to_le_bytes());
        record.extend_from_slice(&DOS_DATE.to_le_bytes());
        record.extend_from_slice(&crc.to_le_bytes());
        record.extend_from_slice(&(*size as u32).to_le_bytes());
        record.extend_from_slice(&(*size as u32).to_le_bytes());
        record.extend_from_slice(&(name_bytes.len() as u16).to_le_bytes());
        record.extend_from_slice(&0u16.to_le_bytes()); // extra
        record.extend_from_slice(&0u16.to_le_bytes()); // comment
        record.extend_from_slice(&0u16.to_le_bytes()); // disk number
        record.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        record.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        record.extend_from_slice(&(*local_offset as u32).to_le_bytes());
        record.extend_from_slice(name_bytes);
        out.write_all(&record)?;
        offset += record.len() as u64;
    }
    let cd_size = offset - cd_start;

    let mut eocd = Vec::with_capacity(22);
    eocd.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes());
    eocd.extend_from_slice(&(central.len() as u16).to_le_bytes());
    eocd.extend_from_slice(&(central.len() as u16).to_le_bytes());
    eocd.extend_from_slice(&(cd_size as u32).to_le_bytes());
    eocd.extend_from_slice(&(cd_start as u32).to_le_bytes());
    eocd.extend_from_slice(&0u16.to_le_bytes());
    out.write_all(&eocd)?;
    out.flush()
}

// ---------------------------------------------------------------------------
// Verify.

#[derive(Debug)]
pub struct VerifyReport {
    pub ok: bool,
    pub app_name: String,
    pub app_version: String,
    pub publisher: String,
    pub manifest_version: u32,
    pub file_count: usize,
    pub total_bytes: u64,
    pub files: Vec<(String, u64)>,
    /// (check name, passed, detail) in the order the checks ran.
    pub checks: Vec<(String, bool, String)>,
}

/// Verifies a built Setup.exe: overlay readable, embedded manifest sane,
/// and the recorded file list matching the payloads actually present.
pub fn verify_package(path: &Path) -> std::io::Result<VerifyReport> {
    verify_package_with(path, None, None)
}

/// Like [`verify_package`], with opt-in per-file progress +
/// cancellation. Cancellation is checked between payload decodes (the
/// expensive phase) and returns the shared `Interrupted`
/// "verify cancelled" error; callers that pass no hooks — the CLI
/// always does — get exactly [`verify_package`]'s behavior.
pub fn verify_package_with(
    path: &Path,
    mut progress: Option<&mut dyn FnMut(&PackProgress)>,
    cancel: Option<&AtomicBool>,
) -> std::io::Result<VerifyReport> {
    let mut report = VerifyReport {
        ok: false,
        app_name: String::new(),
        app_version: String::new(),
        publisher: String::new(),
        manifest_version: 0,
        file_count: 0,
        total_bytes: 0,
        files: Vec::new(),
        checks: Vec::new(),
    };
    let file_len = std::fs::metadata(path)?.len();
    let file = std::fs::File::open(path)?;
    let (manifest, records, _options) = match OverlayReader::read_package(file)? {
        Some(pkg) => {
            report.checks.push((
                "overlay readable".to_string(),
                true,
                "SeanPak overlay found".to_string(),
            ));
            pkg
        }
        None => {
            report.checks.push((
                "overlay readable".to_string(),
                false,
                "no SeanPak overlay found in this file".to_string(),
            ));
            return Ok(report);
        }
    };

    report.app_name = manifest.app_name();
    report.app_version = manifest.app_version();
    report.publisher = manifest.publisher();
    report.manifest_version = manifest.version;
    report.file_count = records.len();
    report.total_bytes = manifest.total_uncompressed_bytes;
    report.files = records
        .iter()
        .map(|r| (r.relative_path(), r.file_size))
        .collect();

    // Manifest sanity.
    let mut sane = true;
    let mut sane_notes: Vec<String> = Vec::new();
    if report.app_name.trim().is_empty() {
        sane = false;
        sane_notes.push("app name is empty".to_string());
    }
    if report.app_version.trim().is_empty() {
        sane = false;
        sane_notes.push("app version is empty".to_string());
    }
    if manifest.file_count as usize != records.len() {
        sane = false;
        sane_notes.push(format!(
            "header file_count {} != {} records read",
            manifest.file_count,
            records.len()
        ));
    }
    let summed: u64 = records.iter().map(|r| r.file_size).sum();
    if summed != manifest.total_uncompressed_bytes {
        sane = false;
        sane_notes.push(format!(
            "header total {} bytes != {} bytes summed from records",
            manifest.total_uncompressed_bytes, summed
        ));
    }
    if !records.is_empty() && (manifest.primary_exe_index as usize) >= records.len() {
        sane = false;
        sane_notes.push("primary exe index is out of range".to_string());
    }
    if !(2..=6).contains(&manifest.version) {
        sane = false;
        sane_notes.push(format!("unsupported manifest version {}", manifest.version));
    }
    report.checks.push((
        "manifest sane".to_string(),
        sane,
        if sane {
            "header agrees with the record table".to_string()
        } else {
            sane_notes.join("; ")
        },
    ));

    // File list matches payloads: paths safe, stored ranges in bounds
    // and non-overlapping, and every payload fully readable AND
    // decompressing to exactly the recorded original bytes (v4 payloads
    // may be stored compressed; `stored_len` gives the on-disk length).
    let mut list_ok = true;
    let mut list_notes: Vec<String> = Vec::new();
    let mut ranges: Vec<(u64, u64, String)> = Vec::new();
    for (idx, record) in records.iter().enumerate() {
        let rel = record.relative_path();
        if !is_safe_relative_path(&rel) {
            list_ok = false;
            list_notes.push(format!("unsafe stored path \"{rel}\""));
            continue;
        }
        let stored = OverlayReader::stored_len(&records, idx, file_len);
        let end = record.payload_offset.saturating_add(stored);
        if end > file_len {
            list_ok = false;
            list_notes.push(format!(
                "\"{rel}\" payload ends at {end}, past the file size {file_len}"
            ));
            continue;
        }
        ranges.push((record.payload_offset, end, rel));
    }
    ranges.sort();
    for pair in ranges.windows(2) {
        if pair[1].0 < pair[0].1 {
            list_ok = false;
            list_notes.push(format!(
                "payloads of \"{}\" and \"{}\" overlap",
                pair[0].2, pair[1].2
            ));
        }
    }
    if list_ok {
        // Decode every payload end to end to prove the bytes are there
        // and the compression (if any) is sound.
        let mut file = std::fs::File::open(path)?;
        for idx in 0..records.len() {
            if crate::scanner::is_cancelled(cancel) {
                return Err(crate::scanner::cancelled_error("verify"));
            }
            if let Some(sink) = progress.as_deref_mut() {
                sink(&PackProgress {
                    file_index: idx,
                    file_count: records.len(),
                    rel_path: records[idx].relative_path(),
                });
            }
            match OverlayReader::read_payload(&mut file, &records, idx, file_len) {
                Ok(bytes) if bytes.len() as u64 == records[idx].file_size => {}
                Ok(_) => {
                    list_ok = false;
                    list_notes.push(format!(
                        "payload of \"{}\" decodes to the wrong length",
                        records[idx].relative_path()
                    ));
                    break;
                }
                Err(e) => {
                    list_ok = false;
                    list_notes.push(format!(
                        "payload of \"{}\" cannot be decoded: {e}",
                        records[idx].relative_path()
                    ));
                    break;
                }
            }
        }
    }
    report.checks.push((
        "file list matches payloads".to_string(),
        list_ok,
        if list_ok {
            format!("{} payloads present and readable", records.len())
        } else {
            list_notes.join("; ")
        },
    ));

    report.ok = report.checks.iter().all(|(_, passed, _)| *passed);
    Ok(report)
}

// ---------------------------------------------------------------------------
// Sandbox verify.

#[derive(Debug)]
pub struct SandboxReport {
    pub ok: bool,
    /// The caller cancelled via the flag passed to
    /// [`sandbox_verify_with`]; the sandbox folder was removed and the
    /// remaining phases never ran.
    pub cancelled: bool,
    pub install_dir: PathBuf,
    /// (check name, passed, detail) in the order the checks ran.
    pub checks: Vec<(String, bool, String)>,
    /// Files left in the install dir after uninstall (expected: just
    /// the running uninstaller copy and its log).
    pub leftovers: Vec<String>,
}

/// The full end-to-end proof of a built installer: silently install it
/// into a fresh temp dir, compare every installed file byte-for-byte
/// against the package payload, silently uninstall, and assert nothing
/// the install owned is left behind. The installer under test is run as
/// a subprocess, so this works on any platform that can execute it
/// (on Windows CI it exercises the real Windows build; a foreign PE on
/// the wrong OS fails the first check honestly instead of pretending).
pub fn sandbox_verify(setup_path: &Path) -> std::io::Result<SandboxReport> {
    sandbox_verify_with(setup_path, None)
}

/// Like [`sandbox_verify`], with opt-in cooperative cancellation. The
/// flag is checked between phases and between compared files — a
/// subprocess already running (the install or uninstall under test)
/// runs to completion, since killing it would leave the sandbox in an
/// unknown state. A cancelled run reports `cancelled: true`, removes
/// the sandbox folder, and never runs the remaining phases.
pub fn sandbox_verify_with(
    setup_path: &Path,
    cancel: Option<&AtomicBool>,
) -> std::io::Result<SandboxReport> {
    use crate::hash::sha256_file_hex;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let root =
        std::env::temp_dir().join(format!("seanpak_sandbox_{}_{}", std::process::id(), nanos));
    std::fs::create_dir_all(&root)?;
    let install_dir = root.join("app");
    let mut report = SandboxReport {
        ok: false,
        cancelled: false,
        install_dir: install_dir.clone(),
        checks: Vec::new(),
        leftovers: Vec::new(),
    };
    let finish = |report: SandboxReport| -> std::io::Result<SandboxReport> {
        let _ = std::fs::remove_dir_all(&root);
        Ok(report)
    };
    // A cancelled run ends at the next check point: one honest check
    // line, the sandbox folder removed by `finish`, no further phases.
    let note_cancelled = |report: &mut SandboxReport, when: &str| -> bool {
        if !crate::scanner::is_cancelled(cancel) {
            return false;
        }
        report.cancelled = true;
        report.checks.push((
            "cancelled".to_string(),
            false,
            format!("cancelled by user {when}"),
        ));
        true
    };

    let staged = root.join("SetupUnderTest.exe");
    std::fs::copy(setup_path, &staged)?;
    let dir_arg = install_dir.to_string_lossy().into_owned();
    let run = |exe: &Path, args: &[&str]| -> std::io::Result<(bool, String)> {
        let out = std::process::Command::new(exe).args(args).output()?;
        let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&out.stderr));
        Ok((out.status.success(), text))
    };

    // 1. Silent install of the staged installer. A package with
    // components (v6) installs ALL of them explicitly: the check
    // below compares every payload byte against the install, which a
    // default selection (some components default off) would fail.
    let mut install_args: Vec<String> = vec![
        "--install".to_string(),
        "--to".to_string(),
        dir_arg.clone(),
        "--silent".to_string(),
        "--json".to_string(),
    ];
    if let Ok(Some((_, records, _, power))) =
        std::fs::File::open(setup_path).and_then(OverlayReader::read_package_full)
    {
        if !power.components.is_empty() {
            let all = power.part_ids(power.has_core_files(&records)).join(",");
            install_args.push("--components".to_string());
            install_args.push(all);
        }
    }
    let install_arg_refs: Vec<&str> = install_args.iter().map(|s| s.as_str()).collect();
    if note_cancelled(&mut report, "before the test install ran") {
        return finish(report);
    }
    let (installed_ok, install_output) = match run(&staged, &install_arg_refs) {
        Ok(result) => result,
        Err(e) => {
            report.checks.push((
                "sandbox install runs".to_string(),
                false,
                format!("cannot execute the installer on this platform: {e}"),
            ));
            return finish(report);
        }
    };
    report.checks.push((
        "sandbox install runs".to_string(),
        installed_ok,
        if installed_ok {
            "installer exited 0".to_string()
        } else {
            format!("installer failed: {}", install_output.trim())
        },
    ));
    if !installed_ok {
        return finish(report);
    }

    // 2. Receipt written, and installed files match the payload bytes.
    let receipt = crate::receipt::read_receipt(&install_dir);
    report.checks.push((
        "install receipt written".to_string(),
        receipt.is_some(),
        match &receipt {
            Some(r) => format!("{} {} ({})", r.app, r.version, r.mode),
            None => "seanpak-receipt.json missing or unreadable".to_string(),
        },
    ));
    let payload_dir = root.join("payload");
    let mut files_ok = true;
    let mut files_detail = String::new();
    match OverlayReader::extract_file(setup_path, &payload_dir) {
        Ok(written) => {
            for payload_file in &written {
                if crate::scanner::is_cancelled(cancel) {
                    files_ok = false;
                    files_detail = "cancelled by user during the comparison".to_string();
                    report.cancelled = true;
                    break;
                }
                let rel = payload_file
                    .strip_prefix(&payload_dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                let installed_file = install_dir.join(&rel);
                match (
                    sha256_file_hex(payload_file),
                    sha256_file_hex(&installed_file),
                ) {
                    (Ok(a), Ok(b)) if a == b => {}
                    _ => {
                        files_ok = false;
                        files_detail = format!("\"{rel}\" differs from its payload");
                        break;
                    }
                }
            }
            if files_ok {
                files_detail = format!("{} files byte-identical", written.len());
            }
        }
        Err(e) => {
            files_ok = false;
            files_detail = format!("cannot extract payload for comparison: {e}");
        }
    }
    report.checks.push((
        "installed files match payload".to_string(),
        files_ok,
        files_detail,
    ));
    if report.cancelled {
        report.checks.push((
            "cancelled".to_string(),
            false,
            "cancelled by user during the comparison; the sandbox folder was removed".to_string(),
        ));
        return finish(report);
    }
    if note_cancelled(&mut report, "before the test uninstall ran") {
        return finish(report);
    }

    // 3. Silent uninstall via the installed Uninstall.exe.
    let uninstaller = install_dir.join("Uninstall.exe");
    let (uninstalled_ok, uninstall_output) =
        match run(&uninstaller, &["--uninstall", "--to", &dir_arg, "--json"]) {
            Ok(result) => result,
            Err(e) => (false, format!("cannot run Uninstall.exe: {e}")),
        };
    report.checks.push((
        "sandbox uninstall runs".to_string(),
        uninstalled_ok,
        if uninstalled_ok {
            "uninstaller exited 0".to_string()
        } else {
            format!("uninstaller failed: {}", uninstall_output.trim())
        },
    ));

    // 4. Residue: everything the install OWNED (payload files + the
    // receipt) must be gone. The running uninstaller copy and its log
    // are known leftovers (a process cannot delete its own running
    // image). Files an install hook created are NOT residue — hooks
    // exist to make state the installer does not track, and their
    // changes are never uninstalled — so they are reported in
    // `leftovers` without failing the check.
    let mut remaining: Vec<String> = Vec::new();
    collect_relative_files(&install_dir, &install_dir, &mut remaining);
    remaining.sort();
    report.leftovers = remaining.clone();
    let mut owned: Vec<String> = Vec::new();
    if let Ok(Some((_, records, _, _))) =
        std::fs::File::open(setup_path).and_then(OverlayReader::read_package_full)
    {
        owned.extend(records.iter().map(|r| r.relative_path().replace('\\', "/")));
    }
    owned.push(crate::receipt::RECEIPT_FILE_NAME.to_string());
    let residue: Vec<&String> = remaining
        .iter()
        .filter(|f| owned.contains(&f.replace('\\', "/")))
        .collect();
    report.checks.push((
        "no install residue".to_string(),
        residue.is_empty(),
        if residue.is_empty() {
            format!(
                "install-owned files all removed; leftovers: {}",
                remaining.join(", ")
            )
        } else {
            format!(
                "install-owned files left behind: {}",
                residue
                    .iter()
                    .map(|s| s.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        },
    ));

    report.ok = report.checks.iter().all(|(_, passed, _)| *passed);
    finish(report)
}

fn collect_relative_files(base: &Path, dir: &Path, out: &mut Vec<String>) {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_relative_files(base, &path, out);
            } else if let Ok(rel) = path.strip_prefix(base) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

fn is_safe_relative_path(rel: &str) -> bool {
    let normalized = rel.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') {
        return false;
    }
    normalized
        .split('/')
        .all(|part| !part.is_empty() && part != "." && part != ".." && !part.contains(':'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "seanpak_pack_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn sample_app(root: &Path) -> (PathBuf, PathBuf) {
        let files = root.join("files");
        std::fs::create_dir_all(files.join("docs")).unwrap();
        std::fs::write(files.join("app.exe"), b"fake-pe").unwrap();
        std::fs::write(files.join("docs/readme.txt"), b"hello portable").unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xAAu8; 5000]).unwrap();
        (files, stub)
    }

    fn sample_manifest() -> AppManifest {
        AppManifest::from_json(
            r#"{"appName":"Test App","version":"1.2.0","publisher":"Tester","primaryExe":"app.exe"}"#,
        )
        .unwrap()
    }

    #[test]
    fn zip_round_trip_stored_entries() {
        let root = unique_temp("zip");
        let a = root.join("a.txt");
        let b = root.join("b.bin");
        std::fs::write(&a, b"alpha").unwrap();
        std::fs::write(&b, vec![9u8; 1000]).unwrap();
        let zip_path = root.join("out.zip");
        write_portable_zip(
            &[
                (a.clone(), "a.txt".to_string(), false),
                (b.clone(), "sub/b.bin".to_string(), false),
            ],
            &zip_path,
        )
        .unwrap();
        let bytes = std::fs::read(&zip_path).unwrap();
        assert_eq!(&bytes[0..4], b"PK\x03\x04");
        // Find the EOCD and read the entry count.
        let eocd = bytes.len() - 22;
        assert_eq!(&bytes[eocd..eocd + 4], b"PK\x05\x06");
        let count = u16::from_le_bytes([bytes[eocd + 10], bytes[eocd + 11]]);
        assert_eq!(count, 2);
        // The stored payloads appear verbatim.
        assert!(bytes.windows(5).any(|w| w == b"alpha"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_produces_release_shape_and_verifies() {
        let root = unique_temp("pack");
        let (files, stub) = sample_app(&root);
        let out_dir = root.join("dist");
        let manifest = sample_manifest();

        let outcome = pack(&manifest, &files, &out_dir, &stub, false).unwrap();
        assert_eq!(outcome.setup.name, "TestApp-v1.2.0-x64-Setup.exe");
        assert_eq!(outcome.portable.name, "TestApp-v1.2.0-x64-Portable.zip");
        assert_eq!(outcome.stub_bytes, 5000);
        assert!(outcome.setup.path.is_file());
        assert!(outcome.portable.path.is_file());

        let sums = std::fs::read_to_string(&outcome.sums_path).unwrap();
        let setup_hash = sha256_file_hex(&outcome.setup.path).unwrap();
        let portable_hash = sha256_file_hex(&outcome.portable.path).unwrap();
        assert!(sums.contains(&format!("{setup_hash}  TestApp-v1.2.0-x64-Setup.exe")));
        assert!(sums.contains(&format!("{portable_hash}  TestApp-v1.2.0-x64-Portable.zip")));

        let report = verify_package(&outcome.setup.path).unwrap();
        assert!(report.ok, "{:?}", report.checks);
        assert_eq!(report.app_name, "Test App");
        assert_eq!(report.file_count, 2);

        // Identical re-pack is accepted without --force...
        let again = pack(&manifest, &files, &out_dir, &stub, false).unwrap();
        assert_eq!(again.setup.sha256, outcome.setup.sha256);

        // ...but a changed payload refuses without --force and passes with it.
        std::fs::write(files.join("app.exe"), b"different-pe").unwrap();
        let refused = pack(&manifest, &files, &out_dir, &stub, false);
        assert!(matches!(refused, Err(PackError::RefuseOverwrite(_))));
        let forced = pack(&manifest, &files, &out_dir, &stub, true).unwrap();
        assert_ne!(forced.setup.sha256, outcome.setup.sha256);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_cancelled_before_start_leaves_no_outputs() {
        let root = unique_temp("cancel0");
        let (files, stub) = sample_app(&root);
        let out_dir = root.join("dist");
        std::fs::create_dir_all(&out_dir).unwrap();
        std::fs::write(out_dir.join("keep.txt"), b"not ours").unwrap();

        let cancel = AtomicBool::new(true);
        let result = pack_with_hooks(
            &sample_manifest(),
            &files,
            &out_dir,
            &stub,
            false,
            None,
            Some(&cancel),
        );
        assert!(matches!(result, Err(PackError::Cancelled)), "{result:?}");
        // No partial release set, and the unrelated file is untouched.
        assert!(!out_dir.join("TestApp-v1.2.0-x64-Setup.exe").exists());
        assert!(!out_dir.join("TestApp-v1.2.0-x64-Portable.zip").exists());
        assert!(!out_dir.join("SHA256SUMS.txt").exists());
        assert_eq!(
            std::fs::read(out_dir.join("keep.txt")).unwrap(),
            b"not ours"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_cancelled_mid_build_keeps_preexisting_outputs() {
        let root = unique_temp("cancel1");
        let (files, stub) = sample_app(&root);
        let out_dir = root.join("dist");
        let manifest = sample_manifest();

        // A completed pack first: its outputs are pre-existing files
        // for the cancelled run and must survive it untouched.
        let first = pack(&manifest, &files, &out_dir, &stub, false).unwrap();
        std::fs::write(files.join("app.exe"), b"changed-pe").unwrap();

        // The progress hook trips the cancel flag on the first file —
        // the same flag the studio's Cancel button sets.
        let cancel = AtomicBool::new(false);
        let mut progress = |p: &PackProgress| {
            if p.file_index == 0 {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        };
        let result = pack_with_hooks(
            &manifest,
            &files,
            &out_dir,
            &stub,
            true,
            Some(&mut progress),
            Some(&cancel),
        );
        assert!(matches!(result, Err(PackError::Cancelled)), "{result:?}");
        assert_eq!(
            sha256_file_hex(&first.setup.path).unwrap(),
            first.setup.sha256,
            "the pre-existing Setup.exe must be byte-identical after a cancelled re-pack"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn pack_cancelled_at_placement_leaves_no_partial_set() {
        let root = unique_temp("cancel2");
        let (files, stub) = sample_app(&root);
        let out_dir = root.join("dist");

        // Trip the flag on the very last progress event (the portable
        // zip's final file): the build completes, and the cancellation
        // lands in the placement phase, before any output is placed.
        let cancel = AtomicBool::new(false);
        let events = std::sync::atomic::AtomicUsize::new(0);
        let mut progress = |_: &PackProgress| {
            // 2 files -> 2 build events + 2 zip events; fire on the 4th.
            if events.fetch_add(1, std::sync::atomic::Ordering::Relaxed) == 3 {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        };
        let result = pack_with_hooks(
            &sample_manifest(),
            &files,
            &out_dir,
            &stub,
            false,
            Some(&mut progress),
            Some(&cancel),
        );
        assert!(matches!(result, Err(PackError::Cancelled)), "{result:?}");
        assert!(!out_dir.join("TestApp-v1.2.0-x64-Setup.exe").exists());
        assert!(!out_dir.join("TestApp-v1.2.0-x64-Portable.zip").exists());
        assert!(!out_dir.join("SHA256SUMS.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_cancelled_stops_with_interrupted() {
        let root = unique_temp("cancel3");
        let (files, stub) = sample_app(&root);
        let out_dir = root.join("dist");
        let outcome = pack(&sample_manifest(), &files, &out_dir, &stub, false).unwrap();

        let cancel = AtomicBool::new(true);
        let err = verify_package_with(&outcome.setup.path, None, Some(&cancel)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        // The unset flag verifies normally.
        let cancel = AtomicBool::new(false);
        assert!(
            verify_package_with(&outcome.setup.path, None, Some(&cancel))
                .unwrap()
                .ok
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn sandbox_cancelled_before_install_reports_cancelled() {
        let root = unique_temp("cancel4");
        let (files, stub) = sample_app(&root);
        let out_dir = root.join("dist");
        let outcome = pack(&sample_manifest(), &files, &out_dir, &stub, false).unwrap();

        let cancel = AtomicBool::new(true);
        let report = sandbox_verify_with(&outcome.setup.path, Some(&cancel)).unwrap();
        assert!(report.cancelled);
        assert!(!report.ok);
        // The sandbox folder is gone.
        assert!(!report.install_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn verify_rejects_truncated_and_plain_files() {
        let root = unique_temp("verify");
        let (files, stub) = sample_app(&root);
        let out_dir = root.join("dist");
        let outcome = pack(&sample_manifest(), &files, &out_dir, &stub, false).unwrap();

        let truncated = root.join("truncated.exe");
        let bytes = std::fs::read(&outcome.setup.path).unwrap();
        std::fs::write(&truncated, &bytes[..bytes.len() - 10]).unwrap();
        let report = verify_package(&truncated).unwrap();
        assert!(!report.ok);

        let plain = root.join("plain.exe");
        std::fs::write(&plain, vec![0u8; 8192]).unwrap();
        let report = verify_package(&plain).unwrap();
        assert!(!report.ok);

        let _ = std::fs::remove_dir_all(&root);
    }
}
