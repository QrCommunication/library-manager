# Contrat IPC de Library Manager

Ce document est la référence commune à `crates/library-core/src/models.rs` et `devices.rs`, aux commandes Tauri et à `src/lib/contracts.ts`. Les propriétés JSON sont en `camelCase`. Les enums sont les chaînes ci-dessous. Les `Option<T>` Rust deviennent `T | null`, jamais une valeur inventée. Les dates sont des chaînes RFC 3339 UTC, les tailles des nombres entiers sûrs et les identifiants des chaînes opaques générées par le backend. Les livres et tâches utilisent des UUID ; les appareils peuvent employer une identité stable dérivée du volume ou du protocole. Une pagination est bornée à 200 livres.

## Types de bibliothèque

```ts
type BookFormat = 'epub' | 'mobi' | 'azw3' | 'fb2' | 'txt' | 'html' | 'pdf' | 'cbz';
type ReadStatus = 'unread' | 'reading' | 'finished';
type MetadataStatus = 'pending' | 'verified' | 'needsReview' | 'failed';
type BookSort = 'title' | 'author' | 'series' | 'added' | 'updated' | 'size' | 'progress' | 'published' | 'rating';
interface Book {
  id: string;
  title: string;
  authors: string[];
  authorSort: string;
  series: string | null;
  seriesIndex: number | null;
  genres: string[];
  tags: string[];
  language: string;
  description: string;
  isbn: string | null;
  publisher: string | null;
  published: string | null;
  coverPath: string | null;
  format: BookFormat;
  sizeBytes: number;
  addedAt: string;
  updatedAt: string;
  readStatus: ReadStatus;
  readingProgress: number;
  favorite: boolean;
  rating: number | null;
  notes: string;
  metadataStatus: MetadataStatus;
  metadataConfidence: number | null;
  revision: number;
  onDeviceIds: string[];
}
interface BookQuery {
  search: string;
  authors: string[];
  series: string[];
  genres: string[];
  tags: string[];
  languages: string[];
  formats: BookFormat[];
  deviceId: string | null;
  onDevice: boolean | null;
  readStatus: ReadStatus | null;
  favorite: boolean | null;
  metadataStatus: MetadataStatus | null;
  missingCover: boolean | null;
  minSizeBytes: number | null;
  maxSizeBytes: number | null;
  sort: BookSort;
  descending: boolean;
  offset: number;
  limit: number;
}
interface BookPage { items: Book[]; total: number; offset: number; limit: number }
interface Facet { value: string; count: number }
interface LibraryFacets {
  authors: Facet[]; series: Facet[]; genres: Facet[]; tags: Facet[];
  languages: Facet[]; formats: Facet[]; devices: Facet[];
}
interface BookPatch {
  title?: string; authors?: string[]; authorSort?: string;
  series?: string | null; seriesIndex?: number | null;
  genres?: string[]; tags?: string[]; language?: string; description?: string;
  isbn?: string | null; publisher?: string | null; published?: string | null;
  readStatus?: ReadStatus; favorite?: boolean; rating?: number | null; notes?: string;
}
interface BookFile {
  id: string; bookId: string; format: BookFormat;
  variant: 'original' | 'normalized' | 'optimized' | 'converted';
  profile: string | null; sizeBytes: number; sha256: string; createdAt: string;
}
```

`coverPath` est une URI opaque autorisée par l’application, pas un chemin de sortie arbitraire. `onDeviceIds` est calculé à partir de l’inventaire actuel des appareils connectés. Une déconnexion invalide immédiatement ce calcul, même si l’inventaire historique reste dans SQLite. Dans un filtre multi-valeurs, les valeurs d’une même facette sont combinées par OU ; les différentes facettes sont combinées par ET. `seriesIndex` accepte zéro et des décimales.

Un `BookPatch` distingue propriété absente (ne rien changer) et `null` (effacer un champ nullable). `book_update` refuse les patches vides ; `book_review` accepte un patch vide pour valider explicitement une proposition sans modification de valeur. Les champs libres ont des longueurs maximales ; un numéro de série ou une confiance non fini est invalide. Le serveur vérifie la révision avant d’appliquer le patch et renvoie `revisionConflict` en cas de changement concurrent.

## Appareils, jobs et optimisation

```ts
type DeviceTransport = 'usb' | 'crosspoint' | 'calibreWireless';
interface Device {
  id: string; label: string; transport: DeviceTransport;
  connected: boolean; writable: boolean; profile: string;
  mountPath: string | null; address: string | null;
  totalBytes: number | null; freeBytes: number | null;
  bookCount: number; matchedBookCount: number; lastSeenAt: string;
}
interface DeviceInventoryBook {
  deviceId: string; relativePath: string; bookId: string | null; sha256: string | null;
  title: string; authors: string[]; format: BookFormat; sizeBytes: number;
  lastSeenAt: string; warnings: string[];
}
interface DeviceInventoryPage {
  items: DeviceInventoryBook[]; total: number; offset: number; limit: number;
}
interface DeviceIndexProgress {
  phase: 'discovering' | 'reading' | 'finalizing';
  visitedEntries: number; processedBooks: number; totalBooks: number;
  bytesRead: number; totalBytes: number; currentPath: string | null;
}
type JobKind = 'import' | 'enrich' | 'optimize' | 'convert' | 'deviceIndex' | 'transfer' | 'chat';
type JobStatus = 'queued' | 'running' | 'waitingForConfiguration' | 'waitingForNetwork' | 'completed' | 'failed' | 'cancelled';
interface Job {
  id: string; kind: JobKind; status: JobStatus; progress: number;
  message: string; bookIds: string[]; result: unknown | null;
  error: AppError | null; createdAt: string; updatedAt: string;
}
type OptimizationPreset = 'lossless' | 'balanced' | 'xteink' | 'textOnly';
interface OptimizationProfile {
  id: string; name: string; maxImageWidth: number | null;
  maxImageHeight: number | null; jpegQuality: number;
  grayscale: boolean; removeImages: boolean; removeEmbeddedFonts: boolean;
  simplifyCss: boolean; compressionLevel: number;
}
interface OptimizationReport {
  bookId: string; fileId: string; beforeBytes: number; afterBytes: number;
  imagesChanged: number; imagesRemoved: number; fontsRemoved: number;
  chaptersBefore: number; chaptersAfter: number;
  textPreserved: boolean; warnings: string[];
}
interface Operation {
  id: string; kind: string; status: 'applied' | 'reverted' | 'failed';
  description: string; reversible: boolean; createdAt: string;
}
interface RemoveBookSelection { bookId: string; expectedRevision: number }
interface RemoveBooksResult { removedBookIds: string[]; operations: Operation[] }
```

Une progression est dans `[0,1]`. Un job terminé ne passe pas de nouveau à `running`. Un job interrompu par fermeture de l’application est repris ou signalé en échec suivant son journal ; les copies partiellement écrites ne deviennent jamais des fichiers finaux. Les modèles ne reçoivent aucune action `delete`, `shell` ou `writeFile`.

## IA, sources et réglages

```ts
type ProviderId = 'zai' | 'kimi' | 'minimax' | 'codex' | 'claude' | 'mistral';
interface Provider {
  id: ProviderId; name: string; configured: boolean;
  connectionMode: 'api' | 'localCli'; supportsTools: boolean;
  status: 'ready' | 'needsKey' | 'unavailable';
}
interface Model {
  id: string; name: string; description: string;
  contextWindow: number | null; supportsTools: boolean | null;
}
interface ModelCatalog {
  providerId: ProviderId; models: Model[];
  source: 'api' | 'officialCatalog' | 'localCli' | 'cache';
  fetchedAt: string; stale: boolean; error: AppError | null;
}
interface WebSource {
  url: string; title: string; excerpt: string; retrievedAt: string;
}
interface MetadataProposal {
  bookId: string; patch: BookPatch; confidence: number;
  evidence: { field: string; value: string; confidence: number; sourceUrls: string[] }[];
  warnings: string[]; providerId: ProviderId; modelId: string;
}
interface MetadataReview {
  state: 'pending' | 'applied' | 'dismissed' | 'obsolete';
  sourceRevision?: number;
  reviewRevision?: number;
  resolvedRevision?: number;
}
interface MetadataResult {
  proposal: MetadataProposal;
  review: MetadataReview;
}
interface Conversation { id: string; title: string; createdAt: string }
interface ChatMessage {
  id: string; conversationId: string; role: 'user' | 'assistant' | 'system';
  content: string; sources: WebSource[]; createdAt: string;
}
interface Settings {
  language: 'system' | string;
  theme: 'system' | 'light' | 'dark';
  libraryRoot: string;
  providerId: ProviderId | null;
  modelId: string | null;
  autoEnrich: boolean;
  webEnabled: boolean;
  autoApplyConfidence: number;
  defaultDeviceProfile: string;
  defaultOptimizationProfile: string;
  maxConcurrentJobs: number;
}
interface AppError {
  code: 'invalidInput' | 'notFound' | 'unsupportedFormat' | 'encryptedBook'
    | 'invalidEpub' | 'unsafePath' | 'revisionConflict' | 'deviceDisconnected'
    | 'insufficientSpace' | 'networkUnavailable' | 'providerNotConfigured'
    | 'providerError' | 'rateLimited' | 'secretStoreUnavailable'
    | 'conversionFailed' | 'operationConflict' | 'profileInUse' | 'cancelled' | 'internal';
  message: string;
  retryable: boolean;
  detail: string | null;
}
```

Les six fournisseurs utilisent leur API dans cette version. `codex` correspond à l’API OpenAI Responses ; aucun CLI Codex ou Claude n’est invoqué. Les valeurs `localCli` du contrat sont réservées à une extension et ne sont pas annoncées comme disponibles. Les capacités affichées sont découvertes ou documentées ; une valeur inconnue reste `null`. Le moteur web commun fournit des sources à tous les connecteurs. `webEnabled=false` est un choix utilisateur explicite, affiché dans le chat et empêchant de prétendre que l’information a été vérifiée en ligne.

`settings_get` et `providers_list` ne retournent jamais une clé. Une clé est envoyée uniquement à `provider_set_secret`, conservée dans le trousseau Linux ou en mémoire pour la session, et supprimée des erreurs/journaux. Les erreurs publiques sont des objets ; l’interface localise leur code et montre un détail borné sans donnée sensible.

## Lecteur

```ts
interface ReaderTocItem { label: string; sectionIndex: number; fragment: string | null; children: ReaderTocItem[] }
interface ReaderManifest {
  bookId: string; title: string; authors: string[];
  sections: { index: number; title: string; sizeBytes: number }[];
  toc: ReaderTocItem[]; savedLocation: string | null; savedProgress: number;
}
interface ReaderSection {
  bookId: string; sectionIndex: number; html: string;
  resources: { id: string; url: string; mediaType: string }[];
  warnings: string[];
}
```

Le backend retire scripts, gestionnaires d’événements, formulaires, frames, liens de ressource distante et URL dangereuses. Les URLs de ressources locales sont réécrites vers des identifiants dont le backend contrôle le livre et le chemin ZIP. Le frontend rend la section dans une iframe à origine opaque avec `sandbox` sans `allow-scripts` ni `allow-same-origin`. Le bridge IPC existe seulement dans la fenêtre de l’application. `reader_save_progress` reçoit une position bornée et ne peut pas modifier les fichiers EPUB ni les caches d’un appareil.

## Commandes

Les noms ci-dessous sont les chaînes exactes utilisées par `invoke`. Les paramètres sont un objet JSON nommé. Les traitements longs retournent immédiatement un `Job` durable ; la progression arrive par événements puis `jobs_list` permet de reprendre l’affichage.

| Commande | Paramètres | Réponse |
| --- | --- | --- |
| `app_bootstrap` | aucun | `{ version, systemLanguage, settings, devices, pendingJobs }` |
| `library_list` | `{ query: BookQuery }` | `BookPage` |
| `library_facets` | aucun | `LibraryFacets` |
| `book_get` | `{ id }` | `Book` |
| `book_files` | `{ id }` | `BookFile[]` |
| `import_books` | `{ paths: string[] }` | `Job` |
| `book_update` | `{ id, patch: BookPatch, expectedRevision }` | `Book` |
| `book_review` | `{ id, jobId, patch: BookPatch, expectedRevision }` | `Book` |
| `books_remove` | `{ requestId: string, books: RemoveBookSelection[] }` | `RemoveBooksResult` |
| `book_enrich` | `{ id }` | `Job` |
| `book_optimize` | `{ id, profileId }` | `Job` |
| `book_convert` | `{ id, format: BookFormat }` | `Job` |
| `conversion_capabilities` | aucun | `{ inputs: BookFormat[], outputs: BookFormat[], warnings: string[] }` |
| `optimization_profiles` | aucun | `OptimizationProfile[]` |
| `devices_scan` | aucun | `Device[]` |
| `device_index` | `{ id }` | `Job` |
| `device_inventory` | `{ id, offset: number, limit: number, unknownOnly: boolean }` | `DeviceInventoryPage` |
| `device_import` | `{ id, relativePaths: string[] \| null }` | `Job` |
| `device_connect_wireless` | `{ address, transport, label, password: string | null }` | `Device` |
| `device_disconnect` | `{ id }` | `null` |
| `device_transfer` | `{ id, bookIds: string[], profileId: string | null }` | `Job` |
| `reader_open` | `{ id }` | `ReaderManifest` |
| `reader_section` | `{ id, sectionIndex }` | `ReaderSection` |
| `reader_save_progress` | `{ id, location, progress }` | `null` |
| `providers_list` | aucun | `Provider[]` |
| `provider_models` | `{ id: ProviderId, force: boolean }` | `ModelCatalog` |
| `provider_set_secret` | `{ id: ProviderId, secret, persist: boolean }` | `null` |
| `provider_clear_secret` | `{ id: ProviderId }` | `null` |
| `conversations_list` | aucun | `Conversation[]` |
| `conversation_messages` | `{ id }` | `ChatMessage[]` |
| `chat_send` | `{ conversationId: string | null, text, bookIds: string[], allowChanges?: boolean }` | `Job` |
| `settings_get` | aucun | `Settings` |
| `settings_save` | `{ settings: Settings }` | `Settings` |
| `jobs_list` | aucun | `Job[]` |
| `job_cancel` | `{ id }` | `Job` |
| `operations_list` | aucun | `Operation[]` |
| `operation_undo` | `{ id }` | `Operation` |

`import_books` accepte des chemins sources absolus, validés comme fichiers réguliers. Un appel accepte au plus 200 chemins ; l’interface découpe les sélections plus importantes en lots séquentiels. `device_import` reçoit uniquement des chemins relatifs issus de l’inventaire de l’appareil connecté. La racine de bibliothèque est le dossier de données de l’application, affiché en lecture seule et imposée côté backend. Les destinations, variantes et fichiers d’appareil sont calculés depuis des IDs autoritatifs. Les commandes n’acceptent ni commande shell, ni SQL, ni chemin de destination fourni pour contourner le stockage.

### Validation durable d’une proposition

Les nouveaux résultats des jobs `enrich` utilisent l’enveloppe `{ proposal, review }`. `sourceRevision` désigne la révision de référence de l’analyse ; `reviewRevision` désigne la révision sur laquelle la proposition en attente peut être examinée. Une édition personnelle peut déplacer `reviewRevision` sans changer `sourceRevision` ni consommer la proposition. Une revue résolue porte `resolvedRevision`. Les champs de révision sont optionnels dans le type ci-dessus pour conserver la lecture des résultats historiques ; une nouvelle proposition `pending` possède une `reviewRevision` valide. Les anciens résultats contenant directement une `MetadataProposal` restent compatibles sans leur attribuer une révision source fictive.

`book_review({ id, jobId, patch, expectedRevision })` lie la validation au job `enrich` terminé dont la proposition concerne ce livre. Le backend vérifie la révision du livre et, pour une enveloppe, `reviewRevision` ainsi que l’état `pending`. La transaction commune écrit le livre, les éventuelles variantes, l’historique et la résolution de revue. Le patch peut être vide pour acquitter une proposition dont les valeurs correspondent déjà au catalogue ; il suit sinon les mêmes contrôles de champs que `book_update`.

Une validation explicite marque la proposition `applied`. Une modification bibliographique ordinaire résout les propositions en attente comme `dismissed`, ou `obsolete` si leur révision ne correspond plus ; une modification des notes, favoris, évaluations ou état de lecture les conserve. Les états résolus ne doivent plus offrir d’action de revue. Un conflit de révision renvoie `revisionConflict` et exige une nouvelle lecture. Un état incompatible renvoie `operationConflict`. La répétition sans changement d’une validation déjà appliquée à la même révision résolue restitue le livre sans nouvel historique ; cela n’autorise pas à réappliquer une proposition après une autre édition.

### Retrait réversible du catalogue

`books_remove` reçoit un UUID canonique `requestId` et de 1 à 200 livres distincts, chacun avec `bookId` et `expectedRevision` strictement positive. Les propriétés inconnues d’une entrée `RemoveBookSelection` sont refusées. Le backend valide la sélection complète et ses révisions avant le retrait transactionnel : aucun livre introuvable ou en conflit n’est ignoré pour produire un succès partiel.

La réponse contient `removedBookIds` et une opération réversible de type `catalogueRemove` par livre. Les reçus incluent les instantanés nécessaires à la restauration. Le retrait concerne le catalogue local ; il conserve les fichiers originaux, les variantes et les copies physiques sur les appareils. Un événement `library:changed` de raison `catalogueRemove` invite à actualiser les vues.

En cas de réponse incertaine, le client répète le même `requestId` avec les mêmes couples livre/révision : tant que les opérations restent appliquées, le backend restitue les opérations existantes sans nouveau retrait ni doublon d’historique. L’ordre des livres est normalisé côté serveur. Réutiliser cet identifiant avec un autre contenu, ou après annulation du retrait, provoque `operationConflict`. Après un conflit de révision, le client relit les livres, fait confirmer la nouvelle sélection et utilise un nouvel identifiant si le contenu de la demande change.

`operation_undo({ id })` restaure un retrait à partir de son instantané, après contrôle des fichiers conservés et des collisions avec le catalogue courant. Un conflit ne doit pas écraser une entrée existante. La restauration et son état d’historique sont transactionnels ; elle n’effectue aucune suppression physique sur un appareil.

`chat_send` accepte au plus 200 identifiants de livres sélectionnés. `allowChanges` vaut `false` lorsqu’il est omis ou nul ; les requêtes historiques restent donc en lecture seule. Le backend enregistre le texte, la sélection et cette autorisation avec le message utilisateur avant de créer le job. L’interface consomme la case d’autorisation après acceptation du job : chaque demande suivante nécessite un nouveau choix explicite. Une reprise doit conserver ce périmètre exact : ni le modèle, ni un extrait de livre ou de site ne peuvent élargir les droits. Le contexte initial fournit au plus 32 aperçus de métadonnées et annonce cette limite ; les identifiants des autres livres sélectionnés restent disponibles aux outils bornés. Une réponse est limitée à huit étapes d’outil ou de réponse finale, et le contexte JSON à 384 KiB. Une seule complétion corrective du format est autorisée pour toute la requête (neuf appels maximum dans ce cycle, hors planner préalable), puis le contrat strict est revalidé avant toute exécution. Le texte invalide reste éphémère, borné à 64 KiB et ne peut pas accorder de permission. Ces limites ne constituent pas une lecture de toute la sélection.

Le chat utilise des outils internes `librarySearch`, `bookInspect`, `webSearch`, `webFetch`, `updateMetadata`, `organizeBooks` et `verifyMetadata` dans un protocole JSON strict. Ils ne sont pas de nouvelles commandes IPC publiques. Les recherches de bibliothèque restent bornées à 48 résultats par page. `webEnabled=false` interdit les outils web ; une citation ou preuve web doit correspondre à une source réellement récupérée pendant la requête. Les outils n’exposent ni shell, ni suppression, ni SQL, ni chemin arbitraire de lecture ou de destination.

Une modification bibliographique exige `allowChanges=true`, un livre dans la sélection enregistrée et une inspection préalable du fichier réel. `bookInspect` vérifie SHA-256 et taille par le stockage géré, puis fournit au plus 12 000 caractères d’extraits de pages titre, copyright/édition et chapitre lorsqu’ils sont disponibles. Il distingue `originalFileId` / `originalSha256` de `fileId` / `sha256` pour l’EPUB inspecté : un EPUB converti peut être lu sans confondre sa preuve avec l’original TXT ou un autre format. Un original absent ou un format sans EPUB lisible ne permet pas de prétendre à une inspection ; un EPUB dérivé porte un avertissement de revue manuelle. Le moteur recontrôle l’original, l’EPUB inspecté, la révision et l’EPUB actif utilisé pour la transformation avant d’admettre une modification. Un hash ou une taille divergents provoquent un conflit. Un ISBN proposé doit avoir un checksum valide et figurer dans les métadonnées embarquées, l’extrait réellement inspecté — notamment le copyright — ou une source effectivement récupérée. Le modèle ne peut pas modifier notes personnelles, favoris, évaluations ou état de lecture par cet outil.

`organizeBooks` utilise uniquement la convention gérée auteur/série/titre et vérifie les révisions ; les originaux restent immuables et les changements sont auditables et réversibles. Un lot d’organisation ou de vérification accepte au plus 200 livres. `verifyMetadata` peut mettre en file une analyse des livres sélectionnés sans autoriser de modification bibliographique : ces jobs ont l’origine interne `assistantReview`, stockent une proposition à examiner et n’appliquent jamais automatiquement son patch. Leur création ne signifie pas que l’analyse est terminée. Seuls les jobs d’analyse non terminaux empêchent une nouvelle vérification ; un ancien échec, une annulation ou une analyse terminée permettent une relance. La déduplication des demandes simultanées conserve l’origine et la révision de référence.

Les mutations du chat conservent des reçus durables ; une reprise avec le même identifiant d’outil et les mêmes arguments réutilise le résultat enregistré, tandis qu’une action interrompue au résultat incertain est refusée. Les extraits de lecture restent temporaires et doivent être relus lors d’une reprise. La progression du chat fournit les étapes réelles dans `Job.message`, sans pourcentage d’avancement inventé ; les comptes de livres modifiés ne sont publiés qu’après résultat. L’annulation arrête les opérations suivantes et attend le règlement des mutations en cours avant de libérer le worker et le profil. Elle ne supprime pas les actions déjà terminées ni les analyses déjà créées, qui apparaissent dans Activité et peuvent être annulées individuellement.

`device_inventory` exige un appareil connecté, un offset entier positif ou nul et une limite de 1 à 200. `unknownOnly` retient les entrées sans livre local correspondant. Pendant un inventaire USB, les livres dont la lecture est terminée sont accessibles avant la publication finale. Ces entrées ont un chemin relatif et peuvent avoir un `bookId` ou `sha256` nul ; elles ne sont pas des objets `Book` du catalogue local.

Pour `device_import`, `relativePaths: null` sélectionne dans le worker tous les livres sans correspondance locale. La sélection utilise l’inventaire backend complet, borné à 20 000 livres, et reste indépendante de la page affichée ou du résultat limité de `deviceIndex`. Chaque source est revalidée et son contenu copié doit correspondre au SHA-256 inventorié avant l’insertion locale. Un fichier sans empreinte vérifiable est refusé. Le résultat du job porte `deviceId` dès enqueue, puis `imported`, `duplicates`, `total`, `processed`, `errorsByCode`, `bookIds`, `warnings`, `warningCount`, `bookIdsTruncated` et `warningsTruncated`. Les tableaux de diagnostics sont limités à 500 entrées ; les compteurs restent exacts. Les erreurs par fichier n’empêchent pas le traitement des sources valides suivantes.

`deviceIndex` conserve un résultat `{ deviceId, books, total, truncated, warnings }`. L’aperçu renvoyé à l’interface est limité à 500 lignes et 768 KiB ; l’index et le calcul des présences en base restent complets. `transfer` renvoie les comptes `copied`, `skipped`, `failed`, les résultats par livre et les avertissements. Une tâche `chat` conserve son `conversationId` dès sa mise en file, même lorsqu’elle attend une configuration.

Pendant un job USB `deviceIndex`, `result` expose `{ deviceId, indexProgress: DeviceIndexProgress }`. La phase `discovering` publie les compteurs d’énumération sans pourcentage estimé sur la durée. Après découverte, `progress` suit les octets lus, de façon monotone et plafonnée à 0,99 jusqu’à la complétion du job. Les mises à jour sont limitées à une toutes les 200 ms, avec publication immédiate aux changements de phase ou du nombre de livres traités. `message` reste vide pour laisser l’interface traduire les étapes. La raison `deviceIndexProgress` de `library:changed` demande de recharger l’inventaire partiel sans ajouter un nouveau type d’événement.

## Événements

| Nom | Payload | Interprétation |
| --- | --- | --- |
| `library:changed` | `{ bookIds: string[], reason: string }` | Recharger page et facettes pertinentes. |
| `devices:changed` | `{ devices: Device[] }` | Remplacer l’état de connexion et recharger les présences. |
| `job:updated` | `{ job: Job }` | Remplacer le job par ID, progression monotone tant qu’il tourne. |
| `chat:delta` | `{ conversationId, messageId, text, finished: boolean }` | Ajouter le texte à un message ; recharger les messages à la fin. |
| `reader:progress` | `{ bookId, location, progress }` | Synchroniser le badge du livre et la position. |

Les abonnements sont supprimés au démontage de leurs panneaux. Les événements constituent des indications d’actualisation ; les réponses des commandes restent la source de vérité après une reconnexion ou une perte d’événement.
