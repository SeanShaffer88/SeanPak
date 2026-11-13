//! Package inspection (v7): `inspect` reads a built Setup.exe and
//! reports everything the package declares — identity, the install
//! options block, the v6 power block (components with packed sizes,
//! hooks, writes, rich shortcuts), and the per-file list with sizes,
//! stored sizes, compression, and component ownership.
//!
//! `diff` compares two packages: identity, options, components, and
//! the payload files by SHA-256 of their original bytes. The CLI maps
//! the result onto exit codes (0 = identical, 6 = differences found,
//! 3 = unreadable package); this module only computes the facts.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Seek;
use std::path::Path;

use crate::hash::sha256_hex;
use crate::manifest::{jobj, jstr, JsonValue};
use crate::overlay::{FileRecordHeader, InstallOptions, OverlayReader, PowerSpec};

/// Human label for a payload compression codec byte (manifest v4+).
pub fn codec_label(codec: u8) -> String {
    match codec {
        crate::codec::CODEC_STORED => "stored".to_string(),
        crate::codec::CODEC_LZ => "lz".to_string(),
        crate::codec::CODEC_XPRESS_HUFF => "xpress-huffman".to_string(),
        other => format!("unknown({other})"),
    }
}

/// One payload file in an inspection report.
#[derive(Debug, Clone)]
pub struct InspectFile {
    pub path: String,
    /// Original (uncompressed) size.
    pub size: u64,
    /// Bytes the payload occupies in the package.
    pub stored_size: u64,
    pub compression: u8,
    pub component: String,
    pub is_primary: bool,
}

/// A full inspection of one package.
#[derive(Debug, Clone)]
pub struct InspectReport {
    pub app_name: String,
    pub version: String,
    pub publisher: String,
    /// On-disk overlay manifest version (2–6).
    pub manifest_version: u32,
    pub options: InstallOptions,
    pub power: PowerSpec,
    pub files: Vec<InspectFile>,
    pub total_bytes: u64,
    pub total_stored_bytes: u64,
}

/// Reads a package for inspection. An error means the package is
/// unreadable (the CLI's exit 3).
pub fn inspect_package(path: &Path) -> Result<InspectReport, String> {
    let mut file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let stream_len = file
        .seek(std::io::SeekFrom::End(0))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let (manifest, records, options, power) = OverlayReader::read_package_full(file)
        .map_err(|e| format!("cannot read package {}: {e}", path.display()))?
        .ok_or_else(|| format!("no SeanPak overlay found in {}", path.display()))?;
    let files = records
        .iter()
        .enumerate()
        .map(|(idx, record)| InspectFile {
            path: record.relative_path(),
            size: record.file_size,
            stored_size: OverlayReader::stored_len(&records, idx, stream_len),
            compression: record.compression(),
            component: power.component_id_for_file(&record.relative_path()),
            is_primary: idx as u32 == manifest.primary_exe_index,
        })
        .collect::<Vec<_>>();
    Ok(InspectReport {
        app_name: manifest.app_name(),
        version: manifest.app_version(),
        publisher: manifest.publisher(),
        manifest_version: manifest.version,
        options,
        power,
        total_bytes: files.iter().map(|f| f.size).sum(),
        total_stored_bytes: files.iter().map(|f| f.stored_size).sum(),
        files,
    })
}

/// SHA-256 of every payload's original bytes, keyed by path. Used by
/// `diff` to compare contents, not just sizes.
fn payload_hashes(
    path: &Path,
    records: &[FileRecordHeader],
) -> Result<BTreeMap<String, String>, String> {
    let mut file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    let stream_len = file
        .seek(std::io::SeekFrom::End(0))
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    let mut out = BTreeMap::new();
    for idx in 0..records.len() {
        let bytes = OverlayReader::read_payload(&mut file, records, idx, stream_len)
            .map_err(|e| format!("cannot read payload {}: {e}", records[idx].relative_path()))?;
        out.insert(records[idx].relative_path(), sha256_hex(&bytes));
    }
    Ok(out)
}

/// The option fields two packages can differ in, as (name, A, B)
/// triples — only the differences.
fn option_deltas(a: &InstallOptions, b: &InstallOptions) -> Vec<(String, String, String)> {
    let mut out: Vec<(String, String, String)> = Vec::new();
    let mut cmp = |name: &str, va: String, vb: String| {
        if va != vb {
            out.push((name.to_string(), va, vb));
        }
    };
    let yn = |on: bool| if on { "yes" } else { "no" }.to_string();
    cmp(
        "installLocation",
        a.install_location(),
        b.install_location(),
    );
    cmp(
        "startMenuFolder",
        a.start_menu_folder(),
        b.start_menu_folder(),
    );
    cmp(
        "desktopShortcut",
        yn(a.wants_desktop_shortcut()),
        yn(b.wants_desktop_shortcut()),
    );
    cmp(
        "startMenuShortcut",
        yn(a.wants_start_menu_shortcut()),
        yn(b.wants_start_menu_shortcut()),
    );
    cmp(
        "pathEntry",
        yn(a.wants_path_entry()),
        yn(b.wants_path_entry()),
    );
    cmp(
        "fileAssociations",
        a.file_associations(),
        b.file_associations(),
    );
    cmp(
        "launchAfterInstall",
        format!(
            "offered {}, default {}",
            yn(a.offers_launch_after_install()),
            yn(a.launch_after_install_default_checked())
        ),
        format!(
            "offered {}, default {}",
            yn(b.offers_launch_after_install()),
            yn(b.launch_after_install_default_checked())
        ),
    );
    cmp(
        "startAtLogin",
        format!(
            "offered {}, default {}",
            yn(a.offers_start_at_login()),
            yn(a.start_at_login_default_checked())
        ),
        format!(
            "offered {}, default {}",
            yn(b.offers_start_at_login()),
            yn(b.start_at_login_default_checked())
        ),
    );
    cmp("dataDirs", a.data_dirs().join(";"), b.data_dirs().join(";"));
    out
}

/// The result of comparing two packages.
#[derive(Debug, Clone)]
pub struct DiffReport {
    pub app_a: String,
    pub app_b: String,
    /// (field, value in A, value in B) for identity differences.
    pub identity_deltas: Vec<(String, String, String)>,
    /// Option differences, (field, A, B).
    pub option_deltas: Vec<(String, String, String)>,
    /// Power-block differences in plain language (component added /
    /// removed / resized, hooks / writes / shortcuts count changes) —
    /// empty when the two power specs are identical.
    pub power_deltas: Vec<String>,
    pub files_added: Vec<String>,
    pub files_removed: Vec<String>,
    /// Paths present in both whose payload bytes differ.
    pub files_changed: Vec<String>,
}

impl DiffReport {
    /// True when nothing differs at all — the CLI's exit 0.
    pub fn identical(&self) -> bool {
        self.identity_deltas.is_empty()
            && self.option_deltas.is_empty()
            && self.power_deltas.is_empty()
            && self.files_added.is_empty()
            && self.files_removed.is_empty()
            && self.files_changed.is_empty()
    }
}

/// Compares two packages. An error means one of them is unreadable
/// (the CLI's exit 3).
pub fn diff_packages(a_path: &Path, b_path: &Path) -> Result<DiffReport, String> {
    let a = inspect_package(a_path)?;
    let b = inspect_package(b_path)?;

    let mut identity_deltas = Vec::new();
    if a.app_name != b.app_name {
        identity_deltas.push((
            "appName".to_string(),
            a.app_name.clone(),
            b.app_name.clone(),
        ));
    }
    if a.version != b.version {
        identity_deltas.push(("version".to_string(), a.version.clone(), b.version.clone()));
    }
    if a.publisher != b.publisher {
        identity_deltas.push((
            "publisher".to_string(),
            a.publisher.clone(),
            b.publisher.clone(),
        ));
    }

    // Payload comparison by hash of the original bytes.
    let records_of = |path: &Path| -> Result<Vec<FileRecordHeader>, String> {
        let file = File::open(path).map_err(|e| format!("cannot open {}: {e}", path.display()))?;
        let (_, records) = OverlayReader::read_manifest(file)
            .map_err(|e| format!("cannot read package {}: {e}", path.display()))?
            .ok_or_else(|| format!("no SeanPak overlay found in {}", path.display()))?;
        Ok(records)
    };
    let hashes_a = payload_hashes(a_path, &records_of(a_path)?)?;
    let hashes_b = payload_hashes(b_path, &records_of(b_path)?)?;
    let mut files_added = Vec::new();
    let mut files_removed = Vec::new();
    let mut files_changed = Vec::new();
    for (path, hash) in &hashes_b {
        match hashes_a.get(path) {
            None => files_added.push(path.clone()),
            Some(other) if other != hash => files_changed.push(path.clone()),
            _ => {}
        }
    }
    for path in hashes_a.keys() {
        if !hashes_b.contains_key(path) {
            files_removed.push(path.clone());
        }
    }

    // Power-block differences, in plain language.
    let mut power_deltas = Vec::new();
    if a.power != b.power {
        for component in &b.power.components {
            match a.power.component(&component.id) {
                None => power_deltas.push(format!("component \"{}\" added", component.id)),
                Some(old) => {
                    if old != component {
                        power_deltas.push(format!(
                            "component \"{}\" changed (packed size {} -> {}, {} files -> {} files)",
                            component.id,
                            old.packed_size,
                            component.packed_size,
                            old.files.len(),
                            component.files.len()
                        ));
                    }
                }
            }
        }
        for component in &a.power.components {
            if b.power.component(&component.id).is_none() {
                power_deltas.push(format!("component \"{}\" removed", component.id));
            }
        }
        let counts = [
            (
                "pre-install hooks",
                a.power.pre_hooks.len(),
                b.power.pre_hooks.len(),
            ),
            (
                "post-install hooks",
                a.power.post_hooks.len(),
                b.power.post_hooks.len(),
            ),
            (
                "registry writes",
                a.power.registry_writes.len(),
                b.power.registry_writes.len(),
            ),
            (
                "environment writes",
                a.power.env_writes.len(),
                b.power.env_writes.len(),
            ),
            (
                "rich shortcuts",
                a.power.shortcuts.len(),
                b.power.shortcuts.len(),
            ),
        ];
        for (name, na, nb) in counts {
            if na != nb {
                power_deltas.push(format!("{name}: {na} -> {nb}"));
            } else if na > 0 {
                // Same count can still mean different content.
                let same = match name {
                    "pre-install hooks" => a.power.pre_hooks == b.power.pre_hooks,
                    "post-install hooks" => a.power.post_hooks == b.power.post_hooks,
                    "registry writes" => a.power.registry_writes == b.power.registry_writes,
                    "environment writes" => a.power.env_writes == b.power.env_writes,
                    _ => a.power.shortcuts == b.power.shortcuts,
                };
                if !same {
                    power_deltas.push(format!("{name} changed ({na} in both)"));
                }
            }
        }
    }

    Ok(DiffReport {
        app_a: a.app_name.clone(),
        app_b: b.app_name.clone(),
        identity_deltas,
        option_deltas: option_deltas(&a.options, &b.options),
        power_deltas,
        files_added,
        files_removed,
        files_changed,
    })
}

/// The inspection report as a JSON value (the CLI's `--json` shape).
pub fn inspect_json(report: &InspectReport) -> JsonValue {
    let options = &report.options;
    jobj(vec![
        ("command", jstr("inspect")),
        ("status", jstr("ok")),
        ("app", jstr(report.app_name.clone())),
        ("version", jstr(report.version.clone())),
        ("publisher", jstr(report.publisher.clone())),
        (
            "manifestVersion",
            JsonValue::Number(report.manifest_version as f64),
        ),
        (
            "options",
            jobj(vec![
                ("installLocation", jstr(options.install_location())),
                ("startMenuFolder", jstr(options.start_menu_folder())),
                (
                    "desktopShortcut",
                    JsonValue::Bool(options.wants_desktop_shortcut()),
                ),
                (
                    "startMenuShortcut",
                    JsonValue::Bool(options.wants_start_menu_shortcut()),
                ),
                ("pathEntry", JsonValue::Bool(options.wants_path_entry())),
                (
                    "fileAssociations",
                    JsonValue::Array(
                        crate::sysinstall::parse_extensions(&options.file_associations())
                            .iter()
                            .map(|e| jstr(e.clone()))
                            .collect(),
                    ),
                ),
                (
                    "launchAfterInstall",
                    jobj(vec![
                        (
                            "offered",
                            JsonValue::Bool(options.offers_launch_after_install()),
                        ),
                        (
                            "defaultChecked",
                            JsonValue::Bool(options.launch_after_install_default_checked()),
                        ),
                    ]),
                ),
                (
                    "startAtLogin",
                    jobj(vec![
                        ("offered", JsonValue::Bool(options.offers_start_at_login())),
                        (
                            "defaultChecked",
                            JsonValue::Bool(options.start_at_login_default_checked()),
                        ),
                    ]),
                ),
                (
                    "dataDirs",
                    JsonValue::Array(
                        options
                            .data_dirs()
                            .iter()
                            .map(|d| jstr(d.clone()))
                            .collect(),
                    ),
                ),
            ]),
        ),
        ("power", report.power.to_json_value()),
        (
            "files",
            JsonValue::Array(
                report
                    .files
                    .iter()
                    .map(|f| {
                        jobj(vec![
                            ("path", jstr(f.path.clone())),
                            ("size", JsonValue::Number(f.size as f64)),
                            ("storedSize", JsonValue::Number(f.stored_size as f64)),
                            ("compression", jstr(codec_label(f.compression))),
                            ("component", jstr(f.component.clone())),
                            ("primary", JsonValue::Bool(f.is_primary)),
                        ])
                    })
                    .collect(),
            ),
        ),
        ("fileCount", JsonValue::Number(report.files.len() as f64)),
        ("totalBytes", JsonValue::Number(report.total_bytes as f64)),
        (
            "totalStoredBytes",
            JsonValue::Number(report.total_stored_bytes as f64),
        ),
    ])
}

/// The diff report as a JSON value.
pub fn diff_json(report: &DiffReport, identical: bool) -> JsonValue {
    let triples = |items: &[(String, String, String)]| {
        JsonValue::Array(
            items
                .iter()
                .map(|(field, a, b)| {
                    jobj(vec![
                        ("field", jstr(field.clone())),
                        ("a", jstr(a.clone())),
                        ("b", jstr(b.clone())),
                    ])
                })
                .collect(),
        )
    };
    let strings =
        |items: &[String]| JsonValue::Array(items.iter().map(|s| jstr(s.clone())).collect());
    jobj(vec![
        ("command", jstr("diff")),
        (
            "status",
            jstr(if identical { "identical" } else { "different" }),
        ),
        ("identical", JsonValue::Bool(identical)),
        ("identity", triples(&report.identity_deltas)),
        ("options", triples(&report.option_deltas)),
        ("power", strings(&report.power_deltas)),
        ("filesAdded", strings(&report.files_added)),
        ("filesRemoved", strings(&report.files_removed)),
        ("filesChanged", strings(&report.files_changed)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::AppManifest;
    use std::path::PathBuf;

    fn unique_temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "seanpak_inspect_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ))
    }

    fn pack(root: &Path, version: &str, body: &[u8]) -> PathBuf {
        let src = root.join(format!("src-{version}"));
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.exe"), body).unwrap();
        let manifest = AppManifest::from_json(&format!(
            r#"{{"appName": "Diff App", "version": "{version}", "primaryExe": "app.exe"}}"#
        ))
        .unwrap();
        let stub = root.join("stub.exe");
        if !stub.exists() {
            std::fs::write(&stub, vec![0xCCu8; 5000]).unwrap();
        }
        let out = root.join(format!("out-{version}"));
        crate::pack::pack(&manifest, &src, &out, &stub, false)
            .unwrap()
            .setup
            .path
    }

    #[test]
    fn inspect_reports_identity_and_files() {
        let root = unique_temp("basic");
        std::fs::create_dir_all(&root).unwrap();
        let setup = pack(&root, "1.0.0", b"payload-one");
        let report = inspect_package(&setup).unwrap();
        assert_eq!(report.app_name, "Diff App");
        assert_eq!(report.version, "1.0.0");
        assert_eq!(report.files.len(), 1);
        assert_eq!(report.files[0].path, "app.exe");
        assert_eq!(report.files[0].component, crate::overlay::CORE_COMPONENT_ID);
        assert!(report.files[0].is_primary);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn diff_of_identical_packages_is_clean() {
        let root = unique_temp("same");
        std::fs::create_dir_all(&root).unwrap();
        let setup = pack(&root, "1.0.0", b"payload-one");
        let report = diff_packages(&setup, &setup).unwrap();
        assert!(report.identical());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn diff_names_changed_files_and_version() {
        let root = unique_temp("changed");
        std::fs::create_dir_all(&root).unwrap();
        let a = pack(&root, "1.0.0", b"payload-one");
        let b = pack(&root, "1.1.0", b"payload-two-longer");
        let report = diff_packages(&a, &b).unwrap();
        assert!(!report.identical());
        assert_eq!(report.files_changed, vec!["app.exe".to_string()]);
        assert!(report
            .identity_deltas
            .iter()
            .any(|(f, va, vb)| f == "version" && va == "1.0.0" && vb == "1.1.0"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn unreadable_package_is_an_error() {
        let root = unique_temp("bad");
        std::fs::create_dir_all(&root).unwrap();
        let plain = root.join("plain.exe");
        std::fs::write(&plain, vec![0u8; 2048]).unwrap();
        assert!(inspect_package(&plain).is_err());
        assert!(diff_packages(&plain, &plain).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
