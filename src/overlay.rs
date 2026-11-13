//! Direct PE Overlay Engine (per-file compressed payloads since v4)
//! Appends raw sector-aligned file payloads directly to the host PE executable overlay.
//! Provides maximum sustained NVMe throughput with 0 heap allocation in write loops.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const OVERLAY_MAGIC: &[u8; 16] = b"SEANPAK_OVL\0\0\0\0\0";
/// Magic written by CrucibleKit-era builds (3.x). The reader still accepts
/// it so packages built before the SeanPak rename can be listed, extracted,
/// and installed; new packages are always written with `OVERLAY_MAGIC`.
pub const CRUCIBLEKIT_OVERLAY_MAGIC: &[u8; 16] = b"CRUCIBLEKIT_OVL\0";
/// Magic written by pre-rename (RustPak-era) builds. Kept readable for the
/// same reason as the CrucibleKit magic.
pub const LEGACY_OVERLAY_MAGIC: &[u8; 16] = b"RUSTPAK_OVERLAY\0";
/// Every legacy magic the reader accepts, newest first.
pub const LEGACY_OVERLAY_MAGICS: &[&[u8; 16]] = &[CRUCIBLEKIT_OVERLAY_MAGIC, LEGACY_OVERLAY_MAGIC];

/// True when `magic` identifies a SeanPak overlay of any product era.
fn is_overlay_magic(magic: &[u8; 16]) -> bool {
    magic == OVERLAY_MAGIC || LEGACY_OVERLAY_MAGICS.contains(&magic)
}
pub const SECTOR_ALIGNMENT: u64 = 4096;
pub const BUFFER_SIZE: usize = 65536; // 64 KB stack/fixed buffer

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FileRecordHeader {
    pub relative_path_utf8: [u8; 260],
    pub file_size: u64,
    pub payload_offset: u64,
    pub attributes: u32,
    pub is_executable: u8,
    pub reserved: [u8; 3],
}

impl Default for FileRecordHeader {
    fn default() -> Self {
        Self {
            relative_path_utf8: [0u8; 260],
            file_size: 0,
            payload_offset: 0,
            attributes: 0,
            is_executable: 0,
            reserved: [0u8; 3],
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct OverlayManifestHeader {
    pub magic: [u8; 16],
    pub version: u32,
    pub file_count: u32,
    pub primary_exe_index: u32,
    pub total_uncompressed_bytes: u64,
    pub app_name_utf8: [u8; 128],
    pub app_version_utf8: [u8; 32],
    pub publisher_utf8: [u8; 128],
}

/// Reads a NUL-terminated fixed-size UTF-8 field as an owned string.
pub fn fixed_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

impl OverlayManifestHeader {
    pub fn app_name(&self) -> String {
        fixed_str(&self.app_name_utf8)
    }

    pub fn app_version(&self) -> String {
        fixed_str(&self.app_version_utf8)
    }

    pub fn publisher(&self) -> String {
        fixed_str(&self.publisher_utf8)
    }
}

impl FileRecordHeader {
    pub fn relative_path(&self) -> String {
        fixed_str(&self.relative_path_utf8)
    }

    /// Compression codec of the stored payload (manifest v4+; the first
    /// reserved byte). Packages written before v4 always carry 0
    /// ([`crate::codec::CODEC_STORED`]), so the same accessor is correct
    /// for every readable package.
    pub fn compression(&self) -> u8 {
        self.reserved[0]
    }

    pub fn set_compression(&mut self, codec: u8) {
        self.reserved[0] = codec;
    }
}

/// Install-time choices recorded in a version 3 overlay, stored as a fixed
/// block immediately after the file-record table (payload offsets account
/// for it). Version 2 packages have no block; reading one yields
/// [`InstallOptions::defaults_for`] for its app name, so legacy packages
/// stay installable with sensible defaults.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct InstallOptions {
    pub install_location_utf8: [u8; 260],
    pub start_menu_folder_utf8: [u8; 128],
    pub file_associations_utf8: [u8; 128],
    pub desktop_shortcut: u8,
    pub start_menu_shortcut: u8,
    pub path_entry: u8,
    pub reserved: [u8; 5],
}

impl InstallOptions {
    /// Defaults matching what earlier SeanPak/CrucibleKit installers did:
    /// per-app folder under Program Files, Start Menu entry on, desktop
    /// shortcut on, no file associations, no PATH entry.
    pub fn defaults_for(app_name: &str) -> Self {
        let mut options = Self {
            install_location_utf8: [0u8; 260],
            start_menu_folder_utf8: [0u8; 128],
            file_associations_utf8: [0u8; 128],
            desktop_shortcut: 1,
            start_menu_shortcut: 1,
            path_entry: 0,
            reserved: [0u8; 5],
        };
        copy_str_to_fixed(
            &mut options.install_location_utf8,
            &format!("C:\\Program Files\\{app_name}"),
        );
        copy_str_to_fixed(&mut options.start_menu_folder_utf8, app_name);
        options
    }

    pub fn install_location(&self) -> String {
        fixed_str(&self.install_location_utf8)
    }

    pub fn start_menu_folder(&self) -> String {
        fixed_str(&self.start_menu_folder_utf8)
    }

    pub fn file_associations(&self) -> String {
        fixed_str(&self.file_associations_utf8)
    }

    pub fn wants_desktop_shortcut(&self) -> bool {
        self.desktop_shortcut != 0
    }

    pub fn wants_start_menu_shortcut(&self) -> bool {
        self.start_menu_shortcut != 0
    }

    pub fn wants_path_entry(&self) -> bool {
        self.path_entry != 0
    }
}

pub struct OverlayWriter;

impl OverlayWriter {
    /// Scans `source_dir`, then packs every file found into a sector-aligned
    /// overlay appended to a copy of `stub_binary_path` at `output_exe_path`.
    /// The output file itself is excluded when it lives inside `source_dir`.
    /// Returns the total size of the written executable.
    pub fn build_from_folder(
        stub_binary_path: &Path,
        output_exe_path: &Path,
        source_dir: &Path,
        app_name: &str,
        version: &str,
        publisher: &str,
    ) -> std::io::Result<u64> {
        Self::build_from_folder_with_options(
            stub_binary_path,
            output_exe_path,
            source_dir,
            app_name,
            version,
            publisher,
            &InstallOptions::defaults_for(app_name),
        )
    }

    /// Like [`OverlayWriter::build_from_folder`], but records explicit
    /// install-time `options` in the package (options block, since manifest version 3) instead of
    /// the per-app defaults.
    #[allow(clippy::too_many_arguments)]
    pub fn build_from_folder_with_options(
        stub_binary_path: &Path,
        output_exe_path: &Path,
        source_dir: &Path,
        app_name: &str,
        version: &str,
        publisher: &str,
        options: &InstallOptions,
    ) -> std::io::Result<u64> {
        let scanned = crate::scanner::ScannedFolder::scan(source_dir)?;
        let output_key = std::fs::canonicalize(output_exe_path)
            .unwrap_or_else(|_| output_exe_path.to_path_buf());
        let mut files_to_pack = Vec::with_capacity(scanned.files.len());
        for f in &scanned.files {
            let key =
                std::fs::canonicalize(&f.absolute_path).unwrap_or_else(|_| f.absolute_path.clone());
            if key == output_key {
                continue;
            }
            let is_primary = scanned.suggested_primary_exe.as_ref() == Some(&f.relative_path);
            files_to_pack.push((f.absolute_path.clone(), f.relative_path.clone(), is_primary));
        }
        Self::build_overlay(
            stub_binary_path,
            output_exe_path,
            source_dir,
            &files_to_pack,
            app_name,
            version,
            publisher,
            options,
        )
    }

    /// Packs source directory files into a standalone PE output executable by appending
    /// a sector-aligned direct binary overlay.
    #[allow(clippy::too_many_arguments)]
    pub fn build_overlay(
        stub_binary_path: &Path,
        output_exe_path: &Path,
        _source_dir: &Path,
        files: &[(PathBuf, String, bool)], // (absolute_path, relative_path, is_primary)
        app_name: &str,
        version: &str,
        publisher: &str,
        options: &InstallOptions,
    ) -> std::io::Result<u64> {
        // Copy host stub executable to target output path
        std::fs::copy(stub_binary_path, output_exe_path)?;

        let mut out_file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(output_exe_path)?;

        // Seek to end of PE file
        let pe_end_offset = out_file.seek(SeekFrom::End(0))?;

        // Align to sector boundary (4096 bytes)
        let aligned_overlay_start =
            (pe_end_offset + (SECTOR_ALIGNMENT - 1)) & !(SECTOR_ALIGNMENT - 1);
        let padding_needed = (aligned_overlay_start - pe_end_offset) as usize;
        if padding_needed > 0 {
            let pad = [0u8; 4096];
            out_file.write_all(&pad[..padding_needed])?;
        }

        let mut manifest = OverlayManifestHeader {
            magic: *OVERLAY_MAGIC,
            version: 4,
            file_count: files.len() as u32,
            primary_exe_index: 0,
            total_uncompressed_bytes: 0,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };

        // Copy strings to fixed-size buffers
        copy_str_to_fixed(&mut manifest.app_name_utf8, app_name);
        copy_str_to_fixed(&mut manifest.app_version_utf8, version);
        copy_str_to_fixed(&mut manifest.publisher_utf8, publisher);

        let manifest_size = std::mem::size_of::<OverlayManifestHeader>() as u64;
        let records_total_size = (files.len() * std::mem::size_of::<FileRecordHeader>()) as u64;
        let options_size = std::mem::size_of::<InstallOptions>() as u64;
        let mut current_payload_offset =
            aligned_overlay_start + manifest_size + records_total_size + options_size;

        let mut record_headers = Vec::with_capacity(files.len());
        let mut total_bytes: u64 = 0;
        // Stored payloads, in record order. Payloads are compressed per
        // file (manifest v4), so each file is buffered once at pack time;
        // `file_size` in the record stays the ORIGINAL size and the stored
        // length is recoverable from the next record's offset (payloads
        // are laid out contiguously, as they always have been).
        let mut stored_payloads: Vec<Vec<u8>> = Vec::with_capacity(files.len());

        for (idx, (abs_path, rel_path, is_primary)) in files.iter().enumerate() {
            let raw = std::fs::read(abs_path)?;
            let file_size = raw.len() as u64;
            total_bytes += file_size;
            let (codec, stored) = crate::codec::compress_best(&raw);

            let mut header = FileRecordHeader {
                file_size,
                payload_offset: current_payload_offset,
                attributes: 0x80, // FILE_ATTRIBUTE_NORMAL
                is_executable: if *is_primary { 1 } else { 0 },
                ..Default::default()
            };
            header.set_compression(codec);

            copy_str_to_fixed(&mut header.relative_path_utf8, rel_path);
            if *is_primary {
                manifest.primary_exe_index = idx as u32;
            }

            record_headers.push(header);
            current_payload_offset += stored.len() as u64;
            stored_payloads.push(stored);
        }

        manifest.total_uncompressed_bytes = total_bytes;

        // Write manifest header
        let manifest_bytes = unsafe {
            std::slice::from_raw_parts(
                &manifest as *const OverlayManifestHeader as *const u8,
                std::mem::size_of::<OverlayManifestHeader>(),
            )
        };
        out_file.write_all(manifest_bytes)?;

        // Write file record headers table
        for header in &record_headers {
            let header_bytes = unsafe {
                std::slice::from_raw_parts(
                    header as *const FileRecordHeader as *const u8,
                    std::mem::size_of::<FileRecordHeader>(),
                )
            };
            out_file.write_all(header_bytes)?;
        }

        // Write the version 3 install-options block after the record table.
        let options_bytes = unsafe {
            std::slice::from_raw_parts(
                options as *const InstallOptions as *const u8,
                std::mem::size_of::<InstallOptions>(),
            )
        };
        out_file.write_all(options_bytes)?;

        // Write the stored (possibly compressed) payloads in record order.
        for stored in &stored_payloads {
            out_file.write_all(stored)?;
        }

        out_file.flush()?;
        let total_written = out_file.seek(SeekFrom::End(0))?;
        Ok(total_written)
    }
}

pub struct OverlayReader;

impl OverlayReader {
    /// Discovers and validates the direct overlay table within the host executable.
    pub fn read_manifest<R: Read + Seek>(
        reader: R,
    ) -> std::io::Result<Option<(OverlayManifestHeader, Vec<FileRecordHeader>)>> {
        match Self::read_package(reader)? {
            Some((manifest, records, _options)) => Ok(Some((manifest, records))),
            None => Ok(None),
        }
    }

    /// Like [`OverlayReader::read_manifest`], but also returns the package's
    /// install options. Version 3 packages carry an explicit options block;
    /// older packages (any accepted magic, manifest version 2) yield
    /// [`InstallOptions::defaults_for`] for the recorded app name.
    pub fn read_package<R: Read + Seek>(
        mut reader: R,
    ) -> std::io::Result<Option<(OverlayManifestHeader, Vec<FileRecordHeader>, InstallOptions)>>
    {
        let stream_len = reader.seek(SeekFrom::End(0))?;
        let manifest_size = std::mem::size_of::<OverlayManifestHeader>() as u64;

        if stream_len < manifest_size {
            return Ok(None);
        }

        // Search backward from end or scan sectors
        let mut search_offset = 0u64;
        let mut found = None;

        while search_offset + manifest_size <= stream_len {
            reader.seek(SeekFrom::Start(search_offset))?;
            let mut magic_buf = [0u8; 16];
            if reader.read_exact(&mut magic_buf).is_ok() && is_overlay_magic(&magic_buf) {
                found = Some(search_offset);
                break;
            }
            search_offset += SECTOR_ALIGNMENT;
        }

        let overlay_start = match found {
            Some(o) => o,
            None => return Ok(None),
        };

        reader.seek(SeekFrom::Start(overlay_start))?;
        let mut manifest = OverlayManifestHeader {
            magic: [0u8; 16],
            version: 0,
            file_count: 0,
            primary_exe_index: 0,
            total_uncompressed_bytes: 0,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };

        let manifest_slice = unsafe {
            std::slice::from_raw_parts_mut(
                &mut manifest as *mut OverlayManifestHeader as *mut u8,
                std::mem::size_of::<OverlayManifestHeader>(),
            )
        };
        reader.read_exact(manifest_slice)?;

        let mut records = Vec::with_capacity(manifest.file_count as usize);
        for _ in 0..manifest.file_count {
            let mut record = FileRecordHeader::default();
            let record_slice = unsafe {
                std::slice::from_raw_parts_mut(
                    &mut record as *mut FileRecordHeader as *mut u8,
                    std::mem::size_of::<FileRecordHeader>(),
                )
            };
            reader.read_exact(record_slice)?;
            records.push(record);
        }

        let options = if manifest.version >= 3 {
            let mut options = InstallOptions::defaults_for(&manifest.app_name());
            let options_slice = unsafe {
                std::slice::from_raw_parts_mut(
                    &mut options as *mut InstallOptions as *mut u8,
                    std::mem::size_of::<InstallOptions>(),
                )
            };
            reader.read_exact(options_slice)?;
            options
        } else {
            InstallOptions::defaults_for(&manifest.app_name())
        };

        Ok(Some((manifest, records, options)))
    }

    /// The stored (on-disk) length of `records[index]`'s payload.
    /// Payloads are laid out contiguously in record order in every format
    /// version, so a payload runs to the next payload's offset — or to
    /// the end of the stream for the last one. For uncompressed records
    /// this equals `file_size`; for v4 compressed records it is the
    /// compressed size.
    pub fn stored_len(records: &[FileRecordHeader], index: usize, stream_len: u64) -> u64 {
        let start = records[index].payload_offset;
        let mut end = stream_len;
        for record in records {
            if record.payload_offset > start {
                end = end.min(record.payload_offset);
            }
        }
        end.saturating_sub(start)
    }

    /// Reads and decompresses one record's payload, returning the
    /// original file bytes (exactly `record.file_size` of them).
    pub fn read_payload<R: Read + Seek>(
        reader: &mut R,
        records: &[FileRecordHeader],
        index: usize,
        stream_len: u64,
    ) -> std::io::Result<Vec<u8>> {
        let record = &records[index];
        let stored_len = Self::stored_len(records, index, stream_len);
        reader.seek(SeekFrom::Start(record.payload_offset))?;
        let mut stored = vec![0u8; stored_len as usize];
        reader.read_exact(&mut stored)?;
        crate::codec::decompress(record.compression(), &stored, record.file_size as usize)
    }

    /// Extracts every file recorded in the overlay into `dest_dir`, creating
    /// subdirectories as needed and returning the paths written, in record
    /// order. Record paths are validated before anything is written for that
    /// record: absolute paths and `..` traversal are rejected, so a crafted
    /// overlay cannot write outside `dest_dir`.
    pub fn extract<R: Read + Seek>(reader: R, dest_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        Self::extract_with_control(reader, dest_dir, &mut ExtractControl::default())
    }

    /// Like [`OverlayReader::extract`], with per-file progress and
    /// cancellation: `control.on_file` runs before each file is written,
    /// and a set `control.cancel` flag aborts the extraction between
    /// files with an `Interrupted` error (files already written stay
    /// written — callers roll back).
    pub fn extract_with_control<R: Read + Seek>(
        reader: R,
        dest_dir: &Path,
        control: &mut ExtractControl<'_>,
    ) -> std::io::Result<Vec<PathBuf>> {
        let mut reader = reader;
        let (_manifest, records) = Self::read_manifest(&mut reader)?.ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "no SeanPak overlay found in this file",
            )
        })?;
        let stream_len = reader.seek(SeekFrom::End(0))?;

        std::fs::create_dir_all(dest_dir)?;
        let mut written = Vec::with_capacity(records.len());
        for (idx, record) in records.iter().enumerate() {
            if let Some(cancel) = control.cancel {
                if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::Interrupted,
                        "extraction cancelled",
                    ));
                }
            }
            let rel = record.relative_path();
            if let Some(on_file) = control.on_file.as_deref_mut() {
                on_file(idx, records.len(), &rel);
            }
            let target = safe_join(dest_dir, &rel)?;
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = Self::read_payload(&mut reader, &records, idx, stream_len)?;
            std::fs::write(&target, &bytes)?;
            written.push(target);
        }
        Ok(written)
    }

    /// Convenience wrapper: opens `packed_path` and extracts its overlay.
    pub fn extract_file(packed_path: &Path, dest_dir: &Path) -> std::io::Result<Vec<PathBuf>> {
        let file = File::open(packed_path)?;
        Self::extract(file, dest_dir)
    }
}

/// Per-file progress callback for extraction: `(index, total, path)`.
pub type OnExtractFile<'a> = dyn FnMut(usize, usize, &str) + 'a;

/// Progress + cancellation hooks for [`OverlayReader::extract_with_control`].
#[derive(Default)]
pub struct ExtractControl<'a> {
    /// Called as `(index, total, relative_path)` before each file lands.
    pub on_file: Option<&'a mut OnExtractFile<'a>>,
    /// When this flag is set, extraction aborts between files.
    pub cancel: Option<&'a std::sync::atomic::AtomicBool>,
}

/// Joins a stored relative path onto `dest_dir`, rejecting anything that
/// could escape it (absolute paths, drive/prefix components, `..`).
fn safe_join(dest_dir: &Path, rel: &str) -> std::io::Result<PathBuf> {
    let normalized = rel.replace('\\', "/");
    if normalized.is_empty() || normalized.starts_with('/') {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("invalid empty or absolute overlay path: {rel:?}"),
        ));
    }
    let mut out = dest_dir.to_path_buf();
    for part in normalized.split('/') {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." || part.contains(':') {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("overlay path escapes destination: {rel:?}"),
            ));
        }
        out.push(part);
    }
    if out == dest_dir {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("overlay path names no file: {rel:?}"),
        ));
    }
    Ok(out)
}

fn copy_str_to_fixed(dest: &mut [u8], src: &str) {
    let bytes = src.as_bytes();
    let len = bytes.len().min(dest.len() - 1);
    dest[..len].copy_from_slice(&bytes[..len]);
    dest[len] = 0;
}

/// Public wrapper around the fixed-field string copy, for code that
/// builds [`InstallOptions`] outside this module (the JSON manifest).
pub fn copy_str_to_fixed_pub(dest: &mut [u8], src: &str) {
    copy_str_to_fixed(dest, src);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn unique_temp(tag: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "seanpak_overlay_{}_{}_{}",
            std::process::id(),
            tag,
            nanos
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn roundtrip_pack_read_extract() {
        let root = unique_temp("roundtrip");
        let src = root.join("src");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("app.exe"), b"fake-pe-bytes").unwrap();
        std::fs::write(src.join("sub/data.bin"), vec![7u8; 5000]).unwrap();

        // A stub of non-sector-multiple size exercises the alignment padding.
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xAAu8; 5000]).unwrap();
        let out = root.join("Setup.exe");

        let total = OverlayWriter::build_from_folder(
            &stub,
            &out,
            &src,
            "Test App",
            "1.2.3.4",
            "Test Publisher",
        )
        .unwrap();
        assert_eq!(total, std::fs::metadata(&out).unwrap().len());
        // Overlay start is 4096-aligned: 5000-byte stub pads up to 8192.
        assert!(total > 8192);

        let file = File::open(&out).unwrap();
        let (manifest, records) = OverlayReader::read_manifest(file)
            .unwrap()
            .expect("overlay must be discoverable");
        assert_eq!(manifest.app_name(), "Test App");
        assert_eq!(manifest.app_version(), "1.2.3.4");
        assert_eq!(manifest.publisher(), "Test Publisher");
        assert_eq!(manifest.file_count, 2);
        assert_eq!(records.len(), 2);
        let primary = &records[manifest.primary_exe_index as usize];
        assert_eq!(primary.relative_path(), "app.exe");

        let dest = root.join("extracted");
        let written = OverlayReader::extract_file(&out, &dest).unwrap();
        assert_eq!(written.len(), 2);
        assert_eq!(
            std::fs::read(dest.join("app.exe")).unwrap(),
            b"fake-pe-bytes"
        );
        assert_eq!(
            std::fs::read(dest.join("sub/data.bin")).unwrap(),
            vec![7u8; 5000]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_manifest_on_plain_file_is_none() {
        let cursor = Cursor::new(vec![0u8; 1024]);
        assert!(OverlayReader::read_manifest(cursor).unwrap().is_none());
    }

    /// Hand-builds a manifest-version-2 package (no options block) carrying
    /// `magic`, holding one file `old.txt` with `payload` as its content.
    fn hand_built_v2_package(magic: &[u8; 16], app_name: &str, payload: &[u8]) -> Vec<u8> {
        let mut bytes = vec![0u8; SECTOR_ALIGNMENT as usize];
        let mut manifest = OverlayManifestHeader {
            magic: *magic,
            version: 2,
            file_count: 1,
            primary_exe_index: 0,
            total_uncompressed_bytes: payload.len() as u64,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };
        copy_str_to_fixed(&mut manifest.app_name_utf8, app_name);
        let header_size = std::mem::size_of::<OverlayManifestHeader>();
        let record_size = std::mem::size_of::<FileRecordHeader>();
        let mut record = FileRecordHeader {
            file_size: payload.len() as u64,
            payload_offset: SECTOR_ALIGNMENT + (header_size + record_size) as u64,
            ..Default::default()
        };
        copy_str_to_fixed(&mut record.relative_path_utf8, "old.txt");
        let manifest_bytes = unsafe {
            std::slice::from_raw_parts(
                &manifest as *const OverlayManifestHeader as *const u8,
                header_size,
            )
        };
        bytes.extend_from_slice(manifest_bytes);
        let record_bytes = unsafe {
            std::slice::from_raw_parts(&record as *const FileRecordHeader as *const u8, record_size)
        };
        bytes.extend_from_slice(record_bytes);
        bytes.extend_from_slice(payload);
        bytes
    }

    fn assert_legacy_package_readable(magic: &[u8; 16], tag: &str) {
        // Packages built before the SeanPak rename carry a legacy magic and
        // no options block; they must stay listable, extractable, and yield
        // default install options.
        let root = unique_temp(tag);
        let payload = b"legacy-bytes";
        let bytes = hand_built_v2_package(magic, "Legacy App", payload);

        let (read_back, records, options) = OverlayReader::read_package(Cursor::new(bytes.clone()))
            .unwrap()
            .expect("legacy overlay must be discoverable");
        assert_eq!(read_back.app_name(), "Legacy App");
        assert_eq!(records.len(), 1);
        assert_eq!(options.install_location(), "C:\\Program Files\\Legacy App");
        assert_eq!(options.start_menu_folder(), "Legacy App");
        assert!(options.wants_desktop_shortcut());
        assert!(options.wants_start_menu_shortcut());
        assert!(!options.wants_path_entry());

        let dest = root.join("dest");
        let written = OverlayReader::extract(Cursor::new(bytes), &dest).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(std::fs::read(dest.join("old.txt")).unwrap(), payload);

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn read_manifest_accepts_legacy_rustpak_magic() {
        assert_legacy_package_readable(LEGACY_OVERLAY_MAGIC, "legacy_rustpak");
    }

    #[test]
    fn read_manifest_accepts_legacy_cruciblekit_magic() {
        assert_legacy_package_readable(CRUCIBLEKIT_OVERLAY_MAGIC, "legacy_cruciblekit");
    }

    #[test]
    fn new_packages_carry_seanpak_magic_and_roundtrip_options() {
        let root = unique_temp("options");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("app.exe"), b"fake-pe-bytes").unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xAAu8; 5000]).unwrap();
        let out = root.join("Setup.exe");

        let mut options = InstallOptions::defaults_for("Opt App");
        copy_str_to_fixed(&mut options.install_location_utf8, "C:\\Apps\\Opt App");
        copy_str_to_fixed(&mut options.file_associations_utf8, ".txt,.md");
        options.desktop_shortcut = 0;
        options.path_entry = 1;

        OverlayWriter::build_from_folder_with_options(
            &stub,
            &out,
            &src,
            "Opt App",
            "2.0.0",
            "Test Publisher",
            &options,
        )
        .unwrap();

        // The bytes at the aligned overlay start are the new SeanPak magic.
        let raw = std::fs::read(&out).unwrap();
        let aligned = 8192usize; // 5000-byte stub pads up to 2 * 4096
        assert_eq!(&raw[aligned..aligned + 16], &OVERLAY_MAGIC[..]);

        let file = File::open(&out).unwrap();
        let (manifest, records, read_back) = OverlayReader::read_package(file)
            .unwrap()
            .expect("new overlay must be discoverable");
        assert_eq!(manifest.version, 4);
        assert_eq!(manifest.app_name(), "Opt App");
        assert_eq!(records.len(), 1);
        assert_eq!(read_back.install_location(), "C:\\Apps\\Opt App");
        assert_eq!(read_back.start_menu_folder(), "Opt App");
        assert_eq!(read_back.file_associations(), ".txt,.md");
        assert!(!read_back.wants_desktop_shortcut());
        assert!(read_back.wants_start_menu_shortcut());
        assert!(read_back.wants_path_entry());

        let dest = root.join("extracted");
        let written = OverlayReader::extract_file(&out, &dest).unwrap();
        assert_eq!(written.len(), 1);
        assert_eq!(
            std::fs::read(dest.join("app.exe")).unwrap(),
            b"fake-pe-bytes"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn extract_rejects_traversal_records() {
        // Hand-build a packed file whose single record tries to escape.
        let root = unique_temp("traversal");
        let payload = b"evil";
        let mut bytes = vec![0u8; SECTOR_ALIGNMENT as usize];
        let mut manifest = OverlayManifestHeader {
            magic: *OVERLAY_MAGIC,
            version: 2,
            file_count: 1,
            primary_exe_index: 0,
            total_uncompressed_bytes: payload.len() as u64,
            app_name_utf8: [0u8; 128],
            app_version_utf8: [0u8; 32],
            publisher_utf8: [0u8; 128],
        };
        copy_str_to_fixed(&mut manifest.app_name_utf8, "Evil");
        let header_size = std::mem::size_of::<OverlayManifestHeader>();
        let record_size = std::mem::size_of::<FileRecordHeader>();
        let mut record = FileRecordHeader {
            file_size: payload.len() as u64,
            payload_offset: SECTOR_ALIGNMENT + (header_size + record_size) as u64,
            ..Default::default()
        };
        copy_str_to_fixed(&mut record.relative_path_utf8, "../evil.txt");
        let manifest_bytes = unsafe {
            std::slice::from_raw_parts(
                &manifest as *const OverlayManifestHeader as *const u8,
                header_size,
            )
        };
        bytes.extend_from_slice(manifest_bytes);
        let record_bytes = unsafe {
            std::slice::from_raw_parts(&record as *const FileRecordHeader as *const u8, record_size)
        };
        bytes.extend_from_slice(record_bytes);
        bytes.extend_from_slice(payload);

        let dest = root.join("dest");
        let err = OverlayReader::extract(Cursor::new(bytes), &dest).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert!(!root.join("evil.txt").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn v4_packages_compress_and_round_trip() {
        let root = unique_temp("v4compress");
        let src = root.join("src");
        std::fs::create_dir_all(&src).unwrap();
        let mut payload = Vec::new();
        for i in 0..50_000u32 {
            payload.extend_from_slice(format!("line {i} of a very repetitive file\n").as_bytes());
            if payload.len() > 200_000 {
                break;
            }
        }
        std::fs::write(src.join("big.txt"), &payload).unwrap();
        std::fs::write(src.join("tiny.bin"), b"\x01\x02\x03").unwrap();
        let stub = root.join("stub.exe");
        std::fs::write(&stub, vec![0xAAu8; 5000]).unwrap();
        let out = root.join("Setup.exe");

        OverlayWriter::build_from_folder(&stub, &out, &src, "Zip App", "1.0.0", "Tester").unwrap();
        let file = File::open(&out).unwrap();
        let (manifest, records) = OverlayReader::read_manifest(file).unwrap().unwrap();
        assert_eq!(manifest.version, 4);
        let big = records
            .iter()
            .find(|r| r.relative_path() == "big.txt")
            .unwrap();
        assert_ne!(big.compression(), crate::codec::CODEC_STORED);
        assert_eq!(big.file_size, payload.len() as u64);
        // The whole package is smaller than stub + raw payloads would be.
        let package_len = std::fs::metadata(&out).unwrap().len();
        assert!(package_len < 8192 + payload.len() as u64);

        let dest = root.join("extracted");
        OverlayReader::extract_file(&out, &dest).unwrap();
        assert_eq!(std::fs::read(dest.join("big.txt")).unwrap(), payload);
        assert_eq!(
            std::fs::read(dest.join("tiny.bin")).unwrap(),
            b"\x01\x02\x03"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn safe_join_accepts_nested_and_rejects_escapes() {
        let base = Path::new("/tmp/base");
        assert_eq!(
            safe_join(base, "sub/file.txt").unwrap(),
            Path::new("/tmp/base/sub/file.txt")
        );
        assert_eq!(
            safe_join(base, "sub\\file.txt").unwrap(),
            Path::new("/tmp/base/sub/file.txt")
        );
        for bad in ["../x", "a/../../x", "/abs", "C:/win", "", "."] {
            assert!(safe_join(base, bad).is_err(), "must reject {bad:?}");
        }
    }
}
