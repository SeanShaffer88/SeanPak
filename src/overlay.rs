//! Direct PE Overlay Engine (per-file compressed payloads since v4)
//! Appends raw sector-aligned file payloads directly to the host PE executable overlay.
//! Provides maximum sustained NVMe throughput with 0 heap allocation in write loops.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const OVERLAY_MAGIC: &[u8; 16] = b"SEANPAK_OVL\0\0\0\0\0";
/// Magic written by CrucibleKit-era builds (3.x). The reader still accepts
/// it so packages built before the SeanPak rename can be listed, extracted,
/// and installed; new packages are always written with `OVERLAY_MAGIC`.
pub const CRUCIBLEKIT_OVERLAY_MAGIC: &[u8; 16] = b"CRUCIBLEKIT_OVL\0";
/// Magic written by pre-rename (RustPak-era) builds. Kept readable for the
/// same reason as the CrucibleKit magic.
pub const LEGACY_OVERLAY_MAGIC: &[u8; 16] = b"RUSTPAK_OVERLAY\0";
/// Every legacy magic the reader accepts, newest first.
pub const LEGACY_OVERLAY_MAGICS: &[&[u8; 16]] = &[CRUCIBLEKIT_OVERLAY_MAGIC, LEGACY_OVERLAY_MAGIC];

/// True when `magic` identifies a SeanPak overlay of any product era.
fn is_overlay_magic(magic: &[u8; 16]) -> bool {
    magic == OVERLAY_MAGIC || LEGACY_OVERLAY_MAGICS.contains(&magic)
}
pub const SECTOR_ALIGNMENT: u64 = 4096;
pub const BUFFER_SIZE: usize = 65536; // 64 KB stack/fixed buffer

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FileRecordHeader {
    pub relative_path_utf8: [u8; 260],
    pub file_size: u64,
    pub payload_offset: u64,
    pub attributes: u32,
    pub is_executable: u8,
    pub reserved: [u8; 3],
}

impl Default for FileRecordHeader {
    fn default() -> Self {
        Self {
            relative_path_utf8: [0u8; 260],
            file_size: 0,
            payload_offset: 0,
            attributes: 0,
            is_executable: 0,
            reserved: [0u8; 3],
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OverlayManifestHeader {
    pub magic: [u8; 16],
    pub version: u32,
    pub file_count: u32,
    pub primary_exe_index: u32,
    pub total_uncompressed_bytes: u64,
    pub app_name_utf8: [u8; 128],
    pub app_version_utf8: [u8; 32],
    pub publisher_utf8: [u8; 128],
}

/// Reads a NUL-terminated fixed-size UTF-8 field as an owned string.
pub fn fixed_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

impl OverlayManifestHeader {
    pub fn app_name(&self) -> String {
        fixed_str(&self.app_name_utf8)
    }

    pub fn app_version(&self) -> String {
        fixed_str(&self.app_version_utf8)
    }

    pub fn publisher(&self) -> String {
        fixed_str(&self.publisher_utf8)
    }
}

impl FileRecordHeader {
    pub fn relative_path(&self) -> String {
        fixed_str(&self.relative_path_utf8)
    }

    /// Compression codec of the stored payload (manifest v4+; the first
    /// reserved byte). Packages written before v4 always carry 0
    /// ([`crate::codec::CODEC_STORED`]), so the same accessor is correct
    /// for every readable package.
    pub fn compression(&self) -> u8 {
        self.reserved[0]
    }

    pub fn set_compression(&mut self, codec: u8) {
        self.reserved[0] = codec;
    }
}

/// Capacity of the recorded data-directories field (the v5 options
/// extension): the app's data locations as one `;`-separated list of
/// token paths (see `crate::uninstall::resolve_data_dirs`).
pub const DATA_DIRS_CAPACITY: usize = 1024;

/// Install-time choices recorded in an overlay's options block, stored
/// immediately after the file-record table (payload offsets account for
/// it). The on-disk layout is versioned: manifest v3/v4 packages carry
/// the base block only ([`OptionsDiskBase`]); v5 packages append an
/// extension block ([`OptionsDiskExt`]) with the launch-after-install
/// offer, the start-at-login offer, and the app's data directories.
/// Version 2 packages have no block at all. Readers convert every
/// version into this one logical struct — pre-v5 packages get the
/// extension defaults (launch offered, start-at-login not offered, no
/// data directories) — so legacy packages stay installable with
/// sensible defaults. This struct is the in-memory form; it is never
/// serialized raw.
#[derive(Debug, Clone, Copy)]
pub struct InstallOptions {
    pub install_location_utf8: [u8; 260],
    pub start_menu_folder_utf8: [u8; 128],
    pub file_associations_utf8: [u8; 128],
    pub desktop_shortcut: u8,
    pub start_menu_shortcut: u8,
    pub path_entry: u8,
    pub reserved: [u8; 5],
    /// v5: the Finish page offers "Launch <App>" (0 = not offered).
    pub launch_after_install: u8,
    /// v5: the Finish page's launch checkbox starts checked.
    pub launch_after_install_default: u8,
    /// v5: the install offers "Start <App> at login" (0 = not offered).
    pub start_at_login: u8,
    /// v5: the start-at-login choice starts on. The wizard's Options
    /// page edits this field in place (like `desktop_shortcut`), so at
    /// apply time it holds the user's final choice.
    pub start_at_login_default: u8,
    /// v5: the app's data directories, `;`-separated token paths.
    pub data_dirs_utf8: [u8; DATA_DIRS_CAPACITY],
}

/// The v3/v4 on-disk options block — byte-for-byte the layout those
/// packages carry, kept so the reader/writer conversions stay explicit.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct OptionsDiskBase {
    install_location_utf8: [u8; 260],
    start_menu_folder_utf8: [u8; 128],
    file_associations_utf8: [u8; 128],
    desktop_shortcut: u8,
    start_menu_shortcut: u8,
    path_entry: u8,
    reserved: [u8; 5],
}

/// The v5 on-disk extension block, written immediately after the base
/// block in v5 packages.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct OptionsDiskExt {
    launch_after_install: u8,
    launch_after_install_default: u8,
    start_at_login: u8,
    start_at_login_default: u8,
    data_dirs_utf8: [u8; DATA_DIRS_CAPACITY],
}

const OPTIONS_BASE_DISK_SIZE: usize = std::mem::size_of::<OptionsDiskBase>();
const OPTIONS_EXT_DISK_SIZE: usize = std::mem::size_of::<OptionsDiskExt>();

impl InstallOptions {
    /// Defaults matching what earlier SeanPak/CrucibleKit installers did:
    /// Start Menu entry on, desktop
    /// shortcut on, no file associations, no PATH entry — plus the v5
    /// extension defaults: launch-after-install offered (checked), the
    /// start-at-login option not offered, no data directories. The
    /// recorded location is the v5.1.0 per-user default: the token
    /// path `%LOCALAPPDATA%\Programs\<AppFolder>`
    /// ([`crate::install::default_install_location`]), resolved on the
    /// installing account at install time — never Program Files,
    /// which a per-user installer cannot write.
    pub fn defaults_for(app_name: &str) -> Self {
        let mut options = Self {
            install_location_utf8: [0u8; 260],
            start_menu_folder_utf8: [0u8; 128],
            file_associations_utf8: [0u8; 128],
            desktop_shortcut: 1,
            start_menu_shortcut: 1,
            path_entry: 0,
            reserved: [0u8; 5],
            launch_after_install: 1,
            launch_after_install_default: 1,
            start_at_login: 0,
            start_at_login_default: 0,
            data_dirs_utf8: [0u8; DATA_DIRS_CAPACITY],
        };
        copy_str_to_fixed(
            &mut options.install_location_utf8,
            &crate::install::default_install_location(app_name),
        );
        copy_str_to_fixed(&mut options.start_menu_folder_utf8, app_name);
        options
    }

    /// The v3/v4 disk form of the base fields.
    fn disk_base(&self) -> OptionsDiskBase {
        OptionsDiskBase {
            install_location_utf8: self.install_location_utf8,
            start_menu_folder_utf8: self.start_menu_folder_utf8,
            file_associations_utf8: self.file_associations_utf8,
            desktop_shortcut: self.desktop_shortcut,
            start_menu_shortcut: self.start_menu_shortcut,
            path_entry: self.path_entry,
            reserved: self.reserved,
        }
    }

    /// Logical options from a v3/v4 base block: the extension fields
    /// take the pre-v5 defaults (the same values [`Self::defaults_for`]
    /// uses), because those packages recorded no extension.
    fn from_disk_base(base: &OptionsDiskBase) -> Self {
        Self {
            install_location_utf8: base.install_location_utf8,
            start_menu_folder_utf8: base.start_menu_folder_utf8,
            file_associations_utf8: base.file_associations_utf8,
            desktop_shortcut: base.desktop_shortcut,
            start_menu_shortcut: base.start_menu_shortcut,
            path_entry: base.path_entry,
            reserved: base.reserved,
            launch_after_install: 1,
            launch_after_install_default: 1,
            start_at_login: 0,
            start_at_login_default: 0,
            data_dirs_utf8: [0u8; DATA_DIRS_CAPACITY],
        }
    }

    /// The v5 disk form of the extension fields.
    fn disk_ext(&self) -> OptionsDiskExt {
        OptionsDiskExt {
            launch_after_install: self.launch_after_install,
            launch_after_install_default: self.launch_after_install_default,
            start_at_login: self.start_at_login,
            start_at_login_default: self.start_at_login_default,
            data_dirs_utf8: self.data_dirs_utf8,
        }
    }

    /// Merges a v5 extension block read from disk into these options.
    fn apply_disk_ext(&mut self, ext: &OptionsDiskExt) {
        self.launch_after_install = ext.launch_after_install;
        self.launch_after_install_default = ext.launch_after_install_default;
        self.start_at_login = ext.start_at_login;
        self.start_at_login_default = ext.start_at_login_default;
        self.data_dirs_utf8 = ext.data_dirs_utf8;
    }

    pub fn install_location(&self) -> String {
        fixed_str(&self.install_location_utf8)
    }

    pub fn start_menu_folder(&self) -> String {
        fixed_str(&self.start_menu_folder_utf8)
    }

    pub fn file_associations(&self) -> String {
        fixed_str(&self.file_associations_utf8)
    }

    pub fn wants_desktop_shortcut(&self) -> bool {
        self.desktop_shortcut != 0
    }

    pub fn wants_start_menu_shortcut(&self) -> bool {
        self.start_menu_shortcut != 0
    }

    pub fn wants_path_entry(&self) -> bool {
        self.path_entry != 0
    }

    /// The package offers "Launch <App>" on the Finish page.
    pub fn offers_launch_after_install(&self) -> bool {
        self.launch_after_install != 0
    }

    /// The Finish page's launch checkbox starts checked.
    pub fn launch_after_install_default_checked(&self) -> bool {
        self.launch_after_install_default != 0
    }

    /// The package offers the "Start <App> at login" install option.
    pub fn offers_start_at_login(&self) -> bool {
        self.start_at_login != 0
    }

    /// The start-at-login option starts on (and, after the wizard's
    /// Options page edits it in place, holds the user's final choice).
    pub fn start_at_login_default_checked(&self) -> bool {
        self.start_at_login_default != 0
    }

    /// The effective start-at-login choice an install applies: only an
    /// offered option can be on.
    pub fn start_at_login_chosen(&self) -> bool {
        self.offers_start_at_login() && self.start_at_login_default_checked()
    }

    /// The app's recorded data directories (token paths, as written in
    /// the manifest — resolution happens at install time).
    pub fn data_dirs(&self) -> Vec<String> {
        fixed_str(&self.data_dirs_utf8)
            .split(';')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect()
    }
}

// ---------------------------------------------------------------------------
// The v6 power block: components, install hooks, registry/environment
// writes, and rich shortcuts — the "power bundle" a manifest can declare.
//
// Unlike the fixed-layout options blocks, this block is variable-length,
// so it is stored as a u32 length prefix plus a JSON document (rendered
// and parsed with the crate's hand-rolled JSON in `crate::manifest`),
// written immediately after the v5 options extension. Readers of
// pre-v6 packages get an empty [`PowerSpec`] — every file belongs to
// the implicit `core` component and nothing else applies — so old
// packages install exactly as they always did.

/// The implicit component every unclaimed file belongs to. Always
/// installed; manifests may not declare it.
pub const CORE_COMPONENT_ID: &str = "core";

/// What a failing install hook does to the install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookFailurePolicy {
    /// The install fails and rolls back (exit 5).
    Abort,
    /// A warning is recorded and the install proceeds.
    Continue,
}

impl HookFailurePolicy {
    pub fn as_str(&self) -> &'static str {
        match self {
            HookFailurePolicy::Abort => "abort",
            HookFailurePolicy::Continue => "continue",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "abort" => Some(HookFailurePolicy::Abort),
            "continue" => Some(HookFailurePolicy::Continue),
            _ => None,
        }
    }
}

/// One pre/post install command declared by the manifest.
#[derive(Debug, Clone, PartialEq)]
pub struct HookSpec {
    pub command: String,
    pub args: Vec<String>,
    pub on_failure: HookFailurePolicy,
}

/// The registry value kinds a manifest write can declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegValueKind {
    Sz,
    Dword,
}

impl RegValueKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            RegValueKind::Sz => "sz",
            RegValueKind::Dword => "dword",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "sz" => Some(RegValueKind::Sz),
            "dword" => Some(RegValueKind::Dword),
            _ => None,
        }
    }
}

/// One HKCU registry write declared by the manifest. `key` is relative
/// to HKEY_CURRENT_USER; `name` is the value name ("" = the key's
/// default value); `value` is the string form (for `dword`, the
/// canonical decimal rendering of the number).
#[derive(Debug, Clone, PartialEq)]
pub struct RegWriteSpec {
    pub key: String,
    pub name: String,
    pub kind: RegValueKind,
    pub value: String,
}

/// One per-user environment-variable write (HKCU\Environment).
#[derive(Debug, Clone, PartialEq)]
pub struct EnvWriteSpec {
    pub name: String,
    pub value: String,
}

/// Where a rich shortcut's link file lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShortcutLocation {
    Desktop,
    StartMenu,
    Both,
}

impl ShortcutLocation {
    pub fn as_str(&self) -> &'static str {
        match self {
            ShortcutLocation::Desktop => "desktop",
            ShortcutLocation::StartMenu => "startMenu",
            ShortcutLocation::Both => "both",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "desktop" => Some(ShortcutLocation::Desktop),
            "startMenu" => Some(ShortcutLocation::StartMenu),
            "both" => Some(ShortcutLocation::Both),
            _ => None,
        }
    }

    pub fn wants_desktop(&self) -> bool {
        matches!(self, ShortcutLocation::Desktop | ShortcutLocation::Both)
    }

    pub fn wants_start_menu(&self) -> bool {
        matches!(self, ShortcutLocation::StartMenu | ShortcutLocation::Both)
    }
}

/// One rich shortcut declared by the manifest: a `.lnk` with explicit
/// target/arguments/icon, beyond the automatic primary-exe shortcuts.
#[derive(Debug, Clone, PartialEq)]
pub struct RichShortcutSpec {
    pub name: String,
    /// Install-relative path of the target file.
    pub target: String,
    pub arguments: String,
    /// Install-relative icon file (defaults to the target itself).
    pub icon: Option<String>,
    pub icon_index: i32,
    pub description: String,
    pub location: ShortcutLocation,
    /// Best-effort pin to taskbar (recorded, never fatal).
    pub pin: bool,
}

/// One optional (or required) component: a named subset of the
/// package's files the user can include or skip at install time.
#[derive(Debug, Clone, PartialEq)]
pub struct ComponentSpec {
    pub id: String,
    pub name: String,
    pub description: String,
    /// Files (package-relative) this component claims.
    pub files: Vec<String>,
    pub required: bool,
    pub default_selected: bool,
    /// Packed (stored, post-compression) size of the component's
    /// files, computed by the packer with its own codec. Zero in a
    /// manifest; filled in the package's power block.
    pub packed_size: u64,
}

/// Everything the v6 power block declares. An empty spec (the default,
/// and what every pre-v6 package reads as) changes nothing about an
/// install.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PowerSpec {
    pub components: Vec<ComponentSpec>,
    pub pre_hooks: Vec<HookSpec>,
    pub post_hooks: Vec<HookSpec>,
    pub registry_writes: Vec<RegWriteSpec>,
    pub env_writes: Vec<EnvWriteSpec>,
    pub shortcuts: Vec<RichShortcutSpec>,
}

/// Normalizes a package-relative path for component bookkeeping.
fn normalize_rel(rel: &str) -> String {
    rel.replace('\\', "/")
}

impl PowerSpec {
    pub fn is_empty(&self) -> bool {
        self.components.is_empty()
            && self.pre_hooks.is_empty()
            && self.post_hooks.is_empty()
            && self.registry_writes.is_empty()
            && self.env_writes.is_empty()
            && self.shortcuts.is_empty()
    }

    pub fn component(&self, id: &str) -> Option<&ComponentSpec> {
        self.components.iter().find(|c| c.id == id)
    }

    /// The component that claims `rel` (a package-relative path), or
    /// [`CORE_COMPONENT_ID`] when no component claims it.
    pub fn component_id_for_file(&self, rel: &str) -> String {
        let rel = normalize_rel(rel);
        for c in &self.components {
            if c.files.iter().any(|f| normalize_rel(f) == rel) {
                return c.id.clone();
            }
        }
        CORE_COMPONENT_ID.to_string()
    }

    /// True when the package has files no component claims (a real
    /// `core` part exists).
    pub fn has_core_files(&self, records: &[FileRecordHeader]) -> bool {
        records
            .iter()
            .any(|r| self.component_id_for_file(&r.relative_path()) == CORE_COMPONENT_ID)
    }

    /// Total packed (stored) size of the payloads the implicit
    /// `core` component owns, derived from the record offsets the
    /// same way payload reads measure them.
    pub fn core_packed_size(&self, records: &[FileRecordHeader], stream_len: u64) -> u64 {
        records
            .iter()
            .enumerate()
            .filter(|(_, r)| self.component_id_for_file(&r.relative_path()) == CORE_COMPONENT_ID)
            .map(|(idx, _)| OverlayReader::stored_len(records, idx, stream_len))
            .sum()
    }

    /// Every selectable part id, in stable order: `core` first (when
    /// `include_core`), then the declared components in order.
    pub fn part_ids(&self, include_core: bool) -> Vec<String> {
        let mut ids = Vec::new();
        if include_core {
            ids.push(CORE_COMPONENT_ID.to_string());
        }
        ids.extend(self.components.iter().map(|c| c.id.clone()));
        ids
    }

    /// The selection an install uses when nothing overrides it:
    /// required components always; with a previous install's record
    /// (`previous_installed` + `previous_known` — the ids the previous
    /// package offered), components the previous package knew keep
    /// their previous in/out state and brand-new components take
    /// their manifest default; without a previous record, manifest
    /// defaults rule. `core` is included when `include_core` (the
    /// package has unclaimed files).
    pub fn default_selection(
        &self,
        previous: Option<(&[String], &[String])>,
        include_core: bool,
    ) -> Vec<String> {
        let mut selected: Vec<String> = Vec::new();
        if include_core {
            selected.push(CORE_COMPONENT_ID.to_string());
        }
        for c in &self.components {
            let on = if c.required {
                true
            } else if let Some((installed, known)) = previous {
                if known.iter().any(|k| k == &c.id) {
                    installed.iter().any(|i| i == &c.id)
                } else {
                    c.default_selected
                }
            } else {
                c.default_selected
            };
            if on {
                selected.push(c.id.clone());
            }
        }
        selected
    }

    /// Resolves the effective component selection for one install.
    ///
    /// - `explicit`: a caller's full selection (CLI `--components`,
    ///   the wizard's checkboxes). Every id must name a declared
    ///   component (or `core`).
    /// - `without`: ids to remove from the resolved selection (CLI
    ///   `--without`); removing a required component, the primary
    ///   executable's component, or an unknown id is an error.
    /// - `previous`: the previous install's (installed, known) ids,
    ///   feeding [`PowerSpec::default_selection`].
    ///
    /// Required components, `core` (when the package has unclaimed
    /// files), and the component holding the primary executable are
    /// always in the result — an install never strands its own
    /// primary exe. The result is in part order (core first, then
    /// manifest order).
    pub fn resolve_selection(
        &self,
        explicit: Option<&[String]>,
        without: &[String],
        previous: Option<(&[String], &[String])>,
        records: &[FileRecordHeader],
        primary_rel: Option<&str>,
    ) -> Result<Vec<String>, String> {
        let include_core = self.has_core_files(records);
        let mut chosen: Vec<String> = match explicit {
            Some(ids) => {
                let mut out: Vec<String> = Vec::new();
                for id in ids {
                    if id == CORE_COMPONENT_ID {
                        if include_core && !out.iter().any(|o| o == CORE_COMPONENT_ID) {
                            out.push(CORE_COMPONENT_ID.to_string());
                        }
                        continue;
                    }
                    match self.component(id) {
                        Some(_) => {
                            if !out.contains(id) {
                                out.push(id.clone());
                            }
                        }
                        None => {
                            return Err(format!(
                                "unknown component \"{id}\" (this package offers: {})",
                                self.part_ids(true).join(", ")
                            ))
                        }
                    }
                }
                out
            }
            None => self.default_selection(previous, include_core),
        };
        // Required parts are never out.
        if include_core && !chosen.iter().any(|c| c == CORE_COMPONENT_ID) {
            chosen.push(CORE_COMPONENT_ID.to_string());
        }
        for c in &self.components {
            if c.required && !chosen.contains(&c.id) {
                chosen.push(c.id.clone());
            }
        }
        // The primary executable's component is always installed.
        let primary_component = primary_rel.map(|rel| self.component_id_for_file(rel));
        if let Some(pc) = &primary_component {
            if !chosen.contains(pc) {
                chosen.push(pc.clone());
            }
        }
        for id in without {
            if id == CORE_COMPONENT_ID {
                return Err(
                    "the core component cannot be removed — it holds the package's unclaimed files"
                        .to_string(),
                );
            }
            let Some(component) = self.component(id) else {
                return Err(format!(
                    "unknown component \"{id}\" (this package offers: {})",
                    self.part_ids(true).join(", ")
                ));
            };
            if component.required {
                return Err(format!(
                    "component \"{id}\" is required and cannot be removed"
                ));
            }
            if primary_component.as_deref() == Some(id.as_str()) {
                return Err(format!(
                    "component \"{id}\" holds the app's main executable and cannot be removed"
                ));
            }
            chosen.retain(|c| c != id);
        }
        // Stable part order in the result.
        Ok(self
            .part_ids(include_core)
            .into_iter()
            .filter(|id| chosen.contains(id))
            .collect())
    }

    /// The JSON document stored in the v6 power block (and the shape
    /// the manifest's power fields render to — one encoding, two
    /// homes).
    pub fn to_json_value(&self) -> crate::manifest::JsonValue {
        use crate::manifest::{jobj, jstr, JsonValue};
        let hook_json = |h: &HookSpec| {
            jobj(vec![
                ("command", jstr(h.command.clone())),
                (
                    "args",
                    JsonValue::Array(h.args.iter().map(|a| jstr(a.clone())).collect()),
                ),
                ("onFailure", jstr(h.on_failure.as_str())),
            ])
        };
        let mut pairs: Vec<(&str, JsonValue)> = Vec::new();
        if !self.components.is_empty() {
            pairs.push((
                "components",
                JsonValue::Array(
                    self.components
                        .iter()
                        .map(|c| {
                            jobj(vec![
                                ("id", jstr(c.id.clone())),
                                ("name", jstr(c.name.clone())),
                                ("description", jstr(c.description.clone())),
                                (
                                    "files",
                                    JsonValue::Array(
                                        c.files.iter().map(|f| jstr(f.clone())).collect(),
                                    ),
                                ),
                                ("required", JsonValue::Bool(c.required)),
                                ("defaultSelected", JsonValue::Bool(c.default_selected)),
                                ("packedSize", JsonValue::Number(c.packed_size as f64)),
                            ])
                        })
                        .collect(),
                ),
            ));
        }
        if !self.pre_hooks.is_empty() || !self.post_hooks.is_empty() {
            pairs.push((
                "hooks",
                jobj(vec![
                    (
                        "preInstall",
                        JsonValue::Array(self.pre_hooks.iter().map(hook_json).collect()),
                    ),
                    (
                        "postInstall",
                        JsonValue::Array(self.post_hooks.iter().map(hook_json).collect()),
                    ),
                ]),
            ));
        }
        if !self.registry_writes.is_empty() {
            pairs.push((
                "registryWrites",
                JsonValue::Array(
                    self.registry_writes
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
            ));
        }
        if !self.env_writes.is_empty() {
            pairs.push((
                "envWrites",
                JsonValue::Array(
                    self.env_writes
                        .iter()
                        .map(|w| {
                            jobj(vec![
                                ("name", jstr(w.name.clone())),
                                ("value", jstr(w.value.clone())),
                            ])
                        })
                        .collect(),
                ),
            ));
        }
        if !self.shortcuts.is_empty() {
            pairs.push((
                "shortcuts",
                JsonValue::Array(
                    self.shortcuts
                        .iter()
                        .map(|s| {
                            let mut fields: Vec<(&str, JsonValue)> = vec![
                                ("name", jstr(s.name.clone())),
                                ("target", jstr(s.target.clone())),
                                ("arguments", jstr(s.arguments.clone())),
                            ];
                            if let Some(icon) = &s.icon {
                                fields.push(("icon", jstr(icon.clone())));
                            }
                            fields.push(("iconIndex", JsonValue::Number(s.icon_index as f64)));
                            fields.push(("description", jstr(s.description.clone())));
                            fields.push(("location", jstr(s.location.as_str())));
                            fields.push(("pin", JsonValue::Bool(s.pin)));
                            jobj(fields)
                        })
                        .collect(),
                ),
            ));
        }
        jobj(pairs)
    }

    /// Parses a power-block JSON document. Lenient by design (a
    /// package in the field must stay installable): unknown or
    /// malformed entries are skipped, never fatal.
    pub fn from_json_value(root: &crate::manifest::JsonValue) -> PowerSpec {
        use crate::manifest::JsonValue;
        let str_list = |v: Option<&JsonValue>| -> Vec<String> {
            match v {
                Some(JsonValue::Array(items)) => items
                    .iter()
                    .filter_map(|i| i.as_str().map(str::to_string))
                    .collect(),
                _ => Vec::new(),
            }
        };
        let get_s = |v: &JsonValue, key: &str| -> String {
            v.get(key)
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string()
        };
        let get_b = |v: &JsonValue, key: &str, default: bool| -> bool {
            v.get(key).and_then(|x| x.as_bool()).unwrap_or(default)
        };
        let parse_hooks = |v: Option<&JsonValue>| -> Vec<HookSpec> {
            match v {
                Some(JsonValue::Array(items)) => items
                    .iter()
                    .filter_map(|item| {
                        let command = get_s(item, "command");
                        if command.is_empty() {
                            return None;
                        }
                        Some(HookSpec {
                            command,
                            args: str_list(item.get("args")),
                            on_failure: item
                                .get("onFailure")
                                .and_then(|x| x.as_str())
                                .and_then(HookFailurePolicy::parse)
                                .unwrap_or(HookFailurePolicy::Abort),
                        })
                    })
                    .collect(),
                _ => Vec::new(),
            }
        };
        let mut spec = PowerSpec::default();
        if let Some(JsonValue::Array(items)) = root.get("components") {
            for item in items {
                let id = get_s(item, "id");
                if id.is_empty() || id == CORE_COMPONENT_ID {
                    continue;
                }
                spec.components.push(ComponentSpec {
                    id,
                    name: get_s(item, "name"),
                    description: get_s(item, "description"),
                    files: str_list(item.get("files")),
                    required: get_b(item, "required", false),
                    default_selected: get_b(item, "defaultSelected", true),
                    packed_size: match item.get("packedSize") {
                        Some(JsonValue::Number(n)) => *n as u64,
                        _ => 0,
                    },
                });
            }
        }
        if let Some(hooks) = root.get("hooks") {
            spec.pre_hooks = parse_hooks(hooks.get("preInstall"));
            spec.post_hooks = parse_hooks(hooks.get("postInstall"));
        }
        if let Some(JsonValue::Array(items)) = root.get("registryWrites") {
            for item in items {
                let key = get_s(item, "key");
                if key.is_empty() {
                    continue;
                }
                spec.registry_writes.push(RegWriteSpec {
                    key,
                    name: get_s(item, "name"),
                    kind: item
                        .get("type")
                        .and_then(|x| x.as_str())
                        .and_then(RegValueKind::parse)
                        .unwrap_or(RegValueKind::Sz),
                    value: get_s(item, "value"),
                });
            }
        }
        if let Some(JsonValue::Array(items)) = root.get("envWrites") {
            for item in items {
                let name = get_s(item, "name");
                if name.is_empty() {
                    continue;
                }
                spec.env_writes.push(EnvWriteSpec {
                    name,
                    value: get_s(item, "value"),
                });
            }
        }
        if let Some(JsonValue::Array(items)) = root.get("shortcuts") {
            for item in items {
                let name = get_s(item, "name");
                let target = get_s(item, "target");
                if name.is_empty() || target.is_empty() {
                    continue;
                }
                spec.shortcuts.push(RichShortcutSpec {
                    name,
                    target,
                    arguments: get_s(item, "arguments"),
                    icon: item
                        .get("icon")
                        .and_then(|x| x.as_str())
                        .map(str::to_string),
                    icon_index: match item.get("iconIndex") {
                        Some(JsonValue::Number(n)) => *n as i32,
                        _ => 0,
                    },
                    description: get_s(item, "description"),
                    location: item
                        .get("location")
                        .and_then(|x| x.as_str())
                        .and_then(ShortcutLocation::parse)
                        .unwrap_or(ShortcutLocation::StartMenu),
                    pin: get_b(item, "pin", false),
                });
            }
        }
        spec
    }
}

pub struct OverlayWriter;

/// Per-file progress callback for packing: `(index, total, path)`,
/// with the same shape as extraction's [`OnExtractFile`].
pub type OnBuildFile<'a> = dyn FnMut(usize, usize, &str) + 'a;

/// Progress + cancellation hooks for
/// [`OverlayWriter::build_overlay_with_power_ctl`] — the writer-side
/// mirror of [`ExtractControl`].
#[derive(Default)]
pub struct BuildControl<'a> {
    /// Called as `(index, total, relative_path)` before each file is
    /// packed.
    pub on_file: Option<&'a mut OnBuildFile<'a>>,
    /// When this flag is set, packing aborts between files with an
    /// `Interrupted` error.
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
}

impl OverlayWriter {
    /// Scans `source_dir`, then packs every file found into a sector-aligned
    /// overlay appended to a copy of `stub_binary_path` at `output_exe_path`.
    /// The output file itself is excluded when it lives inside `source_dir`.
    /// Returns the total size of the written executable.
    pub fn build_from_folder(
        stub_binary_path: &Path,
        output_exe_path: &Path,
        source_dir: &Path,
        app_name: &str,
        version: &str,
        publisher: &str,
    ) -> std::io::Result<u64> {
        Self::build_from_folder_with_options(
            stub_binary_path,
            output_exe_path,
            source_dir,
            app_name,
            version,
            publisher,
            &InstallOptions::defaults_for(app_name),
        )
    }

    /// Like [`OverlayWriter::build_from_folder`], but records explicit
    /// install-time `options` in the package's v7 layout instead of the
    /// per-app defaults.
    #[allow(clippy::too_many_arguments)]
    pub fn build_from_folder_with_options(
        stub_binary_path: &Path,
        output_exe_path: &Path,
        source_dir: &Path,
        app_name: &str,
        version: &str,
        publisher: &str,
        options: &InstallOptions,
    ) -> std::io::Result<u64> {
        let scanned = crate::scanner::ScannedFolder::scan(source_dir)?;
        let output_key = std::fs::canonicalize(output_exe_path)
            .unwrap_or_else(|_| output_exe_path.to_path_buf());
        let mut files_to_pack = Vec::with_capacity(scanned.files.len());
        for f in &scanned.files {
            let key =
                std::fs::canonicalize(&f.absolute_path).unwrap_or_else(|_| f.absolute_path.clone());
            if key == output_key {
                continue;
            }
            let is_primary = scanned.suggested_primary_exe.as_ref() == Some(&f.relative_path);
            files_to_pack.push((f.absolute_path.clone(), f.relative_path.clone(), is_primary));
        }
        Self::build_overlay(
            stub_binary_path,
            output_exe_path,
            source_dir,
            &files_to_pack,
            app_name,
            version,
            publisher,
            options,
        )
    }

    /// Packs source directory files into a standalone PE output executable by appending
    /// a sector-aligned direct binary overlay.
    #[allow(clippy::too_many_arguments)]
    pub fn build_overlay(
        stub_binary_path: &Path,
        output_exe_path: &Path,
        source_dir: &Path,
        files: &[(PathBuf, String, bool)], // (absolute_path, relative_path, is_primary)
        app_name: &str,
        version: &str,
        publisher: &str,
        options: &InstallOptions,
    ) -> std::io::Result<u64> {
        let (total, _power) = Self::build_overlay_with_power(
            stub_binary_path,
            output_exe_path,
            source_dir,
            files,
            app_name,
            version,
            publisher,
            options,
            &PowerSpec::default(),
        )?;
        Ok(total)
    }

    /// Like [`OverlayWriter::build_overlay`], but also records the
    /// package's [`PowerSpec`] in the v6 power block. Returns the
    /// total written size and the power spec as stored — identical to
    /// the input except each component's `packed_size` is filled with
    /// the stored (post-compression) size the packer's own codec
    /// produced for that component's files.
    #[allow(clippy::too_many_arguments)]
    pub fn build_overlay_with_power(
        stub_binary_path: &Path,
        output_exe_path: &Path,
        source_dir: &Path,
        files: &[(PathBuf, String, bool)], // (absolute_path, relative_path, is_primary)
        app_name: &str,
        version: &str,
        publisher: &str,
        options: &InstallOptions,
        power: &PowerSpec,
    ) -> std::io::Result<(u64, PowerSpec)> {
        Self::build_overlay_with_power_ctl(
            stub_binary_path,
            output_exe_path,
            source_dir,
            files,
            app_name,
            version,
            publisher,
            options,
            power,
            &mut BuildControl::default(),
        )
    }

    /// Like [`OverlayWriter::build_overlay_with_power`], with
    /// per-file progress + cancellation hooks ([`BuildControl`]).
    /// Cancellation aborts between files with an `Interrupted` error;
    /// the partially written output executable is the caller's to
    /// remove (the pack operation builds into a temp directory and
    /// deletes it).
    #[allow(clippy::too_many_arguments)]
    pub fn build_overlay_with_power_ctl(
        stub_binary_path: &Path,
        output_exe_path: &Path,
        _source_dir: &Path,
        files: &[(PathBuf, String, bool)], // (absolute_path, relative_path, is_primary)
        app_name: &str,
        version: &str,
        publisher: &str,
        options: &InstallOptions,
        power: &PowerSpec,
        control: &mut BuildControl<'_>,
    ) -> std::io::Result<(u64, PowerSpec)> {
        // Copy host stub executable to target output path
        std::fs::copy(stub_binary_path, output_exe_path)?;

        let mut out_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(output_exe_path)?;

        // Seek to end of PE file
        let pe_end_offset = out_file.seek(SeekFrom::End(0))?;

        // Align to sector boundary (4096 bytes)
        let aligned_overlay_start =
            (pe_end_offset + (SECTOR_ALIGNMENT - 1)) & !(SECTOR_ALIGNMENT - 1);
        let padding_needed = (aligned_overlay_start - pe_end_offset) as usize;
        if padding_needed > 0 {
            let pad = [0u8; 4096];
            out_file.write_all(&pad[..padding_needed])?;
        }

        let mut manifest = OverlayManifestHeader {
            magic: *OVERLAY_MAGIC,
            version: 7,
            file_count: files.len() as u32,
            primary_exe_index: 0,
            total_uncompressed_bytes: 0,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };

        // Copy strings to fixed-size buffers
        copy_str_to_fixed(&mut manifest.app_name_utf8, app_name);
        copy_str_to_fixed(&mut manifest.app_version_utf8, version);
        copy_str_to_fixed(&mut manifest.publisher_utf8, publisher);

        let mut record_headers = Vec::with_capacity(files.len());
        let mut total_bytes: u64 = 0;
        // Stored payloads, in record order. Payloads are compressed per
        // file (manifest v4), so each file is buffered once at pack time;
        // `file_size` in the record stays the ORIGINAL size and the stored
        // length is recoverable from the next record's offset (payloads
        // are laid out contiguously, as they always have been). Payload
        // offsets are assigned after this loop: the v6 power block's
        // length depends on the per-component packed sizes, which only
        // exist once every file is compressed.
        let mut stored_payloads: Vec<Vec<u8>> = Vec::with_capacity(files.len());
        let mut payload_digests: Vec<[u8; 32]> = Vec::with_capacity(files.len());

        for (idx, (abs_path, rel_path, is_primary)) in files.iter().enumerate() {
            if let Some(cancel) = control.cancel {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "pack cancelled",
                    ));
                }
            }
            if let Some(on_file) = control.on_file.as_deref_mut() {
                on_file(idx, files.len(), rel_path);
            }
            let raw = std::fs::read(abs_path)?;
            payload_digests.push(crate::hash::sha256_digest(&raw));
            let file_size = raw.len() as u64;
            total_bytes += file_size;
            let (codec, stored) = crate::codec::compress_best(&raw);

            let mut header = FileRecordHeader {
                file_size,
                payload_offset: 0, // assigned below, once sizes are known
                attributes: 0x80,  // FILE_ATTRIBUTE_NORMAL
                is_executable: if *is_primary { 1 } else { 0 },
                ..Default::default()
            };
            header.set_compression(codec);

            copy_str_to_fixed(&mut header.relative_path_utf8, rel_path);
            if *is_primary {
                manifest.primary_exe_index = idx as u32;
            }

            record_headers.push(header);
            stored_payloads.push(stored);
        }

        manifest.total_uncompressed_bytes = total_bytes;

        // The stored power block: the declared spec with each
        // component's packed size filled from the stored payloads.
        let mut computed_power = power.clone();
        for component in &mut computed_power.components {
            component.packed_size = 0;
        }
        for ((_abs, rel_path, _primary), stored) in files.iter().zip(&stored_payloads) {
            let id = computed_power.component_id_for_file(rel_path);
            if let Some(component) = computed_power.components.iter_mut().find(|c| c.id == id) {
                component.packed_size += stored.len() as u64;
            }
        }
        let power_json = computed_power.to_json_value().render();
        let power_block_size = 4 + power_json.len() as u64;

        let manifest_size = std::mem::size_of::<OverlayManifestHeader>() as u64;
        let records_total_size = (files.len() * std::mem::size_of::<FileRecordHeader>()) as u64;
        // v5 options: the v3/v4 base block plus the v5 extension block,
        // then the v6 power block.
        let options_size = (OPTIONS_BASE_DISK_SIZE + OPTIONS_EXT_DISK_SIZE) as u64;
        let integrity_table_size = payload_digests.len() as u64 * 32;
        let mut current_payload_offset = aligned_overlay_start
            + manifest_size
            + records_total_size
            + options_size
            + power_block_size
            + integrity_table_size;
        for (header, stored) in record_headers.iter_mut().zip(&stored_payloads) {
            header.payload_offset = current_payload_offset;
            current_payload_offset += stored.len() as u64;
        }

        // Write manifest header
        let manifest_bytes = unsafe {
            std::slice::from_raw_parts(
                &manifest as *const OverlayManifestHeader as *const u8,
                std::mem::size_of::<OverlayManifestHeader>(),
            )
        };
        out_file.write_all(manifest_bytes)?;

        // Write file record headers table
        for header in &record_headers {
            let header_bytes = unsafe {
                std::slice::from_raw_parts(
                    header as *const FileRecordHeader as *const u8,
                    std::mem::size_of::<FileRecordHeader>(),
                )
            };
            out_file.write_all(header_bytes)?;
        }

        // Write the install-options blocks after the record table: the
        // base block (the v3 layout) followed by the v5 extension.
        let base = options.disk_base();
        let base_bytes = unsafe {
            std::slice::from_raw_parts(
                &base as *const OptionsDiskBase as *const u8,
                OPTIONS_BASE_DISK_SIZE,
            )
        };
        out_file.write_all(base_bytes)?;
        let ext = options.disk_ext();
        let ext_bytes = unsafe {
            std::slice::from_raw_parts(
                &ext as *const OptionsDiskExt as *const u8,
                OPTIONS_EXT_DISK_SIZE,
            )
        };
        out_file.write_all(ext_bytes)?;

        // Write the v6 power block: a u32 length prefix + the JSON
        // document, immediately after the v5 extension block.
        out_file.write_all(&(power_json.len() as u32).to_le_bytes())?;
        out_file.write_all(power_json.as_bytes())?;

        // v7 stores one raw SHA-256 digest for each original payload.
        for digest in &payload_digests {
            out_file.write_all(digest)?;
        }

        // Write the stored (possibly compressed) payloads in record order.
        for stored in &stored_payloads {
            out_file.write_all(stored)?;
        }

        out_file.flush()?;
        let total_written = out_file.seek(SeekFrom::End(0))?;
        Ok((total_written, computed_power))
    }
}

pub struct OverlayReader;

/// A fully read package: the manifest header, the file records, the
/// install options, and the v6 power block (empty for older
/// packages) — what [`OverlayReader::read_package_full`] returns.
pub type PackageFull = (
    OverlayManifestHeader,
    Vec<FileRecordHeader>,
    InstallOptions,
    PowerSpec,
);

/// Package metadata plus the optional v7 SHA-256 table, in record order.
pub type PackageWithIntegrity = (
    OverlayManifestHeader,
    Vec<FileRecordHeader>,
    InstallOptions,
    PowerSpec,
    Option<Vec<[u8; 32]>>,
);

impl OverlayReader {
    /// Returns the byte offset where package payload data ends. Authenticode
    /// stores its certificate table at the PE security directory's file offset;
    /// excluding that appended table keeps a signed installer readable without
    /// treating the signature as the last file's compressed payload.
    pub fn payload_stream_len<R: Read + Seek>(reader: &mut R) -> std::io::Result<u64> {
        let stream_len = reader.seek(SeekFrom::End(0))?;
        if stream_len < 64 {
            return Ok(stream_len);
        }

        reader.seek(SeekFrom::Start(0))?;
        let mut dos_header = [0u8; 64];
        reader.read_exact(&mut dos_header)?;
        if dos_header[0] != b'M' || dos_header[1] != b'Z' {
            return Ok(stream_len);
        }

        let pe_offset = u32::from_le_bytes(dos_header[0x3c..0x40].try_into().unwrap()) as u64;
        if pe_offset > stream_len.saturating_sub(24) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "truncated PE header while locating Authenticode certificate table",
            ));
        }
        reader.seek(SeekFrom::Start(pe_offset))?;
        let mut pe_header = [0u8; 24];
        reader.read_exact(&mut pe_header)?;
        if &pe_header[..4] != b"PE\0\0" {
            return Ok(stream_len);
        }
        let optional_header_size =
            u16::from_le_bytes([pe_header[20], pe_header[21]]) as u64;
        let optional_header_start = pe_offset + 24;
        if optional_header_size < 2 {
            return Ok(stream_len);
        }

        reader.seek(SeekFrom::Start(optional_header_start))?;
        let mut magic_bytes = [0u8; 2];
        reader.read_exact(&mut magic_bytes)?;
        let (number_of_directories_offset, directories_offset) =
            match u16::from_le_bytes(magic_bytes) {
                0x10b => (92u64, 96u64),  // PE32
                0x20b => (108u64, 112u64), // PE32+
                _ => return Ok(stream_len),
            };
        if optional_header_size < number_of_directories_offset + 4
            || optional_header_size < directories_offset + 5 * 8
        {
            return Ok(stream_len);
        }

        reader.seek(SeekFrom::Start(
            optional_header_start + number_of_directories_offset,
        ))?;
        let mut directory_count = [0u8; 4];
        reader.read_exact(&mut directory_count)?;
        if u32::from_le_bytes(directory_count) < 5 {
            return Ok(stream_len);
        }

        // IMAGE_DIRECTORY_ENTRY_SECURITY is index 4. Unlike the other PE
        // directories, its address is a file offset, not an RVA.
        reader.seek(SeekFrom::Start(
            optional_header_start + directories_offset + 4 * 8,
        ))?;
        let mut security_directory = [0u8; 8];
        reader.read_exact(&mut security_directory)?;
        let certificate_offset =
            u32::from_le_bytes(security_directory[..4].try_into().unwrap()) as u64;
        let certificate_size =
            u32::from_le_bytes(security_directory[4..].try_into().unwrap()) as u64;
        if certificate_offset == 0 && certificate_size == 0 {
            return Ok(stream_len);
        }
        let certificate_end = certificate_offset.checked_add(certificate_size).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Authenticode certificate table range overflows",
            )
        })?;
        if certificate_offset == 0 || certificate_size == 0 || certificate_end != stream_len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "invalid PE Authenticode certificate table range",
            ));
        }
        Ok(certificate_offset)
    }

    /// Discovers and validates the direct overlay table within the host executable.
    pub fn read_manifest<R: Read + Seek>(
        reader: R,
    ) -> std::io::Result<Option<(OverlayManifestHeader, Vec<FileRecordHeader>)>> {
        match Self::read_package(reader)? {
            Some((manifest, records, _options)) => Ok(Some((manifest, records))),
            None => Ok(None),
        }
    }

    /// Like [`OverlayReader::read_manifest`], but also returns the package's
    /// install options. Version 3+ packages carry an explicit options
    /// block (v5 appends the extension block); older packages (any
    /// accepted magic, manifest version 2) yield
    /// [`InstallOptions::defaults_for`] for the recorded app name, and
    /// v3/v4 packages get the extension defaults.
    pub fn read_package<R: Read + Seek>(
        reader: R,
    ) -> std::io::Result<Option<(OverlayManifestHeader, Vec<FileRecordHeader>, InstallOptions)>>
    {
        match Self::read_package_full(reader)? {
            Some((manifest, records, options, _power)) => Ok(Some((manifest, records, options))),
            None => Ok(None),
        }
    }

    /// Like [`OverlayReader::read_package`], but also returns the
    /// package's v6 [`PowerSpec`] (components, hooks, writes, rich
    /// shortcuts). Pre-v6 packages read as an empty spec.
    pub fn read_package_full<R: Read + Seek>(
        reader: R,
    ) -> std::io::Result<Option<PackageFull>> {
        Ok(Self::read_package_with_integrity(reader)?.map(
            |(manifest, records, options, power, _digests)| (manifest, records, options, power),
        ))
    }

    /// Reads package metadata and, for v7+, its ordered SHA-256 table.
    /// Older packages return None for the integrity table.
    pub fn read_package_with_integrity<R: Read + Seek>(
        mut reader: R,
    ) -> std::io::Result<Option<PackageWithIntegrity>> {
        let stream_len = reader.seek(SeekFrom::End(0))?;
        let manifest_size = std::mem::size_of::<OverlayManifestHeader>() as u64;

        if stream_len < manifest_size {
            return Ok(None);
        }

        // Search backward from end or scan sectors
        let mut search_offset = 0u64;
        let mut found = None;

        while search_offset + manifest_size <= stream_len {
            reader.seek(SeekFrom::Start(search_offset))?;
            let mut magic_buf = [0u8; 16];
            if reader.read_exact(&mut magic_buf).is_ok() && is_overlay_magic(&magic_buf) {
                found = Some(search_offset);
                break;
            }
            search_offset += SECTOR_ALIGNMENT;
        }

        let overlay_start = match found {
            Some(o) => o,
            None => return Ok(None),
        };

        reader.seek(SeekFrom::Start(overlay_start))?;
        let mut manifest = OverlayManifestHeader {
            magic: [0u8; 16],
            version: 0,
            file_count: 0,
            primary_exe_index: 0,
            total_uncompressed_bytes: 0,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };

        let manifest_slice = unsafe {
            std::slice::from_raw_parts_mut(
                &mut manifest as *mut OverlayManifestHeader as *mut u8,
                std::mem::size_of::<OverlayManifestHeader>(),
            )
        };
        reader.read_exact(manifest_slice)?;

        let mut records = Vec::with_capacity(manifest.file_count as usize);
        for _ in 0..manifest.file_count {
            let mut record = FileRecordHeader::default();
            let record_slice = unsafe {
                std::slice::from_raw_parts_mut(
                    &mut record as *mut FileRecordHeader as *mut u8,
                    std::mem::size_of::<FileRecordHeader>(),
                )
            };
            reader.read_exact(record_slice)?;
            records.push(record);
        }

        let options = if manifest.version >= 3 {
            let mut base = OptionsDiskBase {
                install_location_utf8: [0u8; 260],
                start_menu_folder_utf8: [0u8; 128],
                file_associations_utf8: [0u8; 128],
                desktop_shortcut: 0,
                start_menu_shortcut: 0,
                path_entry: 0,
                reserved: [0u8; 5],
            };
            let base_slice = unsafe {
                std::slice::from_raw_parts_mut(
                    &mut base as *mut OptionsDiskBase as *mut u8,
                    OPTIONS_BASE_DISK_SIZE,
                )
            };
            reader.read_exact(base_slice)?;
            let mut options = InstallOptions::from_disk_base(&base);
            if manifest.version >= 5 {
                let mut ext = OptionsDiskExt {
                    launch_after_install: 0,
                    launch_after_install_default: 0,
                    start_at_login: 0,
                    start_at_login_default: 0,
                    data_dirs_utf8: [0u8; DATA_DIRS_CAPACITY],
                };
                let ext_slice = unsafe {
                    std::slice::from_raw_parts_mut(
                        &mut ext as *mut OptionsDiskExt as *mut u8,
                        OPTIONS_EXT_DISK_SIZE,
                    )
                };
                reader.read_exact(ext_slice)?;
                options.apply_disk_ext(&ext);
            }
            options
        } else {
            InstallOptions::defaults_for(&manifest.app_name())
        };

        // The v6 power block follows the options blocks: a u32 length
        // prefix + a JSON document. Anything malformed is an honest
        // read error — a v6-or-newer package whose power block cannot be read
        // must not silently install as if it had none.
        let power = if manifest.version >= 6 {
            let mut len_buf = [0u8; 4];
            reader.read_exact(&mut len_buf)?;
            let len = u32::from_le_bytes(len_buf) as u64;
            let here = reader.stream_position()?;
            if len > 64 * 1024 * 1024 || here + len > stream_len {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("power block length {len} is out of bounds"),
                ));
            }
            let mut buf = vec![0u8; len as usize];
            reader.read_exact(&mut buf)?;
            let text = String::from_utf8(buf).map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "power block is not valid UTF-8",
                )
            })?;
            let value = crate::manifest::parse_json(&text).map_err(|e| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    format!("power block is not valid JSON: {e}"),
                )
            })?;
            PowerSpec::from_json_value(&value)
        } else {
            PowerSpec::default()
        };

        let payload_digests = if manifest.version >= 7 {
            let table_start = reader.stream_position()?;
            let table_len = (manifest.file_count as u64)
                .checked_mul(32)
                .ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "payload digest table length overflow",
                    )
                })?;
            let table_end = table_start.checked_add(table_len).ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "payload digest table range overflow",
                )
            })?;
            let payload_end = Self::payload_stream_len(&mut reader)?;
            let first_payload = records
                .iter()
                .map(|record| record.payload_offset)
                .min()
                .unwrap_or(table_end);
            if table_end > payload_end || table_end > first_payload {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "payload digest table overlaps or exceeds payload data",
                ));
            }
            reader.seek(SeekFrom::Start(table_start))?;
            let mut digests = Vec::with_capacity(records.len());
            for _ in &records {
                let mut digest = [0u8; 32];
                reader.read_exact(&mut digest)?;
                digests.push(digest);
            }
            Some(digests)
        } else {
            None
        };

        Ok(Some((manifest, records, options, power, payload_digests)))
    }

    /// The stored (on-disk) length of `records[index]`'s payload.
    /// Payloads are laid out contiguously in record order in every format
    /// version, so a payload runs to the next payload's offset — or to
    /// the end of the stream for the last one. For uncompressed records
    /// this equals `file_size`; for v4 compressed records it is the
    /// compressed size.
    pub fn stored_len(records: &[FileRecordHeader], index: usize, stream_len: u64) -> u64 {
        let start = records[index].payload_offset;
        let mut end = stream_len;
        for record in records {
            if record.payload_offset > start {
                end = end.min(record.payload_offset);
            }
        }
        end.saturating_sub(start)
    }

    /// Reads and decompresses one record's payload, returning the
    /// original file bytes (exactly `record.file_size` of them).
    pub fn read_payload<R: Read + Seek>(
        reader: &mut R,
        records: &[FileRecordHeader],
        index: usize,
        stream_len: u64,
    ) -> std::io::Result<Vec<u8>> {
        let record = &records[index];
        let payload_end = Self::payload_stream_len(reader)?.min(stream_len);
        let stored_len = Self::stored_len(records, index, payload_end);
        reader.seek(SeekFrom::Start(record.payload_offset))?;
        let mut stored = vec![0u8; stored_len as usize];
        reader.read_exact(&mut stored)?;
        crate::codec::decompress(record.compression(), &stored, record.file_size as usize)
    }

    /// Extracts every file recorded in the overlay into `dest_dir`, creating
    /// subdirectories as needed and returning the paths written, in record
    /// order. Record paths are validated before anything is written for that
    /// record: absolute paths and `..` traversal are rejected, so a crafted
    /// overlay cannot write outside `dest_dir`.
    pub fn extract<R: Read + Seek>(reader: R, dest_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        Self::extract_with_control(reader, dest_dir, &mut ExtractControl::default())
    }

    /// Like [`OverlayReader::extract`], with per-file progress and
    /// cancellation: `control.on_file` runs before each file is written,
    /// and a set `control.cancel` flag aborts the extraction between
    /// files with an `Interrupted` error (files already written stay
    /// written — callers roll back).
    pub fn extract_with_control<R: Read + Seek>(
        reader: R,
        dest_dir: &Path,
        control: &mut ExtractControl<'_>,
    ) -> std::io::Result<Vec<PathBuf>> {
        Self::extract_selected_with_control(reader, dest_dir, &|_| true, control)
    }

    /// Like [`OverlayReader::extract_with_control`], but writes only
    /// the records `include` accepts — the component-selective install
    /// path. The progress callback's `(index, total)` counts the
    /// *selected* files, so progress stays honest about what this run
    /// actually writes.
    pub fn extract_selected_with_control<R: Read + Seek>(
        reader: R,
        dest_dir: &Path,
        include: &dyn Fn(&FileRecordHeader) -> bool,
        control: &mut ExtractControl<'_>,
    ) -> std::io::Result<Vec<PathBuf>> {
        let mut reader = reader;
        let (_manifest, records, _options, _power, payload_digests) =
            Self::read_package_with_integrity(&mut reader)?.ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "no SeanPak overlay found in this file",
                )
            })?;
        let stream_len = Self::payload_stream_len(&mut reader)?;
        let selected: Vec<usize> = records
            .iter()
            .enumerate()
            .filter(|(_, record)| include(record))
            .map(|(idx, _)| idx)
            .collect();
        let total = selected.len();

        std::fs::create_dir_all(dest_dir)?;
        let mut written = Vec::with_capacity(total);
        for (seq, idx) in selected.into_iter().enumerate() {
            let record = &records[idx];
            if let Some(cancel) = control.cancel {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "extraction cancelled",
                    ));
                }
            }
            let rel = record.relative_path();
            if let Some(on_file) = control.on_file.as_deref_mut() {
                on_file(seq, total, &rel);
            }
            let target = safe_join(dest_dir, &rel)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = Self::read_payload(&mut reader, &records, idx, stream_len)?;
            if let Some(digests) = &payload_digests {
                let expected = digests.get(idx).ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "payload digest table is incomplete",
                    )
                })?;
                if crate::hash::sha256_digest(&bytes) != *expected {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("payload integrity check failed for {rel}"),
                    ));
                }
            }
            std::fs::write(&target, &bytes)?;
            written.push(target);
        }
        Ok(written)
    }

    /// Convenience wrapper: opens `packed_path` and extracts its overlay.
    pub fn extract_file(packed_path: &Path, dest_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        let file = File::open(packed_path)?;
        Self::extract(file, dest_dir)
    }
}

/// Per-file progress callback for extraction: `(index, total, path)`.
pub type OnExtractFile<'a> = dyn FnMut(usize, usize, &str) + 'a;

/// Progress + cancellation hooks for [`OverlayReader::extract_with_control`].
#[derive(Default)]
pub struct ExtractControl<'a> {
    /// Called as `(index, total, relative_path)` before each file lands.
    pub on_file: Option<&'a mut OnExtractFile<'a>>,
    /// When this flag is set, extraction aborts between files.
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
}

/// Joins a stored relative path onto `dest_dir`, rejecting anything that
/// could escape it (absolute paths, drive/prefix components, `..`).
fn safe_join(dest_dir: &Path, rel: &str) -> std::io::Result<PathBuf> {
    let normalized = rel.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid empty or absolute overlay path: {rel:?}"),
        ));
    }
    let mut out = dest_dir.to_path_buf();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("overlay path escapes destination: {rel:?}"),
            ));
        }
        out.push(part);
    }
    if out == dest_dir {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("overlay path names no file: {rel:?}"),
        ));
    }
    Ok(out)
}

fn copy_str_to_fixed(dest: &mut [u8], src: &str) {
    let bytes = src.as_bytes();
    let len = bytes.len().min(dest.len() - 1);
    dest[..len].copy_from_slice(&bytes[..len]);
    dest[len] = 0;
}

/// Public wrapper around the fixed-field string copy, for code that
/// builds [`InstallOptions`] outside this module (the JSON manifest).
pub fn copy_str_to_fixed_pub(dest: &mut [u8], src: &str) {
    copy_str_to_fixed(dest, src);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn unique_temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "seanpak_overlay_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn test_pe_stub(size: usize) -> Vec<u8> {
        let mut bytes = vec![0u8; size];
        bytes[..2].copy_from_slice(b"MZ");
        let pe_offset = 0x80usize;
        bytes[0x3c..0x40].copy_from_slice(&(pe_offset as u32).to_le_bytes());
        bytes[pe_offset..pe_offset + 4].copy_from_slice(b"PE\0\0");
        bytes[pe_offset + 4 + 16..pe_offset + 4 + 18]
            .copy_from_slice(&0x00f0u16.to_le_bytes());
        let optional = pe_offset + 24;
        bytes[optional..optional + 2].copy_from_slice(&0x020bu16.to_le_bytes());
        bytes[optional + 108..optional + 112].copy_from_slice(&16u32.to_le_bytes());
        bytes
    }

    #[test]
    fn payload_stream_len_excludes_authenticode_certificate_table() {
        let certificate_offset = 0x400u32;
        let certificate_size = 16u32;
        let mut bytes = test_pe_stub(certificate_offset as usize + certificate_size as usize);
        let security_entry = 0x80 + 24 + 112 + 4 * 8;
        bytes[security_entry..security_entry + 4]
            .copy_from_slice(&certificate_offset.to_le_bytes());
        bytes[security_entry + 4..security_entry + 8]
            .copy_from_slice(&certificate_size.to_le_bytes());

        let mut cursor = Cursor::new(bytes);
        assert_eq!(
            OverlayReader::payload_stream_len(&mut cursor).unwrap(),
            certificate_offset as u64
        );

        let mut unsigned = Cursor::new(test_pe_stub(0x500));
        assert_eq!(
            OverlayReader::payload_stream_len(&mut unsigned).unwrap(),
            0x500
        );
    }

    #[test]
    fn extract_reads_payload_before_authenticode_certificate_table() {
        let root = unique_temp("authenticode");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let expected = b"payload remains readable after signing";
        std::fs::write(src.join("app.exe"), expected).unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, test_pe_stub(5000)).unwrap();
        let package = root.join("signed-setup.exe");
        OverlayWriter::build_from_folder(
            &stub,
            &package,
            &src,
            "Signed Test",
            "1.0.0",
            "Test Publisher",
        )
        .unwrap();

        let mut image = std::fs::read(&package).unwrap();
        let certificate_offset = u32::try_from(image.len()).unwrap();
        let certificate_size = 16u32;
        let security_entry = 0x80 + 24 + 112 + 4 * 8;
        image[security_entry..security_entry + 4]
            .copy_from_slice(&certificate_offset.to_le_bytes());
        image[security_entry + 4..security_entry + 8]
            .copy_from_slice(&certificate_size.to_le_bytes());
        image.extend_from_slice(&[0u8; 16]);
        std::fs::write(&package, image).unwrap();

        let mut file = std::fs::File::open(&package).unwrap();
        assert_eq!(
            OverlayReader::payload_stream_len(&mut file).unwrap(),
            certificate_offset as u64
        );
        let destination = root.join("extracted");
        OverlayReader::extract_file(&package, &destination).unwrap();
        assert_eq!(std::fs::read(destination.join("app.exe")).unwrap(), expected.to_vec());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn roundtrip_pack_read_extract() {
        let root = unique_temp("roundtrip");
        let src = root.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("app.exe"), b"fake-pe-bytes").unwrap();
        std::fs::write(src.join("sub/data.bin"), vec![7u8; 5000]).unwrap();

        // A stub of non-sector-multiple size exercises the alignment padding.
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xAAu8; 5000]).unwrap();
        let out = root.join("Setup.exe");

        let total = OverlayWriter::build_from_folder(
            &stub,
            &out,
            &src,
            "Test App",
            "1.2.3.4",
            "Test Publisher",
        )
        .unwrap();
        assert_eq!(total, std::fs::metadata(&out).unwrap().len());
        // Overlay start is 4096-aligned: 5000-byte stub pads up to 8192.
        assert!(total > 8192);

        let file = File::open(&out).unwrap();
        let (manifest, records) = OverlayReader::read_manifest(file)
            .unwrap()
            .expect("overlay must be discoverable");
        assert_eq!(manifest.app_name(), "Test App");
        assert_eq!(manifest.app_version(), "1.2.3.4");
        assert_eq!(manifest.publisher(), "Test Publisher");
        assert_eq!(manifest.file_count, 2);
        assert_eq!(records.len(), 2);
        let primary = &records[manifest.primary_exe_index as usize];
        assert_eq!(primary.relative_path(), "app.exe");

        let dest = root.join("extracted");
        let written = OverlayReader::extract_file(&out, &dest).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(
            std::fs::read(dest.join("app.exe")).unwrap(),
            b"fake-pe-bytes"
        );
        assert_eq!(
            std::fs::read(dest.join("sub/data.bin")).unwrap(),
            vec![7u8; 5000]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_manifest_on_plain_file_is_none() {
        let cursor = Cursor::new(vec![0u8; 1024]);
        assert!(OverlayReader::read_manifest(cursor).unwrap().is_none());
    }

    /// Hand-builds a manifest-version-2 package (no options block) carrying
    /// `magic`, holding one file `old.txt` with `payload` as its content.
    fn hand_built_v2_package(magic: &[u8; 16], app_name: &str, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; SECTOR_ALIGNMENT as usize];
        let mut manifest = OverlayManifestHeader {
            magic: *magic,
            version: 2,
            file_count: 1,
            primary_exe_index: 0,
            total_uncompressed_bytes: payload.len() as u64,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };
        copy_str_to_fixed(&mut manifest.app_name_utf8, app_name);
        let header_size = std::mem::size_of::<OverlayManifestHeader>();
        let record_size = std::mem::size_of::<FileRecordHeader>();
        let mut record = FileRecordHeader {
            file_size: payload.len() as u64,
            payload_offset: SECTOR_ALIGNMENT + (header_size + record_size) as u64,
            ..Default::default()
        };
        copy_str_to_fixed(&mut record.relative_path_utf8, "old.txt");
        let manifest_bytes = unsafe {
            std::slice::from_raw_parts(
                &manifest as *const OverlayManifestHeader as *const u8,
                header_size,
            )
        };
        bytes.extend_from_slice(manifest_bytes);
        let record_bytes = unsafe {
            std::slice::from_raw_parts(&record as *const FileRecordHeader as *const u8, record_size)
        };
        bytes.extend_from_slice(record_bytes);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn assert_legacy_package_readable(magic: &[u8; 16], tag: &str) {
        // Packages built before the SeanPak rename carry a legacy magic and
        // no options block; they must stay listable, extractable, and yield
        // default install options.
        let root = unique_temp(tag);
        let payload = b"legacy-bytes";
        let bytes = hand_built_v2_package(magic, "Legacy App", payload);

        let (read_back, records, options) = OverlayReader::read_package(Cursor::new(bytes.clone()))
            .unwrap()
            .expect("legacy overlay must be discoverable");
        assert_eq!(read_back.app_name(), "Legacy App");
        assert_eq!(records.len(), 1);
        // No recorded location: the synthesized default is the
        // v5.1.0 per-user token path (resolved at install time).
        assert_eq!(
            options.install_location(),
            "%LOCALAPPDATA%\\Programs\\Legacy App"
        );
        assert_eq!(options.start_menu_folder(), "Legacy App");
        assert!(options.wants_desktop_shortcut());
        assert!(options.wants_start_menu_shortcut());
        assert!(!options.wants_path_entry());
        // Pre-v5 packages get the extension defaults.
        assert!(options.offers_launch_after_install());
        assert!(options.launch_after_install_default_checked());
        assert!(!options.offers_start_at_login());
        assert!(options.data_dirs().is_empty());

        let dest = root.join("dest");
        let written = OverlayReader::extract(Cursor::new(bytes), &dest).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(std::fs::read(dest.join("old.txt")).unwrap(), payload);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn version_four_packages_read_with_extension_defaults() {
        // A hand-built v4 package: the base options block only, with the
        // payload offset accounting for the base block alone. The reader
        // must supply the v5 extension defaults and still find the payload.
        let root = unique_temp("v4pkg");
        let payload = b"v4 payload bytes";
        let header_size = std::mem::size_of::<OverlayManifestHeader>();
        let record_size = std::mem::size_of::<FileRecordHeader>();
        let mut bytes = vec![0u8; SECTOR_ALIGNMENT as usize];
        let mut manifest = OverlayManifestHeader {
            magic: *OVERLAY_MAGIC,
            version: 4,
            file_count: 1,
            primary_exe_index: 0,
            total_uncompressed_bytes: payload.len() as u64,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };
        copy_str_to_fixed(&mut manifest.app_name_utf8, "V4 App");
        let mut record = FileRecordHeader {
            file_size: payload.len() as u64,
            payload_offset: SECTOR_ALIGNMENT
                + (header_size + record_size + OPTIONS_BASE_DISK_SIZE) as u64,
            ..Default::default()
        };
        copy_str_to_fixed(&mut record.relative_path_utf8, "v4.txt");
        let manifest_bytes = unsafe {
            std::slice::from_raw_parts(
                &manifest as *const OverlayManifestHeader as *const u8,
                header_size,
            )
        };
        bytes.extend_from_slice(manifest_bytes);
        let record_bytes = unsafe {
            std::slice::from_raw_parts(&record as *const FileRecordHeader as *const u8, record_size)
        };
        bytes.extend_from_slice(record_bytes);
        let mut options = InstallOptions::defaults_for("V4 App");
        options.path_entry = 1;
        let base = options.disk_base();
        let base_bytes = unsafe {
            std::slice::from_raw_parts(
                &base as *const OptionsDiskBase as *const u8,
                OPTIONS_BASE_DISK_SIZE,
            )
        };
        bytes.extend_from_slice(base_bytes);
        bytes.extend_from_slice(payload);

        let (read_back, records, read_options) =
            OverlayReader::read_package(Cursor::new(bytes.clone()))
                .unwrap()
                .expect("v4 overlay must be discoverable");
        assert_eq!(read_back.version, 4);
        assert_eq!(records.len(), 1);
        assert!(read_options.wants_path_entry());
        assert!(read_options.offers_launch_after_install());
        assert!(read_options.launch_after_install_default_checked());
        assert!(!read_options.offers_start_at_login());
        assert!(read_options.data_dirs().is_empty());

        let dest = root.join("dest");
        OverlayReader::extract(Cursor::new(bytes), &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("v4.txt")).unwrap(), payload);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_manifest_accepts_legacy_rustpak_magic() {
        assert_legacy_package_readable(LEGACY_OVERLAY_MAGIC, "legacy_rustpak");
    }

    #[test]
    fn read_manifest_accepts_legacy_cruciblekit_magic() {
        assert_legacy_package_readable(CRUCIBLEKIT_OVERLAY_MAGIC, "legacy_cruciblekit");
    }

    #[test]
    fn new_packages_carry_seanpak_magic_and_roundtrip_options() {
        let root = unique_temp("options");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.exe"), b"fake-pe-bytes").unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xAAu8; 5000]).unwrap();
        let out = root.join("Setup.exe");

        let mut options = InstallOptions::defaults_for("Opt App");
        copy_str_to_fixed(&mut options.install_location_utf8, "C:\\Apps\\Opt App");
        copy_str_to_fixed(&mut options.file_associations_utf8, ".txt,.md");
        options.desktop_shortcut = 0;
        options.path_entry = 1;
        options.launch_after_install = 0;
        options.start_at_login = 1;
        options.start_at_login_default = 1;
        copy_str_to_fixed(
            &mut options.data_dirs_utf8,
            "%APPDATA%\\Opt App;%INSTALLDIR%\\data",
        );

        OverlayWriter::build_from_folder_with_options(
            &stub,
            &out,
            &src,
            "Opt App",
            "2.0.0",
            "Test Publisher",
            &options,
        )
        .unwrap();

        // The bytes at the aligned overlay start are the new SeanPak magic.
        let raw = std::fs::read(&out).unwrap();
        let aligned = 8192usize; // 5000-byte stub pads up to 2 * 4096
        assert_eq!(&raw[aligned..aligned + 16], &OVERLAY_MAGIC[..]);

        let file = File::open(&out).unwrap();
        let (manifest, records, read_back) = OverlayReader::read_package(file)
            .unwrap()
            .expect("new overlay must be discoverable");
        assert_eq!(manifest.version, 7);
        assert_eq!(manifest.app_name(), "Opt App");
        assert_eq!(records.len(), 1);
        assert_eq!(read_back.install_location(), "C:\\Apps\\Opt App");
        assert_eq!(read_back.start_menu_folder(), "Opt App");
        assert_eq!(read_back.file_associations(), ".txt,.md");
        assert!(!read_back.wants_desktop_shortcut());
        assert!(read_back.wants_start_menu_shortcut());
        assert!(read_back.wants_path_entry());
        // The v5 extension fields round-trip too.
        assert!(!read_back.offers_launch_after_install());
        assert!(read_back.offers_start_at_login());
        assert!(read_back.start_at_login_chosen());
        assert_eq!(
            read_back.data_dirs(),
            vec![
                "%APPDATA%\\Opt App".to_string(),
                "%INSTALLDIR%\\data".to_string()
            ]
        );

        let dest = root.join("extracted");
        let written = OverlayReader::extract_file(&out, &dest).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(
            std::fs::read(dest.join("app.exe")).unwrap(),
            b"fake-pe-bytes"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extract_rejects_payload_digest_mismatch_before_writing() {
        let root = unique_temp("payload-integrity");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.bin"), b"x").unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xAAu8; 5000]).unwrap();
        let package = root.join("setup.exe");
        OverlayWriter::build_from_folder(&stub, &package, &src, "Integrity", "1.0.0", "Tester")
            .unwrap();

        let (manifest, records, _, _, digests) =
            OverlayReader::read_package_with_integrity(File::open(&package).unwrap())
                .unwrap()
                .unwrap();
        assert_eq!(manifest.version, 7);
        assert_eq!(digests.as_ref().map(Vec::len), Some(records.len()));
        assert_eq!(
            digests.as_ref().unwrap()[0],
            crate::hash::sha256_digest(b"x")
        );
        assert_eq!(records[0].compression(), crate::codec::CODEC_STORED);

        let mut image = std::fs::read(&package).unwrap();
        image[records[0].payload_offset as usize] ^= 1;
        std::fs::write(&package, image).unwrap();

        let dest = root.join("extracted");
        let error = OverlayReader::extract_file(&package, &dest).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("payload integrity check failed"));
        assert!(!dest.join("app.bin").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extract_rejects_traversal_records() {
        // Hand-build a packed file whose single record tries to escape.
        let root = unique_temp("traversal");
        let payload = b"evil";
        let mut bytes = vec![0u8; SECTOR_ALIGNMENT as usize];
        let mut manifest = OverlayManifestHeader {
            magic: *OVERLAY_MAGIC,
            version: 2,
            file_count: 1,
            primary_exe_index: 0,
            total_uncompressed_bytes: payload.len() as u64,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };
        copy_str_to_fixed(&mut manifest.app_name_utf8, "Evil");
        let header_size = std::mem::size_of::<OverlayManifestHeader>();
        let record_size = std::mem::size_of::<FileRecordHeader>();
        let mut record = FileRecordHeader {
            file_size: payload.len() as u64,
            payload_offset: SECTOR_ALIGNMENT + (header_size + record_size) as u64,
            ..Default::default()
        };
        copy_str_to_fixed(&mut record.relative_path_utf8, "../evil.txt");
        let manifest_bytes = unsafe {
            std::slice::from_raw_parts(
                &manifest as *const OverlayManifestHeader as *const u8,
                header_size,
            )
        };
        bytes.extend_from_slice(manifest_bytes);
        let record_bytes = unsafe {
            std::slice::from_raw_parts(&record as *const FileRecordHeader as *const u8, record_size)
        };
        bytes.extend_from_slice(record_bytes);
        bytes.extend_from_slice(payload);

        let dest = root.join("dest");
        let err = OverlayReader::extract(Cursor::new(bytes), &dest).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(!root.join("evil.txt").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn v4_packages_compress_and_round_trip() {
        let root = unique_temp("v4compress");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut payload = Vec::new();
        for i in 0..50_000u32 {
            payload.extend_from_slice(format!("line {i} of a very repetitive file\n").as_bytes());
            if payload.len() > 200_000 {
                break;
            }
        }
        std::fs::write(src.join("big.txt"), &payload).unwrap();
        std::fs::write(src.join("tiny.bin"), b"\x01\x02\x03").unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xAAu8; 5000]).unwrap();
        let out = root.join("Setup.exe");

        OverlayWriter::build_from_folder(&stub, &out, &src, "Zip App", "1.0.0", "Tester").unwrap();
        let file = File::open(&out).unwrap();
        let (manifest, records) = OverlayReader::read_manifest(file).unwrap().unwrap();
        assert_eq!(manifest.version, 7);
        let big = records
            .iter()
            .find(|r| r.relative_path() == "big.txt")
            .unwrap();
        assert_ne!(big.compression(), crate::codec::CODEC_STORED);
        assert_eq!(big.file_size, payload.len() as u64);
        // The whole package is smaller than stub + raw payloads would be.
        let package_len = std::fs::metadata(&out).unwrap().len();
        assert!(package_len < 8192 + payload.len() as u64);

        let dest = root.join("extracted");
        OverlayReader::extract_file(&out, &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("big.txt")).unwrap(), payload);
        assert_eq!(
            std::fs::read(dest.join("tiny.bin")).unwrap(),
            b"\x01\x02\x03"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    fn record_named(rel: &str, size: u64) -> FileRecordHeader {
        let mut record = FileRecordHeader {
            file_size: size,
            ..Default::default()
        };
        copy_str_to_fixed(&mut record.relative_path_utf8, rel);
        record
    }

    fn sample_power() -> PowerSpec {
        PowerSpec {
            components: vec![
                ComponentSpec {
                    id: "extras".to_string(),
                    name: "Extras".to_string(),
                    description: "Optional extras".to_string(),
                    files: vec!["extras/big.bin".to_string()],
                    required: false,
                    default_selected: false,
                    packed_size: 0,
                },
                ComponentSpec {
                    id: "engine".to_string(),
                    name: "Engine".to_string(),
                    description: String::new(),
                    files: vec!["engine.dll".to_string()],
                    required: true,
                    default_selected: true,
                    packed_size: 0,
                },
            ],
            pre_hooks: vec![HookSpec {
                command: "prep.exe".to_string(),
                args: vec!["--go".to_string()],
                on_failure: HookFailurePolicy::Continue,
            }],
            post_hooks: vec![HookSpec {
                command: "finish.exe".to_string(),
                args: Vec::new(),
                on_failure: HookFailurePolicy::Abort,
            }],
            registry_writes: vec![RegWriteSpec {
                key: "Software\\Power App".to_string(),
                name: "Mode".to_string(),
                kind: RegValueKind::Dword,
                value: "7".to_string(),
            }],
            env_writes: vec![EnvWriteSpec {
                name: "POWER_APP_HOME".to_string(),
                value: "C:\\Power".to_string(),
            }],
            shortcuts: vec![RichShortcutSpec {
                name: "Power Console".to_string(),
                target: "app.exe".to_string(),
                arguments: "--console".to_string(),
                icon: None,
                icon_index: 0,
                description: "Opens the console".to_string(),
                location: ShortcutLocation::Both,
                pin: true,
            }],
        }
    }

    #[test]
    fn power_block_round_trips_with_packed_sizes() {
        let root = unique_temp("power");
        let src = root.join("src");
        std::fs::create_dir_all(src.join("extras")).unwrap();
        std::fs::write(src.join("app.exe"), b"fake-pe-bytes").unwrap();
        std::fs::write(src.join("engine.dll"), b"engine-bytes").unwrap();
        std::fs::write(src.join("extras/big.bin"), vec![9u8; 20_000]).unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xAAu8; 5000]).unwrap();
        let out = root.join("Setup.exe");

        let options = InstallOptions::defaults_for("Power App");
        let files: Vec<(PathBuf, String, bool)> = vec![
            (src.join("app.exe"), "app.exe".to_string(), true),
            (src.join("engine.dll"), "engine.dll".to_string(), false),
            (
                src.join("extras/big.bin"),
                "extras/big.bin".to_string(),
                false,
            ),
        ];
        let (_total, stored_power) = OverlayWriter::build_overlay_with_power(
            &stub,
            &out,
            &src,
            &files,
            "Power App",
            "3.0.0",
            "Tester",
            &options,
            &sample_power(),
        )
        .unwrap();
        // Packed sizes are filled by the packer's own codec: big.bin is
        // highly compressible, so its packed size is far below 20,000.
        let extras = stored_power.component("extras").unwrap();
        assert!(extras.packed_size > 0 && extras.packed_size < 20_000);
        assert_eq!(
            stored_power.component("engine").unwrap().packed_size,
            12 // "engine-bytes" stores uncompressed or near it
        );

        let file = File::open(&out).unwrap();
        let (manifest, records, _options, power) = OverlayReader::read_package_full(file)
            .unwrap()
            .expect("v6 overlay must be discoverable");
        assert_eq!(manifest.version, 7);
        assert_eq!(records.len(), 3);
        assert_eq!(power, stored_power);
        assert_eq!(power.component_id_for_file("app.exe"), "core");
        assert_eq!(power.component_id_for_file("engine.dll"), "engine");
        assert_eq!(
            power.component_id_for_file("extras\\big.bin"),
            "extras",
            "backslash paths resolve too"
        );

        // Payloads still extract byte-for-byte past the power block.
        let dest = root.join("extracted");
        let written = OverlayReader::extract_file(&out, &dest).unwrap();
        assert_eq!(written.len(), 3);
        assert_eq!(
            std::fs::read(dest.join("extras/big.bin")).unwrap(),
            vec![9u8; 20_000]
        );

        // Selective extraction writes only the chosen component's files.
        let dest2 = root.join("selected");
        let mut control = ExtractControl::default();
        let only_extras =
            |r: &FileRecordHeader| power.component_id_for_file(&r.relative_path()) == "extras";
        let written = OverlayReader::extract_selected_with_control(
            File::open(&out).unwrap(),
            &dest2,
            &only_extras,
            &mut control,
        )
        .unwrap();
        assert_eq!(written.len(), 1);
        assert!(dest2.join("extras/big.bin").exists());
        assert!(!dest2.join("app.exe").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn version_five_packages_read_with_empty_power() {
        // A hand-built v5 package: base + extension options blocks, no
        // power block. The reader must supply an empty power spec and
        // still find the payload.
        let root = unique_temp("v5pkg");
        let payload = b"v5 payload bytes";
        let header_size = std::mem::size_of::<OverlayManifestHeader>();
        let record_size = std::mem::size_of::<FileRecordHeader>();
        let mut bytes = vec![0u8; SECTOR_ALIGNMENT as usize];
        let mut manifest = OverlayManifestHeader {
            magic: *OVERLAY_MAGIC,
            version: 5,
            file_count: 1,
            primary_exe_index: 0,
            total_uncompressed_bytes: payload.len() as u64,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };
        copy_str_to_fixed(&mut manifest.app_name_utf8, "V5 App");
        let mut record = FileRecordHeader {
            file_size: payload.len() as u64,
            payload_offset: SECTOR_ALIGNMENT
                + (header_size + record_size + OPTIONS_BASE_DISK_SIZE + OPTIONS_EXT_DISK_SIZE)
                    as u64,
            ..Default::default()
        };
        copy_str_to_fixed(&mut record.relative_path_utf8, "v5.txt");
        let manifest_bytes = unsafe {
            std::slice::from_raw_parts(
                &manifest as *const OverlayManifestHeader as *const u8,
                header_size,
            )
        };
        bytes.extend_from_slice(manifest_bytes);
        let record_bytes = unsafe {
            std::slice::from_raw_parts(&record as *const FileRecordHeader as *const u8, record_size)
        };
        bytes.extend_from_slice(record_bytes);
        let options = InstallOptions::defaults_for("V5 App");
        let base = options.disk_base();
        let base_bytes = unsafe {
            std::slice::from_raw_parts(
                &base as *const OptionsDiskBase as *const u8,
                OPTIONS_BASE_DISK_SIZE,
            )
        };
        bytes.extend_from_slice(base_bytes);
        let ext = options.disk_ext();
        let ext_bytes = unsafe {
            std::slice::from_raw_parts(
                &ext as *const OptionsDiskExt as *const u8,
                OPTIONS_EXT_DISK_SIZE,
            )
        };
        bytes.extend_from_slice(ext_bytes);
        bytes.extend_from_slice(payload);

        let (read_back, records, _opts, power, digests) =
            OverlayReader::read_package_with_integrity(Cursor::new(bytes.clone()))
                .unwrap()
                .expect("v5 overlay must be discoverable");
        assert_eq!(read_back.version, 5);
        assert!(digests.is_none());
        assert_eq!(records.len(), 1);
        assert!(power.is_empty());

        let dest = root.join("dest");
        OverlayReader::extract(Cursor::new(bytes), &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("v5.txt")).unwrap(), payload);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn version_six_packages_read_without_integrity_table() {
        let root = unique_temp("v6pkg");
        let payload = b"v6 payload bytes";
        let header_size = std::mem::size_of::<OverlayManifestHeader>();
        let record_size = std::mem::size_of::<FileRecordHeader>();
        let mut bytes = vec![0u8; SECTOR_ALIGNMENT as usize];
        let mut manifest = OverlayManifestHeader {
            magic: *OVERLAY_MAGIC,
            version: 6,
            file_count: 1,
            primary_exe_index: 0,
            total_uncompressed_bytes: payload.len() as u64,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };
        copy_str_to_fixed(&mut manifest.app_name_utf8, "V6 App");
        let power_json = PowerSpec::default().to_json_value().render();
        let metadata_size = header_size
            + record_size
            + OPTIONS_BASE_DISK_SIZE
            + OPTIONS_EXT_DISK_SIZE
            + 4
            + power_json.len();
        let mut record = FileRecordHeader {
            file_size: payload.len() as u64,
            payload_offset: SECTOR_ALIGNMENT + metadata_size as u64,
            ..Default::default()
        };
        copy_str_to_fixed(&mut record.relative_path_utf8, "v6.txt");

        let manifest_bytes = unsafe {
            std::slice::from_raw_parts(
                &manifest as *const OverlayManifestHeader as *const u8,
                header_size,
            )
        };
        bytes.extend_from_slice(manifest_bytes);
        let record_bytes = unsafe {
            std::slice::from_raw_parts(&record as *const FileRecordHeader as *const u8, record_size)
        };
        bytes.extend_from_slice(record_bytes);
        let options = InstallOptions::defaults_for("V6 App");
        let base = options.disk_base();
        let base_bytes = unsafe {
            std::slice::from_raw_parts(
                &base as *const OptionsDiskBase as *const u8,
                OPTIONS_BASE_DISK_SIZE,
            )
        };
        bytes.extend_from_slice(base_bytes);
        let ext = options.disk_ext();
        let ext_bytes = unsafe {
            std::slice::from_raw_parts(
                &ext as *const OptionsDiskExt as *const u8,
                OPTIONS_EXT_DISK_SIZE,
            )
        };
        bytes.extend_from_slice(ext_bytes);
        bytes.extend_from_slice(&(power_json.len() as u32).to_le_bytes());
        bytes.extend_from_slice(power_json.as_bytes());
        bytes.extend_from_slice(payload);

        let (read_back, records, _, power, digests) =
            OverlayReader::read_package_with_integrity(Cursor::new(bytes.clone()))
                .unwrap()
                .expect("v6 overlay must be discoverable");
        assert_eq!(read_back.version, 6);
        assert_eq!(records.len(), 1);
        assert!(power.is_empty());
        assert!(digests.is_none());

        let dest = root.join("dest");
        OverlayReader::extract(Cursor::new(bytes), &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("v6.txt")).unwrap(), payload);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn selection_resolution_rules() {
        let power = sample_power();
        let records = vec![
            record_named("app.exe", 10),
            record_named("engine.dll", 10),
            record_named("extras/big.bin", 10),
        ];
        let primary = Some("app.exe");

        // Defaults: core + required engine; extras defaults off.
        let sel = power
            .resolve_selection(None, &[], None, &records, primary)
            .unwrap();
        assert_eq!(sel, vec!["core".to_string(), "engine".to_string()]);

        // Explicit selection adds extras; unknown ids are errors.
        let sel = power
            .resolve_selection(Some(&["extras".to_string()]), &[], None, &records, primary)
            .unwrap();
        assert_eq!(
            sel,
            vec![
                "core".to_string(),
                "extras".to_string(),
                "engine".to_string()
            ]
        );
        assert!(power
            .resolve_selection(Some(&["nope".to_string()]), &[], None, &records, primary)
            .is_err());

        // --without: a default-on optional would drop; required, the
        // primary's component, and core refuse.
        let mut power2 = sample_power();
        power2.components[0].default_selected = true;
        let sel = power2
            .resolve_selection(None, &["extras".to_string()], None, &records, primary)
            .unwrap();
        assert_eq!(sel, vec!["core".to_string(), "engine".to_string()]);
        assert!(power2
            .resolve_selection(None, &["engine".to_string()], None, &records, primary)
            .is_err());
        assert!(power2
            .resolve_selection(None, &["core".to_string()], None, &records, primary)
            .is_err());

        // The primary executable's component is force-included even
        // when it is optional and unselected.
        let mut power3 = PowerSpec::default();
        power3.components.push(ComponentSpec {
            id: "main".to_string(),
            name: "Main".to_string(),
            description: String::new(),
            files: vec!["app.exe".to_string()],
            required: false,
            default_selected: false,
            packed_size: 0,
        });
        let records3 = vec![record_named("app.exe", 10), record_named("loose.txt", 10)];
        let sel = power3
            .resolve_selection(None, &[], None, &records3, primary)
            .unwrap();
        assert_eq!(sel, vec!["core".to_string(), "main".to_string()]);
        assert!(power3
            .resolve_selection(None, &["main".to_string()], None, &records3, primary)
            .is_err());

        // Upgrade: a component the previous package knew keeps its
        // in/out state; a brand-new one takes its manifest default.
        let installed = vec!["core".to_string(), "engine".to_string()];
        let known = vec![
            "core".to_string(),
            "engine".to_string(),
            "extras".to_string(),
        ];
        let sel = power
            .resolve_selection(None, &[], Some((&installed, &known)), &records, primary)
            .unwrap();
        assert_eq!(sel, vec!["core".to_string(), "engine".to_string()]);
        let known_without_extras = vec!["core".to_string(), "engine".to_string()];
        let mut power4 = sample_power();
        power4.components[0].default_selected = true;
        let sel = power4
            .resolve_selection(
                None,
                &[],
                Some((&installed, &known_without_extras)),
                &records,
                primary,
            )
            .unwrap();
        assert!(sel.contains(&"extras".to_string()));
    }

    #[test]
    fn safe_join_accepts_nested_and_rejects_escapes() {
        let base = Path::new("/tmp/base");
        assert_eq!(
            safe_join(base, "sub/file.txt").unwrap(),
            Path::new("/tmp/base/sub/file.txt")
        );
        assert_eq!(
            safe_join(base, "sub\\file.txt").unwrap(),
            Path::new("/tmp/base/sub/file.txt")
        );
        for bad in ["../x", "a/../../x", "/abs", "C:/win", "", "."] {
            assert!(safe_join(base, bad).is_err(), "must reject {bad:?}");
        }
    }
}
