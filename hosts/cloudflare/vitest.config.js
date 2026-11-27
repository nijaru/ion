import { fileURLToPath } from "node:url";
import { cloudflareTest } from "@cloudflare/vitest-plugin";
import { defineConfig } from "vitest/config";

export default defineConfig({
  plugins: [cloudflareTest({
    wrangler: { configPath: fileURLToPath(new URL("./test/wrangler.jsonc", import.meta.url)) },
  })],
  test: {
    include: process.env.ION_WORKER_BRIDGE === "1" ? ["test/*.test.js"] : ["test/journal.test.js"],
  },
});
