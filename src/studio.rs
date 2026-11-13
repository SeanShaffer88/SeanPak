//! The SeanPak Builder — the secondary, human-facing surface over
//! the same operations the CLI exposes.
//!
//! Layout (v5.0.0, SEAN-approved mockups in `~/workspace/seanpak/
//! mockups-v5/`): a guided, page-based flow in place of the 4.x
//! single dense IDE window. A slim left nav (Home · Projects ·
//! Build · Settings) switches pages: a Home dashboard with recent
//! projects, a three-step New Project flow (details → program
//! folder → summary), the project editor as three tabs (Files and
//! Folders · Installer Options · Program Requirements), and the
//! build as three states (Build Settings → Building → Build
//! Finished). The theme is VS Code's GitHub Dark High Contrast,
//! taken directly through the shared tokens in `src/theme.rs`.
//! Every action calls a shared operation — pack is
//! `pack::pack_with_hooks`, validate is
//! `manifest::AppManifest::from_json`, verify is
//! `pack::verify_package_with` / `pack::sandbox_verify_with`, and
//! the file listing is `scanner::ScannedFolder::scan_with` — so the
//! Builder can do nothing the CLI cannot (see `docs/PARITY.md`).
//! The worker operations take the Builder's cancel flag and
//! progress sink; the CLI passes neither, so its behavior is
//! unchanged.
//!
//! This module's first half is the platform-independent document,
//! settings, and recent-projects model (unit-tested on any host);
//! the Win32 window itself is Windows-only, like the wizard.

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
// Studio settings: window geometry, last-used paths, and the recent
// projects list, as a small JSON file beside the executable — the
// same "beside the exe" location the diagnostics log uses (see
// `diag::log_path`). No registry, no config framework.

/// One project the Builder has opened or saved: the manifest path
/// plus the name/version it carried at the time, so the Home
/// dashboard and the Projects page can list it without opening
/// every file. (The modified time shown beside it is read live from
/// the file, so it stays true even if the entry is stale.)
#[derive(Debug, Clone, PartialEq)]
pub struct RecentProject {
    pub path: PathBuf,
    pub name: String,
    pub version: String,
}

/// The recent-projects list is a convenience, not an archive: the
/// dashboard shows the first few and the Projects page the rest.
pub const MAX_RECENT_PROJECTS: usize = 12;

/// How long ago `then` was, in the plain words the dashboard uses
/// ("just now", "5 minutes ago", "2 hours ago", "3 days ago", or a
/// date once it is older than a month). Pure, so it is unit-tested.
pub fn relative_time_text(now_unix: u64, then_unix: u64) -> String {
    let secs = now_unix.saturating_sub(then_unix);
    match secs {
        0..=59 => "just now".to_string(),
        60..=3599 => {
            let n = secs / 60;
            format!("{n} minute{} ago", if n == 1 { "" } else { "s" })
        }
        3600..=86399 => {
            let n = secs / 3600;
            format!("{n} hour{} ago", if n == 1 { "" } else { "s" })
        }
        86400..=2591999 => {
            let n = secs / 86400;
            format!("{n} day{} ago", if n == 1 { "" } else { "s" })
        }
        _ => "over a month ago".to_string(),
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct StudioSettings {
    /// (x, y, width, height) of the window when it last closed.
    pub window_rect: Option<(i32, i32, i32, i32)>,
    pub manifest_path: Option<PathBuf>,
    pub files_dir: Option<PathBuf>,
    pub out_dir: Option<PathBuf>,
    /// Projects opened or saved, most recent first.
    pub recents: Vec<RecentProject>,
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
        let recents = JsonValue::Array(
            self.recents
                .iter()
                .map(|r| {
                    jobj(vec![
                        ("path", jstr(r.path.display().to_string())),
                        ("name", jstr(r.name.clone())),
                        ("version", jstr(r.version.clone())),
                    ])
                })
                .collect(),
        );
        jobj(vec![
            ("window", window),
            ("manifestPath", path(&self.manifest_path)),
            ("filesDir", path(&self.files_dir)),
            ("outDir", path(&self.out_dir)),
            ("recentProjects", recents),
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
        let recents = match root.get("recentProjects") {
            Some(JsonValue::Array(items)) => items
                .iter()
                .filter_map(|item| {
                    let path = item.get("path").and_then(|v| v.as_str())?;
                    if path.is_empty() {
                        return None;
                    }
                    Some(RecentProject {
                        path: PathBuf::from(path),
                        name: item
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        version: item
                            .get("version")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .take(MAX_RECENT_PROJECTS)
                .collect(),
            _ => Vec::new(),
        };
        Some(Self {
            window_rect,
            manifest_path: get_path("manifestPath"),
            files_dir: get_path("filesDir"),
            out_dir: get_path("outDir"),
            recents,
        })
    }

    /// Records a project as just opened or saved: an existing entry
    /// for the same path moves to the front with fresh name/version,
    /// a new one is prepended, and the tail is trimmed to the cap.
    pub fn touch_recent(&mut self, path: &Path, name: &str, version: &str) {
        self.recents.retain(|r| r.path != path);
        self.recents.insert(
            0,
            RecentProject {
                path: path.to_path_buf(),
                name: name.to_string(),
                version: version.to_string(),
            },
        );
        self.recents.truncate(MAX_RECENT_PROJECTS);
    }

    /// Drops one entry (the file itself is never touched).
    pub fn remove_recent(&mut self, path: &Path) {
        self.recents.retain(|r| r.path != path);
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
            recents: vec![RecentProject {
                path: PathBuf::from("C:/apps/Foo/manifest.json"),
                name: "Foo".to_string(),
                version: "1.2.0".to_string(),
            }],
        };
        let text = settings.to_json().render();
        let back = StudioSettings::from_json(&text).expect("settings parse");
        assert_eq!(settings, back);
        // Garbage and undersized windows fall back cleanly.
        assert_eq!(StudioSettings::from_json("not json"), None);
        let tiny = StudioSettings::from_json(r#"{"window":[0,0,10,10]}"#).unwrap();
        assert_eq!(tiny.window_rect, None);
        // A pre-v5 settings file (no recentProjects) loads with an
        // empty list rather than failing.
        let legacy = StudioSettings::from_json(r#"{"window":[0,0,800,600]}"#).unwrap();
        assert!(legacy.recents.is_empty());
    }

    #[test]
    fn recents_touch_moves_to_front_and_caps() {
        let mut settings = StudioSettings::default();
        settings.touch_recent(Path::new("C:/a/manifest.json"), "A", "1.0");
        settings.touch_recent(Path::new("C:/b/manifest.json"), "B", "2.0");
        settings.touch_recent(Path::new("C:/a/manifest.json"), "A", "1.1");
        assert_eq!(settings.recents.len(), 2);
        assert_eq!(settings.recents[0].name, "A");
        assert_eq!(settings.recents[0].version, "1.1");
        for i in 0..MAX_RECENT_PROJECTS + 3 {
            settings.touch_recent(Path::new(&format!("C:/p{i}/manifest.json")), "P", "1.0");
        }
        assert_eq!(settings.recents.len(), MAX_RECENT_PROJECTS);
        settings.remove_recent(Path::new(&format!(
            "C:/p{}/manifest.json",
            MAX_RECENT_PROJECTS + 2
        )));
        assert_eq!(settings.recents.len(), MAX_RECENT_PROJECTS - 1);
    }

    #[test]
    fn relative_time_words() {
        let now = 1_000_000_000u64;
        assert_eq!(relative_time_text(now, now), "just now");
        assert_eq!(relative_time_text(now, now - 60), "1 minute ago");
        assert_eq!(relative_time_text(now, now - 1500), "25 minutes ago");
        assert_eq!(relative_time_text(now, now - 3600), "1 hour ago");
        assert_eq!(relative_time_text(now, now - 7200), "2 hours ago");
        assert_eq!(relative_time_text(now, now - 86400), "1 day ago");
        assert_eq!(relative_time_text(now, now - 432000), "5 days ago");
        assert_eq!(
            relative_time_text(now, now - 86400 * 40),
            "over a month ago"
        );
        // A file timestamped in the future reads as "just now", never negative.
        assert_eq!(relative_time_text(now, now + 500), "just now");
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
    fn blank_document_carries_the_v510_creation_defaults() {
        // A new Builder project creates installers with the desktop
        // icon on and launch-after-setup offered + ticked, and no
        // recorded location (the pack default is the per-user token
        // path, never Program Files).
        let doc = StudioDocument::new_blank();
        assert!(doc.manifest.desktop_shortcut);
        assert!(doc.manifest.launch_after_install_offered);
        assert!(doc.manifest.launch_after_install_default);
        assert_eq!(doc.manifest.install_location, None);
        let options = doc.manifest.to_install_options();
        assert_eq!(
            options.install_location(),
            "%LOCALAPPDATA%\\Programs\\New App"
        );
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
        relative_time_text, scan_package_with, tree_entries, FileRow, ScanOutcome, StudioDocument,
        StudioSettings,
    };
    use crate::manifest::{jobj, jstr, AppManifest, JsonValue, MANIFEST_SCHEMA_JSON};
    use crate::pack::{PackError, PackOutcome};
    use crate::scanner::format_bytes;

    // Colors come from the shared theme module (VS Code GitHub Dark
    // High Contrast tokens) — this window keeps no private palette.
    use crate::theme;

    // ---- pages ----

    /// The Builder's pages. The left nav selects a section (Home,
    /// Projects, Build, Settings); the editor is three pages under
    /// Projects, and the build is three pages under Build.
    #[derive(Clone, Copy, PartialEq)]
    enum Page {
        Home,
        ProjectList,
        NewProject,
        EditorFiles,
        EditorOptions,
        EditorRequirements,
        BuildSettings,
        Building,
        BuildDone,
        Settings,
    }

    // ---- controls ----
    // Manifest-field IDs keep their 4.x numbers so fill/harvest and
    // the job handlers carry over unchanged.
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
    const ID_LBL_PACKAGE_HDR: usize = 165;
    const ID_FOOTER_SEP: usize = 166;
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
    // Field labels inside the options page's details group.
    const PANEL_LABELS: [usize; 7] = [170, 171, 172, 173, 174, 175, 176];
    // Every owner-drawn checkbox in the window (a click toggles the
    // stored check state — see WM_COMMAND).
    const CHECKBOX_IDS: [usize; 13] = [
        ID_CHK_DESKTOP,
        ID_CHK_STARTMENU,
        ID_CHK_PATH,
        ID_CHK_LAUNCH_OFFER,
        ID_CHK_LAUNCH_DEFAULT,
        ID_CHK_LOGIN_OFFER,
        ID_CHK_LOGIN_DEFAULT,
        ID_CHK_SANDBOX,
        ID_CHK_WINREQ,
        ID_CHK_RT1,
        ID_CHK_RT2,
        ID_CHK_RT3,
        ID_CHK_RT4,
    ];

    // Nav rail.
    const ID_NAV_HOME: usize = 300;
    const ID_NAV_PROJECTS: usize = 301;
    const ID_NAV_BUILD: usize = 302;
    const ID_NAV_SETTINGS: usize = 303;
    // Home.
    const ID_LBL_HOME_TITLE: usize = 310;
    const ID_LBL_HOME_SUB: usize = 311;
    const ID_BTN_NEW_PROJECT: usize = 312;
    const ID_LBL_RECENT_HDR: usize = 313;
    const ID_BTN_VIEW_ALL: usize = 314;
    const ID_RECENTS: usize = 315;
    // Projects list.
    const ID_LBL_PL_TITLE: usize = 320;
    const ID_LBL_PL_SUB: usize = 321;
    const ID_BTN_PL_OPEN: usize = 322;
    const ID_BTN_PL_REMOVE: usize = 323;
    const ID_BTN_PL_NEW: usize = 324;
    const ID_BTN_PL_PICK: usize = 325;
    const ID_LBL_PL_CURRENT: usize = 326;
    // New Project flow.
    const ID_LBL_NP_TITLE: usize = 330;
    const ID_LBL_NP_SUB: usize = 331;
    const ID_LBL_NP_STEP1: usize = 332;
    const ID_LBL_NP_STEP2: usize = 333;
    const ID_LBL_NP_STEP3: usize = 334;
    const ID_EDT_NP_NAME: usize = 335;
    const ID_EDT_NP_VERSION: usize = 336;
    const ID_EDT_NP_PUBLISHER: usize = 337;
    const ID_LBL_NP_NAME: usize = 338;
    const ID_LBL_NP_VERSION: usize = 339;
    const ID_LBL_NP_PUBLISHER: usize = 340;
    const ID_EDT_NP_FOLDER: usize = 341;
    const ID_BTN_NP_BROWSE: usize = 342;
    const ID_LBL_NP_FOLDER: usize = 343;
    const ID_LBL_NP_FOLDER_NOTE: usize = 344;
    const ID_LBL_NP_SUMMARY: usize = 345;
    const ID_BTN_NP_CANCEL: usize = 346;
    const ID_BTN_NP_BACK: usize = 347;
    const ID_BTN_NP_NEXT: usize = 348;
    // Editor tabs.
    const ID_TAB_FILES: usize = 350;
    const ID_TAB_OPTIONS: usize = 351;
    const ID_TAB_REQUIREMENTS: usize = 352;
    // Files page.
    const ID_LBL_FILES_TITLE: usize = 360;
    const ID_LBL_FILES_SUB: usize = 361;
    const ID_LBL_FOLDER_PATH_LBL: usize = 363;
    // Options page.
    const ID_LBL_OPT_TITLE: usize = 370;
    const ID_LBL_OPT_SUB: usize = 371;
    const ID_LBL_OPT_DETAILS: usize = 372;
    const ID_LBL_OPT_ICON: usize = 373;
    const ID_LBL_ICON_VALUE: usize = 374;
    const ID_LBL_OPT_SHORTCUTS: usize = 375;
    const ID_LBL_DATA_NOTE: usize = 376;
    const ID_BTN_SCHEMA: usize = 377;
    const ID_LBL_PREVIEW_HDR: usize = 378;
    const ID_LBL_LOC_NOTE: usize = 379;
    // Requirements page.
    const ID_LBL_REQ_TITLE: usize = 380;
    const ID_LBL_REQ_SUB: usize = 381;
    const ID_LBL_REQ_WIN_HDR: usize = 382;
    const ID_CHK_WINREQ: usize = 383;
    const ID_LBL_WINREQ_TAG: usize = 384;
    const ID_LBL_REQ_RT_HDR: usize = 385;
    const ID_CHK_RT1: usize = 386;
    const ID_CHK_RT2: usize = 387;
    const ID_CHK_RT3: usize = 388;
    const ID_CHK_RT4: usize = 389;
    const ID_LBL_RT_TAG1: usize = 390;
    const ID_LBL_RT_TAG2: usize = 391;
    const ID_LBL_RT_TAG3: usize = 392;
    const ID_LBL_RT_TAG4: usize = 393;
    const ID_LBL_REQ_NOTE: usize = 394;
    const ID_BTN_REQ_BACK: usize = 395;
    const ID_BTN_REQ_NEXT: usize = 396;
    // Build Settings page.
    const ID_LBL_BS_TITLE: usize = 400;
    const ID_LBL_BS_SUB: usize = 401;
    const ID_LBL_BS_ARCH_HDR: usize = 402;
    const ID_LBL_BS_ARCH: usize = 403;
    const ID_LBL_BS_COMP_HDR: usize = 404;
    const ID_CHK_COMPRESS: usize = 405;
    const ID_LBL_BS_OUT_HDR: usize = 406;
    const ID_LBL_BS_GET_HDR: usize = 407;
    const ID_LBL_OUTPUTS: usize = 408;
    const ID_LBL_BS_CHECK_HDR: usize = 409;
    const ID_BTN_BS_BACK: usize = 410;
    const ID_LBL_BS_CONSOLE_HDR: usize = 411;
    // Building page.
    const ID_LBL_ING_TITLE: usize = 420;
    const ID_LBL_ING_SUB: usize = 421;
    const ID_LBL_ING_PHASE: usize = 422;
    const ID_PROGRESS: usize = 423;
    const ID_LBL_ING_PCT: usize = 424;
    const ID_BTN_CLEAR: usize = 425;
    // Build Finished page.
    const ID_LBL_DONE_TITLE: usize = 430;
    const ID_LBL_DONE_SUB: usize = 431;
    const ID_LBL_DONE_SUMMARY: usize = 432;
    const ID_LBL_DONE_REASON: usize = 433;
    const ID_BTN_DONE_INSTALLER: usize = 434;
    const ID_BTN_DONE_FOLDER: usize = 435;
    const ID_BTN_DONE_AGAIN: usize = 436;
    const ID_BTN_DONE_BACK: usize = 437;
    // Settings page.
    const ID_LBL_SET_TITLE: usize = 440;
    const ID_LBL_SET_RECENTS_HDR: usize = 441;
    const ID_LBL_SET_RECENTS: usize = 442;
    const ID_BTN_CLEAR_RECENTS: usize = 443;
    const ID_LBL_SET_DIAG_HDR: usize = 444;
    const ID_LBL_SET_LOG: usize = 445;
    const ID_BTN_OPEN_LOG: usize = 446;
    const ID_LBL_SET_SETTINGS: usize = 447;
    const ID_LBL_SET_ABOUT_HDR: usize = 448;
    const ID_LBL_SET_VERSION: usize = 449;
    const ID_BTN_ABOUT: usize = 450;
    const ID_LBL_SET_FACTS: usize = 451;

    /// Every control the layout shows or hides per page (the footer
    /// status line, its separator, and Cancel are always visible and
    /// not listed here).
    const ALL_PAGE_CONTROLS: &[usize] = &[
        ID_NAV_HOME,
        ID_NAV_PROJECTS,
        ID_NAV_BUILD,
        ID_NAV_SETTINGS,
        ID_LBL_HOME_TITLE,
        ID_LBL_HOME_SUB,
        ID_BTN_NEW_PROJECT,
        ID_LBL_RECENT_HDR,
        ID_BTN_VIEW_ALL,
        ID_RECENTS,
        ID_LBL_PL_TITLE,
        ID_LBL_PL_SUB,
        ID_BTN_PL_OPEN,
        ID_BTN_PL_REMOVE,
        ID_BTN_PL_NEW,
        ID_BTN_PL_PICK,
        ID_LBL_PL_CURRENT,
        ID_LBL_NP_TITLE,
        ID_LBL_NP_SUB,
        ID_LBL_NP_STEP1,
        ID_LBL_NP_STEP2,
        ID_LBL_NP_STEP3,
        ID_EDT_NP_NAME,
        ID_EDT_NP_VERSION,
        ID_EDT_NP_PUBLISHER,
        ID_LBL_NP_NAME,
        ID_LBL_NP_VERSION,
        ID_LBL_NP_PUBLISHER,
        ID_EDT_NP_FOLDER,
        ID_BTN_NP_BROWSE,
        ID_LBL_NP_FOLDER,
        ID_LBL_NP_FOLDER_NOTE,
        ID_LBL_NP_SUMMARY,
        ID_BTN_NP_CANCEL,
        ID_BTN_NP_BACK,
        ID_BTN_NP_NEXT,
        ID_TAB_FILES,
        ID_TAB_OPTIONS,
        ID_TAB_REQUIREMENTS,
        ID_BTN_TB_OPEN,
        ID_BTN_TB_SAVE,
        ID_BTN_TB_SAVEAS,
        ID_BTN_TB_VALIDATE,
        ID_LBL_APPVER,
        ID_LBL_FILES_TITLE,
        ID_LBL_FILES_SUB,
        ID_LBL_FOLDER_PATH_LBL,
        ID_EDT_FILES_DIR,
        ID_BTN_FILES_DIR,
        ID_LBL_FILES_PANE_HDR,
        ID_TREE,
        ID_BTN_PROGRAM_FOLDER,
        ID_LBL_PACKAGE_HDR,
        ID_LIST,
        ID_LBL_TOTALS,
        ID_BTN_REMOVE,
        ID_LBL_OPT_TITLE,
        ID_LBL_OPT_SUB,
        ID_LBL_OPT_DETAILS,
        PANEL_LABELS[0],
        PANEL_LABELS[1],
        PANEL_LABELS[2],
        PANEL_LABELS[3],
        PANEL_LABELS[4],
        PANEL_LABELS[5],
        PANEL_LABELS[6],
        ID_EDT_NAME,
        ID_EDT_VERSION,
        ID_EDT_PUBLISHER,
        ID_EDT_PRIMARY,
        ID_EDT_INSTALL_LOC,
        ID_EDT_SM_FOLDER,
        ID_EDT_ASSOC,
        ID_LBL_OPT_ICON,
        ID_LBL_ICON_VALUE,
        ID_LBL_LOC_NOTE,
        ID_LBL_OPT_SHORTCUTS,
        ID_CHK_DESKTOP,
        ID_CHK_STARTMENU,
        ID_CHK_PATH,
        ID_LBL_LAUNCH,
        ID_CHK_LAUNCH_OFFER,
        ID_CHK_LAUNCH_DEFAULT,
        ID_LBL_LOGIN,
        ID_CHK_LOGIN_OFFER,
        ID_CHK_LOGIN_DEFAULT,
        ID_LBL_DATADIRS,
        ID_EDT_DATA_DIRS,
        ID_LBL_DATA_NOTE,
        ID_LBL_PREVIEW_HDR,
        ID_LBL_POWER,
        ID_EDT_POWER,
        ID_BTN_SCHEMA,
        ID_BTN_PREVIEW,
        ID_LBL_REQ_TITLE,
        ID_LBL_REQ_SUB,
        ID_LBL_REQ_WIN_HDR,
        ID_CHK_WINREQ,
        ID_LBL_WINREQ_TAG,
        ID_LBL_REQ_RT_HDR,
        ID_CHK_RT1,
        ID_CHK_RT2,
        ID_CHK_RT3,
        ID_CHK_RT4,
        ID_LBL_RT_TAG1,
        ID_LBL_RT_TAG2,
        ID_LBL_RT_TAG3,
        ID_LBL_RT_TAG4,
        ID_LBL_REQ_NOTE,
        ID_BTN_REQ_BACK,
        ID_BTN_REQ_NEXT,
        ID_LBL_BS_TITLE,
        ID_LBL_BS_SUB,
        ID_LBL_BS_ARCH_HDR,
        ID_LBL_BS_ARCH,
        ID_LBL_BS_COMP_HDR,
        ID_CHK_COMPRESS,
        ID_LBL_BS_OUT_HDR,
        ID_EDT_OUT_DIR,
        ID_BTN_OUT_DIR,
        ID_LBL_BS_GET_HDR,
        ID_LBL_OUTPUTS,
        ID_LBL_BS_CHECK_HDR,
        ID_BTN_VALIDATE,
        ID_BTN_VERIFY,
        ID_CHK_SANDBOX,
        ID_BTN_TEST,
        ID_BTN_OPEN_OUTPUT,
        ID_BTN_BS_BACK,
        ID_BTN_BUILD,
        ID_LBL_BS_CONSOLE_HDR,
        ID_OUTPUT,
        ID_LBL_ING_TITLE,
        ID_LBL_ING_SUB,
        ID_LBL_ING_PHASE,
        ID_PROGRESS,
        ID_LBL_ING_PCT,
        ID_BTN_CLEAR,
        ID_LBL_DONE_TITLE,
        ID_LBL_DONE_SUB,
        ID_LBL_DONE_SUMMARY,
        ID_LBL_DONE_REASON,
        ID_BTN_DONE_INSTALLER,
        ID_BTN_DONE_FOLDER,
        ID_BTN_DONE_AGAIN,
        ID_BTN_DONE_BACK,
        ID_LBL_SET_TITLE,
        ID_LBL_SET_RECENTS_HDR,
        ID_LBL_SET_RECENTS,
        ID_LBL_SET_DIAG_HDR,
        ID_LBL_SET_LOG,
        ID_BTN_OPEN_LOG,
        ID_LBL_SET_SETTINGS,
        ID_LBL_SET_ABOUT_HDR,
        ID_LBL_SET_VERSION,
        ID_BTN_ABOUT,
        ID_LBL_SET_FACTS,
        ID_LBL_FILES_HDR,
    ];

    /// Buttons and lists disabled while a job runs (its inputs are
    /// snapshotted when it starts; a second operation must not
    /// begin). Cancel is the mirror image — enabled only then.
    const ACTION_IDS: &[usize] = &[
        ID_NAV_HOME,
        ID_NAV_PROJECTS,
        ID_NAV_BUILD,
        ID_NAV_SETTINGS,
        ID_BTN_NEW_PROJECT,
        ID_BTN_VIEW_ALL,
        ID_BTN_PL_OPEN,
        ID_BTN_PL_REMOVE,
        ID_BTN_PL_NEW,
        ID_BTN_PL_PICK,
        ID_BTN_NP_CANCEL,
        ID_BTN_NP_BACK,
        ID_BTN_NP_NEXT,
        ID_BTN_NP_BROWSE,
        ID_TAB_FILES,
        ID_TAB_OPTIONS,
        ID_TAB_REQUIREMENTS,
        ID_BTN_TB_OPEN,
        ID_BTN_TB_SAVE,
        ID_BTN_TB_SAVEAS,
        ID_BTN_TB_VALIDATE,
        ID_BTN_PROGRAM_FOLDER,
        ID_BTN_REMOVE,
        ID_BTN_FILES_DIR,
        ID_BTN_OUT_DIR,
        ID_BTN_PREVIEW,
        ID_BTN_SCHEMA,
        ID_BTN_REQ_BACK,
        ID_BTN_REQ_NEXT,
        ID_BTN_VALIDATE,
        ID_BTN_VERIFY,
        ID_BTN_TEST,
        ID_BTN_OPEN_OUTPUT,
        ID_BTN_BS_BACK,
        ID_BTN_BUILD,
        ID_BTN_CLEAR,
        ID_BTN_DONE_INSTALLER,
        ID_BTN_DONE_FOLDER,
        ID_BTN_DONE_AGAIN,
        ID_BTN_DONE_BACK,
        ID_BTN_CLEAR_RECENTS,
        ID_BTN_OPEN_LOG,
        ID_BTN_ABOUT,
        ID_LIST,
        ID_TREE,
        ID_RECENTS,
    ];

    // Messages / styles windows-sys keeps behind features this crate
    // does not enable; defined locally like the wizard does.
    const WM_APP_JOB: u32 = 0x8000 + 21;
    const WM_APP_PROGRESS: u32 = 0x8000 + 22;
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
    const ES_AUTOHSCROLL: u32 = 0x0080;
    const ES_MULTILINE: u32 = 0x0004;
    const ES_READONLY: u32 = 0x0800;
    const ES_AUTOVSCROLL: u32 = 0x0040;
    const BM_GETCHECK_MSG: u32 = 0x00F0;
    const BM_SETCHECK_MSG: u32 = 0x00F1;
    const BST_CHECKED_STATE: usize = 1;
    const LOGPIXELSX_INDEX: i32 = 88;
    const VK_CONTROL_KEY: i32 = 0x11;
    const LVN_ITEMCHANGED_I32: i32 = -101;
    const NM_CUSTOMDRAW_I32: i32 = -12;
    const NM_DBLCLK_I32: i32 = -3;
    const SS_RIGHT: u32 = 0x0002;
    const SS_CENTER: u32 = 0x0001;
    const LVS_EX_CHECKBOXES: u32 = 0x0400;
    const LVIS_STATEIMAGEMASK: u32 = 0xF000;
    const PBM_SETPOS_MSG: u32 = 0x0400 + 2;
    const PBM_SETBARCOLOR_MSG: u32 = 0x0400 + 9;
    const PBM_SETBKCOLOR_MSG: u32 = 0x2000 + 1;
    const PBS_SMOOTH: u32 = 0x01;

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
        Scan {
            gen: u64,
            result: Result<ScanOutcome, String>,
        },
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

    /// What the Build Finished page shows: the summary on success,
    /// the plain-English reason on failure.
    struct DoneInfo {
        ok: bool,
        summary: String,
        reason: String,
    }

    /// One row of the Files page's folder listing: a file (its
    /// package-relative path) or a directory (the files beneath it,
    /// for check toggling).
    struct TreeRowInfo {
        rel: Option<String>,
        files: Vec<String>,
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
        /// Folder-listing rows → file / directory membership, mirroring
        /// the listing control.
        tree_rows: Vec<TreeRowInfo>,
        /// The widest listing row in pixels, so the listing's first
        /// column is at least that wide and long names can be
        /// scrolled to in full.
        tree_col_w: i32,
        /// The file the totals line describes (selected in either the
        /// table or the listing).
        selected_rel: Option<String>,
        /// The power-blocks parse error last reported to the output
        /// console, so a harvest that runs on every operation does
        /// not repeat the same complaint; cleared when the text parses.
        power_error_reported: Option<String>,
        /// The page on screen.
        page: Page,
        /// Builder settings (geometry, last paths, recent projects),
        /// loaded at startup and saved as they change.
        settings: StudioSettings,
        /// New Project flow: the current step (0 details, 1 folder,
        /// 2 summary), the document set aside while the flow runs,
        /// and the page Cancel returns to.
        np_step: usize,
        np_backup: Option<StudioDocument>,
        np_return: Page,
        /// Set while programmatically changing listing check states,
        /// so the toggle handler does not treat them as user edits.
        suppress_tree_check: bool,
        /// Bumped whenever a scan starts and whenever the document
        /// is swapped out from under a running scan (New Project
        /// cancel), so a late scan result for a folder the user has
        /// already left is dropped instead of adopted.
        scan_gen: u64,
        /// When the running/last build started (Build time readout).
        build_started: Option<std::time::Instant>,
        /// What the Build Finished page shows.
        done: Option<DoneInfo>,
        /// Client-area rectangles the window paints itself (the
        /// options page's installer preview, the finished page's
        /// badge), recorded by layout.
        preview_rect: Option<(i32, i32, i32, i32)>,
        badge_rect: Option<(i32, i32, i32, i32)>,
        /// New Project step-rail circle centers + radius, by step.
        np_circles: Vec<(i32, i32, i32)>,
    }

    static STATE: Mutex<Option<StudioState>> = Mutex::new(None);
    static mut BODY_FONT: HFONT = std::ptr::null_mut();
    static mut HEADER_FONT: HFONT = std::ptr::null_mut();
    static mut TITLE_FONT: HFONT = std::ptr::null_mut();

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
        // once the Files page is shown, so startup stays instant.
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
        // `seanpak <folder>` still lands in the editor on that
        // folder, as it always has; a bare run opens the dashboard.
        let from_folder = initial_folder.is_some();
        if let Some(folder) = initial_folder {
            doc.files_dir = Some(folder);
        }
        let entries_dir = doc.files_dir.clone();
        let start_page = if from_folder {
            Page::EditorFiles
        } else {
            Page::Home
        };

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
                tree_rows: Vec::new(),
                tree_col_w: 0,
                selected_rel: None,
                power_error_reported: None,
                page: start_page,
                settings,
                np_step: 0,
                np_backup: None,
                np_return: Page::Home,
                suppress_tree_check: false,
                scan_gen: 0,
                build_started: None,
                done: None,
                preview_rect: None,
                badge_rect: None,
                np_circles: Vec::new(),
            });
        }

        unsafe {
            let init = INITCOMMONCONTROLSEX {
                dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
                dwICC: ICC_LISTVIEW_CLASSES | ICC_WIN95_CLASSES | ICC_PROGRESS_CLASS,
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

            let (x, y, w, h) = match with_state(|s| s.settings.window_rect) {
                Some(Some(rect)) => rect,
                _ => (CW_USEDEFAULT, CW_USEDEFAULT, 1180, 780),
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
            create_controls(hwnd, instance);
            apply_fonts(hwnd);
            create_tooltips(hwnd, instance);
            fill_controls(hwnd);
            with_state(|s| s.suppress_dirty = false);
            update_title(hwnd);
            show_page(hwnd, start_page);

            ShowWindow(hwnd, SW_SHOWDEFAULT);
            UpdateWindow(hwnd);
            if start_page == Page::EditorFiles {
                start_scan(hwnd);
                // The 4.x first-run guidance — only when there is
                // no folder to scan (a running scan and the Home
                // page speak for themselves).
                if with_state(|s| s.doc.files_dir.is_none()).unwrap_or(false) {
                    set_status(
                        hwnd,
                        StatusKind::Neutral,
                        "Click Program Folder… and pick the folder your built program is in — its files will appear here.",
                    );
                }
            }

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
                if with_state(|s| s.page) == Some(Page::EditorFiles) {
                    let list = GetDlgItem(hwnd, ID_LIST as i32);
                    if !list.is_null() {
                        SetFocus(list);
                    }
                }
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
        let dpi = GetDeviceCaps(hdc, LOGPIXELSX_INDEX) as u32;
        ReleaseDC(hwnd, hdc);
        if dpi == 0 {
            96
        } else {
            dpi
        }
    }

    unsafe fn create_fonts(dpi: u32) {
        if !BODY_FONT.is_null() {
            DeleteObject(BODY_FONT as _);
        }
        if !HEADER_FONT.is_null() {
            DeleteObject(HEADER_FONT as _);
        }
        if !TITLE_FONT.is_null() {
            DeleteObject(TITLE_FONT as _);
        }
        let mk = |height: i32, weight: i32| {
            CreateFontW(
                -scale(height, dpi),
                0,
                0,
                0,
                weight,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                0,
                WIDE_SANS.as_ptr(),
            )
        };
        BODY_FONT = mk(15, FW_NORMAL as i32);
        HEADER_FONT = mk(14, FW_SEMIBOLD as i32);
        TITLE_FONT = mk(27, FW_SEMIBOLD as i32);
    }

    // ---- control creation ----

    unsafe fn create_controls(parent: HWND, instance: HINSTANCE) {
        let static_class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let button_class: Vec<u16> = "BUTTON\0".encode_utf16().collect();
        let edit_class: Vec<u16> = "EDIT\0".encode_utf16().collect();
        let list_class: Vec<u16> = "SysListView32\0".encode_utf16().collect();
        let progress_class: Vec<u16> = "msctls_progress32\0".encode_utf16().collect();

        // Controls start hidden; layout() shows the current page's
        // set. (Creating them visible would flash the whole shell
        // before the first layout runs.)
        let mk = |class: &[u16], text: &str, style: u32, id: usize| -> HWND {
            let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
            unsafe {
                CreateWindowExW(
                    0,
                    class.as_ptr(),
                    wide.as_ptr(),
                    WS_CHILD | style,
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
        let btn = |text: &str, id: usize| mk(&button_class, text, WS_TABSTOP | BS_OWNERDRAW, id);
        let chk = |text: &str, id: usize| mk(&button_class, text, WS_TABSTOP | BS_OWNERDRAW, id);
        let lbl = |text: &str, id: usize| mk(&static_class, text, 0, id);
        let edt = |id: usize| mk(&edit_class, "", WS_TABSTOP | WS_BORDER | ES_AUTOHSCROLL, id);
        let setup_list = |list: HWND, cols: &[(&str, i32)], checkboxes: bool| {
            let ext = if checkboxes {
                LVS_EX_FULLROWSELECT | LVS_EX_CHECKBOXES
            } else {
                LVS_EX_FULLROWSELECT
            };
            SendMessageW(
                list,
                LVM_SETEXTENDEDLISTVIEWSTYLE,
                ext as WPARAM,
                ext as LPARAM,
            );
            SendMessageW(list, LVM_SETBKCOLOR, 0, theme::SURFACE as LPARAM);
            SendMessageW(list, LVM_SETTEXTBKCOLOR, 0, theme::SURFACE as LPARAM);
            SendMessageW(list, LVM_SETTEXTCOLOR, 0, theme::FOREGROUND as LPARAM);
            for (idx, (title, width)) in cols.iter().enumerate() {
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
        };

        // Nav rail (hidden on the New Project flow, which has its
        // own step rail).
        btn("Home", ID_NAV_HOME);
        btn("Projects", ID_NAV_PROJECTS);
        btn("Build", ID_NAV_BUILD);
        btn("Settings", ID_NAV_SETTINGS);

        // Footer: separator, status line, Cancel (always present;
        // enabled only while a job runs).
        mk(&static_class, "", 0, ID_FOOTER_SEP);
        lbl("", ID_LBL_STATUS);
        btn("Cancel", ID_BTN_CANCEL);

        // Home.
        lbl("Welcome to SeanPak", ID_LBL_HOME_TITLE);
        lbl(
            "Create, configure and build Windows installers from your project settings.",
            ID_LBL_HOME_SUB,
        );
        btn("+ New Project", ID_BTN_NEW_PROJECT);
        lbl("Recent Projects", ID_LBL_RECENT_HDR);
        btn("View all →", ID_BTN_VIEW_ALL);
        let recents = mk(
            &list_class,
            "",
            WS_TABSTOP | WS_BORDER | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
            ID_RECENTS,
        );
        setup_list(
            recents,
            &[("Project", 320), ("Version", 110), ("Modified", 170)],
            false,
        );

        // Projects list.
        lbl("Projects", ID_LBL_PL_TITLE);
        lbl(
            "Projects you have opened or saved in the Builder. Double-click one to open it.",
            ID_LBL_PL_SUB,
        );
        lbl("", ID_LBL_PL_CURRENT);
        btn("Open", ID_BTN_PL_OPEN);
        btn("Remove from list", ID_BTN_PL_REMOVE);
        btn("+ New Project", ID_BTN_PL_NEW);
        btn("Open a manifest file…", ID_BTN_PL_PICK);

        // New Project flow.
        lbl("Create a New Setup Project", ID_LBL_NP_TITLE);
        lbl("", ID_LBL_NP_SUB);
        lbl("Project Details", ID_LBL_NP_STEP1);
        lbl("Program Folder", ID_LBL_NP_STEP2);
        lbl("Summary", ID_LBL_NP_STEP3);
        lbl("Program name", ID_LBL_NP_NAME);
        edt(ID_EDT_NP_NAME);
        lbl("Version", ID_LBL_NP_VERSION);
        edt(ID_EDT_NP_VERSION);
        lbl("Publisher", ID_LBL_NP_PUBLISHER);
        edt(ID_EDT_NP_PUBLISHER);
        lbl("Program folder", ID_LBL_NP_FOLDER);
        edt(ID_EDT_NP_FOLDER);
        btn("Browse…", ID_BTN_NP_BROWSE);
        lbl(
            "Pick the folder your built program is in — everything in it becomes the package. SeanPak scans it on the next step.",
            ID_LBL_NP_FOLDER_NOTE,
        );
        lbl("", ID_LBL_NP_SUMMARY);
        btn("Cancel", ID_BTN_NP_CANCEL);
        btn("← Back", ID_BTN_NP_BACK);
        btn("Next →", ID_BTN_NP_NEXT);

        // Editor chrome: tabs + the everyday commands.
        btn("Files and Folders", ID_TAB_FILES);
        btn("Installer Options", ID_TAB_OPTIONS);
        btn("Requirements", ID_TAB_REQUIREMENTS);
        btn("Open…", ID_BTN_TB_OPEN);
        btn("Save", ID_BTN_TB_SAVE);
        btn("Save As…", ID_BTN_TB_SAVEAS);
        btn("Check details", ID_BTN_TB_VALIDATE);
        mk(&static_class, "", SS_RIGHT, ID_LBL_APPVER);

        // Files page.
        lbl("Files and Folders", ID_LBL_FILES_TITLE);
        lbl(
            "Select which files and folders to include in your installer, and set their destination paths.",
            ID_LBL_FILES_SUB,
        );
        lbl("Program folder", ID_LBL_FOLDER_PATH_LBL);
        edt(ID_EDT_FILES_DIR);
        btn("Browse…", ID_BTN_FILES_DIR);
        lbl("PROGRAM FOLDER", ID_LBL_FILES_PANE_HDR);
        let tree = mk(
            &list_class,
            "",
            WS_TABSTOP | WS_BORDER | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
            ID_TREE,
        );
        setup_list(tree, &[("Source", 200), ("Destination", 150)], true);
        btn("Program Folder…", ID_BTN_PROGRAM_FOLDER);
        lbl("FILES IN PACKAGE", ID_LBL_PACKAGE_HDR);
        let list = mk(
            &list_class,
            "",
            WS_TABSTOP | WS_BORDER | LVS_REPORT | LVS_SHOWSELALWAYS,
            ID_LIST,
        );
        // No LVS_EX_GRIDLINES: the native gridline color is a fixed
        // light system color. The theme's custom draw (WM_NOTIFY)
        // paints selection and gridlines in token colors instead.
        setup_list(
            list,
            &[
                ("File", 380),
                ("Destination", 180),
                ("Size", 110),
                ("Packed (est.)", 120),
            ],
            false,
        );
        lbl("", ID_LBL_TOTALS);
        btn("Remove selected", ID_BTN_REMOVE);

        // Options page.
        lbl("Installer Options", ID_LBL_OPT_TITLE);
        lbl(
            "Choose what the installer sets up for your users.",
            ID_LBL_OPT_SUB,
        );
        lbl("PROGRAM DETAILS", ID_LBL_OPT_DETAILS);
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
            lbl(label, PANEL_LABELS[i]);
            edt(*edit_id);
        }
        lbl("Program icon", ID_LBL_OPT_ICON);
        lbl(
            "Built-in SeanPak icon — custom program icons are not supported yet",
            ID_LBL_ICON_VALUE,
        );
        lbl("", ID_LBL_LOC_NOTE);
        lbl("SHORTCUTS AND STARTUP", ID_LBL_OPT_SHORTCUTS);
        chk("Desktop shortcut", ID_CHK_DESKTOP);
        chk("Start Menu shortcut", ID_CHK_STARTMENU);
        chk("Add to PATH", ID_CHK_PATH);
        lbl("Launch after install", ID_LBL_LAUNCH);
        chk("Offer", ID_CHK_LAUNCH_OFFER);
        chk("On by default", ID_CHK_LAUNCH_DEFAULT);
        lbl("Start at login", ID_LBL_LOGIN);
        chk("Offer", ID_CHK_LOGIN_OFFER);
        chk("On by default", ID_CHK_LOGIN_DEFAULT);
        lbl("Data folders", ID_LBL_DATADIRS);
        edt(ID_EDT_DATA_DIRS);
        lbl(
            "Data folders: files your program saves in its data folder are kept when the program is removed, unless the user chooses to delete them.",
            ID_LBL_DATA_NOTE,
        );
        lbl(
            "Live preview — the installer your users will see",
            ID_LBL_PREVIEW_HDR,
        );
        lbl("Advanced options (JSON)", ID_LBL_POWER);
        mk(
            &edit_class,
            "",
            WS_TABSTOP | WS_BORDER | ES_MULTILINE | ES_AUTOVSCROLL | WS_VSCROLL | WS_HSCROLL,
            ID_EDT_POWER,
        );
        btn("Manifest format…", ID_BTN_SCHEMA);
        btn("Preview installer", ID_BTN_PREVIEW);

        // Requirements page.
        lbl("Program Requirements", ID_LBL_REQ_TITLE);
        lbl(
            "Things a computer must have before your program can run.",
            ID_LBL_REQ_SUB,
        );
        lbl("Windows version", ID_LBL_REQ_WIN_HDR);
        chk("Requires Windows 10 or later", ID_CHK_WINREQ);
        lbl("Built in", ID_LBL_WINREQ_TAG);
        lbl("Runtimes", ID_LBL_REQ_RT_HDR);
        chk(".NET Framework 4.8 or higher", ID_CHK_RT1);
        chk(".NET 8 Desktop Runtime", ID_CHK_RT2);
        chk("Visual C++ Redistributable (x64)", ID_CHK_RT3);
        chk("Microsoft Edge WebView2 Runtime", ID_CHK_RT4);
        lbl("Coming later", ID_LBL_RT_TAG1);
        lbl("Coming later", ID_LBL_RT_TAG2);
        lbl("Coming later", ID_LBL_RT_TAG3);
        lbl("Coming later", ID_LBL_RT_TAG4);
        lbl(
            "SeanPak can't add runtimes to an installer yet. If your program needs one, mention it in your program's readme for now.",
            ID_LBL_REQ_NOTE,
        );
        btn("← Back", ID_BTN_REQ_BACK);
        btn("Next →", ID_BTN_REQ_NEXT);

        // Build Settings page.
        lbl("Build Settings", ID_LBL_BS_TITLE);
        lbl(
            "Configure how your installer should be built.",
            ID_LBL_BS_SUB,
        );
        lbl("Architecture", ID_LBL_BS_ARCH_HDR);
        lbl(
            "x64 (64-bit) — SeanPak builds x64 installers.",
            ID_LBL_BS_ARCH,
        );
        lbl("Compression", ID_LBL_BS_COMP_HDR);
        chk(
            "Compress files in the installer (smaller Setup file)",
            ID_CHK_COMPRESS,
        );
        lbl("Output folder", ID_LBL_BS_OUT_HDR);
        edt(ID_EDT_OUT_DIR);
        btn("Browse…", ID_BTN_OUT_DIR);
        lbl("You will get", ID_LBL_BS_GET_HDR);
        lbl("", ID_LBL_OUTPUTS);
        lbl("Check a built Setup file", ID_LBL_BS_CHECK_HDR);
        btn("Check details", ID_BTN_VALIDATE);
        btn("Verify package", ID_BTN_VERIFY);
        chk("Also test-install", ID_CHK_SANDBOX);
        btn("Test install", ID_BTN_TEST);
        lbl("OUTPUT", ID_LBL_BS_CONSOLE_HDR);
        mk(
            &edit_class,
            "",
            WS_BORDER | ES_MULTILINE | ES_AUTOVSCROLL | ES_READONLY | WS_VSCROLL,
            ID_OUTPUT,
        );
        btn("Open output folder", ID_BTN_OPEN_OUTPUT);
        btn("← Back", ID_BTN_BS_BACK);
        btn("Build Setup", ID_BTN_BUILD);

        // Building page.
        lbl("Building Installer", ID_LBL_ING_TITLE);
        lbl(
            "Please wait while your project is compiled and packaged.",
            ID_LBL_ING_SUB,
        );
        lbl("Building…", ID_LBL_ING_PHASE);
        mk(&static_class, "", SS_RIGHT, ID_LBL_ING_PCT);
        let progress = mk(&progress_class, "", PBS_SMOOTH, ID_PROGRESS);
        SendMessageW(
            progress,
            PBM_SETBARCOLOR_MSG,
            0,
            theme::SELECTION_INDICATOR as LPARAM,
        );
        SendMessageW(progress, PBM_SETBKCOLOR_MSG, 0, theme::SURFACE as LPARAM);
        btn("Clear", ID_BTN_CLEAR);

        // Build Finished page (title, sub, and the summary/reason
        // block are centered, as drawn in the approved mockup).
        mk(&static_class, "", SS_CENTER, ID_LBL_DONE_TITLE);
        mk(&static_class, "", SS_CENTER, ID_LBL_DONE_SUB);
        mk(&static_class, "", SS_CENTER, ID_LBL_DONE_SUMMARY);
        mk(&static_class, "", SS_CENTER, ID_LBL_DONE_REASON);
        btn("Open the Installer", ID_BTN_DONE_INSTALLER);
        btn("Open Folder", ID_BTN_DONE_FOLDER);
        btn("Build Another", ID_BTN_DONE_AGAIN);
        btn("Back to Build Settings", ID_BTN_DONE_BACK);

        // Settings page.
        lbl("Settings", ID_LBL_SET_TITLE);
        lbl("RECENT PROJECTS", ID_LBL_SET_RECENTS_HDR);
        lbl("", ID_LBL_SET_RECENTS);
        btn("Clear the list", ID_BTN_CLEAR_RECENTS);
        lbl("DIAGNOSTICS", ID_LBL_SET_DIAG_HDR);
        lbl("", ID_LBL_SET_LOG);
        btn("Open the log", ID_BTN_OPEN_LOG);
        lbl("", ID_LBL_SET_SETTINGS);
        lbl("ABOUT", ID_LBL_SET_ABOUT_HDR);
        lbl("", ID_LBL_SET_VERSION);
        btn("About SeanPak Builder…", ID_BTN_ABOUT);
        lbl("", ID_LBL_SET_FACTS);

        // Fixed states that are displays, not choices: the Windows
        // requirement is built into every SeanPak installer, the
        // runtime rows are not available yet, and compression is
        // always on (each file is compressed only when smaller).
        // Disabled controls say so honestly instead of pretending
        // to be settings.
        set_check(parent, ID_CHK_WINREQ, true);
        set_check(parent, ID_CHK_COMPRESS, true);
        for id in [
            ID_CHK_WINREQ,
            ID_CHK_RT1,
            ID_CHK_RT2,
            ID_CHK_RT3,
            ID_CHK_RT4,
            ID_CHK_COMPRESS,
        ] {
            set_enabled(parent, id, false);
        }
        set_enabled(parent, ID_BTN_CANCEL, false);
    }

    unsafe fn create_tooltips(parent: HWND, instance: HINSTANCE) {
        // One short plain-English line per main control — what it
        // does, in the words a first-time user would use. ("Program",
        // not "payload"; "Setup file", not "artifact".)
        let tips: [(usize, &str); 73] = [
            (ID_NAV_HOME, "The dashboard: start a new project or reopen a recent one"),
            (ID_NAV_PROJECTS, "Your projects, and the one you are editing"),
            (ID_NAV_BUILD, "Build settings, the build itself, and checking a built Setup file"),
            (ID_NAV_SETTINGS, "Builder settings: recent projects, the diagnostics log, about"),
            (ID_BTN_NEW_PROJECT, "Start a new installer project, step by step"),
            (ID_BTN_VIEW_ALL, "See the full list of recent projects"),
            (ID_RECENTS, "Projects you have opened or saved. Double-click one to open it"),
            (ID_BTN_PL_OPEN, "Open the selected project in the editor"),
            (ID_BTN_PL_REMOVE, "Take the selected project off this list — the project files stay where they are"),
            (ID_BTN_PL_NEW, "Start a new installer project, step by step"),
            (ID_BTN_PL_PICK, "Open a manifest file from anywhere on this computer"),
            (ID_EDT_NP_NAME, "Your program's name — shown in the installer and used in the Setup file name"),
            (ID_EDT_NP_VERSION, "Your program's version, like 1.2.0 — used in the Setup file name"),
            (ID_EDT_NP_PUBLISHER, "Your name or your company — shown in the installer"),
            (ID_EDT_NP_FOLDER, "The folder your built program is in — everything in it is packed into the Setup file"),
            (ID_BTN_NP_BROWSE, "Browse for the program folder"),
            (ID_BTN_NP_BACK, "Back to the previous step"),
            (ID_BTN_NP_NEXT, "Continue to the next step"),
            (ID_BTN_NP_CANCEL, "Stop making this project — nothing is created"),
            (ID_TAB_FILES, "Choose which files go into the installer"),
            (ID_TAB_OPTIONS, "Program details, shortcuts, startup options, and advanced extras"),
            (ID_TAB_REQUIREMENTS, "What a computer needs before your program can run"),
            (ID_BTN_TB_OPEN, "Open a manifest file — the file that describes your program"),
            (ID_BTN_TB_SAVE, "Save the manifest file"),
            (ID_BTN_TB_SAVEAS, "Save the manifest file under a new name"),
            (ID_BTN_TB_VALIDATE, "Check the program details for mistakes before building"),
            (ID_LIST, "The files that will be packed into the Setup file. Select files and click Remove selected to leave them out"),
            (ID_TREE, "Everything in the program folder, folders included. Tick a box to include that file or folder; selecting a file here selects it in the table too"),
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
            (ID_BTN_SCHEMA, "Show the manifest format — every field a manifest can carry"),
            (ID_EDT_FILES_DIR, "The folder your built program is in — everything in it is packed into the Setup file"),
            (ID_BTN_FILES_DIR, "Browse for the program folder"),
            (ID_EDT_OUT_DIR, "The folder the finished Setup file, portable .zip, and SHA256SUMS.txt are written to"),
            (ID_BTN_OUT_DIR, "Browse for the output folder"),
            (ID_CHK_WINREQ, "Every SeanPak installer already needs Windows 10 or later — this is built in, not a choice"),
            (ID_CHK_RT1, "Not available yet — SeanPak can't add runtimes to an installer"),
            (ID_CHK_COMPRESS, "Always on — SeanPak compresses each file only when that makes it smaller, so there is nothing to choose"),
            (ID_BTN_VALIDATE, "Check the program details for mistakes — the same check the build runs first"),
            (ID_BTN_VERIFY, "Check a built Setup file: read it back and prove every file inside is intact"),
            (ID_CHK_SANDBOX, "Also test-install: verifying first installs into a temp folder, checks every file, then uninstalls and checks nothing is left behind"),
            (ID_BTN_TEST, "Install the built Setup file into a temp folder, check every file, then uninstall and check nothing is left"),
            (ID_BTN_BUILD, "Build the Setup file, the portable .zip, and SHA256SUMS.txt into the output folder"),
            (ID_BTN_OPEN_OUTPUT, "Open the folder where the Setup file is built"),
            (ID_BTN_PREVIEW, "Open the Setup file you built, to see the installer your users will see"),
            (ID_BTN_REQ_BACK, "Back to Installer Options"),
            (ID_BTN_REQ_NEXT, "Continue to Build Settings"),
            (ID_BTN_BS_BACK, "Back to the project editor"),
            (ID_BTN_CLEAR, "Clear the build log"),
            (ID_BTN_CANCEL, "Stop the running operation. A stopped build removes its unfinished files"),
            (ID_BTN_DONE_INSTALLER, "Open the Setup file you just built, to see the installer your users will see"),
            (ID_BTN_DONE_FOLDER, "Open the output folder in Explorer"),
            (ID_BTN_DONE_AGAIN, "Build again — back to Build Settings"),
            (ID_BTN_DONE_BACK, "Back to Build Settings"),
            (ID_BTN_CLEAR_RECENTS, "Empty the recent projects list — the project files stay where they are"),
            (ID_BTN_OPEN_LOG, "Open SeanPak's diagnostics log in your text editor"),
            (ID_BTN_ABOUT, "What SeanPak Builder is, in plain words"),
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
        let title_ids = [
            ID_LBL_HOME_TITLE,
            ID_LBL_PL_TITLE,
            ID_LBL_NP_TITLE,
            ID_LBL_FILES_TITLE,
            ID_LBL_OPT_TITLE,
            ID_LBL_REQ_TITLE,
            ID_LBL_BS_TITLE,
            ID_LBL_ING_TITLE,
            ID_LBL_DONE_TITLE,
            ID_LBL_SET_TITLE,
        ];
        let header_ids = [
            ID_LBL_RECENT_HDR,
            ID_LBL_FILES_PANE_HDR,
            ID_LBL_PACKAGE_HDR,
            ID_LBL_OPT_DETAILS,
            ID_LBL_OPT_SHORTCUTS,
            ID_LBL_POWER,
            ID_LBL_REQ_WIN_HDR,
            ID_LBL_REQ_RT_HDR,
            ID_LBL_BS_ARCH_HDR,
            ID_LBL_BS_COMP_HDR,
            ID_LBL_BS_OUT_HDR,
            ID_LBL_BS_GET_HDR,
            ID_LBL_BS_CHECK_HDR,
            ID_LBL_BS_CONSOLE_HDR,
            ID_LBL_SET_RECENTS_HDR,
            ID_LBL_SET_DIAG_HDR,
            ID_LBL_SET_ABOUT_HDR,
            ID_LBL_PREVIEW_HDR,
        ];
        unsafe fn set_font(hwnd: HWND, id: usize, font: HFONT) {
            let ctrl = GetDlgItem(hwnd, id as i32);
            if !ctrl.is_null() {
                SendMessageW(ctrl, WM_SETFONT, font as WPARAM, 1);
            }
        }
        for id in ALL_PAGE_CONTROLS {
            let font = if title_ids.contains(id) {
                TITLE_FONT
            } else if header_ids.contains(id) {
                HEADER_FONT
            } else {
                BODY_FONT
            };
            set_font(hwnd, *id, font);
        }
        set_font(hwnd, ID_LBL_STATUS, BODY_FONT);
        set_font(hwnd, ID_BTN_CANCEL, BODY_FONT);
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

    unsafe fn set_vis(hwnd: HWND, id: usize, show: bool) {
        let ctrl = GetDlgItem(hwnd, id as i32);
        if !ctrl.is_null() {
            ShowWindow(ctrl, if show { SW_SHOW } else { SW_HIDE });
        }
    }

    fn set_status(hwnd: HWND, kind: StatusKind, text: &str) {
        with_state(|s| s.status_kind = kind);
        unsafe { set_text(hwnd, ID_LBL_STATUS, text) };
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

    /// Appends one line to the output console. The edit is read-only
    /// for typing but fully selectable/copyable.
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

    /// A console line with a wall-clock timestamp — the Building
    /// page's log style.
    fn append_build_log(hwnd: HWND, line: &str) {
        let mut st: SYSTEMTIME = unsafe { std::mem::zeroed() };
        unsafe {
            windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut st);
        }
        append_output(
            hwnd,
            &format!(
                "[{:02}:{:02}:{:02}] {}",
                st.wHour, st.wMinute, st.wSecond, line
            ),
        );
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
        refresh_loc_note(hwnd);
    }

    /// The editor toolbar's right-hand readout: "<app name> ·
    /// <version>" from the document.
    fn refresh_appver(hwnd: HWND) {
        let text =
            with_state(|s| format!("{} · {}", s.doc.manifest.app_name, s.doc.manifest.version))
                .unwrap_or_default();
        unsafe { set_text(hwnd, ID_LBL_APPVER, &text) };
    }

    /// The Options page's install-location note: what the wizard
    /// will use as the default folder, in plain words.
    fn refresh_loc_note(hwnd: HWND) {
        let text = with_state(|s| {
            let m = &s.doc.manifest;
            match &m.install_location {
                Some(loc) if !loc.trim().is_empty() => {
                    if loc.contains('%') {
                        // A per-user token path (v5.1.0): show the
                        // recorded value AND where it lands on this
                        // PC — it resolves per user at install time.
                        format!(
                            "Install location default: {loc} — on this PC: {}",
                            crate::install::resolve_install_location(loc, &m.app_name).display()
                        )
                    } else {
                        format!("Install location default: {loc}")
                    }
                }
                _ => format!(
                    "Installs go to each user's own folder — no admin rights needed (AppData\\Local\\Programs\\{})",
                    m.app_name
                ),
            }
        })
        .unwrap_or_default();
        unsafe { set_text(hwnd, ID_LBL_LOC_NOTE, &text) };
    }

    /// The Build Settings page's "You will get" list: the three file
    /// names the build writes, from the document as harvested.
    fn refresh_outputs_label(hwnd: HWND) {
        let text = with_state(|s| {
            format!(
                "{}\r\n{}\r\nSHA256SUMS.txt",
                s.doc.manifest.setup_file_name(),
                s.doc.manifest.portable_file_name()
            )
        })
        .unwrap_or_default();
        unsafe { set_text(hwnd, ID_LBL_OUTPUTS, &text) };
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
        // output console once per distinct error — harvest runs
        // before every operation, so a standing typo must not spam.
        let power_result = parse_power_blocks(&power_raw);
        match &power_result {
            Ok(_) => {
                with_state(|s| s.power_error_reported = None);
            }
            Err(errors) => {
                let message = format!(
                    "Advanced options not applied (previous value kept): {}",
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
            if let Ok(power) = &power_result {
                m.components = power.components.clone();
                m.pre_hooks = power.pre_hooks.clone();
                m.post_hooks = power.post_hooks.clone();
                m.registry_writes = power.registry_writes.clone();
                m.env_writes = power.env_writes.clone();
                m.shortcuts = power.shortcuts.clone();
            }
            let fd = files_dir.trim();
            s.doc.files_dir = if fd.is_empty() {
                None
            } else {
                Some(PathBuf::from(fd))
            };
            let od = out_dir.trim();
            if !od.is_empty() {
                s.doc.out_dir = PathBuf::from(od);
            }
        });
        update_path_tips(hwnd);
        refresh_appver(hwnd);
        refresh_loc_note(hwnd);
    }

    // ---- pages ----

    /// The controls a page shows (before the step/state adjustments
    /// layout applies for the New Project steps and the Finished
    /// page's success/failure variants).
    fn visible_set(page: Page) -> Vec<usize> {
        let nav = [ID_NAV_HOME, ID_NAV_PROJECTS, ID_NAV_BUILD, ID_NAV_SETTINGS];
        let chrome = [
            ID_TAB_FILES,
            ID_TAB_OPTIONS,
            ID_TAB_REQUIREMENTS,
            ID_BTN_TB_OPEN,
            ID_BTN_TB_SAVE,
            ID_BTN_TB_SAVEAS,
            ID_BTN_TB_VALIDATE,
            ID_LBL_APPVER,
        ];
        let mut v: Vec<usize> = Vec::new();
        match page {
            Page::Home => {
                v.extend(nav);
                v.extend([
                    ID_LBL_HOME_TITLE,
                    ID_LBL_HOME_SUB,
                    ID_BTN_NEW_PROJECT,
                    ID_LBL_RECENT_HDR,
                    ID_BTN_VIEW_ALL,
                    ID_RECENTS,
                ]);
            }
            Page::ProjectList => {
                v.extend(nav);
                v.extend([
                    ID_LBL_PL_TITLE,
                    ID_LBL_PL_SUB,
                    ID_LBL_PL_CURRENT,
                    ID_RECENTS,
                    ID_BTN_PL_OPEN,
                    ID_BTN_PL_REMOVE,
                    ID_BTN_PL_NEW,
                    ID_BTN_PL_PICK,
                ]);
            }
            Page::NewProject => {
                v.extend([
                    ID_LBL_NP_TITLE,
                    ID_LBL_NP_SUB,
                    ID_LBL_NP_STEP1,
                    ID_LBL_NP_STEP2,
                    ID_LBL_NP_STEP3,
                    ID_LBL_NP_NAME,
                    ID_EDT_NP_NAME,
                    ID_LBL_NP_VERSION,
                    ID_EDT_NP_VERSION,
                    ID_LBL_NP_PUBLISHER,
                    ID_EDT_NP_PUBLISHER,
                    ID_LBL_NP_FOLDER,
                    ID_EDT_NP_FOLDER,
                    ID_BTN_NP_BROWSE,
                    ID_LBL_NP_FOLDER_NOTE,
                    ID_LBL_NP_SUMMARY,
                    ID_BTN_NP_CANCEL,
                    ID_BTN_NP_BACK,
                    ID_BTN_NP_NEXT,
                ]);
            }
            Page::EditorFiles => {
                v.extend(nav);
                v.extend(chrome);
                v.extend([
                    ID_LBL_FILES_TITLE,
                    ID_LBL_FILES_SUB,
                    ID_LBL_FOLDER_PATH_LBL,
                    ID_EDT_FILES_DIR,
                    ID_BTN_FILES_DIR,
                    ID_LBL_FILES_PANE_HDR,
                    ID_TREE,
                    ID_BTN_PROGRAM_FOLDER,
                    ID_LBL_PACKAGE_HDR,
                    ID_LIST,
                    ID_LBL_TOTALS,
                    ID_BTN_REMOVE,
                    ID_OUTPUT,
                ]);
            }
            Page::EditorOptions => {
                v.extend(nav);
                v.extend(chrome);
                v.extend([
                    ID_LBL_OPT_TITLE,
                    ID_LBL_OPT_SUB,
                    ID_LBL_OPT_DETAILS,
                    PANEL_LABELS[0],
                    PANEL_LABELS[1],
                    PANEL_LABELS[2],
                    PANEL_LABELS[3],
                    PANEL_LABELS[4],
                    PANEL_LABELS[5],
                    PANEL_LABELS[6],
                    ID_EDT_NAME,
                    ID_EDT_VERSION,
                    ID_EDT_PUBLISHER,
                    ID_EDT_PRIMARY,
                    ID_EDT_INSTALL_LOC,
                    ID_EDT_SM_FOLDER,
                    ID_EDT_ASSOC,
                    ID_LBL_OPT_ICON,
                    ID_LBL_ICON_VALUE,
                    ID_LBL_LOC_NOTE,
                    ID_LBL_OPT_SHORTCUTS,
                    ID_CHK_DESKTOP,
                    ID_CHK_STARTMENU,
                    ID_CHK_PATH,
                    ID_LBL_LAUNCH,
                    ID_CHK_LAUNCH_OFFER,
                    ID_CHK_LAUNCH_DEFAULT,
                    ID_LBL_LOGIN,
                    ID_CHK_LOGIN_OFFER,
                    ID_CHK_LOGIN_DEFAULT,
                    ID_LBL_DATADIRS,
                    ID_EDT_DATA_DIRS,
                    ID_LBL_DATA_NOTE,
                    ID_LBL_PREVIEW_HDR,
                    ID_LBL_POWER,
                    ID_EDT_POWER,
                    ID_BTN_SCHEMA,
                    ID_BTN_PREVIEW,
                ]);
            }
            Page::EditorRequirements => {
                v.extend(nav);
                v.extend(chrome);
                v.extend([
                    ID_LBL_REQ_TITLE,
                    ID_LBL_REQ_SUB,
                    ID_LBL_REQ_WIN_HDR,
                    ID_CHK_WINREQ,
                    ID_LBL_WINREQ_TAG,
                    ID_LBL_REQ_RT_HDR,
                    ID_CHK_RT1,
                    ID_CHK_RT2,
                    ID_CHK_RT3,
                    ID_CHK_RT4,
                    ID_LBL_RT_TAG1,
                    ID_LBL_RT_TAG2,
                    ID_LBL_RT_TAG3,
                    ID_LBL_RT_TAG4,
                    ID_LBL_REQ_NOTE,
                    ID_BTN_REQ_BACK,
                    ID_BTN_REQ_NEXT,
                ]);
            }
            Page::BuildSettings => {
                v.extend(nav);
                v.extend([
                    ID_LBL_BS_TITLE,
                    ID_LBL_BS_SUB,
                    ID_LBL_BS_ARCH_HDR,
                    ID_LBL_BS_ARCH,
                    ID_LBL_BS_COMP_HDR,
                    ID_CHK_COMPRESS,
                    ID_LBL_BS_OUT_HDR,
                    ID_EDT_OUT_DIR,
                    ID_BTN_OUT_DIR,
                    ID_LBL_BS_GET_HDR,
                    ID_LBL_OUTPUTS,
                    ID_LBL_BS_CHECK_HDR,
                    ID_BTN_VALIDATE,
                    ID_BTN_VERIFY,
                    ID_CHK_SANDBOX,
                    ID_BTN_TEST,
                    ID_LBL_BS_CONSOLE_HDR,
                    ID_OUTPUT,
                    ID_BTN_OPEN_OUTPUT,
                    ID_BTN_BS_BACK,
                    ID_BTN_BUILD,
                ]);
            }
            Page::Building => {
                v.extend(nav);
                v.extend([
                    ID_LBL_ING_TITLE,
                    ID_LBL_ING_SUB,
                    ID_LBL_ING_PHASE,
                    ID_PROGRESS,
                    ID_LBL_ING_PCT,
                    ID_OUTPUT,
                    ID_BTN_CLEAR,
                ]);
            }
            Page::BuildDone => {
                v.extend(nav);
                v.extend([
                    ID_LBL_DONE_TITLE,
                    ID_LBL_DONE_SUB,
                    ID_LBL_DONE_SUMMARY,
                    ID_LBL_DONE_REASON,
                    ID_BTN_DONE_INSTALLER,
                    ID_BTN_DONE_FOLDER,
                    ID_BTN_DONE_AGAIN,
                    ID_BTN_DONE_BACK,
                ]);
            }
            Page::Settings => {
                v.extend(nav);
                v.extend([
                    ID_LBL_SET_TITLE,
                    ID_LBL_SET_RECENTS_HDR,
                    ID_LBL_SET_RECENTS,
                    ID_BTN_CLEAR_RECENTS,
                    ID_LBL_SET_DIAG_HDR,
                    ID_LBL_SET_LOG,
                    ID_BTN_OPEN_LOG,
                    ID_LBL_SET_SETTINGS,
                    ID_LBL_SET_ABOUT_HDR,
                    ID_LBL_SET_VERSION,
                    ID_BTN_ABOUT,
                    ID_LBL_SET_FACTS,
                ]);
            }
        }
        v
    }

    /// Switches the window to a page: records it, runs the page's
    /// entry refresh, then lays out (which shows/hides and
    /// positions) and repaints.
    fn show_page(hwnd: HWND, page: Page) {
        with_state(|s| s.page = page);
        match page {
            Page::Home | Page::ProjectList => fill_recents(hwnd),
            Page::EditorFiles => ensure_scan(hwnd),
            Page::EditorOptions => refresh_loc_note(hwnd),
            Page::BuildSettings => {
                unsafe { harvest_fields(hwnd) };
                refresh_outputs_label(hwnd);
            }
            Page::BuildDone => fill_done(hwnd),
            Page::Settings => fill_settings_page(hwnd),
            Page::NewProject => refresh_np(hwnd),
            _ => {}
        }
        unsafe { layout(hwnd) };
        unsafe {
            InvalidateRect(hwnd, std::ptr::null(), 1);
        }
    }

    /// Starts the folder scan when the Files page is entered with a
    /// folder that has not been scanned (or has changed).
    fn ensure_scan(hwnd: HWND) {
        if is_busy() {
            return;
        }
        let stale = with_state(|s| {
            s.doc.files_dir.is_some() && (s.scan.is_none() || s.entries_dir != s.doc.files_dir)
        })
        .unwrap_or(false);
        if stale {
            start_scan(hwnd);
        }
    }

    /// Positions every control from the client size, the current
    /// DPI scale, and the current page. Also the visibility pass:
    /// controls not in the page's set are hidden.
    unsafe fn layout(hwnd: HWND) {
        let dpi = with_state(|s| s.dpi).unwrap_or(96);
        let s = |v: i32| scale(v, dpi);
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        let (w, h) = (client.right, client.bottom);
        let page = with_state(|s| s.page).unwrap_or(Page::Home);

        unsafe fn mv(hwnd: HWND, id: usize, x: i32, y: i32, w: i32, h: i32) {
            let ctrl = GetDlgItem(hwnd, id as i32);
            if !ctrl.is_null() {
                MoveWindow(ctrl, x, y, w.max(0), h.max(0), 1);
            }
        }
        unsafe fn set_col(hwnd: HWND, id: usize, col: usize, width: i32) {
            let ctrl = GetDlgItem(hwnd, id as i32);
            if !ctrl.is_null() {
                SendMessageW(ctrl, LVM_SETCOLUMNWIDTH, col, width.max(0) as LPARAM);
            }
        }

        // Visibility first.
        let visible = visible_set(page);
        for id in ALL_PAGE_CONTROLS {
            set_vis(hwnd, *id, visible.contains(id));
        }
        // The New Project steps and the Finished page's variants
        // narrow the set further.
        if page == Page::NewProject {
            let step = with_state(|s| s.np_step).unwrap_or(0);
            for id in [
                ID_LBL_NP_NAME,
                ID_EDT_NP_NAME,
                ID_LBL_NP_VERSION,
                ID_EDT_NP_VERSION,
                ID_LBL_NP_PUBLISHER,
                ID_EDT_NP_PUBLISHER,
            ] {
                set_vis(hwnd, id, step == 0);
            }
            for id in [
                ID_LBL_NP_FOLDER,
                ID_EDT_NP_FOLDER,
                ID_BTN_NP_BROWSE,
                ID_LBL_NP_FOLDER_NOTE,
            ] {
                set_vis(hwnd, id, step == 1);
            }
            set_vis(hwnd, ID_LBL_NP_SUMMARY, step == 2);
            set_vis(hwnd, ID_BTN_NP_BACK, step > 0);
        }
        if page == Page::BuildDone {
            let ok = with_state(|s| s.done.as_ref().map(|d| d.ok))
                .flatten()
                .unwrap_or(true);
            set_vis(hwnd, ID_LBL_DONE_SUMMARY, ok);
            set_vis(hwnd, ID_BTN_DONE_INSTALLER, ok);
            set_vis(hwnd, ID_BTN_DONE_AGAIN, ok);
            set_vis(hwnd, ID_LBL_DONE_REASON, !ok);
            set_vis(hwnd, ID_BTN_DONE_BACK, !ok);
        }

        // Footer: separator, status line, Cancel.
        let footer_top = h - s(48);
        mv(hwnd, ID_FOOTER_SEP, 0, footer_top, w, s(1));
        mv(
            hwnd,
            ID_LBL_STATUS,
            s(16),
            footer_top + s(14),
            (w - s(140)).max(s(80)),
            s(20),
        );
        mv(
            hwnd,
            ID_BTN_CANCEL,
            w - s(120),
            footer_top + s(8),
            s(104),
            s(32),
        );
        let content_bottom = footer_top - s(16);

        // Nav rail (the New Project flow replaces it with its step
        // rail).
        let rail_w = if page == Page::NewProject { 0 } else { s(176) };
        if rail_w > 0 {
            for (i, id) in [ID_NAV_HOME, ID_NAV_PROJECTS, ID_NAV_BUILD, ID_NAV_SETTINGS]
                .iter()
                .enumerate()
            {
                mv(
                    hwnd,
                    *id,
                    s(12),
                    s(20) + i as i32 * s(44),
                    rail_w - s(24),
                    s(36),
                );
            }
        }
        let cx = rail_w + s(28);
        let cw = (w - cx - s(24)).max(s(200));

        // Editor chrome geometry (tabs left, commands right, title
        // block under them).
        let editor_chrome = |hwnd: HWND| {
            mv(hwnd, ID_TAB_FILES, cx, s(8), s(160), s(30));
            mv(hwnd, ID_TAB_OPTIONS, cx + s(168), s(8), s(170), s(30));
            mv(hwnd, ID_TAB_REQUIREMENTS, cx + s(346), s(8), s(150), s(30));
            mv(
                hwnd,
                ID_BTN_TB_VALIDATE,
                cx + cw - s(128),
                s(8),
                s(128),
                s(30),
            );
            mv(
                hwnd,
                ID_BTN_TB_SAVEAS,
                cx + cw - s(236),
                s(8),
                s(100),
                s(30),
            );
            mv(hwnd, ID_BTN_TB_SAVE, cx + cw - s(328), s(8), s(84), s(30));
            mv(hwnd, ID_BTN_TB_OPEN, cx + cw - s(432), s(8), s(96), s(30));
            mv(hwnd, ID_LBL_APPVER, cx, s(44), cw, s(18));
        };

        match page {
            Page::Home => {
                mv(hwnd, ID_LBL_HOME_TITLE, cx, s(12), cw, s(38));
                mv(hwnd, ID_LBL_HOME_SUB, cx, s(54), cw, s(20));
                mv(hwnd, ID_BTN_NEW_PROJECT, cx, s(94), s(170), s(38));
                mv(hwnd, ID_LBL_RECENT_HDR, cx, s(162), s(240), s(24));
                mv(
                    hwnd,
                    ID_BTN_VIEW_ALL,
                    cx + cw - s(110),
                    s(158),
                    s(110),
                    s(28),
                );
                let list_h = (content_bottom - s(196)).clamp(s(120), s(280));
                mv(hwnd, ID_RECENTS, cx, s(196), cw, list_h);
                set_col(hwnd, ID_RECENTS, 1, s(110));
                set_col(hwnd, ID_RECENTS, 2, s(170));
                set_col(hwnd, ID_RECENTS, 0, cw - s(110) - s(170) - s(17));
            }
            Page::ProjectList => {
                mv(hwnd, ID_LBL_PL_TITLE, cx, s(12), cw, s(38));
                mv(hwnd, ID_LBL_PL_SUB, cx, s(54), cw, s(20));
                mv(hwnd, ID_LBL_PL_CURRENT, cx, s(86), cw, s(20));
                let list_h = content_bottom - s(118) - s(52);
                mv(hwnd, ID_RECENTS, cx, s(118), cw, list_h);
                set_col(hwnd, ID_RECENTS, 1, s(110));
                set_col(hwnd, ID_RECENTS, 2, s(170));
                set_col(hwnd, ID_RECENTS, 0, cw - s(110) - s(170) - s(17));
                let by = content_bottom - s(40);
                mv(hwnd, ID_BTN_PL_OPEN, cx, by, s(120), s(32));
                mv(hwnd, ID_BTN_PL_REMOVE, cx + s(132), by, s(180), s(32));
                mv(hwnd, ID_BTN_PL_PICK, cx + s(324), by, s(220), s(32));
                mv(hwnd, ID_BTN_PL_NEW, cx + cw - s(170), by, s(170), s(32));
            }
            Page::NewProject => {
                let mx = s(48);
                let form_x = mx + s(250);
                let form_w = (w - form_x - mx).max(s(200));
                mv(hwnd, ID_LBL_NP_TITLE, form_x, s(12), form_w, s(38));
                mv(hwnd, ID_LBL_NP_SUB, form_x, s(54), form_w, s(20));
                // Step rail: labels here, circles painted in WM_PAINT.
                let mut circles = Vec::new();
                for (i, id) in [ID_LBL_NP_STEP1, ID_LBL_NP_STEP2, ID_LBL_NP_STEP3]
                    .iter()
                    .enumerate()
                {
                    let y = s(120) + i as i32 * s(44);
                    mv(hwnd, *id, mx + s(36), y, s(200), s(24));
                    circles.push((mx + s(12), y + s(12), s(11)));
                }
                with_state(|s| s.np_circles = circles);
                // Step 1 fields.
                mv(hwnd, ID_LBL_NP_NAME, form_x, s(120), form_w, s(20));
                mv(
                    hwnd,
                    ID_EDT_NP_NAME,
                    form_x,
                    s(146),
                    form_w.min(s(480)),
                    s(28),
                );
                mv(hwnd, ID_LBL_NP_VERSION, form_x, s(190), form_w, s(20));
                mv(hwnd, ID_EDT_NP_VERSION, form_x, s(216), s(220), s(28));
                mv(hwnd, ID_LBL_NP_PUBLISHER, form_x, s(262), form_w, s(20));
                mv(
                    hwnd,
                    ID_EDT_NP_PUBLISHER,
                    form_x,
                    s(288),
                    form_w.min(s(480)),
                    s(28),
                );
                // Step 2 fields.
                mv(hwnd, ID_LBL_NP_FOLDER, form_x, s(120), form_w, s(20));
                mv(
                    hwnd,
                    ID_EDT_NP_FOLDER,
                    form_x,
                    s(146),
                    form_w - s(112),
                    s(28),
                );
                mv(
                    hwnd,
                    ID_BTN_NP_BROWSE,
                    form_x + form_w - s(100),
                    s(146),
                    s(100),
                    s(28),
                );
                mv(hwnd, ID_LBL_NP_FOLDER_NOTE, form_x, s(190), form_w, s(44));
                // Step 3.
                mv(hwnd, ID_LBL_NP_SUMMARY, form_x, s(120), form_w, s(240));
                // Flow footer.
                let by = content_bottom - s(38);
                mv(hwnd, ID_BTN_NP_CANCEL, mx, by, s(110), s(34));
                mv(hwnd, ID_BTN_NP_BACK, w - mx - s(240), by, s(110), s(34));
                mv(hwnd, ID_BTN_NP_NEXT, w - mx - s(118), by, s(118), s(34));
            }
            Page::EditorFiles => {
                editor_chrome(hwnd);
                mv(hwnd, ID_LBL_FILES_TITLE, cx, s(72), cw, s(38));
                mv(hwnd, ID_LBL_FILES_SUB, cx, s(114), cw, s(20));
                let left_w = s(340);
                mv(hwnd, ID_LBL_FOLDER_PATH_LBL, cx, s(148), left_w, s(20));
                mv(hwnd, ID_EDT_FILES_DIR, cx, s(172), left_w - s(96), s(26));
                mv(
                    hwnd,
                    ID_BTN_FILES_DIR,
                    cx + left_w - s(88),
                    s(172),
                    s(88),
                    s(26),
                );
                mv(hwnd, ID_LBL_FILES_PANE_HDR, cx, s(210), left_w, s(20));
                let tree_h = content_bottom - s(234) - s(48);
                mv(hwnd, ID_TREE, cx, s(234), left_w, tree_h);
                {
                    let dest_w = s(128);
                    let measured = with_state(|s| s.tree_col_w).unwrap_or(0);
                    let col0 = (left_w - dest_w).max(measured);
                    set_col(hwnd, ID_TREE, 0, col0);
                    set_col(hwnd, ID_TREE, 1, dest_w);
                }
                mv(
                    hwnd,
                    ID_BTN_PROGRAM_FOLDER,
                    cx,
                    content_bottom - s(40),
                    s(170),
                    s(32),
                );
                let x2 = cx + left_w + s(24);
                let w2 = (cw - left_w - s(24)).max(s(200));
                mv(hwnd, ID_LBL_PACKAGE_HDR, x2, s(148), w2, s(20));
                let list_h = content_bottom - s(172) - s(150);
                mv(hwnd, ID_LIST, x2, s(172), w2, list_h);
                {
                    let total = w2 - s(17);
                    set_col(hwnd, ID_LIST, 1, s(150));
                    set_col(hwnd, ID_LIST, 2, s(95));
                    set_col(hwnd, ID_LIST, 3, s(115));
                    set_col(hwnd, ID_LIST, 0, total - s(150) - s(95) - s(115));
                }
                mv(hwnd, ID_LBL_TOTALS, x2, content_bottom - s(142), w2, s(38));
                // A mini console under the table: scan warnings and
                // open/save reports land here (the same control is
                // the full log on the Build pages).
                mv(hwnd, ID_OUTPUT, x2, content_bottom - s(96), w2, s(56));
                mv(
                    hwnd,
                    ID_BTN_REMOVE,
                    x2 + w2 - s(160),
                    content_bottom - s(40),
                    s(160),
                    s(32),
                );
            }
            Page::EditorOptions => {
                editor_chrome(hwnd);
                mv(hwnd, ID_LBL_OPT_TITLE, cx, s(72), cw, s(38));
                mv(hwnd, ID_LBL_OPT_SUB, cx, s(114), cw, s(20));
                let right_w = s(364);
                let form_w = (cw - right_w - s(24)).max(s(300));
                let rx = cx + form_w + s(24);
                let label_w = s(150);
                let edit_x = cx + label_w + s(6);
                let edit_w = form_w - label_w - s(6);
                mv(hwnd, ID_LBL_OPT_DETAILS, cx, s(148), form_w, s(20));
                for (i, label_id) in PANEL_LABELS.iter().enumerate() {
                    let y = s(176) + i as i32 * s(29);
                    mv(hwnd, *label_id, cx, y + s(3), label_w, s(20));
                    let edit_id = [
                        ID_EDT_NAME,
                        ID_EDT_VERSION,
                        ID_EDT_PUBLISHER,
                        ID_EDT_PRIMARY,
                        ID_EDT_INSTALL_LOC,
                        ID_EDT_SM_FOLDER,
                        ID_EDT_ASSOC,
                    ][i];
                    mv(hwnd, edit_id, edit_x, y, edit_w, s(26));
                }
                let icon_y = s(176) + 7 * s(29);
                mv(hwnd, ID_LBL_OPT_ICON, cx, icon_y + s(3), label_w, s(20));
                mv(
                    hwnd,
                    ID_LBL_ICON_VALUE,
                    edit_x,
                    icon_y + s(3),
                    edit_w,
                    s(20),
                );
                mv(hwnd, ID_LBL_LOC_NOTE, cx, icon_y + s(28), form_w, s(20));
                let sc_y = icon_y + s(56);
                mv(hwnd, ID_LBL_OPT_SHORTCUTS, cx, sc_y, form_w, s(20));
                mv(hwnd, ID_CHK_DESKTOP, cx, sc_y + s(26), form_w / 2, s(24));
                mv(
                    hwnd,
                    ID_CHK_STARTMENU,
                    cx + form_w / 2,
                    sc_y + s(26),
                    form_w - form_w / 2,
                    s(24),
                );
                mv(hwnd, ID_CHK_PATH, cx, sc_y + s(52), form_w, s(24));
                mv(hwnd, ID_LBL_LAUNCH, cx, sc_y + s(82), label_w, s(20));
                mv(
                    hwnd,
                    ID_CHK_LAUNCH_OFFER,
                    edit_x,
                    sc_y + s(80),
                    s(104),
                    s(24),
                );
                mv(
                    hwnd,
                    ID_CHK_LAUNCH_DEFAULT,
                    edit_x + s(112),
                    sc_y + s(80),
                    edit_w - s(112),
                    s(24),
                );
                mv(hwnd, ID_LBL_LOGIN, cx, sc_y + s(110), label_w, s(20));
                mv(
                    hwnd,
                    ID_CHK_LOGIN_OFFER,
                    edit_x,
                    sc_y + s(108),
                    s(104),
                    s(24),
                );
                mv(
                    hwnd,
                    ID_CHK_LOGIN_DEFAULT,
                    edit_x + s(112),
                    sc_y + s(108),
                    edit_w - s(112),
                    s(24),
                );
                let dd_y = sc_y + s(138);
                mv(hwnd, ID_LBL_DATADIRS, cx, dd_y + s(3), label_w, s(20));
                mv(hwnd, ID_EDT_DATA_DIRS, edit_x, dd_y, edit_w, s(26));
                mv(hwnd, ID_LBL_DATA_NOTE, cx, dd_y + s(34), form_w, s(38));
                // Right column: live preview, then the advanced
                // editor flexing to the bottom.
                mv(hwnd, ID_LBL_PREVIEW_HDR, rx, s(148), right_w, s(20));
                let preview_rect = (rx, s(174), right_w, s(252));
                with_state(|st| {
                    st.preview_rect = Some(preview_rect);
                });
                let power_y = s(442);
                mv(hwnd, ID_LBL_POWER, rx, power_y, right_w, s(20));
                mv(
                    hwnd,
                    ID_EDT_POWER,
                    rx,
                    power_y + s(24),
                    right_w,
                    (content_bottom - power_y - s(24)).max(s(40)),
                );
                mv(
                    hwnd,
                    ID_BTN_PREVIEW,
                    cx,
                    content_bottom - s(40),
                    s(180),
                    s(32),
                );
                mv(
                    hwnd,
                    ID_BTN_SCHEMA,
                    cx + s(192),
                    content_bottom - s(40),
                    s(180),
                    s(32),
                );
            }
            Page::EditorRequirements => {
                editor_chrome(hwnd);
                mv(hwnd, ID_LBL_REQ_TITLE, cx, s(72), cw, s(38));
                mv(hwnd, ID_LBL_REQ_SUB, cx, s(114), cw, s(20));
                mv(hwnd, ID_LBL_REQ_WIN_HDR, cx, s(148), cw, s(20));
                mv(hwnd, ID_CHK_WINREQ, cx, s(176), s(340), s(26));
                mv(hwnd, ID_LBL_WINREQ_TAG, cx + s(352), s(176), s(100), s(26));
                mv(hwnd, ID_LBL_REQ_RT_HDR, cx, s(228), cw, s(20));
                for (i, (chk_id, tag_id)) in [
                    (ID_CHK_RT1, ID_LBL_RT_TAG1),
                    (ID_CHK_RT2, ID_LBL_RT_TAG2),
                    (ID_CHK_RT3, ID_LBL_RT_TAG3),
                    (ID_CHK_RT4, ID_LBL_RT_TAG4),
                ]
                .iter()
                .enumerate()
                {
                    let y = s(256) + i as i32 * s(34);
                    mv(hwnd, *chk_id, cx, y, s(400), s(26));
                    mv(hwnd, *tag_id, cx + s(412), y, s(110), s(26));
                }
                mv(hwnd, ID_LBL_REQ_NOTE, cx, s(404), cw.min(s(620)), s(46));
                mv(
                    hwnd,
                    ID_BTN_REQ_BACK,
                    cx,
                    content_bottom - s(40),
                    s(110),
                    s(32),
                );
                mv(
                    hwnd,
                    ID_BTN_REQ_NEXT,
                    cx + cw - s(130),
                    content_bottom - s(40),
                    s(130),
                    s(32),
                );
            }
            Page::BuildSettings => {
                mv(hwnd, ID_LBL_BS_TITLE, cx, s(12), cw, s(38));
                mv(hwnd, ID_LBL_BS_SUB, cx, s(54), cw, s(20));
                mv(hwnd, ID_LBL_BS_ARCH_HDR, cx, s(92), cw, s(20));
                mv(hwnd, ID_LBL_BS_ARCH, cx, s(118), cw, s(22));
                mv(hwnd, ID_LBL_BS_COMP_HDR, cx, s(156), cw, s(20));
                mv(hwnd, ID_CHK_COMPRESS, cx, s(182), s(480), s(26));
                mv(hwnd, ID_LBL_BS_OUT_HDR, cx, s(224), cw, s(20));
                mv(hwnd, ID_EDT_OUT_DIR, cx, s(250), cw - s(112), s(26));
                mv(
                    hwnd,
                    ID_BTN_OUT_DIR,
                    cx + cw - s(100),
                    s(250),
                    s(100),
                    s(26),
                );
                mv(hwnd, ID_LBL_BS_GET_HDR, cx, s(292), cw, s(20));
                mv(hwnd, ID_LBL_OUTPUTS, cx, s(318), cw, s(62));
                mv(hwnd, ID_LBL_BS_CHECK_HDR, cx, s(396), cw, s(20));
                mv(hwnd, ID_BTN_VALIDATE, cx, s(422), s(150), s(30));
                mv(hwnd, ID_CHK_SANDBOX, cx + s(162), s(422), s(190), s(30));
                mv(hwnd, ID_BTN_VERIFY, cx + s(364), s(422), s(150), s(30));
                mv(hwnd, ID_BTN_TEST, cx + s(526), s(422), s(140), s(30));
                mv(hwnd, ID_LBL_BS_CONSOLE_HDR, cx, s(470), cw, s(20));
                mv(
                    hwnd,
                    ID_OUTPUT,
                    cx,
                    s(496),
                    cw,
                    (content_bottom - s(496) - s(52)).max(s(40)),
                );
                let by = content_bottom - s(40);
                mv(hwnd, ID_BTN_BS_BACK, cx, by, s(110), s(32));
                mv(
                    hwnd,
                    ID_BTN_OPEN_OUTPUT,
                    cx + cw - s(360),
                    by,
                    s(180),
                    s(32),
                );
                mv(hwnd, ID_BTN_BUILD, cx + cw - s(168), by, s(168), s(32));
            }
            Page::Building => {
                mv(hwnd, ID_LBL_ING_TITLE, cx, s(12), cw, s(38));
                mv(hwnd, ID_LBL_ING_SUB, cx, s(54), cw, s(20));
                mv(hwnd, ID_LBL_ING_PHASE, cx, s(108), cw - s(70), s(22));
                mv(hwnd, ID_LBL_ING_PCT, cx + cw - s(60), s(108), s(60), s(22));
                mv(hwnd, ID_PROGRESS, cx, s(136), cw, s(22));
                mv(
                    hwnd,
                    ID_OUTPUT,
                    cx,
                    s(178),
                    cw,
                    (content_bottom - s(178) - s(52)).max(s(40)),
                );
                mv(
                    hwnd,
                    ID_BTN_CLEAR,
                    cx,
                    content_bottom - s(40),
                    s(100),
                    s(32),
                );
            }
            Page::BuildDone => {
                let badge = s(68);
                let bx = cx + (cw - badge) / 2;
                let badge_rect = (bx, s(84), badge, badge);
                with_state(|st| st.badge_rect = Some(badge_rect));
                mv(hwnd, ID_LBL_DONE_TITLE, cx, s(172), cw, s(38));
                mv(hwnd, ID_LBL_DONE_SUB, cx, s(214), cw, s(20));
                let sum_w = s(560).min(cw);
                mv(
                    hwnd,
                    ID_LBL_DONE_SUMMARY,
                    cx + (cw - sum_w) / 2,
                    s(256),
                    sum_w,
                    s(130),
                );
                mv(
                    hwnd,
                    ID_LBL_DONE_REASON,
                    cx + (cw - sum_w) / 2,
                    s(256),
                    sum_w,
                    s(130),
                );
                let ok = with_state(|s| s.done.as_ref().map(|d| d.ok))
                    .flatten()
                    .unwrap_or(true);
                if ok {
                    let total = s(190) + s(12) + s(140) + s(12) + s(140);
                    let x0 = cx + (cw - total) / 2;
                    mv(hwnd, ID_BTN_DONE_INSTALLER, x0, s(410), s(190), s(34));
                    mv(hwnd, ID_BTN_DONE_FOLDER, x0 + s(202), s(410), s(140), s(34));
                    mv(hwnd, ID_BTN_DONE_AGAIN, x0 + s(354), s(410), s(140), s(34));
                } else {
                    let total = s(200) + s(12) + s(140);
                    let x0 = cx + (cw - total) / 2;
                    mv(hwnd, ID_BTN_DONE_BACK, x0, s(410), s(200), s(34));
                    mv(hwnd, ID_BTN_DONE_FOLDER, x0 + s(212), s(410), s(140), s(34));
                }
            }
            Page::Settings => {
                mv(hwnd, ID_LBL_SET_TITLE, cx, s(12), cw, s(38));
                mv(hwnd, ID_LBL_SET_RECENTS_HDR, cx, s(72), cw, s(20));
                mv(hwnd, ID_LBL_SET_RECENTS, cx, s(100), cw, s(20));
                mv(hwnd, ID_BTN_CLEAR_RECENTS, cx, s(128), s(150), s(30));
                mv(hwnd, ID_LBL_SET_DIAG_HDR, cx, s(178), cw, s(20));
                mv(hwnd, ID_LBL_SET_LOG, cx, s(206), cw, s(20));
                mv(hwnd, ID_BTN_OPEN_LOG, cx, s(234), s(150), s(30));
                mv(hwnd, ID_LBL_SET_SETTINGS, cx, s(272), cw, s(20));
                mv(hwnd, ID_LBL_SET_ABOUT_HDR, cx, s(312), cw, s(20));
                mv(hwnd, ID_LBL_SET_VERSION, cx, s(340), cw, s(20));
                mv(hwnd, ID_BTN_ABOUT, cx, s(368), s(220), s(30));
                mv(hwnd, ID_LBL_SET_FACTS, cx, s(416), cw, s(40));
            }
        }
    }

    // ---- list helpers ----

    /// Inserts one list-view row (cell 0 inserts, the rest set text).
    unsafe fn lv_insert(list: HWND, row: i32, cells: &[String]) {
        for (col_idx, cell) in cells.iter().enumerate() {
            let mut wide: Vec<u16> = cell.encode_utf16().chain(std::iter::once(0)).collect();
            let item = LVITEMW {
                mask: LVIF_TEXT,
                iItem: row,
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
                SendMessageW(
                    list,
                    LVM_SETITEMTEXTW,
                    row as usize,
                    &item as *const _ as LPARAM,
                );
            }
        }
    }

    /// The destination column's text for a package-relative folder
    /// ("" = the program folder itself).
    fn dest_text(dir: &str) -> String {
        if dir.is_empty() {
            "Program folder".to_string()
        } else {
            format!("Program folder\\{}", dir.replace('/', "\\"))
        }
    }

    // ---- files table + folder listing ----

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
            lv_insert(list, row_idx as i32, row);
        }
        fill_tree(hwnd);
        update_totals(hwnd);
    }

    /// Rebuilds the Files page's folder listing from the last scan
    /// (falling back to the document entries before the first scan
    /// lands): the whole program folder, directories included, each
    /// row carrying a tick box for inclusion in the package. The
    /// center table shows only what will be packed.
    unsafe fn fill_tree(hwnd: HWND) {
        let tree = GetDlgItem(hwnd, ID_TREE as i32);
        if tree.is_null() {
            return;
        }
        let (all_files, entries) = with_state(|s| {
            let files = match &s.scan {
                Some(scan) => scan.all_files.clone(),
                None => s.doc.entries.clone(),
            };
            (files, s.doc.entries.clone())
        })
        .unwrap_or_default();
        let listing = tree_entries(&all_files);
        SendMessageW(tree, LVM_DELETEALLITEMS, 0, 0);
        with_state(|s| s.suppress_tree_check = true);
        let dpi = with_state(|s| s.dpi).unwrap_or(96);
        let mut rows: Vec<TreeRowInfo> = Vec::with_capacity(listing.len());
        let mut dir_stack: Vec<(usize, String)> = Vec::new();
        let mut widest: i32 = 0;
        for (idx, entry) in listing.iter().enumerate() {
            while dir_stack
                .last()
                .map(|(d, _)| *d >= entry.depth)
                .unwrap_or(false)
            {
                dir_stack.pop();
            }
            let (info, dest) = if let Some(rel) = &entry.rel {
                let dir = match rel.rfind('/') {
                    Some(i) => rel[..i].to_string(),
                    None => String::new(),
                };
                (
                    TreeRowInfo {
                        rel: Some(rel.clone()),
                        files: vec![rel.clone()],
                    },
                    dest_text(&dir),
                )
            } else {
                let name = entry.label.trim_end_matches('/');
                let prefix = match dir_stack.last() {
                    Some((_, p)) => format!("{p}/{name}"),
                    None => name.to_string(),
                };
                let files: Vec<String> = all_files
                    .iter()
                    .filter(|f| f.starts_with(&format!("{prefix}/")))
                    .cloned()
                    .collect();
                dir_stack.push((entry.depth, prefix.clone()));
                (TreeRowInfo { rel: None, files }, dest_text(&prefix))
            };
            let checked = !info.files.is_empty() && info.files.iter().all(|f| entries.contains(f));
            let mut wide: Vec<u16> = entry
                .label
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let text_w = SendMessageW(tree, LVM_GETSTRINGWIDTHW, 0, wide.as_mut_ptr() as LPARAM);
            let row_w = text_w as i32 + scale(20, dpi) * entry.depth as i32 + scale(16, dpi);
            widest = widest.max(row_w);
            let item = LVITEMW {
                mask: LVIF_TEXT | LVIF_INDENT | LVIF_STATE,
                iItem: idx as i32,
                iSubItem: 0,
                state: (if checked { 2 } else { 1 }) << 12,
                stateMask: LVIS_STATEIMAGEMASK,
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
            let mut dest_wide: Vec<u16> = dest.encode_utf16().chain(std::iter::once(0)).collect();
            let sub = LVITEMW {
                mask: LVIF_TEXT,
                iItem: idx as i32,
                iSubItem: 1,
                state: 0,
                stateMask: 0,
                pszText: dest_wide.as_mut_ptr(),
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
            SendMessageW(tree, LVM_SETITEMTEXTW, idx, &sub as *const _ as LPARAM);
            rows.push(info);
        }
        with_state(|s| {
            s.tree_rows = rows;
            s.tree_col_w = widest;
            s.suppress_tree_check = false;
        });
        // Apply the measured first-column width right away (layout
        // does the same on the next resize).
        let mut tree_rect: RECT = std::mem::zeroed();
        GetClientRect(tree, &mut tree_rect);
        let dpi2 = dpi;
        SendMessageW(
            tree,
            LVM_SETCOLUMNWIDTH,
            0,
            ((tree_rect.right - scale(128, dpi2)).max(widest)) as LPARAM,
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

    /// Table selection: remember the file so the totals line
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

    /// Listing selection: remember the file, mirror the selection
    /// into the table, and scroll it into view.
    unsafe fn on_tree_selection(hwnd: HWND, index: i32) {
        if index < 0 {
            return;
        }
        let rel =
            with_state(|s| s.tree_rows.get(index as usize).and_then(|r| r.rel.clone())).flatten();
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

    /// A listing tick box changed: add or remove that file (or every
    /// file under that folder) from the package list.
    fn on_tree_check(hwnd: HWND, index: i32, checked: bool) {
        if index < 0 || with_state(|s| s.suppress_tree_check).unwrap_or(false) {
            return;
        }
        let changed = with_state(|s| {
            let Some(row) = s.tree_rows.get(index as usize) else {
                return false;
            };
            let files = row.files.clone();
            if files.is_empty() {
                return false;
            }
            if checked {
                for f in &files {
                    if !s.doc.entries.contains(f) {
                        s.doc.entries.push(f.clone());
                    }
                }
            } else {
                s.doc.entries.retain(|e| !files.contains(e));
            }
            s.doc.entries.sort();
            true
        })
        .unwrap_or(false);
        if !changed {
            return;
        }
        let all = with_state(|s| s.scan.as_ref().map(|sc| sc.all_files.clone())).flatten();
        if let Some(all) = all {
            with_state(|s| s.doc.sync_files_field(&all));
        }
        mark_dirty(hwnd);
        unsafe { fill_list(hwnd) };
        let count = with_state(|s| s.doc.entries.len()).unwrap_or(0);
        set_status(
            hwnd,
            StatusKind::Neutral,
            &format!("{count} files in the package"),
        );
    }

    // ---- recent projects ----

    /// Fills the shared recents list (the Home page shows the first
    /// five; the Projects page shows them all), and the Projects
    /// page's "currently open" line.
    fn fill_recents(hwnd: HWND) {
        let list = unsafe { GetDlgItem(hwnd, ID_RECENTS as i32) };
        if list.is_null() {
            return;
        }
        let (recents, current, page) = with_state(|s| {
            (
                s.settings.recents.clone(),
                s.doc.manifest_path.clone(),
                s.page,
            )
        })
        .unwrap_or_else(|| (Vec::new(), None, Page::Home));
        unsafe {
            SendMessageW(list, LVM_DELETEALLITEMS, 0, 0);
        }
        let limit = if page == Page::Home { 5 } else { recents.len() };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        for (i, r) in recents.iter().take(limit).enumerate() {
            let name = if r.name.is_empty() {
                r.path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| r.path.display().to_string())
            } else {
                r.name.clone()
            };
            let modified = std::fs::metadata(&r.path)
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| format!("Modified {}", relative_time_text(now, d.as_secs())))
                .unwrap_or_else(|| "—".to_string());
            let cells = [name, r.version.clone(), modified];
            unsafe { lv_insert(list, i as i32, &cells) };
        }
        if page == Page::ProjectList {
            let text = match &current {
                Some(p) => format!("Currently open: {}", p.display()),
                None => {
                    "No project file is open yet — pick one above, open a manifest file, or start a new project."
                        .to_string()
                }
            };
            unsafe { set_text(hwnd, ID_LBL_PL_CURRENT, &text) };
        }
    }

    /// Records a project in the recent list (front, fresh
    /// name/version) and saves the settings right away, so the list
    /// survives even if the Builder is closed uncleanly.
    fn touch_recent(hwnd: HWND, path: &std::path::Path) {
        let (name, version) = with_state(|s| {
            (
                s.doc.manifest.app_name.clone(),
                s.doc.manifest.version.clone(),
            )
        })
        .unwrap_or_default();
        with_state(|s| {
            s.settings.touch_recent(path, &name, &version);
            s.settings.save();
        });
        let _ = hwnd;
    }

    /// The recents row the user selected, if any.
    fn selected_recent_index(hwnd: HWND) -> Option<usize> {
        let list = unsafe { GetDlgItem(hwnd, ID_RECENTS as i32) };
        if list.is_null() {
            return None;
        }
        let idx = unsafe {
            SendMessageW(
                list,
                LVM_GETNEXTITEM,
                -1isize as WPARAM,
                LVNI_SELECTED as LPARAM,
            )
        };
        if idx < 0 {
            None
        } else {
            Some(idx as usize)
        }
    }

    // ---- jobs ----

    fn set_busy(hwnd: HWND, busy: bool) {
        with_state(|s| s.busy = busy);
        for id in ACTION_IDS {
            unsafe { set_enabled(hwnd, *id, !busy) };
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
                if with_state(|s| s.page) == Some(Page::Building) && total > 0 {
                    let pct = (done * 100 / total) as i32;
                    unsafe {
                        let bar = GetDlgItem(hwnd, ID_PROGRESS as i32);
                        if !bar.is_null() {
                            SendMessageW(bar, PBM_SETPOS_MSG, pct as WPARAM, 0);
                        }
                        set_text(hwnd, ID_LBL_ING_PCT, &format!("{pct}%"));
                        set_text(hwnd, ID_LBL_ING_PHASE, &kind.progress_text(done, total));
                    }
                }
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
                if with_state(|s| s.page) == Some(Page::NewProject) {
                    refresh_np(hwnd);
                }
            }
            JobKind::Pack => {
                append_build_log(hwnd, "Build cancelled — the unfinished files were removed.");
                set_status(
                    hwnd,
                    StatusKind::Neutral,
                    "Build cancelled — the unfinished files were removed",
                );
                show_page(hwnd, Page::BuildSettings);
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
        let gen = with_state(|s| {
            s.scan_gen += 1;
            s.scan_gen
        })
        .unwrap_or(0);
        let hwnd_value = hwnd as usize;
        let JobHooks { job, cancel, .. } = hooks;
        std::thread::spawn(move || {
            let result = scan_package_with(&dir, Some(&cancel))
                .map_err(|e| format!("Cannot scan {}: {e}", dir.display()));
            let result = if cancel.load(Ordering::Relaxed) {
                JobResult::Cancelled(JobKind::Scan)
            } else {
                JobResult::Scan { gen, result }
            };
            post_job(hwnd_value, &job, result);
        });
    }

    unsafe fn on_scan_done(hwnd: HWND, gen: u64, result: Result<ScanOutcome, String>) {
        // A scan for a document the user has already left (the New
        // Project flow was cancelled, another scan superseded it)
        // must not land on the current document.
        if with_state(|s| s.scan_gen) != Some(gen) {
            set_busy(hwnd, false);
            return;
        }
        set_busy(hwnd, false);
        match result {
            Ok(scan) => {
                let (count, missing) = with_state(|s| {
                    let dir_changed = s.entries_dir != s.doc.files_dir;
                    if s.doc.entries.is_empty() || dir_changed {
                        s.doc.entries = scan.all_files.clone();
                        s.entries_dir = s.doc.files_dir.clone();
                    }
                    s.doc.sync_files_field(&scan.all_files);
                    let rows = build_rows(&s.doc.entries, Some(&scan));
                    let missing = rows.iter().filter(|r| !r.on_disk).count();
                    let count = rows.len();
                    s.scan = Some(scan);
                    (count, missing)
                })
                .unwrap_or((0, 0));
                fill_list(hwnd);
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
                if with_state(|s| s.page) == Some(Page::NewProject) {
                    refresh_np(hwnd);
                }
            }
            Err(e) => {
                append_output(hwnd, &format!("Error: {e}"));
                set_status(
                    hwnd,
                    StatusKind::Error,
                    "Scan failed — see the output above",
                );
                if with_state(|s| s.page) == Some(Page::NewProject) {
                    refresh_np(hwnd);
                }
            }
        }
    }

    // ---- actions ----

    /// Opens a manifest file by path: reads it, swaps the document,
    /// records it in the recent projects, scans its folder, and
    /// lands in the editor. Shared by the file picker and the
    /// recent-projects list.
    unsafe fn open_manifest_path(hwnd: HWND, path: &std::path::Path) {
        if is_busy() {
            return;
        }
        // Build the document by hand (not via open_manifest) so the
        // folder scan — with its size estimates — happens once, on
        // the window's worker, instead of blocking the open.
        let text = match std::fs::read_to_string(path) {
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
                doc.manifest_path = Some(path.to_path_buf());
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
                touch_recent(hwnd, path);
                show_page(hwnd, Page::EditorFiles);
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
        open_manifest_path(hwnd, &path);
    }

    /// Opens the recent project at `index` in the shared recents
    /// list (Home shows the first five, the Projects page all — the
    /// list is most-recent-first on both, so the index is the same).
    unsafe fn on_open_recent(hwnd: HWND, index: usize) {
        let path = with_state(|s| s.settings.recents.get(index).map(|r| r.path.clone())).flatten();
        let Some(path) = path else {
            set_status(
                hwnd,
                StatusKind::Neutral,
                "Select a project in the list first",
            );
            return;
        };
        if !path.is_file() {
            append_output(
                hwnd,
                &format!(
                    "That project file is gone: {} — you can take it off the list with Remove from list.",
                    path.display()
                ),
            );
            set_status(
                hwnd,
                StatusKind::Error,
                "That project file no longer exists",
            );
            return;
        }
        open_manifest_path(hwnd, &path);
    }

    unsafe fn on_remove_recent(hwnd: HWND) {
        let Some(index) = selected_recent_index(hwnd) else {
            set_status(
                hwnd,
                StatusKind::Neutral,
                "Select a project in the list first",
            );
            return;
        };
        let removed = with_state(|s| {
            let path = s.settings.recents.get(index).map(|r| r.path.clone());
            if let Some(p) = &path {
                s.settings.remove_recent(p);
                s.settings.save();
            }
            path
        })
        .flatten();
        if let Some(p) = removed {
            set_status(
                hwnd,
                StatusKind::Neutral,
                &format!(
                    "Removed {} from the list — the file stays where it is",
                    p.display()
                ),
            );
        }
        fill_recents(hwnd);
    }

    unsafe fn on_clear_recents(hwnd: HWND) {
        with_state(|s| {
            s.settings.recents.clear();
            s.settings.save();
        });
        fill_recents(hwnd);
        fill_settings_page(hwnd);
        set_status(hwnd, StatusKind::Neutral, "Recent projects list cleared");
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
                touch_recent(hwnd, &path);
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
        refresh_outputs_label(hwnd);
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
        with_state(|s| s.build_started = Some(std::time::Instant::now()));
        // The Building page starts from a clean progress readout.
        unsafe {
            let bar = GetDlgItem(hwnd, ID_PROGRESS as i32);
            if !bar.is_null() {
                SendMessageW(bar, PBM_SETPOS_MSG, 0, 0);
            }
            set_text(hwnd, ID_LBL_ING_PCT, "0%");
            set_text(hwnd, ID_LBL_ING_PHASE, "Building…");
        }
        show_page(hwnd, Page::Building);
        append_build_log(
            hwnd,
            &format!(
                "Starting build of {} {}…",
                manifest.app_name, manifest.version
            ),
        );
        append_build_log(
            hwnd,
            &format!("Reading project settings from {}", files_dir.display()),
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

    /// "2m 34s" / "48s" — the Finished page's Build time readout.
    fn format_duration(d: std::time::Duration) -> String {
        let secs = d.as_secs();
        if secs < 60 {
            format!("{secs}s")
        } else {
            format!("{}m {:02}s", secs / 60, secs % 60)
        }
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
                append_build_log(
                    hwnd,
                    &format!(
                        "Wrote {} ({} bytes, sha256 {})",
                        outcome.setup.path.display(),
                        outcome.setup.size,
                        outcome.setup.sha256
                    ),
                );
                append_build_log(
                    hwnd,
                    &format!(
                        "Wrote {} ({} bytes, sha256 {})",
                        outcome.portable.path.display(),
                        outcome.portable.size,
                        outcome.portable.sha256
                    ),
                );
                append_build_log(hwnd, &format!("Wrote {}", outcome.sums_path.display()));
                append_build_log(hwnd, "Build completed successfully.");
                let build_time = with_state(|s| s.build_started.take())
                    .flatten()
                    .map(|t| format_duration(t.elapsed()))
                    .unwrap_or_else(|| "—".to_string());
                let (app_name, version) = with_state(|s| {
                    (
                        s.doc.manifest.app_name.clone(),
                        s.doc.manifest.version.clone(),
                    )
                })
                .unwrap_or_default();
                let out_location = outcome
                    .sums_path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "the output folder".to_string());
                with_state(|s| {
                    s.last_setup = Some(outcome.setup.path.clone());
                    s.done = Some(DoneInfo {
                        ok: true,
                        summary: format!(
                            "Project: {app_name}\r\nVersion: {version}\r\nArchitecture: x64\r\nOutput location: {out_location}\r\nBuild time: {build_time}"
                        ),
                        reason: String::new(),
                    });
                });
                // Name what was built and where — no guessing.
                set_status(
                    hwnd,
                    StatusKind::Ok,
                    &format!(
                        "Built: {}, the portable .zip, and SHA256SUMS.txt in {}",
                        outcome.setup.name, out_location
                    ),
                );
                show_page(hwnd, Page::BuildDone);
            }
            Err(fail) => {
                append_build_log(hwnd, &format!("Error: {}", fail.message));
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
                        return;
                    }
                }
                with_state(|s| {
                    s.done = Some(DoneInfo {
                        ok: false,
                        summary: String::new(),
                        reason: fail.message.clone(),
                    });
                });
                set_status(
                    hwnd,
                    StatusKind::Error,
                    "Pack failed — the reason is on the results page",
                );
                show_page(hwnd, Page::BuildDone);
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
            append_output(hwnd, "The output folder does not exist yet — it is created when you build. If the path looks wrong, fix it in the Output folder field on the Build Settings page.");
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
            Some(JobResult::Scan { gen, result }) => on_scan_done(hwnd, gen, result),
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
        show_page(hwnd, Page::BuildSettings);
        set_status(
            hwnd,
            StatusKind::Neutral,
            "Manifest format shown in the output console on the Build page",
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
            "SeanPak Builder {}\n\nThe Builder is a view over the same operations the seanpak CLI runs — pack, validate, and verify behave identically here. Fully offline; nothing is sent anywhere.",
            env!("CARGO_PKG_VERSION")
        )
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
        let caption: Vec<u16> = "About SeanPak Builder\0".encode_utf16().collect();
        MessageBoxW(hwnd, text.as_ptr(), caption.as_ptr(), MB_OK);
    }

    // ---- New Project flow ----

    /// Enters the flow: fields cleared (version prefilled with the
    /// house default), the current document left untouched until
    /// Create Project replaces it.
    fn on_np_start(hwnd: HWND, from: Page) {
        with_state(|s| {
            s.np_step = 0;
            s.np_return = from;
            s.np_backup = None;
        });
        unsafe {
            set_text(hwnd, ID_EDT_NP_NAME, "");
            set_text(hwnd, ID_EDT_NP_VERSION, "1.0.0");
            set_text(hwnd, ID_EDT_NP_PUBLISHER, "");
            set_text(hwnd, ID_EDT_NP_FOLDER, "");
        }
        show_page(hwnd, Page::NewProject);
    }

    /// Refreshes the flow's step-dependent texts: the subtitle, the
    /// Next button's caption, and (on step 3) the summary.
    fn refresh_np(hwnd: HWND) {
        let step = with_state(|s| s.np_step).unwrap_or(0);
        let sub = match step {
            0 => "Provide basic information about your program.",
            1 => "Pick the folder your built program is in.",
            _ => "Check the details, then create the project.",
        };
        unsafe {
            set_text(hwnd, ID_LBL_NP_SUB, sub);
            set_text(
                hwnd,
                ID_BTN_NP_NEXT,
                if step == 2 {
                    "Create Project"
                } else {
                    "Next →"
                },
            );
        }
        if step == 2 {
            let summary = with_state(|s| {
                let m = &s.doc.manifest;
                let folder = s
                    .doc
                    .files_dir
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let files_line = match &s.scan {
                    Some(scan) => {
                        let total: u64 = scan.rows_by_rel.iter().map(|r| r.size).sum();
                        format!(
                            "Program folder: {folder}\r\nFiles found: {} ({})",
                            scan.all_files.len(),
                            format_bytes(total)
                        )
                    }
                    None => {
                        if s.busy {
                            format!("Program folder: {folder}\r\nFiles found: scanning…")
                        } else {
                            format!("Program folder: {folder}\r\nFiles found: —")
                        }
                    }
                };
                format!(
                    "Program name: {}\r\nVersion: {}\r\nPublisher: {}\r\n{files_line}\r\n\r\nThe project is not saved yet — after creating it, use Save in the editor to keep it as a file.",
                    m.app_name, m.version, m.publisher
                )
            })
            .unwrap_or_default();
            unsafe { set_text(hwnd, ID_LBL_NP_SUMMARY, &summary) };
        }
        unsafe {
            InvalidateRect(hwnd, std::ptr::null(), 1);
        }
    }

    unsafe fn on_np_browse(hwnd: HWND) {
        let Some(folder) = crate::native_dialog::pick_folder() else {
            return;
        };
        set_text(hwnd, ID_EDT_NP_FOLDER, &folder.to_string_lossy());
    }

    /// Next: step 1 → 2 (details into the scratch fields), step 2 →
    /// 3 (build the new document from the fields, set the current
    /// one aside, scan the folder), step 3 = Create Project.
    unsafe fn on_np_next(hwnd: HWND) {
        let step = with_state(|s| s.np_step).unwrap_or(0);
        match step {
            0 => {
                if get_text(hwnd, ID_EDT_NP_NAME).trim().is_empty() {
                    set_status(hwnd, StatusKind::Error, "Give the program a name first");
                    return;
                }
                with_state(|s| s.np_step = 1);
                show_page(hwnd, Page::NewProject);
            }
            1 => {
                let folder_text = get_text(hwnd, ID_EDT_NP_FOLDER);
                let folder = folder_text.trim();
                if folder.is_empty() {
                    set_status(
                        hwnd,
                        StatusKind::Error,
                        "Choose the program folder first — Browse… finds it",
                    );
                    return;
                }
                let folder = PathBuf::from(folder);
                if !folder.is_dir() {
                    set_status(
                        hwnd,
                        StatusKind::Error,
                        "That folder does not exist — pick another one",
                    );
                    return;
                }
                let name = get_text(hwnd, ID_EDT_NP_NAME);
                let version = get_text(hwnd, ID_EDT_NP_VERSION);
                let publisher = get_text(hwnd, ID_EDT_NP_PUBLISHER);
                let value = jobj(vec![
                    ("appName", jstr(name.trim().to_string())),
                    (
                        "version",
                        jstr(if version.trim().is_empty() {
                            "1.0.0".to_string()
                        } else {
                            version.trim().to_string()
                        }),
                    ),
                    ("publisher", jstr(publisher.trim().to_string())),
                ]);
                let manifest = match AppManifest::from_json(&value.render()) {
                    Ok(m) => m,
                    Err(errors) => {
                        set_status(
                            hwnd,
                            StatusKind::Error,
                            &format!("Check the details: {}", errors.join("; ")),
                        );
                        return;
                    }
                };
                let mut doc = StudioDocument::new_blank();
                doc.manifest = manifest;
                doc.out_dir = folder.join("dist");
                doc.files_dir = Some(folder);
                with_state(|s| {
                    // Set the current document aside exactly once;
                    // Back/Cancel restore it.
                    if s.np_backup.is_none() {
                        s.np_backup = Some(std::mem::replace(&mut s.doc, doc));
                    } else {
                        s.doc = doc;
                    }
                    s.scan = None;
                    s.entries_dir = None;
                    s.last_setup = None;
                    s.np_step = 2;
                });
                show_page(hwnd, Page::NewProject);
                start_scan(hwnd);
            }
            _ => {
                // Create Project: keep the new document, drop the
                // set-aside one, land in the editor.
                with_state(|s| {
                    s.np_backup = None;
                    s.doc.dirty = true;
                });
                fill_controls(hwnd);
                fill_list(hwnd);
                update_title(hwnd);
                show_page(hwnd, Page::EditorFiles);
                set_status(
                    hwnd,
                    StatusKind::Ok,
                    "Project created — check the files, then Save to keep it as a file",
                );
            }
        }
    }

    unsafe fn on_np_back(hwnd: HWND) {
        let step = with_state(|s| s.np_step).unwrap_or(0);
        if step > 0 {
            with_state(|s| s.np_step = step - 1);
            show_page(hwnd, Page::NewProject);
        }
    }

    /// Cancel: stop any scan the flow started, restore the document
    /// that was set aside, and return to where the flow began.
    unsafe fn on_np_cancel(hwnd: HWND) {
        if is_busy() {
            with_state(|s| s.cancel.store(true, Ordering::Relaxed));
        }
        let back = with_state(|s| {
            if let Some(doc) = s.np_backup.take() {
                s.doc = doc;
            }
            s.scan = None;
            // Any scan still running belongs to the abandoned
            // document — its result must not land on the restored one.
            s.scan_gen += 1;
            s.entries_dir = s.doc.files_dir.clone();
            s.np_step = 0;
            s.np_return
        })
        .unwrap_or(Page::Home);
        fill_controls(hwnd);
        fill_list(hwnd);
        update_title(hwnd);
        show_page(hwnd, back);
        set_status(
            hwnd,
            StatusKind::Neutral,
            "New project cancelled — nothing was created",
        );
    }

    // ---- Build Finished + Settings fills ----

    fn fill_done(hwnd: HWND) {
        let done = with_state(|s| {
            s.done
                .as_ref()
                .map(|d| (d.ok, d.summary.clone(), d.reason.clone()))
        })
        .flatten();
        let (ok, summary, reason) = done.unwrap_or((true, String::new(), String::new()));
        unsafe {
            if ok {
                set_text(hwnd, ID_LBL_DONE_TITLE, "Build Finished!");
                set_text(
                    hwnd,
                    ID_LBL_DONE_SUB,
                    "Your installer has been created and is ready to use.",
                );
                set_text(hwnd, ID_LBL_DONE_SUMMARY, &summary);
            } else {
                set_text(hwnd, ID_LBL_DONE_TITLE, "Build Failed");
                set_text(
                    hwnd,
                    ID_LBL_DONE_SUB,
                    "The build did not finish. Nothing outside the output folder was changed.",
                );
                set_text(hwnd, ID_LBL_DONE_REASON, &reason);
            }
        }
    }

    fn fill_settings_page(hwnd: HWND) {
        let (count, log, settings_file) = with_state(|s| {
            (
                s.settings.recents.len(),
                crate::diag::log_path().display().to_string(),
                super::settings_path().display().to_string(),
            )
        })
        .unwrap_or_default();
        unsafe {
            set_text(
                hwnd,
                ID_LBL_SET_RECENTS,
                &format!(
                    "{count} project{} in the list — the newest {} are kept.",
                    if count == 1 { "" } else { "s" },
                    super::MAX_RECENT_PROJECTS
                ),
            );
            set_text(hwnd, ID_LBL_SET_LOG, &format!("Diagnostics log: {log}"));
            set_text(
                hwnd,
                ID_LBL_SET_SETTINGS,
                &format!("Builder settings file: {settings_file}"),
            );
            set_text(
                hwnd,
                ID_LBL_SET_VERSION,
                &format!(
                    "SeanPak Builder {} — a view over the same operations the seanpak command line runs.",
                    env!("CARGO_PKG_VERSION")
                ),
            );
            set_text(hwnd, ID_LBL_SET_FACTS, &default_facts());
        }
    }

    /// Saves geometry + last-used paths + recents. Called from
    /// WM_CLOSE, before the window is destroyed.
    unsafe fn save_settings(hwnd: HWND) {
        harvest_fields(hwnd);
        let mut rect: RECT = std::mem::zeroed();
        GetWindowRect(hwnd, &mut rect);
        with_state(|s| {
            s.settings.window_rect = Some((
                rect.left,
                rect.top,
                (rect.right - rect.left).max(400),
                (rect.bottom - rect.top).max(300),
            ));
            s.settings.manifest_path = s.doc.manifest_path.clone();
            s.settings.files_dir = s.doc.files_dir.clone();
            s.settings.out_dir = Some(s.doc.out_dir.clone());
            s.settings.save();
        });
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

    // ---- painting ----

    /// Text of a child control (the theme's own helper is private
    /// to theme.rs; this is the same read).
    unsafe fn control_text(ctrl: HWND) -> String {
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

    unsafe fn select_body_font(hdc: HDC) {
        let font = BODY_FONT;
        if !font.is_null() {
            SelectObject(hdc, font as _);
        }
    }

    /// Left-nav button: flat over the rail, the selected section
    /// raised with a blue bar at its left edge.
    unsafe fn draw_nav_button(hwnd: HWND, dis: *const DRAWITEMSTRUCT) {
        let rc = (*dis).rcItem;
        let id = (*dis).CtlID as usize;
        let page = with_state(|s| s.page).unwrap_or(Page::Home);
        let selected = matches!(
            (id, page),
            (ID_NAV_HOME, Page::Home)
                | (ID_NAV_PROJECTS, Page::ProjectList)
                | (
                    ID_NAV_PROJECTS,
                    Page::EditorFiles | Page::EditorOptions | Page::EditorRequirements
                )
                | (
                    ID_NAV_BUILD,
                    Page::BuildSettings | Page::Building | Page::BuildDone
                )
                | (ID_NAV_SETTINGS, Page::Settings)
        );
        let disabled = (*dis).itemState & ODS_DISABLED != 0;
        let pressed = (*dis).itemState & ODS_SELECTED != 0;
        let hdc = (*dis).hDC;
        let fill = if selected {
            theme::SELECTED
        } else if pressed {
            theme::PRESSED_FILL
        } else {
            theme::SURFACE
        };
        FillRect(hdc, &rc, theme::brush(fill));
        if selected {
            let bar = RECT {
                left: rc.left,
                top: rc.top,
                right: rc.left + 3,
                bottom: rc.bottom,
            };
            FillRect(hdc, &bar, theme::brush(theme::SELECTION_INDICATOR));
        }
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(
            hdc,
            if disabled {
                theme::DISABLED_FOREGROUND
            } else if selected {
                theme::FOREGROUND
            } else {
                theme::MUTED
            },
        );
        select_body_font(hdc);
        let label = control_text((*dis).hwndItem);
        let mut wide: Vec<u16> = label.encode_utf16().collect();
        let mut text_rc = RECT {
            left: rc.left + 16,
            top: rc.top,
            right: rc.right - 6,
            bottom: rc.bottom,
        };
        DrawTextW(
            hdc,
            wide.as_mut_ptr(),
            wide.len() as i32,
            &mut text_rc,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        if (*dis).itemState & ODS_FOCUS != 0 && !disabled {
            DrawFocusRect(hdc, &rc);
        }
        let _ = hwnd;
    }

    /// Editor tab: the current page raised with a blue underline,
    /// the others flat and muted.
    unsafe fn draw_tab_button(hwnd: HWND, dis: *const DRAWITEMSTRUCT) {
        let rc = (*dis).rcItem;
        let id = (*dis).CtlID as usize;
        let page = with_state(|s| s.page).unwrap_or(Page::Home);
        let selected = matches!(
            (id, page),
            (ID_TAB_FILES, Page::EditorFiles)
                | (ID_TAB_OPTIONS, Page::EditorOptions)
                | (ID_TAB_REQUIREMENTS, Page::EditorRequirements)
        );
        let hdc = (*dis).hDC;
        FillRect(
            hdc,
            &rc,
            theme::brush(if selected {
                theme::RAISED
            } else {
                theme::BACKGROUND
            }),
        );
        if selected {
            let bar = RECT {
                left: rc.left,
                top: rc.bottom - 2,
                right: rc.right,
                bottom: rc.bottom,
            };
            FillRect(hdc, &bar, theme::brush(theme::SELECTION_INDICATOR));
        }
        SetBkMode(hdc, TRANSPARENT as i32);
        SetTextColor(
            hdc,
            if selected {
                theme::FOREGROUND
            } else {
                theme::MUTED
            },
        );
        select_body_font(hdc);
        let label = control_text((*dis).hwndItem);
        let mut wide: Vec<u16> = label.encode_utf16().collect();
        let mut text_rc = rc;
        DrawTextW(
            hdc,
            wide.as_mut_ptr(),
            wide.len() as i32,
            &mut text_rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        if (*dis).itemState & ODS_FOCUS != 0 {
            DrawFocusRect(hdc, &rc);
        }
        let _ = hwnd;
    }

    /// The page-specific painting: the nav rail, the New Project
    /// step rail, the Installer Options preview panel, and the
    /// Build Finished badge. Child controls paint themselves; this
    /// is only what no control covers.
    unsafe fn paint_custom(hwnd: HWND, hdc: HDC) {
        let (page, scale) =
            with_state(|s| (s.page, s.dpi as f32 / 96.0)).unwrap_or((Page::Home, 1.0));
        let mut client: RECT = std::mem::zeroed();
        GetClientRect(hwnd, &mut client);
        FillRect(hdc, &client, theme::brush(theme::BACKGROUND));
        match page {
            Page::NewProject => paint_np_rail(hwnd, hdc, scale),
            Page::EditorOptions => paint_preview(hwnd, hdc),
            Page::BuildDone => paint_badge(hwnd, hdc),
            _ => {
                // The nav rail: a surface strip with a hairline at
                // its right edge.
                let rail = RECT {
                    left: 0,
                    top: 0,
                    right: (176.0 * scale) as i32,
                    bottom: client.bottom,
                };
                FillRect(hdc, &rail, theme::brush(theme::SURFACE));
                let edge = RECT {
                    left: rail.right - 1,
                    top: 0,
                    right: rail.right,
                    bottom: client.bottom,
                };
                FillRect(hdc, &edge, theme::brush(theme::EDGE));
            }
        }
    }

    unsafe fn paint_np_rail(hwnd: HWND, hdc: HDC, scale: f32) {
        let (circles, step) =
            with_state(|s| (s.np_circles.clone(), s.np_step)).unwrap_or((Vec::new(), 0));
        if circles.len() < 2 {
            return;
        }
        // The connecting line behind the circles.
        let pen = CreatePen(PS_SOLID, 1, theme::EDGE);
        let old_pen = SelectObject(hdc, pen as _);
        let (x0, y_first, _) = circles[0];
        let (_, y_last, _) = circles[circles.len() - 1];
        MoveToEx(hdc, x0, y_first, std::ptr::null_mut());
        LineTo(hdc, x0, y_last);
        SelectObject(hdc, old_pen);
        DeleteObject(pen as _);
        SetBkMode(hdc, TRANSPARENT as i32);
        select_body_font(hdc);
        for (i, (cx, cy, r)) in circles.iter().enumerate() {
            let (fill, text_color) = if i < step {
                (theme::SUCCESS, 0x00FF_FFFF)
            } else if i == step {
                (theme::SELECTION_INDICATOR, 0x00FF_FFFF)
            } else {
                (theme::RAISED, theme::MUTED)
            };
            let rect = RECT {
                left: cx - r,
                top: cy - r,
                right: cx + r,
                bottom: cy + r,
            };
            let brush = theme::brush(fill);
            let old_brush = SelectObject(hdc, brush as _);
            let null_pen = GetStockObject(NULL_PEN);
            let prev_pen = SelectObject(hdc, null_pen as _);
            Ellipse(hdc, rect.left, rect.top, rect.right, rect.bottom);
            SelectObject(hdc, prev_pen);
            SelectObject(hdc, old_brush);
            SetTextColor(hdc, text_color);
            let label = format!("{}", i + 1);
            let mut wide: Vec<u16> = label.encode_utf16().collect();
            let mut text_rc = rect;
            DrawTextW(
                hdc,
                wide.as_mut_ptr(),
                wide.len() as i32,
                &mut text_rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
        }
        let _ = (hwnd, scale);
    }

    /// A static sketch of the installer's first page with the
    /// project's name — what the Preview installer button opens for
    /// real. Drawn, not a control, so it can never be clicked by
    /// mistake.
    unsafe fn paint_preview(hwnd: HWND, hdc: HDC) {
        let Some((Some(rect), app_name)) = with_state(|s| {
            (
                s.preview_rect,
                if s.doc.manifest.app_name.trim().is_empty() {
                    "Your Program".to_string()
                } else {
                    s.doc.manifest.app_name.clone()
                },
            )
        }) else {
            return;
        };
        let (x, y, w, _h) = rect;
        let panel = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + 252,
        };
        FillRect(hdc, &panel, theme::brush(theme::RAISED));
        FrameRect(hdc, &panel, theme::brush(theme::EDGE));
        SetBkMode(hdc, TRANSPARENT as i32);
        // Window title.
        let font = HEADER_FONT;
        let old_font = if !font.is_null() {
            SelectObject(hdc, font as _)
        } else {
            std::ptr::null_mut()
        };
        SetTextColor(hdc, theme::FOREGROUND);
        let title = format!("{app_name} Setup");
        let mut wide: Vec<u16> = title.encode_utf16().collect();
        let mut rc = RECT {
            left: x + 14,
            top: y + 10,
            right: x + w - 12,
            bottom: y + 32,
        };
        DrawTextW(
            hdc,
            wide.as_mut_ptr(),
            wide.len() as i32,
            &mut rc,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE | DT_END_ELLIPSIS,
        );
        if !old_font.is_null() {
            SelectObject(hdc, old_font);
        }
        select_body_font(hdc);
        // The wizard's step rail.
        let steps = ["Welcome", "Location", "Review", "Install", "Finish"];
        for (i, name) in steps.iter().enumerate() {
            let sy = y + 52 + (i as i32) * 26;
            if i == 0 {
                let dot = RECT {
                    left: x + 14,
                    top: sy + 4,
                    right: x + 22,
                    bottom: sy + 12,
                };
                let old_brush = SelectObject(hdc, theme::brush(theme::SELECTION_INDICATOR) as _);
                let null_pen = GetStockObject(NULL_PEN);
                let prev_pen = SelectObject(hdc, null_pen as _);
                Ellipse(hdc, dot.left, dot.top, dot.right, dot.bottom);
                SelectObject(hdc, prev_pen);
                SelectObject(hdc, old_brush);
            }
            SetTextColor(
                hdc,
                if i == 0 {
                    theme::FOREGROUND
                } else {
                    theme::MUTED
                },
            );
            let mut wide: Vec<u16> = name.encode_utf16().collect();
            let mut rc = RECT {
                left: x + 30,
                top: sy,
                right: x + 110,
                bottom: sy + 18,
            };
            DrawTextW(
                hdc,
                wide.as_mut_ptr(),
                wide.len() as i32,
                &mut rc,
                DT_LEFT | DT_VCENTER | DT_SINGLELINE,
            );
        }
        // Body copy + the two wizard buttons.
        SetTextColor(hdc, theme::MUTED);
        let body = format!(
            "Welcome\r\n\r\nThis wizard will guide you through the installation of {app_name}."
        );
        let mut wide: Vec<u16> = body.encode_utf16().collect();
        let mut rc = RECT {
            left: x + 118,
            top: y + 52,
            right: x + w - 14,
            bottom: y + 150,
        };
        DrawTextW(
            hdc,
            wide.as_mut_ptr(),
            wide.len() as i32,
            &mut rc,
            DT_LEFT | DT_TOP | DT_WORDBREAK,
        );
        let by = y + 252 - 44;
        let next_rc = RECT {
            left: x + w - 14 - 76,
            top: by,
            right: x + w - 14,
            bottom: by + 30,
        };
        FillRect(hdc, &next_rc, theme::brush(theme::ACTION));
        SetTextColor(hdc, 0x00FF_FFFF);
        let mut wide: Vec<u16> = "Next".encode_utf16().collect();
        let mut rc = next_rc;
        DrawTextW(
            hdc,
            wide.as_mut_ptr(),
            wide.len() as i32,
            &mut rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        let cancel_rc = RECT {
            left: next_rc.left - 84,
            top: by,
            right: next_rc.left - 8,
            bottom: by + 30,
        };
        FillRect(hdc, &cancel_rc, theme::brush(theme::RAISED));
        FrameRect(hdc, &cancel_rc, theme::brush(theme::EDGE));
        SetTextColor(hdc, theme::FOREGROUND);
        let mut wide: Vec<u16> = "Cancel".encode_utf16().collect();
        let mut rc = cancel_rc;
        DrawTextW(
            hdc,
            wide.as_mut_ptr(),
            wide.len() as i32,
            &mut rc,
            DT_CENTER | DT_VCENTER | DT_SINGLELINE,
        );
        let _ = hwnd;
    }

    unsafe fn paint_badge(hwnd: HWND, hdc: HDC) {
        let Some((Some(rect), ok)) =
            with_state(|s| (s.badge_rect, s.done.as_ref().map(|d| d.ok).unwrap_or(true)))
        else {
            return;
        };
        let (x, y, w, h) = rect;
        let circle = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        let fill = if ok {
            theme::SUCCESS
        } else {
            theme::DANGER_FILL
        };
        let old_brush = SelectObject(hdc, theme::brush(fill) as _);
        let null_pen = GetStockObject(NULL_PEN);
        let prev_pen = SelectObject(hdc, null_pen as _);
        Ellipse(hdc, circle.left, circle.top, circle.right, circle.bottom);
        SelectObject(hdc, prev_pen);
        SelectObject(hdc, old_brush);
        SetBkMode(hdc, TRANSPARENT as i32);
        if ok {
            // A white check mark.
            let pen = CreatePen(PS_SOLID, 5, 0x00FF_FFFF);
            let old_pen = SelectObject(hdc, pen as _);
            let cx = x + w / 2;
            let cy = y + h / 2;
            MoveToEx(hdc, cx - 16, cy, std::ptr::null_mut());
            LineTo(hdc, cx - 5, cy + 11);
            LineTo(hdc, cx + 17, cy - 12);
            SelectObject(hdc, old_pen);
            DeleteObject(pen as _);
        } else {
            SetTextColor(hdc, 0x00FF_FFFF);
            let font = TITLE_FONT;
            let old_font = if !font.is_null() {
                SelectObject(hdc, font as _)
            } else {
                std::ptr::null_mut()
            };
            let mut wide: Vec<u16> = "!".encode_utf16().collect();
            let mut rc = circle;
            DrawTextW(
                hdc,
                wide.as_mut_ptr(),
                wide.len() as i32,
                &mut rc,
                DT_CENTER | DT_VCENTER | DT_SINGLELINE,
            );
            if !old_font.is_null() {
                SelectObject(hdc, old_font);
            }
        }
        let _ = hwnd;
    }

    // ---- window procedure ----

    unsafe extern "system" fn wnd_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_CTLCOLORSTATIC => {
                let hdc = wparam as HDC;
                let id = GetDlgCtrlID(lparam as HWND) as usize;
                SetBkMode(hdc, TRANSPARENT as i32);
                if id == ID_FOOTER_SEP {
                    SetBkColor(hdc, theme::EDGE);
                    return theme::brush(theme::EDGE) as LRESULT;
                }
                if id == ID_OUTPUT {
                    // A read-only edit reports here, not CTLCOLOREDIT.
                    SetTextColor(hdc, theme::FOREGROUND);
                    SetBkColor(hdc, theme::READ_ONLY_FILL);
                    return theme::brush(theme::READ_ONLY_FILL) as LRESULT;
                }
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
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                if id == ID_LBL_DATA_NOTE || id == ID_LBL_REQ_NOTE {
                    SetTextColor(hdc, theme::INFO);
                    SetBkMode(hdc, OPAQUE as i32);
                    SetBkColor(hdc, theme::INFO_FILL);
                    return theme::brush(theme::INFO_FILL) as LRESULT;
                }
                if id == ID_LBL_WINREQ_TAG {
                    SetTextColor(hdc, theme::SUCCESS);
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                if id == ID_LBL_RT_TAG1
                    || id == ID_LBL_RT_TAG2
                    || id == ID_LBL_RT_TAG3
                    || id == ID_LBL_RT_TAG4
                {
                    SetTextColor(hdc, theme::WARNING);
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                if id == ID_LBL_DONE_REASON {
                    SetTextColor(hdc, theme::DANGER);
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                if id == ID_LBL_NP_STEP1 || id == ID_LBL_NP_STEP2 || id == ID_LBL_NP_STEP3 {
                    let idx = id - ID_LBL_NP_STEP1;
                    let step = with_state(|s| s.np_step).unwrap_or(0);
                    SetTextColor(
                        hdc,
                        if idx < step {
                            theme::SUCCESS
                        } else if idx == step {
                            theme::FOREGROUND
                        } else {
                            theme::MUTED
                        },
                    );
                    return theme::brush(theme::BACKGROUND) as LRESULT;
                }
                const MUTED_IDS: &[usize] = &[
                    ID_LBL_HOME_SUB,
                    ID_LBL_RECENT_HDR,
                    ID_LBL_PL_SUB,
                    ID_LBL_NP_SUB,
                    ID_LBL_NP_FOLDER_NOTE,
                    ID_LBL_FILES_HDR,
                    ID_LBL_PACKAGE_HDR,
                    ID_LBL_FILES_PANE_HDR,
                    ID_LBL_FILES_SUB,
                    ID_LBL_FOLDER_PATH_LBL,
                    ID_LBL_OPT_SUB,
                    ID_LBL_OPT_DETAILS,
                    ID_LBL_OPT_ICON,
                    ID_LBL_ICON_VALUE,
                    ID_LBL_OPT_SHORTCUTS,
                    ID_LBL_LOC_NOTE,
                    ID_LBL_LAUNCH,
                    ID_LBL_LOGIN,
                    ID_LBL_DATADIRS,
                    ID_LBL_PREVIEW_HDR,
                    ID_LBL_POWER,
                    ID_LBL_APPVER,
                    ID_LBL_TOTALS,
                    ID_LBL_PL_CURRENT,
                    ID_LBL_REQ_SUB,
                    ID_LBL_REQ_WIN_HDR,
                    ID_LBL_REQ_RT_HDR,
                    ID_LBL_BS_SUB,
                    ID_LBL_BS_ARCH_HDR,
                    ID_LBL_BS_ARCH,
                    ID_LBL_BS_COMP_HDR,
                    ID_LBL_BS_OUT_HDR,
                    ID_LBL_BS_GET_HDR,
                    ID_LBL_OUTPUTS,
                    ID_LBL_BS_CHECK_HDR,
                    ID_LBL_BS_CONSOLE_HDR,
                    ID_LBL_ING_SUB,
                    ID_LBL_ING_PHASE,
                    ID_LBL_DONE_SUB,
                    ID_LBL_SET_RECENTS_HDR,
                    ID_LBL_SET_RECENTS,
                    ID_LBL_SET_DIAG_HDR,
                    ID_LBL_SET_LOG,
                    ID_LBL_SET_SETTINGS,
                    ID_LBL_SET_ABOUT_HDR,
                    ID_LBL_SET_VERSION,
                    ID_LBL_SET_FACTS,
                    ID_LBL_NP_SUMMARY,
                ];
                if MUTED_IDS.contains(&id) || PANEL_LABELS.contains(&id) {
                    SetTextColor(hdc, theme::MUTED);
                } else {
                    SetTextColor(hdc, theme::FOREGROUND);
                }
                theme::brush(theme::BACKGROUND) as LRESULT
            }
            WM_CTLCOLOREDIT => {
                let hdc = wparam as HDC;
                let ctrl = lparam as HWND;
                if !ctrl.is_null() && IsWindowEnabled(ctrl) == 0 {
                    SetTextColor(hdc, theme::DISABLED_FOREGROUND);
                    SetBkColor(hdc, theme::DISABLED_FILL);
                    return theme::brush(theme::DISABLED_FILL) as LRESULT;
                }
                SetTextColor(hdc, theme::FOREGROUND);
                SetBkColor(hdc, theme::SURFACE);
                theme::brush(theme::SURFACE) as LRESULT
            }
            WM_DRAWITEM => {
                let dis = lparam as *const DRAWITEMSTRUCT;
                if dis.is_null() || (*dis).CtlType != ODT_BUTTON {
                    return 0;
                }
                let id = (*dis).CtlID as usize;
                const PRIMARY_IDS: &[usize] = &[
                    ID_BTN_BUILD,
                    ID_BTN_NEW_PROJECT,
                    ID_BTN_PL_NEW,
                    ID_BTN_NP_NEXT,
                    ID_BTN_DONE_INSTALLER,
                    ID_BTN_REQ_NEXT,
                ];
                if CHECKBOX_IDS.contains(&id) {
                    theme::draw_checkbox(&*dis);
                } else if id == ID_NAV_HOME
                    || id == ID_NAV_PROJECTS
                    || id == ID_NAV_BUILD
                    || id == ID_NAV_SETTINGS
                {
                    draw_nav_button(hwnd, dis);
                } else if id == ID_TAB_FILES || id == ID_TAB_OPTIONS || id == ID_TAB_REQUIREMENTS {
                    draw_tab_button(hwnd, dis);
                } else {
                    theme::draw_push_button(&*dis, PRIMARY_IDS.contains(&id));
                }
                1
            }
            WM_COMMAND => {
                let id = wparam & 0xFFFF;
                let code = ((wparam >> 16) & 0xFFFF) as u32;
                // Owner-drawn checkboxes do not toggle themselves.
                if code == BN_CLICKED && CHECKBOX_IDS.contains(&id) {
                    let ctrl = GetDlgItem(hwnd, id as i32);
                    if !ctrl.is_null() && IsWindowEnabled(ctrl) != 0 {
                        let now = SendMessageW(ctrl, BM_GETCHECK, 0, 0);
                        SendMessageW(
                            ctrl,
                            BM_SETCHECK,
                            if now == BST_CHECKED as isize { 0 } else { 1 },
                            0,
                        );
                    }
                }
                if code == EN_CHANGE {
                    const FIELD_IDS: &[usize] = &[
                        ID_EDT_NAME,
                        ID_EDT_VERSION,
                        ID_EDT_PUBLISHER,
                        ID_EDT_PRIMARY,
                        ID_EDT_INSTALL_LOC,
                        ID_EDT_SM_FOLDER,
                        ID_EDT_ASSOC,
                        ID_EDT_FILES_DIR,
                        ID_EDT_OUT_DIR,
                        ID_EDT_DATA_DIRS,
                        ID_EDT_POWER,
                    ];
                    if FIELD_IDS.contains(&id) {
                        mark_dirty(hwnd);
                        if id == ID_EDT_OUT_DIR
                            && with_state(|s| s.page).unwrap_or(Page::Home) == Page::BuildSettings
                        {
                            refresh_outputs_label(hwnd);
                        }
                    }
                    return 0;
                }
                match id {
                    ID_NAV_HOME => show_page(hwnd, Page::Home),
                    ID_NAV_PROJECTS => show_page(hwnd, Page::ProjectList),
                    ID_NAV_BUILD => show_page(hwnd, Page::BuildSettings),
                    ID_NAV_SETTINGS => show_page(hwnd, Page::Settings),
                    ID_TAB_FILES => show_page(hwnd, Page::EditorFiles),
                    ID_TAB_OPTIONS => show_page(hwnd, Page::EditorOptions),
                    ID_TAB_REQUIREMENTS => show_page(hwnd, Page::EditorRequirements),
                    ID_BTN_NEW_PROJECT => on_np_start(hwnd, Page::Home),
                    ID_BTN_PL_NEW => on_np_start(hwnd, Page::ProjectList),
                    ID_BTN_VIEW_ALL => show_page(hwnd, Page::ProjectList),
                    ID_BTN_PL_PICK => on_open_manifest(hwnd),
                    ID_BTN_PL_OPEN => {
                        if let Some(i) = selected_recent_index(hwnd) {
                            on_open_recent(hwnd, i)
                        }
                    }
                    ID_BTN_PL_REMOVE => on_remove_recent(hwnd),
                    ID_BTN_NP_BROWSE => on_np_browse(hwnd),
                    ID_BTN_NP_NEXT => on_np_next(hwnd),
                    ID_BTN_NP_BACK => on_np_back(hwnd),
                    ID_BTN_NP_CANCEL => on_np_cancel(hwnd),
                    ID_BTN_TB_OPEN => on_open_manifest(hwnd),
                    ID_BTN_TB_SAVE => on_save(hwnd, false),
                    ID_BTN_TB_SAVEAS => on_save(hwnd, true),
                    ID_BTN_TB_VALIDATE | ID_BTN_VALIDATE => on_validate(hwnd),
                    ID_BTN_SCHEMA => on_schema(hwnd),
                    ID_BTN_FILES_DIR | ID_BTN_PROGRAM_FOLDER => on_choose_files_dir(hwnd),
                    ID_BTN_OUT_DIR => on_choose_out_dir(hwnd),
                    ID_BTN_REMOVE => on_remove_selected(hwnd),
                    ID_BTN_BUILD => on_pack(hwnd, false),
                    ID_BTN_VERIFY => on_verify(hwnd, get_check(hwnd, ID_CHK_SANDBOX)),
                    ID_BTN_TEST => on_verify(hwnd, true),
                    ID_BTN_OPEN_OUTPUT | ID_BTN_DONE_FOLDER => on_open_output_folder(hwnd),
                    ID_BTN_PREVIEW | ID_BTN_DONE_INSTALLER => on_preview_installer(hwnd),
                    ID_BTN_CLEAR => set_text(hwnd, ID_OUTPUT, ""),
                    ID_BTN_CANCEL => on_cancel_job(hwnd),
                    ID_BTN_REQ_BACK => show_page(hwnd, Page::EditorOptions),
                    ID_BTN_REQ_NEXT => {
                        harvest_fields(hwnd);
                        show_page(hwnd, Page::BuildSettings);
                    }
                    ID_BTN_BS_BACK => show_page(hwnd, Page::EditorFiles),
                    ID_BTN_DONE_AGAIN | ID_BTN_DONE_BACK => show_page(hwnd, Page::BuildSettings),
                    ID_BTN_CLEAR_RECENTS => on_clear_recents(hwnd),
                    ID_BTN_OPEN_LOG => on_open_log(hwnd),
                    ID_BTN_ABOUT => on_about(hwnd),
                    2 => {
                        // Esc arrives as IDCANCEL: cancel a running
                        // job; on the New Project flow, leave it.
                        if is_busy() {
                            on_cancel_job(hwnd);
                        } else if with_state(|s| s.page).unwrap_or(Page::Home) == Page::NewProject {
                            on_np_cancel(hwnd);
                        }
                    }
                    _ => {
                        if code == BN_CLICKED {
                            const OPTION_CHECKS: &[usize] = &[
                                ID_CHK_DESKTOP,
                                ID_CHK_STARTMENU,
                                ID_CHK_PATH,
                                ID_CHK_LAUNCH_OFFER,
                                ID_CHK_LAUNCH_DEFAULT,
                                ID_CHK_LOGIN_OFFER,
                                ID_CHK_LOGIN_DEFAULT,
                            ];
                            if OPTION_CHECKS.contains(&id) {
                                mark_dirty(hwnd);
                            }
                        }
                    }
                }
                0
            }
            WM_NOTIFY => {
                let hdr = lparam as *const NMHDR;
                if hdr.is_null() {
                    return 0;
                }
                let code = (*hdr).code as i32;
                let from = (*hdr).idFrom;
                if code == NM_CUSTOMDRAW_I32 {
                    let drawn = match from {
                        ID_LIST => theme::listview_custom_draw(lparam, true),
                        ID_TREE | ID_RECENTS => theme::listview_custom_draw(lparam, false),
                        _ => None,
                    };
                    if let Some(result) = drawn {
                        return result;
                    }
                    return DefWindowProcW(hwnd, msg, wparam, lparam);
                }
                if code == LVN_ITEMCHANGED_I32 {
                    let nmlv = lparam as *const NMLISTVIEW;
                    let item = (*nmlv).iItem;
                    if item >= 0 {
                        match from {
                            ID_LIST => {
                                if (*nmlv).uNewState & LVIS_SELECTED != 0 {
                                    on_list_selection(hwnd, item);
                                }
                            }
                            ID_TREE => {
                                let old_idx = ((*nmlv).uOldState >> 12) & 0xF;
                                let new_idx = ((*nmlv).uNewState >> 12) & 0xF;
                                let sup = with_state(|s| s.suppress_tree_check).unwrap_or(false);
                                if (*nmlv).uChanged & LVIF_STATE != 0 && old_idx != new_idx && !sup
                                {
                                    on_tree_check(hwnd, item, new_idx == 2);
                                } else if (*nmlv).uNewState & LVIS_SELECTED != 0 {
                                    on_tree_selection(hwnd, item);
                                }
                            }
                            _ => {}
                        }
                    }
                    return 0;
                }
                if code == NM_DBLCLK_I32 && from == ID_RECENTS {
                    let nia = lparam as *const NMITEMACTIVATE;
                    if (*nia).iItem >= 0 {
                        on_open_recent(hwnd, (*nia).iItem as usize);
                    }
                    return 0;
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_APP_JOB => {
                on_job_message(hwnd);
                0
            }
            WM_APP_PROGRESS => {
                on_progress_message(hwnd);
                0
            }
            WM_PAINT => {
                let mut ps: PAINTSTRUCT = std::mem::zeroed();
                let hdc = BeginPaint(hwnd, &mut ps);
                if !hdc.is_null() {
                    paint_custom(hwnd, hdc);
                }
                EndPaint(hwnd, &ps);
                0
            }
            WM_SIZE => {
                layout(hwnd);
                InvalidateRect(hwnd, std::ptr::null(), 0);
                0
            }
            WM_GETMINMAXINFO => {
                let mmi = lparam as *mut MINMAXINFO;
                let scale = with_state(|s| s.dpi as f32 / 96.0).unwrap_or(1.0);
                (*mmi).ptMinTrackSize.x = (980.0 * scale) as i32;
                (*mmi).ptMinTrackSize.y = (720.0 * scale) as i32;
                0
            }
            WM_DPICHANGED => {
                let new_dpi = ((wparam >> 16) & 0xFFFF) as u32;
                with_state(|s| s.dpi = new_dpi);
                create_fonts(new_dpi);
                apply_fonts(hwnd);
                let suggested = lparam as *const RECT;
                if !suggested.is_null() {
                    SetWindowPos(
                        hwnd,
                        std::ptr::null_mut(),
                        (*suggested).left,
                        (*suggested).top,
                        (*suggested).right - (*suggested).left,
                        (*suggested).bottom - (*suggested).top,
                        SWP_NOZORDER | SWP_NOACTIVATE,
                    );
                }
                layout(hwnd);
                InvalidateRect(hwnd, std::ptr::null(), 1);
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
