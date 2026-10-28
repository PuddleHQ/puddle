// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const url = vi.hoisted(() => ({ id: "web" }));
vi.mock("$app/state", () => ({
  page: {
    get params() {
      return { id: url.id };
    },
  },
}));

const h = await vi.hoisted(async () => {
  const fake = await import("#lib/testing/fake-environment.ts");
  const ws = await import("#lib/testing/fake-workspaces.ts");
  return { api: new fake.FakeEnvApi(), workspace: ws.workspace };
});
vi.mock("#lib/api/client.ts", () => ({ api: h.api }));

import { toasts } from "#lib/stores/toasts.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { secret, variable } from "#lib/testing/fake-environment.ts";
import Page from "./+page.svelte";

const { api, workspace } = h;

beforeEach(() => {
  url.id = "web";
  workspaces.list = [
    workspace("web", { status: "running" }),
    workspace("docs", { status: "stopped" }),
  ];
  api.global = [variable("EDITOR", { scope: "global", value: "vim" })];
  api.own = {
    web: [
      variable("MODE", { value: "dev" }),
      secret("TOKEN", ["api.example.com"]),
    ],
  };
  api.secretValues = {};
  api.calls = [];
  api.bodies = [];
  api.down = false;
  api.refuse = null;
});

afterEach(() => {
  cleanup();
  for (const t of [...toasts.items]) toasts.dismiss(t.id);
});

const table = () => screen.findByRole("table", { name: "Environment of web" });
const rowOf = async (name: string) => {
  const t = await table();
  return within(t)
    .getAllByRole("row")
    .find((r) => r.getAttribute("data-name") === name) as HTMLElement;
};
const openAdd = async () => {
  await fireEvent.click(
    await screen.findByRole("button", { name: "Add variable or secret" }),
  );
  return screen.findByRole("dialog");
};

describe("the table", () => {
  it("lists the workspace's own first, then the global ones, and never a secret's value", async () => {
    api.secretValues["TOKEN"] = "hunter2";
    render(Page);
    await table();
    const names = screen
      .getAllByRole("row")
      .slice(1)
      .map((r) => r.getAttribute("data-name"));
    expect(names).toEqual(["MODE", "TOKEN", "EDITOR"]);
    const token = await rowOf("TOKEN");
    expect(token).toHaveTextContent("Secret");
    expect(token).toHaveTextContent("Stored in Puddle, never shown");
    expect(token).toHaveTextContent("api.example.com");
    expect(document.body.textContent).not.toContain("hunter2");
    expect(await rowOf("MODE")).toHaveTextContent("dev");
    expect(await rowOf("EDITOR")).toHaveTextContent("Every workspace");
  });

  it("says plain variables wait for the next start while the workspace runs, and secrets don't", async () => {
    render(Page);
    await table();
    expect(
      screen.getByText(/Plain variables apply at the next start/),
    ).toHaveTextContent("the sandbox is made again");
    expect(screen.getByText(/apply at once, with no restart/)).toBeTruthy();
    expect(document.body.textContent).toContain("never enters the workspace");
  });

  it("says both kinds are read at the start of a stopped workspace", async () => {
    url.id = "docs";
    render(Page);
    expect(
      await screen.findByText(/Both kinds are read when the workspace starts/),
    ).toBeTruthy();
    const t = await screen.findByRole("table", { name: "Environment of docs" });
    expect(within(t).getByText("EDITOR")).toBeTruthy();
  });

  it("marks a global variable the workspace's own hides", async () => {
    api.own["web"] = [variable("EDITOR", { value: "nano" })];
    render(Page);
    await table();
    const hidden = screen
      .getAllByRole("row")
      .find(
        (r) =>
          r.getAttribute("data-name") === "EDITOR" &&
          r.getAttribute("data-scope") === "global",
      );
    expect(hidden).toHaveTextContent(
      "Hidden here by this workspace's own EDITOR",
    );
  });

  it("shows loading and failed states, and an empty list", async () => {
    url.id = "nope";
    render(Page);
    expect(screen.getByText(/Loading environment/)).toBeTruthy();
    cleanup();
    url.id = "docs";
    api.global = [];
    render(Page);
    expect(await screen.findByText(/no global ones reach it/)).toBeTruthy();
    cleanup();
    api.down = true;
    render(Page);
    expect(await screen.findByText(/Couldn't read the variables/)).toBeTruthy();
  });
});

describe("adding", () => {
  it("adds a plain variable and says it waits for the next start", async () => {
    render(Page);
    const dialog = await openAdd();
    expect(dialog).toHaveTextContent("Applies to web.");
    await fireEvent.input(within(dialog).getByLabelText("Name"), {
      target: { value: "LOG_LEVEL" },
    });
    await fireEvent.input(within(dialog).getByLabelText("Value"), {
      target: { value: "debug" },
    });
    await fireEvent.click(within(dialog).getByRole("button", { name: "Add" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(await rowOf("LOG_LEVEL")).toHaveTextContent("debug");
    expect(api.bodies.at(-1)).toEqual({ kind: "plain", value: "debug" });
    expect(toasts.items.at(-1)?.message).toBe("Saved LOG_LEVEL.");
    expect(screen.getByRole("status", { name: "" }).textContent).toContain(
      "LOG_LEVEL: Plain variables apply at the next start",
    );
  });

  it("goes back to a plain variable after looking at a secret, and the hosts field goes away", async () => {
    render(Page);
    const dialog = await openAdd();
    await fireEvent.click(
      within(dialog).getByRole("radio", {
        name: /Secret: the workspace never sees/,
      }),
    );
    expect(
      within(dialog).getByLabelText("Hosts it may be sent to"),
    ).toBeTruthy();
    await fireEvent.click(
      within(dialog).getByRole("radio", {
        name: /Variable: the workspace sees its value/,
      }),
    );
    expect(
      within(dialog).queryByLabelText("Hosts it may be sent to"),
    ).toBeNull();
  });

  it("adds a secret: the value goes once, the table never shows it", async () => {
    render(Page);
    const dialog = await openAdd();
    await fireEvent.click(
      within(dialog).getByRole("radio", {
        name: /Secret: the workspace never sees/,
      }),
    );
    expect(dialog).toHaveTextContent(
      "Puddle will decrypt connections to each host",
    );
    await fireEvent.input(within(dialog).getByLabelText("Name"), {
      target: { value: "NPM_TOKEN" },
    });
    await fireEvent.input(within(dialog).getByLabelText("Value"), {
      target: { value: "npm_abc123" },
    });
    await fireEvent.input(
      within(dialog).getByLabelText("Hosts it may be sent to"),
      {
        target: { value: "registry.npmjs.org, *.npmjs.org" },
      },
    );
    await fireEvent.click(within(dialog).getByRole("button", { name: "Add" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    const row = await rowOf("NPM_TOKEN");
    expect(row).toHaveTextContent("registry.npmjs.org and *.npmjs.org");
    expect(api.bodies.at(-1)).toEqual({
      kind: "secret",
      hosts: ["registry.npmjs.org", "*.npmjs.org"],
      value: "npm_abc123",
    });
    expect(document.body.textContent).not.toContain("npm_abc123");
    expect(screen.getByRole("status")).toHaveTextContent(
      "NPM_TOKEN: A secret's hosts and value apply at once",
    );
  });

  it("checks a name, hosts and a new secret's value before sending anything", async () => {
    render(Page);
    const dialog = await openAdd();
    const submit = () =>
      fireEvent.click(within(dialog).getByRole("button", { name: "Add" }));
    await submit();
    expect(within(dialog).getByRole("alert")).toHaveTextContent(
      "Give the variable a name.",
    );
    await fireEvent.input(within(dialog).getByLabelText("Name"), {
      target: { value: "1BAD" },
    });
    expect(within(dialog).queryByRole("alert")).toBeNull();
    await submit();
    expect(within(dialog).getByRole("alert")).toHaveTextContent(
      "does not start with a digit",
    );
    await fireEvent.input(within(dialog).getByLabelText("Name"), {
      target: { value: "OK_NAME" },
    });
    await fireEvent.click(
      within(dialog).getByRole("radio", { name: /Secret:/ }),
    );
    await submit();
    expect(within(dialog).getByRole("alert")).toHaveTextContent(
      "Name at least one host",
    );
    await fireEvent.input(
      within(dialog).getByLabelText("Hosts it may be sent to"),
      {
        target: { value: "a.example.com" },
      },
    );
    await submit();
    expect(within(dialog).getByRole("alert")).toHaveTextContent(
      "A new secret needs its value.",
    );
    expect(api.calls.filter((c) => c.startsWith("PUT"))).toEqual([]);
  });

  it("shows the service's refusal and keeps the dialog open; typing clears it", async () => {
    render(Page);
    const dialog = await openAdd();
    await fireEvent.input(within(dialog).getByLabelText("Name"), {
      target: { value: "PATH" },
    });
    api.refuse = { status: 422, message: "PATH comes from the image" };
    await fireEvent.click(within(dialog).getByRole("button", { name: "Add" }));
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "PATH comes from the image.",
    );
    await fireEvent.input(within(dialog).getByLabelText("Name"), {
      target: { value: "PATH2" },
    });
    expect(within(dialog).queryByRole("alert")).toBeNull();
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("starts empty each time it opens", async () => {
    render(Page);
    let dialog = await openAdd();
    await fireEvent.input(within(dialog).getByLabelText("Name"), {
      target: { value: "HALF" },
    });
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    dialog = await openAdd();
    expect(within(dialog).getByLabelText("Name")).toHaveValue("");
  });
});

describe("changing a row", () => {
  it("edits a plain variable's value", async () => {
    render(Page);
    const row = await rowOf("MODE");
    await fireEvent.click(
      within(row).getByRole("button", { name: "Edit MODE" }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(dialog).toHaveTextContent("Edit MODE");
    expect(within(dialog).getByLabelText("Name")).toBeDisabled();
    expect(within(dialog).getByLabelText("Value")).toHaveValue("dev");
    await fireEvent.input(within(dialog).getByLabelText("Value"), {
      target: { value: "prod" },
    });
    await fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(await rowOf("MODE")).toHaveTextContent("prod");
  });

  it("keeps a secret's value when only its hosts change", async () => {
    api.secretValues["TOKEN"] = "kept";
    render(Page);
    const row = await rowOf("TOKEN");
    await fireEvent.click(
      within(row).getByRole("button", { name: "Edit TOKEN" }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByLabelText("Value")).toHaveValue("");
    expect(dialog).toHaveTextContent(
      "Leave it empty to keep the value Puddle holds",
    );
    expect(
      within(dialog).getByLabelText("Hosts it may be sent to"),
    ).toHaveValue("api.example.com");
    await fireEvent.input(
      within(dialog).getByLabelText("Hosts it may be sent to"),
      {
        target: { value: "api.example.com\nother.example.com" },
      },
    );
    await fireEvent.click(within(dialog).getByRole("button", { name: "Save" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.bodies.at(-1)).toEqual({
      kind: "secret",
      hosts: ["api.example.com", "other.example.com"],
    });
    expect(api.secretValues["TOKEN"]).toBe("kept");
  });

  it("overrides a global variable with the workspace's own, starting from its name", async () => {
    render(Page);
    const row = await rowOf("EDITOR");
    expect(within(row).queryByRole("button", { name: /Remove/ })).toBeNull();
    await fireEvent.click(
      within(row).getByRole("button", { name: "Override EDITOR for web" }),
    );
    const dialog = await screen.findByRole("dialog");
    expect(within(dialog).getByLabelText("Name")).toHaveValue("EDITOR");
    await fireEvent.input(within(dialog).getByLabelText("Value"), {
      target: { value: "nano" },
    });
    await fireEvent.click(within(dialog).getByRole("button", { name: "Add" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.own["web"]?.map((v) => v.name)).toContain("EDITOR");
  });

  it("removes a plain variable at once", async () => {
    render(Page);
    const row = await rowOf("MODE");
    await fireEvent.click(
      within(row).getByRole("button", { name: "Remove MODE" }),
    );
    await waitFor(() =>
      expect(api.own["web"]?.map((v) => v.name)).toEqual(["TOKEN"]),
    );
    await waitFor(() =>
      expect(toasts.items.at(-1)?.message).toBe("Removed MODE."),
    );
  });

  it("asks before removing a secret, because its value can't be shown again", async () => {
    render(Page);
    const row = await rowOf("TOKEN");
    await fireEvent.click(
      within(row).getByRole("button", { name: "Remove TOKEN" }),
    );
    let dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Remove TOKEN?");
    expect(dialog).toHaveTextContent("It can't be shown again");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await waitFor(() => expect(screen.queryByRole("alertdialog")).toBeNull());
    expect(api.calls.some((c) => c.startsWith("DELETE"))).toBe(false);
    await fireEvent.click(
      within(row).getByRole("button", { name: "Remove TOKEN" }),
    );
    dialog = await screen.findByRole("alertdialog");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Remove secret" }),
    );
    await waitFor(() =>
      expect(api.own["web"]?.map((v) => v.name)).toEqual(["MODE"]),
    );
  });

  it("says when a removal fails", async () => {
    render(Page);
    const row = await rowOf("MODE");
    api.refuse = { status: 500, message: "x" };
    await fireEvent.click(
      within(row).getByRole("button", { name: "Remove MODE" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Puddle couldn't remove the variable.",
    );
  });
});
