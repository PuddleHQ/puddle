// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, describe, expect, it, vi } from "vitest";
import type { Mode, Outcome } from "#lib/session/session.svelte.ts";

const navigation = vi.hoisted(() => ({ goto: vi.fn(async () => undefined) }));
vi.mock("$app/navigation", () => navigation);

import SignInGate from "./SignInGate.svelte";

afterEach(cleanup);

function fake(over: Record<string, unknown> = {}) {
  return {
    mode: "signed-out" as Mode,
    invite: null as string | null,
    passwordLogin: true,
    busy: false,
    signIn: vi.fn(async (): Promise<Outcome> => ({ ok: true })),
    setup: vi.fn(async (): Promise<Outcome> => ({ ok: true })),
    redeemInvite: vi.fn(async (): Promise<Outcome> => ({ ok: true })),
    ...over,
  };
}
const mount = (session: ReturnType<typeof fake>) =>
  render(SignInGate, { props: { session } } as never);

async function fill(label: RegExp | string, value: string) {
  await fireEvent.input(screen.getByLabelText(label), { target: { value } });
}

describe("SignInGate: signing in", () => {
  it("stays on the page it is on after a plain sign-in", async () => {
    navigation.goto.mockClear();
    const session = fake();
    mount(session);
    await fill("User name", "Tijs");
    await fill("Password", "correct horse battery");
    await fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(navigation.goto).not.toHaveBeenCalled();
  });

  it("asks for a name and a password and sends them trimmed", async () => {
    const session = fake();
    mount(session);
    expect(
      screen.getByRole("heading", { name: "Sign in to Puddle" }),
    ).toBeVisible();
    expect(screen.queryByLabelText(/again/)).toBeNull();
    await fill("User name", "  Tijs ");
    await fill("Password", "correct horse battery");
    await fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(session.signIn).toHaveBeenCalledWith(
      "Tijs",
      "correct horse battery",
    );
    // Nothing keeps the password once it was sent.
    expect(screen.getByLabelText("Password")).toHaveValue("");
  });

  it("shows why it was refused", async () => {
    const session = fake({
      signIn: vi.fn(async (): Promise<Outcome> => ({
        ok: false,
        message: "Wrong name or password.",
      })),
    });
    mount(session);
    await fill("User name", "Tijs");
    await fill("Password", "nope");
    await fireEvent.click(screen.getByRole("button", { name: "Sign in" }));
    expect(await screen.findByText("Wrong name or password.")).toBeVisible();
  });

  it("says so when passwords are switched off and when the service is down", () => {
    mount(fake({ passwordLogin: false }));
    expect(screen.getByRole("alert")).toHaveTextContent(/switched off here/);
    cleanup();
    mount(fake({ mode: "unavailable" }));
    expect(screen.getByRole("alert")).toHaveTextContent(/isn't answering/);
    expect(screen.queryByLabelText("Password")).toBeNull();
  });
});

describe("SignInGate: the first visitor", () => {
  it("makes the owner account after checking the password twice", async () => {
    const session = fake({ mode: "setup" });
    mount(session);
    expect(
      screen.getByRole("heading", { name: "Welcome to Puddle" }),
    ).toBeVisible();
    expect(
      screen.getByText(/The account you make now is the owner's/),
    ).toBeVisible();
    await fill("User name", "Tijs");
    await fill("Password", "short");
    await fill("The same password again", "short");
    await fireEvent.click(
      screen.getByRole("button", { name: "Create the owner account" }),
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "A password needs at least 12 characters.",
    );
    await fill("Password", "correct horse battery");
    await fill("The same password again", "correct horse batterY");
    await fireEvent.click(
      screen.getByRole("button", { name: "Create the owner account" }),
    );
    expect(screen.getByRole("alert")).toHaveTextContent(
      "The two passwords are not the same.",
    );
    expect(session.setup).not.toHaveBeenCalled();
    await fill("Password", "correct horse battery");
    await fill("The same password again", "correct horse battery");
    await fireEvent.click(
      screen.getByRole("button", { name: "Create the owner account" }),
    );
    expect(session.setup).toHaveBeenCalledWith("Tijs", "correct horse battery");
  });
});

describe("SignInGate: an invite link", () => {
  it("sets a new password with the link's secret and asks for no name", async () => {
    const session = fake({ invite: "the-secret" });
    mount(session);
    expect(
      screen.getByRole("heading", { name: "Set a new password" }),
    ).toBeVisible();
    expect(screen.queryByLabelText("User name")).toBeNull();
    await fill("New password", "a brand new password");
    await fill("The same password again", "a brand new password");
    await fireEvent.click(
      screen.getByRole("button", { name: "Set password and sign in" }),
    );
    expect(session.redeemInvite).toHaveBeenCalledWith(
      "the-secret",
      "a brand new password",
    );
    expect(navigation.goto).toHaveBeenCalledWith("/");
  });

  it("shows the refusal of a link that is used up", async () => {
    const session = fake({
      invite: "x",
      redeemInvite: vi.fn(async (): Promise<Outcome> => ({
        ok: false,
        message: "This invite link is not valid any more.",
      })),
    });
    mount(session);
    await fill("New password", "a brand new password");
    await fill("The same password again", "a brand new password");
    await fireEvent.click(
      screen.getByRole("button", { name: "Set password and sign in" }),
    );
    expect(
      await screen.findByText("This invite link is not valid any more."),
    ).toBeVisible();
  });
});
