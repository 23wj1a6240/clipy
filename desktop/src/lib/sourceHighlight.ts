/**
 * When the source-highlight border (the red frame around the display/window
 * about to be recorded) may be on screen.
 *
 * The border is an always-on-top overlay window, so anything that lets it
 * outlive the picker paints a "recording" cue over the whole display with no
 * recording running. Two rules keep it honest, mirroring Cap's target
 * overlay, which only exists while its main window has focus:
 *
 * - it follows the popover's focus: click away and the border goes with it;
 *   come back and it returns;
 * - it does not come back when Rust re-shows the popover on its own after a
 *   recording (the "restored" tail). That reshow is not the user opening a
 *   picker, so the border waits until they touch the picker again.
 */
export interface SourceHighlightInputs {
  /** A picker session is open in the popover. */
  pickerOpen: boolean;
  /** The popover window currently has keyboard focus. */
  panelFocused: boolean;
  /** The user has engaged the picker since the last automatic reshow. */
  armed: boolean;
  /** The fullscreen area-select overlay is up. */
  areaSelecting: boolean;
  /** "Click a window" pick is in progress. */
  windowSelecting: boolean;
}

export function shouldShowSourceHighlight(inputs: SourceHighlightInputs): boolean {
  return (
    inputs.pickerOpen &&
    inputs.panelFocused &&
    inputs.armed &&
    !inputs.areaSelecting &&
    !inputs.windowSelecting
  );
}

export type PanelFocusTransition = {
  /** Was the popover already tracked as visible before this focus event? */
  wasVisible: boolean;
  focused: boolean;
};

/**
 * Focus regained on a popover that was already on screen is the user coming
 * back to it, which re-arms the highlight. The first focus after Rust shows
 * the window (visible was false) is not: that is how both a tray open and the
 * post-recording reshow arrive, and the reshow must stay disarmed.
 */
export function focusReArmsHighlight(transition: PanelFocusTransition): boolean {
  return transition.focused && transition.wasVisible;
}
