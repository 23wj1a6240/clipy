/** Local edition stand-ins for `components/UpgradePrompt`: there is no plan to
 *  upgrade, so the banner and the plan row render nothing. `stubs.typecheck.ts`
 *  keeps the signatures in step with the real ones. */
export function upgradeUrl(_surface: string): string {
  return "";
}

export function UpgradeBanner(_props: { onUpgrade: () => void }) {
  return null;
}

export function PlanRow(_props: { onUpgrade: () => void }) {
  return null;
}
