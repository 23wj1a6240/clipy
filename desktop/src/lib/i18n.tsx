/**
 * The desktop app's language. docs/i18n.md, "Desktop app".
 *
 * Every window (the recorder popover and each overlay: countdown, camera,
 * stop controls, What's new, setup, notes...) mounts `DesktopIntl` from
 * main.tsx, so `useTranslations("desktop")` works anywhere under it.
 *
 * Which language:
 *   1. the choice in Settings → Preferences → Language, kept in localStorage
 *      (every Clipy webview shares one origin, so all windows see it and the
 *      `storage` event moves open windows along);
 *   2. if nothing was ever chosen on this machine, the account's language
 *      (`GET /api/me/locale` through Rust, which holds the bearer token), so a
 *      language picked on the web follows the account; it is then remembered
 *      as this machine's choice;
 *   3. else the OS language ("System default"), from the webview's
 *      `navigator.languages`, mapped with `normalizeLocale()`;
 *   4. else English.
 *
 * The main window reports the resolved language to Rust (`set_ui_locale`) so
 * the tray menu, notifications, window titles and dialogs follow it
 * (src-tauri/src/i18n.rs, which reads the `native` subtree of the same
 * desktop.json). A choice made here is saved on the account too
 * (`save_account_locale`), so the web app follows.
 *
 * Imports from the shared catalog are relative here (not `@clipy/i18n`) so
 * the root vitest run, which has no such alias, can render desktop
 * components. Components import `useTranslations` from `use-intl` directly.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { IntlProvider } from "use-intl";
import englishDesktop from "../vendor/i18n/messages/en/desktop.json";
import { brandMessages } from "./edition";
import { loadMessages } from "../vendor/i18n/catalog";
import {
  DEFAULT_LOCALE,
  LOCALES,
  LOCALE_INFO,
  isLocale,
  normalizeLocale,
  type Locale,
} from "../vendor/i18n/locales";

export type { Locale };
export { LOCALES, LOCALE_INFO };

/** "system" follows the OS language. */
export type LanguageChoice = Locale | "system";

const CHOICE_KEY = "clipy:language";
const CHANGE_EVENT = "clipy:language-changed";

type DesktopMessages = { desktop: typeof englishDesktop };
const ENGLISH: DesktopMessages = { desktop: englishDesktop };

function readStorage(): string | null {
  try {
    return window.localStorage.getItem(CHOICE_KEY);
  } catch {
    return null;
  }
}

/** The stored choice, or null when this machine never chose one. */
export function readLanguageChoice(): LanguageChoice | null {
  const raw = readStorage();
  if (raw === "system") return "system";
  return isLocale(raw) ? raw : null;
}

/** The OS language as the webview reports it, mapped onto a shipped locale. */
export function systemLocale(
  languages: readonly string[] = typeof navigator === "undefined"
    ? []
    : navigator.languages?.length
      ? navigator.languages
      : [navigator.language],
): Locale {
  for (const tag of languages) {
    const locale = normalizeLocale(tag);
    if (locale) return locale;
  }
  return DEFAULT_LOCALE;
}

export function resolveDesktopLocale(choice: LanguageChoice | null): Locale {
  return choice && choice !== "system" ? choice : systemLocale();
}

function writeChoice(choice: LanguageChoice) {
  try {
    window.localStorage.setItem(CHOICE_KEY, choice);
  } catch {
    // Storage blocked: the choice lasts for this window only.
  }
  window.dispatchEvent(new CustomEvent(CHANGE_EVENT, { detail: choice }));
}

interface LanguageContextValue {
  choice: LanguageChoice;
  locale: Locale;
  setChoice: (choice: LanguageChoice) => void;
}

const LanguageContext = createContext<LanguageContextValue>({
  choice: "system",
  locale: DEFAULT_LOCALE,
  setChoice: () => {},
});

/** The Settings language row reads and changes the choice through this. */
export function useLanguageChoice(): LanguageContextValue {
  return useContext(LanguageContext);
}

function isMainWindow(): boolean {
  return typeof window !== "undefined" && (window.location.hash === "" || window.location.hash === "#");
}

function timeZone(): string | undefined {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return undefined;
  }
}

/** A provider with fixed messages, for tests and static renders. */
export function StaticDesktopIntl({
  locale = DEFAULT_LOCALE,
  messages = ENGLISH,
  children,
}: {
  locale?: Locale;
  messages?: DesktopMessages;
  children: ReactNode;
}) {
  const branded = useMemo(() => brandMessages(messages), [messages]);
  return (
    <IntlProvider locale={locale} messages={branded} timeZone={timeZone()} onError={() => undefined}>
      {children}
    </IntlProvider>
  );
}

export function DesktopIntl({ children }: { children: ReactNode }) {
  const [stored, setStored] = useState<LanguageChoice | null>(() => readLanguageChoice());
  const choice: LanguageChoice = stored ?? "system";
  const locale = resolveDesktopLocale(stored);
  const [loaded, setLoaded] = useState<{ locale: Locale; messages: DesktopMessages }>({
    locale: DEFAULT_LOCALE,
    messages: ENGLISH,
  });
  // Set once the window has shown its first strings. After that a language
  // change keeps the tree mounted (with the old strings until the new ones
  // load): unmounting would drop unsaved state such as screenshot annotations.
  const shownRef = useRef(false);

  // Other windows (storage) and this one (custom event) changing the choice.
  useEffect(() => {
    const onStorage = (event: StorageEvent) => {
      if (event.key === CHOICE_KEY) setStored(readLanguageChoice());
    };
    const onChange = () => setStored(readLanguageChoice());
    window.addEventListener("storage", onStorage);
    window.addEventListener(CHANGE_EVENT, onChange);
    return () => {
      window.removeEventListener("storage", onStorage);
      window.removeEventListener(CHANGE_EVENT, onChange);
    };
  }, []);

  useEffect(() => {
    let active = true;
    if (locale === DEFAULT_LOCALE) {
      setLoaded({ locale, messages: ENGLISH });
      return;
    }
    loadMessages(locale, ["desktop"] as const)
      .then((messages) => {
        if (active) setLoaded({ locale, messages: messages as unknown as DesktopMessages });
      })
      .catch(() => {
        // Shown in English rather than not at all.
        if (active) setLoaded({ locale, messages: ENGLISH });
      });
    return () => {
      active = false;
    };
  }, [locale]);

  useEffect(() => {
    document.documentElement.lang = LOCALE_INFO[locale].locale;
    if (isMainWindow()) void invoke("set_ui_locale", { locale }).catch(() => {});
  }, [locale]);

  // Never chosen on this machine: adopt the account's language once signed
  // in. Main window only, so one request answers for every window.
  useEffect(() => {
    if (stored !== null || !isMainWindow()) return;
    let done = false;
    const adopt = () => {
      if (done) return;
      void invoke<string | null>("get_account_locale")
        .then((accountLocale) => {
          if (done || readLanguageChoice() !== null || !isLocale(accountLocale)) return;
          done = true;
          writeChoice(accountLocale);
        })
        .catch(() => {});
    };
    adopt();
    let unlisten: (() => void) | undefined;
    void listen("auth-state-changed", adopt)
      .then((fn) => (done ? fn() : (unlisten = fn)))
      .catch(() => {});
    return () => {
      done = true;
      unlisten?.();
    };
  }, [stored]);

  const setChoice = useCallback((next: LanguageChoice) => {
    writeChoice(next);
    void invoke("save_account_locale", { locale: next === "system" ? null : next }).catch(() => {});
  }, []);

  const context = useMemo(() => ({ choice, locale, setChoice }), [choice, locale, setChoice]);

  // A non-English window waits for its first strings (a local import, a few ms)
  // rather than flashing English first.
  if (!shownRef.current && loaded.locale !== locale && locale !== DEFAULT_LOCALE) return null;
  shownRef.current = true;
  // (On a failed load `loaded` holds this locale with the English strings.)

  return (
    <LanguageContext.Provider value={context}>
      <StaticDesktopIntl locale={loaded.locale} messages={loaded.messages}>
        {children}
      </StaticDesktopIntl>
    </LanguageContext.Provider>
  );
}
