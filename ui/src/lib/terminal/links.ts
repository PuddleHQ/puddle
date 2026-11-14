// SPDX-License-Identifier: AGPL-3.0-or-later
// What a link in a terminal's output may do: `http` and `https` addresses open in the
// default browser; an address on this computer opens only when the workspace forwards that port
// to the same port; everything else opens nothing.
import type { PortEntry } from "#lib/workspaces/ports.ts";

export type LinkVerdict =
  /** Opens `url` (the address as the browser reads it, never the text the program showed). */
  | { kind: "open"; url: string }
  /** A link to this computer's port `port`, which the workspace does not forward to the same port. */
  | { kind: "forward"; port: number; url: string }
  /** Not an `http` or `https` address. */
  | { kind: "ignore" };

/**
 * Whether an address names this computer: `localhost` and its subdomains, the loopback
 * addresses 127.0.0.0/8 and [::1], the unspecified addresses 0.0.0.0 and [::], and the
 * IPv4-mapped form of any of those. The URL parser has already normalised the spelling
 * (`0x7f000001` is `127.0.0.1`, `[::ffff:127.0.0.1]` is `[::ffff:7f00:1]`).
 */
function isLocalHost(hostname: string): boolean {
  const host = hostname.toLowerCase().replace(/\.$/, "");
  if (host === "localhost" || host.endsWith(".localhost")) return true;
  if (/^127(\.\d{1,3}){3}$/.test(host) || host === "0.0.0.0") return true;
  if (host === "[::1]" || host === "[::]") return true;
  const mapped = /^\[::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})\]$/.exec(host);
  if (mapped === null) return false;
  const high = parseInt(mapped[1], 16);
  const low = parseInt(mapped[2], 16);
  return high >> 8 === 127 || (high === 0 && low === 0);
}

/** The port an address names, with the scheme's default when it names none. */
function portOf(url: URL): number {
  if (url.port !== "") return Number(url.port);
  return url.protocol === "https:" ? 443 : 80;
}

export function judgeLink(
  text: string,
  forwards: readonly PortEntry[],
): LinkVerdict {
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return { kind: "ignore" };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") {
    return { kind: "ignore" };
  }
  if (!isLocalHost(url.hostname)) {
    return { kind: "open", url: url.href };
  }
  const port = portOf(url);
  const same = forwards.some(
    (f) => f.forwarded && f.guest_port === port && f.host_port === port,
  );
  return same
    ? { kind: "open", url: url.href }
    : { kind: "forward", port, url: url.href };
}

/** What hovering a link says: where it goes, or what to do first. */
export function hoverText(verdict: LinkVerdict): string | null {
  switch (verdict.kind) {
    case "open":
      return verdict.url;
    case "forward":
      return `${verdict.url} (forward port ${verdict.port} on the Ports tab to open it)`;
    case "ignore":
      return null;
  }
}
