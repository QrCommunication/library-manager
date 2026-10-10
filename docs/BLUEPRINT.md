# Architecture réalisée de Library Manager

État du 10 octobre 2026 : **0.2.1 en préparation**, non encore attestée comme livraison native sur toutes les plateformes. [IPC.md](IPC.md) décrit les **37 commandes effectivement enregistrées** dans `src-tauri/src/lib.rs` et les cinq événements. [RELEASE_0.2.1_PLAN.md](RELEASE_0.2.1_PLAN.md) suit cette livraison ; [QUALITY.md](QUALITY.md) et [MASTER_PLAN.md](MASTER_PLAN.md) conservent les preuves et leur historique. Ce document décrit l’architecture source, sans transformer un build ou une fixture en preuve de fonctionnement sur une liseuse.

## Application et frontières

Library Manager est une application desktop autonome sous GPL-3.0-only : Tauri 2, Svelte 5, TypeScript stricte et SQLite embarqué. Les cibles de 0.2.1 sont Linux, Windows x64 et macOS Intel/ARM. Le moteur Rust `library-core` ne dépend pas de Tauri ou d’une WebView. Calibre n’est ni installé ni exécuté ; le moteur MOBI est distribué avec l’application. La disponibilité et la validation de chaque paquet restent des résultats à établir séparément.

`src-tauri/src/main.rs` appelle `library_manager_lib::run()`. La bibliothèque desktop est produite uniquement en `rlib`, puis liée au binaire `library-manager`. La distribution Linux n’expose aucune bibliothèque native mobile et ne produit plus les sorties ABI staticlib/cdylib. Les configurations DEB/RPM/AppImage conservent ce binaire et le sidecar. Voir la [structure Tauri](https://v2.tauri.app/start/project-structure/) et les [types de liaison Rust](https://doc.rust-lang.org/reference/linkage.html).

Les commandes IPC font l’entrée/sortie et délèguent aux services. `LibraryManager` compose les services et exécute les jobs ; `BookRepository` possède les transactions métier. Les contrôleurs n’exécutent pas de SQL. Fichiers/ZIP/images utilisent `spawn_blocking`, le réseau reste async ; concurrence réglable de 1 à 4.

Le Manager détient un verrou privé `runtime.lock`, ouvert par la façade de fichiers sûre et conservé par tous ses clones. L’identité, les droits privés et le verrou natif sont contrôlés avant reprise des jobs ; une deuxième ouverture produit `profileInUse`. Le plugin d’instance unique ramène la fenêtre quand le mécanisme de session est disponible ; les erreurs d’initialisation restent affichables. La cartographie autorisée se trouve dans `~/.Codex/projects/library-manager/memory/`.

## Carte des fichiers présents

| Fichier | Responsabilité |
| --- | --- |
| `Cargo.toml`, `rust-toolchain.toml` | Workspace core/desktop, versions partagées, Rust 1.99 et profil de livraison. |
| `crates/library-core/src/lib.rs` | Façade publique du moteur et `deny(unsafe_code)` global. |
| `crates/library-core/src/models.rs`, `error.rs` | Contrats Serde, filtres, patches nullable et erreurs publiques sans secrets. |
| `crates/library-core/migrations/001_initial.sql`, `database.rs` | Schéma versionné, connexions privées et migrations SQLite. |
| `crates/library-core/src/book_repository.rs` | Livres, variantes, recherche, facettes, présence, révisions, progression et journal transactionnel. |
| `crates/library-core/src/storage.rs` | Originaux immuables, SHA-256, capacités de chemin et publication atomique sans écrasement. |
| `crates/library-core/src/secure_fs.rs`, `secure_fs/unix.rs`, `secure_fs/windows.rs` | Capacités de répertoire, ouvertures relatives sans liens, identités/snapshots, publication exclusive, permissions et barrières de durabilité natives. |
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
| `crates/library-core/src/providers.rs` | Six API, catalogues dynamiques, secrets privés et diagnostics fournisseur filtrés. |
| `crates/library-core/src/web.rs` | Recherche et pages publiques, protection SSRF et budgets. |
| `crates/library-core/src/enrichment.rs` | Propositions bibliographiques avec preuves ; aucune mutation directe. |
| `crates/library-core/src/chat.rs`, `chat_tools.rs` | Conversations, plans de recherche, boucle de sept outils typés, périmètre autorisé et reçus de reprise. |
| `crates/library-core/src/jobs.rs` | File durable, tentatives, reprise, attente, résultats et tokens d’annulation. |
| `src-tauri/src/lib.rs`, `main.rs`, `commands.rs` | Shell, état d’initialisation, plugins et pont IPC/événements. |
| `src-tauri/build.rs`, `vendor/libmobi-0.12/` | Construction native du sidecar depuis les sources versionnées. |
| `src-tauri/tauri.conf.json`, `capabilities/default.json` | Fenêtre, CSP, dialogue natif, bundles et ressources ; aucune permission shell arbitraire. |
| `src/lib/contracts.ts`, `api.ts`, `preview.ts` | IPC typée et aperçu navigateur explicitement identifié avec fixtures synthétiques. |
| `src/lib/i18n.ts`, `locales/fr.json`, `locales/en.json` | Langue système, sélection, fallback et découverte des dictionnaires. |
| `src/lib/components/LibraryView.svelte` | Grille/table, recherche, tris, facettes, pagination, sélection et présence. |
| `src/lib/components/BookPanel.svelte` | Fiche, propositions IA, variantes et actions. |
| `src/lib/components/ReaderView.svelte` | Sommaire, préférences, progression et iframe opaque srcdoc. |
| `src/lib/components/DevicesView.svelte` | Appareils, connexion manuelle, inventaire et transferts. |
| `src/lib/components/ChatView.svelte`, `SettingsView.svelte` | Chat, fournisseurs/modèles, sources, langue, thème et préférences. |
| `src/lib/components/ActivityView.svelte` | Jobs, résultats, attentes/erreurs, annulation et opérations réversibles. |
| `src/lib/request-scheduler.ts` | Requêtes sérialisées, recherche différée et rafraîchissements regroupés sans invalider une réponse courante valide. |
| `src/lib/selection-capabilities.ts`, `components/SelectionActions.svelte` | Limite commune de 200 livres, disponibilité réelle des métadonnées et barre d’actions partagée. |
| `src/lib/components/DeviceTransferDialog.svelte`, `RemoveBooksDialog.svelte` | Sélection de destination et confirmation explicite ; retrait du catalogue avec révisions et reçu idempotent. |
| `src/App.svelte`, `src/app.css` | Navigation, session de recherche conservée au retour du lecteur, identité et thèmes. |
| `src/lib/api.test.ts`, `i18n.test.ts`, `preview.test.ts` | Tests frontend des contrats, traductions et aperçu. |
| `scripts/native-smoke.py`, `native-assistant-smoke.py` | Parcours du vrai binaire avec tauri-driver, profils isolés, droits/outils et actions du catalogue. |
| `scripts/third-party-notices.py`, `vendor/licenses/` | Inventaire verrouillé et notices originales vérifiables. |
| `.github/workflows/ci.yml` | Contrôles, paquets, contenu distribué, source correspondante, SHA-256 et artefacts CI. |
| `.github/workflows/desktop.yml` | Tests et builds natifs Windows/macOS, sidecar, provenance, statut de signature et vérifications Apple conditionnelles. |

## Persistance et intégrité

SQLite utilise WAL, clés étrangères, JSON1, FTS5, tables strictes et requêtes liées. `books` conserve bibliographie, données personnelles, progression et révision ; `book_files` conserve variantes, chemins relatifs, formats, profils, tailles et hash. Le format et la taille affichés viennent de l’original. Les listes sont des JSON validés.

`devices`/`device_books` conservent identités et inventaires ; un ancien chemin seul n’autorise jamais une écriture. `jobs` conserve une enveloppe privée versionnée, tentative, résultat et erreur publique. `operations` conserve les snapshots avant/après du livre et de ses variantes actives. `settings`, `provider_catalogs`, `conversations` et `messages` stockent préférences non secrètes, catalogues et chat.

La recherche combine FTS5 sur titre/auteurs/série/genres/description et recherche littérale sur ISBN, éditeur et notes. Les notes sont recherchables localement et exclues des contextes IA. Les filtres utilisent OR dans une dimension et AND entre dimensions. Livre, FTS, variantes et journal sont modifiés dans une même transaction.

Les originaux sont conservés dans `books/originals/<sha256>.<format>`. Les variantes ont des chemins auteur → série avec indices zéro/fractionnaires et identifiant évitant les collisions. Undo restaure les variantes actives du snapshot précédent, garde les artefacts historiques et refuse une révision récente. Les preuves/propositions IA restent dans les résultats de jobs ; le journal conserve les snapshots, pas un registre supplémentaire de preuve par champ.

`secure_fs` représente l’autorité par un handle de répertoire, plutôt que par un chemin déjà vérifié puis rouvert librement. Les enfants sont ouverts relativement à ce handle ; les symlinks et les reparse points/junctions Windows sont refusés. Les snapshots enregistrent identité, taille, modification et changement réel. La publication ne remplace jamais une cible existante ; le nettoyage vérifie l’identité avant de retirer un fichier. L’adaptateur Unix utilise les primitives du système via `rustix`. Windows ajoute des handles natifs, ACL owner-only et appels FFI documentés : l’exemption `allow(unsafe_code)` est limitée à cet adaptateur, avec justifications de sûreté, et ne s’étend pas aux services. Une garantie de durabilité ou d’identité indisponible produit une erreur explicite. Les tests compilés ou exécutés sous Linux ne prouvent pas ces garanties sur un système de fichiers Windows.

`books_remove` retire jusqu’à 200 livres dans une transaction contrôlée par leurs révisions. L’opération `catalogueRemove` conserve un reçu lié au `requestId` : répéter les mêmes arguments retourne le résultat enregistré ; changer les arguments sous cet identifiant est refusé. Originaux, variantes et copies sur les appareils sont conservés. Undo restaure le catalogue après contrôle des fichiers et collisions ; ce parcours ne supprime pas physiquement les fichiers d’une liseuse.

## Import et enrichissement

Le dialogue natif sélectionne les fichiers. Le service valide un fichier régulier et ses bornes, identifie le format par extension contrôlée, calcule le hash, déduplique, copie l’original puis catalogue le livre. Un EPUB compatible reçoit une couverture privée si disponible et une variante normalisée avec métadonnées et nom auteur → série. Certains EPUB imparfaits restent catalogués avec avertissements ; une transformation peut être refusée sans bloquer le reste du lot.

Les autres formats sont conservés avec une bibliographie initiale prudente. Ils ne sont pas convertis automatiquement à l’import. Le glisser-déposer n’est pas un point d’entrée implémenté ; la conversion est une action explicite.

Avec enrichissement automatique activé, chaque nouvel import planifie un job durable. Fournisseur/clé/modèle absents : attente configuration ; réseau indisponible : reprises bornées. Les livres restent utilisables. Un import partiel conserve ses compteurs succès/doublons/erreurs. Le chat persiste son véritable conversationId avant génération.

L’enrichissement récupère des sources publiques puis demande une proposition JSON. ISBN, dates, langues, listes, champs nullable, indices et confiance sont validés. L’application automatique exige des preuves réellement récupérées et le seuil réglé ; les changements identitaires nécessitent des domaines indépendants. Sinon, la proposition reste disponible pour revue.

L’inspection lit le fichier réel après contrôle de taille et de SHA-256. Elle distingue l’original de l’EPUB inspecté, qui peut être un dérivé, et transmet des extraits bornés de titre/copyright/édition et de chapitre, au plus 12 000 caractères. Elle ne prétend pas lire intégralement chaque livre. Patch et preuves suivent la même normalisation de champs ; un encodage JSON supplémentaire unique peut être décodé strictement, sans supprimer librement des guillemets ni accepter un ISBN au checksum incorrect. Voir [ENRICHMENT.md](ENRICHMENT.md).

Un job d’import conserve sa révision de départ. Une correction humaine durant une attente de clé ou une requête empêche l’application automatique ; une fiche vérifiée manuellement n’est pas rétrogradée. Seul `LibraryService` applique un patch, avec contrôle de révision du repository. Notes, favoris, évaluations et progression ne sont pas modifiables par le LLM.

Les analyses `assistantReview` restent toujours manuelles. Les analyses manuelles et celles issues d’import suivent leurs politiques de preuve, de confiance et de révision décrites dans [METADATA_POLICY.md](METADATA_POLICY.md). Le résultat d’analyse peut porter `{ proposal, review }` ; la revue passe durablement de `pending` à `applied`, `dismissed` ou `obsolete`. `book_review` vérifie le livre, le job et la révision ; un patch vide valide aussi une proposition déjà identique. Les modifications personnelles réancrent une revue compatible, alors qu’une correction bibliographique peut la résoudre ou la rendre obsolète.

La barre commune reste accessible depuis les vues de bibliothèque, les livres locaux sélectionnés dans Appareils et l’Assistant. Analyse grisée si le fournisseur choisi n’est pas reconnu, configuré et prêt, ou si le modèle est vide ; transferts soumis à un appareil connecté accessible en écriture ; retrait soumis à confirmation. La fiche et les listes n’ouvrent que les propositions encore éligibles à la révision courante. Le scheduler sépare changements de recherche et événements de rafraîchissement : un flux d’événements ne maintient plus indéfiniment la bibliothèque à zéro pendant le chargement.

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

Z.ai, Kimi/Moonshot, MiniMax, OpenAI/Codex, Claude et Mistral utilisent leurs API officielles. Codex désigne OpenAI Responses. Les modèles proviennent d’endpoints/catalogues officiels avec provenance/date ; cache ancien et erreur de rafraîchissement sont explicites. Les clés restent privées, en session ou dans le coffre disponible à la demande, sans fallback en texte clair dans SQLite. Les diagnostics d’authentification, requête, réponse incomplète/malformée ou métadonnées invalides utilisent des clés publiques autorisées et traduites ; les réponses brutes et détails inconnus ne sont pas affichés comme erreurs.

`WebClient` obtient des sources avant les appels LLM pour les six fournisseurs. Pages publiques uniquement en HTTPS sur le port 443 ; DNS/redirections revalidés, plages privées refusées, budgets taille/durée. Le client LAN est distinct. Le choix explicite `webEnabled=false` retire la recherche et son absence reste visible.

Le chat prépare un plan de recherche en lecture seule puis exécute une boucle JSON stricte de sept outils : `librarySearch`, `bookInspect`, `webSearch`, `webFetch`, `updateMetadata`, `organizeBooks`, `verifyMetadata`. Le contexte initial contient au plus 32 fiches sélectionnées, avec le périmètre complet borné à 200 IDs ; JSON limité à 384 KiB, huit étapes et une seule réparation de format supplémentaire. Les citations doivent correspondre aux sources effectivement obtenues. Notes, évaluations et progression ne sont pas envoyées automatiquement.

L’autorisation de modifier/organiser est désactivée par défaut et s’applique à **une seule question**, pour ses seuls IDs sélectionnés. Le backend persiste ce périmètre avant exécution ; le modèle, les pages web et le texte du livre ne peuvent l’élargir. Une correction exige inspection, preuves, hash des fichiers et révision compatibles. Les reçus durables réutilisent un résultat connu sans seconde mutation ; un résultat interrompu incertain bloque sa réexécution automatique. L’annulation arrête les étapes suivantes et attend les mutations déjà engagées. Ces outils n’accordent ni suppression, ni transfert, ni SQL, ni shell, ni chemin libre. Voir [CHAT.md](CHAT.md).

Les valeurs de contrat `localCli` sont réservées, sans capacité disponible. Aucun CLI Codex, Claude ou Calibre invoqué. La racine du stockage est fixée en lecture seule dans les paramètres ; aucune commande applicative de déplacement ou de sauvegarde du profil n’est distribuée.

## Appareils et transports

Les montages Linux USB/SD et enfants MTP déjà accessibles sont redécouverts et revalidés. Le code 0.2.1 ajoute des probes en lecture seule : PowerShell système pour les disques USB/volumes amovibles Windows hors boot/system ; `diskutil` pour les volumes physiques externes macOS. Sortie limitée à 4 MiB, 128 volumes et délai global de dix secondes ; identifiants validés avant les appels suivants. Aucun montage automatique ni probe d’écriture. Les tests JSON/plist et Linux passent ; les commandes et garanties de fichiers doivent encore être attestées sur les runners natifs, puis sur les cartes concernées.

Liens, reparse points, montages système, lecture seule et volumes ambigus ne donnent pas de capacité d’écriture. Inventaire, hash et inspection EPUB utilisent les fichiers ouverts relativement à la capacité du répertoire, avec contrôle des snapshots avant/après. Présence rapprochée par contenu et filtrable pour les seuls appareils actuellement connectés. Le scan périodique indexe les nouvelles connexions sans copie automatique. Les identités stables fournies par le système de fichiers sont une exigence : un format qui ne les fournit pas doit être refusé explicitement, sans prétendre à une compatibilité physique non testée.

L’inventaire complet reste en base. Le résultat UI d’un job est limité à 500 lignes et 768 KiB, avec total/truncated/avertissement ; cette limite ne tronque pas la présence enregistrée.

CrossPoint utilise son serveur HTTP LAN port 80 : inventaire, upload temporaire UUID, relecture hash puis rename sans écrasement. WebSocket et découverte UDP du firmware ne sont pas des fonctions de découverte de cette version. Le firmware étudié n’offre pas d’authentification HTTP ; réseau local de confiance requis.

Calibre sans fil est un serveur TCP smart-device autonome démarré explicitement sur l’IP locale de l’ordinateur, port 9090. L’utilisateur configure cette adresse/ce port dans KOReader ; aucune découverte UDP. L’appareil devient connecté après handshake réel, capacités/identité et challenge facultatif. Ce protocole historique est non chiffré. Copies dans des noms réservés à Library Manager, relecture SHA-256 et nettoyage limité à ces fichiers. Une rupture peut laisser une copie partielle : intention conservée et avertissement, sans fausse présence. Voir [DEVICE_PROTOCOL.md](DEVICE_PROTOCOL.md).

## Contrôles et distribution

Les tests Rust utilisent archives synthétiques, bases temporaires, faux montages, clients HTTP/TCP locaux et fixtures publiques libmobi. Les tests frontend couvrent contrats, traductions, aperçu, scheduler et disponibilité des actions. `scripts/native-smoke.py` et `native-assistant-smoke.py` exercent le bridge natif avec profil isolé ; ils ne remplacent pas un essai physique USB/KOReader/CrossPoint. Les scénarios de revue/retrait vérifient persistance, répétition idempotente et conservation des octets.

Le workflow Linux `.github/workflows/ci.yml` contrôle frontend, Rust et contenu distribué, construit DEB/RPM/AppImage, puis génère source correspondante, notices et SHA-256. Le workflow distinct `.github/workflows/desktop.yml` construit et teste nativement Windows x64 MSVC sur `windows-2025`, macOS ARM sur `macos-26` et macOS Intel sur `macos-26-intel`. Les plateformes exécutent les tests workspace et Clippy avant leurs paquets. Windows produit MSI/NSIS ; macOS produit application et DMG avec cible minimale macOS 13. Les sidecars sont construits pour leur architecture puis exécutés et contrôlés dans le processus de collecte.

Les artefacts desktop incluent `package-report.json`, version/commit/cible/architecture, statut de signature et SHA-256. Le workflow enregistre explicitement que GUI native et Windows 11 n’ont pas été testés par cette collecte. Windows est actuellement empaqueté unsigned. Sur macOS, aucune configuration Apple signifie un paquet unsigned explicitement identifié ; une configuration partielle bloque le build sans repli silencieux. Une configuration complète prépare un keychain temporaire, signe application/sidecar/DMG, soumet la notarisation puis vérifie codesign, équipe, stapler et Gatekeeper. Ces étapes configurées ne prouvent ni disponibilité des secrets ni notarisation accomplie : leurs rapports et la réussite du run exact sont nécessaires. Les secrets temporaires sont nettoyés dans une étape `always()`.

Les workflows déposent des artefacts de revue. Publication de Release, téléchargements publics anonymes, hash des fichiers publiés et essais du paquet exact constituent une étape de livraison distincte. À cet état, la compilation croisée de l’adaptateur Windows et les tests locaux ne valent pas validation native complète de 0.2.1.

`scripts/third-party-notices.py` génère/vérifie les notices depuis dépendances verrouillées et textes originaux, avec --self-test/--check. Les paquets incluent GPL Library Manager, LGPL libmobi et notices. Node/pnpm/Rust et SDK Linux sont nécessaires à la construction ; les utilisateurs des paquets n’installent pas ces outils ni Calibre. Voir [BUILD.md](BUILD.md).

Compilation, artefact CI, parcours natif et essai physique sont des preuves distinctes, consignées dans [MASTER_PLAN.md](MASTER_PLAN.md). Un build ne démontre pas à lui seul le fonctionnement sur une liseuse.
