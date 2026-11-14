// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import InviteLinkDialog from "./InviteLinkDialog.svelte";

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const show = (origin = "https://puddle.example") =>
  render(InviteLinkDialog, {
    open: true,
    name: "Sam",
    secret: "abc123",
    expiresAt: 1_800_000_000_000,
    origin,
  } as never);

describe("the invite link dialog", () => {
  it("shows the link once, with the secret after the #", async () => {
    show();
    await screen.findByRole("dialog", { name: "Invite link for Sam" });
    expect(screen.getByLabelText("Invite link")).toHaveValue(
      "https://puddle.example/invite#abc123",
    );
    expect(screen.queryByText(/can't reach/)).toBeNull();
  });

  it("warns when the address is one the person cannot reach", async () => {
    show("http://127.0.0.1:4000");
    expect(await screen.findByText(/can't\s+reach/)).toBeInTheDocument();
  });

  it("copies the link", async () => {
    const writeText = vi.fn(async () => undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });
    show();
    await fireEvent.click(
      await screen.findByRole("button", { name: "Copy link" }),
    );
    expect(writeText).toHaveBeenCalledWith(
      "https://puddle.example/invite#abc123",
    );
    expect(await screen.findByText("Copied.")).toBeInTheDocument();
  });

  it("says so when the copy is refused", async () => {
    vi.stubGlobal("navigator", {
      clipboard: {
        writeText: vi.fn(async () => Promise.reject(new Error("no"))),
      },
    });
    show();
    await fireEvent.click(
      await screen.findByRole("button", { name: "Copy link" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Couldn't copy it",
    );
    expect(screen.queryByText("Copied.")).toBeNull();
  });

  it("closes on Done and tells its owner, who then drops the secret", async () => {
    const onClose = vi.fn();
    render(InviteLinkDialog, {
      open: true,
      name: "Sam",
      secret: "abc123",
      expiresAt: 1,
      origin: "https://puddle.example",
      onClose,
    } as never);
    await fireEvent.click(await screen.findByRole("button", { name: "Done" }));
    await vi.waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  });

  it("closes on Done", async () => {
    show();
    await fireEvent.click(await screen.findByRole("button", { name: "Done" }));
    await vi.waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });
});
