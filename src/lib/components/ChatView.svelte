<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import { ExternalLink, MessageCircle, Plus, RefreshCw, Send, Sparkles, UserRound, X } from '@lucide/svelte';
  import { isPreview, normalizePublicError, openExternal, request, subscribe } from '../api';
  import type { AppError, Book, ChatDeltaEvent, ChatMessage, Conversation, Job, Provider, Settings, WebSource } from '../contracts';
  import { formatDate, formatProviderDiagnostic, locale, t } from '../i18n';
  import { createRequestScheduler } from '../request-scheduler';
  import SelectionActions from './SelectionActions.svelte';
  import { isMetadataReady, MAX_SELECTED_BOOKS, type MetadataDisabledReason } from '../selection-capabilities';

  interface Props {
    selectedBookIds: string[];
    refreshVersion: number;
    onOpenBook?(book: Book): void;
    onChooseBooks?(): void;
    verifyRequested?: boolean;
    onVerifyStarted?(): void;
    onNotify(message: string): void;
    onError(error: unknown): void;
    metadataReady?: boolean;
    metadataDisabledReason?: MetadataDisabledReason | null;
    transferReady?: boolean;
    onTransferSelected?(initialDeviceId?: string): void;
    onRemoveSelected?(): void;
    onClearSelection?(): void;
    onOpenSettings?(): void;
  }
  let { selectedBookIds, refreshVersion, onOpenBook, onChooseBooks, verifyRequested = false, onVerifyStarted,
    onNotify, onError, metadataReady = false, metadataDisabledReason = null, transferReady = false,
    onTransferSelected, onRemoveSelected, onClearSelection, onOpenSettings }: Props = $props();
  const MAX_TEXT_LENGTH = 8000;
  const MAX_CONTEXT_BOOKS = 32;
  const BOOK_LOAD_CONCURRENCY = 4;
  const demo = isPreview();
  let disposed = false;
  let configGeneration = 0;
  let historyGeneration = 0;
  let messageGeneration = 0;
  let viewVersion = 0;
  let selectedGeneration = 0;
  const unlisteners: Array<() => void> = [];
  let conversations = $state<Conversation[]>([]);
  let messages = $state<ChatMessage[]>([]);
  let settings = $state<Settings | null>(null);
  let providers = $state<Provider[]>([]);
  let activeConversationId = $state<string | null>(null);
  let activeJob = $state<Job | null>(null);
  let jobConversationId = $state<string | null>(null);
  let followJob = $state(false);
  let draft = $state('');
  let submittedText = $state('');
  let streamedText = $state('');
  let streamingMessageId = $state<string | null>(null);
  let sending = $state(false);
  let cancelling = $state(false);
  let loadingMessages = $state(false);
  let allowChanges = $state(false);
  let enriching = $state(false);
  let enrichmentJobs = $state<Job[]>([]);
  let enrichmentSummary = $state<{ queued: number; skipped: number; failed: number } | null>(null);
  let selectedBooks = $state<Book[]>([]);
  let loadingSelectedBooks = $state(false);
  let openingBookId = $state<string | null>(null);
  let failure = $state<AppError | null>(null);
  let chatScroll = $state<HTMLDivElement>();
  const currentProvider = $derived(providers.find((provider) => provider.id === settings?.providerId));
  const configured = $derived(metadataReady && isMetadataReady(settings, providers));
  const jobPending = $derived(activeJob !== null && !['completed', 'failed', 'cancelled'].includes(activeJob.status));
  const contextBookIds = $derived([...new Set(selectedBookIds)]);
  const contextKey = $derived(contextBookIds.join('\u0000'));
  const contextTooLarge = $derived(contextBookIds.length > MAX_SELECTED_BOOKS);
  const reviewBooks = $derived(selectedBooks.filter((book) => contextBookIds.includes(book.id)
    && enrichmentJobs.some((job) => completedProposalForBook(job, book))));
  const canSend = $derived(!demo && configured && !sending && !jobPending && !contextTooLarge && draft.trim().length > 0 && draft.length <= MAX_TEXT_LENGTH);
  const number = $derived(new Intl.NumberFormat($locale));
  const failureDetail = $derived(failure?.code === 'providerError'
    ? formatProviderDiagnostic(failure.code, failure.detail, $locale) : failure?.detail);

  function mergeEnrichmentJobs(incoming: Job[]): void {
    const known = new Map(enrichmentJobs.map((job) => [job.id, job]));
    for (const job of incoming) {
      if (job.kind !== 'enrich') continue;
      const previous = known.get(job.id);
      if (!previous || job.updatedAt >= previous.updatedAt) known.set(job.id, job);
    }
    enrichmentJobs = [...known.values()];
  }

  function enrichmentPending(id: string): boolean {
    return enrichmentJobs.some((job) => job.bookIds.includes(id)
      && !['completed', 'failed', 'cancelled'].includes(job.status));
  }

  async function loadSelectedBooks(ids: string[]): Promise<Book[]> {
    const generation = selectedGeneration;
    const loaded: Book[] = [];
    for (let start = 0; start < ids.length && !disposed && generation === selectedGeneration; start += BOOK_LOAD_CONCURRENCY) {
      const results = await Promise.allSettled(ids.slice(start, start + BOOK_LOAD_CONCURRENCY)
        .map((id) => request('book_get', { id })));
      for (const result of results) if (result.status === 'fulfilled') loaded.push(result.value);
    }
    return loaded;
  }

  const selectedBooksScheduler = createRequestScheduler<string[], Book[]>({
    load: loadSelectedBooks,
    onStart: () => { loadingSelectedBooks = true; },
    onSuccess: (books) => { selectedBooks = books; },
    onError: report,
    onSettled: () => { loadingSelectedBooks = false; },
  });

  const backgroundScheduler = createRequestScheduler<string, void>({
    load: async () => { await Promise.allSettled([loadConfiguration(), loadHistory(), recoverJob()]); },
    onStart: () => undefined,
    onSuccess: () => undefined,
    onError: report,
    onSettled: () => undefined,
    queryDelayMs: 0,
  });

  async function verifySelected(): Promise<void> {
    if (disposed || demo || !metadataReady || enriching || !contextBookIds.length || contextTooLarge) return;
    const ids = [...contextBookIds];
    enriching = true;
    enrichmentSummary = { queued: 0, skipped: 0, failed: 0 };
    failure = null;
    try {
      // Refresh eligibility before enqueueing; live job events keep it current during the batch.
      mergeEnrichmentJobs(await request('jobs_list', undefined));
      for (const id of ids) {
        if (disposed || !metadataReady) break;
        if (enrichmentPending(id)) {
          enrichmentSummary.skipped += 1;
          continue;
        }
        try {
          const job = await request('book_enrich', { id });
          if (disposed) break;
          mergeEnrichmentJobs([job]);
          enrichmentSummary.queued += 1;
        } catch (error) {
          if (disposed) break;
          enrichmentSummary.failed += 1;
          report(error);
        }
      }
      if (!disposed) onNotify($t('chat.enrichmentQueued', { count: enrichmentSummary.queued }));
    } catch (error) {
      if (!disposed) {
        enrichmentSummary.failed = ids.length;
        report(error);
      }
    } finally { if (!disposed) enriching = false; }
  }

  async function openSelected(id: string, reviewOnly = false): Promise<void> {
    if (!onOpenBook || openingBookId !== null) return;
    openingBookId = id;
    try {
      const current = await request('book_get', { id });
      if (!disposed) {
        if (reviewOnly && !enrichmentJobs.some((job) => completedProposalForBook(job, current))) {
          onNotify($t('library.noReviewProposals'));
          return;
        }
        onOpenBook(current);
      }
    } catch (error) { if (!disposed) report(error); }
    finally { if (!disposed) openingBookId = null; }
  }

  function report(error: unknown): void {
    failure = normalizePublicError(error);
    onError(failure);
  }
  function isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  }
  function completedProposalForBook(job: Job, book: Book): boolean {
    const bookId = book.id;
    if (job.kind !== 'enrich' || job.status !== 'completed' || !job.bookIds.includes(bookId) || !isRecord(job.result)) return false;
    const proposal = 'proposal' in job.result ? job.result.proposal : job.result;
    if (!isRecord(proposal) || proposal.bookId !== bookId
      || !isRecord(proposal.patch) || !Object.keys(proposal.patch).length) return false;
    if ('review' in job.result) {
      const review = job.result.review;
      return isRecord(review) && review.state === 'pending' && typeof review.reviewRevision === 'number'
        && Number.isSafeInteger(review.reviewRevision) && review.reviewRevision >= 0
        && review.reviewRevision === book.revision;
    }
    return book.metadataStatus === 'needsReview';
  }
  function isWebSource(value: unknown): value is WebSource {
    return isRecord(value) && typeof value.url === 'string' && typeof value.title === 'string'
      && typeof value.excerpt === 'string' && typeof value.retrievedAt === 'string';
  }
  function isChatMessage(value: unknown): value is ChatMessage {
    return isRecord(value) && typeof value.id === 'string' && typeof value.conversationId === 'string'
      && ['user', 'assistant', 'system'].includes(String(value.role)) && typeof value.role === 'string'
      && typeof value.content === 'string' && typeof value.createdAt === 'string'
      && Array.isArray(value.sources) && value.sources.every((source: unknown) => isWebSource(source));
  }
  function resultConversationId(result: unknown): string | null {
    return isRecord(result) && typeof result.conversationId === 'string' && result.conversationId.length > 0 ? result.conversationId : null;
  }
  function safeUrl(value: string): boolean {
    try {
      const url = new URL(value);
      return ['http:', 'https:'].includes(url.protocol) && !url.username && !url.password && !/[\u0000-\u001f\u007f]/u.test(value);
    } catch { return false; }
  }
  function visibleSources(value: ChatMessage): WebSource[] {
    const seen = new Set<string>();
    return value.sources.filter((source) => {
      if (!safeUrl(source.url) || seen.has(source.url)) return false;
      seen.add(source.url);
      return true;
    });
  }
  async function visitSource(url: string): Promise<void> {
    try { await openExternal(url); } catch (error) { if (!disposed) report(error); }
  }
  async function loadConfiguration(): Promise<void> {
    const generation = ++configGeneration;
    const [settingsResult, providersResult] = await Promise.allSettled([
      request('settings_get', undefined), request('providers_list', undefined),
    ] as const);
    if (disposed || generation !== configGeneration) return;
    if (settingsResult.status === 'fulfilled') settings = settingsResult.value;
    else report(settingsResult.reason);
    if (providersResult.status === 'fulfilled') providers = providersResult.value;
    else report(providersResult.reason);
  }
  async function loadHistory(): Promise<void> {
    const generation = ++historyGeneration;
    try {
      const history = await request('conversations_list', undefined);
      if (!disposed && generation === historyGeneration) conversations = history;
    } catch (error) { if (!disposed && generation === historyGeneration) report(error); }
  }
  async function loadMessages(id: string): Promise<void> {
    const generation = ++messageGeneration;
    loadingMessages = true;
    try {
      const savedMessages = await request('conversation_messages', { id });
      if (disposed || generation !== messageGeneration || activeConversationId !== id) return;
      messages = savedMessages;
      if (streamingMessageId && savedMessages.some((message) => message.id === streamingMessageId)) {
        streamedText = '';
        streamingMessageId = null;
      }
      void scrollToLatest(true);
    } catch (error) { if (!disposed && generation === messageGeneration) report(error); }
    finally { if (!disposed && generation === messageGeneration) loadingMessages = false; }
  }
  async function scrollToLatest(force = false): Promise<void> {
    const element = chatScroll;
    if (!element) return;
    const nearBottom = element.scrollHeight - element.scrollTop - element.clientHeight < 100;
    await tick();
    if (!disposed && (force || nearBottom)) element.scrollTop = element.scrollHeight;
  }
  function selectConversation(id: string | null): void {
    if (sending) return;
    allowChanges = false;
    viewVersion += 1;
    followJob = false;
    activeConversationId = id;
    messages = [];
    streamedText = '';
    streamingMessageId = null;
    messageGeneration += 1;
    loadingMessages = false;
    failure = null;
    if (id) void loadMessages(id);
  }
  function trackJob(job: Job): void {
    const previousStatus = activeJob?.status;
    activeJob = job;
    const conversationId = resultConversationId(job.result);
    if (conversationId) {
      jobConversationId = conversationId;
      if (followJob && activeConversationId !== conversationId) {
        activeConversationId = conversationId;
        messages = [];
        void loadMessages(conversationId);
      }
    }
    if (job.status === previousStatus) return;
    if (job.status === 'completed') {
      if (isChatMessage(job.result) && activeConversationId === job.result.conversationId) {
        const response = job.result;
        messages = [...messages.filter((message) => message.id !== response.id), response];
        streamedText = '';
        streamingMessageId = null;
        void scrollToLatest();
      }
      submittedText = '';
      if (activeConversationId) void loadMessages(activeConversationId);
      void loadHistory();
    } else if (job.status === 'failed' || job.status === 'cancelled') {
      if (!draft.trim()) draft = submittedText;
      submittedText = '';
      streamedText = '';
      streamingMessageId = null;
      if (job.error) report(job.error);
    }
  }
  function receiveDelta(payload: ChatDeltaEvent): void {
    if (!jobPending || payload.conversationId !== jobConversationId || payload.conversationId !== activeConversationId) return;
    if (streamingMessageId !== payload.messageId) {
      streamingMessageId = payload.messageId;
      streamedText = '';
    }
    streamedText += payload.text;
    void scrollToLatest();
    if (payload.finished) {
      void loadMessages(payload.conversationId);
      void recoverJob();
    }
  }
  async function recoverJob(): Promise<void> {
    const id = activeJob?.id;
    if (id && !jobPending) return;
    try {
      const jobs = await request('jobs_list', undefined);
      if (disposed || activeJob?.id !== id) return;
      mergeEnrichmentJobs(jobs);
      const updated = id ? jobs.find((job) => job.id === id) : jobs.filter((job) => job.kind === 'chat'
        && !['completed', 'failed', 'cancelled'].includes(job.status))
        .sort((left, right) => right.createdAt.localeCompare(left.createdAt))[0];
      if (updated) trackJob(updated);
    } catch (error) { if (!disposed && activeJob?.id === id) report(error); }
  }
  async function connectEvents(): Promise<void> {
    const results = await Promise.allSettled([
      subscribe('job:updated', ({ job }) => {
        if (disposed) return;
        const previous = enrichmentJobs.find((item) => item.id === job.id);
        const selectedEnrichmentFinished = job.kind === 'enrich'
          && job.bookIds.some((id) => contextBookIds.includes(id))
          && ['completed', 'failed', 'cancelled'].includes(job.status)
          && (!previous || previous.status !== job.status || job.updatedAt > previous.updatedAt);
        mergeEnrichmentJobs([job]);
        if (selectedEnrichmentFinished) selectedBooksScheduler.refresh();
        if (job.id === activeJob?.id) trackJob(job);
      }),
      subscribe('chat:delta', (payload) => { if (!disposed) receiveDelta(payload); }),
    ]);
    for (const result of results) {
      if (result.status === 'fulfilled') {
        if (disposed) void Promise.resolve().then(result.value).catch(() => undefined);
        else unlisteners.push(result.value);
      } else if (!disposed) report(result.reason);
    }
  }
  $effect(() => {
    void refreshVersion;
    untrack(() => backgroundScheduler.refresh());
  });
  $effect(() => {
    void contextKey;
    untrack(() => {
      allowChanges = false;
      selectedGeneration += 1;
      selectedBooks = selectedBooks.filter((book) => contextBookIds.includes(book.id));
      selectedBooksScheduler.setQuery([...contextBookIds]);
    });
  });
  $effect(() => {
    if (!verifyRequested) return;
    untrack(() => {
      // Consume the navigation request before starting asynchronous batch work.
      onVerifyStarted?.();
      void verifySelected();
    });
  });
  onMount(() => { backgroundScheduler.setQuery('chat'); void connectEvents(); });
  onDestroy(() => {
    disposed = true;
    configGeneration += 1;
    historyGeneration += 1;
    messageGeneration += 1;
    selectedGeneration += 1;
    selectedBooksScheduler.dispose();
    backgroundScheduler.dispose();
    void Promise.allSettled(unlisteners.map((unsubscribe) => Promise.resolve().then(unsubscribe)));
  });

  async function send(): Promise<void> {
    if (!canSend) return;
    const text = draft.trim();
    const previousConversation = activeConversationId;
    const sentViewVersion = viewVersion;
    const sentAllowChanges = allowChanges;
    sending = true;
    failure = null;
    try {
      const job = await request('chat_send', { conversationId: previousConversation, text, bookIds: [...contextBookIds], allowChanges: sentAllowChanges });
      if (disposed) return;
      allowChanges = false;
      submittedText = text;
      draft = '';
      streamedText = '';
      streamingMessageId = null;
      followJob = viewVersion === sentViewVersion;
      jobConversationId = previousConversation;
      activeJob = null;
      trackJob(job);
      onNotify(`${$t('jobs.chat')} · ${$t(`jobs.${job.status}`)}`);
      if (activeConversationId) void loadMessages(activeConversationId);
      void loadHistory();
      void recoverJob();
    } catch (error) { if (!disposed) report(error); }
    finally { if (!disposed) sending = false; }
  }
  async function cancelJob(): Promise<void> {
    if (!activeJob || !jobPending || cancelling) return;
    const id = activeJob.id;
    cancelling = true;
    try {
      const cancelled = await request('job_cancel', { id });
      if (!disposed && activeJob?.id === id) trackJob(cancelled);
    } catch (error) { if (!disposed) report(error); }
    finally { if (!disposed) cancelling = false; }
  }
  async function refresh(): Promise<void> {
    failure = null;
    backgroundScheduler.refresh();
    selectedBooksScheduler.refresh();
    if (activeConversationId) await loadMessages(activeConversationId);
  }
  function handleKey(event: KeyboardEvent): void {
    if (event.key === 'Enter' && !event.shiftKey && !event.isComposing) {
      event.preventDefault();
      void send();
    }
  }
</script>

<section class="page">
  <div class="page-header"><div><p class="eyebrow">Library Manager</p><h1 class="page-title">{$t('chat.title')}</h1><p class="page-subtitle">{$t('providers.internetByApp')}</p></div><button class="button secondary" type="button" onclick={refresh}><RefreshCw size={17} />{$t('actions.refresh')}</button></div>
  {#if failure}<div class="error-banner" role="alert"><div class="grow"><strong>{$t(`errors.${failure.code}`)}</strong>{#if failureDetail}<p>{failureDetail}</p>{/if}</div><button class="icon-button" type="button" onclick={() => { failure = null; }} aria-label={$t('actions.close')}><X size={16} /></button></div>{/if}
  <div class="panel chat-layout">
    <aside class="chat-history" aria-label={$t('chat.conversations')}>
      <button class="button primary" type="button" disabled={sending} onclick={() => selectConversation(null)}><Plus size={16} />{$t('chat.newConversation')}</button>
      <h2 class="eyebrow">{$t('chat.conversations')}</h2>
      {#each conversations as conversation (conversation.id)}<button class="conversation-item" class:active={activeConversationId === conversation.id} aria-current={activeConversationId === conversation.id ? 'true' : undefined} disabled={sending} onclick={() => selectConversation(conversation.id)}><span class="conversation-title">{conversation.title}</span><span class="conversation-date">{formatDate(conversation.createdAt, $locale)}</span></button>{:else}<p class="field-hint">{$t('common.none')}</p>{/each}
    </aside>
    <div class="chat-main">
      <div class="chat-toolbar"><span class="badge" class:success={configured}>{$t('chat.provider')}: {currentProvider?.name ?? $t('common.none')}</span><span class="badge">{$t('chat.model')}: {settings?.modelId ?? $t('common.none')}</span><span class="badge" class:success={settings?.webEnabled === true} class:warning={settings?.webEnabled !== true}>{$t(settings?.webEnabled ? 'chat.internetEnabled' : 'chat.internetDisabled')}</span></div>
      {#if !configured}<p class="configuration-hint">{$t('chat.noModel')}</p>{/if}
      {#if demo}<p class="configuration-hint">{$t('app.previewDescription')}</p>{/if}
        <section class="selected-books stack" aria-label={$t('chat.selectedBooks', { count: contextBookIds.length })}>
          <div class="spread"><p class="small">{$t('chat.selectedBooks', { count: contextBookIds.length })}</p><button class="button ghost" type="button" disabled={!onChooseBooks} onclick={() => onChooseBooks?.()}>{$t('chat.chooseBooks')}</button></div>
          <SelectionActions selectedCount={contextBookIds.length} {metadataReady} {metadataDisabledReason} {transferReady}
            {demo} busy={enriching || sending || jobPending} onVerifySelected={() => { void verifySelected(); }}
            {onTransferSelected} {onRemoveSelected} {onClearSelection} {onOpenSettings} />
          {#if enriching}<p class="field-hint" role="status">{$t('chat.verifyingSelected')}</p>{/if}
          {#if !contextBookIds.length}<p class="field-hint">{$t('chat.noSelectedBooks')}</p>{/if}
          {#if contextTooLarge}<p class="field-hint context-overflow" role="status">{$t('chat.selectionTooLarge', { count: MAX_SELECTED_BOOKS })}</p>{/if}
          <div class="selected-book-list row wrap">{#each contextBookIds as id (id)}{@const selected = selectedBooks.find((item) => item.id === id)}<button class="source-chip" type="button" disabled={!onOpenBook || openingBookId !== null} onclick={() => void openSelected(id)} title={$t('chat.openSelected')}>{selected?.title ?? $t(loadingSelectedBooks ? 'common.loading' : 'common.unknown')}{#if selected?.metadataStatus === 'needsReview'}<span class="badge warning">{$t('book.needsReview')}</span>{/if}</button>{/each}</div>
          {#if reviewBooks.length}<div class="stack" aria-label={$t('editor.reviewTitle')}>{#each reviewBooks as book (book.id)}<div class="spread"><span class="small">{book.title}</span><button class="button secondary" type="button" disabled={!onOpenBook || openingBookId !== null} onclick={() => void openSelected(book.id, true)}>{$t('chat.reviewProposal')}</button></div>{/each}</div>{/if}
          {#if enrichmentSummary}<div class="row wrap small" role="status"><span>{$t('chat.enrichmentQueued', { count: enrichmentSummary.queued })}</span><span>{$t('chat.enrichmentSkipped', { count: enrichmentSummary.skipped })}</span><span>{$t('chat.enrichmentFailed', { count: enrichmentSummary.failed })}</span></div>{/if}
        </section>
      <div class="chat-scroll" bind:this={chatScroll} aria-label={$t('chat.title')} aria-busy={loadingMessages}>
        {#if loadingMessages && !messages.length}<p class="field-hint">{$t('common.loading')}</p>{/if}
        {#each messages as message (message.id)}
          {@const sources = visibleSources(message)}
          <article class="chat-message" class:user={message.role === 'user'}>
            <span class="message-role">{#if message.role === 'user'}<UserRound size={15} />{:else if message.role === 'assistant'}<Sparkles size={15} /><span>{$t('sidebar.chat')}</span>{:else}<MessageCircle size={15} /><span>{$t('common.details')}</span>{/if}<time datetime={message.createdAt}>{formatDate(message.createdAt, $locale)}</time></span>
            <p class="message-content">{message.content}</p>
            {#if sources.length}<div class="message-sources" aria-label={$t('chat.sources')}>{#each sources as source (source.url)}<button class="source-chip" type="button" onclick={() => visitSource(source.url)} title={source.excerpt}><ExternalLink size={14} />{source.title || new URL(source.url).hostname}</button>{/each}</div>{/if}
          </article>
        {/each}
        {#if streamedText}<article class="chat-message" aria-busy="true"><span class="message-role"><Sparkles size={15} />{$t('sidebar.chat')}</span><p class="message-content">{streamedText}</p></article>{:else if jobPending && activeConversationId === jobConversationId}<p class="field-hint thinking" aria-live="polite"><Sparkles size={16} />{$t('chat.thinking')}</p>{/if}
        {#if !loadingMessages && !messages.length && !streamedText && !jobPending}<div class="empty-state chat-empty"><MessageCircle /><p>{$t('chat.empty')}</p></div>{/if}
      </div>
      {#if activeJob}<div class="chat-job" aria-live="polite"><div class="spread"><p class="small"><strong>{$t('jobs.chat')}</strong> · {$t(`jobs.${activeJob.status}`)}</p>{#if jobPending}<button class="button ghost" type="button" disabled={cancelling} onclick={cancelJob}><X size={15} />{$t('actions.cancel')}</button>{/if}</div>{#if jobPending}<progress value={activeJob.progress} max="1" aria-label={$t('common.progress')}></progress>{/if}{#if activeJob.message}<p class="field-hint">{activeJob.message}</p>{/if}</div>{/if}
      <form class="chat-composer" onsubmit={(event) => { event.preventDefault(); void send(); }}>
        <label class="checkbox-field"><input type="checkbox" bind:checked={allowChanges} disabled={demo || sending || jobPending || !contextBookIds.length} aria-describedby="chat-change-scope" />{$t('chat.allowChanges')}</label>
        <p id="chat-change-scope" class="field-hint">{$t('chat.allowChangesScope')}</p>
        {#if contextBookIds.length > MAX_CONTEXT_BOOKS}<p class="field-hint">{$t('chat.contextSample', { sample: MAX_CONTEXT_BOOKS, count: contextBookIds.length })}</p>{/if}
        <label class="sr-only" for="chat-input">{$t('chat.placeholder')}</label><textarea id="chat-input" class="textarea" bind:value={draft} maxlength={MAX_TEXT_LENGTH} placeholder={$t('chat.placeholder')} onkeydown={handleKey} disabled={sending} aria-describedby="chat-context-count chat-text-count"></textarea>
        <div class="composer-footer"><p id="chat-context-count" class="field-hint" class:context-overflow={contextTooLarge}>{$t('chat.selectedBooks', { count: contextBookIds.length })} / {MAX_SELECTED_BOOKS}</p><span id="chat-text-count" class="field-hint">{number.format(draft.length)} / {number.format(MAX_TEXT_LENGTH)}</span><button class="button primary" type="submit" disabled={!canSend}><Send size={16} />{$t('chat.send')}</button></div>
      </form>
    </div>
  </div>
</section>

<style>
  .chat-layout { min-height: 650px; }
  .chat-history h2 { margin: 14px 0 4px; }
  .conversation-title { display: block; font-weight: 600; overflow-wrap: anywhere; }
  .conversation-date { display: block; margin-top: 6px; color: var(--text-muted); font-size: 11px; }
  .configuration-hint { padding: 12px 20px; border-bottom: 1px solid var(--border); color: var(--text-muted); font-size: 12px; }
  .message-role { display: flex; align-items: center; gap: 7px; }
  .message-role time { margin-inline-start: auto; font-size: 10px; font-weight: 400; }
  .message-content { white-space: pre-wrap; overflow-wrap: anywhere; }
  .chat-empty { min-height: 280px; }
  .thinking { display: flex; align-items: center; gap: 9px; }
  .chat-job { padding: 10px 20px; border-top: 1px solid var(--border); background: var(--surface-muted); }
  .chat-job progress { margin: 5px 0 8px; }
  .chat-composer { display: flex; flex-direction: column; gap: 10px; }
  .chat-composer .textarea { min-height: 90px; max-height: 180px; }
  .composer-footer { display: flex; align-items: center; flex-wrap: wrap; gap: 10px; }
  .composer-footer .button { margin-inline-start: auto; }
  .context-overflow { color: var(--danger); font-weight: 650; }
  .selected-books { padding: 12px 20px; border-bottom: 1px solid var(--border); }
  .selected-book-list { max-height: 140px; overflow-y: auto; }
  .selected-book-list .source-chip { overflow-wrap: anywhere; white-space: normal; }
</style>
