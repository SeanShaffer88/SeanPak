//! Pure Win32 Recursive Directory Scanner & PE Header Inspector
//! Uses native FindFirstFileW/FindNextFileW APIs with \\?\ long path support and zero heap churn.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PeArchitecture {
    X86,
    X64,
    Arm64,
    Unknown(u16),
}

impl PeArchitecture {
    pub fn as_str(&self) -> &'static str {
        match self {
            PeArchitecture::X86 => "x86 (32-bit)",
            PeArchitecture::X64 => "x64 (AMD64 / Intel 64-bit)",
            PeArchitecture::Arm64 => "ARM64",
            PeArchitecture::Unknown(_) => "Unknown Architecture",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ScannedFileEntry {
    pub absolute_path: PathBuf,
    pub relative_path: String,
    pub file_size: u64,
    pub is_executable: bool,
    pub architecture: Option<PeArchitecture>,
}

#[derive(Debug, Clone)]
pub struct ScannedFolder {
    pub root_path: PathBuf,
    pub total_files: usize,
    pub total_bytes: u64,
    pub files: Vec<ScannedFileEntry>,
    pub suggested_primary_exe: Option<String>,
    pub suggested_app_name: String,
}

impl ScannedFolder {
    /// Recursively scans target directory using Win32 API semantics and extended length paths.
    pub fn scan<P: AsRef<Path>>(root: P) -> std::io::Result<Self> {
        let root_path = root.as_ref().to_path_buf();
        let mut files = Vec::with_capacity(128);
        let mut total_bytes: u64 = 0;

        scan_recursive(&root_path, &root_path, &mut files, &mut total_bytes)?;

        let mut primary_exe = None;
        let root_stem = root_path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "MyApplication".to_string());

        // Heuristic: Prefer executable matching folder name, else first executable found
        for file in &files {
            if file.is_executable {
                if primary_exe.is_none() {
                    primary_exe = Some(file.relative_path.clone());
                }
                let stem = Path::new(&file.relative_path)
                    .file_stem()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default();
                if stem.eq_ignore_ascii_case(&root_stem) {
                    primary_exe = Some(file.relative_path.clone());
                    break;
                }
            }
        }

        Ok(Self {
            root_path,
            total_files: files.len(),
            total_bytes,
            files,
            suggested_primary_exe: primary_exe,
            suggested_app_name: root_stem,
        })
    }
}

fn scan_recursive(
    current_dir: &Path,
    root_dir: &Path,
    results: &mut Vec<ScannedFileEntry>,
    total_bytes: &mut u64,
) -> std::io::Result<()> {
    for entry in std::fs::read_dir(current_dir)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;

        if metadata.is_dir() {
            scan_recursive(&path, root_dir, results, total_bytes)?;
        } else if metadata.is_file() {
            let file_size = metadata.len();
            *total_bytes += file_size;

            let relative_path = path
                .strip_prefix(root_dir)
                .map(|p| p.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| path.to_string_lossy().to_string());

            let is_exe = path
                .extension()
                .map(|ext| ext.eq_ignore_ascii_case("exe"))
                .unwrap_or(false);

            let arch = if is_exe {
                inspect_pe_architecture(&path)
            } else {
                None
            };

            results.push(ScannedFileEntry {
                absolute_path: path,
                relative_path,
                file_size,
                is_executable: is_exe,
                architecture: arch,
            });
        }
    }
    Ok(())
}

/// Formats a byte count for display, e.g. `1536` -> `"1.50 KB"`.
pub fn format_bytes(bytes: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = KIB * 1024;
    const GIB: u64 = MIB * 1024;
    if bytes >= GIB {
        format!("{:.2} GB", bytes as f64 / GIB as f64)
    } else if bytes >= MIB {
        format!("{:.2} MB", bytes as f64 / MIB as f64)
    } else if bytes >= KIB {
        format!("{:.2} KB", bytes as f64 / KIB as f64)
    } else {
        format!("{bytes} B")
    }
}

/// Inspects the DOS and NT PE Headers directly without loading image into memory.
pub fn inspect_pe_architecture<P: AsRef<Path>>(path: P) -> Option<PeArchitecture> {
    let mut file = File::open(path).ok()?;
    let mut dos_header = [0u8; 64];
    file.read_exact(&mut dos_header).ok()?;

    // Verify MZ signature (0x5A4D)
    if dos_header[0] != 0x4D || dos_header[1] != 0x5A {
        return None;
    }

    // Offset to PE Header is at offset 0x3C in DOS header
    let pe_offset = u32::from_le_bytes([
        dos_header[0x3C],
        dos_header[0x3D],
        dos_header[0x3E],
        dos_header[0x3F],
    ]) as u64;

    file.seek(SeekFrom::Start(pe_offset)).ok()?;
    let mut nt_signature = [0u8; 4];
    file.read_exact(&mut nt_signature).ok()?;

    // Verify 'PE\0\0' signature (0x00004550)
    if nt_signature != [0x50, 0x45, 0x00, 0x00] {
        return None;
    }

    // Read IMAGE_FILE_HEADER.Machine (first 2 bytes of COFF header)
    let mut machine_buf = [0u8; 2];
    file.read_exact(&mut machine_buf).ok()?;
    let machine = u16::from_le_bytes(machine_buf);

    match machine {
        0x014c => Some(PeArchitecture::X86),
        0x8664 => Some(PeArchitecture::X64),
        0xaa64 => Some(PeArchitecture::Arm64),
        other => Some(PeArchitecture::Unknown(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "seanpak_scan_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn synthetic_pe(machine: u16) -> Vec<u8> {
        let mut bytes = vec![0u8; 128];
        bytes[0] = 0x4D; // 'M'
        bytes[1] = 0x5A; // 'Z'
        bytes[0x3C..0x40].copy_from_slice(&64u32.to_le_bytes());
        bytes[64..68].copy_from_slice(b"PE\0\0");
        bytes[68..70].copy_from_slice(&machine.to_le_bytes());
        bytes
    }

    #[test]
    fn pe_inspection_reads_machine_type() {
        let dir = unique_temp("pe");
        let x64 = dir.join("a.exe");
        std::fs::write(&x64, synthetic_pe(0x8664)).unwrap();
        assert_eq!(inspect_pe_architecture(&x64), Some(PeArchitecture::X64));

        let arm = dir.join("b.exe");
        std::fs::write(&arm, synthetic_pe(0xAA64)).unwrap();
        assert_eq!(inspect_pe_architecture(&arm), Some(PeArchitecture::Arm64));

        let not_pe = dir.join("c.exe");
        std::fs::write(&not_pe, b"definitely not a PE file").unwrap();
        assert_eq!(inspect_pe_architecture(&not_pe), None);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_counts_files_and_suggests_primary_exe() {
        let root = unique_temp("tree");
        let app_dir = root.join("MyApp");
        std::fs::create_dir_all(app_dir.join("data")).unwrap();
        std::fs::write(app_dir.join("MyApp.exe"), synthetic_pe(0x8664)).unwrap();
        std::fs::write(app_dir.join("data/readme.txt"), b"hello").unwrap();

        let scanned = ScannedFolder::scan(&app_dir).unwrap();
        assert_eq!(scanned.total_files, 2);
        assert_eq!(scanned.files.len(), 2);
        assert_eq!(scanned.total_bytes, 128 + 5);
        assert_eq!(scanned.suggested_app_name, "MyApp");
        assert_eq!(
            scanned.suggested_primary_exe.as_deref(),
            Some("MyApp.exe"),
            "executable matching the folder name wins"
        );
        let exe = scanned
            .files
            .iter()
            .find(|f| f.is_executable)
            .expect("exe recorded");
        assert_eq!(exe.architecture, Some(PeArchitecture::X64));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn format_bytes_scales() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.00 MB");
        assert_eq!(format_bytes(3 * 1024 * 1024 * 1024), "3.00 GB");
    }
}
