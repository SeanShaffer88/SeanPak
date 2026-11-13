//! The install receipt — `seanpak-receipt.json` in the install dir.
//!
//! Written at the end of every successful install. It is both the user's
//! record (what was installed, where, when, with which options) and the
//! machine journal the rest of SeanPak works from:
//!
//! - upgrade/repair detection reads the recorded version,
//! - uninstall removes exactly the artifacts recorded here (shortcut
//!   files, file associations with their prior values, the PATH segment,
//!   the start-at-login Run entry) and offers to delete the recorded
//!   data directories,
//! - `verify --sandbox` compares installed files against the recorded
//!   payload hashes.
//!
//! Rendered and parsed with the crate's hand-rolled JSON support — no
//! new dependencies.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use crate::manifest::{jobj, jstr, parse_json, JsonValue};

pub const RECEIPT_FILE_NAME: &str = "seanpak-receipt.json";

#[derive(Debug, Clone, PartialEq)]
pub struct ReceiptFile {
    pub path: String,
    pub size: u64,
    pub sha256: String,
}

/// One file association an install applied, with the state it replaced —
/// everything uninstall needs to put things back exactly.
#[derive(Debug, Clone, PartialEq)]
pub struct AssocRecord {
    /// Extension without the dot, lowercase (for example `txt`).
    pub extension: String,
    /// ProgId the extension was pointed at (`SeanPak.<App>`).
    pub prog_id: String,
    /// The extension key's default value before install, if any.
    pub prior_default: Option<String>,
    /// True when install created the extension key itself.
    pub created_ext_key: bool,
    /// True when install created the ProgId key.
    pub created_prog_id: bool,
}

/// The HKCU Run ("start at login") entry an install applied, with the
/// state it replaced — uninstall removes the value only while it still
/// holds `command`, then restores `prior_value` if there was one.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRecord {
    /// The Run value's name (the app name).
    pub name: String,
    /// The command written (the quoted primary-exe path).
    pub command: String,
    /// The value that was there before install, if any.
    pub prior_value: Option<String>,
}

/// One registry value an install wrote (v6 `registryWrites`), with the
/// state it replaced — uninstall restores the prior value exactly, or
/// deletes the value (and the key, when the install created it and it
/// is now empty) when there was none.
#[derive(Debug, Clone, PartialEq)]
pub struct RegWriteRecord {
    /// HKCU-relative key path.
    pub key: String,
    /// Value name ("" = the key's default value).
    pub name: String,
    /// The prior value's kind: "sz" (REG_SZ), "expand"
    /// (REG_EXPAND_SZ), or "dword" — `None` when the value was absent.
    pub prior_kind: Option<String>,
    /// The prior value in string form (decimal for a dword).
    pub prior_value: Option<String>,
    /// True when the install created the key itself.
    pub created_key: bool,
}

/// One environment variable an install wrote (v6 `envWrites`), with
/// the state it replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvWriteRecord {
    pub name: String,
    /// The prior value — `None` when the variable was absent.
    pub prior_value: Option<String>,
    /// The prior value's registry type (REG_SZ / REG_EXPAND_SZ), so a
    /// restore writes back the same type. 0 when there was no prior.
    pub prior_type: u32,
}

/// The recorded outcome of a best-effort taskbar pin (v6 rich
/// shortcuts). Informational only — a pin is never something an
/// uninstall can (or should) undo.
#[derive(Debug, Clone, PartialEq)]
pub struct PinRecord {
    /// The shortcut's display name.
    pub name: String,
    /// The link file that was pinned.
    pub link_path: String,
    /// "pinned" or "unavailable".
    pub outcome: String,
}

/// System artifacts an install applied (all paths are the exact files /
/// values created, so removal touches nothing else).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AppliedArtifacts {
    pub shortcuts: Vec<String>,
    pub associations: Vec<AssocRecord>,
    pub path_segment_added: Option<String>,
    pub start_at_login: Option<RunRecord>,
    /// Journaled registry writes (v6), restored by uninstall.
    pub registry_writes: Vec<RegWriteRecord>,
    /// Journaled environment writes (v6), restored by uninstall.
    pub env_writes: Vec<EnvWriteRecord>,
    /// Taskbar pin outcomes (v6) — informational, nothing to remove.
    pub pins: Vec<PinRecord>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InstallReceipt {
    pub app: String,
    pub version: String,
    pub publisher: String,
    pub install_path: String,
    pub installed_at_unix: u64,
    /// `install`, `upgrade`, `repair`, or `reinstall`.
    pub mode: String,
    pub previous_version: Option<String>,
    pub files: Vec<ReceiptFile>,
    pub desktop_shortcut: bool,
    pub start_menu_shortcut: bool,
    pub start_menu_folder: String,
    pub path_entry: bool,
    pub file_associations: Vec<String>,
    /// The package offered "Launch <app>" on the Finish page…
    pub launch_after_install_offered: bool,
    /// …and the checkbox started checked.
    pub launch_after_install_default: bool,
    /// The install applied the start-at-login choice (the Run entry
    /// itself, when applied, is journaled in `artifacts`).
    pub start_at_login: bool,
    /// The app's data directories, resolved to absolute paths at
    /// install time. The uninstall's data-purge option works from
    /// exactly this list.
    pub data_dirs: Vec<String>,
    /// The install-time preset's data-purge default (v7, from an
    /// answer file's `purgeDataOnUninstall`): the interactive
    /// uninstall confirmation's checkbox starts in this state. It is
    /// a default only — a silent uninstall still never purges without
    /// its explicit `--purge-data` flag.
    pub purge_data_default: bool,
    /// Component ids this install actually installed (v6), in part
    /// order — `core` included when the package has unclaimed files.
    /// Uninstall removes exactly these components' payload files, and
    /// an upgrade uses this (with `components_known`) as the default
    /// selection for the new package.
    pub components: Vec<String>,
    /// Every component id the package offered at install time (v6).
    pub components_known: Vec<String>,
    /// Every install hook that ran (v6): phase, command, exit code,
    /// failure policy, outcome.
    pub hooks: Vec<HookRunRecord>,
    pub artifacts: AppliedArtifacts,
}

/// One install hook run (v6): what ran, how it ended, and the failure
/// policy that decided what the failure meant.
#[derive(Debug, Clone, PartialEq)]
pub struct HookRunRecord {
    /// "preInstall" or "postInstall".
    pub phase: String,
    pub command: String,
    pub args: Vec<String>,
    /// The process exit code; `None` when the process could not be
    /// started or was killed by a signal.
    pub exit_code: Option<i64>,
    /// "abort" or "continue".
    pub on_failure: String,
    pub succeeded: bool,
}

pub fn receipt_path(dest: &Path) -> PathBuf {
    dest.join(RECEIPT_FILE_NAME)
}

impl InstallReceipt {
    pub fn now_unix() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    pub fn to_json_value(&self) -> JsonValue {
        let files = JsonValue::Array(
            self.files
                .iter()
                .map(|f| {
                    jobj(vec![
                        ("path", jstr(f.path.clone())),
                        ("size", JsonValue::Number(f.size as f64)),
                        ("sha256", jstr(f.sha256.clone())),
                    ])
                })
                .collect(),
        );
        let associations = JsonValue::Array(
            self.artifacts
                .associations
                .iter()
                .map(|a| {
                    jobj(vec![
                        ("extension", jstr(a.extension.clone())),
                        ("progId", jstr(a.prog_id.clone())),
                        (
                            "priorDefault",
                            a.prior_default.clone().map(jstr).unwrap_or(JsonValue::Null),
                        ),
                        ("createdExtKey", JsonValue::Bool(a.created_ext_key)),
                        ("createdProgId", JsonValue::Bool(a.created_prog_id)),
                    ])
                })
                .collect(),
        );
        let mut pairs: Vec<(&str, JsonValue)> = vec![
            ("receiptVersion", JsonValue::Number(1.0)),
            ("app", jstr(self.app.clone())),
            ("version", jstr(self.version.clone())),
            ("publisher", jstr(self.publisher.clone())),
            ("installPath", jstr(self.install_path.clone())),
            (
                "installedAt",
                jstr(crate::diag::format_utc(self.installed_at_unix)),
            ),
            (
                "installedAtUnix",
                JsonValue::Number(self.installed_at_unix as f64),
            ),
            ("mode", jstr(self.mode.clone())),
            ("files", files),
            (
                "options",
                jobj(vec![
                    ("desktopShortcut", JsonValue::Bool(self.desktop_shortcut)),
                    (
                        "startMenuShortcut",
                        JsonValue::Bool(self.start_menu_shortcut),
                    ),
                    ("startMenuFolder", jstr(self.start_menu_folder.clone())),
                    ("pathEntry", JsonValue::Bool(self.path_entry)),
                    (
                        "fileAssociations",
                        JsonValue::Array(
                            self.file_associations
                                .iter()
                                .map(|e| jstr(e.clone()))
                                .collect(),
                        ),
                    ),
                    (
                        "launchAfterInstallOffered",
                        JsonValue::Bool(self.launch_after_install_offered),
                    ),
                    (
                        "launchAfterInstallDefault",
                        JsonValue::Bool(self.launch_after_install_default),
                    ),
                    ("startAtLogin", JsonValue::Bool(self.start_at_login)),
                    ("purgeDataDefault", JsonValue::Bool(self.purge_data_default)),
                ]),
            ),
            (
                "dataDirs",
                JsonValue::Array(self.data_dirs.iter().map(|d| jstr(d.clone())).collect()),
            ),
            (
                "components",
                JsonValue::Array(self.components.iter().map(|c| jstr(c.clone())).collect()),
            ),
            (
                "componentsKnown",
                JsonValue::Array(
                    self.components_known
                        .iter()
                        .map(|c| jstr(c.clone()))
                        .collect(),
                ),
            ),
            (
                "hooks",
                JsonValue::Array(
                    self.hooks
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
                "artifacts",
                jobj(vec![
                    (
                        "shortcuts",
                        JsonValue::Array(
                            self.artifacts
                                .shortcuts
                                .iter()
                                .map(|s| jstr(s.clone()))
                                .collect(),
                        ),
                    ),
                    ("associations", associations),
                    (
                        "pathSegmentAdded",
                        self.artifacts
                            .path_segment_added
                            .clone()
                            .map(jstr)
                            .unwrap_or(JsonValue::Null),
                    ),
                    (
                        "startAtLogin",
                        match &self.artifacts.start_at_login {
                            Some(run) => jobj(vec![
                                ("name", jstr(run.name.clone())),
                                ("command", jstr(run.command.clone())),
                                (
                                    "priorValue",
                                    run.prior_value.clone().map(jstr).unwrap_or(JsonValue::Null),
                                ),
                            ]),
                            None => JsonValue::Null,
                        },
                    ),
                    (
                        "registryWrites",
                        JsonValue::Array(
                            self.artifacts
                                .registry_writes
                                .iter()
                                .map(|w| {
                                    jobj(vec![
                                        ("key", jstr(w.key.clone())),
                                        ("name", jstr(w.name.clone())),
                                        (
                                            "priorKind",
                                            w.prior_kind
                                                .clone()
                                                .map(jstr)
                                                .unwrap_or(JsonValue::Null),
                                        ),
                                        (
                                            "priorValue",
                                            w.prior_value
                                                .clone()
                                                .map(jstr)
                                                .unwrap_or(JsonValue::Null),
                                        ),
                                        ("createdKey", JsonValue::Bool(w.created_key)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "envWrites",
                        JsonValue::Array(
                            self.artifacts
                                .env_writes
                                .iter()
                                .map(|w| {
                                    jobj(vec![
                                        ("name", jstr(w.name.clone())),
                                        (
                                            "priorValue",
                                            w.prior_value
                                                .clone()
                                                .map(jstr)
                                                .unwrap_or(JsonValue::Null),
                                        ),
                                        ("priorType", JsonValue::Number(w.prior_type as f64)),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                    (
                        "pins",
                        JsonValue::Array(
                            self.artifacts
                                .pins
                                .iter()
                                .map(|p| {
                                    jobj(vec![
                                        ("name", jstr(p.name.clone())),
                                        ("linkPath", jstr(p.link_path.clone())),
                                        ("outcome", jstr(p.outcome.clone())),
                                    ])
                                })
                                .collect(),
                        ),
                    ),
                ]),
            ),
        ];
        if let Some(prev) = &self.previous_version {
            pairs.push(("previousVersion", jstr(prev.clone())));
        }
        jobj(pairs)
    }

    pub fn render(&self) -> String {
        self.to_json_value().render()
    }

    pub fn write_to(&self, dest: &Path) -> std::io::Result<PathBuf> {
        let path = receipt_path(dest);
        std::fs::write(&path, self.render())?;
        Ok(path)
    }

    pub fn from_json(text: &str) -> Option<InstallReceipt> {
        let root = parse_json(text).ok()?;
        let get_s = |key: &str| root.get(key).and_then(|v| v.as_str()).map(str::to_string);
        let options = root.get("options").cloned().unwrap_or(JsonValue::Null);
        let artifacts = root.get("artifacts").cloned().unwrap_or(JsonValue::Null);
        let opt_b = |key: &str| options.get(key).and_then(|v| v.as_bool()).unwrap_or(false);
        let str_list = |value: Option<&JsonValue>| -> Vec<String> {
            match value {
                Some(JsonValue::Array(items)) => items
                    .iter()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect(),
                _ => Vec::new(),
            }
        };
        let files = match root.get("files") {
            Some(JsonValue::Array(items)) => items
                .iter()
                .filter_map(|item| {
                    Some(ReceiptFile {
                        path: item.get("path")?.as_str()?.to_string(),
                        size: match item.get("size") {
                            Some(JsonValue::Number(n)) => *n as u64,
                            _ => 0,
                        },
                        sha256: item
                            .get("sha256")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        let associations = match artifacts.get("associations") {
            Some(JsonValue::Array(items)) => items
                .iter()
                .filter_map(|item| {
                    Some(AssocRecord {
                        extension: item.get("extension")?.as_str()?.to_string(),
                        prog_id: item
                            .get("progId")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        prior_default: item
                            .get("priorDefault")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        created_ext_key: item
                            .get("createdExtKey")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                        created_prog_id: item
                            .get("createdProgId")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        let start_at_login = match artifacts.get("startAtLogin") {
            Some(run @ JsonValue::Object(_)) => Some(RunRecord {
                name: run
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                command: run
                    .get("command")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                prior_value: run
                    .get("priorValue")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
            }),
            _ => None,
        };
        let opt_s = |item: &JsonValue, key: &str| {
            item.get(key).and_then(|v| v.as_str()).map(str::to_string)
        };
        let registry_writes = match artifacts.get("registryWrites") {
            Some(JsonValue::Array(items)) => items
                .iter()
                .filter_map(|item| {
                    Some(RegWriteRecord {
                        key: item.get("key")?.as_str()?.to_string(),
                        name: opt_s(item, "name").unwrap_or_default(),
                        prior_kind: opt_s(item, "priorKind"),
                        prior_value: opt_s(item, "priorValue"),
                        created_key: item
                            .get("createdKey")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        let env_writes = match artifacts.get("envWrites") {
            Some(JsonValue::Array(items)) => items
                .iter()
                .filter_map(|item| {
                    Some(EnvWriteRecord {
                        name: item.get("name")?.as_str()?.to_string(),
                        prior_value: opt_s(item, "priorValue"),
                        prior_type: match item.get("priorType") {
                            Some(JsonValue::Number(n)) => *n as u32,
                            _ => 0,
                        },
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        let pins = match artifacts.get("pins") {
            Some(JsonValue::Array(items)) => items
                .iter()
                .filter_map(|item| {
                    Some(PinRecord {
                        name: item.get("name")?.as_str()?.to_string(),
                        link_path: opt_s(item, "linkPath").unwrap_or_default(),
                        outcome: opt_s(item, "outcome").unwrap_or_default(),
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        let hooks = match root.get("hooks") {
            Some(JsonValue::Array(items)) => items
                .iter()
                .filter_map(|item| {
                    Some(HookRunRecord {
                        phase: item.get("phase")?.as_str()?.to_string(),
                        command: opt_s(item, "command").unwrap_or_default(),
                        args: str_list(item.get("args")),
                        exit_code: match item.get("exitCode") {
                            Some(JsonValue::Number(n)) => Some(*n as i64),
                            _ => None,
                        },
                        on_failure: opt_s(item, "onFailure").unwrap_or_else(|| "abort".to_string()),
                        succeeded: item
                            .get("succeeded")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(false),
                    })
                })
                .collect(),
            _ => Vec::new(),
        };
        Some(InstallReceipt {
            app: get_s("app")?,
            version: get_s("version").unwrap_or_default(),
            publisher: get_s("publisher").unwrap_or_default(),
            install_path: get_s("installPath").unwrap_or_default(),
            installed_at_unix: match root.get("installedAtUnix") {
                Some(JsonValue::Number(n)) => *n as u64,
                _ => 0,
            },
            mode: get_s("mode").unwrap_or_else(|| "install".to_string()),
            previous_version: get_s("previousVersion"),
            files,
            desktop_shortcut: opt_b("desktopShortcut"),
            start_menu_shortcut: opt_b("startMenuShortcut"),
            start_menu_folder: options
                .get("startMenuFolder")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            path_entry: opt_b("pathEntry"),
            file_associations: str_list(options.get("fileAssociations")),
            // Receipts written before these fields existed parse with
            // the same defaults the pre-v5 options used.
            launch_after_install_offered: options
                .get("launchAfterInstallOffered")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            launch_after_install_default: options
                .get("launchAfterInstallDefault")
                .and_then(|v| v.as_bool())
                .unwrap_or(true),
            start_at_login: opt_b("startAtLogin"),
            // Receipts written before v7 parse with the safe default.
            purge_data_default: opt_b("purgeDataDefault"),
            data_dirs: str_list(root.get("dataDirs")),
            // Receipts written before v6 parse with empty power
            // records (no components recorded = everything was core).
            components: str_list(root.get("components")),
            components_known: str_list(root.get("componentsKnown")),
            hooks,
            artifacts: AppliedArtifacts {
                shortcuts: str_list(artifacts.get("shortcuts")),
                associations,
                path_segment_added: artifacts
                    .get("pathSegmentAdded")
                    .and_then(|v| v.as_str())
                    .map(str::to_string),
                start_at_login,
                registry_writes,
                env_writes,
                pins,
            },
        })
    }
}

/// Reads the receipt in `dest`, if one is present and parseable.
pub fn read_receipt(dest: &Path) -> Option<InstallReceipt> {
    let text = std::fs::read_to_string(receipt_path(dest)).ok()?;
    InstallReceipt::from_json(&text)
}

/// Compares dotted version strings numerically (`1.10.0` > `1.9.0`).
/// Non-numeric suffixes (everything from the first `-` on, and any
/// non-digit tail of a component) are ignored for ordering, except that
/// a pre-release suffix sorts BEFORE the plain release (`1.0.0-beta` <
/// `1.0.0`).
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let (core_a, suffix_a) = split_suffix(a);
    let (core_b, suffix_b) = split_suffix(b);
    let parts_a: Vec<u64> = core_a
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect();
    let parts_b: Vec<u64> = core_b
        .split('.')
        .map(|p| p.parse::<u64>().unwrap_or(0))
        .collect();
    for i in 0..parts_a.len().max(parts_b.len()) {
        let x = parts_a.get(i).copied().unwrap_or(0);
        let y = parts_b.get(i).copied().unwrap_or(0);
        match x.cmp(&y) {
            Ordering::Equal => continue,
            other => return other,
        }
    }
    match (suffix_a.is_empty(), suffix_b.is_empty()) {
        (true, true) | (false, false) => Ordering::Equal,
        (true, false) => Ordering::Greater, // release > pre-release
        (false, true) => Ordering::Less,
    }
}

fn split_suffix(version: &str) -> (&str, &str) {
    match version.split_once('-') {
        Some((core, suffix)) => (core, suffix),
        None => (version, ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> InstallReceipt {
        InstallReceipt {
            app: "Test App".to_string(),
            version: "1.2.0".to_string(),
            publisher: "Tester".to_string(),
            install_path: "C:\\Apps\\Test App".to_string(),
            installed_at_unix: 1_700_000_000,
            mode: "upgrade".to_string(),
            previous_version: Some("1.1.0".to_string()),
            files: vec![ReceiptFile {
                path: "app.exe".to_string(),
                size: 42,
                sha256: "ab12".to_string(),
            }],
            desktop_shortcut: true,
            start_menu_shortcut: true,
            start_menu_folder: "Test App".to_string(),
            path_entry: true,
            file_associations: vec!["txt".to_string()],
            launch_after_install_offered: true,
            launch_after_install_default: false,
            start_at_login: true,
            purge_data_default: true,
            data_dirs: vec!["C:\\Users\\x\\AppData\\Roaming\\Test App".to_string()],
            components: vec!["core".to_string(), "extras".to_string()],
            components_known: vec![
                "core".to_string(),
                "extras".to_string(),
                "engine".to_string(),
            ],
            hooks: vec![HookRunRecord {
                phase: "postInstall".to_string(),
                command: "finish.exe".to_string(),
                args: vec!["--done".to_string()],
                exit_code: Some(0),
                on_failure: "abort".to_string(),
                succeeded: true,
            }],
            artifacts: AppliedArtifacts {
                shortcuts: vec!["C:\\Users\\x\\Desktop\\Test App.lnk".to_string()],
                associations: vec![AssocRecord {
                    extension: "txt".to_string(),
                    prog_id: "SeanPak.TestApp".to_string(),
                    prior_default: Some("txtfile".to_string()),
                    created_ext_key: false,
                    created_prog_id: true,
                }],
                path_segment_added: Some("C:\\Apps\\Test App".to_string()),
                start_at_login: Some(RunRecord {
                    name: "Test App".to_string(),
                    command: "\"C:\\Apps\\Test App\\app.exe\"".to_string(),
                    prior_value: None,
                }),
                registry_writes: vec![RegWriteRecord {
                    key: "Software\\Test App".to_string(),
                    name: "Mode".to_string(),
                    prior_kind: Some("dword".to_string()),
                    prior_value: Some("3".to_string()),
                    created_key: false,
                }],
                env_writes: vec![EnvWriteRecord {
                    name: "TEST_APP_HOME".to_string(),
                    prior_value: None,
                    prior_type: 0,
                }],
                pins: vec![PinRecord {
                    name: "Test App".to_string(),
                    link_path: "C:\\Users\\x\\Desktop\\Test App.lnk".to_string(),
                    outcome: "unavailable".to_string(),
                }],
            },
        }
    }

    #[test]
    fn receipt_round_trips_through_json() {
        let receipt = sample();
        let text = receipt.render();
        let back = InstallReceipt::from_json(&text).expect("receipt must parse back");
        assert_eq!(back, receipt);
        assert!(text.contains("\"installedAt\":\"2023-11-14 22:13:20Z\""));
    }

    #[test]
    fn legacy_receipt_without_new_fields_parses_with_defaults() {
        // A receipt written before the launch/login/data fields existed.
        let text = r#"{"app":"Old App","version":"1.0","installPath":"C:\\Apps\\Old App",
            "options":{"desktopShortcut":true},"artifacts":{"shortcuts":[]}}"#;
        let receipt = InstallReceipt::from_json(text).expect("legacy receipt must parse");
        assert!(receipt.launch_after_install_offered);
        assert!(receipt.launch_after_install_default);
        assert!(!receipt.start_at_login);
        assert!(receipt.data_dirs.is_empty());
        assert!(receipt.artifacts.start_at_login.is_none());
        assert!(receipt.components.is_empty());
        assert!(receipt.hooks.is_empty());
        assert!(receipt.artifacts.registry_writes.is_empty());
        assert!(receipt.artifacts.env_writes.is_empty());
        assert!(receipt.artifacts.pins.is_empty());
    }

    #[test]
    fn receipt_write_and_read_from_disk() {
        let dir = std::env::temp_dir().join(format!(
            "seanpak_receipt_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(read_receipt(&dir).is_none());
        sample().write_to(&dir).unwrap();
        let back = read_receipt(&dir).expect("receipt on disk must read back");
        assert_eq!(back.app, "Test App");
        assert_eq!(back.artifacts.associations.len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn version_comparison_is_numeric_and_suffix_aware() {
        assert_eq!(compare_versions("1.10.0", "1.9.0"), Ordering::Greater);
        assert_eq!(compare_versions("2.0", "2.0.0"), Ordering::Equal);
        assert_eq!(compare_versions("1.0.0-beta", "1.0.0"), Ordering::Less);
        assert_eq!(compare_versions("1.0.0", "1.0.0-beta"), Ordering::Greater);
        assert_eq!(compare_versions("0.7.2", "0.7.10"), Ordering::Less);
        assert_eq!(compare_versions("4.1.0", "4.1.0"), Ordering::Equal);
    }
}
