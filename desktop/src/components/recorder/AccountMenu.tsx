import { useCallback, useEffect, useRef, useState } from "react";
import { useTranslations } from "use-intl";
import type { DesktopUser } from "../../lib/bridge";
import { deviceName } from "../../lib/platform";
import { Icon, type IconName } from "./ui";

/**
 * The signed-in person's face in the recorder header, and everything about
 * their account one click away: who is signed in, their recordings, the web
 * account page, switching to someone else, and signing out.
 *
 * Sign out used to live only at the bottom of Settings → Preferences, below
 * the fold, and QA teams sharing a Mac could not find it. Signing out asks
 * once, inside the same card, so a stray click never logs anyone out.
 */
export function initialsFor(user: Pick<DesktopUser, "name" | "email"> | null): string {
  return (
    (user?.name ?? user?.email ?? "")
      .split(/[\s._-]+/)
      .filter(Boolean)
      .slice(0, 2)
      .map((s) => s[0]?.toUpperCase() ?? "")
      .join("") || "·"
  );
}

function Avatar({ user, isPro, size }: { user: DesktopUser; isPro: boolean; size: number }) {
  const ring = isPro ? "relative ring-2 ring-surface" : "";
  return (
    <span className="relative inline-flex shrink-0" style={{ width: size, height: size }}>
      {isPro && <span aria-hidden className="absolute -inset-[3px] rounded-full bg-gradient-flash" />}
      {user.avatar_url ? (
        <img src={user.avatar_url} alt="" className={`h-full w-full rounded-full object-cover ${ring}`} referrerPolicy="no-referrer" />
      ) : (
        <span
          className={`flex h-full w-full items-center justify-center rounded-full bg-flash-500 font-bold text-white ${ring}`}
          style={{ fontSize: Math.round(size * 0.38) }}
        >
          {initialsFor(user)}
        </span>
      )}
    </span>
  );
}

function Action({
  icon,
  label,
  onClick,
  tone = "default",
  disabled,
}: {
  icon: IconName | "signOut" | "switchUser";
  label: string;
  onClick: () => void;
  tone?: "default" | "danger";
  disabled?: boolean;
}) {
  return (
    <button
      type="button"
      role="menuitem"
      onClick={onClick}
      disabled={disabled}
      className={`group flex w-full items-center gap-3 rounded-xl px-3 py-2.5 text-left text-[15px] font-medium transition-colors disabled:opacity-50 ${
        tone === "danger" ? "text-flash-600 hover:bg-flash-500/10" : "text-ink-text hover:bg-row"
      }`}
    >
      <span
        className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-lg transition-transform group-hover:scale-110 ${
          tone === "danger" ? "bg-flash-500/10 text-flash-600" : "bg-row text-ink-secondary"
        }`}
      >
        {icon === "signOut" ? <SignOutGlyph /> : icon === "switchUser" ? <SwitchGlyph /> : <Icon name={icon} size={17} />}
      </span>
      <span className="min-w-0 flex-1 truncate">{label}</span>
    </button>
  );
}

function SignOutGlyph({ size = 17 }: { size?: number }) {
  return (
    <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.9} strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <path d="M14 4h3a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2h-3" />
      <path d="M10 16l-4-4 4-4" />
      <path d="M6 12h9" />
    </svg>
  );
}

function SwitchGlyph() {
  return (
    <svg width={17} height={17} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth={1.9} strokeLinecap="round" strokeLinejoin="round" aria-hidden>
      <circle cx="9" cy="8" r="3.2" />
      <path d="M3.5 19a5.5 5.5 0 0 1 11 0" />
      <path d="M17 7.5h4.5" />
      <path d="m19.5 5.5 2 2-2 2" />
      <path d="M21 14.5h-4.5" />
      <path d="m18.5 12.5-2 2 2 2" />
    </svg>
  );
}

export function AccountMenu({
  user,
  isPro,
  onOpenRecordings,
  onEditAccount,
  onSignOut,
  onSwitchAccount,
}: {
  user: DesktopUser;
  isPro: boolean;
  onOpenRecordings: () => void;
  onEditAccount: () => void;
  onSignOut: () => Promise<void>;
  onSwitchAccount: () => Promise<void>;
}) {
  const t = useTranslations("desktop.home.account");
  const [open, setOpen] = useState(false);
  const [confirming, setConfirming] = useState(false);
  const [working, setWorking] = useState<null | "signOut" | "switch">(null);
  const ref = useRef<HTMLDivElement | null>(null);
  const confirmRef = useRef<HTMLButtonElement | null>(null);
  // Read by the key handler, so stepping into the confirmation does not
  // re-attach the listeners (which would leave a tick with no Escape).
  const confirmingRef = useRef(false);
  confirmingRef.current = confirming;

  const close = useCallback(() => {
    setOpen(false);
    setConfirming(false);
  }, []);

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) close();
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      // Escape steps back out of the confirmation before closing the card.
      if (confirmingRef.current) setConfirming(false);
      else close();
    };
    const timer = window.setTimeout(() => {
      document.addEventListener("mousedown", onDoc);
      document.addEventListener("keydown", onKey);
    }, 0);
    return () => {
      window.clearTimeout(timer);
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open, close]);

  useEffect(() => {
    if (confirming) confirmRef.current?.focus();
  }, [confirming]);

  const run = async (kind: "signOut" | "switch") => {
    setWorking(kind);
    try {
      await (kind === "signOut" ? onSignOut() : onSwitchAccount());
    } finally {
      // On success this component unmounts with the signed-in screen; on a
      // failure the card stays open so the person can try again.
      setWorking(null);
    }
  };

  const name = user.name ?? user.email;

  return (
    <div ref={ref} className="relative">
      <button
        type="button"
        onClick={() => (open ? close() : setOpen(true))}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={t("open", { email: user.email })}
        title={user.email}
        className={`flex h-9 w-9 items-center justify-center rounded-full transition-transform hover:scale-105 ${open ? "scale-105" : ""}`}
      >
        <Avatar user={user} isPro={isPro} size={28} />
      </button>

      {open && (
        <div
          role="menu"
          aria-label={t("title")}
          className="absolute right-0 top-11 z-50 w-[288px] overflow-hidden rounded-2xl bg-surface shadow-pop animate-pop-in"
        >
          {/* Identity card: a brand band with the avatar riding its edge. */}
          <div className="relative h-14 bg-gradient-flash">
            <div aria-hidden className="absolute inset-0 opacity-30 [background:radial-gradient(circle_at_85%_20%,white,transparent_45%)]" />
          </div>
          <div className="-mt-8 px-4">
            <div className="rounded-full bg-surface p-1" style={{ width: 64, height: 64 }}>
              <Avatar user={user} isPro={isPro} size={56} />
            </div>
          </div>
          <div className="px-4 pb-3 pt-2">
            <div className="flex items-center gap-2">
              <span className="truncate text-[16px] font-semibold text-ink-text">{name}</span>
              {isPro && (
                <span className="inline-flex shrink-0 items-center gap-1 rounded-full bg-spark-500 px-2 py-0.5 text-[11px] font-bold text-ink-text">
                  <Icon name="crown" size={11} />
                  {/* product name */}
                  Pro
                </span>
              )}
            </div>
            {user.name && <div className="truncate text-[13px] text-ink-secondary">{user.email}</div>}
            <div className="mt-1.5 flex items-center gap-1.5 text-[12px] text-ink-secondary">
              <span aria-hidden className="h-1.5 w-1.5 rounded-full bg-emerald-500" />
              {t("signedInOn", { device: deviceName })}
            </div>
          </div>

          <div className="border-t border-strong/40 p-1.5">
            {confirming ? (
              <div key="confirm" className="clipy-fade-in px-2.5 pb-1.5 pt-2">
                <div className="flex items-start gap-3">
                  <span className="mt-0.5 flex h-9 w-9 shrink-0 items-center justify-center rounded-xl bg-flash-500/10 text-flash-600">
                    <SignOutGlyph size={19} />
                  </span>
                  <div className="min-w-0">
                    <div className="text-[15px] font-semibold text-ink-text">{t("confirmTitle", { device: deviceName })}</div>
                    <p className="mt-0.5 text-[12.5px] leading-snug text-ink-secondary">{t("confirmBody")}</p>
                  </div>
                </div>
                <div className="mt-3 flex gap-2">
                  <button
                    type="button"
                    onClick={() => setConfirming(false)}
                    disabled={working !== null}
                    className="flex-1 rounded-full border border-strong py-2 text-[14px] font-semibold text-ink-text hover:bg-row disabled:opacity-50"
                  >
                    {t("cancel")}
                  </button>
                  <button
                    ref={confirmRef}
                    type="button"
                    onClick={() => void run("signOut")}
                    disabled={working !== null}
                    className="flex-1 rounded-full bg-flash-500 py-2 text-[14px] font-semibold text-white hover:bg-flash-600 disabled:opacity-60"
                  >
                    {working === "signOut" ? t("signingOut") : t("signOut")}
                  </button>
                </div>
              </div>
            ) : (
              <div key="actions" className="clipy-fade-in">
                <Action icon="library" label={t("recordings")} onClick={() => { close(); onOpenRecordings(); }} />
                <Action icon="external" label={t("editAccount")} onClick={() => { close(); onEditAccount(); }} />
                <Action
                  icon="switchUser"
                  label={working === "switch" ? t("signingOut") : t("switchAccount")}
                  disabled={working !== null}
                  onClick={() => void run("switch")}
                />
                <div className="mx-3 my-1 h-px bg-row" />
                <Action icon="signOut" tone="danger" label={t("signOut")} onClick={() => setConfirming(true)} />
              </div>
            )}
          </div>
        </div>
      )}
    </div>
  );
}
