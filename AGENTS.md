# SeanPak — agent notes

## Project Manifest

- **Project:** SeanPak — the single custom TesselGUI installer for all of SEAN's projects (formerly RustPak, then CrucibleKit).
- **Tier:** t2 (CLI/library-class tool; the installer stub it emits is t1b-class).
- **Stack:** Rust, `windows-sys` 0.59 only (no other runtime dependencies), static CRT (`+crt-static`), native Win32 UI — no web runtimes, no telemetry, fully offline.
- **Design standard:** TesselGUI 1.4.0, dark theme only (GitHub Dark High Contrast values; the tokens live in `src/theme.rs`, the single palette every window consumes); installer wizard follows the approved Design E (Rail) layout; the Builder studio's Design H (IDE) layout was replaced in 5.0.0 by the approved guided page flow (mockups approved by SEAN 2026-11-13), themed directly on VS Code GitHub Dark High Contrast per SEAN's ruling for the redesign — the same values the shared tokens carry. (Designs C/D superseded; Design H superseded by the v5 flow.)
- **Release shape:** exactly `SeanPak-v{VERSION}-x64-Setup.exe` + `SeanPak-v{VERSION}-x64-Portable.zip` + `SHA256SUMS.txt`.
- **Per-user mandate (with SEAN's 2026-11-13 exception):** installs are per-user by default and nothing ever *requires* administrator rights; the embedded manifest stays `asInvoker`. The single exception (v4.8.0, SEAN's hybrid pick): at a location the writability verdict finds unwritable, the surfaces offer **Restart as administrator** — only on the user's explicit click (wizard / uninstall confirmation) or the explicit CLI `--elevate` flag, never silently, never as the default, and never re-offered to a relaunched instance (the `--elevated-relaunch` marker is the loop guard).
- **Repository:** https://github.com/SeanShaffer88/SeanPak

---

## Development standard

This project follows SEAN's **Universal Windows Desktop Master Prompt** — the standing development standard for all of SEAN's Windows desktop projects, current and future. The full text is maintained by SEAN (ask him for the current version); this section is the in-repo summary, and the project's own instructions add project-specific requirements on top of it.

Core standards:

- Production quality only: no rough demos or placeholder features presented as finished; safe defaults for beginners, explained advanced controls for professionals.
- Accessibility is a requirement: keyboard navigation, accessible names, visible focus, sufficient contrast, no meaning by color alone, specific error messages with a fix path.
- Every consequential action explains its risks, limitations, and recovery options; destructive or irreversible actions are confirmed, previewed where feasible, and backed up or undoable where feasible.
- Data safety: local-first storage, offline-capable core features, versioned data formats, atomic writes, tested migrations, and no silent data loss.
- Verification honesty: compatibility and readiness claims are labeled Verified, Partially verified, or Unverified; never claim a check that did not run.

Standing defaults (a project brief may override a specific default):

- Product-family look: the black/red aesthetic matching InstallScope is preferred, not mandatory.
- Rust is the default for new projects and for new components where it fits; existing stacks are preserved — this is not a rewrite mandate.
- Support as many Windows versions as makes sense for the project; consider Windows 10 support when feasible without materially extending development time. Never claim support that has not been verified.
- Status and task reports include a Progress % and an ETA whenever they can be estimated responsibly.
- Public documentation avoids competitor comparisons by default (internal research may name competitors; license attributions for bundled components are kept).
- License: MIT for SEAN-owned projects; forks retain their upstream license.
