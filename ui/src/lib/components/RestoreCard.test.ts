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

const svc = vi.hoisted(() => ({
  preview: vi.fn(),
  apply: vi.fn(),
  result: vi.fn(),
  dismiss: vi.fn(),
  copies: vi.fn(),
  remove: vi.fn(),
}));
vi.mock("#lib/settings/restore.ts", async () => {
  const real = await vi.importActual<typeof import("#lib/settings/restore.ts")>(
    "#lib/settings/restore.ts",
  );
  return {
    ...real,
    previewRestore: svc.preview,
    applyRestore: svc.apply,
    loadResult: svc.result,
    dismissResult: svc.dismiss,
    loadSafetyCopies: svc.copies,
    deleteSafetyCopy: svc.remove,
  };
});

import RestoreCard from "./RestoreCard.svelte";

const preview = {
  staging_id: "ab".repeat(16),
  format: 1,
  puddle_version: "0.1.0",
  schema_version: 32,
  created_at_ms: 1_700_000_000_000,
  parts: [
    { name: "rules", in_backup: 5, add: 2, differ: 1, same: 2, only_here: 3 },
    { name: "users", in_backup: 2, add: 1, differ: 0, same: 1, only_here: 0 },
  ],
  workspaces: [
    { id: "ws-one", name: "Acme", disk: "present" },
    { id: "ws-two", name: "Gone", disk: "missing" },
  ],
  secrets_needing_value: [{ workspace_id: null, name: "TOKEN" }],
  identity_tokens_needing_value: [{ identity: "work", host: "github.com" }],
  running_workspaces: 2,
};

beforeEach(() => {
  svc.result.mockResolvedValue({ ok: true, value: null });
  svc.copies.mockResolvedValue({ ok: true, value: [] });
});
afterEach(() => {
  cleanup();
  for (const mock of Object.values(svc)) mock.mockReset();
});

async function choose(file = new File(["x"], "b.tar.gz")) {
  render(RestoreCard);
  await fireEvent.change(screen.getByTestId("restore-file"), {
    target: { files: [file] },
  });
}

describe("RestoreCard", () => {
  it("changes nothing and shows no preview before a file is chosen", async () => {
    render(RestoreCard);
    await waitFor(() => expect(svc.copies).toHaveBeenCalled());
    expect(screen.queryByTestId("restore-preview")).toBeNull();
    expect(svc.preview).not.toHaveBeenCalled();
    expect(svc.apply).not.toHaveBeenCalled();
    expect(screen.getByTestId("safety-copies-empty")).toBeTruthy();
  });

  it("shows the reason a file is refused and offers no confirm", async () => {
    svc.preview.mockResolvedValue({
      ok: false,
      reason: "This is not a gzip archive.",
    });
    await choose();
    expect(
      (await screen.findByTestId("restore-problem")).textContent,
    ).toContain("This is not a gzip archive.");
    expect(screen.queryByTestId("restore-continue")).toBeNull();
  });

  it("previews the parts, the missing disks, the secrets and the running workspaces", async () => {
    svc.preview.mockResolvedValue({ ok: true, value: preview });
    await choose();
    expect(await screen.findByTestId("restore-preview")).toBeTruthy();
    expect(screen.getByTestId("restore-part-rules").textContent).toContain(
      "Rules",
    );
    expect(screen.getByTestId("restore-missing-disks").textContent).toContain(
      "Gone",
    );
    expect(
      screen.getByTestId("restore-missing-disks").textContent,
    ).not.toContain("Acme");
    expect(screen.getByTestId("restore-secrets").textContent).toContain(
      "TOKEN",
    );
    expect(screen.getByTestId("restore-identity-tokens").textContent).toContain(
      "work (github.com)",
    );
    expect(screen.getByTestId("restore-running").textContent).toContain(
      "2 workspaces",
    );
    // Add what is missing is the default.
    expect(
      (screen.getByLabelText(/Add what is missing/) as HTMLInputElement)
        .checked,
    ).toBe(true);
    expect(svc.apply).not.toHaveBeenCalled();
  });

  it("confirms twice: Continue opens a dialog that names the mode, and only its button applies", async () => {
    svc.preview.mockResolvedValue({ ok: true, value: preview });
    svc.apply.mockResolvedValue({
      ok: true,
      value: { safetyCopy: "20261115T225500Z", restartRequired: true },
    });
    await choose();
    await fireEvent.click(await screen.findByTestId("restore-continue"));
    expect(
      await screen.findByText("Add what is missing from the file."),
    ).toBeTruthy();
    expect(svc.apply).not.toHaveBeenCalled();
    await fireEvent.click(
      screen.getByRole("button", { name: "Add and restart" }),
    );
    await waitFor(() =>
      expect(svc.apply).toHaveBeenCalledWith(preview.staging_id, "add"),
    );
    expect(
      (await screen.findByTestId("restore-waiting")).textContent,
    ).toContain("2026-11-15 22:55 UTC");
    expect(screen.queryByTestId("restore-preview")).toBeNull();
  });

  it("Replace says what is dropped before it is confirmed", async () => {
    svc.preview.mockResolvedValue({ ok: true, value: preview });
    svc.apply.mockResolvedValue({
      ok: true,
      value: { safetyCopy: "20261115T225500Z", restartRequired: true },
    });
    await choose();
    await fireEvent.click(await screen.findByLabelText(/^Replace/));
    await fireEvent.click(screen.getByTestId("restore-continue"));
    expect(
      await screen.findByText(
        /1 entries here are replaced and 3 that are not in the file are dropped/,
      ),
    ).toBeTruthy();
    await fireEvent.click(
      screen.getByRole("button", { name: "Replace and restart" }),
    );
    await waitFor(() =>
      expect(svc.apply).toHaveBeenCalledWith(preview.staging_id, "replace"),
    );
  });

  it("keeps the dialog open with the reason when the restore did not start", async () => {
    svc.preview.mockResolvedValue({ ok: true, value: preview });
    svc.apply.mockResolvedValue({
      ok: false,
      reason:
        "The safety copy could not be made, so the restore did not start.",
    });
    await choose();
    await fireEvent.click(await screen.findByTestId("restore-continue"));
    await fireEvent.click(
      await screen.findByRole("button", { name: "Add and restart" }),
    );
    expect((await screen.findByRole("alert")).textContent).toContain(
      "did not start",
    );
    expect(screen.getByTestId("restore-preview")).toBeTruthy();
  });

  it("Cancel drops the preview without applying", async () => {
    svc.preview.mockResolvedValue({ ok: true, value: preview });
    await choose();
    await fireEvent.click(
      await screen.findByRole("button", { name: "Cancel" }),
    );
    expect(screen.queryByTestId("restore-preview")).toBeNull();
    expect(svc.apply).not.toHaveBeenCalled();
  });

  it("shows what the last restore did until it is dismissed", async () => {
    svc.result.mockResolvedValue({
      ok: true,
      value: {
        applied: false,
        mode: "add",
        safety_copy: "20261115T225500Z",
        at_ms: 1,
        message: "The restore failed and was undone.",
      },
    });
    svc.dismiss.mockResolvedValue({ ok: true, value: null });
    render(RestoreCard);
    const banner = await screen.findByTestId("restore-result");
    expect(banner.textContent).toContain("failed and was undone");
    expect(banner.getAttribute("role")).toBe("alert");
    await fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    await waitFor(() =>
      expect(screen.queryByTestId("restore-result")).toBeNull(),
    );
  });

  it("lists the safety copies with their sizes and deletes one only after a confirm", async () => {
    svc.copies.mockResolvedValue({
      ok: true,
      value: [{ name: "20261115T225500Z", size_bytes: 2048 }],
    });
    svc.remove.mockResolvedValue({ ok: true, value: null });
    render(RestoreCard);
    const row = await screen.findByTestId("safety-copy-20261115T225500Z");
    expect(row.textContent).toContain("2026-11-15 22:55 UTC");
    expect(row.textContent).toContain("2 KB");
    await fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    expect(svc.remove).not.toHaveBeenCalled();
    svc.copies.mockResolvedValue({ ok: true, value: [] });
    await fireEvent.click(
      await screen.findByRole("button", { name: "Delete the copy" }),
    );
    await waitFor(() =>
      expect(svc.remove).toHaveBeenCalledWith("20261115T225500Z"),
    );
    expect(await screen.findByTestId("safety-copies-empty")).toBeTruthy();
  });
});

describe("RestoreCard, the rest of its states", () => {
  it("says why the safety copies or the last result cannot be read", async () => {
    svc.result.mockResolvedValue({ ok: false, reason: "down" });
    svc.copies.mockResolvedValue({ ok: false, reason: "The list is down." });
    render(RestoreCard);
    expect((await screen.findByRole("alert")).textContent).toContain(
      "The list is down.",
    );
    expect(screen.queryByTestId("restore-result")).toBeNull();
  });

  it("does nothing when the file chooser closes without a file", async () => {
    render(RestoreCard);
    await fireEvent.change(screen.getByTestId("restore-file"), {
      target: { files: [] },
    });
    expect(svc.preview).not.toHaveBeenCalled();
  });

  it("the button opens the file chooser", async () => {
    render(RestoreCard);
    const click = vi.spyOn(
      screen.getByTestId("restore-file") as HTMLInputElement,
      "click",
    );
    await fireEvent.click(screen.getByTestId("restore-choose"));
    expect(click).toHaveBeenCalled();
  });

  it("a successful restore result is a status, and a failed dismiss keeps it", async () => {
    svc.result.mockResolvedValue({
      ok: true,
      value: {
        applied: true,
        mode: "add",
        safety_copy: "20261115T225500Z",
        at_ms: 1,
        message: "Restored.",
      },
    });
    svc.dismiss.mockResolvedValue({ ok: false, reason: "down" });
    render(RestoreCard);
    const banner = await screen.findByTestId("restore-result");
    expect(banner.getAttribute("role")).toBe("status");
    await fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    await waitFor(() => expect(svc.dismiss).toHaveBeenCalled());
    expect(screen.getByTestId("restore-result")).toBeTruthy();
  });

  it("a preview without a manifest, secrets or running workspaces says so quietly and singular", async () => {
    svc.preview.mockResolvedValue({
      ok: true,
      value: {
        ...preview,
        puddle_version: null,
        created_at_ms: null,
        secrets_needing_value: [],
        identity_tokens_needing_value: [],
        running_workspaces: 1,
        workspaces: [],
      },
    });
    await choose();
    expect(await screen.findByTestId("restore-preview")).toBeTruthy();
    expect(screen.getByText(/version not recorded/)).toBeTruthy();
    expect(screen.queryByTestId("restore-secrets")).toBeNull();
    expect(screen.queryByTestId("restore-identity-tokens")).toBeNull();
    expect(screen.queryByTestId("restore-missing-disks")).toBeNull();
    expect(screen.getByTestId("restore-running").textContent).toContain(
      "1 workspace running",
    );
  });

  it("cancelling the confirm dialog drops its error, and cancelling a delete keeps the copy", async () => {
    svc.preview.mockResolvedValue({ ok: true, value: preview });
    svc.apply.mockResolvedValue({ ok: false, reason: "no space" });
    svc.copies.mockResolvedValue({
      ok: true,
      value: [{ name: "20261115T225500Z", size_bytes: 1 }],
    });
    svc.remove.mockResolvedValue({ ok: false, reason: "cannot delete" });
    await choose();
    await fireEvent.click(await screen.findByTestId("restore-continue"));
    await fireEvent.click(
      await screen.findByRole("button", { name: "Add and restart" }),
    );
    expect((await screen.findAllByRole("alert"))[0]?.textContent).toContain(
      "no space",
    );
    await fireEvent.click(
      within(
        screen.getAllByRole("alertdialog").at(-1) as HTMLElement,
      ).getByRole("button", {
        name: "Cancel",
      }),
    );

    await fireEvent.click(
      await screen.findByRole("button", { name: "Delete" }),
    );
    await fireEvent.click(
      await screen.findByRole("button", { name: "Delete the copy" }),
    );
    expect((await screen.findAllByRole("alert"))[0]?.textContent).toContain(
      "cannot delete",
    );
    await fireEvent.click(
      within(
        screen.getAllByRole("alertdialog").at(-1) as HTMLElement,
      ).getByRole("button", {
        name: "Cancel",
      }),
    );
    expect(screen.getByTestId("safety-copy-20261115T225500Z")).toBeTruthy();
  });

  it("shows no running notice when nothing runs, and switching back to Add works", async () => {
    svc.preview.mockResolvedValue({
      ok: true,
      value: { ...preview, running_workspaces: 0 },
    });
    await choose();
    await screen.findByTestId("restore-preview");
    expect(screen.queryByTestId("restore-running")).toBeNull();
    await fireEvent.click(screen.getByLabelText(/^Replace/));
    await fireEvent.click(screen.getByLabelText(/Add what is missing/));
    expect(
      (screen.getByLabelText(/Add what is missing/) as HTMLInputElement)
        .checked,
    ).toBe(true);
  });
});
