// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it, vi } from "vitest";
const post = vi.hoisted(() => vi.fn());
vi.mock("#lib/api/client.ts", () => ({ api: { POST: post } }));

import { allowedNotice, checkHosts, hostList } from "./hosts.ts";

describe("hostList", () => {
  it("joins one, two and three hosts in words", () => {
    expect(hostList(["a.com"])).toBe("a.com");
    expect(hostList(["a.com", "b.com"])).toBe("a.com and b.com");
    expect(hostList(["a.com", "b.com", "c.com"])).toBe(
      "a.com, b.com and c.com",
    );
  });
});

describe("allowedNotice", () => {
  it("is null when nothing was allowed", () => {
    expect(allowedNotice([])).toBeNull();
  });
  it("names the hosts and where the rules are", () => {
    expect(allowedNotice([{ host: "github.com" }])).toBe(
      "Puddle allowed github.com for this workspace so it can clone. The rule is on the Rules screen, marked as made by the create.",
    );
    expect(allowedNotice([{ host: "a.com" }, { host: "b.com" }])).toContain(
      "The rules are on the Rules screen",
    );
  });
});

describe("checkHosts", () => {
  it("posts the addresses and the workspace and returns the hosts", async () => {
    const hosts = [{ host: "github.com", state: "new", blocked_by: null }];
    post.mockResolvedValueOnce({ data: { hosts } });
    expect(await checkHosts(["https://github.com/a/b"], "made-1")).toEqual(
      hosts,
    );
    expect(post).toHaveBeenLastCalledWith("/api/repository-hosts", {
      body: { urls: ["https://github.com/a/b"], workspace: "made-1" },
    });
    post.mockResolvedValueOnce({ data: { hosts: [] } });
    await checkHosts(["https://github.com/a/b"]);
    expect(post).toHaveBeenLastCalledWith("/api/repository-hosts", {
      body: { urls: ["https://github.com/a/b"] },
    });
  });

  it("is null when the service refuses or cannot be reached", async () => {
    post.mockResolvedValueOnce({ error: {} });
    expect(await checkHosts(["https://github.com/a/b"])).toBeNull();
    post.mockRejectedValueOnce(new TypeError("down"));
    expect(await checkHosts(["https://github.com/a/b"])).toBeNull();
  });
});
