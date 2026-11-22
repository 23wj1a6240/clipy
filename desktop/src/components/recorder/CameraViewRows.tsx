import { useTranslations } from "use-intl";
import type { WebcamLayoutDto, WebcamShapeDto, WebcamSizeDto } from "../../lib/bridge";
import { CAMERA_VIEW_ITEMS, applyCameraView, cameraViewOf, type CameraView } from "../../lib/cameraView";
import { InlineSelect, SettingRow } from "./ui";

/** The camera view and size rows, shared by Effects and Settings so the two
 *  never offer different choices. */
export function CameraViewRows({
  layout,
  shape,
  size,
  onLayout,
  onShape,
  onSize,
}: {
  layout: WebcamLayoutDto;
  shape: WebcamShapeDto;
  size: WebcamSizeDto;
  onLayout: (layout: WebcamLayoutDto) => void;
  onShape: (shape: WebcamShapeDto) => void;
  onSize: (size: WebcamSizeDto) => void;
}) {
  const t = useTranslations("desktop.camera");
  const choose = (view: CameraView) => {
    const next = applyCameraView(view);
    onLayout(next.layout);
    if (next.shape) onShape(next.shape);
  };
  return (
    <>
      <SettingRow
        title={t("view.title")}
        subtitle={t("view.subtitle")}
        trailing={
          <InlineSelect<CameraView>
            ariaLabel={t("view.title")}
            value={cameraViewOf(layout, shape)}
            onChange={choose}
            items={CAMERA_VIEW_ITEMS.map((item) => ({ value: item.value, label: t(`view.${item.value}`) }))}
          />
        }
      />
      <SettingRow
        title={t("size.title")}
        trailing={
          <InlineSelect<WebcamSizeDto>
            ariaLabel={t("size.title")}
            value={size}
            onChange={onSize}
            items={[
              { value: "small", label: t("size.small") },
              { value: "medium", label: t("size.medium") },
              { value: "large", label: t("size.large") },
            ]}
          />
        }
      />
    </>
  );
}
