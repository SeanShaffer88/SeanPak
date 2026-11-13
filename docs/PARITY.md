# SeanPak — Parity / Feature Status

Status of the SeanPak plan (Wave 1 native engine v2 + suite contract)
against what the code actually does today. "Shipped" means present in the
tree and covered by the local gate; Windows runtime behavior stays
Unverified until SEAN's field pass (see QUALITY.md).

| Capability | Status | Notes |
|---|---|---|
| PE overlay packager (sector-aligned, uncompressed) | Shipped (pre-4.0) | `src/overlay.rs`; magic `SEANPAK_OVL`, legacy `CRUCIBLEKIT_OVL` + `RUSTPAK_OVERLAY` readable |
| CLI modes (`--batch`, `--list`, `--extract`, `--install`, `--help`, `--version`) | Shipped (pre-4.0) | Full CLI parity for packaging/inspection |
| Builder GUI (folder pick, build, UAC self-elevate) | Shipped (pre-4.0) | Design D studio layout is the target for a later slice; current window is the 3.0.0 layout with the rev2 palette |
| Installer wizard (Design C ledger review) | Shipped in 4.0.0, Windows behavior Unverified | `src/wizard.rs`; bare run of a built installer opens it |
| Install = extract to chosen location + rollback journal | Shipped in 4.0.0, Unverified on Windows | Existing targets snapshotted before overwrite; failure rolls back |
| Uninstaller + ARP entry | Shipped in 4.0.0, Unverified on Windows | Copies itself as `Uninstall.exe`; ARP uninstall string runs it with `--uninstall` |
| `--uninstall` (payload removal + ARP cleanup) | Shipped in 4.0.0, Unverified on Windows | The running uninstaller copy cannot delete itself; reported honestly |
| Per-app install options in the manifest (v3) | Shipped in 4.0.0 | Install location, Start Menu folder, desktop shortcut flag, file associations, PATH flag; v2 packages read back with defaults |
| Desktop / Start Menu shortcut creation | **Remaining (Slice 2)** | Recorded in the manifest and shown in the wizard; not yet created at install time |
| File associations applied at install | **Remaining (Slice 2)** | Recorded + shown only |
| PATH entry applied at install | **Remaining (Slice 2)** | Recorded + shown only |
| Wizard pages 2–4 (Options editing, progress, finish states) | **Remaining (Slice 2)** | Slice 1 ships the Review page (Step 1) + Finish state on the same page; only the install location is editable (double-click row) |
| Windows Compression API payload compression | **Remaining** | Payloads are stored uncompressed by design today |
| Project model file (`.seanpak` project) | **Remaining** | Builds are folder + flags driven today |
| Suite manifest drop-in for other projects | **Partial** | The v3 install-options block is the seed; a standalone per-app manifest file format is not defined yet |
| Inno/NSIS script backends | Shipped (pre-4.0) | External tools, never linked |

## AI control — GUI vs CLI parity

SeanPak's primary user is SEAN's AI packaging his programs for release, so
the headless pipeline is the main workflow and the Builder GUI is
secondary. Every capability is a command first; the GUI surfaces are views
over the same shared operations. Every command supports `--json`
machine-readable output, exit codes are deterministic (0 success,
2 usage, 3 bad manifest/overlay, 4 I/O, 5 install failure), and
build/install operations append JSON-lines records to `seanpak.log`.

| Capability | Shared operation | GUI surface | CLI surface |
|---|---|---|---|
| Pack a release set (Setup.exe + Portable.zip + SHA256SUMS.txt) from a JSON manifest | `pack::pack` | Not in the Builder window (planned) | `pack <manifest.json> <files-dir> [--out-dir] [--force] [--json]` |
| Prove a built installer (overlay, manifest sanity, payloads) | `pack::verify_package` | — | `verify <Setup.exe> [--json]` |
| Print the manifest JSON Schema | `manifest::MANIFEST_SCHEMA_JSON` | — | `schema` |
| Validate a manifest | `manifest::AppManifest::from_json` | — | `validate <manifest.json> [--json]` |
| Build a native overlay installer from a folder | `overlay::OverlayWriter` | Builder window | `--batch <folder> --out <exe> [--json]` |
| Generate Inno/NSIS scripts | `backends` | Builder window | `--batch ... --engine inno\|nsis [--script-only]` |
| Install (extract + Uninstall.exe + uninstall entry, rollback on failure) | `install::perform_install` | Wizard Install button | `--install [--to] [--dry-run] [--json] [--silent\|--headless]` |
| Preview an install without changing anything | `install::plan_install` | Wizard review table (Design C) | `--install --dry-run [--json]` |
| Review install options before installing | overlay `InstallOptions` (manifest v3) | Wizard review table | `validate` / `--list [--json]` expose the same fields |
| List overlay contents | `overlay::OverlayReader` | — | `--list <packed.exe> [--json]` |
| Extract overlay contents | `overlay::OverlayReader` | — | `--extract <packed.exe> --to <dir> [--json]` |
| Uninstall | overlay record replay + ARP cleanup | ARP / `Uninstall.exe` | `--uninstall [--to] [--json]` |

GUI-only capability: picking a folder with the native dialog and editing
fields interactively in the Builder window — input conveniences over the
same folder scan and options the CLI takes as arguments. No build,
install, verification, or uninstall capability exists only in the GUI.
