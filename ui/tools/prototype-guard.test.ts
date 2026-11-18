// SPDX-License-Identifier: AGPL-3.0-or-later
// The desktop app's prototype guard (crates/puddle-app/src/window/prototype_guard.js), run in a
// fresh JavaScript realm as the main window runs it.
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { runInNewContext } from "node:vm";
import { describe, expect, it } from "vitest";

const guard = readFileSync(
  resolve(
    import.meta.dirname,
    "../../crates/puddle-app/src/window/prototype_guard.js",
  ),
  "utf8",
);

/** Runs the guard, then `code` (strict, as a module is), in a new realm; returns `code`'s value. */
function underGuard(code: string): unknown {
  return runInNewContext(`${guard}\n(function () { "use strict"; ${code} })()`);
}

/** The name of the error `code` throws under the guard (the realm's own TypeError is not this one's). */
function thrown(code: string): string | null {
  try {
    underGuard(code);
    return null;
  } catch (error) {
    return (error as Error).name;
  }
}

describe("the prototype guard", () => {
  it("lets a plain object take its own toString, as the terminal emulator does when it loads", () => {
    expect(
      underGuard(`
        const keys = {};
        keys.toString = () => "own";
        return [String(keys), Object.keys(keys).join(), String({})];
      `),
    ).toEqual(["own", "toString", "[object Object]"]);
  });

  it("keeps the built-ins working on every object", () => {
    expect(
      underGuard(`
        const o = { a: 1 };
        return [o.hasOwnProperty("a"), o.constructor === Object, ({}).valueOf() !== null];
      `),
    ).toEqual([true, true, true]);
  });

  it("refuses a change to a built-in on the prototype itself", () => {
    expect(() =>
      underGuard(`({}).__proto__.toString = () => "polluted";`),
    ).toThrow(/Object.prototype is locked: toString can't be changed/);
    expect(underGuard(`return String({});`)).toBe("[object Object]");
  });

  it("refuses a new property on the prototype", () => {
    expect(thrown(`Object.prototype.isAdmin = true;`)).toBe("TypeError");
    expect(thrown(`({}).__proto__["polluted"] = 1;`)).toBe("TypeError");
    expect(underGuard(`return ({}).isAdmin;`)).toBeUndefined();
  });

  it("refuses to redefine a built-in on the prototype", () => {
    expect(
      thrown(
        `Object.defineProperty(Object.prototype, "toString", { value: () => "x" });`,
      ),
    ).toBe("TypeError");
  });

  it("leaves each built-in as a getter and setter that can't be redefined", () => {
    expect(
      underGuard(`
        const found = Object.getOwnPropertyDescriptor(Object.prototype, "toString");
        return [typeof found.get, typeof found.set, found.configurable, "value" in found];
      `),
    ).toEqual(["function", "function", false, false]);
  });

  it("refuses an own property on an object that is frozen", () => {
    expect(thrown(`const o = Object.freeze({}); o.toString = () => "x";`)).toBe(
      "TypeError",
    );
  });
});
