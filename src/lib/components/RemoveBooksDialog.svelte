<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte';
  import { RefreshCw, Trash2, X } from '@lucide/svelte';
  import { isPreview, normalizePublicError, PublicError, request } from '../api';
  import type { AppError, Book, RemoveBookSelection, RemoveBooksResult } from '../contracts';
  import { t } from '../i18n';
  import { MAX_SELECTED_BOOKS } from '../selection-capabilities';

  interface Props {
    bookIds: readonly string[];
    onClose(): void;
    onRemoved(result: RemoveBooksResult): void;
    onNotify?(message: string): void;
  }
  interface RemovalRequest { requestId: string; books: RemoveBookSelection[] }

  let { bookIds, onClose, onRemoved, onNotify }: Props = $props();
  const selectedBookIds = untrack(() => [...new Set(bookIds)]);
  const dialogId = $props.id();
  const demo = isPreview();
  const READ_CONCURRENCY = 4;
  const validSelection = selectedBookIds.length > 0 && selectedBookIds.length <= MAX_SELECTED_BOOKS
    && selectedBookIds.every((id) => typeof id === 'string' && id.trim().length > 0);
  let dialog: HTMLDialogElement;
  let previousFocus: HTMLElement | null = null;
  let disposed = false;
  let loadGeneration = 0;
  let requestId = crypto.randomUUID();
  let books = $state<Book[]>([]);
  let payload = $state<RemovalRequest | null>(null);
  let loading = $state(true);
  let busy = $state(false);
  let needsRefresh = $state(false);
  let retrySameRequest = $state(false);
  let failure = $state<AppError | null>(null);
  const canRemove = $derived(!demo && !loading && !busy && !needsRefresh && validSelection && payload !== null);

  onMount(() => {
    previousFocus = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialog.showModal();
    void loadBooks();
  });
  onDestroy(() => {
    disposed = true;
    loadGeneration += 1;
    dialog?.close();
    if (previousFocus?.isConnected) previousFocus.focus();
  });

  function close(): void {
    if (!busy) onClose();
  }

  async function loadBooks(): Promise<void> {
    if (disposed || busy || retrySameRequest) return;
    const generation = ++loadGeneration;
    loading = true;
    failure = null;
    try {
      if (!validSelection) throw new PublicError({ code: 'invalidInput', message: $t('errors.invalidInput'), detail: null, retryable: false });
      const loaded: Book[] = [];
      for (let start = 0; start < selectedBookIds.length; start += READ_CONCURRENCY) {
        const batch = selectedBookIds.slice(start, start + READ_CONCURRENCY);
        const results = await Promise.all(batch.map((id) => request('book_get', { id })));
        if (disposed || generation !== loadGeneration) return;
        if (results.some((book, index) => book.id !== batch[index]
          || !Number.isSafeInteger(book.revision) || book.revision < 0)) {
          throw new PublicError({ code: 'invalidInput', message: $t('errors.invalidInput'), detail: null, retryable: false });
        }
        loaded.push(...results);
      }
      if (disposed || generation !== loadGeneration) return;
      const selection = loaded.map((book) => ({ bookId: book.id, expectedRevision: book.revision }));
      if (payload !== null && JSON.stringify(payload.books) !== JSON.stringify(selection)) requestId = crypto.randomUUID();
      books = loaded;
      payload = { requestId, books: selection };
      needsRefresh = false;
    } catch (error: unknown) {
      if (!disposed && generation === loadGeneration) {
        failure = normalizePublicError(error);
        needsRefresh = true;
      }
    } finally {
      if (!disposed && generation === loadGeneration) loading = false;
    }
  }

  async function remove(): Promise<void> {
    if (!canRemove || !payload) return;
    const submitted = { requestId: payload.requestId, books: payload.books.map((book) => ({ ...book })) };
    busy = true;
    failure = null;
    let removed: RemoveBooksResult;
    try {
      removed = await request('books_remove', submitted);
    } catch (error: unknown) {
      if (!disposed) {
        failure = normalizePublicError(error);
        needsRefresh = failure.code === 'revisionConflict';
        retrySameRequest = !needsRefresh;
      }
      return;
    } finally {
      if (!disposed) busy = false;
    }
    if (disposed) return;
    onNotify?.($t('library.removeCompleted', { count: removed.removedBookIds.length }));
    onRemoved(removed);
    onClose();
  }
</script>

<dialog class="modal" bind:this={dialog} aria-labelledby={`${dialogId}-title`} aria-describedby={`${dialogId}-description`}
  aria-busy={loading || busy} oncancel={(event) => { event.preventDefault(); close(); }}>
  <form onsubmit={(event) => { event.preventDefault(); void remove(); }}>
    <div class="panel-header">
      <h2 id={`${dialogId}-title`}>{$t('library.removeTitle')}</h2>
      <button class="icon-button" type="button" disabled={busy} onclick={close} aria-label={$t('actions.close')}><X size={20} aria-hidden="true" /></button>
    </div>
    <div class="panel-body stack">
      <p id={`${dialogId}-description`}>{$t('library.removeDescription')}</p>
      <p>{$t('library.removeCount', { count: selectedBookIds.length })}</p>
      {#if demo}<p class="field-hint">{$t('app.previewDescription')}</p>{/if}
      {#if selectedBookIds.length > MAX_SELECTED_BOOKS}<p class="field-hint">{$t('library.selectionLimit', { count: MAX_SELECTED_BOOKS })}</p>{/if}
      {#if failure}<div class="error-banner" role="alert"><div><strong>{$t(`errors.${failure.code}`)}</strong>{#if failure.detail}<p>{failure.detail}</p>{/if}</div></div>{/if}
      {#if retrySameRequest}<p class="field-hint" role="status">{$t('library.removeRetry')}</p>{/if}
      {#if needsRefresh && payload}<p class="field-hint" role="status">{$t('library.removeConflict')}</p>{/if}
      {#if loading || busy}<p class="field-hint" role="status">{$t('common.loading')}</p>{/if}
      {#if books.length}<ul class="removal-books">{#each books as book (book.id)}<li><strong>{book.title}</strong><span class="small muted">{book.authors.join(', ')}</span></li>{/each}</ul>{/if}
      {#if needsRefresh && !retrySameRequest}<button class="button secondary" type="button" disabled={loading || busy || !validSelection} onclick={() => { void loadBooks(); }}><RefreshCw size={16} aria-hidden="true" />{$t('actions.refresh')}</button>{/if}
    </div>
    <div class="panel-footer">
      <button class="button secondary" type="button" disabled={busy} onclick={close}>{$t('actions.cancel')}</button>
      <button class="button primary" type="submit" disabled={!canRemove}><Trash2 size={16} aria-hidden="true" />{$t(retrySameRequest ? 'actions.retry' : 'library.removeSelected')}</button>
    </div>
  </form>
</dialog>

<style>
  .removal-books { display: grid; gap: 10px; max-height: 300px; overflow-y: auto; padding-inline-start: 22px; }
  .removal-books li { overflow-wrap: anywhere; }
  .removal-books span { display: block; }
</style>
