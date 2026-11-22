import type { ClipEntryDto } from "./bridge";
import { englishT, type DesktopT } from "./messages";

/**
 * What a clipboard history item looks like it is, for its icon and colour in
 * the picker. A best guess from the text alone: it never changes what gets
 * pasted, so a wrong guess only costs a different icon.
 */
export type ClipKind = "image" | "color" | "link" | "email" | "code" | "text";

/** The kind's label on a row, e.g. "Link" (`clipboard.kinds`). */
export function clipKindLabel(kind: ClipKind, t: DesktopT = englishT): string {
  return t(`clipboard.kinds.${kind}`);
}

const HEX_COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;
const CSS_COLOR = /^(?:rgba?|hsla?)\(\s*[\d.%\s,/]+\)$/i;
const LINK = /^https?:\/\/[^\s]+$/i;
const EMAIL = /^[^\s@]+@[^\s@]+\.[a-z]{2,}$/i;
// A shell command, or text with the punctuation code is made of. Prose has
// commas and full stops, rarely braces, semicolons at line ends or `=>`.
const SHELL = /^(?:\$\s*)?(?:pnpm|npm|npx|yarn|git|cd|ls|cat|curl|brew|cargo|docker|kubectl|ssh|sudo|python3?|node)\s/;
const CODE_MARKS = /[{}]|;\s*$|=>|\(\)|^\s*(?:const|let|var|function|import|export|return|def|class|fn|pub|SELECT|select)\b|<\/?[a-z][\w-]*[\s>]/m;

export function clipKind(entry: Pick<ClipEntryDto, "text" | "image">): ClipKind {
  if (entry.image) return "image";
  const text = entry.text.trim();
  if (!text) return "text";
  if (!/\s/.test(text)) {
    if (HEX_COLOR.test(text)) return "color";
    if (LINK.test(text)) return "link";
    if (EMAIL.test(text)) return "email";
  }
  if (CSS_COLOR.test(text)) return "color";
  if (SHELL.test(text) || CODE_MARKS.test(text)) return "code";
  return "text";
}

/** The CSS colour a "color" item names, or null when it is something else. */
export function clipSwatch(entry: Pick<ClipEntryDto, "text" | "image">): string | null {
  return clipKind(entry) === "color" ? entry.text.trim() : null;
}

/** The second line of a row: the host for a link, else its size. */
export function clipDetail(entry: Pick<ClipEntryDto, "text" | "image">, t: DesktopT = englishT): string {
  if (entry.image) return `${entry.image.width}×${entry.image.height}`;
  const text = entry.text;
  if (clipKind(entry) === "link") {
    try {
      return new URL(text.trim()).hostname;
    } catch {
      // Fall through to the size.
    }
  }
  // Counts as strings, so a long item reads "1234 characters", not "1,234".
  const lines = text.split("\n").length;
  if (lines > 1) return t("clipboard.lines", { lines: String(lines) });
  const chars = [...text].length;
  return t("clipboard.characters", { count: chars, chars: String(chars) });
}

/** The first non-empty line, which is what a row shows. */
export function clipFirstLine(text: string): string {
  return text.split("\n").find((line) => line.trim()) ?? "";
}
