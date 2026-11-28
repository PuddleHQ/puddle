// SPDX-License-Identifier: AGPL-3.0-or-later
// One reading of a host name for every place that shows one (Inbox, Rules, Activity, the strip):
// a `*.` wildcard mark, the part before the registrable domain, the registrable domain itself
// (the part a person recognises, drawn bold) and the port (greyed when it is the usual 443).
import { isIpLiteral } from "#lib/decision/model.ts";

export interface HostParts {
  /** The pattern began with `*.`: the host and every name under it. */
  wildcard: boolean;
  /** Labels before the registrable domain, with their trailing dot: `api.`. */
  lead: string;
  /** The registrable domain, or the whole host when no split is known (an IP literal). */
  main: string;
  /** The port as shown, or `null` when there is none. */
  port: number | null;
  /** The port is 443, the one every ordinary request uses: drawn grey. */
  usualPort: boolean;
}

const SECOND_LEVEL = new Set(["co", "com", "org", "net", "gov", "ac", "edu"]);

/**
 * A guess at the registrable domain for the screens that are not told it (the service answers
 * with the real one, from the public suffix list, only for the Inbox). The last two labels, or
 * three under a two-letter country code with a common second level (`example.co.uk`).
 */
export function guessRegistrable(host: string): string {
  if (isIpLiteral(host)) return host;
  const labels = host.split(".");
  if (labels.length <= 2) return host;
  const tld = labels[labels.length - 1] ?? "";
  const second = labels[labels.length - 2] ?? "";
  const take = tld.length === 2 && SECOND_LEVEL.has(second) ? 3 : 2;
  return labels.slice(-take).join(".");
}

/**
 * Splits `value` (`*.api.example.com`, `example.com:8443`, `host`) for drawing. `port` wins over
 * one written in the value; `registrable` is the service's answer when the caller has it.
 */
export function hostParts(
  value: string,
  options: { port?: number | null; registrable?: string } = {},
): HostParts {
  let rest = value;
  const wildcard = rest.startsWith("*.");
  if (wildcard) rest = rest.slice(2);
  else if (rest.startsWith(".")) rest = rest.slice(1);
  let port = options.port ?? null;
  const written = /^([^:]+):(\d{1,5})$/.exec(rest);
  if (written) {
    rest = written[1] ?? rest;
    port ??= Number(written[2]);
  }
  // Port 0 is the SRV lookup (no port known yet): nothing to show.
  if (port === 0) port = null;
  const registrable = options.registrable ?? guessRegistrable(rest);
  const split =
    registrable !== rest &&
    !isIpLiteral(rest) &&
    rest.endsWith(`.${registrable}`);
  return {
    wildcard,
    lead: split ? rest.slice(0, rest.length - registrable.length) : "",
    main: split ? registrable : rest,
    port,
    usualPort: port === 443,
  };
}
