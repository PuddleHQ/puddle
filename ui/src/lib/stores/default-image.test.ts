// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
import { DefaultImage } from "./default-image.svelte.ts";

const store = (get: () => Promise<unknown>) =>
  new DefaultImage({ GET: vi.fn(get) } as never);

describe("DefaultImage", () => {
  it("keeps the line the host read", async () => {
    const image = store(async () => ({
      data: { image: "x", description: "Debian trixie dev container" },
    }));
    await image.load();
    expect(image.description).toBe("Debian trixie dev container");
    expect(image.image).toBe("x");
  });

  it("keeps what it had when a read fails", async () => {
    let fail = false;
    const image = store(async () => {
      if (fail) throw new Error("down");
      return { data: { image: "x", description: "A line" } };
    });
    await image.load();
    fail = true;
    await image.load();
    expect(image.description).toBe("A line");
  });
});
