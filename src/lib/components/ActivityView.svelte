<script lang="ts">
  import { onDestroy, tick, untrack } from 'svelte';
  import { Activity, ArrowDownUp, Check, CircleAlert, Clock, ExternalLink, History, RefreshCw, RotateCcw, X } from '@lucide/svelte';
  import { isPreview, normalizePublicError, openExternal, PublicError, request } from '../api';
  import type { AppError, Job, JobKind, JobStatus, Operation } from '../contracts';
  import { formatDate, formatProviderDiagnostic, formatSize, locale, t } from '../i18n';

  interface Props { refreshVersion: number; onNotify(message: string): void; onError(error: unknown): void }
  interface Metric { label: string; value: string }
  interface ResultSummary { metrics: Metric[]; fields: Metric[]; warnings: string[]; sources: string[] }
  type SortOrder = 'newest' | 'oldest' | 'progressHighest';
  let { refreshVersion, onNotify, onError }: Props = $props();
  const POLL_INTERVAL_MS = 2500;
  const statuses: JobStatus[] = ['queued', 'running', 'waitingForConfiguration', 'waitingForNetwork', 'completed', 'failed', 'cancelled'];
  const kinds: JobKind[] = ['import', 'enrich', 'optimize', 'convert', 'deviceIndex', 'transfer', 'chat'];
  const terminalStatuses: ReadonlySet<JobStatus> = new Set(['completed', 'failed', 'cancelled']);
  const metadataFields = ['title', 'authors', 'series', 'seriesIndex', 'genres', 'tags', 'language', 'description', 'isbn', 'publisher', 'published'] as const;
  const demo = isPreview();
  let jobs = $state<Job[]>([]);
  let operations = $state<Operation[]>([]);
  let statusFilter = $state<JobStatus | 'all'>('all');
  let kindFilter = $state<JobKind | 'all'>('all');
  let operationFilter = $state<Operation['status'] | 'all'>('all');
  let sortOrder = $state<SortOrder>('newest');
  let loading = $state(true);
  let refreshing = $state(false);
  let initialized = false;
  let disposed = false;
  let generation = 0;
  let pollTimer: ReturnType<typeof setTimeout> | null = null;
  let failure = $state<PublicError | null>(null);
  let cancelling = $state<string[]>([]);
  let actionErrors = $state<Record<string, PublicError>>({});
  let undoTarget = $state<Operation | null>(null);
  let undoFailure = $state<PublicError | null>(null);
  let undoing = $state(false);
  let undoDialog: HTMLDialogElement | undefined;
  const number = $derived(new Intl.NumberFormat($locale));
  const failureDetail = $derived(failure ? errorDetail(failure) : null);
  const undoFailureDetail = $derived(undoFailure ? errorDetail(undoFailure) : null);
  const percent = $derived(new Intl.NumberFormat($locale, { style: 'percent', maximumFractionDigits: 0 }));
  const pendingCount = $derived(jobs.filter((job) => !terminalStatuses.has(job.status)).length);
  const completedCount = $derived(jobs.filter((job) => job.status === 'completed').length);
  const failedCount = $derived(jobs.filter((job) => job.status === 'failed').length);
  const visibleJobs = $derived(jobs.filter((job) => (statusFilter === 'all' || job.status === statusFilter)
    && (kindFilter === 'all' || job.kind === kindFilter)).sort(compareJobs));
  const visibleOperations = $derived(operations.filter((operation) => operationFilter === 'all' || operation.status === operationFilter)
    .sort((a, b) => sortOrder === 'oldest' ? timestamp(a.createdAt) - timestamp(b.createdAt) : timestamp(b.createdAt) - timestamp(a.createdAt)));

  function errorDetail(error: AppError): string | null {
    return error.code === 'providerError' ? formatProviderDiagnostic(error.code, error.detail, $locale) : error.detail;
  }
  function timestamp(value: string): number { const parsed = Date.parse(value); return Number.isFinite(parsed) ? parsed : 0; }
  function progressValue(job: Job): number | undefined {
    return Number.isFinite(job.progress) && job.progress >= 0 && job.progress <= 1 ? job.progress : undefined;
  }
  function compareJobs(a: Job, b: Job): number {
    if (sortOrder === 'oldest') return timestamp(a.createdAt) - timestamp(b.createdAt);
    if (sortOrder === 'progressHighest') return (progressValue(b) ?? -1) - (progressValue(a) ?? -1) || timestamp(b.createdAt) - timestamp(a.createdAt);
    return timestamp(b.createdAt) - timestamp(a.createdAt);
  }
  function record(value: unknown): value is Record<string, unknown> { return typeof value === 'object' && value !== null && !Array.isArray(value); }
  function count(value: unknown): value is number { return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0; }
  function strings(value: unknown): value is string[] { return Array.isArray(value) && value.every((item: unknown) => typeof item === 'string'); }
  function localize(value: string): string {
    for (const key of [value, `jobs.${value}`, `reader.${value}`, `errors.${value}`]) {
      const translated = $t(key);
      if (translated !== key) return translated;
    }
    return value;
  }
  function safeSource(value: string): string | null {
    if (/\p{Cc}/u.test(value)) return null;
    try {
      const url = new URL(value);
      return ['https:', 'http:'].includes(url.protocol) && !url.username && !url.password ? url.href : null;
    } catch { return null; }
  }
  function resultSummary(job: Job): ResultSummary {
    const summary: ResultSummary = { metrics: [], fields: [], warnings: [], sources: [] };
    if (!record(job.result)) return summary;
    const result = job.result;
    summary.warnings = strings(result.warnings) ? [...new Set(result.warnings)] : [];
    function metric(key: string, value: unknown): void { if (count(value)) summary.metrics.push({ label: $t(key), value: number.format(value) }); }
    if (job.kind === 'import') {
      metric('jobs.imported', result.imported); metric('jobs.duplicates', result.duplicates);
    } else if (job.kind === 'optimize' || job.kind === 'convert') {
      if (count(result.beforeBytes) && count(result.afterBytes)) summary.metrics.push({ label: $t('common.size'), value: `${formatSize(result.beforeBytes, $locale)} → ${formatSize(result.afterBytes, $locale)}` });
      if (job.kind === 'optimize') {
        metric('jobs.imagesChanged', result.imagesChanged); metric('jobs.imagesRemoved', result.imagesRemoved); metric('jobs.fontCount', result.fontsRemoved);
        if (typeof result.textPreserved === 'boolean') summary.metrics.push({ label: $t('jobs.textPreserved'), value: $t(result.textPreserved ? 'common.yes' : 'common.no') });
      } else {
        if (typeof result.sourceFormat === 'string' && typeof result.targetFormat === 'string') summary.metrics.push({ label: $t('book.format'), value: `${result.sourceFormat.toUpperCase()} → ${result.targetFormat.toUpperCase()}` });
        metric('jobs.chapterCount', result.sectionCount); metric('jobs.imagesPreserved', result.imagesPreserved);
      }
    } else if (job.kind === 'transfer') {
      metric('jobs.copied', result.copied); metric('jobs.skipped', result.skipped); metric('jobs.failed', result.failed);
      if (Array.isArray(result.items)) for (const item of result.items) {
        if (record(item) && typeof item.errorCode === 'string') summary.warnings.push(item.errorCode);
      }
    } else if (job.kind === 'deviceIndex' && Array.isArray(result.books)) {
      if (result.books.every((book: unknown) => record(book) && typeof book.title === 'string' && typeof book.relativePath === 'string' && strings(book.authors) && count(book.sizeBytes))) {
        summary.metrics.push({ label: $t('library.title'), value: $t('jobs.bookCount', { count: result.books.length }) });
        for (const book of result.books) if (record(book) && strings(book.warnings)) summary.warnings.push(...book.warnings);
      }
    } else if (job.kind === 'enrich') {
      const proposal = record(result.proposal) ? result.proposal : result;
      if (typeof proposal.bookId !== 'string' || !record(proposal.patch) || typeof proposal.confidence !== 'number'
        || !Number.isFinite(proposal.confidence) || proposal.confidence < 0 || proposal.confidence > 1 || !Array.isArray(proposal.evidence)) return summary;
      summary.metrics.push({ label: $t('editor.reviewTitle'), value: $t('book.confidence', { progress: percent.format(proposal.confidence) }) });
      for (const key of metadataFields) {
        const value: unknown = proposal.patch[key];
        if (value === undefined) continue;
        const display = value === null ? $t('common.none') : typeof value === 'string' ? value.slice(0, 1200)
          : strings(value) ? value.join(' · ').slice(0, 1200) : typeof value === 'number' && Number.isFinite(value) ? number.format(value) : null;
        if (display !== null) summary.fields.push({ label: $t(`book.${key}`), value: display });
      }
      if (strings(proposal.warnings)) summary.warnings.push(...proposal.warnings);
      for (const evidence of proposal.evidence) {
        if (!record(evidence) || !strings(evidence.sourceUrls)) continue;
        for (const source of evidence.sourceUrls) {
          const safe = safeSource(source);
          if (safe && summary.sources.length < 32 && !summary.sources.includes(safe)) summary.sources.push(safe);
        }
      }
    }
    summary.warnings = [...new Set(summary.warnings)];
    return summary;
  }
  function report(error: unknown): PublicError {
    const normalized = normalizePublicError(error);
    onError(normalized);
    return normalized;
  }
  function stopPolling(): void { if (pollTimer !== null) clearTimeout(pollTimer); pollTimer = null; }
  function schedulePolling(): void {
    stopPolling();
    if (!disposed && jobs.some((job) => !terminalStatuses.has(job.status))) {
      pollTimer = setTimeout(() => { pollTimer = null; void loadActivity(true); }, POLL_INTERVAL_MS);
    }
  }
  async function loadActivity(background = false): Promise<void> {
    stopPolling();
    const current = ++generation;
    loading = !initialized;
    refreshing = !background;
    const [jobResponse, operationResponse] = await Promise.allSettled([request('jobs_list', undefined), request('operations_list', undefined)] as const);
    if (disposed || current !== generation) return;
    let loadFailure: PublicError | null = null;
    if (jobResponse.status === 'fulfilled') jobs = jobResponse.value;
    else loadFailure = normalizePublicError(jobResponse.reason);
    if (operationResponse.status === 'fulfilled') operations = operationResponse.value;
    else loadFailure ??= normalizePublicError(operationResponse.reason);
    if (loadFailure && (!failure || failure.code !== loadFailure.code || failure.message !== loadFailure.message)) onError(loadFailure);
    failure = loadFailure;
    initialized = true;
    loading = false;
    refreshing = false;
    schedulePolling();
  }
  async function cancelJob(job: Job): Promise<void> {
    if (demo || cancelling.includes(job.id) || terminalStatuses.has(job.status)) return;
    cancelling = [...cancelling, job.id];
    const errors = { ...actionErrors }; delete errors[job.id]; actionErrors = errors;
    try {
      const updated = await request('job_cancel', { id: job.id });
      if (disposed) return;
      jobs = jobs.map((entry) => entry.id === updated.id ? updated : entry);
      onNotify(`${$t(`jobs.${updated.kind}`)} · ${$t(`jobs.${updated.status}`)}`);
      await loadActivity();
    } catch (error) { if (!disposed) actionErrors = { ...actionErrors, [job.id]: report(error) }; }
    finally { if (!disposed) cancelling = cancelling.filter((id) => id !== job.id); }
  }
  async function confirmUndo(operation: Operation): Promise<void> {
    if (demo || !operation.reversible || operation.status !== 'applied' || undoing) return;
    undoTarget = operation; undoFailure = null;
    await tick();
    if (!disposed && undoDialog && !undoDialog.open) undoDialog.showModal();
  }
  function closeUndo(): void { if (!undoing) { undoDialog?.close(); undoTarget = null; undoFailure = null; } }
  async function undoOperation(): Promise<void> {
    const target = undoTarget;
    if (!target || undoing || demo) return;
    undoing = true; undoFailure = null;
    try {
      const updated = await request('operation_undo', { id: target.id });
      if (disposed) return;
      operations = operations.map((entry) => entry.id === updated.id ? updated : entry);
      if (updated.status !== 'reverted') throw new PublicError({ code: 'operationConflict', message: $t('jobs.undoConflict'), detail: null, retryable: false });
      onNotify($t('jobs.undoComplete', { description: localize(updated.description) }));
      undoing = false; closeUndo();
      await loadActivity();
    } catch (error) { if (!disposed) undoFailure = report(error); }
    finally { if (!disposed) undoing = false; }
  }
  async function openSource(url: string): Promise<void> { try { await openExternal(url); } catch (error) { report(error); } }
  function badge(status: JobStatus | Operation['status']): string {
    if (status === 'failed') return 'danger';
    if (status === 'completed' || status === 'applied') return 'success';
    if (status === 'waitingForConfiguration' || status === 'waitingForNetwork') return 'warning';
    return status === 'running' ? 'accent' : '';
  }
  $effect(() => { void refreshVersion; untrack(() => { void loadActivity(); }); });
  onDestroy(() => { disposed = true; generation += 1; stopPolling(); });
</script>

<section class="page activity-page" aria-labelledby="activity-title">
  <header class="page-header">
    <div><p class="eyebrow">Library Manager</p><h1 id="activity-title" class="page-title">{$t('jobs.title')}</h1></div>
    <button class="button secondary" onclick={() => loadActivity()} disabled={refreshing} aria-label={$t('actions.refresh')}><RefreshCw size={18} class={refreshing ? 'spin' : ''} />{$t('actions.refresh')}</button>
  </header>
  <div class="activity-stats" aria-label={$t('jobs.tasks')}>
    <div class="panel stat"><Activity size={21} /><span>{$t('jobs.tasks')}</span><strong>{number.format(jobs.length)}</strong></div>
    <div class="panel stat"><Clock size={21} /><span>{$t('jobs.active')}</span><strong>{number.format(pendingCount)}</strong></div>
    <div class="panel stat"><Check size={21} /><span>{$t('jobs.completed')}</span><strong>{number.format(completedCount)}</strong></div>
    <div class="panel stat"><CircleAlert size={21} /><span>{$t('jobs.failed')}</span><strong>{number.format(failedCount)}</strong></div>
  </div>
  {#if failure}<div class="error-banner" role="alert"><CircleAlert size={18} /><div>{$t(`errors.${failure.code}`)}{#if failureDetail}<p class="small">{failureDetail}</p>{/if}</div><button class="button ghost" onclick={() => loadActivity()}>{$t('actions.retry')}</button></div>{/if}
  <div class="toolbar wrap">
    <label class="field"><span>{$t('jobs.tasks')}</span><select class="select" bind:value={kindFilter}><option value="all">{$t('filters.all')}</option>{#each kinds as kind}<option value={kind}>{$t(`jobs.${kind}`)}</option>{/each}</select></label>
    <label class="field"><span>{$t('jobs.status')}</span><select class="select" bind:value={statusFilter}><option value="all">{$t('filters.all')}</option>{#each statuses as status}<option value={status}>{$t(`jobs.${status}`)}</option>{/each}</select></label>
    <label class="field"><span class="row"><ArrowDownUp size={14} />{$t('sort.label')}</span><select class="select" bind:value={sortOrder}><option value="newest">{$t('jobs.newest')}</option><option value="oldest">{$t('jobs.oldest')}</option><option value="progressHighest">{$t('jobs.progressHighest')}</option></select></label>
  </div>
  {#if loading}<div class="empty-state" role="status">{$t('common.loading')}</div>
  {:else if visibleJobs.length === 0}<div class="panel empty-state"><Activity size={32} /><p>{$t('jobs.empty')}</p>{#if statusFilter !== 'all' || kindFilter !== 'all'}<button class="button secondary" onclick={() => { statusFilter = 'all'; kindFilter = 'all'; }}>{$t('filters.clear')}</button>{/if}</div>
  {:else}
    <div class="stack activity-jobs">
      {#each visibleJobs as job (job.id)}
        {@const result = resultSummary(job)}
        {@const jobProgress = progressValue(job)}
        {@const actionError = actionErrors[job.id]}
        {@const jobErrorDetail = job.error ? errorDetail(job.error) : null}
        {@const actionErrorDetail = actionError ? errorDetail(actionError) : null}
        <article class="panel activity-job">
          <div class="row spread wrap"><div class="row"><span class="activity-icon"><Activity size={20} /></span><div><h2 class="job-title">{$t(`jobs.${job.kind}`)}</h2><p class="small muted">{formatDate(job.createdAt, $locale)} · {$t('jobs.bookCount', { count: job.bookIds.length })}</p></div></div><span class={`badge ${badge(job.status)}`}>{$t(`jobs.${job.status}`)}</span></div>
          <div class="row progress-row"><progress max="1" value={jobProgress} aria-label={$t('common.progress')}></progress><span class="small numeric">{jobProgress === undefined ? $t('common.unknown') : percent.format(jobProgress)}</span></div>
          {#if job.message}<p class="small muted job-message">{localize(job.message)}</p>{/if}
          {#if job.error}<div class="error-banner" role="alert"><CircleAlert size={17} /><div>{$t(`errors.${job.error.code}`)}{#if jobErrorDetail}<p class="small">{jobErrorDetail}</p>{/if}<span class="small muted">{job.error.code}</span></div></div>{/if}
          {#if actionError}<div class="error-banner" role="alert">{$t(`errors.${actionError.code}`)}{#if actionErrorDetail}<p class="small">{actionErrorDetail}</p>{/if}</div>{/if}
          {#if result.metrics.length || result.fields.length || result.warnings.length || result.sources.length}
            <details class="job-result"><summary>{$t('jobs.result')}{#if result.warnings.length}<span class="badge warning">{number.format(result.warnings.length)}</span>{/if}</summary>
              {#if result.metrics.length}<dl class="result-metrics">{#each result.metrics as metric}<div><dt>{metric.label}</dt><dd>{metric.value}</dd></div>{/each}</dl>{/if}
              {#if result.fields.length}<dl class="proposal-fields">{#each result.fields as field}<div><dt>{field.label}</dt><dd>{field.value}</dd></div>{/each}</dl>{/if}
              {#if result.warnings.length}<div class="warning-list"><h3 class="small">{$t('jobs.warnings')}</h3><ul>{#each result.warnings as warning}<li>{localize(warning)}</li>{/each}</ul></div>{/if}
              {#if result.sources.length}<div class="stack sources-list"><h3 class="small">{$t('book.sources')}</h3>{#each result.sources as source}<button class="button ghost source-button" onclick={() => openSource(source)}><ExternalLink size={15} /><span>{source}</span></button>{/each}</div>{/if}
            </details>
          {/if}
          {#if !terminalStatuses.has(job.status)}<div class="row job-actions"><button class="button ghost" disabled={demo || cancelling.includes(job.id)} onclick={() => cancelJob(job)}><X size={16} />{$t('actions.cancel')}</button></div>{/if}
        </article>
      {/each}
    </div>
  {/if}
  <section class="stack operations-section" aria-labelledby="operations-title">
    <div class="row spread wrap"><h2 id="operations-title" class="section-title row"><History size={21} />{$t('jobs.operations')}</h2><label class="field operation-filter"><span class="sr-only">{$t('jobs.operations')}</span><select class="select" bind:value={operationFilter}><option value="all">{$t('filters.all')}</option>{#each ['applied', 'reverted', 'failed'] as status}<option value={status}>{$t(`jobs.${status}`)}</option>{/each}</select></label></div>
    {#if !loading && visibleOperations.length === 0}<div class="panel empty-state"><History size={30} /><p>{$t('jobs.empty')}</p></div>{/if}
    {#each visibleOperations as operation (operation.id)}
      <article class="panel operation-row row spread wrap"><div class="grow"><h3 class="job-title">{localize(operation.description)}</h3><p class="small muted">{formatDate(operation.createdAt, $locale)}</p></div><span class={`badge ${badge(operation.status)}`}>{$t(`jobs.${operation.status}`)}</span>{#if operation.reversible && operation.status === 'applied'}<button class="button secondary" disabled={demo || undoing} onclick={() => confirmUndo(operation)}><RotateCcw size={16} />{$t('actions.undo')}</button>{/if}</article>
    {/each}
  </section>
</section>

<dialog class="modal" bind:this={undoDialog} aria-labelledby="undo-title" oncancel={(event) => { if (undoing) event.preventDefault(); }} onclose={() => { if (!undoing) { undoTarget = null; undoFailure = null; } }}>
  <form onsubmit={(event) => { event.preventDefault(); void undoOperation(); }} class="stack">
    <div class="row spread"><h2 id="undo-title" class="section-title">{$t('jobs.undoTitle')}</h2><button class="icon-button" type="button" aria-label={$t('actions.close')} onclick={closeUndo} disabled={undoing}><X size={20} /></button></div>
    <p>{$t('jobs.undoDescription')}</p>{#if undoTarget}<p class="panel undo-description">{localize(undoTarget.description)}</p>{/if}
    {#if undoFailure}<div class="error-banner" role="alert"><CircleAlert size={18} /><div>{$t(undoFailure.code === 'operationConflict' ? 'jobs.undoConflict' : `errors.${undoFailure.code}`)}{#if undoFailureDetail}<p class="small">{undoFailureDetail}</p>{/if}<span class="small muted">{undoFailure.code}</span></div></div>{/if}
    <div class="row justify-end"><button class="button secondary" type="button" disabled={undoing} onclick={closeUndo}>{$t('actions.cancel')}</button><button class="button primary" type="submit" disabled={demo || undoing || !undoTarget}><RotateCcw size={16} />{$t('actions.undo')}</button></div>
  </form>
</dialog>

<style>
  .activity-stats { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 14px; margin-bottom: 24px; }
  .stat { display: grid; grid-template-columns: 24px 1fr; align-items: center; gap: 8px 12px; padding: 20px; color: var(--text-muted); }
  .stat strong { grid-column: 2; font-size: 30px; line-height: 1.1; font-variant-numeric: tabular-nums; color: var(--text); }
  .activity-page > .toolbar { margin-block: 20px; align-items: end; }
  .activity-page > .toolbar .field { min-width: 180px; }
  .activity-jobs { gap: 14px; }
  .activity-job { padding: 21px 24px; }
  .activity-icon { display: grid; place-items: center; width: 44px; height: 44px; border-radius: 13px; color: var(--accent); background: var(--accent-soft); }
  .job-title { font-size: 15px; margin: 0; line-height: 1.5; }
  .progress-row { margin-top: 17px; gap: 14px; }
  progress { flex: 1; width: 100%; height: 7px; overflow: hidden; accent-color: var(--accent); }
  .numeric { min-width: 48px; text-align: right; font-variant-numeric: tabular-nums; }
  .job-message { margin-block: 12px 0; white-space: pre-wrap; overflow-wrap: anywhere; }
  .job-result { margin-top: 16px; border-top: 1px solid var(--border); }
  .job-result summary { display: flex; align-items: center; gap: 10px; min-height: 44px; cursor: pointer; font-size: 13px; font-weight: 650; }
  .job-result summary:focus-visible { outline: 3px solid var(--focus); outline-offset: 3px; border-radius: 6px; }
  .result-metrics { display: flex; flex-wrap: wrap; gap: 16px 28px; margin: 4px 0 16px; }
  .result-metrics dt, .proposal-fields dt { font-size: 12px; color: var(--text-muted); }
  .result-metrics dd { margin: 4px 0 0; font-weight: 650; font-variant-numeric: tabular-nums; }
  .proposal-fields { display: grid; gap: 10px; margin-block: 16px; }
  .proposal-fields > div { display: grid; grid-template-columns: 160px 1fr; gap: 14px; padding-bottom: 10px; border-bottom: 1px solid var(--border); }
  .proposal-fields dd { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font-size: 13px; }
  .warning-list { padding: 12px 16px; border-radius: 12px; background: var(--warning-soft); font-size: 13px; }
  .warning-list ul { padding-left: 18px; margin-bottom: 0; }
  .warning-list li { margin-block: 5px; overflow-wrap: anywhere; }
  .source-button { justify-content: start; text-align: left; min-height: 44px; font-size: 12px; overflow-wrap: anywhere; }
  .source-button :global(svg) { flex-shrink: 0; }
  .sources-list { margin-top: 14px; gap: 3px; }
  .job-actions { justify-content: end; margin-top: 12px; }
  .operations-section { margin-top: 34px; gap: 12px; }
  .operation-row { padding: 18px 22px; gap: 16px; }
  .operation-filter { min-width: 180px; }
  .undo-description { padding: 14px 16px; font-size: 14px; overflow-wrap: anywhere; }
  .modal > form { padding: 28px; }
  .justify-end { justify-content: end; }
  .spin { animation: rotation 1s linear infinite; }
  @keyframes rotation { to { transform: rotate(360deg); } }
  @media (prefers-reduced-motion: reduce) { .spin { animation: none; } }
  @media (max-width: 1200px) { .activity-stats { grid-template-columns: repeat(2, minmax(0, 1fr)); } .proposal-fields > div { grid-template-columns: 130px 1fr; } }
</style>
