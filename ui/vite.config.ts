// SPDX-License-Identifier: GPL-3.0-or-later
import adapter from "@sveltejs/adapter-static";
import { sveltekit } from "@sveltejs/kit/vite";
import { defineConfig } from "vitest/config";
import { devProxy } from "./tools/dev-proxy.ts";
import { shippedPackages } from "./tools/shipped-packages.ts";

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
          exclude: ["src/routes/**", "src/lib/components/**"],
          setupFiles: ["./vitest.globals.ts"],
        },
      },
    ],
    coverage: {
      provider: "v8",
      include: ["src/**/*.{ts,svelte}", "tools/**/*.ts"],
      exclude: [
        "src/**/*.test.ts",
        "tools/**/*.test.ts",
        "scripts/**/*.test.ts",
        "src/lib/api/schema.d.ts",
        "src/lib/testing/**",
        "src/app.d.ts",
      ],
      reporter: ["text-summary", "text", "lcov"],
      thresholds: {
        lines: 85,
        branches: 80,
        functions: 85,
        statements: 85,
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
