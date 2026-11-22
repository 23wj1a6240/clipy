import type { CSSProperties } from "react";
import { useTranslations } from "use-intl";
import type { CameraLookDto } from "../../lib/bridge";
import { clampStrength } from "../../lib/cameraLook";
import { SettingRow, Switch } from "./ui";

/** The "Touch up my appearance" (with its strength slider) and "Auto
 *  lighting" rows, shared by Effects and Settings. */
export function CameraLookRows({ look, onChange }: { look: CameraLookDto; onChange: (look: CameraLookDto) => void }) {
  const t = useTranslations("desktop.camera");
  const strength = Math.round(clampStrength(look.touchUpStrength) * 100);
  return (
    <>
      <SettingRow
        icon="smile"
        title={t("touchUp.title")}
        subtitle={t("touchUp.subtitle")}
        trailing={
          <Switch
            checked={look.touchUpEnabled}
            onChange={(v) => onChange({ ...look, touchUpEnabled: v })}
            label={t("touchUp.title")}
          />
        }
      >
        {look.touchUpEnabled && (
          <input
            type="range"
            min={0}
            max={100}
            step={5}
            value={strength}
            aria-label={t("touchUp.strength")}
            onChange={(e) => onChange({ ...look, touchUpStrength: Number(e.target.value) / 100 })}
            className="clipy-range w-full"
            style={{ "--fill": `${strength}%` } as CSSProperties}
          />
        )}
      </SettingRow>
      <SettingRow
        icon="bulb"
        title={t("autoLighting.title")}
        subtitle={t("autoLighting.subtitle")}
        trailing={
          <Switch
            checked={look.autoLighting}
            onChange={(v) => onChange({ ...look, autoLighting: v })}
            label={t("autoLighting.title")}
          />
        }
      />
    </>
  );
}
