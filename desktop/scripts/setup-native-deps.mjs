// @ts-check

/**
 * macOS: fetches the prebuilt FFmpeg framework the recorder links against
 * and unpacks it into `src-tauri/target/native-deps/`.
 *
 * The archive is Spacedrive's `native-deps` release (pinned version, SHA-256
 * checked before it is unpacked). Two places read the result:
 *   - `bundle.macOS.frameworks` in `tauri.conf.json` copies the framework
 *     into the app bundle;
 *   - `FFMPEG_DIR` in `src-tauri/.cargo/config.toml` points ffmpeg-sys-next
 *     at its headers and libraries when compiling.
 *
 * Runs again only when the pinned version or architecture changes (a
 * `.version` file records the last unpack). Other platforms: nothing to do.
 */

import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { existsSync } from "node:fs";
import { mkdir, readdir, readFile, rm, writeFile } from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const run = promisify(execFile);
/** @param {string} msg */
const log = (msg) => console.log(`[setup-native-deps] ${msg}`);

const RELEASE = "v0.25";
const BASE_URL = `https://github.com/spacedriveapp/native-deps/releases/download/${RELEASE}`;
/** @type {Record<string, { name: string, sha256: string }>} Archive name and SHA-256 per CPU. */
const ARCHIVES = {
  aarch64: {
    name: "native-deps-aarch64-darwin-apple.tar.xz",
    sha256: "59c42dbf4dd2009c1d04c049df8d8e25f79fd2d245531e3d269d83407116c895",
  },
  x86_64: {
    name: "native-deps-x86_64-darwin-apple.tar.xz",
    sha256: "138f2bab8214d8971d043c1d42eb09039743b144c07a33fc6c76f774414407ce",
  },
};
/** Only FFmpeg's own libraries and headers are kept from the framework. */
const KEEP = /^(libav|libsw|libpostproc)/;

const srcTauri = resolve(dirname(fileURLToPath(import.meta.url)), "..", "src-tauri");
const targetDir = join(srcTauri, "target");
const outDir = join(targetDir, "native-deps");
const marker = join(outDir, ".version");

function cpu() {
  const fromTriple = process.env.RUST_TARGET_TRIPLE?.split("-")[0];
  if (fromTriple) return fromTriple;
  return process.arch === "arm64" ? "aarch64" : "x86_64";
}

/** @param {string} file */
async function sha256(file) {
  return createHash("sha256").update(await readFile(file)).digest("hex");
}

/** @param {string} url @param {string} dest */
async function download(url, dest) {
  log(`downloading ${url}`);
  const res = await fetch(url);
  if (!res.ok) throw new Error(`download failed: HTTP ${res.status}`);
  const body = Buffer.from(await res.arrayBuffer());
  await writeFile(dest, body);
  log(`downloaded ${(body.length / 1048576).toFixed(1)} MB`);
}

/** Removes everything in `dir` whose name does not match KEEP. @param {string} dir */
async function prune(dir) {
  if (!existsSync(dir)) return;
  for (const name of await readdir(dir)) {
    if (!KEEP.test(name)) await rm(join(dir, name), { recursive: true, force: true });
  }
}

/** macOS refuses to load unsigned dylibs; an ad-hoc signature is enough for local builds. @param {string} dir */
async function signDylibs(dir) {
  if (!existsSync(dir)) return;
  const identity = process.env.APPLE_SIGNING_IDENTITY || "-";
  const keychain = process.env.APPLE_KEYCHAIN ? ["--keychain", process.env.APPLE_KEYCHAIN] : [];
  const entries = await readdir(dir, { recursive: true, withFileTypes: true });
  const dylibs = entries.filter((e) => e.isFile() && e.name.endsWith(".dylib")).map((e) => join(e.parentPath, e.name));
  await Promise.all(
    dylibs.map((file) =>
      run("codesign", [...keychain, "-s", identity, "-f", file]).catch((err) => {
        console.warn(`[setup-native-deps] could not sign ${file}: ${err.message}`);
      }),
    ),
  );
}

async function main() {
  if (process.platform !== "darwin") {
    log(`nothing to do on ${process.platform}`);
    return;
  }
  const arch = cpu();
  const archive = ARCHIVES[arch];
  if (!archive) throw new Error(`no native-deps archive for ${arch}`);

  const stamp = `${RELEASE}-${arch}`;
  if (existsSync(marker) && (await readFile(marker, "utf8")) === stamp) {
    log(`up to date (${stamp})`);
    return;
  }

  await mkdir(targetDir, { recursive: true });
  const archivePath = join(targetDir, archive.name);
  if (!existsSync(archivePath) || (await sha256(archivePath)) !== archive.sha256) {
    await download(`${BASE_URL}/${archive.name}`, archivePath);
  }
  const actual = await sha256(archivePath);
  if (actual !== archive.sha256) {
    await rm(archivePath, { force: true });
    throw new Error(`checksum mismatch for ${archive.name}: expected ${archive.sha256}, got ${actual}`);
  }

  await rm(outDir, { recursive: true, force: true });
  await mkdir(outDir, { recursive: true });
  log(`unpacking into ${outDir}`);
  await run("tar", ["-xf", archivePath, "-C", outDir]);

  // The archive's top-level lib/ and include/ are symlinks into this
  // framework, so it keeps its upstream name.
  const framework = join(outDir, "Spacedrive.framework");
  await prune(join(framework, "Libraries"));
  await prune(join(framework, "Headers"));
  await rm(join(framework, "Resources", "Models"), { recursive: true, force: true });
  await signDylibs(join(framework, "Libraries"));

  await writeFile(marker, stamp);
  log(`done (${stamp})`);
}

main().catch((err) => {
  console.error(`[setup-native-deps] ${err.message}`);
  process.exit(1);
});
