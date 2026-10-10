# Library Manager

<img src="public/library-manager.png" width="96" height="96" alt="Library Manager" />

**Your library, wherever you read.** A standalone Linux desktop application for organizing ebooks, reading EPUBs, and preparing files for ereaders.

[Français](README.md) · [Downloads](https://github.com/QrCommunication/library-manager/releases/tag/v0.2.0) · [Architecture](docs/BLUEPRINT.md) · [Issues](https://github.com/QrCommunication/library-manager/issues)

Developed by QR Communication under **GPL-3.0**. The Rust engine, SQLite database, and MOBI converter are bundled. **Calibre, Node, Rust, and Python are not required to run release packages.**

## Library

- Cover grid and table views; grouping by author, series, or genre.
- Search across titles, authors, series, genres, descriptions, ISBNs, publishers, and personal notes with accent handling.
- Combined filters for authors, series, genres, tags, language, format, reading status, favorites, metadata review, missing covers, size, and device presence.
- Sort by title, author, series and number, added/updated date, size, progress, publication, or rating.
- Personal notes, favorites, ratings, reading progress, and file variants.
- Immutable originals and normalized **Author → Series → numbered title** paths. Volume zero and decimal series positions are supported.

## Assistant and metadata

Imports automatically queue metadata work when automatic enrichment is enabled. Books remain usable when an AI provider has not been configured; enrichment waits for configuration.

Choose **Z.ai, Kimi, MiniMax, Codex through the OpenAI API, Claude, or Mistral**. Model catalogs are retrieved on demand from provider APIs or official catalogs, with source and retrieval date. Z.ai uses its public official catalog where a model-list API is unavailable.

Library Manager provides common Internet research tools for every provider. Bibliographic sources accompany metadata proposals; uncertain corrections remain available for review. Models cannot execute shell commands or delete files. Personal notes and reading progress are protected from metadata enrichment.

In the **Library**, check the books you want to work on. The selection bar offers **Verify metadata** to queue their analyses, or **Assistant** to work with that selection. The assistant can inspect files and request analyses; permission to edit and organize applies only to the request being sent. Without that permission, it remains read-only. You can select up to 200 books.

Choose **Review proposals**, or **Review** beside a book with an available proposal, to open its review window. Compare current values, proposed changes, and their sources, then apply the proposal. Fields already matching the catalog are not shown as changes. Changes are recorded in history, where reversible operations can be undone. Inspection reads the stored file and verifies its integrity. EPUB excerpts prioritize title, copyright, and edition pages, followed by a chapter. This reading is bounded and does not cover the entire book.

Providers require API credentials and may charge for use. A chat-site subscription does not automatically provide an API key. Keys can be kept in session memory or the Linux secret service. The application never silently falls back to plaintext storage.

See [provider documentation](docs/PROVIDERS.md) and [metadata policy](docs/METADATA_POLICY.md).

## Devices and optimization

Mounted USB/SD volumes are detected and indexed. Books already present on a connected device are highlighted and filterable. Disconnecting removes current-device presence immediately. MTP devices are available when the Linux desktop has already mounted them through GVfs.

USB inventory progress follows the amount of data actually read. Detected books appear as the inventory advances. Books stored only on the reader are marked in the library: import one book or all books missing from the local catalog, while preserving the files on the card. Results distinguish successful imports, duplicates, and rejected files.

The **CrossPoint** connector talks directly to the firmware's HTTP transfer service on your LAN. Enable file-transfer mode on the reader and enter its address. No Calibre installation is needed. Transfers verify their contents and do not silently overwrite existing files.

An **integrated Calibre wireless server** also accepts compatible clients such as KOReader's Calibre plugin. Start it explicitly in Readers, using the computer's LAN address and port 9090. This version uses an address configured manually; UDP discovery and physical trials of this wireless protocol are not yet validated. The protocol is implemented inside Library Manager, with no Calibre installation.

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

Download the **DEB, RPM, or AppImage** from the [v0.2.0 release](https://github.com/QrCommunication/library-manager/releases/tag/v0.2.0). These packages target **Linux x86_64 (AMD64)** and are built on **Ubuntu 22.04 with glibc 2.35**. They require glibc 2.35 or newer and compatible GTK/WebKit system libraries. See [the validation report](docs/QUALITY.md) for the environments actually tested and the remaining limits.

On Debian, Ubuntu, and derivatives, install the downloaded DEB:

```sh
sudo apt install ./library-manager_0.2.0_amd64.deb
```

On Fedora and compatible RPM distributions:

```sh
sudo dnf install ./library-manager-0.2.0-1.x86_64.rpm
```

The package manager installs required GTK/WebKit system libraries and `ca-certificates` for HTTPS connections. Launch **Library Manager** from the applications menu.

For the AppImage, make the downloaded file executable and start it:

```sh
chmod +x ./library-manager_0.2.0_amd64.AppImage
./library-manager_0.2.0_amd64.AppImage
```

The AppImage uses the host operating system's libraries, services, and trusted certificate store. If FUSE is unavailable, start it in extraction mode:

```sh
APPIMAGE_EXTRACT_AND_RUN=1 ./library-manager_0.2.0_amd64.AppImage
```

It needs no Calibre installation or Node, Rust, or Python runtime. Release assets include SHA-256 checksums.

## Localization

French and English are included. The default follows your system language; settings can override it. Add a JSON dictionary under `src/lib/locales/` to make another language available automatically. See [LOCALIZATION.md](docs/LOCALIZATION.md).

## Development

Stack: **Tauri 2, Rust, Svelte 5, TypeScript, and bundled SQLite**. Build-tool versions are pinned in manifests and lockfiles. The core engine is independent of the WebView.

```sh
pnpm install --frozen-lockfile
pnpm tauri dev
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for native dependencies and checks. The browser `?demo=1` preview uses fictional books and cannot access real files, devices, or providers. Native builds use the real services.

## Data and security

Library data and conversations are stored locally. AI requests send relevant metadata and excerpts to the configured provider; AI enrichment is not an offline feature. Automatic enrichment and Internet research can be disabled in settings.

Never attach keys or private books to an issue. See [SECURITY.md](SECURITY.md) and [THIRD_PARTY.md](docs/THIRD_PARTY.md) for bundled-engine licensing.

Automated tests, package verification, and physical-device tests are separate evidence. The [validation report](docs/QUALITY.md) records their actual scope, known dependency alerts, and device-testing limits.
