//! The uninstall operation — the single implementation behind the
//! installed `Uninstall.exe` / `seanpak --uninstall` path (the GUI
//! confirm window is a view over it, never a separate code path).
//!
//! [`perform_uninstall`] deletes every payload file the package
//! records under the install directory, removes now-empty payload
//! subdirectories, removes exactly the system artifacts the install
//! receipt journaled (shortcuts, file associations, the PATH segment,
//! the start-at-login Run entry), and removes the receipt and the
//! Add/Remove Programs entry. Only when the caller explicitly asks
//! (`purge_data`) does it then delete the app's recorded data
//! directories.
//!
//! Data deletion is deliberately NOT rollback-journaled: it happens
//! only on the user's explicit, warned choice — the interactive
//! confirm window's checkbox (default unchecked) or the
//! `--purge-data` flag — and the confirm text says plainly that
//! deleted data cannot be recovered.
//!
//! This module also owns data-directory resolution: the manifest's
//! `dataDirs` token paths become absolute paths at install time
//! (recorded in the receipt), and the purge works from exactly those
//! recorded absolutes.

use std::path::{Component, Path, PathBuf};

use crate::diag;
use crate::manifest::{jstr, JsonValue};
use crate::overlay::FileRecordHeader;
use crate::receipt::read_receipt;

/// The roots data-dir tokens resolve against.
#[derive(Debug, Clone, PartialEq)]
pub struct DataDirRoots {
    pub app_data: PathBuf,
    pub local_app_data: PathBuf,
    pub user_profile: PathBuf,
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// This machine's data-dir roots: `%APPDATA%`, `%LOCALAPPDATA%` and
/// `%USERPROFILE%` on Windows; elsewhere, `APPDATA` falls back to the
/// local app data dir and the profile to `$HOME` — the same fallbacks
/// [`crate::install::local_app_data_dir`] uses — so host installs
/// resolve tokens the same way everywhere.
pub fn data_dir_roots() -> DataDirRoots {
    let local = crate::install::local_app_data_dir();
    DataDirRoots {
        app_data: env_path("APPDATA").unwrap_or_else(|| local.clone()),
        local_app_data: local,
        user_profile: env_path("USERPROFILE")
            .or_else(|| env_path("HOME"))
            .unwrap_or_else(std::env::temp_dir),
    }
}

/// Resolves one `dataDirs` token path against explicit roots. Token
/// matching is case-insensitive; a token must be followed by a path
/// below its root (the manifest validator enforces that too — this is
/// the second line of defense) and any `..` component refuses
/// resolution. Absolute paths stand as written; anything else is
/// relative to the install directory.
pub fn resolve_data_dir_with_roots(
    entry: &str,
    roots: &DataDirRoots,
    install_dir: &Path,
) -> Option<PathBuf> {
    let e = entry.trim();
    if e.is_empty() {
        return None;
    }
    let upper = e.to_uppercase();
    let candidates: [(&str, &Path); 4] = [
        ("%APPDATA%", roots.app_data.as_path()),
        ("%LOCALAPPDATA%", roots.local_app_data.as_path()),
        ("%USERPROFILE%", roots.user_profile.as_path()),
        ("%INSTALLDIR%", install_dir),
    ];
    for (token, root) in candidates {
        if upper.starts_with(token) {
            // The token is pure ASCII, so this byte slice is the
            // remainder of the original (case-preserved) entry.
            let rest = e[token.len()..].trim_start_matches(['\\', '/']);
            if rest.is_empty() {
                return None;
            }
            let rel = rest.replace('\\', "/");
            if rel.split('/').any(|part| part == "..") {
                return None;
            }
            return Some(root.join(rel));
        }
    }
    if e.split(['\\', '/']).any(|part| part == "..") {
        return None;
    }
    let absolute = e.chars().nth(1) == Some(':') || e.starts_with(['\\', '/']);
    if absolute {
        return Some(PathBuf::from(e.replace('\\', "/")));
    }
    Some(install_dir.join(e.replace('\\', "/")))
}

/// Resolves every token path in `entries` against this machine's
/// roots and `install_dir` — what install records in the receipt.
pub fn resolve_data_dirs(entries: &[String], install_dir: &Path) -> Vec<PathBuf> {
    let roots = data_dir_roots();
    entries
        .iter()
        .filter_map(|e| resolve_data_dir_with_roots(e, &roots, install_dir))
        .collect()
}

/// What an uninstall did — the CLI report's source.
#[derive(Debug, Clone, Default)]
pub struct UninstallOutcome {
    /// Payload files removed (record-relative paths, in record order).
    pub removed_files: Vec<String>,
    /// Journaled system artifacts removed (shortcuts + associations +
    /// PATH segment + start-at-login entry).
    pub artifacts_removed: usize,
    pub receipt_removed: bool,
    /// Data directories actually deleted (absolute paths) — purge only.
    pub data_deleted: Vec<String>,
    /// Data directories that were refused or could not be deleted,
    /// each with the reason.
    pub data_delete_failures: Vec<String>,
}

/// The purge's safety gate: a recorded directory is only ever deleted
/// when it is absolute, names at least one real folder below its root
/// (never a drive, share, or filesystem root itself), and is not the
/// install directory. The receipt is user-editable text, so this gate
/// does not trust the manifest validation that produced it.
fn purge_target_allowed(path: &Path, dest: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let components: Vec<Component> = path.components().collect();
    let has_named_folder = components.iter().any(|c| matches!(c, Component::Normal(_)));
    if !has_named_folder || components.len() < 2 {
        return false;
    }
    !same_path(path, dest)
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| {
        p.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_lowercase()
    };
    norm(a) == norm(b)
}

/// The uninstall operation described in the module docs. The running
/// uninstaller's own copy inside `dest` is not a payload record, so
/// it is left behind (the CLI reports that separately).
pub fn perform_uninstall(
    app_name: &str,
    records: &[FileRecordHeader],
    dest: &Path,
    purge_data: bool,
) -> UninstallOutcome {
    let mut outcome = UninstallOutcome::default();

    // The install receipt is the journal of system artifacts this
    // install applied; read it before anything is deleted so the
    // uninstall removes exactly those — and knows the data dirs.
    let receipt = read_receipt(dest);

    let mut subdirs: Vec<PathBuf> = Vec::new();
    for record in records {
        let rel = record.relative_path().replace('\\', "/");
        if rel.is_empty() || rel.contains("..") {
            continue;
        }
        let target = dest.join(&rel);
        if target.is_file() {
            match std::fs::remove_file(&target) {
                Ok(()) => outcome.removed_files.push(rel),
                Err(e) => eprintln!("[WARN] Could not remove {}: {e}", target.display()),
            }
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

    // Remove the journaled system artifacts.
    let mut had_run_entry = false;
    if let Some(receipt) = &receipt {
        had_run_entry = receipt.artifacts.start_at_login.is_some();
        outcome.artifacts_removed = receipt.artifacts.shortcuts.len()
            + receipt.artifacts.associations.len()
            + receipt.artifacts.path_segment_added.is_some() as usize
            + had_run_entry as usize;
        crate::sysinstall::remove_artifacts(&receipt.artifacts);
    }
    if had_run_entry {
        diag::record(
            "autostart-removed",
            vec![("app", jstr(app_name)), ("via", jstr("uninstall"))],
        );
    }

    // The optional data purge: explicit, warned, and not journaled.
    if purge_data {
        if let Some(receipt) = &receipt {
            for recorded in &receipt.data_dirs {
                let path = PathBuf::from(recorded);
                if !purge_target_allowed(&path, dest) {
                    outcome.data_delete_failures.push(format!(
                        "{} (refused: not a specific folder below a root, or the install folder itself)",
                        path.display()
                    ));
                    continue;
                }
                if !path.exists() {
                    continue; // already gone — nothing to delete or report
                }
                let result = if path.is_dir() {
                    std::fs::remove_dir_all(&path)
                } else {
                    std::fs::remove_file(&path)
                };
                match result {
                    Ok(()) => outcome
                        .data_deleted
                        .push(path.to_string_lossy().into_owned()),
                    Err(e) => outcome
                        .data_delete_failures
                        .push(format!("{}: {e}", path.display())),
                }
            }
        }
        if !outcome.data_deleted.is_empty() {
            diag::record(
                "data-deleted",
                vec![
                    ("app", jstr(app_name)),
                    (
                        "paths",
                        JsonValue::Array(
                            outcome
                                .data_deleted
                                .iter()
                                .map(|p| jstr(p.clone()))
                                .collect(),
                        ),
                    ),
                ],
            );
        }
    }

    outcome.receipt_removed = std::fs::remove_file(crate::receipt::receipt_path(dest)).is_ok();
    crate::wizard::delete_uninstall_entry(app_name);
    diag::record(
        "uninstall",
        vec![
            ("app", jstr(app_name)),
            ("from", jstr(dest.to_string_lossy().into_owned())),
            (
                "removed",
                JsonValue::Number(outcome.removed_files.len() as f64),
            ),
            (
                "artifactsRemoved",
                JsonValue::Number(outcome.artifacts_removed as f64),
            ),
            (
                "dataDeleted",
                JsonValue::Number(outcome.data_deleted.len() as f64),
            ),
        ],
    );
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::{copy_str_to_fixed_pub, InstallOptions, OverlayReader, OverlayWriter};
    use crate::receipt::{InstallReceipt, RECEIPT_FILE_NAME};
    use std::io::Cursor;

    fn test_roots() -> DataDirRoots {
        DataDirRoots {
            app_data: PathBuf::from("C:\\Users\\x\\AppData\\Roaming"),
            local_app_data: PathBuf::from("C:\\Users\\x\\AppData\\Local"),
            user_profile: PathBuf::from("C:\\Users\\x"),
        }
    }

    #[test]
    fn token_resolution_covers_every_root_and_relative_paths() {
        let roots = test_roots();
        let install = Path::new("C:\\Apps\\My App");
        assert_eq!(
            resolve_data_dir_with_roots("%APPDATA%\\My App", &roots, install),
            Some(PathBuf::from("C:\\Users\\x\\AppData\\Roaming").join("My App"))
        );
        assert_eq!(
            resolve_data_dir_with_roots("%localappdata%\\My App\\cache", &roots, install),
            Some(
                PathBuf::from("C:\\Users\\x\\AppData\\Local")
                    .join("My App")
                    .join("cache")
            )
        );
        assert_eq!(
            resolve_data_dir_with_roots("%USERPROFILE%\\Saved Games\\My App", &roots, install),
            Some(
                PathBuf::from("C:\\Users\\x")
                    .join("Saved Games")
                    .join("My App")
            )
        );
        assert_eq!(
            resolve_data_dir_with_roots("%INSTALLDIR%\\data", &roots, install),
            Some(install.join("data"))
        );
        assert_eq!(
            resolve_data_dir_with_roots("userdata", &roots, install),
            Some(install.join("userdata"))
        );
        assert_eq!(
            resolve_data_dir_with_roots("D:\\Data\\My App", &roots, install),
            Some(PathBuf::from("D:/Data/My App"))
        );
        // Refusals: bare roots and escapes resolve to nothing.
        assert_eq!(
            resolve_data_dir_with_roots("%APPDATA%", &roots, install),
            None
        );
        assert_eq!(
            resolve_data_dir_with_roots("%APPDATA%\\..\\evil", &roots, install),
            None
        );
        assert_eq!(
            resolve_data_dir_with_roots("..\\evil", &roots, install),
            None
        );
        assert_eq!(resolve_data_dir_with_roots("", &roots, install), None);
    }

    #[test]
    fn purge_gate_refuses_roots_and_the_install_dir() {
        // Host-shaped absolute paths (on Windows the recorded paths are
        // drive-rooted and the same component logic applies).
        let dest = Path::new("/Apps/My App");
        assert!(purge_target_allowed(
            Path::new("/Users/x/AppData/Roaming/My App"),
            dest
        ));
        assert!(!purge_target_allowed(Path::new("/"), dest));
        assert!(!purge_target_allowed(Path::new("/Apps/My App"), dest));
        assert!(!purge_target_allowed(Path::new("/Apps/My App/"), dest));
        assert!(!purge_target_allowed(Path::new("relative/data"), dest));
    }

    fn unique_temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "seanpak_uninstall_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn bare_receipt(app: &str, dest: &Path, data_dirs: Vec<String>) -> InstallReceipt {
        InstallReceipt {
            app: app.to_string(),
            version: "1.0".to_string(),
            publisher: String::new(),
            install_path: dest.to_string_lossy().into_owned(),
            installed_at_unix: 0,
            mode: "install".to_string(),
            previous_version: None,
            files: Vec::new(),
            desktop_shortcut: false,
            start_menu_shortcut: false,
            start_menu_folder: app.to_string(),
            path_entry: false,
            file_associations: Vec::new(),
            launch_after_install_offered: true,
            launch_after_install_default: true,
            start_at_login: false,
            purge_data_default: false,
            data_dirs,
            components: Vec::new(),
            components_known: Vec::new(),
            hooks: Vec::new(),
            artifacts: Default::default(),
        }
    }

    #[test]
    fn purge_refuses_a_receipt_that_names_the_install_dir() {
        // A hand-edited receipt must not turn the purge into a weapon:
        // the install dir itself and a bare root are refused, and the
        // refusal is reported, not silently skipped.
        let root = unique_temp("guard");
        let dest = root.join("app");
        std::fs::create_dir_all(&dest).unwrap();
        std::fs::write(dest.join("keep.txt"), b"keep").unwrap();
        bare_receipt(
            "Guard App",
            &dest,
            vec![dest.to_string_lossy().into_owned(), "/".to_string()],
        )
        .write_to(&dest)
        .unwrap();
        let outcome = perform_uninstall("Guard App", &[], &dest, true);
        assert!(outcome.data_deleted.is_empty());
        assert_eq!(outcome.data_delete_failures.len(), 2);
        assert!(dest.join("keep.txt").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn install_records_data_dirs_and_uninstall_purges_only_on_request() {
        let root = unique_temp("lifecycle");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.exe"), b"fake-pe").unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0u8; 8192]).unwrap();
        let pkg = root.join("Setup.exe");
        let mut options = InstallOptions::defaults_for("Data App");
        copy_str_to_fixed_pub(&mut options.data_dirs_utf8, "data");
        OverlayWriter::build_from_folder_with_options(
            &stub, &pkg, &src, "Data App", "1.0", "", &options,
        )
        .unwrap();

        let bytes = std::fs::read(&pkg).unwrap();
        let (_manifest, records, _opts) = OverlayReader::read_package(Cursor::new(bytes))
            .unwrap()
            .unwrap();
        let dest = root.join("installed");

        // Install; the receipt records the resolved data dir.
        crate::install::perform_install(&pkg, &dest).unwrap();
        let receipt = read_receipt(&dest).expect("receipt");
        let data_dir = dest.join("data");
        assert_eq!(
            receipt.data_dirs,
            vec![data_dir.to_string_lossy().into_owned()]
        );
        assert!(receipt.launch_after_install_offered);
        assert!(!receipt.start_at_login);

        // The app writes data at runtime; uninstall without purge keeps it.
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("settings.json"), b"{}").unwrap();
        let outcome = perform_uninstall("Data App", &records, &dest, false);
        assert!(outcome.data_deleted.is_empty());
        assert!(data_dir.join("settings.json").exists());
        assert!(!dest.join(RECEIPT_FILE_NAME).exists());

        // Reinstall, replant, purge: the data dir is deleted and reported.
        crate::install::perform_install(&pkg, &dest).unwrap();
        std::fs::create_dir_all(&data_dir).unwrap();
        std::fs::write(data_dir.join("settings.json"), b"{}").unwrap();
        let outcome = perform_uninstall("Data App", &records, &dest, true);
        assert_eq!(
            outcome.data_deleted,
            vec![data_dir.to_string_lossy().into_owned()]
        );
        assert!(!data_dir.exists());
        let _ = std::fs::remove_dir_all(&root);
    }
}
