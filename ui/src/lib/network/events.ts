// SPDX-License-Identifier: GPL-3.0-or-later
// The one stream event the network-health screen acts on.
import type { components } from "#lib/api/schema.d.ts";

export type NetworkChanged = Extract<
  components["schemas"]["Event"],
  { type: "network_changed" }
>;

export type ProxySignInFailed = Extract<
  components["schemas"]["Event"],
  { type: "proxy_sign_in_failed" }
>;

/** Whether `raw` is a `proxy_sign_in_failed` event that names its proxy. */
export function isProxySignInFailed(raw: unknown): raw is ProxySignInFailed {
  return (
    typeof raw === "object" &&
    raw !== null &&
    (raw as Record<string, unknown>)["type"] === "proxy_sign_in_failed" &&
    typeof (raw as Record<string, unknown>)["proxy"] === "string"
  );
}

/** Whether `raw` is a `network_changed` event with a numeric epoch. */
export function isNetworkChanged(raw: unknown): raw is NetworkChanged {
  return (
    typeof raw === "object" &&
    raw !== null &&
    (raw as Record<string, unknown>)["type"] === "network_changed" &&
    typeof (raw as Record<string, unknown>)["epoch"] === "number"
  );
}
