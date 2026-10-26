// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, describe, expect, it, vi } from "vitest";

const api = await vi.hoisted(async () => {
  const mod = await import("#lib/testing/fake-settings.ts");
  return new mod.FakeSettings();
});
vi.mock("#lib/api/client.ts", () => ({ api }));

import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { NO_CUSTOM, PALETTE_KEY } from "./palette.ts";
import { palette } from "./palette.svelte.ts";

const root = document.documentElement;

beforeEach(() => {
  api.reset();
  globalSettings.view = null;
  localStorage.clear();
  delete root.dataset["palette"];
  root.removeAttribute("style");
  palette.choice = "pond";
  palette.custom = { ...NO_CUSTOM };
});

describe("the palette and the settings", () => {
  it("applies the palette stored in the settings, whatever the page remembered", async () => {
    localStorage.setItem(
      PALETTE_KEY,
      JSON.stringify({ choice: "ember", custom: NO_CUSTOM }),
    );
    palette.init();
    expect(root.dataset["palette"]).toBe("ember");
    api.ui.palette = "custom";
    api.ui.custom_accent = "#7a1fa2";
    await palette.sync();
    expect(palette.choice).toBe("custom");
    expect(palette.custom.accent).toBe("#7a1fa2");
    expect(root.style.getPropertyValue("--color-accent")).toContain("#7a1fa2");
    expect(JSON.parse(localStorage.getItem(PALETTE_KEY) ?? "{}").choice).toBe(
      "custom",
    );
  });

  it("an unset palette is Pond, and an unreadable service keeps the hint", async () => {
    localStorage.setItem(
      PALETTE_KEY,
      JSON.stringify({ choice: "dusk", custom: NO_CUSTOM }),
    );
    palette.init();
    api.down = true;
    await palette.sync();
    expect(palette.choice).toBe("dusk");
    api.down = false;
    await palette.sync();
    expect(palette.choice).toBe("pond");
    expect(root.dataset["palette"]).toBeUndefined();
  });

  it("keeps a palette picked while the settings are still being read", async () => {
    palette.init();
    const read = palette.sync();
    palette.preview("graphite", NO_CUSTOM);
    await read;
    expect(palette.choice).toBe("graphite");
    expect(root.dataset["palette"]).toBe("graphite");
  });

  it("stores a new choice through the API, loading the settings first when needed", async () => {
    expect(await palette.set("meadow")).toEqual({ ok: true });
    expect(api.ui.palette).toBe("meadow");
    expect(palette.choice).toBe("meadow");
  });

  it("applies a choice even when it can't be stored", async () => {
    api.down = true;
    expect(await palette.set("dusk")).toEqual({
      ok: false,
      message: "Puddle's service isn't answering.",
    });
    expect(palette.choice).toBe("dusk");
    expect(root.dataset["palette"]).toBe("dusk");
  });

  it("a settings value it does not know counts as Pond", async () => {
    (api.ui as Record<string, unknown>)["palette"] = "neon";
    await palette.sync();
    expect(palette.choice).toBe("pond");
  });
});
