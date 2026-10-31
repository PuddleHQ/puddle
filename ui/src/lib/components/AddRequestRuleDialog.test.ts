// SPDX-License-Identifier: GPL-3.0-or-later
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { NewRequestRule } from "#lib/rules/request-rules.ts";
import type { RequestRuleRefusal } from "#lib/stores/request-rules.svelte.ts";
import AddRequestRuleDialog from "./AddRequestRuleDialog.svelte";

afterEach(cleanup);

type Submit = (rule: NewRequestRule) => Promise<RequestRuleRefusal | null>;

async function open(
  onSubmit: Submit = vi.fn<Submit>(async () => null),
  workspaces: string[] = ["shop"],
) {
  const view = render(AddRequestRuleDialog, {
    props: { open: true, workspaces, onSubmit },
  } as never);
  await screen.findByRole("dialog", { name: "Add a method and path rule" });
  return { view, onSubmit };
}

const field = (name: string) => screen.getByLabelText(name);
const submit = () =>
  fireEvent.click(screen.getByRole("button", { name: "Add rule" }));

async function fill(
  host: string,
  methods: string,
  path: string,
  workspace = "shop",
) {
  await fireEvent.input(field("Host"), { target: { value: host } });
  await fireEvent.input(field("Methods"), { target: { value: methods } });
  await fireEvent.input(field("Path"), { target: { value: path } });
  await fireEvent.input(field("Workspace name"), {
    target: { value: workspace },
  });
}

describe("the Add method and path rule dialog", () => {
  it("says what the rule does and that it never allows anything", async () => {
    await open();
    expect(screen.getByRole("dialog")).toHaveAccessibleDescription(
      /never allows anything/,
    );
    expect(
      screen.getByText(/decrypts this host while the rule exists/),
    ).toBeInTheDocument();
  });

  it("sends a complete request: trimmed host, upper-case methods, the path, the workspace", async () => {
    const { onSubmit } = await open();
    await fill(" api.example.com ", "delete, put", " /repos/** ");
    await submit();
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith({
        host: "api.example.com",
        methods: ["DELETE", "PUT"],
        path: "/repos/**",
        workspace: "shop",
      }),
    );
  });

  it("sends no methods for every method, and no workspace for every workspace", async () => {
    const { onSubmit } = await open();
    await fireEvent.click(screen.getByLabelText(/Every workspace/));
    expect(screen.queryByLabelText("Workspace name")).toBeNull();
    await fireEvent.input(field("Host"), {
      target: { value: "a.example.com" },
    });
    await fireEvent.input(field("Path"), { target: { value: "/**" } });
    await submit();
    await waitFor(() =>
      expect(onSubmit).toHaveBeenCalledWith({
        host: "a.example.com",
        methods: [],
        path: "/**",
        workspace: null,
      }),
    );
  });

  it("checks every field before asking and puts each message by its field", async () => {
    const { onSubmit } = await open();
    await fill("10.0.0.1", "GE(T)", "repos", "Bad Name");
    await submit();
    const alerts = await screen.findAllByRole("alert");
    expect(alerts.map((a) => a.textContent)).toEqual([
      expect.stringContaining("IP addresses"),
      expect.stringContaining("not an HTTP method"),
      expect.stringContaining("starts with /"),
      expect.stringContaining("lower-case letters"),
    ]);
    expect(field("Host")).toHaveAttribute("aria-invalid", "true");
    expect(field("Host").getAttribute("aria-describedby")).toContain(
      "-host-error",
    );
    expect(field("Path")).toHaveAttribute("aria-invalid", "true");
    expect(onSubmit).not.toHaveBeenCalled();
  });

  it("shows the API's refusal at the field it concerns and keeps what was typed", async () => {
    const onSubmit = vi.fn(async () => ({
      field: "path" as const,
      message: "The path reaches outside the host.",
    }));
    await open(onSubmit);
    await fill("a.example.com", "GET", "/a/%2e%2e/b");
    await submit();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "The path reaches outside the host.",
    );
    expect(field("Path")).toHaveAttribute("aria-invalid", "true");
    expect(field("Path")).toHaveValue("/a/%2e%2e/b");
    expect(field("Host")).not.toHaveAttribute("aria-invalid");
  });

  it("shows a refusal about the whole form under the fields", async () => {
    const onSubmit = vi.fn(async () => ({
      field: "form" as const,
      message: "At most 500 rules in one scope.",
    }));
    await open(onSubmit);
    await fill("a.example.com", "", "/a");
    await submit();
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "At most 500 rules",
    );
  });

  it("cancel closes and empties the form", async () => {
    await open();
    await fireEvent.input(field("Host"), {
      target: { value: "a.example.com" },
    });
    await fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
  });

  it("labels every control and submits as a form, so Enter in a field sends it", async () => {
    const onSubmit = vi.fn(async () => null);
    await open(onSubmit);
    for (const name of ["Host", "Methods", "Path", "Workspace name"]) {
      expect(field(name)).toBeInTheDocument();
    }
    expect(screen.getByRole("radio", { name: "One workspace" })).toBeChecked();
    await fill("a.example.com", "", "/a");
    const form = field("Host").closest("form")!;
    await fireEvent.submit(form);
    await waitFor(() => expect(onSubmit).toHaveBeenCalledTimes(1));
  });

  it("shows the workspace field again when One workspace is chosen after Every workspace", async () => {
    await open();
    await fireEvent.click(screen.getByLabelText(/Every workspace/));
    expect(screen.queryByLabelText("Workspace name")).toBeNull();
    await fireEvent.click(screen.getByLabelText("One workspace"));
    expect(screen.getByLabelText("Workspace name")).toBeInTheDocument();
  });

  it("clears on request", async () => {
    const { view } = await open();
    await fireEvent.input(field("Host"), {
      target: { value: "a.example.com" },
    });
    (view.component as unknown as { clear: () => void }).clear();
    await waitFor(() => expect(field("Host")).toHaveValue(""));
  });
});
