// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { hasUnsavedEdits, holdReload } from "./unsaved-edits.ts";

describe("unsaved edits", () => {
  it("is false with no form registered", () => {
    expect(hasUnsavedEdits()).toBe(false);
  });

  it("is true while any registered form has edits, and follows them", () => {
    let first = false;
    let second = false;
    const release = [holdReload(() => first), holdReload(() => second)];
    expect(hasUnsavedEdits()).toBe(false);
    second = true;
    expect(hasUnsavedEdits()).toBe(true);
    first = true;
    second = false;
    expect(hasUnsavedEdits()).toBe(true);
    first = false;
    expect(hasUnsavedEdits()).toBe(false);
    release.forEach((r) => r());
  });

  it("stops counting a form once it unregisters", () => {
    const release = holdReload(() => true);
    expect(hasUnsavedEdits()).toBe(true);
    release();
    expect(hasUnsavedEdits()).toBe(false);
  });

  it("keeps two registrations of one function separate", () => {
    const test = () => true;
    const a = holdReload(test);
    const b = holdReload(test);
    a();
    expect(hasUnsavedEdits()).toBe(true);
    b();
    expect(hasUnsavedEdits()).toBe(false);
  });
});
