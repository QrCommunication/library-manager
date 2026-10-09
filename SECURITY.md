# Security policy

Library Manager manages local books and can contact user-selected AI providers. Security reports must not contain API keys, private books, database copies, or personal reading notes.

Report a vulnerability privately through [GitHub private vulnerability reporting](https://github.com/QrCommunication/library-manager/security/advisories/new). Describe the affected version, a minimal synthetic fixture, reproduction steps, and the observed impact. If private reporting is unavailable, open an issue asking for a private reporting channel without disclosing exploit details.

The latest published release is supported. Fixes are released through the public repository and release packages. A successful test or dependency scan is evidence for its tested scope, not a guarantee that every reader, ebook, or provider has been exercised.

## Application boundaries

- Original imports are immutable. Rewrites and conversions use managed variants, staging files, exclusive publication, and content hashes.
- An EPUB, its metadata, and Internet search results are untrusted data. They never authorize shell commands, arbitrary file writes, or deletion.
- The reader sanitizes book HTML and blocks scripts and remote resources.
- Internet tools and authenticated AI requests use separate clients. Public web requests never carry provider credentials. User-paired LAN devices use a separate transport.
- API keys belong in the system secret service or session memory. They do not belong in SQLite, exports, issue reports, or logs.
- Device detection and indexing are read-only. Sending books is an explicit action. Existing device files are not silently overwritten.

## Development checks

Run the checks documented in CONTRIBUTING.md and include the affected path in regression coverage. Keep Rust and JavaScript lockfiles committed. Review advisory warnings as well as reported vulnerabilities; a zero-vulnerability count does not mean a zero-warning dependency graph.

## French

Signalez les failles en privé par le lien ci-dessus, avec une version, un exemple synthétique et les étapes de reproduction. Ne joignez ni livre privé, ni clé API, ni copie de votre bibliothèque. La dernière version publiée reçoit les correctifs.
