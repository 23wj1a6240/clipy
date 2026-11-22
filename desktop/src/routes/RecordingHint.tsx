import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getRecordingControlsHint, setRecordingHintBounds, type RecordingControlsHint } from "../lib/bridge";

export default function RecordingHint() {
  const [hint, setHint] = useState<RecordingControlsHint | null>(null);
  useEffect(() => {
    let disposed = false;
    let receivedEvent = false;
    let unlisten: (() => void) | undefined;
    listen<RecordingControlsHint>("recording-controls-hint", ({ payload }) => {
      receivedEvent = true;
      if (!disposed) setHint(payload);
    }).then(async (off) => {
      if (disposed) { off(); return; }
      unlisten = off;
      const initial = await getRecordingControlsHint();
      if (!disposed && !receivedEvent) setHint(initial);
    }).catch((error) => console.warn("recording hint unavailable", error));
    return () => { disposed = true; unlisten?.(); };
  }, []);
  // On Windows 10 (2004+) this window takes clicks (it cannot be click-through and
  // stay out of the recording), so Rust cuts its clickable area down to the
  // bubble. Elsewhere the call does nothing.
  const tipRef = useRef<HTMLDivElement>(null);
  const showing = hint !== null;
  useEffect(() => {
    const tip = tipRef.current;
    if (!tip) return;
    const observer = new ResizeObserver(() => {
      const box = tip.getBoundingClientRect();
      void setRecordingHintBounds({ x: box.left, y: box.top, width: box.width, height: box.height })
        .catch((error) => console.warn("recording hint bounds failed", error));
    });
    observer.observe(tip);
    return () => observer.disconnect();
  }, [showing]);
  return (
    <main className="pointer-events-none flex h-full w-full items-center bg-transparent p-1">
      {hint && <div ref={tipRef} role="tooltip" className="hud-tooltip" style={{ position: "static", transform: "none", animation: "none" }}>{hint.text}</div>}
    </main>
  );
}
