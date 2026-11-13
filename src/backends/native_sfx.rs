use super::{BuildConfig, InstallerBackend, ToolStatus};
use crate::overlay::OverlayWriter;
use std::fs;
use std::path::PathBuf;

/// The native SeanPak engine: a sector-aligned PE overlay appended to a copy
/// of the SeanPak executable itself. The produced file is a self-extracting
/// installer — running it (or `seanpak --extract <file>`) unpacks the payload
/// with the overlay reader. No external tool, stub binary, or compression
/// crate is involved.
pub struct NativeSfxBackend;

impl NativeSfxBackend {
    pub fn new() -> Self {
        Self
    }
}

impl Default for NativeSfxBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl InstallerBackend for NativeSfxBackend {
    fn name(&self) -> &'static str {
        "Native SeanPak Overlay"
    }

    fn description(&self) -> &'static str {
        "Built-in self-extracting installer: sector-aligned PE overlay on the SeanPak executable, extracted by the same program. No external tools required."
    }

    fn script_extension(&self) -> &'static str {
        "txt"
    }

    fn check_tool_installed(&self) -> ToolStatus {
        ToolStatus::Embedded
    }

    /// For the native engine the "script" is a plain-text build summary:
    /// everything the overlay manifest will record, human-readable.
    fn generate_script(&self, config: &BuildConfig) -> std::io::Result<String> {
        let install_dir = match &config.target_dir {
            super::TargetInstallDir::ProgramFiles64 => "Program Files (extraction default)",
            super::TargetInstallDir::LocalAppData => "LocalAppData (extraction default)",
            super::TargetInstallDir::Custom(custom) => custom.as_str(),
        };
        Ok(format!(
            "SeanPak native overlay build summary\n\
             App: {} {}\n\
             Publisher: {}\n\
             Website: {}\n\
             Source directory: {}\n\
             Primary executable: {}\n\
             Install location hint: {}\n\
             Privileges: {:?}\n\
             Desktop shortcut: {} (honored by Inno/NSIS engines; the native overlay extracts files only)\n\
             Start menu shortcut: {}\n\
             Output: {}\n",
            config.app_name,
            config.version,
            config.publisher,
            config.website,
            config.source_dir.display(),
            config.primary_exe,
            install_dir,
            config.privileges,
            config.desktop_shortcut,
            config.start_menu_shortcut,
            config.output_dir.join(&config.output_exe_name).display(),
        ))
    }

    fn build(
        &self,
        config: &BuildConfig,
        log_fn: &mut dyn FnMut(&str),
    ) -> std::io::Result<PathBuf> {
        log_fn(&format!(
            "Building native overlay installer for '{}'...",
            config.app_name
        ));

        if !config.source_dir.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!(
                    "source directory does not exist: {}",
                    config.source_dir.display()
                ),
            ));
        }

        fs::create_dir_all(&config.output_dir)?;
        let output_file = config.output_dir.join(&config.output_exe_name);
        let stub = std::env::current_exe().map_err(std::io::Error::other)?;

        let bytes = OverlayWriter::build_from_folder(
            &stub,
            &output_file,
            &config.source_dir,
            &config.app_name,
            &config.version,
            &config.publisher,
        )?;
        log_fn(&format!(
            "Wrote self-extracting installer: {} ({bytes} bytes)",
            output_file.display()
        ));
        Ok(output_file)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backends::BuildConfig;
    use crate::overlay::OverlayReader;

    fn unique_temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "seanpak_sfx_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn summary_mentions_app_and_output() {
        let backend = NativeSfxBackend::new();
        let config = BuildConfig {
            app_name: "Widget".to_string(),
            ..Default::default()
        };
        let script = backend.generate_script(&config).unwrap();
        assert!(script.contains("Widget"));
        assert!(script.contains("MyApplication-Setup.exe") || script.contains("Widget"));
        assert_eq!(backend.script_extension(), "txt");
        assert!(matches!(
            backend.check_tool_installed(),
            ToolStatus::Embedded
        ));
    }

    #[test]
    fn build_produces_extractable_installer() {
        let root = unique_temp("build");
        let src = root.join("src");
        fs::create_dir_all(&src).unwrap();
        fs::write(src.join("widget.exe"), b"widget-bytes").unwrap();
        fs::write(src.join("notes.txt"), b"hello").unwrap();

        let config = BuildConfig {
            app_name: "Widget".to_string(),
            source_dir: src,
            primary_exe: "widget.exe".to_string(),
            output_dir: root.join("dist"),
            output_exe_name: "Widget-Setup.exe".to_string(),
            ..Default::default()
        };

        let backend = NativeSfxBackend::new();
        let mut logs = Vec::new();
        let out = backend
            .build(&config, &mut |line| logs.push(line.to_string()))
            .unwrap();
        assert!(out.is_file());

        let file = fs::File::open(&out).unwrap();
        let (manifest, records) = OverlayReader::read_manifest(file)
            .unwrap()
            .expect("built installer must carry an overlay");
        assert_eq!(manifest.app_name(), "Widget");
        assert_eq!(records.len(), 2);

        let dest = root.join("installed");
        let written = OverlayReader::extract_file(&out, &dest).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(fs::read(dest.join("widget.exe")).unwrap(), b"widget-bytes");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn build_rejects_missing_source_dir() {
        let root = unique_temp("missing");
        let config = BuildConfig {
            source_dir: root.join("does-not-exist"),
            output_dir: root.join("dist"),
            ..Default::default()
        };
        let backend = NativeSfxBackend::new();
        let mut sink = |_: &str| {};
        let err = backend.build(&config, &mut sink).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
        let _ = fs::remove_dir_all(&root);
    }
}
