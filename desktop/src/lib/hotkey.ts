import { isWindows } from "./platform";

export function formatHotkey(hotkey: string): string {
  const parts = hotkey.split("+");
  const key = parts.pop()?.replace(/^(Key|Digit)/, "") ?? "";
  const labels: Record<string, string> = isWindows
    ? { Meta: "Win", Ctrl: "Ctrl", Control: "Ctrl", Alt: "Alt", Option: "Alt", Shift: "Shift" }
    : { Meta: "⌘", Ctrl: "⌃", Control: "⌃", Alt: "⌥", Option: "⌥", Shift: "⇧" };
  return [...parts.map((part) => labels[part] ?? part), key].join(isWindows ? "+" : "");
}

/** `Meta+Shift+Digit2` → ["⌘", "⇧", "2"] (or Win/Ctrl/Alt/Shift words). */
export function hotkeyKeycaps(hotkey: string): string[] {
  if (!hotkey) return [];
  const parts = hotkey.split("+");
  const key = parts.pop()?.replace(/^(Key|Digit)/, "") ?? "";
  const labels: Record<string, string> = isWindows
    ? { Meta: "Win", Ctrl: "Ctrl", Control: "Ctrl", Alt: "Alt", Option: "Alt", Shift: "Shift" }
    : { Meta: "⌘", Ctrl: "⌃", Control: "⌃", Alt: "⌥", Option: "⌥", Shift: "⇧" };
  return [...parts.map((part) => labels[part] ?? part), key];
}
