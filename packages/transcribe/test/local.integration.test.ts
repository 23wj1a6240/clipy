/**
 * Real models, real speech. Opt in with CLIPY_TEST_MODELS=1: it downloads
 * both models (about 265 MB) into CLIPY_TEST_MODEL_DIR or the default cache
 * folder, and needs espeak-ng to synthesise the speech.
 */
import { execFileSync, spawnSync } from "node:child_process";
import { writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { join } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";
import { defaultModelDir, downloadModel, modelStatus, transcribe, type TranscribeResult } from "../src/index.js";
import { LOCAL_CHUNKING } from "../src/local.js";
import { hasCommand, tempDir } from "./helpers.js";

const enabled = process.env.CLIPY_TEST_MODELS === "1" && hasCommand("espeak-ng");
const modelDir = process.env.CLIPY_TEST_MODEL_DIR || defaultModelDir();
const dir = tempDir();

const SENTENCE = "The quick brown fox jumps over the lazy dog. Please open the settings page and click save.";

function speak(text: string, out: string, voice = "en-us") {
  execFileSync("espeak-ng", ["-v", voice, "-s", "150", "-w", out, text]);
}

function ff(args: string[]) {
  execFileSync("ffmpeg", ["-hide_banner", "-loglevel", "error", "-y", ...args]);
}

function words(r: TranscribeResult): string {
  return r.segments
    .map((s) => s.text)
    .join(" ")
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s]/gu, "");
}

function assertTimeline(r: TranscribeResult, totalMs: number) {
  for (let i = 0; i < r.segments.length; i++) {
    const s = r.segments[i];
    expect(s.endMs).toBeGreaterThanOrEqual(s.startMs);
    expect(s.endMs).toBeLessThanOrEqual(totalMs + 1000);
    if (i > 0) expect(s.startMs).toBeGreaterThanOrEqual(r.segments[i - 1].endMs);
  }
}

describe.runIf(enabled)("local models (CLIPY_TEST_MODELS=1)", () => {
  beforeAll(async () => {
    await downloadModel("english-fast", { modelDir });
    await downloadModel("multilingual", { modelDir });
  }, 900_000);

  it("english-fast transcribes a short spoken clip inside a video", async () => {
    const wav = join(dir, "short.wav");
    speak(SENTENCE, wav);
    const mp4 = join(dir, "short.mp4");
    ff(["-f", "lavfi", "-i", "color=c=black:s=64x64:r=5", "-i", wav, "-shortest", "-c:v", "libx264", "-pix_fmt", "yuv420p", "-c:a", "aac", mp4]);
    const t0 = Date.now();
    const r = await transcribe(mp4, { kind: "local", model: "english-fast" }, { modelDir });
    console.log(`english-fast short: ${Date.now() - t0} ms`, JSON.stringify(r));
    expect(r.engine).toBe("local:english-fast");
    expect(r.language).toBe("en");
    expect(r.gaps).toEqual([]);
    const text = words(r);
    expect(text).toContain("quick brown fox");
    expect(text).toContain("settings page");
    expect(r.segments[0].startMs).toBeLessThan(1500);
  }, 120_000);

  it("english-fast handles a 7-minute recording in pieces of at most 180 s", async () => {
    const one = join(dir, "para.wav");
    speak(`${SENTENCE} Then we check the dashboard again and confirm the numbers.`, one);
    const pause = join(dir, "pause.wav");
    ff(["-f", "lavfi", "-i", "anullsrc=r=22050:cl=mono:d=1.2", "-c:a", "pcm_s16le", pause]);
    const list = join(dir, "list.txt");
    const lines: string[] = [];
    for (let i = 0; i < 40; i++) lines.push(`file '${one}'`, `file '${pause}'`);
    writeFileSync(list, `${lines.join("\n")}\n`);
    const long = join(dir, "long.wav");
    ff(["-f", "concat", "-safe", "0", "-i", list, "-c:a", "pcm_s16le", long]);
    const progress: number[] = [];
    const t0 = Date.now();
    const r = await transcribe(long, { kind: "local", model: "english-fast" }, { modelDir, onProgress: (p) => p.stage === "transcribe" && progress.push(p.fraction) });
    const ms = Date.now() - t0;
    const totalMs = Number(execFileSync("ffprobe", ["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", long]).toString()) * 1000;
    console.log(`english-fast long: ${(totalMs / 1000).toFixed(0)} s of audio in ${ms} ms, ${r.segments.length} segments, gaps ${JSON.stringify(r.gaps)}`);
    expect(totalMs).toBeGreaterThan(400_000);
    expect(r.gaps).toEqual([]);
    assertTimeline(r, totalMs);
    const count = words(r).split("click save").length - 1;
    expect(count).toBeGreaterThanOrEqual(36); // 40 spoken; allow a few recognition slips
    expect(count).toBeLessThanOrEqual(40); // overlap must not duplicate
    expect(r.segments[r.segments.length - 1].endMs).toBeGreaterThan(totalMs - 15_000);
    expect(progress.length).toBeGreaterThan(2);
  }, 600_000);

  it("contains a native crash: an over-long piece becomes a failure, not a dead process", async () => {
    // Parakeet aborts on input this long; force a single 8-minute piece.
    const saved = { ...LOCAL_CHUNKING["english-fast"] };
    LOCAL_CHUNKING["english-fast"] = { targetSec: 600, maxSec: 600, overlapSec: 0, minSec: 15 };
    try {
      await expect(transcribe(join(dir, "long.wav"), { kind: "local", model: "english-fast" }, { modelDir })).rejects.toThrow(
        /failed on every part/,
      );
    } finally {
      LOCAL_CHUNKING["english-fast"] = saved;
    }
  }, 300_000);

  it("multilingual detects the language and covers a minute of speech in 30 s windows", async () => {
    const es = join(dir, "es.wav");
    speak("Hola, buenos días. Hoy vamos a revisar el informe de ventas y después abrimos la configuración.", es, "es");
    const r1 = await transcribe(es, { kind: "local", model: "multilingual" }, { modelDir });
    console.log("multilingual es:", JSON.stringify(r1));
    expect(r1.engine).toBe("local:multilingual");
    expect(r1.language).toBe("es");
    expect(words(r1)).toMatch(/hola|buenos|informe|ventas|configuraci/);

    const en = join(dir, "en.wav");
    speak(`${SENTENCE} ${SENTENCE} ${SENTENCE} ${SENTENCE} ${SENTENCE} ${SENTENCE}`, en);
    const t0 = Date.now();
    const r2 = await transcribe(en, { kind: "local", model: "multilingual" }, { modelDir });
    const totalMs = Number(execFileSync("ffprobe", ["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", en]).toString()) * 1000;
    console.log(`multilingual en: ${(totalMs / 1000).toFixed(0)} s in ${Date.now() - t0} ms`, JSON.stringify(r2));
    expect(totalMs).toBeGreaterThan(35_000);
    expect(r2.segments.length).toBeGreaterThanOrEqual(2);
    expect(r2.gaps).toEqual([]);
    assertTimeline(r2, totalMs);
    expect(words(r2).split("click save").length - 1).toBeGreaterThanOrEqual(4);
  }, 300_000);
});

/**
 * Quiet speech with the installed english-fast model, when one is already on
 * this machine (no download) and a speech synthesiser (espeak-ng, or macOS
 * `say`) is available.
 */
function engineResolvable(): boolean {
  try {
    createRequire(import.meta.url).resolve("sherpa-onnx-node");
    return true;
  } catch {
    return false;
  }
}

const quietEnabled =
  modelStatus("english-fast", modelDir).installed && engineResolvable() && (hasCommand("espeak-ng") || process.platform === "darwin");

describe.runIf(quietEnabled)("quiet speech with the installed english-fast model", () => {
  it("transcribes speech peaking near -31 dBFS across pieces, and reports only the silent pieces as gaps", async () => {
    const raw = join(dir, "quiet-raw.wav");
    if (hasCommand("espeak-ng")) speak(SENTENCE, raw);
    else execFileSync("say", ["-o", raw, "--data-format=LEI16@22050", SENTENCE]);
    const quiet = join(dir, "quiet.wav");
    // Lower the speech until it peaks at -31 dBFS.
    const probe = spawnSync("ffmpeg", ["-hide_banner", "-i", raw, "-af", "volumedetect", "-f", "null", "-"], { encoding: "utf8" });
    const peak = Number(/max_volume:\s*(-?[0-9.]+) dB/.exec(probe.stderr)?.[1] ?? 0);
    ff(["-i", raw, "-af", `volume=${(-31 - peak).toFixed(1)}dB`, "-ar", "16000", "-ac", "1", "-c:a", "pcm_s16le", quiet]);
    // speech, 400 s of silence, speech: four pieces, the two middle ones silent.
    const long = join(dir, "quiet-long.wav");
    ff([
      "-i", quiet, "-f", "lavfi", "-i", "anullsrc=r=16000:cl=mono:d=400", "-i", quiet,
      "-filter_complex", "[0:a][1:a][2:a]concat=n=3:v=0:a=1", "-c:a", "pcm_s16le", long,
    ]);
    const short = await transcribe(quiet, { kind: "local", model: "english-fast" }, { modelDir });
    expect(words(short)).toContain("settings page");
    expect(short.gaps).toEqual([]);
    const r = await transcribe(long, { kind: "local", model: "english-fast" }, { modelDir });
    console.log("quiet long:", JSON.stringify(r));
    expect(words(r).split("settings page").length - 1).toBe(2);
    expect(r.gaps.length).toBeGreaterThanOrEqual(1);
    expect(r.gaps.every((g) => g.reason === "no speech detected")).toBe(true);
  }, 300_000);
});
