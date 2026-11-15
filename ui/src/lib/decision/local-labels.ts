// SPDX-License-Identifier: AGPL-3.0-or-later
// The words for the local-destination toggles (R-14) and for what put an address on the company
// network. The service decides which toggle blocks a request (`blocked_by` on the request); the
// UI only names it. The record types are keyed by the generated schema's enum, so a category the
// service adds fails the type check here until it has a label.
import type { components } from "#lib/api/schema.d.ts";

export type LocalCategory = components["schemas"]["LocalCategory"];

export const LOCAL_LABELS: Record<LocalCategory, string> = {
  loopback: "This computer (loopback)",
  private: "Private networks",
  link_local: "Link-local",
  metadata: "Cloud metadata",
  special: "Other special ranges",
  company_network: "Company network",
};

/** What put an address on the company network, in words (the wire carries the code). */
export const COMPANY_SIGNAL_TEXT: Record<string, string> = {
  pac_direct: "your company's proxy script sends it straight out",
  dns_suffix: "its name ends in one of your company's DNS suffixes",
  private_access_cgnat:
    "a company private-access client (such as Zscaler, Netskope, WARP or Tailscale) is active and this address is in its range",
  vpn_route: "this computer reaches it through a VPN connection",
};

/** The reason, or `null` for a signal this version doesn't know (shown as no reason at all). */
export function companySignalText(signal: string | null): string | null {
  return signal === null ? null : (COMPANY_SIGNAL_TEXT[signal] ?? null);
}
