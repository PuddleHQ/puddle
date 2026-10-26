// SPDX-License-Identifier: GPL-3.0-or-later
// The inline script in app.html applies the density hint before the first style resolution. It
// can't import density.ts, so this runs it and checks it does what loadDensity + applyDensity do.
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";
import { applyDensity, DENSITY_KEY, loadDensity } from "./density.ts";

const page = readFileSync(
  join(import.meta.dirname, "..", "..", "app.html"),
  "utf8",
);
const scripts = [...page.matchAll(/<script>([\s\S]*?)<\/script>/g)].map(
  (m) => m[1] ?? "",
);

type Storage = { getItem: (key: string) => string | null };

function stored(value: string | undefined): Storage {
  return { getItem: (key) => (key === DENSITY_KEY ? (value ?? null) : null) };
}

const unreadable: Storage = {
  getItem: () => {
    throw new Error("blocked");
  },
};

function runHint(storage: Storage): HTMLElement {
  const root = document.createElement("html");
  const fakeDocument = { documentElement: root };
  for (const script of scripts) {
    new Function("localStorage", "document", script)(storage, fakeDocument);
  }
  return root;
}

function expected(storage: Storage): HTMLElement {
  const root = document.createElement("html");
  applyDensity(root, loadDensity(storage as globalThis.Storage));
  return root;
}

describe("the density hint in app.html", () => {
  it("is one inline script", () => {
    expect(scripts).toHaveLength(1);
  });

  it.each([
    ["compact", stored("compact")],
    ["comfortable", stored("comfortable")],
    ["an unknown value", stored("tight")],
    ["no hint", stored(undefined)],
    ["unreadable storage", unreadable],
  ])("with %s, marks <html> as the app does once it starts", (_, storage) => {
    expect(runHint(storage).dataset["density"]).toBe(
      expected(storage).dataset["density"],
    );
  });

  it("marks compact", () => {
    expect(runHint(stored("compact")).dataset["density"]).toBe("compact");
  });
});
