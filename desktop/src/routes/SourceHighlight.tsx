import { useEffect, useState } from "react";
import { onSourceHighlightStyle, type SourceHighlightStyle } from "../lib/bridge";

export default function SourceHighlight() {
  const [style, setStyle] = useState<SourceHighlightStyle>("preview");

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;
    onSourceHighlightStyle(setStyle).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, []);

  return (
    <main className="pointer-events-none flex h-full w-full bg-transparent p-1">
      <div
        data-style={style}
        className={`source-highlight-frame h-full w-full rounded-md${
          style === "recording" ? " source-highlight-frame--recording" : ""
        }`}
      />
    </main>
  );
}
