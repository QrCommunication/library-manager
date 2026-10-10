<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { BookOpen, Cable, Check, Link, RefreshCw, Send, Tablet, Unplug, Wifi, X } from '@lucide/svelte';
  import { isPreview, normalizePublicError, PublicError, request } from '../api';
  import type { BookFormat, Device, Job, WirelessTransport } from '../contracts';
  import { formatDate, formatSize, locale, t } from '../i18n';
  import SelectionActions from './SelectionActions.svelte';
  import { MAX_SELECTED_BOOKS, type MetadataDisabledReason } from '../selection-capabilities';

  interface Props {
    devices: Device[];
    selectedBookIds: string[];
    refreshVersion: number;
    onDevicesChange(devices: Device[]): void;
    onNotify(message: string): void;
    onError(error: unknown): void;
    metadataReady?: boolean;
    metadataDisabledReason?: MetadataDisabledReason | null;
    transferReady?: boolean;
    onOpenAssistant?(): void;
    onVerifySelected?(): void;
    onTransferSelected?(initialDeviceId?: string): void;
    onRemoveSelected?(): void;
    onClearSelection?(): void;
    onOpenSettings?(): void;
  }
  let { devices, selectedBookIds, refreshVersion, onDevicesChange, onNotify, onError, metadataReady = false,
    metadataDisabledReason = null, transferReady = false, onOpenAssistant, onVerifySelected,
    onTransferSelected, onRemoveSelected, onClearSelection, onOpenSettings }: Props = $props();
  let pairDialog: HTMLDialogElement;
  let inventoryDialog: HTMLDialogElement;
  interface InventoryBook {
    relativePath: string;
    title: string;
    authors: string[];
    format: BookFormat;
    sizeBytes: number;
    bookId: string | null;
  }
  interface Inventory {
    deviceId: string;
    books: InventoryBook[];
    total: number | null;
    truncated: boolean;
    warnings: string[];
    updatedAt: string;
  }
  let receipts = $state<Record<string, Job>>({});
  let inventories = $state<Record<string, Inventory>>({});
  let inventoryDeviceId = $state<string | null>(null);
  let inventorySearch = $state('');
  let inventoryMatch = $state<'all' | 'known' | 'unknown'>('all');
  interface IndexProgress {
    phase: 'discovering' | 'reading' | 'finalizing';
    visitedEntries: number;
    processedBooks: number;
    totalBooks: number;
    bytesRead: number;
    totalBytes: number;
    currentPath: string | null;
  }
  let inventoryLoading = $state(false);
  let inventoryGeneration = 0;
  const INVENTORY_PAGE_SIZE = 200;
  let jobsGeneration = 0;
  let pollTimer: ReturnType<typeof setTimeout> | undefined;
  const POLL_INTERVAL_MS = 2500;
  const FORMATS: readonly string[] = ['epub', 'mobi', 'azw3', 'fb2', 'txt', 'html', 'pdf', 'cbz'];
  const inventoryDevice = $derived(devices.find((device) => device.id === inventoryDeviceId));
  const inventory = $derived(inventoryDeviceId === null ? undefined : inventories[inventoryDeviceId]);
  const inventoryReceipt = $derived(inventoryDeviceId === null ? undefined : receipts[inventoryDeviceId]);
  const inventoryRows = $derived((inventory?.books ?? []).filter((book) => {
    const matchesPresence = inventoryMatch === 'all' || (inventoryMatch === 'known' ? book.bookId !== null : book.bookId === null);
    const search = inventorySearch.trim().toLocaleLowerCase($locale);
    return matchesPresence && (!search || [book.title, book.relativePath, ...book.authors, book.format].join(' ').toLocaleLowerCase($locale).includes(search));
  }));
  let disposed = false;
  let scanning = $state(false);
  let busyDevice = $state<string | null>(null);
  let pairing = $state(false);
  let failure = $state<PublicError | null>(null);
  let pairFailure = $state<PublicError | null>(null);
  let transport = $state<WirelessTransport>('crosspoint');
  let address = $state('');
  let label = $state('');
  let password = $state('');
  const demo = isPreview();
  const actionsBusy = $derived(scanning || pairing || busyDevice !== null);
  const number = $derived(new Intl.NumberFormat($locale));
  const percent = $derived(new Intl.NumberFormat($locale, { style: 'percent', maximumFractionDigits: 0 }));

  function report(error: unknown, surface: 'page' | 'pair' = 'page'): void {
    const normalized = normalizePublicError(error);
    if (surface === 'pair') pairFailure = normalized;
    else failure = normalized;
    onError(normalized);
  }
  function invalidInput(): never {
    throw new PublicError({ code: 'invalidInput', message: $t('errors.invalidInput'), detail: null, retryable: false });
  }
  function storageUsed(device: Device): number | null {
    if (device.totalBytes === null || device.freeBytes === null || device.totalBytes <= 0) return null;
    return Math.min(1, Math.max(0, (device.totalBytes - device.freeBytes) / device.totalBytes));
  }
  function rememberJob(deviceId: string, job: Job): void {
    receipts = { ...receipts, [deviceId]: job };
    onNotify(`${$t(`jobs.${job.kind}`)} · ${$t(`jobs.${job.status}`)}`);
    schedulePoll([job]);
  }
  function record(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  }
  function inventoryBook(value: unknown): value is InventoryBook {
    return record(value) && typeof value.relativePath === 'string' && value.relativePath.length > 0
      && typeof value.title === 'string' && Array.isArray(value.authors) && value.authors.every((author) => typeof author === 'string')
      && typeof value.format === 'string' && FORMATS.includes(value.format)
      && typeof value.sizeBytes === 'number' && Number.isSafeInteger(value.sizeBytes) && value.sizeBytes >= 0
      && (value.bookId === null || typeof value.bookId === 'string');
  }
  function parseIndexProgress(job: Job): IndexProgress | null {
    if (job.kind !== 'deviceIndex' || !record(job.result) || !record(job.result.indexProgress)) return null;
    const value = job.result.indexProgress;
    if (typeof value.phase !== 'string' || !['discovering', 'reading', 'finalizing'].includes(value.phase)) return null;
    const counters = ['visitedEntries', 'processedBooks', 'totalBooks', 'bytesRead', 'totalBytes'] as const;
    if (!counters.every((key) => typeof value[key] === 'number' && Number.isSafeInteger(value[key]) && value[key] >= 0)) return null;
    if (!(value.currentPath === null || typeof value.currentPath === 'string')) return null;
    if ((value.processedBooks as number) > (value.totalBooks as number) || (value.bytesRead as number) > (value.totalBytes as number)) return null;
    return {
      phase: value.phase as IndexProgress['phase'], visitedEntries: value.visitedEntries as number,
      processedBooks: value.processedBooks as number, totalBooks: value.totalBooks as number,
      bytesRead: value.bytesRead as number, totalBytes: value.totalBytes as number,
      currentPath: value.currentPath,
    };
  }
  function readFraction(value: IndexProgress): number {
    return value.totalBytes > 0 ? value.bytesRead / value.totalBytes : value.totalBooks === 0 ? 0 : value.processedBooks / value.totalBooks;
  }
  async function loadInventory(append = false): Promise<void> {
    const id = inventoryDeviceId;
    if (!id || inventoryLoading || inventoryDevice?.transport !== 'usb' || !inventoryDevice.connected) return;
    const generation = ++inventoryGeneration;
    inventoryLoading = true;
    const previous = inventories[id];
    const target = append ? (previous?.books.length ?? 0) + INVENTORY_PAGE_SIZE : Math.max(INVENTORY_PAGE_SIZE, previous?.books.length ?? 0);
    let books = append ? [...(previous?.books ?? [])] : [];
    let offset = books.length;
    let total = previous?.total ?? 0;
    try {
      do {
        const page = await request('device_inventory', { id, offset, limit: INVENTORY_PAGE_SIZE, unknownOnly: false });
        if (disposed || inventoryDeviceId !== id || generation !== inventoryGeneration) return;
        books = [...new Map([...books, ...page.items].map((book) => [book.relativePath, book])).values()];
        offset += page.items.length;
        total = page.total;
        if (page.items.length === 0 || offset >= total) break;
      } while (books.length < target);
      inventories = { ...inventories, [id]: {
        deviceId: id, books, total, truncated: books.length < total,
        warnings: [], updatedAt: new Date().toISOString(),
      } };
    } catch (error) {
      if (!disposed && inventoryDeviceId === id && generation === inventoryGeneration) report(error);
    } finally {
      if (generation === inventoryGeneration) inventoryLoading = false;
    }
  }
  function parseInventory(job: Job): Inventory | null {
    if (job.kind !== 'deviceIndex' || job.status !== 'completed' || !record(job.result)) return null;
    const result = job.result;
    if (typeof result.deviceId !== 'string' || !Array.isArray(result.books) || !result.books.every(inventoryBook)) return null;
    const total = typeof result.total === 'number' && Number.isSafeInteger(result.total) && result.total >= result.books.length ? result.total : null;
    return {
      deviceId: result.deviceId, books: result.books,
      total: total ?? (result.truncated === true ? null : result.books.length),
      truncated: result.truncated === true || (total !== null && total > result.books.length),
      warnings: Array.isArray(result.warnings) ? result.warnings.filter((warning): warning is string => typeof warning === 'string') : [],
      updatedAt: job.updatedAt,
    };
  }
  function terminal(job: Job): boolean {
    return ['completed', 'failed', 'cancelled'].includes(job.status);
  }
  function warningText(warning: string): string {
    for (const key of [warning, `jobs.${warning}`, `errors.${warning}`]) {
      const translated = $t(key);
      if (translated !== key) return translated;
    }
    return warning;
  }
  function schedulePoll(jobs: Job[]): void {
    clearTimeout(pollTimer);
    pollTimer = undefined;
    if (!disposed && ((inventoryDevice?.transport === 'usb' && inventoryDevice.connected) || jobs.some((job) => !terminal(job) && (job.kind === 'deviceIndex' || Object.values(receipts).some((receipt) => receipt.id === job.id))))) {
      pollTimer = setTimeout(() => { void refreshJobs(); }, POLL_INTERVAL_MS);
    }
  }
  function applyJobs(jobs: Job[]): void {
    const latest = { ...inventories };
    const updatedReceipts = Object.fromEntries(Object.entries(receipts).map(([id, previous]) => [id, jobs.find((job) => job.id === previous.id) ?? previous]));
    for (const job of [...jobs].sort((left, right) => left.updatedAt.localeCompare(right.updatedAt))) {
      if (job.kind === 'deviceIndex' && record(job.result) && typeof job.result.deviceId === 'string') {
        const id = job.result.deviceId;
        const previousReceipt = updatedReceipts[id];
        if (!previousReceipt || job.updatedAt >= previousReceipt.updatedAt) updatedReceipts[id] = job;
      }
      const snapshot = parseInventory(job);
      if (!snapshot || devices.some((device) => device.id === snapshot.deviceId && device.transport === 'usb')) continue;
      const previousInventory = latest[snapshot.deviceId];
      if (!previousInventory || snapshot.updatedAt >= previousInventory.updatedAt) latest[snapshot.deviceId] = snapshot;
      const previous = updatedReceipts[snapshot.deviceId];
      if (!previous || (terminal(previous) && job.updatedAt >= previous.updatedAt)) updatedReceipts[snapshot.deviceId] = job;
    }
    inventories = latest;
    receipts = updatedReceipts;
    schedulePoll(jobs);
  }
  async function refreshJobs(): Promise<void> {
    const generation = ++jobsGeneration;
    try {
      const jobs = await request('jobs_list', undefined);
      if (!disposed && generation === jobsGeneration) {
        applyJobs(jobs);
        void loadInventory();
      }
    } catch (error) {
      if (!disposed && generation === jobsGeneration) {
        report(error);
        schedulePoll(Object.values(receipts));
      }
    }
  }
  function showInventory(device: Device): void {
    inventoryGeneration += 1;
    inventoryLoading = false;
    inventoryDeviceId = device.id;
    inventorySearch = '';
    inventoryMatch = 'all';
    inventoryDialog.showModal();
    void refreshJobs();
  }
  function closeInventory(): void {
    inventoryDialog.close();
    inventoryDeviceId = null;
    inventoryGeneration += 1;
    inventoryLoading = false;
    schedulePoll(Object.values(receipts));
  }
  $effect(() => {
    void refreshVersion;
    untrack(() => { void refreshJobs(); });
  });
  onDestroy(() => {
    disposed = true;
    jobsGeneration += 1;
    inventoryGeneration += 1;
    clearTimeout(pollTimer);
    inventoryDialog?.close();
    password = '';
    pairDialog?.close();
  });

  async function scan(): Promise<void> {
    if (scanning || pairing || busyDevice !== null) return;
    scanning = true;
    failure = null;
    try {
      const detected = await request('devices_scan', undefined);
      if (disposed) return;
      onDevicesChange(detected);
      onNotify($t('devices.scanComplete', { count: detected.filter((device) => device.connected).length }));
    } catch (error) { if (!disposed) report(error); }
    finally { if (!disposed) scanning = false; }
  }
  async function indexDevice(device: Device): Promise<void> {
    if (demo || !device.connected || scanning || pairing || busyDevice !== null) return;
    busyDevice = device.id;
    failure = null;
    try {
      const job = await request('device_index', { id: device.id });
      if (!disposed) rememberJob(device.id, job);
    } catch (error) { if (!disposed) report(error); }
    finally { if (!disposed) busyDevice = null; }
  }
  async function disconnect(device: Device): Promise<void> {
    if (demo || device.transport === 'usb' || scanning || pairing || busyDevice !== null) return;
    busyDevice = device.id;
    failure = null;
    try {
      await request('device_disconnect', { id: device.id });
      if (disposed) return;
      onDevicesChange(devices.filter((entry) => entry.id !== device.id));
      onNotify($t('devices.connectionRemoved', { name: device.label }));
      const detected = await request('devices_scan', undefined);
      if (!disposed) onDevicesChange(detected);
    } catch (error) { if (!disposed) report(error); }
    finally { if (!disposed) busyDevice = null; }
  }
  function showPairing(): void {
    pairFailure = null;
    password = '';
    pairDialog.showModal();
  }
  function changeTransport(value: string): void {
    if (value !== 'crosspoint' && value !== 'calibreWireless') return;
    transport = value;
    address = value === 'calibreWireless' ? '0.0.0.0:9090' : '';
    password = '';
    pairFailure = null;
  }
  function closePairing(): void {
    if (pairing) return;
    password = '';
    pairDialog.close();
  }
  function validateAddress(value: string): void {
    if (!value.trim() || /[\u0000-\u001f\u007f]/u.test(value)) invalidInput();
    if (transport === 'crosspoint') {
      try {
        const url = new URL(value);
        if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || url.hash) invalidInput();
      } catch { invalidInput(); }
    } else {
      const separator = value.lastIndexOf(':');
      const port = Number(value.slice(separator + 1));
      if (separator < 1 || !Number.isSafeInteger(port) || port < 1 || port > 65535 || /[/?#\s]/u.test(value)) invalidInput();
    }
  }
  async function pair(): Promise<void> {
    if (demo || pairing || scanning || busyDevice !== null) return;
    pairFailure = null;
    try {
      const deviceAddress = address.trim();
      validateAddress(deviceAddress);
      if (!label.trim()) invalidInput();
      pairing = true;
      const device = await request('device_connect_wireless', {
        address: deviceAddress, transport, label: label.trim(), password: password || null,
      });
      if (disposed) return;
      onDevicesChange([...devices.filter((entry) => entry.id !== device.id), device]);
      onNotify(`${$t('devices.connectionAdded', { name: device.label })} · ${$t(`devices.${device.connected ? 'connected' : 'disconnected'}`)}`);
      password = '';
      pairDialog.close();
      label = '';
      const detected = await request('devices_scan', undefined);
      if (!disposed) onDevicesChange(detected);
    } catch (error) { if (!disposed) report(error, pairDialog.open ? 'pair' : 'page'); }
    finally { if (!disposed) { pairing = false; password = ''; } }
  }
  function prepareTransfer(device: Device): void {
    if (demo || disposed || actionsBusy || !device.connected || !device.writable
      || selectedBookIds.length === 0 || selectedBookIds.length > MAX_SELECTED_BOOKS) return;
    onTransferSelected?.(device.id);
  }
</script>

{#snippet jobProgress(job: Job)}
  {@const progress = parseIndexProgress(job)}
  {#if progress && !terminal(job)}
    {#if progress.phase === 'discovering'}
      <progress max="1" aria-label={$t('common.progress')}></progress>
      <p class="field-hint">{$t('devices.inventoryDiscovering', { entries: number.format(progress.visitedEntries), books: number.format(progress.totalBooks) })}</p>
    {:else}
      {@const fraction = readFraction(progress)}
      <progress value={fraction} max="1" aria-label={$t('common.progress')}></progress>
      <p class="small">{$t('devices.inventoryProgress', { percent: percent.format(fraction) })}</p>
      <p class="field-hint">{$t('devices.inventoryReading', { processed: number.format(progress.processedBooks), total: number.format(progress.totalBooks), read: formatSize(progress.bytesRead, $locale), size: formatSize(progress.totalBytes, $locale) })}</p>
      {#if progress.phase === 'finalizing'}<p class="field-hint">{$t('devices.inventoryFinalizing')}</p>{/if}
    {/if}
    {#if progress.currentPath}<p class="small muted inventory-path">{progress.currentPath}</p>{/if}
  {:else}
    <progress value={job.progress} max="1" aria-label={$t('common.progress')}></progress>
  {/if}
{/snippet}


<section class="page">
  <div class="page-header">
    <div><p class="eyebrow">Library Manager</p><h1 class="page-title">{$t('devices.title')}</h1><p class="page-subtitle">{$t('devices.standaloneHint')}</p></div>
    <div class="toolbar"><button class="button secondary" type="button" disabled={scanning || pairing || busyDevice !== null} onclick={scan}><RefreshCw size={17} class={scanning ? 'scanning' : ''} />{$t('devices.scan')}</button><button class="button primary" type="button" disabled={demo || scanning || pairing || busyDevice !== null} onclick={showPairing}><Link size={17} />{$t('devices.connectWireless')}</button></div>
  </div>
  {#if failure}<div class="error-banner" role="alert"><div class="grow"><strong>{$t(`errors.${failure.code}`)}</strong>{#if failure.detail}<p>{failure.detail}</p>{/if}</div><button class="icon-button" type="button" onclick={() => { failure = null; }} aria-label={$t('actions.close')}><X size={16} /></button></div>{/if}
  <SelectionActions selectedCount={selectedBookIds.length} {metadataReady} {metadataDisabledReason} {transferReady}
    {demo} busy={actionsBusy} {onOpenAssistant} {onVerifySelected} {onTransferSelected} {onRemoveSelected}
    {onClearSelection} {onOpenSettings} />
  <p class="field-hint device-hint">{$t('devices.mtpHint')}</p>
  {#if devices.length}
    <div class="device-grid">
      {#each devices as device (device.id)}
        {@const used = storageUsed(device)}
        {@const receipt = receipts[device.id]}
        <article class="panel device-card stack">
          <div class="spread"><div class="device-icon">{#if device.transport === 'usb'}<Cable />{:else}<Wifi />{/if}</div><span class="badge" class:success={device.connected}>{$t(`devices.${device.connected ? 'connected' : 'disconnected'}`)}</span></div>
          <div><h2>{device.label}</h2><p class="small muted">{$t(`devices.${device.transport}`)}</p></div>
          <div class="row wrap"><span class="badge">{device.profile}</span>{#if !device.writable}<span class="badge warning">{$t('devices.readOnly')}</span>{/if}</div>
          {#if device.address}<p class="small muted address">{device.address}</p>{/if}
          {#if device.mountPath}<p class="small muted address">{device.mountPath}</p>{/if}
          {#if device.transport === 'calibreWireless'}<p class="field-hint">{$t('devices.calibreServerHint')}</p>{/if}
          {#if device.freeBytes !== null}<div class="device-storage"><div class="spread"><span>{$t('devices.freeSpace', { size: formatSize(device.freeBytes, $locale) })}</span>{#if used !== null}<span>{percent.format(used)}</span>{/if}</div>{#if used !== null}<progress value={used} max="1" aria-label={$t('book.size')}></progress>{/if}{#if device.totalBytes !== null}<p class="small muted total-size">{$t('common.size')}: {formatSize(device.totalBytes, $locale)}</p>{/if}</div>{/if}
          {#if device.transport !== 'calibreWireless' || device.bookCount > 0}<div class="stack device-counts"><p>{$t('devices.books', { count: device.bookCount })}</p><p class="small muted">{$t('devices.matched', { count: device.matchedBookCount })}</p></div>{/if}
          <p class="small muted">{$t('common.date')}: {formatDate(device.lastSeenAt, $locale)}</p>
          <div class="row wrap"><button class="button secondary" type="button" onclick={() => showInventory(device)}><BookOpen size={16} />{$t('devices.viewBooks')}</button><button class="button secondary" type="button" disabled={demo || !device.connected || scanning || pairing || busyDevice !== null} onclick={() => indexDevice(device)}><RefreshCw size={16} />{$t('devices.index')}</button><button class="button primary" type="button" disabled={demo || !onTransferSelected || !device.connected || !device.writable || !selectedBookIds.length || selectedBookIds.length > MAX_SELECTED_BOOKS || actionsBusy} onclick={() => prepareTransfer(device)}><Send size={16} />{$t('actions.transfer')}</button></div>
          {#if device.transport !== 'usb'}<button class="button ghost" type="button" disabled={demo || scanning || pairing || busyDevice !== null} onclick={() => disconnect(device)}><Unplug size={16} />{$t('devices.disconnect')}</button>{/if}
          {#if receipt}<div class="receipt stack" aria-live="polite"><p class="small"><strong>{$t(`jobs.${receipt.kind}`)}</strong> · {$t(`jobs.${receipt.status}`)}</p>{@render jobProgress(receipt)}{#if receipt.message}<p class="field-hint">{receipt.message}</p>{/if}{#if receipt.error}<p class="field-hint">{$t(`errors.${receipt.error.code}`)}</p>{/if}</div>{/if}
        </article>
      {/each}
    </div>
  {:else}
    <div class="empty-state"><Tablet /><h2>{$t('devices.emptyTitle')}</h2><p>{$t('devices.emptyDescription')}</p><button class="button primary" type="button" disabled={scanning} onclick={scan}><RefreshCw size={17} />{$t('devices.scan')}</button></div>
  {/if}
</section>

<dialog class="modal" bind:this={pairDialog} aria-labelledby="pair-title" oncancel={(event) => { event.preventDefault(); closePairing(); }}>
  <form onsubmit={(event) => { event.preventDefault(); void pair(); }}>
    <div class="panel-header"><h2 id="pair-title">{$t('devices.connectWireless')}</h2><button class="icon-button" type="button" disabled={pairing} onclick={closePairing} aria-label={$t('actions.close')}><X size={20} /></button></div>
    <div class="panel-body stack">
      {#if pairFailure}<div class="error-banner" role="alert"><div><strong>{$t(`errors.${pairFailure.code}`)}</strong>{#if pairFailure.detail}<p>{pairFailure.detail}</p>{/if}</div></div>{/if}
      <fieldset disabled={pairing || demo}><div class="stack">
        <div class="field"><label for="wireless-transport">{$t('devices.connectWireless')}</label><select id="wireless-transport" class="select" value={transport} onchange={(event) => changeTransport(event.currentTarget.value)}><option value="crosspoint">{$t('devices.crosspoint')}</option><option value="calibreWireless">{$t('devices.calibreWireless')}</option></select></div>
        {#if transport === 'calibreWireless'}<p class="field-hint">{$t('devices.calibreServerHint')}</p><p class="field-hint">{$t('devices.calibreTransferHint')}</p>{/if}
        <div class="field"><label for="wireless-address">{$t(transport === 'calibreWireless' ? 'devices.listenAddress' : 'devices.address')}</label><input id="wireless-address" class="input" bind:value={address} placeholder={transport === 'calibreWireless' ? '0.0.0.0:9090' : 'http://192.168.1.42'} spellcheck="false" required /></div>
        <div class="field"><label for="wireless-label">{$t('devices.label')}</label><input id="wireless-label" class="input" bind:value={label} required /></div>
        <div class="field"><label for="wireless-password">{$t('devices.password')} · {$t('common.optional')}</label><input id="wireless-password" class="input" type="password" bind:value={password} autocomplete="off" /></div>
      </div></fieldset>
    </div>
    <div class="panel-footer"><button class="button secondary" type="button" disabled={pairing} onclick={closePairing}>{$t('actions.cancel')}</button><button class="button primary" type="submit" disabled={demo || pairing || !address.trim() || !label.trim()}><Check size={16} />{$t('actions.add')}</button></div>
  </form>
</dialog>




<dialog class="modal inventory-modal" bind:this={inventoryDialog} aria-labelledby="inventory-title" oncancel={(event) => { event.preventDefault(); closeInventory(); }}>
  <div class="panel-header"><div><h2 id="inventory-title">{$t('devices.inventoryTitle')}</h2><p class="small muted">{inventoryDevice?.label ?? $t('devices.title')}</p></div><button class="icon-button" type="button" onclick={closeInventory} aria-label={$t('actions.close')}><X size={20} /></button></div>
  <div class="panel-body stack">
    {#if inventoryReceipt && !terminal(inventoryReceipt)}<div class="receipt stack" aria-live="polite"><p>{$t(`jobs.${inventoryReceipt.kind}`)} · {$t(`jobs.${inventoryReceipt.status}`)}</p>{@render jobProgress(inventoryReceipt)}</div>{/if}
    {#if inventory}
      <p class="small muted">{$t('common.date')}: {formatDate(inventory.updatedAt, $locale)}</p>
      <p class="small muted" role="status">{$t('devices.inventoryLoaded', { count: number.format(inventory.books.length), total: inventory.total === null ? $t('common.unknown') : number.format(inventory.total) })}</p>
      {#if inventory.truncated && inventoryDevice?.transport === 'usb'}<button class="button secondary" type="button" disabled={inventoryLoading} onclick={() => { void loadInventory(true); }}>{$t('devices.inventoryLoadMore')}</button>{:else if inventory.truncated}<p class="inventory-warning" role="status">{$t('devices.inventoryTruncated', { count: inventory.books.length, total: inventory.total ?? $t('common.unknown') })}</p>{/if}
      {#each inventory.warnings.filter((warning) => warning !== 'deviceIndexResultTruncated') as warning}<p class="field-hint">{warningText(warning)}</p>{/each}
      <div class="inventory-filters"><div class="field grow"><label for="inventory-search">{$t('devices.inventorySearch')}</label><input id="inventory-search" class="input" type="search" bind:value={inventorySearch} /></div><div class="field"><label for="inventory-match">{$t('devices.inLibrary')}</label><select id="inventory-match" class="select" bind:value={inventoryMatch}><option value="all">{$t('common.all')}</option><option value="known">{$t('devices.inLibrary')}</option><option value="unknown">{$t('devices.unknownBook')}</option></select></div></div>
      <p class="small muted" aria-live="polite">{$t('devices.inventoryFiltered', { count: inventoryRows.length })}</p>
      {#if inventoryRows.length}<div class="table-wrap inventory-table"><table class="data-table"><thead><tr><th scope="col">{$t('book.title')}</th><th scope="col">{$t('book.authors')}</th><th scope="col">{$t('devices.relativePath')}</th><th scope="col">{$t('book.format')}</th><th scope="col">{$t('book.size')}</th><th scope="col">{$t('devices.inLibrary')}</th></tr></thead><tbody>{#each inventoryRows as entry (entry.relativePath)}<tr class:on-device={entry.bookId !== null}><td class="table-title">{entry.title}</td><td>{entry.authors.join(', ') || $t('common.unknown')}</td><td class="inventory-path">{entry.relativePath}</td><td>{entry.format.toUpperCase()}</td><td>{formatSize(entry.sizeBytes, $locale)}</td><td><span class="badge" class:success={entry.bookId !== null}>{$t(entry.bookId !== null ? 'devices.inLibrary' : 'devices.unknownBook')}</span></td></tr>{/each}</tbody></table></div>{:else}<p class="muted">{$t('devices.inventoryNoResults')}</p>{/if}
    {:else}<p class="muted">{$t('devices.inventoryUnavailable')}</p>{/if}
  </div>
  <div class="panel-footer">{#if inventoryDevice}<button class="button secondary" type="button" disabled={demo || !inventoryDevice.connected || scanning || pairing || busyDevice !== null || (inventoryReceipt !== undefined && !terminal(inventoryReceipt))} onclick={() => { if (inventoryDevice) void indexDevice(inventoryDevice); }}><RefreshCw size={16} />{$t('devices.index')}</button>{/if}<button class="button primary" type="button" onclick={closeInventory}>{$t('actions.close')}</button></div>
</dialog>

<style>
  h2 { font-size: 18px; line-height: 1.4; overflow-wrap: anywhere; }
  fieldset { min-width: 0; padding: 0; margin: 0; border: 0; }
  .device-hint { margin-bottom: 24px; }
  .device-card { gap: 16px; }
  .device-card > div > .muted { margin-top: 5px; }
  .device-counts { gap: 6px; }
  .device-storage { margin: 5px 0; }
  .total-size { margin-top: 8px; }
  .address { overflow-wrap: anywhere; }
  .receipt { padding: 14px; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--surface-muted); }
  .modal { width: 560px; }
  .modal .panel-body { gap: 20px; }
  .inventory-modal { width: 1080px; max-width: calc(100vw - 48px); }
  .inventory-filters { display: flex; gap: 16px; align-items: end; flex-wrap: wrap; }
  .inventory-table { max-height: 45vh; overflow: auto; }
  .inventory-table th { position: sticky; top: 0; z-index: 1; }
  .inventory-table td { max-width: 240px; overflow-wrap: anywhere; }
  .inventory-path { min-width: 160px; font-family: var(--font-mono); font-size: 12px; }
  .inventory-warning { padding: 16px; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--warning-soft); }
  @media (prefers-reduced-motion: reduce) { .toolbar :global(.scanning) { animation: none; } }
  .toolbar :global(.scanning) { animation: rotate 1.4s linear infinite; }
  @keyframes rotate { to { transform: rotate(360deg); } }
</style>
