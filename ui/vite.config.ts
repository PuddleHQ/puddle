// SPDX-License-Identifier: GPL-3.0-or-later
import { readFileSync } from "node:fs";
import adapter from "@sveltejs/adapter-static";
import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vitest/config";
import { devProxy } from "./tools/dev-proxy.ts";
import { shippedPackages } from "./tools/shipped-packages.ts";

// The best coverage reached so far (scripts/ui-coverage-ratchet.ts raises it, never lowers it): the
// thresholds below, so a run under any of the four fails.
const baseline = JSON.parse(
  readFileSync(new URL("./coverage-baseline.json", import.meta.url), "utf8"),
) as Record<"lines" | "branches" | "functions" | "statements", number>;

export default defineConfig({
  plugins: [
    sveltekit({
      // A single-page app: every route is answered by index.html and rendered in the browser
      // (src/routes/+layout.ts turns SSR off), so puddle-api can serve it as plain files.
      adapter: adapter({ fallback: "index.html" }),
    }),
    shippedPackages(),
  ],
  server: { proxy: devProxy() },
  test: {
    environment: "jsdom",
    // Every test file still gets its own jsdom and module graph, but inside one VM context per file
    // in a long-lived worker thread, so the worker, jsdom and the Node modules load once, not per file.
    // vitest.globals.ts adds the Node globals a bare jsdom context lacks.
    pool: "vmThreads",
    // Two projects over one config. The warm-up and the testing-library setup cost about a sixth of a
    // CPU second per test file, so only the files that draw components run them; the stores, models
    // and tools tests get the globals alone. Both keep jsdom: a plain Node environment would compile
    // the Svelte stores for the server, not as they ship.
    projects: [
      {
        extends: true,
        test: {
          name: "dom",
          include: [
            "src/routes/**/*.test.ts",
            "src/lib/components/**/*.test.ts",
          ],
          setupFiles: ["./vitest.globals.ts", "./vitest.setup.ts"],
        },
      },
      {
        extends: true,
        test: {
          name: "logic",
          include: [
            "src/**/*.test.ts",
            "tools/**/*.test.ts",
            "scripts/**/*.test.ts",
          ],
          // A test file in any new directory lands here by default.
          exclude: ["src/routes/**", "src/lib/components/**"],
          setupFiles: ["./vitest.globals.ts"],
        },
      },
    ],
    coverage: {
      provider: "v8",
      include: ["src/**/*.{ts,svelte}", "tools/**/*.ts", "scripts/**/*.ts"],
      exclude: [
        "src/**/*.test.ts",
        "tools/**/*.test.ts",
        "scripts/**/*.test.ts",
        "src/lib/api/schema.d.ts",
        "src/lib/testing/**",
        "src/app.d.ts",
        // Process glue that starts a dev server, a fixture binary or a browser; the e2e gate runs them.
        "scripts/dev-fixture.ts",
        "scripts/fixture-binary.ts",
        "scripts/webkit-probe.ts",
      ],
      reporter: ["text-summary", "text", "lcov", "json-summary"],
      thresholds: {
        ...baseline,
        "src/lib/**": { lines: 100, functions: 100, statements: 100 },
        "src/lib/api/**": { lines: 95, branches: 90 },
        "src/lib/decision/**": { lines: 95, branches: 90 },
        "src/lib/rules/**": { lines: 95, branches: 90 },
        "src/lib/audit/**": { lines: 95, branches: 90 },
        "src/lib/workspaces/**": { lines: 95, branches: 90 },
        "src/lib/settings/**": { lines: 95, branches: 90 },
        "src/lib/network/**": { lines: 95, branches: 90 },
        "src/lib/notify/**": { lines: 95, branches: 90 },
        "src/lib/identities/**": { lines: 95, branches: 90 },
        "src/lib/repos/**": { lines: 95, branches: 90 },
      },
    },
  },
  resolve: process.env["VITEST"] ? { conditions: ["browser"] } : {},
});
