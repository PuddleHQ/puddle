// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { port } from "#lib/testing/fake-environment.ts";
import {
  asPortForwardFailed,
  forwardText,
  hasForward,
  isPortsEvent,
  parsePort,
} from "./ports.ts";

describe("parsePort", () => {
  it("accepts a whole number from 1 to 65535", () => {
    expect(parsePort("3000")).toBe(3000);
    expect(parsePort(" 1 ")).toBe(1);
    expect(parsePort("65535")).toBe(65535);
  });

  it("refuses the rest", () => {
    for (const bad of ["", "0", "65536", "12.5", "-1", "80a", "123456"]) {
      expect(parsePort(bad)).toBeNull();
    }
  });
});

describe("a row", () => {
  it("says whether its forward is open, only kept, or none", () => {
    expect(forwardText(port(3000, { forwarded: true, host_port: 3001 }))).toBe(
      "Open on 127.0.0.1:3001",
    );
    expect(forwardText(port(3000, { pinned: true, host_port: 3000 }))).toBe(
      "Kept for 127.0.0.1:3000, not open",
    );
    expect(forwardText(port(3000))).toBe("Not forwarded");
    expect(hasForward(port(3000, { pinned: true }))).toBe(true);
    expect(hasForward(port(3000, { forwarded: true }))).toBe(true);
    expect(hasForward(port(3000))).toBe(false);
  });
});

describe("events", () => {
  it("tells the list changed for one workspace, by either event", () => {
    for (const type of [
      "workspace_ports_changed",
      "workspace_port_forward_failed",
    ]) {
      expect(isPortsEvent({ type, workspace: "web" }, "web")).toBe(true);
      expect(isPortsEvent({ type, workspace: "docs" }, "web")).toBe(false);
    }
    expect(isPortsEvent({ type: "rules_changed" }, "web")).toBe(false);
    expect(isPortsEvent(undefined, "web")).toBe(false);
  });

  it("reads a failed forward only when every field is there", () => {
    const event = {
      type: "workspace_port_forward_failed",
      workspace: "web",
      guest_port: 3000,
      host_port: 3000,
      reason: "port 3000 is in use",
    };
    expect(asPortForwardFailed(event)).toEqual(event);
    expect(asPortForwardFailed({ ...event, host_port: "x" })).toBeNull();
    expect(asPortForwardFailed({ ...event, guest_port: undefined })).toBeNull();
    expect(asPortForwardFailed({ ...event, reason: 1 })).toBeNull();
    expect(asPortForwardFailed({ ...event, workspace: 1 })).toBeNull();
    expect(asPortForwardFailed({ ...event, type: "x" })).toBeNull();
    expect(asPortForwardFailed(null)).toBeNull();
  });
});
