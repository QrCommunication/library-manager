# Architecture de Library Manager

Décision du 9 octobre 2026. Voir [IPC.md](IPC.md) pour le contrat de l’application et [MASTER_PLAN.md](MASTER_PLAN.md) pour l’état réel de livraison.

## Principes retenus

Library Manager est une application Linux autonome, distribuée sous GPL-3.0. Tauri 2 héberge une interface Svelte 5 et TypeScript stricte. Un crate Rust `library-core` contient les opérations sur les livres, SQLite, les appareils et les fournisseurs IA. Le noyau n’importe aucune API Tauri : `cargo test -p library-core` vérifie les fonctions métier sans WebView.

Le dossier de départ était vide : aucun modèle, service, schéma ou convention de code antérieur n’est à prolonger. Les règles globales AGENTS.md imposent la cartographie persistante, la séparation des responsabilités, une analyse d’impact et une validation de non-régression. La cartographie autorisée est dans `~/.Codex/projects/library-manager/memory/`.

SQLite est compilé avec l’application, avec WAL, clés étrangères, JSON1, FTS5 et un délai de contention de cinq secondes. Les requêtes utilisent des paramètres liés. Les listes d’auteurs, genres et étiquettes sont des tableaux JSON validés ; des requêtes `json_each` construisent les facettes. Cette structure évite des tables d’administration inutiles tout en permettant des filtres combinés. Les fichiers ont leurs propres lignes : un même livre peut avoir un original, un EPUB corrigé et plusieurs variantes optimisées.

Les originaux importés sont copiés dans un stockage privé et restent immuables. Les nouveaux fichiers sont construits dans un répertoire temporaire de même volume, validés, puis renommés atomiquement. Les chemins de fichiers gérés sont relatifs dans SQLite. Aucun contenu privé, jeton ou historique de lecture ne fait partie du dépôt public.

## Organisation des fichiers

| Fichier | Responsabilité et dépendances |
| --- | --- |
| `Cargo.toml` | Workspace `crates/library-core` et `src-tauri`, versions et profil de livraison. |
| `crates/library-core/Cargo.toml` | SQLite bundled, Tokio, Serde, XML, ZIP, images, HTTP et erreurs structurées ; aucun Tauri. |
| `crates/library-core/src/lib.rs` | Exports, `LibraryManager` et assemblage des services ; pas de logique de contrôleur. |
| `crates/library-core/src/models.rs` | Types du contrat, filtres, appareils, jobs, propositions IA, réglages et profils. |
| `crates/library-core/src/error.rs` | Codes stables et erreurs internes ; conversion en erreurs publiques sans secret. |
| `crates/library-core/migrations/001_initial.sql` | Schéma versionné décrit ci-dessous. |
| `crates/library-core/src/database.rs` | Connexions, migrations, transactions et accès aux lignes persistantes. |
| `crates/library-core/src/storage.rs` | Stockage privé, SHA-256, chemins bornés, écriture atomique, journal et annulation. |
| `crates/library-core/src/library.rs` | Imports idempotents, facettes, filtrage, mises à jour et révisions. |
| `crates/library-core/src/epub.rs` | Inspection ZIP/XML, métadonnées EPUB 2/3, manifeste, spine, couvertures et réécriture OPF. |
| `crates/library-core/src/reader.rs` | Sections assainies, ressources autorisées, sommaire et positions de lecture. |
| `crates/library-core/src/optimizer.rs` | Profils, images, compression et contrôle d’intégrité des dérivés. |
| `crates/library-core/src/conversion.rs` | Matrice de conversion, génération EPUB/HTML/TXT/FB2 et invocation bornée du moteur MOBI embarqué. |
| `crates/library-core/src/mobi.rs` | Export MOBI 6 simple lorsqu’il est proposé, sans DRM ni promesse d’export KF8. |
| `crates/library-core/src/devices.rs` | Montages Linux, identification USB/SD, inventaires et rapprochement des livres. |
| `crates/library-core/src/transfer.rs` | Plans auteur → série, espace libre, copies vérifiées et annulation conditionnelle. |
| `crates/library-core/src/crosspoint.rs` | Client LAN HTTP/WebSocket CrossPoint, authentification et inventaire distant. |
| `crates/library-core/src/calibre_wireless.rs` | Adaptateur du protocole appareil sans fil, distinct du serveur HTTP CrossPoint. |
| `crates/library-core/src/providers.rs` | Six fournisseurs configurables, catalogue de modèles et adaptateurs de messages. |
| `crates/library-core/src/web.rs` | Recherche bibliographique et récupération de pages publiques avec limites et protection SSRF. |
| `crates/library-core/src/enrichment.rs` | Sources, proposition structurée, confiance, validation et application des métadonnées. |
| `crates/library-core/src/chat.rs` | Conversations, contexte des livres, appels aux fournisseurs et sources affichables. |
| `crates/library-core/src/jobs.rs` | File durable, reprise, progression, annulation et limitation de concurrence. |
| `src-tauri/src/lib.rs` | Configuration Tauri, état partagé, commandes, événements et ressources du lecteur. |
| `src-tauri/src/main.rs` | Entrée native minimale. |
| `src-tauri/src/commands.rs` | Validation des entrées IPC et délégation au noyau. |
| `src-tauri/tauri.conf.json` | Identifiant, CSP, icônes, ressources embarquées et bundles DEB/RPM/AppImage. |
| `src-tauri/capabilities/default.json` | Fenêtre principale et dialogue natif ; aucune permission shell arbitraire. |
| `src/lib/contracts.ts` | Types de `IPC.md` et unions des commandes/événements. |
| `src/lib/api.ts` | Façade typée `invoke`/`listen` ; aperçu navigateur explicitement identifié. |
| `src/lib/i18n.ts` | Langue système normalisée, choix utilisateur et fallback de clés. |
| `src/locales/fr.json`, `src/locales/en.json` | Dictionnaires complets de même structure. |
| `src/lib/components/LibraryView.svelte` | Grille et table, recherche, tri, facettes, sélection et badges appareil. |
| `src/lib/components/BookDetails.svelte` | Métadonnées, provenance, révisions, variantes et actions sur le livre. |
| `src/lib/components/ReaderView.svelte` | Sommaire, réglages de lecture, progression et iframe de contenu isolée. |
| `src/lib/components/DevicesView.svelte` | Appareils connectés, profils, inventaires, surbrillance et transfert. |
| `src/lib/components/ChatView.svelte` | Conversation, contexte sélectionné, fournisseur, modèle et citations. |
| `src/lib/components/SettingsView.svelte` | Langue, stockage, fournisseurs, secrets masqués et profils. |
| `src/App.svelte` | Shell immédiatement visible et navigation ; chargements et erreurs isolés par panneau. |
| `src/app.css` | Identité visuelle, contraste, thèmes, focus clavier et mouvement réduit. |
| `scripts/build-mobi-engine.sh` | Construction reproductible du moteur embarqué depuis une source vérifiée. |
| `scripts/check-locales.mjs` | Parité des clés et validité des paramètres de traduction. |
| `.github/workflows/ci.yml` | Format, Clippy, tests Rust, tests UI, types et build. |
| `.github/workflows/release.yml` | Construction des paquets, sommes SHA-256 et publication de versions. |

Les fichiers de service restent focalisés. Si un module dépasse son rôle, la subdivision se fait par responsabilité réelle, sans imposer une interface abstraite à chaque fonction. Les adaptateurs sont justifiés pour les six fournisseurs IA et les trois transports d’appareils.

## Schéma SQLite

`books` contient `id`, `title`, `authors_json`, `author_sort`, `series`, `series_index`, `genres_json`, `tags_json`, `language`, `description`, `isbn`, `publisher`, `published`, `cover_relative_path`, `read_status`, `reading_progress`, `reader_location`, `favorite`, `rating`, `notes`, `metadata_status`, `metadata_confidence`, `revision`, `added_at`, `updated_at`. Les indices portent sur auteur/titre, série/numéro, langue et statut. Les tableaux JSON ont un `CHECK(json_valid(...))`. La progression est dans `[0,1]`, la note dans `[0,5]` et la confiance dans `[0,1]`.

`book_files` contient `id`, `book_id`, `format`, `variant`, `profile`, `relative_path`, `sha256`, `size_bytes`, `created_at`. `relative_path` est unique ; la déduplication d’un original se fait par SHA-256. Les formats et tailles affichés dans `Book` sont dérivés du fichier principal, sans colonne redondante dans `books`.

`devices` contient `id`, `label`, `transport`, `profile`, `mount_identity`, `last_seen_at`. Une racine de montage est redécouverte à chaque connexion : un ancien chemin seul n’autorise jamais une écriture. `device_books` contient `device_id`, `relative_path`, `book_id` nullable, `sha256`, `title`, `authors_json`, `format`, `size_bytes`, `last_seen_at`, avec clé composée appareil/chemin. Le lien au livre est supprimé avec `SET NULL` ; les informations d’un livre distant restent disponibles.

`jobs` contient `id`, `kind`, `status`, `progress`, `payload_json`, `result_json`, `error_json`, `created_at`, `updated_at`. `operations` contient `id`, `kind`, `status`, `before_json`, `after_json`, `backup_relative_path`, `created_at`. `settings` stocke les réglages non secrets. `provider_catalogs` conserve modèles, source, date et dernière erreur. `conversations` et `messages` conservent le chat et ses sources. La provenance des corrections est conservée dans le journal de l’opération, avec ancienne valeur, nouvelle valeur, source et confiance par champ.

`books_search` est une table FTS5 de titre, auteurs, série, genres et description, avec identifiant de livre non indexé. La transaction d’écriture met à jour livre, fichiers, recherche et journal ensemble. Une requête utilisateur n’est jamais interprétée comme du SQL ; les tokens FTS sont échappés.

## Flux métier

### Import et enrichissement

Dialogue/drag-and-drop → validation du fichier régulier et de ses limites → détection du format par signature → SHA-256 et déduplication → copie immuable → conversion interne vers EPUB si nécessaire → inspection OPF/spine/couverture → enregistrement → job d’enrichissement durable → sources bibliographiques publiques → fournisseur choisi → proposition JSON validée → application sûre ou état `needsReview` → EPUB dérivé et chemin auteur → série → événement de bibliothèque.

Un fournisseur non configuré ou un réseau indisponible laisse le livre utilisable et le job `waitingForConfiguration` ou `waitingForNetwork`. La bibliothèque ne fabrique pas des informations vérifiées. Chaque ajout déclenche effectivement le pipeline ; un échec n’empêche pas les autres imports.

Les ISBN et rôles auteur/contributeur sont distingués. Un numéro décimal permet préquelles et nouvelles intercalées. Une édition scindée, une intégrale et une traduction sont des éditions du même univers : le nom de série ne dépend pas du découpage éditorial. Une proposition doit justifier une fusion de série et ne supprime jamais un fichier supposé doublon sans opération visible.

### Lecteur

Identifiant de livre → EPUB géré → sommaire et section → XML/HTML assaini → ressources locales résolues par identifiants opaques → iframe `sandbox` sans scripts ni accès au bridge → position persistée. La CSP bloque scripts, formulaires, frames supplémentaires et connexions distantes. Les liens externes sont des actions explicites de l’utilisateur, traitées par l’application. Le lecteur possède un état de chargement et d’erreur qui ne masque jamais le shell.

### Optimisation et transfert

Livres sélectionnés + appareil identifié + profil → prévision des tailles et destinations → variante optimisée séparée → ZIP/OPF/spine et texte contrôlés → nouvelle vérification du montage et de l’espace libre → copie temporaire sur le même volume → SHA-256 → renommage final → journal et index de présence → événement.

Le profil `lossless` recomprime sans perte. `balanced` réduit les images trop grandes tout en gardant toutes les illustrations. `xteink` vise les petites liseuses avec images adaptées et réglages EPUB simples. Une variante sans images ou sans polices demande une sélection explicite et porte un nom qui décrit cette perte. Les caches de progression, polices et réglages `.crosspoint` ne sont pas effacés. La mise à jour d’un fichier existant conserve une sauvegarde et ne change pas son chemin sans nécessité.

Un appareil débranché annule proprement la copie en cours. L’annulation d’un transfert restaure uniquement les chemins dont le hash correspond encore au journal ; elle refuse d’écraser une modification extérieure.

### IA et Internet

Les connecteurs Z.ai, Kimi/Moonshot, MiniMax, OpenAI/Codex, Claude et Mistral implémentent modèles et messages. La découverte à la volée conserve sa provenance ; un catalogue officiel téléchargé et un endpoint authentifié sont distingués. Un échec montre l’erreur et la date du cache, pas une fausse liste présentée comme actuelle.

L’accès Internet appartient au moteur commun. L’enrichissement récupère des sources avant l’appel au modèle, quel que soit le fournisseur. Les modèles capables d’appels d’outils reçoivent aussi `web_search` et `web_fetch`. Les outils sont bornés, publics et sans accès aux fichiers privés. Les APIs autonomes sont le chemin principal ; une connexion à un abonnement via un CLI déjà présent est un mode optionnel identifié et ne devient jamais une dépendance d’installation.

Les requêtes web suivent seulement des URLs HTTP(S) publiques après validation DNS et à chaque redirection, limitent taille, durée et nombre de réponses, et ignorent les instructions trouvées dans un livre ou une page. Le client LAN d’appareil est séparé : seule une adresse d’appareil configurée peut être privée.

## Conversions autonomes

EPUB est le format pivot. TXT, HTML et FB2 sont inspectés et empaquetés nativement. EPUB peut être exporté vers TXT, HTML et FB2 en conservant les chapitres, avec avertissement explicite lorsque le format cible perd de la mise en page.

Pour MOBI et KF8/AZW3 non chiffrés, embarquer `mobitool` construit depuis libmobi v0.12 LGPL-3.0. Il sait reconstruire un EPUB avec `-e`. Configuration recommandée : `--disable-shared --enable-static --enable-tools-static --enable-xmlwriter --with-libxml2=no --with-zlib=no --disable-encryption`. Le source et sa licence sont inclus ou accessibles depuis la distribution ; le build vérifie la source et le binaire n’a pas besoin d’une installation Calibre. Le sous-processus reçoit uniquement des arguments structurés, dans un dossier temporaire privé, avec délai, sortie bornée et contrôle du fichier généré. Le résultat de `mobitool -e` est réinspecté : l’outil amont décrit son EPUB comme une reconstruction nécessitant validation.

Un export MOBI depuis EPUB requiert un écrivain dédié : MOBI 6, UTF-8, enregistrements PalmDOC non compressés, EXTH et images compatibles. Il doit être vérifié par le lecteur libmobi et des fixtures publiques avant d’apparaître dans la matrice des capacités. Libmobi ne fournit pas un générateur complet KF8 ; ne pas annoncer un export AZW3. Les DRM sont signalés et refusés, sans tentative de contournement. Le PDF garde sa nature de document fixe ; aucune conversion fidèle de PDF en EPUB n’est promise.

Sources primaires : [libmobi](https://github.com/bfabiszewski/libmobi), [release v0.12](https://github.com/bfabiszewski/libmobi/releases/tag/v0.12), [implémentation de mobitool](https://github.com/bfabiszewski/libmobi/blob/public/tools/mobitool.c).

## Connexions sans fil

CrossPoint expose un serveur web HTTP et WebSocket ; ce transport est distinct du protocole d’appareil intelligent Calibre. L’adaptateur CrossPoint suit les endpoints et le schéma de sa version de firmware. Le mode compatible Calibre, s’il est exposé, implémente le protocole dans Library Manager ; il n’exécute ni n’installe Calibre. Aucun mode sans fil n’est marqué disponible avant handshake et inventaire réellement implémentés et testés.

## Ordre de construction et critères d’acceptation

1. Figer modèles/IPC, schéma et erreurs ; migrer une base vide puis existante sans perte.
2. Construire stockage, EPUB et bibliothèque ; prouver import, couverture, facettes, déduplication et conservation de l’original.
3. Construire lecteur isolé, conversion et optimisation ; comparer chapitres/texte et ouvrir les dérivés avec un second parseur.
4. Construire jobs, fournisseurs et web ; tester tous les contrats d’API, discovery, reprise, erreurs et provenance.
5. Construire appareils et transfert ; tester faux montages, déconnexion, collision, espace insuffisant, rollback et lecture seule.
6. Construire les panneaux Svelte et FR/EN ; vérifier recherches, sélection, appareil présent, chat et changement de langue au clavier.
7. Tester l’application native et les paquets ; vérifier qu’aucune invocation Calibre ni dépendance métier extérieure n’est nécessaire.
8. Revue sécurité et non-régression, documentation d’exploitation, publication du dépôt et téléchargements des paquets avec SHA-256.

Chaque tâche d’implémentation possède un fichier ou une responsabilité atomique et met à jour la cartographie correspondante dans la même tâche. Les tests critiques portent sur le comportement : ZIP traversant/bombe, OPF invalide, DRM, scripts EPUB, import répété, metadata concurrentes, réseau privé/redirection, clés absentes, sortie de processus excessive, débranchement, ancien montage, cache de présence et annulation après modification externe.

Les jobs CPU/ZIP/images utilisent un pool bloquant borné ; le réseau utilise Tokio. Les requêtes de bibliothèque sont paginées, les couvertures sont mises en cache et les gros EPUB ne sont pas envoyés au frontend en base64. La base et les originaux peuvent être sauvegardés depuis une commande documentée. Les clés IA sont conservées par le service de secrets Linux, jamais dans les exports ou journaux. Si le trousseau n’est pas disponible, l’interface l’explique et propose une session non persistante, sans repli silencieux en texte clair.
