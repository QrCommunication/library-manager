import { derived, get, writable } from 'svelte/store';

interface Dictionary {
  [key: string]: string | Dictionary;
}

export interface AvailableLanguage {
  code: string;
  name: string;
  nativeName: string;
}

export type TranslationParams = Record<string, string | number>;
export type Translator = (key: string, params?: TranslationParams) => string;

const FALLBACK_LOCALE = 'en';
const SIZE_UNITS = ['B', 'KiB', 'MiB', 'GiB', 'TiB', 'PiB'] as const;
const PROVIDER_DIAGNOSTIC_KEYS = new Set([
  'providerDiagnostics.authenticationRejected',
  'providerDiagnostics.requestRejected',
  'providerDiagnostics.requestFailed',
  'providerDiagnostics.responseIncomplete',
  'providerDiagnostics.responseMalformed',
  'providerDiagnostics.noFinalAnswer',
  'providerDiagnostics.metadataInvalid',
]);
const modules = import.meta.glob<unknown>('./locales/*.json', {
  eager: true,
  import: 'default',
});
const catalogs = new Map<string, Dictionary>();

function canonicalLocale(value: string): string | null {
  const tag = value.trim().split(/[.@]/)[0]?.replaceAll('_', '-');
  if (!tag) return null;
  try {
    return Intl.getCanonicalLocales(tag)[0] ?? null;
  } catch {
    return null;
  }
}

function isDictionary(value: unknown): value is Dictionary {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return false;
  return Object.values(value).every((entry) => typeof entry === 'string' || isDictionary(entry));
}

for (const [path, catalog] of Object.entries(modules)) {
  const fileName = path.match(/\/([^/]+)\.json$/)?.[1];
  const code = fileName ? canonicalLocale(fileName) : null;
  if (code && isDictionary(catalog)) catalogs.set(code, catalog);
}

export function normalizeLocale(value: string | null | undefined): string {
  const canonical = value ? canonicalLocale(value) : null;
  if (canonical && catalogs.has(canonical)) return canonical;
  const base = canonical?.split('-')[0];
  return base && catalogs.has(base) ? base : FALLBACK_LOCALE;
}

function systemLocale(): string {
  if (typeof navigator === 'undefined') return FALLBACK_LOCALE;
  return navigator.languages?.[0] || navigator.language || FALLBACK_LOCALE;
}

function languageName(code: string, language: string): string {
  try {
    return new Intl.DisplayNames([language], { type: 'language' }).of(code) || code;
  } catch {
    return code;
  }
}

export const availableLanguages: AvailableLanguage[] = [...catalogs.keys()]
  .sort((left, right) => left.localeCompare(right, FALLBACK_LOCALE))
  .map((code) => ({
    code,
    name: languageName(code, FALLBACK_LOCALE),
    nativeName: languageName(code, code),
  }));

export const locale = writable(normalizeLocale(systemLocale()));

export function setLanguage(choice: string, systemLanguage?: string): void {
  locale.set(normalizeLocale(choice === 'system' ? systemLanguage || systemLocale() : choice));
}

function lookup(catalog: Dictionary | undefined, key: string): string | undefined {
  let value: string | Dictionary | undefined = catalog;
  for (const part of key.split('.')) {
    if (typeof value !== 'object' || !Object.hasOwn(value, part)) return undefined;
    value = value[part];
  }
  return typeof value === 'string' ? value : undefined;
}

function pluralKey(key: string, language: string, count: number | undefined): string {
  if (count === undefined || !Number.isFinite(count)) return key;
  const category = new Intl.PluralRules(language).select(count);
  return `${key}${category.charAt(0).toUpperCase()}${category.slice(1)}`;
}

function createTranslator(language: string): Translator {
  const formatter = new Intl.NumberFormat(language);
  return (key, params) => {
    const count = typeof params?.count === 'number' ? params.count : undefined;
    const localized = catalogs.get(language);
    const fallback = catalogs.get(FALLBACK_LOCALE);
    const template = lookup(localized, pluralKey(key, language, count))
      ?? lookup(localized, key)
      ?? lookup(fallback, pluralKey(key, FALLBACK_LOCALE, count))
      ?? lookup(fallback, key)
      ?? key;
    return template.replace(/\{([a-zA-Z][a-zA-Z0-9_]*)\}/g, (match: string, name: string) => {
      const value = params?.[name];
      if (value === undefined) return match;
      return typeof value === 'number' ? formatter.format(value) : value;
    });
  };
}

export const t = derived(locale, ($locale) => createTranslator(normalizeLocale($locale)));

export function formatProviderDiagnostic(
  code: string,
  detail: string | null,
  language = get(locale),
): string | null {
  if (code !== 'providerError' || detail === null || !PROVIDER_DIAGNOSTIC_KEYS.has(detail)) return null;
  const translated = createTranslator(normalizeLocale(language))(detail);
  return translated === detail || translated.trim() === '' ? null : translated;
}

export function formatSize(bytes: number, language = get(locale)): string {
  if (!Number.isFinite(bytes) || bytes < 0) return '—';
  let amount = bytes;
  let index = 0;
  while (amount >= 1024 && index < SIZE_UNITS.length - 1) {
    amount /= 1024;
    index += 1;
  }
  const number = new Intl.NumberFormat(normalizeLocale(language), {
    maximumFractionDigits: index === 0 ? 0 : 1,
  }).format(amount);
  return `${number}\u00a0${SIZE_UNITS[index]}`;
}

export function formatDate(iso: string, language = get(locale)): string {
  const date = new Date(iso);
  if (!Number.isFinite(date.getTime())) return '—';
  return new Intl.DateTimeFormat(normalizeLocale(language), {
    dateStyle: 'medium',
    timeStyle: 'short',
  }).format(date);
}
