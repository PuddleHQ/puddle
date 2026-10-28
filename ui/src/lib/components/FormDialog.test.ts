// SPDX-License-Identifier: GPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { createRawSnippet } from "svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import FormDialog from "./FormDialog.svelte";

afterEach(cleanup);

const children = createRawSnippet(() => ({
  render: () => `<form><input aria-label="Name" /></form>`,
}));

describe("the form dialog", () => {
  it("tells its owner when Escape closes it", async () => {
    const onClose = vi.fn();
    render(FormDialog, {
      props: { open: true, title: "Rename", children, onClose },
    } as never);
    await fireEvent.keyDown(await screen.findByRole("dialog"), {
      key: "Escape",
    });
    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("closes without an owner listening", async () => {
    render(FormDialog, {
      props: { open: true, title: "Rename", children },
    } as never);
    await fireEvent.keyDown(await screen.findByRole("dialog"), {
      key: "Escape",
    });
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });
});
