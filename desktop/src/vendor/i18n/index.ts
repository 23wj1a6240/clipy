// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (i18n/index.ts) by the export; the desktop app shares it.
/**
 * The shared catalog: locales, namespaces, the message loader and formatting.
 * Framework-free, imported by the web app, the extension and the desktop app.
 * Guide: docs/i18n.md.
 */
export * from './locales';
export * from './namespaces';
export * from './catalog';
export * from './format';
export type { Messages } from './types';
