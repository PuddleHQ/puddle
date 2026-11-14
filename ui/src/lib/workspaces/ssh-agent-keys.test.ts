// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { MAX_KEYS, parseKeyList } from "./ssh-agent-keys.ts";

const key = (n: number) => `SHA256:${String(n).padStart(43, "A")}`;

describe("parseKeyList", () => {
  it("takes nothing, or only blank lines, as every key", () => {
    expect(parseKeyList("")).toEqual({ ok: true, keys: null });
    expect(parseKeyList(" \n\r\n  \n")).toEqual({ ok: true, keys: null });
  });

  it("takes one fingerprint per line, trimmed, once each", () => {
    expect(parseKeyList(` ${key(1)} \r\n\n${key(2)}\n${key(1)}\n`)).toEqual({
      ok: true,
      keys: [key(1), key(2)],
    });
  });

  it("names the first line that is not a SHA-256 fingerprint", () => {
    for (const bad of [
      "ssh-ed25519 AAAA",
      "SHA256:short",
      `MD5:${"A".repeat(43)}`,
      `SHA256:${"A".repeat(43)}=`,
      `SHA256:${"A".repeat(44)}`,
    ]) {
      const result = parseKeyList(`${key(1)}\n${bad}\n`);
      expect(result.ok).toBe(false);
      if (!result.ok) expect(result.message).toContain(`"${bad}"`);
    }
  });

  it("allows 64 keys and refuses more", () => {
    const keys = (n: number) =>
      Array.from({ length: n }, (_, i) => key(i)).join("\n");
    expect(parseKeyList(keys(MAX_KEYS))).toMatchObject({ ok: true });
    expect(parseKeyList(keys(MAX_KEYS + 1))).toEqual({
      ok: false,
      message: "Use at most 64 keys.",
    });
  });
});
