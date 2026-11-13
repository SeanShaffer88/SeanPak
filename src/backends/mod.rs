use std::path::PathBuf;

#[derive(Debug, Clone)]
pub enum ToolStatus {
    Ready(PathBuf),
    NotFound { hint: String },
    Embedded,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TargetInstallDir {
    #[default]
    ProgramFiles64,
    LocalAppData,
    Custom(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PrivilegeLevel {
    #[default]
    RequireAdministrator,
    AsInvoker,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendEngineType {
    #[default]
    InnoSetup,
    Nsis,
    NativeSfx,
}

#[derive(Debug, Clone)]
pub struct BuildConfig {
    pub app_name: String,
    pub version: String,
    pub publisher: String,
    pub website: String,
    pub source_dir: PathBuf,
    pub primary_exe: String,
    pub custom_icon_path: Option<PathBuf>,
    pub target_dir: TargetInstallDir,
    pub privileges: PrivilegeLevel,
    pub desktop_shortcut: bool,
    pub start_menu_shortcut: bool,
    pub startup_registry: bool,
    pub clean_uninstaller: bool,
    pub file_association_ext: Option<String>,
    pub file_association_desc: Option<String>,
    pub output_dir: PathBuf,
    pub output_exe_name: String,
}

impl Default for BuildConfig {
    fn default() -> Self {
        Self {
            app_name: "MyApplication".to_string(),
            version: "1.0.0.0".to_string(),
            publisher: "MyCompany Inc.".to_string(),
            website: "https://example.com".to_string(),
            source_dir: PathBuf::new(),
            primary_exe: "app.exe".to_string(),
            custom_icon_path: None,
            target_dir: TargetInstallDir::ProgramFiles64,
            privileges: PrivilegeLevel::RequireAdministrator,
            desktop_shortcut: true,
            start_menu_shortcut: true,
            startup_registry: false,
            clean_uninstaller: true,
            file_association_ext: None,
            file_association_desc: None,
            output_dir: PathBuf::from("dist"),
            output_exe_name: "MyApplication-Setup.exe".to_string(),
        }
    }
}

pub trait InstallerBackend {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    fn script_extension(&self) -> &'static str;
    fn check_tool_installed(&self) -> ToolStatus;
    fn generate_script(&self, config: &BuildConfig) -> std::io::Result<String>;
    fn build(&self, config: &BuildConfig, log_fn: &mut dyn FnMut(&str))
        -> std::io::Result<PathBuf>;
}

pub mod inno;
pub mod native_sfx;
pub mod nsis;

pub use inno::InnoSetupBackend;
pub use native_sfx::NativeSfxBackend;
pub use nsis::NsisBackend;
