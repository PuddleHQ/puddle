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

const api = await vi.hoisted(async () => {
  const mod = await import("#lib/testing/fake-settings.ts");
  return new mod.FakeSettings();
});
vi.mock("#lib/api/client.ts", () => ({ api }));
const nav = vi.hoisted(() => ({
  hooks: [] as ((n: { cancel: () => void; willUnload: boolean }) => void)[],
}));
vi.mock("$app/navigation", () => ({
  beforeNavigate: (hook: (typeof nav.hooks)[number]) => nav.hooks.push(hook),
}));

import { microsoftChosenToast, MS_TERMS_VERSION } from "#lib/settings/model.ts";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import { density } from "#lib/theme/density.svelte.ts";
import { palette } from "#lib/theme/palette.svelte.ts";
import { NO_CUSTOM } from "#lib/theme/palette.ts";
import { theme } from "#lib/theme/theme.svelte.ts";
import Page from "./+page.svelte";

beforeEach(() => {
  nav.hooks = [];
  api.reset();
  globalSettings.view = null;
  globalSettings.consents = null;
  globalSettings.status = "loading";
  globalSettings.reason = null;
  toasts.items = [];
  localStorage.clear();
  delete document.documentElement.dataset["theme"];
  theme.choice = "system";
  delete document.documentElement.dataset["density"];
  density.choice = "comfortable";
  delete document.documentElement.dataset["palette"];
  document.documentElement.removeAttribute("style");
  palette.choice = "pond";
  palette.custom = { ...NO_CUSTOM };
});
afterEach(cleanup);

async function open() {
  render(Page);
  await screen.findByRole("heading", { level: 2, name: "Appearance" });
}

// The licence test is the first to open "Third-party licences", which needs jsdom's queued `toggle`
// event, vitest's first load of the notices file and the first render of its text. Under load
// those cold steps outlasted that test's one-second wait, so open it once here, before the tests.
// The waits are long on purpose: nothing is asserted about speed, and a slow warm-up must not
// fail the whole file at collection.
{
  const slow = { timeout: 30_000 };
  render(Page);
  await screen.findByRole("heading", { level: 2, name: "About" }, slow);
  await fireEvent.click(screen.getByText("Third-party licences"));
  await screen.findByLabelText(
    "Third-party licences",
    { selector: "pre" },
    slow,
  );
  cleanup();
  api.reset();
}

async function choose(label: string | RegExp, value: string) {
  const select = screen.getByLabelText(label) as HTMLSelectElement;
  select.value = value;
  await fireEvent.change(select);
}

/** Presses Save and waits for the answer. */
async function save(says: string | RegExp = "Settings saved.") {
  await fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await screen.findByText(says);
}

const putBodies = () =>
  api.calls
    .map((c, i) => [c, api.bodies[i]] as const)
    .filter(([c]) => c === "PUT /api/settings");

describe("the screen", () => {
  it("offers to run the system check again, on its own page with a way back here", async () => {
    await open();
    expect(
      screen.getByRole("link", { name: "Run the system check again" }),
    ).toHaveAttribute("href", "/welcome/check?from=settings");
  });

  it("says when the version could not be read", async () => {
    await open();
    expect(screen.queryByText(/version unavailable/)).toBeNull();
    cleanup();
    api.refuse.set("GET /api/health", {
      status: 500,
      error: "internal",
      message: "broken",
    });
    await open();
    expect(await screen.findByText(/version unavailable/)).toBeInTheDocument();
  });

  it("has one h1, every section and the stored values", async () => {
    api.layer.memory = 4096;
    await open();
    expect(screen.getAllByRole("heading", { level: 1 })).toHaveLength(1);
    const names = screen
      .getAllByRole("heading", { level: 2 })
      .map((h) => h.textContent);
    expect(names).toEqual([
      "Appearance",
      "App icon",
      "Notifications",
      "Inbox",
      "Workspaces",
      "Network",
      "Browser VS Code",
      "Git and credentials",
      "Environment",
      "Privacy",
      "About",
    ]);
    expect(screen.getByLabelText("Default memory")).toHaveValue("4096");
    expect(screen.getByLabelText("Theme")).toHaveValue("system");
    expect(
      screen.getByLabelText("System notifications for new requests"),
    ).toBeChecked();
    expect(screen.getByLabelText("Sound")).not.toBeChecked();
    expect(screen.getByLabelText("Zoom shortcuts")).toBeChecked();
    expect(screen.getByLabelText("Server")).toHaveValue("code_server");
    expect(screen.getByText(/Open VSX/)).toBeInTheDocument();
  });

  it("links to the network health details", async () => {
    await open();
    expect(screen.getByRole("link", { name: /Details/ })).toHaveAttribute(
      "href",
      "/settings/network-health",
    );
  });

  it("states that Puddle sends nothing", async () => {
    await open();
    expect(
      screen.getByText(/Puddle sends nothing about you or your use of it/),
    ).toBeInTheDocument();
  });

  it("says when the settings can't be read, and when a newer Puddle wrote them", async () => {
    api.down = true;
    render(Page);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Couldn't read Puddle's settings.",
    );
    cleanup();
    api.down = false;
    globalSettings.status = "loading";
    api.refuse.set("GET /api/settings", {
      status: 409,
      error: "newer_settings",
      message: "newer",
    });
    render(Page);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /newer Puddle\. Update Puddle/,
    );
  });

  it("shows why the settings can't be read when the API says", async () => {
    api.refuse.set("GET /api/settings", {
      status: 409,
      error: "settings_unreadable",
      message:
        "Puddle cannot read the global settings: global settings must be an object",
    });
    render(Page);
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("Couldn't read Puddle's settings.");
    expect(alert).toHaveTextContent("global settings must be an object");
  });

  it("lists the stored fields it doesn't know, and nothing when there are none", async () => {
    await open();
    expect(screen.queryByText(/fields this Puddle doesn't know/)).toBeNull();
    cleanup();
    globalSettings.view = null;
    globalSettings.status = "loading";
    api.unknown = ["workspace_defaults.memry", "color"];
    await open();
    const note = screen.getByText(/fields this Puddle doesn't know/);
    expect(note).toHaveTextContent("workspace_defaults.memry, color");
    expect(note).toHaveTextContent("kept but have no effect");
  });

  it("shows Puddle's version and loads the licences when asked", async () => {
    api.version = "9.8.7";
    await open();
    expect(await screen.findByText("Puddle 9.8.7")).toBeInTheDocument();
    await fireEvent.click(screen.getByText("Third-party licences"));
    const text = await screen.findByLabelText("Third-party licences", {
      selector: "pre",
    });
    expect(text.textContent).toMatch(/Third-party software shipped/);
  });
});

describe("explicit Save", () => {
  it("starts clean: Save and Discard are off and nothing is sent until Save", async () => {
    await open();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Discard" })).toBeDisabled();
    expect(screen.getByText("No unsaved changes.")).toBeInTheDocument();
    await fireEvent.click(screen.getByLabelText("Sound"));
    await choose("Default memory", "16384");
    expect(screen.getByText(/You have unsaved changes/)).toBeInTheDocument();
    expect(putBodies()).toHaveLength(0);
    expect(api.ui.sound).toBeNull();
  });

  it("sends every edit as one change", async () => {
    await open();
    await fireEvent.click(screen.getByLabelText("Sound"));
    await fireEvent.click(screen.getByLabelText("Private networks"));
    await choose("Closing the window", "quit");
    await save();
    expect(putBodies()).toHaveLength(1);
    expect(api.ui.sound).toBe(true);
    expect(api.ui.close_behaviour).toBe("quit");
    expect(api.layer.local_toggles.private).toBe(true);
    expect(api.layer.local_toggles.loopback).toBeNull();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    // With nothing left in the draft, the controls show what was stored.
    expect(screen.getByLabelText("Sound")).toBeChecked();
    expect(screen.getByLabelText("Private networks")).toBeChecked();
    expect(screen.getByLabelText("Closing the window")).toHaveValue("quit");
  });

  it("names the saved settings that apply at the next start", async () => {
    await open();
    expect(
      screen.getAllByText("Applies when a workspace next starts."),
    ).toHaveLength(3);
    await choose("Default memory", "16384");
    expect(
      screen.getByText(/Default memory apply when a workspace next starts/),
    ).toBeInTheDocument();
    await fireEvent.click(screen.getByLabelText("Sound"));
    await save(
      "Settings saved. Default memory apply when a workspace next starts.",
    );
    expect(api.layer.memory).toBe(16_384);
  });

  it("sets the default CPU count from 1 to 32 and says a running workspace keeps its own", async () => {
    await open();
    const cpus = screen.getByLabelText("Default CPUs") as HTMLSelectElement;
    expect(cpus).toHaveValue("1");
    expect([...cpus.options].map((o) => o.value)).toEqual(
      Array.from({ length: 32 }, (_, i) => String(i + 1)),
    );
    expect(cpus.options[0]?.text).toBe("1 CPU");
    expect(cpus.options[3]?.text).toBe("4 CPUs");
    expect(
      screen.getByText(/A running workspace keeps the count it started with/),
    ).toBeInTheDocument();
    await choose("Default CPUs", "4");
    expect(
      screen.getByText(/Default CPUs apply when a workspace next starts/),
    ).toBeInTheDocument();
    await save(
      "Settings saved. Default CPUs apply when a workspace next starts.",
    );
    expect(api.layer.cpus).toBe(4);
    expect(screen.getByLabelText("Default CPUs")).toHaveValue("4");
  });

  it("says nothing about a restart for settings that apply at once", async () => {
    await open();
    await fireEvent.click(screen.getByLabelText("Sound"));
    await save("Settings saved.");
    expect(screen.queryByText(/next starts\.$/, { selector: "p" })).toBeNull();
  });

  it("drops an edit that puts a value back to what is stored", async () => {
    await open();
    await fireEvent.click(screen.getByLabelText("Sound"));
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    await fireEvent.click(screen.getByLabelText("Sound"));
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
  });

  it("Discard puts every control back to what is stored", async () => {
    await open();
    await fireEvent.click(screen.getByLabelText("Sound"));
    await choose("Default memory", "16384");
    await fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(screen.getByLabelText("Sound")).not.toBeChecked();
    expect(screen.getByLabelText("Default memory")).toHaveValue("8192");
    expect(screen.getByText("No unsaved changes.")).toBeInTheDocument();
    expect(putBodies()).toHaveLength(0);
  });

  it("keeps the edits and says why when the save is refused", async () => {
    await open();
    api.refuse.set("PUT /api/settings", {
      status: 422,
      error: "invalid",
      message: "no thanks",
    });
    await fireEvent.click(screen.getByLabelText("Sound"));
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("no thanks");
    expect(screen.getByLabelText("Sound")).toBeChecked();
    expect(screen.getByRole("button", { name: "Save" })).toBeEnabled();
    expect(api.ui.sound).toBeNull();
  });

  it("blocks leaving with unsaved edits, and lets go once they are saved", async () => {
    await open();
    const leave = () => {
      let cancelled = false;
      for (const hook of nav.hooks)
        hook({ cancel: () => (cancelled = true), willUnload: false });
      return cancelled;
    };
    expect(leave()).toBe(false);
    await fireEvent.click(screen.getByLabelText("Sound"));
    expect(leave()).toBe(true);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Save or discard them first",
    );
    for (const hook of nav.hooks) {
      let cancelled = false;
      hook({ cancel: () => (cancelled = true), willUnload: true });
      expect(cancelled).toBe(false);
    }
    await save();
    expect(leave()).toBe(false);
  });
});

describe("appearance and notifications", () => {
  it("previews the theme at once and stores it, with a hint for the first paint, on Save", async () => {
    await open();
    await choose("Theme", "dark");
    expect(document.documentElement.dataset["theme"]).toBe("dark");
    expect(localStorage.getItem("puddle.theme")).toBe("dark");
    expect(api.ui.theme).toBeNull();
    await save();
    expect(api.ui.theme).toBe("dark");
  });

  it("goes back to the stored theme and density on Discard", async () => {
    await open();
    await choose("Theme", "dark");
    await choose("Density", "compact");
    expect(document.documentElement.dataset["density"]).toBe("compact");
    await fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(document.documentElement.dataset["theme"]).toBeUndefined();
    expect(document.documentElement.dataset["density"]).toBeUndefined();
    expect(screen.getByLabelText("Theme")).toHaveValue("system");
  });

  it("ends an unsaved preview when the screen is left", async () => {
    await open();
    await choose("Theme", "dark");
    cleanup();
    expect(document.documentElement.dataset["theme"]).toBeUndefined();
  });

  it("keeps the previewed theme when the save is refused", async () => {
    await open();
    api.refuse.set("PUT /api/settings", {
      status: 500,
      error: "internal",
      message: "x",
    });
    await choose("Theme", "light");
    await fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("x");
    expect(document.documentElement.dataset["theme"]).toBe("light");
  });

  it("stores the density on Save", async () => {
    await open();
    expect(screen.getByLabelText("Density")).toHaveValue("comfortable");
    await choose("Density", "compact");
    expect(api.ui.density).toBeNull();
    await save();
    expect(api.ui.density).toBe("compact");
    expect(localStorage.getItem("puddle.density")).toBe("compact");
    await choose("Density", "comfortable");
    await save();
    expect(api.ui.density).toBe("comfortable");
    expect(document.documentElement.dataset["density"]).toBeUndefined();
  });

  it("previews a palette at once and stores it on Save, and Discard puts Pond back", async () => {
    await open();
    expect(screen.getByLabelText("Colours")).toHaveValue("pond");
    await choose("Colours", "meadow");
    expect(document.documentElement.dataset["palette"]).toBe("meadow");
    expect(api.ui.palette).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: "Discard" }));
    expect(document.documentElement.dataset["palette"]).toBeUndefined();
    expect(screen.getByLabelText("Colours")).toHaveValue("pond");
    await choose("Colours", "ember");
    await save();
    expect(api.ui.palette).toBe("ember");
    expect(localStorage.getItem("puddle.palette")).toContain("ember");
  });

  it("shows the stored palette and colours when the screen opens", async () => {
    api.ui.palette = "custom";
    api.ui.custom_accent = "#7a1fa2";
    await palette.sync(); // the root layout does this once per launch
    await open();
    await waitFor(() =>
      expect(screen.getByLabelText("Accent colour")).toHaveValue("#7a1fa2"),
    );
    expect(screen.getByLabelText("Colours")).toHaveValue("custom");
  });

  it("lets the user pick colours for Custom, previews them and stores them", async () => {
    await open();
    expect(screen.queryByLabelText("Accent colour")).toBeNull();
    await choose("Colours", "custom");
    const accent = screen.getByLabelText("Accent colour") as HTMLInputElement;
    accent.value = "#7a1fa2";
    await fireEvent.input(accent);
    expect(
      document.documentElement.style.getPropertyValue("--color-accent"),
    ).toContain("#7a1fa2");
    await save();
    expect(api.ui.palette).toBe("custom");
    expect(api.ui.custom_accent).toBe("#7a1fa2");
    expect(api.ui.custom_text).toBeNull();
  });

  it("resets one custom colour to Pond's, and that is an edit until saved", async () => {
    api.ui.palette = "custom";
    api.ui.custom_accent = "#7a1fa2";
    api.ui.custom_text = "#102030";
    await palette.sync();
    await open();
    const reset = await screen.findByRole("button", {
      name: "Reset accent colour",
    });
    await waitFor(() => expect(reset).toBeEnabled());
    expect(
      screen.getByRole("button", { name: "Reset background" }),
    ).toBeDisabled();
    await fireEvent.click(reset);
    expect(reset).toBeDisabled();
    await save();
    expect(api.ui.custom_accent).toBeNull();
    expect(api.ui.custom_text).toBe("#102030");
  });

  it("warns, naming the pair, when picked colours miss AA, and not for ones that pass", async () => {
    await open();
    await choose("Theme", "light");
    await choose("Colours", "custom");
    expect(screen.queryByText(/hard to read/)).toBeNull();
    const text = screen.getByLabelText("Text") as HTMLInputElement;
    text.value = "#cccccc";
    await fireEvent.input(text);
    const warning = screen.getByText(/hard to read/).closest("[role=status]");
    expect(warning).toHaveTextContent(
      /Text on the background has \d\.\d:1 in the light theme; it needs 4\.5:1\./,
    );
    expect(warning).toHaveTextContent(/and \d+ more\./);
    text.value = "#101010";
    await fireEvent.input(text);
    expect(screen.queryByText(/hard to read/)).toBeNull();
  });

  it("lists a short warning in full, for a pale accent", async () => {
    await open();
    await choose("Theme", "light");
    await choose("Colours", "custom");
    const accent = screen.getByLabelText("Accent colour") as HTMLInputElement;
    accent.value = "#0a8aa5";
    await fireEvent.input(accent);
    const warning = screen.getByText(/hard to read/).closest("[role=status]");
    expect(warning).toHaveTextContent(
      /The accent on the background has \d\.\d:1/,
    );
    expect(warning).not.toHaveTextContent(/more\./);
  });

  it("saves each notification setting", async () => {
    await open();
    await fireEvent.click(
      screen.getByLabelText("System notifications for new requests"),
    );
    await fireEvent.click(screen.getByLabelText("Sound"));
    await choose("Closing the window", "quit");
    await save();
    expect(api.ui.notifications).toBe(false);
    expect(api.ui.sound).toBe(true);
    expect(api.ui.close_behaviour).toBe("quit");
  });
});

describe("notification and inbox switches", () => {
  const rows: [string, keyof typeof api.ui, boolean][] = [
    [
      "Only when Puddle is not the window in front",
      "notify_only_unfocused",
      true,
    ],
    ["Show the number of waiting requests", "badge", true],
    ["A workspace runs out of memory", "notify_oom", true],
    ["A workspace stops on its own", "notify_unexpected_stop", true],
    [
      "Confirm before a rule affects every workspace",
      "confirm_global_rules",
      true,
    ],
    ["Keyboard shortcuts in the inbox", "inbox_shortcuts", false],
    ["Minimizing hides Puddle in the tray", "minimize_to_tray", false],
  ];

  it("shows each switch at Puddle's default and saves a change on Save", async () => {
    await open();
    for (const [label, , on] of rows) {
      const box = screen.getByLabelText(label);
      if (on) expect(box).toBeChecked();
      else expect(box).not.toBeChecked();
      await fireEvent.click(box);
    }
    expect(putBodies()).toHaveLength(0);
    await save();
    for (const [, key, on] of rows) expect(api.ui[key]).toBe(!on);
  });

  it("shows what is stored", async () => {
    api.ui.badge = false;
    api.ui.confirm_global_rules = false;
    await open();
    expect(
      screen.getByLabelText("Show the number of waiting requests"),
    ).not.toBeChecked();
    expect(
      screen.getByLabelText("Confirm before a rule affects every workspace"),
    ).not.toBeChecked();
  });

  it("switches off the focus choice while system notifications are off", async () => {
    await open();
    const only = screen.getByLabelText(
      "Only when Puddle is not the window in front",
    );
    expect(only).toBeEnabled();
    await fireEvent.click(
      screen.getByLabelText("System notifications for new requests"),
    );
    expect(only).toBeDisabled();
  });
});

describe("workspace defaults and the network", () => {
  it("changes the default memory", async () => {
    await open();
    await choose("Default memory", "16384");
    await save(/Default memory apply when a workspace next starts/);
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
        name: "Keep names",
      }),
    );
    await screen.findByText("Saved.");
    expect(api.neverDecrypt).toEqual(["*.corp.test", "bank.example"]);
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

  it("turns the global direct SSH default on only after the trust text, then saves it with the rest", async () => {
    await open();
    const box = screen.getByLabelText("Allow direct SSH for new workspaces");
    expect(box).not.toBeChecked();
    await fireEvent.click(box);
    const dialog = await screen.findByRole("alertdialog", {
      name: "Allow direct SSH for new workspaces?",
    });
    expect(dialog).toHaveTextContent("signed-in GitHub token");
    expect(putBodies()).toHaveLength(0);
    await fireEvent.click(
      within(dialog).getByRole("button", { name: "Allow for new workspaces" }),
    );
    expect(api.layer.direct_ssh).toBeNull();
    expect(
      screen.getByLabelText("Allow direct SSH for new workspaces"),
    ).toBeChecked();
    await save();
    expect(api.layer.direct_ssh).toBe(true);
    await fireEvent.click(
      screen.getByLabelText("Allow direct SSH for new workspaces"),
    );
    await save();
    expect(api.layer.direct_ssh).toBe(false);
  });

  it("keeps logins by default and turns the default off and back on", async () => {
    await open();
    const box = screen.getByLabelText("Keep logins made in workspaces");
    expect(box).toBeChecked();
    await fireEvent.click(box);
    await save(/Login capture apply when a workspace next starts/);
    expect(api.layer.capture_logins).toBe(false);
    expect(
      screen.getByLabelText("Keep logins made in workspaces"),
    ).not.toBeChecked();
    await fireEvent.click(
      screen.getByLabelText("Keep logins made in workspaces"),
    );
    await save(/Login capture apply/);
    expect(api.layer.capture_logins).toBe(true);
  });

  it("cancelling the global trust text changes nothing", async () => {
    await open();
    await fireEvent.click(
      screen.getByLabelText("Allow direct SSH for new workspaces"),
    );
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
    await fireEvent.change(input);
    expect(await screen.findByText(/Use between 30 and 86400/)).toBeVisible();
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(putBodies()).toHaveLength(0);
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    // With another edit waiting, the bar says why Save is off.
    await fireEvent.click(screen.getByLabelText("Sound"));
    expect(
      screen.getByText("Fix the reconnection grace to save."),
    ).toBeVisible();
    expect(screen.getByRole("button", { name: "Save" })).toBeDisabled();
    await fireEvent.input(input, { target: { value: "600" } });
    await fireEvent.change(input);
    await save();
    expect(api.layer.reconnection_grace).toBe(600);
    expect(screen.queryByText(/Use between/)).toBeNull();
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
    expect(await screen.findByText(/service isn't answering/)).toBeVisible();
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
