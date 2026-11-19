// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { rule } from "#lib/testing/fake-rules.ts";
import { DEFAULT_SORT } from "#lib/rules/model.ts";
import RuleTable from "./RuleTable.svelte";

afterEach(cleanup);

const rules = [
  rule(1, { pattern: "a.example.com" }),
  rule(2, { pattern: "b.example.com" }),
];

describe("RuleTable", () => {
  it("shows each rule's precedence and sorts by it", async () => {
    const onSort = vi.fn();
    const ranked = [
      rule(1, { pattern: "a.example.com", precedence: 5 }),
      rule(2, { pattern: "b.example.com", precedence: 8 }),
    ];
    render(RuleTable, {
      props: { rules: ranked, sort: DEFAULT_SORT, now: 5_000_000, onSort },
    });
    const cells = [...document.querySelectorAll("td.precedence")].map(
      (cell) => cell.textContent,
    );
    expect(cells).toEqual(["5", "8"]);
    await fireEvent.click(
      screen.getByRole("button", { name: "Sort by precedence" }),
    );
    expect(onSort).toHaveBeenCalledWith("precedence");
  });

  it("is read-only without handlers: plain headers and no action buttons", () => {
    render(RuleTable, { props: { rules, sort: DEFAULT_SORT, now: 5_000_000 } });
    expect(screen.getAllByRole("columnheader")).toHaveLength(6);
    expect(screen.queryAllByRole("button")).toHaveLength(0);
    expect(screen.getByText("a.example.com")).toBeInTheDocument();
  });

  it("sorts, changes expiry and deletes through its handlers when it has them", async () => {
    const onSort = vi.fn();
    const onExpiry = vi.fn();
    const onDelete = vi.fn();
    render(RuleTable, {
      props: {
        rules,
        sort: DEFAULT_SORT,
        now: 5_000_000,
        onSort,
        onExpiry,
        onDelete,
      },
    });
    expect(screen.getAllByRole("columnheader")).toHaveLength(7);
    await fireEvent.click(screen.getByRole("button", { name: "Sort by host" }));
    expect(onSort).toHaveBeenCalledWith("pattern");
    await fireEvent.click(
      screen.getAllByRole("button", { name: /^Change expiry/ })[0]!,
    );
    expect(onExpiry).toHaveBeenCalledWith(rules[0], expect.any(HTMLElement));
    await fireEvent.click(
      screen.getAllByRole("button", { name: /^Delete rule/ })[0]!,
    );
    expect(onDelete).toHaveBeenCalledWith(rules[0], expect.any(HTMLElement));
  });

  it("offers Edit on each row when it can edit, and names the rule", async () => {
    const onEdit = vi.fn();
    render(RuleTable, {
      props: {
        rules,
        sort: DEFAULT_SORT,
        now: 5_000_000,
        onExpiry: vi.fn(),
        onDelete: vi.fn(),
        onEdit,
      },
    });
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Edit rule: allow b.example.com for every workspace",
      }),
    );
    expect(onEdit).toHaveBeenCalledWith(rules[1], expect.any(HTMLElement));
  });

  it("has no Edit button without a handler", () => {
    render(RuleTable, {
      props: {
        rules,
        sort: DEFAULT_SORT,
        now: 5_000_000,
        onExpiry: vi.fn(),
        onDelete: vi.fn(),
      },
    });
    expect(screen.queryByRole("button", { name: /^Edit rule/ })).toBeNull();
  });

  it("has a switch column only when it can switch, and shows an off rule as off", async () => {
    const onEnabled = vi.fn();
    const mixed = [
      rule(1, { pattern: "a.example.com" }),
      rule(2, { pattern: "b.example.com", enabled: false }),
    ];
    render(RuleTable, {
      props: { rules: mixed, sort: DEFAULT_SORT, now: 5_000_000, onEnabled },
    });
    expect(screen.getAllByRole("columnheader")).toHaveLength(7);
    const first = screen.getByRole("switch", {
      name: "Rule on: allow a.example.com for every workspace",
    });
    const second = screen.getByRole("switch", {
      name: "Rule on: allow b.example.com for every workspace",
    });
    expect(first).toBeChecked();
    expect(second).not.toBeChecked();
    expect(screen.getAllByText("off")).toHaveLength(1);
    // The table asks the page and keeps showing what the rule is until the page changes it.
    await fireEvent.click(first);
    expect(onEnabled).toHaveBeenCalledWith(mixed[0], false);
    expect(first).toBeChecked();
    await fireEvent.click(second);
    expect(onEnabled).toHaveBeenCalledWith(mixed[1], true);
    expect(second).not.toBeChecked();
  });

  describe("with many rules", () => {
    const many = Array.from({ length: 1000 }, (_, i) =>
      rule(i + 1, { pattern: `host${i}.example.com` }),
    );
    const rows = () => document.querySelectorAll("tbody tr").length;

    // Drawing every row costs jsdom about 0.4 ms each (0.4 s for 1000 rules alone, 3 to 7 s on a
    // host with a load of 28), so the test that waits for all of them uses a few chunks, not 1000:
    // the slicing is the same at any length past the first slice, and the page's 1000-rule case is
    // covered in the browser by the "1000 rules" block of e2e/inbox-at-volume.spec.ts.
    const few = many.slice(0, 160);

    it("draws a first slice at once, then every row, so none is out of reach", async () => {
      render(RuleTable, {
        props: { rules: few, sort: DEFAULT_SORT, now: 5_000_000 },
      });
      expect(rows()).toBeGreaterThan(0);
      expect(rows()).toBeLessThan(few.length);
      await waitFor(() => expect(rows()).toBe(few.length));
      expect(
        document.querySelector("tbody tr:last-child td.host")?.textContent,
      ).toContain("host159.example.com");
    });

    it("tells assistive technology the true row count while rows are still coming", () => {
      render(RuleTable, {
        props: { rules: many, sort: DEFAULT_SORT, now: 5_000_000 },
      });
      const table = screen.getByRole("table");
      expect(table).toHaveAttribute("aria-rowcount", "1001");
      expect(document.querySelector("tbody tr:first-child")).toHaveAttribute(
        "aria-rowindex",
        "2",
      );
    });

    it("shows a short list in full on the first render", () => {
      render(RuleTable, {
        props: { rules, sort: DEFAULT_SORT, now: 5_000_000 },
      });
      expect(rows()).toBe(2);
    });
  });
});
