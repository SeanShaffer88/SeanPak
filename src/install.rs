//! The install operation — the single implementation behind both the
//! wizard's Install button and the `--install` command (everything-is-a-
//! command: the GUI is a view over this, never a separate code path).
//!
//! [`perform_install`] extracts the payload of the installer at
//! `exe_path` into `dest`, snapshotting every existing target first so
//! any failure rolls the destination back to its previous state, then
//! copies the installer in as `Uninstall.exe` and writes the Add/Remove
//! Programs entry whose uninstall string runs that copy with
//! `--uninstall`. [`plan_install`] computes exactly what a run would do
//! without touching anything (the `--dry-run` report).
//!
//! Shortcut creation, file associations, and PATH entries are recorded
//! in the package manifest and shown in the wizard, but are not applied
//! yet — see docs/PARITY.md.

use std::fs::File;
use std::path::{Path, PathBuf};

use crate::overlay::{FileRecordHeader, InstallOptions, OverlayManifestHeader, OverlayReader};
use crate::rollback::TransactionRollbackLog;
use crate::wizard::uninstall_key_path;

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
}

pub fn plan_install(
    manifest: &OverlayManifestHeader,
    records: &[FileRecordHeader],
    dest: &Path,
) -> InstallPlan {
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
    }
}

#[derive(Debug)]
pub struct InstallOutcome {
    pub files_written: usize,
    pub dest: PathBuf,
    /// True when the ARP entry was written (Windows only; false off
    /// Windows, where there is no registry to write to).
    pub arp_written: bool,
}

/// Performs the install described by the module docs. On any failure the
/// destination is rolled back before the error is returned.
pub fn perform_install(exe_path: &Path, dest: &Path) -> Result<InstallOutcome, String> {
    let file =
        File::open(exe_path).map_err(|e| format!("cannot open {}: {e}", exe_path.display()))?;
    let (manifest, records) = OverlayReader::read_manifest(file)
        .map_err(|e| format!("cannot read package: {e}"))?
        .ok_or_else(|| "this executable carries no SeanPak overlay".to_string())?;

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

    let written = match OverlayReader::extract_file(exe_path, dest) {
        Ok(w) => w,
        Err(e) => {
            tx.rollback();
            return Err(format!("extraction failed: {e}"));
        }
    };
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
    tx.commit()
        .map_err(|e| format!("cannot commit install journal: {e}"))?;
    Ok(InstallOutcome {
        files_written: written.len(),
        dest: dest.to_path_buf(),
        arp_written,
    })
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
