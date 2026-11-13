//! System artifacts: the parts of an install that live outside the
//! install directory — Desktop / Start Menu shortcuts, per-user file
//! associations, the per-user PATH entry, and the per-user start-at-login
//! Run entry.
//!
//! [`plan_artifacts`] computes what an install *would* apply from the
//! package's install options (pure apart from resolving the user's
//! Desktop / Programs folders). [`apply_artifacts`] applies a plan and
//! returns an [`AppliedArtifacts`] journal — exactly what was created,
//! including the prior values of anything overwritten.
//! [`remove_artifacts`] consumes that journal (it is stored in the
//! install receipt) and undoes precisely those changes: shortcuts are
//! deleted, an association is only touched while it still points at
//! this install's ProgId, and PATH loses only the exact segment the
//! install added.
//!
//! Everything is per-user (HKCU, user folders): no admin rights needed.
//! Off Windows there is no shell to integrate with, so applying is an
//! honest no-op that records nothing; the pure planning and PATH-string
//! logic is platform-independent and unit-tested.

use std::path::{Path, PathBuf};

use crate::overlay::InstallOptions;
use crate::receipt::AppliedArtifacts;
#[cfg(windows)]
use crate::receipt::{AssocRecord, RunRecord};

#[derive(Debug, Clone, PartialEq)]
pub struct ShortcutSpec {
    pub link_path: PathBuf,
    pub target: PathBuf,
    pub arguments: String,
    pub description: String,
    pub working_dir: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ArtifactPlan {
    pub shortcuts: Vec<ShortcutSpec>,
    /// Extensions (no dot, lowercase) to associate with the primary exe.
    pub associations: Vec<String>,
    pub prog_id: String,
    pub path_dir: Option<PathBuf>,
    /// The primary executable's installed path, when the package has
    /// one — the target of the association open command and of the
    /// start-at-login Run entry.
    pub primary_target: Option<PathBuf>,
    /// Apply the start-at-login Run entry (the option was offered and
    /// the user's final choice is on, and a primary exe exists).
    pub start_at_login: bool,
}

/// ProgId for `app_name`: `SeanPak.<AppName>` with everything outside
/// [A-Za-z0-9.] stripped, so it is always a valid registry key fragment.
pub fn prog_id_for(app_name: &str) -> String {
    let cleaned: String = app_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '.')
        .collect();
    if cleaned.is_empty() {
        "SeanPak.App".to_string()
    } else {
        format!("SeanPak.{cleaned}")
    }
}

/// Splits the manifest's association string (`.txt,.md`) into clean
/// extensions (`txt`, `md`).
pub fn parse_extensions(csv: &str) -> Vec<String> {
    csv.split(',')
        .map(|e| e.trim().trim_start_matches('.').to_lowercase())
        .filter(|e| !e.is_empty() && e.chars().all(|c| c.is_ascii_alphanumeric()))
        .collect()
}

/// Case-insensitive `;`-segment membership for PATH-style values.
pub fn path_contains_segment(path_value: &str, segment: &str) -> bool {
    let wanted = segment.trim().trim_end_matches(['\\', '/']);
    path_value.split(';').any(|part| {
        part.trim()
            .trim_end_matches(['\\', '/'])
            .eq_ignore_ascii_case(wanted)
    })
}

/// Returns `path_value` with `segment` appended, or unchanged when the
/// segment is already present.
pub fn path_add_segment(path_value: &str, segment: &str) -> String {
    if path_contains_segment(path_value, segment) {
        return path_value.to_string();
    }
    let trimmed = path_value.trim_end_matches(';');
    if trimmed.is_empty() {
        segment.to_string()
    } else {
        format!("{trimmed};{segment}")
    }
}

/// Returns `path_value` with every segment equal to `segment` removed —
/// and nothing else. Used at uninstall time for the exact segment the
/// install added.
pub fn path_remove_segment(path_value: &str, segment: &str) -> String {
    let wanted = segment.trim().trim_end_matches(['\\', '/']);
    path_value
        .split(';')
        .filter(|part| {
            !part
                .trim()
                .trim_end_matches(['\\', '/'])
                .eq_ignore_ascii_case(wanted)
        })
        .collect::<Vec<_>>()
        .join(";")
}

/// Builds the artifact plan. `desktop_root` / `programs_root` are the
/// user's Desktop and Start Menu Programs folders (None when unknown —
/// off Windows they are always None, so no shortcuts are planned).
pub fn plan_with_roots(
    options: &InstallOptions,
    app_name: &str,
    dest: &Path,
    primary_rel: Option<&str>,
    desktop_root: Option<&Path>,
    programs_root: Option<&Path>,
) -> ArtifactPlan {
    let mut plan = ArtifactPlan {
        prog_id: prog_id_for(app_name),
        ..Default::default()
    };
    plan.associations = parse_extensions(&options.file_associations());
    if options.wants_path_entry() {
        plan.path_dir = Some(dest.to_path_buf());
    }
    if let Some(primary) = primary_rel {
        let target = dest.join(primary.replace('\\', "/"));
        plan.primary_target = Some(target.clone());
        if options.wants_desktop_shortcut() {
            if let Some(desktop) = desktop_root {
                plan.shortcuts.push(ShortcutSpec {
                    link_path: desktop.join(format!("{app_name}.lnk")),
                    target: target.clone(),
                    arguments: String::new(),
                    description: format!("Open {app_name}"),
                    working_dir: dest.to_path_buf(),
                });
            }
        }
        if options.wants_start_menu_shortcut() {
            if let Some(programs) = programs_root {
                let folder = options.start_menu_folder();
                let folder = if folder.trim().is_empty() {
                    app_name.to_string()
                } else {
                    folder
                };
                let dir = programs.join(&folder);
                plan.shortcuts.push(ShortcutSpec {
                    link_path: dir.join(format!("{app_name}.lnk")),
                    target: target.clone(),
                    arguments: String::new(),
                    description: format!("Open {app_name}"),
                    working_dir: dest.to_path_buf(),
                });
                plan.shortcuts.push(ShortcutSpec {
                    link_path: dir.join(format!("Uninstall {app_name}.lnk")),
                    target: dest.join("Uninstall.exe"),
                    arguments: format!("--uninstall --to \"{}\"", dest.display()),
                    description: format!("Uninstall {app_name}"),
                    working_dir: dest.to_path_buf(),
                });
            }
        }
    }
    plan.start_at_login = options.start_at_login_chosen() && plan.primary_target.is_some();
    plan
}

/// Plans artifacts using the real user folders on Windows.
pub fn plan_artifacts(
    options: &InstallOptions,
    app_name: &str,
    dest: &Path,
    primary_rel: Option<&str>,
) -> ArtifactPlan {
    #[cfg(windows)]
    {
        plan_with_roots(
            options,
            app_name,
            dest,
            primary_rel,
            win::desktop_folder().as_deref(),
            win::programs_folder().as_deref(),
        )
    }
    #[cfg(not(windows))]
    {
        plan_with_roots(options, app_name, dest, primary_rel, None, None)
    }
}

/// Applies a plan, returning the journal of what was actually created.
/// On failure everything applied so far in this call is removed again
/// before the error is returned, so a failed apply leaves no trace.
#[cfg(windows)]
pub fn apply_artifacts(
    plan: &ArtifactPlan,
    app_name: &str,
    dest: &Path,
) -> Result<AppliedArtifacts, String> {
    let mut applied = AppliedArtifacts::default();
    let run = (|| -> Result<(), String> {
        win::ensure_com();
        for spec in &plan.shortcuts {
            if let Some(parent) = spec.link_path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("cannot create {}: {e}", parent.display()))?;
            }
            win::create_shortcut(spec)?;
            applied
                .shortcuts
                .push(spec.link_path.to_string_lossy().into_owned());
        }
        if !plan.associations.is_empty() {
            let primary_target = plan
                .primary_target
                .clone()
                .unwrap_or_else(|| dest.to_path_buf());
            let open_command = format!("\"{}\" \"%1\"", primary_target.display());
            for ext in &plan.associations {
                let record = win::apply_association(ext, &plan.prog_id, app_name, &open_command)?;
                applied.associations.push(record);
            }
        }
        if let Some(dir) = &plan.path_dir {
            applied.path_segment_added = win::apply_path_entry(&dir.to_string_lossy())?;
        }
        if plan.start_at_login {
            if let Some(target) = &plan.primary_target {
                let command = format!("\"{}\"", target.display());
                applied.start_at_login = Some(win::apply_run_entry(app_name, &command)?);
            }
        }
        Ok(())
    })();
    if let Err(e) = run {
        remove_artifacts(&applied);
        return Err(e);
    }
    Ok(applied)
}

/// Off Windows there is no shell integration to apply; the install
/// itself (files + uninstaller + receipt) is unaffected.
#[cfg(not(windows))]
pub fn apply_artifacts(
    _plan: &ArtifactPlan,
    _app_name: &str,
    _dest: &Path,
) -> Result<AppliedArtifacts, String> {
    Ok(AppliedArtifacts::default())
}

/// Undoes exactly what a journal records. Best-effort and careful:
/// associations are only reverted while they still point at the
/// journal's ProgId, PATH loses only the recorded segment, and shortcut
/// files are deleted (their Start Menu folder is removed when it ends
/// up empty).
pub fn remove_artifacts(artifacts: &AppliedArtifacts) {
    for link in &artifacts.shortcuts {
        let path = PathBuf::from(link);
        let _ = std::fs::remove_file(&path);
        if let Some(parent) = path.parent() {
            // Removes the Start Menu app folder when this was its last
            // shortcut; fails harmlessly when anything else lives there.
            let _ = std::fs::remove_dir(parent);
        }
    }
    #[cfg(windows)]
    {
        for record in &artifacts.associations {
            win::remove_association(record);
        }
        if let Some(segment) = &artifacts.path_segment_added {
            win::remove_path_entry(segment);
        }
        if let Some(record) = &artifacts.start_at_login {
            win::remove_run_entry(record);
        }
    }
}

/// Reads this app's Add/Remove Programs entry (HKCU) when present:
/// `(display version, install location)`. Windows only.
#[cfg(windows)]
pub fn arp_install(app_name: &str) -> Option<(String, PathBuf)> {
    win::arp_install(app_name)
}

#[cfg(not(windows))]
pub fn arp_install(_app_name: &str) -> Option<(String, PathBuf)> {
    None
}

// ---------------------------------------------------------------------------
// Windows implementation.

#[cfg(windows)]
mod win {
    use super::*;
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::*;
    use windows_sys::Win32::System::Com::*;
    use windows_sys::Win32::System::Registry::*;
    use windows_sys::Win32::UI::Shell::*;
    use windows_sys::Win32::UI::WindowsAndMessaging::*;

    const CLASSES: &str = "Software\\Classes";
    const ENVIRONMENT_KEY: &str = "Environment";
    const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

    pub fn ensure_com() {
        unsafe {
            // S_OK / S_FALSE / RPC_E_CHANGED_MODE are all fine here.
            let _ = CoInitializeEx(std::ptr::null(), COINIT_APARTMENTTHREADED as u32);
        }
    }

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn folder_path(id: &windows_sys::core::GUID) -> Option<PathBuf> {
        unsafe {
            let mut pwstr: *mut u16 = std::ptr::null_mut();
            let hr = SHGetKnownFolderPath(id, 0, std::ptr::null_mut(), &mut pwstr);
            if hr < 0 || pwstr.is_null() {
                return None;
            }
            let mut len = 0usize;
            while *pwstr.add(len) != 0 {
                len += 1;
            }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(pwstr, len));
            CoTaskMemFree(pwstr as *mut c_void);
            Some(PathBuf::from(text))
        }
    }

    pub fn desktop_folder() -> Option<PathBuf> {
        folder_path(&FOLDERID_Desktop)
    }

    pub fn programs_folder() -> Option<PathBuf> {
        folder_path(&FOLDERID_Programs)
    }

    // -- Registry helpers (HKCU only) ------------------------------------

    fn reg_open(subkey: &str, write: bool) -> Option<HKEY> {
        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            let access = if write {
                KEY_READ | KEY_WRITE
            } else {
                KEY_READ
            };
            let rc = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                wide(subkey).as_ptr(),
                0,
                access,
                &mut hkey,
            );
            if rc == 0 {
                Some(hkey)
            } else {
                None
            }
        }
    }

    fn reg_create(subkey: &str) -> Result<HKEY, String> {
        unsafe {
            let mut hkey: HKEY = std::ptr::null_mut();
            let rc = RegCreateKeyExW(
                HKEY_CURRENT_USER,
                wide(subkey).as_ptr(),
                0,
                std::ptr::null_mut(),
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                std::ptr::null(),
                &mut hkey,
                std::ptr::null_mut(),
            );
            if rc != 0 {
                return Err(format!("cannot open registry key {subkey} (error {rc})"));
            }
            Ok(hkey)
        }
    }

    fn reg_get_sz(subkey: &str, name: Option<&str>) -> Option<(String, u32)> {
        unsafe {
            let hkey = reg_open(subkey, false)?;
            let name_wide = name.map(wide);
            let name_ptr = name_wide
                .as_ref()
                .map(|v| v.as_ptr())
                .unwrap_or(std::ptr::null());
            let mut ty: u32 = 0;
            let mut size: u32 = 0;
            let rc = RegQueryValueExW(
                hkey,
                name_ptr,
                std::ptr::null(),
                &mut ty,
                std::ptr::null_mut(),
                &mut size,
            );
            if rc != 0 || size == 0 {
                RegCloseKey(hkey);
                return None;
            }
            let mut buffer = vec![0u8; size as usize];
            let rc = RegQueryValueExW(
                hkey,
                name_ptr,
                std::ptr::null(),
                &mut ty,
                buffer.as_mut_ptr(),
                &mut size,
            );
            RegCloseKey(hkey);
            if rc != 0 {
                return None;
            }
            let units: Vec<u16> = buffer
                .as_chunks::<2>()
                .0
                .iter()
                .map(|c| u16::from_le_bytes(*c))
                .collect();
            let text = String::from_utf16_lossy(&units);
            Some((text.trim_end_matches('\0').to_string(), ty))
        }
    }

    fn reg_set_sz(subkey: &str, name: Option<&str>, value: &str, ty: u32) -> Result<(), String> {
        unsafe {
            let hkey = reg_create(subkey)?;
            let name_wide = name.map(wide);
            let name_ptr = name_wide
                .as_ref()
                .map(|v| v.as_ptr())
                .unwrap_or(std::ptr::null());
            let value_wide = wide(value);
            let rc = RegSetValueExW(
                hkey,
                name_ptr,
                0,
                ty,
                value_wide.as_ptr() as *const u8,
                (value_wide.len() * 2) as u32,
            );
            RegCloseKey(hkey);
            if rc != 0 {
                return Err(format!(
                    "cannot write registry value in {subkey} (error {rc})"
                ));
            }
            Ok(())
        }
    }

    fn reg_delete_value(subkey: &str, name: Option<&str>) {
        unsafe {
            if let Some(hkey) = reg_open(subkey, true) {
                let name_wide = name.map(wide);
                let name_ptr = name_wide
                    .as_ref()
                    .map(|v| v.as_ptr())
                    .unwrap_or(std::ptr::null());
                RegDeleteValueW(hkey, name_ptr);
                RegCloseKey(hkey);
            }
        }
    }

    fn reg_delete_tree(subkey: &str) {
        unsafe {
            RegDeleteTreeW(HKEY_CURRENT_USER, wide(subkey).as_ptr());
        }
    }

    fn key_exists(subkey: &str) -> bool {
        reg_open(subkey, false)
            .map(|h| unsafe { RegCloseKey(h) })
            .is_some()
    }

    // -- Shortcuts ---------------------------------------------------------
    //
    // windows-sys 0.59 does not expose the IShellLinkW / IPersistFile COM
    // interfaces in this build, so their vtables are declared here and
    // the objects are created with CoCreateInstance — plain FFI, no new
    // dependencies.

    type HResult = i32;
    type ComPtr = *mut c_void;
    type FnQueryInterface =
        unsafe extern "system" fn(ComPtr, *const windows_sys::core::GUID, *mut ComPtr) -> HResult;
    type FnRelease = unsafe extern "system" fn(ComPtr) -> u32;
    type FnSetGetPath = unsafe extern "system" fn(ComPtr, *const u16) -> HResult;
    type FnSave = unsafe extern "system" fn(ComPtr, *const u16, i32) -> HResult;

    #[repr(C)]
    struct IShellLinkWVtbl {
        query_interface: FnQueryInterface,
        add_ref: unsafe extern "system" fn(ComPtr) -> u32,
        release: FnRelease,
        get_path: unsafe extern "system" fn(ComPtr, *mut u16, i32, *mut c_void, u32) -> HResult,
        get_id_list: unsafe extern "system" fn(ComPtr, *mut *mut c_void) -> HResult,
        set_id_list: unsafe extern "system" fn(ComPtr, *const c_void) -> HResult,
        get_description: unsafe extern "system" fn(ComPtr, *mut u16, i32) -> HResult,
        set_description: FnSetGetPath,
        get_working_directory: unsafe extern "system" fn(ComPtr, *mut u16, i32) -> HResult,
        set_working_directory: FnSetGetPath,
        get_arguments: unsafe extern "system" fn(ComPtr, *mut u16, i32) -> HResult,
        set_arguments: FnSetGetPath,
        get_hotkey: unsafe extern "system" fn(ComPtr, *mut u16) -> HResult,
        set_hotkey: unsafe extern "system" fn(ComPtr, u16) -> HResult,
        get_show_cmd: unsafe extern "system" fn(ComPtr, *mut i32) -> HResult,
        set_show_cmd: unsafe extern "system" fn(ComPtr, i32) -> HResult,
        get_icon_location: unsafe extern "system" fn(ComPtr, *mut u16, i32, *mut i32) -> HResult,
        set_icon_location: unsafe extern "system" fn(ComPtr, *const u16, i32) -> HResult,
        set_relative_path: unsafe extern "system" fn(ComPtr, *const u16, u32) -> HResult,
        resolve: unsafe extern "system" fn(ComPtr, HWND, u32) -> HResult,
        set_path: FnSetGetPath,
    }

    #[repr(C)]
    struct IPersistFileVtbl {
        query_interface: FnQueryInterface,
        add_ref: unsafe extern "system" fn(ComPtr) -> u32,
        release: FnRelease,
        get_class_id: unsafe extern "system" fn(ComPtr, *mut windows_sys::core::GUID) -> HResult,
        is_dirty: unsafe extern "system" fn(ComPtr) -> HResult,
        load: unsafe extern "system" fn(ComPtr, *const u16, u32) -> HResult,
        save: FnSave,
        save_completed: unsafe extern "system" fn(ComPtr, *const u16) -> HResult,
        get_cur_file: unsafe extern "system" fn(ComPtr, *mut *mut u16) -> HResult,
    }

    const CLSID_SHELL_LINK: windows_sys::core::GUID =
        windows_sys::core::GUID::from_u128(0x00021401_0000_0000_C000_000000000046);
    const IID_ISHELL_LINK_W: windows_sys::core::GUID =
        windows_sys::core::GUID::from_u128(0x000214F9_0000_0000_C000_000000000046);
    const IID_IPERSIST_FILE: windows_sys::core::GUID =
        windows_sys::core::GUID::from_u128(0x0000010B_0000_0000_C000_000000000046);
    const CLSCTX_INPROC_SERVER: u32 = 0x1;

    pub fn create_shortcut(spec: &ShortcutSpec) -> Result<(), String> {
        unsafe {
            let mut link: ComPtr = std::ptr::null_mut();
            let hr = CoCreateInstance(
                &CLSID_SHELL_LINK,
                std::ptr::null_mut(),
                CLSCTX_INPROC_SERVER,
                &IID_ISHELL_LINK_W,
                &mut link,
            );
            if hr < 0 || link.is_null() {
                return Err(format!("cannot create shortcut object (HRESULT {hr:#x})"));
            }
            let vtbl = *(link as *const *const IShellLinkWVtbl);
            let fail = |hr: HResult, what: &str| -> String {
                format!("shortcut {what} failed (HRESULT {hr:#x})")
            };
            let mut result: Result<(), String> = Ok(());
            let hr = ((*vtbl).set_path)(link, wide(&spec.target.to_string_lossy()).as_ptr());
            if hr < 0 {
                result = Err(fail(hr, "SetPath"));
            }
            if result.is_ok() && !spec.arguments.is_empty() {
                let hr = ((*vtbl).set_arguments)(link, wide(&spec.arguments).as_ptr());
                if hr < 0 {
                    result = Err(fail(hr, "SetArguments"));
                }
            }
            if result.is_ok() {
                let hr = ((*vtbl).set_working_directory)(
                    link,
                    wide(&spec.working_dir.to_string_lossy()).as_ptr(),
                );
                if hr < 0 {
                    result = Err(fail(hr, "SetWorkingDirectory"));
                }
            }
            if result.is_ok() && !spec.description.is_empty() {
                let hr = ((*vtbl).set_description)(link, wide(&spec.description).as_ptr());
                if hr < 0 {
                    result = Err(fail(hr, "SetDescription"));
                }
            }
            if result.is_ok() {
                let mut persist: ComPtr = std::ptr::null_mut();
                let hr = ((*vtbl).query_interface)(link, &IID_IPERSIST_FILE, &mut persist);
                if hr < 0 || persist.is_null() {
                    result = Err(fail(hr, "QueryInterface(IPersistFile)"));
                } else {
                    let pvtbl = *(persist as *const *const IPersistFileVtbl);
                    let hr = ((*pvtbl).save)(
                        persist,
                        wide(&spec.link_path.to_string_lossy()).as_ptr(),
                        1,
                    );
                    if hr < 0 {
                        result = Err(fail(hr, "Save"));
                    }
                    ((*pvtbl).release)(persist);
                }
            }
            ((*vtbl).release)(link);
            result
        }
    }

    // -- File associations --------------------------------------------------

    pub fn apply_association(
        ext: &str,
        prog_id: &str,
        app_name: &str,
        open_command: &str,
    ) -> Result<AssocRecord, String> {
        let ext_key = format!("{CLASSES}\\.{ext}");
        let prog_key = format!("{CLASSES}\\{prog_id}");
        let record = AssocRecord {
            extension: ext.to_string(),
            prog_id: prog_id.to_string(),
            prior_default: reg_get_sz(&ext_key, None).map(|(v, _)| v),
            created_ext_key: !key_exists(&ext_key),
            created_prog_id: !key_exists(&prog_key),
        };
        reg_set_sz(&ext_key, None, prog_id, REG_SZ)?;
        reg_set_sz(&prog_key, None, &format!("{app_name} file"), REG_SZ)?;
        reg_set_sz(
            &format!("{prog_key}\\shell\\open\\command"),
            None,
            open_command,
            REG_SZ,
        )?;
        Ok(record)
    }

    pub fn remove_association(record: &AssocRecord) {
        let ext_key = format!("{CLASSES}\\.{0}", record.extension);
        let current = reg_get_sz(&ext_key, None).map(|(v, _)| v);
        // Only revert while the extension still points at our ProgId —
        // if the user (or another app) changed it since, leave it alone.
        if current.as_deref() == Some(record.prog_id.as_str()) {
            match (&record.prior_default, record.created_ext_key) {
                (Some(prior), _) => {
                    let _ = reg_set_sz(&ext_key, None, prior, REG_SZ);
                }
                (None, true) => reg_delete_tree(&ext_key),
                (None, false) => reg_delete_value(&ext_key, None),
            }
        }
        if record.created_prog_id {
            reg_delete_tree(&format!("{CLASSES}\\{0}", record.prog_id));
        }
    }

    // -- PATH (HKCU\Environment) ---------------------------------------------

    fn broadcast_environment_change() {
        unsafe {
            let env_wide = wide("Environment");
            let mut result: usize = 0;
            SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                0,
                env_wide.as_ptr() as LPARAM,
                SMTO_ABORTIFHUNG,
                5000,
                &mut result,
            );
        }
    }

    pub fn apply_path_entry(segment: &str) -> Result<Option<String>, String> {
        let (old, ty) = reg_get_sz(ENVIRONMENT_KEY, Some("Path"))
            .unwrap_or_else(|| (String::new(), REG_EXPAND_SZ));
        if super::path_contains_segment(&old, segment) {
            return Ok(None);
        }
        let new = super::path_add_segment(&old, segment);
        reg_set_sz(ENVIRONMENT_KEY, Some("Path"), &new, ty)?;
        broadcast_environment_change();
        Ok(Some(segment.to_string()))
    }

    pub fn remove_path_entry(segment: &str) {
        if let Some((old, ty)) = reg_get_sz(ENVIRONMENT_KEY, Some("Path")) {
            let new = super::path_remove_segment(&old, segment);
            if new != old && reg_set_sz(ENVIRONMENT_KEY, Some("Path"), &new, ty).is_ok() {
                broadcast_environment_change();
            }
        }
    }

    // -- Start at login (HKCU Run) ------------------------------------------

    /// Writes the Run value named `app_name` with `command`, recording
    /// the value it replaced (if any) so removal can restore it.
    pub fn apply_run_entry(app_name: &str, command: &str) -> Result<RunRecord, String> {
        let prior_value = reg_get_sz(RUN_KEY, Some(app_name)).map(|(v, _)| v);
        reg_set_sz(RUN_KEY, Some(app_name), command, REG_SZ)?;
        Ok(RunRecord {
            name: app_name.to_string(),
            command: command.to_string(),
            prior_value,
        })
    }

    /// Removes the Run entry an install applied — only while the value
    /// still holds the recorded command (a value the user or another
    /// tool has since pointed elsewhere is left alone). Restores the
    /// value the install replaced, when there was one.
    pub fn remove_run_entry(record: &RunRecord) {
        let current = reg_get_sz(RUN_KEY, Some(&record.name)).map(|(v, _)| v);
        if current.as_deref() == Some(record.command.as_str()) {
            match &record.prior_value {
                Some(prior) => {
                    let _ = reg_set_sz(RUN_KEY, Some(&record.name), prior, REG_SZ);
                }
                None => reg_delete_value(RUN_KEY, Some(&record.name)),
            }
        }
    }

    // -- ARP detection --------------------------------------------------------

    pub fn arp_install(app_name: &str) -> Option<(String, PathBuf)> {
        let key = crate::wizard::uninstall_key_path(app_name);
        let version = reg_get_sz(&key, Some("DisplayVersion")).map(|(v, _)| v)?;
        let location = reg_get_sz(&key, Some("InstallLocation"))
            .map(|(v, _)| PathBuf::from(v))
            .unwrap_or_default();
        Some((version, location))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_segment_logic_is_exact_and_case_insensitive() {
        let value = "C:\\Tools;C:\\Apps\\My App;D:\\Bin";
        assert!(path_contains_segment(value, "c:\\apps\\my app"));
        assert!(path_contains_segment(value, "C:\\Apps\\My App\\"));
        assert!(!path_contains_segment(value, "C:\\Apps\\My"));
        assert_eq!(
            path_add_segment(value, "E:\\New"),
            "C:\\Tools;C:\\Apps\\My App;D:\\Bin;E:\\New"
        );
        assert_eq!(path_add_segment(value, "C:\\Tools"), value);
        assert_eq!(path_add_segment("", "E:\\New"), "E:\\New");
        assert_eq!(
            path_remove_segment(value, "C:\\Apps\\My App"),
            "C:\\Tools;D:\\Bin"
        );
        assert_eq!(path_remove_segment(value, "Z:\\Nope"), value);
        assert_eq!(
            path_remove_segment("C:\\Apps\\My App", "C:\\Apps\\My App"),
            ""
        );
    }

    #[test]
    fn extensions_and_prog_id_are_cleaned() {
        assert_eq!(
            parse_extensions(".txt, MD ,, .json"),
            vec!["txt", "md", "json"]
        );
        assert_eq!(parse_extensions(""), Vec::<String>::new());
        assert_eq!(prog_id_for("My App!"), "SeanPak.MyApp");
        assert_eq!(prog_id_for("Dev.Text"), "SeanPak.Dev.Text");
        assert_eq!(prog_id_for("!!!"), "SeanPak.App");
    }

    #[test]
    fn plan_with_roots_builds_expected_shortcuts() {
        let dest = Path::new("C:\\Apps\\My App");
        let mut options = InstallOptions::defaults_for("My App");
        options.path_entry = 1;
        crate::overlay::copy_str_to_fixed_pub(&mut options.file_associations_utf8, ".txt,.md");
        let plan = plan_with_roots(
            &options,
            "My App",
            dest,
            Some("myapp.exe"),
            Some(Path::new("C:\\Users\\x\\Desktop")),
            Some(Path::new("C:\\Users\\x\\Programs")),
        );
        assert_eq!(plan.shortcuts.len(), 3); // desktop + start menu app + start menu uninstall
        assert_eq!(
            plan.shortcuts[0].link_path,
            Path::new("C:\\Users\\x\\Desktop").join("My App.lnk")
        );
        assert_eq!(plan.shortcuts[1].target, dest.join("myapp.exe"));
        assert_eq!(plan.shortcuts[2].target, dest.join("Uninstall.exe"));
        assert_eq!(plan.associations, vec!["txt", "md"]);
        assert_eq!(plan.path_dir.as_deref(), Some(dest));
        assert_eq!(plan.prog_id, "SeanPak.MyApp");
        assert_eq!(
            plan.primary_target.as_deref(),
            Some(dest.join("myapp.exe").as_path())
        );
        // Start-at-login is not offered by default.
        assert!(!plan.start_at_login);
    }

    #[test]
    fn plan_start_at_login_follows_the_effective_choice() {
        let dest = Path::new("C:\\Apps\\My App");
        let mut options = InstallOptions::defaults_for("My App");
        options.start_at_login = 1;
        options.start_at_login_default = 1;
        let plan = plan_with_roots(&options, "My App", dest, Some("myapp.exe"), None, None);
        assert!(plan.start_at_login);
        assert_eq!(
            plan.primary_target.as_deref(),
            Some(dest.join("myapp.exe").as_path())
        );

        // Offered but defaulted off (or turned off in the wizard): no entry.
        options.start_at_login_default = 0;
        let plan = plan_with_roots(&options, "My App", dest, Some("myapp.exe"), None, None);
        assert!(!plan.start_at_login);

        // No primary exe: nothing to point the Run entry at.
        options.start_at_login_default = 1;
        let plan = plan_with_roots(&options, "My App", dest, None, None, None);
        assert!(!plan.start_at_login);
        assert!(plan.primary_target.is_none());
    }
}
