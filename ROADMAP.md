# SeanPak roadmap

This document separates shipped capabilities from candidate improvements and product decisions. Candidate items are considerations, not release promises; each needs a defined scope and Windows 11 x64 validation before implementation is considered complete.

## Already supported

- A native Builder GUI and a full CLI/JSON workflow over the same packaging and verification operations. JSON project manifests are text-based and Git-friendly; there is no separate embedded scripting language.
- Headless and silent installs, dry-run plans, a reusable GitHub Actions workflow, and deterministic release assets provide the current CI/CD path.
- Native self-extracting Setup.exe packages, portable ZIP output, and optional Inno Setup and NSIS backends. Setup and Portable are separate artifacts today.
- Per-user installation by default, optional user-invoked elevation, components, install hooks, and journaled registry/environment changes.
- Receipts, rollback, repair/upgrade, and an uninstall flow that avoids deleting app data unless requested.
- Windows CI runs a golden package through sandbox install and uninstall verification, including a no-residue check for that fixture.

## Candidate roadmap

### Trust, inventory, and deployment formats

- [ ] Add an Authenticode path for generated PE installers and executable payloads. Define certificate handling, timestamping, verification, and secrets-safe CI. Signing can identify a publisher but does not guarantee that SmartScreen will show no warnings; validate signed artifacts and state that limitation. Continue checksumming the portable ZIP.
- [ ] Generate an SBOM for the exact files and versions an installer ships, with SPDX and/or CycloneDX output tied to the build artifact.
- [ ] Assess signed build provenance and attestations (for example, SLSA-style provenance), including how artifact hashes are verified. Keep local/offline verification available; any transparency-log lookup at install time must be optional and explicit about network use.
- [ ] Prove byte-for-byte reproducible builds from the same inputs rather than assuming deterministic release assets imply reproducibility.
- [ ] Assess centralized signing through an HSM or cloud KMS so developers do not handle exportable signing keys; define permissions, audit, recovery, and compatibility with hosted CI.
- [ ] Assess MSIX generation, including compatibility limits and the support model for Package Support Framework (PSF) fixups; document the result before committing to implementation.
- [ ] Assess MSI generation and its servicing, upgrade, and uninstall behavior against SeanPak's existing package model.
- [ ] Consider Windows package-manager outputs such as winget, Chocolatey, and Scoop from the same manifest. Homebrew is outside the current Windows-only target unless platform scope changes.
- [ ] Evaluate a single dual-mode artifact that can run portably or install; compare it with the current separate Setup.exe and Portable ZIP outputs.

### Authoring, conditions, and prerequisites

- [ ] Add declarative conditions for supported Windows versions, architecture, detected prerequisites, and existing software. Surface results in validation, dry-run, CLI/JSON, and the Builder rather than hiding them in scripts.
- [ ] Assess prerequisite installation as well as detection. Define bundled/offline versus downloaded/online behavior, integrity verification, user consent, reboot handling, recovery, and unattended policy.
- [ ] Decide whether existing install hooks and the shared CLI/manifest model leave a real gap for custom actions or scripting. Before adding arbitrary code execution, define its trust boundary, privilege and consent rules, logging/redaction, rollback behavior, and unattended policy.
- [ ] Keep JSON as the reviewable project source of truth. Consider typed SDKs or code-first authoring only if they generate the same manifest and invoke the same CLI pipeline.
- [ ] Evaluate policy-as-code for allowed install paths, required signatures, and permitted registry/environment changes, with validation results visible before install.
- [ ] Consider a visual live preview/hot reload for installer flows and whether visual, declarative, and code authoring can stay synchronized without creating competing project formats.

### Suites, updates, and recovery

- [ ] Design a suite/bootstrapper that coordinates multiple packages and prerequisites, with explicit offline/online behavior, progress reporting, failure recovery, and unattended operation.
- [ ] Evaluate differential updates and streaming delivery. Define integrity, rollback, network-consent, and offline behavior; do not execute unverified streamed payloads.
- [ ] Evaluate self-healing behavior, starting with an explicit user-driven repair flow. A resident agent or background repair service needs a separate privacy, privilege, and lifecycle decision.
- [ ] Expand Windows CI from the current golden install/uninstall fixture to disposable Windows 11 x64 lifecycle tests for fresh install, upgrade, repair, reinstall, and clean uninstall; add downgrade cases only where the product supports them.
- [ ] Track tested version-to-version upgrade paths and detect broken migrations before release.
- [ ] Consider time-travel install diagnostics that record and replay filesystem/registry actions, with sensitive values redacted and opt-in collection.

### Accessible, localized experience

- [ ] Audit the Builder and installer for keyboard operation, focus visibility, accessible names, screen-reader behavior, contrast, and Windows High Contrast Mode; fix gaps and add regression checks. Do not claim WCAG compliance without testing against a defined standard.
- [ ] Add a localization pipeline for user-facing strings and choose initial locales based on maintained translations. Do not commit to a language count without translation ownership and update coverage.
- [ ] Consider limited brand-kit import (logo, palette, and fonts) and white-label variants only after defining accessible color constraints and how they fit the shared TesselGUI design.
- [ ] Evaluate web-tech or alternate UI hosts only if they preserve native accessibility, offline behavior, signing, and the shared install pipeline.

### Enterprise operations and ecosystem

- [ ] Evaluate fleet-manifest output for Windows deployment systems such as Intune and SCCM, plus Ansible/Terraform where their Windows support fits the target.
- [ ] Consider local install-audit and compliance reports, with a clear data-retention and export model.
- [ ] Consider install-progress telemetry and post-install launch observability only with explicit opt-in consent; remain offline and silent by default.
- [ ] Assess a reusable-module catalog only with package provenance, permissions, review, and revocation controls; do not treat third-party install actions as trusted by default.
- [ ] Evaluate container/environment detection as a compatibility diagnostic. Detection does not imply support for Windows containers, Wine, or CI sandboxes beyond the validated Windows 11 x64 target.

### Developer workflow

- [ ] Evaluate Visual Studio/MSBuild integration that invokes the existing manifest and CLI pipeline without introducing a second packaging implementation. The CLI and GitHub Actions already cover baseline automation; validate any Docker or other runner support against the Windows target before promising it.

## Product decisions required

- **Per-machine installs:** the current design is per-user and does not require administrator rights. Decide whether an explicitly opt-in per-machine mode is wanted before adding it; do not change current defaults or elevation rules implicitly.
- **Package-specific wizard branding and UI customization:** the app uses one shared TesselGUI design standard. Decide whether limited brand inputs, alternate UI hosts, or multi-tenant variants are needed, and define accessibility and security limits before changing installer screens or allowing package-provided UI code.
- **Install observability and resident repair:** any network reporting or background agent needs an explicit privacy, consent, privilege, and support decision; neither is part of the default offline installer model.

## Promotion criteria

Move a candidate into active work only after its user need, compatibility limits, failure/recovery behavior, and Windows 11 x64 validation plan are defined. Keep the CLI/JSON contract and local-first/offline behavior explicit when a feature affects them.
