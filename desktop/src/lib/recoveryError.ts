/**
 * The line Home shows when Recover fails.
 *
 * Rust sends one of its own plain sentences when the cause is known (offline:
 * `OFFLINE_RECOVER` in `src-tauri/src/capture/failure.rs`); those are shown
 * as they are, never behind "Couldn't recover this recording:", which is for
 * errors that need the context. Keep the sentence below equal to the Rust one.
 */
import { englishT, type DesktopT } from "./messages";

export const OFFLINE_RECOVER = englishT("recovery.offline");

/** Rust's English sentence → its catalog key, shown translated. */
const PLAIN_SENTENCES: Record<string, string> = { [OFFLINE_RECOVER]: "recovery.offline" };

export function recoveryErrorMessage(err: unknown, t: DesktopT = englishT): string {
  const text = (err instanceof Error ? err.message : String(err)).trim();
  const plain = PLAIN_SENTENCES[text];
  if (plain) return t(plain);
  return t("recovery.failed", { error: text });
}
