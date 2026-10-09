import type { UnlistenFn } from '@tauri-apps/api/event';
import type {
  AppError, Book, BookFormat, BookPage, BookQuery, Device, Facet, IpcCommand,
  IpcContracts, IpcEvents, IpcParams, IpcResult, LibraryFacets, OptimizationProfile,
  Provider, ReaderManifest, ReaderSection, Settings,
} from './contracts';

interface BookSeed {
  title: string;
  author: string;
  genre: string;
  language?: 'fr' | 'en';
  series?: string;
  index?: number;
  format?: BookFormat;
}

const SEEDS: readonly BookSeed[] = [
  { title: 'Le jardin des cartes oubliées', author: 'Maëlle Veyron', genre: 'fantasy', series: 'Les Atlas silencieux', index: 1 },
  { title: 'La cité derrière la brume', author: 'Maëlle Veyron', genre: 'fantasy', series: 'Les Atlas silencieux', index: 2 },
  { title: 'Les étoiles sans frontière', author: 'Maëlle Veyron', genre: 'fantasy', series: 'Les Atlas silencieux', index: 3 },
  { title: 'Avant que les îles dérivent', author: 'Maëlle Veyron', genre: 'fantasy', series: 'Les Atlas silencieux', index: 0.5 },
  { title: 'Les veilleurs du phare intérieur', author: 'Solène Arven', genre: 'fantasy', series: 'Chroniques du sel', index: 1 },
  { title: 'Le chant des marées blanches', author: 'Solène Arven', genre: 'fantasy', series: 'Chroniques du sel', index: 2 },
  { title: 'Un royaume sous les vagues', author: 'Solène Arven', genre: 'fantasy', series: 'Chroniques du sel', index: 3 },
  { title: 'La dernière saison des algues', author: 'Solène Arven', genre: 'fantasy', series: 'Chroniques du sel', index: 4, format: 'mobi' },
  { title: 'The Observatory of Small Suns', author: 'Elias Northmere', genre: 'science-fiction', language: 'en', series: 'The Glass Meridian', index: 1 },
  { title: 'A Thousand Quiet Orbits', author: 'Elias Northmere', genre: 'science-fiction', language: 'en', series: 'The Glass Meridian', index: 2 },
  { title: 'Beyond the Amber Signal', author: 'Elias Northmere', genre: 'science-fiction', language: 'en', series: 'The Glass Meridian', index: 3 },
  { title: 'La mémoire des satellites', author: 'Noé Valcendre', genre: 'science-fiction', series: 'Nébuleuses de cuivre', index: 1 },
  { title: 'Les horloges de la planète rouge', author: 'Noé Valcendre', genre: 'science-fiction', series: 'Nébuleuses de cuivre', index: 2, format: 'azw3' },
  { title: 'Le silence après les comètes', author: 'Noé Valcendre', genre: 'science-fiction', series: 'Nébuleuses de cuivre', index: 3 },
  { title: 'Une clé pour le quai des ombres', author: 'Iris Dorlac', genre: 'mystery', series: 'Les enquêtes de Lune', index: 1 },
  { title: 'Le témoin du mercredi bleu', author: 'Iris Dorlac', genre: 'mystery', series: 'Les enquêtes de Lune', index: 2 },
  { title: 'La lettre au timbre effacé', author: 'Iris Dorlac', genre: 'thriller', series: 'Les enquêtes de Lune', index: 3, format: 'fb2' },
  { title: 'The Clockmaker of Willow Square', author: 'June Bellwater', genre: 'historical', language: 'en', series: 'Willow Square', index: 1 },
  { title: 'Letters from the Copper Winter', author: 'June Bellwater', genre: 'historical', language: 'en', series: 'Willow Square', index: 2 },
  { title: 'The Orchard Before the Trains', author: 'June Bellwater', genre: 'historical', language: 'en', series: 'Willow Square', index: 3 },
  { title: 'Le carnet des chemins tranquilles', author: 'Maëlle Veyron', genre: 'essay' },
  { title: 'Petite histoire des nuages domestiques', author: 'Noé Valcendre', genre: 'science', format: 'pdf' },
  { title: 'Building a Gentle Digital Life', author: 'Elias Northmere', genre: 'technology', language: 'en', format: 'html' },
  { title: 'La géométrie des jours heureux', author: 'Solène Arven', genre: 'romance' },
  { title: 'Les mots qui poussent la nuit', author: 'Iris Dorlac', genre: 'poetry' },
  { title: 'The Sparrow and the Pocket Moon', author: 'June Bellwater', genre: 'children', language: 'en' },
  { title: 'Les aventures du petit tram orange', author: 'Solène Arven', genre: 'comics', format: 'cbz' },
  { title: 'Un atelier au bord des saisons', author: 'Noé Valcendre', genre: 'biography' },
  { title: 'The Art of Keeping Quiet Places', author: 'June Bellwater', genre: 'essay', language: 'en' },
  { title: 'Quatre promenades dans un futur proche', author: 'Elias Northmere', genre: 'science-fiction', format: 'txt' },
  { title: 'La maison aux fenêtres de papier', author: 'Iris Dorlac', genre: 'romance' },
  { title: 'The Case of the Missing Afternoon', author: 'June Bellwater', genre: 'mystery', language: 'en' },
  { title: 'Dix machines pour regarder le ciel', author: 'Noé Valcendre', genre: 'technology' },
  { title: 'Atlas des petites choses impossibles', author: 'Maëlle Veyron', genre: 'other' },
  { title: 'An Almanac for Wandering Gardens', author: 'Elias Northmere', genre: 'science', language: 'en' },
  { title: 'Les lanternes du retour', author: 'Solène Arven', genre: 'historical' },
];

const PALETTE = ['#274d53', '#554063', '#925341', '#405840', '#335772', '#815c31', '#734658', '#385959'];
const DEMO_DATE = '2026-10-09T10:00:00.000Z';
const collator = new Intl.Collator('fr', { sensitivity: 'base', numeric: true });

function escapeMarkup(value: string): string {
  return value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;').replaceAll("'", '&#39;');
}

function titleLines(title: string): string[] {
  const lines: string[] = [];
  let current = '';
  for (const word of title.split(' ')) {
    if (current && `${current} ${word}`.length > 18) {
      lines.push(current);
      current = word;
    } else current = current ? `${current} ${word}` : word;
  }
  if (current) lines.push(current);
  return lines;
}

function cover(seed: BookSeed, index: number): string {
  const color = PALETTE[index % PALETTE.length] ?? '#274d53';
  const lines = titleLines(seed.title).map((line, position) =>
    `<tspan x="38" y="${225 + position * 42}">${escapeMarkup(line)}</tspan>`).join('');
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" width="360" height="540" viewBox="0 0 360 540">
    <rect width="360" height="540" fill="${color}"/>
    <path d="M-40 135 Q180 -75 400 135 M-40 155 Q180 -55 400 155 M-40 175 Q180 -35 400 175" fill="none" stroke="#f3e8d0" stroke-width="2" opacity=".32"/>
    <circle cx="${120 + index % 5 * 26}" cy="117" r="40" fill="#f3e8d0" opacity=".8"/>
    <path d="M38 188 H322" stroke="#f3e8d0" opacity=".45"/>
    <text font-family="Georgia,serif" font-size="30" fill="#fff8e9">${lines}</text>
    <text x="38" y="455" font-family="sans-serif" font-size="16" fill="#fff8e9">${escapeMarkup(seed.author)}</text>
    <text x="38" y="500" font-family="sans-serif" font-size="11" letter-spacing="2" fill="#f3e8d0">LIBRARY MANAGER · DEMO</text>
  </svg>`;
  return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(svg)}`;
}

const books: Book[] = SEEDS.map((seed, index) => {
  const language = seed.language ?? 'fr';
  const readStatus = index % 8 === 0 ? 'reading' : index % 7 === 0 ? 'finished' : 'unread';
  return {
    id: `preview-book-${String(index + 1).padStart(2, '0')}`,
    title: seed.title,
    authors: index === 9 ? [seed.author, 'Noé Valcendre'] : [seed.author],
    authorSort: seed.author.split(' ').reverse().join(', '),
    series: seed.series ?? null,
    seriesIndex: seed.index ?? null,
    genres: [seed.genre],
    tags: ['demo', index % 2 === 0 ? 'collection' : 'discovery'],
    language,
    description: language === 'fr'
      ? 'Livre entièrement fictif, créé pour découvrir Library Manager. Le titre, les auteurs, la couverture et le texte de lecture sont des données de démonstration.'
      : 'An entirely fictional book created to explore Library Manager. Its title, authors, cover and reading text are demonstration data.',
    isbn: null,
    publisher: 'Library Manager · Demo',
    published: `${2014 + index % 12}-04-12`,
    coverPath: index % 13 === 0 ? null : cover(seed, index),
    format: seed.format ?? 'epub',
    sizeBytes: seed.format === 'cbz' ? 24_200_000 : 245_000 + index * 83_517,
    addedAt: new Date(Date.UTC(2026, 8, 1 + index)).toISOString(),
    updatedAt: new Date(Date.UTC(2026, 9, 1 + index % 9)).toISOString(),
    readStatus,
    readingProgress: readStatus === 'finished' ? 1 : readStatus === 'reading' ? (index % 6 + 1) / 8 : 0,
    favorite: index % 6 === 0 || index % 11 === 0,
    rating: index % 3 === 0 ? 3 + index % 5 / 2 : null,
    notes: '',
    metadataStatus: index % 10 === 0 ? 'needsReview' : index % 11 === 0 ? 'pending' : 'verified',
    metadataConfidence: null,
    revision: 1,
    onDeviceIds: index % 3 === 0 ? ['preview-xteink'] : [],
  };
});

const devices: Device[] = [
  {
    id: 'preview-xteink', label: 'Demo · Xteink X4 Pro', transport: 'usb', connected: true,
    writable: false, profile: 'xteink', mountPath: null, address: null,
    totalBytes: 64 * 1024 ** 3, freeBytes: 54 * 1024 ** 3,
    bookCount: books.filter((book) => book.onDeviceIds.includes('preview-xteink')).length,
    matchedBookCount: books.filter((book) => book.onDeviceIds.includes('preview-xteink')).length,
    lastSeenAt: DEMO_DATE,
  },
  {
    id: 'preview-wireless', label: 'Demo · CrossPoint', transport: 'crosspoint', connected: false,
    writable: false, profile: 'generic', mountPath: null, address: null,
    totalBytes: null, freeBytes: null,
    bookCount: books.filter((book) => book.onDeviceIds.includes('preview-wireless')).length,
    matchedBookCount: books.filter((book) => book.onDeviceIds.includes('preview-wireless')).length,
    lastSeenAt: DEMO_DATE,
  },
];

let settings: Settings = {
  language: 'system', theme: 'system', libraryRoot: 'demo://library', providerId: null,
  modelId: null, autoEnrich: true, webEnabled: true, autoApplyConfidence: 0.92,
  defaultDeviceProfile: 'xteink', defaultOptimizationProfile: 'xteink', maxConcurrentJobs: 2,
};

const profiles: OptimizationProfile[] = [
  { id: 'lossless', name: 'Lossless', maxImageWidth: null, maxImageHeight: null, jpegQuality: 100, grayscale: false, removeImages: false, removeEmbeddedFonts: false, simplifyCss: false, compressionLevel: 9 },
  { id: 'balanced', name: 'Balanced', maxImageWidth: 1200, maxImageHeight: 1600, jpegQuality: 85, grayscale: false, removeImages: false, removeEmbeddedFonts: true, simplifyCss: false, compressionLevel: 9 },
  { id: 'xteink', name: 'Xteink', maxImageWidth: 480, maxImageHeight: 800, jpegQuality: 75, grayscale: true, removeImages: false, removeEmbeddedFonts: true, simplifyCss: true, compressionLevel: 9 },
  { id: 'textOnly', name: 'Text only', maxImageWidth: null, maxImageHeight: null, jpegQuality: 100, grayscale: false, removeImages: true, removeEmbeddedFonts: true, simplifyCss: true, compressionLevel: 9 },
];

const providers: Provider[] = [
  { id: 'zai', name: 'z.ai', configured: false, connectionMode: 'api', supportsTools: false, status: 'unavailable' },
  { id: 'kimi', name: 'Kimi', configured: false, connectionMode: 'api', supportsTools: false, status: 'unavailable' },
  { id: 'minimax', name: 'MiniMax', configured: false, connectionMode: 'api', supportsTools: false, status: 'unavailable' },
  { id: 'codex', name: 'Codex', configured: false, connectionMode: 'api', supportsTools: false, status: 'unavailable' },
  { id: 'claude', name: 'Claude', configured: false, connectionMode: 'api', supportsTools: false, status: 'unavailable' },
  { id: 'mistral', name: 'Mistral', configured: false, connectionMode: 'api', supportsTools: false, status: 'unavailable' },
];

const locations = new Map<string, string>();
const listeners = new Map<keyof IpcEvents, Set<(payload: unknown) => void>>();

export function isPreview(): boolean {
  return typeof window !== 'undefined' && !('__TAURI_INTERNALS__' in window)
    && new URLSearchParams(window.location.search).get('demo') === '1';
}

function publicError(code: AppError['code'], message: string, detail: string | null = null): AppError {
  return { code, message, retryable: false, detail };
}

function requirePreview(): void {
  if (!isPreview()) throw publicError('internal', 'Demo mode is available only in a browser with ?demo=1.');
}

function unavailable(command: IpcCommand): never {
  throw publicError('operationConflict', 'This action requires the desktop application and is unavailable in demo mode.', command);
}

function bookById(id: string): Book {
  const book = books.find((candidate) => candidate.id === id);
  if (!book) throw publicError('notFound', 'The demonstration book was not found.');
  return book;
}

function normalizeSearch(value: string): string {
  return value.normalize('NFKD').replace(/\p{M}/gu, '').toLocaleLowerCase('fr');
}

function overlaps(selected: readonly string[], values: readonly string[]): boolean {
  return selected.length === 0 || values.some((value) => selected.includes(value));
}

function matches(book: Book, query: BookQuery): boolean {
  const haystack = normalizeSearch([book.title, ...book.authors, book.series ?? '', ...book.genres, ...book.tags, book.description, book.isbn ?? ''].join(' '));
  const terms = normalizeSearch(query.search).trim().split(/\s+/u).filter(Boolean);
  const present = query.deviceId === null ? book.onDeviceIds.length > 0 : book.onDeviceIds.includes(query.deviceId);
  return terms.every((term) => haystack.includes(term))
    && overlaps(query.authors, book.authors)
    && overlaps(query.series, book.series === null ? [] : [book.series])
    && overlaps(query.genres, book.genres)
    && overlaps(query.tags, book.tags)
    && overlaps(query.languages, [book.language])
    && overlaps(query.formats, [book.format])
    && (query.onDevice === null ? query.deviceId === null || present : present === query.onDevice)
    && (query.readStatus === null || book.readStatus === query.readStatus)
    && (query.favorite === null || book.favorite === query.favorite)
    && (query.metadataStatus === null || book.metadataStatus === query.metadataStatus)
    && (query.missingCover === null || (book.coverPath === null) === query.missingCover)
    && (query.minSizeBytes === null || book.sizeBytes >= query.minSizeBytes)
    && (query.maxSizeBytes === null || book.sizeBytes <= query.maxSizeBytes);
}

function compareBooks(left: Book, right: Book, sort: BookQuery['sort']): number {
  switch (sort) {
    case 'title': return collator.compare(left.title, right.title);
    case 'author': return collator.compare(left.authorSort, right.authorSort) || collator.compare(left.title, right.title);
    case 'series': {
      if (left.series === null && right.series !== null) return 1;
      if (right.series === null && left.series !== null) return -1;
      return collator.compare(left.series ?? '', right.series ?? '')
        || (left.seriesIndex ?? -1) - (right.seriesIndex ?? -1)
        || collator.compare(left.title, right.title);
    }
    case 'added': return left.addedAt.localeCompare(right.addedAt);
    case 'updated': return left.updatedAt.localeCompare(right.updatedAt);
    case 'size': return left.sizeBytes - right.sizeBytes;
    case 'progress': return left.readingProgress - right.readingProgress;
    case 'published': return (left.published ?? '').localeCompare(right.published ?? '');
    case 'rating': return (left.rating ?? -1) - (right.rating ?? -1);
  }
}

function listBooks(query: BookQuery): BookPage {
  if (!Number.isSafeInteger(query.limit) || query.limit < 1 || query.limit > 200
    || !Number.isSafeInteger(query.offset) || query.offset < 0) {
    throw publicError('invalidInput', 'Page size must be 1..200 and the offset must be nonnegative.');
  }
  const found = books.filter((book) => matches(book, query));
  const direction = query.descending ? -1 : 1;
  found.sort((left, right) => direction * (compareBooks(left, right, query.sort) || left.id.localeCompare(right.id)));
  return { items: found.slice(query.offset, query.offset + query.limit), total: found.length, offset: query.offset, limit: query.limit };
}

function facet(values: readonly string[]): Facet[] {
  const counts = new Map<string, number>();
  for (const value of values) counts.set(value, (counts.get(value) ?? 0) + 1);
  return [...counts].map(([value, count]) => ({ value, count }))
    .sort((left, right) => collator.compare(left.value, right.value));
}

function facets(): LibraryFacets {
  return {
    authors: facet(books.flatMap((book) => book.authors)),
    series: facet(books.flatMap((book) => book.series === null ? [] : [book.series])),
    genres: facet(books.flatMap((book) => book.genres)),
    tags: facet(books.flatMap((book) => book.tags)),
    languages: facet(books.map((book) => book.language)),
    formats: facet(books.map((book) => book.format)),
    devices: facet(books.flatMap((book) => book.onDeviceIds)),
  };
}

function sectionTitles(book: Book): string[] {
  return book.language === 'fr' ? ['Un départ tranquille', 'Le passage des lanternes', 'Un horizon à inventer']
    : ['A Quiet Beginning', 'The Lantern Crossing', 'An Unwritten Horizon'];
}

function readerSection(id: string, sectionIndex: number): ReaderSection {
  const book = bookById(id);
  if (book.format !== 'epub') throw publicError('unsupportedFormat', 'The demo reader opens the fictional EPUB books only.');
  const title = sectionTitles(book)[sectionIndex];
  if (!Number.isSafeInteger(sectionIndex) || title === undefined) throw publicError('invalidInput', 'The chapter index is invalid.');
  const paragraphs = book.language === 'fr' ? [
    'Ce texte original est une démonstration du lecteur de Library Manager. Aucun extrait d’un livre publié ne figure dans cet aperçu.',
    'Au matin, une lumière douce se posa sur la table. Une carte attendait près de la fenêtre, avec un chemin que personne n’avait encore suivi. Le jardin gardait le souvenir d’une pluie légère.',
    'Elle prit le temps de regarder les lignes, les détours et les espaces laissés vides. Certaines histoires commencent par un grand voyage ; celle-ci commençait par l’attention portée aux choses proches.',
    'Plus loin, les lanternes s’allumaient une à une. Il suffisait peut-être de marcher sans précipitation, de garder une place pour les rencontres et de laisser le prochain paysage apparaître.',
  ] : [
    'This original text demonstrates the Library Manager reader. No excerpt from a published book is included in this preview.',
    'Morning light settled on the table. A map waited beside the window, showing a path that nobody had followed yet. The garden still remembered a little rain.',
    'She studied its lines, its turning points and its empty spaces. Some stories begin with a long journey; this one began with attention to the things close at hand.',
    'Beyond the garden, lanterns appeared one by one. Perhaps it was enough to walk without hurry, leave room for a meeting and let the next landscape reveal itself.',
  ];
  const html = `<!doctype html><html lang="${book.language}"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>${escapeMarkup(title)}</title><style>body{max-width:62ch;margin:0 auto;padding:48px 36px;font-family:Georgia,serif;font-size:19px;line-height:1.8;color:inherit;background:transparent}h1{font-size:2rem;line-height:1.3}p{margin:1.4em 0}small{font-family:system-ui,sans-serif;font-size:12px}</style></head><body><article><small>LIBRARY MANAGER · DEMO</small><h1>${escapeMarkup(title)}</h1>${paragraphs.map((paragraph) => `<p>${escapeMarkup(paragraph)}</p>`).join('')}</article></body></html>`;
  return { bookId: id, sectionIndex, html, resources: [], warnings: [] };
}

function readerManifest(id: string): ReaderManifest {
  const book = bookById(id);
  const titles = sectionTitles(book);
  if (book.format !== 'epub') throw publicError('unsupportedFormat', 'The demo reader opens the fictional EPUB books only.');
  return {
    bookId: id, title: book.title, authors: book.authors,
    sections: titles.map((title, index) => ({ index, title, sizeBytes: new TextEncoder().encode(readerSection(id, index).html).length })),
    toc: titles.map((label, sectionIndex) => ({ label, sectionIndex, fragment: null, children: [] })),
    savedLocation: locations.get(id) ?? null, savedProgress: book.readingProgress,
  };
}

function emit<Event extends keyof IpcEvents>(event: Event, payload: IpcEvents[Event]): void {
  for (const handler of listeners.get(event) ?? []) handler(structuredClone(payload));
}

type PreviewHandlers = {
  [Command in IpcCommand]: (params: IpcContracts[Command]['params']) => IpcContracts[Command]['result'];
};

const handlers: PreviewHandlers = {
  app_bootstrap: () => ({ version: '0.1.0', systemLanguage: typeof navigator === 'undefined' ? 'en' : navigator.language, settings, devices, pendingJobs: [] }),
  library_list: ({ query }) => listBooks(query),
  library_facets: () => facets(),
  book_get: ({ id }) => bookById(id),
  book_files: ({ id }) => {
    const book = bookById(id);
    return [{ id: `preview-file-${id}`, bookId: id, format: book.format, variant: 'original', profile: null, sizeBytes: book.sizeBytes, sha256: '', createdAt: book.addedAt }];
  },
  import_books: () => unavailable('import_books'),
  book_update: () => unavailable('book_update'),
  book_enrich: () => unavailable('book_enrich'),
  book_optimize: () => unavailable('book_optimize'),
  book_convert: () => unavailable('book_convert'),
  conversion_capabilities: () => ({ inputs: [], outputs: [], warnings: ['previewConversionUnavailable'] }),
  optimization_profiles: () => profiles,
  devices_scan: () => devices,
  device_index: () => unavailable('device_index'),
  device_connect_wireless: () => unavailable('device_connect_wireless'),
  device_disconnect: () => unavailable('device_disconnect'),
  device_transfer: () => unavailable('device_transfer'),
  reader_open: ({ id }) => readerManifest(id),
  reader_section: ({ id, sectionIndex }) => readerSection(id, sectionIndex),
  reader_save_progress: ({ id, location, progress }) => {
    const book = bookById(id);
    if (!Number.isFinite(progress) || progress < 0 || progress > 1 || location.length > 4096) {
      throw publicError('invalidInput', 'The reading position is invalid.');
    }
    book.readingProgress = progress;
    book.readStatus = progress === 1 ? 'finished' : progress > 0 ? 'reading' : 'unread';
    book.revision += 1;
    book.updatedAt = new Date().toISOString();
    locations.set(id, location);
    emit('reader:progress', { bookId: id, location, progress });
    emit('library:changed', { bookIds: [id], reason: 'previewReadingProgress' });
    return null;
  },
  providers_list: () => providers,
  provider_models: () => unavailable('provider_models'),
  provider_set_secret: () => unavailable('provider_set_secret'),
  provider_clear_secret: () => unavailable('provider_clear_secret'),
  conversations_list: () => [],
  conversation_messages: () => [],
  chat_send: () => unavailable('chat_send'),
  settings_get: () => settings,
  settings_save: ({ settings: next }) => {
    if (!Number.isFinite(next.autoApplyConfidence) || next.autoApplyConfidence < 0 || next.autoApplyConfidence > 1
      || !Number.isSafeInteger(next.maxConcurrentJobs) || next.maxConcurrentJobs < 1 || next.maxConcurrentJobs > 8
      || !['system', 'light', 'dark'].includes(next.theme)) {
      throw publicError('invalidInput', 'The demonstration settings are invalid.');
    }
    settings = structuredClone(next);
    return settings;
  },
  jobs_list: () => [],
  job_cancel: () => unavailable('job_cancel'),
  operations_list: () => [],
  operation_undo: () => unavailable('operation_undo'),
};

export async function requestPreview<Command extends IpcCommand>(
  command: Command,
  params: IpcParams<Command>,
): Promise<IpcResult<Command>> {
  requirePreview();
  // The mapped table keeps each command's parameter/result pair correlated.
  const handler: (input: IpcParams<Command>) => IpcResult<Command> = handlers[command];
  return structuredClone(handler(params));
}

export async function subscribePreview<Event extends keyof IpcEvents>(
  event: Event,
  handler: (payload: IpcEvents[Event]) => void,
): Promise<UnlistenFn> {
  requirePreview();
  const wrapper = (payload: unknown): void => handler(payload as IpcEvents[Event]);
  const subscribers = listeners.get(event) ?? new Set<(payload: unknown) => void>();
  subscribers.add(wrapper);
  listeners.set(event, subscribers);
  return () => {
    subscribers.delete(wrapper);
    if (subscribers.size === 0) listeners.delete(event);
  };
}
