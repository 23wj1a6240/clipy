import { useTranslations } from "use-intl";
import type { DisplayInfoDto } from "../../lib/bridge";
import { ScreenHeader } from "./ui";

/** `undefined` = still loading, `null` = capture failed, string = data URL. */
export type DisplayThumbnails = Record<string, string | null | undefined>;

export function displayThumbnailKey(displayId: number): string {
  return `display:${displayId}`;
}

/**
 * The "Select desktop" sheet: one live thumbnail per display, the chosen
 * one outlined. Used both to pick the Full screen source and, for Custom
 * size, to pick which display the drawing overlay opens on.
 */
export function SelectDesktopSheet({
  displays,
  thumbnails,
  selectedId,
  onBack,
  onSelect,
}: {
  displays: DisplayInfoDto[];
  thumbnails: DisplayThumbnails;
  selectedId: number | null;
  onBack: () => void;
  onSelect: (displayId: number) => void;
}) {
  const t = useTranslations("desktop.selectDesktop");
  return (
    <div className="clipy-sheet">
      <ScreenHeader title={t("title")} onBack={onBack} />
      <div className="clipy-scroll px-4 pb-4 pt-1">
        <div className="grid grid-cols-2 gap-x-3 gap-y-4">
          {displays.map((display) => {
            const selected = display.id === selectedId;
            const thumbnail = thumbnails[displayThumbnailKey(display.id)];
            return (
              <button
                key={display.id}
                type="button"
                aria-pressed={selected}
                onClick={() => onSelect(display.id)}
                className="group flex flex-col items-center gap-2 text-center"
              >
                <span
                  className={`flex aspect-[16/10] w-full items-center justify-center overflow-hidden rounded-lg border-2 bg-row transition-colors ${
                    selected ? "border-sheet-link" : "border-subtle group-hover:border-strong"
                  }`}
                >
                  {thumbnail ? (
                    <img src={thumbnail} alt="" className="h-full w-full object-cover" draggable={false} />
                  ) : thumbnail === null ? (
                    <span className="text-[11px] text-ink-secondary">{t("noPreview")}</span>
                  ) : (
                    <span className="h-2.5 w-2.5 animate-pulse rounded-full bg-stone-400" />
                  )}
                </span>
                <span
                  className={`text-[13px] leading-tight ${
                    selected ? "font-semibold text-sheet-link" : "text-ink-secondary group-hover:text-ink-text"
                  }`}
                >
                  {display.name}
                </span>
              </button>
            );
          })}
        </div>
      </div>
    </div>
  );
}
