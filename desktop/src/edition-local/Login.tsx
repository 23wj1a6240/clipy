/** Local edition stand-in for `routes/Login`: there is no Clipy account to
 *  sign in to, and `App` never renders this. `stubs.typecheck.ts` keeps the
 *  props in step with the real screen. */
export default function Login(_props: { onLocal?: () => void }) {
  return null;
}
