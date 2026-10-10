<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { getCurrentWebview } from '@tauri-apps/api/webview';
  import type { UnlistenFn } from '@tauri-apps/api/event';
  import {
    Activity, BookOpen, Heart, Layers, Library, LoaderCircle, MessageSquare,
    Plus, RefreshCw, Search, Settings as SettingsIcon, Tablet, Tags, Users, X,
  } from '@lucide/svelte';
  import LibraryView, { type LibraryViewState } from './lib/components/LibraryView.svelte';
  import BookPanel from './lib/components/BookPanel.svelte';
  import DevicesView from './lib/components/DevicesView.svelte';
  import ChatView from './lib/components/ChatView.svelte';
  import SettingsView from './lib/components/SettingsView.svelte';
  import ReaderView from './lib/components/ReaderView.svelte';
  import ActivityView from './lib/components/ActivityView.svelte';
  import DeviceTransferDialog from './lib/components/DeviceTransferDialog.svelte';
  import RemoveBooksDialog from './lib/components/RemoveBooksDialog.svelte';
  import { chooseBooks, isNative, isPreview, normalizePublicError, request, subscribe } from './lib/api';
  import { defaultQuery } from './lib/contracts';
  import type { AppBootstrap, AppError, Book, BookQuery, Device, Job, OptimizationProfile, Provider, RemoveBooksResult, Settings } from './lib/contracts';
  import { setLanguage, t } from './lib/i18n';
  import { createRequestScheduler } from './lib/request-scheduler';
  import { isMetadataReady, metadataDisabledReason as getMetadataDisabledReason, MAX_SELECTED_BOOKS } from './lib/selection-capabilities';

  type View = 'library' | 'devices' | 'chat' | 'activity' | 'settings';
  type NavigationKey = View | 'authors' | 'series' | 'genres' | 'reading' | 'favorites';
  type GroupBy = 'none' | 'author' | 'series' | 'genre';

  const navigation = [
    { key: 'library', label: 'sidebar.library', icon: Library },
    { key: 'authors', label: 'filters.authors', icon: Users },
    { key: 'series', label: 'filters.series', icon: Layers },
    { key: 'genres', label: 'filters.genres', icon: Tags },
    { key: 'reading', label: 'sidebar.reading', icon: BookOpen },
    { key: 'favorites', label: 'sidebar.favorites', icon: Heart },
    { key: 'devices', label: 'sidebar.devices', icon: Tablet },
    { key: 'chat', label: 'sidebar.chat', icon: MessageSquare },
    { key: 'activity', label: 'sidebar.activity', icon: Activity },
  ] as const;

  let bootstrap = $state<AppBootstrap | null>(null);
  let devices = $state<Device[]>([]);
  let jobs = $state<Job[]>([]);
  let providers = $state<Provider[]>([]);
  let profiles = $state<OptimizationProfile[]>([]);
  let capabilitiesGeneration = 0;
  let transferRequest = $state<{ bookIds: string[]; initialDeviceId: string | null } | null>(null);
  let removeBookIds = $state<string[] | null>(null);
  let transferTargets = $state<Record<string, string>>({});
  let startupBusy = $state(true);
  let importing = $state(false);
  let choosingBooks = $state(false);
  let importGeneration = 0;
  const IMPORT_BATCH_SIZE = 200;
  let dragging = $state(false);
  let activeView = $state<View>('library');
  let navigationKey = $state<NavigationKey>('library');
  let initialQuery = $state<BookQuery>(defaultQuery());
  let libraryState = $state<LibraryViewState | null>(null);
  let groupBy = $state<GroupBy>('none');
  let search = $state('');
  let selectedBookIds = $state<string[]>([]);
  let verifyRequested = $state(false);
  let detailBook = $state<Book | null>(null);
  let readingBook = $state<Book | null>(null);
  let refreshVersion = $state(0);
  let error = $state<AppError | null>(null);
  let toast = $state<string | null>(null);
  let searchInput = $state<HTMLInputElement>();
  let toastTimer: ReturnType<typeof setTimeout> | undefined;
  let destroyed = false;
  const unlisteners: UnlistenFn[] = [];
  let jobEventSequence = 0;
  const jobEventVersions = new Map<string, number>();

  function mergeJobSnapshot(snapshot: Job[], startedAtSequence: number): boolean {
    if (destroyed) return false;
    let updatedDuringRead = false;
    const merged = new Map(jobs.map((job) => [job.id, job]));
    for (const job of snapshot) {
      const existing = merged.get(job.id);
      // A database snapshot may resolve/rebase a review without changing the
      // job timestamp. Prefer it unless a later event updated the same job.
      if ((jobEventVersions.get(job.id) ?? 0) > startedAtSequence) {
        updatedDuringRead = true;
        continue;
      }
      if (existing && existing.updatedAt > job.updatedAt) continue;
      merged.set(job.id, job);
    }
    jobs = [...merged.values()];
    return updatedDuringRead;
  }

  const jobRequests = createRequestScheduler<void, { jobs: Job[]; startedAtSequence: number }>({
    load: async () => {
      const startedAtSequence = jobEventSequence;
      return { jobs: await request('jobs_list', undefined), startedAtSequence };
    },
    onStart: () => undefined,
    onSuccess: ({ jobs: snapshot, startedAtSequence }) => {
      if (mergeJobSnapshot(snapshot, startedAtSequence)) jobRequests.refresh();
    },
    onError: reportError,
    onSettled: () => undefined,
    queryDelayMs: 0,
  });

  const connectedDevice = $derived(devices.find((device) => device.connected) ?? null);
  const transferReady = $derived(devices.some((device) => device.connected && device.writable));
  const transferReceipts = $derived(jobs.flatMap((job) => {
    const deviceId = transferTargets[job.id];
    return job.kind === 'transfer' && deviceId ? [{ deviceId, job }] : [];
  }));
  const metadataReady = $derived(isMetadataReady(bootstrap?.settings, providers));
  const metadataDisabledReason = $derived(getMetadataDisabledReason(bootstrap?.settings, providers));
  const activeJobCount = $derived(jobs.filter((job) => job.status === 'queued' || job.status === 'running').length);
  const preview = isPreview();

  function notify(message: string): void {
    if (destroyed) return;
    if (toastTimer) clearTimeout(toastTimer);
    toast = message;
    toastTimer = setTimeout(() => { toast = null; }, 6000);
  }

  function reportError(cause: unknown): void {
    if (!destroyed) error = normalizePublicError(cause);
  }

  function applySettings(settings: Settings): void {
    setLanguage(settings.language, bootstrap?.systemLanguage);
    document.documentElement.dataset.theme = settings.theme;
  }

  function changeSettings(settings: Settings): void {
    if (bootstrap) bootstrap.settings = settings;
    applySettings(settings);
    refreshVersion += 1;
    void refreshCapabilities();
  }

  async function refreshCapabilities(): Promise<void> {
    const generation = ++capabilitiesGeneration;
    providers = [];
    const results = await Promise.allSettled([
      request('providers_list', undefined), request('optimization_profiles', undefined),
    ]);
    if (destroyed || generation !== capabilitiesGeneration) return;
    const [providerResult, profileResult] = results;
    if (providerResult.status === 'fulfilled') providers = providerResult.value;
    else reportError(providerResult.reason);
    if (profileResult.status === 'fulfilled') profiles = profileResult.value;
    else { profiles = []; reportError(profileResult.reason); }
  }

  function notifySettings(message: string): void {
    notify(message);
    void refreshCapabilities();
  }

  function navigate(key: NavigationKey): void {
    verifyRequested = false;
    navigationKey = key;
    readingBook = null;
    detailBook = null;
    if (key === 'devices' || key === 'chat' || key === 'activity' || key === 'settings') {
      activeView = key;
      return;
    }
    activeView = 'library';
    libraryState = null;
    const query = defaultQuery();
    groupBy = key === 'authors' ? 'author' : key === 'series' ? 'series' : key === 'genres' ? 'genre' : 'none';
    if (key === 'authors') query.sort = 'author';
    if (key === 'series') query.sort = 'series';
    if (key === 'reading') query.readStatus = 'reading';
    if (key === 'favorites') query.favorite = true;
    initialQuery = query;
  }

  function openAssistant(): void {
    navigationKey = 'chat';
    activeView = 'chat';
    readingBook = null;
    detailBook = null;
  }

  function verifySelectedBooks(): void {
    if (preview || !metadataReady || selectedBookIds.length === 0 || selectedBookIds.length > MAX_SELECTED_BOOKS) return;
    selectedBookIds = [...selectedBookIds];
    verifyRequested = true;
    openAssistant();
  }

  function transferSelectedBooks(initialDeviceId?: string): void {
    if (preview || transferRequest !== null || removeBookIds !== null || !transferReady || selectedBookIds.length === 0 || selectedBookIds.length > MAX_SELECTED_BOOKS) return;
    if (initialDeviceId && !devices.some((device) => device.id === initialDeviceId && device.connected && device.writable)) return;
    detailBook = null;
    transferRequest = { bookIds: [...new Set(selectedBookIds)], initialDeviceId: initialDeviceId ?? null };
  }

  function removeSelectedBooks(): void {
    if (destroyed || preview || removeBookIds !== null || transferRequest !== null) return;
    const ids = [...new Set(selectedBookIds)];
    if (ids.length === 0 || ids.length > MAX_SELECTED_BOOKS || ids.some((id) => !id.trim())) return;
    removeBookIds = ids;
  }

  function acceptTransfer(job: Job, deviceId: string): void {
    if (destroyed || job.kind !== 'transfer' || !deviceId) return;
    // Keep the human-confirmed target through view changes; job updates replace
    // the receipt's status/result without inventing a public backend payload.
    transferTargets = { ...transferTargets, [job.id]: deviceId };
    updateJob(job);
  }

  function acceptRemoval(result: RemoveBooksResult): void {
    if (destroyed) return;
    const removed = new Set(result.removedBookIds);
    selectedBookIds = selectedBookIds.filter((id) => !removed.has(id));
    if (detailBook && removed.has(detailBook.id)) detailBook = null;
    if (readingBook && removed.has(readingBook.id)) readingBook = null;
    removeBookIds = null;
    refreshVersion += 1;
  }

  function chooseAssistantBooks(): void {
    verifyRequested = false;
    navigationKey = 'library';
    activeView = 'library';
    readingBook = null;
    detailBook = null;
  }

  function openReader(book: Book): void {
    detailBook = null;
    readingBook = book;
  }

  async function loadBootstrap(): Promise<void> {
    const startedAtSequence = jobEventSequence;
    startupBusy = true;
    error = null;
    try {
      const result = await request('app_bootstrap', undefined);
      if (destroyed) return;
      bootstrap = result;
      devices = result.devices;
      mergeJobSnapshot(result.pendingJobs, startedAtSequence);
      applySettings(result.settings);
      await refreshCapabilities();
    } catch (cause: unknown) {
      reportError(cause);
    } finally {
      if (!destroyed) startupBusy = false;
    }
  }

  async function importPaths(paths: string[]): Promise<void> {
    if (destroyed || importing || choosingBooks) return;
    const uniquePaths = [...new Set(paths.filter((path) => path.length > 0))];
    if (uniquePaths.length === 0) return;
    const generation = ++importGeneration;
    importing = true;
    let queued = 0;
    try {
      for (let offset = 0; offset < uniquePaths.length; offset += IMPORT_BATCH_SIZE) {
        if (destroyed || generation !== importGeneration) return;
        const batch = uniquePaths.slice(offset, offset + IMPORT_BATCH_SIZE);
        const job = await request('import_books', { paths: batch });
        if (destroyed || generation !== importGeneration) return;
        jobs = [...jobs.filter((existing) => existing.id !== job.id), job];
        queued += batch.length;
        if (offset === 0) navigate('library');
        notify($t('library.importQueued', { count: queued, total: uniquePaths.length }));
        refreshVersion += 1;
      }
    } catch (cause: unknown) {
      if (generation === importGeneration) reportError(cause);
    } finally {
      if (!destroyed && generation === importGeneration) importing = false;
    }
  }

  async function importBooks(): Promise<void> {
    if (destroyed || importing || choosingBooks) return;
    choosingBooks = true;
    try {
      const paths = await chooseBooks($t('library.import'));
      choosingBooks = false;
      await importPaths(paths);
    } catch (cause: unknown) {
      reportError(cause);
    } finally {
      if (!destroyed) choosingBooks = false;
    }
  }

  function keepSubscription(unlisten: UnlistenFn): void {
    if (destroyed) void Promise.allSettled([Promise.resolve().then(unlisten)]);
    else unlisteners.push(unlisten);
  }

  function updateJob(job: Job): void {
    if (destroyed) return;
    const previous = jobs.find((candidate) => candidate.id === job.id);
    jobEventVersions.set(job.id, ++jobEventSequence);
    jobs = [...jobs.filter((candidate) => candidate.id !== job.id), job];
    if (!previous && job.kind === 'transfer' && !['completed', 'failed', 'cancelled'].includes(job.status)) {
      // Let the device view adopt an accepted transfer before its completion.
      refreshVersion += 1;
    }
    if (previous?.status !== job.status && ['completed', 'failed', 'cancelled'].includes(job.status)) {
      refreshVersion += 1;
      if (job.error) reportError(job.error);
      else notify(`${$t(`jobs.${job.kind}`)} · ${$t(`jobs.${job.status}`)}`);
    }
  }

  function focusSearch(event: KeyboardEvent): void {
    if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === 'k') {
      event.preventDefault();
      searchInput?.focus();
      searchInput?.select();
    }
  }

  onMount(() => {
    jobRequests.setQuery(undefined);
    void loadBootstrap();
    document.addEventListener('keydown', focusSearch);
    if (isNative() || preview) {
      void Promise.allSettled([
        subscribe('library:changed', () => {
          if (destroyed) return;
          refreshVersion += 1;
          jobRequests.refresh();
        }),
        subscribe('devices:changed', (event) => { if (!destroyed) devices = event.devices; }),
        subscribe('job:updated', (event) => updateJob(event.job)),
      ]).then((results) => {
        for (const result of results) {
          if (result.status === 'fulfilled') keepSubscription(result.value);
          else reportError(result.reason);
        }
      });
    }
    if (isNative()) {
      void getCurrentWebview().onDragDropEvent(({ payload }) => {
        if (destroyed) return;
        dragging = payload.type === 'enter' || payload.type === 'over';
        if (payload.type === 'drop') void importPaths(payload.paths);
      }).then(keepSubscription).catch(reportError);
    }
  });

  onDestroy(() => {
    destroyed = true;
    importGeneration += 1;
    capabilitiesGeneration += 1;
    jobRequests.dispose();
    if (toastTimer) clearTimeout(toastTimer);
    if (typeof document !== 'undefined') document.removeEventListener('keydown', focusSearch);
    void Promise.allSettled(unlisteners.map((unlisten) => Promise.resolve().then(unlisten)));
  });
</script>

<a class="skip-link" href="#main-content">{$t('library.title')}</a>

<div class="app-shell">
  <aside class="sidebar" aria-label={$t('app.name')}>
    <div class="sidebar-brand">
      <img src="/library-manager.png" alt="" width="42" height="42" />
      <div class="brand-text">Library<br />Manager<span class="brand-subtitle">{$t('sidebar.library')}</span></div>
    </div>
    <nav class="sidebar-nav" aria-label={$t('app.name')}>
      {#each navigation as item (item.key)}
        <button
          class="nav-item"
          class:active={navigationKey === item.key}
          aria-current={navigationKey === item.key ? 'page' : undefined}
          aria-label={$t(item.label)}
          title={$t(item.label)}
          onclick={() => navigate(item.key)}
        >
          <item.icon size={19} aria-hidden="true" />
          <span class="nav-label">{$t(item.label)}</span>
          {#if item.key === 'activity' && activeJobCount > 0}<span class="nav-count">{activeJobCount}</span>{/if}
        </button>
      {/each}
    </nav>
    <div class="sidebar-footer">
      <div class="sidebar-card">
        <span class="eyebrow">{$t('devices.title')}</span>
        <strong>{connectedDevice?.label ?? $t('devices.disconnected')}</strong>
        <span>{connectedDevice ? $t('devices.connected') : $t('devices.emptyDescription')}</span>
      </div>
      <button class="nav-item" class:active={navigationKey === 'settings'} aria-current={navigationKey === 'settings' ? 'page' : undefined} aria-label={$t('sidebar.settings')} title={$t('sidebar.settings')} onclick={() => navigate('settings')}>
        <SettingsIcon size={19} aria-hidden="true" /><span class="nav-label">{$t('sidebar.settings')}</span>
      </button>
      <span class="small muted nav-label">{$t('settings.version', { name: bootstrap?.version ?? '0.1.0' })}</span>
    </div>
  </aside>

  <div class="main-panel">
    {#if preview}
      <div class="preview-banner" role="status">
        <strong>{$t('app.preview')}</strong><span>{$t('app.previewDescription')}</span>
      </div>
    {/if}
    <header class="topbar">
      <div class="global-search">
        <Search size={18} aria-hidden="true" />
        <input
          class="input"
          type="search"
          bind:this={searchInput}
          bind:value={search}
          aria-label={$t('search.label')}
          placeholder={$t('search.placeholder')}
          oninput={() => { if (activeView !== 'library' || readingBook) navigate('library'); }}
        />
      </div>
      <div class="row">
        <button class="device-indicator" onclick={() => navigate('devices')} title={$t('devices.title')}>
          <span class="status-dot" class:connected={connectedDevice !== null}></span>
          <span>{connectedDevice?.label ?? $t('devices.disconnected')}</span>
        </button>
        <button class="icon-button" aria-label={$t('library.import')} title={$t('library.import')} disabled={preview || startupBusy || !bootstrap || importing || choosingBooks} onclick={() => void importBooks()}><Plus size={19} aria-hidden="true" /></button>
      </div>
    </header>

    <main id="main-content" tabindex="-1">
      {#if error}
        <div class="shell-error">
          <div class="error-banner" role="alert">
            <div class="grow"><strong>{$t(`errors.${error.code}`)}</strong>{#if !isNative() && !preview}<p>{$t('app.previewDescription')}</p>{/if}</div>
            <button class="icon-button" aria-label={$t('actions.close')} onclick={() => { error = null; }}><X size={18} aria-hidden="true" /></button>
          </div>
        </div>
      {/if}
      {#if startupBusy}
        <div class="empty-state" role="status"><LoaderCircle size={28} aria-hidden="true" /><h2>{$t('app.loading')}</h2></div>
      {:else if !bootstrap}
        <div class="empty-state">
          <Library size={48} aria-hidden="true" /><h2>{$t('app.initializationError')}</h2>
          <p>{$t('app.previewDescription')}</p>
          <div class="row wrap">
            <button class="button primary" onclick={() => void loadBootstrap()}><RefreshCw size={17} aria-hidden="true" />{$t('actions.retry')}</button>
            {#if !isNative() && !preview}<a class="button secondary" href="?demo=1">{$t('app.preview')}</a>{/if}
          </div>
        </div>
      {:else if readingBook}
        <ReaderView book={readingBook} onClose={() => { readingBook = null; }} onNotify={notify} onError={reportError} />
      {:else if activeView === 'library'}
        <LibraryView
          {initialQuery} {search} {groupBy} {refreshVersion} {selectedBookIds} {importing} {devices} {jobs}
          initialState={libraryState} onStateChange={(state: LibraryViewState) => { libraryState = state; }}
          onSelectionChange={(ids: string[]) => { selectedBookIds = ids; }}
          onOpenBook={(book: Book) => { detailBook = book; }}
          onOpenAssistant={openAssistant} onVerifySelected={verifySelectedBooks}
          {metadataReady} {metadataDisabledReason} {transferReady}
          onTransferSelected={transferSelectedBooks} onOpenSettings={() => navigate('settings')}
          onRemoveSelected={removeSelectedBooks}
          onReadBook={openReader} onImport={importBooks} onNotify={notify} onError={reportError}
        />
      {:else if activeView === 'devices'}
        <DevicesView
          {devices} {selectedBookIds} {refreshVersion} {transferReceipts}
          {metadataReady} {metadataDisabledReason} {transferReady}
          onOpenAssistant={openAssistant} onVerifySelected={verifySelectedBooks}
          onTransferSelected={transferSelectedBooks} onOpenSettings={() => navigate('settings')}
          onRemoveSelected={removeSelectedBooks} onClearSelection={() => { selectedBookIds = []; }}
          onDevicesChange={(next: Device[]) => { devices = next; }} onNotify={notify} onError={reportError}
        />
      {:else if activeView === 'chat'}
        <ChatView
          {selectedBookIds} {refreshVersion} {verifyRequested}
          {metadataReady} {metadataDisabledReason} {transferReady}
          onTransferSelected={transferSelectedBooks} onOpenSettings={() => navigate('settings')}
          onRemoveSelected={removeSelectedBooks} onClearSelection={() => { selectedBookIds = []; }}
          onVerifyStarted={() => { verifyRequested = false; }} onChooseBooks={chooseAssistantBooks}
          onOpenBook={(book: Book) => { detailBook = book; }} onNotify={notify} onError={reportError}
        />
      {:else if activeView === 'activity'}
        <ActivityView {refreshVersion} onNotify={notify} onError={reportError} />
      {:else if activeView === 'settings'}
        <SettingsView settings={bootstrap.settings} onSettingsChange={changeSettings} onNotify={notifySettings} onError={reportError} />
      {/if}
    </main>
  </div>
</div>

{#if detailBook}
  <BookPanel
    book={detailBook} {refreshVersion} onClose={() => { detailBook = null; }} onReadBook={openReader}
    {metadataReady} {metadataDisabledReason} onOpenSettings={() => navigate('settings')}
    onUpdated={(book: Book) => { detailBook = book; refreshVersion += 1; }} onNotify={notify} onError={reportError}
  />
{/if}

{#if transferRequest}
  <DeviceTransferDialog bookIds={transferRequest.bookIds} {devices} {profiles}
    initialDeviceId={transferRequest.initialDeviceId} onClose={() => { transferRequest = null; }}
    onQueued={acceptTransfer} onNotify={notify} />
{/if}

{#if removeBookIds}
  <RemoveBooksDialog bookIds={removeBookIds} onClose={() => { removeBookIds = null; }}
    onRemoved={acceptRemoval} onNotify={notify} />
{/if}

{#if dragging}
  <div class="drop-overlay" role="status"><div class="drop-zone active"><Plus size={32} aria-hidden="true" /><h2>{$t('library.dropTitle')}</h2><p>{$t('library.dropDescription')}</p></div></div>
{/if}

{#if toast}
  <div class="toast" role="status"><span class="grow">{toast}</span><button class="icon-button" aria-label={$t('actions.close')} onclick={() => { toast = null; }}><X size={18} aria-hidden="true" /></button></div>
{/if}

<style>
  .sidebar { overflow-y: auto; }
  .shell-error { padding: 22px 40px 0; }
  .shell-error .error-banner { margin-bottom: 0; }
  .drop-overlay { position: fixed; inset: 0; z-index: var(--z-drawer); display: grid; place-items: center; padding: 32px; background: var(--overlay); pointer-events: none; }
  .drop-overlay .drop-zone { width: min(560px, 100%); box-shadow: var(--shadow-md); }
  @media (max-width: 1200px) { .shell-error { padding-inline: 28px; } }
  @media (max-width: 680px) { .shell-error { padding-inline: 18px; } }
</style>
