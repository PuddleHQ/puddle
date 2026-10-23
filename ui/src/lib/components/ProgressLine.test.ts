// SPDX-License-Identifier: GPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import ProgressLine from "./ProgressLine.svelte";

afterEach(cleanup);

describe("ProgressLine", () => {
  it("shows nothing when nothing is going on", () => {
    const { container } = render(ProgressLine, {
      props: { busy: null, progress: undefined },
    });
    expect(container.textContent?.trim()).toBe("");
  });

  it("names the operation until a step is known", () => {
    render(ProgressLine, { props: { busy: "starting", progress: undefined } });
    expect(screen.getByRole("status")).toHaveTextContent("Starting");
  });

  it("names the step and where it sits, and quotes the detail as text", () => {
    render(ProgressLine, {
      props: {
        busy: "creating",
        progress: {
          step: "cloning",
          detail: "<b>https://example.org/x.git</b>",
          repository: null,
          operation: "creating",
          failed: false,
        },
      },
    });
    const line = screen.getByRole("status");
    expect(line).toHaveTextContent("Cloning the repository");
    expect(line).toHaveTextContent("step 3 of 3");
    expect(line).toHaveTextContent("<b>https://example.org/x.git</b>");
    expect(line.querySelector("b b")).toBeNull();
  });

  it("names the checkout a cloning step is on, and its place in a list of several", () => {
    const progress = {
      step: "cloning" as const,
      detail: "https://example.org/web.git",
      repository: "web",
      operation: "creating" as const,
      failed: false,
    };
    const list = ["api", "web"].map((directory) => ({ directory }));
    const { unmount } = render(ProgressLine, {
      props: { busy: "creating", progress, repositories: list },
    });
    expect(screen.getByRole("status")).toHaveTextContent("Cloning web, 2 of 2");
    unmount();
    render(ProgressLine, {
      props: {
        busy: "creating",
        progress,
        repositories: [{ directory: "web" }],
      },
    });
    const line = screen.getByRole("status");
    expect(line).toHaveTextContent("Cloning web");
    expect(line).not.toHaveTextContent("of 1");
  });

  it("keeps the generic words for a checkout the list does not have", () => {
    render(ProgressLine, {
      props: {
        busy: "creating",
        progress: {
          step: "cloning",
          detail: null,
          repository: "gone",
          operation: "creating",
          failed: false,
        },
        repositories: [{ directory: "api" }, { directory: "web" }],
      },
    });
    expect(screen.getByRole("status")).toHaveTextContent("Cloning gone");
    expect(screen.getByRole("status")).not.toHaveTextContent("of 2");
  });

  it("leaves out the position for a step its operation doesn't have", () => {
    render(ProgressLine, {
      props: {
        busy: null,
        progress: {
          step: "cloning",
          detail: null,
          repository: null,
          operation: "starting",
          failed: false,
        },
      },
    });
    expect(screen.getByRole("status")).not.toHaveTextContent("step");
  });

  it("says what failed and why, and can be dismissed", async () => {
    const onDismiss = vi.fn();
    render(ProgressLine, {
      props: {
        busy: null,
        onDismiss,
        progress: {
          step: "failed",
          detail: "no boot",
          repository: null,
          operation: "starting",
          failed: true,
        },
      },
    });
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("Starting failed.");
    expect(alert).toHaveTextContent("no boot");
    await fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(onDismiss).toHaveBeenCalledOnce();
  });

  it("copes with a failure of unknown operation and no way to dismiss", () => {
    render(ProgressLine, {
      props: {
        busy: null,
        progress: {
          step: "failed",
          detail: null,
          repository: null,
          operation: null,
          failed: true,
        },
      },
    });
    expect(screen.getByRole("alert")).toHaveTextContent("That failed.");
    expect(screen.queryByRole("button")).toBeNull();
  });
});
