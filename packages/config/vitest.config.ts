import { fileURLToPath } from "node:url";
import { defineConfig } from "vitest/config";

export default defineConfig({
  // Tests run against @clipy/moments' source, so they work before it is built.
  resolve: {
    alias: { "@clipy/moments": fileURLToPath(new URL("../moments/src/index.ts", import.meta.url)) },
  },
  test: { include: ["test/**/*.test.ts"], environment: "node" },
});
