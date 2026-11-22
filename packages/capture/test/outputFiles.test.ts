import { mkdtempSync, existsSync, statSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { probe, removeFailedOutput } from "../src/capture.js";
import { FAKE_FFPROBE } from "./fakeTools.js";

const dir = mkdtempSync(join(tmpdir(), "clipy-capture-out-"));

describe("removeFailedOutput", () => {
  it("leaves a file the capture never wrote", () => {
    const path = join(dir, "mine.mp4");
    writeFileSync(path, "the user's own recording");
    removeFailedOutput(path, statSync(path));
    expect(existsSync(path)).toBe(true);
  });

  it("removes a file the failed capture created or rewrote", () => {
    const created = join(dir, "created.mp4");
    writeFileSync(created, "");
    removeFailedOutput(created, null);
    expect(existsSync(created)).toBe(false);

    const rewritten = join(dir, "rewritten.mp4");
    writeFileSync(rewritten, "old content");
    const before = statSync(rewritten);
    writeFileSync(rewritten, "");
    removeFailedOutput(rewritten, before);
    expect(existsSync(rewritten)).toBe(false);
  });

  it("does nothing when there is no file", () => {
    expect(() => removeFailedOutput(join(dir, "absent.mp4"), null)).not.toThrow();
  });
});

describe("probe", () => {
  it("passes the file as file:<absolute path>, so a colon in its name is not a protocol", async () => {
    const seen: string[][] = [];
    const run = async (_cmd: string, args: string[]) => {
      seen.push(args);
      return { status: 0, stdout: JSON.stringify({ format: { duration: "1.5" }, streams: [] }), stderr: "" };
    };
    const info = await probe("take2:final.mp4", run, { ...process.env, CLIPY_FFPROBE: FAKE_FFPROBE });
    expect(info?.durationMs).toBe(1500);
    expect(seen[0][seen[0].length - 1]).toBe(`file:${join(process.cwd(), "take2:final.mp4")}`);
  });
});
