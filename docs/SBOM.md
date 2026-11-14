# SPDX SBOM output

SeanPak can write an SPDX 2.3 JSON sidecar for a built Setup executable:

```powershell
seanpak.exe sbom .\dist\Example-Setup.exe
# writes .\dist\Example-Setup.spdx.json
seanpak.exe sbom .\dist\Example-Setup.exe --out .\dist\sbom.json --json
```

The default file is next to Setup.exe and uses its name with the `.spdx.json` extension. `--out` chooses another path. Existing files are never overwritten; the SBOM is written to a temporary file and then placed atomically. The JSON success response includes the output path, SPDX version, app identity, file count, and Setup SHA-256.

## What it contains

- The application name and version from the embedded package manifest.
- Every payload path and SHA-256 computed from its decoded bytes.
- The SHA-256 checksum of the exact Setup.exe, including any Authenticode signature already applied.
- SPDX package and containment relationships linking the installer, application, and payload files.

The command verifies the package before writing. For format v7, this includes checking stored payload digests. Legacy packages can still be inventoried, but their format has no stored payload digests to compare against; the SBOM records hashes computed from the decoded bytes. Generate the sidecar after signing if it should identify the signed Setup.exe that will be distributed.

The inventory describes all files embedded in the installer, not a third-party dependency graph. If install-time optional components are configured, the installer may select a subset. SeanPak does not infer licenses or copyright notices: unknown values are recorded as `NOASSERTION`.

## Release output and reproducibility

SBOM generation is a separate command and does not change `pack` output. The default release remains exactly Setup.exe, Portable.zip, and SHA256SUMS.txt. The SPDX sidecar is not added to SHA256SUMS.txt.

The SPDX `created` timestamp uses the current UTC time unless `SOURCE_DATE_EPOCH` is set to a nonnegative Unix timestamp. Set the same value when regenerating an SBOM to keep its creation time and document namespace stable for identical package contents.

The JSON follows the [official SPDX 2.3 JSON schema](https://github.com/spdx/spdx-spec/blob/develop/schemas/spdx-schema-2-3.json).
