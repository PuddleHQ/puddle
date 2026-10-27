// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { asEnvFailed, isEnvChanged } from "./events.ts";

const workspace = { kind: "workspace", name: "web" } as const;
const global = { kind: "global" } as const;

describe("isEnvChanged", () => {
  it("lets a global change reach both lists, since a workspace lists the global ones too", () => {
    expect(isEnvChanged({ type: "global_env_changed" }, global)).toBe(true);
    expect(isEnvChanged({ type: "global_env_changed" }, workspace)).toBe(true);
  });

  it("lets a workspace change reach that workspace only", () => {
    const own = { type: "workspace_env_changed", workspace: "web" };
    expect(isEnvChanged(own, workspace)).toBe(true);
    expect(isEnvChanged({ ...own, workspace: "docs" }, workspace)).toBe(false);
    expect(isEnvChanged(own, global)).toBe(false);
  });

  it("ignores anything else", () => {
    expect(isEnvChanged({ type: "rules_changed" }, global)).toBe(false);
    expect(isEnvChanged("text", global)).toBe(false);
    expect(isEnvChanged(null, global)).toBe(false);
  });
});

describe("asEnvFailed", () => {
  it("reads the event with its fields and nothing less", () => {
    const event = {
      type: "workspace_env_failed",
      workspace: "web",
      reason: "the store is locked",
    };
    expect(asEnvFailed(event)).toEqual(event);
    expect(asEnvFailed({ ...event, reason: 3 })).toBeNull();
    expect(asEnvFailed({ ...event, workspace: undefined })).toBeNull();
    expect(asEnvFailed({ ...event, type: "other" })).toBeNull();
    expect(asEnvFailed(undefined)).toBeNull();
  });
});
