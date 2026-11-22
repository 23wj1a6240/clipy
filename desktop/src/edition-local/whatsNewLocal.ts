import { englishT } from "../lib/messages";
import type { Feature, FeaturePlatform } from "../lib/whatsNew";

/**
 * The What's new catalog as the local edition tells it.
 *
 * - "Let your agent record" drives this app through `clipy ... --local`
 *   and hands back an AREC path, not a share link.
 * - "Local AI" is the only way this edition records, and its Windows line in
 *   the official catalog says the audio is transcribed on Clipy's server,
 *   which is not true here (a downloaded model or the user's own provider
 *   transcribes it, chosen in Settings).
 */
export function localEditionFeatures(features: readonly Feature[], platform: FeaturePlatform): Feature[] {
  return features
    .filter((f) => f.addedIn[platform] !== undefined)
    .map((f) =>
      f.id === "agent"
        ? {
            ...f,
            blurb: {
              macos: englishT("whatsNew.features.agent.blurbLocal"),
              windows: englishT("whatsNew.features.agent.blurbLocal"),
            },
          }
        : f.id === "local_ai"
        ? {
            ...f,
            blurb: {
              macos: "Every recording stays on this Mac: the video, a transcript and an AREC your agent can read.",
              windows:
                "Every recording stays on this PC: the video, a transcript and an AREC your agent can read. Pick how it is transcribed in Settings.",
            },
          }
        : f,
    );
}
