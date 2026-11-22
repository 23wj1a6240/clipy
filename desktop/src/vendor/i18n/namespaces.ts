// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (i18n/namespaces.ts) by the export; the desktop app shares it.
/**
 * Every message namespace. One JSON file per namespace per locale, so agents
 * converting different surfaces never edit the same file:
 *
 *   i18n/messages/<locale>/<namespace>.json
 *
 * Keep one entry per line. Adding a namespace means: a line here, a line in
 * i18n/types.ts, and an `en` file (the other locales may start as `{}`).
 * `pnpm i18n:check` fails if these three disagree.
 */
export const NAMESPACES = ['desktop'] as const;

export type Namespace = (typeof NAMESPACES)[number];

/** The namespace a marketing page's copy lives in: `/pricing` → `marketing-pricing`. */
export function marketingNamespaceFor(path: string): string {
  const slug = path.replace(/^\/+|\/+$/g, '').replace(/\//g, '-');
  return `marketing-${slug || 'home'}`;
}

export function isNamespace(value: string): value is Namespace {
  return (NAMESPACES as readonly string[]).includes(value);
}
