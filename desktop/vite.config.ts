import { readdirSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { defineConfig, type Plugin } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "tailwindcss";
import autoprefixer from "autoprefixer";
import tailwindConfig from "./tailwind.config.js";

const root = dirname(fileURLToPath(import.meta.url));
const src = join(root, "src");

/**
 * Two editions build from this one source:
 *
 * - the official app, `vite build` (any mode but `oss`);
 * - the local edition, `vite build --mode oss`: no Clipy account, no upload,
 *   no updater, Local recording only.
 *
 * The edition reaches the code as literal constants (`src/lib/edition.ts`),
 * so every edition branch folds away at build time and the official bundle
 * is byte for byte what it was before the local edition existed.
 */
const LOCAL_MODE = "oss";

/** Local edition stubs live here. The official build never imports them. */
const LOCAL_DIR = "edition-local";

/**
 * Cloud-only modules the local edition replaces with a small stub, keyed by
 * path under `src/`. The stubs keep the same exports (their types are checked
 * against the real files), so shared code needs no import changes, and the
 * real files can stay out of the open-source export. NativeStudio and the
 * Studio helpers are why the local build never reaches the monorepo's
 * `lib/animation-editor`.
 */
const LOCAL_STUBS: Record<string, string> = {
  "routes/Login.tsx": "Login.tsx",
  "routes/ShareLink.tsx": "ShareLink.tsx",
  "routes/ProGateCard.tsx": "ProGateCard.tsx",
  "routes/NativeStudio.tsx": "NativeStudio.tsx",
  "components/UpgradePrompt.tsx": "UpgradePrompt.tsx",
  "lib/studioScreenCharmUi.ts": "studioScreenCharmUi.ts",
};

const stubTargets = new Map(
  Object.entries(LOCAL_STUBS).map(([from, to]) => [
    resolve(src, from),
    resolve(src, LOCAL_DIR, to),
  ]),
);

/**
 * `resolve.alias` keyed by resolved file rather than by import specifier, so
 * `./routes/Login`, `./Login` and `../routes/Login` all land on the stub.
 */
function localEditionStubs(): Plugin {
  return {
    name: "clipy-local-edition-stubs",
    enforce: "pre",
    async resolveId(source, importer, options) {
      if (!importer || !source.startsWith(".")) return null;
      const resolved = await this.resolve(source, importer, { ...options, skipSelf: true });
      if (!resolved) return null;
      const stub = stubTargets.get(resolved.id.split("?")[0]);
      return stub ? { ...resolved, id: stub } : null;
    },
  };
}

/**
 * Tailwind reads class names from these files. The official build skips the
 * local edition's own folder, so the stubs and the provider settings add no
 * CSS to it; the local build reads everything.
 */
function tailwindContent(local: boolean): string[] {
  const shared = readdirSync(src, { withFileTypes: true }).flatMap((entry) => {
    if (entry.isDirectory()) {
      return entry.name === LOCAL_DIR && !local ? [] : [`./src/${entry.name}/**/*.{ts,tsx}`];
    }
    return /\.tsx?$/.test(entry.name) ? [`./src/${entry.name}`] : [];
  });
  return tailwindConfig.content.flatMap((pattern: string) =>
    pattern === "./src/**/*.{ts,tsx}" ? shared : [pattern],
  );
}

// https://tauri.app/v2/reference/config — Tauri expects devUrl on 1420 by default
export default defineConfig(({ mode }) => {
  const local = mode === LOCAL_MODE;
  return {
    plugins: local ? [localEditionStubs(), react()] : [react()],
    define: {
      __CLIPY_EDITION__: JSON.stringify(local ? "local" : "cloud"),
      __CLIPY_LOCAL_EDITION__: JSON.stringify(local),
    },
    css: {
      // Same plugins as postcss.config.js, with the edition's content list.
      postcss: {
        plugins: [tailwindcss({ ...tailwindConfig, content: tailwindContent(local) }), autoprefixer()],
      },
    },
    resolve: {
      // The screenshot annotation window imports the web editor's hook from
      // ../components. Without this, that file would resolve React from the
      // repository root and the hook would run against a second React copy.
      dedupe: ["react", "react-dom"],
      // The shared translation catalog (docs/i18n.md, "Desktop app").
      alias: {
        "@clipy/i18n": resolve(root, "src/vendor/i18n"),
      },
    },
    clearScreen: false,
    server: {
      port: 1420,
      strictPort: true,
      host: "127.0.0.1",
      // Hot reload via Vite only; Tauri polls the devUrl on boot
      hmr: {
        protocol: "ws",
        host: "127.0.0.1",
        port: 1421,
      },
    },
    envPrefix: ["VITE_", "TAURI_"],
    build: {
      target: "safari15",
      minify: "esbuild",
      sourcemap: false,
    },
  };
});
