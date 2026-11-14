# SeanPak package format

SeanPak appends a sector-aligned overlay to the Windows executable stub. The overlay contains a manifest header, one file record per payload, install options, the versioned PowerSpec block, optional integrity metadata, and then the payload data.

## Version 7 payload integrity

New packages include a 32-byte SHA-256 digest for each file's original, uncompressed bytes. Digests follow the PowerSpec block in record order; payload offsets begin after the digest table. The `verify` and `diff` commands check digests before trusting payload bytes. Install and Modify check every payload before repair decisions or install changes; extraction checks each selected file before writing it.

Readers continue to accept legacy package versions using their existing layouts. Legacy packages have no recorded payload digest, so this check cannot be applied to them.

The digest table detects accidental corruption and incomplete writes; it does not authenticate a package because an attacker who can modify the package can also replace its digests. Use Authenticode signing to establish publisher identity and package authenticity.
