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

import { report } from "#lib/testing/fake-network.ts";
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

describe("workspace defaults and the network", () => {
  it("sets the default CPU count from 1 up to the computer's cores and says a running workspace keeps its own", async () => {
    await open();
    const cpus = screen.getByLabelText("Default CPUs") as HTMLSelectElement;
    expect(cpus).toHaveValue("4");
    expect([...cpus.options].map((o) => o.value)).toEqual(
      Array.from({ length: 16 }, (_, i) => String(i + 1)),
    );
    expect(cpus.options[0]?.text).toBe("1 CPU");
    expect(
      screen.getByText(/A running workspace keeps the count it started with/),
    ).toBeInTheDocument();
    await choose("Default CPUs", "6");
    await save();
    expect(api.layer.cpus).toBe(6);
    expect(screen.getByLabelText("Default CPUs")).toHaveValue("6");
  });

  it("takes the CPU maximum from the setting, not from a fixed 32, and saves a new one", async () => {
    api.hostCpus = 64;
    await open();
    const cpus = screen.getByLabelText("Default CPUs") as HTMLSelectElement;
    expect(cpus.options).toHaveLength(64);
    const max = screen.getByLabelText(
      "Most CPUs a workspace may have",
    ) as HTMLInputElement;
    expect(max.value).toBe("");
    expect(max.placeholder).toBe("64");
    await fireEvent.input(max, { target: { value: "6" } });
    await save();
    expect(api.limits.max_cpus).toBe(6);
    await waitFor(() =>
      expect(
        (screen.getByLabelText("Default CPUs") as HTMLSelectElement).options,
      ).toHaveLength(6),
    );
    await fireEvent.input(max, { target: { value: "" } });
    await save();
    expect(api.limits.max_cpus).toBeNull();
  });

  it("refuses a CPU maximum that is not a whole number from 1 to 255", async () => {
    await open();
    const max = screen.getByLabelText("Most CPUs a workspace may have");
    await fireEvent.input(max, { target: { value: "300" } });
    expect(screen.getByRole("alert")).toHaveTextContent(
      "Use a whole number from 1 to 255.",
    );
    expect(
      screen.getByRole("button", {
        name: "Save Most CPUs a workspace may have",
      }),
    ).toBeDisabled();
    expect(putBodies()).toHaveLength(0);
  });

  it("keeps the CPU maximum when another setting is saved", async () => {
    api.limits = { max_cpus: 9 };
    await open();
    await fireEvent.click(screen.getByLabelText("Sound"));
    await save();
    expect(api.limits.max_cpus).toBe(9);
  });

  it("changes the default memory", async () => {
    await open();
    await choose("Default memory", "16384");
    await save();
    const row = screen.getByLabelText("Default memory").closest(".control");
    expect(
      within(row as HTMLElement).getByText(
        "Applies when a workspace next starts.",
      ),
    ).toBeInTheDocument();
    expect(api.layer.memory).toBe(16_384);
  });

  it("switches a local category on without touching the others", async () => {
    await open();
    await fireEvent.click(screen.getByLabelText("Private networks"));
    await save();
    expect(api.layer.local_toggles.private).toBe(true);
    expect(api.layer.local_toggles.loopback).toBeNull();
    await fireEvent.click(screen.getByLabelText("This computer (loopback)"));
    await save();
    expect(api.layer.local_toggles.loopback).toBe(true);
    expect(api.layer.local_toggles.private).toBe(true);
  });

  it("explains that a toggle only makes a destination approvable", async () => {
    await open();
    expect(
      screen.getByText(/each one still needs a rule or your approval/),
    ).toBeInTheDocument();
  });

  it("saves suffix-rule reach, zoom and clipboard", async () => {
    await open();
    await fireEvent.click(
      screen.getByLabelText("Let suffix rules reach local addresses"),
    );
    await fireEvent.click(screen.getByLabelText("Zoom shortcuts"));
    await choose("Clipboard reads by pages", "deny");
    await save();
    expect(api.layer.wildcards_reach_local).toBe(true);
    expect(api.layer.zoom_hotkeys).toBe(false);
    expect(api.layer.clipboard_read).toBe("deny");
  });

  it("shows the names no workspace decrypts and saves a new list", async () => {
    api.neverDecrypt = ["bank.example"];
    await open();
    const box = (await screen.findByRole("textbox", {
      name: "Never decrypt, in any workspace",
    })) as HTMLTextAreaElement;
    expect(box.value).toBe("bank.example");
    await fireEvent.input(box, {
      target: { value: "bank.example\n*.corp.test" },
    });
    await fireEvent.click(
      within(box.closest("form") as HTMLElement).getByRole("button", {
        name: "Save Never decrypt, in any workspace",
      }),
    );
    await screen.findByText("Saved.");
    expect(api.neverDecrypt).toEqual(["*.corp.test", "bank.example"]);
  });

  it("stores a proxy sign-in, lists it by name, never shows the password and forgets it on Remove", async () => {
    await open();
    await screen.findByText("No proxy sign-in is stored.");
    await fireEvent.input(screen.getByLabelText("Proxy"), {
      target: { value: "proxy.corp.example:8080" },
    });
    await fireEvent.input(screen.getByLabelText("User name"), {
      target: { value: "alice" },
    });
    const password = screen.getByLabelText("Password") as HTMLInputElement;
    expect(password.type).toBe("password");
    await fireEvent.input(password, { target: { value: "s3cret-pw" } });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save proxy sign-in" }),
    );
    await screen.findByText("Saved.");
    expect(api.proxyPasswords.get("proxy.corp.example:8080")).toBe("s3cret-pw");
    const list = screen.getByRole("list", { name: "Stored proxy sign-ins" });
    expect(list).toHaveTextContent("proxy.corp.example:8080");
    expect(list).toHaveTextContent("signs in as alice");
    expect(document.body.textContent).not.toContain("s3cret-pw");
    // The boxes are empty again: nothing keeps the password on the screen.
    expect(password.value).toBe("");

    await fireEvent.click(
      screen.getByRole("button", {
        name: "Remove the sign-in for proxy.corp.example:8080",
      }),
    );
    await screen.findByText("No proxy sign-in is stored.");
    expect(api.proxySignIns).toEqual([]);
  });

  it("says why a proxy sign-in was refused and keeps what was typed", async () => {
    await open();
    await screen.findByText("No proxy sign-in is stored.");
    api.refuse.set("PUT /api/proxy-sign-ins", {
      status: 503,
      error: "unavailable",
      message:
        "the operating system's credential store did not answer; unlock it and try again",
    });
    await fireEvent.input(screen.getByLabelText("Proxy"), {
      target: { value: "p:1" },
    });
    await fireEvent.input(screen.getByLabelText("User name"), {
      target: { value: "alice" },
    });
    await fireEvent.input(screen.getByLabelText("Password"), {
      target: { value: "pw" },
    });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save proxy sign-in" }),
    );
    expect(
      await screen.findByText(/credential store did not answer/),
    ).toBeInTheDocument();
    expect((screen.getByLabelText("Password") as HTMLInputElement).value).toBe(
      "pw",
    );
  });

  it("offers the proxies the network report names, and says when a removal was refused", async () => {
    const r = report();
    r.proxy.https_proxy = "proxy.corp.example:8080";
    api.health = r;
    api.proxySignIns = [{ proxy: "proxy.corp.example:8080", user: "alice" }];
    await open();
    await screen.findByRole("list", { name: "Stored proxy sign-ins" });
    await waitFor(() => {
      const options = document.querySelectorAll("datalist option");
      expect([...options].map((o) => o.getAttribute("value"))).toEqual([
        "proxy.corp.example:8080",
      ]);
    });
    api.refuse.set("DELETE /api/proxy-sign-ins/{proxy}", {
      status: 503,
      error: "unavailable",
      message: "the credential store did not answer",
    });
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Remove the sign-in for proxy.corp.example:8080",
      }),
    );
    expect(
      await screen.findByText("The credential store did not answer."),
    ).toBeInTheDocument();
    expect(api.proxySignIns).toHaveLength(1);
  });

  it("says the proxy sign-ins can't be read, and shows no form", async () => {
    api.refuse.set("GET /api/proxy-sign-ins", {
      status: 500,
      error: "internal",
      message: "sign-ins broken",
    });
    await open();
    expect(await screen.findByText("Sign-ins broken.")).toBeInTheDocument();
    expect(screen.queryByLabelText("User name")).toBeNull();
  });

  it("shows the limits for hosts left alone and saves new ones in the units the API keeps", async () => {
    await open();
    const minutes = (await screen.findByLabelText(
      "Leave a host alone for (minutes)",
    )) as HTMLInputElement;
    const hosts = screen.getByLabelText(
      "Hosts remembered per workspace",
    ) as HTMLInputElement;
    expect([minutes.value, hosts.value]).toEqual(["720", "1024"]);
    await fireEvent.input(minutes, { target: { value: "90" } });
    await fireEvent.input(hosts, { target: { value: "64" } });
    await fireEvent.click(
      screen.getByRole("button", { name: "Save limits for hosts left alone" }),
    );
    await screen.findByText("Saved.");
    expect(api.learnedLimits).toEqual({ ttl_secs: 5400, cap: 64 });
  });

  it("says why the limits were refused, and when they can't be read", async () => {
    await open();
    await screen.findByLabelText("Leave a host alone for (minutes)");
    api.refuse.set("PUT /api/interception/learned-limits", {
      status: 422,
      error: "invalid",
      message: "the size must be 1 to 10000 hosts",
    });
    await fireEvent.input(
      screen.getByLabelText("Hosts remembered per workspace"),
      { target: { value: "64" } },
    );
    await fireEvent.click(
      screen.getByRole("button", { name: "Save limits for hosts left alone" }),
    );
    expect(
      await screen.findByText("The size must be 1 to 10000 hosts."),
    ).toBeInTheDocument();
  });

  it("says the limits can't be read, and shows no boxes", async () => {
    api.refuse.set("GET /api/interception/learned-limits", {
      status: 500,
      error: "internal",
      message: "limits broken",
    });
    await open();
    expect(await screen.findByText("Limits broken.")).toBeInTheDocument();
    expect(
      screen.queryByLabelText("Hosts remembered per workspace"),
    ).toBeNull();
  });

  it("says the never-decrypt list can't be read, and shows no editor", async () => {
    api.refuse.set("GET /api/interception/never-decrypt", {
      status: 500,
      error: "internal",
      message: "store broken",
    });
    await open();
    expect(await screen.findByText("Store broken.")).toBeInTheDocument();
    expect(
      screen.queryByRole("textbox", {
        name: "Never decrypt, in any workspace",
      }),
    ).toBeNull();
  });

  it("turns the global direct SSH default on only after the trust text, then saves it at once", async () => {
    await open();
    const box = screen.getByLabelText("Allow direct SSH by default");
    expect(box).not.toBeChecked();
    await fireEvent.click(box);
    const dialog = await screen.findByRole("alertdialog", {
      name: "Allow direct SSH by default?",
    });
    expect(dialog).toHaveTextContent("signed-in GitHub token");
    expect(putBodies()).toHaveLength(0);
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Allow by default" }),
    );
    await save();
    expect(api.layer.direct_ssh).toBe(true);
    expect(screen.getByLabelText("Allow direct SSH by default")).toBeChecked();
    await fireEvent.click(screen.getByLabelText("Allow direct SSH by default"));
    await save();
    expect(api.layer.direct_ssh).toBe(false);
  });

  it("keeps logins by default and turns the default off and back on", async () => {
    await open();
    const box = screen.getByLabelText("Keep logins made in workspaces");
    expect(box).toBeChecked();
    await fireEvent.click(box);
    await save();
    expect(api.layer.capture_logins).toBe(false);
    expect(
      screen.getByLabelText("Keep logins made in workspaces"),
    ).not.toBeChecked();
    await fireEvent.click(
      screen.getByLabelText("Keep logins made in workspaces"),
    );
    await save();
    expect(api.layer.capture_logins).toBe(true);
  });

  it("warns about a credential Puddle did not add by default and turns the default off and back on", async () => {
    await open();
    const box = screen.getByLabelText(
      "Warn about a credential Puddle did not add",
    );
    expect(box).toBeChecked();
    await fireEvent.click(box);
    await save();
    expect(api.layer.warn_foreign_credentials).toBe(false);
    expect(
      screen.getByLabelText("Warn about a credential Puddle did not add"),
    ).not.toBeChecked();
    await fireEvent.click(
      screen.getByLabelText("Warn about a credential Puddle did not add"),
    );
    await save();
    expect(api.layer.warn_foreign_credentials).toBe(true);
  });

  it("cancelling the global trust text changes nothing", async () => {
    await open();
    await fireEvent.click(screen.getByLabelText("Allow direct SSH by default"));
    await fireEvent.click(
      await screen.findByRole("button", { name: "Cancel" }),
    );
    expect(putBodies()).toHaveLength(0);
    expect(api.layer.direct_ssh).toBeNull();
  });

  it("checks the reconnection grace before saving it", async () => {
    await open();
    const input = screen.getByLabelText("Reconnection grace (seconds)");
    await fireEvent.input(input, { target: { value: "5" } });
    expect(await screen.findByText(/Use between 30 and 86400/)).toBeVisible();
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(putBodies()).toHaveLength(0);
    expect(
      screen.getByRole("button", { name: "Save Reconnection grace (seconds)" }),
    ).toBeDisabled();
    await fireEvent.input(input, { target: { value: "600" } });
    await save();
    expect(api.layer.reconnection_grace).toBe(600);
    expect(screen.queryByText(/Use between/)).toBeNull();
  });
});
