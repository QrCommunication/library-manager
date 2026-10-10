<script lang="ts">
  import { onDestroy, onMount, untrack } from 'svelte';
  import { Send, X } from '@lucide/svelte';
  import { isPreview, normalizePublicError, request } from '../api';
  import type { AppError, Device, Job, OptimizationProfile } from '../contracts';
  import { t } from '../i18n';
  import { MAX_SELECTED_BOOKS } from '../selection-capabilities';

  interface Props {
    bookIds: readonly string[];
    devices: readonly Device[];
    profiles: readonly OptimizationProfile[];
    initialDeviceId?: string | null;
    onClose(): void;
    onQueued?(job: Job, deviceId: string): void;
    onNotify?(message: string): void;
  }

  let { bookIds, devices, profiles, initialDeviceId = null, onClose, onQueued, onNotify }: Props = $props();
  const dialogId = $props.id();
  const selectedBookIds = untrack(() => [...new Set(bookIds)]);
  const demo = isPreview();
  let dialog: HTMLDialogElement;
  let disposed = false;
  let busy = $state(false);
  let failure = $state<AppError | null>(null);
  let selectedDeviceId = $state(untrack(() => initialDeviceId ?? devices.find((device) => device.connected && device.writable)?.id ?? ''));
  let optimizeTransfer = $state(untrack(() => profiles.length > 0));
  let selectedProfileId = $state(untrack(() => profiles[0]?.id ?? ''));
  const selectedDevice = $derived(devices.find((device) => device.id === selectedDeviceId));
  const selectedProfile = $derived(profiles.find((profile) => profile.id === selectedProfileId));
  const validSelection = selectedBookIds.length > 0 && selectedBookIds.length <= MAX_SELECTED_BOOKS
    && selectedBookIds.every((id) => typeof id === 'string' && id.trim().length > 0);
  const canTransfer = $derived(!demo && !busy && validSelection && selectedDevice?.connected === true
    && selectedDevice.writable && (!optimizeTransfer || selectedProfile !== undefined));

  onMount(() => dialog.showModal());
  onDestroy(() => { disposed = true; });

  function close(): void {
    if (!busy) onClose();
  }

  function profileLabel(profile: OptimizationProfile): string {
    const key = `optimization.${profile.id}`;
    const translated = $t(key);
    return translated === key ? profile.name : translated;
  }

  async function transfer(): Promise<void> {
    if (!canTransfer || !selectedDevice) return;
    const id = selectedDevice.id;
    const profileId = optimizeTransfer ? selectedProfileId : null;
    busy = true;
    failure = null;
    let queued: Job;
    try {
      queued = await request('device_transfer', { id, bookIds: [...selectedBookIds], profileId });
    } catch (error: unknown) {
      if (!disposed) failure = normalizePublicError(error);
      return;
    } finally {
      if (!disposed) busy = false;
    }
    if (disposed) return;
    onQueued?.(queued, id);
    onNotify?.(`${$t(`jobs.${queued.kind}`)} · ${$t(`jobs.${queued.status}`)}`);
    onClose();
  }
</script>

<dialog class="modal" bind:this={dialog} aria-labelledby={`${dialogId}-title`} aria-busy={busy}
  oncancel={(event) => { event.preventDefault(); close(); }}>
  <form onsubmit={(event) => { event.preventDefault(); void transfer(); }}>
    <div class="panel-header">
      <div><h2 id={`${dialogId}-title`}>{$t('devices.transferTitle')}</h2><p class="small muted">{selectedDevice?.label ?? $t('devices.title')}</p></div>
      <button class="icon-button" type="button" disabled={busy} onclick={close} aria-label={$t('actions.close')}><X size={20} aria-hidden="true" /></button>
    </div>
    <div class="panel-body stack">
      {#if failure}<div class="error-banner" role="alert"><div><strong>{$t(`errors.${failure.code}`)}</strong>{#if failure.detail}<p>{failure.detail}</p>{/if}</div></div>{/if}
      <p>{$t('library.selectedCount', { count: selectedBookIds.length })}</p>
      {#if selectedBookIds.length === 0}<p class="field-hint">{$t('chat.noSelectedBooks')}</p>{/if}
      {#if selectedBookIds.length > MAX_SELECTED_BOOKS}<p class="field-hint" role="status">{$t('library.selectionLimit', { count: MAX_SELECTED_BOOKS })}</p>{/if}
      <fieldset disabled={busy || demo}>
        <div class="stack">
          <div class="field">
            <label for={`${dialogId}-device`}>{$t('filters.device')}</label>
            <select id={`${dialogId}-device`} class="select" bind:value={selectedDeviceId}>
              <option value="" disabled>{$t('devices.noWritableDevice')}</option>
              {#each devices as device (device.id)}<option value={device.id} disabled={!device.connected || !device.writable}>{device.label}</option>{/each}
            </select>
          </div>
          {#if !selectedDevice}<p class="field-hint">{$t('devices.noWritableDevice')}</p>
          {:else if !selectedDevice.connected}<p class="field-hint">{$t('errors.deviceDisconnected')}</p>
          {:else if !selectedDevice.writable}<p class="field-hint">{$t('devices.readOnly')}</p>{/if}
          <label class="checkbox-field"><input type="checkbox" bind:checked={optimizeTransfer} />{$t('devices.optimizeBeforeTransfer')}</label>
          {#if optimizeTransfer}
            <div class="field"><label for={`${dialogId}-profile`}>{$t('settings.optimizationProfile')}</label>
              <select id={`${dialogId}-profile`} class="select" bind:value={selectedProfileId} disabled={!profiles.length}>
                {#each profiles as profile (profile.id)}<option value={profile.id}>{profileLabel(profile)}</option>{/each}
              </select>
            </div>
            {#if selectedProfile?.removeImages}<p class="field-hint">{$t('optimization.removeImagesWarning')}</p>{/if}
          {/if}
        </div>
      </fieldset>
      {#if selectedDevice?.transport === 'calibreWireless'}<p class="field-hint">{$t('devices.calibreTransferHint')}</p>{/if}
      <p class="field-hint">{$t('devices.folderStructure')}</p>
      <p class="field-hint">{$t('devices.preserveProgress')}</p>
      <p class="field-hint">{$t('editor.preserveOriginal')}</p>
      {#if busy}<p class="field-hint" role="status">{$t('common.loading')}</p>{/if}
    </div>
    <div class="panel-footer">
      <button class="button secondary" type="button" disabled={busy} onclick={close}>{$t('actions.cancel')}</button>
      <button class="button primary" type="submit" disabled={!canTransfer}><Send size={16} aria-hidden="true" />{$t('actions.transfer')}</button>
    </div>
  </form>
</dialog>
