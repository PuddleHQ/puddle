// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { FakeSettings } from "#lib/testing/fake-settings.ts";
import { AuditRetentionStore } from "#lib/stores/audit-retention.svelte.ts";
import AuditRetention from "./AuditRetention.svelte";

let api: FakeSettings;
beforeEach(() => {
  api = new FakeSettings();
});
afterEach(cleanup);

function show() {
  render(AuditRetention, {
    store: new AuditRetentionStore(api as never),
  });
}

const box = () =>
  screen.findByLabelText(
    /Keep the activity log for/,
  ) as Promise<HTMLInputElement>;

describe("the activity log card", () => {
  it("shows the size, growth, oldest record, file and the days", async () => {
    show();
    const days = await box();
    expect(days.value).toBe("90");
    expect(screen.getByText("1,200 records, 469 KB")).toBeInTheDocument();
    expect(screen.getByText("40 records, 16 KB")).toBeInTheDocument();
    expect(
      screen.getByText("280 records, 109 KB (16 KB a day)"),
    ).toBeInTheDocument();
    expect(screen.getByText("4.0 MB, 64 KB of it free")).toBeInTheDocument();
    expect(screen.getByText(/2023-11-14 \(\d+ days ago\)/)).toBeInTheDocument();
    expect(screen.queryByText(/getting large/)).toBeNull();
  });

  it("warns when the log is over its warning size", async () => {
    api.auditSize = { ...api.auditSize, bytes: 3_000_000_000 };
    show();
    expect(await screen.findByRole("status")).toHaveTextContent(
      "The activity log is getting large: 2.8 GB",
    );
  });

  it("says there are no records yet on an empty log", async () => {
    api.auditSize = { ...api.auditSize, records: 0, bytes: 0, oldest_ts: null };
    show();
    expect(await screen.findByText("No records yet")).toBeInTheDocument();
  });

  it("saves the days and says so", async () => {
    show();
    const days = await box();
    await fireEvent.input(days, { target: { value: "30" } });
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Save how long the activity log is kept",
      }),
    );
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    expect(api.bodies).toEqual([{ days: 30 }]);
    expect(api.auditSize.retention_days).toBe(30);
  });

  it("shows the server's refusal and clears the saved note when typed over", async () => {
    show();
    const days = await box();
    api.refuse.set("PUT /api/audit/retention", {
      status: 422,
      error: "invalid",
      message: "the log is kept for 1 to 3650 days (0 given)",
    });
    await fireEvent.input(days, { target: { value: "0" } });
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Save how long the activity log is kept",
      }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The log is kept for 1 to 3650 days (0 given).",
    );
    expect(screen.queryByText("Saved.")).toBeNull();
  });

  it("does not send a box that is not a whole number", async () => {
    show();
    const days = await box();
    await fireEvent.input(days, { target: { value: "" } });
    await fireEvent.click(
      screen.getByRole("button", {
        name: "Save how long the activity log is kept",
      }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Type a whole number of days.",
    );
    expect(api.bodies).toEqual([]);
  });

  it("says it could not read the log, with the reason", async () => {
    api.down = true;
    show();
    expect(
      await screen.findByText("Couldn't read the size of the activity log."),
    ).toBeInTheDocument();
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(
        "Puddle's service isn't answering.",
      ),
    );
  });
});
