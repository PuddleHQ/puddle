// SPDX-License-Identifier: GPL-3.0-or-later
import { cleanup, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it } from "vitest";
import LoadNotice from "./LoadNotice.svelte";

afterEach(cleanup);

describe("LoadNotice", () => {
  it("says what could not be read and why, and that it retries", () => {
    render(LoadNotice, {
      props: { what: "the rules", message: "Disk is full." },
    });
    expect(screen.getByRole("status")).toHaveTextContent(
      "Couldn't read the rules. Disk is full. Trying again.",
    );
  });

  it("warns that what is shown may be out of date after an earlier read", () => {
    render(LoadNotice, {
      props: { what: "the rules", message: null, stale: true },
    });
    expect(screen.getByRole("status")).toHaveTextContent(
      "Couldn't refresh the rules. What is shown may be out of date; puddle keeps trying.",
    );
  });
});
