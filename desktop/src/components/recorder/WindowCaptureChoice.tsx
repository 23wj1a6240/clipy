import type { KeyboardEvent } from "react";
import { useTranslations } from "use-intl";
import type { WindowCaptureModeDto } from "../../lib/bridge";
import { Icon } from "./ui";

/**
 * How "Specific window" records, as two cards rather than a dropdown: the
 * choice is a trade (privacy and focus against Camera and Flare), and each
 * side has to be visible to be chosen on purpose.
 */
type Chip = "windowAnywhere" | "otherWindows" | "popups" | "camera" | "flare" | "outsideArea";

/** Words come from `desktop.windowCapture.<value>` and `.chips`. */
const OPTIONS: Array<{
  value: WindowCaptureModeDto;
  badge?: boolean;
  includes: Chip[];
  leavesOut: Chip[];
}> = [
  {
    value: "window",
    badge: true,
    includes: ["windowAnywhere"],
    // Menus and pop-ups that open as their own windows are not part of the
    // window's pixels, so they are left out too.
    leavesOut: ["otherWindows", "popups", "camera", "flare"],
  },
  {
    value: "area",
    includes: ["camera", "flare", "popups"],
    leavesOut: ["outsideArea"],
  },
];

export function WindowCaptureChoice({
  value,
  onChange,
}: {
  value: WindowCaptureModeDto;
  onChange: (value: WindowCaptureModeDto) => void;
}) {
  // Arrow keys move the selection, as in any radio group.
  const t = useTranslations("desktop.windowCapture");
  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    if (!["ArrowDown", "ArrowUp", "ArrowLeft", "ArrowRight"].includes(event.key)) return;
    event.preventDefault();
    const next = value === "window" ? "area" : "window";
    onChange(next);
    const target = event.currentTarget.querySelector<HTMLButtonElement>(`[data-value="${next}"]`);
    target?.focus();
  };

  return (
    <div role="radiogroup" aria-label={t("label")} className="flex flex-col gap-2" onKeyDown={onKeyDown}>
      {OPTIONS.map((option) => {
        const selected = option.value === value;
        return (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={selected}
            data-value={option.value}
            tabIndex={selected ? 0 : -1}
            onClick={() => onChange(option.value)}
            className={`flex w-full gap-3 rounded-2xl border p-3 text-left transition-colors ${
              selected ? "border-flash-500 bg-flash-500/5" : "border-subtle bg-row hover:bg-row-hover"
            }`}
          >
            {/* The diagram carries the radio mark, so the title gets the
                full width of a 340 px popover instead of wrapping. */}
            <span className="relative mt-0.5 h-[44px] w-[56px] shrink-0">
              <WindowCaptureDiagram mode={option.value} selected={selected} />
              <span
                aria-hidden
                className={`absolute -right-1.5 -top-1.5 flex h-[18px] w-[18px] items-center justify-center rounded-full border-2 border-white ${
                  selected ? "bg-flash-500 text-white" : "bg-strong"
                }`}
              >
                {selected && <Icon name="check" size={11} />}
              </span>
            </span>
            <span className="min-w-0 flex-1">
              <span className="flex flex-wrap items-center gap-x-1.5 gap-y-0.5">
                <span className="text-[15px] font-semibold leading-tight text-ink-text">{t(`${option.value}.title`)}</span>
                {option.badge && (
                  <span className="rounded-full bg-surface px-1.5 py-px text-[10px] font-semibold uppercase tracking-wide text-ink-secondary">
                    {t("default")}
                  </span>
                )}
              </span>
              <span className="mt-1 block text-[12px] leading-snug text-ink-secondary">{t(`${option.value}.body`)}</span>
              <span className="mt-2 flex flex-wrap gap-1">
                {option.includes.map((item) => (
                  <span key={item} className="rounded-full bg-status-ok-soft px-2 py-0.5 text-[11px] font-medium text-status-ok">
                    {t(`chips.${item}`)}
                  </span>
                ))}
                {option.leavesOut.map((item) => (
                  <span key={item} className="rounded-full bg-stone-500/10 px-2 py-0.5 text-[11px] font-medium text-ink-secondary line-through decoration-1">
                    {t(`chips.${item}`)}
                  </span>
                ))}
              </span>
            </span>
          </button>
        );
      })}
    </div>
  );
}

/**
 * A 56x44 sketch of what ends up in the video. "window": the recorded
 * window is solid and a window lying over it is drawn faint, because it
 * stays out. "area": a dashed frame on the screen, with the camera bubble
 * inside it.
 */
function WindowCaptureDiagram({ mode, selected }: { mode: WindowCaptureModeDto; selected: boolean }) {
  const accent = selected ? "#FF4D29" : "currentColor";
  return (
    <svg width="56" height="44" viewBox="0 0 56 44" aria-hidden className="text-ink-secondary">
      <rect x="0.5" y="0.5" width="55" height="43" rx="6" fill="none" stroke="currentColor" strokeOpacity="0.25" />
      {mode === "window" ? (
        <>
          <rect x="7" y="8" width="30" height="24" rx="3" fill={accent} fillOpacity="0.14" stroke={accent} strokeWidth="1.5" />
          <line x1="7" y1="13" x2="37" y2="13" stroke={accent} strokeWidth="1.2" />
          <rect x="27" y="18" width="22" height="18" rx="3" fill="currentColor" fillOpacity="0.06" stroke="currentColor" strokeOpacity="0.35" strokeDasharray="2 2" />
        </>
      ) : (
        <>
          <rect x="6" y="6" width="34" height="27" rx="2" fill="none" stroke={accent} strokeWidth="1.5" strokeDasharray="3 2" />
          <rect x="9" y="9" width="28" height="21" rx="2.5" fill="currentColor" fillOpacity="0.08" stroke="currentColor" strokeOpacity="0.4" />
          <circle cx="33" cy="26" r="5" fill={accent} fillOpacity="0.25" stroke={accent} strokeWidth="1.2" />
        </>
      )}
    </svg>
  );
}
