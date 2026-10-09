# Contrat IPC de Library Manager

Ce document est la référence commune à `crates/library-core/src/models.rs`, aux commandes Tauri et à `src/lib/contracts.ts`. Les propriétés JSON sont en `camelCase`. Les enums sont les chaînes ci-dessous. Les `Option<T>` Rust deviennent `T | null`, jamais une valeur inventée. Les dates sont des chaînes RFC 3339 UTC, les tailles des nombres entiers sûrs et les identifiants des chaînes opaques générées par le backend. Les livres et tâches utilisent des UUID ; les appareils peuvent employer une identité stable dérivée du volume ou du protocole. Une pagination est bornée à 200 livres.

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

Un `BookPatch` distingue propriété absente (ne rien changer) et `null` (effacer un champ nullable). Les patches vides sont refusés. Les champs libres ont des longueurs maximales ; un numéro de série ou une confiance non fini est invalide. Le serveur vérifie la révision avant d’appliquer le patch et renvoie `revisionConflict` en cas de changement concurrent.

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
| `book_enrich` | `{ id }` | `Job` |
| `book_optimize` | `{ id, profileId }` | `Job` |
| `book_convert` | `{ id, format: BookFormat }` | `Job` |
| `conversion_capabilities` | aucun | `{ inputs: BookFormat[], outputs: BookFormat[], warnings: string[] }` |
| `optimization_profiles` | aucun | `OptimizationProfile[]` |
| `devices_scan` | aucun | `Device[]` |
| `device_index` | `{ id }` | `Job` |
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
| `chat_send` | `{ conversationId: string | null, text, bookIds: string[] }` | `Job` |
| `settings_get` | aucun | `Settings` |
| `settings_save` | `{ settings: Settings }` | `Settings` |
| `jobs_list` | aucun | `Job[]` |
| `job_cancel` | `{ id }` | `Job` |
| `operations_list` | aucun | `Operation[]` |
| `operation_undo` | `{ id }` | `Operation` |

Seule l’importation accepte des chemins sources, validés comme fichiers réguliers. Un appel accepte au plus 200 chemins ; l’interface découpe les sélections plus importantes en lots séquentiels. La racine de bibliothèque est le dossier de données de l’application, affiché en lecture seule et imposé côté backend. Les destinations, variantes et fichiers d’appareil sont calculés depuis des IDs autoritatifs. Les commandes n’acceptent ni commande shell, ni SQL, ni chemin de destination fourni pour contourner le stockage.

`deviceIndex` conserve un résultat `{ deviceId, books, total, truncated, warnings }`. L’aperçu renvoyé à l’interface est limité à 500 lignes et 768 KiB ; l’index et le calcul des présences en base restent complets. `transfer` renvoie les comptes `copied`, `skipped`, `failed`, les résultats par livre et les avertissements. Une tâche `chat` conserve son `conversationId` dès sa mise en file, même lorsqu’elle attend une configuration.

## Événements

| Nom | Payload | Interprétation |
| --- | --- | --- |
| `library:changed` | `{ bookIds: string[], reason: string }` | Recharger page et facettes pertinentes. |
| `devices:changed` | `{ devices: Device[] }` | Remplacer l’état de connexion et recharger les présences. |
| `job:updated` | `{ job: Job }` | Remplacer le job par ID, progression monotone tant qu’il tourne. |
| `chat:delta` | `{ conversationId, messageId, text, finished: boolean }` | Ajouter le texte à un message ; recharger les messages à la fin. |
| `reader:progress` | `{ bookId, location, progress }` | Synchroniser le badge du livre et la position. |

Les abonnements sont supprimés au démontage de leurs panneaux. Les événements constituent des indications d’actualisation ; les réponses des commandes restent la source de vérité après une reconnexion ou une perte d’événement.
