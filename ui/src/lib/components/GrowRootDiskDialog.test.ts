// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { workspace } from "#lib/testing/fake-workspaces.ts";
import GrowRootDiskDialog from "./GrowRootDiskDialog.svelte";

afterEach(cleanup);

function mount(
  onSubmit: (gib: number) => Promise<string | null>,
  over: Record<string, unknown> = {},
) {
  render(GrowRootDiskDialog, {
    props: {
      open: true,
      workspace: workspace("demo", {
        status: "running",
        root_disk_size_mib: 10_240,
      }),
      onSubmit,
      ...over,
    },
  } as never);
}

const field = () => screen.findByRole("textbox", { name: /New size in GiB/ });

describe("the grow root disk dialog", () => {
  it("starts at ten GiB more, says what happens to a running workspace and sends the size", async () => {
    const onSubmit = vi.fn(async () => null);
    mount(onSubmit);
    const input = await field();
    expect(input).toHaveValue("20");
    expect(screen.getByRole("dialog")).toHaveTextContent(
      "Puddle restarts it; nothing saved to disk is lost",
    );
    expect(screen.getByRole("dialog")).toHaveTextContent("now 10 GiB");
    await fireEvent.input(input, { target: { value: "32" } });
    await fireEvent.click(screen.getByRole("button", { name: "Make bigger" }));
    await vi.waitFor(() => expect(onSubmit).toHaveBeenCalledWith(32));
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("says a stopped workspace has the room at its next start", async () => {
    mount(async () => null, {
      workspace: workspace("demo", {
        status: "stopped",
        root_disk_size_mib: 10_240,
      }),
    });
    expect(await screen.findByRole("dialog")).toHaveTextContent(
      "has the room when it starts next",
    );
  });

  it("refuses a size that is not bigger, with the reason, and asks nothing", async () => {
    const onSubmit = vi.fn(async () => null);
    mount(onSubmit);
    const input = await field();
    await fireEvent.input(input, { target: { value: "8" } });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "A root disk only grows: choose more than 10 GiB.",
    );
    expect(input).toHaveAttribute("aria-invalid", "true");
    await fireEvent.click(screen.getByRole("button", { name: "Make bigger" }));
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("shows the service's refusal, keeps the dialog open and clears it on the next edit", async () => {
    const onSubmit = vi.fn(async () => "this computer has 3 GiB free");
    mount(onSubmit);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Make bigger" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "this computer has 3 GiB free",
    );
    expect(screen.getByRole("dialog")).toBeInTheDocument();
    await fireEvent.input(await field(), { target: { value: "21" } });
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("closes on Cancel without asking", async () => {
    const onSubmit = vi.fn(async () => null);
    mount(onSubmit);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Cancel" }),
    );
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
    expect(onSubmit).not.toHaveBeenCalled();
  });
});
