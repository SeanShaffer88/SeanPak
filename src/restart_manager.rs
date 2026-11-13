//! Windows Restart Manager API Wrapper
//! Provides zero-failure process unlocking, handle queries, and graceful shutdown/restart
//! of in-use applications prior to file mutation.
//!
//! The real implementation is Windows-only; on other platforms
//! `RestartManagerSession::new` returns `None` so callers degrade gracefully.

#[cfg(windows)]
#[allow(non_snake_case)]
mod imp {
    use std::ffi::c_void;
    use std::path::Path;

    type RmStartSessionFn = unsafe extern "system" fn(
        pSessionHandle: *mut u32,
        dwSessionFlags: u32,
        strSessionKey: *mut u16,
    ) -> u32;

    type RmRegisterResourcesFn = unsafe extern "system" fn(
        dwSessionHandle: u32,
        nFiles: u32,
        rgsFilenames: *const *const u16,
        nApplications: u32,
        rgApplications: *const c_void,
        nServices: u32,
        rgsServiceNames: *const *const u16,
    ) -> u32;

    type RmGetListFn = unsafe extern "system" fn(
        dwSessionHandle: u32,
        pnProcInfoNeeded: *mut u32,
        pnProcInfo: *mut u32,
        rgAffectedApps: *mut c_void,
        lpdwRebootReasons: *mut u32,
    ) -> u32;

    type RmShutdownFn = unsafe extern "system" fn(
        dwSessionHandle: u32,
        lActionFlags: u32,
        fnStatus: *const c_void,
    ) -> u32;

    type RmRestartFn = unsafe extern "system" fn(
        dwSessionHandle: u32,
        dwRestartFlags: u32,
        fnStatus: *const c_void,
    ) -> u32;

    type RmEndSessionFn = unsafe extern "system" fn(dwSessionHandle: u32) -> u32;

    pub struct RestartManagerSession {
        handle: u32,
        rm_end_session: RmEndSessionFn,
        rm_shutdown: RmShutdownFn,
        rm_restart: RmRestartFn,
        rm_register_resources: RmRegisterResourcesFn,
        rm_get_list: RmGetListFn,
    }

    impl RestartManagerSession {
        /// Attempts to dynamically initialize a Restart Manager session.
        /// Gracefully degrades if rstrtmgr.dll is missing on legacy kernels.
        pub fn new() -> Option<Self> {
            unsafe {
                use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};

                let lib_name: Vec<u16> = "rstrtmgr.dll\0".encode_utf16().collect();
                let h_module = LoadLibraryW(lib_name.as_ptr());
                if h_module.is_null() {
                    return None;
                }

                let p_start: RmStartSessionFn = std::mem::transmute(GetProcAddress(
                    h_module,
                    c"RmStartSession".as_ptr() as *const u8,
                )?);
                let p_register: RmRegisterResourcesFn = std::mem::transmute(GetProcAddress(
                    h_module,
                    c"RmRegisterResources".as_ptr() as *const u8,
                )?);
                let p_get_list: RmGetListFn = std::mem::transmute(GetProcAddress(
                    h_module,
                    c"RmGetList".as_ptr() as *const u8,
                )?);
                let p_shutdown: RmShutdownFn = std::mem::transmute(GetProcAddress(
                    h_module,
                    c"RmShutdown".as_ptr() as *const u8,
                )?);
                let p_restart: RmRestartFn = std::mem::transmute(GetProcAddress(
                    h_module,
                    c"RmRestart".as_ptr() as *const u8,
                )?);
                let p_end: RmEndSessionFn = std::mem::transmute(GetProcAddress(
                    h_module,
                    c"RmEndSession".as_ptr() as *const u8,
                )?);

                let mut session_handle = 0u32;
                let mut session_key = [0u16; 64];

                let err = p_start(&mut session_handle, 0, session_key.as_mut_ptr());
                if err != 0 {
                    return None;
                }

                Some(Self {
                    handle: session_handle,
                    rm_end_session: p_end,
                    rm_shutdown: p_shutdown,
                    rm_restart: p_restart,
                    rm_register_resources: p_register,
                    rm_get_list: p_get_list,
                })
            }
        }

        /// Registers a file to detect whether any running process holds an active lock.
        pub fn register_file<P: AsRef<Path>>(&self, file_path: P) -> bool {
            let abs = match std::fs::canonicalize(file_path.as_ref()) {
                Ok(p) => p,
                Err(_) => file_path.as_ref().to_path_buf(),
            };

            let wide_path: Vec<u16> = abs
                .to_string_lossy()
                .encode_utf16()
                .chain(std::iter::once(0))
                .collect();
            let path_ptr = wide_path.as_ptr();

            unsafe {
                let res = (self.rm_register_resources)(
                    self.handle,
                    1,
                    &path_ptr,
                    0,
                    std::ptr::null(),
                    0,
                    std::ptr::null(),
                );
                res == 0
            }
        }

        /// Gracefully stops processes locking registered resources.
        pub fn shutdown_locking_processes(&self) -> bool {
            unsafe {
                // RM_SHUTDOWN_TYPE: RmForceShutdown = 0x1, RmShutdownNormal = 0x0
                let res = (self.rm_shutdown)(self.handle, 0x1, std::ptr::null());
                res == 0
            }
        }

        /// Restarts processes after mutation has completed.
        pub fn restart_processes(&self) -> bool {
            unsafe {
                let res = (self.rm_restart)(self.handle, 0, std::ptr::null());
                res == 0
            }
        }

        /// Returns how many processes Restart Manager reports as using the
        /// registered resources, or `None` if the query itself fails.
        pub fn affected_process_count(&self) -> Option<u32> {
            unsafe {
                let mut needed: u32 = 0;
                let mut count: u32 = 0;
                let mut reasons: u32 = 0;
                // With pnProcInfo = 0 the call reports ERROR_MORE_DATA (234) and
                // fills pnProcInfoNeeded when processes are affected.
                let res = (self.rm_get_list)(
                    self.handle,
                    &mut needed,
                    &mut count,
                    std::ptr::null_mut(),
                    &mut reasons,
                );
                match res {
                    0 => Some(0),
                    234 => Some(needed),
                    _ => None,
                }
            }
        }
    }

    impl Drop for RestartManagerSession {
        fn drop(&mut self) {
            if self.handle != 0 {
                unsafe {
                    (self.rm_end_session)(self.handle);
                }
            }
        }
    }
}

#[cfg(windows)]
pub use imp::RestartManagerSession;

#[cfg(not(windows))]
pub struct RestartManagerSession {
    _private: (),
}

#[cfg(not(windows))]
impl RestartManagerSession {
    pub fn new() -> Option<Self> {
        None
    }

    pub fn register_file<P: AsRef<std::path::Path>>(&self, _file_path: P) -> bool {
        false
    }

    pub fn affected_process_count(&self) -> Option<u32> {
        None
    }

    pub fn shutdown_locking_processes(&self) -> bool {
        false
    }

    pub fn restart_processes(&self) -> bool {
        false
    }
}
