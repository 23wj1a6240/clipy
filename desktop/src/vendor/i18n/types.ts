// SPDX-License-Identifier: Apache-2.0
// Copied from the Clipy monorepo (i18n/types.ts) by the export; the desktop app shares it.
/** The desktop catalog's shape (the export keeps only the desktop namespace). */
import type desktop from './messages/en/desktop.json';

export interface Messages {
  desktop: typeof desktop;
}
