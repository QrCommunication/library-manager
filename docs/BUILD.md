# Construire Library Manager

Library Manager est autonome à l’exécution. Calibre, Node.js, Rust et Python ne sont pas nécessaires pour utiliser les paquets Linux. Node.js et Rust sont des outils de construction ; libmobi est compilé et embarqué automatiquement.

## Environnement de référence

- Linux x86_64, Ubuntu 22.04 ou système plus récent avec WebKitGTK 4.1 et GTK 3.
- Rust 1.99.0, fixé dans `rust-toolchain.toml`.
- Node.js 26.11.1 ou version stable plus récente compatible.
- pnpm 10.33.0, fixé par le champ `packageManager`.
- Un compilateur C, `make`, `pkg-config`, `patchelf` et `rpm` pour construire les paquets.

Installer les dépendances de construction sur Ubuntu :

```sh
sudo apt update
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev \
  libgtk-3-dev librsvg2-dev libssl-dev libdbus-1-dev patchelf rpm curl ca-certificates
```

Installer Node.js depuis sa distribution officielle et Rust avec rustup. Les versions ne doivent pas être abaissées pour contourner une erreur de compilation. Depuis le dépôt :

```sh
pnpm install --frozen-lockfile
pnpm check
pnpm test
pnpm build
cargo build --workspace --locked
cargo test -p library-core --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

La construction du workspace prépare le moteur MOBI avant les tests de conversion. Aucun téléchargement de livre ni aucun identifiant IA n’est nécessaire aux tests ordinaires. Le test réseau explicitement ignoré peut être lancé séparément et ne réalise aucun appel payant.

## Développement

```sh
pnpm tauri dev
```

Le serveur Vite écoute uniquement sur `127.0.0.1:1420`. L’application native utilise la vraie bibliothèque dans son dossier de données. Le navigateur seul ne dispose pas du pont natif : ajouter `?demo=1` permet de vérifier l’interface avec un jeu de livres fictifs et des actions de fichiers désactivées.

## Paquets Linux

```sh
pnpm tauri build --bundles deb,rpm,appimage
```

Les paquets sont créés dans `target/release/bundle/`. Le moteur MOBI est inclus comme exécutable compagnon ; ne pas distribuer l’exécutable principal seul. Vérifier les dépendances du DEB/RPM, la présence de ce moteur et les licences, puis installer le paquet dans un système vierge avant publication.

Les paquets de référence sont construits sur Ubuntu 22.04 pour maintenir une base glibc 2.35. Compiler sur une distribution plus récente peut augmenter la version minimale de glibc ; cela doit être annoncé dans les notes de publication. Les compilateurs et outils de développement ne font pas partie des dépendances d’exécution.

## Test natif du paquet dans un environnement isolé

`scripts/native-smoke.py` utilise uniquement Python standard et le protocole HTTP WebDriver. Il ne nécessite aucun plugin de test dans l’application. Installer le paquet et les outils de test dans un conteneur Linux avec GTK/WebKit, Xvfb et une session D-Bus isolée ; les outils ne sont pas inclus dans les dépendances d’exécution de Library Manager. Utiliser `tauri-driver` 2.1.0 et le `WebKitWebDriver` de la distribution. Aucune carte SD, collection personnelle ni clé API n’est nécessaire.

Avant de lancer le driver, créer des répertoires de profil temporaires et les exporter dans le même environnement que le driver et le script. La racine de données doit être neuve et vide ; le script refuse un profil préexistant et vérifie la racine renvoyée par le backend avant toute mutation.

```sh
native_smoke_root=$(mktemp -d /tmp/library-manager-native-smoke.XXXXXX)
export XDG_DATA_HOME="$native_smoke_root/data"
export XDG_CONFIG_HOME="$native_smoke_root/config"
export XDG_CACHE_HOME="$native_smoke_root/cache"
export TAURI_WEBVIEW_AUTOMATION=true
mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"
```

Dans le conteneur avec Xvfb actif, lancer le driver puis le script :

```sh
tauri-driver --port 4444 --native-port 4445 --native-host 127.0.0.1 \
  --native-driver /usr/bin/WebKitWebDriver &
native_smoke_driver_pid=$!
trap 'kill "$native_smoke_driver_pid" 2>/dev/null || true' EXIT
python3 scripts/native-smoke.py \
  --driver-url http://127.0.0.1:4444 \
  --binary /usr/bin/library-manager \
  --screenshot "$native_smoke_root/native-window.png" \
  --report "$native_smoke_root/native-report.json"
```

Le script démarre la session avec `capabilities.alwaysMatch["tauri:options"] = { application, args: [] }`. Il appelle le vrai pont IPC depuis `execute/async`, sans charger la démonstration. Le parcours vérifie fenêtre/bootstrap, six fournisseurs API, import TXT et déduplication, lecture/progression, optimisation Xteink, conversion EPUB vers MOBI puis reconstruction d’un MOBI synthétique par libmobi, modifications de métadonnées avec révision et annulation, attente IA sans configuration, langues FR/EN et conservation après redémarrage. Le MOBI de retour a un libellé PalmDB distinct, sans changement des records, pour exercer le chemin d’entrée MOBI plutôt qu’une conversion EPUB vers EPUB.

Le rapport conserve uniquement états, compteurs, codes publics et SHA-256 du binaire. La capture PNG est facultative. Les sessions sont fermées en `finally`, les appels et attentes sont bornés, et un état de tâche échoué ou annulé ne vaut jamais réussite. `--timeout` fixe une durée globale de 30 à 600 secondes (240 par défaut). Les tests purs du script se lancent avec `python3 scripts/native-smoke.py --self-test` ; ils ne constituent pas une preuve de fonctionnement du paquet. L’exécution E2E réelle doit produire un rapport `status: passed` pour le binaire distribué.

## Notices tierces reproductibles

Après installation des dépendances verrouillées, lancer `python3 scripts/third-party-notices.py --self-test`, puis `python3 scripts/third-party-notices.py --check`. Le générateur travaille avec `cargo metadata --locked --offline` et les sources locales. Les notices omises des archives Cargo de `dlopen2` et `alloc-stdlib` sont conservées dans `vendor/licenses/` : version, licence, repository, révision VCS et SHA-256 du texte original sont vérifiés. Le document généré et les licences de l’application/libmobi sont inclus dans les paquets. Les autres limites d’inventaire sont signalées explicitement ; la présence d’un package dans l’inventaire toutes plateformes ne prouve pas son inclusion dans chaque binaire Linux.

## Architecture et données

Le workspace Rust sépare `library-core` de la coque Tauri. Les tests du moteur ne nécessitent pas d’écran. Les données privées, livres et clés API ne doivent jamais être ajoutés au dépôt ni aux fixtures publiques. Pour tester une collection personnelle, utiliser un profil temporaire et publier uniquement les nombres et codes de validation.

Les cartographies locales de développement sont privées et ne remplacent pas les documents d’architecture du dépôt. Les comportements publics sont décrits dans [BLUEPRINT.md](BLUEPRINT.md), [IPC.md](IPC.md), [METADATA_POLICY.md](METADATA_POLICY.md) et [DEVICE_PROTOCOL.md](DEVICE_PROTOCOL.md).
