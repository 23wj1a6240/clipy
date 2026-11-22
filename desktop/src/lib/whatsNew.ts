import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";
import { hostPlatform } from "./platform";
import { englishT, type DesktopT } from "./messages";
import { LOCAL_EDITION } from "./edition";
import { localEditionFeatures } from "../edition-local/whatsNewLocal";

/**
 * Everything the What's new window and the recorder's update card know.
 *
 * This catalog is the one source for both the release notes ("new since your
 * last version") and the checklist of things to try. A feature is new when
 * the version it shipped in is newer than the one the user updated from. Add
 * a feature here in the same change that ships it, with `addedIn` set to the
 * version in desktop/src-tauri/tauri.conf.json at merge time. Do not guess
 * the release number: betas use numbers ahead of stable (Mac beta 0.1.76 and
 * Windows beta 0.1.78 while stable was 0.1.74), so the stable that ships it
 * may be several numbers higher. The tauri.conf version is never above that
 * release and always above the previous stable, which is all "new" needs.
 *
 * The ids are mirrored in `src-tauri/src/whats_new.rs` (FEATURE_IDS), which
 * refuses any other id; a test keeps the two lists equal.
 */

export const FEATURE_IDS = [
  "flare",
  "pointer",
  "draw",
  "clipboard",
  "screenshot_markup",
  "screenshot_style",
  "window_recording",
  "custom_size",
  "local_ai",
  "agent",
] as const;

export type FeatureId = (typeof FEATURE_IDS)[number];
export type FeaturePlatform = "macos" | "windows";

export interface Feature {
  id: FeatureId;
  /** Heading on the card and the practice page. */
  title: string;
  /** Short label in the checklist. */
  label: string;
  /** One sentence, per platform where the platforms differ. */
  blurb: Record<FeaturePlatform, string>;
  /** The release it first shipped in, per platform. A platform that is
   * missing does not have the feature, and it is never listed there. */
  addedIn: Partial<Record<FeaturePlatform, string>>;
}

/** A feature's words, from `desktop.whatsNew.features.<id>` in the catalog:
 *  `blurb` when both platforms read the same, else `blurbMac` and
 *  `blurbWindows`. */
function featureWords(id: FeatureId, t: DesktopT): Pick<Feature, "title" | "label" | "blurb"> {
  const base = `whatsNew.features.${id}`;
  const shared = PLATFORM_BLURBS.has(id) ? null : t(`${base}.blurb`);
  return {
    title: t(`${base}.title`),
    label: t(`${base}.label`),
    blurb: {
      macos: shared ?? t(`${base}.blurbMac`),
      windows: shared ?? t(`${base}.blurbWindows`),
    },
  };
}

/** Features whose sentence differs per platform. */
const PLATFORM_BLURBS = new Set<FeatureId>(["clipboard", "window_recording", "local_ai"]);

/** Order, ids and release versions. The words are in the catalog. */
const FEATURE_RELEASES: ReadonlyArray<Pick<Feature, "id" | "addedIn">> = [
  { id: "clipboard", addedIn: { macos: "0.1.75", windows: "0.1.75" } },
  { id: "window_recording", addedIn: { macos: "0.1.74", windows: "0.1.64" } },
  // Windows 1.1.77 shipped before this merged, so Windows dates it to the
  // release that first carries it (1.1.82); Windows users updating from
  // 1.1.77 see it as new.
  { id: "pointer", addedIn: { macos: "1.1.82", windows: "1.1.82" } },
  { id: "screenshot_style", addedIn: { macos: "1.1.77", windows: "1.1.82" } },
  { id: "screenshot_markup", addedIn: { macos: "0.1.72", windows: "0.1.72" } },
  { id: "flare", addedIn: { macos: "0.1.64", windows: "0.1.64" } },
  { id: "draw", addedIn: { macos: "0.1.39", windows: "0.1.64" } },
  { id: "custom_size", addedIn: { macos: "0.1.22", windows: "0.1.64" } },
  { id: "local_ai", addedIn: { macos: "0.1.64", windows: "0.1.71" } },
  { id: "agent", addedIn: { macos: "0.1.41" } },
];

/** The catalog in English (what tests and non-React callers see). */
export const FEATURES: readonly Feature[] = FEATURE_RELEASES.map((release) => ({
  ...release,
  ...featureWords(release.id, englishT),
}));

/** The same feature with its words in the window's language. */
export function localizeFeature(feature: Feature, t: DesktopT): Feature {
  const words = featureWords(feature.id, t);
  // The local edition rewrites the Local AI sentence (edition-local/whatsNewLocal.ts).
  if (LOCAL_EDITION && feature.id === "local_ai") return { ...feature, ...words, blurb: feature.blurb };
  if (LOCAL_EDITION && feature.id === "agent") {
    const blurb = t("whatsNew.features.agent.blurbLocal");
    return { ...feature, ...words, blurb: { macos: blurb, windows: blurb } };
  }
  return { ...feature, ...words };
}

/** Used when the version the user updated from is unknown (an update from a
 * release older than What's new itself): anything added after this counts
 * as new. */
export const UNKNOWN_PREVIOUS_BASELINE = "0.1.71";

export function currentFeaturePlatform(): FeaturePlatform {
  return hostPlatform === "windows" ? "windows" : "macos";
}

function versionParts(version: string): number[] {
  const core = version.trim().replace(/^v/, "").split(/[-+]/)[0] ?? "";
  return core.split(".").map((part) => {
    const n = Number.parseInt(part, 10);
    return Number.isFinite(n) ? n : 0;
  });
}

/** Negative when a is older than b. Mirrors `version_lt` in whats_new.rs. */
export function compareVersions(a: string, b: string): number {
  const x = versionParts(a);
  const y = versionParts(b);
  for (let i = 0; i < Math.max(x.length, y.length); i += 1) {
    const diff = (x[i] ?? 0) - (y[i] ?? 0);
    if (diff !== 0) return diff;
  }
  return 0;
}

export function featuresFor(platform: FeaturePlatform): Feature[] {
  if (LOCAL_EDITION) return localEditionFeatures(FEATURES, platform);
  return FEATURES.filter((f) => f.addedIn[platform] !== undefined);
}

export function featureById(id: string): Feature | undefined {
  return FEATURES.find((f) => f.id === id);
}

/** Features that shipped after `previous` and up to `current`, newest first. */
export function newFeatures(
  previous: string | null,
  current: string,
  platform: FeaturePlatform,
): Feature[] {
  const from = previous ?? UNKNOWN_PREVIOUS_BASELINE;
  return featuresFor(platform)
    .filter((f) => {
      const added = f.addedIn[platform]!;
      return compareVersions(added, from) > 0 && compareVersions(added, current) <= 0;
    })
    .sort((a, b) => compareVersions(b.addedIn[platform]!, a.addedIn[platform]!));
}

/** The `count` most recently added features, for the window when there is
 * no update to talk about. */
export function recentFeatures(current: string, platform: FeaturePlatform, count = 3): Feature[] {
  return featuresFor(platform)
    .filter((f) => compareVersions(f.addedIn[platform]!, current) <= 0)
    .sort((a, b) => compareVersions(b.addedIn[platform]!, a.addedIn[platform]!))
    .slice(0, count);
}

// ─── Bridge ──────────────────────────────────────────────────────────────────

export interface WhatsNewStateDto {
  currentVersion: string;
  /** The version this session updated from, when known. */
  previousVersion: string | null;
  /** This session is the first since an update. */
  updated: boolean;
  /** The update's news has not been seen yet. */
  pending: boolean;
  tried: string[];
}

/** What the update card and the window list: the update's new features, or
 * nothing when this session did not follow an update. */
export function newsFor(state: WhatsNewStateDto, platform = currentFeaturePlatform()): Feature[] {
  if (!state.updated) return [];
  return newFeatures(state.previousVersion, state.currentVersion, platform);
}

export function getWhatsNewState(): Promise<WhatsNewStateDto> {
  return invoke<WhatsNewStateDto>("get_whats_new_state");
}

export function acknowledgeWhatsNew(): Promise<void> {
  return invoke<void>("acknowledge_whats_new");
}

/** The card's "Later": hidden until the next launch, not marked as seen. */
export function snoozeWhatsNew(): Promise<void> {
  return invoke<void>("snooze_whats_new");
}

export function markFeatureTried(feature: FeatureId): Promise<void> {
  return invoke<void>("mark_feature_tried", { feature });
}

export function showWhatsNewWindow(feature?: FeatureId): Promise<void> {
  return invoke<void>("show_whats_new_window", { feature: feature ?? null });
}

export function showRecorderFromWhatsNew(): Promise<void> {
  return invoke<void>("show_recorder_from_whats_new");
}

export function onWhatsNewChanged(handler: (state: WhatsNewStateDto) => void): Promise<UnlistenFn> {
  return listen<WhatsNewStateDto>("whats-new-changed", ({ payload }) => handler(payload));
}

export function onWhatsNewFocus(handler: (feature: string) => void): Promise<UnlistenFn> {
  return listen<string>("whats-new-focus", ({ payload }) => handler(payload));
}
