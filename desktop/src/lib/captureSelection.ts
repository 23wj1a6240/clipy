import type { CaptureSourceDto, CaptureSourcesDto } from "./bridge";

type SourceTab = Exclude<CaptureSourceDto["type"], "camera">;

export function sourceMatchesTab(source: CaptureSourceDto | null, tab: SourceTab): source is CaptureSourceDto {
  return source !== null && source.type === tab;
}

export function fallbackSourceForTab(tab: SourceTab, sources: CaptureSourcesDto): CaptureSourceDto | null {
  // An area must always come from an explicit selection. Missing windows must
  // never silently expand the recording to the whole display.
  if (tab === "area") return null;
  const candidates = tab === "window" ? sources.windows : sources.displays;
  return candidates[0] ? { type: tab, id: candidates[0].id } : null;
}

type DisplayInfo = CaptureSourcesDto["displays"][number];

/** Identifies a monitor setup: which displays are connected, where, and at
 *  what size. Saved with the source so it is restored only on the same setup. */
export function displayLayoutKey(displays: readonly DisplayInfo[]): string {
  return [...displays]
    .sort((a, b) => a.id - b.id)
    .map((d) => `${d.id}@${Math.round(d.x)},${Math.round(d.y)},${Math.round(d.width)}x${Math.round(d.height)}`)
    .join("|");
}

/** Settings saved before the layout was recorded have none; their display
 *  is still restored when it exists, as it was before. */
export function savedDisplayLayoutMatches(
  saved: string | null | undefined,
  displays: readonly DisplayInfo[],
): boolean {
  return !saved || saved === displayLayoutKey(displays);
}
