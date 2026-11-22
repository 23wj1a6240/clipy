export default function CursorTarget() {
  return (
    <main className="cursor-target-shell" aria-label="Clipy cursor target">
      <div className="cursor-target-grid">
        <textarea
          className="cursor-target-zone cursor-target-text"
          aria-label="Clipy cursor text target"
          defaultValue="Text"
        />
        <a
          className="cursor-target-zone cursor-target-pointer"
          href="#cursor-target"
          aria-label="Clipy cursor pointer target"
        >
          Pointer
        </a>
        <div className="cursor-target-zone cursor-target-resize-x" aria-label="Clipy cursor resize horizontal target">
          Resize X
        </div>
        <div className="cursor-target-zone cursor-target-resize-y" aria-label="Clipy cursor resize vertical target">
          Resize Y
        </div>
        <div className="cursor-target-zone cursor-target-resize-east" aria-label="Clipy cursor resize east target">
          East
        </div>
        <div className="cursor-target-zone cursor-target-resize-west" aria-label="Clipy cursor resize west target">
          West
        </div>
        <div className="cursor-target-zone cursor-target-resize-north" aria-label="Clipy cursor resize north target">
          North
        </div>
        <div className="cursor-target-zone cursor-target-resize-south" aria-label="Clipy cursor resize south target">
          South
        </div>
        <div className="cursor-target-zone cursor-target-resize-nwse" aria-label="Clipy cursor resize northwest southeast target">
          NWSE
        </div>
        <div className="cursor-target-zone cursor-target-resize-nesw" aria-label="Clipy cursor resize northeast southwest target">
          NESW
        </div>
        <div className="cursor-target-zone cursor-target-cross" aria-label="Clipy cursor crosshair target">
          Cross
        </div>
        <div className="cursor-target-zone cursor-target-not-allowed" aria-label="Clipy cursor not allowed target">
          Blocked
        </div>
        <div className="cursor-target-zone cursor-target-move" aria-label="Clipy cursor move target">
          Move
        </div>
        <div className="cursor-target-zone cursor-target-grab" aria-label="Clipy cursor open hand target">
          Grab
        </div>
        <div className="cursor-target-zone cursor-target-grabbing" aria-label="Clipy cursor closed hand target">
          Hold
        </div>
        <div className="cursor-target-zone cursor-target-help" aria-label="Clipy cursor help target">
          Help
        </div>
        <div className="cursor-target-zone cursor-target-zoom" aria-label="Clipy cursor zoom in target">
          Zoom In
        </div>
        <div className="cursor-target-zone cursor-target-zoom-out" aria-label="Clipy cursor zoom out target">
          Zoom Out
        </div>
      </div>
    </main>
  );
}
