import { describe, expect, it } from 'vitest';
import type { Provider, ProviderId, Settings } from './contracts';
import { isMetadataReady, metadataDisabledReason } from './selection-capabilities';

function settings(patch: Partial<Settings> = {}): Settings {
  return {
    language: 'fr',
    theme: 'system',
    libraryRoot: '/synthetic/test-library',
    providerId: 'minimax',
    modelId: 'synthetic-model',
    autoEnrich: false,
    webEnabled: false,
    autoApplyConfidence: 0.95,
    defaultDeviceProfile: 'default',
    defaultOptimizationProfile: 'balanced',
    maxConcurrentJobs: 2,
    ...patch,
  };
}

function provider(patch: Partial<Provider> = {}): Provider {
  return {
    id: 'minimax',
    name: 'Synthetic provider',
    configured: true,
    connectionMode: 'api',
    supportsTools: true,
    status: 'ready',
    ...patch,
  };
}

function expectUnavailable(
  configured: Settings | null | undefined,
  providers: readonly Provider[] | null | undefined,
  reason: 'providerMissing' | 'modelMissing' | 'providerUnavailable',
): void {
  expect(metadataDisabledReason(configured, providers)).toBe(reason);
  expect(isMetadataReady(configured, providers)).toBe(false);
}

describe('selected-book metadata readiness', () => {
  it('allows a selected model when its API provider is configured and ready', () => {
    expect(metadataDisabledReason(settings(), [provider()])).toBeNull();
    expect(isMetadataReady(settings(), [provider()])).toBe(true);
  });

  it('allows Codex OAuth through the local CLI without requiring an API key', () => {
    const configured = settings({ providerId: 'codex', modelId: 'synthetic-codex-model' });
    const oauth = provider({ id: 'codex', connectionMode: 'localCli' });
    expect(metadataDisabledReason(configured, [oauth])).toBeNull();
    expect(isMetadataReady(configured, [oauth])).toBe(true);
  });

  it('keeps metadata actions unavailable while settings are absent', () => {
    for (const missing of [null, undefined]) {
      expectUnavailable(missing, [provider()], 'providerMissing');
    }
  });

  it('requires an explicitly selected provider even when another provider is ready', () => {
    expectUnavailable(settings({ providerId: null }), [provider()], 'providerMissing');
  });

  it('rejects an unrecognized provider from a malformed runtime settings response', () => {
    // Runtime data can violate the TypeScript union before readiness is evaluated.
    const unknown = 'unrecognized-provider' as ProviderId;
    expectUnavailable(settings({ providerId: unknown }), [provider({ id: unknown })], 'providerMissing');
  });

  it('requires a selected model before enabling a configured provider', () => {
    expectUnavailable(settings({ modelId: null }), [provider()], 'modelMissing');
  });

  it('refuses both empty and whitespace-only model identifiers', () => {
    for (const blank of ['', '   ', '\t\n']) {
      expectUnavailable(settings({ modelId: blank }), [provider()], 'modelMissing');
    }
  });

  it('waits for a provider catalog rather than assuming saved settings prove readiness', () => {
    for (const missing of [null, undefined, []]) {
      expectUnavailable(settings(), missing, 'providerUnavailable');
    }
  });

  it('does not enable metadata because a different provider happens to be ready', () => {
    expectUnavailable(settings(), [provider({ id: 'codex', connectionMode: 'localCli' })], 'providerUnavailable');
  });

  it('refuses an unconfigured selected provider despite a ready status', () => {
    expectUnavailable(settings(), [provider({ configured: false })], 'providerUnavailable');
  });

  it('keeps metadata disabled while the selected provider requires credentials', () => {
    expectUnavailable(settings(), [provider({ status: 'needsKey' })], 'providerUnavailable');
  });

  it('keeps metadata disabled while the configured selected provider is unavailable', () => {
    expectUnavailable(settings(), [provider({ status: 'unavailable' })], 'providerUnavailable');
  });
});
