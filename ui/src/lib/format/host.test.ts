// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { guessRegistrable, hostParts } from "./host.ts";

describe("hostParts", () => {
  it("bolds the registrable domain and keeps the labels before it apart", () => {
    expect(hostParts("api.stripe.com", { port: 443 })).toEqual({
      wildcard: false,
      lead: "api.",
      main: "stripe.com",
      port: 443,
      usualPort: true,
    });
  });

  it("marks a wildcard and reads a port written in the value", () => {
    expect(hostParts("*.github.com:8443")).toEqual({
      wildcard: true,
      lead: "",
      main: "github.com",
      port: 8443,
      usualPort: false,
    });
  });

  it("trusts the registrable domain the service gave", () => {
    expect(
      hostParts("a.b.example.org", { registrable: "b.example.org" }),
    ).toMatchObject({
      lead: "a.",
      main: "b.example.org",
    });
  });

  it("keeps an IP literal whole and shows no port for an SRV lookup", () => {
    expect(hostParts("10.0.0.5", { port: 0 })).toMatchObject({
      lead: "",
      main: "10.0.0.5",
      port: null,
    });
  });

  it("guesses three labels under a country second level", () => {
    expect(guessRegistrable("www.example.co.uk")).toBe("example.co.uk");
    expect(guessRegistrable("www.example.com")).toBe("example.com");
    expect(guessRegistrable("localhost")).toBe("localhost");
  });
});
