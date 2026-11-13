# SeanPak roadmap

This document separates capabilities already shipped from candidate improvements and product decisions. Candidate items have no promised release dates; each needs a defined scope and Windows 11 x64 validation before implementation is considered complete.

## Already supported

- A native Builder GUI and a full CLI/JSON workflow over the same packaging and verification operations. The shared manifest and CLI are the current automation interface; there is no separate embedded scripting language.
- Headless and silent installs, dry-run plans, a reusable GitHub Actions workflow, and deterministic release assets provide the current CI/CD path.
- Native self-extracting Setup.exe packages, portable ZIP output, and optional Inno Setup and NSIS backends.
- Per-user installation by default, optional user-invoked elevation, components, install hooks, and journaled registry/environment changes.
- Receipts, rollback, repair/upgrade, and an uninstall flow that avoids deleting app data unless requested.

## Candidate roadmap

### Trust and deployment formats

- [ ] Add an Authenticode path for generated PE installers and executable payloads. Define certificate handling, timestamping, verification, and secrets-safe CI. Signing can identify a publisher but does not guarantee that SmartScreen will show no warnings; validate the signed artifacts and state that limitation. Continue checksumming the portable ZIP.
- [ ] Assess MSIX generation, including compatibility limits and the support model for Package Support Framework (PSF) fixups; document the result before committing to implementation.
- [ ] Assess MSI generation and its servicing, upgrade, and uninstall behavior against SeanPak's existing package model.

### Install logic, suites, and prerequisites

- [ ] Design a suite/bootstrapper that coordinates multiple packages and prerequisites, with explicit offline/online behavior, progress reporting, failure recovery, and unattended operation.
- [ ] Add declarative conditions for supported Windows versions, architecture, detected prerequisites, and existing software. Surface results in validation, dry-run, CLI/JSON, and the Builder rather than hiding them in scripts.
- [ ] Assess prerequisite installation as well as detection. Define bundled/offline versus downloaded/online behavior, integrity verification, user consent, reboot handling, recovery, and unattended policy.
- [ ] Decide whether existing install hooks and the shared CLI/manifest model leave a real gap for custom actions or scripting. Before adding arbitrary code execution, define its trust boundary, privilege and consent rules, logging/redaction, rollback behavior, and unattended policy.

### Accessible, localized experience

- [ ] Audit the Builder and installer for keyboard operation, focus visibility, accessible names, contrast, and Windows High Contrast Mode; fix gaps and add regression checks.
- [ ] Add a localization pipeline for user-facing strings and choose initial locales based on maintained translations. Do not commit to a language count without translation ownership and update coverage.

### Developer workflow

- [ ] Evaluate Visual Studio/MSBuild integration that invokes the existing manifest and CLI pipeline without introducing a second packaging implementation. The CLI and GitHub Actions already cover baseline automation; validate any Docker or other runner support against the Windows target before promising it.

## Product decisions required

- **Per-machine installs:** the current design is per-user and does not require administrator rights. Decide whether an explicitly opt-in per-machine mode is wanted before adding it; do not change current defaults or elevation rules implicitly.
- **Package-specific wizard branding and UI customization:** the app uses one shared TesselGUI design standard. Decide whether limited brand inputs or alternate UI hosts are needed, and define accessibility and security limits before changing installer screens or allowing package-provided UI code.

## Promotion criteria

Move a candidate into active work only after its user need, compatibility limits, failure/recovery behavior, and Windows 11 x64 validation plan are defined. Keep the CLI/JSON contract and local-first/offline behavior explicit when a feature affects them.
