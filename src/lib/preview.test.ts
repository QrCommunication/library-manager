import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { defaultQuery } from './contracts';
import type { BookQuery } from './contracts';

let preview: typeof import('./preview');
beforeEach(async () => {
  vi.resetModules();
  vi.stubGlobal('window', { location: { search: '?demo=1' } });
  vi.stubGlobal('navigator', { language: 'fr-FR', languages: ['fr-FR'] });
  preview = await import('./preview');
});
afterEach(() => vi.unstubAllGlobals());

async function list(patch: Partial<BookQuery> = {}) {
  return preview.requestPreview('library_list', { query: { ...defaultQuery(), limit: 200, ...patch } });
}

describe('explicit preview library', () => {
  it('requires an explicit browser demo and never activates inside the desktop application', async () => {
    expect(preview.isPreview()).toBe(true);
    vi.stubGlobal('window', { location: { search: '' } });
    await expect(list()).rejects.toMatchObject({ code: 'internal' });
    vi.stubGlobal('window', { location: { search: '?demo=1' }, __TAURI_INTERNALS__: {} });
    expect(preview.isPreview()).toBe(false);
    await expect(list()).rejects.toMatchObject({ code: 'internal' });
  });

  it('combines selected authors with OR and independent dimensions with AND without duplicate books', async () => {
    const union = await list({ authors: ['Maëlle Veyron', 'Solène Arven', 'Maëlle Veyron'] });
    expect(union.total).toBe(13);
    expect(new Set(union.items.map((book) => book.id)).size).toBe(13);
    const series = await list({ authors: ['Maëlle Veyron', 'Solène Arven'], series: ['Les Atlas silencieux'] });
    expect(series.total).toBe(4);
    expect(series.items.every((book) => book.authors.includes('Maëlle Veyron'))).toBe(true);
    expect((await list({ genres: ['fantasy'], languages: ['en'] })).total).toBe(0);
    expect((await list({ authors: ['Solène Arven'], formats: ['mobi'] })).items.map((book) => book.title)).toEqual(['La dernière saison des algues']);
    expect((await list({ search: 'MAELLE atlas' })).total).toBe(5);
  });

  it('counts facets from actual fictional books and keeps returned snapshots independent', async () => {
    const facets = await preview.requestPreview('library_facets', undefined);
    expect(facets.authors.find((facet) => facet.value === 'Maëlle Veyron')?.count).toBe(6);
    expect(facets.series.find((facet) => facet.value === 'Les Atlas silencieux')?.count).toBe(4);
    expect(facets.languages.map((facet) => facet.value).sort()).toEqual(['en', 'fr']);
    const snapshot = await list();
    const book = snapshot.items[0];
    expect(book).toBeDefined();
    if (!book) throw new Error('Expected fictional book');
    const original = book.title;
    book.title = 'Mutated consumer snapshot';
    expect((await preview.requestPreview('book_get', { id: book.id })).title).toBe(original);
    facets.authors.length = 0;
    expect((await preview.requestPreview('library_facets', undefined)).authors.length).toBeGreaterThan(0);
  });

  it('highlights and filters only books on currently connected preview devices', async () => {
    const devices = await preview.requestPreview('devices_scan', undefined);
    const connected = devices.filter((device) => device.connected).map((device) => device.id);
    const page = await list();
    expect(page.items.flatMap((book) => book.onDeviceIds).every((id) => connected.includes(id))).toBe(true);
    expect((await list({ deviceId: 'preview-xteink', onDevice: true })).total).toBe(12);
    expect((await list({ deviceId: 'preview-xteink', onDevice: false })).total).toBe(24);
    expect((await list({ onDevice: true })).total).toBe(12);
    expect((await list({ deviceId: 'preview-wireless', onDevice: true })).total).toBe(0);
  });

  it('preserves fractional series indices and valid zero offsets while sorting numerically', async () => {
    const series = await list({ series: ['Les Atlas silencieux'], sort: 'series', descending: false, offset: 0 });
    expect(series.items.map((book) => book.seriesIndex)).toEqual([0.5, 1, 2, 3]);
    expect(series.items[0]?.title).toBe('Avant que les îles dérivent');
    const first = await list({ sort: 'series', descending: false, offset: 0, limit: 2 });
    const next = await list({ sort: 'series', descending: false, offset: 2, limit: 2 });
    expect(first.total).toBe(36);
    expect(first.items).toHaveLength(2);
    expect(next.items).toHaveLength(2);
    expect(first.items.some((book) => next.items.some((other) => other.id === book.id))).toBe(false);
    expect((await list({ offset: 200 })).items).toEqual([]);
    for (const patch of [{ limit: 0 }, { limit: 201 }, { offset: -1 }, { offset: 0.5 }, { limit: Number.NaN }]) {
      await expect(list(patch)).rejects.toMatchObject({ code: 'invalidInput' });
    }
  });

  it('updates reading revisions, emits changes and restores the saved location without duplicate listeners', async () => {
    const id = 'preview-book-01';
    const before = await preview.requestPreview('book_get', { id });
    const changed = vi.fn();
    const progress = vi.fn();
    const unsubscribe = await preview.subscribePreview('library:changed', changed);
    const stopProgress = await preview.subscribePreview('reader:progress', progress);
    await preview.requestPreview('reader_save_progress', { id, location: 'chapter:1:paragraph:2', progress: 0.75 });
    const after = await preview.requestPreview('book_get', { id });
    expect(after.revision).toBe(before.revision + 1);
    expect(after.readStatus).toBe('reading');
    expect(after.readingProgress).toBe(0.75);
    const manifest = await preview.requestPreview('reader_open', { id });
    expect(manifest.savedLocation).toBe('chapter:1:paragraph:2');
    expect(manifest.savedProgress).toBe(0.75);
    expect(changed).toHaveBeenCalledTimes(1);
    expect(progress).toHaveBeenCalledTimes(1);
    unsubscribe(); stopProgress();
    await preview.requestPreview('reader_save_progress', { id, location: 'end', progress: 1 });
    expect((await preview.requestPreview('book_get', { id })).readStatus).toBe('finished');
    expect(changed).toHaveBeenCalledTimes(1);
    await expect(preview.requestPreview('reader_save_progress', { id, location: 'bad', progress: 2 })).rejects.toMatchObject({ code: 'invalidInput' });
    expect((await preview.requestPreview('book_get', { id })).readingProgress).toBe(1);
  });

  it('never fabricates successful real operations and reports truthful conversion and provider capabilities', async () => {
    const id = 'preview-book-01';
    for (const command of ['import_books', 'book_update', 'book_enrich', 'book_optimize', 'book_convert', 'device_index', 'device_transfer', 'provider_models', 'provider_set_secret', 'chat_send', 'job_cancel', 'operation_undo'] as const) {
      // No handler for these operations reads arguments: every action must reject before doing work.
      await expect(preview.requestPreview(command, {} as never)).rejects.toMatchObject({ code: 'operationConflict', detail: command });
    }
    expect(await preview.requestPreview('jobs_list', undefined)).toEqual([]);
    expect(await preview.requestPreview('operations_list', undefined)).toEqual([]);
    expect((await preview.requestPreview('providers_list', undefined)).find((provider) => provider.id === 'codex')?.connectionMode).toBe('api');
    expect(await preview.requestPreview('conversion_capabilities', undefined)).toEqual({ inputs: [], outputs: [], warnings: ['previewConversionUnavailable'] });
    expect((await preview.requestPreview('book_get', { id })).revision).toBe(1);
    await expect(preview.requestPreview('reader_section', { id, sectionIndex: -1 })).rejects.toMatchObject({ code: 'invalidInput' });
    await expect(preview.requestPreview('reader_open', { id: 'preview-book-08' })).rejects.toMatchObject({ code: 'unsupportedFormat' });
    await expect(preview.requestPreview('book_get', { id: 'unknown' })).rejects.toMatchObject({ code: 'notFound' });
  });
});
