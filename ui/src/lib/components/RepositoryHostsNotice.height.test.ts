// SPDX-License-Identifier: AGPL-3.0-or-later
// Its own file: Svelte's `bind:clientHeight` makes one ResizeObserver on first use and keeps it, so
// the observer stub below has to be in place before any other test renders the component.
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { RepositoryHost } from "#lib/workspaces/hosts.ts";
import RepositoryHostsNotice from "./RepositoryHostsNotice.svelte";

afterEach(cleanup);

const host = (
  name: string,
  state: RepositoryHost["state"],
): RepositoryHost => ({
  host: name,
  state,
  blocked_by: null,
});

describe("RepositoryHostsNotice height", () => {
  const original = globalThis.ResizeObserver;
  let fire: () => void = () => {};
  let natural = 0;

  afterEach(() => {
    globalThis.ResizeObserver = original;
    Reflect.deleteProperty(HTMLElement.prototype, "clientHeight");
  });

  it("never gets shorter once it has been taller, so no row above it moves", async () => {
    globalThis.ResizeObserver = class {
      constructor(callback: (entries: unknown[]) => void) {
        fire = () => callback([{ target: document.querySelector(".slot") }]);
      }
      observe() {}
      unobserve() {}
      disconnect() {}
    } as unknown as typeof ResizeObserver;
    // jsdom has no layout: the slot is as tall as its notes (63 px for two lines) unless the
    // component has set a minimum.
    Object.defineProperty(HTMLElement.prototype, "clientHeight", {
      configurable: true,
      get(this: HTMLElement) {
        const floor = Number.parseFloat(this.style.minHeight) || 0;
        return this.classList.contains("slot") ? Math.max(natural, floor) : 0;
      },
    });
    const answers = [
      [host("github.com", "new"), host("gitlab.com", "new")],
      [host("github.com", "new")],
    ];
    const check = vi.fn(async () => answers.shift() ?? null);
    const { rerender } = render(RepositoryHostsNotice, {
      props: {
        urls: ["https://github.com/a/b.git", "https://gitlab.com/a/b.git"],
        check,
        delay: 0,
      },
    } as never);
    await screen.findByTestId("hosts-new");
    natural = 63;
    fire();
    const slot = document.querySelector<HTMLElement>(".slot");
    await vi.waitFor(() => expect(slot?.style.minHeight).toBe("63px"));

    await rerender({ urls: ["https://github.com/a/b.git"] });
    await vi.waitFor(() => expect(check).toHaveBeenCalledTimes(2));
    natural = 42;
    fire();
    await vi.waitFor(() =>
      expect(screen.getByTestId("hosts-new")).not.toHaveTextContent("gitlab"),
    );
    expect(slot?.style.minHeight).toBe("63px");
  });
});
