# SeanPak — Quality Record

Honest status as of the 4.2.0 (Builder studio) build. Anything not proven on a
real Windows machine is marked **Unverified** — VM/CI evidence is never
presented as Windows runtime proof.

## Gate status

| Check | Status |
|---|---|
| `cargo fmt --check` | Verified locally (studio gate; also Slice 2 gate) |
| `cargo clippy --all-targets -- -D warnings` (host) | Verified locally (studio gate; also Slice 2 gate) |
| `cargo test` (host, incl. `tests/golden_package.rs`) | Verified locally (studio gate; also Slice 2 gate) |
| `cargo check --all-targets` for `x86_64-pc-windows-gnu` | Verified locally (studio gate; also Slice 2 gate) |
| `cargo clippy --all-targets -- -D warnings` for `x86_64-pc-windows-gnu` | Verified locally (studio gate; also Slice 2 gate) |
| Builder studio model (document open/sync/validate/save, settings round-trip, scan rows + packed estimates, copy-in helpers) | Verified locally on the host build (unit tests in `src/studio.rs`) |
| Builder studio operation chain (open manifest → scan → sync → validate → `pack::pack` → `verify_package`) | Verified locally on the host build (unit test walks the exact chain the buttons run); CLI validate/pack/verify smoke re-run after the GUI swap |
| AI-control CLI end-to-end (schema → validate → pack → verify → install `--dry-run` → install → uninstall, incl. overwrite refusal and exit codes) | Verified locally on the Linux host build (packed installer is the same binary + overlay, so the full lifecycle ran); Windows runtime remains Unverified below |
| Slice 2 lifecycle on the host build (pack with compressible payloads → smaller than stored-only → verify → `verify --sandbox` → install with receipt → upgrade → downgrade refused with exit 5 → repair restores a corrupted file → uninstall leaves only the running uninstaller copy + its log) | Verified locally (Slice 2 gate smoke + golden-package test) |
| Compression round-trip (LZ codec on host; v4 packages smaller, byte-identical extraction; v3/v2/legacy packages still read + install) | Verified locally (Slice 2 gate) |
| Rollback journal uniqueness fix (same-millisecond transaction collision) | Verified locally — the collision was reproduced by the test suite and fixed |
| Installer stub size | Measured: 635,904 bytes for a Windows PE release build (gnu-linked) and 607,384 bytes for the host release build — far under the blueprint's <15 MB target; CI enforces a 4 MiB budget on the x86_64 MSVC leg (first hosted run Unverified) |
| Shipped Windows binary imports | Verified for the gnu-linked PE: system DLLs only (kernel32, advapi32, cabinet, comctl32, comdlg32, gdi32, ole32, shell32, user32, msvcrt, ntdll) — nothing unshipped |
| Wizard pages 1–4 render / behave on real Windows | **Unverified** — needs SEAN's on-machine run |
| Install on real Windows (shortcuts, associations, PATH, ARP, receipt) | **Unverified** |
| Upgrade / repair / uninstall artifact removal on real Windows | **Unverified** |
| Windows Compression API codec path on real Windows | **Unverified** (the LZ fallback path is host-verified; the API path is compile-verified for the Windows target) |
| `--uninstall` round-trip on real Windows | **Unverified** |
| Builder studio window on real Windows (4.2.0): layout/DPI, table, panel binding, worker pack/verify, tooltips, persistence | **Unverified** — compile-verified for the Windows target in the local gate; needs SEAN's on-machine run |
| Idle RAM / launch time on Windows | **Unverified** — not yet measured |
| Release assets (Setup.exe + Portable.zip + SHA256SUMS.txt) | **Unverified** — no 4.1.0 release published yet |

## Design conformance

- TesselGUI rev2 palette constants applied to the wizard and the studio
  window background/text (native Win32 controls take colors only as far as
  the platform allows; the studio's primary Build Setup button is
  owner-drawn to carry the exact app-green).
- Wizard layout follows the approved Design C mockup (ledger review table,
  Item / Value / Change, footer with keyboard hint + Cancel / Install),
  extended in Slice 2 with the Options / Progress / Finish pages in the
  same language and the app monogram tile in the header.
- Builder layout follows the approved Design D mockup (dominant files
  table, APP MANIFEST panel, OUTPUT strip, footer with status + Test
  install + green Build Setup). Two deliberate deviations: the panel
  omits the mockup's License row (the manifest schema has no license
  field), and it adds the fields the schema does have that the mockup
  didn't draw (primary exe, install location, file associations, PATH
  toggle, files/output folders) — the scope rule was "every field the
  manifest supports".
- Scores are deliberately not invented: no numeric QUALITY score is claimed
  until the Windows field pass exists.
