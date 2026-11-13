//! Answer files (v7): a JSON preset that answers an install's
//! questions ahead of time — the location, every install option, the
//! component selection, and the uninstall data-purge default.
//!
//! An answer file is only ever used when someone asks for it: the CLI
//! takes `--install --answers <file.json>` explicitly, and the wizard
//! *offers* an `answers.json` found beside the Setup.exe on its Review
//! page (Apply / Ignore) — it is never applied silently.
//!
//! The format is deliberately strict, because a preset that drifts
//! from the package should fail loudly instead of installing something
//! half-right: unknown fields are errors, every validation message
//! names the offending field, and component ids are checked by the
//! shared selection resolution (which names the offered parts).

use crate::manifest::{jobj, jstr, parse_json, JsonValue};
use crate::overlay::{FileRecordHeader, InstallOptions, PowerSpec};

/// File name the wizard looks for beside the Setup.exe.
pub const ANSWERS_FILE_NAME: &str = "answers.json";

const KNOWN_FIELDS: [&str; 11] = [
    "installLocation",
    "desktopShortcut",
    "startMenuShortcut",
    "startMenuFolder",
    "fileAssociations",
    "pathEntry",
    "startAtLogin",
    "launchAfterInstall",
    "components",
    "withoutComponents",
    "purgeDataOnUninstall",
];

/// A parsed answer file. Every field is optional; an absent field
/// leaves the package's own default in charge.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Answers {
    /// Absolute install location (wins over the package default;
    /// an explicit `--to` still wins over this).
    pub install_location: Option<String>,
    pub desktop_shortcut: Option<bool>,
    pub start_menu_shortcut: Option<bool>,
    pub start_menu_folder: Option<String>,
    /// Normalized extensions (no dot, lowercase), like the receipt's.
    pub file_associations: Option<Vec<String>>,
    pub path_entry: Option<bool>,
    /// The start-at-login choice (offering it is implied).
    pub start_at_login: Option<bool>,
    /// The launch-after-install choice for this run: the wizard's
    /// Finish checkbox starts in this state; a headless install
    /// launches only when this is true.
    pub launch_after_install: Option<bool>,
    /// Explicit full component selection (ids).
    pub components: Option<Vec<String>>,
    /// Component ids to leave out of the default selection.
    pub without_components: Vec<String>,
    /// The uninstall confirmation's data-purge checkbox default,
    /// recorded in the receipt at install time. Never a silent purge:
    /// a headless uninstall still requires `--purge-data`.
    pub purge_data_on_uninstall: bool,
}

/// Parses + validates answer-file JSON text. Every problem found is
/// reported (not just the first), and every message names its field.
pub fn parse_answers(text: &str) -> Result<Answers, Vec<String>> {
    let root = match parse_json(text) {
        Ok(v) => v,
        Err(e) => return Err(vec![format!("answers file is not valid JSON: {e}")]),
    };
    let pairs = match &root {
        JsonValue::Object(pairs) => pairs,
        _ => return Err(vec!["answers file root must be a JSON object".to_string()]),
    };
    let mut errors: Vec<String> = Vec::new();
    for (key, _) in pairs {
        if !KNOWN_FIELDS.contains(&key.as_str()) {
            errors.push(format!(
                "unknown field \"{key}\" in answers file (allowed: {})",
                KNOWN_FIELDS.join(", ")
            ));
        }
    }
    let get_bool = |key: &str, errors: &mut Vec<String>| -> Option<bool> {
        match root.get(key) {
            None => None,
            Some(v) => match v.as_bool() {
                Some(b) => Some(b),
                None => {
                    errors.push(format!("\"{key}\" must be true or false"));
                    None
                }
            },
        }
    };
    let get_str = |key: &str, errors: &mut Vec<String>| -> Option<String> {
        match root.get(key) {
            None => None,
            Some(v) => match v.as_str() {
                Some(s) => Some(s.to_string()),
                None => {
                    errors.push(format!("\"{key}\" must be a string"));
                    None
                }
            },
        }
    };
    let get_str_list = |key: &str, errors: &mut Vec<String>| -> Option<Vec<String>> {
        match root.get(key) {
            None => None,
            Some(JsonValue::Array(items)) => {
                let mut out = Vec::new();
                for item in items {
                    match item.as_str() {
                        Some(s) => out.push(s.to_string()),
                        None => {
                            errors.push(format!("\"{key}\" entries must be strings"));
                            return None;
                        }
                    }
                }
                Some(out)
            }
            Some(_) => {
                errors.push(format!("\"{key}\" must be an array of strings"));
                None
            }
        }
    };

    let mut answers = Answers::default();

    if let Some(location) = get_str("installLocation", &mut errors) {
        // The same absoluteness rule the manifest enforces.
        let drive = location.chars().nth(1) == Some(':');
        let unc = location.starts_with("\\\\");
        if !drive && !unc {
            errors.push(
                "\"installLocation\" must be an absolute Windows path (for example \"C:\\\\Program Files\\\\MyApp\")"
                    .to_string(),
            );
        } else {
            answers.install_location = Some(location);
        }
    }
    answers.desktop_shortcut = get_bool("desktopShortcut", &mut errors);
    answers.start_menu_shortcut = get_bool("startMenuShortcut", &mut errors);
    if let Some(folder) = get_str("startMenuFolder", &mut errors) {
        if folder.contains(['/', '\\']) {
            errors.push("\"startMenuFolder\" must be a plain name, no path separators".to_string());
        } else {
            answers.start_menu_folder = Some(folder);
        }
    }
    if let Some(raw) = get_str_list("fileAssociations", &mut errors) {
        let mut normalized = Vec::new();
        for entry in &raw {
            let ext = entry.trim().trim_start_matches('.').to_lowercase();
            if ext.is_empty() || !ext.chars().all(|c| c.is_ascii_alphanumeric()) {
                errors.push(format!(
                    "\"fileAssociations\" entry \"{entry}\" is not a valid extension"
                ));
            } else {
                normalized.push(ext);
            }
        }
        answers.file_associations = Some(normalized);
    }
    answers.path_entry = get_bool("pathEntry", &mut errors);
    answers.start_at_login = get_bool("startAtLogin", &mut errors);
    answers.launch_after_install = get_bool("launchAfterInstall", &mut errors);
    if let Some(ids) = get_str_list("components", &mut errors) {
        for id in &ids {
            if id.is_empty() {
                errors.push("\"components\" entries must not be empty".to_string());
            }
        }
        answers.components = Some(ids);
    }
    if let Some(ids) = get_str_list("withoutComponents", &mut errors) {
        for id in &ids {
            if id.is_empty() {
                errors.push("\"withoutComponents\" entries must not be empty".to_string());
            }
        }
        answers.without_components = ids;
    }
    answers.purge_data_on_uninstall =
        get_bool("purgeDataOnUninstall", &mut errors).unwrap_or(false);

    if !errors.is_empty() {
        return Err(errors);
    }
    Ok(answers)
}

/// Loads an answer file from disk: read + [`parse_answers`], with the
/// read failure kept distinct from validation failures.
pub fn load_answers(path: &std::path::Path) -> Result<Answers, Vec<String>> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_answers(&text),
        Err(e) => Err(vec![format!(
            "cannot read answers file {}: {e}",
            path.display()
        )]),
    }
}

impl Answers {
    /// Applies the preset's option fields onto a package's recorded
    /// install options (the same in-place edit the wizard's Options
    /// page performs): choice fields only — the offers themselves
    /// stay the package's, except start-at-login and launch, where
    /// naming a choice implies the offer so the choice can take
    /// effect.
    pub fn apply_to_options(&self, options: &mut InstallOptions) {
        if let Some(on) = self.desktop_shortcut {
            options.desktop_shortcut = on as u8;
        }
        if let Some(on) = self.start_menu_shortcut {
            options.start_menu_shortcut = on as u8;
        }
        if let Some(folder) = &self.start_menu_folder {
            crate::overlay::copy_str_to_fixed_pub(
                &mut options.start_menu_folder_utf8,
                folder.trim(),
            );
        }
        if let Some(extensions) = &self.file_associations {
            let joined = extensions
                .iter()
                .map(|e| format!(".{e}"))
                .collect::<Vec<_>>()
                .join(",");
            crate::overlay::copy_str_to_fixed_pub(&mut options.file_associations_utf8, &joined);
        }
        if let Some(on) = self.path_entry {
            options.path_entry = on as u8;
        }
        if let Some(choice) = self.start_at_login {
            options.start_at_login = 1;
            options.start_at_login_default = choice as u8;
        }
        if let Some(choice) = self.launch_after_install {
            options.launch_after_install = 1;
            options.launch_after_install_default = choice as u8;
        }
    }
}

/// The template `seanpak answers-template <Setup.exe>` prints: every
/// field, filled with the value an install of this package would use
/// by default (the resolved component selection included), so editing
/// the template changes exactly the lines a preset author means to.
pub fn answers_template(
    options: &InstallOptions,
    power: &PowerSpec,
    records: &[FileRecordHeader],
    primary_rel: Option<&str>,
) -> JsonValue {
    let selection = power
        .resolve_selection(None, &[], None, records, primary_rel)
        .unwrap_or_else(|_| power.default_selection(None, power.has_core_files(records)));
    jobj(vec![
        ("installLocation", jstr(options.install_location())),
        (
            "desktopShortcut",
            JsonValue::Bool(options.wants_desktop_shortcut()),
        ),
        (
            "startMenuShortcut",
            JsonValue::Bool(options.wants_start_menu_shortcut()),
        ),
        ("startMenuFolder", jstr(options.start_menu_folder())),
        (
            "fileAssociations",
            JsonValue::Array(
                crate::sysinstall::parse_extensions(&options.file_associations())
                    .iter()
                    .map(|e| jstr(format!(".{e}")))
                    .collect(),
            ),
        ),
        ("pathEntry", JsonValue::Bool(options.wants_path_entry())),
        (
            "startAtLogin",
            JsonValue::Bool(options.start_at_login_chosen()),
        ),
        (
            "launchAfterInstall",
            JsonValue::Bool(options.launch_after_install_default_checked()),
        ),
        (
            "components",
            JsonValue::Array(selection.iter().map(|id| jstr(id.clone())).collect()),
        ),
        ("withoutComponents", JsonValue::Array(Vec::new())),
        ("purgeDataOnUninstall", JsonValue::Bool(false)),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::overlay::InstallOptions;

    #[test]
    fn parses_a_full_preset() {
        let answers = parse_answers(
            r#"{
                "installLocation": "C:\\Apps\\Thing",
                "desktopShortcut": false,
                "startMenuFolder": "Thing",
                "fileAssociations": [".txt", "MD"],
                "pathEntry": true,
                "startAtLogin": true,
                "launchAfterInstall": false,
                "components": ["core", "extras"],
                "withoutComponents": ["docs"],
                "purgeDataOnUninstall": true
            }"#,
        )
        .unwrap();
        assert_eq!(answers.install_location.as_deref(), Some("C:\\Apps\\Thing"));
        assert_eq!(answers.desktop_shortcut, Some(false));
        assert_eq!(
            answers.file_associations,
            Some(vec!["txt".to_string(), "md".to_string()])
        );
        assert_eq!(
            answers.components,
            Some(vec!["core".to_string(), "extras".to_string()])
        );
        assert_eq!(answers.without_components, vec!["docs".to_string()]);
        assert!(answers.purge_data_on_uninstall);
    }

    #[test]
    fn unknown_fields_and_bad_types_are_errors_naming_the_field() {
        let errors = parse_answers(r#"{"destinaton": "C:\\x", "pathEntry": "yes"}"#).unwrap_err();
        assert!(
            errors.iter().any(|e| e.contains("\"destinaton\"")),
            "{errors:?}"
        );
        assert!(
            errors.iter().any(|e| e.contains("\"pathEntry\"")),
            "{errors:?}"
        );
    }

    #[test]
    fn relative_location_and_bad_extension_are_refused() {
        let errors = parse_answers(r#"{"installLocation": "Apps\\Thing"}"#).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("installLocation")));
        let errors = parse_answers(r#"{"fileAssociations": ["t.x t"]}"#).unwrap_err();
        assert!(errors.iter().any(|e| e.contains("fileAssociations")));
        let errors = parse_answers("not json").unwrap_err();
        assert!(errors.iter().any(|e| e.contains("not valid JSON")));
    }

    #[test]
    fn apply_to_options_edits_the_choice_fields() {
        let answers = parse_answers(
            r#"{"desktopShortcut": false, "startAtLogin": true, "fileAssociations": [".md"]}"#,
        )
        .unwrap();
        let mut options = InstallOptions::defaults_for("Thing");
        answers.apply_to_options(&mut options);
        assert!(!options.wants_desktop_shortcut());
        assert!(options.offers_start_at_login());
        assert!(options.start_at_login_chosen());
        assert_eq!(options.file_associations(), ".md");
    }

    #[test]
    fn template_reflects_package_defaults() {
        let options = InstallOptions::defaults_for("Thing");
        let power = PowerSpec::default();
        let template = answers_template(&options, &power, &[], None);
        let text = template.render();
        let parsed = parse_answers(&text).unwrap();
        assert_eq!(parsed.desktop_shortcut, Some(true));
        assert_eq!(parsed.start_at_login, Some(false));
        assert!(!parsed.purge_data_on_uninstall);
    }
}
