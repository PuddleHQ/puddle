// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  ackMessage,
  clampSize,
  MAX_INPUT_BYTES,
  parseEnded,
  resizeMessage,
  socketUrl,
  splitInput,
} from "./wire.ts";

describe("clampSize", () => {
  it("keeps a size inside 2 to 500 columns and 1 to 300 rows", () => {
    expect(clampSize(80, 24)).toEqual({ cols: 80, rows: 24 });
    expect(clampSize(1, 0)).toEqual({ cols: 2, rows: 1 });
    expect(clampSize(900, 900)).toEqual({ cols: 500, rows: 300 });
    expect(clampSize(80.9, 24.2)).toEqual({ cols: 80, rows: 24 });
    expect(clampSize(Number.NaN, Number.POSITIVE_INFINITY)).toEqual({
      cols: 2,
      rows: 1,
    });
  });
});

describe("control messages", () => {
  it("writes the resize and the acknowledgement as the host reads them", () => {
    expect(JSON.parse(resizeMessage({ cols: 100, rows: 30 }))).toEqual({
      type: "resize",
      cols: 100,
      rows: 30,
    });
    expect(JSON.parse(ackMessage(4096))).toEqual({ type: "ack", bytes: 4096 });
  });

  it("reads the end the host sends", () => {
    expect(parseEnded('{"type":"ended","reason":"exited","status":3}')).toEqual(
      { reason: "exited", status: 3, message: null },
    );
    expect(
      parseEnded('{"type":"ended","reason":"crashed","message":"oom"}'),
    ).toEqual({ reason: "crashed", status: null, message: "oom" });
  });

  it("ignores anything else", () => {
    expect(parseEnded("not json")).toBeNull();
    expect(parseEnded("3")).toBeNull();
    expect(parseEnded("null")).toBeNull();
    expect(parseEnded('{"type":"other","reason":"exited"}')).toBeNull();
    expect(parseEnded('{"type":"ended","reason":"lost"}')).toBeNull();
    expect(parseEnded('{"type":"ended","reason":7}')).toBeNull();
  });
});

describe("splitInput", () => {
  it("cuts a long paste into messages of at most 64 KiB, in order", () => {
    const bytes = Uint8Array.from(
      { length: MAX_INPUT_BYTES * 2 + 5 },
      (_, i) => i % 251,
    );
    const pieces = splitInput(bytes);
    expect(pieces.map((p) => p.length)).toEqual([
      MAX_INPUT_BYTES,
      MAX_INPUT_BYTES,
      5,
    ]);
    expect(Uint8Array.from(pieces.flatMap((p) => [...p]))).toEqual(bytes);
    expect(splitInput(new Uint8Array())).toEqual([]);
  });
});

describe("socketUrl", () => {
  it("follows the page's scheme and keeps the id out of the query", () => {
    expect(socketUrl("http://127.0.0.1:5000", "a b", 3)).toBe(
      "ws://127.0.0.1:5000/api/workspaces/a%20b/terminals/3",
    );
    expect(socketUrl("https://puddle.example", "w", 1)).toBe(
      "wss://puddle.example/api/workspaces/w/terminals/1",
    );
  });
});
