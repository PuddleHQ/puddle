// SPDX-License-Identifier: AGPL-3.0-or-later
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
vi.mock("$app/navigation", () => ({ goto: vi.fn() }));
vi.mock("$app/state", () => ({
  page: {
    url: new URL("http://127.0.0.1/settings/appearance"),
    status: 200,
    error: null,
  },
}));

import { report } from "#lib/testing/fake-network.ts";
import SettingsHost from "#lib/testing/SettingsHost.svelte";
import { microsoftChosenToast, MS_TERMS_VERSION } from "#lib/settings/model.ts";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { identities } from "#lib/stores/identities.svelte.ts";
import { toasts } from "#lib/stores/toasts.svelte.ts";
import { density } from "#lib/theme/density.svelte.ts";
import { palette } from "#lib/theme/palette.svelte.ts";
import { NO_CUSTOM } from "#lib/theme/palette.ts";
import { theme } from "#lib/theme/theme.svelte.ts";
import Appearance from "./(pages)/appearance/+page.svelte";
import Credentials from "./(pages)/credentials/+page.svelte";
import Editor from "./(pages)/editor/+page.svelte";
import EditorServer from "./(pages)/editor-server/+page.svelte";
import Environment from "./(pages)/environment/+page.svelte";
import Git from "./(pages)/git/+page.svelte";
import Network from "./(pages)/network/+page.svelte";
import Notifications from "./(pages)/notifications/+page.svelte";
import ThisComputer from "./(pages)/this-computer/+page.svelte";
import Workspaces from "./(pages)/workspaces/+page.svelte";

// Every test here mounts all ten pages of Settings, so one test costs about a second on an idle
// host and several times that on a loaded one; the default 5 s limit is under the loaded cost.
vi.setConfig({ testTimeout: 30_000 });

const ALL = [
  Appearance,
  Notifications,
  ThisComputer,
  Workspaces,
  Editor,
  Network,
  Credentials,
  Environment,
  EditorServer,
  Git,
];

beforeEach(() => {
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

/** Every page of Settings at once, in the real frame, so a test reaches any setting by its label. */
function show() {
  return render(SettingsHost, { props: { pages: ALL } });
}

async function open() {
  show();
  await screen.findByRole("heading", { level: 2, name: "Appearance" });
}

// The licence test is the first to open "Third-party licences", which needs jsdom's queued `toggle`
// event, vitest's first load of the notices file and the first render of its text. Under load
// those cold steps outlasted that test's one-second wait, so open it once here, before the tests.
{
  const slow = { timeout: 30_000 };
  show();
  await screen.findByRole(
    "heading",
    { level: 2, name: "Privacy and about" },
    slow,
  );
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

const putBodies = () =>
  api.calls
    .map((c, i) => [c, api.bodies[i]] as const)
    .filter(([c]) => c === "PUT /api/settings");

/**
 * Lets every change in flight finish: presses the Save button of a typed value when one shows (the
 * Save button of a field, never a bar), then waits until no row says "Saving".
 */
async function save(says?: string | RegExp) {
  const typed = screen.queryAllByRole("button", { name: /^Save / });
  for (const button of typed) await fireEvent.click(button);
  await waitFor(() => expect(screen.queryByText(/^Saving/)).toBeNull());
  if (says) await screen.findByText(says);
}

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
      "Devices and sign-in",
      "Back up and restore",
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
