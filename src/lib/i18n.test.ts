import { get } from 'svelte/store';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import fr from './locales/fr.json';
import en from './locales/en.json';
import { availableLanguages, formatDate, formatProviderDiagnostic, formatSize, locale, normalizeLocale, setLanguage, t } from './i18n';

const localeDocuments = import.meta.glob<unknown>('./locales/*.json', { eager: true, import: 'default' });

function entries(value: unknown, prefix = ''): Record<string, string> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) throw new Error(`Invalid dictionary at ${prefix}`);
  const result: Record<string, string> = {};
  for (const [key, child] of Object.entries(value)) {
    const path = prefix ? `${prefix}.${key}` : key;
    if (typeof child === 'string') result[path] = child;
    else Object.assign(result, entries(child, path));
  }
  return result;
}

function placeholders(value: string): string[] {
  return [...new Set([...value.matchAll(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g)].map((match) => match[1] ?? ''))].sort();
}

beforeEach(() => { setLanguage('en'); });
afterEach(() => { vi.unstubAllGlobals(); setLanguage('en'); Reflect.deleteProperty(fr, 'testPluralFixture'); Reflect.deleteProperty(en, 'testPluralFixture'); });

describe('translation catalog contract', () => {
  it('keeps every French and English key and interpolation parameter in parity', () => {
    const french = entries(fr);
    const english = entries(en);
    expect(Object.keys(french).sort()).toEqual(Object.keys(english).sort());
    for (const [key, translation] of Object.entries(english)) {
      expect(translation.trim(), key).not.toBe('');
      expect(french[key]?.trim(), key).not.toBe('');
      expect(placeholders(french[key] ?? ''), key).toEqual(placeholders(translation));
      const params = Object.fromEntries(placeholders(translation).map((name) => [name, name === 'count' ? 2 : 'fixture']));
      for (const language of ['fr', 'en']) {
        setLanguage(language);
        expect(get(t)(key, params), `${language}:${key}`).not.toMatch(/\{[a-zA-Z][a-zA-Z0-9_]*\}/);
      }
    }
  });

  it('discovers every valid locale JSON without maintaining a second language list', () => {
    const expected = Object.entries(localeDocuments).map(([path, document]) => {
      expect(Object.keys(entries(document)).length).toBeGreaterThan(0);
      const filename = path.split('/').at(-1);
      if (!filename) throw new Error('Expected a locale filename');
      return Intl.getCanonicalLocales(filename.slice(0, -5))[0];
    }).sort();
    expect(availableLanguages.map((language) => language.code).sort()).toEqual(expected);
    for (const language of availableLanguages) {
      expect(language.name).not.toBe('');
      expect(language.nativeName).not.toBe('');
      expect(normalizeLocale(language.code)).toBe(language.code);
    }
  });

  it('renders the actual application version and does not leak a raw name placeholder', () => {
    for (const language of ['fr', 'en']) {
      setLanguage(language);
      expect(get(t)('settings.version', { name: '0.1.0' })).toBe('Version 0.1.0');
      expect(get(t)('app.previewConversionUnavailable')).not.toBe('app.previewConversionUnavailable');
    }
  });
});

describe('language selection and fallback', () => {
  it('normalizes system locale forms and falls back for absent or unsupported languages', () => {
    for (const value of ['fr_FR.UTF-8', 'fr-CA', 'FR-fr']) expect(normalizeLocale(value)).toBe('fr');
    for (const value of ['en_US.UTF-8', 'en-GB']) expect(normalizeLocale(value)).toBe('en');
    for (const value of ['', null, undefined, 'de-DE', '%%%invalid']) expect(normalizeLocale(value)).toBe('en');
    vi.stubGlobal('navigator', { languages: ['fr-CA', 'en-US'], language: 'en-US' });
    setLanguage('system');
    expect(get(locale)).toBe('fr');
    setLanguage('system', 'en_US.UTF-8');
    expect(get(locale)).toBe('en');
    setLanguage('fr', 'en-US');
    expect(get(locale)).toBe('fr');
    expect(get(t)('sidebar.library')).toBe('Bibliothèque');
  });

  it('selects plural categories in each language and uses English only for missing translations', () => {
    Reflect.set(fr, 'testPluralFixture', { booksOne: 'Un livre : {count}', booksOther: 'Livres : {count}' });
    Reflect.set(en, 'testPluralFixture', { booksOne: 'One book: {count}', booksOther: 'Books: {count}', fallback: 'English fallback {name}' });
    setLanguage('fr');
    expect(get(t)('testPluralFixture.books', { count: 0 })).toBe('Un livre : 0');
    expect(get(t)('testPluralFixture.books', { count: 1 })).toBe('Un livre : 1');
    expect(get(t)('testPluralFixture.books', { count: 2 })).toBe('Livres : 2');
    expect(get(t)('testPluralFixture.fallback', { name: 'value' })).toBe('English fallback value');
    setLanguage('en');
    expect(get(t)('testPluralFixture.books', { count: 0 })).toBe('Books: 0');
    expect(get(t)('testPluralFixture.books', { count: 1 })).toBe('One book: 1');
    expect(get(t)('testPluralFixture.books', { count: 2 })).toBe('Books: 2');
    expect(get(t)('missing.key')).toBe('missing.key');
    expect(get(t)('constructor')).toBe('constructor');
  });

  it('formats numeric parameters and sizes in the selected language with bounded invalid-value fallbacks', () => {
    setLanguage('fr');
    expect(get(t)('library.bookCount', { count: 1234 })).toContain(new Intl.NumberFormat('fr').format(1234));
    expect(formatSize(1536, 'fr')).toBe('1,5\u00a0KiB');
    expect(formatSize(1536, 'en')).toBe('1.5\u00a0KiB');
    expect(formatSize(0, 'fr')).toBe('0\u00a0B');
    for (const value of [-1, Number.NaN, Number.POSITIVE_INFINITY]) expect(formatSize(value)).toBe('—');
    expect(formatDate('invalid')).toBe('—');
    expect(formatDate('2026-10-09T10:00:00Z', 'fr')).toContain('2026');
    expect(formatDate('2026-10-09T10:00:00Z', 'en')).not.toBe(formatDate('2026-10-09T10:00:00Z', 'fr'));
  });
});

describe('public provider diagnostics', () => {
  const diagnostics = [
    ['authenticationRejected', 'Le fournisseur a refusé la clé API ou les permissions du compte.', 'The provider rejected the API key or account permissions.'],
    ['requestRejected', 'Le fournisseur a refusé les paramètres de la requête.', 'The provider rejected the request parameters.'],
    ['requestFailed', 'La requête adressée au fournisseur IA a échoué.', 'The request to the AI provider failed.'],
    ['responseIncomplete', 'Le fournisseur a renvoyé une réponse incomplète ou l’a refusée. Réessayez ou choisissez un autre modèle.', 'The provider response was incomplete or refused.'],
    ['responseMalformed', 'Le fournisseur a renvoyé une réponse JSON invalide.', 'The provider returned invalid JSON.'],
    ['noFinalAnswer', 'Le fournisseur n’a renvoyé aucun texte final exploitable.', 'The provider did not return a usable final answer.'],
    ['metadataInvalid', 'La réponse du fournisseur ne respecte pas le format requis pour les métadonnées.', 'The metadata response does not match the required format.'],
  ] as const;

  it.each(diagnostics)('renders the %s diagnosis as a readable French or English message', (name, french, english) => {
    const detail = `providerDiagnostics.${name}`;
    expect(formatProviderDiagnostic('providerError', detail, 'fr')).toBe(french);
    expect(formatProviderDiagnostic('providerError', detail, 'en')).toBe(english);
    for (const language of ['fr', 'en']) {
      const rendered = formatProviderDiagnostic('providerError', detail, language);
      expect(rendered).not.toBe(detail);
      expect(rendered).not.toContain('providerDiagnostics.');
    }
  });

  it('hides raw provider responses, secrets, paths and unrecognized or malformed diagnostic keys', () => {
    for (const detail of [
      null,
      '',
      'providerDiagnostics.unknown',
      'ProviderDiagnostics.metadataInvalid',
      ' providerDiagnostics.metadataInvalid',
      'providerDiagnostics.metadataInvalid ',
      'providerDiagnostics.metadataInvalid\n',
      'providerDiagnostics.metadataInvalid\0',
      'providerDiagnostics.metadataInvalid\u200b',
      'Authorization: Bearer PRIVATE_TOKEN',
      '/home/private/library/book.epub',
      '{"content":"PRIVATE_RESPONSE"}',
      '<think>PRIVATE_REASONING</think>Answer',
    ]) {
      for (const language of ['fr', 'en']) {
        expect(formatProviderDiagnostic('providerError', detail, language), `${language}:${detail}`).toBeNull();
      }
    }
    for (const code of ['invalidInput', 'networkUnavailable', 'rateLimited', 'providerNotConfigured', 'provider_error', 'ProviderError', '']) {
      for (const [name] of diagnostics) {
        expect(formatProviderDiagnostic(code, `providerDiagnostics.${name}`, 'fr'), `${code}:${name}`).toBeNull();
      }
    }
  });

  it('normalizes explicit regional locales and follows the current language without caching an earlier translation', () => {
    const detail = 'providerDiagnostics.metadataInvalid';
    const french = 'La réponse du fournisseur ne respecte pas le format requis pour les métadonnées.';
    const english = 'The metadata response does not match the required format.';
    expect(formatProviderDiagnostic('providerError', detail, 'fr_FR.UTF-8')).toBe(french);
    expect(formatProviderDiagnostic('providerError', detail, 'en_US.UTF-8')).toBe(english);
    const rendered: Array<string | null> = [];
    const unsubscribe = locale.subscribe((language) => {
      rendered.push(formatProviderDiagnostic('providerError', detail, language));
    });
    try {
      expect(formatProviderDiagnostic('providerError', detail)).toBe(english);
      setLanguage('fr');
      expect(formatProviderDiagnostic('providerError', detail)).toBe(french);
      setLanguage('en');
      expect(formatProviderDiagnostic('providerError', detail)).toBe(english);
      expect(rendered).toEqual([english, french, english]);
    } finally {
      unsubscribe();
    }
  });
});
