// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (i18n/locales.ts) by the export; the desktop app shares it.
/**
 * The languages Clipy ships in, and every mapping between them.
 *
 * Plain TypeScript with no framework import: the web app (Next), the Chrome
 * extension (Vite), the desktop app (Vite + Tauri) and the node scripts under
 * scripts/i18n all read this file. Mobile apps read the generated codes from
 * here too (see docs/i18n.md, "Mobile").
 *
 * A `Locale` is a BCP 47 tag and is the only identifier code passes around.
 * The URL segment (`/pt-br/pricing`), the Chrome `_locales` folder, the
 * Android `values-` folder and the iOS `.lproj` folder are all derived from it
 * here, never typed elsewhere.
 */

export const LOCALES = ['en', 'pt-BR', 'es', 'zh-CN', 'ja', 'hi'] as const;
export type Locale = (typeof LOCALES)[number];

/** The source language. Every key exists in it; the others are overlays. */
export const DEFAULT_LOCALE: Locale = 'en';

/** Locales that have translations layered over English. */
export const TRANSLATED_LOCALES = LOCALES.filter(
  (locale): locale is Exclude<Locale, 'en'> => locale !== DEFAULT_LOCALE,
);

export interface LocaleInfo {
  /** BCP 47 tag, the same as the key. */
  locale: Locale;
  /** First path segment of a localized marketing URL. English has none. */
  urlSegment: string | null;
  /** The language's name in that language, for the language picker. */
  nativeName: string;
  /** The language's name in English, for admin screens and logs. */
  englishName: string;
  /** The tag passed to Intl.* formatters. */
  intlLocale: string;
  /** hreflang value in <link rel="alternate"> and sitemaps. */
  hreflang: string;
  /** Open Graph `og:locale`. */
  ogLocale: string;
  /** Chrome extension `_locales/<code>/messages.json` folder. */
  chromeCode: string;
  /** Android resource qualifier: `values-<code>`. */
  androidQualifier: string;
  /** iOS / macOS `<code>.lproj` folder. */
  appleCode: string;
  /** All shipped locales are left to right; kept so an RTL one cannot be forgotten. */
  writingDirection: 'ltr' | 'rtl';
}

export const LOCALE_INFO: Readonly<Record<Locale, LocaleInfo>> = {
  en: {
    locale: 'en',
    urlSegment: null,
    nativeName: 'English',
    englishName: 'English',
    // en-US, not en: every Intl call this catalog replaces was hardcoded to
    // 'en-US', and English output must not change.
    intlLocale: 'en-US',
    hreflang: 'en',
    ogLocale: 'en_US',
    chromeCode: 'en',
    androidQualifier: '',
    appleCode: 'en',
    writingDirection: 'ltr',
  },
  'pt-BR': {
    locale: 'pt-BR',
    urlSegment: 'pt-br',
    nativeName: 'Português (Brasil)',
    englishName: 'Portuguese (Brazil)',
    intlLocale: 'pt-BR',
    hreflang: 'pt-BR',
    ogLocale: 'pt_BR',
    chromeCode: 'pt_BR',
    androidQualifier: 'pt-rBR',
    appleCode: 'pt-BR',
    writingDirection: 'ltr',
  },
  es: {
    locale: 'es',
    urlSegment: 'es',
    nativeName: 'Español',
    englishName: 'Spanish',
    intlLocale: 'es',
    hreflang: 'es',
    ogLocale: 'es_ES',
    chromeCode: 'es',
    androidQualifier: 'es',
    appleCode: 'es',
    writingDirection: 'ltr',
  },
  'zh-CN': {
    locale: 'zh-CN',
    urlSegment: 'zh',
    nativeName: '简体中文',
    englishName: 'Chinese (Simplified)',
    intlLocale: 'zh-CN',
    hreflang: 'zh-Hans',
    ogLocale: 'zh_CN',
    chromeCode: 'zh_CN',
    androidQualifier: 'zh-rCN',
    appleCode: 'zh-Hans',
    writingDirection: 'ltr',
  },
  ja: {
    locale: 'ja',
    urlSegment: 'ja',
    nativeName: '日本語',
    englishName: 'Japanese',
    intlLocale: 'ja',
    hreflang: 'ja',
    ogLocale: 'ja_JP',
    chromeCode: 'ja',
    androidQualifier: 'ja',
    appleCode: 'ja',
    writingDirection: 'ltr',
  },
  hi: {
    locale: 'hi',
    urlSegment: 'hi',
    nativeName: 'हिन्दी',
    englishName: 'Hindi',
    // Latin digits: hi-IN formats with Latin numerals by default, which is
    // what Indian product UIs use. Do not switch to -u-nu-deva.
    intlLocale: 'hi-IN',
    hreflang: 'hi',
    ogLocale: 'hi_IN',
    chromeCode: 'hi',
    androidQualifier: 'hi',
    appleCode: 'hi',
    writingDirection: 'ltr',
  },
};

/** The cookie product pages read the chosen language from. */
export const LOCALE_COOKIE = 'NEXT_LOCALE';
/** One year, the same as next-intl's default. */
export const LOCALE_COOKIE_MAX_AGE_SECONDS = 60 * 60 * 24 * 365;

export function isLocale(value: unknown): value is Locale {
  return typeof value === 'string' && (LOCALES as readonly string[]).includes(value);
}

/**
 * Maps any language tag a browser, OS, Chrome or a phone reports onto a Clipy
 * locale, or null when Clipy has no translation for it.
 *
 * - Case and `_` vs `-` do not matter (`pt_br`, `PT-BR`).
 * - Any Portuguese maps to pt-BR, the one Portuguese Clipy has.
 * - Any Spanish (es-MX, es-419, es-ES) maps to es.
 * - Simplified Chinese (zh, zh-CN, zh-SG, zh-Hans*) maps to zh-CN.
 *   Traditional Chinese (zh-TW, zh-HK, zh-MO, zh-Hant*) maps to null, so those
 *   readers get English rather than a script they did not ask for.
 */
export function normalizeLocale(tag: string | null | undefined): Locale | null {
  if (!tag) return null;
  const parts = tag.trim().replace(/_/g, '-').toLowerCase().split('-').filter(Boolean);
  if (parts.length === 0) return null;
  const [language, ...rest] = parts;
  switch (language) {
    case 'en':
      return 'en';
    case 'pt':
      return 'pt-BR';
    case 'es':
      return 'es';
    case 'ja':
      return 'ja';
    case 'hi':
      return 'hi';
    case 'zh': {
      if (rest.includes('hant')) return null;
      if (rest.includes('hans')) return 'zh-CN';
      if (rest.some((part) => part === 'tw' || part === 'hk' || part === 'mo')) return null;
      return 'zh-CN';
    }
    default:
      return null;
  }
}

/**
 * Picks the best Clipy locale from an Accept-Language header (or
 * `navigator.languages` joined with commas), honouring q-values. Returns null
 * when nothing matches, so the caller decides the fallback.
 *
 * Marketing pages never redirect on this: it is only for product surfaces and
 * for suggesting a language in the picker.
 */
export function matchAcceptLanguage(header: string | null | undefined): Locale | null {
  if (!header) return null;
  const ranked = header
    .split(',')
    .map((entry, index) => {
      const [tag, ...params] = entry.trim().split(';');
      const qParam = params.map((p) => p.trim()).find((p) => p.startsWith('q='));
      const q = qParam ? Number.parseFloat(qParam.slice(2)) : 1;
      return { tag: tag.trim(), q: Number.isFinite(q) ? q : 0, index };
    })
    .filter((entry) => entry.tag && entry.tag !== '*' && entry.q > 0)
    .sort((a, b) => b.q - a.q || a.index - b.index);
  for (const entry of ranked) {
    const locale = normalizeLocale(entry.tag);
    if (locale) return locale;
  }
  return null;
}

/** `pt-br` → `pt-BR`. Null for `en` (English has no prefix) and unknown segments. */
export function localeFromSegment(segment: string | null | undefined): Locale | null {
  if (!segment) return null;
  const lower = segment.toLowerCase();
  for (const locale of TRANSLATED_LOCALES) {
    if (LOCALE_INFO[locale].urlSegment === lower) return locale;
  }
  return null;
}

/** The URL segments of the translated locales: `pt-br`, `es`, `zh`, `ja`, `hi`. */
export const LOCALE_URL_SEGMENTS: readonly string[] = TRANSLATED_LOCALES.map(
  (locale) => LOCALE_INFO[locale].urlSegment as string,
);

/** The locale a pathname is prefixed with (`/ja/pricing` → `ja`), else null. */
export function localeFromPathname(pathname: string | null | undefined): Locale | null {
  if (!pathname) return null;
  const first = pathname.split('/')[1];
  return localeFromSegment(first);
}

/** Removes a locale prefix: `/ja/pricing` → `/pricing`, `/ja` → `/`. */
export function stripLocalePrefix(pathname: string): string {
  const locale = localeFromPathname(pathname);
  if (!locale) return pathname;
  const rest = pathname.slice(1 + (LOCALE_INFO[locale].urlSegment as string).length);
  return rest === '' ? '/' : rest;
}

/**
 * The URL of an English path in a locale: `/pricing` + `ja` → `/ja/pricing`.
 * English returns the path unchanged. Only call this for paths that have a
 * localized version (lib/i18n/routes.ts decides which do).
 */
export function prefixPath(path: string, locale: Locale): string {
  const segment = LOCALE_INFO[locale].urlSegment;
  if (!segment) return path;
  return path === '/' ? `/${segment}` : `/${segment}${path}`;
}
