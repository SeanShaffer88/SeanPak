//! The SeanPak Builder studio (TesselGUI Design H — the IDE layout) —
//! the secondary, human-facing surface over the same operations the
//! CLI exposes.
//!
//! Layout (approved mockup `h-studio-ide`): a slim toolbar strip along
//! the top (Open / Save / Save As / Check details, with the
//! app name · version at its right), a files-directory listing pane
//! on the left, the dominant files detail table in the center, the
//! app-manifest editor on the right, and an output strip along the
//! bottom, in the shared TesselGUI 1.4.0 palette. Every action
//! calls a shared operation — pack is `pack::pack_with_hooks`, validate is
//! `manifest::AppManifest::from_json`, verify is `pack::verify_package_with`
//! / `pack::sandbox_verify_with`, and the file listing is
//! `scanner::ScannedFolder::scan_with` — so the studio can do nothing the
//! CLI cannot (see `docs/PARITY.md`). The worker operations take
//! the studio's cancel flag and progress sink; the CLI passes
//! neither, so its behavior is unchanged.
//!
//! This module's first half is the platform-independent document and
//! settings model (unit-tested on any host); the Win32 window itself is
//! Windows-only, like the wizard.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

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
    scan_package_with(files_dir, None)
}

/// Like [`scan_package`], with cooperative cancellation: the flag is
/// checked between files (the size estimate is the slow part), and a
/// set flag returns the shared `Interrupted` "scan cancelled" error
/// so the studio can keep showing the previous folder contents.
pub fn scan_package_with(
    files_dir: &Path,
    cancel: Option<&AtomicBool>,
) -> std::io::Result<ScanOutcome> {
    let scanned = ScannedFolder::scan_with(files_dir, cancel)?;
    let mut all_files: Vec<String> = Vec::with_capacity(scanned.files.len());
    let mut rows_by_rel: Vec<FileRow> = Vec::with_capacity(scanned.files.len());
    for f in &scanned.files {
        if crate::scanner::is_cancelled(cancel) {
            return Err(crate::scanner::cancelled_error("scan"));
        }
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

/// One row of the left pane's files-directory listing: a directory
/// (label ends with `/`, `rel` is `None`) or a file (label is its base
/// name, `rel` its package-relative path), with its indent depth.
#[derive(Debug, Clone, PartialEq)]
pub struct TreeEntry {
    pub label: String,
    pub depth: usize,
    pub rel: Option<String>,
}

/// Builds the left pane's listing from a files directory's contents
/// (package-relative, `/`-separated): each directory appears once, at
/// its first file, and files nest under their directories by depth.
/// Pure, so the shape of the listing is unit-tested.
pub fn tree_entries(all_files: &[String]) -> Vec<TreeEntry> {
    let mut sorted: Vec<&str> = all_files.iter().map(|f| f.as_str()).collect();
    sorted.sort_unstable();
    let mut out: Vec<TreeEntry> = Vec::new();
    let mut seen_dirs: Vec<String> = Vec::new();
    for file in sorted {
        let parts: Vec<&str> = file.split('/').filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            continue;
        }
        let mut prefix = String::new();
        for (i, part) in parts.iter().enumerate().take(parts.len() - 1) {
            if i > 0 {
                prefix.push('/');
            }
            prefix.push_str(part);
            if !seen_dirs.contains(&prefix) {
                seen_dirs.push(prefix.clone());
                out.push(TreeEntry {
                    label: format!("{part}/"),
                    depth: i,
                    rel: None,
                });
            }
        }
        out.push(TreeEntry {
            label: parts[parts.len() - 1].to_string(),
            depth: parts.len() - 1,
            rel: Some(file.to_string()),
        });
    }
    out
}

/// The manifest panel's text form of the `dataDirs` list:
/// semicolon-separated, the same separator the overlay block uses.
pub fn data_dirs_text(dirs: &[String]) -> String {
    dirs.join("; ")
}

/// Parses the panel's data-folders text back into the list: entries
/// are split on `;`, trimmed, and empties dropped. (Validation of the
/// entries themselves stays with the shared manifest rules, which run
/// on Validate / Save / Build exactly as for the CLI.)
pub fn parse_data_dirs(text: &str) -> Vec<String> {
    text.split(';')
        .map(|d| d.trim().to_string())
        .filter(|d| !d.is_empty())
        .collect()
}

/// The manifest panel's text form of the v6 power blocks: the whole
/// power object (components, hooks, registryWrites, envWrites,
/// shortcuts) as one pretty-printed JSON document — the same
/// encoding the manifest file and the package's v6 block use, so
/// what the pane shows is byte-for-byte what a hand-edited manifest
/// would carry. One editor for all five blocks is deliberate: the
/// blocks are heterogeneous (nested objects, enums, lists of files),
/// and five structured mini-editors would not fit the pane; the JSON
/// is the project's one hand-rolled model, validated by the same
/// parser the CLI's manifest reader uses.
pub fn power_blocks_text(power: &crate::overlay::PowerSpec) -> String {
    render_pretty(&power.to_json_value())
}

/// Parses the pane's power-blocks text back into a [`PowerSpec`].
/// Empty text means "no power blocks". Anything else must be valid
/// JSON carrying only the five known blocks — the same strictness
/// the manifest reader applies (unknown keys and bad shapes are
/// errors naming the field).
pub fn parse_power_blocks(text: &str) -> Result<crate::overlay::PowerSpec, Vec<String>> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(crate::overlay::PowerSpec::default());
    }
    let value = crate::manifest::parse_json(trimmed)
        .map_err(|e| vec![format!("power blocks are not valid JSON: {e}")])?;
    crate::manifest::power_spec_from_json(&value)
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
        let manifest = AppManifest::from_json(
            r#"{"appName":"T","version":"1.0","fileAssociations":["txt"],"launchAfterInstall":{"offered":true,"defaultChecked":false},"startAtLogin":true,"dataDirs":["%APPDATA%/T"]}"#,
        )
        .unwrap();
        assert!(manifest.launch_after_install_offered);
        assert!(!manifest.launch_after_install_default);
        assert!(manifest.start_at_login_offered);
        assert!(manifest.start_at_login_default);
        assert_eq!(manifest.data_dirs, vec!["%APPDATA%/T".to_string()]);
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
    fn tree_entries_nest_files_under_their_directories() {
        let files = vec![
            "docs/cheatsheet.pdf".to_string(),
            "ai_md.exe".to_string(),
            "docs/deep/x.txt".to_string(),
            "README.md".to_string(),
        ];
        let tree = tree_entries(&files);
        let shape: Vec<(&str, usize, bool)> = tree
            .iter()
            .map(|e| (e.label.as_str(), e.depth, e.rel.is_some()))
            .collect();
        assert_eq!(
            shape,
            vec![
                ("README.md", 0, true),
                ("ai_md.exe", 0, true),
                ("docs/", 0, false),
                ("cheatsheet.pdf", 1, true),
                ("deep/", 1, false),
                ("x.txt", 2, true),
            ]
        );
        // A file's rel is its full package path.
        assert_eq!(tree[3].rel.as_deref(), Some("docs/cheatsheet.pdf"));
        assert!(tree_entries(&[]).is_empty());
    }

    #[test]
    fn power_blocks_text_round_trips() {
        let manifest = AppManifest::from_json(
            r#"{
                "appName": "Power App",
                "version": "1.0.0",
                "components": [
                    {"id": "extras", "name": "Extras", "files": ["extras.dll"], "defaultSelected": true}
                ],
                "hooks": {"postInstall": [{"command": "app.exe", "args": ["--setup"], "onFailure": "continue"}]},
                "registryWrites": [
                    {"key": "Software\\Power App", "name": "Mode", "type": "sz", "value": "on"}
                ],
                "envWrites": [{"name": "POWER_APP_HOME", "value": "%INSTALLDIR%"}],
                "shortcuts": [
                    {"name": "Power App", "target": "app.exe", "location": "desktop", "pin": true}
                ]
            }"#,
        )
        .unwrap();
        let power = manifest.to_power_spec();
        let text = power_blocks_text(&power);
        let parsed = parse_power_blocks(&text).unwrap();
        assert_eq!(parsed, power);
        // Empty text is the empty spec; unknown blocks are errors.
        assert_eq!(
            parse_power_blocks("  ").unwrap(),
            crate::overlay::PowerSpec::default()
        );
        assert!(parse_power_blocks(r#"{"componentz": []}"#).is_err());
        assert!(parse_power_blocks("{not json").is_err());
    }

    #[test]
    fn data_dirs_text_round_trips() {
        let dirs = vec![
            "%APPDATA%/My App".to_string(),
            "%LOCALAPPDATA%/My App/Cache".to_string(),
        ];
        let text = data_dirs_text(&dirs);
        assert_eq!(parse_data_dirs(&text), dirs);
        assert_eq!(parse_data_dirs(""), Vec::<String>::new());
        assert_eq!(
            parse_data_dirs(" %APPDATA%/A ;; %USERPROFILE%/B "),
            vec!["%APPDATA%/A".to_string(), "%USERPROFILE%/B".to_string()]
        );
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
    fn scan_package_with_cancel_flag_stops() {
        let root = unique_temp("scancel");
        std::fs::write(root.join("a.txt"), b"aaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let cancel = AtomicBool::new(true);
        let err = scan_package_with(&root, Some(&cancel)).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::Interrupted);
        let cancel = AtomicBool::new(false);
        assert!(scan_package_with(&root, Some(&cancel)).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }
}

#[cfg(windows)]
mod imp {
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::Graphics::Gdi::*;
    use windows_sys::Win32::UI::Controls::*;
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        EnableWindow, GetKeyState, IsWindowEnabled, SetFocus,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    use super::{
        build_rows, data_dirs_text, parse_data_dirs, parse_power_blocks, power_blocks_text,
        scan_package_with, tree_entries, FileRow, ScanOutcome, StudioDocument, StudioSettings,
    };
    use crate::manifest::{jstr, AppManifest, JsonValue, MANIFEST_SCHEMA_JSON};
    use crate::pack::{PackError, PackOutcome};
    use crate::scanner::format_bytes;

    // Colors come from the shared TesselGUI 1.4.0 theme module —
    // this window keeps no private palette.
    use crate::theme;

    // Controls.
    const ID_LIST: usize = 100;
    const ID_BTN_PROGRAM_FOLDER: usize = 101;
    const ID_BTN_REMOVE: usize = 103;
    const ID_BTN_CANCEL: usize = 104;
    const ID_BTN_OPEN_OUTPUT: usize = 105;
    const ID_BTN_PREVIEW: usize = 106;
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
    const ID_LBL_PACKAGE_HDR: usize = 165;
    const ID_FOOTER_SEP: usize = 166;
    // IDE layout (Design H): toolbar strip, left files pane, center
    // totals line, and the install-options fields in the manifest pane.
    const ID_BTN_TB_OPEN: usize = 180;
    const ID_BTN_TB_SAVE: usize = 181;
    const ID_BTN_TB_SAVEAS: usize = 182;
    const ID_BTN_TB_VALIDATE: usize = 184;
    const ID_LBL_APPVER: usize = 185;
    const ID_LBL_FILES_PANE_HDR: usize = 186;
    const ID_TREE: usize = 187;
    const ID_LBL_TOTALS: usize = 190;
    const ID_CHK_LAUNCH_OFFER: usize = 191;
    const ID_CHK_LAUNCH_DEFAULT: usize = 192;
    const ID_CHK_LOGIN_OFFER: usize = 193;
    const ID_CHK_LOGIN_DEFAULT: usize = 194;
    const ID_EDT_DATA_DIRS: usize = 195;
    const ID_LBL_LAUNCH: usize = 196;
    const ID_LBL_LOGIN: usize = 197;
    const ID_LBL_DATADIRS: usize = 198;
    const ID_EDT_POWER: usize = 199;
    const ID_LBL_POWER: usize = 200;
    // Field labels inside the manifest panel (colored as one group).
    const PANEL_LABELS: [usize; 9] = [170, 171, 172, 173, 174, 175, 176, 177, 178];
    // Every checkbox in the window (all owner-drawn; a click toggles
    // the stored check state — see WM_COMMAND).
    const CHECKBOX_IDS: [usize; 8] = [
        ID_CHK_DESKTOP,
        ID_CHK_STARTMENU,
        ID_CHK_PATH,
        ID_CHK_LAUNCH_OFFER,
        ID_CHK_LAUNCH_DEFAULT,
        ID_CHK_LOGIN_OFFER,
        ID_CHK_LOGIN_DEFAULT,
        ID_CHK_SANDBOX,
    ];

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
    const IDM_BUILD_PREVIEW: usize = 9025;
    const IDM_TOOLS_SCHEMA: usize = 9031;
    const IDM_TOOLS_LOG: usize = 9032;
    const IDM_HELP_ABOUT: usize = 9041;

    // Messages / styles windows-sys keeps behind features this crate
    // does not enable; defined locally like the wizard does.
    const WM_APP_JOB: u32 = 0x8000 + 21;
    const WM_APP_PROGRESS: u32 = 0x8000 + 22;
    const WM_DPICHANGED_MSG: u32 = 0x02E0;
    const EM_SETSEL_MSG: u32 = 0x00B1;
    const EM_REPLACESEL_MSG: u32 = 0x00C2;
    const EM_SCROLLCARET_MSG: u32 = 0x00B7;
    const TTM_ADDTOOLW_MSG: u32 = 0x0400 + 50;
    const TTM_UPDATETIPTEXTW_MSG: u32 = 0x0400 + 57;
    const TTM_RELAYEVENT_MSG: u32 = 0x0407;
    const TTS_ALWAYSTIP_STYLE: u32 = 0x01;
    const TTF_IDISHWND_FLAG: u32 = 0x0001;
    const TTF_SUBCLASS_FLAG: u32 = 0x0010;
    const BS_OWNERDRAW: u32 = 0x000B;
    // BN_CLICKED (the notification owner-drawn checkboxes toggle on).
    const BN_CLICKED_CODE: usize = 0;
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
    // LVN_ITEMCHANGED is (-101); NMHDR.code is unsigned, compare as i32.
    const LVN_ITEMCHANGED_I32: i32 = -101;
    // Right-aligned static text (windows-sys keeps the SS_ styles
    // behind a feature this crate does not enable).
    const SS_RIGHT: u32 = 0x0002;

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

    /// Which worker operation is running (or ran last): drives the
    /// status line's wording and the cancelled-job report.
    #[derive(Clone, Copy, PartialEq)]
    enum JobKind {
        Scan,
        Pack,
        Verify,
        TestInstall,
    }

    impl JobKind {
        /// The status line while the job runs (progress counts are
        /// appended by `progress_text` when the operation has them).
        fn start_text(self) -> &'static str {
            match self {
                JobKind::Scan => "Scanning the program folder… (Cancel stops it)",
                JobKind::Pack => "Building Setup… (Cancel stops it)",
                JobKind::Verify => "Verifying package… (Cancel stops it)",
                JobKind::TestInstall => "Test-installing… (Cancel stops it)",
            }
        }

        fn progress_text(self, done: usize, total: usize) -> String {
            match self {
                JobKind::Pack => {
                    format!("Building Setup… — file {done} of {total} (Cancel stops it)")
                }
                JobKind::Verify => {
                    format!("Verifying package… — file {done} of {total} (Cancel stops it)")
                }
                _ => self.start_text().to_string(),
            }
        }
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
        /// The user cancelled the job via the shared cancel flag;
        /// the operation has already undone its partial outputs.
        Cancelled(JobKind),
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
        busy: bool,
        job: Arc<Mutex<Option<JobResult>>>,
        /// The running job's cancel flag (a fresh one per job; the
        /// Cancel button sets it, the operations poll it).
        cancel: Arc<AtomicBool>,
        /// The running job's latest progress as (files done, total),
        /// written by the worker and read on WM_APP_PROGRESS.
        progress: Arc<Mutex<Option<(usize, usize)>>>,
        /// The kind of job currently running (status wording).
        job_kind: JobKind,
        last_setup: Option<PathBuf>,
        dpi: u32,
        suppress_dirty: bool,
        status_kind: StatusKind,
        tooltip: usize,
        /// Kept alive for the tooltip tool infos.
        tip_texts: Vec<Vec<u16>>,
        /// Left-pane listing rows → package-relative file path
        /// (`None` for directory rows), mirroring the tree control.
        tree_rels: Vec<Option<String>>,
        /// The widest left-pane row in pixels, so the listing's one
        /// column is at least that wide and long names can be
        /// scrolled to in full.
        tree_col_w: i32,
        /// The file the totals line describes (selected in either the
        /// center table or the left listing).
        selected_rel: Option<String>,
        /// The power-blocks parse error last reported to the output
        /// strip, so a harvest that runs on every operation does not
        /// repeat the same complaint; cleared when the text parses.
        power_error_reported: Option<String>,
    }

    static STATE: Mutex<Option<StudioState>> = Mutex::new(None);
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
                busy: false,
                job: Arc::new(Mutex::new(None)),
                cancel: Arc::new(AtomicBool::new(false)),
                progress: Arc::new(Mutex::new(None)),
                job_kind: JobKind::Scan,
                last_setup: None,
                dpi: 96,
                suppress_dirty: true,
                status_kind: StatusKind::Neutral,
                tooltip: 0,
                tip_texts: Vec::new(),
                tree_rels: Vec::new(),
                tree_col_w: 0,
                selected_rel: None,
                power_error_reported: None,
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
                hbrBackground: theme::brush(theme::BACKGROUND),
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
                ("Choose Program Folder…", IDM_PROJ_FILES),
                ("Output Folder…", IDM_PROJ_OUT),
            ]),
        );
        top(
            bar,
            "Build",
            submenu(&[
                ("Check Details", IDM_BUILD_VALIDATE),
                ("Build Setup", IDM_BUILD_PACK),
                ("Verify Package", IDM_BUILD_VERIFY),
                ("Test Install", IDM_BUILD_TEST),
                ("Preview Installer", IDM_BUILD_PREVIEW),
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

        // Toolbar strip (Design H): the everyday commands as buttons
        // over the same handlers the menus run, plus the app · version
        // readout at the right.
        mk(
            &button_class,
            "Open…",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_TB_OPEN,
        );
        mk(
            &button_class,
            "Save",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_TB_SAVE,
        );
        mk(
            &button_class,
            "Save As…",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_TB_SAVEAS,
        );
        mk(
            &button_class,
            "Check details",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_TB_VALIDATE,
        );
        mk(&static_class, "", SS_RIGHT, ID_LBL_APPVER);

        // Left pane: the files directory listing (directories and
        // files, indented). Selecting a file here selects it in the
        // center table too.
        mk(&static_class, "PROGRAM FOLDER", 0, ID_LBL_FILES_PANE_HDR);
        let tree = mk(
            &list_class,
            "",
            WS_TABSTOP
                | WS_BORDER
                | LVS_REPORT
                | LVS_SINGLESEL
                | LVS_SHOWSELALWAYS
                | LVS_NOCOLUMNHEADER,
            ID_TREE,
        );
        SendMessageW(
            tree,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            (LVS_EX_FULLROWSELECT) as WPARAM,
            (LVS_EX_FULLROWSELECT) as LPARAM,
        );
        SendMessageW(tree, LVM_SETBKCOLOR, 0, theme::SURFACE as LPARAM);
        SendMessageW(tree, LVM_SETTEXTBKCOLOR, 0, theme::SURFACE as LPARAM);
        SendMessageW(tree, LVM_SETTEXTCOLOR, 0, theme::FOREGROUND as LPARAM);
        {
            let mut wide: Vec<u16> = "Name".encode_utf16().chain(std::iter::once(0)).collect();
            let col = LVCOLUMNW {
                mask: LVCF_TEXT | LVCF_WIDTH,
                fmt: 0,
                cx: 240,
                pszText: wide.as_mut_ptr(),
                cchTextMax: 0,
                iSubItem: 0,
                iImage: 0,
                iOrder: 0,
                cxMin: 0,
                cxDefault: 0,
                cxIdeal: 0,
            };
            SendMessageW(tree, LVM_INSERTCOLUMNW, 0, &col as *const _ as LPARAM);
        }

        mk(&static_class, "FILES IN PACKAGE", 0, ID_LBL_FILES_HDR);
        let list = mk(
            &list_class,
            "",
            WS_TABSTOP | WS_BORDER | LVS_REPORT | LVS_SHOWSELALWAYS,
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
        // The header is painted by a subclass on the header control
        // itself, in theme colors (owner-draw never painted — see
        // theme::subclass_list_header for the diagnosis).
        theme::subclass_list_header(list);
        // One door into the package: choose the folder the built
        // program lives in. (The old Add files / Add folder buttons
        // copied files into that folder and dead-ended with an error
        // until a folder was chosen — removed in 4.7.2.)
        mk(
            &button_class,
            "Program Folder…",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_PROGRAM_FOLDER,
        );
        mk(
            &button_class,
            "Remove selected",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_REMOVE,
        );
        // Totals / selection line under the center table.
        mk(&static_class, "", 0, ID_LBL_TOTALS);

        // Manifest panel.
        mk(&static_class, "PROGRAM DETAILS", 0, ID_LBL_MANIFEST_HDR);
        let fields: [(&str, usize); 7] = [
            ("Name", ID_EDT_NAME),
            ("Version", ID_EDT_VERSION),
            ("Publisher", ID_EDT_PUBLISHER),
            ("Main program file", ID_EDT_PRIMARY),
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
            WS_TABSTOP | BS_OWNERDRAW,
            ID_CHK_DESKTOP,
        );
        mk(
            &button_class,
            "Start Menu",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_CHK_STARTMENU,
        );
        mk(
            &button_class,
            "Add install folder to PATH",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_CHK_PATH,
        );
        // The 4.4.0 install options: each offer is a pair — offer the
        // option in the installer at all, and whether it starts on.
        mk(&static_class, "Launch after install", 0, ID_LBL_LAUNCH);
        mk(
            &button_class,
            "Offer",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_CHK_LAUNCH_OFFER,
        );
        mk(
            &button_class,
            "On by default",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_CHK_LAUNCH_DEFAULT,
        );
        mk(&static_class, "Start at login", 0, ID_LBL_LOGIN);
        mk(
            &button_class,
            "Offer",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_CHK_LOGIN_OFFER,
        );
        mk(
            &button_class,
            "On by default",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_CHK_LOGIN_DEFAULT,
        );
        mk(&static_class, "Data folders", 0, ID_LBL_DATADIRS);
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
            ID_EDT_DATA_DIRS,
        );
        // The v6 power blocks as one JSON editor (see
        // power_blocks_text for why one editor, not five grids).
        mk(&static_class, "Advanced options (JSON)", 0, ID_LBL_POWER);
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_MULTILINE | ES_AUTOVSCROLL | WS_VSCROLL,
            ID_EDT_POWER,
        );
        mk(&static_class, "PACKAGE", 0, ID_LBL_PACKAGE_HDR);
        mk(&static_class, "Program folder", 0, PANEL_LABELS[7]);
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL,
            ID_EDT_FILES_DIR,
        );
        mk(
            &button_class,
            "Browse…",
            WS_TABSTOP | BS_OWNERDRAW,
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
            "Browse…",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_OUT_DIR,
        );

        // Output strip.
        mk(&static_class, "OUTPUT", 0, ID_LBL_OUTPUT_HDR);
        mk(
            &button_class,
            "Check details",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_VALIDATE,
        );
        mk(
            &button_class,
            "Verify package",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_VERIFY,
        );
        mk(
            &button_class,
            "Also test-install",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_CHK_SANDBOX,
        );
        mk(
            &edit_class,
            "",
            WS_BORDER | ES_MULTILINE | ES_READONLY | ES_AUTOVSCROLL | WS_VSCROLL,
            ID_OUTPUT,
        );
        mk(&static_class, "", 0, ID_LBL_FACTS);

        // Footer: a 1px edge separator above the status line.
        mk(&static_class, "", 0, ID_FOOTER_SEP);
        mk(&static_class, "Ready", 0, ID_LBL_STATUS);
        mk(
            &button_class,
            "Open output folder",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_OPEN_OUTPUT,
        );
        mk(
            &button_class,
            "Preview installer",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_PREVIEW,
        );
        mk(
            &button_class,
            "Test install",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_TEST,
        );
        mk(
            &button_class,
            "Build Setup",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_BUILD,
        );
        // Cancel is enabled only while a job runs (set_busy owns its
        // state from here on).
        let cancel_btn = mk(
            &button_class,
            "Cancel",
            WS_TABSTOP | BS_OWNERDRAW,
            ID_BTN_CANCEL,
        );
        EnableWindow(cancel_btn, 0);

        create_tooltips(parent, instance);
    }

    unsafe fn create_tooltips(parent: HWND, instance: HINSTANCE) {
        // One short plain-English line per main control — what it
        // does, in the words a first-time user would use. ("Program",
        // not "payload"; "Setup file", not "artifact".)
        let tips: [(usize, &str); 36] = [
            (ID_LIST, "The files that will be packed into the Setup file. Select files and click Remove selected to leave them out"),
            (ID_TREE, "Everything in the program folder, folders included. Selecting a file here selects it in the table too"),
            (ID_BTN_TB_OPEN, "Open a manifest file — the file that describes your program"),
            (ID_BTN_TB_SAVE, "Save the manifest file"),
            (ID_BTN_TB_SAVEAS, "Save the manifest file under a new name"),
            (ID_BTN_TB_VALIDATE, "Check the program details for mistakes before building"),
            (ID_BTN_PROGRAM_FOLDER, "Choose the folder your built program is in — everything in it becomes the package"),
            (ID_BTN_REMOVE, "Remove the selected files from the package — the files themselves stay in the folder"),
            (ID_EDT_NAME, "Your program's name — shown in the installer and used in the Setup file name"),
            (ID_EDT_VERSION, "Your program's version, like 1.2.0 — used in the Setup file name"),
            (ID_EDT_PUBLISHER, "Your name or your company — shown in the installer"),
            (ID_EDT_PRIMARY, "The main .exe file in the program folder — the one shortcuts point to and the installer can launch"),
            (ID_EDT_INSTALL_LOC, "Where the program installs on a user's computer — leave empty for the usual per-user folder"),
            (ID_EDT_SM_FOLDER, "The Start Menu folder the shortcuts go in — leave empty to use the program name"),
            (ID_EDT_ASSOC, "File types this program opens, comma-separated, like .txt, .md — Windows will offer your program for them"),
            (ID_CHK_DESKTOP, "Put a shortcut on the user's desktop"),
            (ID_CHK_STARTMENU, "Put a shortcut in the Start Menu"),
            (ID_CHK_PATH, "Add the install folder to the user's PATH, so the program can be run from any command line"),
            (ID_CHK_LAUNCH_OFFER, "Offer: show a 'Launch the program' tick box on the installer's last page"),
            (ID_CHK_LAUNCH_DEFAULT, "On by default: that 'Launch the program' box starts out ticked"),
            (ID_CHK_LOGIN_OFFER, "Offer: show a 'Start the program when Windows starts' tick box in the installer"),
            (ID_CHK_LOGIN_DEFAULT, "On by default: that 'Start at login' box starts out ticked"),
            (ID_EDT_DATA_DIRS, "The program's own data folders, semicolon-separated, like %APPDATA%/My App. Uninstall asks before deleting them, and the box starts unticked"),
            (ID_EDT_POWER, "Advanced extras as one JSON block: optional components, install hooks (commands run during install), registry and environment values the installer writes and later restores, and extra shortcuts. Leave it as {} if you don't need any of them"),
            (ID_EDT_FILES_DIR, "The folder your built program is in — everything in it is packed into the Setup file"),
            (ID_BTN_FILES_DIR, "Browse for the program folder"),
            (ID_EDT_OUT_DIR, "The folder the finished Setup file, portable .zip, and SHA256SUMS.txt are written to"),
            (ID_BTN_OUT_DIR, "Browse for the output folder"),
            (ID_BTN_VALIDATE, "Check the program details for mistakes — the same check the build runs first"),
            (ID_BTN_VERIFY, "Check a built Setup file: read it back and prove every file inside is intact"),
            (ID_CHK_SANDBOX, "Also test-install: verifying first installs into a temp folder, checks every file, then uninstalls and checks nothing is left behind"),
            (ID_BTN_TEST, "Install the built Setup file into a temp folder, check every file, then uninstall and check nothing is left"),
            (ID_BTN_BUILD, "Build the Setup file, the portable .zip, and SHA256SUMS.txt into the output folder"),
            (ID_BTN_OPEN_OUTPUT, "Open the folder where the Setup file is built"),
            (ID_BTN_PREVIEW, "Open the Setup file you built, to see the installer your users will see"),
            (ID_BTN_CANCEL, "Stop the running operation. A stopped build removes its unfinished files"),
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

    /// Replaces one control's tooltip text (the text buffer is kept
    /// alive in state like the initial tips).
    unsafe fn set_tip_text(hwnd: HWND, id: usize, text: &str) {
        let (tooltip, ctrl) = match with_state(|s| s.tooltip) {
            Some(t) if t != 0 => (t as HWND, GetDlgItem(hwnd, id as i32)),
            _ => return,
        };
        if ctrl.is_null() {
            return;
        }
        let mut wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let ti = TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_IDISHWND_FLAG,
            hwnd,
            uId: ctrl as usize,
            rect: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            hinst: std::ptr::null_mut(),
            lpszText: wide.as_mut_ptr(),
            lParam: 0,
            lpReserved: std::ptr::null_mut(),
        };
        SendMessageW(
            tooltip,
            TTM_UPDATETIPTEXTW_MSG,
            0,
            &ti as *const _ as LPARAM,
        );
        with_state(|s| s.tip_texts.push(wide));
    }

    /// The two path fields are single-line and clip long paths: their
    /// tooltips carry the FULL current path instead, so a clipped
    /// path is always readable. With no path set, the tooltip is the
    /// field's plain explanation. Refreshed wherever the fields are
    /// filled or harvested.
    unsafe fn update_path_tips(hwnd: HWND) {
        for (id, base) in [
            (
                ID_EDT_FILES_DIR,
                "The folder your built program is in — everything in it is packed into the Setup file",
            ),
            (
                ID_EDT_OUT_DIR,
                "The folder the finished Setup file, portable .zip, and SHA256SUMS.txt are written to",
            ),
        ] {
            let path = get_text(hwnd, id);
            let trimmed = path.trim();
            if trimmed.is_empty() {
                set_tip_text(hwnd, id, base);
            } else {
                set_tip_text(hwnd, id, trimmed);
            }
        }
    }

    unsafe fn apply_fonts(hwnd: HWND) {
        let header_ids = [
            ID_LBL_FILES_HDR,
            ID_LBL_FILES_PANE_HDR,
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
        let all_ids: &[usize] = &[
            ID_LIST,
            ID_TREE,
            ID_BTN_TB_OPEN,
            ID_BTN_TB_SAVE,
            ID_BTN_TB_SAVEAS,
            ID_BTN_TB_VALIDATE,
            ID_LBL_APPVER,
            ID_LBL_FILES_PANE_HDR,
            ID_LBL_TOTALS,
            ID_BTN_PROGRAM_FOLDER,
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
            ID_CHK_LAUNCH_OFFER,
            ID_CHK_LAUNCH_DEFAULT,
            ID_CHK_LOGIN_OFFER,
            ID_CHK_LOGIN_DEFAULT,
            ID_LBL_LAUNCH,
            ID_LBL_LOGIN,
            ID_LBL_DATADIRS,
            ID_EDT_DATA_DIRS,
            ID_LBL_POWER,
            ID_EDT_POWER,
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
            ID_BTN_OPEN_OUTPUT,
            ID_BTN_PREVIEW,
            ID_BTN_CANCEL,
            ID_LBL_FACTS,
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
            let font = if header_ids.contains(id) {
                HEADER_FONT
            } else {
                BODY_FONT
            };
            set_font(hwnd, *id, font);
        }
    }

    /// Positions every control from the client size and the current
    /// DPI scale (Design H): toolbar strip fixed at the top, output
    /// strip and footer fixed at the bottom; between them the left
    /// listing pane and the manifest pane keep their widths while the
    /// center files table absorbs the slack.
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
        let toolbar_h = s(40);
        let footer_h = s(52);
        let strip_h = s(150);
        let left_w = s(250);
        let panel_w = s(404);
        let footer_top = h - footer_h;
        let strip_y = footer_top - strip_h - s(8);
        let main_top = toolbar_h;
        let main_h = (strip_y - s(8) - main_top).max(s(60));
        let btn_row_h = s(30);

        unsafe fn mv(hwnd: HWND, id: usize, x: i32, y: i32, w: i32, h: i32) {
            let ctrl = GetDlgItem(hwnd, id as i32);
            if !ctrl.is_null() {
                MoveWindow(ctrl, x, y, w.max(0), h.max(0), 1);
            }
        }

        // Toolbar strip.
        let tb_y = s(6);
        mv(hwnd, ID_BTN_TB_OPEN, m, tb_y, s(84), s(28));
        mv(hwnd, ID_BTN_TB_SAVE, m + s(92), tb_y, s(72), s(28));
        mv(hwnd, ID_BTN_TB_SAVEAS, m + s(172), tb_y, s(92), s(28));
        mv(hwnd, ID_BTN_TB_VALIDATE, m + s(272), tb_y, s(116), s(28));
        mv(
            hwnd,
            ID_LBL_APPVER,
            w - m - s(320),
            tb_y + s(4),
            s(320),
            s(20),
        );

        // Left pane: the files directory listing.
        mv(hwnd, ID_LBL_FILES_PANE_HDR, m, main_top, left_w, s(20));
        mv(hwnd, ID_TREE, m, main_top + s(26), left_w, main_h - s(26));
        let tree = GetDlgItem(hwnd, ID_TREE as i32);
        if !tree.is_null() {
            // The column is at least as wide as the widest row
            // (measured when the listing was filled), so long names
            // can be scrolled to in full instead of ending under the
            // pane's edge.
            let content_w = with_state(|s| s.tree_col_w).unwrap_or(0);
            SendMessageW(
                tree,
                LVM_SETCOLUMNWIDTH,
                0,
                (left_w.max(content_w)).max(s(120)) as LPARAM,
            );
        }

        // Center: files table + its buttons + the totals line.
        let cx = m + left_w + s(12);
        let center_w = (w - m - panel_w - s(12) - cx).max(s(200));
        // Two full lines of body text: at s(38) the second line (the
        // stub / selected-file line) was clipped by the output strip.
        let totals_h = s(48);
        mv(hwnd, ID_LBL_FILES_HDR, cx, main_top, center_w, s(20));
        mv(
            hwnd,
            ID_LIST,
            cx,
            main_top + s(26),
            center_w,
            main_h - s(26) - btn_row_h - s(8) - totals_h,
        );
        let btn_y = main_top + main_h - totals_h - btn_row_h;
        mv(hwnd, ID_BTN_PROGRAM_FOLDER, cx, btn_y, s(160), btn_row_h);
        mv(hwnd, ID_BTN_REMOVE, cx + s(168), btn_y, s(150), btn_row_h);
        mv(
            hwnd,
            ID_LBL_TOTALS,
            cx,
            btn_y + btn_row_h + s(4),
            center_w,
            s(42),
        );
        // Column widths: the path column absorbs the slack, and the
        // fixed columns are sized so every header label fits whole
        // at the default window size, with the vertical scrollbar's
        // width reserved so the last column is not clipped by it.
        let list = GetDlgItem(hwnd, ID_LIST as i32);
        if !list.is_null() {
            let fixed = s(150) + s(95) + s(115);
            SendMessageW(
                list,
                LVM_SETCOLUMNWIDTH,
                0,
                (center_w - fixed - s(20)).max(s(100)) as LPARAM,
            );
            SendMessageW(list, LVM_SETCOLUMNWIDTH, 1, s(150) as LPARAM);
            SendMessageW(list, LVM_SETCOLUMNWIDTH, 2, s(95) as LPARAM);
            SendMessageW(list, LVM_SETCOLUMNWIDTH, 3, s(115) as LPARAM);
        }

        // Right pane: the manifest editor.
        let px = w - m - panel_w;
        let label_w = s(140);
        let edit_x = px + label_w + s(8);
        let edit_w = panel_w - label_w - s(8);
        mv(hwnd, ID_LBL_MANIFEST_HDR, px, main_top, panel_w, s(20));
        let mut row_y = main_top + s(26);
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
            mv(hwnd, PANEL_LABELS[i], px, row_y + s(2), label_w, s(20));
            mv(hwnd, *edit_id, edit_x, row_y, edit_w, s(25));
            row_y += s(29);
        }
        mv(hwnd, ID_CHK_DESKTOP, px, row_y, panel_w / 2, s(24));
        mv(
            hwnd,
            ID_CHK_STARTMENU,
            px + panel_w / 2,
            row_y,
            panel_w / 2,
            s(24),
        );
        row_y += s(27);
        mv(hwnd, ID_CHK_PATH, px, row_y, panel_w, s(24));
        row_y += s(27);
        // Offer pairs: label at the left, "Offer" and "On by default"
        // checkboxes sharing the edit column.
        let half = edit_w / 2;
        mv(hwnd, ID_LBL_LAUNCH, px, row_y + s(2), label_w, s(20));
        mv(hwnd, ID_CHK_LAUNCH_OFFER, edit_x, row_y, half, s(24));
        mv(
            hwnd,
            ID_CHK_LAUNCH_DEFAULT,
            edit_x + half,
            row_y,
            edit_w - half,
            s(24),
        );
        row_y += s(29);
        mv(hwnd, ID_LBL_LOGIN, px, row_y + s(2), label_w, s(20));
        mv(hwnd, ID_CHK_LOGIN_OFFER, edit_x, row_y, half, s(24));
        mv(
            hwnd,
            ID_CHK_LOGIN_DEFAULT,
            edit_x + half,
            row_y,
            edit_w - half,
            s(24),
        );
        row_y += s(29);
        mv(hwnd, ID_LBL_DATADIRS, px, row_y + s(2), label_w, s(20));
        mv(hwnd, ID_EDT_DATA_DIRS, edit_x, row_y, edit_w, s(25));
        row_y += s(31);
        // The manifest pane runs down to the footer — the output
        // strip occupies only the left/center columns — so the
        // PACKAGE cluster anchors to the pane's bottom and the Power
        // blocks editor flexes in the space between. Before this the
        // pane flowed top-down into the strip's rows at ordinary
        // window sizes, and the cluster collided with the strip's
        // buttons and the facts text.
        let pane_bottom = footer_top - s(8);
        let cluster_h = s(23) + s(29) + s(29);
        let cluster_top = pane_bottom - cluster_h;
        // "Advanced options (JSON)" is too long for the label column,
        // so its label takes the pane's full width and the editor
        // sits below it, still in the edit column.
        let power_h = (cluster_top - s(6) - row_y).clamp(s(58), s(142));
        mv(hwnd, ID_LBL_POWER, px, row_y, panel_w, s(20));
        mv(
            hwnd,
            ID_EDT_POWER,
            edit_x,
            row_y + s(22),
            edit_w,
            power_h - s(22),
        );
        mv(hwnd, ID_LBL_PACKAGE_HDR, px, cluster_top, panel_w, s(20));
        // Program folder row (+ Browse… button, wide enough to carry
        // its word — a bare "…" was neither plain English nor a
        // usable accessible name).
        let files_y = cluster_top + s(23);
        mv(hwnd, PANEL_LABELS[7], px, files_y + s(2), label_w, s(20));
        mv(
            hwnd,
            ID_EDT_FILES_DIR,
            edit_x,
            files_y,
            edit_w - s(70),
            s(25),
        );
        mv(
            hwnd,
            ID_BTN_FILES_DIR,
            edit_x + edit_w - s(64),
            files_y,
            s(64),
            s(25),
        );
        // Output folder row (+ browse button).
        let out_y = files_y + s(29);
        mv(hwnd, PANEL_LABELS[8], px, out_y + s(2), label_w, s(20));
        mv(hwnd, ID_EDT_OUT_DIR, edit_x, out_y, edit_w - s(70), s(25));
        mv(
            hwnd,
            ID_BTN_OUT_DIR,
            edit_x + edit_w - s(64),
            out_y,
            s(64),
            s(25),
        );

        // Output strip: the left/center columns only — its right
        // edge stops at the manifest pane, so nothing here can meet
        // the PACKAGE cluster. The facts line runs under the output
        // edit instead of in a right-hand column of its own.
        let strip_right = px - s(12);
        mv(hwnd, ID_LBL_OUTPUT_HDR, m, strip_y, s(120), s(20));
        mv(
            hwnd,
            ID_CHK_SANDBOX,
            strip_right - s(394),
            strip_y,
            s(148),
            s(24),
        );
        mv(
            hwnd,
            ID_BTN_VERIFY,
            strip_right - s(238),
            strip_y,
            s(130),
            s(26),
        );
        mv(
            hwnd,
            ID_BTN_VALIDATE,
            strip_right - s(100),
            strip_y,
            s(100),
            s(26),
        );
        mv(
            hwnd,
            ID_OUTPUT,
            m,
            strip_y + s(30),
            strip_right - m,
            strip_h - s(74),
        );
        mv(
            hwnd,
            ID_LBL_FACTS,
            m,
            strip_y + strip_h - s(40),
            strip_right - m,
            s(38),
        );

        // Footer: the status line, then the action row — Cancel,
        // Open output folder, Preview installer, Test install, and
        // the one primary action, Build Setup.
        mv(hwnd, ID_FOOTER_SEP, 0, footer_top - s(2), w, s(1));
        let btn_y2 = footer_top + s(10);
        mv(hwnd, ID_BTN_BUILD, w - m - s(160), btn_y2, s(160), s(32));
        mv(hwnd, ID_BTN_TEST, w - m - s(308), btn_y2, s(140), s(32));
        mv(hwnd, ID_BTN_PREVIEW, w - m - s(456), btn_y2, s(140), s(32));
        mv(
            hwnd,
            ID_BTN_OPEN_OUTPUT,
            w - m - s(614),
            btn_y2,
            s(150),
            s(32),
        );
        mv(hwnd, ID_BTN_CANCEL, w - m - s(718), btn_y2, s(96), s(32));
        mv(
            hwnd,
            ID_LBL_STATUS,
            m,
            footer_top + s(16),
            (w - m - s(718) - s(8) - m).max(s(80)),
            s(22),
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
        set_check(hwnd, ID_CHK_LAUNCH_OFFER, m.launch_after_install_offered);
        set_check(hwnd, ID_CHK_LAUNCH_DEFAULT, m.launch_after_install_default);
        set_check(hwnd, ID_CHK_LOGIN_OFFER, m.start_at_login_offered);
        set_check(hwnd, ID_CHK_LOGIN_DEFAULT, m.start_at_login_default);
        set_text(hwnd, ID_EDT_DATA_DIRS, &data_dirs_text(&m.data_dirs));
        // The power editor shows the blocks as pretty JSON; a
        // multiline edit control wants CRLF line breaks.
        set_text(
            hwnd,
            ID_EDT_POWER,
            &power_blocks_text(&m.to_power_spec()).replace('\n', "\r\n"),
        );
        set_text(
            hwnd,
            ID_EDT_FILES_DIR,
            &files_dir
                .map(|p| p.display().to_string())
                .unwrap_or_default(),
        );
        set_text(hwnd, ID_EDT_OUT_DIR, &out_dir.display().to_string());
        with_state(|s| s.suppress_dirty = false);
        update_path_tips(hwnd);
        refresh_appver(hwnd);
    }

    /// The toolbar strip's right-hand readout: "<app name> · <version>"
    /// from the document (refreshed when controls are filled and after
    /// a harvest, so it follows the Name/Version fields).
    fn refresh_appver(hwnd: HWND) {
        let text =
            with_state(|s| format!("{} · {}", s.doc.manifest.app_name, s.doc.manifest.version))
                .unwrap_or_default();
        unsafe { set_text(hwnd, ID_LBL_APPVER, &text) };
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
        let launch_offer = get_check(hwnd, ID_CHK_LAUNCH_OFFER);
        let launch_default = get_check(hwnd, ID_CHK_LAUNCH_DEFAULT);
        let login_offer = get_check(hwnd, ID_CHK_LOGIN_OFFER);
        let login_default = get_check(hwnd, ID_CHK_LOGIN_DEFAULT);
        let data_dirs_raw = get_text(hwnd, ID_EDT_DATA_DIRS);
        let power_raw = get_text(hwnd, ID_EDT_POWER);
        let files_dir = get_text(hwnd, ID_EDT_FILES_DIR);
        let out_dir = get_text(hwnd, ID_EDT_OUT_DIR);
        // The power blocks: parse first. On success the five typed
        // fields are replaced wholesale (the pane edits the same
        // model the manifest file carries). On failure the document
        // keeps its previous blocks and the error is reported to the
        // output strip once per distinct error — harvest runs before
        // every operation, so a standing typo must not spam it.
        let power_result = parse_power_blocks(&power_raw);
        match &power_result {
            Ok(_) => {
                with_state(|s| s.power_error_reported = None);
            }
            Err(errors) => {
                let message = format!(
                    "Power blocks not applied (previous value kept): {}",
                    errors.join("; ")
                );
                let fresh = with_state(|s| {
                    if s.power_error_reported.as_deref() == Some(message.as_str()) {
                        false
                    } else {
                        s.power_error_reported = Some(message.clone());
                        true
                    }
                })
                .unwrap_or(false);
                if fresh {
                    append_output(hwnd, &message);
                }
            }
        }
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
            m.launch_after_install_offered = launch_offer;
            m.launch_after_install_default = launch_default;
            m.start_at_login_offered = login_offer;
            m.start_at_login_default = login_default;
            m.data_dirs = parse_data_dirs(&data_dirs_raw);
            if let Ok(spec) = &power_result {
                m.components = spec.components.clone();
                m.pre_hooks = spec.pre_hooks.clone();
                m.post_hooks = spec.post_hooks.clone();
                m.registry_writes = spec.registry_writes.clone();
                m.env_writes = spec.env_writes.clone();
                m.shortcuts = spec.shortcuts.clone();
            }
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
        update_path_tips(hwnd);
        refresh_appver(hwnd);
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
        fill_tree(hwnd);
        update_totals(hwnd);
    }

    /// Rebuilds the left pane's listing from the last scan (falling
    /// back to the document entries before the first scan lands):
    /// the whole files directory, directories included — the center
    /// table shows only what will be packed.
    unsafe fn fill_tree(hwnd: HWND) {
        let tree = GetDlgItem(hwnd, ID_TREE as i32);
        if tree.is_null() {
            return;
        }
        let entries = with_state(|s| {
            let files = match &s.scan {
                Some(scan) => scan.all_files.clone(),
                None => s.doc.entries.clone(),
            };
            tree_entries(&files)
        })
        .unwrap_or_default();
        SendMessageW(tree, LVM_DELETEALLITEMS, 0, 0);
        let mut rels: Vec<Option<String>> = Vec::with_capacity(entries.len());
        // Measure the widest row (text + indent) so the listing's
        // one column can be made wide enough for every name to be
        // scrolled to in full.
        let dpi = with_state(|s| s.dpi).unwrap_or(96);
        let mut widest: i32 = 0;
        for (idx, entry) in entries.iter().enumerate() {
            rels.push(entry.rel.clone());
            let mut wide: Vec<u16> = entry
                .label
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let text_w = SendMessageW(tree, LVM_GETSTRINGWIDTHW, 0, wide.as_mut_ptr() as LPARAM);
            let row_w = text_w as i32 + scale(20, dpi) * entry.depth as i32 + scale(16, dpi);
            widest = widest.max(row_w);
            let item = LVITEMW {
                mask: LVIF_TEXT | LVIF_INDENT,
                iItem: idx as i32,
                iSubItem: 0,
                state: 0,
                stateMask: 0,
                pszText: wide.as_mut_ptr(),
                cchTextMax: 0,
                iImage: 0,
                lParam: 0,
                iIndent: entry.depth as i32,
                iGroupId: 0,
                cColumns: 0,
                puColumns: std::ptr::null_mut(),
                piColFmt: std::ptr::null_mut(),
                iGroup: 0,
            };
            SendMessageW(tree, LVM_INSERTITEMW, 0, &item as *const _ as LPARAM);
        }
        with_state(|s| {
            s.tree_rels = rels;
            s.tree_col_w = widest;
        });
        // Apply the measured width right away (layout does the same
        // on the next resize): the column is never narrower than the
        // pane, and wider when a name needs it.
        let mut tree_rect: RECT = std::mem::zeroed();
        GetClientRect(tree, &mut tree_rect);
        SendMessageW(
            tree,
            LVM_SETCOLUMNWIDTH,
            0,
            (tree_rect.right.max(widest)) as LPARAM,
        );
    }

    /// The two-line readout under the center table: package totals,
    /// then either the selected file's facts or the installer stub
    /// (this executable) the package will be built around.
    fn update_totals(hwnd: HWND) {
        let text = with_state(|s| {
            let rows = build_rows(&s.doc.entries, s.scan.as_ref());
            let total: u64 = rows.iter().map(|r| r.size).sum();
            let packed: u64 = rows.iter().filter_map(|r| r.packed).sum();
            let line1 = format!(
                "{} files · {} total · packed ≈ {}",
                rows.len(),
                format_bytes(total),
                format_bytes(packed)
            );
            let line2 = match &s.selected_rel {
                Some(rel) => match rows.iter().find(|r| &r.rel == rel) {
                    Some(row) => format!(
                        "Selected: {} — {} · packed {}",
                        row.rel.replace('/', "\\"),
                        if row.on_disk {
                            format_bytes(row.size)
                        } else {
                            "missing".to_string()
                        },
                        match row.packed {
                            Some(p) => format_bytes(p),
                            None => "—".to_string(),
                        }
                    ),
                    None => format!("Selected: {}", rel.replace('/', "\\")),
                },
                None => {
                    let stub = std::env::current_exe()
                        .ok()
                        .and_then(|p| std::fs::metadata(p).ok())
                        .map(|md| format_bytes(md.len()))
                        .unwrap_or_else(|| "—".to_string());
                    format!("Installer stub (this executable): {stub}")
                }
            };
            format!("{line1}\r\n{line2}")
        })
        .unwrap_or_default();
        unsafe { set_text(hwnd, ID_LBL_TOTALS, &text) };
    }

    /// Center-table selection: remember the file so the totals line
    /// describes it.
    fn on_list_selection(hwnd: HWND, index: i32) {
        if index < 0 {
            return;
        }
        let rel = with_state(|s| {
            let rows = build_rows(&s.doc.entries, s.scan.as_ref());
            rows.get(index as usize).map(|r| r.rel.clone())
        })
        .flatten();
        if let Some(rel) = rel {
            with_state(|s| s.selected_rel = Some(rel));
            update_totals(hwnd);
        }
    }

    /// Left-listing selection: remember the file, mirror the
    /// selection into the center table, and scroll it into view.
    unsafe fn on_tree_selection(hwnd: HWND, index: i32) {
        if index < 0 {
            return;
        }
        let rel = with_state(|s| s.tree_rels.get(index as usize).cloned().flatten()).flatten();
        let Some(rel) = rel else {
            return; // a directory row: nothing to select in the table
        };
        with_state(|s| s.selected_rel = Some(rel.clone()));
        let row = with_state(|s| {
            let rows = build_rows(&s.doc.entries, s.scan.as_ref());
            rows.iter().position(|r| r.rel == rel)
        })
        .flatten();
        if let Some(row) = row {
            let list = GetDlgItem(hwnd, ID_LIST as i32);
            if !list.is_null() {
                let clear = LVITEMW {
                    mask: LVIF_STATE,
                    iItem: -1,
                    iSubItem: 0,
                    state: 0,
                    stateMask: LVIS_SELECTED | LVIS_FOCUSED,
                    pszText: std::ptr::null_mut(),
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
                SendMessageW(
                    list,
                    LVM_SETITEMSTATE,
                    -1isize as WPARAM,
                    &clear as *const _ as LPARAM,
                );
                let mut set = clear;
                set.iItem = row as i32;
                set.state = LVIS_SELECTED | LVIS_FOCUSED;
                SendMessageW(list, LVM_SETITEMSTATE, row, &set as *const _ as LPARAM);
                SendMessageW(list, LVM_ENSUREVISIBLE, row, 0);
            }
        }
        update_totals(hwnd);
    }

    /// Buttons that start work or change the job's inputs: all
    /// disabled while a job runs (its inputs are snapshotted when it
    /// starts; a second operation must not begin). Cancel is the
    /// mirror image — enabled only while a job runs.
    const ACTION_IDS: [usize; 12] = [
        ID_BTN_PROGRAM_FOLDER,
        ID_BTN_REMOVE,
        ID_BTN_TB_OPEN,
        ID_BTN_TB_SAVE,
        ID_BTN_TB_SAVEAS,
        ID_BTN_TB_VALIDATE,
        ID_BTN_VALIDATE,
        ID_BTN_VERIFY,
        ID_BTN_TEST,
        ID_BTN_BUILD,
        ID_BTN_OPEN_OUTPUT,
        ID_BTN_PREVIEW,
    ];

    fn set_busy(hwnd: HWND, busy: bool) {
        with_state(|s| s.busy = busy);
        for id in ACTION_IDS {
            unsafe { set_enabled(hwnd, id, !busy) };
        }
        unsafe { set_enabled(hwnd, ID_BTN_CANCEL, busy) };
    }

    fn is_busy() -> bool {
        with_state(|s| s.busy).unwrap_or(false)
    }

    /// What a job starter needs: the result slot plus this job's
    /// fresh cancel flag and progress slot.
    struct JobHooks {
        job: Arc<Mutex<Option<JobResult>>>,
        cancel: Arc<AtomicBool>,
        progress: Arc<Mutex<Option<(usize, usize)>>>,
    }

    /// Starts a job: fresh cancel flag + progress slot in state,
    /// action buttons off, Cancel on, and the status line naming the
    /// operation (so the user always knows what Cancel would stop).
    fn begin_job(hwnd: HWND, kind: JobKind) -> Option<JobHooks> {
        let hooks = with_state(|s| {
            s.cancel = Arc::new(AtomicBool::new(false));
            s.progress = Arc::new(Mutex::new(None));
            s.job_kind = kind;
            JobHooks {
                job: s.job.clone(),
                cancel: s.cancel.clone(),
                progress: s.progress.clone(),
            }
        })?;
        set_busy(hwnd, true);
        set_status(hwnd, StatusKind::Neutral, kind.start_text());
        Some(hooks)
    }

    /// The Cancel button: set the running job's flag and say so; the
    /// job's own completion handler restores the controls.
    fn on_cancel_job(hwnd: HWND) {
        with_state(|s| s.cancel.store(true, Ordering::Relaxed));
        unsafe { set_enabled(hwnd, ID_BTN_CANCEL, false) };
        set_status(hwnd, StatusKind::Neutral, "Cancelling…");
    }

    /// Records a progress count in the slot and posts the progress
    /// message (the UI thread reads the slot) — throttled by the
    /// callers to the first, every tenth, and the last file, which
    /// is plenty for a status line.
    fn report_progress(
        hwnd_value: usize,
        slot: &Arc<Mutex<Option<(usize, usize)>>>,
        done: usize,
        total: usize,
    ) {
        if let Ok(mut g) = slot.lock() {
            *g = Some((done, total));
        }
        unsafe {
            PostMessageW(hwnd_value as HWND, WM_APP_PROGRESS, 0, 0);
        }
    }

    fn on_progress_message(hwnd: HWND) {
        let snapshot = with_state(|s| (s.job_kind, s.progress.lock().ok().and_then(|g| *g)));
        if let Some((kind, Some((done, total)))) = snapshot {
            if is_busy() {
                set_status(hwnd, StatusKind::Neutral, &kind.progress_text(done, total));
            }
        }
    }

    /// A cancelled job lands here: controls restored, and the report
    /// in plain words. A cancelled scan keeps the previous folder
    /// contents on screen; a cancelled build has already removed the
    /// files it placed (see pack_with_hooks).
    fn on_cancelled(hwnd: HWND, kind: JobKind) {
        set_busy(hwnd, false);
        match kind {
            JobKind::Scan => {
                append_output(
                    hwnd,
                    "Scan cancelled — the table still shows the previous folder contents.",
                );
                set_status(
                    hwnd,
                    StatusKind::Neutral,
                    "Scan cancelled — previous contents kept",
                );
            }
            JobKind::Pack => {
                append_output(hwnd, "Build cancelled — the unfinished files were removed.");
                set_status(
                    hwnd,
                    StatusKind::Neutral,
                    "Build cancelled — the unfinished files were removed",
                );
            }
            JobKind::Verify => {
                append_output(hwnd, "Verify cancelled.");
                set_status(hwnd, StatusKind::Neutral, "Verify cancelled");
            }
            JobKind::TestInstall => {
                append_output(hwnd, "Test install cancelled.");
                set_status(hwnd, StatusKind::Neutral, "Test install cancelled");
            }
        }
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
        let files_dir = with_state(|s| s.doc.files_dir.clone()).flatten();
        let Some(dir) = files_dir else {
            set_facts(hwnd, &default_facts());
            set_status(
                hwnd,
                StatusKind::Neutral,
                "Click Program Folder… and pick the folder your built program is in — its files will appear here.",
            );
            return;
        };
        let Some(hooks) = begin_job(hwnd, JobKind::Scan) else {
            return;
        };
        let hwnd_value = hwnd as usize;
        let JobHooks { job, cancel, .. } = hooks;
        std::thread::spawn(move || {
            let result = scan_package_with(&dir, Some(&cancel))
                .map_err(|e| format!("Cannot scan {}: {e}", dir.display()));
            let result = if cancel.load(Ordering::Relaxed) {
                JobResult::Cancelled(JobKind::Scan)
            } else {
                JobResult::Scan(result)
            };
            post_job(hwnd_value, &job, result);
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
                            "Warning: {missing} file(s) in the manifest list are not in the program folder — pack will refuse until they are removed from the list or restored."
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

    /// When the program folder was typed rather than picked, it can
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
                "Program folder changed — rescanning so the table shows the new folder. Run the action again.",
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
        let Some((files_dir, out_dir)) =
            with_state(|s| (s.doc.files_dir.clone(), s.doc.out_dir.clone()))
        else {
            return;
        };
        let Some(files_dir) = files_dir else {
            append_output(
                hwnd,
                "No program folder chosen yet. Click Program Folder… and pick the folder your built program is in — then click Build Setup again.",
            );
            set_status(
                hwnd,
                StatusKind::Error,
                "Click Program Folder… and pick the folder your built program is in",
            );
            return;
        };
        let stub = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                append_output(hwnd, &format!("Error: cannot locate own executable: {e}"));
                return;
            }
        };
        let Some(hooks) = begin_job(hwnd, JobKind::Pack) else {
            return;
        };
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
        let JobHooks {
            job,
            cancel,
            progress,
        } = hooks;
        std::thread::spawn(move || {
            let mut on_progress = |p: &crate::pack::PackProgress| {
                let done = p.file_index + 1;
                if done == 1 || done == p.file_count || done.is_multiple_of(10) {
                    report_progress(hwnd_value, &progress, done, p.file_count);
                }
            };
            let result = crate::pack::pack_with_hooks(
                &manifest,
                &files_dir,
                &out_dir,
                &stub,
                force,
                Some(&mut on_progress),
                Some(&cancel),
            );
            let result = match result {
                Ok(outcome) => JobResult::Pack(Ok(outcome)),
                Err(PackError::Cancelled) => JobResult::Cancelled(JobKind::Pack),
                Err(e) => JobResult::Pack(Err(PackFail {
                    refused_overwrite: matches!(e, PackError::RefuseOverwrite(_)),
                    message: e.to_string(),
                })),
            };
            post_job(hwnd_value, &job, result);
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
                // Name what was built and where — no guessing.
                let out_folder = outcome
                    .sums_path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "the output folder".to_string());
                set_status(
                    hwnd,
                    StatusKind::Ok,
                    &format!(
                        "Built: {}, the portable .zip, and SHA256SUMS.txt in {}",
                        outcome.setup.name, out_folder
                    ),
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
        let kind = if sandbox {
            JobKind::TestInstall
        } else {
            JobKind::Verify
        };
        let Some(hooks) = begin_job(hwnd, kind) else {
            return;
        };
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
        let JobHooks {
            job,
            cancel,
            progress,
        } = hooks;
        std::thread::spawn(move || {
            if sandbox {
                match crate::pack::sandbox_verify_with(&path, Some(&cancel)) {
                    Ok(report) if report.cancelled => {
                        post_job(hwnd_value, &job, JobResult::Cancelled(JobKind::TestInstall));
                    }
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
                        post_job(
                            hwnd_value,
                            &job,
                            JobResult::Verify {
                                sandbox,
                                path,
                                ok: report.ok,
                                lines,
                            },
                        );
                    }
                    Err(e) => post_job(
                        hwnd_value,
                        &job,
                        JobResult::Verify {
                            sandbox,
                            path,
                            ok: false,
                            lines: vec![format!("Error: cannot sandbox-verify: {e}")],
                        },
                    ),
                }
            } else {
                let mut on_progress = |p: &crate::pack::PackProgress| {
                    let done = p.file_index + 1;
                    if done == 1 || done == p.file_count || done.is_multiple_of(10) {
                        report_progress(hwnd_value, &progress, done, p.file_count);
                    }
                };
                match crate::pack::verify_package_with(&path, Some(&mut on_progress), Some(&cancel))
                {
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
                        post_job(
                            hwnd_value,
                            &job,
                            JobResult::Verify {
                                sandbox,
                                path,
                                ok: report.ok,
                                lines,
                            },
                        );
                    }
                    Err(_) if cancel.load(Ordering::Relaxed) => {
                        post_job(hwnd_value, &job, JobResult::Cancelled(JobKind::Verify));
                    }
                    Err(e) => post_job(
                        hwnd_value,
                        &job,
                        JobResult::Verify {
                            sandbox,
                            path,
                            ok: false,
                            lines: vec![format!("Error: cannot verify: {e}")],
                        },
                    ),
                }
            }
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

    /// The document's output folder as an absolute path (a relative
    /// value resolves against the program folder, else the process
    /// working folder) — what Open output folder and Preview
    /// installer act on.
    fn resolved_out_dir(doc: &StudioDocument) -> PathBuf {
        if doc.out_dir.is_absolute() {
            return doc.out_dir.clone();
        }
        if let Some(files_dir) = &doc.files_dir {
            return files_dir.join(&doc.out_dir);
        }
        std::env::current_dir()
            .map(|cwd| cwd.join(&doc.out_dir))
            .unwrap_or_else(|_| doc.out_dir.clone())
    }

    /// The Setup file this project builds into its output folder —
    /// the exact name the pack operation produces.
    fn expected_setup_path(doc: &StudioDocument) -> PathBuf {
        resolved_out_dir(doc).join(doc.manifest.setup_file_name())
    }

    /// ShellExecuteW "open", the same launch mechanism as the log
    /// viewer: a file path, or a program plus parameters.
    unsafe fn shell_open(hwnd: HWND, file: &str, params: Option<&str>) {
        let verb: Vec<u16> = "open\0".encode_utf16().collect();
        let file_w: Vec<u16> = file.encode_utf16().chain(std::iter::once(0)).collect();
        let params_w: Vec<u16> = params
            .unwrap_or("")
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();
        windows_sys::Win32::UI::Shell::ShellExecuteW(
            hwnd,
            verb.as_ptr(),
            file_w.as_ptr(),
            if params.is_some() {
                params_w.as_ptr()
            } else {
                std::ptr::null()
            },
            std::ptr::null(),
            SW_SHOWDEFAULT,
        );
    }

    /// Opens Windows Explorer at the output folder, selecting the
    /// built Setup file when it is there.
    unsafe fn on_open_output_folder(hwnd: HWND) {
        if is_busy() {
            return;
        }
        harvest_fields(hwnd);
        let Some((out_dir, setup, last_setup)) = with_state(|s| {
            (
                resolved_out_dir(&s.doc),
                expected_setup_path(&s.doc),
                s.last_setup.clone(),
            )
        }) else {
            return;
        };
        if !out_dir.is_dir() {
            append_output(hwnd, "The output folder does not exist yet — it is created when you build. If the path looks wrong, fix it in the Output folder field under PACKAGE.");
            set_status(
                hwnd,
                StatusKind::Error,
                "The output folder does not exist yet — it is created when you build",
            );
            return;
        }
        let select = if setup.is_file() {
            Some(setup)
        } else {
            last_setup.filter(|p| p.is_file() && p.parent() == Some(out_dir.as_path()))
        };
        match select {
            Some(path) => {
                let params = format!("/select,\"{}\"", path.display());
                shell_open(hwnd, "explorer.exe", Some(&params));
            }
            None => shell_open(hwnd, &out_dir.display().to_string(), None),
        }
        set_status(
            hwnd,
            StatusKind::Neutral,
            "Output folder opened in Explorer",
        );
    }

    /// Launches the project's built Setup file, so the real
    /// installer wizard opens — the answer to "how do I see the
    /// other view?": the same program IS the installer when it
    /// carries a package, and this runs the built file to prove it.
    unsafe fn on_preview_installer(hwnd: HWND) {
        if is_busy() {
            return;
        }
        harvest_fields(hwnd);
        let Some(setup) = with_state(|s| expected_setup_path(&s.doc)) else {
            return;
        };
        if setup.is_file() {
            shell_open(hwnd, &setup.display().to_string(), None);
            append_output(hwnd, "Opened the installer preview — this is what people see when they run your Setup file. Nothing is installed unless they click Install.");
            set_status(hwnd, StatusKind::Neutral, "Opened the installer preview — this is what people see when they run your Setup file. Nothing is installed unless they click Install.");
        } else {
            append_output(hwnd, "No Setup file yet. Click Build Setup first, then Preview installer to see what people will see.");
            set_status(hwnd, StatusKind::Neutral, "No Setup file yet. Click Build Setup first, then Preview installer to see what people will see.");
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
            Some(JobResult::Cancelled(kind)) => on_cancelled(hwnd, kind),
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
                if id == ID_FOOTER_SEP {
                    // The footer's 1px edge separator: an empty
                    // static filled edge-to-edge with the edge brush.
                    SetBkMode(hdc, OPAQUE as i32);
                    SetBkColor(hdc, theme::EDGE);
                    return theme::brush(theme::EDGE) as LRESULT;
                }
                if id == ID_OUTPUT {
                    // A read-only edit reports as a static, not via
                    // WM_CTLCOLOREDIT. It takes the read-only fill —
                    // letting it fall through to the window brush was
                    // what made the output panel a different dark
                    // than every edit field above it.
                    SetBkMode(hdc, OPAQUE as i32);
                    SetBkColor(hdc, theme::READ_ONLY_FILL);
                    SetTextColor(hdc, theme::FOREGROUND);
                    return theme::brush(theme::READ_ONLY_FILL) as LRESULT;
                }
                SetBkMode(hdc, TRANSPARENT as i32);
                if id == ID_LBL_STATUS {
                    let kind = with_state(|s| s.status_kind).unwrap_or(StatusKind::Neutral);
                    SetTextColor(
                        hdc,
                        match kind {
                            StatusKind::Ok => theme::SUCCESS,
                            StatusKind::Error => theme::DANGER,
                            StatusKind::Neutral => theme::MUTED,
                        },
                    );
                } else if id == ID_LBL_FILES_HDR
                    || id == ID_LBL_FILES_PANE_HDR
                    || id == ID_LBL_MANIFEST_HDR
                    || id == ID_LBL_OUTPUT_HDR
                    || id == ID_LBL_PACKAGE_HDR
                    || id == ID_LBL_FACTS
                    || id == ID_LBL_TOTALS
                    || id == ID_LBL_APPVER
                {
                    // Section captions and genuinely secondary text.
                    SetTextColor(hdc, theme::MUTED);
                } else {
                    // Everything else is an enabled control's label
                    // (the manifest field labels, the option rows).
                    SetTextColor(hdc, theme::FOREGROUND);
                }
                theme::brush(theme::BACKGROUND) as LRESULT
            }
            WM_CTLCOLOREDIT => {
                let hdc = wparam as HDC;
                let ctrl = lparam as HWND;
                SetBkMode(hdc, OPAQUE as i32);
                if IsWindowEnabled(ctrl) == 0 {
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
                // No header arm: the list headers are painted by
                // their subclass (theme::subclass_list_header) —
                // header draw-item messages never reach this window.
                match dis.CtlID as usize {
                    ID_BTN_BUILD => theme::draw_push_button(dis, true),
                    ID_BTN_TB_OPEN
                    | ID_BTN_TB_SAVE
                    | ID_BTN_TB_SAVEAS
                    | ID_BTN_TB_VALIDATE
                    | ID_BTN_PROGRAM_FOLDER
                    | ID_BTN_REMOVE
                    | ID_BTN_FILES_DIR
                    | ID_BTN_OUT_DIR
                    | ID_BTN_VALIDATE
                    | ID_BTN_VERIFY
                    | ID_BTN_TEST
                    | ID_BTN_OPEN_OUTPUT
                    | ID_BTN_PREVIEW
                    | ID_BTN_CANCEL => theme::draw_push_button(dis, false),
                    id if CHECKBOX_IDS.contains(&id) => theme::draw_checkbox(dis),
                    _ => return DefWindowProcW(hwnd, msg, wparam, lparam),
                }
                1
            }
            WM_COMMAND => {
                let control_id = wparam & 0xFFFF;
                let notify = (wparam >> 16) & 0xFFFF;
                // Owner-drawn checkboxes do not toggle themselves —
                // a click flips the stored check state here, and the
                // arms below react exactly as they did for native
                // auto-checkboxes (dirty tracking).
                if notify == BN_CLICKED_CODE && CHECKBOX_IDS.contains(&control_id) {
                    let now = get_check(hwnd, control_id);
                    set_check(hwnd, control_id, !now);
                }
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
                    IDM_BUILD_PREVIEW => on_preview_installer(hwnd),
                    IDM_TOOLS_SCHEMA => on_schema(hwnd),
                    IDM_TOOLS_LOG => on_open_log(hwnd),
                    IDM_HELP_ABOUT => on_about(hwnd),
                    ID_BTN_TB_OPEN => on_open_manifest(hwnd),
                    ID_BTN_TB_SAVE => on_save(hwnd, false),
                    ID_BTN_TB_SAVEAS => on_save(hwnd, true),
                    ID_BTN_TB_VALIDATE => on_validate(hwnd),
                    ID_BTN_PROGRAM_FOLDER => on_choose_files_dir(hwnd),
                    ID_BTN_REMOVE => on_remove_selected(hwnd),
                    ID_BTN_CANCEL => on_cancel_job(hwnd),
                    ID_BTN_OPEN_OUTPUT => on_open_output_folder(hwnd),
                    ID_BTN_PREVIEW => on_preview_installer(hwnd),
                    // Esc (delivered as IDCANCEL by the dialog-message
                    // handling) cancels a running job, like the
                    // wizard's progress page.
                    2 if is_busy() => on_cancel_job(hwnd),
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
                    | ID_EDT_OUT_DIR | ID_EDT_DATA_DIRS | ID_EDT_POWER => {
                        if notify == EN_CHANGE_CODE {
                            mark_dirty(hwnd);
                        }
                    }
                    ID_CHK_DESKTOP
                    | ID_CHK_STARTMENU
                    | ID_CHK_PATH
                    | ID_CHK_LAUNCH_OFFER
                    | ID_CHK_LAUNCH_DEFAULT
                    | ID_CHK_LOGIN_OFFER
                    | ID_CHK_LOGIN_DEFAULT => {
                        mark_dirty(hwnd);
                    }
                    ID_CHK_SANDBOX => {
                        // A verify option, not a manifest field: no dirty.
                    }
                    _ => {}
                }
                0
            }
            WM_NOTIFY => {
                let hdr = lparam as *const NMHDR;
                if !hdr.is_null() {
                    // Theme custom draw (selection fill, gridlines)
                    // for the two list views.
                    let custom = match (*hdr).idFrom {
                        ID_LIST => theme::listview_custom_draw(lparam, true),
                        ID_TREE => theme::listview_custom_draw(lparam, false),
                        _ => None,
                    };
                    if let Some(result) = custom {
                        return result;
                    }
                }
                if !hdr.is_null() && (*hdr).code as i32 == LVN_ITEMCHANGED_I32 {
                    let nmlv = lparam as *const NMLISTVIEW;
                    if !nmlv.is_null() && (*nmlv).uNewState & LVIS_SELECTED != 0 {
                        match (*hdr).idFrom {
                            ID_LIST => on_list_selection(hwnd, (*nmlv).iItem),
                            ID_TREE => on_tree_selection(hwnd, (*nmlv).iItem),
                            _ => {}
                        }
                    }
                }
                0
            }
            m if m == WM_APP_JOB => {
                on_job_message(hwnd);
                0
            }
            m if m == WM_APP_PROGRESS => {
                on_progress_message(hwnd);
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
