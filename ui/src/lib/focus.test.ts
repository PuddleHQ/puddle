// SPDX-License-Identifier: GPL-3.0-or-later
import { afterEach, describe, expect, it } from "vitest";
import { settleFocus } from "./focus.ts";

afterEach(() => {
  document.body.innerHTML = "";
});

function page(html: string) {
  document.body.innerHTML = html;
}

describe("settleFocus", () => {
  it("focuses the first selector that finds something", async () => {
    page('<button id="a">a</button><button id="b">b</button>');
    await settleFocus(["#missing", "#b", "#a"], undefined);
    expect(document.activeElement?.id).toBe("b");
  });
  it("falls back when no selector finds anything", async () => {
    page('<h1 id="h" tabindex="-1">h</h1>');
    await settleFocus(["#missing"], document.getElementById("h")!);
    expect(document.activeElement?.id).toBe("h");
  });
  it("leaves focus alone when it already sits on something", async () => {
    page('<input id="x"><button id="a">a</button>');
    document.getElementById("x")!.focus();
    await settleFocus(["#a"], undefined);
    expect(document.activeElement?.id).toBe("x");
  });
  it("waits for a dialog to close first", async () => {
    page('<div role="dialog" id="d"></div><button id="a">a</button>');
    const done = settleFocus(["#a"], undefined);
    await new Promise((r) => setTimeout(r, 40));
    expect(document.activeElement?.id).not.toBe("a");
    document.getElementById("d")!.remove();
    await done;
    expect(document.activeElement?.id).toBe("a");
  });
  it("does nothing when there is nowhere to go", async () => {
    page("<p>text</p>");
    await settleFocus(["#none"], undefined);
    expect(document.activeElement).toBe(document.body);
  });
});
