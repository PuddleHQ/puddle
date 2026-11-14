// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import type { PortEntry } from "#lib/workspaces/ports.ts";
import { hoverText, judgeLink } from "./links.ts";

const port = (
  guest: number,
  host: number | null,
  forwarded = true,
): PortEntry => ({
  guest_port: guest,
  host_port: host,
  forwarded,
  listening: true,
  pinned: false,
  problem: null,
});

describe("judgeLink", () => {
  it("opens http and https addresses, as the browser reads them", () => {
    expect(judgeLink("https://example.com/a b", [])).toEqual({
      kind: "open",
      url: "https://example.com/a%20b",
    });
    expect(judgeLink("http://example.com", [])).toEqual({
      kind: "open",
      url: "http://example.com/",
    });
  });

  it("opens nothing else", () => {
    for (const text of [
      "file:///etc/passwd",
      "javascript:alert(1)",
      "ssh://h",
      "nope",
      "",
    ]) {
      expect(judgeLink(text, [])).toEqual({ kind: "ignore" });
    }
  });

  it("opens an address on this computer only when the same port is forwarded to the same port", () => {
    const forwards = [
      port(3000, 3000),
      port(4000, 4001),
      port(5000, 5000, false),
    ];
    expect(judgeLink("http://localhost:3000/x", forwards).kind).toBe("open");
    expect(judgeLink("http://127.0.0.1:3000", forwards).kind).toBe("open");
    expect(judgeLink("http://[::1]:3000", forwards).kind).toBe("open");
    expect(judgeLink("http://localhost:4000", forwards)).toEqual({
      kind: "forward",
      port: 4000,
      url: "http://localhost:4000/",
    });
    expect(judgeLink("http://localhost:5000", forwards).kind).toBe("forward");
    expect(judgeLink("http://LOCALHOST:6000", []).kind).toBe("forward");
  });

  it("treats every loopback and unspecified spelling as this computer", () => {
    for (const text of [
      "http://0.0.0.0:3000/",
      "http://127.0.0.2:3000/",
      "http://127.255.255.254:3000/",
      "http://0x7f000001:3000/",
      "http://localhost.:3000/",
      "http://x.localhost:3000/",
      "http://[::]:3000/",
      "http://[::ffff:127.0.0.1]:3000/",
      "http://[::ffff:0.0.0.0]:3000/",
    ]) {
      expect(judgeLink(text, []), text).toEqual({
        kind: "forward",
        port: 3000,
        url: new URL(text).href,
      });
      expect(judgeLink(text, [port(3000, 3000)]).kind, text).toBe("open");
    }
  });

  it("treats names and addresses that only look local as outside", () => {
    for (const text of [
      "http://127.example.com/",
      "http://localhost.example.com/",
      "http://10.0.0.1/",
      "http://[::ffff:10.0.0.1]/",
      "http://[::ffff:128.0.0.1]/",
    ]) {
      expect(judgeLink(text, []).kind, text).toBe("open");
    }
  });

  it("reads a missing port as the scheme's own", () => {
    expect(judgeLink("http://localhost/", [port(80, 80)]).kind).toBe("open");
    expect(judgeLink("https://localhost/", [port(80, 80)])).toEqual({
      kind: "forward",
      port: 443,
      url: "https://localhost/",
    });
  });
});

describe("hoverText", () => {
  it("shows the address, and what to do first for a port that is not forwarded", () => {
    expect(hoverText({ kind: "open", url: "https://a.b/" })).toBe(
      "https://a.b/",
    );
    expect(
      hoverText({ kind: "forward", port: 3000, url: "http://localhost:3000/" }),
    ).toBe(
      "http://localhost:3000/ (forward port 3000 on the Ports tab to open it)",
    );
    expect(hoverText({ kind: "ignore" })).toBeNull();
  });
});
