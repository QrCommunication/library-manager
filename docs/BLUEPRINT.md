# Architecture réalisée de Library Manager

État du 9 octobre 2026. [IPC.md](IPC.md) décrit les 33 commandes et les cinq événements. [MASTER_PLAN.md](MASTER_PLAN.md) conserve les preuves de validation et l’état des paquets ; ce document décrit les composants présents.

## Application et frontières

Library Manager est une application Linux autonome sous GPL-3.0-only : Tauri 2, Svelte 5, TypeScript stricte et SQLite embarqué. Le moteur Rust `library-core` ne dépend pas de Tauri ou d’une WebView. Calibre n’est ni installé ni exécuté ; le moteur MOBI est distribué avec l’application.

`src-tauri/src/main.rs` appelle `library_manager_lib::run()`. La bibliothèque desktop est produite uniquement en `rlib`, puis liée au binaire `library-manager`. La distribution Linux n’expose aucune bibliothèque native mobile et ne produit plus les sorties ABI staticlib/cdylib. Les configurations DEB/RPM/AppImage conservent ce binaire et le sidecar. Voir la [structure Tauri](https://v2.tauri.app/start/project-structure/) et les [types de liaison Rust](https://doc.rust-lang.org/reference/linkage.html).

Les commandes IPC font l’entrée/sortie et délèguent aux services. `LibraryManager` compose les services et exécute les jobs ; `BookRepository` possède les transactions métier. Les contrôleurs n’exécutent pas de SQL. Fichiers/ZIP/images utilisent `spawn_blocking`, le réseau reste async ; concurrence réglable de 1 à 4.

Le Manager détient un verrou privé `runtime.lock` sans suivi de symlink, conservé par tous ses clones. La reprise des jobs intervient après acquisition. Une deuxième ouverture produit `profileInUse`. Le plugin d’instance unique ramène la fenêtre quand le bus de session est disponible ; les erreurs d’initialisation restent affichables. La cartographie autorisée se trouve dans `~/.Codex/projects/library-manager/memory/`.

## Carte des fichiers présents

| Fichier | Responsabilité |
| --- | --- |
| `Cargo.toml`, `rust-toolchain.toml` | Workspace core/desktop, versions partagées, Rust 1.99 et profil de livraison. |
| `crates/library-core/src/lib.rs` | Façade publique du moteur et interdiction unsafe. |
| `crates/library-core/src/models.rs`, `error.rs` | Contrats Serde, filtres, patches nullable et erreurs publiques sans secrets. |
| `crates/library-core/migrations/001_initial.sql`, `database.rs` | Schéma versionné, connexions privées et migrations SQLite. |
| `crates/library-core/src/book_repository.rs` | Livres, variantes, recherche, facettes, présence, révisions, progression et journal transactionnel. |
| `crates/library-core/src/storage.rs` | Originaux immuables, SHA-256, capacités de chemin et publication atomique sans écrasement. |
| `crates/library-core/src/library.rs` | Import, normalisation, couverture, corrections, conversion, optimisation et undo. |
| `crates/library-core/src/settings.rs` | Préférences validées ; racine de bibliothèque fixée à la construction. |
| `crates/library-core/src/manager.rs` | Cycle de vie, dispatch, annulation, attente configuration, scans et événements. |
| `crates/library-core/src/epub.rs` | ZIP/XML bornés, EPUB 2/3, rôles bibliographiques, OPF, manifeste, spine et sommaire. |
| `crates/library-core/src/reader.rs` | Sections assainies, images locales bornées et positions. |
| `crates/library-core/src/optimizer.rs` | Quatre profils, images/polices/compression et validation des dérivés. |
| `crates/library-core/src/conversion.rs` | Matrice réelle, conversions natives et processus MOBI embarqué borné. |
| `crates/library-core/src/devices.rs` | Montages USB/SD/MTP accessibles, identité, inventaire et rapprochement par contenu. |
| `crates/library-core/src/transfer.rs` | Copies USB et client HTTP CrossPoint, staging, vérification et annulation. |
| `crates/library-core/src/calibre_wireless.rs` | Serveur TCP smart-device autonome compatible Calibre/KOReader. |
| `crates/library-core/src/providers.rs` | Six API, catalogues dynamiques et secrets session/coffre Linux. |
| `crates/library-core/src/web.rs` | Recherche et pages publiques, protection SSRF et budgets. |
| `crates/library-core/src/enrichment.rs` | Propositions bibliographiques avec preuves ; aucune mutation directe. |
| `crates/library-core/src/chat.rs` | Conversations, plans de recherche validés, contexte et citations obtenues. |
| `crates/library-core/src/jobs.rs` | File durable, tentatives, reprise, attente, résultats et tokens d’annulation. |
| `src-tauri/src/lib.rs`, `main.rs`, `commands.rs` | Shell, état d’initialisation, plugins et pont IPC/événements. |
| `src-tauri/build.rs`, `vendor/libmobi-0.12/` | Construction native du sidecar depuis les sources versionnées. |
| `src-tauri/tauri.conf.json`, `capabilities/default.json` | Fenêtre, CSP, dialogue natif et bundles Linux ; aucune permission shell arbitraire. |
| `src/lib/contracts.ts`, `api.ts`, `preview.ts` | IPC typée et aperçu navigateur explicitement identifié avec fixtures synthétiques. |
| `src/lib/i18n.ts`, `locales/fr.json`, `locales/en.json` | Langue système, sélection, fallback et découverte des dictionnaires. |
| `src/lib/components/LibraryView.svelte` | Grille/table, recherche, tris, facettes, pagination, sélection et présence. |
| `src/lib/components/BookPanel.svelte` | Fiche, propositions IA, variantes et actions. |
| `src/lib/components/ReaderView.svelte` | Sommaire, préférences, progression et iframe opaque srcdoc. |
| `src/lib/components/DevicesView.svelte` | Appareils, connexion manuelle, inventaire et transferts. |
| `src/lib/components/ChatView.svelte`, `SettingsView.svelte` | Chat, fournisseurs/modèles, sources, langue, thème et préférences. |
| `src/lib/components/ActivityView.svelte` | Jobs, résultats, attentes/erreurs, annulation et opérations réversibles. |
| `src/App.svelte`, `src/app.css` | Navigation, session de recherche conservée au retour du lecteur, identité et thèmes. |
| `src/lib/api.test.ts`, `i18n.test.ts`, `preview.test.ts` | Tests frontend des contrats, traductions et aperçu. |
| `scripts/native-smoke.py` | Parcours du vrai binaire avec tauri-driver, profil isolé et rapport/capture optionnels. |
| `scripts/third-party-notices.py`, `vendor/licenses/` | Inventaire verrouillé et notices originales vérifiables. |
| `.github/workflows/ci.yml` | Contrôles, paquets, contenu distribué, source correspondante, SHA-256 et artefacts CI. |

## Persistance et intégrité

SQLite utilise WAL, clés étrangères, JSON1, FTS5, tables strictes et requêtes liées. `books` conserve bibliographie, données personnelles, progression et révision ; `book_files` conserve variantes, chemins relatifs, formats, profils, tailles et hash. Le format et la taille affichés viennent de l’original. Les listes sont des JSON validés.

`devices`/`device_books` conservent identités et inventaires ; un ancien chemin seul n’autorise jamais une écriture. `jobs` conserve une enveloppe privée versionnée, tentative, résultat et erreur publique. `operations` conserve les snapshots avant/après du livre et de ses variantes actives. `settings`, `provider_catalogs`, `conversations` et `messages` stockent préférences non secrètes, catalogues et chat.

La recherche combine FTS5 sur titre/auteurs/série/genres/description et recherche littérale sur ISBN, éditeur et notes. Les notes sont recherchables localement et exclues des contextes IA. Les filtres utilisent OR dans une dimension et AND entre dimensions. Livre, FTS, variantes et journal sont modifiés dans une même transaction.

Les originaux sont conservés dans `books/originals/<sha256>.<format>`. Les variantes ont des chemins auteur → série avec indices zéro/fractionnaires et identifiant évitant les collisions. Undo restaure les variantes actives du snapshot précédent, garde les artefacts historiques et refuse une révision récente. Les preuves/propositions IA restent dans les résultats de jobs ; le journal conserve les snapshots, pas un registre supplémentaire de preuve par champ.

## Import et enrichissement

Le dialogue natif sélectionne les fichiers. Le service valide un fichier régulier et ses bornes, identifie le format par extension contrôlée, calcule le hash, déduplique, copie l’original puis catalogue le livre. Un EPUB compatible reçoit une couverture privée si disponible et une variante normalisée avec métadonnées et nom auteur → série. Certains EPUB imparfaits restent catalogués avec avertissements ; une transformation peut être refusée sans bloquer le reste du lot.

Les autres formats sont conservés avec une bibliographie initiale prudente. Ils ne sont pas convertis automatiquement à l’import. Le glisser-déposer n’est pas un point d’entrée implémenté ; la conversion est une action explicite.

Avec enrichissement automatique activé, chaque nouvel import planifie un job durable. Fournisseur/clé/modèle absents : attente configuration ; réseau indisponible : reprises bornées. Les livres restent utilisables. Un import partiel conserve ses compteurs succès/doublons/erreurs. Le chat persiste son véritable conversationId avant génération.

L’enrichissement récupère des sources publiques puis demande une proposition JSON. ISBN, dates, langues, listes, champs nullable, indices et confiance sont validés. L’application automatique exige des preuves réellement récupérées et le seuil réglé ; les changements identitaires nécessitent des domaines indépendants. Sinon, la proposition reste disponible pour revue.

Un job d’import conserve sa révision de départ. Une correction humaine durant une attente de clé ou une requête empêche l’application automatique ; une fiche vérifiée manuellement n’est pas rétrogradée. Seul `LibraryService` applique un patch, avec contrôle de révision du repository. Notes, favoris, évaluations et progression ne sont pas modifiables par le LLM.

Événements : `library:changed`, `devices:changed`, `job:updated`, `chat:delta`, `reader:progress`. Un AtomicBool partage l’annulation avec les travaux bloquants ; les contrôles avant publication et transaction empêchent les fins tardives de ressusciter un job.

## Lecteur et optimisation

Le lecteur choisit une variante EPUB gérée ou l’original EPUB. Un autre format doit avoir une variante EPUB issue d’une conversion explicite. Le backend assainit chaque section : scripts, handlers, formulaires, frames et ressources réseau retirés ; images locales en data URI bornées ; ressources vectorielles incompatibles signalées.

`ReaderView` utilise `iframe srcdoc` avec `sandbox=""` et CSP restrictive, sans origine partagée, scripts ou bridge. Le sommaire imbriqué et les préférences sont disponibles. Section et fragment sont mémorisés ; une position avec fragment ouvre le début du chapitre avec avertissement. Filtres, tri, page et mode grille/table restent conservés après fermeture du lecteur.

Profils : `lossless` recomprime sans changer les ressources ; `balanced` conserve les illustrations avec images limitées à 1200×1600 ; `xteink` utilise 480×800, niveaux de gris et qualité JPEG 75 ; `textOnly` retire les images en préservant texte et légendes accessibles. Les dimensions décrivent le profil, pas tous les modèles Xteink. La suppression sûre des polices met à jour CSS/manifeste/obfuscation ; les ressources impossibles à retirer sans risque restent avec avertissement.

Un dérivé séparé est produit, son texte/spine est comparé et son conteneur rouvert. L’optimisation peut être sélectionnée avant transfert. Aucun original n’est remplacé.

## Conversions autonomes

EPUB/TXT/HTML/FB2 peuvent être convertis vers EPUB/TXT/HTML/FB2/MOBI6. HTML est assaini, ses images locales contrôlées ; TXT conserve Unicode et paragraphes ; FB2 conserve sections et images compatibles. Le rapport signale les pertes de mise en page liées au format cible.

MOBI/KF8/AZW3 non chiffrés sont reconstruits par `mobitool -e`, construit depuis `vendor/libmobi-0.12/` par `src-tauri/build.rs`, puis réinspectés et réencapsulés en EPUB assaini. Arguments structurés, environnement nettoyé, dossier privé, délai et sorties bornées. L’écrivain natif MOBI6 produit PalmDOC UTF-8 non compressé, EXTH et images ; ses fixtures sont validées par le parseur libmobi embarqué.

La matrice annonce les capacités réelles. Aucun export AZW3/KF8. PDF/CBZ sont stockés et filtrables, sans conversion reflow ni lecteur dédié. Les transformations de contenu chiffré sont refusées ; les destinations existantes sont préservées.

## Fournisseurs et Internet

Z.ai, Kimi/Moonshot, MiniMax, OpenAI/Codex, Claude et Mistral utilisent leurs API officielles. Codex désigne OpenAI Responses. Les modèles proviennent d’endpoints/catalogues officiels avec provenance/date ; cache ancien et erreur de rafraîchissement sont explicites. Les clés restent en session ou dans le coffre Linux à la demande, sans fallback en texte clair dans SQLite.

`WebClient` obtient des sources avant les appels LLM pour les six fournisseurs. Pages publiques uniquement en HTTPS sur le port 443 ; DNS/redirections revalidés, plages privées refusées, budgets taille/durée. Le client LAN est distinct. Le choix explicite `webEnabled=false` retire la recherche et son absence reste visible.

Aucune boucle modèle d’appels d’outils web_search/web_fetch. Le chat utilise le service commun, des plans de recherche validés et des résultats locaux réels ; les citations doivent correspondre aux sources obtenues. Contexte borné, sans notes/évaluations/favoris/progression automatiques.

Les valeurs de contrat `localCli` sont réservées, sans capacité disponible. Aucun CLI Codex, Claude ou Calibre invoqué. La racine du stockage est fixée en lecture seule dans les paramètres ; aucune commande applicative de déplacement ou de sauvegarde du profil n’est distribuée.

## Appareils et transports

Les montages Linux USB/SD et enfants MTP déjà accessibles sont redécouverts et revalidés. Liens symboliques, montages système, lecture seule et volumes ambigus ne donnent pas de capacité d’écriture. Présence rapprochée par contenu et filtrable pour les seuls appareils actuellement connectés. Le scan périodique indexe les nouvelles connexions sans copie automatique.

L’inventaire complet reste en base. Le résultat UI d’un job est limité à 500 lignes et 768 KiB, avec total/truncated/avertissement ; cette limite ne tronque pas la présence enregistrée.

CrossPoint utilise son serveur HTTP LAN port 80 : inventaire, upload temporaire UUID, relecture hash puis rename sans écrasement. WebSocket et découverte UDP du firmware ne sont pas des fonctions de découverte de cette version. Le firmware étudié n’offre pas d’authentification HTTP ; réseau local de confiance requis.

Calibre sans fil est un serveur TCP smart-device autonome démarré explicitement sur l’IP locale de l’ordinateur, port 9090. L’utilisateur configure cette adresse/ce port dans KOReader ; aucune découverte UDP. L’appareil devient connecté après handshake réel, capacités/identité et challenge facultatif. Ce protocole historique est non chiffré. Copies dans des noms réservés à Library Manager, relecture SHA-256 et nettoyage limité à ces fichiers. Une rupture peut laisser une copie partielle : intention conservée et avertissement, sans fausse présence. Voir [DEVICE_PROTOCOL.md](DEVICE_PROTOCOL.md).

## Contrôles et distribution

Les tests Rust utilisent archives synthétiques, bases temporaires, faux montages, clients HTTP/TCP locaux et fixtures publiques libmobi. Les tests frontend sont `api.test.ts`, `i18n.test.ts`, `preview.test.ts`. `scripts/native-smoke.py` exerce le bridge natif avec profil isolé ; il ne remplace pas un essai physique USB/KOReader/CrossPoint.

La CI unique `.github/workflows/ci.yml` vérifie frontend, format Rust, core et Clippy workspace, puis construit DEB/RPM/AppImage sur Ubuntu 22.04 avec deux jobs Cargo. Elle contrôle les licences/sidecar dans DEB/RPM, génère source correspondante et SHA-256 puis téléverse des artefacts de revue. Elle ne publie pas une GitHub Release.

`scripts/third-party-notices.py` génère/vérifie les notices depuis dépendances verrouillées et textes originaux, avec --self-test/--check. Les paquets incluent GPL Library Manager, LGPL libmobi et notices. Node/pnpm/Rust et SDK Linux sont nécessaires à la construction ; les utilisateurs des paquets n’installent pas ces outils ni Calibre. Voir [BUILD.md](BUILD.md).

Compilation, artefact CI, parcours natif et essai physique sont des preuves distinctes, consignées dans [MASTER_PLAN.md](MASTER_PLAN.md). Un build ne démontre pas à lui seul le fonctionnement sur une liseuse.
