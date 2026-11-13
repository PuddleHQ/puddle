// SPDX-License-Identifier: GPL-3.0-or-later
// What the environment screens know about variables and secrets: what a name and a host list look
// like before they are sent (the service has the last word and its reason is shown as it is), and
// the sentences the rows and the forms use.
import type { components } from "#lib/api/schema.d.ts";

export type EnvVariable = components["schemas"]["EnvVariable"];
export type EnvSet = components["schemas"]["EnvSetRequest"];
export type SecretPlace = components["schemas"]["SecretPlace"];

/** Where the secret form says a secret is swapped in. */
export type PlaceChoice = SecretPlace["in"];

const NAME = /^[A-Za-z_][A-Za-z0-9_]*$/;

/** What is wrong with a name before it is sent, or `null`. */
export function nameProblem(name: string): string | null {
  if (name === "") return "Give the variable a name.";
  if (!NAME.test(name)) {
    return "A name has letters, digits and _, and does not start with a digit.";
  }
  return null;
}

/** The hosts typed in a box: split on commas, spaces and lines, lower case, each once. */
export function parseHosts(text: string): string[] {
  const seen = new Set<string>();
  for (const part of text.split(/[\s,]+/)) {
    const host = part.trim().toLowerCase();
    if (host !== "") seen.add(host);
  }
  return [...seen];
}

/** The hosts of a secret, one per line, as the box shows them for editing. */
export function hostsText(hosts: readonly string[]): string {
  return hosts.join("\n");
}

/** "api.example.com" or "api.example.com, *.example.org and 3 more", for a row. */
export function hostsSummary(hosts: readonly string[]): string {
  if (hosts.length <= 2) return hosts.join(" and ");
  return `${hosts.slice(0, 2).join(", ")} and ${hosts.length - 2} more`;
}

/** A query parameter name the service takes: 1 to 64 visible characters, none of & = # % +. */
const QUERY_NAME = /^[!-~]{1,64}$/;
const QUERY_NAME_REFUSED = /[&=#%+]/;

/**
 * The place the form's choice comes to, or what is wrong with it. `queryName` counts only for a
 * query parameter.
 */
export function placeOf(
  choice: PlaceChoice,
  queryName: string,
): SecretPlace | { problem: string } {
  if (choice !== "query") return { in: choice };
  const name = queryName.trim();
  if (name === "") {
    return { problem: "Name the query parameter the tool puts the secret in." };
  }
  if (!QUERY_NAME.test(name) || QUERY_NAME_REFUSED.test(name)) {
    return {
      problem:
        "A query parameter name is up to 64 visible characters, without spaces or & = # % +.",
    };
  }
  return { in: "query", name };
}

/** " in the query parameter appid", " in the path", or "" for header values, after a row's hosts. */
export function placeSummary(place: SecretPlace | null): string {
  if (place?.in === "query") return ` in the query parameter ${place.name}`;
  if (place?.in === "path") return " in the path";
  return "";
}

/**
 * The variables a workspace's table shows: its own first, then the global ones it inherits,
 * each by name. A global one the workspace's own hides keeps its row, marked.
 */
export function ordered(list: readonly EnvVariable[]): EnvVariable[] {
  const rank = (v: EnvVariable) => (v.scope === "workspace" ? 0 : 1);
  return [...list].sort(
    (a, b) => rank(a) - rank(b) || a.name.localeCompare(b.name),
  );
}

/** How many plain variables and secrets a list holds. */
export function counts(list: readonly EnvVariable[]): {
  variables: number;
  secrets: number;
} {
  const secrets = list.filter((v) => v.kind === "secret").length;
  return { variables: list.length - secrets, secrets };
}

/** "1 variable", "2 secrets": the count with its noun. */
export const plural = (n: number, noun: string): string =>
  `${n} ${noun}${n === 1 ? "" : "s"}`;

export const PLAIN_AT_NEXT_START =
  "Plain variables apply at the next start of a running workspace; its home folder and installed packages stay. Programs already running keep the old values.";
export const SECRET_AT_ONCE =
  "A secret's hosts and value apply at once, with no restart.";
export const SECRET_NEVER_IN =
  "A secret's value is kept in this computer's credential store. The workspace gets a stand-in text instead, and Puddle swaps in the real value on requests to the hosts you name, so the value never enters the workspace.";
export const PLACE_HINT =
  "Most tools send a key in a header. Pick the query or the path only for an API that takes its key nowhere else, such as ?appid= for OpenWeatherMap or /bot<token>/ for a Telegram bot. The value is swapped there and nowhere else. In a URL the real value also lands in that host's own logs, and an error page that repeats the URL hands it back to the workspace.";
export const SWAP_LIMITS =
  "How the swap works: Puddle finds the stand-in in requests to the hosts you name and puts the real value in its place. It cannot cover a key the tool uses to sign requests itself (AWS access keys with SigV4, HMAC secrets, JWT signing keys), a secret for something other than HTTPS (databases, SMTP, SSH) or a key used only inside the workspace: the tool gets the stand-in there and fails. Prefer a short-lived credential for such a tool; giving the value as a plain variable puts the real value inside the workspace, where anything running there can read it.";
export const DECRYPT_HOSTS =
  "Puddle will decrypt connections to each host you name here, to swap the value in. It decrypts port 443 only.";
