// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  within,
} from "@testing-library/svelte";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { components } from "#lib/api/schema.d.ts";

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

import { notices } from "#lib/notify/notices.svelte.ts";
import Settings from "./settings/+page.svelte";
import { installReset, load, openSettingsTab } from "./detail-setup.ts";

const { api, workspace } = h;

installReset();

describe("the settings tab, logins and certificates", () => {
  const open = openSettingsTab;
  beforeEach(() => {
    url.pathname = "/workspaces/w-demo/settings";
  });

  it("keeps logins by default, turns capture off or back to the global setting, and says the next start applies it", async () => {
    await open();
    const select = screen.getByLabelText(
      "Keep logins made in this workspace",
    ) as HTMLSelectElement;
    expect(select.value).toBe("on");
    expect(
      within(select)
        .getAllByRole("option")
        .map((o) => o.textContent),
    ).toEqual(["On", "Off"]);
    await fireEvent.change(select, { target: { value: "off" } });
    await screen.findByText("Login capture saved.");
    expect(api.overrides["w-demo"]?.capture_logins).toBe(false);
    expect(
      await screen.findByText(
        "The new login setting applies the next time the workspace starts.",
      ),
    ).toBeInTheDocument();
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Keep logins made in this workspace",
      }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.capture_logins).toBeNull(),
    );
  });

  it("warns about a credential Puddle did not add by default and lets the workspace turn the warning off or follow the global setting", async () => {
    await open();
    const select = screen.getByLabelText(
      "Warn when a tool sends a token of its own",
    ) as HTMLSelectElement;
    expect(select.value).toBe("on");
    await fireEvent.change(select, { target: { value: "off" } });
    await screen.findByText("Credential warning saved.");
    expect(api.overrides["w-demo"]?.warn_foreign_credentials).toBe(false);
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Warn when a tool sends a token of its own",
      }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.warn_foreign_credentials).toBeNull(),
    );
  });

  it("sets the interception mode at once and saves the never-decrypt names", async () => {
    await open();
    const select = (await screen.findByRole("combobox", {
      name: "Which hosts to decrypt",
    })) as HTMLSelectElement;
    expect(select.value).toBe("known_only");
    expect(within(select).queryByText(/Use the global/)).toBeNull();
    await fireEvent.change(select, { target: { value: "all" } });
    await screen.findByText("Interception mode saved.");
    expect(api.interception["w-demo"]?.mode).toBe("all");
    expect(
      screen.getByText(/fails on first contact with a host until Puddle/),
    ).toBeInTheDocument();
    const names = screen.getByRole("textbox", {
      name: "Never decrypt in this workspace",
    });
    await fireEvent.input(names, {
      target: { value: "Bank.example\n*.corp.test" },
    });
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Save Never decrypt in this workspace",
      }),
    );
    await screen.findByText("Saved.");
    expect(api.interception["w-demo"]?.never_decrypt).toEqual([
      "*.corp.test",
      "bank.example",
    ]);
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Which hosts to decrypt",
      }),
    );
    await vi.waitFor(() =>
      expect(api.interception["w-demo"]?.mode).toBe("inherit"),
    );
  });

  it("keeps the workspace's own interception mode and says why when following the global mode again is refused", async () => {
    await open();
    const select = (await screen.findByRole("combobox", {
      name: "Which hosts to decrypt",
    })) as HTMLSelectElement;
    await fireEvent.change(select, { target: { value: "all" } });
    await screen.findByText("Interception mode saved.");
    api.refuse.set("PUT /api/workspaces/{id}/interception", {
      status: 500,
      message: "disk full",
    });
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Which hosts to decrypt",
      }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent("Disk full.");
    expect(select.value).toBe("all");
    expect(api.interception["w-demo"]?.mode).toBe("all");
  });

  it("names the global never-decrypt list read-only, and puts the mode back when the save is refused", async () => {
    api.globalNeverDecrypt = ["bank.example"];
    await open();
    const select = (await screen.findByRole("combobox", {
      name: "Which hosts to decrypt",
    })) as HTMLSelectElement;
    expect(
      screen.getByText(/Never decrypted in any workspace/),
    ).toHaveTextContent("bank.example");
    api.refuse.set("PUT /api/workspaces/{id}/interception", {
      status: 500,
      message: "disk full",
    });
    await fireEvent.change(select, { target: { value: "all" } });
    expect(await screen.findByRole("alert")).toHaveTextContent("Disk full.");
    expect(select.value).toBe("known_only");
  });

  const seen = (
    host: string,
    expires_in_secs: number | null,
    count = 1,
  ): components["schemas"]["LearnedHost"] => ({
    host,
    passed_through: expires_in_secs !== null,
    expires_in_secs,
    first_rejected_at: Date.now() - 3 * 3_600_000,
    last_rejected_at: Date.now() - 5 * 60_000,
    count,
  });

  it("lists the hosts a tool refused Puddle's certificate for with their history and tries decrypting them again", async () => {
    api.learned["w-demo"] = [
      seen("pinned.example", 600, 3),
      seen("other.example", 7200),
      seen("old.example", null, 2),
    ];
    await open();
    const group = await screen.findByRole("group", {
      name: "Hosts a tool refused Puddle's certificate for",
    });
    await screen.findByText("pinned.example");
    expect(group).toHaveTextContent("refused 3 times, first 3 hours ago");
    expect(group).toHaveTextContent("last 5 minutes ago");
    expect(group).toHaveTextContent("decrypted again by itself in 10 minutes");
    expect(group).toHaveTextContent("2 hours");
    expect(group).toHaveTextContent("refused once");
    expect(
      screen.queryByRole("button", {
        name: "Try decrypting old.example again",
      }),
      "a host that is not passed through has nothing to try again",
    ).toBeNull();
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Try decrypting pinned.example again",
      }),
    );
    await vi.waitFor(() =>
      expect(
        screen.queryByRole("button", {
          name: "Try decrypting pinned.example again",
        }),
      ).toBeNull(),
    );
    expect(group).toHaveTextContent("pinned.example");
    await fireEvent.click(
      screen.getByRole("button", { name: "Try decrypting all again" }),
    );
    await vi.waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Try decrypting all again" }),
      ).toBeNull(),
    );
    expect(group).toHaveTextContent("other.example");
  });

  it("makes a refused host permanent for the workspace or for every workspace", async () => {
    api.learned["w-demo"] = [
      seen("pinned.example", 600, 3),
      seen("other.example", null, 2),
    ];
    await open();
    const group = await screen.findByRole("group", {
      name: "Hosts a tool refused Puddle's certificate for",
    });
    await screen.findByText("pinned.example");
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Never decrypt pinned.example in this workspace",
      }),
    );
    await vi.waitFor(() =>
      expect(group).not.toHaveTextContent("pinned.example"),
    );
    expect(api.interception["w-demo"]?.never_decrypt).toEqual([
      "pinned.example",
    ]);
    await vi.waitFor(() =>
      expect(
        screen.getByLabelText("Never decrypt in this workspace"),
        "the names above the list show the new one",
      ).toHaveValue("pinned.example"),
    );
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Never decrypt other.example in any workspace",
      }),
    );
    await vi.waitFor(() => expect(group).toHaveTextContent("None."));
    expect(api.globalNeverDecrypt).toContain("other.example");
    await vi.waitFor(() =>
      expect(
        screen.getByText(/Never decrypted in any workspace/),
      ).toHaveTextContent("other.example"),
    );
  });

  it("says why a host could not be tried again and keeps the list", async () => {
    api.learned["w-demo"] = [seen("pinned.example", 600)];
    await open();
    await screen.findByText("pinned.example");
    api.refuse.set("DELETE /api/workspaces/{id}/interception/learned/{host}", {
      status: 500,
      message: "the list is locked",
    });
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Try decrypting pinned.example again",
      }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The list is locked.",
    );
    expect(screen.getByText("pinned.example")).toBeInTheDocument();
  });

  it("says when the learned list can't be read", async () => {
    api.refuse.set("GET /api/workspaces/{id}/interception/learned", {
      status: 500,
      message: "list broken",
    });
    await open();
    expect(await screen.findByText("List broken.")).toBeInTheDocument();
  });

  it("says when the decrypt setting can't be read", async () => {
    api.refuse.set("GET /api/workspaces/{id}/interception", {
      status: 500,
      message: "store broken",
    });
    await open();
    expect(await screen.findByText("Store broken.")).toBeInTheDocument();
    expect(
      screen.queryByRole("switch", { name: "Decrypt every host" }),
    ).toBeNull();
  });

  it("doesn't mention a restart for login capture when the workspace isn't running, and puts the choice back when the save is refused", async () => {
    api.list = [workspace("demo", { status: "stopped" })];
    await open();
    const select = screen.getByLabelText(
      "Keep logins made in this workspace",
    ) as HTMLSelectElement;
    await fireEvent.change(select, { target: { value: "on" } });
    await screen.findByText("Login capture saved.");
    expect(screen.queryByText(/applies the next time/)).toBeNull();
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "no",
    });
    await fireEvent.change(select, { target: { value: "off" } });
    await vi.waitFor(() => expect(select.value).toBe("on"));
  });

  it("puts the credential warning choice back when the save is refused", async () => {
    await open();
    const select = screen.getByLabelText(
      "Warn when a tool sends a token of its own",
    ) as HTMLSelectElement;
    await fireEvent.change(select, { target: { value: "off" } });
    await screen.findByText("Credential warning saved.");
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "no",
    });
    await fireEvent.change(select, { target: { value: "on" } });
    await vi.waitFor(() => expect(select.value).toBe("off"));
  });

  it("lists the kept logins on the settings page, forgets one after a confirmation, and ends its notice", async () => {
    api.kept["w-demo"] = {
      store_readable: true,
      logins: [
        { service: "claude", name: "Claude Code", since: 1_761_480_000_000 },
        { service: "github", name: "GitHub", since: null },
      ],
    };
    notices.add({
      key: "login:w-demo:Claude Code:capture_off",
      tone: "warning",
      title: "Claude Code login in demo can't be used.",
    });
    await open();
    const list = await screen.findByRole("region", { name: "Kept logins" });
    expect(within(list).getAllByRole("listitem")).toHaveLength(2);
    expect(list).toHaveTextContent("Claude Code");
    expect(list).toHaveTextContent("Kept since a time Puddle did not record.");
    expect(list.textContent).not.toMatch(/sk-|gho_|token/i);

    await fireEvent.click(
      within(list).getByRole("button", {
        name: "Forget the Claude Code login",
      }),
    );
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveAccessibleName(
      "Forget the Claude Code login in demo?",
    );
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Forget login" }),
    );
    await vi.waitFor(() =>
      expect(screen.queryByRole("alertdialog")).toBeNull(),
    );
    expect(api.calls).toContain("DELETE /api/workspaces/{id}/logins/{service}");
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    expect(list).toHaveTextContent("Forgot the Claude Code login.");
    expect(
      notices.items.some(
        (n) => n.key === "login:w-demo:Claude Code:capture_off",
      ),
    ).toBe(false);
  });

  it("keeps the row and says why when the forget is refused, and Cancel closes the dialog", async () => {
    api.kept["w-demo"] = {
      store_readable: true,
      logins: [{ service: "claude", name: "Claude Code", since: null }],
    };
    api.refuse.set("DELETE /api/workspaces/{id}/logins/{service}", {
      status: 500,
      message: "boom",
    });
    await open();
    const list = await screen.findByRole("region", { name: "Kept logins" });
    await fireEvent.click(
      within(list).getByRole("button", {
        name: "Forget the Claude Code login",
      }),
    );
    const dialog = await screen.findByRole("alertdialog");
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Forget login" }),
    );
    expect(await within(dialog).findByText("Boom.")).toBeInTheDocument();
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Cancel" }),
    );
    await vi.waitFor(() =>
      expect(screen.queryByRole("alertdialog")).toBeNull(),
    );
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
  });

  it("says so when no login is kept, when the list can't be read, and when the credential store did not answer", async () => {
    await open();
    expect(
      await screen.findByText(/keeps no logins for this workspace/),
    ).toBeInTheDocument();
    cleanup();
    api.kept["w-demo"] = { logins: [], store_readable: false };
    await open();
    expect(
      await screen.findByText(/couldn't read this computer's credential store/),
    ).toBeInTheDocument();
    cleanup();
    api.kept = {};
    api.refuse.set("GET /api/workspaces/{id}/logins", {
      status: 500,
      message: "boom",
    });
    await open();
    expect(
      await screen.findByText("Couldn't read the kept logins."),
    ).toBeInTheDocument();
    expect(screen.getByText("Boom.")).toBeInTheDocument();
    await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(
      await screen.findByText(/keeps no logins for this workspace/),
    ).toBeInTheDocument();
  });

  it("reads the kept logins again when the credential store did not answer and the user retries", async () => {
    api.kept["w-demo"] = { logins: [], store_readable: false };
    await open();
    await screen.findByText(/couldn't read this computer's credential store/);
    api.kept["w-demo"] = { logins: [], store_readable: true };
    await fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(
      await screen.findByText(/keeps no logins for this workspace/),
    ).toBeInTheDocument();
  });

  it("saves a local toggle and the clipboard, keeping the others as they were", async () => {
    await open();
    await fireEvent.change(screen.getByLabelText("Private networks"), {
      target: { value: "on" },
    });
    await screen.findByText("Private networks saved.");
    expect(api.overrides["w-demo"]?.local_toggles.private).toBe(true);
    expect(api.overrides["w-demo"]?.local_toggles.loopback).toBeNull();
    await fireEvent.change(
      screen.getByLabelText("Pages reading the clipboard"),
      {
        target: { value: "deny" },
      },
    );
    await screen.findByText("Clipboard saved.");
    expect(api.overrides["w-demo"]?.clipboard_read).toBe("deny");
    expect(api.overrides["w-demo"]?.local_toggles.private).toBe(true);
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Use the global setting again for Pages reading the clipboard",
      }),
    );
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.clipboard_read).toBeNull(),
    );
    await fireEvent.change(screen.getByLabelText("Private networks"), {
      target: { value: "off" },
    });
    await vi.waitFor(() =>
      expect(api.overrides["w-demo"]?.local_toggles.private).toBe(false),
    );
  });

  it("puts the choice back and says why when the save is refused", async () => {
    const memory = await open();
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "memory is too small",
    });
    await fireEvent.change(memory, { target: { value: "4096" } });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "memory is too small",
    );
    await vi.waitFor(() => expect(memory.value).toBe("8192"));
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "no",
    });
    const toggle = screen.getByLabelText("Link-local") as HTMLSelectElement;
    await fireEvent.change(toggle, { target: { value: "on" } });
    await vi.waitFor(() => expect(toggle.value).toBe("off"));
    api.refuse.set("PUT /api/settings/workspaces/{workspace}", {
      status: 422,
      message: "no",
    });
    const clipboard = screen.getByLabelText(
      "Pages reading the clipboard",
    ) as HTMLSelectElement;
    await fireEvent.change(clipboard, { target: { value: "deny" } });
    await vi.waitFor(() => expect(clipboard.value).toBe("allow"));
  });

  it("says when the settings can't be read, and while they load", async () => {
    await load();
    api.refuse.set("GET /api/settings/workspaces/{workspace}", {
      status: 500,
      message: "x",
    });
    render(Settings);
    expect(screen.getByText(/Loading settings/)).toBeInTheDocument();
    expect(
      await screen.findByText(/Couldn't read this workspace's settings/),
    ).toBeInTheDocument();
  });

  it("shows nothing for a workspace it doesn't know", async () => {
    url.id = "nope";
    await load();
    const { container } = render(Settings);
    expect(container.textContent?.trim()).toBe("");
  });
});
