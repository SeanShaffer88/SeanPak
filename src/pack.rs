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

use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::hash::{crc32, sha256_file_hex};
use crate::manifest::AppManifest;
use crate::overlay::{OverlayReader, OverlayWriter};
use crate::scanner::ScannedFolder;

#[derive(Debug)]
pub enum PackError {
    /// A manifest-listed file is missing, or the file set is unusable.
    BadFiles(String),
    /// An existing output differs and `force` was not given.
    RefuseOverwrite(String),
    Io(std::io::Error),
}

impl std::fmt::Display for PackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PackError::BadFiles(m) => write!(f, "{m}"),
            PackError::RefuseOverwrite(m) => write!(f, "{m}"),
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

#[derive(Debug)]
pub struct PackOutcome {
    pub setup: AssetInfo,
    pub portable: AssetInfo,
    pub sums_path: PathBuf,
    pub stub_bytes: u64,
    pub file_count: usize,
    pub total_payload_bytes: u64,
}

/// One release packaging run. See the module docs for the contract.
pub fn pack(
    manifest: &AppManifest,
    files_dir: &Path,
    out_dir: &Path,
    stub_path: &Path,
    force: bool,
) -> Result<PackOutcome, PackError> {
    let scanned = ScannedFolder::scan(files_dir)?;
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

    let options = manifest.to_install_options();
    let build_result = OverlayWriter::build_overlay(
        stub_path,
        &temp_setup,
        files_dir,
        &files_arg,
        &manifest.app_name,
        &manifest.version,
        &manifest.publisher,
        &options,
    )
    .and_then(|_| write_portable_zip(&files_arg, &temp_portable));
    if let Err(e) = build_result {
        cleanup(&temp_dir);
        return Err(PackError::Io(e));
    }

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

    let setup_info = match place(&temp_setup, &setup_name) {
        Ok(info) => info,
        Err(e) => {
            cleanup(&temp_dir);
            return Err(e);
        }
    };
    let portable_info = match place(&temp_portable, &portable_name) {
        Ok(info) => info,
        Err(e) => {
            cleanup(&temp_dir);
            return Err(e);
        }
    };

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
    })
}

/// Writes a stored (uncompressed) ZIP of `files` — the zero-install
/// portable archive. Entries are sorted by path and stamped with a fixed
/// 1980-01-01 date so identical inputs give identical bytes.
pub fn write_portable_zip(
    files: &[(PathBuf, String, bool)],
    out_path: &Path,
) -> std::io::Result<()> {
    let mut entries: Vec<&(PathBuf, String, bool)> = files.iter().collect();
    entries.sort_by_key(|entry| entry.1.replace('\\', "/"));

    let mut out = std::fs::File::create(out_path)?;
    let mut central: Vec<(String, u32, u64, u64)> = Vec::new(); // name, crc, size, local offset
    let mut offset: u64 = 0;

    const DOS_TIME: u16 = 0;
    const DOS_DATE: u16 = 0x21; // 1980-01-01

    for (abs, rel, _) in &entries {
        let name = rel.replace('\\', "/");
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
    if !(2..=3).contains(&manifest.version) {
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

    // File list matches payloads: paths safe, ranges in bounds and
    // non-overlapping, and every payload fully readable.
    let mut list_ok = true;
    let mut list_notes: Vec<String> = Vec::new();
    let mut ranges: Vec<(u64, u64, String)> = Vec::new();
    for record in &records {
        let rel = record.relative_path();
        if !is_safe_relative_path(&rel) {
            list_ok = false;
            list_notes.push(format!("unsafe stored path \"{rel}\""));
            continue;
        }
        let end = record.payload_offset.saturating_add(record.file_size);
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
        // Read every payload end to end to prove the bytes are there.
        let mut file = std::fs::File::open(path)?;
        let mut buffer = [0u8; 65536];
        for record in &records {
            file.seek(SeekFrom::Start(record.payload_offset))?;
            let mut remaining = record.file_size;
            while remaining > 0 {
                let want = remaining.min(buffer.len() as u64) as usize;
                let n = file.read(&mut buffer[..want])?;
                if n == 0 {
                    list_ok = false;
                    list_notes.push(format!(
                        "payload of \"{}\" is truncated",
                        record.relative_path()
                    ));
                    break;
                }
                remaining -= n as u64;
            }
            if !list_ok {
                break;
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
