// SPDX-License-Identifier: AGPL-3.0-or-later
import { fireEvent, screen, waitFor, within } from "@testing-library/svelte";
import { describe, expect, it, vi } from "vitest";

const { api } = await vi.hoisted(() => import("./settings-harness.ts"));
vi.mock("#lib/api/client.ts", () => ({ api }));
vi.mock("$app/navigation", () => ({ goto: vi.fn() }));
vi.mock("$app/state", () => ({
  page: {
    url: new URL("http://127.0.0.1/settings/appearance"),
    status: 200,
    error: null,
  },
}));

import { microsoftChosenToast, MS_TERMS_VERSION } from "#lib/settings/model.ts";
import { identities } from "#lib/stores/identities.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import {
  choose,
  installReset,
  open,
  putBodies,
  save,
  warmUp,
} from "./settings-setup.ts";

installReset();
await warmUp();

describe("disk space lines", () => {
  const warn = () =>
    screen.getByLabelText("Warn when free space is under (GB)");
  const fail = () =>
    screen.getByLabelText("Fail when free space is under (GB)");
  const type = async (box: HTMLElement, text: string) => {
    await fireEvent.input(box, { target: { value: text } });
    await fireEvent.change(box);
  };

  it("says the lines measure the room left on the drive, not the size of the sparse disks", async () => {
    await open();
    const section = screen
      .getByRole("heading", { level: 2, name: "Disk space" })
      .closest("section");
    expect(section).toHaveTextContent(
      "They do not measure the size of the disks",
    );
    expect(section).toHaveTextContent(
      "root disk (16 GiB for a new workspace) and its volume are sparse files",
    );
  });

  it("says the disk warning percentage is of the disk's own size, as the file takes it on the drive", async () => {
    await open();
    expect(
      screen.getByText(
        /The percentage is of the disk's own size, as the disk file takes it on the drive/,
      ),
    ).toBeInTheDocument();
  });

  it("shows Puddle's lines of 20 and 5 GB and saves a change on its Save button", async () => {
    await open();
    expect(warn()).toHaveValue("20");
    expect(fail()).toHaveValue("5");
    await type(warn(), "50");
    await type(fail(), "10");
    expect(putBodies()).toHaveLength(0);
    await save();
    expect(api.ui.disk_warn_free_gb).toBe(50);
    expect(api.ui.disk_fail_free_gb).toBe(10);
  });

  it("shows what is stored", async () => {
    api.ui.disk_warn_free_gb = 40;
    api.ui.disk_fail_free_gb = 8;
    await open();
    expect(warn()).toHaveValue("40");
    expect(fail()).toHaveValue("8");
  });

  it("refuses an amount that is not a number of GB from 1 to 10000", async () => {
    await open();
    await type(warn(), "0");
    expect(
      await screen.findByText("Use between 1 and 10000 GB."),
    ).toBeVisible();
    expect(warn()).toHaveAttribute("aria-invalid", "true");
    await type(fail(), "lots");
    expect(
      await screen.findByText("Enter a whole number of GB."),
    ).toBeVisible();
    expect(
      screen.getByRole("button", {
        name: "Save Warn when free space is under (GB)",
      }),
    ).toBeDisabled();
    await type(warn(), "30");
    await type(fail(), "6");
    await save();
    expect(api.ui.disk_warn_free_gb).toBe(30);
    expect(api.ui.disk_fail_free_gb).toBe(6);
  });

  it("refuses a failure line above the warning line", async () => {
    await open();
    await type(fail(), "25");
    expect(
      await screen.findByText(
        "The failure line (25 GB) cannot be above the warning line (20 GB).",
      ),
    ).toBeVisible();
    expect(
      screen.getByRole("button", {
        name: "Save Fail when free space is under (GB)",
      }),
    ).toBeDisabled();
    // The pair is checked against the stored lines: raise the warning line first.
    await type(warn(), "25");
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Save Warn when free space is under (GB)",
      }),
    );
    await waitFor(() =>
      expect(screen.queryByText(/cannot be above the warning line/)).toBeNull(),
    );
    await save();
    expect(api.ui.disk_warn_free_gb).toBe(25);
    expect(api.ui.disk_fail_free_gb).toBe(25);
  });

  it("puts a line back to what is stored without it counting as a change", async () => {
    await open();
    await type(warn(), "50");
    await type(warn(), "20");
    expect(
      screen.queryByRole("button", {
        name: "Save Warn when free space is under (GB)",
      }),
    ).toBeNull();
  });
});

describe("the Git defaults for new workspaces", () => {
  const push = () =>
    screen.getByRole("switch", { name: "Only push to allowed repositories" });

  it("show the stored values and save a change through the identities service", async () => {
    identities.gitDefaults = {
      only_push_listed: true,
      only_pull_listed: false,
    };
    const saved = vi
      .spyOn(identities, "setGitDefaults")
      .mockResolvedValue({ ok: true });
    await open();
    await waitFor(() => expect(push()).toBeChecked());
    expect(
      screen.getByRole("switch", {
        name: "Only pull from allowed repositories",
      }),
    ).not.toBeChecked();
    await fireEvent.click(push());
    await waitFor(() =>
      expect(saved).toHaveBeenCalledWith({ only_push_listed: false }),
    );
    saved.mockRestore();
  });

  it("explain each switch by its state, and save the pull switch by its own name", async () => {
    identities.gitDefaults = {
      only_push_listed: false,
      only_pull_listed: true,
    };
    const saved = vi
      .spyOn(identities, "setGitDefaults")
      .mockResolvedValue({ ok: true });
    await open();
    const pull = await screen.findByRole("switch", {
      name: "Only pull from allowed repositories",
    });
    await waitFor(() => expect(pull).toBeChecked());
    expect(
      screen.getByText(/Off: a push may go to any repository/),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/may fetch only from a repository/),
    ).toBeInTheDocument();
    await fireEvent.click(pull);
    await waitFor(() =>
      expect(saved).toHaveBeenCalledWith({ only_pull_listed: false }),
    );
    saved.mockRestore();
  });

  it("put a refused change back and say why", async () => {
    identities.gitDefaults = {
      only_push_listed: true,
      only_pull_listed: false,
    };
    const refused = vi
      .spyOn(identities, "setGitDefaults")
      .mockResolvedValue({ ok: false, message: "Nope." });
    await open();
    await waitFor(() => expect(push()).toBeChecked());
    await fireEvent.click(push());
    await waitFor(() => expect(toasts.items.at(-1)?.message).toBe("Nope."));
    expect(push()).toBeChecked();
    refused.mockRestore();
  });
});

describe("SSH agent pipe", () => {
  const field = () => screen.getByLabelText("SSH agent pipe or socket");
  const type = async (box: HTMLElement, text: string) => {
    await fireEvent.input(box, { target: { value: text } });
    await fireEvent.change(box);
  };

  it("starts empty, saves a typed pipe trimmed on Save and shows what is stored", async () => {
    await open();
    expect(field()).toHaveValue("");
    await type(field(), String.raw`  \\.\pipe\pageant.alice  `);
    expect(putBodies()).toHaveLength(0);
    await save();
    expect(api.ui.ssh_agent_pipe).toBe(String.raw`\\.\pipe\pageant.alice`);
  });

  it("shows a stored pipe and clears it with an empty field", async () => {
    api.ui.ssh_agent_pipe = "/run/agent.sock";
    await open();
    expect(field()).toHaveValue("/run/agent.sock");
    await type(field(), "");
    await save();
    expect(api.ui.ssh_agent_pipe).toBeNull();
  });

  it("refuses a control character and holds its Save until it is fixed", async () => {
    await open();
    await type(field(), "/run/a\tb");
    expect(
      await screen.findByText(
        "A pipe or socket path cannot hold a control character.",
      ),
    ).toBeVisible();
    expect(field()).toHaveAttribute("aria-invalid", "true");
    expect(
      screen.getByRole("button", { name: "Save SSH agent pipe or socket" }),
    ).toBeDisabled();
    await type(field(), "/run/agent.sock");
    await save();
    expect(api.ui.ssh_agent_pipe).toBe("/run/agent.sock");
  });

  it("puts the field back to what is stored without it counting as a change", async () => {
    api.ui.ssh_agent_pipe = "/run/agent.sock";
    await open();
    await type(field(), "/other");
    await type(field(), "/run/agent.sock");
    expect(
      screen.queryByRole("button", { name: "Save SSH agent pipe or socket" }),
    ).toBeNull();
  });
});

describe("Microsoft's server", () => {
  async function pick() {
    await open();
    await choose("Server", "microsoft");
    return await screen.findByRole("dialog");
  }

  it("asks first, in the agreed words, with telemetry unchecked", async () => {
    const dialog = await pick();
    expect(
      within(dialog).getByRole("heading", {
        name: "Use Microsoft's VS Code server?",
      }),
    ).toBeInTheDocument();
    expect(dialog).toHaveTextContent(
      "Puddle downloads the server from Microsoft",
    );
    const link = within(dialog).getByRole("link", {
      name: /Microsoft VS Code Server licence terms/,
    });
    expect(link).toHaveAttribute(
      "href",
      "https://code.visualstudio.com/license/server",
    );
    expect(link).toHaveAttribute("rel", expect.stringContaining("noopener"));
    expect(
      within(dialog).getByLabelText(
        "Allow the server to send telemetry to Microsoft",
      ),
    ).not.toBeChecked();
    // Nothing is stored yet, and the select still shows code-server.
    expect(api.consent.state).toBe("not_asked");
    expect(screen.getByLabelText("Server")).toHaveValue("code_server");
  });

  it("keeps code-server and records nothing when declined", async () => {
    const dialog = await pick();
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Keep code-server" }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.consent.state).toBe("not_asked");
    expect(api.vscode.server).toBeNull();
    expect(api.calls.filter((c) => c.startsWith("PUT"))).toEqual([]);
  });

  it("records the consent with the terms version and chooses the server", async () => {
    const dialog = await pick();
    await fireEvent.click(
      within(dialog).getByLabelText(
        "Allow the server to send telemetry to Microsoft",
      ),
    );
    await fireEvent.click(
      within(dialog).getByRole("button", {
        name: "Accept and use Microsoft's server",
      }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(api.consent).toMatchObject({
      state: "granted",
      terms_version: MS_TERMS_VERSION,
    });
    expect(api.vscode).toMatchObject({ server: "microsoft", telemetry: true });
    expect(screen.getByLabelText("Server")).toHaveValue("microsoft");
    expect(screen.getByLabelText("Send Microsoft telemetry")).toBeChecked();
    expect(screen.getByText(/You accepted Microsoft's terms on/)).toBeVisible();
    expect(toasts.items[0]?.message).toBe(microsoftChosenToast(true));
    // The page draws it: the queue alone showed nothing.
    expect(
      screen.getByText(/downloads the server from Microsoft the next time/),
    ).toBeVisible();
    expect(toasts.items[0]?.message).toMatch(/telemetry is on/);
  });

  it("starts every popup with telemetry off", async () => {
    const dialog = await pick();
    await fireEvent.click(
      within(dialog).getByLabelText(
        "Allow the server to send telemetry to Microsoft",
      ),
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Keep code-server" }),
    );
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    await choose("Server", "microsoft");
    const again = await screen.findByRole("dialog");
    expect(
      within(again).getByLabelText(
        "Allow the server to send telemetry to Microsoft",
      ),
    ).not.toBeChecked();
  });

  it("keeps the popup open and says why when Puddle refuses", async () => {
    const dialog = await pick();
    api.refuse.set("PUT /api/consents/{kind}", {
      status: 500,
      error: "internal",
      message: "disk is full",
    });
    await fireEvent.click(
      within(dialog).getByRole("button", {
        name: "Accept and use Microsoft's server",
      }),
    );
    expect(await within(dialog).findByRole("alert")).toHaveTextContent(
      "disk is full",
    );
    expect(api.vscode.server).toBeNull();
  });

  it("does not ask again once the terms were accepted, and can switch back", async () => {
    api.consent = {
      state: "granted",
      at: 1_700_000_000_000,
      terms_version: MS_TERMS_VERSION,
    };
    await open();
    await choose("Server", "microsoft");
    expect(screen.queryByRole("dialog")).toBeNull();
    await save();
    expect(api.vscode.server).toBe("microsoft");
    await fireEvent.click(screen.getByLabelText("Send Microsoft telemetry"));
    await save();
    expect(api.vscode.telemetry).toBe(true);
    await choose("Server", "code_server");
    await save();
    expect(api.vscode.server).toBe("code_server");
    expect(screen.queryByLabelText("Send Microsoft telemetry")).toBeNull();
    expect(api.consent.state).toBe("granted");
  });

  it("withdraws the consent: records a decline, goes back to code-server and asks again next time", async () => {
    api.consent = {
      state: "granted",
      at: 1_700_000_000_000,
      terms_version: MS_TERMS_VERSION,
    };
    api.vscode = { server: "microsoft", telemetry: true, auto_update: null };
    await open();
    expect(screen.getByLabelText("Server")).toHaveValue("microsoft");
    await fireEvent.click(
      screen.getByRole("button", { name: "Withdraw consent" }),
    );
    await screen.findByText(/Consent withdrawn/);
    expect(api.consent).toMatchObject({
      state: "declined",
      terms_version: MS_TERMS_VERSION,
    });
    expect(api.vscode.server).toBe("code_server");
    // The telemetry choice stays; the screen shows what the API holds now.
    expect(api.vscode.telemetry).toBe(true);
    expect(screen.getByLabelText("Server")).toHaveValue("code_server");
    expect(
      screen.queryByRole("button", { name: "Withdraw consent" }),
    ).toBeNull();
    expect(screen.queryByText(/You accepted Microsoft's terms/)).toBeNull();
    // Choosing Microsoft's server again shows the popup: nothing is assumed from before.
    await choose("Server", "microsoft");
    expect(await screen.findByRole("dialog")).toBeVisible();
  });

  it("offers no withdrawal before the terms were accepted", async () => {
    await open();
    expect(
      screen.queryByRole("button", { name: "Withdraw consent" }),
    ).toBeNull();
  });

  it("says when Puddle refuses the withdrawal and keeps the consent", async () => {
    api.consent = {
      state: "granted",
      at: 1_700_000_000_000,
      terms_version: MS_TERMS_VERSION,
    };
    await open();
    api.refuse.set("PUT /api/consents/{kind}", {
      status: 500,
      error: "internal",
      message: "disk is full",
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Withdraw consent" }),
    );
    expect(await screen.findByText(/disk is full/)).toBeVisible();
    expect(api.consent.state).toBe("granted");
  });

  it("says so when the service does not answer the withdrawal", async () => {
    api.consent = {
      state: "granted",
      at: 1_700_000_000_000,
      terms_version: MS_TERMS_VERSION,
    };
    await open();
    api.down = true;
    await fireEvent.click(
      screen.getByRole("button", { name: "Withdraw consent" }),
    );
    // The restore card, which also reads from the service, may say the same: several places do.
    const said = await screen.findAllByText(/service isn't answering/);
    expect(said[0]).toBeVisible();
    expect(api.consent.state).toBe("granted");
  });

  it("asks again when the terms changed since the user agreed", async () => {
    api.consent = { state: "granted", at: 1, terms_version: "older-terms" };
    await open();
    await choose("Server", "microsoft");
    expect(await screen.findByRole("dialog")).toBeVisible();
  });

  it("saves automatic updates", async () => {
    await open();
    await fireEvent.click(
      screen.getByLabelText("Update the server automatically"),
    );
    await save();
    expect(api.vscode.auto_update).toBe(false);
  });
});

describe("the editor defaults", () => {
  it("shows them unset, saves each one and sends the group whole", async () => {
    await open();
    expect(screen.getByLabelText("Editor colour theme")).toHaveValue("");
    await fireEvent.input(screen.getByLabelText("Editor colour theme"), {
      target: { value: "Default Dark Modern" },
    });
    await save();
    expect(api.layer.editor.colour_theme).toBe("Default Dark Modern");

    await fireEvent.input(screen.getByLabelText("Font size (pixels)"), {
      target: { value: "16" },
    });
    await save();
    expect(api.layer.editor).toMatchObject({
      colour_theme: "Default Dark Modern",
      font_size: 16,
    });

    await fireEvent.input(screen.getByLabelText("Extensions to install"), {
      target: { value: "Rust-Lang.Rust-Analyzer\nesbenp.prettier-vscode" },
    });
    await save();
    expect(api.layer.editor.extensions).toEqual([
      "esbenp.prettier-vscode",
      "rust-lang.rust-analyzer",
    ]);

    const bindings =
      '[{"key":"ctrl+alt+t","command":"workbench.action.terminal.toggleTerminal"}]';
    await fireEvent.input(screen.getByLabelText("Key bindings"), {
      target: { value: bindings },
    });
    await save();
    expect(api.layer.editor.keybindings).toBe(bindings);

    const own = '{"editor.tabSize": 2}';
    await fireEvent.input(screen.getByLabelText("Custom editor settings"), {
      target: { value: own },
    });
    await save();
    expect(api.layer.editor.settings).toBe(own);
  });

  it("clears a field when its box is emptied", async () => {
    api.layer.editor.font_size = 18;
    api.layer.editor.extensions = ["a.b"];
    await open();
    expect(screen.getByLabelText("Font size (pixels)")).toHaveValue("18");
    await fireEvent.input(screen.getByLabelText("Font size (pixels)"), {
      target: { value: "" },
    });
    await fireEvent.input(screen.getByLabelText("Extensions to install"), {
      target: { value: "" },
    });
    await save();
    expect(api.layer.editor.font_size).toBeNull();
    expect(api.layer.editor.extensions).toBeNull();
  });

  it("says why a value cannot be saved and sends nothing", async () => {
    await open();
    await fireEvent.input(screen.getByLabelText("Font size (pixels)"), {
      target: { value: "4" },
    });
    expect(await screen.findByRole("alert")).toHaveTextContent("8 to 40");
    expect(
      screen.getByRole("button", { name: "Save Font size (pixels)" }),
    ).toBeDisabled();
    await fireEvent.input(screen.getByLabelText("Key bindings"), {
      target: { value: "{}" },
    });
    await waitFor(() =>
      expect(screen.getAllByRole("alert").at(-1)).toHaveTextContent(
        "JSON array",
      ),
    );
    expect(putBodies()).toHaveLength(0);
  });
});
