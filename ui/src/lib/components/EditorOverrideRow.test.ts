// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { parseExtensions, parseTheme } from "#lib/settings/editor.ts";
import EditorOverrideRow from "./EditorOverrideRow.svelte";

afterEach(cleanup);

function mount(props: Record<string, unknown>) {
  const table = document.body.appendChild(document.createElement("table"));
  const target = table.appendChild(document.createElement("tbody"));
  return render(EditorOverrideRow as never, {
    target,
    props: {
      id: "row",
      label: "Theme",
      hint: "A hint.",
      value: "Nord",
      source: "workspace",
      parse: parseTheme,
      onsave: () => Promise.resolve(true),
      onreset: () => Promise.resolve(true),
      ...props,
    },
  });
}

describe("EditorOverrideRow", () => {
  it("shows the hint and placeholder, and marks a refused list on its textarea", async () => {
    mount({
      parse: parseExtensions,
      multiline: true,
      value: "",
      placeholder: "publisher.name",
    });
    const area = screen.getByLabelText("Theme") as HTMLTextAreaElement;
    expect(area.placeholder).toBe("publisher.name");
    expect(area.getAttribute("aria-describedby")).toBe("row-hint");
    await fireEvent.input(area, { target: { value: "not an id" } });
    expect(area.getAttribute("aria-invalid")).toBe("true");
    expect(area.getAttribute("aria-describedby")).toContain("row-problem");
    expect(await screen.findByRole("alert")).toBeVisible();
  });

  it("ignores a second submit and disables both buttons while a request is pending", async () => {
    let finish: (ok: boolean) => void = () => {};
    const onsave = vi.fn(
      () => new Promise<boolean>((resolve) => (finish = resolve)),
    );
    const onreset = vi.fn(
      () => new Promise<boolean>((resolve) => (finish = resolve)),
    );
    mount({ onsave, onreset });
    const input = screen.getByLabelText("Theme") as HTMLInputElement;
    await fireEvent.input(input, { target: { value: "Solarized" } });
    await fireEvent.submit(input.form!);
    await fireEvent.submit(input.form!);
    expect(onsave).toHaveBeenCalledTimes(1);
    expect(screen.getByRole("button", { name: "Save Theme" })).toBeDisabled();
    finish(true);
    await vi.waitFor(() =>
      expect(screen.getByRole("button", { name: "Save Theme" })).toBeEnabled(),
    );

    const reset = screen.getByRole("button", {
      name: "Use the global setting again for Theme",
    });
    await fireEvent.click(reset);
    expect(onreset).toHaveBeenCalledTimes(1);
    expect(reset).toBeDisabled();
    finish(true);
    await vi.waitFor(() => expect(reset).toBeEnabled());
  });
});
