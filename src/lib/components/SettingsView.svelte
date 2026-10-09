<script lang="ts">
  import { onDestroy, untrack } from 'svelte';
  import { Check, Globe, KeyRound, Palette, RefreshCw, Settings2, ShieldCheck, Sparkles, X } from '@lucide/svelte';
  import { isPreview, normalizePublicError, PublicError, request } from '../api';
  import type { AppError, ModelCatalog, OptimizationProfile, Provider, ProviderId, Settings } from '../contracts';
  import { availableLanguages, formatDate, locale, t } from '../i18n';
  import { version as appVersion } from '../../../package.json';

  interface Props {
    settings: Settings;
    onSettingsChange(settings: Settings): void;
    onNotify(message: string): void;
    onError(error: unknown): void;
  }
  let { settings, onSettingsChange, onNotify, onError }: Props = $props();
  const demo = isPreview();
  const providerIds: ProviderId[] = ['zai', 'kimi', 'minimax', 'codex', 'claude', 'mistral'];
  const deviceProfiles = ['generic', 'xteink', 'kindle', 'kobo'];
  let disposed = false;
  let supportGeneration = 0;
  let providerGeneration = 0;
  const catalogRequests: Partial<Record<ProviderId, number>> = {};
  let baseline = $state<Settings | null>(null);
  let draft = $state<Settings | null>(null);
  let providers = $state<Provider[]>([]);
  let profiles = $state<OptimizationProfile[]>([]);
  let catalogs = $state<Partial<Record<ProviderId, ModelCatalog>>>({});
  let catalogLoading = $state<ProviderId[]>([]);
  let saving = $state(false);
  let secretBusy = $state<ProviderId | null>(null);
  let secret = $state('');
  let persistSecret = $state(false);
  let failure = $state<AppError | null>(null);
  const dirty = $derived(draft !== null && baseline !== null && JSON.stringify(draft) !== JSON.stringify(baseline));
  const selectedProvider = $derived(providers.find((provider) => provider.id === draft?.providerId));
  const currentCatalog = $derived(draft?.providerId ? catalogs[draft.providerId] : undefined);
  const selectedModel = $derived(currentCatalog?.models.find((model) => model.id === draft?.modelId));
  const combinationUnchanged = $derived(draft !== null && baseline !== null && draft.providerId === baseline.providerId && draft.modelId === baseline.modelId);
  const validCombination = $derived(draft !== null && ((draft.providerId === null && draft.modelId === null) || combinationUnchanged
    || Boolean(draft.modelId && selectedProvider?.configured && selectedProvider.status === 'ready' && selectedModel)));
  const blocked = $derived(saving || secretBusy !== null);
  const number = $derived(new Intl.NumberFormat($locale));
  const percent = $derived(new Intl.NumberFormat($locale, { style: 'percent', maximumFractionDigits: 0 }));

  function copySettings(value: Settings): Settings { return { ...value }; }
  function report(error: unknown): void { failure = normalizePublicError(error); onError(failure); }
  function invalidInput(): never {
    throw new PublicError({ code: 'invalidInput', message: $t('errors.invalidInput'), detail: null, retryable: false });
  }
  function profileName(profile: OptimizationProfile): string {
    const key = `optimization.${profile.id}`;
    const translated = $t(key);
    return translated === key ? profile.name : translated;
  }
  $effect(() => {
    const incoming = copySettings(settings);
    untrack(() => {
      if (draft === null || !dirty) {
        const changedProvider = draft?.providerId !== incoming.providerId;
        baseline = copySettings(incoming);
        draft = copySettings(incoming);
        if (changedProvider) secret = '';
        if (incoming.providerId && !catalogs[incoming.providerId]) void loadCatalog(incoming.providerId, false);
      }
    });
  });

  async function loadProviders(): Promise<void> {
    const generation = ++providerGeneration;
    const loaded = await request('providers_list', undefined);
    if (!disposed && generation === providerGeneration) providers = loaded;
  }
  async function loadSupport(): Promise<void> {
    const generation = ++supportGeneration;
    const providerRequestGeneration = ++providerGeneration;
    const [providerResult, profileResult] = await Promise.allSettled([
      request('providers_list', undefined), request('optimization_profiles', undefined),
    ] as const);
    if (disposed || generation !== supportGeneration) return;
    if (providerResult.status === 'fulfilled') {
      if (providerRequestGeneration === providerGeneration) providers = providerResult.value;
    }
    else report(providerResult.reason);
    if (profileResult.status === 'fulfilled') profiles = profileResult.value;
    else report(profileResult.reason);
  }
  async function loadCatalog(id: ProviderId, force: boolean): Promise<ModelCatalog | null> {
    const generation = (catalogRequests[id] ?? 0) + 1;
    catalogRequests[id] = generation;
    catalogLoading = [...new Set([...catalogLoading, id])];
    try {
      const result = await request('provider_models', { id, force });
      if (disposed || catalogRequests[id] !== generation) return null;
      if (result.providerId !== id) invalidInput();
      const catalog = { ...result, models: [...new Map(result.models.map((model) => [model.id, model])).values()] };
      catalogs = { ...catalogs, [id]: catalog };
      return catalog;
    } catch (error) { if (!disposed && catalogRequests[id] === generation) report(error); return null; }
    finally { if (!disposed && catalogRequests[id] === generation) catalogLoading = catalogLoading.filter((loadingId) => loadingId !== id); }
  }
  function chooseProvider(value: string): void {
    if (!draft || blocked) return;
    failure = null;
    secret = '';
    persistSecret = false;
    if (!value) { draft.providerId = null; draft.modelId = null; return; }
    const id = providerIds.find((candidate) => candidate === value);
    if (!id) return;
    if (draft.providerId !== id) { draft.providerId = id; draft.modelId = null; }
    void loadCatalog(id, false);
  }
  function chooseModel(value: string): void {
    if (!draft || blocked) return;
    draft.modelId = currentCatalog?.models.some((model) => model.id === value) ? value : null;
  }
  function mergeChanges(server: Settings, pending: Settings, previous: Settings): Settings {
    const merged = copySettings(server);
    function apply<Key extends keyof Settings>(key: Key): void {
      if (pending[key] !== previous[key]) merged[key] = pending[key];
    }
    const keys = ['language', 'theme', 'providerId', 'modelId', 'autoEnrich', 'webEnabled', 'autoApplyConfidence', 'defaultDeviceProfile', 'defaultOptimizationProfile', 'maxConcurrentJobs'] as const;
    keys.forEach(apply);
    return merged;
  }
  function validate(value: Settings): void {
    if (value.language !== 'system' && !availableLanguages.some((language) => language.code === value.language)) invalidInput();
    if (!['system', 'light', 'dark'].includes(value.theme) || !deviceProfiles.includes(value.defaultDeviceProfile)) invalidInput();
    if (!Number.isFinite(value.autoApplyConfidence) || value.autoApplyConfidence < 0 || value.autoApplyConfidence > 1) invalidInput();
    if (!Number.isSafeInteger(value.maxConcurrentJobs) || value.maxConcurrentJobs < 1 || value.maxConcurrentJobs > 4) invalidInput();
    if ((value.providerId === null) !== (value.modelId === null)) invalidInput();
    if (value.providerId !== null && !providerIds.includes(value.providerId)) invalidInput();
    if (value.modelId !== null && !value.modelId.trim()) invalidInput();
    if (baseline && value.defaultOptimizationProfile !== baseline.defaultOptimizationProfile
      && !profiles.some((profile) => profile.id === value.defaultOptimizationProfile)) invalidInput();
  }
  async function save(): Promise<void> {
    if (!draft || !baseline || blocked || !dirty || !validCombination) return;
    const pending = copySettings(draft);
    const previous = copySettings(baseline);
    saving = true;
    failure = null;
    try {
      const server = await request('settings_get', undefined);
      const value = mergeChanges(server, pending, previous);
      validate(value);
      const modelChanged = value.providerId !== server.providerId || value.modelId !== server.modelId;
      if (modelChanged && value.providerId && value.modelId) {
        const catalog = await loadCatalog(value.providerId, true);
        if (!catalog || catalog.error || catalog.stale) {
          throw new PublicError(catalog?.error ?? { code: 'providerError', message: $t('providers.stale'), retryable: true, detail: null });
        }
        if (!catalog.models.some((model) => model.id === value.modelId)) invalidInput();
      }
      if (disposed) return;
      const saved = await request('settings_save', { settings: value });
      if (disposed) return;
      baseline = copySettings(saved);
      draft = copySettings(saved);
      onSettingsChange(saved);
      onNotify($t('settings.saved'));
    } catch (error) { if (!disposed) report(error); }
    finally { if (!disposed) saving = false; }
  }
  async function setKey(): Promise<void> {
    if (!selectedProvider || blocked || demo || !secret.trim() || selectedProvider.connectionMode !== 'api') return;
    const id = selectedProvider.id;
    const providerName = selectedProvider.name;
    const value = secret.trim();
    secretBusy = id;
    failure = null;
    try {
      if (/[\u0000-\u001f\u007f]/u.test(value)) invalidInput();
      await request('provider_set_secret', { id, secret: value, persist: persistSecret });
      secret = '';
      if (disposed) return;
      onNotify(`${$t('settings.saved')} · ${providerName}`);
      await loadProviders();
      await loadCatalog(id, true);
    } catch (error) { if (!disposed) report(error); }
    finally { if (!disposed) { secretBusy = null; secret = ''; } }
  }
  async function clearKey(): Promise<void> {
    if (!selectedProvider || blocked || demo || selectedProvider.connectionMode !== 'api') return;
    const id = selectedProvider.id;
    const providerName = selectedProvider.name;
    secretBusy = id;
    failure = null;
    secret = '';
    try {
      await request('provider_clear_secret', { id });
      if (disposed) return;
      onNotify(`${$t('settings.clearKey')} · ${providerName}`);
      await loadProviders();
      await loadCatalog(id, true);
    } catch (error) { if (!disposed) report(error); }
    finally { if (!disposed) secretBusy = null; }
  }
  function reset(): void {
    if (!baseline || blocked) return;
    draft = copySettings(baseline);
    secret = '';
    persistSecret = false;
    failure = null;
  }
  $effect(() => { untrack(() => { void loadSupport(); }); });
  onDestroy(() => { disposed = true; supportGeneration += 1; providerGeneration += 1; secret = ''; });
</script>

<section class="page">
  <div class="page-header"><div><p class="eyebrow">Library Manager</p><h1 class="page-title">{$t('settings.title')}</h1><p class="page-subtitle">{$t('app.tagline')}</p></div><div class="toolbar"><button class="button secondary" type="button" disabled={blocked || !dirty} onclick={reset}>{$t('actions.cancel')}</button><button class="button primary" type="submit" form="settings-form" disabled={blocked || !dirty || !validCombination}><Check size={17} />{$t('actions.save')}</button></div></div>
  {#if failure}<div class="error-banner" role="alert"><div class="grow"><strong>{$t(`errors.${failure.code}`)}</strong>{#if failure.code === 'secretStoreUnavailable'}<p>{$t('settings.noKeyring')}</p>{/if}{#if failure.detail}<p>{failure.detail}</p>{/if}</div><button class="icon-button" type="button" onclick={() => { failure = null; }} aria-label={$t('actions.close')}><X size={16} /></button></div>{/if}
  {#if demo}<p class="field-hint demo-hint">{$t('app.previewDescription')}</p>{/if}
  {#if draft}
    <div class="settings-layout">
      <nav class="settings-nav" aria-label={$t('settings.title')}><a class="nav-item" href="#settings-interface"><Palette size={17} />{$t('settings.theme')}</a><a class="nav-item" href="#settings-library"><Settings2 size={17} />{$t('sidebar.library')}</a><a class="nav-item" href="#settings-providers"><Sparkles size={17} />{$t('providers.title')}</a><a class="nav-item" href="#settings-about"><ShieldCheck size={17} />{$t('settings.about')}</a></nav>
      <form id="settings-form" class="settings-content" onsubmit={(event) => { event.preventDefault(); void save(); }}>
        <section id="settings-interface" class="panel settings-section" aria-labelledby="interface-title"><h2 id="interface-title"><Palette size={20} />{$t('settings.theme')}</h2><fieldset disabled={blocked}><div class="field-row"><div class="field"><label for="settings-language">{$t('settings.language')}</label><select id="settings-language" class="select" bind:value={draft.language}><option value="system">{$t('settings.systemLanguage')}</option>{#each availableLanguages as language (language.code)}<option value={language.code}>{language.nativeName}</option>{/each}</select></div><div class="field"><label for="settings-theme">{$t('settings.theme')}</label><select id="settings-theme" class="select" bind:value={draft.theme}><option value="system">{$t('settings.systemTheme')}</option><option value="light">{$t('settings.lightTheme')}</option><option value="dark">{$t('settings.darkTheme')}</option></select></div></div></fieldset></section>

        <section id="settings-library" class="panel settings-section stack" aria-labelledby="library-settings-title"><h2 id="library-settings-title"><Settings2 size={20} />{$t('sidebar.library')}</h2><fieldset disabled={blocked}><div class="stack">
          <div class="field"><label for="settings-library-root">{$t('settings.libraryRoot')} · {$t('devices.readOnly')}</label><input id="settings-library-root" class="input" value={draft.libraryRoot} readonly /></div>
          <label class="checkbox-field"><input type="checkbox" bind:checked={draft.autoEnrich} />{$t('settings.autoEnrich')}</label>
          <label class="checkbox-field"><input type="checkbox" bind:checked={draft.webEnabled} />{$t('settings.webEnabled')}</label><p class="field-hint">{$t('providers.internetByApp')}</p>
          <div class="field"><label for="settings-confidence">{$t('settings.confidence')} · {percent.format(draft.autoApplyConfidence)}</label><input id="settings-confidence" class="confidence-slider" type="range" min="0" max="1" step="0.01" bind:value={draft.autoApplyConfidence} aria-describedby="confidence-hint" /><p id="confidence-hint" class="field-hint">{$t('settings.confidenceHint')}</p></div>
          <div class="field-row"><div class="field"><label for="settings-device-profile">{$t('settings.deviceProfile')}</label><select id="settings-device-profile" class="select" bind:value={draft.defaultDeviceProfile}><option value="generic">{$t('devices.usb')}</option><option value="xteink">Xteink</option><option value="kindle">Kindle</option><option value="kobo">Kobo</option></select></div><div class="field"><label for="settings-optimization">{$t('settings.optimizationProfile')}</label><select id="settings-optimization" class="select" bind:value={draft.defaultOptimizationProfile} disabled={!profiles.length}>{#if !profiles.some((profile) => profile.id === draft?.defaultOptimizationProfile)}<option value={draft.defaultOptimizationProfile}>{draft.defaultOptimizationProfile}</option>{/if}{#each profiles as profile (profile.id)}<option value={profile.id}>{profileName(profile)}</option>{/each}</select></div></div>
          <div class="field concurrency-field"><label for="settings-concurrency">{$t('settings.maxConcurrentJobs')}</label><input id="settings-concurrency" class="input" type="number" min="1" max="4" step="1" bind:value={draft.maxConcurrentJobs} required /></div>
        </div></fieldset></section>

        <section id="settings-providers" class="panel settings-section stack" aria-labelledby="providers-title"><h2 id="providers-title"><Sparkles size={20} />{$t('providers.title')}</h2>
          <div class="provider-grid">{#each providers as provider (provider.id)}<button class="provider-card provider-choice" class:chosen={draft.providerId === provider.id} type="button" aria-pressed={draft.providerId === provider.id} disabled={blocked} onclick={() => chooseProvider(provider.id)}><div class="provider-initial" aria-hidden="true">{provider.name.slice(0, 1)}</div><strong>{provider.name}</strong><span class="badge" class:success={provider.status === 'ready'} class:warning={provider.status === 'needsKey'}>{$t(`providers.${provider.status}`)}</span><span class="field-hint">{$t(`providers.${provider.connectionMode === 'localCli' ? 'localCli' : 'api'}`)}</span></button>{/each}</div>
          <fieldset disabled={blocked}><div class="field-row"><div class="field"><label for="settings-provider">{$t('chat.provider')}</label><select id="settings-provider" class="select" value={draft.providerId ?? ''} onchange={(event) => chooseProvider(event.currentTarget.value)}><option value="">{$t('common.none')}</option>{#each providers as provider (provider.id)}<option value={provider.id}>{provider.name}</option>{/each}</select></div><div class="field"><label for="settings-model">{$t('chat.model')}</label><select id="settings-model" class="select" value={draft.modelId ?? ''} onchange={(event) => chooseModel(event.currentTarget.value)} disabled={!currentCatalog?.models.length || (draft.providerId !== null && catalogLoading.includes(draft.providerId))}><option value="">{$t('common.none')}</option>{#if draft.modelId && !currentCatalog?.models.some((model) => model.id === draft?.modelId)}<option value={draft.modelId} disabled>{draft.modelId} · {$t('common.unknown')}</option>{/if}{#each currentCatalog?.models ?? [] as model (model.id)}<option value={model.id}>{model.name}</option>{/each}</select></div></div></fieldset>
          {#if selectedProvider}
            <div class="catalog-toolbar"><button class="button secondary" type="button" disabled={blocked || catalogLoading.includes(selectedProvider.id)} onclick={() => loadCatalog(selectedProvider.id, true)}><RefreshCw size={16} />{$t('providers.refreshModels')}</button>{#if catalogLoading.includes(selectedProvider.id)}<p class="field-hint" aria-live="polite">{$t('providers.modelsLoading')}</p>{/if}</div>
            {#if currentCatalog}<div class="catalog-meta row wrap"><span class="badge">{$t(`providers.${currentCatalog.source}`)}</span><span class="field-hint">{formatDate(currentCatalog.fetchedAt, $locale)}</span>{#if currentCatalog.stale}<span class="badge warning">{$t('providers.stale')}</span>{/if}</div>{#if currentCatalog.error}<p class="catalog-error" role="status">{$t(`errors.${currentCatalog.error.code}`)}</p>{/if}{/if}
            {#if !currentCatalog?.models.length && !catalogLoading.includes(selectedProvider.id)}<p class="field-hint">{$t('providers.noModels')}</p>{/if}
            {#if selectedModel}<div class="model-details"><strong>{selectedModel.name}</strong>{#if selectedModel.description}<p>{selectedModel.description}</p>{/if}<dl><dt>{$t('providers.contextWindow')}</dt><dd>{selectedModel.contextWindow === null ? $t('common.unknown') : number.format(selectedModel.contextWindow)}</dd><dt>{$t('providers.toolSupport')}</dt><dd>{selectedModel.supportsTools === null ? $t('common.unknown') : $t(selectedModel.supportsTools ? 'common.yes' : 'common.no')}</dd></dl><p class="field-hint">{$t('providers.internetByApp')}</p></div>{/if}
            {#if selectedProvider.connectionMode === 'api'}<section class="secret-settings stack" aria-labelledby="secret-title"><h3 id="secret-title"><KeyRound size={18} />{$t('settings.apiKey')} · {selectedProvider.name}</h3><div class="field"><label for="provider-secret">{$t('settings.apiKey')}</label><input id="provider-secret" class="input" type="password" bind:value={secret} autocomplete="off" spellcheck="false" disabled={blocked || demo} aria-describedby="secret-hint" /></div><label class="checkbox-field"><input type="checkbox" bind:checked={persistSecret} disabled={blocked || demo} />{$t('settings.persistKey')}</label>{#if !persistSecret}<p class="field-hint">{$t('settings.sessionKey')}</p>{/if}<p id="secret-hint" class="field-hint">{$t('settings.secretHint')}</p><div class="row wrap"><button class="button secondary" type="button" disabled={demo || blocked || !secret.trim()} onclick={setKey}><KeyRound size={16} />{$t('settings.saveKey')}</button><button class="button ghost" type="button" disabled={demo || blocked || !selectedProvider.configured} onclick={clearKey}><X size={16} />{$t('settings.clearKey')}</button></div></section>{:else}<p class="field-hint">{$t('providers.localCli')}</p>{/if}
          {:else}<p class="field-hint">{$t('chat.noModel')}</p>{/if}
        </section>

        <section id="settings-about" class="panel settings-section stack" aria-labelledby="about-title"><h2 id="about-title"><Globe size={20} />{$t('settings.about')}</h2><div class="row about-brand"><img src="/library-manager.png" alt="" width="52" height="52" /><div><strong>Library Manager</strong><p class="field-hint">{$t('settings.version', { name: appVersion })}</p></div></div><p>{$t('app.tagline')}</p><p class="field-hint">{$t('settings.license')}</p><p class="field-hint">{$t('devices.standaloneHint')}</p></section>
        <div class="settings-save"><button class="button secondary" type="button" disabled={blocked || !dirty} onclick={reset}>{$t('actions.cancel')}</button><button class="button primary" type="submit" disabled={blocked || !dirty || !validCombination}><Check size={17} />{$t('actions.save')}</button></div>
      </form>
    </div>
  {:else}<p class="field-hint">{$t('common.loading')}</p>{/if}
</section>

<style>
  fieldset { min-width: 0; margin: 0; padding: 0; border: 0; }
  h2 { display: flex; align-items: center; gap: 11px; margin-bottom: 24px; font-size: 19px; }
  h3 { display: flex; align-items: center; gap: 9px; font-size: 15px; }
  .settings-content { min-width: 0; }
  .settings-nav .nav-item { color: var(--text-muted); font-size: 12px; }
  .settings-nav .nav-item:hover { color: var(--accent-ink); background: var(--accent-soft); }
  .settings-section { scroll-margin-top: 24px; padding: 24px; }
  .settings-section.stack { gap: 22px; }
  .settings-section.stack h2 { margin-bottom: 0; }
  .provider-grid { display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); gap: 13px; }
  .provider-choice { display: flex; flex-direction: column; align-items: flex-start; gap: 10px; padding: 18px; text-align: start; }
  .provider-choice.chosen { border-color: var(--accent); background: var(--accent-soft); }
  .provider-initial { display: grid; place-items: center; width: 36px; height: 36px; border-radius: 11px; background: var(--surface-muted); color: var(--accent-ink); font-weight: 750; font-size: 19px; }
  .catalog-toolbar { display: flex; align-items: center; flex-wrap: wrap; gap: 14px; }
  .catalog-error { color: var(--danger); font-size: 13px; }
  .model-details { padding: 18px; border: 1px solid var(--border); border-radius: var(--radius-sm); background: var(--surface-muted); }
  .model-details > p { margin-top: 10px; font-size: 12px; line-height: 1.7; }
  .model-details dl { display: grid; grid-template-columns: 1fr auto; gap: 10px; margin-top: 16px; font-size: 12px; }
  .model-details dt { color: var(--text-muted); }
  .model-details dd { margin: 0; }
  .secret-settings { border-top: 1px solid var(--border); padding-top: 22px; }
  .confidence-slider { width: 100%; min-height: 44px; accent-color: var(--accent); }
  .concurrency-field { max-width: 240px; }
  .settings-save { display: flex; justify-content: flex-end; flex-wrap: wrap; gap: 12px; margin-top: 24px; }
  .demo-hint { margin-bottom: 22px; }
  .about-brand { align-items: center; }
  @media (max-width: 1100px) { .settings-section { padding: 20px; } .provider-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); } }
  @media (max-width: 760px) { .settings-section { padding: 16px; } .provider-grid { grid-template-columns: 1fr; } }
</style>
