// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, screen, waitFor } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";

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

import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { palette } from "#lib/theme/palette.svelte.ts";
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

describe("the mise switch", () => {
  it("is on by default and a save sends it as the global tools switch", async () => {
    await open();
    const box = screen.getByLabelText(
      "Manage tools with mise in workspaces",
    ) as HTMLInputElement;
    expect(box.checked).toBe(true);
    await fireEvent.click(box);
    expect(box.checked).toBe(false);
    await save();
    expect(api.bodies.at(-1)).toMatchObject({ tools: { mise: false } });
    expect(api.tools.mise).toBe(false);
    expect(
      (
        screen.getByLabelText(
          "Manage tools with mise in workspaces",
        ) as HTMLInputElement
      ).checked,
    ).toBe(false);
  });

  it("is not shown at all in a build without the mise layer (v0.1)", async () => {
    api.miseAvailable = false;
    await open();
    expect(
      screen.queryByLabelText("Manage tools with mise in workspaces"),
    ).toBeNull();
    expect(screen.queryByText(/mise/i)).toBeNull();
    api.miseAvailable = true;
  });

  it("sends each flip on its own, so flipping back stores the first value again", async () => {
    await open();
    const box = screen.getByLabelText("Manage tools with mise in workspaces");
    await fireEvent.click(box);
    await save();
    expect(api.tools.mise).toBe(false);
    await fireEvent.click(box);
    await save();
    expect(api.tools.mise).toBe(true);
    expect(putBodies()).toHaveLength(2);
  });
});

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
      "Notifications",
      "Also notify when",
      "Inbox",
      "Startup and window",
      "Disk space",
      "Back up and restore",
      "Devices and sign-in",
      "Activity log",
      "Diagnostic log",
      "Checks and errors",
      "Privacy and about",
      "New workspaces",
      "Editor defaults",
      "Proxy",
      "Local destinations",
      "Decrypting traffic",
      "Logins and credentials",
      "Limits",
      "Environment",
      "Browser VS Code server",
      "Git and SSH",
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

  it("links to the network health details and the app errors", async () => {
    await open();
    expect(
      screen.getByRole("link", { name: /^Details\s*about network health$/ }),
    ).toHaveAttribute("href", "/settings/network-health");
    expect(
      screen.getByRole("link", { name: /^Details\s*about app errors$/ }),
    ).toHaveAttribute("href", "/settings/app-errors");
  });

  it("states that Puddle sends nothing", async () => {
    await open();
    expect(
      screen.getByText(/Puddle sends nothing about you or your use of it/),
    ).toBeInTheDocument();
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

  it("shows a loading line until the licence text arrives", async () => {
    await open();
    await fireEvent.click(screen.getByText("Third-party licences"));
    expect(screen.queryByText(/Loading/)).not.toBeNull();
    await screen.findByLabelText("Third-party licences", { selector: "pre" });
    expect(screen.queryByText(/Loading/)).toBeNull();
  });
});

/*EXPLICIT*/
describe("appearance and notifications", () => {
  it("shows the theme at once, stores it at once and keeps a hint for the first paint", async () => {
    await open();
    await choose("Theme", "dark");
    expect(document.documentElement.dataset["theme"]).toBe("dark");
    expect(localStorage.getItem("puddle.theme")).toBe("dark");
    await save();
    expect(api.ui.theme).toBe("dark");
  });

  it("goes back to the stored theme and density when the save is refused", async () => {
    await open();
    api.refuse.set("PUT /api/settings", {
      status: 500,
      error: "internal",
      message: "x",
    });
    await choose("Theme", "dark");
    expect(await screen.findByRole("alert")).toHaveTextContent("x");
    await waitFor(() =>
      expect(document.documentElement.dataset["theme"]).toBeUndefined(),
    );
    expect(screen.getByLabelText("Theme")).toHaveValue("system");
    api.refuse.set("PUT /api/settings", {
      status: 500,
      error: "internal",
      message: "y",
    });
    await choose("Density", "compact");
    await waitFor(() => expect(screen.getByText("y")).toBeVisible());
    expect(document.documentElement.dataset["density"]).toBeUndefined();
    expect(api.ui.density).toBeNull();
  });

  it("stores the density at once", async () => {
    await open();
    expect(screen.getByLabelText("Density")).toHaveValue("comfortable");
    await choose("Density", "compact");
    await save();
    expect(api.ui.density).toBe("compact");
    expect(localStorage.getItem("puddle.density")).toBe("compact");
    await choose("Density", "comfortable");
    await save();
    expect(api.ui.density).toBe("comfortable");
    expect(document.documentElement.dataset["density"]).toBeUndefined();
  });

  it("shows a palette at once and stores it at once", async () => {
    await open();
    expect(screen.getByLabelText("Colours")).toHaveValue("pond");
    await choose("Colours", "meadow");
    expect(document.documentElement.dataset["palette"]).toBe("meadow");
    await save();
    expect(api.ui.palette).toBe("meadow");
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
    // Dragging only previews; the pick is stored when it is settled.
    await save();
    expect(api.ui.custom_accent).toBeNull();
    await fireEvent.change(accent);
    await save();
    expect(api.ui.palette).toBe("custom");
    expect(api.ui.custom_accent).toBe("#7a1fa2");
    expect(api.ui.custom_text).toBeNull();
  });

  it("resets one custom colour to Pond's, and that is stored at once", async () => {
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

  it("keeps what only fails in the other theme apart from what is hard to read now", async () => {
    await open();
    await choose("Theme", "system");
    await choose("Colours", "custom");
    const accent = screen.getByLabelText("Accent colour") as HTMLInputElement;
    // Pond's light accent: fine on the light theme showing now, short on the dark one.
    accent.value = "#0a6a85";
    await fireEvent.input(accent);
    expect(screen.queryByText(/hard to read/)).toBeNull();
    const note = screen.getByText(/harder to read/).closest("[role=status]");
    expect(note).toHaveTextContent(/in the dark theme; it needs/);
    expect(note).toHaveTextContent(/and \d+ more\./);
    expect(note).not.toHaveTextContent(/in the light theme/);
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

  it("saves the log detail, from the default and from a level already chosen", async () => {
    await open();
    expect(api.ui.log_level).toBeNull();
    await choose("Log detail", "debug");
    await save();
    expect(api.ui.log_level).toBe("debug");
    await choose("Log detail", "trace");
    await save();
    expect(api.ui.log_level).toBe("trace");
  });
});

describe("notifications in this browser", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("offers this browser's own switch, apart from the saved settings", async () => {
    await open();
    const box = screen.getByLabelText("Notifications in this browser");
    expect(box).not.toBeChecked();
    await fireEvent.click(box);
    await waitFor(() => expect(screen.queryByText(/^Saving/)).toBeNull());
    expect(putBodies()).toHaveLength(0);
  });

  it("does not offer it in the desktop app, which raises its own notifications", async () => {
    vi.stubGlobal("__TAURI_INTERNALS__", {});
    await open();
    expect(screen.queryByLabelText("Notifications in this browser")).toBeNull();
    expect(
      screen.getByLabelText("System notifications for new requests"),
    ).toBeInTheDocument();
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
    ["A workspace's disk is nearly full", "notify_disk_full", true],
    ["A workspace stops on its own", "notify_unexpected_stop", true],
    [
      "The drive that holds Puddle's data runs low on space",
      "notify_low_disk_space",
      true,
    ],
    [
      "Confirm before a rule affects every workspace",
      "confirm_global_rules",
      true,
    ],
    ["Keyboard shortcuts in the inbox", "inbox_shortcuts", true],
    ["Start Puddle when I sign in", "start_at_login", false],
    ["Minimizing hides Puddle in the tray", "minimize_to_tray", false],
  ];

  it("shows each switch at Puddle's default and saves each change at once", async () => {
    await open();
    for (const [label, , on] of rows) {
      const box = screen.getByLabelText(label);
      if (on) expect(box).toBeChecked();
      else expect(box).not.toBeChecked();
      await fireEvent.click(box);
    }
    await save();
    for (const [, key, on] of rows) expect(api.ui[key]).toBe(!on);
  });

  it("checks and saves the disk warning percentage", async () => {
    await open();
    const input = screen.getByLabelText("Warn when a disk is this full (%)");
    expect(input).toHaveValue("95");
    const saveButton = () =>
      screen.getByRole("button", {
        name: "Save Warn when a disk is this full (%)",
      });
    for (const bad of ["49", "101", "lots", ""]) {
      await fireEvent.input(input, { target: { value: bad } });
      expect(saveButton()).toBeDisabled();
    }
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByText("Enter a whole number of percent.")).toBeVisible();
    await fireEvent.input(input, { target: { value: "90 %" } });
    await save();
    expect(api.ui.disk_warning_percent).toBe(90);
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
