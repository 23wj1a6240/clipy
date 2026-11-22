import { useEffect, useRef, useState } from "react";
import { useTranslations } from "use-intl";
import { auroraBackground, dunesBackground, studioBackground } from "../../assets/backgrounds";
import {
  DEFAULT_CAMERA_EFFECTS,
  cameraFrameColors,
  getCameraEffects,
  importCameraBackground,
  onCameraEffectsChanged,
  onCameraEffectsError,
  setCameraEffects,
  type CameraEffects,
} from "../../lib/cameraEffects";
import { isMacOS, isWindows } from "../../lib/platform";
import type { CameraLookDto, WebcamLayoutDto, WebcamShapeDto, WebcamSizeDto } from "../../lib/bridge";
import { CameraLookRows } from "./CameraLookRows";
import { CameraViewRows } from "./CameraViewRows";
import { Icon, Notice, ScreenHeader, SettingRow, Switch, Tabs } from "./ui";

type EffectsTab = "backgrounds" | "frames" | "camera";

/**
 * The camera Effects flow, in the panel instead of on the camera bubble.
 *
 * Backgrounds map onto the native camera pipeline: "None" is the original
 * feed, "Blur" blurs what is behind you, "Clear" cuts you out over the
 * screen, and every colour or picture is a cut-out over that canvas. Frames
 * decorate the bubble. Clearing the background clears the frame with it —
 * a frame sits on the cut-out, so it has nothing to sit on once that is gone.
 */
/** The colour mats, left to right, top to bottom. */
const SOLID_COLORS = [
  "#d6e2f7", "#2f6bd0", "#f7d8d6", "#c53a25", "#3e7f5a",
  "#26282d", "#c9c7f2", "#6558ea", "#f6e4b3", "#eea63a",
  "#b8bac1",
] as const;

const backgroundsSupported = isMacOS || isWindows;
/** Largest custom background image, in MB. */
const MAX_BACKGROUND_MB = 8;

function hexToRgb(hex: string): [number, number, number] {
  const n = parseInt(hex.slice(1), 16);
  return [(n >> 16) & 0xff, (n >> 8) & 0xff, n & 0xff];
}

function rgbToHex([r, g, b]: [number, number, number]): string {
  return `#${[r, g, b].map((v) => v.toString(16).padStart(2, "0")).join("")}`;
}

export function EffectsSheet({
  onBack,
  cameraOn,
  layout,
  shape,
  size,
  mirrored,
  look,
  onLayout,
  onShape,
  onSize,
  onMirror,
  onLook,
}: {
  onBack: () => void;
  cameraOn: boolean;
  layout: WebcamLayoutDto;
  shape: WebcamShapeDto;
  size: WebcamSizeDto;
  mirrored: boolean;
  look: CameraLookDto;
  onLayout: (layout: WebcamLayoutDto) => void;
  onShape: (shape: WebcamShapeDto) => void;
  onSize: (size: WebcamSizeDto) => void;
  onMirror: (mirrored: boolean) => void;
  onLook: (look: CameraLookDto) => void;
}) {
  const t = useTranslations("desktop.effects");
  const tSettings = useTranslations("desktop.settings");
  const [tab, setTab] = useState<EffectsTab>("backgrounds");
  const [effects, setEffects] = useState<CameraEffects>(DEFAULT_CAMERA_EFFECTS);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const uploadRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    let disposed = false;
    const offs: Array<() => void> = [];
    const keep = (off: () => void) => (disposed ? off() : offs.push(off));
    getCameraEffects()
      .then((value) => {
        if (!disposed) setEffects(value);
      })
      .catch((err) => setError(String(err)));
    onCameraEffectsChanged(setEffects).then(keep);
    onCameraEffectsError(setError).then(keep);
    return () => {
      disposed = true;
      offs.forEach((off) => off());
    };
  }, []);

  const apply = async (patch: Partial<CameraEffects>) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const current = await getCameraEffects().catch(() => DEFAULT_CAMERA_EFFECTS);
      const next = { ...current, ...patch };
      // No background means no frame either.
      if (next.background === "original") next.frame = "none";
      await setCameraEffects(next);
      setEffects(next);
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
    }
  };

  const importBackground = async (file: File | undefined) => {
    if (!file || busy) return;
    setBusy(true);
    setError(null);
    try {
      if (file.size > MAX_BACKGROUND_MB * 1024 * 1024 || !["image/png", "image/jpeg"].includes(file.type)) {
        throw new Error(t("imageTooLarge", { mb: MAX_BACKGROUND_MB }));
      }
      await importCameraBackground(Array.from(new Uint8Array(await file.arrayBuffer())));
      const current = await getCameraEffects().catch(() => DEFAULT_CAMERA_EFFECTS);
      await setCameraEffects({ ...current, background: "remove", canvas: "custom" });
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(false);
      if (uploadRef.current) uploadRef.current.value = "";
    }
  };

  const hasBackground = effects.background !== "original";
  const backgroundValue: string =
    effects.background === "original"
      ? "none"
      : effects.background === "blur"
        ? "blur"
        : effects.canvas === "transparent"
          ? "clear"
          : effects.canvas === "solid"
            ? `solid:${rgbToHex(effects.canvasRgb ?? [0, 0, 0])}`
            : effects.canvas;

  const topPicks: Array<{ value: string; label: string; style?: React.CSSProperties; text?: string; patch: Partial<CameraEffects> }> = [
    { value: "none", label: t("backgrounds.none"), text: t("backgrounds.none"), patch: { background: "original", canvas: "transparent" } },
    { value: "blur", label: t("backgrounds.blur"), text: t("backgrounds.blur"), style: { background: "radial-gradient(circle at 35% 35%, #7b8192, #2b2f3a)" }, patch: { background: "blur" } },
    { value: "clear", label: t("backgrounds.clear"), text: t("backgrounds.clear"), style: { background: "repeating-conic-gradient(#e6e2da 0 25%, #ffffff 0 50%) 50% / 14px 14px" }, patch: { background: "remove", canvas: "transparent" } },
    { value: "sunset", label: t("backgrounds.sunset"), style: { background: "linear-gradient(135deg,#2e8bd6 0%,#f2a541 55%,#f25c54 100%)" }, patch: { background: "remove", canvas: "sunset" } },
    { value: "aurora", label: t("backgrounds.aurora"), style: { backgroundImage: `url(${auroraBackground})`, backgroundSize: "cover" }, patch: { background: "remove", canvas: "aurora" } },
    { value: "dunes", label: t("backgrounds.dunes"), style: { backgroundImage: `url(${dunesBackground})`, backgroundSize: "cover" }, patch: { background: "remove", canvas: "dunes" } },
    { value: "studio", label: t("backgrounds.studio"), style: { backgroundImage: `url(${studioBackground})`, backgroundSize: "cover" }, patch: { background: "remove", canvas: "studio" } },
  ];
  const colors: Array<{ value: string; label: string; style: React.CSSProperties; patch: Partial<CameraEffects> }> = [
    { value: "ink", label: t("backgrounds.ink"), style: { background: "#25262b" }, patch: { background: "remove", canvas: "ink" } },
    { value: "cream", label: t("backgrounds.cream"), style: { background: "#f3ede3" }, patch: { background: "remove", canvas: "cream" } },
    ...SOLID_COLORS.map((hex) => ({
      value: `solid:${hex}`,
      label: hex,
      style: { background: hex },
      patch: { background: "remove" as const, canvas: "solid" as const, canvasRgb: hexToRgb(hex) },
    })),
  ];

  const frames: Array<{ value: CameraEffects["frame"]; label: string; group: "top" | "work" }> = (
    [
      ["none", "top"],
      ["coral", "top"],
      ["white", "top"],
      ["amber", "top"],
      ["geometric", "work"],
      ["confetti", "work"],
      ["botanical", "work"],
    ] as const
  ).map(([value, group]) => ({ value, label: t(`frames.${value}`), group }));

  return (
    <div className="clipy-sheet">
      <ScreenHeader title={t("title")} onBack={onBack} />
      <Tabs
        tabs={[
          { value: "backgrounds", label: t("tabs.backgrounds") },
          { value: "frames", label: t("tabs.frames") },
          { value: "camera", label: t("tabs.camera") },
        ]}
        value={tab}
        onChange={setTab}
      />
      <div className="clipy-scroll">
        {!cameraOn && tab !== "camera" && (
          <p className="px-5 pt-3 text-[12px] leading-snug text-ink-secondary">
            {t("cameraOff")}
          </p>
        )}
        {error && (
          <div className="px-5 pt-3">
            <Notice tone="error">{error}</Notice>
          </div>
        )}

        {tab === "backgrounds" && (
          <div key="backgrounds" className="clipy-fade-in px-5 pb-5">
            {!backgroundsSupported && (
              <p className="pt-3 text-[12px] text-ink-secondary">{t("unsupported")}</p>
            )}
            <GroupTitle>{t("topPicks")}</GroupTitle>
            <SwatchGrid>
              {topPicks.map((pick) => (
                <Swatch
                  key={pick.value}
                  label={pick.label}
                  selected={backgroundValue === pick.value}
                  disabled={busy || (!backgroundsSupported && pick.value !== "none")}
                  onClick={() => void apply(pick.patch)}
                  style={pick.style}
                  text={pick.text}
                />
              ))}
              <button
                type="button"
                disabled={busy || !backgroundsSupported}
                onClick={() => uploadRef.current?.click()}
                aria-label={t("addImage")}
                title={t("addImageHint", { mb: MAX_BACKGROUND_MB })}
                className={`mx-auto flex h-[58px] w-[58px] items-center justify-center rounded-full border-2 border-dashed border-strong text-ink-secondary transition hover:border-ink-secondary hover:text-ink-text disabled:opacity-40 ${
                  effects.background === "remove" && effects.canvas === "custom" ? "ring-[3px] ring-[#2f5fd0] ring-offset-2 ring-offset-surface" : ""
                }`}
              >
                <Icon name="close" size={22} className="rotate-45" />
              </button>
              <input
                ref={uploadRef}
                type="file"
                accept="image/png,image/jpeg"
                className="hidden"
                aria-label={t("customImage")}
                onChange={(event) => void importBackground(event.target.files?.[0])}
              />
            </SwatchGrid>
            <GroupTitle>{t("colors")}</GroupTitle>
            <SwatchGrid>
              {colors.map((c) => (
                <Swatch
                  key={c.value}
                  label={c.label}
                  selected={backgroundValue === c.value}
                  disabled={busy || !backgroundsSupported}
                  onClick={() => void apply(c.patch)}
                  style={c.style}
                />
              ))}
            </SwatchGrid>
            <p className="mt-3 text-[11px] leading-snug text-ink-secondary">
              {t("cutOutNote", { device: isWindows ? "PC" : "Mac" })}
            </p>
          </div>
        )}

        {tab === "frames" && (
          <div key="frames" className="clipy-fade-in px-5 pb-5">
            {!hasBackground && (
              <p className="mb-2 mt-3 rounded-xl bg-row px-3 py-2 text-[12px] text-ink-secondary">
                {t("pickBackgroundFirst")}
              </p>
            )}
            <GroupTitle>{t("topPicks")}</GroupTitle>
            <SwatchGrid>
              {frames.filter((f) => f.group === "top").map((f) => (
                <FrameSwatch key={f.value} frame={f.value} label={f.label} selected={effects.frame === f.value} disabled={busy || (!hasBackground && f.value !== "none")} onClick={() => void apply({ frame: f.value })} />
              ))}
            </SwatchGrid>
            <GroupTitle>{t("popularAtWork")}</GroupTitle>
            <SwatchGrid>
              {frames.filter((f) => f.group === "work").map((f) => (
                <FrameSwatch key={f.value} frame={f.value} label={f.label} selected={effects.frame === f.value} disabled={busy || !hasBackground} onClick={() => void apply({ frame: f.value })} />
              ))}
            </SwatchGrid>
          </div>
        )}

        {tab === "camera" && (
          <div key="camera" className="clipy-fade-in pb-4">
            <CameraViewRows layout={layout} shape={shape} size={size} onLayout={onLayout} onShape={onShape} onSize={onSize} />
            <CameraLookRows look={look} onChange={onLook} />
            <SettingRow
              title={tSettings("flip.title")}
              subtitle={tSettings("flip.subtitle")}
              trailing={<Switch checked={mirrored} onChange={onMirror} label={tSettings("flip.title")} />}
            />
          </div>
        )}
      </div>
    </div>
  );
}

function GroupTitle({ children }: { children: React.ReactNode }) {
  return <div className="pb-2 pt-3 text-[17px] font-bold text-ink-text">{children}</div>;
}

function SwatchGrid({ children }: { children: React.ReactNode }) {
  return <div className="grid grid-cols-4 gap-x-2 gap-y-3">{children}</div>;
}

function Swatch({
  label,
  selected,
  disabled,
  onClick,
  style,
  text,
}: {
  label: string;
  selected: boolean;
  disabled?: boolean;
  onClick: () => void;
  style?: React.CSSProperties;
  text?: string;
}) {
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={selected}
      title={label}
      disabled={disabled}
      onClick={onClick}
      className={`mx-auto flex h-[58px] w-[58px] items-center justify-center rounded-full text-[13px] font-medium text-ink-text transition-all duration-150 disabled:opacity-40 ${
        selected ? "scale-105 ring-[3px] ring-[#2f5fd0] ring-offset-2 ring-offset-surface" : "hover:scale-105"
      } ${text && !style ? "bg-row" : ""}`}
      style={style}
    >
      {text}
    </button>
  );
}

function FrameSwatch({
  frame,
  label,
  selected,
  disabled,
  onClick,
}: {
  frame: CameraEffects["frame"];
  label: string;
  selected: boolean;
  disabled: boolean;
  onClick: () => void;
}) {
  const color = frame === "none" ? "#d0cabe" : cameraFrameColors[frame];
  const dashed = frame === "geometric";
  const dotted = frame === "confetti";
  return (
    <button
      type="button"
      aria-label={label}
      aria-pressed={selected}
      disabled={disabled}
      onClick={onClick}
      className={`flex flex-col items-center gap-1 transition-all duration-150 disabled:opacity-40 ${disabled ? "" : "hover:scale-105"}`}
    >
      <span
        className={`flex h-[58px] w-[58px] items-center justify-center rounded-full bg-row text-ink-secondary ${
          selected ? "ring-[3px] ring-[#2f5fd0] ring-offset-2 ring-offset-surface" : ""
        }`}
        style={{ border: `3px ${dotted ? "dotted" : dashed ? "dashed" : "solid"} ${color}` }}
      >
        {frame === "none" ? <span className="text-[13px] font-medium text-ink-text">None</span> : frame === "botanical" ? <span className="text-[20px]">❧</span> : <Icon name="smile" size={26} />}
      </span>
      {frame !== "none" && <span className="text-[12px] text-ink-text">{label}</span>}
    </button>
  );
}
