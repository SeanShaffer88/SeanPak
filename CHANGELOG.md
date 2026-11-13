# Changelog

All notable changes to the SeanPak utility are documented in this file. (3.0.0 was published under the product's former name, CrucibleKit; releases before 3.0.0 under its original name, RustPak.)

## [4.0.0] - 2026-11-12
### Changed
- **CrucibleKit is now SeanPak.** The product, repository (now https://github.com/SeanShaffer88/SeanPak), crate, and executable (`seanpak.exe`) were renamed in one pass; SeanPak is the single custom TesselGUI installer for all of SEAN's projects. The overlay format marker changed from `CRUCIBLEKIT_OVL` to `SEANPAK_OVL`; the reader still accepts both legacy markers (`CRUCIBLEKIT_OVL` and `RUSTPAK_OVERLAY`), so packages built with CrucibleKit or RustPak remain listable, extractable, and installable.
- **A built native installer now opens the SeanPak wizard when run with no arguments** (previously it silently self-extracted). Headless extraction is unchanged via `--install`; `--list` / `--extract` are unchanged.
- Builder GUI and wizard use the TesselGUI revision 2 (GitHub Dark) palette.

### Added
- **Installer wizard (Design C ledger review):** a built installer opens a review page — install location, components, Start Menu folder, desktop shortcut, file associations, PATH entry, and disk space in a gridded Item / Value / Change table read from the package manifest — with Cancel / Install in the footer. The install location can be changed by double-clicking its row.
- **Install performs a real install:** the payload is extracted into the chosen location (existing files are snapshotted first and restored if anything fails), the installer copies itself in as `Uninstall.exe`, and an Add/Remove Programs entry is registered whose uninstall string runs that copy with the new `--uninstall` mode (removes the payload files and the entry; the running uninstaller copy itself is left behind and reported).
- **Overlay manifest version 3:** packages now record install options (install location, Start Menu folder, desktop-shortcut flag, file associations, PATH-entry flag). Version 2 packages read back with defaults. New writer API: `OverlayWriter::build_from_folder_with_options`.
- `.project-tier` (t2) + Project Manifest in AGENTS.md, `QUALITY.md`, and `docs/PARITY.md`.
- **Headless packaging pipeline (`pack`) — the primary workflow, built for control by a local AI:** `seanpak pack <manifest.json> <files-dir> [--out-dir <dir>] [--force] [--json]` produces exactly `<App>-v<version>-x64-Setup.exe`, `<App>-v<version>-x64-Portable.zip` (built app files, zero-install), and `SHA256SUMS.txt`, with names derived deterministically from the manifest. Existing outputs with identical contents are kept; differing outputs are refused without `--force`. With `--json` it prints a build report (each asset's path, size, and SHA-256, the stub byte size, and the manifest used).
- **`verify <Setup.exe>`:** proves a built installer before publishing — overlay readable, embedded manifest sane, embedded file list matching the payloads — with `--json` support and a pass/fail exit code.
- **`schema` and `validate <manifest.json>`:** the app manifest is JSON; `schema` prints its JSON Schema so a valid manifest can be authored without guessing, and `validate` checks one with plain-language errors. The same fields feed the wizard's review table; legacy RustPak/CrucibleKit packages keep reading with defaults.
- **`--json` on every command** (pack, verify, validate, batch build, list, extract, install, uninstall), plus `--headless` / `--silent` / `--dry-run` on the install path: `--install --dry-run` reports exactly what would happen (the full plan as JSON with `--json`) and changes nothing. The CLI `--install` runs the same shared install operation as the wizard's Install button — there is no GUI-only capability.
- **Deterministic exit codes**, documented in `--help` and the README: 0 success · 1 generic/GUI failure · 2 usage error · 3 bad or missing manifest/overlay · 4 I/O failure (including a refused overwrite) · 5 install failed (destination rolled back).
- **Structured outcome log:** build/install/pack/verify/uninstall operations append JSON-lines records to the existing `seanpak.log`, so an AI can read back and confirm outcomes.

### Known limitations
- Shortcut creation, file-association registration, and PATH updates are recorded in the manifest and shown in the wizard but are **not yet applied** at install time (next slice). Wizard pages beyond the review page (options editing, progress, finish as separate pages) are also next-slice work.
- All Windows runtime behavior in this release is **Unverified** until a field pass on a real Windows machine.

## [3.0.0] - 2026-10-20
### Changed
- **RustPak is now CrucibleKit.** The product, repository, crate, and executable were renamed in one pass. The overlay format marker changed from `RUSTPAK_OVERLAY` to `CRUCIBLEKIT_OVL`; the reader still accepts the old marker, so packages built with RustPak remain listable and extractable. Release assets are now named `cruciblekit-*-windows.zip`, and the executable is `cruciblekit.exe`.

### Fixed
- **Silent startup failure** (reported on a real Windows machine as "launches and exits"; the GUI-subsystem binary had no console, so any startup error vanished without a trace — the precise failing step could not be reproduced off that machine). Startup is now observable and hardened:
  - Every startup step (mode selection, window-class registration, window creation, message-loop entry/exit) is recorded in `cruciblekit.log` next to the executable.
  - A fatal startup failure shows a dialog naming the Windows error code and the log location instead of exiting silently.
  - `RegisterClassExW`'s result is checked (an already-registered class is not fatal), window-creation errors are captured before anything can clobber them, and a `GetMessageW` error (-1) is handled explicitly instead of ending the message loop as if the user had quit.
  - The window title shows the real version from the package metadata instead of a hard-coded one.
- **CLI output invisible from terminals:** running with any command-line flag (`--help`, `--batch`, `--list`, ...) now attaches to the parent console and re-points the standard handles, so CLI modes print where they are run. (Previously a GUI-subsystem process printed into the void.)

## [2.1.1] - 2026-10-20
### Added
- **The application icon is real now:** `resources/rustpak.ico` — 8 sizes from 16 to 256 px, rendered from `resources/rustpak_logo.svg` — is in the repository, and `build.rs` embeds it into Windows builds. The 2.0.0 notes claimed an embedded icon shipped when it had not; 2.1.0 made the reference conditional because the file was missing; 2.1.1 closes that gap.
- README **Download** section linking the prebuilt binaries on the latest GitHub Release.

### Changed
- Tag builds now attach their three zips and a generated `SHA256SUMS.txt` to the GitHub Release automatically (the 2.1.0 assets were attached by hand).

## [2.1.0] - 2026-10-20
### Fixed
- **The build itself:** `src/native_dialog.rs` used `SHBrowseForFolderW` / `BROWSEINFOW`, which live behind the `Win32_UI_Shell_Common` feature of `windows-sys` that `Cargo.toml` never enabled — CI had failed on every target since the initial commit. The feature is now enabled, and the crate is warning-free under `clippy -D warnings` on the host and all three Windows targets (x86_64, i686, aarch64 MSVC).
- Windows-only modules (`native_dialog`, `win32_ui`, `restart_manager`, the registry/tick parts of `rollback`) are now `cfg`-gated with portable fallbacks, so the library compiles and its test suite runs on any OS instead of only on Windows targets.
- Inno Setup script generation: the desktop-icon task (and its icon) is now emitted only when a desktop shortcut is requested; previously the task was always emitted and its flag was hard-coded `"unchecked"` in both branches.
- `build.rs` no longer references `resources/rustpak.ico` unconditionally (the file was never in the repo); the icon is embedded when present. (The 2.0.0 notes below claimed an embedded icon shipped — it had not.)

### Added
- **Extraction, the missing half of the native engine:** `OverlayReader::extract` / `extract_file` unpack a built installer, validating every stored path (absolute paths and `..` traversal are rejected) before writing. New CLI modes: `--list <packed.exe>`, `--extract <packed.exe> --to <dir>`, and `--install [--to <dir>]`. A built installer run with no arguments now extracts its own payload next to itself, into a folder named after the packaged app — previously it opened the packager GUI and the payload could never be recovered.
- **Installer backends are real code now:** the Inno Setup, NSIS, and native backends (previously orphaned files that referenced undeclared crates — `anyhow`, `serde`, `zip`, `walkdir` — and were not part of the crate) are ported to the standard library only, wired into `lib.rs`, and selectable from the CLI via `--engine native|inno|nsis` plus `--script-only`. The native backend is reimplemented on the overlay engine (`OverlayWriter::build_from_folder`); the old ZIP-plus-missing-`sfx_stub/stub_template.bin` design is gone.
- **The GUI Build button builds:** Browse now stores the chosen folder (and shows it), Build asks for the output file and packages on a worker thread with a transactional snapshot of any existing output, reporting success/failure in the status line. Previously Build only changed the status text and packaged nothing.
- **Test suite (19 tests):** overlay pack → read → extract round-trip, traversal rejection, synthetic-PE architecture inspection, scanner counts/heuristics, rollback create/modify/drop semantics, and Inno/NSIS/native script-generation contracts. CI previously ran no tests at all.
- `scanner::format_bytes`, manifest/record string accessors (`app_name()`, `relative_path()`, …), `RestartManagerSession::affected_process_count()`, `--help` / `--version`, `--app-name` / `--app-version` / `--publisher` batch flags, and a committed `Cargo.lock`.

### Removed
- `src/gui.rs`: the abandoned egui/eframe prototype from the pre-2.0 design. It was never compiled (not declared in `lib.rs`), referenced scanner APIs that do not exist, and contradicted the 2.0 native-Win32 mandate. The native Win32 GUI in `win32_ui.rs` is the only GUI.

### Notes
- README corrected to match reality: the native engine extracts files only (shortcuts/uninstaller belong to the Inno/NSIS scripts), Restart Manager is a library capability rather than something the packager does to running apps, and the performance figures are stated as design intent instead of measured guarantees.

## [2.0.0] - 2026-10-16
### Added
- **Rebranding to RustPak:** Complete transition to the RustPak identity, including dedicated Black & Crimson Red branding, vector emblem, and embedded multi-resolution Windows icon (`rustpak.ico`).
- **Uncompressed Direct PE Overlay Engine:** Implemented 4096-byte sector-aligned binary overlay streaming (`RUSTPAK_OVERLAY\0` magic) with zero compression latency, sustaining native NVMe bus write throughput.
- **Pure Win32 GDI Architecture:** Removed external GUI frameworks in favor of a native Win32 window class (`RustPakWindowClass`), message loop, and custom dark matte GDI brushes with 0.00% idle CPU and sub-8 MB RAM resident footprint.
- **Dynamic Windows Restart Manager:** Added runtime linking to `rstrtmgr.dll` (`RmStartSession`, `RmRegisterResources`, `RmShutdown`, `RmRestart`) with graceful fallback on legacy Windows NT 6.1 / Windows 7 systems.
- **Transactional Rollback Journaling:** LIFO state tracking of filesystem and registry alterations with automatic rollback execution on unhandled faults or user aborts.
- **COFF Header Architecture Inspection:** Instant binary parsing of `IMAGE_FILE_HEADER.Machine` for x86, x64, and ARM64 detection without DLL loading or external dependency overhead.
- **Universal Long Path Support:** Transparent `\\?\` prefix injection across all Win32 file system operations to eliminate the 260-character `MAX_PATH` limit.
- **Embedded Win32 Application Manifest:** Enforced `asInvoker` privilege level with programmatic self-elevation via `ShellExecuteExW("runas")` strictly on demand.

### Changed
- Replaced third-party compression crates with direct sector-aligned streaming.
- Replaced external folder picker libraries with pure COM `IFileOpenDialog` FFI.
- Optimized release profile with `opt-level = "z"`, `lto = true`, `codegen-units = 1`, and `panic = "abort"`.
