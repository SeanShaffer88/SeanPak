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
use seanpak::studio::BuilderStudio;
use seanpak::wizard::InstallWizard;

const VERSION: &str = env!("CARGO_PKG_VERSION");

// Deterministic exit codes (documented in --help and the README; a local
// AI driving SeanPak branches on these, never on message text).
const EXIT_OK: i32 = 0;
const EXIT_USAGE: i32 = 2;
const EXIT_BAD_MANIFEST: i32 = 3;
const EXIT_IO: i32 = 4;
const EXIT_INSTALL: i32 = 5;
const EXIT_DIFFERENT: i32 = 6;
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
         \x20 seanpak.exe init <dir> [--name <App>] [--force] [--json]\n\
         \x20                                      Scaffold a starter manifest.json (primary\n\
         \x20                                      exe detected, power blocks as inert\n\
         \x20                                      examples) + a PACKAGING.md stub in a\n\
         \x20                                      project folder. The generated manifest is\n\
         \x20                                      validated before init reports success;\n\
         \x20                                      an existing manifest is kept unless --force\n\
         \x20 seanpak.exe inspect <Setup.exe> [--json]\n\
         \x20                                      Report a package: identity, options,\n\
         \x20                                      components with packed sizes, hooks,\n\
         \x20                                      writes, shortcuts, per-file sizes and\n\
         \x20                                      compression\n\
         \x20 seanpak.exe diff <A.exe> <B.exe> [--json]\n\
         \x20                                      Compare two packages: files added,\n\
         \x20                                      removed, or changed by content hash,\n\
         \x20                                      plus option/component deltas. Exit 0 =\n\
         \x20                                      identical, 6 = differences found,\n\
         \x20                                      3 = a package could not be read\n\
         \x20 seanpak.exe answers-template <Setup.exe> [--json]\n\
         \x20                                      Print an answer-file template with the\n\
         \x20                                      package's defaults filled in (edit it,\n\
         \x20                                      then --install --answers <file>)\n\
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
         \x20                                      [--repair] [--force] [--launch]\n\
         \x20                                      [--start-at-login|--no-start-at-login]\n\
         \x20                                      [--components <id,id>] [--without <id,id>]\n\
         \x20                                      [--answers <file.json>]\n\
         \x20                                      --answers presets the install from an\n\
         \x20                                      answer file (location, options,\n\
         \x20                                      components); explicit flags still win\n\
         \x20                                      over the file, and a malformed file is a\n\
         \x20                                      usage error (exit 2) naming the field\n\
         \x20                                      Install this executable's own overlay: the\n\
         \x20                                      same operation the wizard's Install button\n\
         \x20                                      runs (extract + Uninstall.exe + uninstall\n\
         \x20                                      entry + the manifest's shortcuts, file\n\
         \x20                                      associations and PATH entry, all journaled\n\
         \x20                                      for uninstall, rolled back on failure). An\n\
         \x20                                      existing install is detected: same version\n\
         \x20                                      repairs with --repair, a newer version makes\n\
         \x20                                      downgrade refused unless --force, an older\n\
         \x20                                      one upgrades in place. Installs are per-user:\n\
         \x20                                      a destination your account cannot write is\n\
         \x20                                      refused before anything changes (JSON reason\n\
         \x20                                      \"location_not_writable\" plus a suggested\n\
         \x20                                      per-user location). --start-at-login /\n\
         \x20                                      --no-start-at-login override the package's\n\
         \x20                                      start-at-login default for this run;\n\
         \x20                                      --launch starts the app afterwards (an\n\
         \x20                                      explicit opt-in; headless installs never\n\
         \x20                                      launch by default). Default destination:\n\
         \x20                                      a folder named after the app, next to the exe\n\
         \x20 seanpak.exe --modify [--to <dir>] [--dry-run] [--json] [--silent|--headless]\n\
         \x20                                      [--components <id,id>] [--without <id,id>]\n\
         \x20                                      [--start-at-login|--no-start-at-login]\n\
         \x20                                      [--desktop-shortcut|--no-desktop-shortcut]\n\
         \x20                                      [--start-menu-shortcut|--no-start-menu-shortcut]\n\
         \x20                                      [--path-entry|--no-path-entry]\n\
         \x20                                      Change an existing install in place:\n\
         \x20                                      add/remove components, flip the\n\
         \x20                                      shortcut / association / PATH /\n\
         \x20                                      start-at-login choices, rewrite the\n\
         \x20                                      receipt. The pre-modify state is\n\
         \x20                                      snapshotted first; any failure rolls\n\
         \x20                                      back to it. With no option flags and a\n\
         \x20                                      console attached, opens the wizard in\n\
         \x20                                      Modify mode instead. The Add/Remove\n\
         \x20                                      Programs Modify button runs this too\n\
         \x20 seanpak.exe --uninstall [--to <dir>] [--json] [--purge-data]\n\
         \x20                                      Remove an installed app: deletes its payload\n\
         \x20                                      files, its uninstall entry, the journaled\n\
         \x20                                      shortcuts/associations/PATH segment/\n\
         \x20                                      start-at-login entry recorded in\n\
         \x20                                      seanpak-receipt.json, restores the registry\n\
         \x20                                      and environment values the install replaced\n\
         \x20                                      (v6 power packages), and deletes the receipt.\n\
         \x20                                      Interactive runs first show a confirmation\n\
         \x20                                      window with an opt-in checkbox to also\n\
         \x20                                      delete the app's recorded data folders.\n\
         \x20                                      Silent/JSON runs never delete data unless\n\
         \x20                                      --purge-data is passed\n\
         \x20 seanpak.exe --help                   This help\n\
         \x20 seanpak.exe --version                Print version\n\
         \n\
         GLOBAL FLAGS: --json (machine-readable output on every command),\n\
         --silent / --headless (no interactive output beyond the result).\n\
         \n\
         EXIT CODES: 0 success · 1 generic/GUI failure · 2 usage error ·\n\
         3 bad or missing manifest/overlay · 4 I/O failure (including a refused\n\
         overwrite) · 5 install failed (destination rolled back) · 6 differences\n\
         found (diff only).\n\
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

/// A comma-separated flag value as a list of trimmed, non-empty
/// items (`--components extras,engine`).
fn flag_list(args: &[String], names: &[&str]) -> Option<Vec<String>> {
    flag_value(args, names).map(|value| {
        value
            .split(',')
            .map(|item| item.trim().to_string())
            .filter(|item| !item.is_empty())
            .collect()
    })
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

/// [`own_overlay_package`] plus the package's v6 power block.
fn own_overlay_package_full() -> Option<seanpak::overlay::PackageFull> {
    let exe = std::env::current_exe().ok()?;
    let file = File::open(exe).ok()?;
    OverlayReader::read_package_full(file).ok()?
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
/// plan (including the destination writability verdict) and changes
/// nothing; a real run refuses an unwritable destination before the
/// operation starts, with the `location_not_writable` JSON reason.
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
    let (manifest, records, options, power) = match install::read_own_package_full(&exe) {
        Ok(pkg) => pkg,
        Err(e) => return fail(args, "install", EXIT_BAD_MANIFEST, &e),
    };
    // Answer file (v7): an explicit preset for this run. A malformed
    // file is a usage error naming the field, reported before
    // anything else happens. Priority: explicit CLI flags > answer
    // file > package defaults.
    let answers = match flag_value(args, &["--answers"]) {
        Some(path) => match seanpak::answers::load_answers(Path::new(&path)) {
            Ok(a) => Some(a),
            Err(errors) => {
                return fail(
                    args,
                    "install",
                    EXIT_USAGE,
                    &format!("invalid answers file {path}: {}", errors.join("; ")),
                )
            }
        },
        None => None,
    };
    let dest = match flag_value(args, &["--to"]) {
        Some(dir) => PathBuf::from(dir),
        None => match answers.as_ref().and_then(|a| a.install_location.clone()) {
            Some(location) => PathBuf::from(location),
            None => {
                let app = own_overlay_app_name().unwrap_or_else(|| "App".to_string());
                exe.parent()
                    .map(|p| p.join(sanitize_folder_name(&app)))
                    .unwrap_or_else(|| PathBuf::from(sanitize_folder_name(&app)))
            }
        },
    };
    // The start-at-login CLI flags override the package's recorded
    // choice for this run (the dry-run plan reflects them too).
    let start_at_login_override = if has_flag(args, &["--start-at-login"]) {
        Some(true)
    } else if has_flag(args, &["--no-start-at-login"]) {
        Some(false)
    } else {
        None
    };
    let mut options = options;
    if let Some(a) = &answers {
        a.apply_to_options(&mut options);
    }
    if let Some(choice) = start_at_login_override {
        options.start_at_login = 1;
        options.start_at_login_default = choice as u8;
    }
    // Component selection (v6): --components is the explicit full
    // selection, --without trims the default. The plan and the dry
    // run reflect the same resolution the install will use; an
    // invalid selection is a usage error, reported before anything
    // else happens. The answer file's selection fills in when the
    // flags are absent; its trims merge with the flag's.
    let components_flag = flag_list(args, &["--components"])
        .or_else(|| answers.as_ref().and_then(|a| a.components.clone()));
    let mut without_list = flag_list(args, &["--without"]).unwrap_or_default();
    if let Some(a) = &answers {
        for id in &a.without_components {
            if !without_list.contains(id) {
                without_list.push(id.clone());
            }
        }
    }
    let previous_receipt =
        seanpak::receipt::read_receipt(&dest).filter(|r| r.app == manifest.app_name());
    let previous_selection = previous_receipt
        .as_ref()
        .map(|r| (r.components.as_slice(), r.components_known.as_slice()));
    let primary_rel = records
        .get(manifest.primary_exe_index as usize)
        .map(|r| r.relative_path());
    let selection = match power.resolve_selection(
        components_flag.as_deref(),
        &without_list,
        previous_selection,
        &records,
        primary_rel.as_deref(),
    ) {
        Ok(selection) => selection,
        Err(e) => return fail(args, "install", EXIT_USAGE, &e),
    };
    let stream_len = std::fs::metadata(&exe).map(|m| m.len()).unwrap_or(0);
    let plan = install::plan_install(
        &manifest, &records, &options, &dest, &power, &selection, stream_len,
    );

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
                ("startAtLogin", JsonValue::Bool(plan.start_at_login)),
                (
                    "components",
                    JsonValue::Array(
                        plan.components
                            .iter()
                            .map(|c| {
                                jobj(vec![
                                    ("id", jstr(c.id.clone())),
                                    ("name", jstr(c.name.clone())),
                                    ("required", JsonValue::Bool(c.required)),
                                    ("selected", JsonValue::Bool(c.selected)),
                                    ("packedSize", jnum(c.packed_size)),
                                    ("fileCount", jnum(c.file_count as u64)),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "hooks",
                    JsonValue::Array(
                        plan.hooks
                            .iter()
                            .map(|(phase, command)| {
                                jobj(vec![
                                    ("phase", jstr(phase.clone())),
                                    ("command", jstr(command.clone())),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "registryWrites",
                    JsonValue::Array(
                        plan.registry_writes
                            .iter()
                            .map(|w| {
                                jobj(vec![
                                    ("key", jstr(w.key.clone())),
                                    ("name", jstr(w.name.clone())),
                                    ("type", jstr(w.kind.as_str())),
                                    ("value", jstr(w.value.clone())),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "envWrites",
                    JsonValue::Array(
                        plan.env_writes
                            .iter()
                            .map(|w| {
                                jobj(vec![
                                    ("name", jstr(w.name.clone())),
                                    ("value", jstr(w.value.clone())),
                                ])
                            })
                            .collect(),
                    ),
                ),
                (
                    "richShortcuts",
                    JsonValue::Array(
                        plan.rich_shortcuts
                            .iter()
                            .map(|s| jstr(s.clone()))
                            .collect(),
                    ),
                ),
                ("answersApplied", JsonValue::Bool(answers.is_some())),
                ("locationWritable", JsonValue::Bool(plan.location_writable)),
                (
                    "suggestedLocation",
                    plan.suggested_location
                        .clone()
                        .map(|p| jpath(&p))
                        .unwrap_or(JsonValue::Null),
                ),
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
            if plan.start_at_login {
                println!("  + start-at-login entry for the app");
            }
            for component in &plan.components {
                if component.selected {
                    println!(
                        "  + part \"{}\" ({} bytes packed, {} files)",
                        component.name, component.packed_size, component.file_count
                    );
                } else {
                    println!("  - part \"{}\" not selected", component.name);
                }
            }
            for (phase, command) in &plan.hooks {
                println!("  hook {phase}: {command} (listed only — a dry run never runs hooks)");
            }
            for write in &plan.registry_writes {
                println!(
                    "  + registry value HKCU\\{}{}",
                    write.key,
                    if write.name.is_empty() {
                        String::new()
                    } else {
                        format!("\\{}", write.name)
                    }
                );
            }
            for write in &plan.env_writes {
                println!("  + environment variable {}", write.name);
            }
            if !plan.location_writable {
                match &plan.suggested_location {
                    Some(fallback) => println!(
                        "  ! {} is not writable by your user account (it needs administrator access, and this installer runs per user). Per-user location: {}",
                        plan.dest.display(),
                        fallback.display()
                    ),
                    None => println!(
                        "  ! {} is not writable by your user account (it needs administrator access, and this installer runs per user).",
                        plan.dest.display()
                    ),
                }
            }
            println!("Nothing was changed.");
        }
        return EXIT_OK;
    }

    // Pre-flight writability check, before the operation runs: the
    // refusal carries the stable `location_not_writable` reason and
    // the suggested per-user folder in JSON. The operation re-checks
    // authoritatively before it changes anything.
    if let install::LocationVerdict::NotWritable { fallback, detail } =
        install::location_verdict(&dest, &plan.app_name, &options.install_location())
    {
        let message = install::location_not_writable_message(&dest, &fallback, &detail);
        diag::record(
            "install",
            vec![
                ("mode", jstr("real")),
                ("status", jstr("error")),
                ("reason", jstr(install::REASON_LOCATION_NOT_WRITABLE)),
                ("error", jstr(message.clone())),
            ],
        );
        if json_mode(args) {
            print_json(&jobj(vec![
                ("command", jstr("install")),
                ("status", jstr("error")),
                ("reason", jstr(install::REASON_LOCATION_NOT_WRITABLE)),
                ("error", jstr(message)),
                ("dest", jpath(&dest)),
                ("suggestedLocation", jpath(&fallback)),
                ("exitCode", JsonValue::Number(EXIT_INSTALL as f64)),
            ]));
            return EXIT_INSTALL;
        }
        return fail(args, "install", EXIT_INSTALL, &message);
    }

    let request = install::InstallRequest {
        // With an answer file, the preset's option choices drive the
        // real install (not just the plan above): hand the operation
        // the answers-applied options. Without one, the package's own
        // recorded options stay in charge, as before.
        options_override: if answers.is_some() {
            Some(options)
        } else {
            None
        },
        force: has_flag(args, &["--force"]),
        repair: has_flag(args, &["--repair"]),
        start_at_login: start_at_login_override,
        components: components_flag.clone(),
        without_components: without_list.clone(),
        purge_data_default: answers
            .as_ref()
            .map(|a| a.purge_data_on_uninstall)
            .unwrap_or(false),
    };
    match install::perform_install_with(&exe, &dest, &request, None, None) {
        Ok(outcome) => {
            // --launch is an explicit headless opt-in (headless installs
            // never launch by default); an answer file's
            // launchAfterInstall: true is the same explicit opt-in. A
            // launch failure is a warning in the report, never an
            // install failure.
            let launch_requested = has_flag(args, &["--launch"])
                || answers
                    .as_ref()
                    .and_then(|a| a.launch_after_install)
                    .unwrap_or(false);
            let mut launch_performed = false;
            let mut launch_error: Option<String> = None;
            if launch_requested {
                match records.get(manifest.primary_exe_index as usize) {
                    Some(record) => {
                        let target = outcome.dest.join(record.relative_path().replace('\\', "/"));
                        match install::launch_program(&target) {
                            Ok(()) => launch_performed = true,
                            Err(e) => launch_error = Some(e),
                        }
                    }
                    None => {
                        launch_error =
                            Some("the package records no primary executable".to_string());
                    }
                }
            }
            diag::record(
                "install",
                vec![
                    ("mode", jstr("real")),
                    ("app", jstr(plan.app_name.clone())),
                    ("dest", jpath(&outcome.dest)),
                    ("files", jnum(outcome.files_written as u64)),
                    ("arpWritten", JsonValue::Bool(outcome.arp_written)),
                    ("installMode", jstr(outcome.mode.clone())),
                    (
                        "startAtLoginApplied",
                        JsonValue::Bool(outcome.start_at_login_applied),
                    ),
                    ("launchRequested", JsonValue::Bool(launch_requested)),
                    ("launchPerformed", JsonValue::Bool(launch_performed)),
                ],
            );
            if json_mode(args) {
                let mut pairs: Vec<(&str, JsonValue)> = vec![
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
                    (
                        "startAtLoginApplied",
                        JsonValue::Bool(outcome.start_at_login_applied),
                    ),
                    ("launchRequested", JsonValue::Bool(launch_requested)),
                    ("launchPerformed", JsonValue::Bool(launch_performed)),
                    (
                        "componentsInstalled",
                        JsonValue::Array(
                            outcome
                                .components_installed
                                .iter()
                                .map(|c| jstr(c.clone()))
                                .collect(),
                        ),
                    ),
                    (
                        "hooks",
                        JsonValue::Array(
                            outcome
                                .hook_results
                                .iter()
                                .map(|h| {
                                    jobj(vec![
                                        ("phase", jstr(h.phase.clone())),
                                        ("command", jstr(h.command.clone())),
                                        (
                                            "args",
                                            JsonValue::Array(
                                                h.args.iter().map(|a| jstr(a.clone())).collect(),
                                            ),
                                        ),
                                        (
                                            "exitCode",
                                            h.exit_code
                                                .map(|n| JsonValue::Number(n as f64))
                                                .unwrap_or(JsonValue::Null),
                                        ),
                                        ("onFailure", jstr(h.on_failure.clone())),
                                        ("succeeded", JsonValue::Bool(h.succeeded)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "registryWritesApplied",
                        jnum(outcome.registry_writes_applied as u64),
                    ),
                    ("envWritesApplied", jnum(outcome.env_writes_applied as u64)),
                    (
                        "pins",
                        JsonValue::Array(
                            outcome
                                .pins
                                .iter()
                                .map(|p| {
                                    jobj(vec![
                                        ("name", jstr(p.name.clone())),
                                        ("outcome", jstr(p.outcome.clone())),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ];
                if let Some(e) = &launch_error {
                    pairs.push(("launchError", jstr(e.clone())));
                }
                print_json(&jobj(pairs));
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
                if outcome.start_at_login_applied {
                    println!("Start at login: on (per-user Run entry).");
                }
                if !outcome.components_installed.is_empty() {
                    println!(
                        "Parts installed: {}",
                        outcome.components_installed.join(", ")
                    );
                }
                if outcome.registry_writes_applied > 0 {
                    println!(
                        "Registry values written: {} (recorded in the receipt; uninstall restores what was there).",
                        outcome.registry_writes_applied
                    );
                }
                if outcome.env_writes_applied > 0 {
                    println!(
                        "Environment variables set: {} (recorded in the receipt; uninstall restores what was there).",
                        outcome.env_writes_applied
                    );
                }
                for pin in &outcome.pins {
                    if pin.outcome == "pinned" {
                        println!("Pinned \"{}\" to the taskbar.", pin.name);
                    } else {
                        println!(
                            "Note: \"{}\" could not be pinned to the taskbar (Windows did not allow it); the shortcut itself was created.",
                            pin.name
                        );
                    }
                }
                for hook in &outcome.hook_results {
                    if !hook.succeeded {
                        eprintln!(
                            "[WARN] The {} command \"{}\" failed ({}); the install continued because its failure policy is \"continue\". The failure is recorded in the receipt.",
                            hook.phase,
                            hook.command,
                            hook.exit_code
                                .map(|c| format!("exit code {c}"))
                                .unwrap_or_else(|| "could not start".to_string())
                        );
                    }
                }
                if launch_performed {
                    println!("Launched {}.", plan.app_name);
                }
                if let Some(e) = &launch_error {
                    eprintln!("[WARN] Install succeeded, but the program did not start: {e}");
                }
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

/// Uninstall mode (the ARP entry's uninstall string points here): the
/// shared uninstall operation (`seanpak::uninstall::perform_uninstall`)
/// removes the payload, the journaled system artifacts, the receipt,
/// and the Add/Remove Programs entry. Interactive runs (not silent,
/// not `--json`) first open the native confirmation window, whose
/// checkbox is the only interactive route to deleting the app's data
/// folders; silent runs delete data only with an explicit
/// `--purge-data`. The running `Uninstall.exe` copy itself cannot be
/// deleted while it runs; it is reported and left behind.
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

    // The confirmation window lists the data folders the install
    // receipt recorded; read them before anything is deleted. The
    // receipt also carries the data-purge checkbox's starting state
    // (v7: set at install time by an answer file's
    // purgeDataOnUninstall) — the user still decides in the window;
    // silent runs still require the explicit --purge-data flag.
    let receipt = seanpak::receipt::read_receipt(&dest);
    let recorded_data_dirs: Vec<PathBuf> = receipt
        .as_ref()
        .map(|r| r.data_dirs.iter().map(PathBuf::from).collect())
        .unwrap_or_default();
    let purge_default = receipt
        .as_ref()
        .map(|r| r.purge_data_default)
        .unwrap_or(false);

    let mut purge_data = has_flag(args, &["--purge-data"]);
    let interactive = !quiet_mode(args) && !json_mode(args);
    if interactive && !purge_data {
        match seanpak::wizard::confirm_uninstall(
            &manifest.app_name(),
            &recorded_data_dirs,
            purge_default,
        ) {
            Some(purge) => purge_data = purge,
            None => {
                println!("Uninstall cancelled; nothing was removed.");
                return EXIT_OK;
            }
        }
    }

    let outcome =
        seanpak::uninstall::perform_uninstall(&manifest.app_name(), &records, &dest, purge_data);
    let self_copy_note = exe.starts_with(&dest);
    if json_mode(args) {
        print_json(&jobj(vec![
            ("command", jstr("uninstall")),
            ("status", jstr("ok")),
            ("uninstalled", jstr(manifest.app_name())),
            (
                "removed",
                JsonValue::Array(
                    outcome
                        .removed_files
                        .iter()
                        .map(|r| jstr(r.clone()))
                        .collect(),
                ),
            ),
            ("artifactsRemoved", jnum(outcome.artifacts_removed as u64)),
            ("receiptRemoved", JsonValue::Bool(outcome.receipt_removed)),
            ("from", jpath(&dest)),
            ("selfCopyLeftBehind", JsonValue::Bool(self_copy_note)),
            ("dataPurged", JsonValue::Bool(purge_data)),
            (
                "dataDeleted",
                JsonValue::Array(
                    outcome
                        .data_deleted
                        .iter()
                        .map(|d| jstr(d.clone()))
                        .collect(),
                ),
            ),
            (
                "dataDeleteFailures",
                JsonValue::Array(
                    outcome
                        .data_delete_failures
                        .iter()
                        .map(|d| jstr(d.clone()))
                        .collect(),
                ),
            ),
        ]));
    } else if !quiet_mode(args) {
        println!(
            "Uninstalled {}: removed {} file(s) from {}",
            manifest.app_name(),
            outcome.removed_files.len(),
            dest.display()
        );
        for rel in &outcome.removed_files {
            println!("  {rel}");
        }
        for deleted in &outcome.data_deleted {
            println!("  deleted data folder {deleted}");
        }
        for failure in &outcome.data_delete_failures {
            eprintln!("[WARN] Could not delete data folder {failure}");
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

/// Modify mode (v7): change an existing install in place through
/// the shared modify operation. Destination resolution mirrors
/// uninstall: `--to`, else the package's recorded location, else the
/// Add/Remove Programs entry. With no option/component flags and an
/// interactive console, main() routes to the wizard's Modify mode
/// instead; anything flag-bearing (or `--json` / `--silent`) lands
/// here, fully headless. `--dry-run` prints the plan and changes
/// nothing.
fn run_modify(args: &[String]) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            return fail(
                args,
                "modify",
                EXIT_IO,
                &format!("Cannot locate own executable: {e}"),
            )
        }
    };
    let (manifest, _records, options, _power) = match install::read_own_package_full(&exe) {
        Ok(pkg) => pkg,
        Err(e) => return fail(args, "modify", EXIT_BAD_MANIFEST, &e),
    };
    let app_name = manifest.app_name();
    let dest = modify_dest(args, &options, &app_name);

    // Option deltas from the flags, layered over the install's
    // current state: the override the operation expects is the full
    // new option set, which is the receipt's recorded state (via the
    // operation's own reconstruction) with the flagged fields flipped.
    let mut options_override: Option<InstallOptions> = None;
    let mut edited = match seanpak::receipt::read_receipt(&dest) {
        Some(receipt) => seanpak::modify::options_from_receipt(&options, &receipt),
        None => options,
    };
    let mut any_option_flag = false;
    let bool_flag = |args: &[String], on: &str, off: &str| -> Option<bool> {
        if has_flag(args, &[on]) {
            Some(true)
        } else if has_flag(args, &[off]) {
            Some(false)
        } else {
            None
        }
    };
    if let Some(on) = bool_flag(args, "--desktop-shortcut", "--no-desktop-shortcut") {
        edited.desktop_shortcut = on as u8;
        any_option_flag = true;
    }
    if let Some(on) = bool_flag(args, "--start-menu-shortcut", "--no-start-menu-shortcut") {
        edited.start_menu_shortcut = on as u8;
        any_option_flag = true;
    }
    if let Some(on) = bool_flag(args, "--path-entry", "--no-path-entry") {
        edited.path_entry = on as u8;
        any_option_flag = true;
    }
    if let Some(exts) = flag_value(args, &["--file-associations"]) {
        seanpak::overlay::copy_str_to_fixed_pub(&mut edited.file_associations_utf8, &exts);
        any_option_flag = true;
    }
    if any_option_flag {
        options_override = Some(edited);
    }
    let start_at_login = if has_flag(args, &["--start-at-login"]) {
        Some(true)
    } else if has_flag(args, &["--no-start-at-login"]) {
        Some(false)
    } else {
        None
    };
    let request = seanpak::modify::ModifyRequest {
        options_override,
        start_at_login,
        components: flag_list(args, &["--components"]),
        without_components: flag_list(args, &["--without"]).unwrap_or_default(),
    };

    if has_flag(args, &["--dry-run"]) {
        return match seanpak::modify::plan_modify(&exe, &dest, &request) {
            Ok(plan) => {
                if json_mode(args) {
                    print_json(&jobj(vec![
                        ("command", jstr("modify")),
                        ("status", jstr("dry-run")),
                        ("app", jstr(plan.app_name.clone())),
                        ("version", jstr(plan.package_version.clone())),
                        ("installedVersion", jstr(plan.installed_version.clone())),
                        ("dest", jpath(&dest)),
                        (
                            "componentsAdded",
                            JsonValue::Array(
                                plan.components_added
                                    .iter()
                                    .map(|c| jstr(c.clone()))
                                    .collect(),
                            ),
                        ),
                        (
                            "componentsRemoved",
                            JsonValue::Array(
                                plan.components_removed
                                    .iter()
                                    .map(|c| jstr(c.clone()))
                                    .collect(),
                            ),
                        ),
                        (
                            "componentsKept",
                            JsonValue::Array(
                                plan.components_kept
                                    .iter()
                                    .map(|c| jstr(c.clone()))
                                    .collect(),
                            ),
                        ),
                        (
                            "filesAdded",
                            JsonValue::Array(
                                plan.files_added
                                    .iter()
                                    .map(|(rel, size)| {
                                        jobj(vec![
                                            ("path", jstr(rel.clone())),
                                            ("size", jnum(*size)),
                                        ])
                                    })
                                    .collect(),
                            ),
                        ),
                        (
                            "filesRemoved",
                            JsonValue::Array(
                                plan.files_removed.iter().map(|r| jstr(r.clone())).collect(),
                            ),
                        ),
                        ("optionsBefore", modify_options_json(&plan.options_before)),
                        ("optionsAfter", modify_options_json(&plan.options_after)),
                        ("locationWritable", JsonValue::Bool(plan.location_writable)),
                        (
                            "suggestedLocation",
                            plan.suggested_location
                                .clone()
                                .map(|p| jpath(&p))
                                .unwrap_or(JsonValue::Null),
                        ),
                        ("changes", JsonValue::Bool(false)),
                    ]));
                } else {
                    println!(
                        "Dry run: would modify {} {} (installed: {}) at {}",
                        plan.app_name,
                        plan.package_version,
                        plan.installed_version,
                        dest.display()
                    );
                    for id in &plan.components_added {
                        println!("  + part \"{id}\"");
                    }
                    for id in &plan.components_removed {
                        println!("  - part \"{id}\"");
                    }
                    for (rel, size) in &plan.files_added {
                        println!("  + {rel} ({size} bytes)");
                    }
                    for rel in &plan.files_removed {
                        println!("  - {rel}");
                    }
                    println!("Nothing was changed.");
                }
                EXIT_OK
            }
            Err(e) => fail(args, "modify", EXIT_INSTALL, &e),
        };
    }

    match seanpak::modify::perform_modify(&exe, &dest, &request, None, None) {
        Ok(outcome) => {
            diag::record(
                "modify",
                vec![
                    ("app", jstr(app_name.clone())),
                    ("dest", jpath(&dest)),
                    ("filesAdded", jnum(outcome.files_added as u64)),
                    ("filesRemoved", jnum(outcome.files_removed as u64)),
                ],
            );
            if json_mode(args) {
                print_json(&jobj(vec![
                    ("command", jstr("modify")),
                    ("status", jstr("ok")),
                    ("app", jstr(app_name.clone())),
                    ("dest", jpath(&dest)),
                    ("filesAdded", jnum(outcome.files_added as u64)),
                    ("filesRemoved", jnum(outcome.files_removed as u64)),
                    (
                        "componentsBefore",
                        JsonValue::Array(
                            outcome
                                .components_before
                                .iter()
                                .map(|c| jstr(c.clone()))
                                .collect(),
                        ),
                    ),
                    (
                        "componentsAfter",
                        JsonValue::Array(
                            outcome
                                .components_after
                                .iter()
                                .map(|c| jstr(c.clone()))
                                .collect(),
                        ),
                    ),
                    ("receipt", jpath(&outcome.receipt_path)),
                    ("shortcutsCreated", jnum(outcome.shortcuts_created as u64)),
                    (
                        "associationsApplied",
                        jnum(outcome.associations_applied as u64),
                    ),
                    ("pathEntryAdded", JsonValue::Bool(outcome.path_entry_added)),
                    (
                        "startAtLoginApplied",
                        JsonValue::Bool(outcome.start_at_login_applied),
                    ),
                ]));
            } else if !quiet_mode(args) {
                println!(
                    "Modified {} at {}: {} file(s) added, {} removed.",
                    app_name,
                    dest.display(),
                    outcome.files_added,
                    outcome.files_removed
                );
                println!(
                    "Parts now installed: {}",
                    outcome.components_after.join(", ")
                );
                println!("Receipt: {}", outcome.receipt_path.display());
            }
            EXIT_OK
        }
        Err(e) => {
            diag::record(
                "modify",
                vec![("status", jstr("error")), ("error", jstr(e.clone()))],
            );
            fail(args, "modify", EXIT_INSTALL, &e)
        }
    }
}

/// Where a modify acts: `--to`, else the install whose receipt sits
/// at the package's recorded location, else the Add/Remove Programs
/// entry's location, else the recorded location (where the operation
/// will report the plain "nothing to modify" refusal).
fn modify_dest(args: &[String], options: &InstallOptions, app_name: &str) -> PathBuf {
    if let Some(dir) = flag_value(args, &["--to"]) {
        return PathBuf::from(dir);
    }
    let recorded = seanpak::wizard::default_install_dir(options, app_name);
    if seanpak::receipt::read_receipt(&recorded)
        .map(|r| r.app == app_name)
        .unwrap_or(false)
    {
        return recorded;
    }
    match seanpak::sysinstall::arp_install(app_name) {
        Some((_, location)) if !location.as_os_str().is_empty() => location,
        _ => recorded,
    }
}

/// Bare interactive `--modify`: the wizard in Modify mode. The
/// install must exist (receipt at the resolved destination) — the
/// refusal is reported here, in plain words, before any window.
fn run_wizard_modify(
    manifest: OverlayManifestHeader,
    records: Vec<FileRecordHeader>,
    options: InstallOptions,
    power: seanpak::overlay::PowerSpec,
) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[ERROR] Cannot locate own executable: {e}");
            return 1;
        }
    };
    let app_name = manifest.app_name();
    let no_args: Vec<String> = Vec::new();
    let dest = modify_dest(&no_args, &options, &app_name);
    let found = seanpak::receipt::read_receipt(&dest)
        .map(|r| r.app == app_name)
        .unwrap_or(false);
    if !found {
        let message = format!(
            "no SeanPak install of {app_name} found at {} (no install receipt there) — nothing to modify",
            dest.display()
        );
        diag::record(
            "modify",
            vec![("status", jstr("error")), ("error", jstr(message.clone()))],
        );
        eprintln!("[ERROR] {message}");
        return EXIT_INSTALL;
    }
    let mut wizard = InstallWizard::modify(manifest, records, options, exe, power, dest);
    match wizard.run() {
        Ok(()) => 0,
        Err(err_code) => {
            let message = format!(
                "SeanPak could not start the Modify window (Windows error {err_code}).\n\nA diagnostic log was written to:\n{}",
                diag::log_path().display()
            );
            diag::log("fatal", &message.replace('\n', " "));
            diag::error_box("SeanPak Modify failed to start", &message);
            err_code as i32
        }
    }
}

/// The before/after option summary as JSON (modify plan output).
fn modify_options_json(summary: &seanpak::modify::ModifyOptionSummary) -> JsonValue {
    jobj(vec![
        ("desktopShortcut", JsonValue::Bool(summary.desktop_shortcut)),
        (
            "startMenuShortcut",
            JsonValue::Bool(summary.start_menu_shortcut),
        ),
        ("startMenuFolder", jstr(summary.start_menu_folder.clone())),
        (
            "fileAssociations",
            JsonValue::Array(
                summary
                    .file_associations
                    .iter()
                    .map(|e| jstr(e.clone()))
                    .collect(),
            ),
        ),
        ("pathEntry", JsonValue::Bool(summary.path_entry)),
        ("startAtLogin", JsonValue::Bool(summary.start_at_login)),
    ])
}

/// True when a `--modify` invocation carries any option/component
/// flag (or a machine mode): those run headlessly; a bare interactive
/// `--modify` opens the wizard's Modify mode instead.
fn modify_is_headless(args: &[String]) -> bool {
    if json_mode(args) || quiet_mode(args) || has_flag(args, &["--dry-run", "--to"]) {
        return true;
    }
    const OPTION_FLAGS: [&str; 11] = [
        "--components",
        "--without",
        "--start-at-login",
        "--no-start-at-login",
        "--desktop-shortcut",
        "--no-desktop-shortcut",
        "--start-menu-shortcut",
        "--no-start-menu-shortcut",
        "--path-entry",
        "--no-path-entry",
        "--file-associations",
    ];
    args.iter().any(|a| OPTION_FLAGS.contains(&a.as_str()))
}

/// `seanpak inspect <Setup.exe>`: the full package report.
fn run_inspect(args: &[String]) -> i32 {
    let path = match args.get(1) {
        Some(p) if !p.starts_with('-') => PathBuf::from(p),
        _ => {
            return fail(
                args,
                "inspect",
                EXIT_USAGE,
                "usage: seanpak inspect <Setup.exe> [--json]",
            )
        }
    };
    match seanpak::inspect::inspect_package(&path) {
        Ok(report) => {
            if json_mode(args) {
                print_json(&seanpak::inspect::inspect_json(&report));
            } else {
                println!(
                    "{} {} — {}",
                    report.app_name, report.version, report.publisher
                );
                println!("  package format v{}", report.manifest_version);
                println!(
                    "  options: location \"{}\", start menu folder \"{}\"",
                    report.options.install_location(),
                    report.options.start_menu_folder()
                );
                println!(
                    "    desktop shortcut {}, start menu {}, PATH entry {}, associations \"{}\"",
                    if report.options.wants_desktop_shortcut() {
                        "yes"
                    } else {
                        "no"
                    },
                    if report.options.wants_start_menu_shortcut() {
                        "yes"
                    } else {
                        "no"
                    },
                    if report.options.wants_path_entry() {
                        "yes"
                    } else {
                        "no"
                    },
                    report.options.file_associations()
                );
                for component in &report.power.components {
                    println!(
                        "  part \"{}\" — {} bytes packed, {} files{}",
                        component.id,
                        component.packed_size,
                        component.files.len(),
                        if component.default_selected {
                            ", on by default"
                        } else {
                            ""
                        }
                    );
                }
                for hook in report
                    .power
                    .pre_hooks
                    .iter()
                    .chain(&report.power.post_hooks)
                {
                    println!("  hook: {}", seanpak::hooks::display_command(hook));
                }
                for write in &report.power.registry_writes {
                    println!("  registry write: HKCU\\{}\\{}", write.key, write.name);
                }
                for write in &report.power.env_writes {
                    println!("  environment write: {}", write.name);
                }
                for shortcut in &report.power.shortcuts {
                    println!("  shortcut \"{}\" -> {}", shortcut.name, shortcut.target);
                }
                println!(
                    "  files: {} ({} bytes, {} stored)",
                    report.files.len(),
                    report.total_bytes,
                    report.total_stored_bytes
                );
                for f in &report.files {
                    println!(
                        "    {} ({} bytes, {} stored, {}, part {}){}",
                        f.path,
                        f.size,
                        f.stored_size,
                        seanpak::inspect::codec_label(f.compression),
                        f.component,
                        if f.is_primary { " [primary]" } else { "" }
                    );
                }
            }
            EXIT_OK
        }
        Err(e) => fail(args, "inspect", EXIT_BAD_MANIFEST, &e),
    }
}

/// `seanpak diff <A> <B>`: exit 0 identical, 6 differences, 3
/// unreadable.
fn run_diff(args: &[String]) -> i32 {
    let positional: Vec<&String> = args
        .iter()
        .skip(1)
        .filter(|a| !a.starts_with('-'))
        .collect();
    if positional.len() != 2 {
        return fail(
            args,
            "diff",
            EXIT_USAGE,
            "usage: seanpak diff <A.exe> <B.exe> [--json]",
        );
    }
    let (a, b) = (Path::new(positional[0]), Path::new(positional[1]));
    match seanpak::inspect::diff_packages(a, b) {
        Ok(report) => {
            let identical = report.identical();
            if json_mode(args) {
                print_json(&seanpak::inspect::diff_json(&report, identical));
            } else if identical {
                println!("Packages are identical.");
            } else {
                println!("Packages differ:");
                for (field, va, vb) in &report.identity_deltas {
                    println!("  {field}: {va} -> {vb}");
                }
                for (field, va, vb) in &report.option_deltas {
                    println!("  option {field}: {va} -> {vb}");
                }
                for delta in &report.power_deltas {
                    println!("  {delta}");
                }
                for path in &report.files_added {
                    println!("  + {path}");
                }
                for path in &report.files_removed {
                    println!("  - {path}");
                }
                for path in &report.files_changed {
                    println!("  ~ {path} (content changed)");
                }
            }
            if identical {
                EXIT_OK
            } else {
                EXIT_DIFFERENT
            }
        }
        Err(e) => fail(args, "diff", EXIT_BAD_MANIFEST, &e),
    }
}

/// `seanpak init <dir>`: scaffold a starter manifest + PACKAGING.md.
fn run_init(args: &[String]) -> i32 {
    let dir = match args.get(1) {
        Some(p) if !p.starts_with('-') => PathBuf::from(p),
        _ => {
            return fail(
                args,
                "init",
                EXIT_USAGE,
                "usage: seanpak init <dir> [--name <App>] [--force] [--json]",
            )
        }
    };
    let name = flag_value(args, &["--name"]);
    let force = has_flag(args, &["--force"]);
    match seanpak::init::init_project(&dir, name.as_deref(), force) {
        Ok(outcome) => {
            diag::record(
                "init",
                vec![
                    ("dir", jpath(&dir)),
                    ("app", jstr(outcome.app_name.clone())),
                    ("manifest", jpath(&outcome.manifest_path)),
                ],
            );
            if json_mode(args) {
                print_json(&jobj(vec![
                    ("command", jstr("init")),
                    ("status", jstr("ok")),
                    ("app", jstr(outcome.app_name.clone())),
                    ("version", jstr(outcome.version.clone())),
                    ("manifest", jpath(&outcome.manifest_path)),
                    (
                        "packagingDoc",
                        outcome
                            .packaging_path
                            .clone()
                            .map(|p| jpath(&p))
                            .unwrap_or(JsonValue::Null),
                    ),
                    ("filesDir", jstr(outcome.files_dir.clone())),
                    (
                        "primaryExe",
                        outcome
                            .primary_exe
                            .clone()
                            .map(jstr)
                            .unwrap_or(JsonValue::Null),
                    ),
                    ("packCommand", jstr(outcome.pack_command.clone())),
                ]));
            } else if !quiet_mode(args) {
                println!(
                    "Initialized {} {} in {}",
                    outcome.app_name,
                    outcome.version,
                    dir.display()
                );
                println!("  manifest: {}", outcome.manifest_path.display());
                match &outcome.packaging_path {
                    Some(p) => println!("  guide:    {}", p.display()),
                    None => println!("  guide:    PACKAGING.md already existed, left as-is"),
                }
                match &outcome.primary_exe {
                    Some(p) => println!("  primary executable detected: {p}"),
                    None => println!(
                        "  no executable found in the payload folder yet — set \"primaryExe\" in the manifest once the app is built"
                    ),
                }
                println!("  pack with: {}", outcome.pack_command);
            }
            EXIT_OK
        }
        Err(e) => {
            // Refusing to overwrite an existing manifest is the
            // pack RefuseOverwrite shape (exit 4); a missing folder
            // is a usage error.
            if e.contains("already exists") {
                fail(args, "init", EXIT_IO, &e)
            } else if e.contains("is not a folder") {
                fail(args, "init", EXIT_USAGE, &e)
            } else {
                fail(args, "init", EXIT_IO, &e)
            }
        }
    }
}

/// `seanpak answers-template <Setup.exe>`: print the answer-file
/// template for a package, defaults filled.
fn run_answers_template(args: &[String]) -> i32 {
    let path = match args.get(1) {
        Some(p) if !p.starts_with('-') => PathBuf::from(p),
        _ => {
            return fail(
                args,
                "answers-template",
                EXIT_USAGE,
                "usage: seanpak answers-template <Setup.exe> [--json]",
            )
        }
    };
    let file = match File::open(&path) {
        Ok(f) => f,
        Err(e) => {
            return fail(
                args,
                "answers-template",
                EXIT_IO,
                &format!("Cannot open {}: {e}", path.display()),
            )
        }
    };
    match OverlayReader::read_package_full(file) {
        Ok(Some((manifest, records, options, power))) => {
            let primary_rel = records
                .get(manifest.primary_exe_index as usize)
                .map(|r| r.relative_path());
            let template = seanpak::answers::answers_template(
                &options,
                &power,
                &records,
                primary_rel.as_deref(),
            );
            // JSON mode prints the bare template (so it can be piped
            // straight into a file); the plain run labels it.
            if json_mode(args) {
                print_json(&template);
            } else {
                println!(
                    "Answer-file template for {} — save as answers.json and edit:",
                    path.display()
                );
                println!("{}", seanpak::studio::render_pretty(&template));
            }
            EXIT_OK
        }
        Ok(None) => fail(
            args,
            "answers-template",
            EXIT_BAD_MANIFEST,
            &format!("No SeanPak overlay found in {}", path.display()),
        ),
        Err(e) => fail(
            args,
            "answers-template",
            EXIT_IO,
            &format!("Failed to read {}: {e}", path.display()),
        ),
    }
}

/// Bare-run mode for a built installer: opens the Design E (Rail)
/// wizard for this executable's own overlay package.
fn run_wizard(
    manifest: OverlayManifestHeader,
    records: Vec<FileRecordHeader>,
    options: InstallOptions,
    power: seanpak::overlay::PowerSpec,
) -> i32 {
    let exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[ERROR] Cannot locate own executable: {e}");
            return 1;
        }
    };
    let mut wizard = InstallWizard::new(manifest, records, options, exe, power);
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
                for component in &outcome.components {
                    println!(
                        "  part {} \"{}\" — {} bytes packed, {} files",
                        component.id, component.name, component.packed_size, component.file_count
                    );
                }
            }
            EXIT_OK
        }
        Err(PackError::BadFiles(m)) => fail(args, "pack", EXIT_BAD_MANIFEST, &m),
        Err(PackError::RefuseOverwrite(m)) => fail(args, "pack", EXIT_IO, &m),
        // The CLI never passes a cancel flag, so this cannot occur;
        // the arm exists for exhaustiveness.
        Err(PackError::Cancelled) => fail(args, "pack", EXIT_IO, "Packaging cancelled"),
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
            "components",
            JsonValue::Array(
                outcome
                    .components
                    .iter()
                    .map(|c| {
                        jobj(vec![
                            ("id", jstr(c.id.clone())),
                            ("name", jstr(c.name.clone())),
                            ("required", JsonValue::Bool(c.required)),
                            ("packedSize", jnum(c.packed_size)),
                            ("fileCount", jnum(c.file_count as u64)),
                        ])
                    })
                    .collect(),
            ),
        ),
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

    // Word subcommands (the AI-control surface): schema / validate /
    // pack / verify / init / inspect / diff / answers-template.
    match args.first().map(|a| a.as_str()) {
        Some("schema") => std::process::exit(run_schema(&args)),
        Some("validate") => std::process::exit(run_validate(&args)),
        Some("pack") => std::process::exit(run_pack(&args)),
        Some("verify") => std::process::exit(run_verify(&args)),
        Some("init") => std::process::exit(run_init(&args)),
        Some("inspect") => std::process::exit(run_inspect(&args)),
        Some("diff") => std::process::exit(run_diff(&args)),
        Some("answers-template") => std::process::exit(run_answers_template(&args)),
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

    // --modify (v7): the ARP Modify button's target, and the CLI's
    // headless modify. A bare interactive --modify (no option flags,
    // no machine modes) opens the wizard in Modify mode; anything
    // else runs the headless operation.
    if has_flag(&args, &["--modify"]) {
        if modify_is_headless(&args) {
            std::process::exit(run_modify(&args));
        }
        match own_overlay_package_full() {
            Some((manifest, records, options, power)) => {
                std::process::exit(run_wizard_modify(manifest, records, options, power));
            }
            None => std::process::exit(fail(
                &args,
                "modify",
                EXIT_BAD_MANIFEST,
                "This executable carries no SeanPak overlay; nothing to modify",
            )),
        }
    }

    // A built native installer carries an overlay in its own executable:
    // --install extracts the payload headlessly, and running it with no
    // arguments at all opens the installer wizard (Design E, Rail)
    // instead of the packager.
    if has_flag(&args, &["--install", "--extract-self"]) {
        std::process::exit(run_install(&args));
    }
    if args.is_empty() {
        if let Some((manifest, records, options, power)) = own_overlay_package_full() {
            diag::log(
                "mode",
                &format!("installer wizard (own overlay: {})", manifest.app_name()),
            );
            std::process::exit(run_wizard(manifest, records, options, power));
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

    let mut app = BuilderStudio::new(initial_folder);
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
