import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const tauriDir = fileURLToPath(new URL("../src-tauri/", import.meta.url));

test("native Speech entry points reject unsafe hosts before calling Apple", {
  skip: process.platform !== "darwin",
}, (t) => {
  const root = mkdtempSync(path.join(tmpdir(), "clipy-speech-"));
  t.after(() => rmSync(root, { recursive: true, force: true }));
  const driver = path.join(root, "driver.m");
  writeFileSync(driver, `
#import <Foundation/Foundation.h>
#import <Speech/Speech.h>
#import <objc/runtime.h>
extern char *clipy_local_native(const char *json);
extern void clipy_local_free(char *value);
extern char *clipy_stt_open(double rate, unsigned int channels);
// Never prompt or change permissions in a regression test. Exit distinctly if
// an unsafe host reaches the API that caused the reported process termination.
static void unexpectedAuthorization(id self, SEL cmd, id handler) { _exit(86); }
int main(int argc, const char **argv) {
    @autoreleasepool {
        method_setImplementation(class_getClassMethod(SFSpeechRecognizer.class,
            @selector(requestAuthorization:)), (IMP)unexpectedAuthorization);
        char *result = strcmp(argv[1], "live") == 0
            ? clipy_stt_open(48000, 1) : clipy_local_native(argv[1]);
        puts(result ?: "{}");
        clipy_local_free(result);
    }
    return 0;
}
`);
  const binary = path.join(root, "speech-probe");
  const build = spawnSync("xcrun", ["clang", "-fobjc-arc", "-Wno-deprecated-declarations",
    driver, path.join(tauriDir, "src/local/apple.m"), path.join(tauriDir, "src/local/apple_stream.m"),
    "-framework", "Foundation", "-framework", "Speech", "-framework", "AVFoundation",
    "-framework", "AVFAudio", "-framework", "CoreMedia", "-framework", "ImageIO",
    "-framework", "CoreGraphics", "-Wl,-sectcreate,__TEXT,__info_plist," + path.join(tauriDir, "Info.plist"),
    "-o", binary,
  ], { encoding: "utf8" });
  assert.equal(build.status, 0, build.stderr);
  const run = (executable, input) => {
    const result = spawnSync(executable, [input], { encoding: "utf8", timeout: 5000 });
    assert.equal(result.status, 0, `Unexpected exit ${result.status}/${result.signal}: ${result.stderr}`);
    return JSON.parse(result.stdout);
  };
  const inputs = [
    '{"op":"check","authorize":true}',
    '{"op":"check","authorize":false}',
    '{"op":"transcribe","video":"/missing.mp4"}',
    "live",
  ];
  for (const input of inputs) {
    assert.match(run(binary, input).error, /Open Clipy from its application bundle/, input);
  }
  for (const [name, value] of [
    ["missing", ""],
    ["empty", "<key>NSSpeechRecognitionUsageDescription</key><string> </string>"],
    ["wrong-type", "<key>NSSpeechRecognitionUsageDescription</key><true/>"],
    ["valid", "<key>NSSpeechRecognitionUsageDescription</key><string>Transcribe locally.</string>"],
  ]) {
    const contents = path.join(root, `${name}.app`, "Contents");
    mkdirSync(path.join(contents, "MacOS"), { recursive: true });
    const executable = path.join(contents, "MacOS", "speech-probe");
    copyFileSync(binary, executable);
    writeFileSync(path.join(contents, "Info.plist"), `<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>online.clipy.speech-test.${name}</string>
<key>CFBundleExecutable</key><string>speech-probe</string>
<key>CFBundlePackageType</key><string>APPL</string>${value}
</dict></plist>`);
    if (name === "valid") {
      // Capability lookup does not request permission. It must get past the
      // configuration guard even on hosts without on-device language support.
      const result = run(executable, '{"op":"check","authorize":false}');
      assert.ok(result.onDevice === true || /on-device transcription is unavailable/.test(result.error), JSON.stringify(result));
    } else {
      for (const input of inputs) {
        assert.match(run(executable, input).error, /missing its Speech Recognition privacy description/, `${name}: ${input}`);
      }
    }
  }
});
