import type { Provider, ProviderId, Settings } from './contracts';

export const MAX_SELECTED_BOOKS = 200;

const PROVIDER_IDS = new Set<ProviderId>(['zai', 'kimi', 'minimax', 'codex', 'claude', 'mistral']);

export type MetadataDisabledReason = 'providerMissing' | 'modelMissing' | 'providerUnavailable';

export function metadataDisabledReason(
  settings: Settings | null | undefined,
  providers: readonly Provider[] | null | undefined,
): MetadataDisabledReason | null {
  if (!settings?.providerId || !PROVIDER_IDS.has(settings.providerId)) return 'providerMissing';
  if (typeof settings.modelId !== 'string' || !settings.modelId.trim()) return 'modelMissing';
  if (!Array.isArray(providers) || !providers.some((provider) => provider?.id === settings.providerId
    && provider.configured === true && provider.status === 'ready')) return 'providerUnavailable';
  return null;
}

export function isMetadataReady(
  settings: Settings | null | undefined,
  providers: readonly Provider[] | null | undefined,
): boolean {
  return metadataDisabledReason(settings, providers) === null;
}
