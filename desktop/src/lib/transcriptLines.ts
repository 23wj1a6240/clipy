/**
 * Apple's on-device recognizer returns one `SFTranscriptionSegment` per token,
 * so a local recording's transcript is a column of single words each carrying
 * the same second. That is the right shape for the AREC — an agent wants the
 * word timings — and an unreadable shape for a human, so the viewer folds the
 * tokens back into lines before drawing them.
 *
 * Display only. `transcript.json` and `recording.arec` keep every token.
 */

export interface TranscriptLineSegment {
  startMs: number;
  endMs: number;
  text: string;
}

/** A silence this long reads as the end of a thought, so the line breaks. */
const PAUSE_MS = 700;
/** Ceiling so an unbroken monologue does not become one wall of text. */
const MAX_LINE_MS = 14_000;
/** Roughly two wrapped lines in the viewer's 380px column. */
const MAX_LINE_WIDTH = 90;

/**
 * The recognizer is built from `NSLocale.currentLocale`, so the tokens can be
 * Japanese, Chinese, Thai and other scripts that do not separate words with a
 * space. Joining those with an ASCII space would render `你 好 世 界`, so the
 * space is dropped whenever either side of the seam belongs to one of them.
 * Hangul is deliberately absent — Korean does space its words.
 */
const NO_SPACE_SCRIPT =
  /[฀-๿຀-໿က-႟ក-៿　-〿぀-ヿ㐀-䶿一-鿿豈-﫿＀-･]/;

/** Punctuation the recognizer emits as its own token; it takes no space. */
const CLINGS_LEFT = /^[,.!?;:…%)\]}'’”]/;
const SENTENCE_END = /[.!?…。！？]["'’”)\]}」』）]?$/;

function join(line: string, token: string): string {
  if (CLINGS_LEFT.test(token)) return `${line}${token}`;
  if (NO_SPACE_SCRIPT.test(line.slice(-1)) || NO_SPACE_SCRIPT.test(token[0])) {
    return `${line}${token}`;
  }
  return `${line} ${token}`;
}

/**
 * How much room a line asks for, not how many characters it holds: a
 * full-width glyph occupies about two Latin ones, so counting them equally
 * would let a Japanese line run to four wrapped rows instead of two.
 */
function width(text: string): number {
  let total = 0;
  for (const char of text) total += NO_SPACE_SCRIPT.test(char) ? 2 : 1;
  return total;
}

function breaks(line: TranscriptLineSegment, next: TranscriptLineSegment, text: string): boolean {
  if (SENTENCE_END.test(line.text)) return true;
  if (next.startMs - line.endMs >= PAUSE_MS) return true;
  if (next.endMs - line.startMs >= MAX_LINE_MS) return true;
  return width(join(line.text, text)) > MAX_LINE_WIDTH;
}

export function groupTranscriptLines(
  segments: readonly TranscriptLineSegment[]
): TranscriptLineSegment[] {
  const lines: TranscriptLineSegment[] = [];
  let line: TranscriptLineSegment | null = null;

  for (const segment of segments) {
    const text = segment.text.trim();
    if (!text) continue;

    if (line && !breaks(line, segment, text)) {
      line = {
        startMs: line.startMs,
        endMs: Math.max(line.endMs, segment.endMs),
        text: join(line.text, text),
      };
      continue;
    }

    if (line) lines.push(line);
    line = {
      startMs: segment.startMs,
      endMs: Math.max(segment.startMs, segment.endMs),
      text,
    };
  }

  if (line) lines.push(line);
  return lines;
}
