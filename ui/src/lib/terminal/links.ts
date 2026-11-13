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

const LOCAL_HOSTS = new Set(["localhost", "127.0.0.1", "[::1]"]);

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
  if (!LOCAL_HOSTS.has(url.hostname.toLowerCase())) {
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
