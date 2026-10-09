<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { BookOpen, Check, ChevronLeft, ChevronRight, ExternalLink, List, RefreshCw, Settings2, X } from '@lucide/svelte';
  import { normalizePublicError, openExternal, PublicError, request } from '../api';
  import type { AppError, Book, ReaderManifest, ReaderSection, ReaderTocItem } from '../contracts';
  import { locale, t } from '../i18n';

  interface Props { book: Book; onClose(): void; onNotify(message: string): void; onError(error: unknown): void }
  interface TocRow { key: string; label: string; sectionIndex: number; fragment: string | null; depth: number }
  interface ReaderPreferences { fontSize: number; lineHeight: number; width: number; theme: 'light' | 'dark' | 'sepia' }
  interface ExternalSource { url: string; label: string }
  interface SavedPosition { sectionIndex: number; fragment: string | null }
  let { book, onClose, onNotify, onError }: Props = $props();
  let disposed = false;
  let bookGeneration = 0;
  let sectionGeneration = 0;
  let saveQueue: Promise<void> = Promise.resolve();
  let lastSavedSnapshot = '';
  let manifest = $state<ReaderManifest | null>(null);
  let section = $state<ReaderSection | null>(null);
  let position = $state(0);
  let fragment = $state<string | null>(null);
  let progress = $state(0);
  let loading = $state(true);
  let closing = $state(false);
  let pendingSaves = $state(0);
  let failure = $state<AppError | null>(null);
  let preferencesVisible = $state(false);
  let sourcesVisible = $state(false);
  let preferences = $state<ReaderPreferences>({ fontSize: 19, lineHeight: 1.8, width: 64, theme: 'light' });
  let frameHtml = $state<string | null>(null);
  let externalSources = $state<ExternalSource[]>([]);
  const toc = $derived(manifest ? flattenToc(manifest.toc) : []);
  const currentInfo = $derived(manifest?.sections[position]);
  const sectionCount = $derived(manifest?.sections.length ?? 0);
  const percent = $derived(new Intl.NumberFormat($locale, { style: 'percent', maximumFractionDigits: 0 }));

  function report(error: unknown): void { failure = normalizePublicError(error); onError(failure); }
  function invalidInput(): never {
    throw new PublicError({ code: 'invalidInput', message: $t('errors.invalidInput'), detail: null, retryable: false });
  }
  function validFragment(value: string): boolean {
    return value.length > 0 && new TextEncoder().encode(value).length <= 256 && !/[\s<>"'#]|\p{Cc}/u.test(value);
  }
  function parseLocation(value: string | null): SavedPosition | null {
    if (value === null || new TextEncoder().encode(value).length > 1024) return null;
    const match = /^section:(\d+)(?:#(.+))?$/u.exec(value);
    if (!match?.[1]) return null;
    const sectionIndex = Number(match[1]);
    const bookmark = match[2] ?? null;
    return Number.isSafeInteger(sectionIndex) && (bookmark === null || validFragment(bookmark)) ? { sectionIndex, fragment: bookmark } : null;
  }
  function flattenToc(items: ReaderTocItem[], depth = 0, prefix = ''): TocRow[] {
    if (depth > 64) return [];
    return items.flatMap((item, index) => {
      const key = `${prefix}${index}`;
      return [{ key, label: item.label, sectionIndex: item.sectionIndex, fragment: item.fragment, depth }, ...flattenToc(item.children, depth + 1, `${key}.`)];
    });
  }
  function clampNumber(value: number, min: number, max: number, fallback: number): number {
    return Number.isFinite(value) ? Math.min(max, Math.max(min, value)) : fallback;
  }
  function prepareHtml(html: string, choice: ReaderPreferences): string {
    const palettes = { light: ['#fbfaf6', '#293331', '#086f66'], dark: ['#172628', '#dce5dd', '#75c6ba'], sepia: ['#f1e7d2', '#4e422f', '#08655c'] } as const;
    const palette = choice.theme === 'dark' ? palettes.dark : choice.theme === 'sepia' ? palettes.sepia : palettes.light;
    const fontSize = clampNumber(choice.fontSize, 12, 34, 19);
    const lineHeight = clampNumber(choice.lineHeight, 1.3, 2.5, 1.8);
    const width = clampNumber(choice.width, 38, 90, 64);
    const css = `html{background:${palette[0]};color:${palette[1]};color-scheme:${choice.theme === 'dark' ? 'dark' : 'light'}}body{background:${palette[0]}!important;color:${palette[1]}!important;font-size:${fontSize}px!important;line-height:${lineHeight}!important;max-width:${width}ch!important;margin:0 auto!important;padding:42px 32px!important;overflow-wrap:anywhere}a{color:${palette[2]}!important}img{max-width:100%;height:auto}pre{white-space:pre-wrap}table{max-width:100%;overflow-wrap:anywhere}:focus-visible{outline:3px solid ${palette[2]};outline-offset:3px}@media(prefers-reduced-motion:reduce){*{scroll-behavior:auto!important}}`;
    const policy = "default-src 'none'; script-src 'none'; style-src 'unsafe-inline'; img-src data:; font-src data:; connect-src 'none'; object-src 'none'; frame-src 'none'; base-uri 'none'; form-action 'none'";
    const additions = `<meta http-equiv="Content-Security-Policy" content="${policy}"><style>${css}</style>`;
    const prepared = /<\/head\s*>/iu.test(html) ? html.replace(/<\/head\s*>/iu, `${additions}</head>`) : `<!doctype html><html><head><meta charset="utf-8">${additions}</head><body>${html}</body></html>`;
    const document = new DOMParser().parseFromString(prepared, 'text/html');
    // A srcdoc document inherits the parent's base URL. Explicit srcdoc URLs
    // keep footnotes in this opaque frame instead of navigating the application.
    for (const anchor of document.querySelectorAll('a[href]')) {
      const href = anchor.getAttribute('href');
      if (!href?.startsWith('#')) continue;
      try {
        const decoded = decodeURIComponent(href.slice(1));
        const normalized = decoded.normalize('NFC');
        const target = document.getElementById(normalized) ? normalized : decoded;
        if (validFragment(target)) anchor.setAttribute('href', `about:srcdoc#${encodeURIComponent(target)}`);
        else anchor.removeAttribute('href');
      } catch { anchor.removeAttribute('href'); }
    }
    return `<!doctype html>${document.documentElement.outerHTML}`;
  }
  function collectSources(html: string): ExternalSource[] {
    const document = new DOMParser().parseFromString(html, 'text/html');
    const seen = new Set<string>();
    const sources: ExternalSource[] = [];
    for (const anchor of document.querySelectorAll('a[href]')) {
      const value = anchor.getAttribute('href');
      if (!value || /[\u0000-\u001f\u007f]/u.test(value)) continue;
      try {
        const url = new URL(value);
        if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password || seen.has(url.href)) continue;
        seen.add(url.href);
        sources.push({ url: url.href, label: anchor.textContent?.trim() || url.hostname });
      } catch { /* Local footnote anchors stay inside the isolated document. */ }
    }
    return sources;
  }
  $effect(() => {
    const document = section;
    const choice = { ...preferences };
    untrack(() => {
      frameHtml = document ? prepareHtml(document.html, choice) : null;
    });
  });
  async function openBook(id: string): Promise<void> {
    const generation = ++bookGeneration;
    sectionGeneration += 1;
    loading = true;
    failure = null;
    manifest = null;
    section = null;
    externalSources = [];
    lastSavedSnapshot = '';
    progress = 0;
    position = 0;
    fragment = null;
    try {
      const opened = await request('reader_open', { id });
      if (disposed || generation !== bookGeneration || book.id !== id) return;
      if (opened.bookId !== id) invalidInput();
      manifest = opened;
      progress = clampNumber(opened.savedProgress, 0, 1, 0);
      const saved = parseLocation(opened.savedLocation);
      const savedPosition = saved ? opened.sections.findIndex((info) => info.index === saved.sectionIndex) : -1;
      if (opened.sections.length) await navigate(savedPosition >= 0 ? savedPosition : 0, savedPosition >= 0 ? saved?.fragment ?? null : null, false);
    } catch (error) { if (!disposed && generation === bookGeneration) report(error); }
    finally { if (!disposed && generation === bookGeneration) loading = false; }
  }
  $effect(() => { const id = book.id; untrack(() => { void openBook(id); }); });

  async function navigate(nextPosition: number, bookmark: string | null = null, save = true): Promise<void> {
    const opened = manifest;
    const target = opened?.sections[nextPosition];
    if (!opened || !target || !Number.isSafeInteger(nextPosition) || nextPosition < 0) return;
    const id = opened.bookId;
    const generation = ++sectionGeneration;
    loading = true;
    failure = null;
    try {
      const result = await request('reader_section', { id, sectionIndex: target.index });
      if (disposed || generation !== sectionGeneration || manifest?.bookId !== id || book.id !== id) return;
      if (result.bookId !== id || result.sectionIndex !== target.index) invalidInput();
      position = nextPosition;
      fragment = bookmark && validFragment(bookmark) ? bookmark : null;
      section = result;
      externalSources = collectSources(result.html);
      sourcesVisible = false;
      progress = Math.max(progress, nextPosition / opened.sections.length);
      if (save) void persistProgress();
    } catch (error) { if (!disposed && generation === sectionGeneration) report(error); }
    finally { if (!disposed && generation === sectionGeneration) loading = false; }
  }
  function persistProgress(finished = false, force = false): Promise<void> {
    const opened = manifest;
    const current = section;
    if (!opened || !current || current.bookId !== opened.bookId) return Promise.resolve();
    const id = current.bookId;
    const value = finished ? 1 : clampNumber(progress, 0, 1, 0);
    const location = `section:${current.sectionIndex}${fragment ? `#${fragment}` : ''}`;
    const snapshot = `${id}|${location}|${value}`;
    if (!force && snapshot === lastSavedSnapshot) return saveQueue;
    pendingSaves += 1;
    saveQueue = saveQueue.then(async () => {
      try {
        await request('reader_save_progress', { id, location, progress: value });
        if (!disposed && manifest?.bookId === id) {
          lastSavedSnapshot = snapshot;
          if (finished) { progress = 1; onNotify($t('reader.progress', { progress: percent.format(1) })); }
        }
      } catch (error) { if (!disposed && manifest?.bookId === id) report(error); }
      finally { if (!disposed) pendingSaves = Math.max(0, pendingSaves - 1); }
    });
    return saveQueue;
  }
  async function closeReader(): Promise<void> {
    if (closing) return;
    closing = true;
    failure = null;
    try {
      await persistProgress(false, true);
      if (!disposed && failure === null) onClose();
    } finally { if (!disposed) closing = false; }
  }
  async function finishBook(): Promise<void> {
    if (loading || pendingSaves || !section) return;
    failure = null;
    await persistProgress(true, true);
  }
  function handleKey(event: KeyboardEvent): void {
    if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey || event.shiftKey || loading) return;
    if (event.target instanceof HTMLElement && (event.target.closest('input, select, textarea') || event.target.isContentEditable)) return;
    if (event.key === 'ArrowLeft' && position > 0) { event.preventDefault(); void navigate(position - 1); }
    if (event.key === 'ArrowRight' && position + 1 < sectionCount) { event.preventDefault(); void navigate(position + 1); }
  }
  async function visitSource(url: string): Promise<void> {
    try { await openExternal(url); } catch (error) { if (!disposed) report(error); }
  }
  function warningLabel(code: string): string {
    const key = `reader.${code}`;
    const translated = $t(key);
    return translated === key ? code : translated;
  }
  onDestroy(() => {
    disposed = true;
    bookGeneration += 1;
    sectionGeneration += 1;
  });
</script>

<svelte:window onkeydown={handleKey} />
<section class="page reader-page">
  <div class="page-header"><div><p class="eyebrow">{$t('reader.title')}</p><h1 class="page-title reader-title">{manifest?.title ?? book.title}</h1><p class="page-subtitle">{manifest?.authors.join(', ') ?? book.authors.join(', ')}</p></div><button class="button secondary" type="button" onclick={closeReader} disabled={pendingSaves > 0 || closing}><X size={17} />{$t('actions.close')}</button></div>
  {#if failure}<div class="error-banner" role="alert"><div class="grow"><strong>{$t(`errors.${failure.code}`)}</strong>{#if failure.detail}<p>{failure.detail}</p>{/if}</div><button class="button secondary" type="button" onclick={() => section ? navigate(position, fragment, false) : openBook(book.id)} disabled={loading}><RefreshCw size={16} />{$t('actions.retry')}</button></div>{/if}
  <div class="panel reader-layout">
    <aside class="reader-sidebar" aria-label={$t('reader.contents')}>
      <div class="reader-book">{#if book.coverPath}<img src={book.coverPath} alt="" class="reader-cover" />{:else}<BookOpen size={28} />{/if}<strong>{manifest?.title ?? book.title}</strong></div>
      <h2 class="eyebrow">{$t('reader.contents')}</h2>
      {#if toc.length}{#each toc as entry (entry.key)}<button class="chapter-button" class:active={currentInfo?.index === entry.sectionIndex && fragment === entry.fragment} style:padding-inline-start={`${12 + Math.min(entry.depth, 6) * 12}px`} type="button" disabled={loading} aria-current={currentInfo?.index === entry.sectionIndex && fragment === entry.fragment ? 'location' : undefined} onclick={() => { const next = manifest?.sections.findIndex((info) => info.index === entry.sectionIndex) ?? -1; void navigate(next, entry.fragment); }}>{entry.label}</button>{/each}{:else}{#each manifest?.sections ?? [] as info, index (info.index)}<button class="chapter-button" class:active={position === index} type="button" disabled={loading} onclick={() => navigate(index)}>{info.title || $t('reader.chapter', { count: index + 1 })}</button>{/each}{/if}
    </aside>
    <div class="reader-main" data-reader-theme={preferences.theme}>
      <div class="reader-toolbar"><div class="row"><button class="icon-button" type="button" disabled={loading || position <= 0 || !sectionCount} onclick={() => navigate(position - 1)} aria-label={$t('reader.previous')}><ChevronLeft size={20} /></button><button class="icon-button" type="button" disabled={loading || position + 1 >= sectionCount} onclick={() => navigate(position + 1)} aria-label={$t('reader.next')}><ChevronRight size={20} /></button><span class="small chapter-counter">{sectionCount ? position + 1 : 0} / {sectionCount}</span></div><button class="button ghost" type="button" aria-expanded={preferencesVisible} aria-controls="reader-preferences" onclick={() => { preferencesVisible = !preferencesVisible; }}><Settings2 size={16} />{$t('reader.appearance')}</button></div>
      {#if preferencesVisible}<div id="reader-preferences" class="reader-preferences"><div class="field"><label for="reader-font-size">{$t('reader.fontSize')} · {preferences.fontSize}</label><input id="reader-font-size" type="range" min="12" max="34" step="1" bind:value={preferences.fontSize} /></div><div class="field"><label for="reader-line-height">{$t('reader.lineHeight')}</label><input id="reader-line-height" type="range" min="1.3" max="2.5" step="0.1" bind:value={preferences.lineHeight} /></div><div class="field"><label for="reader-width">{$t('reader.width')}</label><input id="reader-width" type="range" min="38" max="90" step="1" bind:value={preferences.width} /></div><div class="field"><label for="reader-theme">{$t('settings.theme')}</label><select id="reader-theme" class="select" bind:value={preferences.theme}><option value="light">{$t('reader.light')}</option><option value="dark">{$t('reader.dark')}</option><option value="sepia">{$t('reader.sepia')}</option></select></div></div>{/if}
      {#if section?.warnings.length}<details class="reader-warnings"><summary>{$t('common.warning')} · {section.warnings.length}</summary><ul>{#each section.warnings as warning, index (index)}<li>{warningLabel(warning)}</li>{/each}</ul></details>{/if}
      {#if fragment}<div class="reader-warnings" role="status">{warningLabel('readerFragmentPosition')}</div>{/if}
      {#if loading}<div class="reader-loading" role="status">{$t('reader.loading')}</div>{/if}
      {#if frameHtml}<iframe class="reader-frame" srcdoc={frameHtml} sandbox="" title={currentInfo?.title || $t('reader.title')} referrerpolicy="no-referrer" aria-busy={loading}></iframe>{:else if !loading}<div class="empty-state reader-empty"><BookOpen /><p>{$t('reader.noSections')}</p></div>{/if}
      {#if externalSources.length}<div class="reader-sources"><button class="button ghost" type="button" aria-expanded={sourcesVisible} aria-controls="reader-external-sources" onclick={() => { sourcesVisible = !sourcesVisible; }}><List size={16} />{$t('reader.externalLink')} · {externalSources.length}</button>{#if sourcesVisible}<div id="reader-external-sources" class="row wrap">{#each externalSources as source (source.url)}<button class="source-chip" type="button" onclick={() => visitSource(source.url)} title={source.url}><ExternalLink size={14} />{source.label}</button>{/each}</div>{/if}</div>{/if}
      <div class="reader-footer"><span>{$t('reader.progress', { progress: percent.format(progress) })}</span><div class="row"><button class="button ghost" type="button" disabled={!section || loading || pendingSaves > 0} onclick={() => persistProgress(false, true)}>{$t('actions.save')}</button><button class="button secondary" type="button" disabled={!section || loading || pendingSaves > 0 || progress === 1} onclick={finishBook}><Check size={16} />{$t('actions.save')} · {$t('book.finished')}</button></div></div>
    </div>
  </div>
</section>

<style>
  .reader-title { font-size: 27px; }
  .reader-layout { min-height: 640px; height: calc(100dvh - 245px); }
  .reader-sidebar h2 { margin: 16px 10px 10px; }
  .reader-book { display: flex; align-items: flex-start; gap: 12px; margin: 0 8px 12px; color: var(--text); font-size: 12px; }
  .reader-cover { flex-shrink: 0; width: 42px; height: 63px; border-radius: 3px; object-fit: cover; box-shadow: var(--shadow-sm); }
  .reader-preferences { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 16px; padding: 16px 20px; border-bottom: 1px solid var(--border); background: var(--surface); color: var(--text); }
  .reader-preferences input { min-height: 44px; width: 100%; accent-color: var(--accent); }
  .reader-warnings { padding: 12px 20px; background: var(--warning-soft); color: var(--warning); font-size: 12px; }
  .reader-warnings summary { min-height: 44px; cursor: pointer; }
  .reader-warnings ul { padding-inline-start: 18px; line-height: 1.8; }
  .reader-loading { padding: 12px 20px; background: var(--surface); color: var(--text-muted); font-size: 12px; }
  .reader-empty { min-height: 0; flex: 1; }
  .reader-sources { padding: 6px 16px; border-top: 1px solid var(--border); background: var(--surface); color: var(--text); }
  .reader-sources > div { padding: 8px 4px; max-height: 160px; overflow-y: auto; }
  .reader-footer { flex-wrap: wrap; }
  @media (max-width: 1150px) { .reader-preferences { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
</style>
