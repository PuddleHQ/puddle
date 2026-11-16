// SPDX-License-Identifier: AGPL-3.0-or-later
import { afterEach, describe, expect, it, vi } from "vitest";
import {
  TabNotificationSwitch,
  type PermissionApi,
} from "./tab-switch.svelte.ts";

const store = (initial?: string) => {
  const data = new Map<string, string>();
  if (initial) data.set("puddle.tab-notifications", initial);
  return {
    getItem: (k: string) => data.get(k) ?? null,
    setItem: (k: string, v: string) => void data.set(k, v),
    data,
  };
};

const browser = (
  permission: NotificationPermission,
  answer: NotificationPermission = permission,
): { permission: NotificationPermission; asked: number } & PermissionApi => {
  const api: {
    permission: NotificationPermission;
    asked: number;
  } & PermissionApi = {
    permission,
    asked: 0,
    requestPermission: async () => {
      api.asked += 1;
      api.permission = answer;
      return answer;
    },
  };
  return api;
};

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("TabNotificationSwitch", () => {
  it("starts off, and is active only with the choice and the permission", () => {
    const s = new TabNotificationSwitch(store(), browser("granted"));
    expect(s.chosen).toBe(false);
    expect(s.active).toBe(false);
    const on = new TabNotificationSwitch(store("on"), browser("granted"));
    expect(on.active).toBe(true);
    const blocked = new TabNotificationSwitch(store("on"), browser("denied"));
    expect(blocked.chosen).toBe(true);
    expect(blocked.active).toBe(false);
  });

  it("asks for the permission when turned on and keeps the choice", async () => {
    const storage = store();
    const api = browser("default", "granted");
    const s = new TabNotificationSwitch(storage, api);
    expect(await s.enable()).toBe("on");
    expect(api.asked).toBe(1);
    expect(s.active).toBe(true);
    expect(storage.data.get("puddle.tab-notifications")).toBe("on");
    expect(new TabNotificationSwitch(storage, api).active).toBe(true);
  });

  it("does not ask again when the permission is already given", async () => {
    const api = browser("granted");
    const s = new TabNotificationSwitch(store(), api);
    expect(await s.enable()).toBe("on");
    expect(api.asked).toBe(0);
  });

  it("stays off and says so when the browser refuses", async () => {
    const storage = store();
    const s = new TabNotificationSwitch(storage, browser("default", "denied"));
    expect(await s.enable()).toBe("denied");
    expect(s.active).toBe(false);
    expect(s.permission).toBe("denied");
    expect(storage.data.get("puddle.tab-notifications")).toBe("off");
  });

  it("treats a permission request that throws as a refusal", async () => {
    const api: PermissionApi = {
      permission: "default",
      requestPermission: async () => {
        throw new Error("not from a click");
      },
    };
    expect(await new TabNotificationSwitch(store(), api).enable()).toBe(
      "denied",
    );
  });

  it("turns off and reads the permission again on request", async () => {
    const api = browser("granted");
    const s = new TabNotificationSwitch(store("on"), api);
    s.disable();
    expect(s.active).toBe(false);
    s.enable();
    api.permission = "denied";
    s.refresh();
    expect(s.permission).toBe("denied");
  });

  it("is unsupported where the browser has no notifications", async () => {
    const s = new TabNotificationSwitch(store(), null);
    expect(s.permission).toBe("unsupported");
    expect(await s.enable()).toBe("unsupported");
    expect(s.active).toBe(false);
    s.refresh();
    expect(s.permission).toBe("unsupported");
  });

  it("survives storage that throws", async () => {
    const broken = {
      getItem: () => {
        throw new Error("no");
      },
      setItem: () => {
        throw new Error("no");
      },
    };
    const s = new TabNotificationSwitch(broken, browser("granted"));
    expect(s.chosen).toBe(false);
    expect(await s.enable()).toBe("on");
    expect(s.active).toBe(true);
  });

  it("uses the page's own storage and Notification by default", () => {
    const s = new TabNotificationSwitch();
    expect(["unsupported", "default", "granted", "denied"]).toContain(
      s.permission,
    );
  });

  it("reads the browser's Notification when the page has one", () => {
    vi.stubGlobal("Notification", {
      permission: "granted",
      requestPermission: async () => "granted",
    });
    expect(new TabNotificationSwitch(store()).permission).toBe("granted");
  });

  it("works where reading local storage throws", () => {
    vi.spyOn(globalThis, "localStorage", "get").mockImplementation(() => {
      throw new Error("blocked");
    });
    expect(new TabNotificationSwitch(undefined, null).chosen).toBe(false);
  });
});
