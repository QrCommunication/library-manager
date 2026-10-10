<script module lang="ts">
  export interface LibraryViewState {
    query: import('../contracts').BookQuery;
    view: 'grid' | 'table';
    filtersVisible: boolean;
  }
</script>

<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { ArrowDownWideNarrow, BookOpen, ChevronLeft, ChevronRight, Grid2X2, Heart, ListFilter, NotebookText, Plus, RefreshCw, Star, Table2, Tablet, X } from '@lucide/svelte';
  import { isPreview, normalizePublicError, request } from '../api';
  import { defaultQuery } from '../contracts';
  import type { AppError, Book, BookFormat, BookPage, BookQuery, BookSort, Device, Job, LibraryFacets } from '../contracts';
  import { formatDate, formatSize, locale, t } from '../i18n';
  import { createRequestScheduler } from '../request-scheduler';
  import DeviceOnlyBooks from './DeviceOnlyBooks.svelte';
  import SelectionActions from './SelectionActions.svelte';
  import type { MetadataDisabledReason } from '../selection-capabilities';

  interface Props {
    initialQuery: BookQuery;
    initialState?: LibraryViewState | null;
    search: string;
    groupBy: 'none' | 'author' | 'series' | 'genre';
    refreshVersion: number;
    selectedBookIds: string[];
    importing: boolean;
    devices?: Device[];
    jobs?: Job[];
    onSelectionChange: (ids: string[]) => void;
    onOpenBook: (book: Book) => void;
    onReadBook: (book: Book) => void;
    onImport: () => Promise<void>;
    onNotify: (message: string) => void;
    onError: (error: unknown) => void;
    onStateChange?: (state: LibraryViewState) => void;
    onOpenAssistant?: () => void;
    onVerifySelected?: () => void;
    metadataReady?: boolean;
    metadataDisabledReason?: MetadataDisabledReason | null;
    transferReady?: boolean;
    onTransferSelected?: (initialDeviceId?: string) => void;
    onRemoveSelected?: () => void;
    onOpenSettings?: () => void;
  }

  let { initialQuery, initialState = null, search, groupBy, refreshVersion, selectedBookIds, importing, devices = [], jobs = [], onSelectionChange, onOpenBook, onReadBook, onImport, onNotify, onError, onStateChange, onOpenAssistant, onVerifySelected, metadataReady = false, metadataDisabledReason = null, transferReady = false, onTransferSelected, onRemoveSelected, onOpenSettings }: Props = $props();
  type FacetField = 'authors' | 'series' | 'genres' | 'tags' | 'languages' | 'formats';
  type ScalarFilter = 'deviceId' | 'onDevice' | 'readStatus' | 'favorite' | 'metadataStatus' | 'missingCover' | 'minSizeBytes' | 'maxSizeBytes';
  interface ActiveChip { field: FacetField | ScalarFilter; value: string | null; label: string }
  interface BookGroup { key: string; label: string; books: Book[] }
  const facetFields: FacetField[] = ['authors', 'series', 'genres', 'tags', 'languages', 'formats'];
  const formats: BookFormat[] = ['epub', 'mobi', 'azw3', 'fb2', 'txt', 'html', 'pdf', 'cbz'];
  const sorts: BookSort[] = ['title', 'author', 'series', 'added', 'updated', 'size', 'progress', 'published', 'rating'];
  const readStatuses = ['unread', 'reading', 'finished'] as const;
  const metadataStatuses = ['pending', 'verified', 'needsReview', 'failed'] as const;

  const restoredState = untrack(() => initialState);
  let appliedInitialQuery = untrack(() => initialQuery);
  let query = $state<BookQuery>(copyQuery(restoredState?.query ?? appliedInitialQuery));
  let page = $state<BookPage | null>(null);
  let facets = $state<LibraryFacets>({ authors: [], series: [], genres: [], tags: [], languages: [], formats: [], devices: [] });
  let deviceNames = $state<Record<string, string>>({});
  let view = $state<'grid' | 'table'>(restoredState?.view ?? 'grid');
  let filtersVisible = $state(restoredState?.filtersVisible ?? false);
  let loading = $state(true);
  let reviewLoading = $state(false);
  let disposed = false;
  let failure = $state<AppError | null>(null);
  let failedCoverIds = $state<string[]>([]);
  let retryVersion = $state(0);
  let selectPageInput = $state<HTMLInputElement>();

  const items = $derived(page?.items ?? []);
  const total = $derived(page?.total ?? 0);
  const proposalJobs = $derived(jobs.filter((job) => completedProposalBookId(job) !== null));
  const proposalBookIds = $derived.by(() => new Set(proposalJobs.map(completedProposalBookId).filter((id): id is string => id !== null)));
  const allVisibleSelected = $derived(items.length > 0 && items.every((book) => selectedBookIds.includes(book.id)));
  const someVisibleSelected = $derived(items.some((book) => selectedBookIds.includes(book.id)));
  const titleKey = $derived(groupBy === 'author' ? 'filters.authors' : groupBy === 'series' ? 'filters.series' : groupBy === 'genre' ? 'filters.genres' : query.favorite === true ? 'sidebar.favorites' : query.readStatus === 'reading' ? 'sidebar.reading' : 'library.title');
  const numberFormatter = $derived(new Intl.NumberFormat($locale));
  const percentFormatter = $derived(new Intl.NumberFormat($locale, { style: 'percent', maximumFractionDigits: 0 }));

  function copyQuery(source: BookQuery): BookQuery {
    return { ...source, authors: [...source.authors], series: [...source.series], genres: [...source.genres], tags: [...source.tags], languages: [...source.languages], formats: [...source.formats] };
  }

  function genreName(value: string): string {
    const key = `genres.${value}`;
    const label = $t(key);
    return label === key ? value : label;
  }

  function languageName(value: string): string {
    try { return new Intl.DisplayNames([$locale], { type: 'language' }).of(value) ?? value; }
    catch { return value; }
  }

  function facetName(field: FacetField, value: string): string {
    return field === 'genres' ? genreName(value) : field === 'languages' ? languageName(value) : field === 'formats' ? value.toUpperCase() : value;
  }

  function progressLabel(book: Book): string {
    return percentFormatter.format(book.readingProgress);
  }

  function seriesLabel(book: Book): string {
    if (book.series === null) return $t('book.noSeries');
    return book.seriesIndex === null ? book.series : `${book.series} · ${numberFormatter.format(book.seriesIndex)}`;
  }

  function devicesLabel(book: Book): string {
    return book.onDeviceIds.map((id) => deviceNames[id] ?? id).join(', ');
  }

  function coverSource(book: Book): string | null {
    return failedCoverIds.includes(book.id) ? null : book.coverPath;
  }

  function coverColor(book: Book): string {
    const colors = ['#274d53', '#554063', '#925341', '#405840', '#335772', '#815c31'];
    const index = [...book.title].reduce((sum, character) => sum + (character.codePointAt(0) ?? 0), 0) % colors.length;
    return colors[index] ?? '#274d53';
  }

  const groups = $derived.by((): BookGroup[] => {
    if (groupBy === 'none') return [{ key: 'all', label: '', books: items }];
    const grouped = new Map<string, Book[]>();
    for (const book of items) {
      const values = groupBy === 'author' ? book.authors : groupBy === 'series' ? [book.series ?? $t('book.noSeries')] : book.genres.map(genreName);
      for (const value of values.length > 0 ? values : [$t('common.unknown')]) {
        const group = grouped.get(value) ?? [];
        group.push(book);
        grouped.set(value, group);
      }
    }
    return [...grouped].sort(([left], [right]) => left.localeCompare(right, $locale, { numeric: true, sensitivity: 'base' }))
      .map(([label, books]) => ({ key: label, label, books }));
  });

  const chips = $derived.by((): ActiveChip[] => {
    const result: ActiveChip[] = [];
    for (const field of facetFields) for (const value of query[field]) result.push({ field, value, label: `${$t(`filters.${field}`)} : ${facetName(field, value)}` });
    if (query.deviceId !== null) result.push({ field: 'deviceId', value: null, label: deviceNames[query.deviceId] ?? query.deviceId });
    if (query.onDevice !== null) result.push({ field: 'onDevice', value: null, label: $t(query.onDevice ? 'filters.present' : 'filters.absent') });
    if (query.readStatus !== null) result.push({ field: 'readStatus', value: null, label: $t(`book.${query.readStatus}`) });
    if (query.favorite !== null) result.push({ field: 'favorite', value: null, label: $t('filters.favorite') });
    if (query.metadataStatus !== null) result.push({ field: 'metadataStatus', value: null, label: $t(`book.${query.metadataStatus}`) });
    if (query.missingCover !== null) result.push({ field: 'missingCover', value: null, label: $t('filters.missingCover') });
    if (query.minSizeBytes !== null) result.push({ field: 'minSizeBytes', value: null, label: `${$t('filters.minSize')} : ${formatSize(query.minSizeBytes)}` });
    if (query.maxSizeBytes !== null) result.push({ field: 'maxSizeBytes', value: null, label: `${$t('filters.maxSize')} : ${formatSize(query.maxSizeBytes)}` });
    return result;
  });

  function resetOffset(): void { query.offset = 0; }

  function facetSelected(field: FacetField, value: string): boolean {
    return field === 'formats' ? query.formats.some((format) => format === value) : query[field].includes(value);
  }

  function toggleFacet(field: FacetField, value: string): void {
    if (field === 'formats') {
      const format = formats.find((candidate) => candidate === value);
      if (!format) return;
      query.formats = query.formats.includes(format) ? query.formats.filter((candidate) => candidate !== format) : [...query.formats, format];
    } else query[field] = query[field].includes(value) ? query[field].filter((candidate) => candidate !== value) : [...query[field], value];
    resetOffset();
  }

  function isFacetField(field: ActiveChip['field']): field is FacetField { return facetFields.some((candidate) => candidate === field); }

  function removeChip(chip: ActiveChip): void {
    if (isFacetField(chip.field)) toggleFacet(chip.field, chip.value ?? '');
    else { query[chip.field] = null; resetOffset(); }
  }

  function resetFilters(): void {
    query = { ...defaultQuery(initialQuery.limit), search, sort: initialQuery.sort };
    onNotify($t('filters.clear'));
  }

  function updateSize(field: 'minSizeBytes' | 'maxSizeBytes', value: string): void {
    const number = Number(value);
    if (value !== '' && (!Number.isFinite(number) || number < 0)) return;
    query[field] = value === '' ? null : Math.round(number * 1024 ** 2);
    resetOffset();
  }

  function toggleSelection(id: string): void {
    onSelectionChange(selectedBookIds.includes(id) ? selectedBookIds.filter((candidate) => candidate !== id) : [...selectedBookIds, id]);
  }

  function toggleVisibleSelection(): void {
    const ids = new Set(items.map((book) => book.id));
    onSelectionChange(allVisibleSelected ? selectedBookIds.filter((id) => !ids.has(id)) : [...new Set([...selectedBookIds, ...ids])]);
  }

  function completedProposalBookId(job: Job): string | null {
    const record = (value: unknown): value is Record<string, unknown> => typeof value === 'object' && value !== null && !Array.isArray(value);
    const strings = (value: unknown): value is string[] => Array.isArray(value) && value.every((item) => typeof item === 'string');
    const confidence = (value: unknown): boolean => typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 1;
    if (job.kind !== 'enrich' || job.status !== 'completed') return null;
    if (record(job.result) && 'review' in job.result) {
      const review = job.result.review;
      if (!record(review) || review.state !== 'pending' || typeof review.reviewRevision !== 'number'
        || !Number.isSafeInteger(review.reviewRevision) || review.reviewRevision < 0) return null;
    }
    const candidate = record(job.result) && 'proposal' in job.result ? job.result.proposal : job.result;
    if (!record(candidate) || typeof candidate.bookId !== 'string' || !job.bookIds.includes(candidate.bookId)
      || !record(candidate.patch) || !confidence(candidate.confidence) || !strings(candidate.warnings)
      || typeof candidate.modelId !== 'string' || !['zai', 'kimi', 'minimax', 'codex', 'claude', 'mistral'].includes(String(candidate.providerId))
      || !Array.isArray(candidate.evidence)) return null;
    const entries = Object.entries(candidate.patch);
    if (!entries.length || !entries.every(([field, value]) => {
      if (['title', 'authorSort', 'language', 'description'].includes(field)) return typeof value === 'string';
      if (['series', 'isbn', 'publisher', 'published'].includes(field)) return value === null || typeof value === 'string';
      if (['authors', 'genres', 'tags'].includes(field)) return strings(value);
      return field === 'seriesIndex' && (value === null || (typeof value === 'number' && Number.isFinite(value)));
    })) return null;
    if (!candidate.evidence.every((evidence) => record(evidence) && typeof evidence.field === 'string'
      && typeof evidence.value === 'string' && confidence(evidence.confidence) && strings(evidence.sourceUrls)
      && evidence.sourceUrls.every((value) => {
        try {
          const url = new URL(value);
          return ['https:', 'http:'].includes(url.protocol) && !url.username && !url.password && !/[\u0000-\u001f\u007f]/u.test(value);
        } catch { return false; }
      }))) return null;
    return candidate.bookId;
  }

  function hasCurrentProposal(book: Book): boolean {
    return proposalJobs.some((job) => {
      const result = job.result;
      const candidate = typeof result === 'object' && result !== null && !Array.isArray(result) && 'proposal' in result ? result.proposal : result;
      if (typeof candidate !== 'object' || candidate === null || Array.isArray(candidate)
        || !('bookId' in candidate) || candidate.bookId !== book.id) return false;
      if (typeof result === 'object' && result !== null && !Array.isArray(result) && 'review' in result) {
        const review = result.review;
        return typeof review === 'object' && review !== null && !Array.isArray(review)
          && 'reviewRevision' in review && review.reviewRevision === book.revision;
      }
      return book.metadataStatus === 'needsReview';
    });
  }

  async function openReview(): Promise<void> {
    if (reviewLoading || disposed) return;
    reviewLoading = true;
    try {
      const visiblePendingIds = new Set(items.filter(hasCurrentProposal).map((book) => book.id));
      const ids = [...proposalBookIds].sort((left, right) => Number(visiblePendingIds.has(right)) - Number(visiblePendingIds.has(left)));
      for (const id of ids) {
        let current: Book;
        try { current = await request('book_get', { id }); }
        catch (error) {
          if (normalizePublicError(error).code === 'notFound') continue;
          throw error;
        }
        if (disposed) return;
        if (hasCurrentProposal(current)) { onOpenBook(current); return; }
      }
      if (!disposed) onNotify($t('library.noReviewProposals'));
    } catch (error) {
      if (!disposed) onError(error);
    } finally {
      if (!disposed) reviewLoading = false;
    }
  }

  function setSort(sort: BookSort): void {
    if (query.sort === sort) query.descending = !query.descending;
    else { query.sort = sort; query.descending = false; }
    resetOffset();
  }

  function sortState(sort: BookSort): 'ascending' | 'descending' | 'none' {
    return query.sort === sort ? query.descending ? 'descending' : 'ascending' : 'none';
  }

  function loadFacets() {
    return Promise.allSettled([request('library_facets', undefined), request('devices_scan', undefined)]);
  }

  const pageRequests = createRequestScheduler<BookQuery, BookPage>({
    load: (nextQuery) => request('library_list', { query: nextQuery }),
    onStart: () => {},
    onSuccess: (result) => { page = result; failure = null; },
    onError: (error) => { failure = normalizePublicError(error); },
    onSettled: () => { loading = false; },
  });
  const facetRequests = createRequestScheduler<undefined, Awaited<ReturnType<typeof loadFacets>>>({
    load: loadFacets,
    onStart: () => {},
    onSuccess: ([facetResult, deviceResult]) => {
      if (facetResult.status === 'fulfilled') facets = facetResult.value;
      else onError(facetResult.reason);
      if (deviceResult.status === 'fulfilled') deviceNames = Object.fromEntries(deviceResult.value.map((device) => [device.id, device.label]));
    },
    onError: (error) => onError(error),
    onSettled: () => {},
  });

  function importBooks(): void { void onImport().catch(onError); }

  $effect(() => {
    const source = initialQuery;
    untrack(() => {
      if (source === appliedInitialQuery) return;
      appliedInitialQuery = source;
      query = { ...copyQuery(source), search };
    });
  });
  $effect(() => {
    const value = search;
    untrack(() => { if (query.search !== value) { query.search = value; resetOffset(); } });
  });
  $effect(() => {
    const state: LibraryViewState = { query: copyQuery(query), view, filtersVisible };
    untrack(() => { onStateChange?.(state); });
  });
  $effect(() => {
    const next = copyQuery(query);
    void retryVersion;
    untrack(() => { loading = true; failure = null; pageRequests.setQuery(next); });
  });
  $effect(() => { untrack(() => facetRequests.setQuery(undefined)); });
  $effect(() => {
    void refreshVersion;
    untrack(() => { pageRequests.refresh(); facetRequests.refresh(); });
  });
  $effect(() => { if (selectPageInput) selectPageInput.indeterminate = someVisibleSelected && !allVisibleSelected; });
  onDestroy(() => {
    disposed = true;
    onStateChange?.({ query: copyQuery(query), view, filtersVisible });
    pageRequests.dispose(); facetRequests.dispose();
  });
</script>

<section class="page" aria-label={$t(titleKey)} aria-busy={loading}>
  <div class="page-header">
    <div><span class="eyebrow">{$t('app.name')}</span><h1 class="page-title">{$t(titleKey)}</h1><p class="page-subtitle">{$t('library.bookCount', { count: total })}</p></div>
    <button class="button primary" disabled={importing || isPreview()} onclick={importBooks}><Plus size={17} aria-hidden="true" />{$t('library.import')}</button>
  </div>

  <div class="toolbar">
    <button class="filter-chip" class:active={filtersVisible || chips.length > 0} aria-expanded={filtersVisible} aria-controls="library-facets" onclick={() => { filtersVisible = !filtersVisible; }}><ListFilter size={16} aria-hidden="true" />{$t('filters.title')}{#if chips.length > 0}<span class="badge accent">{chips.length}</span>{/if}</button>
    <label class="checkbox-field"><input bind:this={selectPageInput} type="checkbox" checked={allVisibleSelected} disabled={items.length === 0} onchange={toggleVisibleSelection} />{$t('library.selectAll')}</label>
    <button class="button secondary" disabled={reviewLoading} aria-busy={reviewLoading} onclick={openReview}><NotebookText size={16} aria-hidden="true" />{$t(reviewLoading ? 'common.loading' : 'library.reviewProposals')}</button>
    <div class="toolbar-spacer"></div>
    <label class="sr-only" for="library-sort">{$t('sort.label')}</label>
    <select id="library-sort" class="select sort-select" bind:value={query.sort} onchange={resetOffset}>{#each sorts as sort}<option value={sort}>{$t(`sort.${sort}`)}</option>{/each}</select>
    <button class="icon-button" class:active={query.descending} aria-pressed={query.descending} aria-label={$t(query.descending ? 'sort.descending' : 'sort.ascending')} title={$t(query.descending ? 'sort.descending' : 'sort.ascending')} onclick={() => { query.descending = !query.descending; resetOffset(); }}><ArrowDownWideNarrow size={19} aria-hidden="true" /></button>
    <div class="segmented" role="group" aria-label={$t('library.title')}><button class="icon-button" class:active={view === 'grid'} aria-pressed={view === 'grid'} aria-label={$t('library.grid')} title={$t('library.grid')} onclick={() => { view = 'grid'; }}><Grid2X2 size={19} aria-hidden="true" /></button><button class="icon-button" class:active={view === 'table'} aria-pressed={view === 'table'} aria-label={$t('library.table')} title={$t('library.table')} onclick={() => { view = 'table'; }}><Table2 size={19} aria-hidden="true" /></button></div>
  </div>

  {#if filtersVisible}
    <div id="library-facets" class="facets">
      {#each facetFields as field}
        <fieldset class="facet-field"><legend>{$t(`filters.${field}`)}</legend><div class="facet-list">
          {#each facets[field] as facet (facet.value)}<label class="checkbox-field"><span class="row grow"><input type="checkbox" checked={facetSelected(field, facet.value)} onchange={() => toggleFacet(field, facet.value)} /><span>{facetName(field, facet.value)}</span></span><span class="badge">{numberFormatter.format(facet.count)}</span></label>{/each}
          {#if facets[field].length === 0}<span class="muted small">{$t('common.none')}</span>{/if}
        </div></fieldset>
      {/each}
      <div class="field"><label for="filter-read">{$t('filters.readStatus')}</label><select id="filter-read" class="select" bind:value={query.readStatus} onchange={resetOffset}><option value={null}>{$t('filters.all')}</option>{#each readStatuses as status}<option value={status}>{$t(`book.${status}`)}</option>{/each}</select></div>
      <div class="field"><label for="filter-metadata">{$t('filters.metadataStatus')}</label><select id="filter-metadata" class="select" bind:value={query.metadataStatus} onchange={resetOffset}><option value={null}>{$t('filters.all')}</option>{#each metadataStatuses as status}<option value={status}>{$t(`book.${status}`)}</option>{/each}</select></div>
      <div class="field"><label for="filter-device">{$t('filters.device')}</label><select id="filter-device" class="select" bind:value={query.deviceId} onchange={resetOffset}><option value={null}>{$t('filters.all')}</option>{#each facets.devices as device (device.value)}<option value={device.value}>{deviceNames[device.value] ?? device.value} ({device.count})</option>{/each}</select></div>
      <div class="field"><label for="filter-presence">{$t('filters.presence')}</label><select id="filter-presence" class="select" bind:value={query.onDevice} onchange={resetOffset}><option value={null}>{$t('filters.all')}</option><option value={true}>{$t('filters.present')}</option><option value={false}>{$t('filters.absent')}</option></select></div>
      <div class="field"><label for="filter-min-size">{$t('filters.minSize')} (MiB)</label><input id="filter-min-size" class="input" type="number" min="0" step="0.1" value={query.minSizeBytes === null ? '' : query.minSizeBytes / 1024 ** 2} oninput={(event) => updateSize('minSizeBytes', event.currentTarget.value)} /></div>
      <div class="field"><label for="filter-max-size">{$t('filters.maxSize')} (MiB)</label><input id="filter-max-size" class="input" type="number" min="0" step="0.1" value={query.maxSizeBytes === null ? '' : query.maxSizeBytes / 1024 ** 2} oninput={(event) => updateSize('maxSizeBytes', event.currentTarget.value)} /></div>
      <label class="checkbox-field"><input type="checkbox" checked={query.favorite === true} onchange={(event) => { query.favorite = event.currentTarget.checked ? true : null; resetOffset(); }} />{$t('filters.favorite')}</label>
      <label class="checkbox-field"><input type="checkbox" checked={query.missingCover === true} onchange={(event) => { query.missingCover = event.currentTarget.checked ? true : null; resetOffset(); }} />{$t('filters.missingCover')}</label>
      <button class="button ghost" onclick={resetFilters}><X size={16} aria-hidden="true" />{$t('filters.clear')}</button>
    </div>
  {/if}

  {#if chips.length > 0}<div class="active-filters" aria-label={$t('filters.active', { count: chips.length })}>{#each chips as chip (`${chip.field}:${chip.value}`)}<button class="filter-chip active" aria-label={`${$t('actions.remove')} : ${chip.label}`} onclick={() => removeChip(chip)}><span>{chip.label}</span><X size={14} aria-hidden="true" /></button>{/each}<button class="button ghost" onclick={resetFilters}>{$t('filters.clear')}</button></div>{/if}
  <SelectionActions selectedCount={selectedBookIds.length} {metadataReady} {metadataDisabledReason} {transferReady}
    demo={isPreview()} busy={importing} {onOpenAssistant} {onVerifySelected} {onTransferSelected} {onRemoveSelected}
    {onOpenSettings} onClearSelection={() => onSelectionChange([])} />

  {#if failure}
    <div class="error-banner" role="alert"><span class="grow">{$t(`errors.${failure.code}`)}</span><button class="button ghost" onclick={() => { retryVersion += 1; }}><RefreshCw size={16} aria-hidden="true" />{$t('actions.retry')}</button></div>
  {/if}
  {#if loading && !page}<div class="empty-state" role="status"><BookOpen size={40} aria-hidden="true" /><p>{$t('common.loading')}</p></div>
  {:else if !failure && items.length === 0}
    <div class="empty-state"><BookOpen size={48} aria-hidden="true" /><h2>{$t(query.search || chips.length > 0 ? 'library.noResults' : 'library.emptyTitle')}</h2><p>{$t(query.search || chips.length > 0 ? 'search.label' : 'library.emptyDescription')}</p>{#if query.search || chips.length > 0}<button class="button secondary" onclick={resetFilters}>{$t('filters.clear')}</button>{:else}<button class="button primary" disabled={importing || isPreview()} onclick={importBooks}><Plus size={17} aria-hidden="true" />{$t('library.import')}</button>{/if}</div>
  {:else}
    {#each groups as group (group.key)}
      {#if group.label}<div class="group-header"><h2>{group.label}</h2><span class="badge">{$t('library.bookCount', { count: group.books.length })}</span></div>{/if}
      {#if view === 'grid'}
        <div class="book-grid">
          {#each group.books as book (book.id)}
            <article class="book-card" class:is-selected={selectedBookIds.includes(book.id)} class:on-device={book.onDeviceIds.length > 0}>
              <button class="book-cover" aria-label={book.title} onclick={() => onOpenBook(book)}>
                {#if coverSource(book)}<img src={coverSource(book) ?? ''} alt="" loading="lazy" decoding="async" onerror={() => { failedCoverIds = [...failedCoverIds, book.id]; }} />{:else}<div class="cover-placeholder" style:background={coverColor(book)}><span>{book.genres.map(genreName).join(' · ')}</span><strong>{book.title}</strong><span>{book.authors.join(', ')}</span></div>{/if}
              </button>
              <label class="selection-control"><input type="checkbox" checked={selectedBookIds.includes(book.id)} aria-label={book.title} onchange={() => toggleSelection(book.id)} /></label>
              <div class="cover-actions"><button class="icon-button" aria-label={`${$t('actions.read')} : ${book.title}`} title={$t('actions.read')} onclick={() => onReadBook(book)}><BookOpen size={17} aria-hidden="true" /></button></div>
              <button class="book-title" onclick={() => onOpenBook(book)}>{book.title}</button>
              <p class="book-author truncate" title={book.authors.join(', ')}>{book.authors.join(', ')}</p>
              {#if book.series}<p class="series-line truncate" title={seriesLabel(book)}>{seriesLabel(book)}</p>{/if}
              <div class="book-meta"><span>{book.format.toUpperCase()} · {formatSize(book.sizeBytes)}</span><span class="row flags">{#if book.favorite}<Heart size={13} fill="currentColor" aria-label={$t('sidebar.favorites')} />{/if}{#if book.notes}<NotebookText size={13} aria-label={$t('book.notes')} title={book.notes} />{/if}{#if book.rating !== null}<Star size={12} aria-label={$t('book.rating')} /><span>{numberFormatter.format(book.rating)}</span>{/if}</span></div>
              <div class="book-badges">{#if book.onDeviceIds.length > 0}<span class="badge accent" title={devicesLabel(book)}><Tablet size={12} aria-hidden="true" />{$t('library.onDevice')}</span>{/if}{#if book.metadataStatus === 'needsReview' || book.metadataStatus === 'failed'}<span class="badge" class:warning={book.metadataStatus === 'needsReview'} class:danger={book.metadataStatus === 'failed'}>{$t(`book.${book.metadataStatus}`)}</span>{/if}</div>
              {#if hasCurrentProposal(book)}<button class="button secondary review-action" aria-label={`${$t('library.reviewProposal')} : ${book.title}`} onclick={() => onOpenBook(book)}><NotebookText size={15} aria-hidden="true" />{$t('library.reviewProposal')}</button>{/if}
              {#if book.readStatus !== 'unread'}<div class="reading-progress"><progress max="1" value={book.readingProgress} aria-label={$t('reader.progress', { progress: progressLabel(book) })}></progress><span>{progressLabel(book)}</span></div>{/if}
            </article>
          {/each}
        </div>
      {:else}
        <div class="table-wrap"><table class="data-table"><caption class="sr-only">{$t('library.table')}</caption><thead><tr>
          <th scope="col"><span class="sr-only">{$t('library.selectAll')}</span></th>
          <th scope="col" aria-sort={sortState('title')}><button class="table-sort" onclick={() => setSort('title')}>{$t('book.title')}</button></th>
          <th scope="col" aria-sort={sortState('author')}><button class="table-sort" onclick={() => setSort('author')}>{$t('book.authors')}</button></th>
          <th scope="col" aria-sort={sortState('series')}><button class="table-sort" onclick={() => setSort('series')}>{$t('book.series')}</button></th>
          <th scope="col">{$t('book.genres')}</th><th scope="col">{$t('book.language')}</th><th scope="col">{$t('book.format')}</th>
          <th scope="col" aria-sort={sortState('size')}><button class="table-sort" onclick={() => setSort('size')}>{$t('book.size')}</button></th>
          <th scope="col" aria-sort={sortState('progress')}><button class="table-sort" onclick={() => setSort('progress')}>{$t('common.progress')}</button></th>
          <th scope="col" aria-sort={sortState('added')}><button class="table-sort" onclick={() => setSort('added')}>{$t('sort.added')}</button></th>
          <th scope="col">{$t('filters.device')}</th><th scope="col"><span class="sr-only">{$t('actions.read')}</span></th>
        </tr></thead><tbody>{#each group.books as book (book.id)}<tr class:on-device={book.onDeviceIds.length > 0}>
          <td><label class="table-check"><input type="checkbox" checked={selectedBookIds.includes(book.id)} aria-label={book.title} onchange={() => toggleSelection(book.id)} /></label></td>
          <td><button class="table-book" onclick={() => onOpenBook(book)}>{#if coverSource(book)}<img class="table-cover" src={coverSource(book) ?? ''} alt="" loading="lazy" onerror={() => { failedCoverIds = [...failedCoverIds, book.id]; }} />{/if}<span class="table-title">{book.title}{#if book.favorite}<Heart size={12} fill="currentColor" aria-label={$t('sidebar.favorites')} />{/if}</span></button>{#if hasCurrentProposal(book)}<button class="button secondary review-action" aria-label={`${$t('library.reviewProposal')} : ${book.title}`} onclick={() => onOpenBook(book)}><NotebookText size={15} aria-hidden="true" />{$t('library.reviewProposal')}</button>{/if}</td>
          <td>{book.authors.join(', ')}</td><td>{seriesLabel(book)}</td><td>{book.genres.map(genreName).join(', ')}</td><td>{book.language.toUpperCase()}</td><td><span class="badge">{book.format.toUpperCase()}</span></td><td>{formatSize(book.sizeBytes)}</td><td>{progressLabel(book)}</td><td>{formatDate(book.addedAt)}</td><td>{#if book.onDeviceIds.length > 0}<span class="badge accent" title={devicesLabel(book)}><Tablet size={12} aria-hidden="true" />{$t('filters.present')}</span>{:else}<span class="muted">—</span>{/if}</td><td><button class="icon-button" aria-label={`${$t('actions.read')} : ${book.title}`} onclick={() => onReadBook(book)}><BookOpen size={18} aria-hidden="true" /></button></td>
        </tr>{/each}</tbody></table></div>
      {/if}
    {/each}
    {#if total > 0}<div class="pagination"><span class="muted small" aria-live="polite">{new Intl.NumberFormat($locale).format((page?.offset ?? 0) + 1)}–{new Intl.NumberFormat($locale).format((page?.offset ?? 0) + items.length)} / {new Intl.NumberFormat($locale).format(total)}</span><div class="row"><button class="button secondary" disabled={loading || query.offset === 0} onclick={() => { query.offset = Math.max(0, query.offset - query.limit); }}><ChevronLeft size={16} aria-hidden="true" />{$t('common.back')}</button><button class="button secondary" disabled={loading || query.offset + items.length >= total} onclick={() => { query.offset += query.limit; }}>{$t('library.loadMore')}<ChevronRight size={16} aria-hidden="true" /></button></div></div>{/if}
  {/if}
  <DeviceOnlyBooks {devices} {jobs} {refreshVersion} {search} {onNotify} {onError} />
</section>

<style>
  .sort-select { width: auto; min-width: 155px; }
  .review-action { margin-top: 8px; }
  .facet-field { min-width: 0; margin: 0; padding: 0; border: 0; }
  .facet-field legend { padding: 0; margin-bottom: 10px; color: var(--text-muted); font-size: 12px; font-weight: 650; }
  .facet-list .checkbox-field { min-width: 0; font-size: 12px; }
  .facet-list .row { gap: 8px; }
  .active-filters { display: flex; flex-wrap: wrap; gap: 8px; margin-bottom: 22px; }
  .group-header { display: flex; align-items: center; gap: 12px; margin: 32px 0 22px; }
  .book-title { min-height: 44px; width: 100%; padding: 0; text-align: start; }
  .series-line { margin-top: 5px; font-size: 11px; color: var(--text-muted); }
  .flags { gap: 4px; color: var(--accent-ink); }
  .reading-progress { display: flex; align-items: center; gap: 8px; margin-top: 11px; font-size: 10px; color: var(--text-muted); }
  .reading-progress progress { flex: 1; min-width: 0; }
  .table-check { display: flex; justify-content: center; align-items: center; min-width: 44px; min-height: 44px; cursor: pointer; }
  .table-book { display: flex; align-items: center; gap: 12px; min-width: 180px; max-width: 300px; min-height: 44px; padding: 0; text-align: start; }
  .table-title { display: flex; align-items: center; gap: 7px; }
  .table-sort { display: flex; align-items: center; min-height: 44px; padding: 0; font-size: inherit; font-weight: inherit; letter-spacing: inherit; text-transform: inherit; }
  .data-table td { font-size: 12px; }
  .pagination { display: flex; align-items: center; justify-content: space-between; gap: 16px; margin-top: 32px; }
  @media (max-width: 680px) { .pagination { flex-direction: column; align-items: flex-start; } .sort-select { flex: 1; min-width: 135px; } }
</style>
