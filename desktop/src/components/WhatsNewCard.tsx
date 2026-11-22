import { useEffect, useLayoutEffect, useMemo, useState } from "react";
import { useTranslations } from "use-intl";
import { getRecorderShortcuts } from "../lib/bridge";
import { hotkeyKeycaps } from "../lib/hotkey";
import { useDesktopT } from "../lib/messages";
import {
  acknowledgeWhatsNew,
  getWhatsNewState,
  localizeFeature,
  newsFor,
  onWhatsNewChanged,
  showWhatsNewWindow,
  snoozeWhatsNew,
  type WhatsNewStateDto,
} from "../lib/whatsNew";

/** How many new features the card names before "and N more". */
const LISTED = 3;

/**
 * The recorder's one-time note after an update. It sits above the source
 * rows and moves nothing: Start recording stays where it was, so someone in a
 * hurry ignores it. The parent renders it only while idle.
 *
 * ✕ marks the update as seen for good; Later hides it until the next launch
 * (held in Rust, since this card unmounts for every recording); "Try them"
 * opens What's new, which also counts as seen.
 */
export function WhatsNewCard({
  onError,
  onLayoutChange,
}: {
  onError: (message: string) => void;
  /** Called after the card appears or goes away. The recorder sizes its
   * window to its content, and this card's state lives here, so the
   * recorder would otherwise not re-measure until something else rendered. */
  onLayoutChange?: () => void;
}) {
  const t = useTranslations("desktop.whatsNewCard");
  const [state, setState] = useState<WhatsNewStateDto | null>(null);
  const [later, setLater] = useState(false);
  const [clipboardHotkey, setClipboardHotkey] = useState("");

  useEffect(() => {
    let disposed = false;
    let off: (() => void) | undefined;
    getWhatsNewState()
      .then((s) => !disposed && setState(s))
      .catch(() => {});
    void onWhatsNewChanged((s) => !disposed && setState(s))
      .then((fn) => (disposed ? fn() : (off = fn)))
      .catch(() => {});
    return () => {
      disposed = true;
      off?.();
    };
  }, []);

  const td = useDesktopT();
  const news = useMemo(() => (state ? newsFor(state).map((f) => localizeFeature(f, td)) : []), [state, td]);
  const listsClipboard = news.some((f) => f.id === "clipboard");

  // The key as the user has it bound, which may not be the default.
  useEffect(() => {
    if (!state?.pending || !listsClipboard) return;
    getRecorderShortcuts()
      .then((rows) => setClipboardHotkey(rows.find((r) => r.action === "clipboard_history")?.hotkey ?? ""))
      .catch(() => {});
  }, [state?.pending, listsClipboard]);

  // An update with nothing worth a card (a fix-only release) is marked seen
  // quietly, so it never surfaces later as an empty card.
  useEffect(() => {
    if (state?.pending && news.length === 0) void acknowledgeWhatsNew().catch(() => {});
  }, [state?.pending, news.length]);

  const visible = Boolean(state?.pending) && !later && news.length > 0;
  useLayoutEffect(() => {
    onLayoutChange?.();
  }, [visible, news.length, onLayoutChange]);

  if (!state || !visible) return null;

  const dismiss = () => void acknowledgeWhatsNew().catch((err) => onError(String(err)));
  const openWindow = () => void showWhatsNewWindow().catch((err) => onError(String(err)));
  const extra = news.length - LISTED;

  return (
    <section
      aria-label={t("updatedTo", { version: state.currentVersion })}
      className="clipy-fade-in relative mx-3.5 mb-2.5 mt-1 rounded-2xl bg-gradient-to-b from-flash-500/[0.09] to-flash-500/[0.035] px-3.5 pb-3 pt-3 shadow-[inset_0_0_0_1px_rgba(255,77,41,0.22)]"
    >
      <button
        type="button"
        onClick={dismiss}
        aria-label={t("dismissLabel")}
        title={t("dismiss")}
        className="absolute right-2 top-2 flex h-6 w-6 items-center justify-center rounded-md text-ink-muted hover:bg-surface/70 hover:text-ink-text"
      >
        <svg width="12" height="12" viewBox="0 0 12 12" aria-hidden>
          <path d="M3 3l6 6M9 3L3 9" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" />
        </svg>
      </button>
      <p className="text-[10.5px] font-bold uppercase tracking-[0.12em] text-flash-600">{t("updatedTo", { version: state.currentVersion })}</p>
      <p className="mt-0.5 text-[15px] font-semibold tracking-[-0.01em] text-ink-text">
        {t("newThings", { count: news.length })}
      </p>
      <ul className="mb-2.5 mt-2 flex flex-col gap-1.5">
        {news.slice(0, LISTED).map((f) => (
          <li key={f.id} className="flex items-center gap-2 text-[12.5px] text-ink-secondary">
            <span aria-hidden className="h-[5px] w-[5px] shrink-0 rounded-full bg-flash-500" />
            <span className="truncate">{f.title}</span>
            {f.id === "clipboard" && clipboardHotkey && (
              <span className="inline-flex shrink-0 gap-0.5">
                {hotkeyKeycaps(clipboardHotkey).map((k, i) => (
                  <kbd
                    key={`${k}-${i}`}
                    className="inline-flex h-[18px] min-w-[18px] items-center justify-center rounded-[5px] bg-surface px-1 font-sans text-[10.5px] font-semibold text-ink-text shadow-[inset_0_0_0_1px_rgba(10,13,22,0.14)]"
                  >
                    {k}
                  </kbd>
                ))}
              </span>
            )}
          </li>
        ))}
        {extra > 0 && <li className="pl-[13px] text-[12px] text-ink-muted">{t("more", { count: extra })}</li>}
      </ul>
      <div className="flex items-center gap-2.5">
        <button
          type="button"
          onClick={openWindow}
          className="rounded-[10px] bg-ink-text px-3 py-1.5 text-[12.5px] font-semibold text-canvas hover:bg-ink-text/85"
        >
          {t("tryIt", { count: news.length })}
        </button>
        <button
          type="button"
          onClick={() => {
            setLater(true);
            void snoozeWhatsNew().catch(() => {});
          }}
          className="rounded-md px-1.5 py-1 text-[12.5px] font-medium text-ink-secondary hover:text-ink-text"
        >
          {t("later")}
        </button>
      </div>
    </section>
  );
}
