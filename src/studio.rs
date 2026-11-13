//! The SeanPak Builder studio (TesselGUI Design D) — the secondary,
//! human-facing surface over the same operations the CLI exposes.
//!
//! Layout (approved mockup `d-builder-studio`): a dominant files table
//! on the left, an app-manifest panel on the right, and an output strip
//! along the bottom, in the TesselGUI rev2 GitHub Dark palette. Every
//! action calls a shared operation — pack is `pack::pack`, validate is
//! `manifest::AppManifest::from_json`, verify is `pack::verify_package`
//! / `pack::sandbox_verify`, and the file listing is
//! `scanner::ScannedFolder::scan` — so the studio can do nothing the
//! CLI cannot (see `docs/PARITY.md`).
//!
//! This module's first half is the platform-independent document and
//! settings model (unit-tested on any host); the Win32 window itself is
//! Windows-only, like the wizard.

use std::path::{Path, PathBuf};

use crate::manifest::{jobj, jstr, write_json_string, AppManifest, JsonValue};
use crate::scanner::ScannedFolder;

/// Files larger than this get no compressed-size estimate in the table
/// (compressing them just to fill a cell would stall the listing); the
/// cell shows an honest "—" instead.
pub const PACKED_ESTIMATE_CAP: u64 = 16 * 1024 * 1024;

/// Estimated stored size of one payload file after SeanPak's
/// store-if-larger compression — the same `codec::compress_best` the
/// packer runs, so the estimate is the real packed size, not a guess.
pub fn estimate_packed_size(path: &Path, size: u64) -> Option<u64> {
    if size == 0 || size > PACKED_ESTIMATE_CAP {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let (_codec, stored) = crate::codec::compress_best(&data);
    Some(stored.len() as u64)
}

/// One row of the studio's files table.
#[derive(Debug, Clone, PartialEq)]
pub struct FileRow {
    /// Package-relative path, "/" separated.
    pub rel: String,
    pub size: u64,
    /// Compressed-size estimate; `None` renders as "—".
    pub packed: Option<u64>,
    /// False when the manifest lists the file but it is not in the
    /// files directory (pack will refuse with a plain error).
    pub on_disk: bool,
}

/// The result of scanning a files directory once: everything the table
/// and the document need, computed off the UI thread.
#[derive(Debug)]
pub struct ScanOutcome {
    pub all_files: Vec<String>,
    pub suggested_primary_exe: Option<String>,
    pub rows_by_rel: Vec<FileRow>,
}

/// Scans `files_dir` and pre-computes a row (with packed-size estimate)
/// for every file in it.
pub fn scan_package(files_dir: &Path) -> std::io::Result<ScanOutcome> {
    let scanned = ScannedFolder::scan(files_dir)?;
    let mut all_files: Vec<String> = Vec::with_capacity(scanned.files.len());
    let mut rows_by_rel: Vec<FileRow> = Vec::with_capacity(scanned.files.len());
    for f in &scanned.files {
        let rel = f.relative_path.replace('\\', "/");
        all_files.push(rel.clone());
        rows_by_rel.push(FileRow {
            rel,
            size: f.file_size,
            packed: estimate_packed_size(&f.absolute_path, f.file_size),
            on_disk: true,
        });
    }
    all_files.sort();
    rows_by_rel.sort_by(|a, b| a.rel.cmp(&b.rel));
    Ok(ScanOutcome {
        all_files,
        suggested_primary_exe: scanned.suggested_primary_exe.map(|p| p.replace('\\', "/")),
        rows_by_rel,
    })
}

/// Builds the table rows for the document's entry list from a scan:
/// listed-but-missing files stay visible (with `on_disk: false`) so the
/// user sees the problem instead of the file silently vanishing.
pub fn build_rows(entries: &[String], scan: Option<&ScanOutcome>) -> Vec<FileRow> {
    entries
        .iter()
        .map(
            |rel| match scan.and_then(|s| s.rows_by_rel.iter().find(|r| &r.rel == rel)) {
                Some(row) => row.clone(),
                None => FileRow {
                    rel: rel.clone(),
                    size: 0,
                    packed: None,
                    on_disk: false,
                },
            },
        )
        .collect()
}

/// Pretty-prints a manifest JSON value (2-space indent) so a saved
/// manifest stays pleasant for a human or an AI to read and edit.
pub fn render_pretty(value: &JsonValue) -> String {
    let mut out = String::new();
    write_pretty(value, 0, &mut out);
    out.push('\n');
    out
}

fn write_pretty(value: &JsonValue, indent: usize, out: &mut String) {
    match value {
        JsonValue::Object(pairs) if !pairs.is_empty() => {
            out.push('{');
            for (i, (k, v)) in pairs.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&"  ".repeat(indent + 1));
                write_json_string(out, k);
                out.push_str(": ");
                write_pretty(v, indent + 1, out);
            }
            out.push('\n');
            out.push_str(&"  ".repeat(indent));
            out.push('}');
        }
        JsonValue::Array(items) if !items.is_empty() => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push('\n');
                out.push_str(&"  ".repeat(indent + 1));
                write_pretty(item, indent + 1, out);
            }
            out.push('\n');
            out.push_str(&"  ".repeat(indent));
            out.push(']');
        }
        other => out.push_str(&other.render()),
    }
}

// ---------------------------------------------------------------------------
// The studio document: one manifest being edited, its files directory,
// and the package file list derived from both.

#[derive(Debug)]
pub struct StudioDocument {
    pub manifest: AppManifest,
    pub manifest_path: Option<PathBuf>,
    pub files_dir: Option<PathBuf>,
    pub out_dir: PathBuf,
    /// Package file list (relative, "/" separated, sorted). Mirrors
    /// `manifest.files` except that "the whole files directory" is
    /// stored as an empty `files` list with `entries` holding the
    /// resolved names.
    pub entries: Vec<String>,
    pub dirty: bool,
}

impl StudioDocument {
    /// A fresh, unsaved document with schema defaults.
    pub fn new_blank() -> Self {
        let manifest = AppManifest::from_json(r#"{"appName":"New App","version":"1.0.0"}"#)
            .expect("the built-in starter manifest is valid");
        Self {
            manifest,
            manifest_path: None,
            files_dir: None,
            out_dir: PathBuf::from("dist"),
            entries: Vec::new(),
            dirty: false,
        }
    }

    /// Opens a manifest file. The files directory defaults to the
    /// manifest's own folder (the layout every SeanPak caller project
    /// uses); entries resolve from the manifest's explicit list, or
    /// from a scan of that folder when the list is empty.
    pub fn open_manifest(path: &Path) -> Result<Self, Vec<String>> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| vec![format!("Cannot read {}: {e}", path.display())])?;
        let manifest = AppManifest::from_json(&text)?;
        let files_dir = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.to_path_buf());
        let mut entries: Vec<String> = manifest.files.clone();
        if entries.is_empty() {
            if let Some(dir) = &files_dir {
                if let Ok(scan) = scan_package(dir) {
                    entries = scan.all_files;
                }
            }
        }
        entries.sort();
        entries.dedup();
        Ok(Self {
            manifest,
            manifest_path: Some(path.to_path_buf()),
            out_dir: files_dir
                .as_ref()
                .map(|d| d.join("dist"))
                .unwrap_or_else(|| PathBuf::from("dist")),
            files_dir,
            entries,
            dirty: false,
        })
    }

    /// Keeps `manifest.files` honest after entries change: when the
    /// entry list is exactly the files directory's content, the
    /// manifest stores the empty (whole-directory) list; otherwise it
    /// stores the explicit list.
    pub fn sync_files_field(&mut self, all_files: &[String]) {
        let mut sorted_entries = self.entries.clone();
        sorted_entries.sort();
        let mut sorted_all = all_files.to_vec();
        sorted_all.sort();
        if sorted_entries == sorted_all {
            self.manifest.files = Vec::new();
        } else {
            self.manifest.files = sorted_entries;
        }
    }

    /// Validates the document exactly the way `seanpak validate` and
    /// `seanpak pack` do: render to JSON, parse with the shared rules.
    pub fn validate(&self) -> Result<AppManifest, Vec<String>> {
        AppManifest::from_json(&self.manifest.to_json_value().render())
    }

    /// Writes the manifest (pretty JSON) to `path` and adopts it as
    /// the document's path.
    pub fn save_to(&mut self, path: &Path) -> Result<(), String> {
        let text = render_pretty(&self.manifest.to_json_value());
        std::fs::write(path, text).map_err(|e| format!("Cannot write {}: {e}", path.display()))?;
        self.manifest_path = Some(path.to_path_buf());
        self.dirty = false;
        Ok(())
    }

    /// The window title for this document, with a trailing " *" as the
    /// unsaved-changes marker.
    pub fn title_text(&self) -> String {
        let name = match &self.manifest_path {
            Some(p) => p
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| p.display().to_string()),
            None => "Untitled manifest".to_string(),
        };
        if self.dirty {
            format!("SeanPak Builder — {name} *")
        } else {
            format!("SeanPak Builder — {name}")
        }
    }
}

// ---------------------------------------------------------------------------
// Studio settings: window geometry + last-used paths, as a small JSON
// file beside the executable — the same "beside the exe" location the
// diagnostics log uses (see `diag::log_path`). No registry, no config
// framework.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StudioSettings {
    /// (x, y, width, height) of the window when it last closed.
    pub window_rect: Option<(i32, i32, i32, i32)>,
    pub manifest_path: Option<PathBuf>,
    pub files_dir: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
}

/// `seanpak-studio.json` beside the executable (temp dir fallback,
/// mirroring `diag::log_path`).
pub fn settings_path() -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("seanpak-studio.json");
        }
    }
    std::env::temp_dir().join("seanpak-studio.json")
}

impl StudioSettings {
    pub fn load() -> Self {
        let Ok(text) = std::fs::read_to_string(settings_path()) else {
            return Self::default();
        };
        Self::from_json(&text).unwrap_or_default()
    }

    /// Best-effort: settings must never block the studio from closing.
    pub fn save(&self) {
        let _ = std::fs::write(settings_path(), self.to_json().render());
    }

    pub fn to_json(&self) -> JsonValue {
        let path = |p: &Option<PathBuf>| match p {
            Some(p) => jstr(p.display().to_string()),
            None => JsonValue::Null,
        };
        let window = match self.window_rect {
            Some((x, y, w, h)) => JsonValue::Array(vec![
                JsonValue::Number(x as f64),
                JsonValue::Number(y as f64),
                JsonValue::Number(w as f64),
                JsonValue::Number(h as f64),
            ]),
            None => JsonValue::Null,
        };
        jobj(vec![
            ("window", window),
            ("manifestPath", path(&self.manifest_path)),
            ("filesDir", path(&self.files_dir)),
            ("outDir", path(&self.out_dir)),
        ])
    }

    pub fn from_json(text: &str) -> Option<Self> {
        let root = crate::manifest::parse_json(text).ok()?;
        let get_path = |key: &str| {
            root.get(key)
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
        };
        let window_rect = match root.get("window") {
            Some(JsonValue::Array(items)) if items.len() == 4 => {
                let num = |v: &JsonValue| match v {
                    JsonValue::Number(n) => Some(*n as i32),
                    _ => None,
                };
                match (
                    num(&items[0]),
                    num(&items[1]),
                    num(&items[2]),
                    num(&items[3]),
                ) {
                    (Some(x), Some(y), Some(w), Some(h)) if w >= 400 && h >= 300 => {
                        Some((x, y, w, h))
                    }
                    _ => None,
                }
            }
            _ => None,
        };
        Some(Self {
            window_rect,
            manifest_path: get_path("manifestPath"),
            files_dir: get_path("filesDir"),
            out_dir: get_path("outDir"),
        })
    }
}

/// Copies one file into `dest_dir` (Add files…). Refuses to overwrite
/// an existing file — the caller reports the skip in plain language.
pub fn copy_file_into(src: &Path, dest_dir: &Path) -> std::io::Result<PathBuf> {
    let name = src
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name"))?;
    let dest = dest_dir.join(name);
    if dest.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists in the package folder", dest.display()),
        ));
    }
    std::fs::copy(src, &dest)?;
    Ok(dest)
}

/// Copies a folder tree into `dest_dir/<folder name>` (Add folder…),
/// returning the package-relative paths of the files copied.
/// Existing files are skipped, never overwritten.
pub fn copy_folder_into(src_dir: &Path, dest_dir: &Path) -> std::io::Result<Vec<String>> {
    let name = src_dir
        .file_name()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no folder name"))?;
    let target_root = dest_dir.join(name);
    let scanned = ScannedFolder::scan(src_dir)?;
    let mut copied: Vec<String> = Vec::new();
    for f in &scanned.files {
        let rel = f.relative_path.replace('\\', "/");
        let dest = target_root.join(&rel);
        if dest.exists() {
            continue;
        }
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(&f.absolute_path, &dest)?;
        copied.push(format!("{}/{rel}", name.to_string_lossy()));
    }
    Ok(copied)
}

// ---------------------------------------------------------------------------
// The window itself.

/// The Builder studio window. A bare `seanpak` run with no overlay
/// opens it; `initial_folder` pre-selects a files directory (the
/// `seanpak <folder>` command line).
pub struct BuilderStudio {
    pub initial_folder: Option<PathBuf>,
}

impl BuilderStudio {
    pub fn new(initial_folder: Option<PathBuf>) -> Self {
        Self { initial_folder }
    }
}

#[cfg(windows)]
impl BuilderStudio {
    /// Opens the studio window and runs its event-driven message loop.
    pub fn run(&mut self) -> Result<(), u32> {
        imp::run(self.initial_folder.clone())
    }
}

#[cfg(not(windows))]
impl BuilderStudio {
    /// The studio is a native Win32 window; other platforms get an error.
    pub fn run(&mut self) -> Result<(), u32> {
        let _ = self
            .initial_folder
            .as_ref()
            .map(|p| p.display().to_string());
        Err(1)
    }
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
            "seanpak_studio_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn settings_round_trip() {
        let settings = StudioSettings {
            window_rect: Some((10, 20, 1180, 780)),
            manifest_path: Some(PathBuf::from("C:/apps/Foo/manifest.json")),
            files_dir: Some(PathBuf::from("C:/apps/Foo")),
            out_dir: None,
        };
        let text = settings.to_json().render();
        let back = StudioSettings::from_json(&text).expect("settings parse");
        assert_eq!(settings, back);
        // Garbage and undersized windows fall back cleanly.
        assert_eq!(StudioSettings::from_json("not json"), None);
        let tiny = StudioSettings::from_json(r#"{"window":[0,0,10,10]}"#).unwrap();
        assert_eq!(tiny.window_rect, None);
    }

    #[test]
    fn pretty_manifest_round_trips() {
        let manifest =
            AppManifest::from_json(r#"{"appName":"T","version":"1.0","fileAssociations":["txt"]}"#)
                .unwrap();
        let pretty = render_pretty(&manifest.to_json_value());
        let reparsed = AppManifest::from_json(&pretty).unwrap();
        assert_eq!(manifest, reparsed);
    }

    #[test]
    fn document_open_sync_and_save() {
        let root = unique_temp("doc");
        let files = root.join("app");
        std::fs::create_dir_all(files.join("docs")).unwrap();
        std::fs::write(files.join("app.exe"), b"pe").unwrap();
        std::fs::write(files.join("docs/readme.txt"), b"hi").unwrap();
        let manifest_path = files.join("manifest.json");
        std::fs::write(&manifest_path, r#"{"appName":"Doc App","version":"2.0.0"}"#).unwrap();

        let mut doc = StudioDocument::open_manifest(&manifest_path).unwrap();
        assert_eq!(doc.files_dir.as_deref(), Some(files.as_path()));
        // No explicit files list -> entries resolve to the whole folder.
        assert!(doc.entries.contains(&"app.exe".to_string()));
        assert!(doc.entries.contains(&"docs/readme.txt".to_string()));
        assert!(!doc.dirty);
        assert_eq!(doc.title_text(), "SeanPak Builder — manifest.json");

        // Removing one entry makes the list explicit on sync. (The
        // manifest itself is a package file here because it lives in
        // the files folder — exactly what the CLI's whole-directory
        // pack would include.)
        let all = doc.entries.clone();
        doc.entries.retain(|e| e != "docs/readme.txt");
        doc.sync_files_field(&all);
        assert_eq!(
            doc.manifest.files,
            vec!["app.exe".to_string(), "manifest.json".to_string()]
        );
        // Restoring the full set collapses back to whole-directory mode.
        doc.entries = all.clone();
        doc.sync_files_field(&all);
        assert!(doc.manifest.files.is_empty());

        // Save -> reopen round trip keeps the manifest intact.
        doc.dirty = true;
        let saved = root.join("saved.json");
        doc.save_to(&saved).unwrap();
        assert!(!doc.dirty);
        let reopened = StudioDocument::open_manifest(&saved).unwrap();
        assert_eq!(reopened.manifest.app_name, "Doc App");
        assert_eq!(reopened.manifest.version, "2.0.0");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn document_validate_uses_shared_rules() {
        let mut doc = StudioDocument::new_blank();
        assert!(doc.validate().is_ok());
        doc.manifest.version = "beta".to_string();
        let errors = doc.validate().unwrap_err();
        assert!(errors.iter().any(|e| e.contains("version")), "{errors:?}");
    }

    #[test]
    fn scan_and_rows_report_missing_entries() {
        let root = unique_temp("scan");
        std::fs::write(root.join("a.txt"), b"aaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let scan = scan_package(&root).unwrap();
        assert_eq!(scan.all_files, vec!["a.txt".to_string()]);
        let rows = build_rows(&["a.txt".to_string(), "ghost.bin".to_string()], Some(&scan));
        assert!(rows[0].on_disk);
        assert!(!rows[1].on_disk);
        assert_eq!(rows[1].packed, None);
    }

    #[test]
    fn estimate_respects_cap_and_shrinks_text() {
        let root = unique_temp("est");
        let small = root.join("small.txt");
        std::fs::write(&small, "repeat repeat repeat repeat repeat").unwrap();
        let est = estimate_packed_size(&small, 34).expect("small file estimates");
        assert!(est <= 34);
        assert_eq!(estimate_packed_size(&small, PACKED_ESTIMATE_CAP + 1), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn studio_chain_packs_and_verifies() {
        // The exact operation chain the studio's buttons run: open a
        // manifest, resolve entries from the folder scan, sync the
        // files field, validate with the shared rules, then call the
        // shared pack operation and verify its Setup.exe.
        let root = unique_temp("chain");
        let files = root.join("payload");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("chain.exe"), b"fake-pe-bytes").unwrap();
        std::fs::write(files.join("notes.txt"), b"chain notes").unwrap();
        let manifest_path = root.join("manifest.json");
        std::fs::write(
            &manifest_path,
            r#"{"appName":"Chain App","version":"0.1.0","primaryExe":"chain.exe"}"#,
        )
        .unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xBBu8; 4096]).unwrap();

        let mut doc = StudioDocument::open_manifest(&manifest_path).unwrap();
        // open_manifest defaults the files dir to the manifest's
        // folder; the studio points it at the payload folder instead.
        doc.files_dir = Some(files.clone());
        let scan = scan_package(&files).unwrap();
        doc.entries = scan.all_files.clone();
        doc.sync_files_field(&scan.all_files);
        assert!(doc.manifest.files.is_empty(), "whole-directory mode");
        let rows = build_rows(&doc.entries, Some(&scan));
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.on_disk));

        let manifest = doc.validate().unwrap();
        let out_dir = root.join("dist");
        let outcome = crate::pack::pack(&manifest, &files, &out_dir, &stub, false).unwrap();
        assert_eq!(outcome.setup.name, "ChainApp-v0.1.0-x64-Setup.exe");
        let report = crate::pack::verify_package(&outcome.setup.path).unwrap();
        assert!(report.ok, "{:?}", report.checks);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn copy_helpers_never_overwrite() {
        let root = unique_temp("copy");
        let src_dir = root.join("src");
        std::fs::create_dir_all(src_dir.join("sub")).unwrap();
        std::fs::write(src_dir.join("sub/f.txt"), b"x").unwrap();
        let lone = root.join("lone.txt");
        std::fs::write(&lone, b"y").unwrap();
        let dest = root.join("dest");
        std::fs::create_dir_all(&dest).unwrap();

        let copied = copy_file_into(&lone, &dest).unwrap();
        assert!(copied.is_file());
        assert!(copy_file_into(&lone, &dest).is_err(), "second copy refuses");

        let copied = copy_folder_into(&src_dir, &dest).unwrap();
        assert_eq!(copied, vec!["src/sub/f.txt".to_string()]);
        assert!(dest.join("src/sub/f.txt").is_file());
        let again = copy_folder_into(&src_dir, &dest).unwrap();
        assert!(
            again.is_empty(),
            "existing files are skipped, not overwritten"
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::UI::Controls::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, GetKeyState, SetFocus};
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use super::{
        build_rows, copy_file_into, copy_folder_into, scan_package, FileRow, ScanOutcome,
        StudioDocument, StudioSettings,
    };
    use crate::manifest::{jstr, AppManifest, JsonValue, MANIFEST_SCHEMA_JSON};
    use crate::pack::{PackError, PackOutcome};
    use crate::scanner::format_bytes;

    // TesselGUI rev2 GitHub Dark palette (COLORREF is 0x00BBGGRR).
    const COLOR_BG: u32 = 0x0017_110D; // #0D1117
    const COLOR_SURFACE: u32 = 0x001D_1612; // #12161D
    const COLOR_FG: u32 = 0x00FC_F6F0; // #F0F6FC
    const COLOR_MUTED: u32 = 0x00A1_9891; // #9198A1
    const COLOR_GREEN: u32 = 0x0037_7F1A; // #1A7F37
    const COLOR_GREEN_DARK: u32 = 0x002D_5314; // #14532D (pressed state)
    const COLOR_DANGER: u32 = 0x0049_51F8; // #F85149

    // Controls.
    const ID_LIST: usize = 100;
    const ID_BTN_ADD_FILES: usize = 101;
    const ID_BTN_ADD_FOLDER: usize = 102;
    const ID_BTN_REMOVE: usize = 103;
    const ID_EDT_NAME: usize = 110;
    const ID_EDT_VERSION: usize = 111;
    const ID_EDT_PUBLISHER: usize = 112;
    const ID_EDT_PRIMARY: usize = 113;
    const ID_EDT_INSTALL_LOC: usize = 114;
    const ID_EDT_SM_FOLDER: usize = 115;
    const ID_EDT_ASSOC: usize = 116;
    const ID_CHK_DESKTOP: usize = 120;
    const ID_CHK_STARTMENU: usize = 121;
    const ID_CHK_PATH: usize = 122;
    const ID_EDT_FILES_DIR: usize = 130;
    const ID_BTN_FILES_DIR: usize = 131;
    const ID_EDT_OUT_DIR: usize = 132;
    const ID_BTN_OUT_DIR: usize = 133;
    const ID_OUTPUT: usize = 140;
    const ID_BTN_VALIDATE: usize = 141;
    const ID_BTN_VERIFY: usize = 142;
    const ID_CHK_SANDBOX: usize = 143;
    const ID_LBL_STATUS: usize = 150;
    const ID_BTN_TEST: usize = 151;
    const ID_BTN_BUILD: usize = 152;
    const ID_LBL_FILES_HDR: usize = 160;
    const ID_LBL_MANIFEST_HDR: usize = 161;
    const ID_LBL_OUTPUT_HDR: usize = 162;
    const ID_LBL_FACTS: usize = 163;
    const ID_LBL_PANEL_NOTE: usize = 164;
    const ID_LBL_PACKAGE_HDR: usize = 165;
    // Field labels inside the manifest panel (colored as one group).
    const PANEL_LABELS: [usize; 9] = [170, 171, 172, 173, 174, 175, 176, 177, 178];

    // Menus.
    const IDM_FILE_OPEN: usize = 9001;
    const IDM_FILE_SAVE: usize = 9002;
    const IDM_FILE_SAVEAS: usize = 9003;
    const IDM_FILE_EXIT: usize = 9004;
    const IDM_PROJ_FILES: usize = 9011;
    const IDM_PROJ_OUT: usize = 9012;
    const IDM_BUILD_VALIDATE: usize = 9021;
    const IDM_BUILD_PACK: usize = 9022;
    const IDM_BUILD_VERIFY: usize = 9023;
    const IDM_BUILD_TEST: usize = 9024;
    const IDM_TOOLS_SCHEMA: usize = 9031;
    const IDM_TOOLS_LOG: usize = 9032;
    const IDM_HELP_ABOUT: usize = 9041;

    // Messages / styles windows-sys keeps behind features this crate
    // does not enable; defined locally like the wizard does.
    const WM_APP_JOB: u32 = 0x8000 + 21;
    const WM_DPICHANGED_MSG: u32 = 0x02E0;
    const EM_SETSEL_MSG: u32 = 0x00B1;
    const EM_REPLACESEL_MSG: u32 = 0x00C2;
    const EM_SCROLLCARET_MSG: u32 = 0x00B7;
    const TTM_ADDTOOLW_MSG: u32 = 0x0400 + 50;
    const TTM_RELAYEVENT_MSG: u32 = 0x0407;
    const TTS_ALWAYSTIP_STYLE: u32 = 0x01;
    const TTF_IDISHWND_FLAG: u32 = 0x0001;
    const TTF_SUBCLASS_FLAG: u32 = 0x0010;
    const BS_AUTOCHECKBOX: u32 = 0x0003;
    const BS_OWNERDRAW: u32 = 0x000B;
    const ES_AUTOHSCROLL: u32 = 0x0080;
    const ES_MULTILINE: u32 = 0x0004;
    const ES_READONLY: u32 = 0x0800;
    const ES_AUTOVSCROLL: u32 = 0x0040;
    const BM_GETCHECK_MSG: u32 = 0x00F0;
    const BM_SETCHECK_MSG: u32 = 0x00F1;
    const BST_CHECKED_STATE: usize = 1;
    const EN_CHANGE_CODE: usize = 0x0300;
    const LOGPIXELSX_INDEX: i32 = 88;
    const VK_CONTROL_KEY: i32 = 0x11;

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

    #[derive(Clone, Copy, PartialEq)]
    enum StatusKind {
        Neutral,
        Ok,
        Error,
    }

    enum JobResult {
        Scan(Result<ScanOutcome, String>),
        Pack(Result<PackOutcome, PackFail>),
        Verify {
            sandbox: bool,
            path: PathBuf,
            ok: bool,
            lines: Vec<String>,
        },
    }

    struct PackFail {
        message: String,
        refused_overwrite: bool,
    }

    struct StudioState {
        doc: StudioDocument,
        /// The scan the table currently shows, and the directory the
        /// document's entry list belongs to.
        scan: Option<ScanOutcome>,
        entries_dir: Option<PathBuf>,
        /// Files copied in by Add, to be added to the entries when the
        /// follow-up scan lands.
        pending_adds: Vec<String>,
        busy: bool,
        job: Arc<Mutex<Option<JobResult>>>,
        last_setup: Option<PathBuf>,
        dpi: u32,
        suppress_dirty: bool,
        status_kind: StatusKind,
        tooltip: usize,
        /// Kept alive for the tooltip tool infos.
        tip_texts: Vec<Vec<u16>>,
    }

    static STATE: Mutex<Option<StudioState>> = Mutex::new(None);
    static mut BG_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut SURFACE_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut GREEN_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut GREEN_DARK_BRUSH: HBRUSH = std::ptr::null_mut();
    static mut BODY_FONT: HFONT = std::ptr::null_mut();
    static mut HEADER_FONT: HFONT = std::ptr::null_mut();

    fn scale(v: i32, dpi: u32) -> i32 {
        v * dpi as i32 / 96
    }

    fn with_state<R>(f: impl FnOnce(&mut StudioState) -> R) -> Option<R> {
        STATE.lock().ok().and_then(|mut g| g.as_mut().map(f))
    }

    pub fn run(initial_folder: Option<PathBuf>) -> Result<(), u32> {
        // Document: last session's manifest when it still exists, else
        // a blank one. Entries resolve from the manifest list only —
        // the folder scan (with its size estimates) runs on a worker
        // once the window is up, so startup stays instant.
        let settings = StudioSettings::load();
        let mut doc = StudioDocument::new_blank();
        if let Some(path) = &settings.manifest_path {
            if path.is_file() {
                if let Ok(text) = std::fs::read_to_string(path) {
                    if let Ok(manifest) = AppManifest::from_json(&text) {
                        doc.entries = manifest.files.clone();
                        doc.files_dir = path
                            .parent()
                            .filter(|p| !p.as_os_str().is_empty())
                            .map(|p| p.to_path_buf());
                        doc.out_dir = doc
                            .files_dir
                            .as_ref()
                            .map(|d| d.join("dist"))
                            .unwrap_or_else(|| PathBuf::from("dist"));
                        doc.manifest = manifest;
                        doc.manifest_path = Some(path.clone());
                    }
                }
            }
        }
        if let Some(dir) = &settings.files_dir {
            if doc.files_dir.is_none() {
                doc.files_dir = Some(dir.clone());
            }
        }
        if let Some(out) = &settings.out_dir {
            doc.out_dir = out.clone();
        }
        if let Some(folder) = initial_folder {
            doc.files_dir = Some(folder);
        }
        let entries_dir = doc.files_dir.clone();

        if let Ok(mut guard) = STATE.lock() {
            *guard = Some(StudioState {
                doc,
                scan: None,
                entries_dir,
                pending_adds: Vec::new(),
                busy: false,
                job: Arc::new(Mutex::new(None)),
                last_setup: None,
                dpi: 96,
                suppress_dirty: true,
                status_kind: StatusKind::Neutral,
                tooltip: 0,
                tip_texts: Vec::new(),
            });
        }

        unsafe {
            let init = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_LISTVIEW_CLASSES | ICC_WIN95_CLASSES,
            };
            InitCommonControlsEx(&init);

            let instance =
                windows_sys::Win32::System::LibraryLoader::GetModuleHandleW(std::ptr::null());
            let class_name: Vec<u16> = "SeanPakStudioWindowClass\0".encode_utf16().collect();

            BG_BRUSH = CreateSolidBrush(COLOR_BG);
            SURFACE_BRUSH = CreateSolidBrush(COLOR_SURFACE);
            GREEN_BRUSH = CreateSolidBrush(COLOR_GREEN);
            GREEN_DARK_BRUSH = CreateSolidBrush(COLOR_GREEN_DARK);

            let icon = LoadIconW(instance, 1 as _);
            let icon = if icon.is_null() {
                LoadIconW(std::ptr::null_mut(), IDI_APPLICATION)
            } else {
                icon
            };
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(wnd_proc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: icon,
                hCursor: LoadCursorW(std::ptr::null_mut(), IDC_ARROW),
                hbrBackground: BG_BRUSH,
                lpszMenuName: std::ptr::null(),
                lpszClassName: class_name.as_ptr(),
                hIconSm: icon,
            };
            if RegisterClassExW(&wc) == 0 {
                let err = GetLastError();
                if err != 1410 {
                    crate::diag::log("studio", &format!("RegisterClassExW failed: error {err}"));
                    return Err(err);
                }
            }

            let (x, y, w, h) = match settings.window_rect {
                Some((x, y, w, h)) => (x, y, w, h),
                None => (CW_USEDEFAULT, CW_USEDEFAULT, 1180, 780),
            };
            let title = with_state(|s| s.doc.title_text()).unwrap_or_else(|| {
                format!("SeanPak Builder — SeanPak {}", env!("CARGO_PKG_VERSION"))
            });
            let title_w: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let hwnd = CreateWindowExW(
                0,
                class_name.as_ptr(),
                title_w.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                x,
                y,
                w,
                h,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                instance,
                std::ptr::null_mut(),
            );
            if hwnd.is_null() {
                let err = GetLastError();
                crate::diag::log("studio", &format!("CreateWindowExW failed: error {err}"));
                return Err(err);
            }
            crate::diag::log("studio", "window created");

            let dpi = current_dpi(hwnd);
            with_state(|s| s.dpi = dpi);
            create_fonts(dpi);
            SetMenu(hwnd, build_menu());
            create_controls(hwnd, instance);
            apply_fonts(hwnd);
            layout(hwnd);
            fill_controls(hwnd);
            with_state(|s| s.suppress_dirty = false);
            update_title(hwnd);

            ShowWindow(hwnd, SW_SHOWDEFAULT);
            UpdateWindow(hwnd);
            let list = GetDlgItem(hwnd, ID_LIST as i32);
            if !list.is_null() {
                SetFocus(list);
            }
            // First scan runs on a worker now the window is visible.
            start_scan(hwnd);

            crate::diag::log("studio", "message loop entered");
            let mut msg: MSG = std::mem::zeroed();
            loop {
                let result = GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0);
                if result == 0 {
                    break;
                }
                if result == -1 {
                    let err = GetLastError();
                    crate::diag::log("studio", &format!("GetMessageW failed: error {err}"));
                    return Err(err);
                }
                let tooltip = with_state(|s| s.tooltip).unwrap_or(0);
                if tooltip != 0 {
                    SendMessageW(
                        tooltip as HWND,
                        TTM_RELAYEVENT_MSG,
                        0,
                        &msg as *const _ as LPARAM,
                    );
                }
                if handle_accelerator(hwnd, &msg) {
                    continue;
                }
                if IsDialogMessageW(hwnd, &msg) == 0 {
                    TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            crate::diag::log("studio", "message loop exited cleanly");
            Ok(())
        }
    }

    /// Ctrl+O / Ctrl+S / Ctrl+K, handled before dialog-message
    /// processing so they work from any control.
    unsafe fn handle_accelerator(hwnd: HWND, msg: &MSG) -> bool {
        if msg.message != WM_KEYDOWN {
            return false;
        }
        if GetKeyState(VK_CONTROL_KEY) >= 0 {
            return false;
        }
        match msg.wParam as u32 {
            0x4F => {
                // Ctrl+O
                on_open_manifest(hwnd);
                true
            }
            0x53 => {
                // Ctrl+S
                on_save(hwnd, false);
                true
            }
            0x4B => {
                // Ctrl+K: jump to the files table (type to search it).
                let list = GetDlgItem(hwnd, ID_LIST as i32);
                if !list.is_null() {
                    SetFocus(list);
                }
                set_status(
                    hwnd,
                    StatusKind::Neutral,
                    "Files table focused — ↑↓ navigate · type to jump to a file",
                );
                true
            }
            _ => false,
        }
    }

    unsafe fn current_dpi(hwnd: HWND) -> u32 {
        let hdc = GetDC(hwnd);
        if hdc.is_null() {
            return 96;
        }
        let dpi = GetDeviceCaps(hdc, LOGPIXELSX_INDEX);
        ReleaseDC(hwnd, hdc);
        if dpi > 0 {
            dpi as u32
        } else {
            96
        }
    }

    unsafe fn create_fonts(dpi: u32) {
        if !BODY_FONT.is_null() {
            DeleteObject(BODY_FONT as _);
        }
        if !HEADER_FONT.is_null() {
            DeleteObject(HEADER_FONT as _);
        }
        let body_h = -scale(15, dpi);
        let header_h = -scale(14, dpi);
        BODY_FONT = CreateFontW(
            body_h,
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
        HEADER_FONT = CreateFontW(
            header_h,
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

    unsafe fn build_menu() -> HMENU {
        unsafe fn submenu(items: &[(&str, usize)]) -> HMENU {
            let menu = CreatePopupMenu();
            for (label, id) in items {
                if *label == "-" {
                    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                } else {
                    let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
                    AppendMenuW(menu, MF_STRING, *id, wide.as_ptr());
                }
            }
            menu
        }
        let bar = CreateMenu();
        let top = |bar: HMENU, label: &str, sub: HMENU| {
            let wide: Vec<u16> = label.encode_utf16().chain(std::iter::once(0)).collect();
            AppendMenuW(bar, MF_POPUP, sub as usize, wide.as_ptr());
        };
        top(
            bar,
            "File",
            submenu(&[
                ("Open Manifest…\tCtrl+O", IDM_FILE_OPEN),
                ("Save\tCtrl+S", IDM_FILE_SAVE),
                ("Save As…", IDM_FILE_SAVEAS),
                ("-", 0),
                ("Exit", IDM_FILE_EXIT),
            ]),
        );
        top(
            bar,
            "Project",
            submenu(&[
                ("Files Folder…", IDM_PROJ_FILES),
                ("Output Folder…", IDM_PROJ_OUT),
            ]),
        );
        top(
            bar,
            "Build",
            submenu(&[
                ("Validate Manifest", IDM_BUILD_VALIDATE),
                ("Build Setup (Pack)", IDM_BUILD_PACK),
                ("Verify Setup.exe", IDM_BUILD_VERIFY),
                ("Test Install (Sandbox Verify)", IDM_BUILD_TEST),
            ]),
        );
        top(
            bar,
            "Tools",
            submenu(&[
                ("Show Manifest Schema", IDM_TOOLS_SCHEMA),
                ("Open seanpak.log", IDM_TOOLS_LOG),
            ]),
        );
        top(
            bar,
            "Help",
            submenu(&[("About SeanPak Builder", IDM_HELP_ABOUT)]),
        );
        bar
    }

    unsafe fn create_controls(parent: HWND, instance: HINSTANCE) {
        let static_class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let button_class: Vec<u16> = "BUTTON\0".encode_utf16().collect();
        let edit_class: Vec<u16> = "EDIT\0".encode_utf16().collect();
        let list_class: Vec<u16> = "SysListView32\0".encode_utf16().collect();

        let mk = |class: &[u16], text: &str, style: u32, id: usize| -> HWND {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    wide.as_ptr(),
                    WS_CHILD | WS_VISIBLE | style,
                    0,
                    0,
                    10,
                    10,
                    parent,
                    id as HMENU,
                    instance,
                    std::ptr::null_mut(),
                )
            }
        };

        mk(&static_class, "FILES IN PACKAGE", 0, ID_LBL_FILES_HDR);
        let list = mk(
            &list_class,
            "",
            WS_TABSTOP | WS_BORDER | LVS_REPORT | LVS_SHOWSELALWAYS,
            ID_LIST,
        );
        SendMessageW(
            list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            (LVS_EX_GRIDLINES | LVS_EX_FULLROWSELECT) as WPARAM,
            (LVS_EX_GRIDLINES | LVS_EX_FULLROWSELECT) as LPARAM,
        );
        SendMessageW(list, LVM_SETBKCOLOR, 0, COLOR_SURFACE as LPARAM);
        SendMessageW(list, LVM_SETTEXTBKCOLOR, 0, COLOR_SURFACE as LPARAM);
        SendMessageW(list, LVM_SETTEXTCOLOR, 0, COLOR_FG as LPARAM);
        for (idx, (title, _width)) in [
            ("File", 380),
            ("Destination", 180),
            ("Size", 110),
            ("Packed (est.)", 120),
        ]
        .iter()
        .enumerate()
        {
            let mut wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                fmt: 0,
                cx: *_width,
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
        mk(
            &button_class,
            "Add files…",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            ID_BTN_ADD_FILES,
        );
        mk(
            &button_class,
            "Add folder…",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            ID_BTN_ADD_FOLDER,
        );
        mk(
            &button_class,
            "Remove",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            ID_BTN_REMOVE,
        );

        // Manifest panel.
        mk(&static_class, "APP MANIFEST", 0, ID_LBL_MANIFEST_HDR);
        let fields: [(&str, usize); 7] = [
            ("Name", ID_EDT_NAME),
            ("Version", ID_EDT_VERSION),
            ("Publisher", ID_EDT_PUBLISHER),
            ("Primary exe", ID_EDT_PRIMARY),
            ("Install location", ID_EDT_INSTALL_LOC),
            ("Start Menu folder", ID_EDT_SM_FOLDER),
            ("File associations", ID_EDT_ASSOC),
        ];
        for (i, (label, edit_id)) in fields.iter().enumerate() {
            mk(&static_class, label, 0, PANEL_LABELS[i]);
            mk(
                &edit_class,
                "",
                WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
                *edit_id,
            );
        }
        mk(
            &button_class,
            "Desktop shortcut",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            ID_CHK_DESKTOP,
        );
        mk(
            &button_class,
            "Start Menu",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            ID_CHK_STARTMENU,
        );
        mk(
            &button_class,
            "Add install folder to PATH",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            ID_CHK_PATH,
        );
        mk(&static_class, "PACKAGE", 0, ID_LBL_PACKAGE_HDR);
        mk(&static_class, "Files folder", 0, PANEL_LABELS[7]);
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
            ID_EDT_FILES_DIR,
        );
        mk(
            &button_class,
            "…",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            ID_BTN_FILES_DIR,
        );
        mk(&static_class, "Output folder", 0, PANEL_LABELS[8]);
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
            ID_EDT_OUT_DIR,
        );
        mk(
            &button_class,
            "…",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            ID_BTN_OUT_DIR,
        );
        mk(
            &static_class,
            "Every field here is the pack manifest — the same JSON `seanpak pack` reads. Associations are comma-separated (.txt, .md).",
            0,
            ID_LBL_PANEL_NOTE,
        );

        // Output strip.
        mk(&static_class, "OUTPUT", 0, ID_LBL_OUTPUT_HDR);
        mk(
            &button_class,
            "Validate",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            ID_BTN_VALIDATE,
        );
        mk(
            &button_class,
            "Verify",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            ID_BTN_VERIFY,
        );
        mk(
            &button_class,
            "Sandbox",
            WS_TABSTOP | BS_AUTOCHECKBOX,
            ID_CHK_SANDBOX,
        );
        mk(
            &edit_class,
            "",
            WS_BORDER | ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | WS_VSCROLL,
            ID_OUTPUT,
        );
        mk(&static_class, "", 0, ID_LBL_FACTS);

        // Footer.
        mk(&static_class, "Ready", 0, ID_LBL_STATUS);
        mk(
            &button_class,
            "Test install",
            WS_TABSTOP | (BS_PUSHBUTTON as u32),
            ID_BTN_TEST,
        );
        mk(
            &button_class,
            "Build Setup",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_BUILD,
        );

        create_tooltips(parent, instance);
    }

    unsafe fn create_tooltips(parent: HWND, instance: HINSTANCE) {
        let tips: [(usize, &str); 8] = [
            (ID_LIST, "Files that will be packed. Ctrl+K jumps here; type to find a file."),
            (ID_BTN_ADD_FILES, "Copy files into the package folder and add them to the package"),
            (ID_BTN_ADD_FOLDER, "Copy a folder (with everything in it) into the package"),
            (ID_BTN_REMOVE, "Remove the selected files from the package list — the files stay in the folder"),
            (ID_BTN_VALIDATE, "Check the manifest with the same rules `seanpak validate` uses"),
            (ID_BTN_VERIFY, "Prove the built Setup.exe (overlay, manifest, payloads)"),
            (ID_CHK_SANDBOX, "Verify by test-installing into a temp folder and checking for residue"),
            (ID_BTN_BUILD, "Pack Setup.exe + Portable.zip + SHA256SUMS.txt — the same operation as `seanpak pack`"),
        ];
        let tooltip = CreateWindowExW(
            WS_EX_TOPMOST,
            TOOLTIPS_CLASS,
            std::ptr::null(),
            WS_POPUP | TTS_ALWAYSTIP_STYLE,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            parent,
            std::ptr::null_mut(),
            instance,
            std::ptr::null_mut(),
        );
        if tooltip.is_null() {
            return;
        }
        with_state(|s| s.tooltip = tooltip as usize);
        for (id, text) in tips {
            let ctrl = GetDlgItem(parent, id as i32);
            if ctrl.is_null() {
                continue;
            }
            let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            let text_ptr = wide.as_mut_ptr();
            let ti = TTTOOLINFOW {
                cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
                uFlags: TTF_IDISHWND_FLAG | TTF_SUBCLASS_FLAG,
                hwnd: parent,
                uId: ctrl as usize,
                rect: RECT {
                    left: 0,
                    top: 0,
                    right: 0,
                    bottom: 0,
                },
                hinst: instance,
                lpszText: text_ptr,
                lParam: 0,
                lpReserved: std::ptr::null_mut(),
            };
            SendMessageW(tooltip, TTM_ADDTOOLW_MSG, 0, &ti as *const _ as LPARAM);
            with_state(|s| s.tip_texts.push(wide));
        }
    }

    unsafe fn apply_fonts(hwnd: HWND) {
        let header_ids = [
            ID_LBL_FILES_HDR,
            ID_LBL_MANIFEST_HDR,
            ID_LBL_OUTPUT_HDR,
            ID_LBL_PACKAGE_HDR,
        ];
        unsafe fn set_font(hwnd: HWND, id: usize, font: HFONT) {
            let ctrl = GetDlgItem(hwnd, id as i32);
            if !ctrl.is_null() {
                SendMessageW(ctrl, WM_SETFONT, font as WPARAM, 1);
            }
        }
        let all_ids: [usize; 40] = [
            ID_LIST,
            ID_BTN_ADD_FILES,
            ID_BTN_ADD_FOLDER,
            ID_BTN_REMOVE,
            ID_EDT_NAME,
            ID_EDT_VERSION,
            ID_EDT_PUBLISHER,
            ID_EDT_PRIMARY,
            ID_EDT_INSTALL_LOC,
            ID_EDT_SM_FOLDER,
            ID_EDT_ASSOC,
            ID_CHK_DESKTOP,
            ID_CHK_STARTMENU,
            ID_CHK_PATH,
            ID_EDT_FILES_DIR,
            ID_BTN_FILES_DIR,
            ID_EDT_OUT_DIR,
            ID_BTN_OUT_DIR,
            ID_OUTPUT,
            ID_BTN_VALIDATE,
            ID_BTN_VERIFY,
            ID_CHK_SANDBOX,
            ID_LBL_STATUS,
            ID_BTN_TEST,
            ID_BTN_BUILD,
            ID_LBL_FACTS,
            ID_LBL_PANEL_NOTE,
            PANEL_LABELS[0],
            PANEL_LABELS[1],
            PANEL_LABELS[2],
            PANEL_LABELS[3],
            PANEL_LABELS[4],
            PANEL_LABELS[5],
            PANEL_LABELS[6],
            PANEL_LABELS[7],
            PANEL_LABELS[8],
            ID_LBL_FILES_HDR,
            ID_LBL_MANIFEST_HDR,
            ID_LBL_OUTPUT_HDR,
            ID_LBL_PACKAGE_HDR,
        ];
        for id in all_ids {
            let font = if header_ids.contains(&id) {
                HEADER_FONT
            } else {
                BODY_FONT
            };
            set_font(hwnd, id, font);
        }
    }

    /// Positions every control from the client size and the current
    /// DPI scale: files table grows, manifest panel keeps its width,
    /// output strip and footer keep their heights.
    unsafe fn layout(hwnd: HWND) {
        let dpi = with_state(|s| s.dpi).unwrap_or(96);
        let s = |v: i32| scale(v, dpi);
        let mut rect: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut rect);
        let w = rect.right;
        let h = rect.bottom;
        if w <= 0 || h <= 0 {
            return;
        }
        let m = s(16);
        let footer_h = s(52);
        let strip_h = s(172);
        let panel_w = s(404);
        let footer_top = h - footer_h;
        let strip_y = footer_top - strip_h - s(8);
        let top_h = strip_y - m - m;
        let btn_row_h = s(30);

        unsafe fn mv(hwnd: HWND, id: usize, x: i32, y: i32, w: i32, h: i32) {
            let ctrl = GetDlgItem(hwnd, id as i32);
            if !ctrl.is_null() {
                MoveWindow(ctrl, x, y, w.max(0), h.max(0), 1);
            }
        }

        // Files table + its buttons.
        let list_w = w - panel_w - m * 3;
        mv(hwnd, ID_LBL_FILES_HDR, m, m, list_w, s(20));
        mv(
            hwnd,
            ID_LIST,
            m,
            m + s(26),
            list_w,
            top_h - s(26) - btn_row_h - s(8),
        );
        let btn_y = m + top_h - btn_row_h;
        mv(hwnd, ID_BTN_ADD_FILES, m, btn_y, s(130), btn_row_h);
        mv(
            hwnd,
            ID_BTN_ADD_FOLDER,
            m + s(138),
            btn_y,
            s(130),
            btn_row_h,
        );
        mv(hwnd, ID_BTN_REMOVE, m + s(276), btn_y, s(110), btn_row_h);
        // Column widths: path column absorbs the slack.
        let list = GetDlgItem(hwnd, ID_LIST as i32);
        if !list.is_null() {
            let fixed = s(180) + s(110) + s(120);
            SendMessageW(
                list,
                LVM_SETCOLUMNWIDTH,
                0,
                (list_w - fixed - s(20)).max(s(120)) as LPARAM,
            );
            SendMessageW(list, LVM_SETCOLUMNWIDTH, 1, s(180) as LPARAM);
            SendMessageW(list, LVM_SETCOLUMNWIDTH, 2, s(110) as LPARAM);
            SendMessageW(list, LVM_SETCOLUMNWIDTH, 3, s(120) as LPARAM);
        }

        // Manifest panel.
        let px = w - m - panel_w;
        let label_w = s(124);
        let edit_x = px + label_w + s(8);
        let edit_w = panel_w - label_w - s(8);
        mv(hwnd, ID_LBL_MANIFEST_HDR, px, m, panel_w, s(20));
        let mut row_y = m + s(28);
        let edit_ids = [
            ID_EDT_NAME,
            ID_EDT_VERSION,
            ID_EDT_PUBLISHER,
            ID_EDT_PRIMARY,
            ID_EDT_INSTALL_LOC,
            ID_EDT_SM_FOLDER,
            ID_EDT_ASSOC,
        ];
        for (i, edit_id) in edit_ids.iter().enumerate() {
            mv(hwnd, PANEL_LABELS[i], px, row_y + s(3), label_w, s(20));
            mv(hwnd, *edit_id, edit_x, row_y, edit_w, s(26));
            row_y += s(32);
        }
        mv(hwnd, ID_CHK_DESKTOP, px, row_y, panel_w / 2, s(26));
        mv(
            hwnd,
            ID_CHK_STARTMENU,
            px + panel_w / 2,
            row_y,
            panel_w / 2,
            s(26),
        );
        row_y += s(30);
        mv(hwnd, ID_CHK_PATH, px, row_y, panel_w, s(26));
        row_y += s(34);
        mv(hwnd, ID_LBL_PACKAGE_HDR, px, row_y, panel_w, s(20));
        row_y += s(26);
        // Files folder row (+ browse button).
        mv(hwnd, PANEL_LABELS[7], px, row_y + s(3), label_w, s(20));
        mv(hwnd, ID_EDT_FILES_DIR, edit_x, row_y, edit_w - s(36), s(26));
        mv(
            hwnd,
            ID_BTN_FILES_DIR,
            edit_x + edit_w - s(30),
            row_y,
            s(30),
            s(26),
        );
        row_y += s(32);
        mv(hwnd, PANEL_LABELS[8], px, row_y + s(3), label_w, s(20));
        mv(hwnd, ID_EDT_OUT_DIR, edit_x, row_y, edit_w - s(36), s(26));
        mv(
            hwnd,
            ID_BTN_OUT_DIR,
            edit_x + edit_w - s(30),
            row_y,
            s(30),
            s(26),
        );
        row_y += s(36);
        mv(hwnd, ID_LBL_PANEL_NOTE, px, row_y, panel_w, s(56));

        // Output strip.
        mv(hwnd, ID_LBL_OUTPUT_HDR, m, strip_y, s(200), s(20));
        let strip_btn_y = strip_y;
        mv(
            hwnd,
            ID_CHK_SANDBOX,
            w - m - s(330),
            strip_btn_y,
            s(110),
            s(24),
        );
        mv(
            hwnd,
            ID_BTN_VERIFY,
            w - m - s(212),
            strip_btn_y,
            s(96),
            s(26),
        );
        mv(
            hwnd,
            ID_BTN_VALIDATE,
            w - m - s(104),
            strip_btn_y,
            s(104),
            s(26),
        );
        let facts_w = s(300);
        mv(
            hwnd,
            ID_OUTPUT,
            m,
            strip_y + s(30),
            w - m * 2 - facts_w - s(12),
            strip_h - s(38),
        );
        mv(
            hwnd,
            ID_LBL_FACTS,
            w - m - facts_w,
            strip_y + s(34),
            facts_w,
            strip_h - s(42),
        );

        // Footer.
        mv(
            hwnd,
            ID_LBL_STATUS,
            m,
            footer_top + s(16),
            w - s(420),
            s(22),
        );
        mv(
            hwnd,
            ID_BTN_TEST,
            w - m - s(308),
            footer_top + s(10),
            s(140),
            s(32),
        );
        mv(
            hwnd,
            ID_BTN_BUILD,
            w - m - s(160),
            footer_top + s(10),
            s(160),
            s(32),
        );
    }

    // ---- small control helpers (wizard idioms) ----

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

    unsafe fn set_text(hwnd: HWND, id: usize, text: &str) {
        let ctrl = GetDlgItem(hwnd, id as i32);
        if !ctrl.is_null() {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            SetWindowTextW(ctrl, wide.as_ptr());
        }
    }

    unsafe fn get_check(hwnd: HWND, id: usize) -> bool {
        let ctrl = GetDlgItem(hwnd, id as i32);
        !ctrl.is_null() && SendMessageW(ctrl, BM_GETCHECK_MSG, 0, 0) as usize == BST_CHECKED_STATE
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

    unsafe fn set_enabled(hwnd: HWND, id: usize, enabled: bool) {
        let ctrl = GetDlgItem(hwnd, id as i32);
        if !ctrl.is_null() {
            EnableWindow(ctrl, if enabled { 1 } else { 0 });
        }
    }

    fn set_status(hwnd: HWND, kind: StatusKind, text: &str) {
        with_state(|s| s.status_kind = kind);
        unsafe { set_text(hwnd, ID_LBL_STATUS, text) };
    }

    fn set_facts(hwnd: HWND, text: &str) {
        unsafe { set_text(hwnd, ID_LBL_FACTS, text) };
    }

    fn default_facts() -> String {
        let codec = if cfg!(windows) {
            "Compression: Windows Compression API"
        } else {
            "Compression: built-in LZ codec"
        };
        format!("{codec}\r\nStatic CRT · Fully offline · No telemetry")
    }

    fn update_title(hwnd: HWND) {
        let title =
            with_state(|s| s.doc.title_text()).unwrap_or_else(|| "SeanPak Builder".to_string());
        unsafe {
            let wide: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
            SetWindowTextW(hwnd, wide.as_ptr());
        }
    }

    /// Appends one line to the output strip. The edit is read-only for
    /// typing but fully selectable/copyable.
    fn append_output(hwnd: HWND, line: &str) {
        unsafe {
            let edit = GetDlgItem(hwnd, ID_OUTPUT as i32);
            if edit.is_null() {
                return;
            }
            let len = GetWindowTextLengthW(edit);
            SendMessageW(edit, EM_SETSEL_MSG, len as WPARAM, len as LPARAM);
            let wide: Vec<u16> = format!("{line}\r\n")
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            SendMessageW(edit, EM_REPLACESEL_MSG, 0, wide.as_ptr() as LPARAM);
            SendMessageW(edit, EM_SCROLLCARET_MSG, 0, 0);
        }
    }

    fn mark_dirty(hwnd: HWND) {
        let changed = with_state(|s| {
            if s.suppress_dirty || s.doc.dirty {
                false
            } else {
                s.doc.dirty = true;
                true
            }
        })
        .unwrap_or(false);
        if changed {
            update_title(hwnd);
        }
    }

    // ---- document <-> controls ----

    /// Controls ← document (programmatic: never marks dirty).
    unsafe fn fill_controls(hwnd: HWND) {
        let snapshot = with_state(|s| {
            (
                s.doc.manifest.clone(),
                s.doc.files_dir.clone(),
                s.doc.out_dir.clone(),
            )
        });
        let Some((m, files_dir, out_dir)) = snapshot else {
            return;
        };
        with_state(|s| s.suppress_dirty = true);
        set_text(hwnd, ID_EDT_NAME, &m.app_name);
        set_text(hwnd, ID_EDT_VERSION, &m.version);
        set_text(hwnd, ID_EDT_PUBLISHER, &m.publisher);
        set_text(hwnd, ID_EDT_PRIMARY, m.primary_exe.as_deref().unwrap_or(""));
        set_text(
            hwnd,
            ID_EDT_INSTALL_LOC,
            m.install_location.as_deref().unwrap_or(""),
        );
        set_text(
            hwnd,
            ID_EDT_SM_FOLDER,
            m.start_menu_folder.as_deref().unwrap_or(""),
        );
        let assoc = m
            .file_associations
            .iter()
            .map(|e| format!(".{e}"))
            .collect::<Vec<_>>()
            .join(", ");
        set_text(hwnd, ID_EDT_ASSOC, &assoc);
        set_check(hwnd, ID_CHK_DESKTOP, m.desktop_shortcut);
        set_check(hwnd, ID_CHK_STARTMENU, m.start_menu_shortcut);
        set_check(hwnd, ID_CHK_PATH, m.path_entry);
        set_text(
            hwnd,
            ID_EDT_FILES_DIR,
            &files_dir
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        set_text(hwnd, ID_EDT_OUT_DIR, &out_dir.display().to_string());
        with_state(|s| s.suppress_dirty = false);
    }

    /// Controls → document. Called before every operation and on
    /// close, so what the user sees is always what gets packed.
    unsafe fn harvest_fields(hwnd: HWND) {
        let name = get_text(hwnd, ID_EDT_NAME);
        let version = get_text(hwnd, ID_EDT_VERSION);
        let publisher = get_text(hwnd, ID_EDT_PUBLISHER);
        let primary = get_text(hwnd, ID_EDT_PRIMARY);
        let install_loc = get_text(hwnd, ID_EDT_INSTALL_LOC);
        let sm_folder = get_text(hwnd, ID_EDT_SM_FOLDER);
        let assoc_raw = get_text(hwnd, ID_EDT_ASSOC);
        let desktop = get_check(hwnd, ID_CHK_DESKTOP);
        let start_menu = get_check(hwnd, ID_CHK_STARTMENU);
        let path_entry = get_check(hwnd, ID_CHK_PATH);
        let files_dir = get_text(hwnd, ID_EDT_FILES_DIR);
        let out_dir = get_text(hwnd, ID_EDT_OUT_DIR);
        with_state(|s| {
            let m = &mut s.doc.manifest;
            m.app_name = name.trim().to_string();
            m.version = version.trim().to_string();
            m.publisher = publisher.trim().to_string();
            let opt = |v: String| {
                let t = v.trim().to_string();
                if t.is_empty() {
                    None
                } else {
                    Some(t)
                }
            };
            m.primary_exe = opt(primary);
            m.install_location = opt(install_loc);
            m.start_menu_folder = opt(sm_folder);
            m.file_associations = assoc_raw
                .split(',')
                .map(|e| e.trim().trim_start_matches('.').to_lowercase())
                .filter(|e| !e.is_empty())
                .collect();
            m.desktop_shortcut = desktop;
            m.start_menu_shortcut = start_menu;
            m.path_entry = path_entry;
            let fd = files_dir.trim().to_string();
            s.doc.files_dir = if fd.is_empty() {
                None
            } else {
                Some(PathBuf::from(fd))
            };
            let od = out_dir.trim().to_string();
            if !od.is_empty() {
                s.doc.out_dir = PathBuf::from(od);
            }
        });
    }

    /// Rebuilds the files table from the document entries + last scan.
    unsafe fn fill_list(hwnd: HWND) {
        let list = GetDlgItem(hwnd, ID_LIST as i32);
        if list.is_null() {
            return;
        }
        let rows: Vec<FileRow> =
            with_state(|s| build_rows(&s.doc.entries, s.scan.as_ref())).unwrap_or_default();
        SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
        let mut display: Vec<[String; 4]> = rows
            .iter()
            .map(|r| {
                let shown = r.rel.replace('/', "\\");
                let dest = match r.rel.rfind('/') {
                    Some(idx) => format!("\\{}", r.rel[..idx].replace('/', "\\")),
                    None => "\\".to_string(),
                };
                let size = if r.on_disk {
                    format_bytes(r.size)
                } else {
                    "missing".to_string()
                };
                let packed = match r.packed {
                    Some(p) => format_bytes(p),
                    None => "—".to_string(),
                };
                [shown, dest, size, packed]
            })
            .collect();
        display.push([
            "Uninstall.exe (auto)".to_string(),
            "\\".to_string(),
            "—".to_string(),
            "—".to_string(),
        ]);
        for (row_idx, row) in display.iter().enumerate() {
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

    fn set_busy(hwnd: HWND, busy: bool) {
        with_state(|s| s.busy = busy);
        for id in [
            ID_BTN_BUILD,
            ID_BTN_TEST,
            ID_BTN_VALIDATE,
            ID_BTN_VERIFY,
            ID_BTN_ADD_FILES,
            ID_BTN_ADD_FOLDER,
            ID_BTN_REMOVE,
        ] {
            unsafe { set_enabled(hwnd, id, !busy) };
        }
    }

    fn is_busy() -> bool {
        with_state(|s| s.busy).unwrap_or(false)
    }

    /// Hands a finished job to the UI thread: stash the result, then
    /// post (never send) the job message.
    fn post_job(hwnd_value: usize, job: &Arc<Mutex<Option<JobResult>>>, result: JobResult) {
        if let Ok(mut slot) = job.lock() {
            *slot = Some(result);
        }
        unsafe {
            PostMessageW(hwnd_value as HWND, WM_APP_JOB, 0, 0);
        }
    }

    // ---- scanning ----

    fn start_scan(hwnd: HWND) {
        if is_busy() {
            return;
        }
        let Some((files_dir, job)) = with_state(|s| (s.doc.files_dir.clone(), s.job.clone()))
        else {
            return;
        };
        let Some(dir) = files_dir else {
            set_facts(hwnd, &default_facts());
            set_status(
                hwnd,
                StatusKind::Neutral,
                "Choose a files folder to see the package contents",
            );
            return;
        };
        set_busy(hwnd, true);
        set_status(hwnd, StatusKind::Neutral, "Scanning package folder…");
        let hwnd_value = hwnd as usize;
        std::thread::spawn(move || {
            let result =
                scan_package(&dir).map_err(|e| format!("Cannot scan {}: {e}", dir.display()));
            post_job(hwnd_value, &job, JobResult::Scan(result));
        });
    }

    unsafe fn on_scan_done(hwnd: HWND, result: Result<ScanOutcome, String>) {
        set_busy(hwnd, false);
        match result {
            Ok(scan) => {
                let (count, total, packed_total, missing) = with_state(|s| {
                    let dir_changed = s.entries_dir != s.doc.files_dir;
                    if s.doc.entries.is_empty() || dir_changed {
                        s.doc.entries = scan.all_files.clone();
                        s.entries_dir = s.doc.files_dir.clone();
                    }
                    if !s.pending_adds.is_empty() {
                        for add in s.pending_adds.drain(..) {
                            if !s.doc.entries.contains(&add) {
                                s.doc.entries.push(add);
                            }
                        }
                        s.doc.entries.sort();
                    }
                    s.doc.sync_files_field(&scan.all_files);
                    let rows = build_rows(&s.doc.entries, Some(&scan));
                    let missing = rows.iter().filter(|r| !r.on_disk).count();
                    let total: u64 = rows.iter().map(|r| r.size).sum();
                    let packed_total: u64 = rows.iter().filter_map(|r| r.packed).sum();
                    let count = rows.len();
                    s.scan = Some(scan);
                    (count, total, packed_total, missing)
                })
                .unwrap_or((0, 0, 0, 0));
                fill_list(hwnd);
                set_facts(
                    hwnd,
                    &format!(
                        "{count} files · {} total\r\nPacked estimate: {}\r\n{}",
                        format_bytes(total),
                        format_bytes(packed_total),
                        default_facts()
                    ),
                );
                if missing > 0 {
                    append_output(
                        hwnd,
                        &format!(
                            "Warning: {missing} file(s) in the manifest list are not in the files folder — pack will refuse until they are removed from the list or restored."
                        ),
                    );
                    set_status(
                        hwnd,
                        StatusKind::Error,
                        &format!("{missing} listed file(s) missing from the folder"),
                    );
                } else {
                    set_status(
                        hwnd,
                        StatusKind::Ok,
                        &format!("{count} files in the package"),
                    );
                }
            }
            Err(e) => {
                append_output(hwnd, &format!("Error: {e}"));
                set_status(
                    hwnd,
                    StatusKind::Error,
                    "Scan failed — see the output above",
                );
            }
        }
    }

    // ---- actions ----

    unsafe fn on_open_manifest(hwnd: HWND) {
        if is_busy() {
            return;
        }
        let Some(path) = crate::native_dialog::pick_open_file(
            "Open SeanPak manifest",
            "JSON files (*.json)",
            "*.json",
        ) else {
            return;
        };
        // Build the document by hand (not via open_manifest) so the
        // folder scan — with its size estimates — happens once, on
        // the window's worker, instead of blocking the open.
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                append_output(hwnd, &format!("Cannot open {}: {e}", path.display()));
                set_status(hwnd, StatusKind::Error, "That manifest could not be read");
                return;
            }
        };
        match AppManifest::from_json(&text) {
            Ok(manifest) => {
                let mut doc = StudioDocument::new_blank();
                doc.entries = manifest.files.clone();
                doc.files_dir = path
                    .parent()
                    .filter(|p| !p.as_os_str().is_empty())
                    .map(|p| p.to_path_buf());
                doc.out_dir = doc
                    .files_dir
                    .as_ref()
                    .map(|d| d.join("dist"))
                    .unwrap_or_else(|| PathBuf::from("dist"));
                doc.manifest = manifest;
                doc.manifest_path = Some(path.clone());
                let entries_dir = doc.files_dir.clone();
                with_state(|s| {
                    s.doc = doc;
                    s.scan = None;
                    s.entries_dir = entries_dir;
                    s.last_setup = None;
                });
                fill_controls(hwnd);
                update_title(hwnd);
                append_output(hwnd, &format!("Opened {}", path.display()));
                start_scan(hwnd);
            }
            Err(errors) => {
                append_output(hwnd, &format!("Cannot open {}:", path.display()));
                for e in &errors {
                    append_output(hwnd, &format!("  {e}"));
                }
                set_status(
                    hwnd,
                    StatusKind::Error,
                    "That manifest has problems — see the output above",
                );
            }
        }
    }

    unsafe fn on_save(hwnd: HWND, save_as: bool) {
        harvest_fields(hwnd);
        let existing = with_state(|s| s.doc.manifest_path.clone()).flatten();
        let path = match existing {
            Some(p) if !save_as => p,
            other => {
                let start = other
                    .as_ref()
                    .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                    .unwrap_or_else(|| "manifest.json".to_string());
                match crate::native_dialog::pick_save_file_as(
                    &start,
                    "Save SeanPak manifest",
                    "JSON files (*.json)",
                    "*.json",
                    "json",
                ) {
                    Some(p) => p,
                    None => return,
                }
            }
        };
        // Keep the stored file list in step with the table first.
        let all = with_state(|s| s.scan.as_ref().map(|sc| sc.all_files.clone())).flatten();
        if let Some(all) = all {
            with_state(|s| {
                s.doc.sync_files_field(&all);
            });
        }
        let result = with_state(|s| s.doc.save_to(&path));
        match result {
            Some(Ok(())) => {
                update_title(hwnd);
                set_status(hwnd, StatusKind::Ok, &format!("Saved {}", path.display()));
                append_output(hwnd, &format!("Saved manifest to {}", path.display()));
            }
            Some(Err(e)) => {
                append_output(hwnd, &format!("Error: {e}"));
                set_status(
                    hwnd,
                    StatusKind::Error,
                    "Save failed — see the output above",
                );
            }
            None => {}
        }
    }

    unsafe fn on_choose_files_dir(hwnd: HWND) {
        if is_busy() {
            return;
        }
        let Some(folder) = crate::native_dialog::pick_folder() else {
            return;
        };
        set_text(hwnd, ID_EDT_FILES_DIR, &folder.to_string_lossy());
        harvest_fields(hwnd);
        mark_dirty(hwnd);
        start_scan(hwnd);
    }

    unsafe fn on_choose_out_dir(hwnd: HWND) {
        let Some(folder) = crate::native_dialog::pick_folder() else {
            return;
        };
        set_text(hwnd, ID_EDT_OUT_DIR, &folder.to_string_lossy());
        harvest_fields(hwnd);
    }

    unsafe fn on_add_files(hwnd: HWND) {
        if is_busy() {
            return;
        }
        harvest_fields(hwnd);
        let files_dir = with_state(|s| s.doc.files_dir.clone()).flatten();
        let Some(files_dir) = files_dir else {
            append_output(hwnd, "Error: choose a files folder first (Project → Files Folder…), then add files to it.");
            set_status(hwnd, StatusKind::Error, "No files folder chosen yet");
            return;
        };
        let picks = crate::native_dialog::pick_open_files(
            "Add files to the package",
            "All files (*.*)",
            "*.*",
        );
        if picks.is_empty() {
            return;
        }
        let mut adds: Vec<String> = Vec::new();
        for src in &picks {
            if let Ok(rel) = src.strip_prefix(&files_dir) {
                adds.push(rel.to_string_lossy().replace('\\', "/"));
                continue;
            }
            match copy_file_into(src, &files_dir) {
                Ok(dest) => {
                    if let Some(name) = dest.file_name() {
                        adds.push(name.to_string_lossy().into_owned());
                    }
                }
                Err(e) => append_output(hwnd, &format!("Skipped {}: {e}", src.display())),
            }
        }
        if adds.is_empty() {
            set_status(hwnd, StatusKind::Neutral, "Nothing added");
            return;
        }
        with_state(|s| s.pending_adds = adds);
        mark_dirty(hwnd);
        start_scan(hwnd);
    }

    unsafe fn on_add_folder(hwnd: HWND) {
        if is_busy() {
            return;
        }
        harvest_fields(hwnd);
        let files_dir = with_state(|s| s.doc.files_dir.clone()).flatten();
        let Some(files_dir) = files_dir else {
            append_output(hwnd, "Error: choose a files folder first (Project → Files Folder…), then add a folder to it.");
            set_status(hwnd, StatusKind::Error, "No files folder chosen yet");
            return;
        };
        let Some(folder) = crate::native_dialog::pick_folder() else {
            return;
        };
        if folder.starts_with(&files_dir) || files_dir.starts_with(&folder) {
            append_output(hwnd, "Error: that folder overlaps the package folder — copying it into itself would never finish. Pick a folder outside the package folder.");
            set_status(
                hwnd,
                StatusKind::Error,
                "Folder overlaps the package folder",
            );
            return;
        }
        match copy_folder_into(&folder, &files_dir) {
            Ok(copied) => {
                append_output(
                    hwnd,
                    &format!(
                        "Copied {} file(s) from {} into the package folder.",
                        copied.len(),
                        folder.display()
                    ),
                );
                with_state(|s| s.pending_adds = copied);
                mark_dirty(hwnd);
                start_scan(hwnd);
            }
            Err(e) => {
                append_output(hwnd, &format!("Error: cannot copy that folder in: {e}"));
                set_status(
                    hwnd,
                    StatusKind::Error,
                    "Add folder failed — see the output above",
                );
            }
        }
    }

    unsafe fn on_remove_selected(hwnd: HWND) {
        if is_busy() {
            return;
        }
        let list = GetDlgItem(hwnd, ID_LIST as i32);
        if list.is_null() {
            return;
        }
        let mut indices: Vec<usize> = Vec::new();
        let mut next = -1isize;
        loop {
            next = SendMessageW(
                list,
                LVM_GETNEXTITEM,
                next as WPARAM,
                LVNI_SELECTED as LPARAM,
            );
            if next < 0 {
                break;
            }
            indices.push(next as usize);
        }
        let removed = with_state(|s| {
            let rows = build_rows(&s.doc.entries, s.scan.as_ref());
            let mut names: Vec<String> = Vec::new();
            for idx in &indices {
                // The trailing "Uninstall.exe (auto)" row is not an entry.
                if let Some(row) = rows.get(*idx) {
                    names.push(row.rel.clone());
                }
            }
            let before = s.doc.entries.len();
            s.doc.entries.retain(|e| !names.contains(e));
            before - s.doc.entries.len()
        })
        .unwrap_or(0);
        if removed == 0 {
            set_status(
                hwnd,
                StatusKind::Neutral,
                "Select a file in the table first",
            );
            return;
        }
        let all = with_state(|s| s.scan.as_ref().map(|sc| sc.all_files.clone())).flatten();
        if let Some(all) = all {
            with_state(|s| s.doc.sync_files_field(&all));
        }
        mark_dirty(hwnd);
        fill_list(hwnd);
        set_status(
            hwnd,
            StatusKind::Neutral,
            &format!(
                "Removed {removed} file(s) from the package list — the files stay in the folder"
            ),
        );
    }

    /// Harvest + sync + validate. On success returns the manifest to
    /// pack with; on failure prints every problem and returns None.
    unsafe fn validated_manifest(hwnd: HWND) -> Option<AppManifest> {
        harvest_fields(hwnd);
        let all = with_state(|s| s.scan.as_ref().map(|sc| sc.all_files.clone())).flatten();
        if let Some(all) = all {
            with_state(|s| s.doc.sync_files_field(&all));
        }
        match with_state(|s| s.doc.validate()) {
            Some(Ok(m)) => Some(m),
            Some(Err(errors)) => {
                append_output(hwnd, "Manifest has problems:");
                for e in &errors {
                    append_output(hwnd, &format!("  {e}"));
                }
                append_output(hwnd, "Fix the field named in each line, then try again.");
                set_status(hwnd, StatusKind::Error, "Manifest is not valid yet");
                None
            }
            None => None,
        }
    }

    /// When the files folder was typed rather than picked, it can
    /// differ from the folder the table was scanned from. Rescan
    /// first and report — packing a stale listing would silently
    /// package the wrong folder's contents. Returns true when a
    /// rescan was started (the caller should stop and let the user
    /// run its action again against the fresh listing).
    unsafe fn rescan_if_folder_changed(hwnd: HWND) -> bool {
        harvest_fields(hwnd);
        let stale = with_state(|s| s.doc.files_dir.is_some() && s.entries_dir != s.doc.files_dir)
            .unwrap_or(false);
        if stale {
            append_output(
                hwnd,
                "Files folder changed — rescanning so the table shows the new folder. Run the action again.",
            );
            start_scan(hwnd);
        }
        stale
    }

    unsafe fn on_validate(hwnd: HWND) {
        if is_busy() {
            return;
        }
        if rescan_if_folder_changed(hwnd) {
            return;
        }
        if let Some(m) = validated_manifest(hwnd) {
            append_output(
                hwnd,
                &format!(
                    "Manifest OK: {} {} — assets: {}, {}",
                    m.app_name,
                    m.version,
                    m.setup_file_name(),
                    m.portable_file_name()
                ),
            );
            set_status(hwnd, StatusKind::Ok, "Manifest is valid");
        }
    }

    unsafe fn on_pack(hwnd: HWND, force: bool) {
        if is_busy() {
            return;
        }
        if !force && rescan_if_folder_changed(hwnd) {
            return;
        }
        let Some(manifest) = validated_manifest(hwnd) else {
            return;
        };
        let Some((files_dir, out_dir, job)) = with_state(|s| {
            (
                s.doc.files_dir.clone(),
                s.doc.out_dir.clone(),
                s.job.clone(),
            )
        }) else {
            return;
        };
        let Some(files_dir) = files_dir else {
            append_output(
                hwnd,
                "Error: choose a files folder first (Project → Files Folder…).",
            );
            set_status(hwnd, StatusKind::Error, "No files folder chosen yet");
            return;
        };
        let stub = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                append_output(hwnd, &format!("Error: cannot locate own executable: {e}"));
                return;
            }
        };
        set_busy(hwnd, true);
        set_status(
            hwnd,
            StatusKind::Neutral,
            &format!("Packing {} {}…", manifest.app_name, manifest.version),
        );
        append_output(
            hwnd,
            &format!(
                "Packing {} {} from {} …",
                manifest.app_name,
                manifest.version,
                files_dir.display()
            ),
        );
        let hwnd_value = hwnd as usize;
        std::thread::spawn(move || {
            let result =
                crate::pack::pack(&manifest, &files_dir, &out_dir, &stub, force).map_err(|e| {
                    PackFail {
                        refused_overwrite: matches!(e, PackError::RefuseOverwrite(_)),
                        message: e.to_string(),
                    }
                });
            post_job(hwnd_value, &job, JobResult::Pack(result));
        });
    }

    unsafe fn on_pack_done(hwnd: HWND, result: Result<PackOutcome, PackFail>) {
        set_busy(hwnd, false);
        match result {
            Ok(outcome) => {
                crate::diag::record(
                    "pack",
                    vec![
                        ("source", jstr("studio")),
                        ("setup", jstr(outcome.setup.path.display().to_string())),
                        ("setupSha256", jstr(outcome.setup.sha256.clone())),
                        (
                            "portable",
                            jstr(outcome.portable.path.display().to_string()),
                        ),
                        ("portableSha256", jstr(outcome.portable.sha256.clone())),
                        ("stubBytes", JsonValue::Number(outcome.stub_bytes as f64)),
                    ],
                );
                append_output(
                    hwnd,
                    &format!(
                        "  {} ({} bytes, sha256 {})",
                        outcome.setup.path.display(),
                        outcome.setup.size,
                        outcome.setup.sha256
                    ),
                );
                append_output(
                    hwnd,
                    &format!(
                        "  {} ({} bytes, sha256 {})",
                        outcome.portable.path.display(),
                        outcome.portable.size,
                        outcome.portable.sha256
                    ),
                );
                append_output(hwnd, &format!("  {}", outcome.sums_path.display()));
                set_facts(
                    hwnd,
                    &format!(
                        "Stub size: {} · {} files packed\r\nPayload: {} · {}",
                        format_bytes(outcome.stub_bytes),
                        outcome.file_count,
                        format_bytes(outcome.total_payload_bytes),
                        default_facts()
                    ),
                );
                with_state(|s| s.last_setup = Some(outcome.setup.path.clone()));
                set_status(
                    hwnd,
                    StatusKind::Ok,
                    "Pack complete — Setup.exe is ready to verify",
                );
            }
            Err(fail) => {
                append_output(hwnd, &format!("Error: {}", fail.message));
                set_status(
                    hwnd,
                    StatusKind::Error,
                    "Pack failed — see the output above",
                );
                if fail.refused_overwrite {
                    let text: Vec<u16> = "An output file already exists with different contents.\n\nOverwrite it with the new package?"
                        .encode_utf16()
                        .chain(std::iter::once(0))
                        .collect();
                    let caption: Vec<u16> = "Build Setup\0".encode_utf16().collect();
                    let answer = MessageBoxW(
                        hwnd,
                        text.as_ptr(),
                        caption.as_ptr(),
                        MB_YESNO | MB_ICONWARNING,
                    );
                    if answer == IDYES {
                        on_pack(hwnd, true);
                    }
                }
            }
        }
    }

    /// Verify (or sandbox-verify) `target`, or ask for a Setup.exe
    /// when none is known yet.
    unsafe fn on_verify(hwnd: HWND, sandbox: bool) {
        if is_busy() {
            return;
        }
        let target = with_state(|s| s.last_setup.clone()).flatten();
        let path = match target {
            Some(p) if p.is_file() => p,
            _ => match crate::native_dialog::pick_open_file(
                "Select a built Setup.exe to verify",
                "Executable files (*.exe)",
                "*.exe",
            ) {
                Some(p) => p,
                None => return,
            },
        };
        let job = with_state(|s| s.job.clone());
        let Some(job) = job else { return };
        set_busy(hwnd, true);
        set_status(
            hwnd,
            StatusKind::Neutral,
            if sandbox {
                "Test-installing into a temp folder…"
            } else {
                "Verifying the package…"
            },
        );
        append_output(
            hwnd,
            &format!(
                "{} {} …",
                if sandbox {
                    "Sandbox-verifying"
                } else {
                    "Verifying"
                },
                path.display()
            ),
        );
        let hwnd_value = hwnd as usize;
        std::thread::spawn(move || {
            let (ok, lines) = if sandbox {
                match crate::pack::sandbox_verify(&path) {
                    Ok(report) => {
                        let mut lines: Vec<String> = report
                            .checks
                            .iter()
                            .map(|(name, passed, detail)| {
                                format!(
                                    "  [{}] {name}: {detail}",
                                    if *passed { "ok" } else { "FAIL" }
                                )
                            })
                            .collect();
                        if !report.leftovers.is_empty() {
                            lines.push(format!(
                                "  Leftovers after uninstall: {}",
                                report.leftovers.join(", ")
                            ));
                        }
                        lines.push(format!(
                            "Sandbox verify: {}",
                            if report.ok { "PASS" } else { "FAIL" }
                        ));
                        (report.ok, lines)
                    }
                    Err(e) => (false, vec![format!("Error: cannot sandbox-verify: {e}")]),
                }
            } else {
                match crate::pack::verify_package(&path) {
                    Ok(report) => {
                        let mut lines: Vec<String> = Vec::new();
                        if !report.app_name.is_empty() {
                            lines.push(format!(
                                "  App: {} {} (publisher {})",
                                report.app_name, report.app_version, report.publisher
                            ));
                        }
                        for (name, passed, detail) in &report.checks {
                            lines.push(format!(
                                "  [{}] {name}: {detail}",
                                if *passed { "ok" } else { "FAIL" }
                            ));
                        }
                        lines.push(format!(
                            "Verify: {}",
                            if report.ok { "PASS" } else { "FAIL" }
                        ));
                        (report.ok, lines)
                    }
                    Err(e) => (false, vec![format!("Error: cannot verify: {e}")]),
                }
            };
            post_job(
                hwnd_value,
                &job,
                JobResult::Verify {
                    sandbox,
                    path,
                    ok,
                    lines,
                },
            );
        });
    }

    unsafe fn on_verify_done(
        hwnd: HWND,
        sandbox: bool,
        path: PathBuf,
        ok: bool,
        lines: Vec<String>,
    ) {
        set_busy(hwnd, false);
        for line in &lines {
            append_output(hwnd, line);
        }
        crate::diag::record(
            if sandbox { "verify-sandbox" } else { "verify" },
            vec![
                ("source", jstr("studio")),
                ("path", jstr(path.display().to_string())),
                ("ok", JsonValue::Bool(ok)),
            ],
        );
        if ok {
            set_status(
                hwnd,
                StatusKind::Ok,
                if sandbox {
                    "Test install passed — install, compare, uninstall, no residue"
                } else {
                    "Verify passed — the package is sound"
                },
            );
        } else {
            set_status(
                hwnd,
                StatusKind::Error,
                "Verification failed — see the output above",
            );
        }
    }

    unsafe fn on_job_message(hwnd: HWND) {
        let taken = with_state(|s| s.job.lock().ok().and_then(|mut slot| slot.take())).flatten();
        match taken {
            Some(JobResult::Scan(result)) => on_scan_done(hwnd, result),
            Some(JobResult::Pack(result)) => on_pack_done(hwnd, result),
            Some(JobResult::Verify {
                sandbox,
                path,
                ok,
                lines,
            }) => on_verify_done(hwnd, sandbox, path, ok, lines),
            None => {}
        }
    }

    unsafe fn on_schema(hwnd: HWND) {
        append_output(
            hwnd,
            "Manifest schema (the same text `seanpak schema` prints):",
        );
        for line in MANIFEST_SCHEMA_JSON.lines() {
            append_output(hwnd, line);
        }
        set_status(
            hwnd,
            StatusKind::Neutral,
            "Schema shown in the output strip",
        );
    }

    unsafe fn on_open_log(hwnd: HWND) {
        let path = crate::diag::log_path();
        append_output(hwnd, &format!("Diagnostics log: {}", path.display()));
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        let file: Vec<u16> = path
            .display()
            .to_string()
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            hwnd,
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWDEFAULT,
        );
    }

    unsafe fn on_about(hwnd: HWND) {
        let text: Vec<u16> = format!(
            "SeanPak Builder {}\n\nThe studio is a view over the same operations the seanpak CLI runs — pack, validate, and verify behave identically here. Fully offline; nothing is sent anywhere.",
            env!("CARGO_PKG_VERSION")
        )
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
        let caption: Vec<u16> = "About SeanPak Builder\0".encode_utf16().collect();
        MessageBoxW(hwnd, text.as_ptr(), caption.as_ptr(), MB_OK);
    }

    /// Saves geometry + last-used paths. Called from WM_CLOSE, before
    /// the window is destroyed.
    unsafe fn save_settings(hwnd: HWND) {
        harvest_fields(hwnd);
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rect);
        let settings = with_state(|s| StudioSettings {
            window_rect: Some((
                rect.left,
                rect.top,
                (rect.right - rect.left).max(400),
                (rect.bottom - rect.top).max(300),
            )),
            manifest_path: s.doc.manifest_path.clone(),
            files_dir: s.doc.files_dir.clone(),
            out_dir: Some(s.doc.out_dir.clone()),
        });
        if let Some(settings) = settings {
            settings.save();
        }
    }

    /// Returns true when the window may close (running operations and
    /// unsaved changes are confirmed, never silently dropped).
    unsafe fn confirm_close(hwnd: HWND) -> bool {
        if is_busy() {
            let text: Vec<u16> = "A package operation is still running.\n\nClose anyway? The operation will be abandoned."
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let caption: Vec<u16> = "SeanPak Builder\0".encode_utf16().collect();
            if MessageBoxW(
                hwnd,
                text.as_ptr(),
                caption.as_ptr(),
                MB_YESNO | MB_ICONWARNING,
            ) != IDYES
            {
                return false;
            }
        }
        let dirty = with_state(|s| s.doc.dirty).unwrap_or(false);
        if dirty {
            let text: Vec<u16> = "The manifest has unsaved changes.\n\nClose without saving?"
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let caption: Vec<u16> = "SeanPak Builder\0".encode_utf16().collect();
            if MessageBoxW(
                hwnd,
                text.as_ptr(),
                caption.as_ptr(),
                MB_YESNO | MB_ICONWARNING,
            ) != IDYES
            {
                return false;
            }
        }
        true
    }

    unsafe fn draw_build_button(lparam: LPARAM) {
        let dis = lparam as *const DRAWITEMSTRUCT;
        if dis.is_null() {
            return;
        }
        let dis = &*dis;
        let disabled = dis.itemState & ODS_DISABLED != 0;
        let pressed = dis.itemState & ODS_SELECTED != 0;
        let brush = if disabled {
            SURFACE_BRUSH
        } else if pressed {
            GREEN_DARK_BRUSH
        } else {
            GREEN_BRUSH
        };
        FillRect(dis.hDC, &dis.rcItem, brush);
        let mut rc = dis.rcItem;
        let text: Vec<u16> = "Build Setup".encode_utf16().collect();
        SetBkMode(dis.hDC, TRANSPARENT as i32);
        SetTextColor(dis.hDC, if disabled { COLOR_MUTED } else { COLOR_FG });
        DrawTextW(
            dis.hDC,
            text.as_ptr(),
            text.len() as i32,
            &mut rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        if dis.itemState & ODS_FOCUS != 0 {
            let mut focus_rc = dis.rcItem;
            focus_rc.left += 3;
            focus_rc.top += 3;
            focus_rc.right -= 3;
            focus_rc.bottom -= 3;
            DrawFocusRect(dis.hDC, &focus_rc);
        }
    }

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
                let hdc = wparam as HDC;
                let ctrl = lparam as HWND;
                let id = GetWindowLongW(ctrl, GWL_ID) as usize;
                SetBkMode(hdc, TRANSPARENT as i32);
                if id == ID_LBL_STATUS {
                    let kind = with_state(|s| s.status_kind).unwrap_or(StatusKind::Neutral);
                    SetTextColor(
                        hdc,
                        match kind {
                            StatusKind::Ok => COLOR_GREEN,
                            StatusKind::Error => COLOR_DANGER,
                            StatusKind::Neutral => COLOR_FG,
                        },
                    );
                } else if id == ID_LBL_FILES_HDR
                    || id == ID_LBL_MANIFEST_HDR
                    || id == ID_LBL_OUTPUT_HDR
                    || id == ID_LBL_PACKAGE_HDR
                    || id == ID_LBL_FACTS
                    || id == ID_LBL_PANEL_NOTE
                    || PANEL_LABELS.contains(&id)
                {
                    SetTextColor(hdc, COLOR_MUTED);
                } else {
                    SetTextColor(hdc, COLOR_FG);
                }
                BG_BRUSH as LRESULT
            }
            WM_CTLCOLOREDIT => {
                let hdc = wparam as HDC;
                SetBkMode(hdc, OPAQUE as i32);
                SetBkColor(hdc, COLOR_SURFACE);
                SetTextColor(hdc, COLOR_FG);
                SURFACE_BRUSH as LRESULT
            }
            WM_DRAWITEM => {
                if wparam == ID_BTN_BUILD {
                    draw_build_button(lparam);
                    return 1;
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_COMMAND => {
                let control_id = wparam & 0xFFFF;
                let notify = (wparam >> 16) & 0xFFFF;
                match control_id {
                    IDM_FILE_OPEN => on_open_manifest(hwnd),
                    IDM_FILE_SAVE => on_save(hwnd, false),
                    IDM_FILE_SAVEAS => on_save(hwnd, true),
                    IDM_FILE_EXIT => {
                        SendMessageW(hwnd, WM_CLOSE, 0, 0);
                    }
                    IDM_PROJ_FILES => on_choose_files_dir(hwnd),
                    IDM_PROJ_OUT => on_choose_out_dir(hwnd),
                    IDM_BUILD_VALIDATE | ID_BTN_VALIDATE => on_validate(hwnd),
                    IDM_BUILD_PACK => on_pack(hwnd, false),
                    IDM_BUILD_VERIFY => {
                        let sandbox = get_check(hwnd, ID_CHK_SANDBOX);
                        on_verify(hwnd, sandbox);
                    }
                    IDM_BUILD_TEST => on_verify(hwnd, true),
                    IDM_TOOLS_SCHEMA => on_schema(hwnd),
                    IDM_TOOLS_LOG => on_open_log(hwnd),
                    IDM_HELP_ABOUT => on_about(hwnd),
                    ID_BTN_ADD_FILES => on_add_files(hwnd),
                    ID_BTN_ADD_FOLDER => on_add_folder(hwnd),
                    ID_BTN_REMOVE => on_remove_selected(hwnd),
                    ID_BTN_FILES_DIR => on_choose_files_dir(hwnd),
                    ID_BTN_OUT_DIR => on_choose_out_dir(hwnd),
                    ID_BTN_VERIFY => {
                        let sandbox = get_check(hwnd, ID_CHK_SANDBOX);
                        on_verify(hwnd, sandbox);
                    }
                    ID_BTN_TEST => on_verify(hwnd, true),
                    ID_BTN_BUILD => on_pack(hwnd, false),
                    ID_EDT_NAME | ID_EDT_VERSION | ID_EDT_PUBLISHER | ID_EDT_PRIMARY
                    | ID_EDT_INSTALL_LOC | ID_EDT_SM_FOLDER | ID_EDT_ASSOC | ID_EDT_FILES_DIR
                    | ID_EDT_OUT_DIR => {
                        if notify == EN_CHANGE_CODE {
                            mark_dirty(hwnd);
                        }
                    }
                    ID_CHK_DESKTOP | ID_CHK_STARTMENU | ID_CHK_PATH => {
                        mark_dirty(hwnd);
                    }
                    ID_CHK_SANDBOX => {
                        // A verify option, not a manifest field: no dirty.
                    }
                    _ => {}
                }
                0
            }
            m if m == WM_APP_JOB => {
                on_job_message(hwnd);
                0
            }
            WM_SIZE => {
                layout(hwnd);
                0
            }
            WM_GETMINMAXINFO => {
                let dpi = with_state(|s| s.dpi).unwrap_or(96);
                let mmi = lparam as *mut MINMAXINFO;
                if !mmi.is_null() {
                    (*mmi).ptMinTrackSize.x = scale(900, dpi);
                    (*mmi).ptMinTrackSize.y = scale(600, dpi);
                }
                0
            }
            m if m == WM_DPICHANGED_MSG => {
                let new_dpi = (wparam & 0xFFFF) as u32;
                if new_dpi > 0 {
                    with_state(|s| s.dpi = new_dpi);
                    create_fonts(new_dpi);
                    apply_fonts(hwnd);
                    let rect = lparam as *const RECT;
                    if !rect.is_null() {
                        SetWindowPos(
                            hwnd,
                            std::ptr::null_mut(),
                            (*rect).left,
                            (*rect).top,
                            (*rect).right - (*rect).left,
                            (*rect).bottom - (*rect).top,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                    }
                    layout(hwnd);
                }
                0
            }
            WM_CLOSE => {
                if confirm_close(hwnd) {
                    save_settings(hwnd);
                    DestroyWindow(hwnd);
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
}
