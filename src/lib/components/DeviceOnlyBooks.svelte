<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { Download, HardDrive, RefreshCw } from '@lucide/svelte';
  import { isPreview, normalizePublicError, request } from '../api';
  import type { AppError, Device, DeviceInventoryBook, Job } from '../contracts';
  import { formatSize, locale, t } from '../i18n';

  interface Props {
    devices: Device[];
    jobs: Job[];
    refreshVersion: number;
    search?: string;
    onNotify(message: string): void;
    onError(error: unknown): void;
  }
  interface InventoryState {
    items: DeviceInventoryBook[];
    total: number;
    loading: boolean;
    error: AppError | null;
  }
  let { devices, jobs, refreshVersion, search = '', onNotify, onError }: Props = $props();
  let pages = $state<Record<string, InventoryState>>({});
  let receipts = $state<Record<string, Job>>({});
  let submitting = $state<Record<string, boolean>>({});
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  const queued = new Set<string>();
  const generations = new Map<string, number>();
  const notified = new Set<string>();
  const PAGE_SIZE = 50;
  const REFRESH_INTERVAL_MS = 1000;
  const demo = isPreview();
  const usbDevices = $derived(devices.filter((device) => device.transport === 'usb'
    && (device.connected || pages[device.id] !== undefined)));
  const imports = $derived(Object.fromEntries(Object.entries(receipts)
    .map(([id, receipt]) => [id, jobs.find((job) => job.id === receipt.id) ?? receipt])));

  function resultObject(job: Job | undefined): Record<string, unknown> {
    return job?.result !== null && typeof job?.result === 'object' && !Array.isArray(job.result)
      ? job.result as Record<string, unknown> : {};
  }
  function terminal(job: Job): boolean {
    return ['completed', 'failed', 'cancelled'].includes(job.status);
  }
  function indexing(id: string): boolean {
    return jobs.some((job) => job.kind === 'deviceIndex' && !terminal(job)
      && resultObject(job).deviceId === id);
  }
  function importJob(id: string): Job | undefined {
    const active = jobs.find((job) => job.kind === 'import' && !terminal(job)
      && resultObject(job).deviceId === id);
    return active ?? imports[id];
  }
  function busy(id: string): boolean {
    const job = importJob(id);
    return submitting[id] === true || (job !== undefined && !terminal(job));
  }
  function counts(job: Job): { imported: number; duplicates: number; failed: number } {
    const result = resultObject(job);
    const number = (value: unknown): number => typeof value === 'number' && Number.isFinite(value)
      && value >= 0 ? value : 0;
    const errors = result.errorsByCode;
    return {
      imported: number(result.imported),
      duplicates: number(result.duplicates),
      failed: typeof errors === 'object' && errors !== null
        ? Object.values(errors).reduce<number>((total, value) => total + number(value), 0) : 0,
    };
  }
  function rows(id: string): DeviceInventoryBook[] {
    const value = search.trim().toLocaleLowerCase($locale);
    return (pages[id]?.items ?? []).filter((book) => !value
      || [book.title, book.relativePath, ...book.authors, book.format]
        .join(' ').toLocaleLowerCase($locale).includes(value));
  }
  function schedule(ids: string[]): void {
    if (disposed) return;
    for (const id of ids) queued.add(id);
    if (timer !== undefined) return;
    timer = setTimeout(() => {
      timer = undefined;
      const ids = [...queued];
      queued.clear();
      for (const id of ids) void load(id, false);
      const active = devices.filter((device) => device.connected && device.transport === 'usb'
        && indexing(device.id)).map((device) => device.id);
      if (active.length > 0) schedule(active);
    }, REFRESH_INTERVAL_MS);
  }
  async function load(id: string, more: boolean): Promise<void> {
    if (disposed || !devices.some((device) => device.id === id && device.connected)) return;
    const previous = pages[id];
    if (previous?.loading) {
      if (!more) schedule([id]);
      return;
    }
    const generation = (generations.get(id) ?? 0) + 1;
    generations.set(id, generation);
    pages[id] = { items: previous?.items ?? [], total: previous?.total ?? 0, loading: true, error: null };
    try {
      let offset = more ? previous?.items.length ?? 0 : 0;
      const target = more ? offset + PAGE_SIZE : Math.max(previous?.items.length ?? 0, PAGE_SIZE);
      const items = more ? [...previous?.items ?? []] : [];
      let total = 0;
      do {
        const page = await request('device_inventory', { id, offset, limit: PAGE_SIZE, unknownOnly: true });
        if (disposed || generations.get(id) !== generation
          || !devices.some((device) => device.id === id && device.connected)) return;
        items.push(...page.items);
        total = page.total;
        offset += page.items.length;
        if (page.items.length < PAGE_SIZE) break;
      } while (offset < Math.min(target, total));
      pages[id] = { items: [...new Map(items.map((book) => [book.relativePath, book])).values()], total, loading: false, error: null };
    } catch (error) {
      if (disposed || generations.get(id) !== generation) return;
      const failure = normalizePublicError(error);
      pages[id] = { items: previous?.items ?? [], total: previous?.total ?? 0, loading: false, error: failure };
      if (previous?.error?.message !== failure.message) onError(failure);
    } finally {
      if (!disposed && generations.get(id) === generation && pages[id]?.loading) {
        pages[id] = { ...pages[id]!, loading: false };
      }
    }
  }
  async function importBooks(device: Device, path: string | null): Promise<void> {
    if (demo || !device.connected || busy(device.id)) return;
    submitting[device.id] = true;
    try {
      const job = await request('device_import', {
        id: device.id, relativePaths: path === null ? null : [path],
      });
      if (!disposed) receipts[device.id] = job;
    } catch (error) {
      if (!disposed) onError(error);
    } finally {
      if (!disposed) submitting[device.id] = false;
    }
  }
  $effect(() => {
    const ids = devices.filter((device) => device.connected && device.transport === 'usb')
      .map((device) => device.id);
    void refreshVersion;
    untrack(() => schedule(ids));
  });
  $effect(() => {
    const indexChanges = jobs.filter((job) => job.kind === 'deviceIndex')
      .map((job) => `${job.id}:${job.updatedAt}`).join('|');
    void indexChanges;
    untrack(() => schedule(devices.filter((device) => device.connected && device.transport === 'usb')
      .map((device) => device.id)));
  });
  $effect(() => {
    for (const [id, job] of Object.entries(imports)) {
      if (terminal(job) && !notified.has(job.id)) {
        notified.add(job.id);
        untrack(() => {
          onNotify($t('deviceLibrary.importResult', counts(job)));
          if (job.error) onError(job.error);
          schedule([id]);
        });
      }
    }
  });
  onDestroy(() => {
    disposed = true;
    clearTimeout(timer);
    queued.clear();
    generations.clear();
  });
</script>

{#if usbDevices.length > 0}
  <section class="device-library" aria-label={$t('deviceLibrary.title')}>
    <div class="section-heading">
      <HardDrive size={19} aria-hidden="true" />
      <h2>{$t('deviceLibrary.title')}</h2>
    </div>
    <p class="muted description">{$t('deviceLibrary.description')}</p>
    {#each usbDevices as device (device.id)}
      {@const page = pages[device.id]}
      {@const job = importJob(device.id)}
      <div class="device-group">
        <div class="device-heading">
          <div>
            <h3>{device.label}</h3>
            <p class="muted" aria-live="polite">{$t('deviceLibrary.loaded', { loaded: page?.items.length ?? 0, total: page?.total ?? 0 })}</p>
          </div>
          <button class="button secondary" disabled={demo || !device.connected || busy(device.id) || !page?.total}
            onclick={() => void importBooks(device, null)}>
            <Download size={16} aria-hidden="true" /> {$t('deviceLibrary.importAll')}
          </button>
        </div>
        {#if !device.connected}<p class="muted">{$t('deviceLibrary.disconnected')}</p>{/if}
        {#if indexing(device.id)}<p class="muted" role="status">{$t('deviceLibrary.identifying')}</p>{/if}
        {#if busy(device.id)}
          <div class="import-progress" role="status">
            <span>{$t('deviceLibrary.importing')}</span>
            {#if job}<progress value={job.progress} max="1" aria-label={$t('deviceLibrary.importing')}></progress>{/if}
          </div>
        {/if}
        {#if job && terminal(job)}<p role="status">{$t('deviceLibrary.importResult', counts(job))}</p>{/if}
        {#if page?.error}
          <div class="failure"><p role="alert">{page.error.message}</p>
            <button class="button secondary" disabled={!device.connected || page.loading}
              onclick={() => void load(device.id, false)}><RefreshCw size={16} aria-hidden="true" /> {$t('actions.retry')}</button>
          </div>
        {/if}
        <ul class="device-books">
          {#each rows(device.id) as book (book.relativePath)}
            <li>
              <div class="book-details">
                <div class="title-row"><strong>{book.title}</strong><span class="device-badge">{$t('deviceLibrary.onlyOnDevice')}</span></div>
                {#if book.authors.length > 0}<p class="authors">{book.authors.join(', ')}</p>{/if}
                <p class="muted resource">{book.format.toUpperCase()} · {formatSize(book.sizeBytes)} · {book.relativePath}</p>
              </div>
              <button class="button secondary" disabled={demo || !device.connected || busy(device.id)}
                onclick={() => void importBooks(device, book.relativePath)} aria-label={`${$t('deviceLibrary.importOne')} : ${book.title}`}>
                <Download size={16} aria-hidden="true" /> {$t('deviceLibrary.importOne')}
              </button>
            </li>
          {/each}
        </ul>
        {#if !page || page.loading}<p class="muted" role="status">{$t('common.loading')}</p>
        {:else if page.items.length === 0 && !page.error}<p class="muted">{$t('deviceLibrary.empty')}</p>{/if}
        {#if page && page.items.length < page.total}
          <button class="button secondary" disabled={!device.connected || page.loading}
            onclick={() => void load(device.id, true)}>{$t('deviceLibrary.loadMore')}</button>
        {/if}
      </div>
    {/each}
  </section>
{/if}

<style>
  .device-library { margin-top: 36px; padding: 24px; border: 1px solid var(--border); border-radius: 16px; background: var(--surface); }
  .section-heading, .device-heading, .title-row, .import-progress { display: flex; align-items: center; gap: 12px; }
  .section-heading { color: var(--accent-ink); }
  h2 { margin: 0; font-size: 18px; } h3 { margin: 0 0 5px; font-size: 15px; }
  p { margin: 6px 0; } .muted { color: var(--text-muted); font-size: 12px; }
  .description { margin-top: 12px; line-height: 1.6; }
  .device-group { margin-top: 22px; } .device-heading { justify-content: space-between; flex-wrap: wrap; }
  .device-books { list-style: none; padding: 0; margin: 14px 0; }
  li { display: flex; align-items: center; justify-content: space-between; gap: 20px; padding: 14px 0; border-top: 1px solid var(--border); }
  .book-details { min-width: 0; } .title-row { flex-wrap: wrap; gap: 8px; } strong { font-size: 13px; overflow-wrap: anywhere; }
  .device-badge { border: 1px solid var(--accent-ink); border-radius: 999px; padding: 3px 8px; color: var(--accent-ink); font-size: 10px; font-weight: 650; }
  .authors { font-size: 12px; } .resource { overflow-wrap: anywhere; line-height: 1.5; }
  li button { flex-shrink: 0; } .import-progress { margin-top: 12px; font-size: 12px; } progress { max-width: 180px; }
  .failure { margin: 12px 0; }
  @media (max-width: 680px) { .device-library { padding: 16px; } li { flex-wrap: wrap; gap: 10px; } }
</style>
