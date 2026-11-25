// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import AllowDenyPair from "./AllowDenyPair.svelte";
import HostName from "./HostName.svelte";

afterEach(cleanup);

describe("HostName", () => {
  it("draws the registrable part bold, a usual port grey and a wildcard as a mark", () => {
    const { container } = render(HostName, {
      props: { host: "*.api.stripe.com", port: 443 },
    });
    expect(container.querySelector("b.main")).toHaveTextContent("stripe.com");
    expect(container.querySelector(".lead")).toHaveTextContent("api.");
    expect(container.querySelector(".wild")).toHaveTextContent("*.");
    expect(container.querySelector(".port")).toHaveClass("usual");
    expect(container.textContent).toBe("*.api.stripe.com:443");
  });

  it("leaves another port at full strength", () => {
    const { container } = render(HostName, {
      props: { host: "example.com", port: 8443 },
    });
    expect(container.querySelector(".port")).not.toHaveClass("usual");
  });
});

describe("AllowDenyPair", () => {
  it("is Allow then Deny, and Deny alone when only that is possible", async () => {
    const onAllow = vi.fn();
    const onDeny = vi.fn();
    render(AllowDenyPair, { props: { onAllow, onDeny } });
    const names = screen
      .getAllByRole("button")
      .map((b) => b.textContent?.trim());
    expect(names).toEqual(["Allow", "Deny"]);
    await fireEvent.click(screen.getByRole("button", { name: "Deny" }));
    expect(onDeny).toHaveBeenCalledOnce();
    cleanup();
    render(AllowDenyPair, { props: { onAllow, onDeny, denyOnly: true } });
    expect(screen.getAllByRole("button")).toHaveLength(1);
  });
});
