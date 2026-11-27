// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, screen, within } from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";

const { h, url, goto } = await vi.hoisted(() => import("./detail-harness.ts"));
vi.mock("$app/state", async () => {
  const { navigations } = await import("./detail-navigation.ts");
  return {
    page: {
      get url() {
        return new URL(`http://127.0.0.1${url.pathname}`);
      },
      get params() {
        navigations.get("n");
        return { id: url.id };
      },
    },
  };
});
vi.mock("$app/navigation", () => ({ goto }));
vi.mock("#lib/stores/pending.svelte.ts", async (original) =>
  (await import("./detail-harness.ts")).pendingMock(await original()),
);
vi.mock("#lib/stores/workspaces.svelte.ts", async (original) =>
  (await import("./detail-harness.ts")).workspacesMock(await original()),
);
vi.mock("#lib/stores/rules.svelte.ts", async (original) =>
  (await import("./detail-harness.ts")).rulesMock(await original()),
);
vi.mock("#lib/stores/request-rules.svelte.ts", async (original) =>
  (await import("./detail-harness.ts")).requestRulesMock(await original()),
);
vi.mock("#lib/api/client.ts", () => ({ api: h.api }));

import { workspaceActions } from "#lib/stores/workspace-actions.svelte.ts";
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { hasUnsavedEdits } from "#lib/stores/unsaved-edits.ts";
import { installReset, openSettingsTab, settingsOf } from "./detail-setup.ts";

const { api, workspace } = h;

installReset();

describe("the settings tab", () => {
  const open = openSettingsTab;
  beforeEach(() => {
    url.pathname = "/workspaces/w-demo/settings";
  });

  it("shows each setting as a row with its value and where the value comes from, and no dropdown option that only says to follow the global setting", async () => {
    const memory = await open();
    expect(memory.value).toBe("8192");
    expect(within(memory).queryByText(/Use the global/)).toBeNull();
    expect(
      screen.getByText(/A change applies at the next restart/),
    ).toBeInTheDocument();
    expect(screen.getAllByText("Global setting").length).toBeGreaterThan(0);
    // The default is the visible resting value of the control: it carries no pill of its own.
    expect(screen.queryByText("Puddle's default")).toBeNull();
    // Following the global setting needs no reset link.
    expect(screen.queryByRole("button", { name: /again for/ })).toBeNull();
    for (const label of [
      "This computer (loopback)",
      "Private networks",
      "Link-local",
      "Cloud metadata",
      "Other special ranges",
      "Pages reading the clipboard",
    ]) {
      expect(screen.getByLabelText(label)).toBeInTheDocument();
    }
    expect(document.getElementById("local-destinations")).not.toBeNull();
    expect(
      screen.getByRole("columnheader", { name: "Setting" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("columnheader", { name: "From" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("columnheader", { name: "Advanced" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("columnheader", { name: "Danger" }),
    ).toBeInTheDocument();
    expect(screen.getAllByRole("table")).toHaveLength(1);
  });

  it("lists stored fields it doesn't know, only when there are some", async () => {
    await open();
    expect(screen.queryByText(/fields this Puddle doesn't know/)).toBeNull();
    cleanup();
    api.unknown = { "w-demo": ["clipboard_reed"] };
    await open();
    expect(
      screen.getByText(/fields this Puddle doesn't know/),
    ).toHaveTextContent("clipboard_reed");
  });

  it("saves a memory choice at once, says the next start applies it, and follows the global setting again from the row's link", async () => {
    const memory = await open();
    await fireEvent.change(memory, { target: { value: "16384" } });
    await screen.findByText("Memory saved.");
    expect(api.overrides["w-demo"]?.memory).toBe(16_384);
    expect(
      await screen.findByText(
        "The new memory applies the next time the workspace starts.",
      ),
    ).toBeInTheDocument();
    expect(screen.getByText("This workspace")).toBeInTheDocument();
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Memory",
      }),
    );
    await vi.waitFor(() => expect(api.overrides["w-demo"]?.memory).toBeNull());
    await vi.waitFor(() =>
      expect(
        screen.queryByRole("button", { name: /again for Memory/ }),
      ).toBeNull(),
    );
  });

  it("saves a CPU count at once; a running workspace is told it keeps its count until the next start", async () => {
    await open();
    const cpus = screen.getByLabelText("CPUs") as HTMLSelectElement;
    expect(cpus.value).toBe("4");
    expect(within(cpus).queryByText(/Use the global/)).toBeNull();
    expect(
      screen.getByText(/A running workspace keeps the count it started with/),
    ).toBeInTheDocument();
    await fireEvent.change(cpus, { target: { value: "2" } });
    await screen.findByText("CPUs saved.");
    expect(api.overrides["w-demo"]?.cpus).toBe(2);
    expect(
      await screen.findByText(/The new CPU count applies the next time/),
    ).toBeInTheDocument();
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for CPUs",
      }),
    );
    await vi.waitFor(() => expect(api.overrides["w-demo"]?.cpus).toBeNull());
  });

  it("says nothing of a restart for a stopped workspace and puts the CPU choice back when the save is refused", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    await open();
    const cpus = screen.getByLabelText("CPUs") as HTMLSelectElement;
    await fireEvent.change(cpus, { target: { value: "2" } });
    await screen.findByText("CPUs saved.");
    expect(screen.queryByText(/The new CPU count/)).toBeNull();
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "no",
    });
    await fireEvent.change(cpus, { target: { value: "8" } });
    await vi.waitFor(() => expect(cpus.value).toBe("2"));
  });

  it("doesn't mention a restart for a workspace that isn't running", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    const memory = await open();
    await fireEvent.change(memory, { target: { value: "4096" } });
    await screen.findByText("Memory saved.");
    expect(screen.queryByText(/applies the next time/)).toBeNull();
  });

  it("turns direct SSH off or back to the global setting at once, and asks the trust text before on", async () => {
    await open();
    const select = screen.getByLabelText(
      "Allow direct SSH",
    ) as HTMLSelectElement;
    const before = select.value;
    await fireEvent.change(select, { target: { value: "on" } });
    expect(workspaceActions.trustOpen).toBe(true);
    expect(workspaceActions.trustFor?.name).toBe("demo");
    expect(api.overrides["w-demo"]?.direct_ssh ?? null).toBeNull();
    expect(select.value).toBe(before);
    await fireEvent.change(select, { target: { value: "off" } });
    await screen.findByText("Allow direct SSH saved.");
    expect(api.overrides["w-demo"]?.direct_ssh).toBe(false);
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Allow direct SSH",
      }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.direct_ssh).toBeNull(),
    );
  });

  it("shows each editor default with where it comes from, and saves a workspace's own value over the global one", async () => {
    api.globalEditor = {
      colour_theme: "Default Dark Modern",
      font_size: 14,
      extensions: ["a.b"],
      keybindings: null,
    };
    await open();
    const theme = screen.getByLabelText(
      "Editor colour theme",
    ) as HTMLInputElement;
    const row = (name: string) =>
      document.querySelector(`[data-setting="${name}"]`) as HTMLElement;
    expect(theme.value).toBe("Default Dark Modern");
    expect(
      within(row("editor-theme")).getByText("Global setting"),
    ).toBeVisible();
    expect(
      within(row("editor-keybindings")).queryByText("Puddle's default"),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: /again for Editor colour theme/ }),
    ).toBeNull();

    await fireEvent.input(theme, { target: { value: "Solarized Dark" } });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save Editor colour theme" }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.editor.colour_theme).toBe(
        "Solarized Dark",
      ),
    );
    // The other fields of the group stay as they were.
    expect(api.overrides["w-demo"]?.editor.font_size).toBeNull();
    await vi.waitFor(() =>
      expect(
        within(row("editor-theme")).getByText("This workspace"),
      ).toBeVisible(),
    );

    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Editor colour theme",
      }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.editor.colour_theme).toBeNull(),
    );
    await vi.waitFor(() => expect(theme.value).toBe("Default Dark Modern"));
  });

  it("saves, clears and resets the font size, extensions and key bindings of a workspace on their own", async () => {
    await open();
    const row = (name: string) =>
      document.querySelector(`[data-setting="${name}"]`) as HTMLElement;
    const own = (name: string) =>
      vi.waitFor(() =>
        expect(within(row(name)).getByText("This workspace")).toBeVisible(),
      );
    const keys = '[{"key":"ctrl+k","command":"a.b"}]';

    await fireEvent.input(screen.getByLabelText("Font size (pixels)"), {
      target: { value: "20" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save Font size (pixels)" }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.editor.font_size).toBe(20),
    );
    await own("editor-font-size");
    await fireEvent.input(screen.getByLabelText("Font size (pixels)"), {
      target: { value: "" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save Font size (pixels)" }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.editor.font_size).toBeNull(),
    );

    await fireEvent.input(screen.getByLabelText("Extensions to install"), {
      target: { value: "a.b" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save Extensions to install" }),
    );
    await own("editor-extensions");
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Extensions to install",
      }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.editor.extensions).toBeNull(),
    );

    await fireEvent.input(screen.getByLabelText("Key bindings"), {
      target: { value: keys },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save Key bindings" }),
    );
    await own("editor-keybindings");
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Key bindings",
      }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.editor.keybindings).toBeNull(),
    );

    await fireEvent.input(screen.getByLabelText("Editor colour theme"), {
      target: { value: "Solarized Dark" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save Editor colour theme" }),
    );
    await own("editor-theme");
    await fireEvent.input(screen.getByLabelText("Editor colour theme"), {
      target: { value: "" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save Editor colour theme" }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.editor.colour_theme).toBeNull(),
    );
    await fireEvent.input(screen.getByLabelText("Font size (pixels)"), {
      target: { value: "21" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save Font size (pixels)" }),
    );
    await own("editor-font-size");
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Font size (pixels)",
      }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.editor.font_size).toBeNull(),
    );
    await fireEvent.input(screen.getByLabelText("Editor colour theme"), {
      target: { value: "Nord" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save Editor colour theme" }),
    );
    await own("editor-theme");
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Editor colour theme",
      }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.editor.colour_theme).toBeNull(),
    );
  }, 30_000);

  it("holds an automatic reload while an editor default is typed and not saved", async () => {
    await open();
    expect(hasUnsavedEdits()).toBe(false);
    await fireEvent.input(screen.getByLabelText("Font size (pixels)"), {
      target: { value: "20" },
    });
    expect(hasUnsavedEdits()).toBe(true);
  });

  it("sends nothing when Enter is pressed on an unchanged or refused value", async () => {
    await open();
    const size = screen.getByLabelText(
      "Font size (pixels)",
    ) as HTMLInputElement;
    const puts = () => api.calls.filter((c) => c.startsWith("PUT")).length;
    await fireEvent.submit(size.form!);
    await fireEvent.input(size, { target: { value: "99" } });
    await fireEvent.submit(size.form!);
    expect(puts()).toBe(0);
  });

  it("holds back a value the API would refuse and says why, and keeps an empty list as none", async () => {
    api.globalEditor = {
      colour_theme: null,
      font_size: null,
      extensions: ["a.b"],
      keybindings: null,
    };
    await open();
    const size = screen.getByLabelText("Font size (pixels)");
    await fireEvent.input(size, { target: { value: "99" } });
    expect(await screen.findByRole("alert")).toHaveTextContent("8 to 40");
    expect(
      screen.getByRole("button", { name: "Save Font size (pixels)" }),
    ).toBeDisabled();

    const list = screen.getByLabelText("Extensions to install");
    await fireEvent.input(list, { target: { value: "" } });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save Extensions to install" }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.editor.extensions).toEqual([]),
    );
    await vi.waitFor(() =>
      expect(
        within(
          document.querySelector(
            '[data-setting="editor-extensions"]',
          ) as HTMLElement,
        ).getByText("This workspace"),
      ).toBeVisible(),
    );
  });

  it("reads the direct SSH override again when the workspace's own direct SSH flips elsewhere", async () => {
    await open();
    const select = screen.getByLabelText(
      "Allow direct SSH",
    ) as HTMLSelectElement;
    expect(select.value).toBe("off");
    api.overrides["w-demo"] = {
      ...(await settingsOf("w-demo")),
      direct_ssh: true,
    };
    api.list = [workspace("demo", { direct_ssh: true })];
    await workspaces.refresh();
    await vi.waitFor(() => expect(select.value).toBe("on"));
  });

  it("shows the two tool switches on, saves each one on its own and returns to the default from the row's link", async () => {
    await open();
    const mise = screen.getByLabelText(
      "Manage this workspace's tools with mise",
    ) as HTMLSelectElement;
    const code = screen.getByLabelText(
      "Let mise run the repository's own code",
    ) as HTMLSelectElement;
    expect(mise.value).toBe("on");
    expect(code.value).toBe("on");
    await fireEvent.change(code, { target: { value: "off" } });
    await screen.findByText("Running repository code saved.");
    expect(api.tools["w-demo"]).toEqual({ mise: null, repo_code: false });
    await fireEvent.change(mise, { target: { value: "off" } });
    await screen.findByText("Tools with mise saved.");
    expect(api.tools["w-demo"]).toEqual({ mise: false, repo_code: false });
    expect(code.value).toBe("off");
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the default again for Let mise run the repository's own code",
      }),
    );
    await vi.waitFor(() =>
      expect(api.tools["w-demo"]).toEqual({ mise: false, repo_code: null }),
    );
  });

  it("puts a tool switch back when the save is refused", async () => {
    await open();
    const mise = screen.getByLabelText(
      "Manage this workspace's tools with mise",
    ) as HTMLSelectElement;
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 500,
      message: "no",
    });
    await fireEvent.change(mise, { target: { value: "off" } });
    await screen.findByText("no");
    expect(mise.value).toBe("on");
  });

  it("hides the tool switches while the global mise switch is off, and keeps their values", async () => {
    api.globalMise = false;
    api.tools["w-demo"] = { mise: false, repo_code: false };
    await open();
    expect(
      screen.queryByLabelText("Manage this workspace's tools with mise"),
    ).toBeNull();
    expect(screen.queryByRole("columnheader", { name: "Tools" })).toBeNull();
    expect(api.tools["w-demo"]).toEqual({ mise: false, repo_code: false });
  });

  it("shows no tools section in a build without the mise layer (v0.1)", async () => {
    api.miseAvailable = false;
    await open();
    expect(screen.queryByRole("columnheader", { name: "Tools" })).toBeNull();
    expect(screen.queryByText(/mise/i)).toBeNull();
  });
});
