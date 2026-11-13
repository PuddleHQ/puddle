// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { DevicesStore, type Device } from "#lib/stores/devices.svelte.ts";
import DevicesPanel from "./DevicesPanel.svelte";

beforeEach(() => {
  vi.useFakeTimers({ toFake: ["Date"] });
  vi.setSystemTime(NOW);
});
afterEach(() => {
  cleanup();
  vi.useRealTimers();
});

const NOW = 1_700_000_000_000;
const device = (id: number, over: Partial<Device> = {}): Device => ({
  id,
  name: `Firefox ${id}`,
  created_at: NOW - 100_000,
  last_seen_at: NOW - 3 * 60_000,
  expires_at: NOW + 1,
  current: false,
  ...over,
});

function setup(
  devices: Device[],
  over: {
    methods?: boolean;
    mode?: string;
    delete?: number;
    password?: number;
  } = {},
) {
  const calls: string[] = [];
  const list = [...devices];
  const api = {
    GET: vi.fn(async (path: string) => {
      calls.push(`GET ${path}`);
      if (path === "/api/devices")
        return { data: { devices: list }, response: { status: 200, ok: true } };
      return over.methods === false
        ? {
            error: { message: "owner only" },
            response: { status: 403, ok: false },
          }
        : {
            data: { methods: [{ method: "password", enabled: true }] },
            response: { status: 200, ok: true },
          };
    }),
    DELETE: vi.fn(
      async (_path: string, init: { params: { path: { id: number } } }) => {
        calls.push(`DELETE ${init.params.path.id}`);
        return {
          response: {
            status: over.delete ?? 204,
            ok: (over.delete ?? 204) < 300,
          },
        };
      },
    ),
    PUT: vi.fn(async (path: string, init: { body: unknown }) => {
      calls.push(`PUT ${path} ${JSON.stringify(init.body)}`);
      if (path === "/api/auth/methods")
        return {
          error: { message: "at least one way of signing in must stay on" },
          response: { status: 422, ok: false },
        };
      const status = over.password ?? 204;
      return status < 300
        ? { response: { status, ok: true } }
        : {
            error: { message: "wrong name or password" },
            response: { status, ok: false },
          };
    }),
  };
  const session = {
    mode: over.mode ?? "signed-in",
    signOut: vi.fn(async () => undefined),
  };
  render(DevicesPanel, {
    props: { session, store: new DevicesStore(api as never) },
  } as never);
  return { api, calls, session };
}

describe("DevicesPanel", () => {
  it("lists the devices, marks this browser and shows when each was last used", async () => {
    setup([device(1, { current: true }), device(2)]);
    const list = await screen.findByRole("list", { name: "Signed-in devices" });
    expect(list).toHaveTextContent("Firefox 1");
    expect(list).toHaveTextContent("This browser");
    expect(list).toHaveTextContent("last used 3 minutes ago");
    expect(list).toHaveTextContent("Firefox 2");
  });

  it("signs another device out without touching this one", async () => {
    const { calls, session } = setup([device(1, { current: true }), device(2)]);
    await fireEvent.click(
      await screen.findByRole("button", { name: "Sign out Firefox 2" }),
    );
    await vi.waitFor(() => expect(calls).toContain("DELETE 2"));
    await vi.waitFor(() => expect(screen.queryByText("Firefox 2")).toBeNull());
    expect(session.signOut).not.toHaveBeenCalled();
  });

  it("signs this browser out through the session, which clears its cookie", async () => {
    const { calls, session } = setup([device(1, { current: true })]);
    await fireEvent.click(
      await screen.findByRole("button", {
        name: "Sign out Firefox 1 (this browser)",
      }),
    );
    expect(session.signOut).toHaveBeenCalledOnce();
    expect(calls.some((c) => c.startsWith("DELETE"))).toBe(false);
  });

  it("says when a sign-out was refused", async () => {
    setup([device(2)], { delete: 500 });
    await fireEvent.click(
      await screen.findByRole("button", { name: "Sign out Firefox 2" }),
    );
    expect(
      await screen.findByText(/Puddle's service hit an error/),
    ).toBeVisible();
  });

  it("says when nothing is signed in", async () => {
    setup([]);
    expect(await screen.findByText("No browser is signed in.")).toBeVisible();
  });

  it("changes the password after checking it twice, then shows the effect", async () => {
    const { calls } = setup([device(1, { current: true })]);
    await screen.findByRole("list");
    const type = (label: string, value: string) =>
      fireEvent.input(screen.getByLabelText(label), { target: { value } });
    await type("Current password", "old password here");
    await type("New password", "short");
    await type("The new password again", "short");
    await fireEvent.click(
      screen.getByRole("button", { name: "Change password" }),
    );
    expect(
      screen.getByText("A password needs at least 12 characters."),
    ).toBeVisible();
    await type("New password", "a brand new password");
    await type("The new password again", "another brand new one");
    await fireEvent.click(
      screen.getByRole("button", { name: "Change password" }),
    );
    expect(
      screen.getByText("The two passwords are not the same."),
    ).toBeVisible();
    await type("The new password again", "a brand new password");
    await fireEvent.click(
      screen.getByRole("button", { name: "Change password" }),
    );
    expect(
      await screen.findByText("Changed. Your other browsers are signed out."),
    ).toBeVisible();
    expect(calls).toContain(
      'PUT /api/auth/password {"current":"old password here","new":"a brand new password"}',
    );
    expect(screen.getByLabelText("New password")).toHaveValue("");
  });

  it("says why the password was not changed", async () => {
    setup([device(1, { current: true })], { password: 401 });
    await screen.findByRole("list");
    const type = (label: string, value: string) =>
      fireEvent.input(screen.getByLabelText(label), { target: { value } });
    await type("Current password", "not the password");
    await type("New password", "a brand new password");
    await type("The new password again", "a brand new password");
    await fireEvent.click(
      screen.getByRole("button", { name: "Change password" }),
    );
    expect(await screen.findByText("Wrong name or password.")).toBeVisible();
    expect(screen.queryByText(/Changed/)).toBeNull();
  });

  it("offers no password change to a window that never signed in", async () => {
    setup([device(1)], { mode: "local" });
    await screen.findByRole("list");
    expect(screen.queryByLabelText("Current password")).toBeNull();
  });

  it("shows the sign-in methods to the owner and the service's refusal to switch the last one off", async () => {
    setup([device(1)]);
    const box = await screen.findByRole("checkbox", {
      name: "Sign in with a password",
    });
    expect(box).toBeChecked();
    await fireEvent.click(box);
    expect(
      await screen.findByText("At least one way of signing in must stay on."),
    ).toBeVisible();
  });

  it("shows no methods to someone who may not read them", async () => {
    setup([device(1)], { methods: false });
    await screen.findByRole("list");
    expect(screen.queryByText("Ways to sign in")).toBeNull();
  });

  it("says why the devices could not be read", async () => {
    const api = {
      GET: vi.fn(async () => ({
        error: { message: "broken" },
        response: { status: 500, ok: false },
      })),
    };
    render(DevicesPanel, {
      props: {
        session: { mode: "signed-in", signOut: vi.fn() },
        store: new DevicesStore(api as never),
      },
    } as never);
    expect(await screen.findByText("Couldn't read the devices.")).toBeVisible();
    expect(screen.getByText("Broken.")).toBeVisible();
  });
});
