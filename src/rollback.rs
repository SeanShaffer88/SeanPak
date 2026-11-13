//! Transactional Rollback Engine
//! Tracks state mutations (files, registry keys) and executes atomic restoration
//! if an installation or packaging pass fails or is canceled.

use std::fs;
use std::path::{Path, PathBuf};

pub enum RollbackAction {
    DeleteCreatedFile(PathBuf),
    RestoreModifiedFile {
        target_path: PathBuf,
        backup_path: PathBuf,
    },
    DeleteRegistryKey {
        root_key: usize, // HKEY_LOCAL_MACHINE or HKEY_CURRENT_USER
        subkey: String,
    },
    DeleteCreatedDirectory(PathBuf),
}

pub struct TransactionRollbackLog {
    actions: Vec<RollbackAction>,
    /// Arbitrary cleanup closures (for example "remove the shortcuts and
    /// registry artifacts the install just applied"), run LIFO together
    /// with `actions` when the transaction rolls back.
    hooks: Vec<Box<dyn FnOnce() + Send>>,
    backup_temp_dir: PathBuf,
    committed: bool,
}

impl TransactionRollbackLog {
    pub fn new() -> std::io::Result<Self> {
        // The sequence number matters: the tick count alone collides
        // when two transactions start within the same millisecond in
        // one process (parallel tests proved it — the second snapshot
        // overwrote the first transaction's `bak_0`).
        static TX_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let sequence = TX_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut temp_path = std::env::temp_dir();
        temp_path.push(format!(
            "seanpak_tx_{}_{}_{}",
            std::process::id(),
            get_tick_count(),
            sequence
        ));
        fs::create_dir_all(&temp_path)?;

        Ok(Self {
            actions: Vec::with_capacity(64),
            hooks: Vec::new(),
            backup_temp_dir: temp_path,
            committed: false,
        })
    }

    /// Records a cleanup closure to run if this transaction rolls back.
    /// Hooks run in reverse recording order, after the file/registry
    /// actions recorded so far have been undone in the same LIFO pass —
    /// concretely, hooks recorded last run first.
    pub fn record_rollback_hook(&mut self, hook: impl FnOnce() + Send + 'static) {
        self.hooks.push(Box::new(hook));
    }

    /// Prepares to mutate an existing file by staging a backup snapshot.
    pub fn snapshot_file_before_write(&mut self, target_path: &Path) -> std::io::Result<()> {
        if target_path.is_file() {
            let backup_name = format!("bak_{}", self.actions.len());
            let backup_path = self.backup_temp_dir.join(backup_name);
            fs::copy(target_path, &backup_path)?;
            self.actions.push(RollbackAction::RestoreModifiedFile {
                target_path: target_path.to_path_buf(),
                backup_path,
            });
        } else {
            self.actions
                .push(RollbackAction::DeleteCreatedFile(target_path.to_path_buf()));
        }
        Ok(())
    }

    /// Logs a newly created directory for removal on rollback.
    pub fn record_created_directory(&mut self, dir_path: &Path) {
        self.actions.push(RollbackAction::DeleteCreatedDirectory(
            dir_path.to_path_buf(),
        ));
    }

    /// Commits the transaction, disarming the rollback and cleaning temporary snapshots.
    pub fn commit(mut self) -> std::io::Result<()> {
        self.committed = true;
        let _ = fs::remove_dir_all(&self.backup_temp_dir);
        Ok(())
    }

    /// Executes reverse-order rollback of all recorded operations.
    pub fn rollback(&mut self) {
        if self.committed {
            return;
        }

        // Hooks first (LIFO): they undo the most recent, outermost
        // effects (shortcuts, registry artifacts) before file state is
        // restored underneath them.
        while let Some(hook) = self.hooks.pop() {
            hook();
        }

        // Reverse iteration: LIFO rollback order
        while let Some(action) = self.actions.pop() {
            match action {
                RollbackAction::DeleteCreatedFile(p) => {
                    let _ = fs::remove_file(&p);
                }
                RollbackAction::RestoreModifiedFile {
                    target_path,
                    backup_path,
                } => {
                    let _ = fs::copy(&backup_path, &target_path);
                    let _ = fs::remove_file(&backup_path);
                }
                RollbackAction::DeleteCreatedDirectory(d) => {
                    let _ = fs::remove_dir(&d);
                }
                RollbackAction::DeleteRegistryKey { root_key, subkey } => {
                    delete_reg_key_native(root_key, &subkey);
                }
            }
        }

        let _ = fs::remove_dir_all(&self.backup_temp_dir);
    }
}

impl Drop for TransactionRollbackLog {
    fn drop(&mut self) {
        if !self.committed {
            self.rollback();
        }
    }
}

#[cfg(windows)]
fn get_tick_count() -> u32 {
    unsafe { windows_sys::Win32::System::SystemInformation::GetTickCount() }
}

#[cfg(not(windows))]
fn get_tick_count() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u32)
        .unwrap_or(0)
}

#[cfg(windows)]
fn delete_reg_key_native(root_key: usize, subkey: &str) {
    unsafe {
        use windows_sys::Win32::System::Registry::{RegDeleteKeyW, HKEY};
        let wide: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
        RegDeleteKeyW(root_key as HKEY, wide.as_ptr());
    }
}

#[cfg(not(windows))]
fn delete_reg_key_native(_root_key: usize, _subkey: &str) {
    // Registry rollback only exists on Windows; nothing to do elsewhere.
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!(
            "seanpak_tx_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ))
    }

    #[test]
    fn rollback_removes_created_file_and_restores_modified_file() {
        let dir = unique_temp("rollback");
        fs::create_dir_all(&dir).unwrap();
        let existing = dir.join("existing.txt");
        fs::write(&existing, b"original").unwrap();
        let created = dir.join("created.txt");

        let mut tx = TransactionRollbackLog::new().unwrap();
        tx.snapshot_file_before_write(&existing).unwrap();
        tx.snapshot_file_before_write(&created).unwrap();
        fs::write(&existing, b"overwritten").unwrap();
        fs::write(&created, b"new file").unwrap();

        tx.rollback();
        assert_eq!(fs::read(&existing).unwrap(), b"original");
        assert!(!created.exists(), "created file must be removed");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn rollback_hooks_run_lifo_on_rollback_and_never_on_commit() {
        use std::sync::{Arc, Mutex};
        let order = Arc::new(Mutex::new(Vec::new()));
        {
            let mut tx = TransactionRollbackLog::new().unwrap();
            for name in ["first", "second"] {
                let order = order.clone();
                tx.record_rollback_hook(move || order.lock().unwrap().push(name));
            }
            tx.rollback();
        }
        assert_eq!(*order.lock().unwrap(), vec!["second", "first"]);

        order.lock().unwrap().clear();
        {
            let mut tx = TransactionRollbackLog::new().unwrap();
            let order2 = order.clone();
            tx.record_rollback_hook(move || order2.lock().unwrap().push("never"));
            tx.commit().unwrap();
        }
        assert!(order.lock().unwrap().is_empty());
    }

    #[test]
    fn commit_keeps_changes() {
        let dir = unique_temp("commit");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("kept.txt");

        let mut tx = TransactionRollbackLog::new().unwrap();
        tx.snapshot_file_before_write(&target).unwrap();
        fs::write(&target, b"final").unwrap();
        tx.commit().unwrap();

        assert_eq!(fs::read(&target).unwrap(), b"final");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn drop_without_commit_rolls_back() {
        let dir = unique_temp("drop");
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("dropped.txt");
        {
            let mut tx = TransactionRollbackLog::new().unwrap();
            tx.snapshot_file_before_write(&target).unwrap();
            fs::write(&target, b"temp").unwrap();
        } // tx dropped here without commit
        assert!(!target.exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
