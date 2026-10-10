<script lang="ts">
  import { BookOpen, Check, NotebookText, Send, Settings, Trash2, X } from '@lucide/svelte';
  import { t } from '../i18n';
  import { MAX_SELECTED_BOOKS, type MetadataDisabledReason } from '../selection-capabilities';

  interface Props {
    selectedCount: number;
    metadataReady: boolean;
    metadataDisabledReason?: MetadataDisabledReason | null;
    transferReady?: boolean;
    busy?: boolean;
    demo?: boolean;
    onOpenAssistant?(): void;
    onVerifySelected?(): void;
    onTransferSelected?(): void;
    onRemoveSelected?(): void;
    onClearSelection?(): void;
    onOpenSettings?(): void;
  }

  let {
    selectedCount, metadataReady, metadataDisabledReason = null, transferReady = false,
    busy = false, demo = false, onOpenAssistant, onVerifySelected,
    onTransferSelected, onRemoveSelected, onClearSelection, onOpenSettings,
  }: Props = $props();
  const metadataHintId = $props.id();
  const count = $derived(Number.isInteger(selectedCount) && selectedCount >= 0 ? selectedCount : 0);
  const selectionDisabled = $derived(demo || busy || count === 0 || count > MAX_SELECTED_BOOKS);
  const selectionReason = $derived(demo ? $t('app.previewDescription') : busy ? $t('common.loading')
    : count > MAX_SELECTED_BOOKS ? $t('library.selectionLimit', { count: MAX_SELECTED_BOOKS })
    : $t('chat.noSelectedBooks'));
  const metadataReason = $derived($t(metadataDisabledReason === 'modelMissing' ? 'chat.noModel' : 'errors.providerNotConfigured'));
  const canVerify = $derived(!selectionDisabled && metadataReady && Boolean(onVerifySelected));
  const canTransfer = $derived(!selectionDisabled && transferReady && Boolean(onTransferSelected));
  const canRemove = $derived(!selectionDisabled && Boolean(onRemoveSelected));
  const canOpenAssistant = $derived(!selectionDisabled && Boolean(onOpenAssistant));
  const canClear = $derived(!demo && !busy && count > 0 && Boolean(onClearSelection));

  function invoke(handler: (() => void) | undefined, allowed: boolean): void {
    if (allowed) handler?.();
  }
</script>

<section class="selection-bar" aria-label={$t('library.selectedCount', { count })}>
  <div class="row"><Check size={17} aria-hidden="true" /><strong>{$t('library.selectedCount', { count })}</strong></div>
  <div class="selection-actions" role="group" aria-label={$t('library.selectedCount', { count })}>
    <button class="button secondary" type="button" disabled={!canOpenAssistant} title={selectionDisabled ? selectionReason : $t('sidebar.chat')} onclick={() => invoke(onOpenAssistant, canOpenAssistant)}><BookOpen size={16} aria-hidden="true" />{$t('sidebar.chat')}</button>
    <button class="button primary" type="button" disabled={!canVerify} aria-describedby={!metadataReady ? metadataHintId : undefined} title={selectionDisabled ? selectionReason : !metadataReady ? metadataReason : $t('library.verifySelected')} onclick={() => invoke(onVerifySelected, canVerify)}><NotebookText size={16} aria-hidden="true" />{$t('library.verifySelected')}</button>
    <button class="button secondary" type="button" disabled={!canTransfer} title={selectionDisabled ? selectionReason : !transferReady ? $t('devices.noWritableDevice') : $t('actions.transfer')} onclick={() => invoke(onTransferSelected, canTransfer)}><Send size={16} aria-hidden="true" />{$t('actions.transfer')}</button>
    <button class="button secondary remove-action" type="button" disabled={!canRemove} title={selectionDisabled ? selectionReason : $t('library.removeSelected')} onclick={() => invoke(onRemoveSelected, canRemove)}><Trash2 size={16} aria-hidden="true" />{$t('library.removeSelected')}</button>
    <button class="button ghost" type="button" disabled={!canClear} onclick={() => invoke(onClearSelection, canClear)}><X size={16} aria-hidden="true" />{$t('library.clearSelection')}</button>
  </div>
  {#if count === 0}<p class="field-hint notice">{$t('chat.noSelectedBooks')}</p>{/if}
  {#if count > MAX_SELECTED_BOOKS}<p class="field-hint notice" role="status">{$t('library.selectionLimit', { count: MAX_SELECTED_BOOKS })}</p>{/if}
  {#if !metadataReady}<div id={metadataHintId} class="notice row wrap"><p class="field-hint">{metadataReason}</p>{#if onOpenSettings}<button class="button ghost" type="button" disabled={demo || busy} onclick={() => invoke(onOpenSettings, !demo && !busy)}><Settings size={15} aria-hidden="true" />{$t('chat.configure')}</button>{/if}</div>{/if}
</section>

<style>
  .selection-actions { display: flex; flex-wrap: wrap; gap: 8px; }
  .notice { flex-basis: 100%; }
  .remove-action { color: var(--danger); }
</style>
