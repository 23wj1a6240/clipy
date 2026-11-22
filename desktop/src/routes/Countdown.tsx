import { useEffect, useState } from "react";
import { onCountdownTick } from "../lib/bridge";

export default function Countdown() {
  const [n, setN] = useState<number | null>(null);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let unlisten: (() => void) | undefined;
    onCountdownTick((value) => {
      setN(value);
      setTick((t) => t + 1);
    }).then((fn) => {
      unlisten = fn;
    });
    return () => unlisten?.();
  }, []);

  return (
    <main className="flex h-full w-full items-center justify-center bg-transparent">
      {n !== null && (
        <div
          key={tick}
          className="countdown-card animate-countdown-pop flex h-52 w-52 items-center justify-center text-white"
        >
          <span className="countdown-ring countdown-ring-a" aria-hidden />
          <span className="countdown-ring countdown-ring-b" aria-hidden />
          <span className="countdown-sweep" aria-hidden />
          <span
            className="relative z-10 font-bold tabular-nums"
            style={{ fontSize: 96, lineHeight: 1 }}
          >
            {n}
          </span>
        </div>
      )}
    </main>
  );
}
