# SeanPak roadmap

This document separates capabilities already shipped from candidate improvements. The candidate list has no promised release dates; each item needs implementation scope and Windows validation before it is considered complete.

## Already supported

- A native Builder GUI and a full CLI/JSON workflow over the same packaging and verification operations.
- Headless and silent install modes, dry-run plans, reusable GitHub Actions packaging, and deterministic release assets.
- Native self-extracting Setup.exe packages, portable ZIP output, plus optional Inno Setup and NSIS backends.
- Per-user installation by default, optional user-invoked elevation, components, install hooks, and journaled registry/environment changes.
- Receipts, rollback, repair/upgrade, and an uninstall flow that avoids deleting app data unless requested.

## Candidate roadmap

### Trust and deployment formats

- [ ] Add an Authenticode signing path for release installers and generated artifacts. Define certificate handling, timestamping, signature verification, and a secrets-safe CI setup.
- [ ] Assess MSIX generation, including the compatibility limits and support model for Package Support Framework (PSF) fixups; document the result before committing to an implementation.
- [ ] Assess MSI generation and its servicing, upgrade, and uninstall behavior against SeanPak's existing package model.

### Suites and install logic

- [ ] Design a suite/bootstrapper that can coordinate multiple packages and prerequisites, with explicit offline/online behavior, progress reporting, failure recovery, and unattended operation.
- [ ] Add declarative conditions for supported Windows versions, architecture, and detected prerequisites or existing software. Make conditions visible in validation, dry-run, CLI/JSON output, and the Builder rather than hiding them in scripts.

### Accessible, localized experience

- [ ] Audit the Builder and installer for keyboard operation, focus visibility, accessible names, contrast, and Windows High Contrast Mode; fix gaps and add regression checks.
- [ ] Add a localization pipeline for user-facing strings and choose the initial supported locales based on maintained translations.

### Developer workflow

- [ ] Evaluate a Visual Studio/MSBuild integration that invokes the same manifest and CLI pipeline, without introducing a second packaging implementation.

## Product decisions required

- **Per-machine installs:** the current design is per-user and does not require administrator rights. Decide whether an explicitly opt-in per-machine mode is wanted before adding it; do not change current defaults or the elevation rules implicitly.
- **Package-specific wizard branding:** the app currently uses one shared TesselGUI design standard. Define which branding extensions, if any, fit that standard before changing installer screens.

## Promotion criteria

Move a candidate into active work only after its user need, compatibility limits, failure/recovery behavior, and Windows 11 x64 validation plan are defined. Keep the CLI/JSON contract and local-first/offline behavior explicit when a feature affects them.
