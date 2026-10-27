// SPDX-License-Identifier: GPL-3.0-or-later
// What the Ports tab knows about ports: what a typed port is, how a row reads, and the stream
// events that change the list.
import type { components } from "#lib/api/schema.d.ts";

export type PortEntry = components["schemas"]["PortEntry"];

/** A port typed in a box: a whole number from 1 to 65535, else `null`. */
export function parsePort(text: string): number | null {
  const trimmed = text.trim();
  if (!/^\d{1,5}$/.test(trimmed)) return null;
  const port = Number(trimmed);
  return port >= 1 && port <= 65535 ? port : null;
}

/** What a row says about its forward, in words. */
export function forwardText(entry: PortEntry): string {
  if (entry.forwarded && entry.host_port !== null) {
    return `Open on 127.0.0.1:${entry.host_port}`;
  }
  if (entry.pinned && entry.host_port !== null) {
    return `Kept for 127.0.0.1:${entry.host_port}, not open`;
  }
  return "Not forwarded";
}

/** Whether a row has a forward or a pin to stop. */
export const hasForward = (entry: PortEntry): boolean =>
  entry.forwarded || entry.pinned;

/** Whether `event` is one that changes what the Ports tab of `name` lists. */
export function isPortsEvent(event: unknown, name: string): boolean {
  if (typeof event !== "object" || event === null) return false;
  const { type, workspace } = event as { type?: unknown; workspace?: unknown };
  return (
    workspace === name &&
    (type === "workspace_ports_changed" ||
      type === "workspace_port_forward_failed")
  );
}

export type PortForwardFailed = Extract<
  components["schemas"]["Event"],
  { type: "workspace_port_forward_failed" }
>;

/** A `workspace_port_forward_failed` event with the fields it must have, or `null`. */
export function asPortForwardFailed(event: unknown): PortForwardFailed | null {
  if (typeof event !== "object" || event === null) return null;
  const e = event as Record<string, unknown>;
  return e["type"] === "workspace_port_forward_failed" &&
    typeof e["workspace"] === "string" &&
    typeof e["reason"] === "string" &&
    typeof e["guest_port"] === "number" &&
    typeof e["host_port"] === "number"
    ? (event as PortForwardFailed)
    : null;
}
