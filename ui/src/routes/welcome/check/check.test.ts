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

const nav = vi.hoisted(() => ({ search: "" }));
vi.mock("$app/state", () => ({
  page: {
    get url() {
      return new URL(`http://127.0.0.1/welcome/check${nav.search}`);
    },
  },
}));

const api = await vi.hoisted(async () => {
  const mod = await import("#lib/testing/fake-first-run.ts");
  return new mod.FakeFirstRun();
});
vi.mock("#lib/api/client.ts", () => ({ api }));

import { brokenReport, healthyReport } from "#lib/testing/fake-first-run.ts";
import { doctor } from "#lib/stores/doctor.svelte.ts";
import Page from "./+page.svelte";

const writeText = vi.fn(async (_text: string) => undefined);

beforeEach(() => {
  nav.search = "";
  api.calls = [];
  api.report = healthyReport();
  api.doctorStatus = 200;
  api.down = false;
  api.hold = null;
  doctor.status = "idle";
  doctor.report = null;
  writeText.mockClear();
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText },
  });
});
afterEach(cleanup);

const runs = () => api.calls.filter((c) => c === "GET /api/doctor").length;
const link = (name: string) => screen.queryByRole("link", { name });

/** Opens a row's closed toggle by its label and finds `text` inside it. */
async function opens(row: HTMLElement, label: string, text: string | RegExp) {
  const summary = within(row).getByText(label, { selector: "summary" });
  const toggle = summary.closest("details")!;
  expect(toggle.open).toBe(false);
  await fireEvent.click(summary);
  expect(toggle.open).toBe(true);
  expect(within(toggle).getByText(text)).toBeInTheDocument();
}

/** Opens the step and presses its start button, as a person does. */
async function start() {
  render(Page);
  await fireEvent.click(
    screen.getByRole("button", { name: "Start system check" }),
  );
}

describe("system check step", () => {
  it("runs nothing until the person starts it, and says first that a window may flash", async () => {
    render(Page);
    const heading = screen.getByRole("heading", {
      level: 1,
      name: "System check",
    });
    expect(document.activeElement).toBe(heading);
    expect(screen.getByText(/window may flash open/)).toBeVisible();
    await Promise.resolve();
    expect(runs()).toBe(0);
    expect(screen.queryByRole("list", { name: "Checks" })).toBeNull();
    // Nothing is known yet, so the step does not go on.
    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Check again" })).toBeNull();
    await fireEvent.click(
      screen.getByRole("button", { name: "Start system check" }),
    );
    await screen.findByText("No problems found.");
    expect(runs()).toBe(1);
    expect(
      screen.queryByRole("button", { name: "Start system check" }),
    ).toBeNull();
  });

  it("says it is checking meanwhile, with the buttons off", async () => {
    let release = () => {};
    api.hold = new Promise<void>((resolve) => (release = resolve));
    await start();
    expect(screen.getByText(/Checking this computer/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Check again" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Continue" })).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Copy report" })).toBeNull();
    release();
    await screen.findByText("No problems found.");
    expect(runs()).toBe(1);
  });

  it("lists each check with its state in words, and goes on when nothing failed", async () => {
    api.report = healthyReport({ elapsed_ms: 2300 });
    await start();
    const list = await screen.findByRole("list", { name: "Checks" });
    const items = within(list).getAllByRole("listitem");
    expect(items).toHaveLength(3);
    expect(items[0]).toHaveTextContent("OK: CPU virtualization");
    expect(items[2]).toHaveTextContent("booted in 1.4 s");
    expect(screen.getByRole("status")).toHaveTextContent(
      "No problems found. Checked in 2.3 s.",
    );
    expect(link("Continue")).toHaveAttribute("href", "/welcome/certificates");
    expect(link("Back")).toHaveAttribute("href", "/welcome");
    expect(link("Leave setup for now")).toBeNull();
  });

  it("shows a warning's impact and remediation without blocking", async () => {
    const report = healthyReport();
    report.checks[0] = {
      ...report.checks[0]!,
      status: "warn",
      summary: "a client is installed",
      finding: "gsa_installed",
      impact: "Company sites may not work from a workspace.",
      remedy: "Ask your IT team for another way in.",
      fix: "The long story, with every command.",
    };
    api.report = report;
    await start();
    await screen.findByText(/No problems found; 1 warning to know about/);
    expect(screen.getByText("Warning:")).toBeInTheDocument();
    const row = screen.getAllByRole("listitem")[0]!;
    await opens(row, "Impact", "Company sites may not work from a workspace.");
    await opens(row, "Remediation", "Ask your IT team for another way in.");
    await opens(row, "More details", "The long story, with every command.");
    expect(link("Continue")).toBeInTheDocument();
  });

  it("explains every row behind toggles that start closed: impact and remediation for a problem, why it matters for a pass", async () => {
    api.report = brokenReport();
    await start();
    const list = await screen.findByRole("list", { name: "Checks" });
    const rows = within(list).getAllByRole("listitem");
    expect(rows).toHaveLength(3);
    for (const row of rows) {
      const toggles = row.querySelectorAll("details");
      expect(toggles.length).toBeGreaterThan(0);
      for (const toggle of toggles) expect(toggle.open).toBe(false);
      for (const text of within(row).queryAllByText(/./, {
        selector: "details > :not(summary)",
      })) {
        expect(text.textContent?.trim()).not.toBe("");
      }
    }
    // A pass says what it protects or makes possible, and has nothing to fix.
    await opens(
      rows[0]!,
      "Why it matters",
      "CPU virtualization lets Puddle run workspaces.",
    );
    expect(within(rows[0]!).queryByText("Remediation")).toBeNull();
    // A problem says what it costs and what to do.
    await opens(
      rows[1]!,
      "Impact",
      "Puddle can't start any workspace: KVM isn't available.",
    );
    await opens(rows[1]!, "Remediation", "Load KVM for your CPU.");
    expect(within(rows[1]!).queryByText("Why it matters")).toBeNull();
    // A check that did not run says what that leaves open.
    await opens(rows[2]!, "Impact", /can't tell yet/);
    await opens(rows[2]!, "Remediation", /check again/);
  });

  it("blocks on a failed check: shows the fix and the evidence, offers no Continue", async () => {
    api.report = brokenReport();
    await start();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "1 problem to fix before Puddle can run workspaces.",
    );
    const row = screen.getAllByRole("listitem")[1]!;
    await opens(
      row,
      "More details",
      "Turn on virtualization in your UEFI setup.",
    );
    expect(screen.getByText("Problem:")).toBeInTheDocument();
    expect(screen.getByText("Not checked:")).toBeInTheDocument();
    const details = screen.getByLabelText("Technical details of Hypervisor");
    expect(details).toHaveTextContent("No such file or directory");
    expect(details).toHaveAttribute("tabindex", "0");
    expect(link("Continue")).toBeNull();
    expect(screen.queryByRole("button", { name: "Continue" })).toBeNull();
    // Leaving does not mark the flow done: it comes back at the next start.
    expect(link("Leave setup for now")).toHaveAttribute("href", "/workspaces");
    expect(
      screen.getByText(/comes back the next time you open Puddle/),
    ).toBeVisible();
  });

  it("checks again and lets the user on once the problem is fixed", async () => {
    api.report = brokenReport();
    await start();
    await screen.findByText(/1 problem to fix/);
    api.report = healthyReport();
    await fireEvent.click(screen.getByRole("button", { name: "Check again" }));
    await screen.findByText("No problems found.");
    expect(runs()).toBe(2);
    expect(link("Continue")).toBeInTheDocument();
  });

  it("copies the report as JSON with its schema version, and says when copying is refused", async () => {
    api.report = brokenReport();
    await start();
    await screen.findByText(/1 problem to fix/);
    await fireEvent.click(screen.getByRole("button", { name: "Copy report" }));
    expect(await screen.findByText("Report copied.")).toBeInTheDocument();
    const copied = JSON.parse(String(writeText.mock.calls[0]?.[0])) as {
      schema_version: number;
      checks: { finding: string | null }[];
    };
    expect(copied.schema_version).toBe(1);
    expect(copied.checks[1]?.finding).toBe("kvm_missing");

    writeText.mockRejectedValueOnce(new Error("denied"));
    await fireEvent.click(screen.getByRole("button", { name: "Copy report" }));
    expect(await screen.findByText(/didn't let Puddle copy/)).toHaveAttribute(
      "role",
      "alert",
    );
    expect(screen.queryByText("Report copied.")).toBeNull();
    // A new run forgets the old copy message.
    await fireEvent.click(screen.getByRole("button", { name: "Check again" }));
    await waitFor(() =>
      expect(screen.queryByText(/didn't let Puddle copy/)).toBeNull(),
    );
  });

  it("says plainly when this Puddle has no system check, and lets the user go on", async () => {
    api.doctorStatus = 503;
    await start();
    expect(await screen.findByText(/can't run the system check/)).toBeVisible();
    expect(link("Continue")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Copy report" })).toBeNull();
  });

  it("says when the service doesn't answer, and lets the user go on and try again", async () => {
    api.down = true;
    await start();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /couldn't run the system check: Puddle's service isn't answering/,
    );
    expect(link("Continue")).toBeInTheDocument();
    api.down = false;
    await fireEvent.click(screen.getByRole("button", { name: "Check again" }));
    await screen.findByText("No problems found.");
  });

  it("gives Puddle's own reason when the check itself fails", async () => {
    api.doctorStatus = 500;
    await start();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Puddle couldn't run the system check: not here",
    );
    expect(link("Continue")).toBeInTheDocument();
  });

  it("keeps the result when the user comes back from a later step", async () => {
    await start();
    await screen.findByText("No problems found.");
    cleanup();
    render(Page);
    expect(screen.getByText("No problems found.")).toBeInTheDocument();
    expect(runs()).toBe(1);
  });

  it("opened from Settings it runs afresh and leads back to Settings, with no step Back", async () => {
    await start();
    await screen.findByText("No problems found.");
    cleanup();
    nav.search = "?from=settings";
    render(Page);
    await waitFor(() => expect(runs()).toBe(2));
    await screen.findByText("No problems found.");
    expect(link("Back to Settings")).toHaveAttribute(
      "href",
      "/settings/this-computer",
    );
    expect(link("Back")).toBeNull();
    expect(link("Continue")).toBeNull();
  });

  it("opened from Settings with a problem, offers no setup text", async () => {
    nav.search = "?from=settings";
    api.report = brokenReport();
    render(Page);
    await screen.findByText(/1 problem to fix/);
    expect(link("Back to Settings")).toBeInTheDocument();
    expect(link("Leave setup for now")).toBeNull();
    expect(screen.queryByText(/comes back the next time/)).toBeNull();
  });
});
