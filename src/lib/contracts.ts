export type BookFormat = 'epub' | 'mobi' | 'azw3' | 'fb2' | 'txt' | 'html' | 'pdf' | 'cbz';
export type ReadStatus = 'unread' | 'reading' | 'finished';
export type MetadataStatus = 'pending' | 'verified' | 'needsReview' | 'failed';
export type BookSort = 'title' | 'author' | 'series' | 'added' | 'updated' | 'size' | 'progress' | 'published' | 'rating';
export type DeviceTransport = 'usb' | 'crosspoint' | 'calibreWireless';
export type WirelessTransport = Exclude<DeviceTransport, 'usb'>;
export type FileVariant = 'original' | 'normalized' | 'optimized' | 'converted';
export type JobKind = 'import' | 'enrich' | 'optimize' | 'convert' | 'deviceIndex' | 'transfer' | 'chat';
export type JobStatus = 'queued' | 'running' | 'waitingForConfiguration' | 'waitingForNetwork' | 'completed' | 'failed' | 'cancelled';
export type OptimizationPreset = 'lossless' | 'balanced' | 'xteink' | 'textOnly';
export type ProviderId = 'zai' | 'kimi' | 'minimax' | 'codex' | 'claude' | 'mistral';
export type ProviderConnectionMode = 'api' | 'localCli';
export type ProviderStatus = 'ready' | 'needsKey' | 'unavailable';
export type ModelCatalogSource = 'api' | 'officialCatalog' | 'localCli' | 'cache';
export type Theme = 'system' | 'light' | 'dark';
export type ErrorCode =
  | 'invalidInput'
  | 'notFound'
  | 'unsupportedFormat'
  | 'encryptedBook'
  | 'invalidEpub'
  | 'unsafePath'
  | 'revisionConflict'
  | 'profileInUse'
  | 'deviceDisconnected'
  | 'insufficientSpace'
  | 'networkUnavailable'
  | 'providerNotConfigured'
  | 'providerError'
  | 'rateLimited'
  | 'secretStoreUnavailable'
  | 'conversionFailed'
  | 'operationConflict'
  | 'cancelled'
  | 'internal';

export interface AppError {
  code: ErrorCode;
  message: string;
  retryable: boolean;
  detail: string | null;
}

export interface BookMetadata {
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
}

export type EpubMetadata = BookMetadata;

export interface Book extends BookMetadata {
  id: string;
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

export interface BookQuery {
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

export interface BookPage {
  items: Book[];
  total: number;
  offset: number;
  limit: number;
}

export interface Facet {
  value: string;
  count: number;
}

export interface LibraryFacets {
  authors: Facet[];
  series: Facet[];
  genres: Facet[];
  tags: Facet[];
  languages: Facet[];
  formats: Facet[];
  devices: Facet[];
}

export interface BookPatch {
  title?: string;
  authors?: string[];
  authorSort?: string;
  series?: string | null;
  seriesIndex?: number | null;
  genres?: string[];
  tags?: string[];
  language?: string;
  description?: string;
  isbn?: string | null;
  publisher?: string | null;
  published?: string | null;
  readStatus?: ReadStatus;
  favorite?: boolean;
  rating?: number | null;
  notes?: string;
}

export interface BookFile {
  id: string;
  bookId: string;
  format: BookFormat;
  variant: FileVariant;
  profile: string | null;
  sizeBytes: number;
  sha256: string;
  createdAt: string;
}

export interface Device {
  id: string;
  label: string;
  transport: DeviceTransport;
  connected: boolean;
  writable: boolean;
  profile: string;
  mountPath: string | null;
  address: string | null;
  totalBytes: number | null;
  freeBytes: number | null;
  bookCount: number;
  matchedBookCount: number;
  lastSeenAt: string;
}

export interface DeviceInventoryBook {
  deviceId: string;
  relativePath: string;
  bookId: string | null;
  sha256: string | null;
  title: string;
  authors: string[];
  format: BookFormat;
  sizeBytes: number;
  lastSeenAt: string;
  warnings: string[];
}

export interface DeviceInventoryPage {
  items: DeviceInventoryBook[];
  total: number;
  offset: number;
  limit: number;
}

export interface Job {
  id: string;
  kind: JobKind;
  status: JobStatus;
  progress: number;
  message: string;
  bookIds: string[];
  result: unknown | null;
  error: AppError | null;
  createdAt: string;
  updatedAt: string;
}

export interface OptimizationProfile {
  id: string;
  name: string;
  maxImageWidth: number | null;
  maxImageHeight: number | null;
  jpegQuality: number;
  grayscale: boolean;
  removeImages: boolean;
  removeEmbeddedFonts: boolean;
  simplifyCss: boolean;
  compressionLevel: number;
}

export interface OptimizationReport {
  bookId: string;
  fileId: string;
  beforeBytes: number;
  afterBytes: number;
  imagesChanged: number;
  imagesRemoved: number;
  fontsRemoved: number;
  chaptersBefore: number;
  chaptersAfter: number;
  textPreserved: boolean;
  warnings: string[];
}

export interface ConversionCapabilities {
  inputs: BookFormat[];
  outputs: BookFormat[];
  warnings: string[];
}

export interface Operation {
  id: string;
  kind: string;
  status: 'applied' | 'reverted' | 'failed';
  description: string;
  reversible: boolean;
  createdAt: string;
}

export interface Provider {
  id: ProviderId;
  name: string;
  configured: boolean;
  connectionMode: ProviderConnectionMode;
  supportsTools: boolean;
  status: ProviderStatus;
}

export interface Model {
  id: string;
  name: string;
  description: string;
  contextWindow: number | null;
  supportsTools: boolean | null;
}

export interface ModelCatalog {
  providerId: ProviderId;
  models: Model[];
  source: ModelCatalogSource;
  fetchedAt: string;
  stale: boolean;
  error: AppError | null;
}

export interface WebSource {
  url: string;
  title: string;
  excerpt: string;
  retrievedAt: string;
}

export interface MetadataEvidence {
  field: string;
  value: string;
  confidence: number;
  sourceUrls: string[];
}

export interface MetadataProposal {
  bookId: string;
  patch: BookPatch;
  confidence: number;
  evidence: MetadataEvidence[];
  warnings: string[];
  providerId: ProviderId;
  modelId: string;
}

export interface Conversation {
  id: string;
  title: string;
  createdAt: string;
}

export interface ChatMessage {
  id: string;
  conversationId: string;
  role: 'user' | 'assistant' | 'system';
  content: string;
  sources: WebSource[];
  createdAt: string;
}

export interface Settings {
  language: string;
  theme: Theme;
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

export interface AppBootstrap {
  version: string;
  systemLanguage: string;
  settings: Settings;
  devices: Device[];
  pendingJobs: Job[];
}

export interface ReaderTocItem {
  label: string;
  sectionIndex: number;
  fragment: string | null;
  children: ReaderTocItem[];
}

export interface ReaderManifestSection {
  index: number;
  title: string;
  sizeBytes: number;
}

export interface ReaderManifest {
  bookId: string;
  title: string;
  authors: string[];
  sections: ReaderManifestSection[];
  toc: ReaderTocItem[];
  savedLocation: string | null;
  savedProgress: number;
}

export interface ReaderResource {
  id: string;
  url: string;
  mediaType: string;
}

export interface ReaderSection {
  bookId: string;
  sectionIndex: number;
  html: string;
  resources: ReaderResource[];
  warnings: string[];
}

interface CommandContract<Params, Result> {
  params: Params;
  result: Result;
}

export interface IpcContracts {
  app_bootstrap: CommandContract<undefined, AppBootstrap>;
  library_list: CommandContract<{ query: BookQuery }, BookPage>;
  library_facets: CommandContract<undefined, LibraryFacets>;
  book_get: CommandContract<{ id: string }, Book>;
  book_files: CommandContract<{ id: string }, BookFile[]>;
  import_books: CommandContract<{ paths: string[] }, Job>;
  book_update: CommandContract<{ id: string; patch: BookPatch; expectedRevision: number }, Book>;
  book_enrich: CommandContract<{ id: string }, Job>;
  book_optimize: CommandContract<{ id: string; profileId: string }, Job>;
  book_convert: CommandContract<{ id: string; format: BookFormat }, Job>;
  conversion_capabilities: CommandContract<undefined, ConversionCapabilities>;
  optimization_profiles: CommandContract<undefined, OptimizationProfile[]>;
  devices_scan: CommandContract<undefined, Device[]>;
  device_index: CommandContract<{ id: string }, Job>;
  device_inventory: CommandContract<{
    id: string;
    offset: number;
    limit: number;
    unknownOnly: boolean;
  }, DeviceInventoryPage>;
  device_import: CommandContract<{ id: string; relativePaths: string[] | null }, Job>;
  device_connect_wireless: CommandContract<{
    address: string;
    transport: WirelessTransport;
    label: string;
    password: string | null;
  }, Device>;
  device_disconnect: CommandContract<{ id: string }, null>;
  device_transfer: CommandContract<{ id: string; bookIds: string[]; profileId: string | null }, Job>;
  reader_open: CommandContract<{ id: string }, ReaderManifest>;
  reader_section: CommandContract<{ id: string; sectionIndex: number }, ReaderSection>;
  reader_save_progress: CommandContract<{ id: string; location: string; progress: number }, null>;
  providers_list: CommandContract<undefined, Provider[]>;
  provider_models: CommandContract<{ id: ProviderId; force: boolean }, ModelCatalog>;
  provider_set_secret: CommandContract<{ id: ProviderId; secret: string; persist: boolean }, null>;
  provider_clear_secret: CommandContract<{ id: ProviderId }, null>;
  conversations_list: CommandContract<undefined, Conversation[]>;
  conversation_messages: CommandContract<{ id: string }, ChatMessage[]>;
  chat_send: CommandContract<{ conversationId: string | null; text: string; bookIds: string[] }, Job>;
  settings_get: CommandContract<undefined, Settings>;
  settings_save: CommandContract<{ settings: Settings }, Settings>;
  jobs_list: CommandContract<undefined, Job[]>;
  job_cancel: CommandContract<{ id: string }, Job>;
  operations_list: CommandContract<undefined, Operation[]>;
  operation_undo: CommandContract<{ id: string }, Operation>;
}

export type IpcCommand = keyof IpcContracts;
export type IpcParams<Command extends IpcCommand> = IpcContracts[Command]['params'];
export type IpcResult<Command extends IpcCommand> = IpcContracts[Command]['result'];

export interface LibraryChangedEvent { bookIds: string[]; reason: string }
export interface DevicesChangedEvent { devices: Device[] }
export interface JobUpdatedEvent { job: Job }
export interface ChatDeltaEvent { conversationId: string; messageId: string; text: string; finished: boolean }
export interface ReaderProgressEvent { bookId: string; location: string; progress: number }

export interface IpcEvents {
  'library:changed': LibraryChangedEvent;
  'devices:changed': DevicesChangedEvent;
  'job:updated': JobUpdatedEvent;
  'chat:delta': ChatDeltaEvent;
  'reader:progress': ReaderProgressEvent;
}

export function defaultQuery(limit = 100): BookQuery {
  if (!Number.isSafeInteger(limit) || limit < 1 || limit > 200) {
    throw new RangeError('Page size must be an integer between 1 and 200');
  }
  return {
    search: '', authors: [], series: [], genres: [], tags: [], languages: [], formats: [],
    deviceId: null, onDevice: null, readStatus: null, favorite: null, metadataStatus: null,
    missingCover: null, minSizeBytes: null, maxSizeBytes: null, sort: 'title',
    descending: false, offset: 0, limit,
  };
}
