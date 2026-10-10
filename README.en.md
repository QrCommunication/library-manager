# Library Manager

<img src="public/library-manager.png" width="96" height="96" alt="Library Manager" />

**Your library, wherever you read.** A standalone application for organizing ebooks, reading EPUBs, and preparing files for ereaders. Version **0.2.1** includes native packaging for Linux, Windows, and macOS.

[Français](README.md) · [Downloads](https://github.com/QrCommunication/library-manager/releases) · [User guide (French)](docs/USER_GUIDE.md) · [Architecture](docs/BLUEPRINT.md) · [Issues](https://github.com/QrCommunication/library-manager/issues)

Developed by QR Communication under **GPL-3.0**. The Rust engine, SQLite database, and MOBI converter are bundled. **Calibre, Node, Rust, and Python are not required to run release packages.**

## Library

- Cover grid and table views; grouping by author, series, or genre.
- Search across titles, authors, series, genres, descriptions, ISBNs, publishers, and personal notes with accent handling.
- Combined filters for authors, series, genres, tags, language, format, reading status, favorites, metadata review, missing covers, size, and device presence.
- Sort by title, author, series and number, added/updated date, size, progress, publication, or rating.
- Personal notes, favorites, ratings, reading progress, and file variants.
- Immutable originals and normalized **Author → Series → numbered title** paths. Volume zero and decimal series positions are supported.

The library and device book views share a selection bar with **Assistant**, **Verify metadata**, **Send to ereader**, and **Remove from library**. A selection can contain up to 200 books. Unavailable actions explain their requirements, such as a ready provider for analysis or a writable device for transfer.

**Remove from library** removes books from the local catalog after confirmation. It preserves original files, variants, and copies on devices. Removal is recorded in history and can be undone; restoration checks the retained files and any conflicts with the current catalog.

## Assistant and metadata

Imports automatically queue metadata work when automatic enrichment is enabled. Books remain usable when an AI provider has not been configured; enrichment waits for configuration.

Choose **Z.ai, Kimi, MiniMax, Codex through the OpenAI API, Claude, or Mistral**. Model catalogs are retrieved on demand from provider APIs or official catalogs, with source and retrieval date. Z.ai uses its public official catalog where a model-list API is unavailable.

Library Manager provides common Internet research tools for every provider. Bibliographic sources accompany metadata proposals; uncertain corrections remain available for review. Models cannot execute shell commands or delete files. Personal notes and reading progress are protected from metadata enrichment.

In the **Library**, check the books you want to work on. Choose **Verify metadata** to queue their analyses, or **Assistant** to open a conversation about that selection. Bulk analysis requires a selected, configured, ready provider and a model. Books with an active analysis are not queued again.

The assistant can inspect files and request analyses. **Allow the assistant to edit and organize selected books** applies only to the request being sent and the selected books; the option resets after that request is accepted. Without permission, the assistant remains read-only. Originals are preserved.

Choose **Review proposals**, or **Review** beside a book with an available proposal, to open its review window. Compare current values, proposed changes, and their sources, then apply the proposal. A metadata review badge may also indicate incomplete metadata without an available proposal.

Proposal validation is stored durably with the book update and its history: an applied proposal does not return after a refresh or restart. Editing personal information does not implicitly validate a proposal. Fields already matching the catalog are not shown as changes; an obsolete proposal cannot silently overwrite a more recent edit. Reversible operations can be undone from history.

Inspection reads the actual stored file and verifies its integrity. EPUB excerpts prioritize title, copyright, and edition pages, followed by a chapter. This reading is bounded and does not cover the entire book.

Providers require API credentials and may charge for use. A chat-site subscription does not automatically provide an API key. Keys can be kept in session memory or the Linux secret service. The application never silently falls back to plaintext storage.

See [provider documentation](docs/PROVIDERS.md) and [metadata policy](docs/METADATA_POLICY.md).

## Devices and optimization

Mounted USB/SD volumes are detected and indexed. Books already present on a connected device are highlighted and filterable. Disconnecting removes current-device presence immediately. MTP devices are available when the Linux desktop has already mounted them through GVfs.

USB inventory progress follows the amount of data actually read. Detected books appear as the inventory advances. Books stored only on the reader are marked in the library: import one book or all books missing from the local catalog, while preserving the files on the card. Results distinguish successful imports, duplicates, and rejected files.

The **CrossPoint** connector talks directly to the firmware's HTTP transfer service on your LAN. Enable file-transfer mode on the reader and enter its address. No Calibre installation is needed. Transfers verify their contents and do not silently overwrite existing files.

To send several books, select them and choose **Send to ereader**. The **Prepare transfer** dialog lets you choose a connected writable device. If needed, enable **Optimize before transfer** and select an optimization profile, then confirm the transfer. The job appears in Activity; files in the local library are preserved.

An **integrated Calibre wireless server** also accepts compatible clients such as KOReader's Calibre plugin. Start it explicitly in **Ereaders**, using the computer's LAN address and port 9090. This version uses an address configured manually; UDP discovery and physical trials of this wireless protocol are not yet validated. The protocol is implemented inside Library Manager, with no Calibre installation.

| EPUB profile | Purpose |
| --- | --- |
| Lossless | Stronger ZIP compression with content preserved. |
| Balanced | Conservative reduction of large images. |
| Xteink | Smaller grayscale images and safe removal of embedded fonts. |
| Text only | Safe removal of images/fonts while retaining text, captions, and descriptions. |

Optimization creates a variant and reports size before/after, image/font changes, warnings, and text/chapter integrity. Optimize separately or before sending to a device. See [device protocols and limits](docs/DEVICE_PROTOCOL.md).

## Formats and reading

| Format | Catalog | Built-in reading | Conversion |
| --- | --- | --- | --- |
| EPUB | Yes | Yes | EPUB, TXT, HTML, FB2, MOBI6 |
| TXT, HTML, FB2 | Yes | Through an EPUB variant | EPUB, TXT, HTML, FB2, MOBI6 |
| Unencrypted MOBI, AZW3/KF8 | Yes | Through an EPUB variant | EPUB, TXT, HTML, FB2, MOBI6 |
| PDF, CBZ | Yes | No built-in viewer for these formats | Original-file storage |

Reflow conversion may flatten complex styling. Warnings explain format limits; identical reproduction of fixed-layout, comic, or interactive content is not promised. DRM is not removed.

The EPUB reader includes a table of contents and saved position. Book HTML is sanitized and isolated, with scripts and remote resources blocked.

## Installation

Use the [v0.2.1 release page](https://github.com/QrCommunication/library-manager/releases/tag/v0.2.1) for packages and their verification reports. Choose the package matching your operating system and processor.

| Platform | Package names |
| --- | --- |
| Linux x86_64 (64-bit) | `library-manager_0.2.1_amd64.deb`, `library-manager-0.2.1-1.x86_64.rpm`, `library-manager_0.2.1_amd64.AppImage` |
| Windows 11 x64 | `library-manager_0.2.1_windows_x64_unsigned.msi`, `library-manager_0.2.1_windows_x64_unsigned_setup.exe` |
| macOS 13 or newer, Apple Silicon | `library-manager_0.2.1_macos_arm64_signed.dmg`, `library-manager_0.2.1_macos_arm64_signed.app.zip` |
| macOS 13 or newer, Intel | `library-manager_0.2.1_macos_x64_signed.dmg`, `library-manager_0.2.1_macos_x64_signed.app.zip` |

The release workflow requires successful checks for the exact source commit before publishing packages. macOS publication additionally requires verified application and DMG signatures, **Accepted** notarization, validated stapled tickets, and successful Gatekeeper assessments. The Windows MSI and EXE installers have **no Authenticode signature**.

Check `BUILD_MANIFEST.json` and `SHA256SUMS` on the release page for source and package provenance. The attached native test reports identify the Linux package and scenarios exercised; the macOS notarization reports record signature, notarization, stapling, and Gatekeeper checks for each architecture. These reports distinguish package checks from physical Windows 11, Mac, or ereader trials.

### Windows installation

Download the Windows x64 EXE or MSI from the release page and run it. The EXE uses NSIS and installs for the current user by default; the MSI uses WiX and installs for all users, requiring administrator permission. Launch **Library Manager** from the Start menu after installation.

The installer checks for Microsoft Edge WebView2. If it is missing, the default installation mode downloads and runs its bootstrapper silently, requiring Internet access. The Windows installer is therefore not a fully offline installer. See the [Tauri installer defaults](https://v2.tauri.app/reference/config/#nsisinstallermode) and [WebView2 installation mode](https://v2.tauri.app/reference/config/#webviewinstallmode).

### macOS installation

macOS **13.0 or later** is required. Choose the **ARM64** package for Apple Silicon or the **x86_64** package for an Intel Mac. Open the matching DMG, drag **Library Manager** into **Applications**, then launch it from that folder. The application ZIP is an alternative: extract it and move **Library Manager.app** into **Applications**. ARM64 and Intel packages are separate; they are not a universal application.

### Linux installation

Linux packages use **Ubuntu 22.04 with glibc 2.35** as their build baseline. They require glibc 2.35 or newer and compatible GTK/WebKit system libraries. Run the following commands from the directory containing the downloaded **0.2.1** package.

On Debian, Ubuntu, and derivatives, install the downloaded DEB:

```sh
sudo apt install ./library-manager_0.2.1_amd64.deb
```

On Fedora and compatible RPM distributions:

```sh
sudo dnf install ./library-manager-0.2.1-1.x86_64.rpm
```

The package manager installs required GTK/WebKit system libraries and `ca-certificates` for HTTPS connections. Launch **Library Manager** from the applications menu.

For the AppImage, make the downloaded file executable and start it:

```sh
chmod +x ./library-manager_0.2.1_amd64.AppImage
./library-manager_0.2.1_amd64.AppImage
```

The AppImage uses the host operating system's libraries, services, and trusted certificate store. If FUSE is unavailable, start it in extraction mode:

```sh
APPIMAGE_EXTRACT_AND_RUN=1 ./library-manager_0.2.1_amd64.AppImage
```

It needs no Calibre installation or Node, Rust, or Python runtime. Release assets include SHA-256 checksums.

### Upgrading and checking the version

Close Library Manager before upgrading. Closing its only window quits this version; there is no tray mode. Install the new package for the same architecture and, on Windows, use the same installer type as your previous installation. On macOS, replace the application in **Applications** with the matching new version. On Linux, install the new DEB/RPM with the package manager or replace the AppImage. Keep your library profile when replacing the application.

Reopen Library Manager and check **Settings → About** and the version shown in the sidebar. Both should show **0.2.1**. If an older version opens, check which installed copy or AppImage your shortcut launches.

See [the validation report](docs/QUALITY.md) for the environments actually tested and the remaining limits.

## Localization

French and English are included. The default follows your system language; settings can override it. Add a JSON dictionary under `src/lib/locales/` to make another language available automatically. See [LOCALIZATION.md](docs/LOCALIZATION.md).

## Development

Stack: **Tauri 2, Rust, Svelte 5, TypeScript, and bundled SQLite**. Build-tool versions are pinned in manifests and lockfiles. The core engine is independent of the WebView.

```sh
pnpm install --frozen-lockfile --force --ignore-scripts
pnpm tauri dev
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for native dependencies and checks. The browser `?demo=1` preview uses fictional books and cannot access real files, devices, or providers. Native builds use the real services.

## Data and security

Library data and conversations are stored locally. AI requests send relevant metadata and excerpts to the configured provider; AI enrichment is not an offline feature. Automatic enrichment and Internet research can be disabled in settings.

Never attach keys or private books to an issue. See [SECURITY.md](SECURITY.md) and [THIRD_PARTY.md](docs/THIRD_PARTY.md) for bundled-engine licensing.

Automated tests, package verification, and physical-device tests are separate evidence. The [validation report](docs/QUALITY.md) records their actual scope, known dependency alerts, and device-testing limits.
