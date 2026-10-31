// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { requestRule } from "#lib/testing/fake-request-rules.ts";
import RequestRuleTable from "./RequestRuleTable.svelte";

afterEach(cleanup);

const NOW = 2_000_000;

describe("the method and path table", () => {
  it("shows host, methods, path, workspace and who made it", () => {
    render(RequestRuleTable, {
      props: {
        now: NOW,
        rules: [
          requestRule(1, {
            host: "api.example.com",
            methods: ["DELETE", "PUT"],
            path: "/repos/**",
            workspace: "shop" as never,
            created_by: "api",
          }),
          requestRule(2, { host: "*.example.org" }),
        ],
      },
    } as never);
    const [first, second] = screen.getAllByRole("row").slice(1);
    expect(first).toHaveTextContent("api.example.com");
    expect(first).toHaveTextContent("DELETE, PUT");
    expect(first).toHaveTextContent("/repos/**");
    expect(first).toHaveTextContent("shop");
    expect(within(first!).getByRole("time")).toHaveAttribute(
      "title",
      expect.stringContaining("by api"),
    );
    expect(second).toHaveTextContent("*.example.org");
    expect(second).toHaveTextContent("and subdomains");
    expect(second).toHaveTextContent("Every method");
    expect(second).toHaveTextContent("Every workspace");
  });

  it("shows the never-decrypt warning in full, labelled in words", () => {
    const warning =
      "api.example.com is decrypted for this rule although the never-decrypt list names it: *.example.com (every workspace). A tool there that does not trust Puddle's certificate will fail on it.";
    render(RequestRuleTable, {
      props: { now: NOW, rules: [requestRule(1, { warning })] },
    } as never);
    const row = screen.getAllByRole("row")[1]!;
    expect(row).toHaveTextContent(`Note: ${warning}`);
  });

  it("is read-only without a delete handler", () => {
    render(RequestRuleTable, {
      props: { now: NOW, rules: [requestRule(1)] },
    } as never);
    expect(screen.queryAllByRole("button")).toHaveLength(0);
    expect(screen.queryByRole("columnheader", { name: "Actions" })).toBeNull();
  });

  it("names the rule on each delete button, so several look different", async () => {
    const onDelete = vi.fn();
    const rules = [
      requestRule(1, { host: "a.example", methods: ["DELETE"], path: "/x" }),
      requestRule(2, { host: "b.example" }),
    ];
    render(RequestRuleTable, { props: { now: NOW, rules, onDelete } } as never);
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Delete rule: refuse DELETE /x on a.example for every workspace",
      }),
    );
    expect(onDelete).toHaveBeenCalledWith(rules[0]);
    expect(
      screen.getByRole("button", {
        name: "Delete rule: refuse every method /** on b.example for every workspace",
      }),
    ).toBeInTheDocument();
  });
});
