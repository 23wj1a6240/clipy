#!/usr/bin/env node
/**
 * This package's type check and tests read @clipy/moments from source, and
 * that source imports files the sync script generates. Generate them only when
 * they are missing: when moments' own scripts already ran (as in `pnpm -r`,
 * which runs moments first), they are left alone, so nothing rewrites them
 * while another package reads them.
 */
import { execFileSync } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const generated = join(here, "..", "..", "moments", "src", "generated", "localMomentPlan.ts");
if (!existsSync(generated)) {
  execFileSync(process.execPath, [join(here, "..", "..", "..", "scripts", "sync-from-monorepo.mjs")], { stdio: "inherit" });
}
