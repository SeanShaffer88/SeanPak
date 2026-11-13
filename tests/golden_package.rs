//! Golden-package fixture: the end-to-end regression net for the AI
//! packaging lifecycle. It packs a tiny fixture app (with a highly
//! compressible payload) using the just-built `seanpak` binary as the
//! stub, then proves the whole chain on the host:
//!
//! 1. the packed Setup.exe is smaller than the stored-only equivalent
//!    (stub + raw payload) — compression actually engaged;
//! 2. `verify_package` passes;
//! 3. `sandbox_verify` passes — silent install into a temp dir,
//!    byte-for-byte file comparison, silent uninstall, no residue
//!    beyond the known leftovers.
//!
//! The fixture "app" is data, not a real program: its executable is a
//! text placeholder, which is fine — SeanPak packages bytes, and the
//! installer under test is SeanPak itself.

use std::path::{Path, PathBuf};

use seanpak::manifest::AppManifest;
use seanpak::pack::{pack, sandbox_verify, verify_package};

fn unique_temp(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!(
        "seanpak_golden_{}_{}_{}",
        std::process::id(),
        tag,
        nanos
    ))
}

/// The fixture app's files: a placeholder exe, a large compressible
/// data file, and a small readme. Returns the raw payload total.
fn write_fixture(files_dir: &Path) -> u64 {
    std::fs::create_dir_all(files_dir.join("data")).unwrap();
    let exe = b"GOLDEN-FIXTURE-EXE placeholder payload\n".repeat(64);
    std::fs::write(files_dir.join("golden.exe"), &exe).unwrap();
    // Long runs of one byte pattern: compresses to almost nothing.
    let mut big = Vec::new();
    for i in 0..6000u32 {
        big.extend_from_slice(format!("golden fixture line {i:05} aaaaaaaa\n").as_bytes());
    }
    std::fs::write(files_dir.join("data").join("big.txt"), &big).unwrap();
    let readme = b"Golden fixture app for SeanPak regression tests.\n";
    std::fs::write(files_dir.join("readme.txt"), readme).unwrap();
    (exe.len() + big.len() + readme.len()) as u64
}

#[test]
fn golden_package_packs_verifies_and_sandbox_installs() {
    let root = unique_temp("run");
    let files_dir = root.join("files");
    let out_dir = root.join("out");
    std::fs::create_dir_all(&out_dir).unwrap();
    let raw_total = write_fixture(&files_dir);

    let manifest = AppManifest::from_json(
        r#"{
            "appName": "GoldenApp",
            "version": "1.2.3",
            "publisher": "SeanPak Tests",
            "primaryExe": "golden.exe",
            "desktopShortcut": true,
            "startMenuShortcut": true,
            "fileAssociations": [".gold"],
            "pathEntry": false
        }"#,
    )
    .expect("fixture manifest must validate");

    let stub = PathBuf::from(env!("CARGO_BIN_EXE_seanpak"));
    let outcome = pack(&manifest, &files_dir, &out_dir, &stub, false).expect("pack must succeed");
    assert_eq!(outcome.file_count, 3);
    assert_eq!(outcome.total_payload_bytes, raw_total);

    // Compression engaged: the Setup.exe must beat the stored-only
    // equivalent (stub + raw payload) by a clear margin.
    let stored_equivalent = outcome.stub_bytes + raw_total;
    assert!(
        outcome.setup.size < stored_equivalent,
        "compressed Setup.exe ({} bytes) must be smaller than the stored-only equivalent ({stored_equivalent} bytes)",
        outcome.setup.size
    );

    // Static verify passes.
    let report = verify_package(&outcome.setup.path).expect("verify must run");
    assert!(report.ok, "verify failed: {:?}", report.checks);
    assert_eq!(report.app_name, "GoldenApp");
    assert_eq!(report.manifest_version, 5);

    // The sandbox proof: real silent install / compare / uninstall /
    // residue check, driven through the packed binary itself.
    let sandbox = sandbox_verify(&outcome.setup.path).expect("sandbox verify must run");
    assert!(
        sandbox.ok,
        "sandbox failed: {:?} (leftovers: {:?})",
        sandbox.checks, sandbox.leftovers
    );
    for leftover in &sandbox.leftovers {
        assert!(
            matches!(leftover.as_str(), "Uninstall.exe" | "seanpak.log"),
            "unexpected sandbox leftover: {leftover}"
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
