# Construire Library Manager

Library Manager embarque son moteur Rust, SQLite et le convertisseur MOBI. Calibre, Node.js, Rust et Python sont inutiles pour exécuter les paquets ; les bibliothèques système et les prérequis propres à chaque plateforme restent nécessaires.

**La version 0.2.1 est en préparation.** Ce document décrit les commandes et contrôles du dépôt actuel, sans attester que les installateurs Windows/macOS sont publiés, signés ou notarisés. Les preuves de la version publiée 0.2.0 restent distinctes dans [QUALITY.md](QUALITY.md) et les [notes de livraison 0.2.0](https://github.com/QrCommunication/library-manager/releases/tag/v0.2.0).

## Outils et plateformes natives

Les versions de référence sont **Rust 1.99.0**, **Node.js 26.11.1** et **pnpm 10.33.0**, fixées dans la toolchain, le manifeste et les workflows. Installer les dépendances verrouillées sans modifier les lockfiles pour contourner une erreur de compilation.

| Plateforme | Runner CI | Cible Rust | Paquets |
| --- | --- | --- | --- |
| Linux x86_64 | Ubuntu 22.04, workflow `ci.yml` | `x86_64-unknown-linux-gnu` | DEB, RPM, AppImage |
| Windows x64 | `windows-2025`, workflow `desktop.yml` | `x86_64-pc-windows-msvc` | MSI, NSIS/EXE |
| macOS Apple Silicon | `macos-26`, workflow `desktop.yml` | `aarch64-apple-darwin` | Application, DMG |
| macOS Intel | `macos-26-intel`, workflow `desktop.yml` | `x86_64-apple-darwin` | Application, DMG |

Construire sur la plateforme et l’architecture cibles : `src-tauri/build.rs` refuse un `TARGET` différent de `HOST`. Les deux builds macOS sont séparés ; ils ne produisent pas une application universelle. La version minimale macOS est **13.0**, dans la configuration Tauri et `MACOSX_DEPLOYMENT_TARGET` du workflow. Un build sur Windows Server ne constitue pas un essai de l’interface sur Windows 11.

### Linux

Ubuntu 22.04 fournit la base glibc 2.35 des paquets de référence. Une compilation sur une distribution plus récente peut augmenter la version minimale de glibc ; annoncer cette différence dans les notes de livraison.

```sh
sudo apt update
sudo apt install build-essential pkg-config libwebkit2gtk-4.1-dev \
  libgtk-3-dev librsvg2-dev libssl-dev libdbus-1-dev patchelf rpm file curl ca-certificates
```

GTK 3 et WebKitGTK 4.1 sont nécessaires. `file`, `patchelf` et les outils RPM servent à la construction des paquets. Les DEB/RPM déclarent aussi `ca-certificates` pour le magasin de confiance TLS.

### Windows

Installer la toolchain Rust **MSVC x64**, les outils C++ Microsoft et les prérequis Tauri. La construction MSI requiert la fonctionnalité Windows VBScript ; le workflow vérifie sa présence et l’active si nécessaire.

MSYS2 **UCRT64** sert uniquement à compiler le moteur MOBI compagnon. Installer `make`, `mingw-w64-ucrt-x86_64-gcc` et `mingw-w64-ucrt-x86_64-zlib`, puis définir `LIBRARY_MANAGER_MSYS2_ROOT` sur le chemin Windows absolu de cette installation. La CI utilise le chemin réel retourné par l’action MSYS2, sans supposer un emplacement fixe.

Le build-script invoque explicitement Bash, GCC, `ar`, `ranlib` et `objdump` de cette installation. Les variables `CC` et le `PATH` UCRT64 restent dans les processus de compilation du sidecar ; ils ne remplacent pas la toolchain MSVC de l’application Rust. Pour la liaison, `build.rs` impose `TOOLS_STATIC=-all-static` et `LIBZ_LDFLAGS=-L/ucrt64/lib -lz`. Le premier transmet la liaison entièrement statique au compilateur via Libtool ; `-static` seul ne fige que les bibliothèques gérées par Libtool. Le second conserve `-lz` dans les dépendances transitives de `libmobi.la` ; le lien final avec `-all-static` sélectionne l’archive UCRT64. Passer directement `/ucrt64/lib/libz.a` dans les flags de la bibliothèque imbrique cette archive dans `libmobi.a`, perd la dépendance et laisse `uncompress` non résolu au lien de l’exécutable. Le chemin `/ucrt64` appartient au shell MSYS2 du sidecar, pas à une installation Linux.

Le checkout Windows peut donner aux prérequis Autotools un horodatage plus récent que leurs fichiers générés. Pour construire l’archive distribuée sans lancer une régénération avec `aclocal-1.16`, `build.rs` passe à GNU Make `--old-file` pour ces sept entrées : `aclocal.m4`, `configure`, `config.h.in`, `Makefile.in`, `src/Makefile.in`, `tools/Makefile.in` et `tests/Makefile.in`. `AM_MAKEFLAGS` transmet les mêmes options aux sous-`make`, car `-o` n’est pas propagé automatiquement. La présence de chaque entrée est contrôlée ; les sources C, objets, `config.status` et Makefiles de sortie conservent leurs dépendances normales. Les fichiers vendor et les empreintes du build ne sont pas modifiés. Cette version de libmobi ne fournit pas `AM_MAINTAINER_MODE` : ajouter `--disable-maintainer-mode` ne remplace pas ce traitement.

L’audit `objdump` exige un exécutable PE x86_64 et refuse les DLL redistribuables MSYS, MinGW ou zlib. Seuls les imports système autorisés passent ce contrôle. Le rapport `sidecar-dependencies.log` accompagne les artefacts Windows ; il ne remplace pas un essai d’installation ou de GUI. Trois tests du build-script ont réussi sous Linux : le choix statique face à une bibliothèque partagée concurrente, la dépendance transitive réelle `exécutable → libmobi.la → uncompress`, avec absence d’archive zlib imbriquée et de dépendance dynamique, et les prérequis Autotools figés sans masquer une erreur de compilation C. Ces tests ne constituent pas une exécution du sidecar Windows ; la CI doit encore auditer les imports du véritable PE et valider les paquets natifs.

L’adaptateur [secure_fs/windows.rs](../crates/library-core/src/secure_fs/windows.rs) publie les fichiers avec `NtSetInformationFile`, classe `FileRenameInformation` (`10`), un handle parent vérifié et un nom relatif. Il conserve `ReplaceIfExists = false` et le contrôle d’identité de la stage. Cet appel remplace le wrapper Win32 `SetFileInformationByHandle` qui a produit l’erreur 87 au renommage dans la CI du commit `8e37f8a` ; l’écriture du fichier avait déjà réussi. Le [contrat NT de renommage](https://learn.microsoft.com/en-us/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information) conserve l’association entre le handle parent et le nom, sans reconstruire un chemin absolu.

Pour synchroniser un répertoire, l’adaptateur ouvre ce même objet avec `NtCreateFile`, un nom vide relatif au handle parent, `FILE_DIRECTORY_FILE`, le mode synchrone et le refus des points de réanalyse. Il demande `FILE_WRITE_DATA | FILE_READ_ATTRIBUTES | SYNCHRONIZE`, contrôle à nouveau le type et l’identité, puis appelle `NtFlushBuffersFileEx` avec des flags nuls. Cette ouverture remplace `ReOpenFile`, qui renvoyait l’erreur 5 avant même l’appel de synchronisation. Les ACL et les droits de partage restent inchangés ; aucune réouverture par chemin absolu ni réussite fictive de synchronisation n’est introduite.

La CI Windows du commit `8e37f8a` a compilé le sidecar MOBI, puis a enregistré **118 tests Rust réussis et 194 échoués**, notamment sur cette barrière commune de fichiers et de profil. Ce résultat ne valide pas les paquets Windows. La compilation croisée Windows GNU des tests du nouvel adaptateur et le formatage ont réussi ; les nouveaux tests couvrent les noms Unicode et d’un caractère, ainsi que la conservation de l’identité et des ACL lors de la barrière de répertoire. Leur exécution native Windows reste attendue avant livraison.

### macOS

Installer les outils de ligne de commande Xcode et construire séparément sur Intel et Apple Silicon. Le moteur MOBI utilise le compilateur C natif, `configure` et `make`, comme sous Linux. Définir `MACOSX_DEPLOYMENT_TARGET=13.0` avant compilation.

## Vérifications communes

Depuis le dépôt, sur la plateforme native :

```sh
pnpm install --frozen-lockfile --force --ignore-scripts
cargo fetch --locked
pnpm check
pnpm test
pnpm build
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
python3 scripts/third-party-notices.py --self-test
python3 scripts/third-party-notices.py --check
```

Sous Windows, utiliser `python` si l’installation ne fournit pas la commande `python3`. La compilation Tauri prépare le sidecar MOBI avant les tests qui en ont besoin. Les tests ordinaires ne nécessitent aucun livre privé ni identifiant IA. Les tests ignorés exigeant un environnement particulier doivent être distingués des tests effectivement exécutés.

Le build-script vérifie les sources embarquées et conserve une empreinte du contenu, des options et de la toolchain pour invalider son cache. Les SHA-256 sont calculés en Rust avec `sha2`, sans dépendre de la commande GNU `sha256sum`. Le workflow fixe `SOURCE_DATE_EPOCH` à la date du commit. Ces contrôles facilitent la reproduction de la construction ; ils n’affirment pas que des paquets signés ou notarisés ont des octets identiques entre deux exécutions.

## Développement et paquets

```sh
pnpm tauri dev
```

Vite écoute sur `127.0.0.1:1420`. L’application native utilise les services et le profil réels. Le navigateur seul peut ouvrir `?demo=1` pour une démonstration fictive sans pont natif ni action sur les fichiers.

Commandes de packaging équivalentes aux cibles des workflows :

```sh
# Linux x86_64 natif
pnpm tauri build --ci --bundles deb,rpm,appimage -- --locked

# Windows x64 natif
pnpm tauri build --ci --target x86_64-pc-windows-msvc --bundles msi,nsis -- --locked

# macOS Apple Silicon natif
pnpm tauri build --ci --target aarch64-apple-darwin --bundles app,dmg -- --locked

# macOS Intel natif
pnpm tauri build --ci --target x86_64-apple-darwin --bundles app,dmg -- --locked
```

Sans définir `CARGO_TARGET_DIR`, les commandes de développement ci-dessus gardent leurs chemins habituels : `target/release/bundle/` sans cible explicite, et `target/<cible>/release/bundle/` avec `--target`. Le moteur MOBI est inclus comme exécutable compagnon, avec le suffixe `.exe` sous Windows. Distribuer le paquet complet avec les licences, jamais l’exécutable principal seul.

### Répertoires et caches en CI

Les workflows [Linux](../.github/workflows/ci.yml) et [Windows/macOS](../.github/workflows/desktop.yml) définissent `CARGO_TARGET_DIR` sur `${{ github.workspace }}/target/cargo`. La signature, les contrôles et la collecte des bundles utilisent ce répertoire explicitement.

| Sortie | Chemin en CI |
| --- | --- |
| Bundles Linux, sans `--target` | `target/cargo/release/bundle/` |
| Bundles Windows/macOS, avec `--target` | `target/cargo/<cible>/release/bundle/` |
| Compilation C et cache local du moteur MOBI | `target/libmobi-<cible>/` |
| Sidecar préparé pour Tauri | `src-tauri/binaries/library-manager-mobitool-<cible>`, avec `.exe` sous Windows |

Cette séparation évite que le nettoyage de `Swatinem/rust-cache` traite `libmobi-<cible>/tests` comme un profil Rust, puis tente d’ouvrir ses sous-répertoires inexistants `target` et `trybuild`. Le cache Rust vise seulement `target/cargo`, utilise le préfixe `v1-isolated-rust` et peut être sauvegardé même si le job échoue (`cache-on-failure: true`).

Le cache C utilise une action distincte sur `target/libmobi-<cible>`. Sa clé combine la cible, le hash de la version réelle du compilateur et le hash de `src-tauri/build.rs` et des sources `vendor/libmobi-0.12`. Avant toute réutilisation, [build.rs](../src-tauri/build.rs) contrôle encore l’empreinte complète du build et le SHA-256 du moteur ; les options, `SOURCE_DATE_EPOCH` et les dépendances de la toolchain restent pris en compte. Ce cache C standard se sauvegarde sur succès du job. La configuration du cache et une fixture locale ne prouvent pas qu’un cache GitHub a effectivement été restauré : vérifier les logs du run concerné pour établir cette réutilisation.


`desktop.yml` vérifie le commit demandé, la concordance des versions, l’architecture de l’application et du sidecar, les licences, l’exécution du sidecar et les SHA-256 des artefacts. Il exporte `package-report.json` et `SHA256SUMS`. Les noms des artefacts distinguent plateforme, architecture et mode de signature. Le workflow ne publie pas lui-même une release GitHub.

L’AppImage utilise les bibliothèques compatibles et les certificats du système. Son montage classique nécessite FUSE. Le mode `APPIMAGE_EXTRACT_AND_RUN=1` et l’exécution de `AppRun` après extraction sont des parcours distincts, à valider séparément.

## Signature et notarisation macOS

Les secrets GitHub attendus pour la méthode API sont, uniquement par leurs noms :

- `APPLE_CERTIFICATE` : P12 encodé en base64, contenant le certificat Developer ID Application **et sa clé privée** ; un fichier CER seul ne suffit pas.
- `APPLE_CERTIFICATE_PASSWORD` : mot de passe du P12.
- `APPLE_SIGNING_IDENTITY` : identité Developer ID à utiliser.
- `APPLE_TEAM_ID` : équipe Apple.
- `APPLE_API_ISSUER` : issuer de la clé API de notarisation.
- `APPLE_API_KEY` : identifiant de cette clé API.
- `APPLE_API_KEY_CONTENT` : contenu privé de sa clé `.p8`.

La méthode alternative utilise `APPLE_ID` et `APPLE_PASSWORD` avec l’équipe et les éléments du certificat. Si les deux méthodes sont complètes, le workflow choisit la clé API et retire `APPLE_ID`/`APPLE_PASSWORD` de l’environnement transmis à Tauri. Une valeur vide reste une variable présente : Tauri CLI 2.12.1 sélectionnerait sinon le mode Apple ID avant le mode API. Le mode Apple ID retire symétriquement les variables API inutilisées. Aucun identifiant, certificat privé ou contenu de secret ne doit entrer dans le dépôt, les logs ou les rapports publics.

Le workflow importe le P12 dans un trousseau temporaire, donne les droits nécessaires à `codesign` et écrit la clé API dans un fichier de permissions `0600` transmis par `APPLE_API_KEY_PATH`. Le nettoyage s’exécute même en cas d’échec et restaure la liste des trousseaux. Une configuration Apple partielle bloque le packaging ; sans aucun secret Apple, un build explicitement marqué `unsigned` peut être produit, sans signature Developer ID ni promesse de notarisation.

Pour le parcours signé, distinguer les contrôles suivants :

1. Construction Tauri de l’application signée et notarisée ; vérification stricte `codesign` de l’application et du sidecar, avec concordance de l’équipe.
2. Validation du ticket de l’application par `stapler`, puis acceptation Gatekeeper par `spctl --assess --type execute`.
3. Signature du DMG, soumission `notarytool submit --wait` et résultat JSON **`Accepted`**.
4. Agrafage et validation du ticket du DMG avec `stapler`, puis contrôle `spctl --assess --type open --context context:primary-signature`.

Sur le checkpoint `8e37f8a`, l’import du P12 et la signature Developer ID réussissent, mais la notarisation échoue avec HTTP 401 parce que les variables Apple ID vides sélectionnent le mauvais mode. La clé centrale a réussi des requêtes Apple en lecture seule avec HTTP 200 ; le nettoyage des variables doit encore être confirmé par la nouvelle CI. La présence de secrets ou la réussite de la compilation ne prouve aucun de ces résultats. `notarization-report.json` décrit les contrôles effectivement réussis pour le commit et les artefacts concernés. Un essai GUI macOS reste une preuve supplémentaire.

## Publication vérifiée depuis le tag

Le workflow [release.yml](../.github/workflows/release.yml) publie les artefacts construits par les CI du **commit exact du tag**. Il se déclenche au push d’un tag stable `vX.Y.Z`. Le déclenchement manuel accepte également un tag existant, avec les mêmes contrôles. Son exécution complète reste à valider pour la livraison 0.2.1 ; la présence du workflow ne prouve pas une publication réussie.

Terminer et pousser les changements sur `main`, puis attendre la réussite des CI Linux, Windows, macOS ARM64/Intel et CodeQL sur ce même SHA avant de créer le tag. Les changements documentaires déclenchent aussi la CI Linux : les artefacts d’un commit précédent ne suffisent pas. Depuis le commit final de `main`, les commandes suivantes utilisent l’authentification SSH de Git :

```sh
git push git@github.com:QrCommunication/library-manager.git main
# Attendre les CI réussies du SHA exact avant ces deux commandes.
git tag v0.2.1 HEAD
git push git@github.com:QrCommunication/library-manager.git refs/tags/v0.2.1
```

Ne pas déplacer un tag existant pour contourner un échec. Le workflow vérifie que le tag correspond au HEAD de `main`, que sa version concorde avec la configuration Tauri et que les jobs requis des CI et les quatre analyses CodeQL ont réussi sur ce commit. Il attend les CI en cours, mais refuse une CI terminée en échec.

La préparation possède uniquement `contents: read` et `actions: read`. Le job de publication dépend de sa réussite et reçoit `contents: write` ainsi que `actions: read`. Les requêtes utilisent le `GITHUB_TOKEN` éphémère du job ; aucune session personnelle `gh` ni aucun jeton personnel GitHub n’est nécessaire. Les redirections vers les CDN ne reçoivent pas ce jeton.

La préparation télécharge les archives CI et vérifie leurs SHA-256, chaque fichier de `SHA256SUMS`, les licences identiques aux sources du tag et le contenu complet de l’archive source. Les rapports de provenance doivent confirmer le commit, la version, l’architecture et l’exécution du moteur compagnon. Les paquets macOS doivent être signés, avec contrôles `codesign`, tickets validés, Gatekeeper et notarisation du DMG **`Accepted`**. Les MSI/EXE Windows sont explicitement **non signés** ; leur audit du sidecar doit refuser les DLL non système non embarquées.

Le workflow installe ensuite le DEB CI dans une image Ubuntu 22.04 et lance quatre parcours, chacun avec un profil synthétique neuf et `--network none` : général DEB, assistant DEB, actions du catalogue DEB, puis parcours général via `AppRun` de l’AppImage extraite. Il exige respectivement au moins 10, 10, 9 et 10 contrôles réussis, aucun appel API payant ni écriture sur appareil physique. Les empreintes relient chaque rapport à l’exécutable réellement lancé et au paquet qui le contient. Trois mesures sont conservées : le binaire installé du DEB, le lanceur `AppRun` exécuté et le binaire ELF `usr/bin/library-manager` de l’AppImage extraite. Les trois parcours DEB doivent utiliser le même binaire ; le rapport AppImage doit correspondre au lanceur mesuré. La provenance et les sommes de contrôle identifient les paquets dont ces fichiers proviennent. Le packaging peut modifier le binaire ELF : son égalité octet par octet avec le binaire du DEB n’est pas requise. L’extraction de l’AppImage ne prouve pas son montage FUSE, ni un parcours graphique Windows ou macOS.

Après ces validations, le manifeste, les rapports, les paquets, les licences, les sources et les sommes de contrôle sont transmis dans un artefact dont le job de publication vérifie l’identité, l’empreinte et l’appartenance à cette exécution. La publication suit cet ordre :

1. Créer ou reprendre un brouillon compatible, téléverser les fichiers manquants et télécharger chaque fichier avec authentification pour vérifier son SHA-256. Un fichier divergent n’est jamais remplacé silencieusement.
2. Rendre la version publique sans la marquer comme dernière version, puis télécharger tous ses fichiers sans authentification et vérifier leurs empreintes.
3. Promouvoir la version comme dernière version seulement après réussite de tous les téléchargements publics, vérifier ce statut et conserver `PUBLIC_VERIFICATION.json` dans les artefacts du workflow.

Un échec bloque les étapes suivantes. Si le contrôle anonyme échoue après ouverture de la version, celle-ci peut rester publique sans promotion comme dernière version ; consulter le run avant de la présenter comme une livraison validée.

## Tests natifs Linux sur profil isolé

Les scripts Python utilisent le protocole HTTP WebDriver, `tauri-driver` et `WebKitWebDriver`, avec GTK/WebKit, Xvfb et une session D-Bus isolée. Ces outils servent aux tests et ne sont pas des dépendances d’exécution de l’application.

Lancer le conteneur ou namespace **sans réseau**, par exemple avec `docker run --network none`, puis créer un profil neuf à l’intérieur. Le driver et le script doivent hériter du même environnement ; le script ne peut pas changer rétroactivement celui d’un driver existant. Ne jamais utiliser un profil personnel.

```sh
native_smoke_root=$(mktemp -d /tmp/library-manager-native-smoke.XXXXXX)
export XDG_DATA_HOME="$native_smoke_root/data"
export XDG_CONFIG_HOME="$native_smoke_root/config"
export XDG_CACHE_HOME="$native_smoke_root/cache"
export TAURI_WEBVIEW_AUTOMATION=true
mkdir -p "$XDG_DATA_HOME" "$XDG_CONFIG_HOME" "$XDG_CACHE_HOME"

tauri-driver --port 4444 --native-port 4445 --native-host 127.0.0.1 \
  --native-driver /usr/bin/WebKitWebDriver &
native_smoke_driver_pid=$!
trap 'kill "$native_smoke_driver_pid" 2>/dev/null || true' EXIT

python3 scripts/native-assistant-smoke.py \
  --catalogue-actions \
  --profile-root "$XDG_DATA_HOME" \
  --driver-url http://127.0.0.1:4444 \
  --binary /usr/bin/library-manager \
  --report "$native_smoke_root/catalogue-actions-report.json"
```

Ce mode importe des livres synthétiques et vérifie les actions communes, le blocage de l’analyse sans fournisseur, la disponibilité avec une configuration synthétique, la validation durable des propositions avec et sans différences, le retrait confirmé et son annulation, ainsi que la conservation des fichiers. La clé de test est fictive, non persistée ; le réseau doit rester désactivé. Les paramètres et le secret de session sont restaurés après le contrôle du fournisseur.

Les propositions synthétiques sont préparées dans la base du **profil neuf marqué, après arrêt de l’application**. Cette fixture ne constitue ni un appel IA réussi ni une modification autorisée d’une base utilisateur. Les actions testées ensuite passent par l’interface native et les commandes officielles.

Pour le parcours général, utiliser `scripts/native-smoke.py` avec `--binary`, `--driver-url` et `--report`, sur un autre profil neuf. Le mode par défaut de `native-assistant-smoke.py` vérifie notamment le brouillon pendant les rafraîchissements et la permission limitée à une demande. Le mode `--review-profile` exige une copie privée marquée et contrôlée, jamais le profil hôte. Ne pas réutiliser le profil neuf entre ces scénarios.

Une réussite doit être établie par un rapport complet `status: passed` correspondant au SHA-256 du **binaire distribué**. Un test de script, une fixture ou un build local ne remplace pas cette preuve. Publier uniquement les états, compteurs, diagnostics sûrs et empreintes ; les captures doivent montrer des données synthétiques. Consulter [QUALITY.md](QUALITY.md) pour les résultats et limites, sans transposer les preuves historiques 0.2.0 à la livraison 0.2.1.

## Notices et données

Le générateur de notices utilise `cargo metadata --locked --offline`, le graphe pnpm verrouillé obtenu par `pnpm list --depth Infinity --json --lockfile-only`, les sources locales et libmobi embarqué. L’inventaire est l’union des dépendances accessibles pour toutes les plateformes, y compris leurs paquets optionnels ; il ne dépend plus des seuls paquets actifs sur l’hôte. Les anciens restes du store pnpm absents de ce graphe sont exclus.

Préparer les sources avec `pnpm install --frozen-lockfile --force --ignore-scripts` et `cargo fetch --locked` avant le contrôle. `--force` installe aussi les dépendances optionnelles étrangères à l’architecture hôte, tandis que `--ignore-scripts` interdit leurs scripts d’installation. Les lockfiles restent inchangés. Avec les paquets déjà présents dans le store, l’installation pnpm peut ajouter `--offline` ; cette option exige que toutes les sources nécessaires aient été téléchargées auparavant.

`python3 scripts/third-party-notices.py --check` compare les notices canoniques sans télécharger de fichiers ni exécuter de scripts de dépendances. Une source ou notice nécessaire absente provoque un échec ; préparer l’union complète permet un contrôle déterministe et hors ligne sur chaque runner. Le snapshot canonique courant recense **701 paquets et 67 limitations d’inventaire des sources**, consignées dans [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md). Ces limitations restent visibles même lorsque le contrôle réussit ; les comptes évoluent avec les dépendances verrouillées.

Une dépendance recensée pour toutes les plateformes n’est pas nécessairement embarquée dans chaque binaire. Inclure les licences de l’application, de libmobi et les notices tierces dans les paquets.

Le workspace sépare `library-core` de la coque Tauri ; les tests du noyau ne nécessitent pas d’écran. Ne jamais ajouter de livres privés ni de clés aux fixtures publiques. Voir [BLUEPRINT.md](BLUEPRINT.md), [IPC.md](IPC.md), [METADATA_POLICY.md](METADATA_POLICY.md) et [DEVICE_PROTOCOL.md](DEVICE_PROTOCOL.md) pour les contrats du produit.
