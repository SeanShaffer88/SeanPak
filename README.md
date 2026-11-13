# SeanPak

## TL;DR
SeanPak turns a folder containing your finished program into a single setup file you can hand to other people. They run that one file and your program unpacks itself onto their computer — no zip files to wrestle with, and nothing else they need to install first. It's free, works offline, and stays small and fast because it's built from Windows' own built-in parts rather than bulky add-ons.

## Description
SeanPak is a low-overhead native Windows installer packaging and distribution utility written in Rust against direct Windows API bindings (`windows-sys`). Instead of compressing payloads into a proprietary archive, SeanPak appends them uncompressed, aligned to 4096-byte sector boundaries, as a PE executable overlay — so packaging is a streaming file copy with no compression latency, and the resulting installer is a real executable that can inspect and extract itself.

A built native installer is the SeanPak executable plus your payload. Run it with no arguments and the SeanPak installer wizard opens (a one-page review of every install choice); `--install` extracts the payload headlessly, next to itself, into a folder named after your app.

## Download
Prebuilt binaries are on the **[latest GitHub Release](https://github.com/SeanShaffer88/SeanPak/releases/latest)**:

- `SeanPak-v<version>-x64-Setup.exe` — the installer
- `SeanPak-v<version>-x64-Portable.zip` — the portable build (executable, README, and application manifest)

`SHA256SUMS.txt` on the release page lists the checksums for both files.

## Design mandates
- **Minimal dependencies:** The only runtime crate is `windows-sys` (direct Win32 FFI definitions). No external runtimes, no VC++ redistributable requirement (`+crt-static`). Installer *script* engines (Inno Setup / NSIS) are driven as external tools, never linked.
- **Sector-aligned direct PE overlay:** Uncompressed raw payloads aligned to 4096-byte boundaries appended to the host PE, with a fixed-layout manifest (`SEANPAK_OVL` magic; the reader also accepts the legacy `CRUCIBLEKIT_OVL` and `RUSTPAK_OVERLAY` markers) recording every file's offset and size.
- **Safe extraction:** The overlay reader validates every stored path before writing — absolute paths and `..` traversal are rejected, so a crafted installer cannot write outside the chosen destination.
- **Windows Restart Manager API (`rstrtmgr.dll`):** The library wraps Restart Manager sessions (loaded dynamically, graceful fallback when unavailable) so hosts can detect processes locking files, query how many are affected, and shut them down / restart them around a file mutation. The packager itself never force-closes your apps.
- **Transactional rollback journaling:** Native builds snapshot an existing output file before overwriting; on failure the snapshot is restored (LIFO transaction log covering created/modified files and created directories, plus registry-key deletion on Windows).
- **Privilege separation (UAC):** Embedded manifest execution level is `asInvoker`. Installs are per-user (shortcuts, associations, and PATH all land under HKCU / the user profile), so neither the packager nor a built installer asks for administrator rights.
- **Windows 11 x64:** Release builds target `x86_64-pc-windows-msvc` only. Pure-logic modules (overlay, scanner, rollback file journal, and script backends) are platform-independent and unit-tested on any host OS.

## Installer engines
| Engine | Flag | Output | Needs |
|---|---|---|---|
| Native overlay | `--engine native` (default) | Self-extracting `Setup.exe` (this program + payload) | Nothing |
| Inno Setup | `--engine inno` | Compiled Inno Setup 6 installer | `ISCC.exe` installed |
| NSIS | `--engine nsis` | Compiled NSIS installer (MUI2) | `makensis.exe` installed |

Use `--script-only` with the Inno/NSIS engines to generate the `.iss` / `.nsi` script without compiling it. Shortcuts, file associations, and startup entries are features of the Inno/NSIS scripts; the native overlay engine's wizard installs the payload to the chosen location and registers an uninstaller (see `docs/PARITY.md` for exactly what the native install does and does not apply yet).

## Directory structure
```
seanpak/
├── app.manifest           # UAC (asInvoker), Common Controls v6, DPI & long-path awareness
├── build.rs               # Embeds manifest, version info, and the app icon
├── build.bat              # One-command release build on Windows
├── Cargo.toml             # LTO / size-optimized release profile, static CRT via .cargo/config.toml
├── CHANGELOG.md
├── CREDITS.md
├── resources/
│   ├── seanpak_logo.svg   # Vector emblem
│   └── seanpak.ico        # Multi-resolution app icon rendered from the emblem
└── src/
    ├── lib.rs             # Crate root re-exporting all modules
    ├── main.rs            # Entry point: CLI (batch / list / extract / install) + GUI dispatcher
    ├── backends/          # Installer engines: native overlay, Inno Setup, NSIS (std-only)
    ├── diag.rs            # Structured log (seanpak.log) incl. JSON-lines outcome records
    ├── hash.rs            # Hand-rolled SHA-256 / CRC-32 (no external crates)
    ├── install.rs         # Shared install operation (wizard + CLI): plan, perform, rollback
    ├── manifest.rs        # JSON app manifest: parser, validation, JSON Schema text
    ├── native_dialog.rs   # Native folder / save-file / icon pickers (SHBrowseForFolder + comdlg32)
    ├── overlay.rs         # Sector-aligned PE overlay writer, reader, and extractor
    ├── pack.rs            # Headless pack pipeline + verify (release set, stored ZIP, sums)
    ├── restart_manager.rs # Dynamic rstrtmgr.dll binding with RAII session lifecycle
    ├── rollback.rs        # Transactional filesystem/registry rollback log
    ├── scanner.rs         # Recursive directory scanner + COFF PE architecture inspector
    ├── studio.rs          # Builder studio GUI (Design D) over the same pack/verify operations
    └── wizard.rs          # Installer wizard (Design C ledger review) over install.rs
```

## Building
Prerequisites: Rust with the MSVC toolchain.

```powershell
cargo build --release --target x86_64-pc-windows-msvc
# or simply:
.\build.bat
```

## Usage
```powershell
# GUI — the Builder studio (optionally with a starting folder):
# a files table + manifest panel over the same pack/validate/verify
# operations the CLI exposes (see below)
.\seanpak.exe [C:\AppBuild]

# Headless packaging (CI/CD friendly, prints a JSON status line)
.\seanpak.exe --batch "C:\AppBuild" --out "dist\AppSetup.exe" `
    --app-name "MyApp" --app-version "1.0.0.0" --publisher "MyCompany"

# Generate an Inno Setup script instead of a native installer
.\seanpak.exe --batch "C:\AppBuild" --engine inno --script-only --out "dist\MyApp.iss"

# Inspect / unpack any SeanPak native installer
.\seanpak.exe --list "dist\AppSetup.exe"
.\seanpak.exe --extract "dist\AppSetup.exe" --to "C:\Unpacked"

# A built installer run with no arguments opens the installer wizard;
# this is the explicit form, with an optional destination:
.\AppSetup.exe --install [--to "D:\Apps\MyApp"]
```

**Headless packaging — the primary workflow.** SeanPak is designed to be
driven by scripts and by a local AI: every command supports `--json`
machine-readable output, and build/install operations append JSON-lines
records to `seanpak.log` next to the executable so outcomes can be read
back and confirmed.

```powershell
# Author a manifest (the format is self-describing):
.\seanpak.exe schema                          # prints the manifest JSON Schema
.\seanpak.exe validate app.json               # checks a manifest, plain-language errors

# Pack the release set: exactly <App>-v<version>-x64-Setup.exe,
# <App>-v<version>-x64-Portable.zip, and SHA256SUMS.txt, named from the
# manifest. Differing existing outputs are refused without --force.
.\seanpak.exe pack app.json "C:\AppBuild" --out-dir dist --json

# Prove a built installer before publishing it (overlay readable,
# embedded manifest sane, embedded file list matches the payloads).
# --sandbox goes further: silent-install into a fresh temp dir,
# compare every file byte-for-byte, silent-uninstall, residue check.
.\seanpak.exe verify "dist\MyApp-v1.0.0-x64-Setup.exe" --json
.\seanpak.exe verify "dist\MyApp-v1.0.0-x64-Setup.exe" --sandbox --json

# Install headlessly — the same operation the wizard's Install button
# runs. --dry-run prints the exact plan (JSON with --json) and changes
# nothing; --silent / --headless suppress interactive output. Install
# applies the manifest's shortcuts, file associations, and PATH entry
# (all journaled for uninstall), writes seanpak-receipt.json into the
# install dir, and detects an existing install: same version repairs
# with --repair, a newer version makes downgrade refused unless
# --force, an older one upgrades in place.
.\AppSetup.exe --install --to "D:\Apps\MyApp" --json
.\AppSetup.exe --install --to "D:\Apps\MyApp" --repair --json
.\AppSetup.exe --uninstall --to "D:\Apps\MyApp" --json
```

**Packaging other projects in CI.** SeanPak is the standing packaging
route for all of SEAN's Windows programs: a project's release workflow
calls the reusable workflow in `.github/workflows/pack-reusable.yml`,
which builds SeanPak, runs `seanpak pack` with the project's manifest
and files, and uploads the three assets. See `docs/PACKAGING.md` for
the caller-workflow example.

**Exit codes** (deterministic; also printed by `--help`): `0` success ·
`1` generic/GUI failure · `2` usage error · `3` bad or missing
manifest/overlay · `4` I/O failure (including a refused overwrite) ·
`5` install failed (destination rolled back).

## Known limitations (honest list)
- The Builder studio (Design D) is the secondary surface: it edits a `manifest.json`, curates the package file list, and runs the same `pack` / `validate` / `verify` operations as the CLI — nothing is GUI-only. Its Windows runtime behavior is **Unverified** until an on-machine field pass (the document/settings model and the full open → validate → pack → verify chain are covered by host tests).
- Payloads are compressed per file (Windows Compression API on Windows; a built-in LZ codec elsewhere) and stored uncompressed when compression would not shrink them. Packages written by older SeanPak/CrucibleKit/RustPak versions remain readable and installable.
- Windows GUI behavior is compile-verified in CI on all three targets; interactive on-machine testing is still recommended before shipping an installer to users. `docs/PARITY.md` tracks exactly what is shipped and what remains.

## License
Licensed under the MIT License. 100% Free and Open Source Software (FOSS).
