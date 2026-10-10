<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte';
  import { BookOpen, Check, ExternalLink, FileText, RefreshCw, Sparkles, X } from '@lucide/svelte';
  import { isPreview, normalizePublicError, openExternal, PublicError, request } from '../api';
  import type { Book, BookFile, BookFormat, BookPatch, ConversionCapabilities, Job, MetadataProposal, OptimizationProfile, ReadStatus } from '../contracts';
  import { formatDate, formatProviderDiagnostic, formatSize, locale, t } from '../i18n';
  import { createRequestScheduler } from '../request-scheduler';

  interface Props {
    book: Book;
    refreshVersion: number;
    onClose(): void;
    onReadBook(book: Book): void;
    onUpdated(book: Book): void;
    onNotify(message: string): void;
    onError(error: unknown): void;
  }
  interface Draft {
    title: string; authors: string; authorSort: string; series: string; seriesIndex: string;
    genres: string; tags: string; language: string; description: string; isbn: string;
    publisher: string; published: string; notes: string; readStatus: ReadStatus;
    favorite: boolean; rating: string;
  }

  let { book, refreshVersion, onClose, onReadBook, onUpdated, onNotify, onError }: Props = $props();
  let dialog: HTMLDialogElement;
  let previousFocus: HTMLElement | null = null;
  let disposed = false;
  let generation = 0;
  let activeBookId: string | null = null;
  let discardRequested = false;
  let baseline = $state<Book | null>(null);
  let remoteBook = $state<Book | null>(null);
  let draft = $state<Draft>(emptyDraft());
  let files = $state<BookFile[]>([]);
  let profiles = $state<OptimizationProfile[]>([]);
  let capabilities = $state<ConversionCapabilities | null>(null);
  let proposal = $state<MetadataProposal | null>(null);
  let warnings = $state<string[]>([]);
  let latestJob = $state<Job | null>(null);
  let selectedProfile = $state('');
  let selectedFormat = $state<BookFormat | ''>('');
  let loading = $state(true);
  let busy = $state(false);
  let conflict = $state(false);
  let failure = $state<PublicError | null>(null);
  const demo = isPreview();
  const displayedBook = $derived(baseline ?? book);
  const dirty = $derived(baseline !== null && JSON.stringify(draft) !== JSON.stringify(draftFromBook(baseline)));
  const chosenProfile = $derived(profiles.find((profile) => profile.id === selectedProfile));
  const proposalPatch = $derived(proposal ? changedProposalPatch(proposal.patch) : {});
  const proposalEntries = $derived(Object.entries(proposalPatch));
  const conversionAvailable = $derived(capabilities?.inputs.includes(displayedBook.format) === true);
  const percent = $derived(new Intl.NumberFormat($locale, { style: 'percent', maximumFractionDigits: 0 }));
  const failureDetail = $derived(failure ? errorDetail(failure) : null);
  const latestJobDetail = $derived(latestJob?.error ? errorDetail(latestJob.error) : null);

  function errorDetail(error: { code: string; detail: string | null }): string | null {
    return error.code === 'providerError' ? formatProviderDiagnostic(error.code, error.detail, $locale) : error.detail;
  }

  function emptyDraft(): Draft {
    return { title: '', authors: '', authorSort: '', series: '', seriesIndex: '', genres: '', tags: '', language: '', description: '', isbn: '', publisher: '', published: '', notes: '', readStatus: 'unread', favorite: false, rating: '' };
  }

  function draftFromBook(value: Book): Draft {
    return {
      title: value.title, authors: value.authors.join('; '), authorSort: value.authorSort,
      series: value.series ?? '', seriesIndex: value.seriesIndex === null ? '' : String(value.seriesIndex),
      genres: value.genres.join(', '), tags: value.tags.join(', '), language: value.language,
      description: value.description, isbn: value.isbn ?? '', publisher: value.publisher ?? '',
      published: value.published ?? '', notes: value.notes, readStatus: value.readStatus,
      favorite: value.favorite, rating: value.rating === null ? '' : String(value.rating),
    };
  }

  function acceptBook(value: Book): void {
    baseline = value;
    draft = draftFromBook(value);
    remoteBook = null;
    conflict = false;
  }

  function showFailure(error: unknown): void {
    failure = normalizePublicError(error);
    onError(failure);
  }

  async function loadData(id: string) {
    const discardDraft = discardRequested;
    discardRequested = false;
    const results = await Promise.allSettled([
      request('book_get', { id }), request('book_files', { id }), request('jobs_list', undefined),
      request('optimization_profiles', undefined), request('conversion_capabilities', undefined),
    ] as const);
    return { results, discardDraft };
  }

  function applyData({ results, discardDraft }: Awaited<ReturnType<typeof loadData>>, id: string): void {
    if (disposed || book.id !== id) return;
    const [bookResult, filesResult, jobsResult, profilesResult, capabilitiesResult] = results;
    if (bookResult.status === 'fulfilled') {
      // A refresh started before a successful save must not restore an older revision.
      if (baseline?.id !== id || bookResult.value.revision >= baseline.revision) {
        if (discardDraft || baseline?.id !== id || !dirty) acceptBook(bookResult.value);
        else if (bookResult.value.revision !== baseline.revision) {
          remoteBook = bookResult.value;
          conflict = true;
        }
      }
    } else showFailure(bookResult.reason);
    if (filesResult.status === 'fulfilled') files = filesResult.value;
    else showFailure(filesResult.reason);
    if (profilesResult.status === 'fulfilled') {
      profiles = profilesResult.value;
      if (!profiles.some((profile) => profile.id === selectedProfile)) {
        selectedProfile = profiles.find((profile) => profile.id === 'xteink')?.id ?? profiles[0]?.id ?? '';
      }
    } else showFailure(profilesResult.reason);
    if (capabilitiesResult.status === 'fulfilled') {
      capabilities = capabilitiesResult.value;
      if (!capabilities.outputs.some((format) => format === selectedFormat)) selectedFormat = capabilities.outputs[0] ?? '';
    } else showFailure(capabilitiesResult.reason);
    if (jobsResult.status === 'fulfilled') {
      const bookJobs = jobsResult.value.filter((job) => job.bookIds.includes(id)).sort((left, right) => right.updatedAt.localeCompare(left.updatedAt));
      latestJob = bookJobs[0] ?? null;
      proposal = bookJobs.filter((job) => job.kind === 'enrich' && job.status === 'completed')
        .map((job) => proposalFromResult(job.result, id)).find((value) => value !== null) ?? null;
      warnings = [...new Set([...bookJobs.flatMap((job) => resultWarnings(job.result)), ...(proposal?.warnings ?? [])])];
    } else showFailure(jobsResult.reason);
  }

  const scheduler = createRequestScheduler<string, Awaited<ReturnType<typeof loadData>>>({
    load: loadData,
    onStart: () => { loading = baseline === null || discardRequested; },
    onSuccess: applyData,
    onError: showFailure,
    onSettled: () => { loading = false; },
    queryDelayMs: 0,
  });

  $effect(() => {
    const id = book.id;
    untrack(() => {
      if (activeBookId === id) return;
      activeBookId = id;
      generation += 1;
      discardRequested = false;
      baseline = null;
      remoteBook = null;
      draft = emptyDraft();
      files = [];
      proposal = null;
      warnings = [];
      latestJob = null;
      failure = null;
      busy = false;
      conflict = false;
      loading = true;
      scheduler.setQuery(id);
    });
  });

  $effect(() => {
    void refreshVersion;
    untrack(() => scheduler.refresh());
  });

  onMount(() => {
    previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.showModal();
  });
  onDestroy(() => {
    disposed = true;
    generation += 1;
    scheduler.dispose();
    dialog?.close();
    if (previousFocus?.isConnected) previousFocus.focus();
  });

  function isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  }
  function isStringArray(value: unknown): value is string[] {
    return Array.isArray(value) && value.every((item: unknown) => typeof item === 'string');
  }
  function isConfidence(value: unknown): value is number {
    return typeof value === 'number' && Number.isFinite(value) && value >= 0 && value <= 1;
  }
  function isPublicUrl(value: string): boolean {
    try {
      const url = new URL(value);
      return ['https:', 'http:'].includes(url.protocol) && !url.username && !url.password && !/[\u0000-\u001f\u007f]/u.test(value);
    } catch { return false; }
  }
  function isBookPatch(value: unknown): value is BookPatch {
    if (!isRecord(value)) return false;
    const strings = ['title', 'authorSort', 'language', 'description', 'notes'];
    const nullableStrings = ['series', 'isbn', 'publisher', 'published'];
    const arrays = ['authors', 'genres', 'tags'];
    return Object.entries(value).every(([key, item]) => {
      if (strings.includes(key)) return typeof item === 'string';
      if (nullableStrings.includes(key)) return item === null || typeof item === 'string';
      if (arrays.includes(key)) return isStringArray(item);
      if (key === 'seriesIndex') return item === null || (typeof item === 'number' && Number.isFinite(item));
      if (key === 'rating') return item === null || (typeof item === 'number' && Number.isFinite(item) && item >= 0 && item <= 5);
      if (key === 'favorite') return typeof item === 'boolean';
      if (key === 'readStatus') return item === 'unread' || item === 'reading' || item === 'finished';
      return false;
    });
  }
  function isMetadataProposal(value: unknown): value is MetadataProposal {
    if (!isRecord(value) || typeof value.bookId !== 'string' || !isBookPatch(value.patch) || !isConfidence(value.confidence)
      || !isStringArray(value.warnings) || typeof value.modelId !== 'string'
      || typeof value.providerId !== 'string' || !['zai', 'kimi', 'minimax', 'codex', 'claude', 'mistral'].includes(value.providerId) || !Array.isArray(value.evidence)) return false;
    return value.evidence.every((item: unknown) => isRecord(item) && typeof item.field === 'string' && typeof item.value === 'string'
      && isConfidence(item.confidence) && isStringArray(item.sourceUrls) && item.sourceUrls.every(isPublicUrl));
  }
  function proposalFromResult(result: unknown, id: string): MetadataProposal | null {
    const candidate = isRecord(result) && 'proposal' in result ? result.proposal : result;
    return isMetadataProposal(candidate) && candidate.bookId === id ? candidate : null;
  }
  function resultWarnings(result: unknown): string[] {
    return isRecord(result) && isStringArray(result.warnings) ? result.warnings : [];
  }
  function bibliographicPatch(value: BookPatch): BookPatch {
    const { notes: _notes, favorite: _favorite, rating: _rating, readStatus: _status, ...metadata } = value;
    return metadata;
  }
  function normalizedProposalValue(field: string, value: unknown, authors: string[] = displayedBook.authors): unknown {
    const text = (input: string): string => input.replace(/\r\n?/gu, '\n').normalize('NFC').trim();
    const name = (input: string): string => text(input).replace(/\s+/gu, ' ');
    if (Array.isArray(value)) {
      const seen = new Set<string>();
      return value.filter((item): item is string => typeof item === 'string').map(name).filter((item) => {
        const key = item.toLowerCase();
        if (!item || seen.has(key)) return false;
        seen.add(key);
        return true;
      });
    }
    if (typeof value !== 'string') return value;
    if (field === 'description') return text(value);
    if (field === 'language') return text(value).replaceAll('_', '-').toLowerCase() || 'und';
    if (field === 'isbn') return text(value).replace(/^(?:urn:isbn:|isbn-1[03]:|isbn[\s:-]*)/iu, '').replace(/[\s-]/gu, '').toUpperCase() || null;
    if (field === 'published') {
      const cleaned = text(value);
      return /^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})$/iu.test(cleaned) && Number.isFinite(Date.parse(cleaned)) ? cleaned.slice(0, 10) : cleaned || null;
    }
    const cleaned = name(value);
    if (field === 'authorSort') return cleaned || name(authors[0] ?? '');
    return ['series', 'publisher'].includes(field) ? cleaned || null : cleaned;
  }
  function changedProposalPatch(value: BookPatch): BookPatch {
    return Object.fromEntries(Object.entries(bibliographicPatch(value)).filter(([field, proposed]) =>
      JSON.stringify(normalizedProposalValue(field, proposed, value.authors ?? displayedBook.authors)) !== JSON.stringify(normalizedProposalValue(field, currentValue(field)))));
  }
  function nullable(value: string): string | null { return value.trim() || null; }
  function values(value: string, separator: RegExp): string[] {
    return [...new Set(value.split(separator).map((part) => part.trim()).filter(Boolean))];
  }
  function invalidInput(): never {
    throw new PublicError({ code: 'invalidInput', message: $t('errors.invalidInput'), detail: null, retryable: false });
  }
  function optionalNumber(value: string, min?: number, max?: number): number | null {
    if (!value.trim()) return null;
    const number = Number(value);
    if (!Number.isFinite(number) || (min !== undefined && number < min) || (max !== undefined && number > max)) invalidInput();
    return number;
  }
  function createPatch(current: Book): BookPatch {
    const previous = draftFromBook(current);
    const patch: BookPatch = {};
    if (draft.title !== previous.title) { patch.title = draft.title.trim(); if (!patch.title) invalidInput(); }
    if (draft.authors !== previous.authors) { patch.authors = values(draft.authors, /[;\n]/u); if (!patch.authors.length) invalidInput(); }
    if (draft.authorSort !== previous.authorSort) patch.authorSort = draft.authorSort.trim();
    if (draft.series !== previous.series) patch.series = nullable(draft.series);
    if (draft.seriesIndex !== previous.seriesIndex) patch.seriesIndex = optionalNumber(draft.seriesIndex);
    if ((draft.series !== previous.series || draft.seriesIndex !== previous.seriesIndex)
      && patch.series !== null && !nullable(draft.series) && draft.seriesIndex.trim()) invalidInput();
    if (patch.series === null) patch.seriesIndex = null;
    if (draft.genres !== previous.genres) patch.genres = values(draft.genres, /[,;\n]/u);
    if (draft.tags !== previous.tags) patch.tags = values(draft.tags, /[,;\n]/u);
    if (draft.language !== previous.language) { patch.language = draft.language.trim(); if (!patch.language) invalidInput(); }
    if (draft.description !== previous.description) patch.description = draft.description;
    if (draft.isbn !== previous.isbn) patch.isbn = nullable(draft.isbn);
    if (draft.publisher !== previous.publisher) patch.publisher = nullable(draft.publisher);
    if (draft.published !== previous.published) patch.published = nullable(draft.published);
    if (draft.notes !== previous.notes) patch.notes = draft.notes;
    if (draft.readStatus !== previous.readStatus) patch.readStatus = draft.readStatus;
    if (draft.favorite !== previous.favorite) patch.favorite = draft.favorite;
    if (draft.rating !== previous.rating) patch.rating = optionalNumber(draft.rating, 0, 5);
    return patch;
  }

  async function persist(patch: BookPatch): Promise<void> {
    if (!baseline || busy || conflict || demo || !Object.keys(patch).length) return;
    const id = baseline.id;
    const expectedRevision = baseline.revision;
    const currentGeneration = generation;
    busy = true;
    failure = null;
    try {
      const updated = await request('book_update', { id, patch, expectedRevision });
      if (disposed || book.id !== id || currentGeneration !== generation) return;
      acceptBook(updated);
      onUpdated(updated);
      onNotify(`${$t('actions.save')} · ${updated.title}`);
    } catch (error) {
      if (disposed || book.id !== id || currentGeneration !== generation) return;
      const publicError = normalizePublicError(error);
      showFailure(publicError);
      if (publicError.code === 'revisionConflict') {
        conflict = true;
        try {
          const current = await request('book_get', { id });
          if (!disposed && book.id === id && currentGeneration === generation) remoteBook = current;
        } catch (reloadError) { if (!disposed && book.id === id && currentGeneration === generation) showFailure(reloadError); }
      }
    } finally { if (!disposed && book.id === id && currentGeneration === generation) busy = false; }
  }
  async function save(): Promise<void> {
    if (!baseline) return;
    try { await persist(createPatch(baseline)); } catch (error) { showFailure(error); }
  }
  function reload(): void {
    failure = null;
    if (remoteBook) acceptBook(remoteBook);
    discardRequested = true;
    loading = true;
    scheduler.setQuery(book.id);
  }
  async function startJob(kind: 'enrich' | 'optimize' | 'convert'): Promise<void> {
    if (busy || demo || !baseline) return;
    const id = baseline.id;
    const currentGeneration = generation;
    busy = true;
    failure = null;
    try {
      let job: Job;
      if (kind === 'enrich') job = await request('book_enrich', { id });
      else if (kind === 'optimize') {
        if (!selectedProfile || !profiles.some((profile) => profile.id === selectedProfile)) invalidInput();
        job = await request('book_optimize', { id, profileId: selectedProfile });
      } else {
        if (!selectedFormat || !capabilities?.outputs.includes(selectedFormat)) invalidInput();
        job = await request('book_convert', { id, format: selectedFormat });
      }
      if (disposed || book.id !== id || currentGeneration !== generation) return;
      latestJob = job;
      onNotify(`${$t(`jobs.${job.kind}`)} · ${$t(`jobs.${job.status}`)}`);
    } catch (error) { if (!disposed && book.id === id && currentGeneration === generation) showFailure(error); }
    finally { if (!disposed && book.id === id && currentGeneration === generation) busy = false; }
  }
  function profileLabel(profile: OptimizationProfile): string {
    const key = `optimization.${profile.id}`;
    const translation = $t(key);
    return translation === key ? profile.name : translation;
  }
  function fieldLabel(field: string): string {
    if (field === 'authorSort') return `${$t('sort.label')} · ${$t('sort.author')}`;
    const key = `book.${field}`;
    const translation = $t(key);
    return translation === key ? field : translation;
  }
  function displayValue(value: unknown): string {
    if (value === null || value === undefined || value === '') return $t('common.none');
    if (Array.isArray(value)) return value.map(String).join(', ');
    if (typeof value === 'number') return new Intl.NumberFormat($locale).format(value);
    return String(value);
  }
  function currentValue(field: string): unknown {
    return Object.entries(displayedBook).find(([key]) => key === field)?.[1];
  }
  async function visitSource(url: string): Promise<void> {
    try { await openExternal(url); } catch (error) { showFailure(error); }
  }
</script>

<dialog class="drawer" bind:this={dialog} aria-labelledby="book-panel-title" oncancel={(event) => { event.preventDefault(); onClose(); }}>
  <div class="drawer-header">
    <div><p class="eyebrow">{$t('common.details')}</p><h2 id="book-panel-title">{displayedBook.title}</h2></div>
    <button class="icon-button" type="button" onclick={onClose} aria-label={$t('actions.close')}><X size={20} /></button>
  </div>
  <div class="drawer-body stack" aria-busy={loading}>
    {#if failure}<div class="error-banner" role="alert"><div class="grow"><strong>{$t(`errors.${failure.code}`)}</strong>{#if failureDetail}<p>{failureDetail}</p>{/if}</div></div>{/if}
    {#if conflict}<div class="metadata-review stack" role="alert"><p>{$t('editor.revisionConflict')}</p><button class="button secondary" type="button" disabled={busy || loading} onclick={reload}><RefreshCw size={16} />{$t('actions.refresh')}</button></div>{/if}
    {#if demo}<p class="field-hint">{$t('app.previewDescription')}</p>{/if}
    <div class="row book-summary">
      {#if displayedBook.coverPath}<img class="book-detail-cover" src={displayedBook.coverPath} alt={displayedBook.title} />{:else}<div class="book-detail-cover cover-placeholder"><strong>{displayedBook.title}</strong><span>{displayedBook.authors.join(', ')}</span></div>{/if}
      <div class="stack grow">
        <p>{displayedBook.authors.join(', ') || $t('common.unknown')}</p>
        <div class="row wrap"><span class="badge">{displayedBook.format.toUpperCase()}</span><span class="badge">{formatSize(displayedBook.sizeBytes, $locale)}</span></div>
        <span class="badge" class:success={displayedBook.metadataStatus === 'verified'} class:warning={displayedBook.metadataStatus === 'needsReview'} class:danger={displayedBook.metadataStatus === 'failed'}>{$t(`book.${displayedBook.metadataStatus}`)}</span>
        {#if displayedBook.metadataConfidence !== null}<p class="small muted">{$t('book.confidence', { progress: percent.format(displayedBook.metadataConfidence) })}</p>{/if}
        {#if displayedBook.onDeviceIds.length}<span class="badge accent">{$t('library.onDevice')} · {displayedBook.onDeviceIds.length}</span>{/if}
        <button class="button primary" type="button" onclick={() => onReadBook(displayedBook)}><BookOpen size={16} />{$t('actions.read')}</button>
      </div>
    </div>
    <div class="row wrap small muted"><span>{$t('sort.added')}: {formatDate(displayedBook.addedAt, $locale)}</span><span>{$t('reader.progress', { progress: percent.format(displayedBook.readingProgress) })}</span></div>

    {#if proposal && proposalEntries.length > 0}
      <section class="metadata-review stack" aria-labelledby="metadata-proposal-title">
        <div class="spread"><h3 id="metadata-proposal-title">{$t('editor.reviewTitle')}</h3><span class="badge">{$t('book.confidence', { progress: percent.format(proposal.confidence) })}</span></div>
        <p class="field-hint">{$t('editor.reviewPending')}</p>
        {#if dirty || conflict}<p class="field-hint">{$t('editor.reviewDraftBlocked')}</p>{/if}
        <p class="small muted">{proposal.providerId} · {proposal.modelId}</p>
        {#each proposalEntries as [field, value] (field)}<div class="proposal-field"><strong>{fieldLabel(field)}</strong><p class="small muted">{$t('editor.currentValue')}: {displayValue(currentValue(field))}</p><p>{$t('editor.proposedValue')}: {displayValue(value)}</p></div>{/each}
        <h4>{$t('book.sources')}</h4>
        {#each proposal.evidence as evidence, index (index)}<div class="stack evidence"><p class="small"><strong>{fieldLabel(evidence.field)}</strong> · {percent.format(evidence.confidence)}</p><p class="small">{evidence.value}</p><div class="row wrap">{#each [...new Set(evidence.sourceUrls)] as url (url)}<button type="button" class="source-chip" onclick={() => visitSource(url)} title={url}><ExternalLink size={14} />{new URL(url).hostname}</button>{/each}</div></div>{/each}
        <button class="button primary" type="button" disabled={demo || busy || loading || dirty || conflict || !proposalEntries.length} onclick={() => persist(proposalPatch)}><Check size={16} />{$t('editor.applyProposal')}</button>
      </section>
    {/if}

    {#if warnings.length}<section class="stack"><h3>{$t('common.warning')}</h3><ul>{#each warnings as warning (warning)}<li class="small">{warning}</li>{/each}</ul></section>{/if}

    <form id="book-metadata-form" class="stack" onsubmit={(event) => { event.preventDefault(); void save(); }}>
      <h3>{$t('editor.title')}</h3>
      <fieldset disabled={demo || busy || loading || baseline === null}>
        <div class="stack">
          <div class="field"><label for="book-title">{$t('book.title')}</label><input id="book-title" class="input" bind:value={draft.title} /></div>
          <div class="field"><label for="book-authors">{$t('book.authors')}</label><input id="book-authors" class="input" bind:value={draft.authors} aria-describedby="book-authors-hint" /><p id="book-authors-hint" class="field-hint">{$t('editor.authorsHint')}</p></div>
          <div class="field"><label for="book-author-sort">{$t('sort.label')} · {$t('sort.author')}</label><input id="book-author-sort" class="input" bind:value={draft.authorSort} /></div>
          <div class="field"><label for="book-series">{$t('book.series')}</label><input id="book-series" class="input" bind:value={draft.series} aria-describedby="book-series-hint" /><p id="book-series-hint" class="field-hint">{$t('editor.seriesHint')}</p></div>
          <div class="field"><label for="book-index">{$t('book.seriesIndex')}</label><input id="book-index" class="input" type="number" step="any" value={draft.seriesIndex} oninput={(event) => { draft.seriesIndex = event.currentTarget.value; }} aria-describedby="book-index-hint" /><p id="book-index-hint" class="field-hint">{$t('editor.indexHint')}</p></div>
          <div class="field"><label for="book-genres">{$t('book.genres')}</label><input id="book-genres" class="input" bind:value={draft.genres} /></div>
          <div class="field"><label for="book-tags">{$t('book.tags')}</label><input id="book-tags" class="input" bind:value={draft.tags} aria-describedby="book-tags-hint" /><p id="book-tags-hint" class="field-hint">{$t('editor.tagsHint')}</p></div>
          <div class="field-row"><div class="field"><label for="book-language">{$t('book.language')}</label><input id="book-language" class="input" bind:value={draft.language} /></div><div class="field"><label for="book-isbn">{$t('book.isbn')}</label><input id="book-isbn" class="input" bind:value={draft.isbn} /></div></div>
          <div class="field-row"><div class="field"><label for="book-publisher">{$t('book.publisher')}</label><input id="book-publisher" class="input" bind:value={draft.publisher} /></div><div class="field"><label for="book-published">{$t('book.published')}</label><input id="book-published" class="input" bind:value={draft.published} /></div></div>
          <div class="field"><label for="book-description">{$t('book.description')}</label><textarea id="book-description" class="textarea" rows="5" bind:value={draft.description}></textarea></div>
        </div>
      </fieldset>
      <section class="stack personal-data" aria-labelledby="personal-data-title">
        <h3 id="personal-data-title">{$t('book.notes')}</h3>
        <fieldset disabled={demo || busy || loading || baseline === null}>
          <div class="stack">
            <div class="field-row"><div class="field"><label for="book-read-status">{$t('filters.readStatus')}</label><select id="book-read-status" class="select" bind:value={draft.readStatus}><option value="unread">{$t('book.unread')}</option><option value="reading">{$t('book.reading')}</option><option value="finished">{$t('book.finished')}</option></select></div><div class="field"><label for="book-rating">{$t('book.rating')} / 5</label><input id="book-rating" class="input" type="number" min="0" max="5" step="0.5" value={draft.rating} oninput={(event) => { draft.rating = event.currentTarget.value; }} /></div></div>
            <label class="checkbox-field"><input type="checkbox" bind:checked={draft.favorite} />{$t('sidebar.favorites')}</label>
            <div class="field"><label for="book-notes">{$t('book.notes')}</label><textarea id="book-notes" class="textarea" rows="4" bind:value={draft.notes}></textarea></div>
          </div>
        </fieldset>
      </section>
      <p class="field-hint">{$t('editor.preserveOriginal')}</p>
    </form>

    <section class="stack" aria-labelledby="book-files-title">
      <h3 id="book-files-title">{$t('book.files')}</h3>
      {#each files as file (file.id)}<div class="file-variant"><div class="stack variant-description"><div class="row wrap"><FileText size={16} /><strong>{file.format.toUpperCase()}</strong><span class="badge">{$t(`book.${file.variant}`)}</span>{#if file.variant === 'original'}<span class="badge">{$t('devices.readOnly')}</span>{/if}</div>{#if file.profile}<p class="small muted">{profiles.find((profile) => profile.id === file.profile)?.name ?? file.profile}</p>{/if}<p class="small muted">{formatDate(file.createdAt, $locale)}</p></div><span>{formatSize(file.sizeBytes, $locale)}</span></div>{:else}<p class="field-hint">{$t('common.none')}</p>{/each}
    </section>
    <section class="stack operations" aria-labelledby="book-operations-title">
      <h3 id="book-operations-title">{$t('jobs.title')}</h3>
      <button class="button secondary" type="button" disabled={demo || busy || loading || baseline === null || dirty || conflict} onclick={() => startJob('enrich')}><Sparkles size={16} />{$t('actions.enrich')}</button>
      <div class="field"><label for="book-profile">{$t('settings.optimizationProfile')}</label><select id="book-profile" class="select" bind:value={selectedProfile} disabled={!profiles.length || busy}>{#each profiles as profile (profile.id)}<option value={profile.id}>{profileLabel(profile)}</option>{/each}</select></div>
      {#if chosenProfile?.removeImages}<p class="field-hint">{$t('optimization.removeImagesWarning')}</p>{/if}
      <button class="button secondary" type="button" disabled={demo || busy || loading || baseline === null || !selectedProfile || dirty || conflict} onclick={() => startJob('optimize')}>{$t('actions.optimize')}</button>
      <div class="field"><label for="book-convert-format">{$t('book.format')}</label><select id="book-convert-format" class="select" bind:value={selectedFormat} disabled={!conversionAvailable || busy}>{#each capabilities?.outputs ?? [] as format (format)}<option value={format}>{format.toUpperCase()}</option>{/each}</select></div>
      {#each capabilities?.warnings ?? [] as warning (warning)}<p class="field-hint">{warning === 'previewConversionUnavailable' ? $t('app.previewConversionUnavailable') : warning}</p>{/each}
      <button class="button secondary" type="button" disabled={demo || busy || loading || baseline === null || !conversionAvailable || !selectedFormat || dirty || conflict} onclick={() => startJob('convert')}>{$t('actions.convert')}</button>
      {#if latestJob}<div class="job-status stack" aria-live="polite"><p class="small"><strong>{$t(`jobs.${latestJob.kind}`)}</strong> · {$t(`jobs.${latestJob.status}`)}</p><progress value={latestJob.progress} max="1" aria-label={$t('common.progress')}></progress>{#if latestJob.message}<p class="field-hint">{latestJob.message}</p>{/if}{#if latestJob.error}<p class="field-hint">{$t(`errors.${latestJob.error.code}`)}</p>{#if latestJobDetail}<p class="field-hint">{latestJobDetail}</p>{/if}{/if}</div>{/if}
    </section>
  </div>
  <div class="drawer-footer"><button class="button primary" type="submit" form="book-metadata-form" disabled={demo || busy || loading || !dirty || conflict}><Check size={16} />{$t('actions.save')}</button><button class="button secondary" type="button" disabled={busy} onclick={onClose}>{$t('actions.close')}</button></div>
</dialog>

<style>
  dialog.drawer { margin: 0 0 0 auto; max-width: none; max-height: none; height: 100dvh; padding: 0; border: 0; border-inline-start: 1px solid var(--border); color: var(--text); }
  dialog:not([open]) { display: none; }
  dialog::backdrop { background: var(--overlay); }
  h2 { font-size: 19px; line-height: 1.3; overflow-wrap: anywhere; }
  h3 { font-size: 15px; }
  h4 { font-size: 12px; }
  fieldset { min-width: 0; margin: 0; padding: 0; border: 0; }
  fieldset:disabled { opacity: 0.72; }
  .book-summary { align-items: flex-start; }
  .book-summary .book-detail-cover.cover-placeholder { padding: 16px 12px; }
  .book-summary .cover-placeholder strong { font-size: 19px; }
  .personal-data, .operations { padding-top: 22px; border-top: 1px solid var(--border); }
  .variant-description { gap: 5px; }
  .proposal-field { padding-block: 10px; border-bottom: 1px solid var(--border-strong); overflow-wrap: anywhere; }
  .proposal-field p { margin-top: 6px; }
  .evidence { gap: 7px; overflow-wrap: anywhere; }
  .metadata-review { gap: 14px; }
  .job-status { padding: 14px; background: var(--surface-muted); border-radius: var(--radius-sm); }
  .drawer-body { gap: 26px; }
  .drawer-body ul { padding-inline-start: 20px; }
  .drawer-body li + li { margin-top: 8px; }
</style>
