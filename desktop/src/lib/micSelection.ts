import type { AudioDeviceDto, AudioOptionsDto } from "./bridge";

/**
 * Microphone selection has two layers, the same split Cap uses: the saved
 * PREFERENCE (what the user picked, persisted verbatim and never rewritten
 * because a device happened to be unplugged) and the RESOLVED value (what
 * this recording actually uses). A preference for a mic that is currently
 * missing resolves to the system default for that recording only, and the
 * mic is selected again the moment it is back.
 */

export function defaultAudioDeviceId(devices: AudioDeviceDto[]): string | null {
  return devices.find((device) => device.is_default)?.id ?? devices[0]?.id ?? null;
}

export function micDeviceConnected(
  options: AudioOptionsDto,
  devices: AudioDeviceDto[],
): boolean {
  return (
    !!options.micDeviceId && devices.some((device) => device.id === options.micDeviceId)
  );
}

/** Options for a recording: the preferred mic when connected, else system
 * default. Never persist the result — it would erase the preference. */
export function resolveAudioOptions(
  options: AudioOptionsDto,
  devices: AudioDeviceDto[],
): AudioOptionsDto {
  if (!options.includeMic) return { ...options, micDeviceId: null };
  if (micDeviceConnected(options, devices)) return options;
  return { ...options, micDeviceId: null };
}

/** The preferred mic is turned on, saved, and not currently connected. */
export function disconnectedMicDeviceId(
  options: AudioOptionsDto,
  devices: AudioDeviceDto[],
): string | null {
  if (!options.includeMic || !options.micDeviceId) return null;
  return micDeviceConnected(options, devices) ? null : options.micDeviceId;
}

/** Concrete device to open for a signal check (the default's real id, so
 * the check names the device it actually listened to). */
export function micDeviceIdForCheck(
  options: AudioOptionsDto,
  devices: AudioDeviceDto[],
): string | null {
  if (!options.includeMic) return null;
  if (micDeviceConnected(options, devices)) return options.micDeviceId;
  return defaultAudioDeviceId(devices);
}
