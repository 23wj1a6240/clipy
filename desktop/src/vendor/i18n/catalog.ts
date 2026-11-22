// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (i18n/catalog.ts) by the export; the desktop app shares it.
/**
 * Loads messages: English first, then the locale's overlay on top, key by key.
 *
 * A translation file only holds the keys that have been translated. Any key it
 * lacks is English, so a half-translated namespace renders English for the
 * rest instead of raw keys. `pnpm i18n:check` makes sure an overlay never has
 * a key English does not.
 *
 * No framework import, so Next (server and client), Vite (extension, desktop)
 * and tests share it. Files load with a dynamic import, which every bundler
 * here splits into one small chunk per file, so a page only downloads the
 * namespaces and the locale it asks for.
 */
import { DEFAULT_LOCALE, type Locale } from './locales';
import type { Namespace } from './namespaces';
import type { Messages } from './types';

/** A message tree: nested objects whose leaves are ICU message strings. */
export interface MessageTree {
  [key: string]: string | MessageTree;
}

export type NamespaceMessages<N extends readonly Namespace[]> = Pick<Messages, N[number]>;

function isTree(value: unknown): value is MessageTree {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/**
 * Copies `overlay` onto `base`, returning a new tree. Strings in the overlay
 * replace strings in the base; keys the base lacks are dropped, because a key
 * that only exists in a translation has nothing to render it.
 */
export function mergeMessages(base: MessageTree, overlay: MessageTree | undefined): MessageTree {
  if (!overlay) return base;
  const out: MessageTree = {};
  for (const [key, baseValue] of Object.entries(base)) {
    const overlayValue = overlay[key];
    if (isTree(baseValue)) {
      out[key] = isTree(overlayValue) ? mergeMessages(baseValue, overlayValue) : baseValue;
    } else {
      out[key] = typeof overlayValue === 'string' && overlayValue !== '' ? overlayValue : baseValue;
    }
  }
  return out;
}

async function importFile(locale: Locale, namespace: Namespace): Promise<MessageTree | undefined> {
  try {
    // Keep this a template with exactly these two variables and the .json
    // extension: webpack and Vite both turn it into one chunk per file.
    const mod = await import(`./messages/${locale}/${namespace}.json`);
    const tree = (mod as { default?: unknown }).default ?? mod;
    return isTree(tree) ? tree : undefined;
  } catch {
    return undefined;
  }
}

const cache = new Map<string, Promise<MessageTree>>();

/** One namespace for one locale, merged over English. Cached per process. */
export function loadNamespace(locale: Locale, namespace: Namespace): Promise<MessageTree> {
  const cacheKey = `${locale}/${namespace}`;
  let pending = cache.get(cacheKey);
  if (!pending) {
    pending = (async () => {
      const english = (await importFile(DEFAULT_LOCALE, namespace)) ?? {};
      if (locale === DEFAULT_LOCALE) return english;
      return mergeMessages(english, await importFile(locale, namespace));
    })();
    cache.set(cacheKey, pending);
    // A failed load must not stick: let the next caller try again.
    pending.catch(() => cache.delete(cacheKey));
  }
  return pending;
}

/**
 * The namespaces a surface needs, keyed by namespace, ready for an
 * IntlProvider or createTranslator:
 *
 *   const messages = await loadMessages('ja', ['common', 'marketing-pricing']);
 */
export async function loadMessages<const N extends readonly Namespace[]>(
  locale: Locale,
  namespaces: N,
): Promise<NamespaceMessages<N>> {
  const trees = await Promise.all(namespaces.map((namespace) => loadNamespace(locale, namespace)));
  const out: MessageTree = {};
  namespaces.forEach((namespace, index) => {
    out[namespace] = trees[index];
  });
  return out as unknown as NamespaceMessages<N>;
}
