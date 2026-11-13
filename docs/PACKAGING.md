# Packaging a project with SeanPak

SeanPak is the standing packaging route for all of SEAN's Windows
programs. A project ships exactly three release assets, all produced by
one `seanpak pack` run:

- `<App>-v<Version>-x64-Setup.exe` — the installer (wizard + silent CLI)
- `<App>-v<Version>-x64-Portable.zip` — the portable build
- `SHA256SUMS.txt` — hashes of both

## The manifest

Each project keeps a SeanPak manifest (JSON) describing the app — name,
version, publisher, main executable, and the install options (desktop /
Start Menu shortcuts, file associations, PATH entry). Run
`seanpak schema` for the full field list and `seanpak validate
<manifest.json>` to check one before packing. The manifest is the same
file a local AI uses for the headless packaging pipeline
(`seanpak pack <manifest.json> <files-dir> --json`).

New projects can start from `seanpak init <dir>`: it writes a starter
manifest (primary executable detected; the power blocks below present
as inert examples) plus a `PACKAGING.md` stub with the pack command,
and validates the generated manifest before reporting success. An
existing manifest is never overwritten without `--force`.

## Power features (4.6.0 engine, 4.7.0 surface, overlay v6)

Five optional manifest blocks for installers that need more than the
basics. All five live in the shared install operation, so the wizard,
the CLI, and the sandbox proof all see the same behavior; `schema` and
`validate` cover them, and the Builder studio's manifest pane edits
them through its Power blocks JSON editor (the same encoding).

- **`components`** — optional parts of the package:
  `[{id, name, description?, files, required?, defaultSelected?}]`.
  Files no component claims form the implicit required part `core`.
  Installs extract only the selected components: the wizard's Options
  page offers one checkbox per optional component (the Review ledger
  reports "N of M parts"), the CLI takes `--components <id,id>`
  (explicit selection) or `--without <id,id>`, and an upgrade defaults
  to the previous install's choices. The receipt records what was
  installed; uninstall removes exactly those files.
- **`hooks`** — `{preInstall: [hook], postInstall: [hook]}` with
  `hook = {command, args?, onFailure: "abort" | "continue"}`.
  Pre-install hooks run after the writability check, before any file
  changes, from the installer's own folder; post-install hooks run
  after everything is applied, from the install folder, before the
  receipt is written. Both get `SEANPAK_INSTALL_DIR`,
  `SEANPAK_APP_NAME`, and `SEANPAK_APP_VERSION` in their environment.
  An `abort` failure stops and rolls back SeanPak's changes (a hook's
  own changes are never rolled back — a command can do anything); a
  `continue` failure is recorded as a warning. Every run is recorded
  in the receipt. Repair runs execute only the post-install phase.
  There are no uninstall hooks yet.
- **`registryWrites` / `envWrites`** — per-user values the install
  sets: `[{key (HKCU-relative), name, type: "sz" | "dword", value}]`
  and `[{name, value}]` for `HKCU\Environment`. HKCU only — the
  validator refuses hive prefixes and escapes. Before writing, the
  installer records each value's prior state (value and type, or
  "absent", plus whether the key was created) in the receipt;
  uninstall restores the priors exactly, deleting values that were
  absent and keys the install created when they are empty again.
- **`shortcuts`** — rich shortcuts beyond the automatic app shortcut:
  `[{name, target, arguments?, icon?, iconIndex?, description?,
  location: "desktop" | "startMenu" | "both", pin?}]`. Targets and
  icons must be packaged files, and a shortcut is created only when
  its target's component is installed. `pin: true` asks Windows to
  pin the shortcut to the taskbar — best-effort only: the outcome
  (`pinned` / `unavailable`) is recorded in the receipt and the log,
  and never fails the install.

## Modify mode (4.7.0)

An installed app can be changed in place — components added or
removed, the shortcut / association / PATH / start-at-login choices
flipped — without a full reinstall:

```powershell
.\AppSetup.exe --modify [--to "D:\Apps\MyApp"] [--components core,extras] [--without docs] `
    [--start-at-login|--no-start-at-login] [--desktop-shortcut|--no-desktop-shortcut] `
    [--start-menu-shortcut|--no-start-menu-shortcut] [--path-entry|--no-path-entry] `
    [--file-associations ".txt,.md"] [--dry-run] [--json]
```

The install is located by its receipt (`--to`, else the package's
recorded location, else the Add/Remove Programs entry). The wizard
opens in Modify mode for a bare interactive `--modify`, and the ARP
entry's **Modify** button runs the same command (installs made before
4.7.0 gain the button the first time a 4.7.0 modify runs). Before
anything changes, the receipt and every file the reconcile will
delete or overwrite are snapshotted; any failure rolls back to the
pre-modify state. Modify runs no install hooks. A folder with no
SeanPak receipt is refused ("nothing to modify", exit 5).

## Answer files (4.7.0)

An answer file presets an install — location, every option, the
component selection:

```json
{
  "installLocation": "D:\\Apps\\MyApp",
  "desktopShortcut": true,
  "startMenuShortcut": true,
  "startMenuFolder": "MyApp",
  "fileAssociations": [".txt", ".md"],
  "pathEntry": false,
  "startAtLogin": false,
  "launchAfterInstall": false,
  "components": ["core", "extras"],
  "withoutComponents": [],
  "purgeDataOnUninstall": false
}
```

```powershell
.\seanpak.exe answers-template "dist\MyApp-v1.0.0-x64-Setup.exe" --json > answers.json
.\AppSetup.exe --install --answers answers.json --json
```

Every field is optional; priority is explicit flags > answer file >
package defaults. The format is strict: unknown fields are errors,
validation messages name the field, and a malformed file exits 2.
The wizard offers an `answers.json` found beside the Setup.exe on its
Review page (Apply preset / Ignore — never applied silently).
`purgeDataOnUninstall` records a default in the receipt: the
interactive uninstall confirmation's data-purge checkbox starts in
that state (the user still decides; silent uninstalls still require
`--purge-data`).

## Inspecting and comparing packages (4.7.0)

```powershell
.\seanpak.exe inspect "dist\MyApp-v1.0.0-x64-Setup.exe" [--json]
.\seanpak.exe diff "dist\MyApp-v1.0.0-x64-Setup.exe" "dist\MyApp-v1.1.0-x64-Setup.exe" [--json]
```

`inspect` reports identity, options, components with packed sizes,
hooks, writes, shortcuts, and the per-file list (size, stored size,
compression, owning component). `diff` compares payloads by content
hash and reports files added / removed / changed plus option and
component deltas; exit **0** = identical, **6** = differences found,
**3** = a package could not be read.

## Caller workflow (GitHub Actions)

A project's release workflow calls SeanPak's reusable workflow, then
attaches the uploaded assets to its GitHub Release:

```yaml
name: Release

on:
  push:
    tags: ["v*"]

jobs:
  package:
    uses: SeanShaffer88/SeanPak/.github/workflows/pack-reusable.yml@main
    with:
      manifest: seanpak.json        # path in this repo
      files: dist/files             # payload directory in this repo
      out: dist-seanpak             # optional; this is the default

  release:
    needs: package
    runs-on: ubuntu-latest
    permissions:
      contents: write
    steps:
      - name: Download SeanPak assets
        uses: actions/download-artifact@v4
        with:
          name: seanpak-release-assets
          path: release-assets
      - name: Attach assets to the GitHub Release
        uses: softprops/action-gh-release@v2
        with:
          files: |
            release-assets/*-Setup.exe
            release-assets/*-Portable.zip
            release-assets/SHA256SUMS.txt
```

Notes:

- The reusable workflow builds SeanPak from source on a Windows runner
  (MSVC, static CRT) and runs the pack there, so the shipped stub is the
  real Windows build.
- Pin `seanpak-ref` (a tag or commit) in the `with:` block if a project
  needs to stay on a known SeanPak version instead of tracking `main`.
- Before cutting a release, `seanpak verify <Setup.exe> --sandbox`
  gives an end-to-end local proof: silent install into a temp dir,
  byte-for-byte file comparison, silent uninstall, residue check.
