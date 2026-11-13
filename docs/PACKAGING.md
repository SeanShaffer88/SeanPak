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
