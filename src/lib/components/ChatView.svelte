<script lang="ts">
  import { onDestroy, onMount, tick, untrack } from 'svelte';
  import { ExternalLink, MessageCircle, Plus, RefreshCw, Send, Sparkles, UserRound, X } from '@lucide/svelte';
  import { isPreview, normalizePublicError, openExternal, request, subscribe } from '../api';
  import type { AppError, ChatDeltaEvent, ChatMessage, Conversation, Job, Provider, Settings, WebSource } from '../contracts';
  import { formatDate, locale, t } from '../i18n';

  interface Props {
    selectedBookIds: string[];
    refreshVersion: number;
    onNotify(message: string): void;
    onError(error: unknown): void;
  }
  let { selectedBookIds, refreshVersion, onNotify, onError }: Props = $props();
  const MAX_TEXT_LENGTH = 8000;
  const MAX_CONTEXT_BOOKS = 32;
  const demo = isPreview();
  let disposed = false;
  let configGeneration = 0;
  let historyGeneration = 0;
  let messageGeneration = 0;
  let viewVersion = 0;
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
  let failure = $state<AppError | null>(null);
  let chatScroll = $state<HTMLDivElement>();
  const currentProvider = $derived(providers.find((provider) => provider.id === settings?.providerId));
  const configured = $derived(Boolean(settings?.modelId && currentProvider?.configured && currentProvider.status === 'ready'));
  const jobPending = $derived(activeJob !== null && !['completed', 'failed', 'cancelled'].includes(activeJob.status));
  const contextBookIds = $derived([...new Set(selectedBookIds)]);
  const contextTooLarge = $derived(contextBookIds.length > MAX_CONTEXT_BOOKS);
  const canSend = $derived(!demo && configured && !sending && !jobPending && !contextTooLarge && draft.trim().length > 0 && draft.length <= MAX_TEXT_LENGTH);
  const number = $derived(new Intl.NumberFormat($locale));

  function report(error: unknown): void {
    failure = normalizePublicError(error);
    onError(failure);
  }
  function isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
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
      const updated = id ? jobs.find((job) => job.id === id) : jobs.filter((job) => job.kind === 'chat'
        && !['completed', 'failed', 'cancelled'].includes(job.status))
        .sort((left, right) => right.createdAt.localeCompare(left.createdAt))[0];
      if (updated) trackJob(updated);
    } catch (error) { if (!disposed && activeJob?.id === id) report(error); }
  }
  async function connectEvents(): Promise<void> {
    const results = await Promise.allSettled([
      subscribe('job:updated', ({ job }) => { if (!disposed && job.id === activeJob?.id) trackJob(job); }),
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
    untrack(() => { void loadConfiguration(); void loadHistory(); void recoverJob(); });
  });
  onMount(() => { void connectEvents(); });
  onDestroy(() => {
    disposed = true;
    configGeneration += 1;
    historyGeneration += 1;
    messageGeneration += 1;
    void Promise.allSettled(unlisteners.map((unsubscribe) => Promise.resolve().then(unsubscribe)));
  });

  async function send(): Promise<void> {
    if (!canSend) return;
    const text = draft.trim();
    const previousConversation = activeConversationId;
    const sentViewVersion = viewVersion;
    sending = true;
    failure = null;
    try {
      const job = await request('chat_send', { conversationId: previousConversation, text, bookIds: [...contextBookIds] });
      if (disposed) return;
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
    await Promise.allSettled([loadConfiguration(), loadHistory(), recoverJob(), activeConversationId ? loadMessages(activeConversationId) : Promise.resolve()]);
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
  {#if failure}<div class="error-banner" role="alert"><div class="grow"><strong>{$t(`errors.${failure.code}`)}</strong>{#if failure.detail}<p>{failure.detail}</p>{/if}</div><button class="icon-button" type="button" onclick={() => { failure = null; }} aria-label={$t('actions.close')}><X size={16} /></button></div>{/if}
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
        <label class="sr-only" for="chat-input">{$t('chat.placeholder')}</label><textarea id="chat-input" class="textarea" bind:value={draft} maxlength={MAX_TEXT_LENGTH} placeholder={$t('chat.placeholder')} onkeydown={handleKey} disabled={sending} aria-describedby="chat-context-count chat-text-count"></textarea>
        <div class="composer-footer"><p id="chat-context-count" class="field-hint" class:context-overflow={contextTooLarge}>{$t('chat.selectedBooks', { count: contextBookIds.length })} / {MAX_CONTEXT_BOOKS}</p><span id="chat-text-count" class="field-hint">{number.format(draft.length)} / {number.format(MAX_TEXT_LENGTH)}</span><button class="button primary" type="submit" disabled={!canSend}><Send size={16} />{$t('chat.send')}</button></div>
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
</style>
