import type { RecordingGateLevel, RecordingOffer, RecordingQuota } from "../lib/bridge";

/** Local edition stand-in for `routes/ProGateCard`: nothing gates a local
 *  recording, so there is no plan card. `stubs.typecheck.ts` keeps the props
 *  in step with the real card. */
export default function ProGateCard(_props: {
  level: RecordingGateLevel;
  quota?: RecordingQuota | null;
  offer?: RecordingOffer | null;
  fallbackMessage?: string | null;
  onUpgrade: () => void;
  onRecheck: () => void;
  rechecking?: boolean;
  stillGated?: boolean;
}) {
  return null;
}
