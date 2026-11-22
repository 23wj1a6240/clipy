import { useCallback, useEffect, useState, type PointerEvent } from "react";
import { useTranslations } from "use-intl";
import type { WindowInfoDto } from "../lib/bridge";
import { completeWindowPicker, getWindowPickerSnapshot, windowAtPoint, windowPickerRect, type WindowPickerSnapshot } from "../lib/windowPicker";

/** Window picker: nothing is dimmed, the window under the pointer
 *  gets a tinted frame with a "Select <app>" button in its middle, and a
 *  click anywhere on it takes it. The popover stays up beside this with
 *  the "Select window" sheet, so the only chrome here is a small hint for
 *  displays that do not have the popover on them. */
export default function WindowSelect() {
  const t = useTranslations("desktop.windowSelect");
  const [snapshot, setSnapshot] = useState<WindowPickerSnapshot | null>(null);
  const [hovered, setHovered] = useState<WindowInfoDto | null>(null);
  const [error, setError] = useState<string | null>(null);
  const finish = useCallback((id: number | null) => {
    completeWindowPicker(id).catch((failure) => setError(String(failure)));
  }, []);

  useEffect(() => {
    let disposed = false;
    getWindowPickerSnapshot().then((value) => { if (!disposed) setSnapshot(value); })
      .catch((failure) => { if (!disposed) setError(String(failure)); });
    const key = (event: KeyboardEvent) => {
      if (event.key === "Escape") { event.preventDefault(); finish(null); }
    };
    window.addEventListener("keydown", key);
    return () => { disposed = true; window.removeEventListener("keydown", key); };
  }, [finish]);

  const target = (event: PointerEvent<HTMLDivElement>) => {
    if (!snapshot) return null;
    return windowAtPoint(snapshot.windows,
      snapshot.display.x + event.clientX * snapshot.display.width / window.innerWidth,
      snapshot.display.y + event.clientY * snapshot.display.height / window.innerHeight);
  };

  return <div className={`fixed inset-0 select-none overflow-hidden ${hovered ? "cursor-pointer" : "cursor-default"}`}
    onPointerMove={(event) => setHovered(target(event))}
    onPointerLeave={() => setHovered(null)}
    onPointerDown={(event) => {
      if (event.button !== 0) return;
      event.preventDefault();
      const selected = target(event);
      if (selected) finish(selected.id);
    }}>
    {hovered && snapshot && (
      <div data-testid="window-select-target" aria-hidden
        className="pointer-events-none absolute flex items-center justify-center rounded-lg border-[3px] border-dashed border-flash-500 bg-flash-500/15"
        style={windowPickerRect(hovered, snapshot.display, window.innerWidth, window.innerHeight)}>
        <span className="max-w-[80%] truncate rounded-xl bg-flash-500 px-5 py-2.5 text-[15px] font-semibold text-white shadow-modal">
          {t("select", { app: hovered.app_name })}
        </span>
      </div>
    )}
    {(error || !hovered) && (
      <div className="pointer-events-none absolute left-1/2 top-8 -translate-x-1/2 rounded-full bg-ink-950/90 px-4 py-2 text-[13px] font-medium text-stone-100 shadow-lg">
        {error ?? t("hint")}
      </div>
    )}
  </div>;
}
