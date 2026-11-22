import type { WebcamDeviceDto } from "./bridge";

export function preferredWebcamDeviceId(
  devices: WebcamDeviceDto[],
  currentDeviceId: string | null,
): string | null {
  if (currentDeviceId && devices.some((device) => device.id === currentDeviceId)) {
    return currentDeviceId;
  }
  return devices.find((device) => !/obs/i.test(device.name))?.id ?? devices[0]?.id ?? null;
}
