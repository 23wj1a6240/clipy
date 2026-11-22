// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (i18n/format.ts) by the export; the desktop app shares it.
/**
 * Locale-aware formatting for dates, numbers, money and relative time.
 *
 * Replace hardcoded `toLocaleString('en-US', ...)` and
 * `new Intl.DateTimeFormat('en-US', ...)` calls with these. Each helper takes a
 * Clipy `Locale` (from useLocale() on the client, or the page's locale on the
 * server) and maps it to the right Intl tag, so English keeps its exact
 * en-US output and the other languages get their own conventions.
 *
 * Money: Clipy charges in US dollars everywhere, so amounts stay USD in every
 * language; only the way the number is written changes (`$9`, `US$ 9`,
 * `9 US$`). Never convert currencies here. Amounts come from lib/pricing.ts
 * and the offer modules, never from a translation.
 *
 * Time zones: on the server these format in the server's zone, on the client
 * in the visitor's. A server-rendered date that the client re-renders can
 * differ across midnight; pass `timeZone` (for example 'UTC') to pin it.
 *
 * Plain TypeScript, no framework import: the extension and desktop use it too.
 */
import { DEFAULT_LOCALE, LOCALE_INFO, type Locale } from './locales';

export function intlLocale(locale: Locale | null | undefined): string {
  return LOCALE_INFO[locale ?? DEFAULT_LOCALE]?.intlLocale ?? LOCALE_INFO[DEFAULT_LOCALE].intlLocale;
}

// Intl constructors are slow enough to matter in long lists; reuse them.
const numberFormats = new Map<string, Intl.NumberFormat>();
const dateFormats = new Map<string, Intl.DateTimeFormat>();
const relativeFormats = new Map<string, Intl.RelativeTimeFormat>();
const listFormats = new Map<string, Intl.ListFormat>();

function cached<T>(map: Map<string, T>, key: string, make: () => T): T {
  let value = map.get(key);
  if (!value) {
    value = make();
    map.set(key, value);
  }
  return value;
}

function numberFormat(locale: Locale, options: Intl.NumberFormatOptions = {}): Intl.NumberFormat {
  const tag = intlLocale(locale);
  return cached(numberFormats, `${tag}|${JSON.stringify(options)}`, () => new Intl.NumberFormat(tag, options));
}

function dateFormat(locale: Locale, options: Intl.DateTimeFormatOptions): Intl.DateTimeFormat {
  const tag = intlLocale(locale);
  return cached(dateFormats, `${tag}|${JSON.stringify(options)}`, () => new Intl.DateTimeFormat(tag, options));
}

type DateInput = Date | string | number;

function toDate(value: DateInput): Date {
  return value instanceof Date ? value : new Date(value);
}

// ---------------------------------------------------------------------------
// Numbers

/** `1234.5` → `1,234.5` (en), `1.234,5` (pt-BR). Replaces `n.toLocaleString('en-US')`. */
export function formatNumber(locale: Locale, value: number, options?: Intl.NumberFormatOptions): string {
  return numberFormat(locale, options).format(value);
}

/** `0.33` → `33%`. Pass the fraction, not the percentage. */
export function formatPercent(
  locale: Locale,
  fraction: number,
  options: Intl.NumberFormatOptions = { maximumFractionDigits: 0 },
): string {
  return numberFormat(locale, { ...options, style: 'percent' }).format(fraction);
}

/** `12_345_678` → `12M` (en), `1234万` (zh-CN, ja), `1.2 करोड़` (hi). */
export function formatCompactNumber(locale: Locale, value: number): string {
  return numberFormat(locale, { notation: 'compact', maximumFractionDigits: 1 }).format(value);
}

/** Bytes as KB/MB/GB with the locale's digits: `formatBytes('en', 1536)` → `1.5 KB`. */
export function formatBytes(locale: Locale, bytes: number): string {
  const units = ['byte', 'kilobyte', 'megabyte', 'gigabyte', 'terabyte'] as const;
  let value = Math.max(0, bytes);
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return numberFormat(locale, {
    style: 'unit',
    unit: units[unit],
    unitDisplay: 'short',
    maximumFractionDigits: value < 10 && unit > 0 ? 1 : 0,
  }).format(value);
}

// ---------------------------------------------------------------------------
// Money

export interface CurrencyOptions {
  /** ISO 4217 code. Defaults to USD, the only currency Clipy charges in. */
  currency?: string;
  /**
   * 'auto' (default) drops `.00` from whole amounts (`$9`, `$72`) and keeps
   * cents otherwise (`$50.40`), which is how Clipy's copy writes prices.
   * 'always' always shows cents.
   */
  cents?: 'auto' | 'always';
}

/**
 * A dollar amount in the reader's number style. The amount is a number of
 * dollars (not cents) from lib/pricing.ts or an offer:
 *
 *   formatCurrency('en', 9)        → "$9"
 *   formatCurrency('en', 50.4)     → "$50.40"
 *   formatCurrency('pt-BR', 9)     → "US$ 9"
 *   formatCurrency('es', 72)       → "72 US$"
 *   formatCurrency('ja', 6)        → "$6"
 */
export function formatCurrency(locale: Locale, amount: number, options: CurrencyOptions = {}): string {
  const currency = options.currency ?? 'USD';
  const whole = Number.isInteger(amount) || Math.abs(amount - Math.round(amount)) < 1e-9;
  const showCents = options.cents === 'always' || !whole;
  return numberFormat(locale, {
    style: 'currency',
    currency,
    minimumFractionDigits: showCents ? 2 : 0,
    maximumFractionDigits: showCents ? 2 : 0,
  }).format(amount);
}

/** Same as formatCurrency, from an integer number of cents (Polar amounts). */
export function formatCents(locale: Locale, cents: number, options: CurrencyOptions = {}): string {
  return formatCurrency(locale, cents / 100, options);
}

// ---------------------------------------------------------------------------
// Dates and times

/** `Nov 18, 2026` (en), `18 de nov. de 2026` (pt-BR). Medium date, no time. */
export function formatDate(
  locale: Locale,
  value: DateInput,
  options: Intl.DateTimeFormatOptions = { year: 'numeric', month: 'short', day: 'numeric' },
): string {
  return dateFormat(locale, options).format(toDate(value));
}

/** `November 18, 2026` (en), `2026年11月18日` (ja). */
export function formatLongDate(locale: Locale, value: DateInput, options: Intl.DateTimeFormatOptions = {}): string {
  return formatDate(locale, value, { year: 'numeric', month: 'long', day: 'numeric', ...options });
}

/** `3:05 PM` (en), `15:05` (pt-BR). */
export function formatTime(
  locale: Locale,
  value: DateInput,
  options: Intl.DateTimeFormatOptions = { hour: 'numeric', minute: '2-digit' },
): string {
  return dateFormat(locale, options).format(toDate(value));
}

/** `Nov 18, 2026, 3:05 PM` (en). Date and time together. */
export function formatDateTime(
  locale: Locale,
  value: DateInput,
  options: Intl.DateTimeFormatOptions = {
    year: 'numeric',
    month: 'short',
    day: 'numeric',
    hour: 'numeric',
    minute: '2-digit',
  },
): string {
  return dateFormat(locale, options).format(toDate(value));
}

/** A clock duration from seconds: `75` → `1:15`, `3725` → `1:02:05`. Digits only, same in every locale. */
export function formatClock(totalSeconds: number): string {
  const s = Math.max(0, Math.floor(totalSeconds));
  const hours = Math.floor(s / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const seconds = s % 60;
  const mm = hours > 0 ? String(minutes).padStart(2, '0') : String(minutes);
  return `${hours > 0 ? `${hours}:` : ''}${mm}:${String(seconds).padStart(2, '0')}`;
}

/**
 * A duration in words, largest two units: `formatDuration('en', 3725)` →
 * `1 hr, 2 min`; `('ja', 3725)` → `1 時間 2 分`. Uses Intl.DurationFormat
 * when the runtime has it, otherwise unit-formatted numbers joined by the
 * locale's list style.
 */
export function formatDuration(locale: Locale, totalSeconds: number): string {
  const s = Math.max(0, Math.round(totalSeconds));
  const hours = Math.floor(s / 3600);
  const minutes = Math.floor((s % 3600) / 60);
  const seconds = s % 60;
  const parts: [number, 'hour' | 'minute' | 'second'][] = [];
  if (hours) parts.push([hours, 'hour']);
  if (minutes) parts.push([minutes, 'minute']);
  if (seconds || parts.length === 0) parts.push([seconds, 'second']);
  const shown = parts.slice(0, 2);
  return formatList(
    locale,
    shown.map(([value, unit]) => numberFormat(locale, { style: 'unit', unit, unitDisplay: 'short' }).format(value)),
    { type: 'unit', style: 'narrow' },
  );
}

// ---------------------------------------------------------------------------
// Relative time

const RELATIVE_STEPS: [Intl.RelativeTimeFormatUnit, number][] = [
  ['second', 60],
  ['minute', 60],
  ['hour', 24],
  ['day', 7],
  ['week', 4.34524],
  ['month', 12],
  ['year', Number.POSITIVE_INFINITY],
];

/**
 * `3 minutes ago`, `in 2 days`, `yesterday` (with numeric: 'auto'). Picks the
 * largest unit that keeps the number at least 1. `now` is injectable for tests
 * and for rendering the same value on server and client.
 */
export function formatRelativeTime(
  locale: Locale,
  value: DateInput,
  now: DateInput = Date.now(),
  options: Intl.RelativeTimeFormatOptions = { numeric: 'auto' },
): string {
  const tag = intlLocale(locale);
  const formatter = cached(
    relativeFormats,
    `${tag}|${JSON.stringify(options)}`,
    () => new Intl.RelativeTimeFormat(tag, options),
  );
  let delta = (toDate(value).getTime() - toDate(now).getTime()) / 1000;
  for (const [unit, size] of RELATIVE_STEPS) {
    if (Math.abs(delta) < size) return formatter.format(Math.round(delta), unit);
    delta /= size;
  }
  return formatter.format(Math.round(delta), 'year');
}

// ---------------------------------------------------------------------------
// Lists

/** `['Mac', 'Windows', 'Chrome']` → `Mac, Windows, and Chrome` (en), `Mac、Windows、Chrome` (ja). */
export function formatList(
  locale: Locale,
  items: readonly string[],
  options: Intl.ListFormatOptions = { type: 'conjunction', style: 'long' },
): string {
  const tag = intlLocale(locale);
  const formatter = cached(listFormats, `${tag}|${JSON.stringify(options)}`, () => new Intl.ListFormat(tag, options));
  return formatter.format(items);
}

// ---------------------------------------------------------------------------
// Names

const regionNames = new Map<string, Intl.DisplayNames | null>();

/**
 * A country's name in `locale` from its ISO 3166 code: `('ja', 'BR')` → `ブラジル`.
 * English returns `englishName` when given, so English copy keeps the exact
 * name it had; otherwise Intl's name, then the code.
 */
export function formatRegionName(locale: Locale, code: string, englishName?: string): string {
  if ((locale ?? DEFAULT_LOCALE) === DEFAULT_LOCALE && englishName) return englishName;
  const tag = intlLocale(locale);
  let names = regionNames.get(tag);
  if (names === undefined) {
    try {
      names = new Intl.DisplayNames([tag], { type: 'region' });
    } catch {
      names = null;
    }
    regionNames.set(tag, names);
  }
  try {
    return names?.of(code.toUpperCase()) ?? englishName ?? code;
  } catch {
    return englishName ?? code;
  }
}
