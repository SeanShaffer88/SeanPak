# SeanPak — Parity / Feature Status

Status of the SeanPak plan (Wave 1 native engine v2 + suite contract)
against what the code actually does today. "Shipped" means present in the
tree and covered by the local gate; Windows runtime behavior stays
Unverified until SEAN's field pass (see QUALITY.md).

| Capability | Status | Notes |
|---|---|---|
| PE overlay packager (sector-aligned, per-file compression) | Shipped; compression in 4.1.0 | `src/overlay.rs`; magic `SEANPAK_OVL`, legacy `CRUCIBLEKIT_OVL` + `RUSTPAK_OVERLAY` readable; manifest v4 adds a per-file codec flag, v5 (4.4.0) adds the options extension block; v4/v3/v2 packages still read + install (with extension defaults) |
| CLI modes (`--batch`, `--list`, `--extract`, `--install`, `--help`, `--version`) | Shipped (pre-4.0) | Full CLI parity for packaging/inspection |
| Builder studio GUI (Design H IDE layout since 4.5.0: toolbar strip, files-directory listing pane, center files table, manifest pane, output strip) | Shipped in 4.2.0 (Design D); layout rebuilt in 4.5.0, Windows behavior Unverified | `src/studio.rs`; replaces the 3.0.0-era builder window. Every action runs a shared CLI operation (see the parity table below); the document/settings model is host-tested |
| Installer wizard (Design E Rail layout since 4.5.0) | Shipped in 4.0.0 (Design C); layout rebuilt in 4.5.0, Windows behavior Unverified | `src/wizard.rs`; bare run of a built installer opens it. The rail carries the app identity, the step list, and the disk figures; pages and behavior unchanged |
| Install = extract to chosen location + rollback journal | Shipped in 4.0.0, Unverified on Windows | Existing targets snapshotted before overwrite; failure rolls back |
| Uninstaller + ARP entry | Shipped in 4.0.0, Unverified on Windows | Copies itself as `Uninstall.exe`; ARP uninstall string runs it with `--uninstall` |
| `--uninstall` (payload removal + ARP cleanup) | Shipped in 4.0.0; shared operation in 4.4.0, Unverified on Windows | `uninstall::perform_uninstall` (the CLI is a thin wrapper); the running uninstaller copy cannot delete itself; reported honestly |
| Per-app install options in the manifest (v3) | Shipped in 4.0.0; extended in 4.4.0 (overlay v5) | Install location, Start Menu folder, desktop shortcut flag, file associations, PATH flag; 4.4.0 adds `launchAfterInstall`, `startAtLogin`, and `dataDirs`; pre-v5 packages read back with defaults (extension defaults for v3/v4). Since 4.5.0 the studio's manifest pane exposes all three (offer/default checkbox pairs; data folders as a semicolon-separated edit) through the same manifest JSON model |
| Desktop / Start Menu shortcut creation | Shipped in 4.1.0, Windows behavior Unverified | `.lnk` via IShellLink at install; paths journaled in the receipt and deleted by uninstall |
| File associations applied at install | Shipped in 4.1.0, Windows behavior Unverified | HKCU `Software\Classes` per-user; prior default recorded in the receipt and restored by uninstall only while the association still points at this app |
| PATH entry applied at install | Shipped in 4.1.0, Windows behavior Unverified | HKCU Environment, append-only-if-absent, change broadcast; uninstall removes only the exact added segment |
| Wizard pages 2–4 (Options editing, progress, finish states) | Shipped in 4.1.0, Windows behavior Unverified | `src/wizard.rs`; Review Change column routes to Options; Progress runs the shared op on a worker thread with Cancel → rollback → honest Finish state; since 4.5.0 the rail shows the app identity and step state (current accented, completed checked) |
| Payload compression (Windows Compression API) | Shipped in 4.1.0 | XPRESS Huffman via `cabinet.dll` on Windows; hand-rolled LZ codec as the portable fallback; per-file store-if-not-shrinking; host round-trip + size win verified in the gate |
| Upgrade / repair / downgrade protection | Shipped in 4.1.0, Windows behavior Unverified | Existing install detected (receipt, then ARP); older upgrades in place with old files journaled, same version repairs via `--repair`, newer refuses downgrade unless `--force`; full lifecycle verified on the host build |
| Install receipt (`seanpak-receipt.json`) | Shipped in 4.1.0 | Written on successful install (files + SHA-256, options applied, journaled artifacts), mirrored into `seanpak.log`; uninstall reads it |
| Launch after install (Finish-page checkbox + `--launch`) | Shipped in 4.4.0, Windows behavior Unverified | Manifest `launchAfterInstall` offer + default; wizard Finish checkbox armed on success, closing with it checked starts the primary exe detached via the shared `install::launch_program`; headless `--launch` is opt-in only and reports requested/performed/error in JSON; a launch failure is a warning, never an install failure |
| Start at login (HKCU Run entry) | Shipped in 4.4.0, Windows behavior Unverified | Manifest `startAtLogin` offer + default; wizard Review row + Options checkbox; applied by the shared artifact plan like shortcuts/PATH, journaled in the receipt with the prior value; uninstall removes it only while it still points at this app (restoring the prior value); `--start-at-login` / `--no-start-at-login` override per run |
| Uninstall data purge + confirmation window | Shipped in 4.4.0, Windows behavior Unverified | Manifest `dataDirs` resolved into the receipt at install; interactive uninstall opens the native confirm window (wizard palette) with the default-unchecked data checkbox listing the folders + cannot-be-recovered warning; silent/JSON uninstalls delete data only with `--purge-data`; JSON reports `dataDeleted`; purge is guarded (absolute, below a root, never the install dir) and not rollback-journaled by design |
| Install-location writability pre-flight | Shipped in 4.3.0, Windows behavior Unverified | `install::location_verdict` / `ensure_location_writable`; a destination this per-user process cannot write is refused before any change (fresh / upgrade / repair alike), with the `location_not_writable` JSON reason and the `%LOCALAPPDATA%\Programs\<AppFolder>` suggestion from the one shared derivation; the wizard Review page flags the row and offers the one-click switch |
| `verify --sandbox` end-to-end proof | Shipped in 4.1.0 | Silent temp-dir install → byte-for-byte compare → silent uninstall → residue assertion; JSON report + exit code; verified on the host build, wired into CI for the Windows build |
| Reusable CI for other projects | Shipped in 4.1.0, first hosted run Unverified | `.github/workflows/pack-reusable.yml` (`workflow_call`) + `docs/PACKAGING.md` caller example |
| Golden-package fixture | Shipped in 4.1.0 | `tests/golden_package.rs` packs/verifies/sandbox-installs a compressible fixture in the host gate; CI runs the same fixture against the Windows build |
| CI stub-size budget | Shipped in 4.1.0, first hosted run Unverified | `build.yml` fails the x86_64 leg if the stub exceeds 4 MiB (measured stub ≈ 0.6 MB) |
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
| Pack a release set (Setup.exe + Portable.zip + SHA256SUMS.txt) from a JSON manifest | `pack::pack` | Studio **Build Setup** button (worker thread; a refused overwrite becomes an explicit confirm + forced retry) | `pack <manifest.json> <files-dir> [--out-dir] [--force] [--json]` |
| Prove a built installer (overlay, manifest sanity, payloads) | `pack::verify_package` | Studio **Verify** button | `verify <Setup.exe> [--json]` |
| Sandbox-proof a built installer (install → compare → uninstall → residue) | `pack::sandbox_verify` | Studio **Test install** button, or Verify with the Sandbox toggle on | `verify <Setup.exe> --sandbox [--json]` |
| Print the manifest JSON Schema | `manifest::MANIFEST_SCHEMA_JSON` | Studio Tools → Show Manifest Schema (prints into the output strip) | `schema` |
| Validate a manifest | `manifest::AppManifest::from_json` | Studio **Validate** button + the manifest panel (same fields, same rules) | `validate <manifest.json> [--json]` |
| Build a native overlay installer from a folder | `overlay::OverlayWriter` | — (the studio packs via `pack::pack`) | `--batch <folder> --out <exe> [--json]` |
| Generate Inno/NSIS scripts | `backends` | — | `--batch ... --engine inno\|nsis [--script-only]` |
| Install (extract + Uninstall.exe + uninstall entry + shortcuts/associations/PATH/login entry, rollback on failure) | `install::perform_install_with` | Wizard Install button (Options page edits the same options first, incl. the start-at-login toggle when offered) | `--install [--to] [--dry-run] [--repair] [--force] [--start-at-login\|--no-start-at-login] [--json] [--silent\|--headless]` |
| Launch the installed app after install | `install::launch_program` | Wizard Finish-page "Launch <App>" checkbox (offered/defaulted per the manifest) | `--install --launch` (explicit opt-in; JSON reports requested/performed/error) |
| Preview an install without changing anything | `install::plan_install` | Wizard review table (Design E Rail) | `--install --dry-run [--json]` |
| Check the install location is writable (per-user pre-flight; refuses before any change with the `location_not_writable` reason + per-user suggestion) | `install::location_verdict` / `ensure_location_writable` | Wizard Review row marker + **Use per-user folder** one-click action; Options page re-checks as the location is edited; Install disabled while unwritable | `--install --dry-run` reports `locationWritable` + `suggestedLocation`; a real `--install` into an unwritable destination exits 5 with the same reason and suggestion |
| Review / edit install options before installing | overlay `InstallOptions` (manifest v3+) | Wizard review table + Options page | `validate` / `--list [--json]` expose the same fields |
| List overlay contents | `overlay::OverlayReader` | — | `--list <packed.exe> [--json]` |
| Extract overlay contents | `overlay::OverlayReader` | — | `--extract <packed.exe> --to <dir> [--json]` |
| Uninstall (payload + journaled artifacts + receipt + ARP cleanup; optional data purge) | `uninstall::perform_uninstall` | ARP / `Uninstall.exe` — interactive runs open the confirmation window with the data checkbox first | `--uninstall [--to] [--purge-data] [--json]` (silent runs never purge without the flag) |

GUI-only capability: none. The studio adds input conveniences over the
same operations — picking files/folders with the native dialogs, copying
picked files into the package folder (a plain file copy the CLI user does
by hand), editing the manifest fields interactively, and curating the
package file list, which is exactly the manifest's `files` field (an
empty list when the package is the whole folder, as with the CLI). Its
window geometry/path persistence (`seanpak-studio.json` beside the exe)
is a UI preference store, not a capability. No build, install,
verification, or uninstall capability exists only in the GUI.
