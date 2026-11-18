// SPDX-License-Identifier: AGPL-3.0-or-later
// The one save behaviour of Settings, once per kind of control: a switch and a dropdown apply the
// moment they change, a typed value applies when its Save button is pressed, and every row says
// "Saved." beside itself with an Undo, or says why it was refused and puts the old value back.
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import { saverContext } from "#lib/settings/context.ts";
import { SettingsSaver } from "#lib/settings/saver.svelte.ts";
import SelectRow from "./SelectRow.svelte";
import SwitchRow from "./SwitchRow.svelte";
import ValueRow from "./ValueRow.svelte";

afterEach(cleanup);

type Outcome = { ok: true } | { ok: false; message: string };

/** A saver whose changes succeed or are refused, and that records what each one set. */
function saverWith(outcome: () => Outcome = () => ({ ok: true })) {
  const sent: unknown[] = [];
  const store = {
    view: {} as never,
    save: vi.fn(async (patch: unknown) => {
      sent.push(patch);
      return outcome();
    }),
  };
  return { saver: new SettingsSaver(store as never), store, sent };
}

describe("a switch", () => {
  function show(
    saver: SettingsSaver,
    apply: (next: boolean) => Promise<boolean>,
  ) {
    render(SwitchRow, {
      props: { id: "set-x", label: "Sound", checked: false, onchange: apply },
      context: saverContext(saver),
    });
  }

  it("applies the moment it is flipped, says Saved and offers Undo that puts it back", async () => {
    const { saver } = saverWith();
    const values: boolean[] = [];
    show(saver, (next) => {
      values.push(next);
      return saver.apply(
        "set-x",
        { ui: { sound: next } },
        { ui: { sound: !next } },
      );
    });
    await fireEvent.click(screen.getByRole("switch", { name: "Sound" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    expect(values).toEqual([true]);
    expect(screen.queryByRole("button", { name: /^Save/ })).toBeNull();
    await fireEvent.click(screen.getByRole("button", { name: /Undo/ }));
    expect(await screen.findByText("Undone.")).toBeInTheDocument();
  });

  it("goes back to what is stored and says why when the change is refused", async () => {
    const { saver } = saverWith(() => ({
      ok: false,
      message: "Puddle said no.",
    }));
    show(saver, (next) =>
      saver.apply("set-x", { ui: { sound: next } }, { ui: { sound: !next } }),
    );
    const box = screen.getByRole("switch", { name: "Sound" });
    await fireEvent.click(box);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Puddle said no.",
    );
    await waitFor(() => expect(box).not.toBeChecked());
    expect(screen.queryByText("Saved.")).toBeNull();
  });
});

describe("a dropdown", () => {
  function show(saver: SettingsSaver) {
    render(SelectRow, {
      props: {
        id: "set-x",
        label: "Density",
        value: "comfortable",
        options: [
          { value: "comfortable", label: "Comfortable" },
          { value: "compact", label: "Compact" },
        ],
        onchange: (next: string) =>
          saver.apply("set-x", { ui: { density: next as "compact" } }),
      },
      context: saverContext(saver),
    });
  }

  it("applies the pick at once and says Saved", async () => {
    const { saver, sent } = saverWith();
    show(saver);
    await fireEvent.change(screen.getByLabelText("Density"), {
      target: { value: "compact" },
    });
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    expect(sent).toHaveLength(1);
  });

  it("shows the old value again when the pick is refused", async () => {
    const { saver } = saverWith(() => ({ ok: false, message: "Not now." }));
    show(saver);
    const select = screen.getByLabelText("Density") as HTMLSelectElement;
    await fireEvent.change(select, { target: { value: "compact" } });
    expect(await screen.findByRole("alert")).toHaveTextContent("Not now.");
    await waitFor(() => expect(select.value).toBe("comfortable"));
  });
});

describe("a typed value", () => {
  function show(saver: SettingsSaver, onsave: (n: number) => Promise<boolean>) {
    render(ValueRow<number>, {
      props: {
        id: "set-x",
        label: "Grace",
        value: "60",
        parse: (text: string) =>
          /^\d+$/.test(text.trim()) && Number(text) >= 30
            ? { ok: true as const, value: Number(text) }
            : { ok: false as const, message: "Use 30 or more." },
        onsave,
      },
      context: saverContext(saver),
    });
  }

  it("sends nothing while typing, shows Save only when the text differs, and applies on Save", async () => {
    const { saver } = saverWith();
    const sent: number[] = [];
    show(saver, (n) => {
      sent.push(n);
      return saver.apply("set-x", { ui: {} });
    });
    const field = screen.getByLabelText("Grace");
    expect(screen.queryByRole("button", { name: /^Save/ })).toBeNull();
    await fireEvent.input(field, { target: { value: "120" } });
    expect(sent).toEqual([]);
    await fireEvent.click(screen.getByRole("button", { name: "Save Grace" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    expect(sent).toEqual([120]);
  });

  it("hides Save again when the text is put back to what is stored", async () => {
    const { saver } = saverWith();
    show(saver, async () => true);
    const field = screen.getByLabelText("Grace");
    await fireEvent.input(field, { target: { value: "90" } });
    expect(screen.getByRole("button", { name: "Save Grace" })).toBeEnabled();
    await fireEvent.input(field, { target: { value: "60" } });
    expect(screen.queryByRole("button", { name: /^Save/ })).toBeNull();
  });

  it("says why a value cannot be used and holds Save back", async () => {
    const { saver } = saverWith();
    const onsave = vi.fn(async () => true);
    show(saver, onsave);
    await fireEvent.input(screen.getByLabelText("Grace"), {
      target: { value: "5" },
    });
    expect(screen.getByRole("alert")).toHaveTextContent("Use 30 or more.");
    expect(screen.getByLabelText("Grace")).toBeInvalid();
    expect(screen.getByRole("button", { name: "Save Grace" })).toBeDisabled();
    expect(onsave).not.toHaveBeenCalled();
  });

  it("keeps what was typed and says why when the save is refused", async () => {
    const { saver } = saverWith(() => ({ ok: false, message: "Refused." }));
    show(saver, (n) =>
      saver.apply("set-x", { ui: {} }).then((ok) => ok && n > 0),
    );
    const field = screen.getByLabelText("Grace") as HTMLInputElement;
    await fireEvent.input(field, { target: { value: "90" } });
    await fireEvent.click(screen.getByRole("button", { name: "Save Grace" }));
    expect(await screen.findByText("Refused.")).toBeInTheDocument();
    expect(field.value).toBe("90");
  });
});
