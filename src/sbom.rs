use crate::hash::{sha256_digest, sha256_file_hex, sha256_hex};
use crate::manifest::{jobj, jstr, JsonValue};
use crate::overlay::OverlayReader;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Summary of an SPDX sidecar written for a verified Setup executable.
#[derive(Debug, Clone)]
pub struct SbomOutcome {
    pub path: PathBuf,
    pub app_name: String,
    pub app_version: String,
    pub file_count: usize,
    pub setup_sha256: String,
}

/// Generate an SPDX 2.3 JSON SBOM from the payloads embedded in Setup.exe.
///
/// The sidecar includes the SHA-256 of each decoded payload and of the exact
/// Setup executable. Existing output files are never overwritten.
pub fn generate_spdx(setup_path: &Path, output_path: &Path) -> Result<SbomOutcome, String> {
    if path_exists(output_path) {
        return Err(format!(
            "refusing to overwrite existing SBOM output: {}",
            output_path.display()
        ));
    }

    let setup_sha256 = sha256_file_hex(setup_path)
        .map_err(|e| format!("cannot hash {}: {e}", setup_path.display()))?;
    let verification = crate::pack::verify_package(setup_path)
        .map_err(|e| format!("cannot verify {}: {e}", setup_path.display()))?;
    if !verification.ok {
        let failures = verification
            .checks
            .iter()
            .filter(|(_, passed, _)| !*passed)
            .map(|(name, _, detail)| format!("{name}: {detail}"))
            .collect::<Vec<_>>();
        let details = if failures.is_empty() {
            "package verification failed".to_string()
        } else {
            failures.join("; ")
        };
        return Err(format!(
            "refusing to create an SBOM for an invalid package: {details}"
        ));
    }

    let mut reader = File::open(setup_path)
        .map_err(|e| format!("cannot open {}: {e}", setup_path.display()))?;
    let stream_len = OverlayReader::payload_stream_len(&mut reader)
        .map_err(|e| format!("cannot read package overlay: {e}"))?;
    let (manifest, records, _options, _power, payload_digests) =
        OverlayReader::read_package_with_integrity(&mut reader)
            .map_err(|e| format!("cannot read package metadata: {e}"))?
            .ok_or_else(|| "no SeanPak package overlay found".to_string())?;

    if records.is_empty() {
        return Err("package contains no payload files".to_string());
    }
    if records.len() != verification.file_count {
        return Err("package file inventory changed during SBOM generation".to_string());
    }
    if payload_digests
        .as_ref()
        .map_or(false, |digests| digests.len() != records.len())
    {
        return Err("package integrity table does not match its file inventory".to_string());
    }

    let app_name = manifest.app_name();
    let app_version = manifest.app_version();
    if app_name != verification.app_name || app_version != verification.app_version {
        return Err("package metadata changed during SBOM generation".to_string());
    }

    let mut files = Vec::with_capacity(records.len());
    let mut file_ids = Vec::with_capacity(records.len());
    for (index, record) in records.iter().enumerate() {
        let payload = OverlayReader::read_payload(&mut reader, &records, index, stream_len)
            .map_err(|e| format!("cannot read payload {}: {e}", record.relative_path()))?;
        let digest = sha256_digest(&payload);
        if let Some(expected) = payload_digests
            .as_ref()
            .and_then(|digests| digests.get(index))
        {
            if digest != *expected {
                return Err(format!(
                    "payload integrity check failed for {}",
                    record.relative_path()
                ));
            }
        }

        let id = format!("SPDXRef-File-{}", index + 1);
        let normalized_path = record.relative_path().replace('\\', "/");
        files.push(jobj(vec![
            ("SPDXID", jstr(id.clone())),
            ("fileName", jstr(format!("./{normalized_path}"))),
            (
                "checksums",
                JsonValue::Array(vec![checksum_json(&sha256_hex(&payload))]),
            ),
            ("licenseConcluded", jstr("NOASSERTION")),
            ("copyrightText", jstr("NOASSERTION")),
        ]));
        file_ids.push(id);
    }

    let confirmed_sha256 = sha256_file_hex(setup_path)
        .map_err(|e| format!("cannot re-hash {}: {e}", setup_path.display()))?;
    if setup_sha256 != confirmed_sha256 {
        return Err("Setup.exe changed while its SBOM was being generated".to_string());
    }

    let created = creation_timestamp()?;
    let setup_name = setup_path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or_else(|| "Setup.exe path has no file name".to_string())?;
    let application = jobj(vec![
        ("SPDXID", jstr("SPDXRef-Application")),
        ("name", jstr(app_name.clone())),
        ("versionInfo", jstr(app_version.clone())),
        ("downloadLocation", jstr("NOASSERTION")),
        ("filesAnalyzed", JsonValue::Bool(true)),
        (
            "hasFiles",
            JsonValue::Array(file_ids.iter().cloned().map(jstr).collect()),
        ),
        ("primaryPackagePurpose", jstr("APPLICATION")),
        ("licenseConcluded", jstr("NOASSERTION")),
        ("licenseDeclared", jstr("NOASSERTION")),
        ("copyrightText", jstr("NOASSERTION")),
        (
            "comment",
            jstr("Inventory includes every embedded payload; install-time optional component choices may select a subset."),
        ),
    ]);
    let installer = jobj(vec![
        ("SPDXID", jstr("SPDXRef-Installer")),
        ("name", jstr(setup_name.clone())),
        ("versionInfo", jstr(app_version.clone())),
        ("downloadLocation", jstr("NOASSERTION")),
        ("packageFileName", jstr(setup_name)),
        ("filesAnalyzed", JsonValue::Bool(false)),
        ("primaryPackagePurpose", jstr("INSTALL")),
        (
            "checksums",
            JsonValue::Array(vec![checksum_json(&setup_sha256)]),
        ),
        ("licenseConcluded", jstr("NOASSERTION")),
        ("licenseDeclared", jstr("NOASSERTION")),
        ("copyrightText", jstr("NOASSERTION")),
    ]);

    let mut relationships = vec![relationship_json(
        "SPDXRef-Installer",
        "CONTAINS",
        "SPDXRef-Application",
    )];
    for id in &file_ids {
        relationships.push(relationship_json(
            "SPDXRef-Application",
            "CONTAINS",
            id,
        ));
    }

    let document = jobj(vec![
        ("spdxVersion", jstr("SPDX-2.3")),
        ("dataLicense", jstr("CC0-1.0")),
        ("SPDXID", jstr("SPDXRef-DOCUMENT")),
        (
            "name",
            jstr(format!("{app_name} {app_version} SBOM")),
        ),
        (
            "documentNamespace",
            jstr(format!(
                "https://spdx.org/spdxdocs/SeanPak/{setup_sha256}/{created}"
            )),
        ),
        (
            "creationInfo",
            jobj(vec![
                ("created", jstr(created)),
                (
                    "creators",
                    JsonValue::Array(vec![jstr(format!(
                        "Tool: SeanPak {}",
                        env!("CARGO_PKG_VERSION")
                    ))]),
                ),
            ]),
        ),
        (
            "comment",
            jstr("Generated from the embedded payloads of the exact Setup executable. License and copyright data are not inferred."),
        ),
        (
            "documentDescribes",
            JsonValue::Array(vec![
                jstr("SPDXRef-Application"),
                jstr("SPDXRef-Installer"),
            ]),
        ),
        (
            "packages",
            JsonValue::Array(vec![application, installer]),
        ),
        ("files", JsonValue::Array(files)),
        ("relationships", JsonValue::Array(relationships)),
    ]);

    let contents = format!("{}\n", document.render());
    write_new_file_atomically(output_path, contents.as_bytes())?;

    Ok(SbomOutcome {
        path: output_path.to_path_buf(),
        app_name,
        app_version,
        file_count: records.len(),
        setup_sha256,
    })
}

fn checksum_json(digest: &str) -> JsonValue {
    jobj(vec![
        ("algorithm", jstr("SHA256")),
        ("checksumValue", jstr(digest)),
    ])
}

fn relationship_json(source: &str, kind: &str, target: &str) -> JsonValue {
    jobj(vec![
        ("spdxElementId", jstr(source)),
        ("relationshipType", jstr(kind)),
        ("relatedSpdxElement", jstr(target)),
    ])
}

fn path_exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

fn write_new_file_atomically(path: &Path, contents: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| "SBOM output path has no file name".to_string())?;
    fs::create_dir_all(parent)
        .map_err(|e| format!("cannot create output directory {}: {e}", parent.display()))?;

    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("system clock is before Unix epoch: {e}"))?
        .as_nanos();
    let temp_path = parent.join(format!(
        ".seanpak-sbom-{}-{nonce}.tmp",
        std::process::id()
    ));
    let mut temp_file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp_path)
        .map_err(|e| format!("cannot create temporary SBOM file: {e}"))?;
    if let Err(e) = temp_file.write_all(contents).and_then(|_| temp_file.sync_all()) {
        drop(temp_file);
        let _ = fs::remove_file(&temp_path);
        return Err(format!("cannot write temporary SBOM file: {e}"));
    }
    drop(temp_file);

    if path_exists(path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!(
            "refusing to overwrite existing SBOM output: {}",
            path.display()
        ));
    }
    if let Err(e) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(format!("cannot place SBOM at {}: {e}", path.display()));
    }
    Ok(())
}

fn creation_timestamp() -> Result<String, String> {
    let epoch_seconds = match std::env::var("SOURCE_DATE_EPOCH") {
        Ok(value) => value
            .parse::<u64>()
            .map_err(|_| "SOURCE_DATE_EPOCH must be a nonnegative Unix timestamp".to_string())?,
        Err(std::env::VarError::NotPresent) => SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| format!("system clock is before Unix epoch: {e}"))?
            .as_secs(),
        Err(std::env::VarError::NotUnicode(_)) => {
            return Err("SOURCE_DATE_EPOCH is not valid Unicode".to_string())
        }
    };
    format_utc(epoch_seconds)
}

fn format_utc(epoch_seconds: u64) -> Result<String, String> {
    const SECONDS_PER_DAY: u64 = 86_400;
    let days = i64::try_from(epoch_seconds / SECONDS_PER_DAY)
        .map_err(|_| "timestamp is outside the supported range".to_string())?;
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524
        - day_of_era / 146_096)
        / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era
        - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };
    if !(0..=9_999).contains(&year) {
        return Err("timestamp is outside the supported RFC 3339 year range".to_string());
    }

    let seconds_in_day = epoch_seconds % SECONDS_PER_DAY;
    let hour = seconds_in_day / 3_600;
    let minute = (seconds_in_day % 3_600) / 60;
    let second = seconds_in_day % 60;
    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::parse_json;
    use crate::overlay::OverlayWriter;
    use std::io::{Read, Seek, SeekFrom, Write};

    fn unique_temp_dir() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "seanpak_sbom_{}_{}",
            std::process::id(),
            nonce
        ));
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn build_setup(dir: &Path) -> (PathBuf, PathBuf) {
        let payload_dir = dir.join("payload");
        fs::create_dir_all(payload_dir.join("docs")).unwrap();
        let stub = dir.join("stub.exe");
        fs::write(&stub, b"MZ SeanPak SBOM test stub").unwrap();
        fs::write(payload_dir.join("app.exe"), b"MZ example application").unwrap();
        fs::write(payload_dir.join("docs/readme.txt"), b"Example payload file.\n").unwrap();
        let setup = dir.join("Example-Setup.exe");
        OverlayWriter::build_from_folder(
            &stub,
            &setup,
            &payload_dir,
            "Example App",
            "1.2.3",
            "Example Publisher",
        )
        .unwrap();
        (payload_dir, setup)
    }

    fn string_field<'a>(value: &'a JsonValue, key: &str) -> &'a str {
        value.get(key).and_then(JsonValue::as_str).unwrap()
    }

    fn array_field(value: &JsonValue, key: &str) -> &[JsonValue] {
        match value.get(key).unwrap() {
            JsonValue::Array(items) => items,
            other => panic!("expected array, got {other:?}"),
        }
    }

    #[test]
    fn sbom_records_exact_setup_and_payload_hashes_and_never_overwrites() {
        let dir = unique_temp_dir();
        let (payload_dir, setup) = build_setup(&dir);
        let output = dir.join("Example-Setup.spdx.json");
        let outcome = generate_spdx(&setup, &output).unwrap();
        let document_text = fs::read_to_string(&output).unwrap();
        let document = parse_json(&document_text).unwrap();

        assert_eq!(string_field(&document, "spdxVersion"), "SPDX-2.3");
        assert_eq!(string_field(&document, "dataLicense"), "CC0-1.0");
        assert!(string_field(&document, "documentNamespace").starts_with(
            "https://spdx.org/spdxdocs/SeanPak/"
        ));
        assert_eq!(outcome.app_name, "Example App");
        assert_eq!(outcome.app_version, "1.2.3");
        assert_eq!(outcome.file_count, 2);

        let packages = array_field(&document, "packages");
        assert_eq!(packages.len(), 2);
        let application = packages
            .iter()
            .find(|package| string_field(package, "SPDXID") == "SPDXRef-Application")
            .unwrap();
        assert_eq!(array_field(application, "hasFiles").len(), 2);

        let files = array_field(&document, "files");
        assert_eq!(files.len(), 2);
        let app_file = files
            .iter()
            .find(|file| string_field(file, "fileName") == "./app.exe")
            .unwrap();
        let app_checksums = array_field(app_file, "checksums");
        assert_eq!(string_field(&app_checksums[0], "algorithm"), "SHA256");
        assert_eq!(
            string_field(&app_checksums[0], "checksumValue"),
            sha256_hex(&fs::read(payload_dir.join("app.exe")).unwrap())
        );

        let installer = packages
            .iter()
            .find(|package| string_field(package, "SPDXID") == "SPDXRef-Installer")
            .unwrap();
        let installer_checksums = array_field(installer, "checksums");
        assert_eq!(
            string_field(&installer_checksums[0], "checksumValue"),
            outcome.setup_sha256
        );
        assert_eq!(outcome.setup_sha256, sha256_file_hex(&setup).unwrap());
        assert_eq!(array_field(&document, "relationships").len(), 3);

        let before = fs::read(&output).unwrap();
        assert!(generate_spdx(&setup, &output).is_err());
        assert_eq!(fs::read(&output).unwrap(), before);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn corrupted_v7_payload_is_rejected_without_writing_a_sidecar() {
        let dir = unique_temp_dir();
        let (_payload_dir, setup) = build_setup(&dir);
        let mut reader = File::open(&setup).unwrap();
        let (_, records, _, _, _) = OverlayReader::read_package_with_integrity(&mut reader)
            .unwrap()
            .unwrap();
        drop(reader);

        let mut package = OpenOptions::new().read(true).write(true).open(&setup).unwrap();
        package.seek(SeekFrom::Start(records[0].payload_offset)).unwrap();
        let mut byte = [0u8; 1];
        package.read_exact(&mut byte).unwrap();
        byte[0] ^= 0x01;
        package.seek(SeekFrom::Start(records[0].payload_offset)).unwrap();
        package.write_all(&byte).unwrap();
        drop(package);

        let output = dir.join("should-not-exist.spdx.json");
        assert!(generate_spdx(&setup, &output).is_err());
        assert!(!path_exists(&output));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn timestamp_conversion_handles_epoch_and_leap_day() {
        assert_eq!(format_utc(0).unwrap(), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400).unwrap(), "2000-02-29T00:00:00Z");
    }
}
