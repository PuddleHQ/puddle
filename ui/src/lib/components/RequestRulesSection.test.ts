// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { FakeSource } from "#lib/testing/fake-inbox.ts";
import {
  FakeRequestRules,
  requestRule,
} from "#lib/testing/fake-request-rules.ts";
import { RequestRulesStore } from "#lib/stores/request-rules.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import Toast from "./Toast.svelte";
import RequestRulesSection from "./RequestRulesSection.svelte";

let api: FakeRequestRules;
let store: RequestRulesStore;

beforeEach(() => {
  api = new FakeRequestRules();
  store = new RequestRulesStore({
    api: api as never,
    source: new FakeSource(),
    pollMs: 60_000,
  });
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
});
afterEach(cleanup);

function mount(props: Record<string, unknown> = {}) {
  render(RequestRulesSection, {
    props: { store, now: 2_000_000, workspaces: ["shop"], ...props },
  } as never);
  return waitFor(() => expect(store.status).not.toBe("loading"));
}

const addButton = () =>
  screen.getByRole("button", { name: "Add method and path rule" });
const fill = async (host: string, path: string, workspace?: string) => {
  const dialog = await screen.findByRole("dialog", {
    name: "Add a method and path rule",
  });
  await fireEvent.input(within(dialog).getByLabelText("Host"), {
    target: { value: host },
  });
  await fireEvent.input(within(dialog).getByLabelText("Methods"), {
    target: { value: "delete" },
  });
  await fireEvent.input(within(dialog).getByLabelText("Path"), {
    target: { value: path },
  });
  if (workspace === undefined) {
    await fireEvent.click(within(dialog).getByLabelText(/Every workspace/));
  } else {
    await fireEvent.input(within(dialog).getByLabelText("Workspace name"), {
      target: { value: workspace },
    });
  }
  return dialog;
};

describe("on the Rules screen", () => {
  it("says what a rule does and that it only refuses", async () => {
    await mount();
    const region = screen.getByRole("region", { name: "Method and path" });
    expect(region).toHaveTextContent("only refuses");
    expect(region).toHaveTextContent("never opens a host");
    expect(region).toHaveTextContent("decrypts the host");
    expect(region).toHaveTextContent("next connection");
  });

  it("shows an empty state that says what a rule is for", async () => {
    await mount();
    expect(
      screen.getByText(/No rules on method and path yet/),
    ).toBeInTheDocument();
  });

  it("says it is loading, and that it could not read the rules", async () => {
    api.down = true;
    render(RequestRulesSection, { props: { store, now: 0 } } as never);
    expect(
      screen.getByText(/Loading the method and path rules/),
    ).toBeInTheDocument();
    expect(
      await screen.findByText(
        /Couldn't read the rules on method and path\. Puddle's service isn't answering\./,
      ),
    ).toBeInTheDocument();
  });

  it("keeps the table but warns that it may be out of date when a refresh fails", async () => {
    api.rules = [requestRule(1)];
    await mount();
    api.down = true;
    await store.refresh();
    expect(await screen.findByText(/may be out of date/)).toBeInTheDocument();
    expect(screen.getByRole("table")).toBeInTheDocument();
  });

  it("Cancel closes the form and Add opens it empty again", async () => {
    await mount();
    await fireEvent.click(addButton());
    const dialog = await fill("api.example.com", "/repos/**", "shop");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await fireEvent.click(addButton());
    expect(
      within(await screen.findByRole("dialog")).getByLabelText("Host"),
    ).toHaveValue("");
  });

  it("adds a rule for one workspace and says so", async () => {
    await mount();
    await fireEvent.click(addButton());
    await fill("api.example.com", "/repos/**", "shop");
    await fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
    await waitFor(() =>
      expect(api.bodies.at(-1)).toEqual({
        host: "api.example.com",
        methods: ["DELETE"],
        path: "/repos/**",
        workspace: "shop",
      }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(screen.getByRole("table")).toHaveTextContent("api.example.com");
    expect(toasts.items.at(-1)?.message).toBe(
      "Added: refuse DELETE /repos/** on api.example.com for workspace shop.",
    );
  });

  it("asks before a rule for every workspace and can go back to the form", async () => {
    await mount();
    await fireEvent.click(addButton());
    await fill("api.example.com", "/repos/**");
    await fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
    const confirm = await screen.findByRole("alertdialog", {
      name: "Refuse in every workspace?",
    });
    expect(confirm).toHaveTextContent(
      "Refuse DELETE /repos/** on api.example.com for every workspace",
    );
    expect(confirm).toHaveTextContent("decrypts the host for all of them");
    expect(api.calls).not.toContain("POST /api/request-rules");
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Cancel" }),
    );
    const form = await screen.findByRole("dialog", {
      name: "Add a method and path rule",
    });
    expect(within(form).getByLabelText("Host")).toHaveValue("api.example.com");
    await fireEvent.click(
      within(form).getByRole("button", { name: "Add rule" }),
    );
    await fireEvent.click(
      await screen.findByRole("button", { name: "Refuse in every workspace" }),
    );
    await waitFor(() =>
      expect(api.bodies.at(-1)).toMatchObject({ workspace: null }),
    );
  });

  it("brings the form back with the refusal when the API refuses a confirmed rule", async () => {
    await mount();
    await fireEvent.click(addButton());
    await fill("api.example.com", "/repos/**");
    await fireEvent.click(screen.getByRole("button", { name: "Add rule" }));
    api.refuse = { status: 422, message: "api.example.com is not a host name" };
    await fireEvent.click(
      await screen.findByRole("button", { name: "Refuse in every workspace" }),
    );
    const form = await screen.findByRole("dialog", {
      name: "Add a method and path rule",
    });
    expect(await within(form).findByRole("alert")).toHaveTextContent(
      "Api.example.com is not a host name.",
    );
  });

  it("shows the API's refusal next to the field and keeps the form open", async () => {
    await mount();
    await fireEvent.click(addButton());
    const dialog = await fill("api.example.com", "/repos/**", "shop");
    api.refuse = { status: 422, message: "methods: GE T is not a method" };
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Add rule" }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "GE T is not a method.",
    );
    expect(within(dialog).getByLabelText("Methods")).toHaveAttribute(
      "aria-invalid",
      "true",
    );
  });

  it("deletes after asking, and Cancel keeps the rule", async () => {
    api.rules = [requestRule(1, { host: "a.example" })];
    await mount();
    const button = screen.getByRole("button", { name: /^Delete rule: refuse/ });
    await fireEvent.click(button);
    const confirm = await screen.findByRole("alertdialog", {
      name: "Delete this rule?",
    });
    expect(confirm).toHaveTextContent("refuse every method /** on a.example");
    await fireEvent.click(
      within(confirm).getByRole("button", { name: "Cancel" }),
    );
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(api.calls.filter((c) => c.startsWith("DELETE"))).toEqual([]);
    await fireEvent.click(button);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Delete rule" }),
    );
    await waitFor(() => expect(api.rules).toEqual([]));
    expect(toasts.items.at(-1)?.message).toMatch(/^Deleted: refuse/);
  });

  it("moves focus to the next rule's delete button, else the heading", async () => {
    api.rules = [
      requestRule(1, { host: "a.example" }),
      requestRule(2, { host: "b.example" }),
    ];
    await mount();
    const del = (host: string) =>
      screen.getByRole("button", {
        name: new RegExp(`^Delete rule: refuse .* on ${host} `),
      });
    const confirmDelete = async () =>
      fireEvent.click(
        await screen.findByRole("button", { name: "Delete rule" }),
      );
    await fireEvent.click(del("a.example"));
    await confirmDelete();
    await waitFor(() => expect(store.rules.map((r) => r.id)).toEqual([2]));
    await waitFor(() => expect(del("b.example")).toHaveFocus());
    await fireEvent.click(del("b.example"));
    await confirmDelete();
    await waitFor(() => expect(store.rules).toEqual([]));
    await waitFor(() =>
      expect(
        screen.getByRole("heading", { name: "Method and path" }),
      ).toHaveFocus(),
    );
  });

  it("says why a delete failed", async () => {
    api.rules = [requestRule(1)];
    await mount();
    render(Toast);
    await fireEvent.click(
      screen.getByRole("button", { name: /^Delete rule: refuse/ }),
    );
    api.refuse = { status: 500, message: "x" };
    await fireEvent.click(
      await screen.findByRole("button", { name: "Delete rule" }),
    );
    expect(await screen.findByText("X.")).toBeInTheDocument();
    expect(store.rules).toHaveLength(1);
  });
});

describe("for one workspace", () => {
  it("lists the rules that apply, read-only", async () => {
    api.rules = [
      requestRule(1, { host: "mine.example", workspace: "shop" as never }),
      requestRule(2, { host: "all.example" }),
      requestRule(3, { host: "theirs.example", workspace: "docs" as never }),
    ];
    await mount({ workspace: "shop" });
    const region = screen.getByRole("region", {
      name: "Rules on method and path",
    });
    const table = within(region).getByRole("table");
    expect(table).toHaveTextContent("mine.example");
    expect(table).toHaveTextContent("all.example");
    expect(table).not.toHaveTextContent("theirs.example");
    expect(within(region).queryByRole("button")).toBeNull();
    expect(
      within(region).getByRole("link", { name: "All method and path rules" }),
    ).toHaveAttribute("href", "/rules");
    expect(
      within(region).getByRole("link", { name: "Rules page" }),
    ).toHaveAttribute("href", "/rules");
  });

  it("says when nothing applies", async () => {
    api.rules = [requestRule(3, { workspace: "docs" as never })];
    await mount({ workspace: "shop" });
    expect(
      screen.getByText("No rule on method and path applies to shop."),
    ).toBeInTheDocument();
  });

  it("says it could not read the rules", async () => {
    api.down = true;
    await mount({ workspace: "shop" });
    expect(
      screen.getByText(/Couldn't read the rules on method and path/),
    ).toBeInTheDocument();
  });
});
