// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
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
});
