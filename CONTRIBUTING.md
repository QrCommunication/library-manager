# Contributing / Contribuer

Library Manager is a GPL-3.0 Linux desktop application. Changes should work without installing Calibre and preserve original books.

## Development environment

Use the toolchain versions in `rust-toolchain.toml` and `package.json`, and the committed `Cargo.lock` and `pnpm-lock.yaml`. The desktop build requires GTK 3 and WebKitGTK 4.1 development packages. Node and pnpm are build tools, not prerequisites for installed release packages.

On Debian or Ubuntu:

```sh
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev libssl-dev patchelf
pnpm install --frozen-lockfile
pnpm tauri dev
```

For RPM packaging, install `rpm`. The bundled MOBI tool is built from the included libmobi source; no ebook-convert executable is invoked.

## Checks

```sh
cargo fmt --all -- --check
cargo test -p library-core --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
pnpm check
pnpm test
pnpm build
```

Use synthetic test books. Publicly licensed upstream libmobi fixtures are included with their license. Never commit purchased books, provider keys, profile databases, personal screenshots, or files copied from a reader.

## Code boundaries

Rust entities and contracts live in `models.rs`. Repositories handle SQLite; focused services handle imports, transformation, devices, AI, and reading. Tauri commands validate requests and delegate to services. They do not execute frontend-supplied shell strings or SQL.

Svelte feature components call the typed IPC client. Localized strings live in `src/lib/locales/`. A new locale file is discovered automatically; match the English key structure and validate translation parity.

Keep IPC documentation, frontend contracts, tests, and project cartography synchronized when changing behavior. Prefer a focused regression test over a test that repeats the implementation. A provider or device adapter must distinguish mocked protocol coverage from real authenticated or physical-device verification.

## Pull requests

Describe the concrete trigger and resulting behavior, list relevant checks, and explain remaining limitations. Use a small reproducible book or protocol fixture when a bug depends on input. Do not claim hardware compatibility based only on compilation.

## Français

Les contributions doivent préserver les originaux, fonctionner sans Calibre et mettre à jour les traductions, contrats, tests et documentation concernés. Utilisez des livres synthétiques pour les tests et décrivez précisément ce qui a été vérifié.
