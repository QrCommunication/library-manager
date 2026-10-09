import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const native = vi.hoisted(() => ({ invoke: vi.fn(), listen: vi.fn(), open: vi.fn(), openUrl: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: native.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: native.listen }));
vi.mock('@tauri-apps/plugin-dialog', () => ({ open: native.open }));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: native.openUrl }));

import { PublicError, chooseBooks, chooseDirectory, isNative, normalizePublicError, openExternal, request, subscribe } from './api';

function browser(demo = false, desktop = false): ReturnType<typeof vi.fn> {
  const open = vi.fn();
  vi.stubGlobal('window', {
    location: { search: demo ? '?demo=1' : '' }, open,
    ...(desktop ? { __TAURI_INTERNALS__: {} } : {}),
  });
  return open;
}

beforeEach(() => { vi.resetAllMocks(); browser(); });
afterEach(() => { vi.unstubAllGlobals(); });

describe('native boundary', () => {
  it('does not call any desktop plugin when native integration is absent', async () => {
    expect(isNative()).toBe(false);
    await expect(request('app_bootstrap', undefined)).rejects.toMatchObject({ code: 'internal' });
    await expect(subscribe('library:changed', vi.fn())).rejects.toMatchObject({ code: 'internal' });
    await expect(chooseBooks()).rejects.toMatchObject({ code: 'internal' });
    await expect(chooseDirectory()).rejects.toMatchObject({ code: 'internal' });
    for (const mock of Object.values(native)) expect(mock).not.toHaveBeenCalled();
  });

  it('forwards typed native commands and actual event payloads', async () => {
    browser(false, true);
    const result = { id: 'real-book', title: 'Book from the native library' };
    native.invoke.mockResolvedValue(result);
    expect(await request('book_get', { id: 'real-book' })).toBe(result);
    expect(native.invoke).toHaveBeenCalledWith('book_get', { id: 'real-book' });
    const listener = vi.fn();
    const unlisten = vi.fn();
    native.listen.mockImplementation(async (_event, callback) => {
      callback({ payload: { bookIds: ['real-book'], reason: 'metadata' } });
      return unlisten;
    });
    expect(await subscribe('library:changed', listener)).toBe(unlisten);
    expect(listener).toHaveBeenCalledWith({ bookIds: ['real-book'], reason: 'metadata' });
    expect(native.listen).toHaveBeenCalledWith('library:changed', expect.any(Function));
  });

  it('does not expose secrets from unstructured native or plugin failures', async () => {
    browser(false, true);
    const sensitive = 'Bearer sk-test-private-fixture-only /private/user/profile SQLite trace';
    for (const error of [new Error(sensitive), sensitive, { code: 'unknown', message: sensitive, detail: sensitive }, JSON.stringify({ trace: sensitive })]) {
      native.invoke.mockRejectedValueOnce(error);
      try {
        await request('app_bootstrap', undefined);
        expect.fail('The native operation must reject');
      } catch (publicError) {
        expect(publicError).toBeInstanceOf(PublicError);
        expect(publicError).toMatchObject({ code: 'internal', retryable: false, detail: null });
        expect(JSON.stringify(publicError)).not.toContain('sk-test-private');
        expect((publicError as Error).message).not.toContain('/private/user');
      }
    }
    native.open.mockRejectedValueOnce(new Error(sensitive));
    await expect(chooseBooks()).rejects.toMatchObject({ detail: null, code: 'internal' });
    expect(normalizePublicError('x'.repeat(16_385)).detail).toBeNull();
  });

  it('preserves structured public errors instead of hiding useful retry and conflict information', () => {
    const value = { code: 'revisionConflict', message: 'Reload the changed book', retryable: true, detail: null };
    const error = normalizePublicError(JSON.stringify(value));
    expect(error).toMatchObject(value);
    expect(normalizePublicError(error)).toBe(error);
    expect(normalizePublicError({ ...value, retryable: 'yes' })).toMatchObject({ code: 'internal', detail: null });
  });

  it('keeps the profile lock error distinct while rejecting unrecognized codes', async () => {
    const value = { code: 'profileInUse', message: 'This profile is already open', retryable: false, detail: null };
    expect(normalizePublicError(value)).toMatchObject(value);
    expect(normalizePublicError(JSON.stringify(value))).toMatchObject(value);
    browser(false, true);
    native.invoke.mockRejectedValueOnce(value);
    await expect(request('app_bootstrap', undefined)).rejects.toMatchObject(value);
    for (const code of ['profile_in_use', 'profileInUseUnexpected', 'unrecognized']) {
      expect(normalizePublicError({ ...value, code })).toMatchObject({ code: 'internal', detail: null });
    }
  });

  it('refuses real file actions in demo mode while allowing the explicit fictional preview', async () => {
    browser(true);
    await expect(chooseBooks()).rejects.toMatchObject({ code: 'operationConflict' });
    await expect(chooseDirectory()).rejects.toMatchObject({ code: 'operationConflict' });
    await expect(request('import_books', { paths: ['/fictional/book.epub'] })).rejects.toMatchObject({ code: 'operationConflict' });
    const bootstrap = await request('app_bootstrap', undefined);
    expect(bootstrap.pendingJobs).toEqual([]);
    expect(bootstrap.devices.every((device) => device.id.startsWith('preview-'))).toBe(true);
    for (const mock of Object.values(native)) expect(mock).not.toHaveBeenCalled();
  });
});

describe('external links and file selection', () => {
  it('rejects dangerous URL schemes, embedded credentials and control characters before opening anything', async () => {
    browser(false, true);
    for (const url of ['javascript:alert(1)', 'file:///private/book.epub', 'data:text/html,test', 'https://user:password@example.org/', 'https://example.org/\nsecret', '/relative/path', 'wss://example.org/']) {
      await expect(openExternal(url)).rejects.toMatchObject({ code: 'invalidInput' });
    }
    expect(native.openUrl).not.toHaveBeenCalled();
    await openExternal('https://example.org/books?q=été');
    expect(native.openUrl).toHaveBeenCalledWith('https://example.org/books?q=%C3%A9t%C3%A9');
  });

  it('opens preview links with an isolated opener and never calls the native plugin', async () => {
    const open = browser(true);
    await openExternal('https://example.org/books');
    expect(open).toHaveBeenCalledWith('https://example.org/books', '_blank', 'noopener,noreferrer');
    expect(native.openUrl).not.toHaveBeenCalled();
  });

  it('preserves cancellation and constrains native file selection to supported formats', async () => {
    browser(false, true);
    native.open.mockResolvedValueOnce(null);
    expect(await chooseBooks('Import')).toEqual([]);
    expect(native.open).toHaveBeenCalledWith(expect.objectContaining({ multiple: true, directory: false, filters: [expect.objectContaining({ extensions: expect.arrayContaining(['epub', 'mobi', 'azw3', 'fb2', 'pdf', 'cbz']) })] }));
    native.open.mockResolvedValueOnce('/chosen/folder');
    expect(await chooseDirectory('Folder', '/default')).toBe('/chosen/folder');
    expect(native.open).toHaveBeenLastCalledWith({ title: 'Folder', defaultPath: '/default', multiple: false, directory: true });
  });
});
