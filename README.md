# SeanPak

## TL;DR
SeanPak turns a folder containing your finished program into a single setup file you can hand to other people. They run that one file and your program unpacks itself onto their computer — no zip files to wrestle with, and nothing else they need to install first. It's free, works offline, and stays small and fast because it's built from Windows' own built-in parts rather than bulky add-ons.

## Description
SeanPak is a low-overhead native Windows installer packager written in Rust against direct Windows API bindings (`windows-sys`). It appends your program's files — each compressed, or stored as-is when compression would not shrink it — to a copy of its own executable as a PE overlay (`SEANPAK_OVL`), producing a single self-contained `Setup.exe`. That file is a real installer, not an archive: run it and the installer wizard opens; run it with flags and it installs, repairs, modifies, or uninstalls headlessly. The same executable is also the packager and the Builder studio — one tool authors the package, builds it, verifies it, and installs it.

SeanPak is built to be driven by a local AI: packaging is the primary workflow, every capability exists on the command line with `--json` output, and the Builder GUI is a secondary surface over the same operations.

## Features

**Packaging**
- One `pack` command produces exactly the release set: `<App>-v<version>-x64-Setup.exe`, `<App>-v<version>-x64-Portable.zip`, and `SHA256SUMS.txt`
- Per-file compression — the Windows Compression API (XPRESS Huffman) on Windows, a built-in LZ codec elsewhere — with files stored uncompressed when compression would not shrink them
- Sector-aligned PE-overlay format; the reader also accepts packages written by the CrucibleKit and RustPak eras (`CRUCIBLEKIT_OVL`, `RUSTPAK_OVERLAY`)
- Safe extraction: every stored path is validated before writing — absolute paths and `..` traversal are rejected, so a crafted installer cannot write outside the destination
- `verify` proves a built package (overlay readable, manifest sane, file list matches payloads); `verify --sandbox` goes further — silent install into a fresh temp folder, byte-for-byte comparison, silent uninstall, residue check
- Alternative engines: generate or compile Inno Setup / NSIS installers (`--engine inno|nsis`, `--script-only`), plus the legacy `--batch` packaging mode
- A reusable GitHub Actions workflow (`pack-reusable.yml`) so other projects package with SeanPak in CI
- Windows Restart Manager awareness: hosts can detect processes locking files; the packager never force-closes your apps

**Built for AI and scripts — the primary workflow**
- Every capability is available from the command line; nothing is GUI-only
- `--json` machine-readable output on every command, plus a JSON-lines outcome log (`seanpak.log`) beside the executable
- `--headless`, `--silent`, and `--dry-run` — a dry run prints the exact plan (locations, components, hooks, writes) and changes nothing
- Deterministic, documented exit codes (`0`–`6`; `6` = `diff` found differences)
- `schema` prints the manifest JSON Schema; `validate` checks a manifest and reports plain-language errors
- `inspect` reports a package: identity, options, components with packed sizes, hooks, registry/environment writes, shortcuts, and per-file sizes and compression
- `diff` compares two packages — files added/removed/changed by content hash, plus identity, option, and component deltas
- `init` scaffolds a project: a starter `manifest.json` (primary executable detected, power blocks present as inert examples) and a `PACKAGING.md` stub; the generated manifest is validated before `init` reports success
- `answers-template` prints an answer file filled with a package's own defaults

**Installing**
- Per-user installs by default: no administrator rights are ever *required*, and nothing elevates on its own. A pre-flight writability check refuses an unwritable destination *before* anything changes and suggests the per-user folder instead — the wizard offers that folder in one click. The one user-invoked exception (4.8.0): when a chosen folder cannot be written by this account, the wizard also offers **Restart as administrator** beside the per-user remedy, saying plainly that the install would then run as the administrator account; declining the Windows prompt leaves the wizard open with the per-user offer intact
- A TesselGUI Rail wizard (Review → Options → Installing → Done) over the same install operation the CLI runs; cancelling mid-install rolls everything back
- Desktop and Start Menu shortcuts, file associations (the previous default handler is captured and restored by uninstall), and a PATH entry — all journaled
- Per-user install locations: `installLocation` accepts `%LOCALAPPDATA%` / `%APPDATA%` / `%USERPROFILE%` token paths resolved on the installing account, and the default location is the per-user Programs folder — never Program Files
- Silent auto-update: re-running a Setup for an already-installed app (same or older version) updates it in place behind a minimal window instead of opening the wizard; a running program is asked to close, and relaunched after a successful update
- Launch after install, offered and checked per the manifest (offered + checked by default for installers created now); headless installs launch only with an explicit `--launch`
- Start at login via a per-user Run entry, journaled and removed by uninstall
- Optional components with packed sizes on the wizard's Options page (`--components` / `--without` on the CLI); upgrades default to the previous install's selection
- Pre/post install hooks with per-hook abort/continue failure policies; every run is recorded in the receipt
- Journaled HKCU registry writes and user environment writes: prior values (or their absence) are captured exactly and restored by uninstall — a pre-existing value of a type SeanPak does not model refuses the install rather than risking data it cannot restore
- Rich shortcuts with arguments, custom icons and descriptions, and a best-effort taskbar pin whose outcome is recorded and never fails the install
- Upgrade in place, `--repair`, and downgrade refusal unless `--force`
- Modify mode: add or remove components and flip install options in place — from the CLI, the wizard, or the Modify button in Add/Remove Programs — with the pre-modify state snapshotted for rollback
- Answer files preset an install (explicit flags > answer file > package defaults); the wizard offers an `answers.json` found beside the Setup.exe on its Review page — Apply or Ignore, never applied silently
- A transactional rollback journal: any failure restores the previous state, and every install writes a receipt (`seanpak-receipt.json`) recording files, artifacts, component selection, hook runs, and write priors
- The uninstaller asks before deleting the app's data folders (opt-in checkbox, off by default); silent uninstalls delete data only with an explicit `--purge-data`

**Builder studio**
- A guided page flow over the same operations as the CLI (redesigned in 5.0.0): Home with recent projects, a three-step New Project flow, an editor with Files and Folders / Installer Options / Program Requirements tabs, then Build Settings, Building, and Build Finished pages — all labeled in plain English with a plain tooltip on every main control
- Recent projects are remembered in the Builder's own settings file beside the exe (no registry writes); clicking one reopens it in the editor, and the Projects and Settings pages manage the list
- One way in: **Program Folder…** picks the folder your built program is in and fills the table, listing, and totals from it; files a manifest lists beyond the folder still show (marked missing) and can be removed with **Remove selected**
- Edits the real `manifest.json` — identity, install location, associations, shortcut and PATH toggles, launch/login offers with their defaults, data folders, and the Advanced options (components, hooks, writes, shortcuts) as JSON on the Installer Options page
- Build Setup / Check details / Verify package (with **Also test-install** for the sandbox run) run on a worker thread — and every long operation can be stopped with the **Cancel** button; a stopped build removes its unfinished files and never touches outputs that existed before it. Building shows a progress bar and a timestamped log; Build Finished names the outputs and offers **Open the Installer** and **Open Folder**
- **Open output folder** opens Explorer at the built files (selecting the Setup file), and **Preview installer** launches the built Setup file so you see the exact installer your users will see
- Window geometry and last-used paths persist between runs

**Engineering**
- Rust + `windows-sys` only: zero third-party runtime dependencies, static CRT, and the shipped executables import Windows system libraries only
- Zero telemetry, fully offline, no account, no network code paths in the installer
- TesselGUI 1.4.0 dark theme (GitHub Dark High Contrast values) from one shared theme module, test-pinned to the published tokens

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
- **Privilege separation (UAC):** Embedded manifest execution level is `asInvoker`. Installs are per-user (shortcuts, associations, and PATH all land under HKCU / the user profile), so neither the packager nor a built installer asks for administrator rights. Elevation exists only as the user's explicit choice at an unwritable folder (the wizard's / confirmation's Restart as administrator button, or the CLI's `--elevate` flag) — a runtime relaunch, never a manifest demand, never the default; headless refusals report it as `"elevationOffered"` in JSON.
- **x64 Windows:** Release builds target `x86_64-pc-windows-msvc` only. Pure-logic modules (overlay, scanner, rollback file journal, and script backends) are platform-independent and unit-tested on any host OS.

## Installer engines
| Engine | Flag | Output | Needs |
|---|---|---|---|
| Native overlay | `--engine native` (default) | Self-extracting `Setup.exe` (this program + payload) | Nothing |
| Inno Setup | `--engine inno` | Compiled Inno Setup 6 installer | `ISCC.exe` installed |
| NSIS | `--engine nsis` | Compiled NSIS installer (MUI2) | `makensis.exe` installed |

Use `--script-only` with the Inno/NSIS engines to generate the `.iss` / `.nsi` script without compiling it. The native overlay engine is the full-featured route — its install applies shortcuts, file associations, PATH, and the startup entry itself, all journaled for uninstall (see `docs/PARITY.md` for the exact matrix); the Inno/NSIS engines remain for projects that specifically want those script formats.

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
    ├── answers.rs         # Answer files: strict preset parser + template (4.7.0)
    ├── codec.rs           # Per-file compression (Windows Compression API / LZ fallback)
    ├── hooks.rs           # Pre/post install hooks (4.6.0)
    ├── init.rs            # `seanpak init` project scaffolding (4.7.0)
    ├── inspect.rs         # `inspect` / `diff` package reports (4.7.0)
    ├── install.rs         # Shared install operation (wizard + CLI): plan, perform, rollback
    ├── manifest.rs        # JSON app manifest: parser, validation, JSON Schema text
    ├── modify.rs          # Shared modify operation (wizard + CLI + ARP Modify) (4.7.0)
    ├── native_dialog.rs   # Native folder / save-file / icon pickers (SHBrowseForFolder + comdlg32)
    ├── overlay.rs         # Sector-aligned PE overlay writer, reader, and extractor
    ├── pack.rs            # Headless pack pipeline + verify (release set, stored ZIP, sums)
    ├── receipt.rs         # Install receipt (seanpak-receipt.json) model + journal records
    ├── restart_manager.rs # Dynamic rstrtmgr.dll binding with RAII session lifecycle
    ├── rollback.rs        # Transactional filesystem/registry rollback log
    ├── scanner.rs         # Recursive directory scanner + COFF PE architecture inspector
    ├── studio.rs          # Builder studio GUI (guided page flow since 5.0.0) over the same pack/verify operations
    ├── sysinstall.rs      # Shortcuts, associations, PATH, Run entry (COM + HKCU, journaled)
    ├── theme.rs           # TesselGUI 1.4.0 dark palette + shared control painters (4.7.1)
    ├── uninstall.rs       # Shared uninstall operation (receipt-driven, data-purge opt-in)
    ├── wizard.rs          # Installer wizard (Design E Rail layout) over install.rs
    └── writes.rs          # Journaled HKCU registry + environment writes (4.6.0)
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
# Home / Projects / editor / Build pages over the same
# pack/validate/verify operations the CLI exposes (see below)
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
# this is the explicit form, with an optional destination. Run again
# for an app that is already installed at the same version or older
# and it updates in place instead — no pages, just a small "Updating"
# window (see "Silent auto-update" below). --to also accepts a
# per-user token path such as "%LOCALAPPDATA%\Programs\MyApp".
.\AppSetup.exe --install [--to "D:\Apps\MyApp"]
```

**Headless packaging — the primary workflow.** SeanPak is designed to be
driven by scripts and by a local AI: every command supports `--json`
machine-readable output, and build/install operations append JSON-lines
records to `seanpak.log` next to the executable so outcomes can be read
back and confirmed.

```powershell
# Scaffold a project: starter manifest.json (primary exe detected,
# power blocks as inert examples) + a PACKAGING.md stub. The generated
# manifest is validated before init reports success; an existing
# manifest is kept unless --force.
.\seanpak.exe init "C:\AppBuild" [--name "MyApp"] [--force] --json

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

# Read a package: identity, options, components with packed sizes,
# hooks, writes, shortcuts, per-file sizes and compression.
.\seanpak.exe inspect "dist\MyApp-v1.0.0-x64-Setup.exe" --json
# Compare two packages (files added / removed / changed by content
# hash, option and component deltas). Exit 0 = identical payloads,
# 6 = differences found, 3 = a package could not be read.
.\seanpak.exe diff "dist\MyApp-v1.0.0-x64-Setup.exe" "dist\MyApp-v1.1.0-x64-Setup.exe" --json

# Install headlessly — the same operation the wizard's Install button
# runs. --dry-run prints the exact plan (JSON with --json) and changes
# nothing; --silent / --headless suppress interactive output. Install
# applies the manifest's shortcuts, file associations, and PATH entry
# (all journaled for uninstall), writes seanpak-receipt.json into the
# install dir, and detects an existing install: same version repairs
# with --repair, a newer version makes downgrade refused unless
# --force, an older one upgrades in place. Installs are per-user and
# never elevate: a destination your account cannot write (for example
# C:\Program Files) is refused before anything changes — the JSON
# report carries reason "location_not_writable" and a suggested
# per-user location (%LOCALAPPDATA%\Programs\<App>), and --dry-run
# plans include the same verdict as locationWritable /
# suggestedLocation. The refusal and the dry-run plan also carry
# "elevationOffered" when the folder could be reached by restarting
# as administrator; adding --elevate performs that restart (Windows
# shows its prompt), waits, and returns the elevated run's exit code.
# A successful install's report carries "action": "install", or
# "action": "update" when an existing install was updated in place.
.\AppSetup.exe --install --to "D:\Apps\MyApp" --json
.\AppSetup.exe --install --to "D:\Apps\MyApp" --repair --json
# --launch starts the app afterwards (an explicit opt-in; headless
# installs never launch by default, and the JSON report records
# launchRequested / launchPerformed). --start-at-login /
# --no-start-at-login override the manifest's start-at-login default.
.\AppSetup.exe --install --to "D:\Apps\MyApp" --launch --json
# Preset an install with an answer file (location, every option, the
# component selection). `answers-template` prints one with the
# package's defaults filled; explicit flags still win over the file,
# and a malformed file is a usage error (exit 2) naming the field.
# The wizard also offers an answers.json found beside the Setup.exe
# on its Review page — Apply / Ignore, never applied silently.
.\seanpak.exe answers-template "dist\MyApp-v1.0.0-x64-Setup.exe" --json > answers.json
.\AppSetup.exe --install --answers answers.json --json
# Change an existing install in place: add/remove components, flip
# the shortcut / association / PATH / start-at-login choices. The
# pre-modify state is snapshotted first; any failure rolls back to
# it. The Add/Remove Programs Modify button runs the same operation,
# and a bare interactive --modify opens the wizard in Modify mode.
.\AppSetup.exe --modify --to "D:\Apps\MyApp" --components core,extras --json
.\AppSetup.exe --modify --to "D:\Apps\MyApp" --dry-run --json
# Interactive uninstall asks first (a native confirmation window);
# silent / --json uninstalls never ask and never delete the app's
# data folders unless --purge-data is passed explicitly.
.\AppSetup.exe --uninstall --to "D:\Apps\MyApp" --json
.\AppSetup.exe --uninstall --to "D:\Apps\MyApp" --purge-data --json
```

**Install options in the manifest.** `installLocation` says where
the app installs: an absolute path, or a per-user token path in the
same grammar as `dataDirs` — `%LOCALAPPDATA%\Programs\MyApp`
resolves on the installing account at install time. When a manifest
omits it, the package records `%LOCALAPPDATA%\Programs\<App>`:
installs are per-user by default, and Program Files is never the
default (a manifest may still name it explicitly). The wizard always
shows the resolved folder. Beyond shortcuts, associations,
and PATH, a manifest can declare: `launchAfterInstall` — whether the
wizard's Finish page offers a "Launch <App>" checkbox and whether it
starts checked (default: offered, checked); `startAtLogin` — whether
the installer offers "Start <App> at login", a per-user Run entry
journaled like the other artifacts and removed by uninstall (default:
not offered); and `dataDirs` — the app's data folders (`%APPDATA%`,
`%LOCALAPPDATA%`, `%USERPROFILE%`, `%INSTALLDIR%` tokens or
install-relative paths), recorded in the install receipt so the
uninstall's opt-in checkbox can offer to delete them. The two option
fields accept a boolean or `{"offered": bool, "defaultChecked": bool}`;
`seanpak schema` documents every field.

**Creation defaults (5.1.0).** Installers created now — by
`seanpak init`, by the Builder's New Project, or by packing a
manifest that omits the fields — default to creating a desktop
shortcut and to offering launch-after-setup already ticked.
A manifest that sets `desktopShortcut` or `launchAfterInstall`
explicitly (including `false`) is never overridden.

**Silent auto-update (5.1.0).** When a Setup wizard starts and the
same app is already installed at the package's target location (the
install receipt there says so) at an older or the same version, the
wizard never opens: a minimal window runs the upgrade in place,
shows "Updated to <version>.", and closes itself after a few
seconds. If the program is running, it is asked to close first; a
program that refuses earns one dialog — **Close it and update** or
**Cancel** — and a successfully updated program that had been
running is relaunched. A failed update rolls back and shows the
error. An installed version newer than the package still gets the
normal wizard and its downgrade refusal.

**Power features (4.6.0 engine, 4.7.0 surface — the Power bundle is
complete).** A manifest can also declare
`components` (optional parts of the package; files no component claims
form the required `core` part), `hooks` (pre/post install commands
with an abort/continue failure policy), `registryWrites` (HKCU values)
and `envWrites` (user environment variables, restored by uninstall),
and `shortcuts` (rich shortcuts with arguments, icons, and a
best-effort taskbar pin). See `docs/PACKAGING.md` for the shapes and
the install-time rules; the wizard offers component checkboxes on its
Options page, and `--install` takes `--components <id,id>` /
`--without <id,id>`. On top of the engine, 4.7.0 adds **Modify mode**
(`--modify`, the wizard's Modify mode, and the ARP entry's Modify
button), **answer files** (`--install --answers`, `answers-template`,
the wizard's Review-page offer), **`inspect` / `diff`** for packages,
**`seanpak init`** scaffolding, and the Builder studio's **Power
blocks** JSON editor on its Installer Options page.

**Packaging other projects in CI.** SeanPak is the standing packaging
route for all of SEAN's Windows programs: a project's release workflow
calls the reusable workflow in `.github/workflows/pack-reusable.yml`,
which builds SeanPak, runs `seanpak pack` with the project's manifest
and files, and uploads the three assets. See `docs/PACKAGING.md` for
the caller-workflow example.

**Exit codes** (deterministic; also printed by `--help`): `0` success ·
`1` generic/GUI failure · `2` usage error · `3` bad or missing
manifest/overlay · `4` I/O failure (including a refused overwrite) ·
`5` install failed (destination rolled back; an unwritable destination is refused before anything changes, with JSON reason `location_not_writable`) ·
`6` differences found (`diff` only).

## Known limitations (honest list)
- The Builder studio is the secondary surface: it edits a `manifest.json`, curates the package file list, and runs the same `pack` / `validate` / `verify` operations as the CLI — nothing is GUI-only. Since 5.0.0 it is a guided page flow (Home, Projects, a three-step New Project flow, an editor with Files and Folders / Installer Options / Program Requirements tabs, Build Settings, Building, Build Finished) replacing the 4.x IDE window; recent projects persist in its settings file beside the exe. The 4.x studio ran on a real Windows desktop (the v4.7.0 and v4.7.1 builds, 2026-11-13; the 4.7.1 header defect and its 4.7.2 subclass-paint fix are in the changelogs). The 5.0.0 page flow — layouts, owner-drawn nav/tabs, the step rail, the include-checkbox listing, the progress and Finished pages — is **Unverified** until the next on-machine field pass, and current screenshots of the new pages are owed from that pass rather than faked.
- Payloads are compressed per file (Windows Compression API on Windows; a built-in LZ codec elsewhere) and stored uncompressed when compression would not shrink them. Packages written by older SeanPak/CrucibleKit/RustPak versions remain readable and installable.
- Windows GUI behavior is compile-verified in CI on all three targets; interactive on-machine testing is still recommended before shipping an installer to users. `docs/PARITY.md` tracks exactly what is shipped and what remains.

## License
Licensed under the MIT License. 100% Free and Open Source Software (FOSS).
