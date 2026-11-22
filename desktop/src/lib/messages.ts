/**
 * Translators for code outside React components (helpers in lib/, module
 * level functions in routes) that produce words the user reads.
 *
 * Such a helper takes a `DesktopT` (keys relative to the `desktop`
 * namespace). Components pass `useDesktopT()`; anything without a provider
 * (tests, non-React callers) gets `englishT`, which reads the English
 * catalog, so there is no second English copy in code. docs/i18n.md.
 */
import { createTranslator, useTranslations } from "use-intl";
import englishDesktop from "../vendor/i18n/messages/en/desktop.json";
import { brandMessages } from "./edition";

export type MessageValues = Record<string, string | number | Date>;

/** `t(key, values)` with keys relative to `desktop`, e.g. `home.start`. */
export type DesktopT = (key: string, values?: MessageValues) => string;

const english = createTranslator({
  locale: "en",
  messages: brandMessages({ desktop: englishDesktop }),
  namespace: "desktop",
});

export const englishT: DesktopT = (key, values) =>
  (english as unknown as DesktopT)(key, values);

/** The current window's `desktop` translator, loosely typed for helpers. */
export function useDesktopT(): DesktopT {
  return useTranslations("desktop") as unknown as DesktopT;
}
