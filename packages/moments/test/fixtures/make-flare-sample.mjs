#!/usr/bin/env node
/**
 * Writes flare-sample.json: a synthetic 27-second screen recording for the
 * moment tests. Nothing in it comes from a real recording; the words, labels
 * and pointer path are invented. It has the shape of a desktop interaction
 * sidecar plus a word-level transcript:
 *
 * - A 1512x982 capture at 0,0, encoded at 1108x720.
 * - Left clicks spread through the take, each with an element label.
 * - A scroll stretch in the middle.
 * - A hold-to-highlight (Control, key code 59) from 20103 ms to 22753 ms,
 *   during which the pointer sweeps around and comes to rest at (367, 471)
 *   from about 22560 ms until 22933 ms. The flare frame (release minus 120 ms) is
 *   22633 ms, inside that rest.
 * - Speech starting at 10120, 15190 and 25130 ms, and `llmMoments`: the
 *   spoken cues a model is expected to return for it.
 *
 * Deterministic: re-running it writes the same file.
 *
 *   node packages/moments/test/fixtures/make-flare-sample.mjs
 */
import { writeFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const OUT = join(dirname(fileURLToPath(import.meta.url)), "flare-sample.json");
const WIDTH = 1512;
const HEIGHT = 982;
const OUT_W = 1108;
const OUT_H = 720;
const DURATION_MS = 26_975;

const round3 = (n) => Math.round(n * 1000) / 1000;

/** A fixed pseudo-random sequence (LCG), so sample spacing looks uneven but never changes. */
let seed = 7;
function rand() {
  seed = (seed * 1103515245 + 12345) % 2147483648;
  return seed / 2147483648;
}

const events = [];
let cursor = { x: 812, y: 388, type: "cursor" };

function move(t, x, y, type = cursor.type) {
  cursor = { x, y, type };
  events.push({ cursor_type: type, kind: "move", time_ms: round3(t), x, y });
}

/** Move along straight legs through `points`, from `t0` to `t1`, sampling every 20 to 40 ms. */
function path(t0, t1, points, type = cursor.type) {
  const all = [{ x: cursor.x, y: cursor.y }, ...points];
  let last = { x: cursor.x, y: cursor.y };
  const lengths = all.slice(1).map((p, i) => Math.hypot(p.x - all[i].x, p.y - all[i].y));
  const total = lengths.reduce((a, b) => a + b, 0) || 1;
  for (let t = t0 + 20 + rand() * 20; t < t1; t += 20 + rand() * 20) {
    // Ease in and out over the whole path.
    const f = 0.5 - Math.cos(Math.PI * ((t - t0) / (t1 - t0))) / 2;
    let along = f * total;
    let i = 0;
    while (i < lengths.length - 1 && along > lengths[i]) along -= lengths[i++];
    const a = all[i];
    const b = all[i + 1];
    const k = lengths[i] ? Math.min(1, along / lengths[i]) : 1;
    const x = Math.round(a.x + (b.x - a.x) * k);
    const y = Math.round(a.y + (b.y - a.y) * k);
    if (last.x === x && last.y === y) continue;
    move(t, x, y, type);
    last = { x, y };
  }
  const end = points[points.length - 1];
  // A sampler reports changes only, so no repeat of the same position.
  if (last.x !== end.x || last.y !== end.y) move(t1, end.x, end.y, type);
}

const clickElements = [];
function click(down, up, label, role, type = cursor.type, upAt = null) {
  events.push({
    button: 1,
    cursor_type: type,
    kind: "down",
    render_x: round3((cursor.x * OUT_W) / WIDTH),
    render_y: round3((cursor.y * OUT_H) / HEIGHT),
    time_ms: round3(down),
    x: cursor.x,
    y: cursor.y,
  });
  clickElements.push({ label, role, time_ms: round3(down) });
  if (upAt) path(down + 30, up - 30, [upAt], type);
  if (up !== null) events.push({ button: 1, cursor_type: type, kind: "up", time_ms: round3(up), x: cursor.x, y: cursor.y });
}

move(4.2, 812, 388);
path(4.2, 1950.4, [{ x: 700, y: 640 }, { x: 655, y: 902 }]);
click(2140.5, 2655.25, "Test page is sharing your screen.", "AXStaticText", "cursor", { x: 1040, y: 730 });
path(2720.2, 4450.6, [{ x: 960, y: 930 }]);
click(4580.25, 4642.75, "Terminal", "AXDockItem");
path(4800.1, 6600.9, [{ x: 742, y: 436 }]);

// Scrolling through the test page.
for (let t = 6720.4; t <= 12240.8; t += 40 + rand() * 30) {
  const delta = Math.round((rand() - 0.6) * 8);
  events.push({ cursor_type: "cursor", delta_x: 0, delta_y: delta, kind: "scroll", time_ms: round3(t), x: 742, y: 436 });
}

path(12290.3, 12650.2, [{ x: 312, y: 248 }], "textcursor");
click(12702.8, 12788.6, "Build log", "AXWindow", "textcursor");
path(12900.7, 15020.1, [{ x: 548, y: 590 }, { x: 702, y: 760 }], "cursor");
click(15160.4, 15248.9, "Retry", "AXButton");
path(15400.2, 17450.5, [{ x: 940, y: 928 }]);
click(17610.9, 17702.3, "Terminal", "AXDockItem");
path(17800.4, 18380.8, [{ x: 486, y: 602 }], "textcursor");
click(18490.1, 18575.4, "Build log", "AXWindow", "textcursor");
path(18650.3, 19950.6, [{ x: 440, y: 540 }]);

// Hold Control to highlight: a sweep around the failing line, then a rest.
events.push({ key_code: 59, kind: "key_down", time_ms: 20103.382 });
path(20150.2, 22566.3, [
  { x: 402, y: 512 },
  { x: 331, y: 598 },
  { x: 366, y: 662 },
  { x: 540, y: 690 },
  { x: 790, y: 655 },
  { x: 852, y: 600 },
  { x: 801, y: 540 },
  { x: 640, y: 488 },
  { x: 512, y: 470 },
  { x: 420, y: 458 },
  { x: 379, y: 462 },
  { x: 367, y: 471 },
]);
events.push({ key_code: 59, kind: "key_up", time_ms: 22753.342 });
path(22933.9, 24000.5, [{ x: 540, y: 575 }], "cursor");
path(24100.1, 26650.2, [{ x: 230, y: 440 }, { x: 64, y: 296 }], "handpointing");
// The take ends while this button is held.
click(26760.3, null, "Test page", "AXWebArea", "handpointing");

events.sort((a, b) => a.time_ms - b.time_ms);

/** Word-level transcript: each utterance starts at `start` and spreads its words over `ms`. */
function utterance(start, ms, text) {
  const words = text.split(" ");
  const each = ms / words.length;
  return words.map((word, i) => ({
    endMs: Math.round(start + each * (i + 1) - 10),
    startMs: Math.round(start + each * i),
    text: word,
  }));
}

const transcript = [
  ...utterance(9270, 510, "Okay so"),
  ...utterance(10120, 4230, "the build on this test page keeps failing at the same step every single time I run it"),
  ...utterance(15190, 1020, "it fails here"),
  ...utterance(18160, 1840, "let me open the build log and look"),
  ...utterance(22310, 2730, "this is the line where the step stops"),
  ...utterance(25130, 1140, "can you check it and fix it"),
];

const sample = {
  note: "Synthetic sample for tests.",
  durationMs: DURATION_MS,
  capture: {
    height: HEIGHT,
    output_height: OUT_H,
    output_width: OUT_W,
    screen_height: HEIGHT,
    screen_width: WIDTH,
    screen_x: 0,
    screen_y: 0,
    width: WIDTH,
    x: 0,
    y: 0,
  },
  events,
  click_elements: clickElements,
  transcript,
  llmMoments: [
    { timestamp_ms: 10120, reason: "The speaker says the build on the test page keeps failing at the same step." },
    { timestamp_ms: 15190, reason: "The speaker points at where the build fails." },
    { timestamp_ms: 25130, reason: "The speaker asks for the failing step to be checked and fixed." },
  ],
};

writeFileSync(OUT, `${JSON.stringify(sample, null, 2)}\n`);
process.stdout.write(`wrote ${OUT} (${events.length} events, ${transcript.length} words)\n`);
