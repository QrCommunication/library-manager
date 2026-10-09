import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import type { UnlistenFn } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { AppError, ErrorCode, IpcCommand, IpcEvents, IpcParams, IpcResult } from './contracts';
import { isPreview, requestPreview, subscribePreview } from './preview';

export { isPreview };

const ERROR_CODES = new Set<ErrorCode>([
  'invalidInput', 'notFound', 'unsupportedFormat', 'encryptedBook', 'invalidEpub',
  'unsafePath', 'revisionConflict', 'profileInUse', 'deviceDisconnected', 'insufficientSpace',
  'networkUnavailable', 'providerNotConfigured', 'providerError', 'rateLimited',
  'secretStoreUnavailable', 'conversionFailed', 'operationConflict', 'cancelled', 'internal',
]);

const BOOK_EXTENSIONS = ['epub', 'mobi', 'azw3', 'fb2', 'txt', 'html', 'htm', 'pdf', 'cbz'];
const MAX_SERIALIZED_ERROR_LENGTH = 16_384;

export class PublicError extends Error implements AppError {
  readonly code: ErrorCode;
  readonly retryable: boolean;
  readonly detail: string | null;

  constructor(error: AppError) {
    super(error.message);
    this.name = 'PublicError';
    this.code = error.code;
    this.retryable = error.retryable;
    this.detail = error.detail;
  }
}

function isPublicError(value: unknown): value is AppError {
  if (typeof value !== 'object' || value === null) return false;
  const candidate = value as Record<string, unknown>;
  return typeof candidate.code === 'string'
    && ERROR_CODES.has(candidate.code as ErrorCode)
    && typeof candidate.message === 'string'
    && typeof candidate.retryable === 'boolean'
    && (candidate.detail === null || typeof candidate.detail === 'string');
}

export function normalizePublicError(error: unknown): PublicError {
  if (error instanceof PublicError) return error;
  if (isPublicError(error)) return new PublicError(error);
  if (typeof error === 'string' && error.length <= MAX_SERIALIZED_ERROR_LENGTH) {
    try {
      const decoded: unknown = JSON.parse(error);
      if (isPublicError(decoded)) return new PublicError(decoded);
    } catch {
      // Unstructured plugin errors are not safe public messages.
    }
  }
  return new PublicError({
    code: 'internal',
    message: 'The native operation could not be completed.',
    retryable: false,
    detail: null,
  });
}

export function isNative(): boolean {
  return typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;
}

function requireNative(): void {
  if (!isNative()) {
    throw new PublicError({
      code: 'internal',
      message: 'Native desktop integration is unavailable in this browser.',
      retryable: false,
      detail: null,
    });
  }
}

function requireFileDialog(): void {
  if (isPreview()) {
    throw new PublicError({
      code: 'operationConflict',
      message: 'File selection requires the desktop application and is unavailable in demo mode.',
      retryable: false,
      detail: null,
    });
  }
  requireNative();
}

export async function request<Command extends IpcCommand>(
  command: Command,
  params: IpcParams<Command>,
): Promise<IpcResult<Command>> {
  try {
    if (isPreview()) return await requestPreview(command, params);
    requireNative();
    return await invoke<IpcResult<Command>>(command, params);
  } catch (error: unknown) {
    throw normalizePublicError(error);
  }
}

export async function subscribe<Event extends keyof IpcEvents>(
  event: Event,
  handler: (payload: IpcEvents[Event]) => void,
): Promise<UnlistenFn> {
  try {
    if (isPreview()) return await subscribePreview(event, handler);
    requireNative();
    return await listen<IpcEvents[Event]>(event, ({ payload }) => handler(payload));
  } catch (error: unknown) {
    throw normalizePublicError(error);
  }
}

export async function chooseBooks(title?: string): Promise<string[]> {
  requireFileDialog();
  try {
    const paths = await open({
      title,
      multiple: true,
      directory: false,
      filters: [{ name: 'EPUB · MOBI · AZW3 · FB2 · TXT · HTML · PDF · CBZ', extensions: BOOK_EXTENSIONS }],
    });
    return paths ?? [];
  } catch (error: unknown) {
    throw normalizePublicError(error);
  }
}

export async function chooseDirectory(title?: string, defaultPath?: string): Promise<string | null> {
  requireFileDialog();
  try {
    return await open({ title, defaultPath, multiple: false, directory: true });
  } catch (error: unknown) {
    throw normalizePublicError(error);
  }
}

export async function openExternal(value: string): Promise<void> {
  const preview = isPreview();
  if (!preview) requireNative();
  let url: URL;
  try {
    if (/[\u0000-\u001f\u007f]/u.test(value)) throw new TypeError('Invalid URL');
    url = new URL(value);
    if (!['http:', 'https:'].includes(url.protocol) || url.username || url.password) {
      throw new TypeError('Unsupported URL');
    }
  } catch {
    throw new PublicError({
      code: 'invalidInput',
      message: 'External links must use HTTP or HTTPS without embedded credentials.',
      retryable: false,
      detail: null,
    });
  }
  try {
    if (preview) {
      window.open(url.href, '_blank', 'noopener,noreferrer');
      return;
    }
    await openUrl(url.href);
  } catch (error: unknown) {
    throw normalizePublicError(error);
  }
}
