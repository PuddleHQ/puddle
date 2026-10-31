// SPDX-License-Identifier: GPL-3.0-or-later
// What the Identities screens know about identities and their credentials, as pure functions:
// how a source is worded, what a credential covers, what the last check of it came to, and the
// checks a form makes before it asks. Nothing here holds a secret; a credential is a reference.
import type { components } from "#lib/api/schema.d.ts";
import { relativeTime } from "#lib/format/relative-time.ts";

export type Identity = components["schemas"]["IdentityView"];
export type GitDefaults = components["schemas"]["GitDefaultsView"];
export type Credential = components["schemas"]["IdentityCredentialView"];
/** A credential as a form edits it and a request carries it: no record of reads. */
export type CredentialDraft = components["schemas"]["IdentityCredential"];
export type LastCheck = components["schemas"]["LastCheck"];
export type Source = components["schemas"]["CredentialSource"];
export type Coverage = components["schemas"]["CredentialCoverage"];
export type FoundAccount = components["schemas"]["FoundAccount"];
export type FoundAccounts = components["schemas"]["FoundAccounts"];
export type IdentityRequest = components["schemas"]["IdentityRequest"];
export type AccountProfile = components["schemas"]["AccountProfile"];
export type CheckResult = components["schemas"]["CheckResult"];

/**
 * One line naming a source, as the host words it (`SourceSpec::describe`): the sign-in notice
 * carries this text, and a credential is matched to it by this text.
 */
export function describeSource(source: Source): string {
  switch (source.kind) {
    case "gh":
      return `gh account ${source.account} on ${source.host}`;
    case "git_credential":
      return source.username === null
        ? `Git credential for https://${source.host}/${source.path}`
        : `Git credential for https://${source.host}/${source.path} (account ${source.username})`;
    case "stored":
      return `stored token ${source.id} for ${source.host}`;
  }
}

/** How a source is shown in a chip or a row. */
export function sourceLabel(source: Source): string {
  switch (source.kind) {
    case "gh":
      return `gh · ${source.account}`;
    case "git_credential":
      return `Git Credential Manager · ${source.host}/${source.path}`;
    case "stored":
      return source.org === null
        ? "Pasted token"
        : `Pasted token · ${source.org}`;
  }
}

/** What a credential covers on its host, in words. */
export function coverageText(covers: Coverage, host: string): string {
  const rest = `the rest of ${host}`;
  if (covers.owners.length === 0) return covers.rest_of_host ? rest : "nothing";
  const owners = covers.owners.join(", ");
  return covers.rest_of_host ? `${owners} and ${rest}` : owners;
}

/** The credential as a chip: `gh · tijs-work · github.com: acme, acme-labs`. */
export function credentialChip(credential: CredentialDraft): string {
  return `${sourceLabel(credential.source)} · ${credential.host}: ${coverageText(credential.covers, credential.host)}`;
}

/** When a credential was last read and what asked for it, as the host recorded it. */
export interface Seen {
  at: number;
  by: LastCheck["by"];
}

/** What the page knows about a credential: its last read (the host keeps it), or a test in flight. */
export type Check =
  | { state: "untested" }
  | { state: "checking" }
  | { state: "ok"; seen?: Seen }
  | { state: "problem"; message: string; needsSignIn: boolean; seen?: Seen };

export const UNTESTED: Check = { state: "untested" };

/** The key a credential's check is kept under. */
export function credentialKey(credential: CredentialDraft): string {
  return `${credential.host}|${describeSource(credential.source)}`;
}

export function checkOf(result: CheckResult): Check {
  return result.readable
    ? { state: "ok" }
    : {
        state: "problem",
        message: result.problem ?? "Puddle can't read it",
        needsSignIn: result.needs_sign_in,
      };
}

/** The check the host recorded for a credential's last read, `UNTESTED` when it was never read. */
export function checkFromLast(last: LastCheck | null): Check {
  if (last === null) return UNTESTED;
  const seen: Seen = { at: last.checked_at, by: last.by };
  return last.readable
    ? { state: "ok", seen }
    : {
        state: "problem",
        message: last.problem ?? "Puddle can't read it",
        needsSignIn: last.needs_sign_in,
        seen,
      };
}

const BY: Record<LastCheck["by"], string> = {
  injection: "a workspace's request",
  repository_list: "a repository list",
  test: "a test",
};

/** "Checked 4 minutes ago by a workspace's request"; empty when the check has no record. */
export function seenText(check: Check, now: number): string {
  if (check.state !== "ok" && check.state !== "problem") return "";
  if (!check.seen) return "";
  return `Checked ${relativeTime(check.seen.at, now)} by ${BY[check.seen.by]}`;
}

export function statusWord(check: Check): string {
  switch (check.state) {
    case "untested":
      return "Not tested";
    case "checking":
      return "Checking…";
    case "ok":
      return "OK";
    case "problem":
      return check.needsSignIn ? "Sign in needed" : "Problem";
  }
}

/** The status of an identity: the worst of its credentials, `untested` when it has none. */
export function identityStatus(checks: readonly Check[]): Check {
  const rank = (c: Check) =>
    c.state === "problem"
      ? c.needsSignIn
        ? 3
        : 4
      : c.state === "checking"
        ? 2
        : c.state === "untested"
          ? 1
          : 0;
  const worst = [...checks].sort((a, b) => rank(b) - rank(a))[0];
  return worst ?? UNTESTED;
}

/** The source a found account becomes, or `null` when it can't be used (an Azure DevOps entry with no organisation). */
export function sourceOf(found: FoundAccount): Source | null {
  switch (found.via) {
    case "gh":
      return { kind: "gh", host: found.host, account: found.account };
    case "gcm_github":
      return {
        kind: "git_credential",
        host: found.host,
        path: found.account,
        username: found.account,
      };
    case "gcm_azure_repos":
      return found.org === null
        ? null
        : {
            kind: "git_credential",
            host: found.host,
            path: found.org,
            username: null,
          };
  }
}

/** What a found account covers until the user says otherwise. */
export function coverageOf(found: FoundAccount): Coverage {
  return found.via === "gcm_azure_repos" && found.org !== null
    ? { owners: [found.org.toLowerCase()], rest_of_host: false }
    : { owners: [], rest_of_host: true };
}

const VIA: Record<FoundAccount["via"], string> = {
  gh: "GitHub CLI",
  gcm_github: "Git Credential Manager",
  gcm_azure_repos: "Git Credential Manager",
};

/** A found account in a list: `tijs-work on github.com (GitHub CLI)`. */
export function foundLabel(found: FoundAccount): string {
  const where = found.org === null ? found.host : `${found.host}/${found.org}`;
  return `${found.account} on ${where} (${VIA[found.via]})`;
}

const OWNER = /^[A-Za-z0-9][A-Za-z0-9._-]*$/;

/** Owners typed as `acme, acme-labs`: the list, or what is wrong. */
export function parseOwners(
  text: string,
): { ok: true; owners: string[] } | { ok: false; message: string } {
  const owners = text
    .split(/[\s,]+/)
    .map((o) => o.trim())
    .filter((o) => o !== "");
  const bad = owners.find((o) => !OWNER.test(o) || o.length > 100);
  if (bad !== undefined) {
    return {
      ok: false,
      message: `"${bad}" is not a user or organisation name: use letters, digits, dots, hyphens and underscores.`,
    };
  }
  const seen = [...new Set(owners.map((o) => o.toLowerCase()))];
  return { ok: true, owners: seen };
}

export function coverageProblem(covers: Coverage): string | null {
  return covers.owners.length === 0 && !covers.rest_of_host
    ? "Name an owner or organisation, or tick the rest of the host."
    : null;
}

/** The author as typed in the dialog. */
export interface Author {
  name: string;
  email: string;
}

/** Whether a profile says anything that fits the author fields. */
export function hasAuthor(profile: AccountProfile): boolean {
  return (
    (profile.name ?? "").trim() !== "" || (profile.email ?? "").trim() !== ""
  );
}

/**
 * The author after taking a profile's name and address. Without `force` only an empty field is
 * filled, so text the user typed is never replaced; with it (the user pressed the button) both
 * fields take what the host said, and a field the host left out keeps its text.
 */
export function authorFill(
  current: Author,
  profile: AccountProfile,
  force: boolean,
): Author {
  const name = (profile.name ?? "").trim();
  const email = (profile.email ?? "").trim();
  const take = (now: string, offered: string) =>
    offered !== "" && (force || now.trim() === "") ? offered : now;
  return {
    name: take(current.name, name),
    email: take(current.email, email),
  };
}

/** Whether pressing the button would change the author. */
export function authorDiffers(
  current: Author,
  profile: AccountProfile,
): boolean {
  const next = authorFill(current, profile, true);
  return next.name !== current.name || next.email !== current.email;
}

/** Whether an owner is already in the owners text (names compare without case). */
export function hasOwner(text: string, owner: string): boolean {
  const parsed = parseOwners(text);
  const wanted = owner.toLowerCase();
  return parsed.ok
    ? parsed.owners.includes(wanted)
    : text
        .toLowerCase()
        .split(/[\s,]+/)
        .includes(wanted);
}

/** The owners text with `owner` added, or taken out when it is there already. */
export function toggleOwner(text: string, owner: string): string {
  const wanted = owner.toLowerCase();
  const words = text.split(/[\s,]+/).filter((w) => w !== "");
  if (hasOwner(text, owner)) {
    return words.filter((w) => w.toLowerCase() !== wanted).join(", ");
  }
  return [...words, owner].join(", ");
}

export const MAX_LABEL = 64;

/** The reason a label can't be used, or `null`. `others` are the other identities' labels. */
export function labelProblem(
  label: string,
  others: readonly string[],
): string | null {
  const text = label.trim();
  if (text === "") return "Give the identity a name.";
  if (text.length > MAX_LABEL)
    return `Use at most ${MAX_LABEL} characters for the name.`;
  if (others.some((o) => o.trim().toLowerCase() === text.toLowerCase()))
    return `Another identity is already called ${text}.`;
  return null;
}

export function authorProblem(name: string, email: string): string | null {
  if (name.trim() === "") return "Enter the name Git writes into commits.";
  const mail = email.trim();
  if (mail === "") return "Enter the email Git writes into commits.";
  if (!/^[^\s@]+@[^\s@]+$/.test(mail))
    return "The email needs an @ and no spaces.";
  return null;
}

/** The first thing wrong with an identity's name and author, and the field it is about. */
export function identityProblem(
  form: { label: string; name: string; email: string },
  others: readonly string[],
): { field: "label" | "name" | "email"; message: string } | null {
  const label = labelProblem(form.label, others);
  if (label) return { field: "label", message: label };
  if (form.name.trim() === "")
    return {
      field: "name",
      message: "Enter the name Git writes into commits.",
    };
  const author = authorProblem(form.name, form.email);
  return author ? { field: "email", message: author } : null;
}

export function hostProblem(host: string): string | null {
  const text = host.trim();
  if (text === "") return "Enter the Git host, for example github.com.";
  if (
    !/^[A-Za-z0-9]([A-Za-z0-9.-]*[A-Za-z0-9])?$/.test(text) ||
    !text.includes(".")
  )
    return "That is not a host name (for example github.com).";
  return null;
}

export function orgProblem(org: string): string | null {
  return OWNER.test(org.trim()) && org.trim().length <= 100
    ? null
    : "Enter the organisation the token belongs to.";
}

export function tokenProblem(token: string): string | null {
  const text = token.trim();
  if (text === "") return "Paste the token.";
  if (/\s/.test(text)) return "A token has no spaces or line breaks.";
  return null;
}

/** Azure DevOps tokens belong to one organisation (the host's `org`); other hosts' do not. */
export function needsOrg(host: string): boolean {
  return host.trim().toLowerCase() === "dev.azure.com";
}

/** The identity among `identities` that covers `owner` on `host`: an exact owner beats the rest of the host. */
export function covering(
  identities: readonly Identity[],
  host: string,
  owner: string,
): Identity | null {
  const h = host.toLowerCase();
  const o = owner.toLowerCase();
  const on = identities.flatMap((i) =>
    i.credentials
      .filter((c) => c.host.toLowerCase() === h)
      .map((c) => ({ identity: i, covers: c.covers })),
  );
  const exact = on.find((c) => c.covers.owners.includes(o));
  const rest = on.find((c) => c.covers.rest_of_host);
  return (exact ?? rest)?.identity ?? null;
}

/** `["a","b","c"]` with `b` moved up: `["b","a","c"]`; unchanged at the ends or when absent. */
export function moved<T>(list: readonly T[], item: T, by: -1 | 1): T[] {
  const at = list.indexOf(item);
  const to = at + by;
  const next = [...list];
  if (at < 0 || to < 0 || to >= list.length) return next;
  next.splice(at, 1);
  next.splice(to, 0, item);
  return next;
}

/** What the identity is used by, for a row. */
export function usedBy(identity: Identity): string {
  const n = identity.workspaces.length;
  return n === 0
    ? "Not used yet"
    : `Used by ${n} workspace${n === 1 ? "" : "s"}`;
}

/** The request that saves `identity` as it is, with one credential list replaced. */
export function requestOf(
  identity: Identity,
  credentials: readonly CredentialDraft[] = identity.credentials,
): IdentityRequest {
  return {
    label: identity.label,
    author: { ...identity.author },
    credentials: credentials.map(({ host, source, covers }) => ({
      host,
      source,
      covers,
    })),
  };
}
