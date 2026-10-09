# Construire Library Manager

Library Manager est autonome à l’exécution. Calibre, Node.js, Rust et Python ne sont pas nécessaires pour utiliser les paquets Linux. Node.js et Rust sont des outils de construction ; libmobi est compilé et embarqué automatiquement.

## Environnement de référence

- Linux x86_64, Ubuntu 22.04 ou système plus récent avec WebKitGTK 4.1 et GTK 3.
- Rust 1.99.0, fixé dans `rust-toolchain.toml`.
- Node.js 26.11.1 ou version stable plus récente compatible.
- pnpm 10.33.0, fixé par le champ `packageManager`.
- Un compilateur C, `make`, `pkg-config`, `patchelf`, `rpm` et `file` pour construire les paquets ; `file` est requis par linuxdeploy pour l’AppImage.

Installer les dépendances de construction sur Ubuntu :

```sh
sudo apt update
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev \
  libgtk-3-dev librsvg2-dev libssl-dev libdbus-1-dev patchelf rpm file curl ca-certificates
```

Installer Node.js depuis sa distribution officielle et Rust avec rustup. Les versions ne doivent pas être abaissées pour contourner une erreur de compilation. Depuis le dépôt :

```sh
pnpm install --frozen-lockfile
pnpm check
pnpm test
pnpm build
cargo build --workspace --locked
cargo test -p library-core --locked
cargo test -p library-manager --lib --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
```

La construction du workspace prépare le moteur MOBI avant les tests de conversion. Aucun téléchargement de livre ni aucun identifiant IA n’est nécessaire aux tests ordinaires. Le test réseau explicitement ignoré peut être lancé séparément et ne réalise aucun appel payant.

Les huit tests de la coque Tauri couvrent six helpers du pont et deux contrôles d’instance unique ; ils ont été exécutés avec succès sur le code de cette version. Cette validation du pont est distincte du parcours GUI du paquet décrit ci-dessous, dont le rapport doit confirmer le résultat pour le binaire distribué.

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

La construction finale des trois formats a réussi depuis [3e6177b](https://github.com/QrCommunication/library-manager/commit/3e6177b1241e35e3ef1cdf2ac6b6466d5742f775). Les DEB/RPM déclarent `ca-certificates` en complément des dépendances GTK 3/WebKitGTK 4.1 calculées par Tauri. Leur gestionnaire de paquets installe donc le magasin de confiance TLS, nécessaire à l’initialisation des clients HTTPS de l’application. Sur Ubuntu 22.04 vierge, l’installation du DEB final a ajouté ce magasin auparavant absent ; le parcours GUI passe les dix étapes, avec deux cartes visibles et une capture enregistrée. Les résultats figurent dans `ubuntu-clean-report.json`. Aucun environnement Calibre, Node.js ou Rust n’a été installé pour exécuter l’application.

L’AppImage utilise les bibliothèques compatibles et les certificats du système. Son montage standard nécessite FUSE. Sans FUSE, utiliser `APPIMAGE_EXTRACT_AND_RUN=1 ./library-manager_0.1.0_amd64.AppImage`, ou extraire le fichier avec `--appimage-extract` puis lancer `squashfs-root/AppRun`. Aucun de ces modes ne nécessite Calibre, Node.js ou Rust à l’exécution.

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

Le script démarre la session avec `capabilities.alwaysMatch["tauri:options"] = { application, args: [] }`. Il appelle le vrai pont IPC depuis `execute/async`, sans charger la démonstration. Le parcours vérifie fenêtre/bootstrap, six fournisseurs API, import TXT et déduplication, conversion explicite TXT vers EPUB avant lecture/progression, optimisation Xteink, sortie MOBI native, import du MOBI synthétique et conversion explicite MOBI vers EPUB avec le moteur libmobi embarqué. Il vérifie ensuite modifications de métadonnées avec révision et annulation, attente IA sans configuration, langues FR/EN et conservation après redémarrage. Chaque préparation EPUB attend une tâche terminée et vérifie rapport et variante avant d’ouvrir le lecteur. Le rapport enregistre le format source réellement choisi pour la sortie MOBI ; ce format dépend de la variante disponible, et un EPUB converti ne remplace pas automatiquement l’original comme source. Le MOBI de retour a un libellé PalmDB distinct, sans changement des records, pour exercer le chemin d’entrée MOBI.

Le rapport conserve uniquement états, compteurs, codes publics et SHA-256 du binaire. La capture PNG est facultative. Les sessions sont fermées en `finally`, les appels et attentes sont bornés, et un état de tâche échoué ou annulé ne vaut jamais réussite. `--timeout` fixe une durée globale de 30 à 600 secondes (240 par défaut). Les tests purs du script se lancent avec `python3 scripts/native-smoke.py --self-test` ; ils ne constituent pas une preuve de fonctionnement du paquet. L’exécution E2E réelle doit produire un rapport `status: passed` pour le binaire distribué.

Les neuf tests purs du script passent. Les dix étapes natives ont également réussi sur le DEB dans l’environnement de construction Ubuntu 22.04, le RPM installé dans Fedora 44 et le contenu de l’AppImage extrait puis lancé par `AppRun`, avec deux cartes visibles. Le cycle arrêt/redémarrage de l’enveloppe extérieure en mode `APPIMAGE_EXTRACT_AND_RUN` n’est pas déclaré validé dans WebDriver : ce parcours échoue au redémarrage, tandis que celui avec `AppRun` extrait passe. Voir [QUALITY.md](QUALITY.md) pour les rapports, les empreintes et le périmètre précis des preuves.

## Notices tierces reproductibles

Après installation des dépendances verrouillées, lancer `python3 scripts/third-party-notices.py --self-test`, puis `python3 scripts/third-party-notices.py --check`. Les sept tests du générateur passent ; le document final recense 647 packages et 43 limites d’inventaire de sources. Le générateur travaille avec `cargo metadata --locked --offline`, le graphe pnpm actif et les sources locales. Il conserve les dépendances transitives et les alias, tout en excluant les anciens packages de la virtual store qui ne sont plus atteignables depuis le projet. Les notices omises des archives Cargo de `dlopen2` et `alloc-stdlib` sont conservées dans `vendor/licenses/` : version, licence, repository, révision VCS et SHA-256 du texte original sont vérifiés. Le document généré et les licences de l’application/libmobi sont inclus dans les paquets. Les autres limites d’inventaire sont signalées explicitement ; la présence d’un package dans l’inventaire toutes plateformes ne prouve pas son inclusion dans chaque binaire Linux.

## Architecture et données

Le workspace Rust sépare `library-core` de la coque Tauri. Les tests du moteur ne nécessitent pas d’écran. Les données privées, livres et clés API ne doivent jamais être ajoutés au dépôt ni aux fixtures publiques. Pour tester une collection personnelle, utiliser un profil temporaire et publier uniquement les nombres et codes de validation.

Les cartographies locales de développement sont privées et ne remplacent pas les documents d’architecture du dépôt. Les comportements publics sont décrits dans [BLUEPRINT.md](BLUEPRINT.md), [IPC.md](IPC.md), [METADATA_POLICY.md](METADATA_POLICY.md) et [DEVICE_PROTOCOL.md](DEVICE_PROTOCOL.md).
