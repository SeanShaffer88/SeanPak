//! Project scaffolding (v7): `seanpak init <dir>` writes a starter
//! `manifest.json` into a project folder — the primary executable
//! detected from the folder scan, the app name taken from the folder
//! (or `--name`), and the v6 power blocks present as inert examples
//! (an example component that claims no files, empty hook lists,
//! empty write/shortcut lists — valid, and nothing installs
//! differently because of them) — plus a `PACKAGING.md` stub with
//! the one pack command for the detected layout.
//!
//! The generated manifest is validated with the real
//! [`AppManifest::from_json`] before anything is written: init only
//! reports success for a manifest the packer itself would accept.
//! An existing manifest is never overwritten without `--force`.

use std::path::{Path, PathBuf};

use crate::manifest::{AppManifest, JsonValue};
use crate::studio::render_pretty;

/// What an init run produced, for the CLI's report.
#[derive(Debug, Clone)]
pub struct InitOutcome {
    pub manifest_path: PathBuf,
    pub packaging_path: Option<PathBuf>,
    pub app_name: String,
    pub version: String,
    pub files_dir: String,
    pub primary_exe: Option<String>,
    pub pack_command: String,
}

/// The PACKAGING.md stub: the one pack command for the detected
/// layout, the verify line, and worked examples of the power blocks
/// (kept here, not in the generated manifest, so the manifest stays
/// clean).
fn packaging_md(app_name: &str, files_dir_arg: &str) -> String {
    format!(
        "# Packaging {app_name} with SeanPak\n\
         \n\
         ## Build the installer\n\
         \n\
         ```\n\
         seanpak pack manifest.json {files_dir_arg}\n\
         ```\n\
         \n\
         This writes `{app_name}-v<version>-x64-Setup.exe`, the matching portable\n\
         `.zip`, and `SHA256SUMS.txt` next to each other. Check a built\n\
         installer any time with:\n\
         \n\
         ```\n\
         seanpak verify <path-to-Setup.exe> --sandbox\n\
         seanpak inspect <path-to-Setup.exe>\n\
         ```\n\
         \n\
         ## Manifest\n\
         \n\
         `manifest.json` (next to this file) is the source of truth: app\n\
         name, version, publisher, the primary executable, shortcuts,\n\
         file associations, and the optional power blocks below.\n\
         \n\
         ## Optional power blocks\n\
         \n\
         The generated manifest already carries inert examples of each\n\
         block. Fill them in as needed — every block is optional, and an\n\
         empty block changes nothing.\n\
         \n\
         - `components` — optional parts the user can include or skip,\n\
           in the installer and later through Modify:\n\
         \n\
           ```json\n\
           \"components\": [\n\
             {{\"id\": \"docs\", \"name\": \"Documentation\", \"files\": [\"docs/manual.pdf\"], \"defaultSelected\": true}}\n\
           ]\n\
           ```\n\
         \n\
         - `hooks` — programs run around the install (`onFailure` is\n\
           `abort` or `continue`):\n\
         \n\
           ```json\n\
           \"hooks\": {{\"preInstall\": [], \"postInstall\": [\n\
             {{\"command\": \"{app_name}.exe\", \"args\": [\"--register\"], \"onFailure\": \"abort\"}}\n\
           ]}}\n\
           ```\n\
         \n\
         - `registryWrites` / `envWrites` — user-scope (HKCU) writes,\n\
           journaled in the receipt and restored on uninstall:\n\
         \n\
           ```json\n\
           \"registryWrites\": [\n\
             {{\"key\": \"Software\\\\{app_name}\", \"name\": \"InstallDir\", \"type\": \"sz\", \"value\": \"%INSTALLDIR%\"}}\n\
           ],\n\
           \"envWrites\": [\n\
             {{\"name\": \"{app_name}_HOME\", \"value\": \"%INSTALLDIR%\"}}\n\
           ]\n\
           ```\n\
         \n\
         - `shortcuts` — extra shortcuts beyond the standard ones\n\
           (`pin: true` attempts a taskbar pin, best effort):\n\
         \n\
           ```json\n\
           \"shortcuts\": [\n\
             {{\"name\": \"{app_name} Help\", \"target\": \"docs/manual.pdf\", \"location\": \"startMenu\", \"pin\": false}}\n\
           ]\n\
           ```\n\
         \n\
         ## Answer files\n\
         \n\
         Unattended installs can be preset with an answer file:\n\
         \n\
         ```\n\
         seanpak answers-template <path-to-Setup.exe> > answers.json\n\
         <path-to-Setup.exe> --install --answers answers.json --silent\n\
         ```\n",
    )
}

/// Scaffolds `<dir>`: writes the starter manifest (+ PACKAGING.md
/// unless one already exists). `files_dir` for packing is `<dir>/files`
/// when that folder exists, else `<dir>` itself.
pub fn init_project(
    dir: &Path,
    name_override: Option<&str>,
    force: bool,
) -> Result<InitOutcome, String> {
    if !dir.is_dir() {
        return Err(format!("{} is not a folder", dir.display()));
    }
    let manifest_path = dir.join("manifest.json");
    if manifest_path.exists() && !force {
        return Err(format!(
            "{} already exists — pass --force to overwrite it",
            manifest_path.display()
        ));
    }

    // Detect the payload folder and the primary executable.
    let nested = dir.join("files");
    let (files_dir_path, files_dir_arg) = if nested.is_dir() {
        (nested, "files".to_string())
    } else {
        (dir.to_path_buf(), ".".to_string())
    };
    let scan = crate::scanner::ScannedFolder::scan(&files_dir_path)
        .map_err(|e| format!("cannot scan {}: {e}", files_dir_path.display()))?;
    let primary_exe = scan
        .suggested_primary_exe
        .as_ref()
        .map(|p| p.replace('\\', "/"));

    let folder_name = dir
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .or_else(|| dir.file_name().map(|n| n.to_string_lossy().into_owned()))
        .filter(|n| !n.is_empty())
        .unwrap_or_else(|| "My App".to_string());
    let app_name = name_override
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty())
        .unwrap_or(folder_name);

    // Build the manifest as raw JSON so the inert example blocks
    // survive exactly as written, then validate with the real
    // manifest parser before writing anything.
    let manifest = starter_manifest(&app_name, primary_exe.as_deref());
    let text = render_pretty(&manifest);
    AppManifest::from_json(&text).map_err(|errors| {
        format!(
            "the generated manifest failed validation (this is a SeanPak bug): {}",
            errors.join("; ")
        )
    })?;

    std::fs::write(&manifest_path, format!("{text}\n"))
        .map_err(|e| format!("cannot write {}: {e}", manifest_path.display()))?;

    let packaging_path = dir.join("PACKAGING.md");
    let packaging_written = if packaging_path.exists() && !force {
        None
    } else {
        let body = packaging_md(&app_name, &files_dir_arg);
        std::fs::write(&packaging_path, body)
            .map_err(|e| format!("cannot write {}: {e}", packaging_path.display()))?;
        Some(packaging_path)
    };

    let pack_command = format!("seanpak pack manifest.json {files_dir_arg}");
    Ok(InitOutcome {
        manifest_path,
        packaging_path: packaging_written,
        app_name,
        version: "1.0.0".to_string(),
        files_dir: files_dir_arg,
        primary_exe,
        pack_command,
    })
}

/// The starter manifest as a JSON value. The power blocks are inert:
/// the example component claims no files and is not selected by
/// default, the hook lists are empty, and the write/shortcut lists
/// are empty — the package behaves exactly like one without them.
fn starter_manifest(app_name: &str, primary_exe: Option<&str>) -> JsonValue {
    use crate::manifest::{jobj, jstr};
    let mut fields: Vec<(&str, JsonValue)> = vec![
        ("appName", jstr(app_name)),
        ("version", jstr("1.0.0")),
        ("publisher", jstr("")),
    ];
    if let Some(primary) = primary_exe {
        fields.push(("primaryExe", jstr(primary)));
    }
    fields.push(("desktopShortcut", JsonValue::Bool(true)));
    fields.push(("startMenuShortcut", JsonValue::Bool(true)));
    // The v5.1.0 creation defaults, written out loud so a project
    // sees (and can change) them: a desktop icon, and the installer's
    // Finish page offering "Launch <app>" already ticked. (`true` is
    // the offer shorthand for offered + starts checked.)
    fields.push(("launchAfterInstall", JsonValue::Bool(true)));
    fields.push((
        "components",
        JsonValue::Array(vec![jobj(vec![
            ("id", jstr("example")),
            ("name", jstr("Example component")),
            (
                "description",
                jstr("Replace with a real optional part, or delete this component."),
            ),
            ("files", JsonValue::Array(vec![])),
            ("defaultSelected", JsonValue::Bool(false)),
        ])]),
    ));
    fields.push((
        "hooks",
        jobj(vec![
            ("preInstall", JsonValue::Array(vec![])),
            ("postInstall", JsonValue::Array(vec![])),
        ]),
    ));
    fields.push(("registryWrites", JsonValue::Array(vec![])));
    fields.push(("envWrites", JsonValue::Array(vec![])));
    fields.push(("shortcuts", JsonValue::Array(vec![])));
    JsonValue::Object(
        fields
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
    )
}

/// Parses rendered JSON back — used by the tests to prove the
/// generated manifest round-trips through the real JSON parser too.
#[cfg(test)]
fn reparse(text: &str) -> Result<JsonValue, String> {
    crate::manifest::parse_json(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "seanpak_init_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ))
    }

    #[test]
    fn init_writes_a_valid_manifest_and_packaging_stub() {
        let root = unique_temp("basic");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("widget.exe"), b"fake").unwrap();
        let outcome = init_project(&root, None, false).unwrap();
        assert_eq!(outcome.primary_exe.as_deref(), Some("widget.exe"));
        assert_eq!(outcome.pack_command, "seanpak pack manifest.json .");
        let packaging = std::fs::read_to_string(outcome.packaging_path.as_ref().unwrap()).unwrap();
        assert!(packaging.contains("seanpak pack manifest.json ."));
        let text = std::fs::read_to_string(&outcome.manifest_path).unwrap();
        let manifest = AppManifest::from_json(&text).unwrap();
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.primary_exe.as_deref(), Some("widget.exe"));
        // The v5.1.0 creation defaults ride the template: desktop
        // icon on, launch-after-setup offered and ticked.
        assert!(manifest.desktop_shortcut);
        assert!(manifest.launch_after_install_offered);
        assert!(manifest.launch_after_install_default);
        // The inert example component survives validation, claims no
        // files, and is off by default.
        assert_eq!(manifest.components.len(), 1);
        assert!(manifest.components[0].files.is_empty());
        assert!(!manifest.components[0].default_selected);
        let _ = reparse(&text).unwrap();
        // Second run refuses without --force, succeeds with it.
        assert!(init_project(&root, None, false).is_err());
        assert!(init_project(&root, None, true).is_ok());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn init_prefers_the_files_subfolder() {
        let root = unique_temp("filesdir");
        let files = root.join("files");
        std::fs::create_dir_all(&files).unwrap();
        std::fs::write(files.join("nested.exe"), b"fake").unwrap();
        let outcome = init_project(&root, Some("Named App"), false).unwrap();
        assert_eq!(outcome.files_dir, "files");
        assert_eq!(outcome.app_name, "Named App");
        assert_eq!(outcome.primary_exe.as_deref(), Some("nested.exe"));
        assert_eq!(outcome.pack_command, "seanpak pack manifest.json files");
        let packaging = std::fs::read_to_string(outcome.packaging_path.as_ref().unwrap()).unwrap();
        assert!(packaging.contains("seanpak pack manifest.json files"));
        let _ = std::fs::remove_dir_all(&root);
    }
}
