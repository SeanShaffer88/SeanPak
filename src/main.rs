#![windows_subsystem = "windows"]

use std::fs::File;
use std::path::{Path, PathBuf};

use seanpak::backends::{
    BuildConfig, InnoSetupBackend, InstallerBackend, NativeSfxBackend, NsisBackend,
};
use seanpak::diag;
use seanpak::install;
use seanpak::manifest::{jobj, jstr, AppManifest, JsonValue, MANIFEST_SCHEMA_JSON};
use seanpak::overlay::{
    FileRecordHeader, InstallOptions, OverlayManifestHeader, OverlayReader, OverlayWriter,
};
use seanpak::pack::{PackError, PackOutcome};
use seanpak::rollback::TransactionRollbackLog;
use seanpak::scanner::ScannedFolder;
use seanpak::win32_ui::NativeWin32App;
use seanpak::wizard::InstallWizard;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// Deterministic exit codes (documented in --help and the README; a local
// AI driving SeanPak branches on these, never on message text).
const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;
const EXIT_BAD_MANIFEST: i32 = 3;
const EXIT_IO: i32 = 4;
const EXIT_INSTALL: i32 = 5;
// (1 remains the generic GUI/startup failure used by the window paths.)

fn json_mode(args: &[String]) -> bool {
    args.iter().any(|a| a == "--json")
}

fn quiet_mode(args: &[String]) -> bool {
    args.iter().any(|a| a == "--silent" || a == "--headless")
}

fn print_json(value: &JsonValue) {
    println!("{}", value.render());
}

fn json_error(command: &str, code: i32, message: &str) -> JsonValue {
    jobj(vec![
        ("command", jstr(command)),
        ("status", jstr("error")),
        ("error", jstr(message.to_string())),
        ("exitCode", JsonValue::Number(code as f64)),
    ])
}

/// Prints an error (JSON when --json is active, plain otherwise) and
/// returns the exit code to terminate with.
fn fail(args: &[String], command: &str, code: i32, message: &str) -> i32 {
    if json_mode(args) {
        print_json(&json_error(command, code, message));
    } else {
        eprintln!("[ERROR] {message}");
    }
    code
}

fn jnum(n: u64) -> JsonValue {
    JsonValue::Number(n as f64)
}

fn jpath(p: &Path) -> JsonValue {
    jstr(p.display().to_string())
}

fn print_usage() {
    println!(
        "SeanPak {VERSION} - native Windows installer packager (PE overlay engine)\n\
         \n\
         PRIMARY WORKFLOW (headless packaging, designed for script/AI control):\n\
         \x20 seanpak.exe pack <manifest.json> <files-dir> [--out-dir <dir>] [--force] [--json]\n\
         \x20                                      Build the release set from a JSON manifest:\n\
         \x20                                      <App>-v<ver>-x64-Setup.exe + -Portable.zip\n\
         \x20                                      + SHA256SUMS.txt, named from the manifest\n\
         \x20 seanpak.exe verify <Setup.exe> [--sandbox] [--json]\n\
         \x20                                      Prove a built installer: overlay readable,\n\
         \x20                                      embedded manifest sane, payloads present.\n\
         \x20                                      --sandbox goes further: silent-install into\n\
         \x20                                      a fresh temp dir, compare every file\n\
         \x20                                      byte-for-byte, silent-uninstall, assert no\n\
         \x20                                      install residue remains\n\
         \x20 seanpak.exe schema                   Print the JSON Schema for the app manifest\n\
         \x20 seanpak.exe validate <manifest.json> [--json]\n\
         \x20                                      Check a manifest before packing with it\n\
         \n\
         MORE COMMANDS:\n\
         \x20 seanpak.exe                          Launch the packager GUI (secondary surface)\n\
         \x20 seanpak.exe <folder>                 Launch the GUI with a source folder\n\
         \x20 seanpak.exe --batch <folder> --out <Setup.exe> [--engine native|inno|nsis]\n\
         \x20                                      [--app-name <name>] [--app-version <v>]\n\
         \x20                                      [--publisher <name>] [--script-only] [--json]\n\
         \x20 seanpak.exe --list <packed.exe> [--json]   List files in a SeanPak overlay\n\
         \x20 seanpak.exe --extract <packed.exe> --to <dir> [--json]\n\
         \x20                                      Extract a SeanPak overlay to a directory\n\
         \x20 seanpak.exe --install [--to <dir>] [--dry-run] [--json] [--silent|--headless]\n\
         \x20                                      [--repair] [--force]\n\
         \x20                                      Install this executable's own overlay: the\n\
         \x20                                      same operation the wizard's Install button\n\
         \x20                                      runs (extract + Uninstall.exe + uninstall\n\
         \x20                                      entry + the manifest's shortcuts, file\n\
         \x20                                      associations and PATH entry, all journaled\n\
         \x20                                      for uninstall, rolled back on failure). An\n\
         \x20                                      existing install is detected: same version\n\
         \x20                                      repairs with --repair, a newer version makes\n\
         \x20                                      downgrade refused unless --force, an older\n\
         \x20                                      one upgrades in place. Default destination:\n\
         \x20                                      a folder named after the app, next to the exe\n\
         \x20 seanpak.exe --uninstall [--to <dir>] [--json]\n\
         \x20                                      Remove an installed app: deletes its payload\n\
         \x20                                      files, its uninstall entry, the journaled\n\
         \x20                                      shortcuts/associations/PATH segment recorded\n\
         \x20                                      in seanpak-receipt.json, and the receipt\n\
         \x20 seanpak.exe --help                   This help\n\
         \x20 seanpak.exe --version                Print version\n\
         \n\
         GLOBAL FLAGS: --json (machine-readable output on every command),\n\
         --silent / --headless (no interactive output beyond the result).\n\
         \n\
         EXIT CODES: 0 success · 1 generic/GUI failure · 2 usage error ·\n\
         3 bad or missing manifest/overlay · 4 I/O failure (including a refused\n\
         overwrite) · 5 install failed (destination rolled back).\n\
         \n\
         A built native installer is this same executable with a payload overlay:\n\
         running it with no arguments opens the SeanPak installer wizard."
    );
}

fn flag_value(args: &[String], names: &[&str]) -> Option<String> {
    for name in names {
        if let Some(pos) = args.iter().position(|a| a == name) {
            if let Some(value) = args.get(pos + 1) {
                return Some(value.clone());
            }
        }
    }
    None
}

fn has_flag(args: &[String], names: &[&str]) -> bool {
    args.iter().any(|a| names.contains(&a.as_str()))
}

/// If this executable itself carries a SeanPak overlay, returns the app name
/// recorded in its manifest. A plain packager binary has no overlay.
fn own_overlay_app_name() -> Option<String> {
    let exe = std::env::current_exe().ok()?;
    let file = File::open(exe).ok()?;
    let (manifest, _records) = OverlayReader::read_manifest(file).ok()??;
    Some(manifest.app_name())
}

/// If this executable itself carries a SeanPak overlay, returns its full
/// package: manifest, file records, and install options (defaults for
/// pre-4.0 packages). A plain packager binary has no overlay.
fn own_overlay_package() -> Option<(OverlayManifestHeader, Vec<FileRecordHeader>, InstallOptions)> {
    let exe = std::env::current_exe().ok()?;
    let file = File::open(exe).ok()?;
    OverlayReader::read_package(file).ok()?
}

fn sanitize_folder_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim().trim_matches('.');
    if trimmed.is_empty() {
        "App".to_string()
    } else {
        trimmed.to_string()
    }
}

fn extract_overlay(packed: &Path, dest: &Path) -> Result<Vec<PathBuf>, String> {
    OverlayReader::extract_file(packed, dest).map_err(|e| format!("Extraction failed: {e}"))
}

fn run_extract(args: &[String], packed: &Path, dest: &Path) -> i32 {
    match extract_overlay(packed, dest) {
        Ok(written) => {
            if json_mode(args) {
                print_json(&jobj(vec![
                    ("command", jstr("extract")),
                    ("status", jstr("ok")),
                    ("extracted", jnum(written.len() as u64)),
                    ("to", jpath(dest)),
                    (
                        "files",
                        JsonValue::Array(written.iter().map(|p| jpath(p.as_path())).collect()),
                    ),
                ]));
            } else {
                println!(
                    "{{\"status\":\"ok\",\"extracted\":{},\"to\":\"{}\"}}",
                    written.len(),
                    dest.display()
                );
                for path in &written {
                    println!("  {}", path.display());
                }
            }
            EXIT_OK
        }
        Err(e) => fail(args, "extract", EXIT_IO, &e),
    }
}

/// Install mode for a built installer: runs the same install operation
/// as the wizard's Install button (see `install::perform_install`) against
/// this executable's own overlay. Default destination is a folder named
/// after the app, next to the executable. `--dry-run` reports the exact
/// plan and changes nothing.
fn run_install(args: &[String]) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            return fail(
                args,
                "install",
                EXIT_IO,
                &format!("Cannot locate own executable: {e}"),
            )
        }
    };
    let (manifest, records, options) = match install::read_own_package(&exe) {
        Ok(pkg) => pkg,
        Err(e) => return fail(args, "install", EXIT_BAD_MANIFEST, &e),
    };
    let dest = match flag_value(args, &["--to"]) {
        Some(dir) => PathBuf::from(dir),
        None => {
            let app = own_overlay_app_name().unwrap_or_else(|| "App".to_string());
            exe.parent()
                .map(|p| p.join(sanitize_folder_name(&app)))
                .unwrap_or_else(|| PathBuf::from(sanitize_folder_name(&app)))
        }
    };
    let plan = install::plan_install(&manifest, &records, &options, &dest);

    if has_flag(args, &["--dry-run"]) {
        diag::record(
            "install",
            vec![
                ("mode", jstr("dry-run")),
                ("app", jstr(plan.app_name.clone())),
                ("dest", jpath(&plan.dest)),
                ("files", jnum(plan.files.len() as u64)),
            ],
        );
        if json_mode(args) {
            print_json(&jobj(vec![
                ("command", jstr("install")),
                ("status", jstr("dry-run")),
                ("app", jstr(plan.app_name.clone())),
                ("version", jstr(plan.app_version.clone())),
                ("mode", jstr(plan.mode.clone())),
                (
                    "previousVersion",
                    plan.previous_version
                        .clone()
                        .map(jstr)
                        .unwrap_or(JsonValue::Null),
                ),
                ("dest", jpath(&plan.dest)),
                (
                    "files",
                    JsonValue::Array(
                        plan.files
                            .iter()
                            .map(|(rel, size)| {
                                jobj(vec![("path", jstr(rel.clone())), ("size", jnum(*size))])
                            })
                            .collect(),
                    ),
                ),
                ("totalBytes", jnum(plan.total_bytes)),
                ("uninstaller", jpath(&plan.uninstaller_path)),
                ("arpKey", jstr(plan.arp_key.clone())),
                (
                    "shortcuts",
                    JsonValue::Array(plan.shortcuts.iter().map(|s| jstr(s.clone())).collect()),
                ),
                (
                    "associations",
                    JsonValue::Array(plan.associations.iter().map(|s| jstr(s.clone())).collect()),
                ),
                ("pathEntry", JsonValue::Bool(plan.path_entry)),
                ("changes", JsonValue::Bool(false)),
            ]));
        } else {
            println!(
                "Dry run: would {} {} {} to {}",
                plan.mode,
                plan.app_name,
                plan.app_version,
                plan.dest.display()
            );
            if let Some(prev) = &plan.previous_version {
                println!("  (version {prev} is installed there now)");
            }
            for (rel, size) in &plan.files {
                println!("  {rel} ({size} bytes)");
            }
            println!("  + Uninstall.exe and uninstall entry {}", plan.arp_key);
            for link in &plan.shortcuts {
                println!("  + shortcut {link}");
            }
            for ext in &plan.associations {
                println!("  + file association .{ext}");
            }
            if plan.path_entry {
                println!("  + PATH entry for the install folder");
            }
            println!("Nothing was changed.");
        }
        return EXIT_OK;
    }

    let request = install::InstallRequest {
        options_override: None,
        force: has_flag(args, &["--force"]),
        repair: has_flag(args, &["--repair"]),
    };
    match install::perform_install_with(&exe, &dest, &request, None, None) {
        Ok(outcome) => {
            diag::record(
                "install",
                vec![
                    ("mode", jstr("real")),
                    ("app", jstr(plan.app_name.clone())),
                    ("dest", jpath(&outcome.dest)),
                    ("files", jnum(outcome.files_written as u64)),
                    ("arpWritten", JsonValue::Bool(outcome.arp_written)),
                    ("installMode", jstr(outcome.mode.clone())),
                ],
            );
            if json_mode(args) {
                print_json(&jobj(vec![
                    ("command", jstr("install")),
                    ("status", jstr("ok")),
                    ("app", jstr(plan.app_name.clone())),
                    ("version", jstr(plan.app_version.clone())),
                    ("installMode", jstr(outcome.mode.clone())),
                    (
                        "previousVersion",
                        outcome
                            .previous_version
                            .clone()
                            .map(jstr)
                            .unwrap_or(JsonValue::Null),
                    ),
                    ("dest", jpath(&outcome.dest)),
                    ("installed", jnum(outcome.files_written as u64)),
                    ("restoredFiles", jnum(outcome.restored_files as u64)),
                    ("arpWritten", JsonValue::Bool(outcome.arp_written)),
                    ("receipt", jpath(&outcome.receipt_path)),
                    ("shortcutsCreated", jnum(outcome.shortcuts_created as u64)),
                    (
                        "associationsApplied",
                        jnum(outcome.associations_applied as u64),
                    ),
                    ("pathEntryAdded", JsonValue::Bool(outcome.path_entry_added)),
                ]));
            } else if !quiet_mode(args) {
                println!(
                    "Installed {} files to {} (uninstall entry: {}).",
                    outcome.files_written,
                    outcome.dest.display(),
                    if outcome.arp_written {
                        "written"
                    } else {
                        "not applicable on this platform"
                    }
                );
                println!("Receipt: {}", outcome.receipt_path.display());
            }
            EXIT_OK
        }
        Err(e) => {
            diag::record(
                "install",
                vec![
                    ("mode", jstr("real")),
                    ("status", jstr("error")),
                    ("error", jstr(e.clone())),
                ],
            );
            fail(args, "install", EXIT_INSTALL, &e)
        }
    }
}

/// Uninstall mode (the ARP entry's uninstall string points here): deletes
/// every payload file this executable's overlay records under the install
/// directory, removes now-empty payload subdirectories, and removes the
/// Add/Remove Programs entry. The running `Uninstall.exe` copy itself
/// cannot be deleted while it runs; it is reported and left behind.
fn run_uninstall(args: &[String]) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[ERROR] Cannot locate own executable: {e}");
            return 1;
        }
    };
    let (manifest, records, options) = match own_overlay_package() {
        Some(pkg) => pkg,
        None => {
            return fail(
                args,
                "uninstall",
                EXIT_BAD_MANIFEST,
                "This executable carries no SeanPak overlay; nothing to uninstall",
            );
        }
    };
    let dest = match flag_value(args, &["--to"]) {
        Some(dir) => PathBuf::from(dir),
        None => seanpak::wizard::default_install_dir(&options, &manifest.app_name()),
    };

    // The install receipt is the journal of system artifacts (shortcuts,
    // associations, PATH segment) this install applied; read it before
    // anything is deleted so uninstall can remove exactly those.
    let receipt = seanpak::receipt::read_receipt(&dest);

    let mut removed: Vec<String> = Vec::new();
    let mut subdirs: Vec<PathBuf> = Vec::new();
    for record in &records {
        let rel = record.relative_path().replace('\\', "/");
        if rel.is_empty() || rel.contains("..") {
            continue;
        }
        let target = dest.join(&rel);
        if target.is_file() {
            match std::fs::remove_file(&target) {
                Ok(()) => removed.push(rel),
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
    // Remove the journaled system artifacts and the receipt itself.
    let mut artifacts_removed = 0usize;
    if let Some(receipt) = &receipt {
        artifacts_removed = receipt.artifacts.shortcuts.len()
            + receipt.artifacts.associations.len()
            + receipt.artifacts.path_segment_added.is_some() as usize;
        seanpak::sysinstall::remove_artifacts(&receipt.artifacts);
    }
    let receipt_removed = std::fs::remove_file(seanpak::receipt::receipt_path(&dest)).is_ok();
    seanpak::wizard::delete_uninstall_entry(&manifest.app_name());
    diag::record(
        "uninstall",
        vec![
            ("app", jstr(manifest.app_name())),
            ("from", jpath(&dest)),
            ("removed", jnum(removed.len() as u64)),
            ("artifactsRemoved", jnum(artifacts_removed as u64)),
        ],
    );
    let self_copy_note = exe.starts_with(&dest);
    if json_mode(args) {
        print_json(&jobj(vec![
            ("command", jstr("uninstall")),
            ("status", jstr("ok")),
            ("uninstalled", jstr(manifest.app_name())),
            (
                "removed",
                JsonValue::Array(removed.iter().map(|r| jstr(r.clone())).collect()),
            ),
            ("artifactsRemoved", jnum(artifacts_removed as u64)),
            ("receiptRemoved", JsonValue::Bool(receipt_removed)),
            ("from", jpath(&dest)),
            ("selfCopyLeftBehind", JsonValue::Bool(self_copy_note)),
        ]));
    } else {
        println!(
            "Uninstalled {}: removed {} file(s) from {}",
            manifest.app_name(),
            removed.len(),
            dest.display()
        );
        for rel in &removed {
            println!("  {rel}");
        }
        if self_copy_note {
            println!(
                "Note: {} is the running uninstaller copy and cannot delete itself; remove it manually.",
                exe.display()
            );
        }
    }
    EXIT_OK
}

/// Bare-run mode for a built installer: opens the Design C wizard for this
/// executable's own overlay package.
fn run_wizard(
    manifest: OverlayManifestHeader,
    records: Vec<FileRecordHeader>,
    options: InstallOptions,
) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[ERROR] Cannot locate own executable: {e}");
            return 1;
        }
    };
    let mut wizard = InstallWizard::new(manifest, records, options, exe);
    match wizard.run() {
        Ok(()) => 0,
        Err(err_code) => {
            let message = format!(
                "SeanPak could not start the installer wizard (Windows error {err_code}).\n\nA diagnostic log was written to:\n{}",
                diag::log_path().display()
            );
            diag::log("fatal", &message.replace('\n', " "));
            diag::error_box("SeanPak installer failed to start", &message);
            err_code as i32
        }
    }
}

fn run_list(args: &[String], packed: &Path) -> i32 {
    let file = match File::open(packed) {
        Ok(f) => f,
        Err(e) => {
            return fail(
                args,
                "list",
                EXIT_IO,
                &format!("Cannot open {}: {e}", packed.display()),
            )
        }
    };
    match OverlayReader::read_manifest(file) {
        Ok(Some((manifest, records))) => {
            if json_mode(args) {
                print_json(&jobj(vec![
                    ("command", jstr("list")),
                    ("status", jstr("ok")),
                    ("app", jstr(manifest.app_name())),
                    ("version", jstr(manifest.app_version())),
                    ("publisher", jstr(manifest.publisher())),
                    ("fileCount", jnum(manifest.file_count as u64)),
                    ("totalBytes", jnum(manifest.total_uncompressed_bytes)),
                    (
                        "files",
                        JsonValue::Array(
                            records
                                .iter()
                                .enumerate()
                                .map(|(i, r)| {
                                    jobj(vec![
                                        ("path", jstr(r.relative_path())),
                                        ("size", jnum(r.file_size)),
                                        (
                                            "primary",
                                            JsonValue::Bool(i as u32 == manifest.primary_exe_index),
                                        ),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ]));
            } else {
                println!("App: {} {}", manifest.app_name(), manifest.app_version());
                println!("Publisher: {}", manifest.publisher());
                println!(
                    "Files: {} ({} bytes uncompressed)",
                    manifest.file_count, manifest.total_uncompressed_bytes
                );
                for (i, record) in records.iter().enumerate() {
                    let marker = if i as u32 == manifest.primary_exe_index {
                        " [primary]"
                    } else {
                        ""
                    };
                    println!(
                        "  {} ({} bytes){}",
                        record.relative_path(),
                        record.file_size,
                        marker
                    );
                }
            }
            EXIT_OK
        }
        Ok(None) => fail(
            args,
            "list",
            EXIT_BAD_MANIFEST,
            &format!("No SeanPak overlay found in {}", packed.display()),
        ),
        Err(e) => fail(
            args,
            "list",
            EXIT_IO,
            &format!("Failed to read {}: {e}", packed.display()),
        ),
    }
}

fn run_batch(args: &[String], target_dir_str: &str) -> i32 {
    let target_path = PathBuf::from(target_dir_str);
    if !target_path.is_dir() {
        return fail(
            args,
            "batch",
            EXIT_USAGE,
            &format!("Target path is not a directory: {}", target_path.display()),
        );
    }

    let scanned = match ScannedFolder::scan(&target_path) {
        Ok(s) => s,
        Err(e) => {
            return fail(
                args,
                "batch",
                EXIT_IO,
                &format!("Directory scan failed: {e}"),
            )
        }
    };

    let engine = flag_value(args, &["--engine"]).unwrap_or_else(|| "native".to_string());
    let app_name =
        flag_value(args, &["--app-name"]).unwrap_or_else(|| scanned.suggested_app_name.clone());
    let version = flag_value(args, &["--app-version"]).unwrap_or_else(|| "1.0.0.0".to_string());
    let publisher =
        flag_value(args, &["--publisher"]).unwrap_or_else(|| "SeanPak Native".to_string());
    let script_only = has_flag(args, &["--script-only"]);

    let default_out_name = match engine.as_str() {
        "inno" => format!("{}.iss", app_name.replace(' ', "_")),
        "nsis" => format!("{}.nsi", app_name.replace(' ', "_")),
        _ => "Setup.exe".to_string(),
    };
    let output = flag_value(args, &["--out", "-o"])
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            target_path
                .parent()
                .unwrap_or(&target_path)
                .join(default_out_name)
        });

    let config = BuildConfig {
        app_name: app_name.clone(),
        version: version.clone(),
        publisher: publisher.clone(),
        source_dir: target_path.clone(),
        primary_exe: scanned
            .suggested_primary_exe
            .clone()
            .unwrap_or_else(|| "app.exe".to_string()),
        output_dir: output
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from(".")),
        output_exe_name: output
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Setup.exe".to_string()),
        ..Default::default()
    };

    let backend: Box<dyn InstallerBackend> = match engine.as_str() {
        "native" => Box::new(NativeSfxBackend::new()),
        "inno" => Box::new(InnoSetupBackend::new()),
        "nsis" => Box::new(NsisBackend::new()),
        other => {
            return fail(
                args,
                "batch",
                EXIT_USAGE,
                &format!("Unknown engine '{other}' (expected native, inno, or nsis)"),
            );
        }
    };

    if script_only {
        return match backend.generate_script(&config) {
            Ok(script) => {
                if let Some(parent) = output.parent() {
                    if !parent.as_os_str().is_empty() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                }
                match std::fs::write(&output, script) {
                    Ok(()) => {
                        diag::record(
                            "build",
                            vec![("engine", jstr(engine.clone())), ("script", jpath(&output))],
                        );
                        if json_mode(args) {
                            print_json(&jobj(vec![
                                ("command", jstr("batch")),
                                ("status", jstr("ok")),
                                ("script", jpath(&output)),
                                ("engine", jstr(engine.clone())),
                            ]));
                        } else {
                            println!("Built {} script: {}", engine, output.display());
                        }
                        EXIT_OK
                    }
                    Err(e) => fail(
                        args,
                        "batch",
                        EXIT_IO,
                        &format!("Writing script failed: {e}"),
                    ),
                }
            }
            Err(e) => fail(
                args,
                "batch",
                EXIT_IO,
                &format!("Script generation failed: {e}"),
            ),
        };
    }

    if engine == "native" {
        // Keep the transactional snapshot/rollback around the native build:
        // an existing output file is restored if packaging fails midway.
        let current_exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("seanpak.exe"));
        let mut tx_log = match TransactionRollbackLog::new() {
            Ok(tx) => tx,
            Err(e) => {
                return fail(
                    args,
                    "batch",
                    EXIT_IO,
                    &format!("Failed to initialize rollback log: {e}"),
                )
            }
        };
        if let Err(e) = tx_log.snapshot_file_before_write(&output) {
            return fail(
                args,
                "batch",
                EXIT_IO,
                &format!("Snapshot staging failed: {e}"),
            );
        }
        return match OverlayWriter::build_from_folder(
            &current_exe,
            &output,
            &target_path,
            &app_name,
            &version,
            &publisher,
        ) {
            Ok(bytes) => {
                let _ = tx_log.commit();
                diag::record(
                    "build",
                    vec![
                        ("engine", jstr("native")),
                        ("output", jpath(&output)),
                        ("bytes", jnum(bytes)),
                        ("files", jnum(scanned.files.len() as u64)),
                    ],
                );
                if json_mode(args) {
                    print_json(&jobj(vec![
                        ("command", jstr("batch")),
                        ("status", jstr("ok")),
                        ("output", jpath(&output)),
                        ("bytes", jnum(bytes)),
                        ("files", jnum(scanned.files.len() as u64)),
                        ("engine", jstr("native")),
                    ]));
                } else {
                    println!(
                        "Built native installer: {} ({bytes} bytes, {} files)",
                        output.display(),
                        scanned.files.len()
                    );
                }
                EXIT_OK
            }
            Err(e) => {
                tx_log.rollback();
                fail(
                    args,
                    "batch",
                    EXIT_IO,
                    &format!("Overlay packaging failed: {e}"),
                )
            }
        };
    }

    // Inno / NSIS: the backend writes its script and invokes the compiler.
    let mut log_lines: Vec<String> = Vec::new();
    let result = backend.build(&config, &mut |line| log_lines.push(line.to_string()));
    if !json_mode(args) && !quiet_mode(args) {
        for line in &log_lines {
            println!("[{engine}] {line}");
        }
    }
    match result {
        Ok(path) => {
            diag::record(
                "build",
                vec![("engine", jstr(engine.clone())), ("output", jpath(&path))],
            );
            if json_mode(args) {
                print_json(&jobj(vec![
                    ("command", jstr("batch")),
                    ("status", jstr("ok")),
                    ("output", jpath(&path)),
                    ("engine", jstr(engine.clone())),
                ]));
            } else if !quiet_mode(args) {
                println!("Built {engine} installer: {}", path.display());
            }
            EXIT_OK
        }
        Err(e) => fail(
            args,
            "batch",
            EXIT_IO,
            &format!("{engine} build failed: {e}"),
        ),
    }
}

// ---------------------------------------------------------------------------
// AI-control commands: schema / validate / pack / verify.

fn run_schema(args: &[String]) -> i32 {
    let _ = args;
    println!("{MANIFEST_SCHEMA_JSON}");
    EXIT_OK
}

fn run_validate(args: &[String]) -> i32 {
    let path = match args.get(1) {
        Some(p) if !p.starts_with('-') => p.clone(),
        _ => {
            return fail(
                args,
                "validate",
                EXIT_USAGE,
                "usage: seanpak validate <manifest.json> [--json]",
            )
        }
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            return fail(
                args,
                "validate",
                EXIT_IO,
                &format!("Cannot read {path}: {e}"),
            )
        }
    };
    match AppManifest::from_json(&text) {
        Ok(manifest) => {
            if json_mode(args) {
                print_json(&jobj(vec![
                    ("command", jstr("validate")),
                    ("status", jstr("ok")),
                    ("manifest", manifest.to_json_value()),
                ]));
            } else {
                println!(
                    "Manifest OK: {} {} (assets: {}, {})",
                    manifest.app_name,
                    manifest.version,
                    manifest.setup_file_name(),
                    manifest.portable_file_name()
                );
            }
            EXIT_OK
        }
        Err(errors) => {
            if json_mode(args) {
                print_json(&jobj(vec![
                    ("command", jstr("validate")),
                    ("status", jstr("error")),
                    (
                        "errors",
                        JsonValue::Array(errors.iter().map(|e| jstr(e.clone())).collect()),
                    ),
                    ("exitCode", JsonValue::Number(EXIT_BAD_MANIFEST as f64)),
                ]));
            } else {
                for e in &errors {
                    eprintln!("[ERROR] {e}");
                }
            }
            EXIT_BAD_MANIFEST
        }
    }
}

/// Parses `pack` arguments: positionals are manifest + files dir; value
/// flags consume the following token.
fn run_pack(args: &[String]) -> i32 {
    let mut positional: Vec<String> = Vec::new();
    let mut out_dir = PathBuf::from("dist");
    let mut force = false;
    let mut i = 1;
    while i < args.len() {
        let a = &args[i];
        match a.as_str() {
            "--out-dir" => match args.get(i + 1) {
                Some(v) => {
                    out_dir = PathBuf::from(v);
                    i += 1;
                }
                None => return fail(args, "pack", EXIT_USAGE, "--out-dir requires a directory"),
            },
            "--force" => force = true,
            _ if a.starts_with('-') => {}
            _ => positional.push(a.clone()),
        }
        i += 1;
    }
    if positional.len() != 2 {
        return fail(
            args,
            "pack",
            EXIT_USAGE,
            "usage: seanpak pack <manifest.json> <files-dir> [--out-dir <dir>] [--force] [--json]",
        );
    }
    let (manifest_path, files_dir) = (&positional[0], PathBuf::from(&positional[1]));
    let text = match std::fs::read_to_string(manifest_path) {
        Ok(t) => t,
        Err(e) => {
            return fail(
                args,
                "pack",
                EXIT_IO,
                &format!("Cannot read {manifest_path}: {e}"),
            )
        }
    };
    let manifest = match AppManifest::from_json(&text) {
        Ok(m) => m,
        Err(errors) => {
            let joined = errors.join("; ");
            return fail(
                args,
                "pack",
                EXIT_BAD_MANIFEST,
                &format!("Invalid manifest {manifest_path}: {joined}"),
            );
        }
    };
    if !files_dir.is_dir() {
        return fail(
            args,
            "pack",
            EXIT_USAGE,
            &format!("Files path is not a directory: {}", files_dir.display()),
        );
    }
    let stub = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            return fail(
                args,
                "pack",
                EXIT_IO,
                &format!("Cannot locate own executable: {e}"),
            )
        }
    };
    match seanpak::pack::pack(&manifest, &files_dir, &out_dir, &stub, force) {
        Ok(outcome) => {
            diag::record(
                "pack",
                vec![
                    ("app", jstr(manifest.app_name.clone())),
                    ("version", jstr(manifest.version.clone())),
                    ("setup", jpath(&outcome.setup.path)),
                    ("setupSha256", jstr(outcome.setup.sha256.clone())),
                    ("portable", jpath(&outcome.portable.path)),
                    ("portableSha256", jstr(outcome.portable.sha256.clone())),
                    ("stubBytes", jnum(outcome.stub_bytes)),
                ],
            );
            if json_mode(args) {
                print_json(&pack_report_json(&manifest, &outcome));
            } else if !quiet_mode(args) {
                println!("Packed {} {}:", manifest.app_name, manifest.version);
                println!(
                    "  {} ({} bytes, sha256 {})",
                    outcome.setup.path.display(),
                    outcome.setup.size,
                    outcome.setup.sha256
                );
                println!(
                    "  {} ({} bytes, sha256 {})",
                    outcome.portable.path.display(),
                    outcome.portable.size,
                    outcome.portable.sha256
                );
                println!("  {}", outcome.sums_path.display());
            }
            EXIT_OK
        }
        Err(PackError::BadFiles(m)) => fail(args, "pack", EXIT_BAD_MANIFEST, &m),
        Err(PackError::RefuseOverwrite(m)) => fail(args, "pack", EXIT_IO, &m),
        Err(PackError::Io(e)) => fail(args, "pack", EXIT_IO, &format!("Packaging failed: {e}")),
    }
}

fn pack_report_json(manifest: &AppManifest, outcome: &PackOutcome) -> JsonValue {
    let asset = |a: &seanpak::pack::AssetInfo| {
        jobj(vec![
            ("name", jstr(a.name.clone())),
            ("path", jpath(&a.path)),
            ("size", jnum(a.size)),
            ("sha256", jstr(a.sha256.clone())),
        ])
    };
    jobj(vec![
        ("command", jstr("pack")),
        ("status", jstr("ok")),
        ("appName", jstr(manifest.app_name.clone())),
        ("version", jstr(manifest.version.clone())),
        ("stubBytes", jnum(outcome.stub_bytes)),
        ("fileCount", jnum(outcome.file_count as u64)),
        ("totalPayloadBytes", jnum(outcome.total_payload_bytes)),
        ("manifest", manifest.to_json_value()),
        (
            "assets",
            JsonValue::Array(vec![asset(&outcome.setup), asset(&outcome.portable)]),
        ),
        ("sha256sums", jpath(&outcome.sums_path)),
    ])
}

fn run_verify(args: &[String]) -> i32 {
    let path = match args.get(1) {
        Some(p) if !p.starts_with('-') => PathBuf::from(p),
        _ => {
            return fail(
                args,
                "verify",
                EXIT_USAGE,
                "usage: seanpak verify <Setup.exe> [--sandbox] [--json]",
            )
        }
    };
    if has_flag(args, &["--sandbox"]) {
        return run_verify_sandbox(args, &path);
    }
    let report = match seanpak::pack::verify_package(&path) {
        Ok(r) => r,
        Err(e) => {
            return fail(
                args,
                "verify",
                EXIT_IO,
                &format!("Cannot verify {}: {e}", path.display()),
            )
        }
    };
    diag::record(
        "verify",
        vec![
            ("path", jpath(&path)),
            ("ok", JsonValue::Bool(report.ok)),
            ("app", jstr(report.app_name.clone())),
        ],
    );
    if json_mode(args) {
        print_json(&jobj(vec![
            ("command", jstr("verify")),
            ("status", jstr(if report.ok { "ok" } else { "failed" })),
            ("path", jpath(&path)),
            ("app", jstr(report.app_name.clone())),
            ("version", jstr(report.app_version.clone())),
            ("publisher", jstr(report.publisher.clone())),
            ("manifestVersion", jnum(report.manifest_version as u64)),
            ("fileCount", jnum(report.file_count as u64)),
            ("totalBytes", jnum(report.total_bytes)),
            (
                "files",
                JsonValue::Array(
                    report
                        .files
                        .iter()
                        .map(|(rel, size)| {
                            jobj(vec![("path", jstr(rel.clone())), ("size", jnum(*size))])
                        })
                        .collect(),
                ),
            ),
            (
                "checks",
                JsonValue::Array(
                    report
                        .checks
                        .iter()
                        .map(|(name, passed, detail)| {
                            jobj(vec![
                                ("name", jstr(name.clone())),
                                ("passed", JsonValue::Bool(*passed)),
                                ("detail", jstr(detail.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
        ]));
    } else {
        println!(
            "Verify {}: {}",
            path.display(),
            if report.ok { "PASS" } else { "FAIL" }
        );
        if !report.app_name.is_empty() {
            println!(
                "  App: {} {} (publisher {})",
                report.app_name, report.app_version, report.publisher
            );
        }
        for (name, passed, detail) in &report.checks {
            println!(
                "  [{}] {name}: {detail}",
                if *passed { "ok" } else { "FAIL" }
            );
        }
    }
    if report.ok {
        EXIT_OK
    } else {
        EXIT_BAD_MANIFEST
    }
}

/// `verify --sandbox`: the end-to-end proof — silent install into a
/// temp dir, byte-for-byte file comparison, silent uninstall, residue
/// check (see `pack::sandbox_verify`).
fn run_verify_sandbox(args: &[String], path: &Path) -> i32 {
    let report = match seanpak::pack::sandbox_verify(path) {
        Ok(r) => r,
        Err(e) => {
            return fail(
                args,
                "verify",
                EXIT_IO,
                &format!("Cannot sandbox-verify {}: {e}", path.display()),
            )
        }
    };
    diag::record(
        "verify-sandbox",
        vec![
            ("path", jpath(path)),
            ("ok", JsonValue::Bool(report.ok)),
            (
                "leftovers",
                JsonValue::Array(report.leftovers.iter().map(|l| jstr(l.clone())).collect()),
            ),
        ],
    );
    if json_mode(args) {
        print_json(&jobj(vec![
            ("command", jstr("verify")),
            ("sandbox", JsonValue::Bool(true)),
            ("status", jstr(if report.ok { "ok" } else { "failed" })),
            ("path", jpath(path)),
            ("installDir", jpath(&report.install_dir)),
            (
                "checks",
                JsonValue::Array(
                    report
                        .checks
                        .iter()
                        .map(|(name, passed, detail)| {
                            jobj(vec![
                                ("name", jstr(name.clone())),
                                ("passed", JsonValue::Bool(*passed)),
                                ("detail", jstr(detail.clone())),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "leftovers",
                JsonValue::Array(report.leftovers.iter().map(|l| jstr(l.clone())).collect()),
            ),
        ]));
    } else {
        println!(
            "Sandbox verify {}: {}",
            path.display(),
            if report.ok { "PASS" } else { "FAIL" }
        );
        for (name, passed, detail) in &report.checks {
            println!(
                "  [{}] {name}: {detail}",
                if *passed { "ok" } else { "FAIL" }
            );
        }
        if !report.leftovers.is_empty() {
            println!(
                "  Leftovers after uninstall: {}",
                report.leftovers.join(", ")
            );
        }
    }
    if report.ok {
        EXIT_OK
    } else {
        EXIT_INSTALL
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    // Any dash-flag argument means a CLI mode. The binary is GUI-subsystem,
    // so attach to the parent terminal before printing anything, or CLI
    // output (--help, --batch, ...) goes nowhere.
    if args.iter().any(|a| a.starts_with('-')) {
        diag::attach_parent_console();
    }
    diag::log("start", &format!("SeanPak {VERSION} args={args:?}"));

    if has_flag(&args, &["--help", "-h", "help"]) {
        print_usage();
        std::process::exit(0);
    }
    if has_flag(&args, &["--version", "-V"]) {
        println!("seanpak {VERSION}");
        std::process::exit(0);
    }

    // Word subcommands (the AI-control surface): schema / validate / pack / verify.
    match args.first().map(|a| a.as_str()) {
        Some("schema") => std::process::exit(run_schema(&args)),
        Some("validate") => std::process::exit(run_validate(&args)),
        Some("pack") => std::process::exit(run_pack(&args)),
        Some("verify") => std::process::exit(run_verify(&args)),
        _ => {}
    }

    // Headless batch packaging: seanpak.exe --batch "C:\App" --out "dist\AppSetup.exe"
    if let Some(pos) = args.iter().position(|a| a == "--batch" || a == "-b") {
        match args.get(pos + 1) {
            Some(dir) => std::process::exit(run_batch(&args, dir)),
            None => {
                eprintln!("[ERROR] --batch requires a target directory");
                std::process::exit(EXIT_USAGE);
            }
        }
    }

    if let Some(pos) = args.iter().position(|a| a == "--list") {
        match args.iter().skip(pos + 1).find(|a| !a.starts_with('-')) {
            Some(file) => std::process::exit(run_list(&args, Path::new(file))),
            None => {
                eprintln!("[ERROR] --list requires a packed executable path");
                std::process::exit(EXIT_USAGE);
            }
        }
    }

    if let Some(pos) = args.iter().position(|a| a == "--extract") {
        match args.iter().skip(pos + 1).find(|a| !a.starts_with('-')) {
            Some(file) => {
                let dest = flag_value(&args, &["--to"])
                    .map(PathBuf::from)
                    .unwrap_or_else(|| PathBuf::from("extracted"));
                std::process::exit(run_extract(&args, Path::new(file), &dest));
            }
            None => {
                eprintln!("[ERROR] --extract requires a packed executable path");
                std::process::exit(EXIT_USAGE);
            }
        }
    }

    // The copied Uninstall.exe in an installed app carries the same overlay
    // and is invoked by Windows with --uninstall (see wizard.rs).
    if has_flag(&args, &["--uninstall"]) {
        std::process::exit(run_uninstall(&args));
    }

    // A built native installer carries an overlay in its own executable:
    // --install extracts the payload headlessly, and running it with no
    // arguments at all opens the installer wizard (Design C) instead of
    // the packager.
    if has_flag(&args, &["--install", "--extract-self"]) {
        std::process::exit(run_install(&args));
    }
    if args.is_empty() {
        if let Some((manifest, records, options)) = own_overlay_package() {
            diag::log(
                "mode",
                &format!("installer wizard (own overlay: {})", manifest.app_name()),
            );
            std::process::exit(run_wizard(manifest, records, options));
        }
        diag::log("mode", "packager GUI (no overlay in own executable)");
    }

    // Interactive native Win32 GUI (Windows only).
    let initial_folder = if !args.is_empty() {
        let candidate = PathBuf::from(&args[0]);
        if candidate.is_dir() {
            Some(candidate)
        } else {
            candidate.parent().map(|p| p.to_path_buf())
        }
    } else {
        None
    };

    let mut app = NativeWin32App::new(initial_folder);
    if let Err(err_code) = app.run() {
        // Never die silently: the failure goes to the log AND a dialog.
        let message = format!(
            "SeanPak could not start its main window (Windows error {err_code}).\n\nA diagnostic log was written to:\n{}",
            diag::log_path().display()
        );
        diag::log("fatal", &message.replace('\n', " "));
        diag::error_box("SeanPak failed to start", &message);
        eprintln!("[ERROR] Win32 message loop terminated with error: {err_code}");
        std::process::exit(err_code as i32);
    }
    diag::log("exit", "clean");
}
