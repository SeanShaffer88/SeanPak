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

use crate::overlay::{copy_str_to_fixed_pub, InstallOptions};

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
      "description": "Recorded install option: create a desktop shortcut (applied by a later SeanPak slice; shown in the wizard today)."
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
      "description": "File extensions to associate (with or without a leading dot, for example \".txt\" or \"md\"). Recorded and shown in the wizard; registration is applied by a later slice."
    },
    "pathEntry": {
      "type": "boolean",
      "default": false,
      "description": "Recorded install option: add the install folder to PATH (applied by a later slice)."
    },
    "files": {
      "type": "array",
      "items": { "type": "string" },
      "default": [],
      "description": "Optional explicit file list: paths relative to the files directory to include in the package. Empty (the default) packs the whole files directory. Entries must be relative, unique, and contain no '..'."
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
        if !self.files.is_empty() {
            pairs.push((
                "files",
                JsonValue::Array(self.files.iter().map(|f| jstr(f.clone())).collect()),
            ));
        }
        jobj(pairs)
    }
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
