//! Journaled registry and environment writes (v6 power bundle).
//!
//! A manifest may declare `registryWrites` (HKCU-relative keys, `sz`
//! or `dword` values) and `envWrites` (user environment variables).
//! The install operation applies them after the file artifacts and
//! records, for each write, exactly what was there before — value and
//! type, or "absent", plus whether the key itself was created — into
//! the receipt journal ([`RegWriteRecord`] / [`EnvWriteRecord`]).
//! Uninstall (through the receipt's artifact removal) restores those
//! priors exactly: values are written back with their original type,
//! values that were absent are deleted, and a key the install created
//! is deleted again when it is empty of values and subkeys.
//!
//! Writes are per-user only by construction: the manifest validator
//! refuses anything but an HKCU-relative key, and every operation here
//! is rooted at HKEY_CURRENT_USER.
//!
//! Off Windows, applying is an honest no-op returning an empty
//! journal (the same stance the PATH handling takes for non-Windows
//! hosts); the planning and journal types are host-tested.

use crate::overlay::PowerSpec;
use crate::receipt::{EnvWriteRecord, RegWriteRecord};

/// NUL-terminated UTF-16 for the registry API (the same helper shape
/// `sysinstall` uses).
#[cfg(windows)]
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Applies the declared registry and environment writes, capturing
/// each write's prior state first. Returns the journal records for
/// the receipt. Errors before changing anything it can avoid: a
/// registry value whose existing type SeanPak does not model (a
/// multi-string, say) refuses the install rather than destroying data
/// it cannot restore.
#[cfg(windows)]
pub fn apply_writes(
    power: &PowerSpec,
) -> Result<(Vec<RegWriteRecord>, Vec<EnvWriteRecord>), String> {
    use crate::overlay::RegValueKind;
    use windows_sys::Win32::System::Registry::*;
    let mut reg_records: Vec<RegWriteRecord> = Vec::new();
    let mut env_records: Vec<EnvWriteRecord> = Vec::new();

    for write in &power.registry_writes {
        let key_wide = wide(&write.key);
        // Capture the prior value at the key (opening it if it exists).
        let mut hkey: HKEY = std::ptr::null_mut();
        let mut created_key = false;
        let mut prior: Option<(u32, Vec<u8>)> = None;
        let open_rc = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                key_wide.as_ptr(),
                0,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                &mut hkey,
            )
        };
        if open_rc == 0 {
            match read_value_raw(hkey, &write.name) {
                Ok(found) => prior = found,
                Err(e) => {
                    unsafe {
                        RegCloseKey(hkey);
                    }
                    return Err(e);
                }
            }
        } else {
            // The key does not exist yet: create it and say so in the
            // journal, so the uninstall deletes it again if empty.
            let mut disposition: u32 = 0;
            let create_rc = unsafe {
                RegCreateKeyExW(
                    HKEY_CURRENT_USER,
                    key_wide.as_ptr(),
                    0,
                    std::ptr::null(),
                    REG_OPTION_NON_VOLATILE,
                    KEY_QUERY_VALUE | KEY_SET_VALUE,
                    std::ptr::null(),
                    &mut hkey,
                    &mut disposition,
                )
            };
            if create_rc != 0 {
                return Err(format!(
                    "Could not create the registry key \"HKEY_CURRENT_USER\\{}\" (Windows error {create_rc}).",
                    write.key
                ));
            }
            created_key = disposition == REG_CREATED_NEW_KEY;
        }
        // Refuse to overwrite a value of a type we cannot restore.
        if let Some((kind, _)) = &prior {
            if !matches!(*kind, REG_SZ | REG_EXPAND_SZ | REG_DWORD) {
                unsafe {
                    RegCloseKey(hkey);
                }
                return Err(format!(
                    "The registry value \"{}\" in \"HKEY_CURRENT_USER\\{}\" has a type SeanPak does not manage, \
                     so it was left untouched and the install stopped rather than risk losing it.",
                    if write.name.is_empty() { "(default)" } else { &write.name },
                    write.key
                ));
            }
        }
        let (prior_kind, prior_value) = match &prior {
            None => (None, None),
            Some((REG_DWORD, bytes)) => {
                let n = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                (Some("dword".to_string()), Some(format!("{n}")))
            }
            Some((REG_EXPAND_SZ, bytes)) => {
                (Some("expand".to_string()), Some(bytes_to_string(bytes)))
            }
            Some((_, bytes)) => (Some("sz".to_string()), Some(bytes_to_string(bytes))),
        };
        // Write the new value.
        let value_name = wide(&write.name);
        let (vtype, bytes): (u32, Vec<u8>) = match write.kind {
            RegValueKind::Sz => (REG_SZ, string_to_bytes(&write.value)),
            RegValueKind::Dword => {
                let n: u32 = write.value.parse().map_err(|_| {
                    format!(
                        "The manifest's registry value \"{}\" is not a valid dword.",
                        write.value
                    )
                })?;
                (REG_DWORD, n.to_le_bytes().to_vec())
            }
        };
        let set_rc = unsafe {
            RegSetValueExW(
                hkey,
                value_name.as_ptr(),
                0,
                vtype,
                bytes.as_ptr(),
                bytes.len() as u32,
            )
        };
        unsafe {
            RegCloseKey(hkey);
        }
        if set_rc != 0 {
            return Err(format!(
                "Could not write the registry value \"{}\" in \"HKEY_CURRENT_USER\\{}\" (Windows error {set_rc}).",
                if write.name.is_empty() { "(default)" } else { &write.name },
                write.key
            ));
        }
        reg_records.push(RegWriteRecord {
            key: write.key.clone(),
            name: write.name.clone(),
            prior_kind,
            prior_value,
            created_key,
        });
    }

    for write in &power.env_writes {
        env_records.push(apply_env_write(&write.name, &write.value)?);
    }
    if !env_records.is_empty() {
        crate::sysinstall::broadcast_env_change();
    }
    Ok((reg_records, env_records))
}

/// Off-Windows: an honest no-op (there is no per-user registry here);
/// the journal comes back empty.
#[cfg(not(windows))]
pub fn apply_writes(
    _power: &PowerSpec,
) -> Result<(Vec<RegWriteRecord>, Vec<EnvWriteRecord>), String> {
    Ok((Vec::new(), Vec::new()))
}

/// Restores journaled registry and environment writes (uninstall /
/// rollback): prior values are written back with their original
/// type, values that were absent are deleted, keys the install
/// created are deleted when now empty, and the environment change is
/// broadcast once.
#[cfg(windows)]
pub fn remove_writes(reg_records: &[RegWriteRecord], env_records: &[EnvWriteRecord]) {
    use windows_sys::Win32::System::Registry::*;
    // Reverse order: later writes to the same key/value unwind first.
    for record in reg_records.iter().rev() {
        restore_reg_value(record);
    }
    // Delete keys the install created (deepest first as recorded in
    // reverse), but only when they are empty of values and subkeys —
    // the app or another installer may have added data since.
    let mut created: Vec<&RegWriteRecord> = reg_records.iter().filter(|r| r.created_key).collect();
    created.sort_by_key(|r| std::cmp::Reverse(r.key.matches('\\').count()));
    let mut seen: Vec<&str> = Vec::new();
    for record in created {
        if seen.contains(&record.key.as_str()) {
            continue;
        }
        seen.push(record.key.as_str());
        let key_wide = wide(&record.key);
        let mut hkey: HKEY = std::ptr::null_mut();
        let open_rc = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                key_wide.as_ptr(),
                0,
                KEY_QUERY_VALUE,
                &mut hkey,
            )
        };
        if open_rc != 0 {
            continue;
        }
        let mut subkeys: u32 = 0;
        let mut values: u32 = 0;
        let info_rc = unsafe {
            RegQueryInfoKeyW(
                hkey,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut subkeys,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut values,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        unsafe {
            RegCloseKey(hkey);
        }
        if info_rc == 0 && subkeys == 0 && values == 0 {
            unsafe {
                RegDeleteKeyW(HKEY_CURRENT_USER, key_wide.as_ptr());
            }
        }
    }
    let mut env_changed = false;
    for record in env_records.iter().rev() {
        env_changed |= remove_env_value(record);
    }
    if env_changed {
        crate::sysinstall::broadcast_env_change();
    }
}

/// Off-Windows: nothing was ever applied, so nothing to restore.
#[cfg(not(windows))]
pub fn remove_writes(_reg_records: &[RegWriteRecord], _env_records: &[EnvWriteRecord]) {}

/// One journaled registry value restored: prior written back (with
/// its recorded type) or, when there was no prior, deleted.
#[cfg(windows)]
fn restore_reg_value(record: &RegWriteRecord) {
    use windows_sys::Win32::System::Registry::*;
    let key_wide = wide(&record.key);
    let mut hkey: HKEY = std::ptr::null_mut();
    let open_rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_wide.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
    };
    if open_rc != 0 {
        return;
    }
    let value_name = wide(&record.name);
    match (&record.prior_kind, &record.prior_value) {
        (Some(kind), Some(value)) => {
            let (vtype, bytes): (u32, Vec<u8>) = match kind.as_str() {
                "dword" => match value.parse::<u32>() {
                    Ok(n) => (REG_DWORD, n.to_le_bytes().to_vec()),
                    Err(_) => (0, Vec::new()),
                },
                "expand" => (REG_EXPAND_SZ, string_to_bytes(value)),
                _ => (REG_SZ, string_to_bytes(value)),
            };
            if vtype != 0 {
                unsafe {
                    RegSetValueExW(
                        hkey,
                        value_name.as_ptr(),
                        0,
                        vtype,
                        bytes.as_ptr(),
                        bytes.len() as u32,
                    );
                }
            }
        }
        _ => unsafe {
            RegDeleteValueW(hkey, value_name.as_ptr());
        },
    }
    unsafe {
        RegCloseKey(hkey);
    }
}

#[cfg(windows)]
fn read_value_raw(
    hkey: windows_sys::Win32::System::Registry::HKEY,
    name: &str,
) -> Result<Option<(u32, Vec<u8>)>, String> {
    use windows_sys::Win32::System::Registry::*;
    let value_name = wide(name);
    let mut vtype: u32 = 0;
    let mut len: u32 = 0;
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut vtype,
            std::ptr::null_mut(),
            &mut len,
        )
    };
    if rc == 2 {
        // ERROR_FILE_NOT_FOUND: the value is absent.
        return Ok(None);
    }
    if rc != 0 {
        return Err(format!(
            "Could not read the current registry state before writing (Windows error {rc})."
        ));
    }
    let mut buf = vec![0u8; len as usize];
    let rc = unsafe {
        RegQueryValueExW(
            hkey,
            value_name.as_ptr(),
            std::ptr::null_mut(),
            &mut vtype,
            buf.as_mut_ptr(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(format!(
            "Could not read the current registry state before writing (Windows error {rc})."
        ));
    }
    buf.truncate(len as usize);
    if vtype == REG_DWORD && buf.len() < 4 {
        buf.resize(4, 0);
    }
    Ok(Some((vtype, buf)))
}

/// One environment write with its prior captured (HKCU\Environment).
#[cfg(windows)]
fn apply_env_write(name: &str, value: &str) -> Result<EnvWriteRecord, String> {
    use windows_sys::Win32::System::Registry::*;
    let key_wide = wide("Environment");
    let mut hkey: HKEY = std::ptr::null_mut();
    let open_rc = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            key_wide.as_ptr(),
            0,
            std::ptr::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_QUERY_VALUE | KEY_SET_VALUE,
            std::ptr::null(),
            &mut hkey,
            std::ptr::null_mut(),
        )
    };
    if open_rc != 0 {
        return Err(format!(
            "Could not open your environment settings (Windows error {open_rc})."
        ));
    }
    let prior = read_value_raw(hkey, name)?;
    let (prior_value, prior_type) = match prior {
        None => (None, 0),
        Some((vtype, bytes)) => (Some(bytes_to_string(&bytes)), vtype),
    };
    let value_name = wide(name);
    let bytes = string_to_bytes(value);
    let set_rc = unsafe {
        RegSetValueExW(
            hkey,
            value_name.as_ptr(),
            0,
            REG_SZ,
            bytes.as_ptr(),
            bytes.len() as u32,
        )
    };
    unsafe {
        RegCloseKey(hkey);
    }
    if set_rc != 0 {
        return Err(format!(
            "Could not set the environment variable \"{name}\" (Windows error {set_rc})."
        ));
    }
    Ok(EnvWriteRecord {
        name: name.to_string(),
        prior_value,
        prior_type,
    })
}

/// One journaled environment variable restored. Returns whether the
/// stored value actually changed (so the caller broadcasts once).
#[cfg(windows)]
fn remove_env_value(record: &EnvWriteRecord) -> bool {
    use windows_sys::Win32::System::Registry::*;
    let key_wide = wide("Environment");
    let mut hkey: HKEY = std::ptr::null_mut();
    let open_rc = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            key_wide.as_ptr(),
            0,
            KEY_SET_VALUE,
            &mut hkey,
        )
    };
    if open_rc != 0 {
        return false;
    }
    let value_name = wide(&record.name);
    let rc = match &record.prior_value {
        Some(prior) => {
            let vtype = if record.prior_type == 0 {
                REG_SZ
            } else {
                record.prior_type
            };
            let bytes = string_to_bytes(prior);
            unsafe {
                RegSetValueExW(
                    hkey,
                    value_name.as_ptr(),
                    0,
                    vtype,
                    bytes.as_ptr(),
                    bytes.len() as u32,
                )
            }
        }
        None => unsafe { RegDeleteValueW(hkey, value_name.as_ptr()) },
    };
    let changed = rc == 0;
    unsafe {
        RegCloseKey(hkey);
    }
    changed
}

/// REG_SZ bytes: UTF-16LE with the trailing NUL the API expects.
#[cfg(windows)]
fn string_to_bytes(s: &str) -> Vec<u8> {
    let mut bytes: Vec<u8> = s.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    bytes.extend_from_slice(&[0, 0]);
    bytes
}

/// Registry string bytes back to a String (trailing NULs stripped).
#[cfg(windows)]
fn bytes_to_string(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u16::from_le_bytes(*pair))
        .collect();
    String::from_utf16_lossy(&units)
        .trim_end_matches('\0')
        .to_string()
}
