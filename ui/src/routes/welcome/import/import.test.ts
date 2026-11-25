// SPDX-License-Identifier: AGPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

const svc = vi.hoisted(() => ({ preview: vi.fn(), apply: vi.fn() }));
vi.mock("#lib/settings/restore.ts", async () => {
  const real = await vi.importActual<typeof import("#lib/settings/restore.ts")>(
    "#lib/settings/restore.ts",
  );
  return { ...real, previewRestore: svc.preview, applyRestore: svc.apply };
});

import Import from "./+page.svelte";

const preview = {
  staging_id: "ab".repeat(16),
  format: 1,
  puddle_version: "0.1.0",
  schema_version: 32,
  created_at_ms: 1_700_000_000_000,
  parts: [
    { name: "rules", in_backup: 5, add: 5, differ: 0, same: 0, only_here: 0 },
  ],
  workspaces: [],
  secrets_needing_value: [{ workspace_id: null, name: "TOKEN" }],
  identity_tokens_needing_value: [],
  running_workspaces: 0,
};

const file = () => new File(["x"], "b.tar.gz", { type: "application/gzip" });
async function choose() {
  await fireEvent.change(screen.getByTestId("import-file"), {
    target: { files: [file()] },
  });
}

beforeEach(() => {
  svc.preview.mockReset();
  svc.apply.mockReset();
});
afterEach(cleanup);

describe("first-run import", () => {
  it("shows what the file would do, with no mode choice, and imports in the add mode", async () => {
    svc.preview.mockResolvedValue({ ok: true, value: preview });
    svc.apply.mockResolvedValue({
      ok: true,
      value: { safetyCopy: "20261115T225500Z", restartRequired: true },
    });
    render(Import);
    await choose();
    expect(await screen.findByTestId("import-preview")).toBeInTheDocument();
    expect(screen.getByTestId("restore-part-rules")).toBeInTheDocument();
    expect(screen.getByTestId("restore-secrets")).toHaveTextContent("TOKEN");
    expect(screen.queryByRole("radio")).toBeNull();
    await fireEvent.click(screen.getByTestId("import-confirm"));
    await waitFor(() =>
      expect(svc.apply).toHaveBeenCalledWith(preview.staging_id, "add"),
    );
    expect(await screen.findByTestId("import-waiting")).toHaveTextContent(
      "Setup continues after the restart",
    );
    expect(screen.queryByTestId("import-preview")).toBeNull();
  });

  it("says why a file is refused and offers no import", async () => {
    svc.preview.mockResolvedValue({ ok: false, reason: "not a gzip archive" });
    render(Import);
    await choose();
    expect(await screen.findByTestId("import-problem")).toHaveTextContent(
      "not a gzip archive",
    );
    expect(screen.queryByTestId("import-confirm")).toBeNull();
  });

  it("keeps the preview and says why when the import could not start", async () => {
    svc.preview.mockResolvedValue({ ok: true, value: preview });
    svc.apply.mockResolvedValue({ ok: false, reason: "disk is full" });
    render(Import);
    await choose();
    await fireEvent.click(await screen.findByTestId("import-confirm"));
    expect(await screen.findByTestId("import-error")).toHaveTextContent(
      "disk is full",
    );
    expect(screen.getByTestId("import-preview")).toBeInTheDocument();
    expect(screen.queryByTestId("import-waiting")).toBeNull();
  });

  it("opens the file picker from the choose button", async () => {
    render(Import);
    const click = vi.spyOn(screen.getByTestId("import-file"), "click");
    await fireEvent.click(screen.getByTestId("import-choose"));
    expect(click).toHaveBeenCalledTimes(1);
  });

  it("goes back to the file choice from the preview", async () => {
    svc.preview.mockResolvedValue({ ok: true, value: preview });
    render(Import);
    await choose();
    await fireEvent.click(
      await screen.findByRole("button", { name: "Choose another file" }),
    );
    expect(screen.queryByTestId("import-preview")).toBeNull();
    expect(screen.getByTestId("import-choose")).toBeInTheDocument();
  });
});
