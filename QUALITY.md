# SeanPak — Quality Record

Honest status as of the 4.7.0 (Power bundle surface) build. Anything not proven on a
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
| Writability pre-flight (4.3.0): verdict + probe, per-user fallback derivation, `--dry-run` verdict fields, operation refusal before anything is created | Verified locally on the host build — unit tests (writable dir, path-under-a-file, read-only dir where permissions bite, fallback shape) plus an end-to-end smoke: pack a fixture → dry-run reports `locationWritable:false` + suggestion → real install into the blocked location exits 5 with JSON reason `location_not_writable` and never creates the destination → writable install and `verify --sandbox` unaffected |
| Wizard writability UX on real Windows (Review marker, Use per-user folder, Install gating, refusal against a real `C:\Program Files` destination) | **Unverified** — compile-verified for the Windows target in the local gate; needs SEAN's re-run of the ai.md field case |
| Install options (4.4.0): overlay v5 write/read round-trip incl. extension fields, hand-built v4 package reads with extension defaults, manifest offer-spec + dataDirs validation (roots/`..`/`;` refused), receipt round-trip + legacy receipt defaults, data-dir token resolution, purge guards (root + install-dir refusal against a hand-edited receipt), artifact plan login flag | Verified locally on the host build (unit tests across `overlay` / `manifest` / `receipt` / `sysinstall` / `uninstall`) |
| Install options lifecycle (4.4.0): pack fixture with `dataDirs` → install records resolved dirs in the receipt → uninstall without purge keeps a planted data dir → reinstall → `--purge-data` deletes it and reports it; same cycle through the real CLI binary plus `verify --sandbox` | Verified locally on the host build (unit lifecycle test + CLI smoke script) |
| Launch after install, Run-entry apply/remove/restore, and the uninstall confirmation window on real Windows | **Unverified** — compile-verified for the Windows target in the local gate; the wizard Finish checkbox, the HKCU Run value against the real registry, and the confirm window's render/flow need SEAN's on-machine run |
| Installer stub size | Measured: 635,904 bytes for a Windows PE release build (gnu-linked) and 607,384 bytes for the host release build — far under the blueprint's <15 MB target; CI enforces a 4 MiB budget on the x86_64 MSVC leg (first hosted run Unverified) |
| Shipped Windows binary imports | Verified for the gnu-linked PE: system DLLs only (kernel32, advapi32, cabinet, comctl32, comdlg32, gdi32, ole32, shell32, user32, msvcrt, ntdll) — nothing unshipped |
| Wizard pages 1–4 render / behave on real Windows | **Unverified** — needs SEAN's on-machine run |
| Install on real Windows (shortcuts, associations, PATH, ARP, receipt) | **Unverified** |
| Upgrade / repair / uninstall artifact removal on real Windows | **Unverified** |
| Windows Compression API codec path on real Windows | **Unverified** (the LZ fallback path is host-verified; the API path is compile-verified for the Windows target) |
| `--uninstall` round-trip on real Windows | **Unverified** |
| Layout rebuild models (4.5.0): studio `tree_entries` listing shape (directories once, files nested by depth) and `data_dirs_text` / `parse_data_dirs` round-trip; wizard `review_rows` without the disk row (disk moved to the rail) | Verified locally on the host build (unit tests in `src/studio.rs` / `src/wizard.rs`) |
| Power bundle engine (4.6.0): overlay v6 power block round-trip + per-component packed sizes, component selection rules (explicit/without/default/upgrade-previous/required + primary forced in), manifest validation for all four blocks (id/claim/hive/name rules, dword canonicalization), receipt round-trip incl. hook + writes + pin records and legacy defaults, hook run outcomes (abort stops phase, continue proceeds, missing command is a failure), plan fields (components/hooks/writes/rich shortcuts), wizard `review_rows` parts + writes summaries | Verified locally on the host build (unit tests in `src/overlay.rs` / `src/manifest.rs` / `src/receipt.rs` / `src/hooks.rs` / `src/install.rs` / `src/sysinstall.rs` / `src/wizard.rs`) |
| Power bundle lifecycle (4.6.0): pack a fixture with two components + a post hook + registry/env writes → verify (v6) → sandbox proof installs all components explicitly → install with one component deselected (its files absent, hook marker present, receipt records the selection + hook) → uninstall removes exactly the installed components' files | Verified locally on the host build (CLI smoke; see the Power bundle engine result in `PLAN.md`) |
| Power bundle surface (4.7.0): modify plan/operation (add component → files land + receipt rewritten `mode: "modify"`; remove component → files deleted; option flip recorded; no-install refusal), answers parser (full preset, unknown-field and bad-type errors naming the field, relative-location refusal) + template round-trip through the parser, receipt `purgeDataDefault` round-trip + legacy default, inspect report shape, diff (identical clean; changed payload + version named; unreadable refused), init (manifest validates with the real parser, files-subfolder detection, `--force` refusal), studio power-blocks text round-trip incl. unknown-block refusal | Verified locally on the host build (unit tests in `src/modify.rs` / `src/answers.rs` / `src/receipt.rs` / `src/inspect.rs` / `src/init.rs` / `src/studio.rs`) |
| Power bundle surface lifecycle (4.7.0): init a scratch project → pack a power fixture → inspect → diff against a modified repack (exit 6, named differences) → install → headless modify (component added, option flipped; files + receipt asserted) → answers-file install into a second folder (preset location + component selection honored) | Verified locally on the host build (CLI smoke; see the Power bundle surface result in `PLAN.md`) |
| Modify wizard rendering + receipt-seeded pages, the ARP Modify button, the Review page's answers offer, the uninstall confirmation's preset-starting checkbox, and the studio Power blocks editor on real Windows (4.7.0) | **Unverified** — compile-verified for the Windows target in the local gate; needs SEAN's on-machine run |
| Component install / hooks / registry + environment write + restore / rich-shortcut icon + arguments / taskbar pin outcome on real Windows | **Unverified** — compile-verified for the Windows target in the local gate; the registry and environment values against the real registry, hook execution, shortcut rendering, and pin behavior need SEAN's on-machine run |
| Builder studio window on real Windows (4.2.0): layout/DPI, table, panel binding, worker pack/verify, tooltips, persistence | **Unverified** — compile-verified for the Windows target in the local gate; needs SEAN's on-machine run |
| Rail wizard + IDE studio layouts on real Windows (4.5.0): rail rendering/step states/disk block, wizard pages in the new geometry; studio toolbar, listing pane + selection sync, totals line, manifest pane incl. the 4.4.0 fields, DPI behavior | **Unverified** — compile-verified for the Windows target in the local gate; needs SEAN's on-machine run (screenshots for the README are owed from the same pass) |
| Idle RAM / launch time on Windows | **Unverified** — not yet measured |
| Release assets (Setup.exe + Portable.zip + SHA256SUMS.txt) | **Unverified** — no 4.1.0 release published yet |

## Design conformance

- TesselGUI rev2 palette constants applied to the wizard and the studio
  window background/text (native Win32 controls take colors only as far as
  the platform allows; the studio's primary Build Setup button is
  owner-drawn to carry the exact app-green).
- Wizard layout follows the approved Design E mockup (Rail): a fixed
  left rail in the darkest panel carries the app monogram tile, name /
  version / publisher, the four steps (current accented, completed
  checked), and the disk figures; the content area keeps the Review
  ledger (Item / Value / Change), Options, Progress, and Finish pages
  and the footer with keyboard hint + Cancel / Install. (Supersedes
  Design C in 4.5.0; behavior unchanged.)
- Builder layout follows the approved Design H mockup (IDE): toolbar
  strip, left files-directory listing pane, center files table with a
  totals/selection line, right manifest pane, OUTPUT strip, footer
  with status + Test install + green Build Setup. (Supersedes Design D
  in 4.5.0.) Two deliberate deviations, carried over from Design D: the
  pane omits the mockup's License row (the manifest schema has no
  license field), and it presents every field the manifest supports —
  since 4.5.0 including `launchAfterInstall`, `startAtLogin`, and
  `dataDirs`. The mockup's selected-file inspector block is folded
  into the totals line under the table rather than a separate block,
  so the pane fits every field without scrolling.
- Scores are deliberately not invented: no numeric QUALITY score is claimed
  until the Windows field pass exists.
