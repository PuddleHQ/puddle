// SPDX-License-Identifier: AGPL-3.0-or-later
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { tabNotifications } from "#lib/notify/tab-switch.svelte.ts";
import TabNotificationsSetting from "./TabNotificationsSetting.svelte";

function browser(permission: NotificationPermission, answer = permission) {
  const fake = {
    permission,
    requestPermission: vi.fn(async () => {
      fake.permission = answer;
      return answer;
    }),
  };
  vi.stubGlobal("Notification", fake);
  tabNotifications.chosen = false;
  tabNotifications.refresh();
  return fake;
}

beforeEach(() => localStorage.clear());
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

const box = () =>
  screen.getByRole("switch", { name: "Notifications in this browser" });

describe("TabNotificationsSetting", () => {
  it("turns on after the browser's permission, and off again", async () => {
    browser("default", "granted");
    render(TabNotificationsSetting);
    expect(box()).not.toBeChecked();
    await fireEvent.click(box());
    await vi.waitFor(() => expect(tabNotifications.chosen).toBe(true));
    await fireEvent.click(box());
    expect(tabNotifications.chosen).toBe(false);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("says so when the browser blocks notifications", async () => {
    browser("default", "denied");
    render(TabNotificationsSetting);
    await fireEvent.click(box());
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "blocked notifications for this page",
    );
    expect(box()).not.toBeChecked();
  });

  it("says so when the browser has no notifications", async () => {
    vi.unstubAllGlobals();
    render(TabNotificationsSetting);
    await fireEvent.click(box());
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "can't show notifications",
    );
  });

  it("warns when the choice is on but the browser's permission was taken away", () => {
    browser("denied");
    tabNotifications.chosen = true;
    render(TabNotificationsSetting);
    expect(screen.getByRole("alert")).toHaveTextContent("blocked");
  });
});
