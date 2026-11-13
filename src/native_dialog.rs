//! Zero-Dependency Pure Win32 Shell Folder & File Pickers
//! Directly interfaces with Windows Shell and Common Dialog APIs (shell32.dll / comdlg32.dll).
//! Zero third-party crates, zero runtime bloat.
//!
//! Windows-only: on other platforms every picker returns `None`.

#[cfg(windows)]
#[allow(non_snake_case, clippy::upper_case_acronyms)]
mod imp {
    use std::path::PathBuf;
    use windows_sys::Win32::System::Com::{
        CoInitializeEx, CoTaskMemFree, CoUninitialize, COINIT_APARTMENTTHREADED,
        COINIT_DISABLE_OLE1DDE,
    };
    use windows_sys::Win32::UI::Shell::{
        SHBrowseForFolderW, SHGetPathFromIDListW, BIF_NEWDIALOGSTYLE, BIF_RETURNONLYFSDIRS,
        BROWSEINFOW,
    };

    #[repr(C)]
    struct OPENFILENAMEW {
        lStructSize: u32,
        hwndOwner: *mut std::ffi::c_void,
        hInstance: *mut std::ffi::c_void,
        lpstrFilter: *const u16,
        lpstrCustomFilter: *mut u16,
        nMaxCustFilter: u32,
        nFilterIndex: u32,
        lpstrFile: *mut u16,
        nMaxFile: u32,
        lpstrFileTitle: *mut u16,
        nMaxFileTitle: u32,
        lpstrInitialDir: *const u16,
        lpstrTitle: *const u16,
        Flags: u32,
        nFileOffset: u16,
        nFileExtension: u16,
        lpstrDefExt: *const u16,
        lCustData: isize,
        lpfnHook: *mut std::ffi::c_void,
        lpTemplateName: *const u16,
        pvReserved: *mut std::ffi::c_void,
        dwReserved: u32,
        FlagsEx: u32,
    }

    #[link(name = "comdlg32")]
    extern "system" {
        fn GetOpenFileNameW(lpofn: *mut OPENFILENAMEW) -> i32;
        fn GetSaveFileNameW(lpofn: *mut OPENFILENAMEW) -> i32;
    }

    const OFN_OVERWRITEPROMPT: u32 = 0x00000002;
    const OFN_PATHMUSTEXIST: u32 = 0x00000800;
    const OFN_FILEMUSTEXIST: u32 = 0x00001000;
    const OFN_EXPLORER: u32 = 0x00080000;

    /// Displays the native Windows folder selection dialog.
    pub fn pick_folder() -> Option<PathBuf> {
        unsafe {
            let hr = CoInitializeEx(
                std::ptr::null_mut(),
                (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
            );
            let uninit_needed = hr >= 0;

            let title: Vec<u16> = "Select Application Source Folder\0"
                .encode_utf16()
                .collect();
            let mut display_name = [0u16; 260];

            let bi = BROWSEINFOW {
                hwndOwner: std::ptr::null_mut(),
                pidlRoot: std::ptr::null_mut(),
                pszDisplayName: display_name.as_mut_ptr(),
                lpszTitle: title.as_ptr(),
                ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
                lpfn: None,
                lParam: 0,
                iImage: 0,
            };

            let pidl = SHBrowseForFolderW(&bi);
            let result = if !pidl.is_null() {
                let mut path_buf = [0u16; 32768];
                let success = SHGetPathFromIDListW(pidl, path_buf.as_mut_ptr());
                CoTaskMemFree(pidl as _);

                if success != 0 {
                    let len = (0..).take_while(|&i| path_buf[i] != 0).count();
                    use std::os::windows::ffi::OsStringExt;
                    let os_str = std::ffi::OsString::from_wide(&path_buf[..len]);
                    Some(PathBuf::from(os_str))
                } else {
                    None
                }
            } else {
                None
            };

            if uninit_needed {
                CoUninitialize();
            }

            result
        }
    }

    /// Displays the native Windows file save dialog for target installer output.
    pub fn pick_save_file(default_filename: &str) -> Option<PathBuf> {
        unsafe {
            let mut file_buf = [0u16; 32768];
            let def_chars: Vec<u16> = default_filename.encode_utf16().collect();
            if def_chars.len() < file_buf.len() {
                file_buf[..def_chars.len()].copy_from_slice(&def_chars);
            }

            let filter: Vec<u16> = "Executable Files (*.exe)\0*.exe\0All Files (*.*)\0*.*\0\0"
                .encode_utf16()
                .collect();
            let title: Vec<u16> = "Save SeanPak Installer Executable\0"
                .encode_utf16()
                .collect();
            let def_ext: Vec<u16> = "exe\0".encode_utf16().collect();

            let mut ofn: OPENFILENAMEW = std::mem::zeroed();
            ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
            ofn.lpstrFilter = filter.as_ptr();
            ofn.lpstrFile = file_buf.as_mut_ptr();
            ofn.nMaxFile = file_buf.len() as u32;
            ofn.lpstrTitle = title.as_ptr();
            ofn.lpstrDefExt = def_ext.as_ptr();
            ofn.Flags = OFN_EXPLORER | OFN_PATHMUSTEXIST | OFN_OVERWRITEPROMPT;

            if GetSaveFileNameW(&mut ofn) != 0 {
                let len = (0..).take_while(|&i| file_buf[i] != 0).count();
                use std::os::windows::ffi::OsStringExt;
                let os_str = std::ffi::OsString::from_wide(&file_buf[..len]);
                Some(PathBuf::from(os_str))
            } else {
                None
            }
        }
    }

    /// Displays the native Windows open dialog for selecting custom application icons (.ico).
    pub fn pick_icon_file() -> Option<PathBuf> {
        unsafe {
            let mut file_buf = [0u16; 32768];
            let filter: Vec<u16> = "Icon Files (*.ico)\0*.ico\0All Files (*.*)\0*.*\0\0"
                .encode_utf16()
                .collect();
            let title: Vec<u16> = "Select Application Icon (.ico)\0".encode_utf16().collect();
            let def_ext: Vec<u16> = "ico\0".encode_utf16().collect();

            let mut ofn: OPENFILENAMEW = std::mem::zeroed();
            ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
            ofn.lpstrFilter = filter.as_ptr();
            ofn.lpstrFile = file_buf.as_mut_ptr();
            ofn.nMaxFile = file_buf.len() as u32;
            ofn.lpstrTitle = title.as_ptr();
            ofn.lpstrDefExt = def_ext.as_ptr();
            ofn.Flags = OFN_EXPLORER | OFN_PATHMUSTEXIST | OFN_FILEMUSTEXIST;

            if GetOpenFileNameW(&mut ofn) != 0 {
                let len = (0..).take_while(|&i| file_buf[i] != 0).count();
                use std::os::windows::ffi::OsStringExt;
                let os_str = std::ffi::OsString::from_wide(&file_buf[..len]);
                Some(PathBuf::from(os_str))
            } else {
                None
            }
        }
    }
}

#[cfg(windows)]
pub use imp::{pick_folder, pick_icon_file, pick_save_file};

#[cfg(not(windows))]
pub fn pick_folder() -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(windows))]
pub fn pick_save_file(_default_filename: &str) -> Option<std::path::PathBuf> {
    None
}

#[cfg(not(windows))]
pub fn pick_icon_file() -> Option<std::path::PathBuf> {
    None
}
