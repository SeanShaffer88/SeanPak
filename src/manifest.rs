//! The SeanPak app manifest — the JSON document an author (human or AI)
//! writes to describe an app to package.
//!
//! `seanpak pack` consumes it, `seanpak schema` prints the JSON Schema
//! for it ([`MANIFEST_SCHEMA_JSON`]), and `seanpak validate` checks a
//! manifest against the same rules used here, so a manifest that passes
//! `validate` is a manifest `pack` will accept. The manifest feeds the
//! overlay's install-options block (see `overlay::InstallOptions`);
//! packages written before the JSON manifest existed (RustPak/CrucibleKit
//! era, manifest versions 1–2) keep reading with default options.
//!
//! The JSON parser is hand-rolled: SeanPak's dependency rule is
//! windows-sys only, so serde is not an option.

use crate::overlay::{
    copy_str_to_fixed_pub, ComponentSpec, EnvWriteSpec, HookFailurePolicy, HookSpec,
    InstallOptions, PowerSpec, RegValueKind, RegWriteSpec, RichShortcutSpec, ShortcutLocation,
    CORE_COMPONENT_ID, DATA_DIRS_CAPACITY,
};

/// The JSON Schema (draft 2020-12) for the app manifest. This exact text
/// is what `seanpak schema` prints.
pub const MANIFEST_SCHEMA_JSON: &str = r##"{
  "$schema": "https://json-schema.org/draft/2020-12/schema",
  "title": "SeanPak App Manifest",
  "description": "Describes one app for `seanpak pack`. Asset file names are derived deterministically: <appName, stripped of spaces and characters outside [A-Za-z0-9._-]>-v<version>-x64-Setup.exe and -Portable.zip.",
  "type": "object",
  "additionalProperties": false,
  "required": ["appName", "version"],
  "properties": {
    "appName": {
      "type": "string",
      "minLength": 1,
      "description": "Display name of the app. Must not contain any of <>:\"/\\|?* — it becomes part of file names and the uninstall entry."
    },
    "version": {
      "type": "string",
      "pattern": "^[0-9][0-9A-Za-z.-]*$",
      "description": "App version, starting with a digit (for example \"1.4.0\" or \"0.7.2-beta.1\"). Used verbatim in asset file names after a 'v'."
    },
    "publisher": {
      "type": "string",
      "default": "",
      "description": "Publisher name recorded in the package and shown by the installer and Windows Settings."
    },
    "primaryExe": {
      "type": "string",
      "description": "Path of the app's main executable relative to the files directory, ending in .exe. Defaults to the first .exe found in the files directory."
    },
    "installLocation": {
      "type": "string",
      "description": "Absolute Windows install path proposed by the installer (for example \"C:\\Program Files\\MyApp\"). Defaults to C:\\Program Files\\<appName>."
    },
    "startMenuFolder": {
      "type": "string",
      "description": "Start Menu folder name recorded for the install. Defaults to appName. A plain name, no path separators."
    },
    "desktopShortcut": {
      "type": "boolean",
      "default": true,
      "description": "Install option: create a desktop shortcut at install time (journaled; removed by uninstall)."
    },
    "startMenuShortcut": {
      "type": "boolean",
      "default": true,
      "description": "Recorded install option: create Start Menu shortcuts."
    },
    "fileAssociations": {
      "type": "array",
      "items": { "type": "string" },
      "default": [],
      "description": "File extensions to associate with the app (with or without a leading dot, for example \".txt\" or \"md\"). Registered per-user (HKCU) at install time and removed by uninstall only while the association still points at this app."
    },
    "pathEntry": {
      "type": "boolean",
      "default": false,
      "description": "Install option: add the install folder to the per-user PATH at install time (appended only if absent; uninstall removes exactly that segment)."
    },
    "launchAfterInstall": {
      "anyOf": [
        { "type": "boolean" },
        { "type": "object", "properties": { "offered": { "type": "boolean" }, "defaultChecked": { "type": "boolean" } }, "additionalProperties": false }
      ],
      "default": { "offered": true, "defaultChecked": true },
      "description": "Whether the installer's Finish page offers a \"Launch <appName>\" checkbox, and whether that checkbox starts checked. A plain boolean is shorthand: true = offered and checked, false = not offered. Only shown when the package has a primary executable."
    },
    "startAtLogin": {
      "anyOf": [
        { "type": "boolean" },
        { "type": "object", "properties": { "offered": { "type": "boolean" }, "defaultChecked": { "type": "boolean" } }, "additionalProperties": false }
      ],
      "default": { "offered": false, "defaultChecked": false },
      "description": "Whether the installer offers \"Start <appName> at login\": a per-user HKCU Run entry pointing at the primary executable, journaled like the other artifacts and removed by uninstall (only while it still points at this app). A plain boolean is shorthand: true = offered and on, false = not offered."
    },
    "dataDirs": {
      "type": "array",
      "items": { "type": "string" },
      "default": [],
      "description": "The app's data folders, recorded (resolved to absolute paths) in the install receipt. The interactive uninstall offers a checkbox to delete them; a silent uninstall never deletes them without an explicit --purge-data. Each entry starts with %APPDATA%, %LOCALAPPDATA%, %USERPROFILE% or %INSTALLDIR%, or is a path relative to the install folder; each must name a folder below its root and must not contain '..' or ';'."
    },
    "components": {
      "type": "array",
      "default": [],
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["id", "name"],
        "properties": {
          "id": { "type": "string", "description": "Unique component id: letters, digits, '-' and '_' only. The id 'core' is reserved for the files no component claims." },
          "name": { "type": "string", "description": "Display name shown in the installer's component choices." },
          "description": { "type": "string", "default": "" },
          "files": { "type": "array", "items": { "type": "string" }, "default": [], "description": "Package files this component claims (paths relative to the files directory). A file may be claimed by only one component; unclaimed files form the always-installed 'core' component." },
          "required": { "type": "boolean", "default": false, "description": "Required components are always installed and cannot be deselected." },
          "defaultSelected": { "type": "boolean", "default": true, "description": "Whether an optional component starts selected on a fresh install. Upgrades keep the previous install's choice for components the previous package also offered." }
        }
      },
      "description": "Optional components: named subsets of the package the user can include or skip at install time (wizard Options page checkboxes, or --components / --without on the command line). The component holding the primary executable is always installed."
    },
    "hooks": {
      "type": "object",
      "additionalProperties": false,
      "default": {},
      "properties": {
        "preInstall": { "type": "array", "items": { "$ref": "#/$defs/hook" }, "default": [], "description": "Commands run after the install location passes the writability check, before any file is extracted. Working directory: the folder the installer was run from." },
        "postInstall": { "type": "array", "items": { "$ref": "#/$defs/hook" }, "default": [], "description": "Commands run after files, shortcuts and writes are applied, before the install is finalized. Working directory: the install folder. On a --repair run only postInstall hooks run." }
      },
      "description": "Pre/post install commands. Each run is recorded in the install receipt; hooks receive SEANPAK_INSTALL_DIR, SEANPAK_APP_NAME and SEANPAK_APP_VERSION in their environment. A hook's own changes are never rolled back — only SeanPak's are."
    },
    "registryWrites": {
      "type": "array",
      "default": [],
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["key", "value"],
        "properties": {
          "key": { "type": "string", "description": "Registry key path relative to HKEY_CURRENT_USER (for example \"Software\\\\MyApp\"). Absolute hives are rejected — writes are per-user only." },
          "name": { "type": "string", "default": "", "description": "Value name; an empty string writes the key's default value." },
          "type": { "type": "string", "enum": ["sz", "dword"], "default": "sz" },
          "value": { "description": "The value to write: a string for type sz; for type dword, an integer or its decimal/0x-prefixed string form." }
        }
      },
      "description": "Per-user (HKCU) registry values written at install time. The prior value (or its absence) is journaled in the install receipt, and uninstall restores it exactly."
    },
    "envWrites": {
      "type": "array",
      "default": [],
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "value"],
        "properties": {
          "name": { "type": "string", "description": "Environment variable name (no '=' character)." },
          "value": { "type": "string" }
        }
      },
      "description": "Per-user environment variables written at install time (HKCU Environment, with the usual change broadcast). Journaled and restored by uninstall exactly like registryWrites."
    },
    "shortcuts": {
      "type": "array",
      "default": [],
      "items": {
        "type": "object",
        "additionalProperties": false,
        "required": ["name", "target"],
        "properties": {
          "name": { "type": "string", "description": "Link display name (a plain file name — no path separators or <>:\"/\\\\|?* characters)." },
          "target": { "type": "string", "description": "Path of the target file relative to the install folder; must be one of the packaged files." },
          "arguments": { "type": "string", "default": "" },
          "icon": { "type": "string", "description": "Icon file relative to the install folder (a packaged file). Defaults to the target itself." },
          "iconIndex": { "type": "integer", "minimum": 0, "default": 0 },
          "description": { "type": "string", "default": "" },
          "location": { "type": "string", "enum": ["desktop", "startMenu", "both"], "default": "startMenu" },
          "pin": { "type": "boolean", "default": false, "description": "Best-effort pin to taskbar after the link is created. Windows restricts pinning, so the outcome (pinned / unavailable) is recorded in the receipt and never fails the install." }
        }
      },
      "description": "Extra shortcuts beyond the automatic primary-executable ones, with arguments, a custom icon, and an optional taskbar pin. Journaled in the receipt; uninstall deletes the link files. A shortcut is only created when its target's component is installed."
    },
    "files": {
      "type": "array",
      "items": { "type": "string" },
      "default": [],
      "description": "Optional explicit file list: paths relative to the files directory to include in the package. Empty (the default) packs the whole files directory. Entries must be relative, unique, and contain no '..'."
    }
  },
  "$defs": {
    "hook": {
      "type": "object",
      "additionalProperties": false,
      "required": ["command"],
      "properties": {
        "command": { "type": "string", "description": "Program to run (a name found on PATH, or a path)." },
        "args": { "type": "array", "items": { "type": "string" }, "default": [] },
        "onFailure": { "type": "string", "enum": ["abort", "continue"], "default": "abort", "description": "abort: the install fails and rolls back (exit code 5), and the report names the failed hook and its exit code. continue: the failure is recorded as a warning in the receipt and the install proceeds." }
      }
    }
  }
}"##;

// ---------------------------------------------------------------------------
// Minimal JSON value + parser (objects keep insertion order).

#[derive(Debug, Clone, PartialEq)]
pub enum JsonValue {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<JsonValue>),
    Object(Vec<(String, JsonValue)>),
}

impl JsonValue {
    pub fn get(&self, key: &str) -> Option<&JsonValue> {
        match self {
            JsonValue::Object(pairs) => pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            JsonValue::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            JsonValue::Bool(b) => Some(*b),
            _ => None,
        }
    }

    /// Compact JSON rendering (used for --json command output).
    pub fn render(&self) -> String {
        let mut out = String::new();
        self.write_to(&mut out);
        out
    }

    fn write_to(&self, out: &mut String) {
        match self {
            JsonValue::Null => out.push_str("null"),
            JsonValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            JsonValue::Number(n) => {
                if n.fract() == 0.0 && n.is_finite() && *n >= -9.0e15 && *n <= 9.0e15 {
                    out.push_str(&format!("{}", *n as i64));
                } else {
                    out.push_str(&format!("{n}"));
                }
            }
            JsonValue::String(s) => write_json_string(out, s),
            JsonValue::Array(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    item.write_to(out);
                }
                out.push(']');
            }
            JsonValue::Object(pairs) => {
                out.push('{');
                for (i, (k, v)) in pairs.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    write_json_string(out, k);
                    out.push(':');
                    v.write_to(out);
                }
                out.push('}');
            }
        }
    }
}

pub fn write_json_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

/// Convenience constructor for a JSON string value.
pub fn jstr(s: impl Into<String>) -> JsonValue {
    JsonValue::String(s.into())
}

/// Convenience constructor for a JSON object from key/value pairs.
pub fn jobj(pairs: Vec<(&str, JsonValue)>) -> JsonValue {
    JsonValue::Object(pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect())
}

/// Parses JSON text. Errors carry a byte position and a plain message.
pub fn parse_json(text: &str) -> Result<JsonValue, String> {
    let mut parser = Parser {
        bytes: text.as_bytes(),
        pos: 0,
    };
    parser.skip_ws();
    let value = parser.parse_value()?;
    parser.skip_ws();
    if parser.pos != parser.bytes.len() {
        return Err(format!(
            "unexpected trailing characters at byte {}",
            parser.pos
        ));
    }
    Ok(value)
}

struct Parser<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn skip_ws(&mut self) {
        while self.pos < self.bytes.len()
            && matches!(self.bytes[self.pos], b' ' | b'\t' | b'\n' | b'\r')
        {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn parse_value(&mut self) -> Result<JsonValue, String> {
        match self.peek() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => Ok(JsonValue::String(self.parse_string()?)),
            Some(b't') => self.parse_literal("true", JsonValue::Bool(true)),
            Some(b'f') => self.parse_literal("false", JsonValue::Bool(false)),
            Some(b'n') => self.parse_literal("null", JsonValue::Null),
            Some(c) if c == b'-' || c.is_ascii_digit() => self.parse_number(),
            Some(c) => Err(format!(
                "unexpected character '{}' at byte {}",
                c as char, self.pos
            )),
            None => Err("unexpected end of input".to_string()),
        }
    }

    fn parse_literal(&mut self, literal: &str, value: JsonValue) -> Result<JsonValue, String> {
        if self.bytes[self.pos..].starts_with(literal.as_bytes()) {
            self.pos += literal.len();
            Ok(value)
        } else {
            Err(format!("invalid literal at byte {}", self.pos))
        }
    }

    fn parse_number(&mut self) -> Result<JsonValue, String> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while let Some(c) = self.peek() {
            if c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'+' | b'-') {
                self.pos += 1;
            } else {
                break;
            }
        }
        let text = std::str::from_utf8(&self.bytes[start..self.pos]).unwrap_or("");
        text.parse::<f64>()
            .map(JsonValue::Number)
            .map_err(|_| format!("invalid number at byte {start}"))
    }

    fn parse_string(&mut self) -> Result<String, String> {
        // Caller has peeked the opening quote.
        self.pos += 1;
        let mut out = String::new();
        loop {
            let c = match self.peek() {
                Some(c) => c,
                None => return Err("unterminated string".to_string()),
            };
            self.pos += 1;
            match c {
                b'"' => return Ok(out),
                b'\\' => {
                    let esc = self.peek().ok_or("unterminated escape")?;
                    self.pos += 1;
                    match esc {
                        b'"' => out.push('"'),
                        b'\\' => out.push('\\'),
                        b'/' => out.push('/'),
                        b'b' => out.push('\u{0008}'),
                        b'f' => out.push('\u{000C}'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let hi = self.parse_hex4()?;
                            if (0xD800..0xDC00).contains(&hi) {
                                // Possible surrogate pair.
                                if self.peek() == Some(b'\\') {
                                    self.pos += 1;
                                    if self.peek() == Some(b'u') {
                                        self.pos += 1;
                                        let lo = self.parse_hex4()?;
                                        if (0xDC00..0xE000).contains(&lo) {
                                            let cp = 0x10000
                                                + (((hi - 0xD800) as u32) << 10)
                                                + (lo - 0xDC00) as u32;
                                            out.push(char::from_u32(cp).unwrap_or('\u{FFFD}'));
                                        } else {
                                            out.push('\u{FFFD}');
                                            out.push(
                                                char::from_u32(lo as u32).unwrap_or('\u{FFFD}'),
                                            );
                                        }
                                        continue;
                                    } else {
                                        return Err("invalid surrogate escape".to_string());
                                    }
                                }
                                out.push('\u{FFFD}');
                            } else {
                                out.push(char::from_u32(hi as u32).unwrap_or('\u{FFFD}'));
                            }
                        }
                        _ => return Err(format!("invalid escape at byte {}", self.pos - 1)),
                    }
                }
                _ => {
                    // Consume the full UTF-8 sequence for this byte.
                    let len = utf8_len(c);
                    let start = self.pos - 1;
                    let end = (start + len).min(self.bytes.len());
                    match std::str::from_utf8(&self.bytes[start..end]) {
                        Ok(s) => {
                            out.push_str(s);
                            self.pos = end;
                        }
                        Err(_) => return Err(format!("invalid UTF-8 at byte {start}")),
                    }
                }
            }
        }
    }

    fn parse_hex4(&mut self) -> Result<u16, String> {
        if self.pos + 4 > self.bytes.len() {
            return Err("truncated \\u escape".to_string());
        }
        let text = std::str::from_utf8(&self.bytes[self.pos..self.pos + 4]).unwrap_or("");
        self.pos += 4;
        u16::from_str_radix(text, 16).map_err(|_| "invalid \\u escape".to_string())
    }

    fn parse_array(&mut self) -> Result<JsonValue, String> {
        self.pos += 1;
        let mut items = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Ok(JsonValue::Array(items));
        }
        loop {
            self.skip_ws();
            items.push(self.parse_value()?);
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    return Ok(JsonValue::Array(items));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.pos)),
            }
        }
    }

    fn parse_object(&mut self) -> Result<JsonValue, String> {
        self.pos += 1;
        let mut pairs: Vec<(String, JsonValue)> = Vec::new();
        self.skip_ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Ok(JsonValue::Object(pairs));
        }
        loop {
            self.skip_ws();
            if self.peek() != Some(b'"') {
                return Err(format!("expected object key at byte {}", self.pos));
            }
            let key = self.parse_string()?;
            self.skip_ws();
            if self.peek() != Some(b':') {
                return Err(format!("expected ':' at byte {}", self.pos));
            }
            self.pos += 1;
            self.skip_ws();
            let value = self.parse_value()?;
            pairs.push((key, value));
            self.skip_ws();
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    return Ok(JsonValue::Object(pairs));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.pos)),
            }
        }
    }
}

fn utf8_len(first: u8) -> usize {
    if first < 0x80 {
        1
    } else if first >> 5 == 0b110 {
        2
    } else if first >> 4 == 0b1110 {
        3
    } else {
        4
    }
}

// ---------------------------------------------------------------------------
// The manifest itself.

const KNOWN_KEYS: &[&str] = &[
    "appName",
    "version",
    "publisher",
    "primaryExe",
    "installLocation",
    "startMenuFolder",
    "desktopShortcut",
    "startMenuShortcut",
    "fileAssociations",
    "pathEntry",
    "launchAfterInstall",
    "startAtLogin",
    "dataDirs",
    "components",
    "hooks",
    "registryWrites",
    "envWrites",
    "shortcuts",
    "files",
];

#[derive(Debug, Clone, PartialEq)]
pub struct AppManifest {
    pub app_name: String,
    pub version: String,
    pub publisher: String,
    pub primary_exe: Option<String>,
    pub install_location: Option<String>,
    pub start_menu_folder: Option<String>,
    pub desktop_shortcut: bool,
    pub start_menu_shortcut: bool,
    pub file_associations: Vec<String>,
    pub path_entry: bool,
    /// The Finish page offers "Launch <appName>"…
    pub launch_after_install_offered: bool,
    /// …and the checkbox starts checked.
    pub launch_after_install_default: bool,
    /// The installer offers "Start <appName> at login"…
    pub start_at_login_offered: bool,
    /// …and the option starts on.
    pub start_at_login_default: bool,
    /// The app's data folders (token paths), recorded into the receipt.
    pub data_dirs: Vec<String>,
    /// Optional components (v6 power block; `packed_size` stays 0
    /// here — the packer fills it in the package).
    pub components: Vec<ComponentSpec>,
    /// Pre-install hooks (v6).
    pub pre_hooks: Vec<HookSpec>,
    /// Post-install hooks (v6).
    pub post_hooks: Vec<HookSpec>,
    /// Journaled HKCU registry writes (v6).
    pub registry_writes: Vec<RegWriteSpec>,
    /// Journaled per-user environment writes (v6).
    pub env_writes: Vec<EnvWriteSpec>,
    /// Rich shortcuts beyond the automatic ones (v6).
    pub shortcuts: Vec<RichShortcutSpec>,
    pub files: Vec<String>,
}

impl AppManifest {
    /// Parses and validates manifest JSON text. On failure returns every
    /// problem found, in plain language — this is what `seanpak validate`
    /// prints and what `seanpak pack` refuses with.
    pub fn from_json(text: &str) -> Result<AppManifest, Vec<String>> {
        let root = match parse_json(text) {
            Ok(v) => v,
            Err(e) => return Err(vec![format!("invalid JSON: {e}")]),
        };
        let pairs = match &root {
            JsonValue::Object(pairs) => pairs,
            _ => return Err(vec!["manifest root must be a JSON object".to_string()]),
        };

        let mut errors: Vec<String> = Vec::new();
        for (key, _) in pairs {
            if !KNOWN_KEYS.contains(&key.as_str()) {
                errors.push(format!(
                    "unknown field \"{key}\" (allowed: {})",
                    KNOWN_KEYS.join(", ")
                ));
            }
        }

        let get_str = |key: &str| -> Option<String> {
            match root.get(key) {
                Some(JsonValue::String(s)) => Some(s.clone()),
                _ => None,
            }
        };
        let check_str_type = |key: &str, errors: &mut Vec<String>| {
            if let Some(v) = root.get(key) {
                if v.as_str().is_none() {
                    errors.push(format!("\"{key}\" must be a string"));
                }
            }
        };
        for key in [
            "appName",
            "version",
            "publisher",
            "primaryExe",
            "installLocation",
            "startMenuFolder",
        ] {
            check_str_type(key, &mut errors);
        }
        let get_bool = |key: &str, default: bool, errors: &mut Vec<String>| -> bool {
            match root.get(key) {
                None => default,
                Some(v) => match v.as_bool() {
                    Some(b) => b,
                    None => {
                        errors.push(format!("\"{key}\" must be true or false"));
                        default
                    }
                },
            }
        };

        let app_name = get_str("appName").unwrap_or_default();
        if app_name.trim().is_empty() {
            errors.push("\"appName\" is required and must not be empty".to_string());
        } else if app_name
            .chars()
            .any(|c| matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
        {
            errors.push("\"appName\" must not contain any of <>:\"/\\|?*".to_string());
        }

        let version = get_str("version").unwrap_or_default();
        if version.is_empty() {
            errors.push("\"version\" is required and must not be empty".to_string());
        } else if !version
            .chars()
            .next()
            .map(|c| c.is_ascii_digit())
            .unwrap_or(false)
            || !version
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
        {
            errors.push(
                "\"version\" must start with a digit and contain only digits, letters, '.' and '-'"
                    .to_string(),
            );
        }

        if let Some(primary) = get_str("primaryExe") {
            if primary.is_empty()
                || !primary.to_lowercase().ends_with(".exe")
                || primary.contains("..")
                || primary.starts_with(['/', '\\'])
                || primary.chars().nth(1) == Some(':')
            {
                errors.push(
                    "\"primaryExe\" must be a path relative to the files directory, ending in .exe"
                        .to_string(),
                );
            }
        }
        if let Some(location) = get_str("installLocation") {
            let drive = location.chars().nth(1) == Some(':');
            let unc = location.starts_with("\\\\");
            if !drive && !unc {
                errors.push(
                    "\"installLocation\" must be an absolute Windows path (for example \"C:\\\\Program Files\\\\MyApp\")"
                        .to_string(),
                );
            }
        }
        if let Some(folder) = get_str("startMenuFolder") {
            if folder.contains(['/', '\\']) {
                errors.push(
                    "\"startMenuFolder\" must be a plain name, no path separators".to_string(),
                );
            }
        }

        let mut associations: Vec<String> = Vec::new();
        if let Some(v) = root.get("fileAssociations") {
            match v {
                JsonValue::Array(items) => {
                    for item in items {
                        match item.as_str() {
                            Some(ext) => match normalize_extension(ext) {
                                Some(e) => associations.push(e),
                                None => errors.push(format!(
                                    "file association \"{ext}\" is not a valid extension"
                                )),
                            },
                            None => errors
                                .push("\"fileAssociations\" entries must be strings".to_string()),
                        }
                    }
                }
                _ => errors.push("\"fileAssociations\" must be an array of strings".to_string()),
            }
        }

        let mut files: Vec<String> = Vec::new();
        if let Some(v) = root.get("files") {
            match v {
                JsonValue::Array(items) => {
                    for item in items {
                        match item.as_str() {
                            Some(path) => {
                                let normalized = path.replace('\\', "/");
                                if normalized.is_empty()
                                    || normalized.starts_with('/')
                                    || normalized.contains("..")
                                    || normalized.chars().nth(1) == Some(':')
                                {
                                    errors.push(format!(
                                        "files entry \"{path}\" must be a relative path without '..'"
                                    ));
                                } else if files.contains(&normalized) {
                                    errors.push(format!("files entry \"{path}\" is duplicated"));
                                } else {
                                    files.push(normalized);
                                }
                            }
                            None => errors.push("\"files\" entries must be strings".to_string()),
                        }
                    }
                }
                _ => errors.push("\"files\" must be an array of strings".to_string()),
            }
        }

        let (launch_after_install_offered, launch_after_install_default) =
            parse_offer_spec(&root, "launchAfterInstall", true, true, &mut errors);
        let (start_at_login_offered, start_at_login_default) =
            parse_offer_spec(&root, "startAtLogin", false, false, &mut errors);

        let mut data_dirs: Vec<String> = Vec::new();
        if let Some(v) = root.get("dataDirs") {
            match v {
                JsonValue::Array(items) => {
                    for item in items {
                        match item.as_str() {
                            Some(entry) => match validate_data_dir(entry) {
                                Ok(()) => data_dirs.push(entry.trim().to_string()),
                                Err(problem) => errors.push(problem),
                            },
                            None => errors.push("\"dataDirs\" entries must be strings".to_string()),
                        }
                    }
                }
                _ => errors.push("\"dataDirs\" must be an array of strings".to_string()),
            }
        }
        let joined_dirs = data_dirs.join(";");
        if joined_dirs.len() >= DATA_DIRS_CAPACITY {
            errors.push(format!(
                "\"dataDirs\" entries total {} characters; the limit is {}",
                joined_dirs.len(),
                DATA_DIRS_CAPACITY - 1
            ));
        }

        // The v6 power blocks. Each parser appends its own plain-
        // language problems to `errors`, so one pass reports everything.
        let components = parse_components(&root, &mut errors);
        let (pre_hooks, post_hooks) = parse_hooks(&root, &mut errors);
        let registry_writes = parse_registry_writes(&root, &mut errors);
        let env_writes = parse_env_writes(&root, &mut errors);
        let shortcuts = parse_shortcuts(&root, &mut errors);

        let desktop_shortcut = get_bool("desktopShortcut", true, &mut errors);
        let start_menu_shortcut = get_bool("startMenuShortcut", true, &mut errors);
        let path_entry = get_bool("pathEntry", false, &mut errors);

        if !errors.is_empty() {
            return Err(errors);
        }
        Ok(AppManifest {
            app_name,
            version,
            publisher: get_str("publisher").unwrap_or_default(),
            primary_exe: get_str("primaryExe"),
            install_location: get_str("installLocation"),
            start_menu_folder: get_str("startMenuFolder"),
            desktop_shortcut,
            start_menu_shortcut,
            file_associations: associations,
            path_entry,
            launch_after_install_offered,
            launch_after_install_default,
            start_at_login_offered,
            start_at_login_default,
            data_dirs,
            components,
            pre_hooks,
            post_hooks,
            registry_writes,
            env_writes,
            shortcuts,
            files,
        })
    }

    /// File-name stem for the release assets: the app name stripped of
    /// spaces and of every character outside [A-Za-z0-9._-], so asset
    /// names are deterministic (see the schema description).
    pub fn asset_stem(&self) -> String {
        let stem: String = self
            .app_name
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            .collect();
        if stem.is_empty() {
            "App".to_string()
        } else {
            stem
        }
    }

    /// `AppName-v<version>-x64-Setup.exe`
    pub fn setup_file_name(&self) -> String {
        format!("{}-v{}-x64-Setup.exe", self.asset_stem(), self.version)
    }

    /// `AppName-v<version>-x64-Portable.zip`
    pub fn portable_file_name(&self) -> String {
        format!("{}-v{}-x64-Portable.zip", self.asset_stem(), self.version)
    }

    /// The overlay install-options block this manifest records.
    pub fn to_install_options(&self) -> InstallOptions {
        let mut options = InstallOptions::defaults_for(&self.app_name);
        if let Some(location) = &self.install_location {
            copy_str_to_fixed_pub(&mut options.install_location_utf8, location);
        }
        if let Some(folder) = &self.start_menu_folder {
            copy_str_to_fixed_pub(&mut options.start_menu_folder_utf8, folder);
        }
        if !self.file_associations.is_empty() {
            let joined = self
                .file_associations
                .iter()
                .map(|e| format!(".{e}"))
                .collect::<Vec<_>>()
                .join(",");
            copy_str_to_fixed_pub(&mut options.file_associations_utf8, &joined);
        }
        options.desktop_shortcut = if self.desktop_shortcut { 1 } else { 0 };
        options.start_menu_shortcut = if self.start_menu_shortcut { 1 } else { 0 };
        options.path_entry = if self.path_entry { 1 } else { 0 };
        options.launch_after_install = self.launch_after_install_offered as u8;
        options.launch_after_install_default = self.launch_after_install_default as u8;
        options.start_at_login = self.start_at_login_offered as u8;
        options.start_at_login_default = self.start_at_login_default as u8;
        if !self.data_dirs.is_empty() {
            copy_str_to_fixed_pub(&mut options.data_dirs_utf8, &self.data_dirs.join(";"));
        }
        options
    }

    /// The manifest rendered back as JSON (the "manifest used" in the
    /// pack build report).
    pub fn to_json_value(&self) -> JsonValue {
        let mut pairs: Vec<(&str, JsonValue)> = vec![
            ("appName", jstr(self.app_name.clone())),
            ("version", jstr(self.version.clone())),
            ("publisher", jstr(self.publisher.clone())),
        ];
        if let Some(p) = &self.primary_exe {
            pairs.push(("primaryExe", jstr(p.clone())));
        }
        if let Some(l) = &self.install_location {
            pairs.push(("installLocation", jstr(l.clone())));
        }
        if let Some(f) = &self.start_menu_folder {
            pairs.push(("startMenuFolder", jstr(f.clone())));
        }
        pairs.push(("desktopShortcut", JsonValue::Bool(self.desktop_shortcut)));
        pairs.push((
            "startMenuShortcut",
            JsonValue::Bool(self.start_menu_shortcut),
        ));
        pairs.push((
            "fileAssociations",
            JsonValue::Array(
                self.file_associations
                    .iter()
                    .map(|e| jstr(e.clone()))
                    .collect(),
            ),
        ));
        pairs.push(("pathEntry", JsonValue::Bool(self.path_entry)));
        pairs.push((
            "launchAfterInstall",
            offer_json_value(
                self.launch_after_install_offered,
                self.launch_after_install_default,
            ),
        ));
        pairs.push((
            "startAtLogin",
            offer_json_value(self.start_at_login_offered, self.start_at_login_default),
        ));
        pairs.push((
            "dataDirs",
            JsonValue::Array(self.data_dirs.iter().map(|d| jstr(d.clone())).collect()),
        ));
        // The v6 power blocks render exactly as the overlay's power
        // block encodes them — one encoding for the manifest file and
        // the package, so a rendered manifest re-parses identically.
        let mut object_pairs: Vec<(String, JsonValue)> =
            pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        if let JsonValue::Object(power_pairs) = self.to_power_spec().to_json_value() {
            object_pairs.extend(power_pairs);
        }
        if !self.files.is_empty() {
            object_pairs.push((
                "files".to_string(),
                JsonValue::Array(self.files.iter().map(|f| jstr(f.clone())).collect()),
            ));
        }
        JsonValue::Object(object_pairs)
    }

    /// The v6 power spec this manifest declares (component packed
    /// sizes stay 0 — the packer computes them into the package).
    pub fn to_power_spec(&self) -> PowerSpec {
        PowerSpec {
            components: self.components.clone(),
            pre_hooks: self.pre_hooks.clone(),
            post_hooks: self.post_hooks.clone(),
            registry_writes: self.registry_writes.clone(),
            env_writes: self.env_writes.clone(),
            shortcuts: self.shortcuts.clone(),
        }
    }
}

/// Parses the five v6 power blocks (`components`, `hooks`,
/// `registryWrites`, `envWrites`, `shortcuts`) from a JSON object —
/// the same parsers [`AppManifest::from_json`] runs, exposed so other
/// surfaces that edit exactly those blocks (the Builder studio's power
/// editor) validate with the identical rules. Unknown keys are an
/// error here too, so a typo'd block name cannot pass silently.
pub fn power_spec_from_json(root: &JsonValue) -> Result<PowerSpec, Vec<String>> {
    let mut errors: Vec<String> = Vec::new();
    if !matches!(root, JsonValue::Object(_)) {
        return Err(vec!["power blocks must be a JSON object".to_string()]);
    }
    if let JsonValue::Object(pairs) = root {
        for (key, _) in pairs {
            if ![
                "components",
                "hooks",
                "registryWrites",
                "envWrites",
                "shortcuts",
            ]
            .contains(&key.as_str())
            {
                errors.push(format!(
                    "unknown power block \"{key}\" (allowed: components, hooks, registryWrites, envWrites, shortcuts)"
                ));
            }
        }
    }
    let components = parse_components(root, &mut errors);
    let (pre_hooks, post_hooks) = parse_hooks(root, &mut errors);
    let registry_writes = parse_registry_writes(root, &mut errors);
    let env_writes = parse_env_writes(root, &mut errors);
    let shortcuts = parse_shortcuts(root, &mut errors);
    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(PowerSpec {
        components,
        pre_hooks,
        post_hooks,
        registry_writes,
        env_writes,
        shortcuts,
    })
}

/// Parses an "offer" field (`launchAfterInstall`, `startAtLogin`): a
/// plain boolean (true = offered with the default on, false = not
/// offered) or an object `{"offered": bool, "defaultChecked": bool}`
/// with either key omittable. Returns (offered, default_checked).
fn parse_offer_spec(
    root: &JsonValue,
    key: &str,
    default_offered: bool,
    default_checked: bool,
    errors: &mut Vec<String>,
) -> (bool, bool) {
    match root.get(key) {
        None => (default_offered, default_checked),
        Some(JsonValue::Bool(b)) => (*b, *b),
        Some(obj @ JsonValue::Object(_)) => {
            let mut field = |name: &str, fallback: bool| -> bool {
                match obj.get(name) {
                    None => fallback,
                    Some(v) => match v.as_bool() {
                        Some(b) => b,
                        None => {
                            errors.push(format!("\"{key}.{name}\" must be true or false"));
                            fallback
                        }
                    },
                }
            };
            (
                field("offered", default_offered),
                field("defaultChecked", default_checked),
            )
        }
        Some(_) => {
            errors.push(format!(
                "\"{key}\" must be true, false, or an object with \"offered\" and \"defaultChecked\""
            ));
            (default_offered, default_checked)
        }
    }
}

/// The JSON form of an offer field — always the object form, so a
/// rendered manifest re-parses to the same value.
fn offer_json_value(offered: bool, default_checked: bool) -> JsonValue {
    jobj(vec![
        ("offered", JsonValue::Bool(offered)),
        ("defaultChecked", JsonValue::Bool(default_checked)),
    ])
}

/// A relative package path as used by `files`, component file lists,
/// and shortcut targets/icons: normalized to `/` separators, or a
/// plain-language error. `what` names the field for the message.
fn check_relative_path(raw: &str, what: &str) -> Result<String, String> {
    let normalized = raw.replace('\\', "/");
    if normalized.is_empty()
        || normalized.starts_with('/')
        || normalized.split('/').any(|part| part == "..")
        || normalized.chars().nth(1) == Some(':')
    {
        return Err(format!(
            "{what} \"{raw}\" must be a path relative to the files directory, without '..'"
        ));
    }
    Ok(normalized)
}

fn parse_components(root: &JsonValue, errors: &mut Vec<String>) -> Vec<ComponentSpec> {
    let mut components: Vec<ComponentSpec> = Vec::new();
    let Some(v) = root.get("components") else {
        return components;
    };
    let JsonValue::Array(items) = v else {
        errors.push("\"components\" must be an array of objects".to_string());
        return components;
    };
    // Files claimed so far, across all components: a file belongs to
    // at most one component.
    let mut claimed: Vec<String> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let at = format!("components[{idx}]");
        if !matches!(item, JsonValue::Object(_)) {
            errors.push(format!("\"{at}\" must be an object"));
            continue;
        }
        let id = item
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if id.is_empty()
            || id == CORE_COMPONENT_ID
            || !id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            errors.push(format!(
                "\"{at}.id\" must be a unique id of letters, digits, '-' and '_' (\"{CORE_COMPONENT_ID}\" is reserved)"
            ));
            continue;
        }
        if components.iter().any(|c| c.id == id) {
            errors.push(format!("component id \"{id}\" is used more than once"));
            continue;
        }
        let name = match item.get("name") {
            Some(JsonValue::String(s)) if !s.trim().is_empty() => s.clone(),
            _ => {
                errors.push(format!("\"{at}.name\" is required and must not be empty"));
                continue;
            }
        };
        let description = match item.get("description") {
            None => String::new(),
            Some(JsonValue::String(s)) => s.clone(),
            Some(_) => {
                errors.push(format!("\"{at}.description\" must be a string"));
                String::new()
            }
        };
        let bool_field = |key: &str, default: bool, errors: &mut Vec<String>| -> bool {
            match item.get(key) {
                None => default,
                Some(x) => match x.as_bool() {
                    Some(b) => b,
                    None => {
                        errors.push(format!("\"{at}.{key}\" must be true or false"));
                        default
                    }
                },
            }
        };
        let required = bool_field("required", false, errors);
        let default_selected = bool_field("defaultSelected", true, errors);
        let mut files: Vec<String> = Vec::new();
        if let Some(list) = item.get("files") {
            match list {
                JsonValue::Array(entries) => {
                    for entry in entries {
                        match entry.as_str() {
                            Some(raw) => {
                                match check_relative_path(raw, &format!("{at}.files entry")) {
                                    Ok(normalized) => {
                                        if claimed.contains(&normalized) {
                                            errors.push(format!(
                                                "file \"{raw}\" is claimed by more than one component"
                                            ));
                                        } else if files.contains(&normalized) {
                                            errors.push(format!(
                                                "\"{at}.files\" entry \"{raw}\" is duplicated"
                                            ));
                                        } else {
                                            claimed.push(normalized.clone());
                                            files.push(normalized);
                                        }
                                    }
                                    Err(e) => errors.push(e),
                                }
                            }
                            None => errors.push(format!("\"{at}.files\" entries must be strings")),
                        }
                    }
                }
                _ => errors.push(format!("\"{at}.files\" must be an array of strings")),
            }
        }
        components.push(ComponentSpec {
            id,
            name,
            description,
            files,
            required,
            default_selected,
            packed_size: 0,
        });
    }
    components
}

fn parse_hook_list(hooks: &JsonValue, key: &str, errors: &mut Vec<String>) -> Vec<HookSpec> {
    let mut out: Vec<HookSpec> = Vec::new();
    let Some(v) = hooks.get(key) else {
        return out;
    };
    let JsonValue::Array(items) = v else {
        errors.push(format!("\"hooks.{key}\" must be an array of objects"));
        return out;
    };
    for (idx, item) in items.iter().enumerate() {
        let at = format!("hooks.{key}[{idx}]");
        if !matches!(item, JsonValue::Object(_)) {
            errors.push(format!("\"{at}\" must be an object"));
            continue;
        }
        let command = match item.get("command") {
            Some(JsonValue::String(s)) if !s.trim().is_empty() => s.clone(),
            _ => {
                errors.push(format!(
                    "\"{at}.command\" is required and must not be empty"
                ));
                continue;
            }
        };
        let mut args: Vec<String> = Vec::new();
        if let Some(list) = item.get("args") {
            match list {
                JsonValue::Array(entries) => {
                    for entry in entries {
                        match entry.as_str() {
                            Some(s) => args.push(s.to_string()),
                            None => errors.push(format!("\"{at}.args\" entries must be strings")),
                        }
                    }
                }
                _ => errors.push(format!("\"{at}.args\" must be an array of strings")),
            }
        }
        let on_failure = match item.get("onFailure") {
            None => HookFailurePolicy::Abort,
            Some(JsonValue::String(s)) => match HookFailurePolicy::parse(s) {
                Some(p) => p,
                None => {
                    errors.push(format!(
                        "\"{at}.onFailure\" must be \"abort\" or \"continue\""
                    ));
                    HookFailurePolicy::Abort
                }
            },
            Some(_) => {
                errors.push(format!(
                    "\"{at}.onFailure\" must be \"abort\" or \"continue\""
                ));
                HookFailurePolicy::Abort
            }
        };
        out.push(HookSpec {
            command,
            args,
            on_failure,
        });
    }
    out
}

fn parse_hooks(root: &JsonValue, errors: &mut Vec<String>) -> (Vec<HookSpec>, Vec<HookSpec>) {
    let Some(v) = root.get("hooks") else {
        return (Vec::new(), Vec::new());
    };
    if !matches!(v, JsonValue::Object(_)) {
        errors.push(
            "\"hooks\" must be an object with \"preInstall\" and \"postInstall\"".to_string(),
        );
        return (Vec::new(), Vec::new());
    }
    (
        parse_hook_list(v, "preInstall", errors),
        parse_hook_list(v, "postInstall", errors),
    )
}

/// Validates a `registryWrites` key: it must be relative to
/// HKEY_CURRENT_USER (writes are per-user only) — absolute hive names
/// and prefixes, leading separators, drive syntax, and `..` segments
/// are all refused, so a manifest can never name a key outside HKCU.
fn validate_registry_key(key: &str) -> Result<String, String> {
    let k = key.trim();
    if k.is_empty() {
        return Err("\"registryWrites\" key must not be empty".to_string());
    }
    if k.starts_with(['\\', '/']) || k.contains(':') || k.contains('/') {
        return Err(format!(
            "registryWrites key \"{key}\" must be a key path relative to HKEY_CURRENT_USER (backslashes only, no hive prefix)"
        ));
    }
    const HIVES: [&str; 10] = [
        "HKEY_CURRENT_USER",
        "HKCU",
        "HKEY_LOCAL_MACHINE",
        "HKLM",
        "HKEY_CLASSES_ROOT",
        "HKCR",
        "HKEY_USERS",
        "HKU",
        "HKEY_CURRENT_CONFIG",
        "HKCC",
    ];
    let first = k.split('\\').next().unwrap_or("");
    if HIVES.iter().any(|h| first.eq_ignore_ascii_case(h)) {
        return Err(format!(
            "registryWrites key \"{key}\" must be relative to HKEY_CURRENT_USER — remove the hive prefix"
        ));
    }
    if k.split('\\').any(|part| part == ".." || part.is_empty()) {
        return Err(format!(
            "registryWrites key \"{key}\" must be a plain key path (no '..' or empty segments)"
        ));
    }
    Ok(k.to_string())
}

fn parse_registry_writes(root: &JsonValue, errors: &mut Vec<String>) -> Vec<RegWriteSpec> {
    let mut writes: Vec<RegWriteSpec> = Vec::new();
    let Some(v) = root.get("registryWrites") else {
        return writes;
    };
    let JsonValue::Array(items) = v else {
        errors.push("\"registryWrites\" must be an array of objects".to_string());
        return writes;
    };
    for (idx, item) in items.iter().enumerate() {
        let at = format!("registryWrites[{idx}]");
        if !matches!(item, JsonValue::Object(_)) {
            errors.push(format!("\"{at}\" must be an object"));
            continue;
        }
        let key = match item.get("key") {
            Some(JsonValue::String(s)) => match validate_registry_key(s) {
                Ok(k) => k,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            },
            _ => {
                errors.push(format!("\"{at}.key\" is required and must be a string"));
                continue;
            }
        };
        let name = match item.get("name") {
            None => String::new(),
            Some(JsonValue::String(s)) => s.clone(),
            Some(_) => {
                errors.push(format!("\"{at}.name\" must be a string"));
                continue;
            }
        };
        let kind = match item.get("type") {
            None => RegValueKind::Sz,
            Some(JsonValue::String(s)) => match RegValueKind::parse(s) {
                Some(k) => k,
                None => {
                    errors.push(format!("\"{at}.type\" must be \"sz\" or \"dword\""));
                    continue;
                }
            },
            Some(_) => {
                errors.push(format!("\"{at}.type\" must be \"sz\" or \"dword\""));
                continue;
            }
        };
        let value = match (kind, item.get("value")) {
            (RegValueKind::Sz, Some(JsonValue::String(s))) => s.clone(),
            (RegValueKind::Sz, _) => {
                errors.push(format!(
                    "\"{at}.value\" is required and must be a string for type \"sz\""
                ));
                continue;
            }
            (RegValueKind::Dword, Some(JsonValue::Number(n))) => {
                if *n < 0.0 || *n > u32::MAX as f64 || n.fract() != 0.0 {
                    errors.push(format!(
                        "\"{at}.value\" must be an integer between 0 and {} for type \"dword\"",
                        u32::MAX
                    ));
                    continue;
                }
                format!("{}", *n as u32)
            }
            (RegValueKind::Dword, Some(JsonValue::String(s))) => {
                let trimmed = s.trim();
                let parsed = match trimmed
                    .strip_prefix("0x")
                    .or_else(|| trimmed.strip_prefix("0X"))
                {
                    Some(hex) => u32::from_str_radix(hex, 16),
                    None => trimmed.parse::<u32>(),
                };
                match parsed {
                    Ok(n) => format!("{n}"),
                    Err(_) => {
                        errors.push(format!(
                            "\"{at}.value\" \"{s}\" is not a valid dword (decimal or 0x-prefixed hex)"
                        ));
                        continue;
                    }
                }
            }
            (RegValueKind::Dword, _) => {
                errors.push(format!(
                    "\"{at}.value\" is required for type \"dword\" (an integer or its string form)"
                ));
                continue;
            }
        };
        writes.push(RegWriteSpec {
            key,
            name,
            kind,
            value,
        });
    }
    writes
}

fn parse_env_writes(root: &JsonValue, errors: &mut Vec<String>) -> Vec<EnvWriteSpec> {
    let mut writes: Vec<EnvWriteSpec> = Vec::new();
    let Some(v) = root.get("envWrites") else {
        return writes;
    };
    let JsonValue::Array(items) = v else {
        errors.push("\"envWrites\" must be an array of objects".to_string());
        return writes;
    };
    for (idx, item) in items.iter().enumerate() {
        let at = format!("envWrites[{idx}]");
        if !matches!(item, JsonValue::Object(_)) {
            errors.push(format!("\"{at}\" must be an object"));
            continue;
        }
        let name = match item.get("name") {
            Some(JsonValue::String(s)) if !s.trim().is_empty() && !s.contains('=') => s.clone(),
            _ => {
                errors.push(format!(
                    "\"{at}.name\" is required (a variable name, without '=')"
                ));
                continue;
            }
        };
        let value = match item.get("value") {
            Some(JsonValue::String(s)) => s.clone(),
            _ => {
                errors.push(format!("\"{at}.value\" is required and must be a string"));
                continue;
            }
        };
        writes.push(EnvWriteSpec { name, value });
    }
    writes
}

/// A shortcut display name must be a plain file-name fragment: it
/// becomes `<name>.lnk` in a user folder.
fn validate_shortcut_name(name: &str) -> Result<(), String> {
    let n = name.trim();
    if n.is_empty()
        || n == "."
        || n == ".."
        || n.ends_with(['.', ' '])
        || n.chars().any(|c| {
            matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') || (c as u32) < 0x20
        })
    {
        return Err(format!(
            "shortcuts name \"{name}\" must be a plain name (no path separators or any of <>:\"/\\|?*)"
        ));
    }
    Ok(())
}

fn parse_shortcuts(root: &JsonValue, errors: &mut Vec<String>) -> Vec<RichShortcutSpec> {
    let mut shortcuts: Vec<RichShortcutSpec> = Vec::new();
    let Some(v) = root.get("shortcuts") else {
        return shortcuts;
    };
    let JsonValue::Array(items) = v else {
        errors.push("\"shortcuts\" must be an array of objects".to_string());
        return shortcuts;
    };
    for (idx, item) in items.iter().enumerate() {
        let at = format!("shortcuts[{idx}]");
        if !matches!(item, JsonValue::Object(_)) {
            errors.push(format!("\"{at}\" must be an object"));
            continue;
        }
        let name = match item.get("name") {
            Some(JsonValue::String(s)) => match validate_shortcut_name(s) {
                Ok(()) => s.clone(),
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            },
            _ => {
                errors.push(format!("\"{at}.name\" is required and must be a string"));
                continue;
            }
        };
        let target = match item.get("target") {
            Some(JsonValue::String(s)) => match check_relative_path(s, &format!("{at}.target")) {
                Ok(normalized) => normalized,
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            },
            _ => {
                errors.push(format!("\"{at}.target\" is required and must be a string"));
                continue;
            }
        };
        let str_field = |key: &str, errors: &mut Vec<String>| -> String {
            match item.get(key) {
                None => String::new(),
                Some(JsonValue::String(s)) => s.clone(),
                Some(_) => {
                    errors.push(format!("\"{at}.{key}\" must be a string"));
                    String::new()
                }
            }
        };
        let arguments = str_field("arguments", errors);
        let description = str_field("description", errors);
        let icon = match item.get("icon") {
            None => None,
            Some(JsonValue::String(s)) => match check_relative_path(s, &format!("{at}.icon")) {
                Ok(normalized) => Some(normalized),
                Err(e) => {
                    errors.push(e);
                    None
                }
            },
            Some(_) => {
                errors.push(format!("\"{at}.icon\" must be a string"));
                None
            }
        };
        let icon_index = match item.get("iconIndex") {
            None => 0,
            Some(JsonValue::Number(n))
                if *n >= 0.0 && n.fract() == 0.0 && *n <= i32::MAX as f64 =>
            {
                *n as i32
            }
            Some(_) => {
                errors.push(format!(
                    "\"{at}.iconIndex\" must be an integer of 0 or more"
                ));
                0
            }
        };
        let location = match item.get("location") {
            None => ShortcutLocation::StartMenu,
            Some(JsonValue::String(s)) => match ShortcutLocation::parse(s) {
                Some(l) => l,
                None => {
                    errors.push(format!(
                        "\"{at}.location\" must be \"desktop\", \"startMenu\" or \"both\""
                    ));
                    ShortcutLocation::StartMenu
                }
            },
            Some(_) => {
                errors.push(format!(
                    "\"{at}.location\" must be \"desktop\", \"startMenu\" or \"both\""
                ));
                ShortcutLocation::StartMenu
            }
        };
        let pin = match item.get("pin") {
            None => false,
            Some(x) => match x.as_bool() {
                Some(b) => b,
                None => {
                    errors.push(format!("\"{at}.pin\" must be true or false"));
                    false
                }
            },
        };
        shortcuts.push(RichShortcutSpec {
            name,
            target,
            arguments,
            icon,
            icon_index,
            description,
            location,
            pin,
        });
    }
    shortcuts
}

/// Validates one `dataDirs` entry. Entries are token paths: one of the
/// `%APPDATA%` / `%LOCALAPPDATA%` / `%USERPROFILE%` / `%INSTALLDIR%`
/// roots followed by at least one folder below it, an absolute Windows
/// path at least two folders below its root, or a path relative to the
/// install folder. `..` and `;` are rejected everywhere: these folders
/// are what an uninstall may delete, so a manifest must never be able
/// to name a root itself (or something outside its root via `..`).
fn validate_data_dir(entry: &str) -> Result<(), String> {
    let e = entry.trim();
    if e.is_empty() {
        return Err("\"dataDirs\" entries must not be empty".to_string());
    }
    if e.contains(';') {
        return Err(format!("dataDirs entry \"{entry}\" must not contain ';'"));
    }
    const TOKENS: [&str; 4] = [
        "%APPDATA%",
        "%LOCALAPPDATA%",
        "%USERPROFILE%",
        "%INSTALLDIR%",
    ];
    let upper = e.to_uppercase();
    for token in TOKENS {
        if upper.starts_with(token) {
            let rest = e[token.len()..].trim_start_matches(['\\', '/']);
            if rest.is_empty() {
                return Err(format!(
                    "dataDirs entry \"{entry}\" must name a folder below {token}, not {token} itself"
                ));
            }
            if rest.split(['\\', '/']).any(|part| part == "..") {
                return Err(format!("dataDirs entry \"{entry}\" must not contain '..'"));
            }
            return Ok(());
        }
    }
    if e.split(['\\', '/']).any(|part| part == "..") {
        return Err(format!("dataDirs entry \"{entry}\" must not contain '..'"));
    }
    let drive_absolute = e.chars().nth(1) == Some(':');
    let rooted = drive_absolute || e.starts_with(['\\', '/']);
    if rooted {
        let parts: Vec<&str> = e.split(['\\', '/']).filter(|p| !p.is_empty()).collect();
        // For a drive path the first part is the drive ("C:") itself.
        let below_root = if drive_absolute {
            parts.len().saturating_sub(1)
        } else {
            parts.len()
        };
        if below_root < 2 {
            return Err(format!(
                "dataDirs entry \"{entry}\" is too broad: name a folder at least two levels below the root"
            ));
        }
    }
    Ok(())
}

fn normalize_extension(raw: &str) -> Option<String> {
    let ext = raw.trim().trim_start_matches('.').to_lowercase();
    if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
        None
    } else {
        Some(ext)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_round_trip_and_escapes() {
        let value = parse_json(r#"{"a":"x\né😀","b":[1,true,null],"c":{"d":2.5}}"#).unwrap();
        assert_eq!(value.get("a").unwrap().as_str().unwrap(), "x\né😀");
        let rendered = value.render();
        let again = parse_json(&rendered).unwrap();
        assert_eq!(value, again);
        assert!(parse_json("{").is_err());
        assert!(parse_json("{} extra").is_err());
    }

    #[test]
    fn schema_text_is_valid_json() {
        let schema = parse_json(MANIFEST_SCHEMA_JSON).unwrap();
        assert_eq!(
            schema.get("title").unwrap().as_str().unwrap(),
            "SeanPak App Manifest"
        );
    }

    #[test]
    fn valid_manifest_parses_with_defaults() {
        let m = AppManifest::from_json(
            r#"{"appName":"Dev Text Tools","version":"0.7.2","fileAssociations":[".TXT","md"]}"#,
        )
        .unwrap();
        assert_eq!(m.app_name, "Dev Text Tools");
        assert_eq!(m.asset_stem(), "DevTextTools");
        assert_eq!(m.setup_file_name(), "DevTextTools-v0.7.2-x64-Setup.exe");
        assert_eq!(
            m.portable_file_name(),
            "DevTextTools-v0.7.2-x64-Portable.zip"
        );
        assert!(m.desktop_shortcut);
        assert!(m.start_menu_shortcut);
        assert!(!m.path_entry);
        assert_eq!(
            m.file_associations,
            vec!["txt".to_string(), "md".to_string()]
        );
        let options = m.to_install_options();
        assert_eq!(options.file_associations(), ".txt,.md");
    }

    #[test]
    fn offer_fields_and_data_dirs_parse_and_carry_through() {
        // Defaults: launch offered + checked, login not offered, no data.
        let m = AppManifest::from_json(r#"{"appName":"X","version":"1.0"}"#).unwrap();
        assert!(m.launch_after_install_offered);
        assert!(m.launch_after_install_default);
        assert!(!m.start_at_login_offered);
        assert!(!m.start_at_login_default);
        assert!(m.data_dirs.is_empty());
        let options = m.to_install_options();
        assert!(options.offers_launch_after_install());
        assert!(options.launch_after_install_default_checked());
        assert!(!options.offers_start_at_login());

        // Object + boolean forms, and data dirs landing in the options.
        let m = AppManifest::from_json(
            r#"{"appName":"X","version":"1.0",
                "launchAfterInstall":{"offered":true,"defaultChecked":false},
                "startAtLogin":true,
                "dataDirs":["%APPDATA%\\X","%INSTALLDIR%\\data","cache"]}"#,
        )
        .unwrap();
        assert!(m.launch_after_install_offered);
        assert!(!m.launch_after_install_default);
        assert!(m.start_at_login_offered);
        assert!(m.start_at_login_default);
        let options = m.to_install_options();
        assert!(options.offers_launch_after_install());
        assert!(!options.launch_after_install_default_checked());
        assert!(options.start_at_login_chosen());
        assert_eq!(
            options.data_dirs(),
            vec![
                "%APPDATA%\\X".to_string(),
                "%INSTALLDIR%\\data".to_string(),
                "cache".to_string()
            ]
        );

        // The rendered manifest re-parses to the same new fields.
        let rendered = m.to_json_value().render();
        let m2 = AppManifest::from_json(&rendered).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn data_dirs_validation_rejects_roots_and_escapes() {
        for bad in [
            r#"{"appName":"X","version":"1.0","dataDirs":["%APPDATA%"]}"#,
            r#"{"appName":"X","version":"1.0","dataDirs":["%APPDATA%\\..\\evil"]}"#,
            r#"{"appName":"X","version":"1.0","dataDirs":["a;b"]}"#,
            r#"{"appName":"X","version":"1.0","dataDirs":["C:\\OnlyOne"]}"#,
            r#"{"appName":"X","version":"1.0","dataDirs":["..\\up"]}"#,
            r#"{"appName":"X","version":"1.0","dataDirs":[""]}"#,
        ] {
            let errors = AppManifest::from_json(bad).unwrap_err();
            assert!(
                errors.iter().any(|e| e.contains("dataDirs")),
                "{bad} -> {errors:?}"
            );
        }
        let errors =
            AppManifest::from_json(r#"{"appName":"X","version":"1.0","startAtLogin":"yes"}"#)
                .unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("startAtLogin")),
            "{errors:?}"
        );
    }

    #[test]
    fn power_blocks_parse_carry_through_and_round_trip() {
        let m = AppManifest::from_json(
            r#"{"appName":"Power App","version":"2.0",
                "components":[
                  {"id":"extras","name":"Extras","files":["extras\\big.bin"],"defaultSelected":false},
                  {"id":"engine","name":"Engine","files":["engine.dll"],"required":true}
                ],
                "hooks":{
                  "preInstall":[{"command":"prep.exe","args":["--go"],"onFailure":"continue"}],
                  "postInstall":[{"command":"finish.exe"}]
                },
                "registryWrites":[
                  {"key":"Software\\Power App","name":"Mode","type":"dword","value":"0x1F"},
                  {"key":"Software\\Power App","value":"plain"}
                ],
                "envWrites":[{"name":"POWER_APP_HOME","value":"C:\\Power"}],
                "shortcuts":[
                  {"name":"Power Console","target":"app.exe","arguments":"--console","location":"both","pin":true}
                ]}"#,
        )
        .unwrap();
        assert_eq!(m.components.len(), 2);
        assert_eq!(m.components[0].files, vec!["extras/big.bin".to_string()]);
        assert!(!m.components[0].default_selected);
        assert!(m.components[1].required);
        assert_eq!(m.pre_hooks.len(), 1);
        assert_eq!(m.pre_hooks[0].on_failure, HookFailurePolicy::Continue);
        assert_eq!(m.post_hooks[0].on_failure, HookFailurePolicy::Abort);
        // The dword hex string canonicalizes to decimal.
        assert_eq!(m.registry_writes[0].value, "31");
        assert_eq!(m.registry_writes[0].kind, RegValueKind::Dword);
        assert_eq!(m.registry_writes[1].kind, RegValueKind::Sz);
        assert_eq!(m.registry_writes[1].name, "");
        assert_eq!(m.env_writes[0].name, "POWER_APP_HOME");
        assert_eq!(m.shortcuts[0].location, ShortcutLocation::Both);
        assert!(m.shortcuts[0].pin);

        // The power spec the packer embeds matches the manifest's.
        let power = m.to_power_spec();
        assert_eq!(power.components.len(), 2);
        assert_eq!(power.shortcuts.len(), 1);

        // The rendered manifest re-parses to the same value.
        let rendered = m.to_json_value().render();
        let m2 = AppManifest::from_json(&rendered).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn power_blocks_validation_reports_clear_errors() {
        let cases: &[(&str, &str)] = &[
            (
                r#"{"appName":"X","version":"1.0","components":[{"id":"core","name":"C"}]}"#,
                "reserved",
            ),
            (
                r#"{"appName":"X","version":"1.0","components":[{"id":"a","name":"A","files":["f.txt"]},{"id":"b","name":"B","files":["f.txt"]}]}"#,
                "more than one component",
            ),
            (
                r#"{"appName":"X","version":"1.0","components":[{"id":"a","name":"A"},{"id":"a","name":"A2"}]}"#,
                "more than once",
            ),
            (
                r#"{"appName":"X","version":"1.0","hooks":{"preInstall":[{"command":"x","onFailure":"retry"}]}}"#,
                "onFailure",
            ),
            (
                r#"{"appName":"X","version":"1.0","registryWrites":[{"key":"HKCU\\Software\\X","value":"v"}]}"#,
                "relative to HKEY_CURRENT_USER",
            ),
            (
                r#"{"appName":"X","version":"1.0","registryWrites":[{"key":"Software\\..\\X","value":"v"}]}"#,
                "registryWrites key",
            ),
            (
                r#"{"appName":"X","version":"1.0","registryWrites":[{"key":"Software\\X","type":"dword","value":"banana"}]}"#,
                "dword",
            ),
            (
                r#"{"appName":"X","version":"1.0","envWrites":[{"name":"A=B","value":"v"}]}"#,
                "envWrites[0].name",
            ),
            (
                r#"{"appName":"X","version":"1.0","shortcuts":[{"name":"bad/name","target":"a.exe"}]}"#,
                "shortcuts name",
            ),
            (
                r#"{"appName":"X","version":"1.0","shortcuts":[{"name":"Ok","target":"../evil.exe"}]}"#,
                "target",
            ),
        ];
        for (json, needle) in cases {
            let errors = AppManifest::from_json(json).unwrap_err();
            assert!(
                errors.iter().any(|e| e.contains(needle)),
                "{json} -> {errors:?} (want mention of {needle:?})"
            );
        }
    }

    #[test]
    fn invalid_manifests_report_clear_errors() {
        let errors = AppManifest::from_json(r#"{"version":"1.0"}"#).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("appName")), "{errors:?}");
        let errors = AppManifest::from_json(r#"{"appName":"X","version":"beta"}"#).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("version")), "{errors:?}");
        let errors =
            AppManifest::from_json(r#"{"appName":"X","version":"1.0","bogus":1}"#).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("unknown field")),
            "{errors:?}"
        );
        let errors = AppManifest::from_json("not json").unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("invalid JSON")),
            "{errors:?}"
        );
        let errors =
            AppManifest::from_json(r#"{"appName":"X","version":"1.0","files":["../escape.txt"]}"#)
                .unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("files entry")),
            "{errors:?}"
        );
    }
}
