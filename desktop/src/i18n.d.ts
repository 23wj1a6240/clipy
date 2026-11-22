/**
 * Types use-intl's message keys and locales from the shared catalog
 * (i18n/, imported as @clipy/i18n). See docs/i18n.md.
 */
import type { Locale, Messages } from './vendor/i18n/index';

declare module 'use-intl' {
  interface AppConfig {
    Locale: Locale;
    Messages: Messages;
  }
}
