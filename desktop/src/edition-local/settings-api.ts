/**
 * Local edition settings: the model provider that picks spoken cues, how a
 * recording is transcribed, and the downloadable transcription models.
 *
 * The Rust side implements these five commands and one event in the local
 * edition only (`#[cfg(not(feature = "cloud"))]`). This header is the
 * contract; keep the two in step.
 *
 * ## Commands
 *
 * All payloads are JSON. Struct fields are camelCase on the wire
 * (`#[serde(rename_all = "camelCase")]`); the enum string values are exactly
 * the ones in the TypeScript unions below. Errors reject with a string the UI
 * shows as is, so it must never contain a key.
 *
 * 1. `get_local_settings` (no arguments) -> `LocalSettingsDto`
 *
 *    Never returns a key, only whether one is stored (`hasApiKey`).
 *    Missing settings read as the defaults: model preset `"none"` (rules
 *    only); transcription `"apple"` on macOS, `"local"` with
 *    `localModel: "english-fast"` on Windows.
 *
 * 2. `save_local_settings` with `{ settings: SaveLocalSettingsRequest }`
 *    -> `LocalSettingsDto` (the stored result, read back)
 *
 *    `model` and `transcription` are each optional: an absent section is
 *    left unchanged. Each key field is a `KeyUpdate`:
 *    `{ "action": "keep" }` keeps the stored key, `{ "action": "clear" }`
 *    removes it, `{ "action": "set", "value": "..." }` replaces it.
 *    (Rust: `#[serde(tag = "action", rename_all = "lowercase")]`.)
 *    Keys go to the owner-only key file through `keychain.rs`, never into
 *    `settings.json`; the preset, model and base URL go to `settings.json`.
 *    The model key is the stored preset's: the UI sends `clear` (or `set`)
 *    whenever the preset changes, so a key is never sent to another provider.
 *    A transcription key belongs to its kind: switching kind and sending
 *    `keep` keeps nothing for the new kind unless one was stored for it.
 *    Validation (reject with a message):
 *    - `model.preset` `"custom"` needs `baseUrl` and `model`;
 *      `"ollama"` and `"lmstudio"` need `model`; `baseUrl`, when set, must be
 *      an http:// or https:// URL.
 *    - `transcription.kind` `"apple"` is macOS only; `"local"` is Windows
 *      only and needs `localModel`.
 *
 * 3. `local_model_status` (no arguments) -> `LocalModelStatusDto[]`
 *
 *    One row per downloadable model (`"english-fast"`, `"multilingual"`),
 *    whatever the platform; the UI shows them on Windows only.
 *
 * 4. `download_local_model` with `{ model: LocalModelId }` -> `void`
 *
 *    Resolves once every file is downloaded, checked against its pinned
 *    SHA-256 and moved into place; rejects with a message otherwise. A second
 *    call for a model already downloading joins it (resolves or rejects with
 *    it) rather than starting another download. Progress arrives as events.
 *
 * 5. `cancel_local_model_download` with `{ model: LocalModelId }` -> `void`
 *
 *    Stops a running download; nothing is installed and its
 *    `download_local_model` call rejects with "The download was cancelled."
 *    A no-op when nothing is downloading (and always on macOS).
 *
 * The model key is stored together with the preset and URL it was saved for
 * and is only used for that pair: a save that changes either with `keep`
 * drops it, so `hasApiKey` reads false until a key is set again.
 *
 * ## Event
 *
 * `local-model-progress`, payload `LocalModelProgressDto`, emitted to every
 * window while a download runs (at most a few times a second), then once
 * with `stage` `"installed"` or `"failed"`.
 *
 * ## Privacy (what the UI tells the user, and what Rust must make true)
 *
 * With model preset `"none"` and transcription `"apple"` or `"local"`,
 * nothing leaves the computer. A model provider receives the transcript text,
 * click labels and a bounded sample of pointer and key events. A
 * transcription key sends the recording's audio to that provider.
 */
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

/** Who reads the transcript for spoken cues. `"none"` = rules only. */
export type ModelPreset =
  | "none"
  | "openrouter"
  | "openai"
  | "gemini"
  | "groq"
  | "ollama"
  | "lmstudio"
  | "custom";

/** Downloadable on-device transcription models (Windows). */
export type LocalModelId = "english-fast" | "multilingual";

/**
 * How a recording is transcribed.
 * - `"apple"`: on this Mac with Apple Speech (macOS only, the default there).
 * - `"local"`: a downloaded model on this PC (Windows only, the default there).
 * - the rest: that provider, with the user's own key; the audio goes to it.
 * - `"none"`: no transcript.
 */
export type TranscriptionKind =
  | "apple"
  | "local"
  | "openai"
  | "groq"
  | "deepgram"
  | "assemblyai"
  | "elevenlabs"
  | "gemini"
  | "none";

export interface ModelSettingsDto {
  preset: ModelPreset;
  /** null = the preset's default model. */
  model: string | null;
  /** null = the preset's own URL. Always set for `"custom"`. */
  baseUrl: string | null;
  /** A key is stored for this provider. The key itself is never returned. */
  hasApiKey: boolean;
}

export interface TranscriptionSettingsDto {
  kind: TranscriptionKind;
  /** Set when `kind` is `"local"`. */
  localModel: LocalModelId | null;
  /** Optional model name for a keyed provider; null = its default. */
  model: string | null;
  /** A key is stored for `kind`. */
  hasApiKey: boolean;
}

/** `get_local_settings` and `save_local_settings` answer this. */
export interface LocalSettingsDto {
  model: ModelSettingsDto;
  transcription: TranscriptionSettingsDto;
}

/** What to do with a stored key on save. */
export type KeyUpdate =
  | { action: "keep" }
  | { action: "clear" }
  | { action: "set"; value: string };

export interface SaveModelSettings {
  preset: ModelPreset;
  model: string | null;
  baseUrl: string | null;
  apiKey: KeyUpdate;
}

export interface SaveTranscriptionSettings {
  kind: TranscriptionKind;
  localModel: LocalModelId | null;
  model: string | null;
  apiKey: KeyUpdate;
}

/** The `settings` argument of `save_local_settings`. */
export interface SaveLocalSettingsRequest {
  model?: SaveModelSettings;
  transcription?: SaveTranscriptionSettings;
}

/** One row of `local_model_status`. */
export interface LocalModelStatusDto {
  id: LocalModelId;
  /** For example "English, fast (Parakeet TDT-CTC 110M)". */
  label: string;
  /** Total download size in bytes. */
  sizeBytes: number;
  installed: boolean;
  /** A download is running now (it may have been started by another window). */
  downloading: boolean;
}

/** Payload of the `local-model-progress` event. */
export interface LocalModelProgressDto {
  model: LocalModelId;
  stage: "downloading" | "verifying" | "installed" | "failed";
  receivedBytes: number;
  totalBytes: number;
  /** Set when `stage` is `"failed"`. */
  error: string | null;
}

export const EVENT_LOCAL_MODEL_PROGRESS = "local-model-progress";

export function getLocalSettings(): Promise<LocalSettingsDto> {
  return invoke<LocalSettingsDto>("get_local_settings");
}

export function saveLocalSettings(settings: SaveLocalSettingsRequest): Promise<LocalSettingsDto> {
  return invoke<LocalSettingsDto>("save_local_settings", { settings });
}

export function localModelStatus(): Promise<LocalModelStatusDto[]> {
  return invoke<LocalModelStatusDto[]>("local_model_status");
}

export function downloadLocalModel(model: LocalModelId): Promise<void> {
  return invoke<void>("download_local_model", { model });
}

export function cancelLocalModelDownload(model: LocalModelId): Promise<void> {
  return invoke<void>("cancel_local_model_download", { model });
}

export function onLocalModelProgress(handler: (progress: LocalModelProgressDto) => void): Promise<UnlistenFn> {
  return listen<LocalModelProgressDto>(EVENT_LOCAL_MODEL_PROGRESS, (event) => handler(event.payload));
}
