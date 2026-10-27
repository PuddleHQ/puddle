// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { SaveResult } from "#lib/stores/interception.svelte.ts";
import NeverDecryptEditor from "./NeverDecryptEditor.svelte";

afterEach(cleanup);

const box = () =>
  screen.getByRole("textbox", { name: "Never decrypt" }) as HTMLTextAreaElement;
const saveButton = () =>
  screen.getByRole("button", { name: "Save names" }) as HTMLButtonElement;

type Save = (names: string[]) => Promise<SaveResult>;

function mount(
  names: string[],
  save: Save = vi.fn<Save>(async () => ({ ok: true })),
) {
  render(NeverDecryptEditor, {
    label: "Never decrypt",
    hint: "One name per line.",
    names,
    save,
  });
  return save;
}

describe("NeverDecryptEditor", () => {
  it("shows the stored names one to a line and has nothing to save yet", () => {
    mount(["a.io", "*.b.io"]);
    expect(box().value).toBe("a.io\n*.b.io");
    expect(saveButton().disabled).toBe(true);
  });

  it("saves the parsed names and says so", async () => {
    const save = mount([]);
    await fireEvent.input(box(), { target: { value: "A.io, b.io\nA.io" } });
    expect(saveButton().disabled).toBe(false);
    await fireEvent.click(saveButton());
    expect(save).toHaveBeenCalledWith(["a.io", "b.io"]);
    expect(await screen.findByText("Saved.")).toBeTruthy();
  });

  it("shows the refusal and keeps what was typed", async () => {
    const save = vi.fn<Save>(async () => ({
      ok: false,
      message: "10.0.0.1 is an IP address.",
    }));
    mount(["a.io"], save);
    await fireEvent.input(box(), { target: { value: "10.0.0.1" } });
    await fireEvent.click(saveButton());
    expect((await screen.findByRole("alert")).textContent).toBe(
      "10.0.0.1 is an IP address.",
    );
    expect(box().value).toBe("10.0.0.1");
    await waitFor(() => expect(saveButton().disabled).toBe(false));
  });

  it("follows the stored list when it changes", async () => {
    const { rerender } = render(NeverDecryptEditor, {
      label: "Never decrypt",
      hint: "h",
      names: ["a.io"],
      save: vi.fn<Save>(async () => ({ ok: true })),
    });
    await rerender({ names: ["z.io"] });
    expect(box().value).toBe("z.io");
  });
});
