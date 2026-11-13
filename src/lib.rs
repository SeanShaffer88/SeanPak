pub mod backends;
pub mod diag;
pub mod hash;
pub mod install;
pub mod manifest;
pub mod native_dialog;
pub mod overlay;
pub mod pack;
pub mod restart_manager;
pub mod rollback;
pub mod scanner;
pub mod win32_ui;
pub mod wizard;

pub use backends::{
    BackendEngineType, BuildConfig, InnoSetupBackend, InstallerBackend, NativeSfxBackend,
    NsisBackend, PrivilegeLevel, TargetInstallDir, ToolStatus,
};
pub use overlay::{
    FileRecordHeader, InstallOptions, OverlayManifestHeader, OverlayReader, OverlayWriter,
};
pub use restart_manager::RestartManagerSession;
pub use rollback::{RollbackAction, TransactionRollbackLog};
pub use scanner::{inspect_pe_architecture, PeArchitecture, ScannedFileEntry, ScannedFolder};
pub use win32_ui::NativeWin32App;
pub use wizard::InstallWizard;
