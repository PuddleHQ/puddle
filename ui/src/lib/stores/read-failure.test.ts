// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { readFailure, SERVICE_DOWN } from "./read-failure.ts";

describe("readFailure", () => {
  it("uses the API's own message as a sentence", () => {
    expect(readFailure({ message: "disk is full" }, { status: 500 })).toBe(
      "Disk is full.",
    );
  });

  it("says the service didn't answer when no response came", () => {
    expect(readFailure(undefined)).toBe(SERVICE_DOWN);
  });

  it("says what the status means when the body has no message", () => {
    expect(readFailure({}, { status: 401 })).toBe(
      "puddle didn't accept this app's key.",
    );
    expect(readFailure({}, { status: 503 })).toBe(
      "puddle's service hit an error (status 503).",
    );
    expect(readFailure(undefined, { status: 404 })).toBe(
      "puddle answered with status 404.",
    );
  });
});
