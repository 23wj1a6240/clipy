import { useEffect, useId, useRef, useState } from "react";
import { cameraFrameColors, type CameraEffects } from "../lib/cameraEffects";

/** Draw after the cover crop so a circle/pill cannot crop its own decoration. */
export default function CameraFrameOverlay({ frame, shape }: { frame: CameraEffects["frame"]; shape: string }) {
  const ref = useRef<SVGSVGElement | null>(null);
  const id = useId().replace(/:/g, "");
  const [size, setSize] = useState({ width: 200, height: 200 });
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    const update = () => { const rect = element.getBoundingClientRect(); if (rect.width && rect.height) setSize({ width: rect.width, height: rect.height }); };
    update();
    const observer = new ResizeObserver(update); observer.observe(element);
    return () => observer.disconnect();
  }, [frame]);
  if (frame === "none") return null;
  const decorative = ["geometric", "confetti", "botanical"].includes(frame);
  const width = decorative ? Math.max(8, Math.min(size.width, size.height) / 18) : 4;
  const stroke = decorative ? `url(#${id})` : cameraFrameColors[frame];
  return <svg ref={ref} aria-hidden="true" className="pointer-events-none absolute inset-0 z-10 h-full w-full" viewBox={`0 0 ${size.width} ${size.height}`}>
    <defs><pattern id={id} patternUnits="userSpaceOnUse" width="20" height="20">
      <rect width="20" height="20" fill={frame === "botanical" ? "#d6e7cf" : frame === "confetti" ? "#f8f3e8" : "#5783e1"}/>
      {frame === "geometric" && <path d="M0 0h20L0 20z" fill="#ff4d29"/>}
      {frame === "confetti" && <><path d="m4 2 4 5-4 5-4-5z" fill="#896bda"/><circle cx="15" cy="14" r="4" fill="#55b79c"/><path d="m12 0 7 5" stroke="#f7d651" strokeWidth="3"/></>}
      {frame === "botanical" && <><ellipse cx="6" cy="6" rx="4" ry="7" transform="rotate(-35 6 6)" fill="#447852"/><ellipse cx="16" cy="16" rx="4" ry="7" transform="rotate(35 16 16)" fill="#719462"/></>}
    </pattern></defs>
    {shape === "circle" ? <ellipse cx={size.width / 2} cy={size.height / 2} rx={(size.width - width) / 2} ry={(size.height - width) / 2} fill="none" stroke={stroke} strokeWidth={width}/>
      : <rect x={width / 2} y={width / 2} width={Math.max(0, size.width - width)} height={Math.max(0, size.height - width)} rx={shape === "pill" ? Math.max(0, Math.min(size.width, size.height) / 2 - width / 2) : Math.max(0, (shape === "rectangle" ? 18 : size.width * .22) - width / 2)} ry={shape === "pill" ? Math.max(0, Math.min(size.width, size.height) / 2 - width / 2) : Math.max(0, (shape === "rectangle" ? 18 : size.height * .22) - width / 2)} fill="none" stroke={stroke} strokeWidth={width}/>}
  </svg>;
}
