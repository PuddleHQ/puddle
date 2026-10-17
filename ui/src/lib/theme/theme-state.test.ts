// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";

const api = await vi.hoisted(async () => {
  const mod = await import("#lib/testing/fake-settings.ts");
  return new mod.FakeSettings();
});
vi.mock("#lib/api/client.ts", () => ({ api }));

import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { theme } from "./theme.svelte.ts";

beforeEach(() => {
  api.reset();
  globalSettings.view = null;
  localStorage.clear();
  delete document.documentElement.dataset["theme"];
  theme.choice = "system";
});

describe("the theme and the settings", () => {
  it("applies the choice stored in the settings, whatever the page remembered", async () => {
    localStorage.setItem("puddle.theme", "light");
    theme.init();
    expect(document.documentElement.dataset["theme"]).toBe("light");
    api.ui.theme = "dark";
    await theme.sync();
    expect(theme.choice).toBe("dark");
    expect(document.documentElement.dataset["theme"]).toBe("dark");
    expect(localStorage.getItem("puddle.theme")).toBe("dark");
  });

  it("an unset choice follows the system, and an unreadable service keeps the hint", async () => {
    localStorage.setItem("puddle.theme", "dark");
    theme.init();
    api.down = true;
    await theme.sync();
    expect(theme.choice).toBe("dark");
    api.down = false;
    await theme.sync();
    expect(theme.choice).toBe("system");
    expect(document.documentElement.dataset["theme"]).toBeUndefined();
  });

  it("keeps a choice made while the settings are still being read", async () => {
    theme.init();
    const read = theme.sync();
    theme.preview("dark");
    await read;
    expect(theme.choice).toBe("dark");
    expect(document.documentElement.dataset["theme"]).toBe("dark");

    api.ui.theme = "dark";
    globalSettings.view = null;
    const reread = theme.sync();
    const saved = theme.set("light");
    await reread;
    expect(await saved).toEqual({ ok: true });
    expect(theme.choice).toBe("light");
    expect(document.documentElement.dataset["theme"]).toBe("light");
  });

  it("stores a new choice through the API, loading the settings first when needed", async () => {
    expect(await theme.set("light")).toEqual({ ok: true });
    expect(api.ui.theme).toBe("light");
    expect(theme.choice).toBe("light");
  });

  it("applies a choice even when it can't be stored", async () => {
    api.down = true;
    expect(await theme.set("dark")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
    expect(theme.choice).toBe("dark");
    expect(document.documentElement.dataset["theme"]).toBe("dark");
  });
});
