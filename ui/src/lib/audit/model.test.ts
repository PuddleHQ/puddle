// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { connection } from "#lib/testing/fake-audit.ts";
import {
  DEFAULT_RANGE,
  NO_FILTER,
  OUTCOMES,
  TYPES,
  bytesLabel,
  describe as row,
  exportName,
  fromSearch,
  splitDetail,
  isFiltered,
  isNarrowed,
  jsonLine,
  outcomeLabel,
  rawJson,
  toQuery,
  toSearch,
  typeLabel,
  type AuditRecord,
  type Filter,
} from "./model.ts";

const rule = {
  id: 7,
  pattern: ".example.com",
  pattern_kind: "suffix",
  scope: "workspace",
  workspace_id: "shop",
  effect: "allow",
  expires_at: null,
  created_at: 1,
  created_by: "ui",
  source_pending_id: null,
};
const pending = {
  id: 3,
  workspace_id: "shop",
  host: "evil.example.org",
  port: 8443,
  first_seen: 1,
  last_seen: 2,
  attempts: 1,
  state: "requested",
  decided_at: null,
  decided_by: null,
  rule_id: null,
};

describe("filters and the API query", () => {
  it("sends nothing for the defaults but the range", () => {
    expect(toQuery(NO_FILTER, 100_000_000)).toEqual({
      from: 100_000_000 - 86_400_000,
    });
    expect(toQuery({ ...NO_FILTER, range: "all" }, 5)).toEqual({});
  });
  it("sends every set filter, trimmed, and measures the range from now", () => {
    const filter: Filter = {
      workspace: "shop",
      type: "connection",
      outcome: "deny",
      category: "company_network",
      q: "  github  ",
      range: "1h",
    };
    expect(toQuery(filter, 10_000_000)).toEqual({
      workspace: "shop",
      type: "connection",
      outcome: "deny",
      category: "company_network",
      q: "github",
      from: 10_000_000 - 3_600_000,
    });
  });
  it("knows when a filter is not the default view", () => {
    expect(isFiltered(NO_FILTER)).toBe(false);
    expect(isFiltered({ ...NO_FILTER, q: "  " })).toBe(false);
    for (const change of [
      { workspace: "a" },
      { type: "connection" },
      { outcome: "allow" },
      { category: "company_network" },
      { q: "x" },
      { range: "7d" },
    ] as Partial<Filter>[]) {
      expect(isFiltered({ ...NO_FILTER, ...change })).toBe(true);
    }
  });
});

describe("isNarrowed", () => {
  it("is false only when nothing is left out of the log", () => {
    expect(isNarrowed({ ...NO_FILTER, range: "all" })).toBe(false);
    expect(isNarrowed(NO_FILTER)).toBe(true);
    expect(isNarrowed({ ...NO_FILTER, range: "all", q: "  " })).toBe(false);
    for (const change of [
      { workspace: "a" },
      { type: "connection" },
      { outcome: "allow" },
      { category: "company_network" },
      { q: "x" },
    ] as Partial<Filter>[]) {
      expect(isNarrowed({ ...NO_FILTER, range: "all", ...change })).toBe(true);
    }
  });
});

describe("the page address", () => {
  it("is empty for the default view", () => {
    expect(toSearch(NO_FILTER)).toBe("");
    expect(fromSearch("")).toEqual(NO_FILTER);
  });
  it("keeps the search in `q` and still reads the `host` of an older address", () => {
    expect(toSearch({ ...NO_FILTER, q: " /v1/items " })).toBe(
      "?q=%2Fv1%2Fitems",
    );
    expect(fromSearch("?q=post").q).toBe("post");
    expect(fromSearch("?host=github.com").q).toBe("github.com");
    expect(fromSearch("?q=a&host=b").q).toBe("a");
  });
  it("round-trips every filter, whatever the host holds", () => {
    const filter: Filter = {
      workspace: "shop",
      type: "rule_created",
      outcome: "blocked",
      category: "company_network",
      q: "a&b=c d",
      range: "all",
    };
    expect(fromSearch(toSearch(filter))).toEqual(filter);
    for (const t of TYPES)
      expect(fromSearch(toSearch({ ...NO_FILTER, type: t.value })).type).toBe(
        t.value,
      );
    for (const o of OUTCOMES)
      expect(
        fromSearch(toSearch({ ...NO_FILTER, outcome: o.value })).outcome,
      ).toBe(o.value);
  });
  it("drops what it does not recognise instead of failing", () => {
    expect(fromSearch("?type=nope&outcome=x&range=forever&other=1")).toEqual({
      ...NO_FILTER,
      range: DEFAULT_RANGE,
    });
  });
});

describe("labels", () => {
  it("names every type and outcome, and passes an unknown one through", () => {
    expect(typeLabel("pending_created")).toBe("Request opened");
    expect(outcomeLabel("allow")).toBe("Allowed");
    expect(typeLabel("later" as never)).toBe("later");
    expect(outcomeLabel("later" as never)).toBe("later");
  });
  it("prints sizes in powers of 1024", () => {
    expect(bytesLabel(0)).toBe("0 B");
    expect(bytesLabel(1023)).toBe("1023 B");
    expect(bytesLabel(1536)).toBe("1.5 KB");
    expect(bytesLabel(20 * 1024)).toBe("20 KB");
    expect(bytesLabel(2 * 1024 * 1024)).toBe("2.0 MB");
    expect(bytesLabel(5 * 1024 ** 5)).toBe("5120 TB");
  });
});

describe("a record as a row", () => {
  it("shows a connection with its outcome, reason and size", () => {
    const view = row(
      connection(1, { bytes_up: 2048, bytes_down: 3 * 1024 * 1024 }).record,
    );
    expect(view).toMatchObject({
      workspace: "demo",
      type: "Connection",
      destination: "h1.example.com:443",
      outcome: { label: "Allowed", tone: "allow" },
    });
    expect(view.detail).toBe("rule 1 · 2.0 KB up, 3.0 MB down");
  });
  it("marks a company-network connection with a chip that says why, and names the toggle that refused it", () => {
    const view = row(
      connection(1, {
        decision: "blocked",
        reason: "toggle:company_network",
        rule_id: null,
        category: "company_network",
        signal: "dns_suffix",
      }).record,
    );
    expect(view.company).toEqual({
      label: "Company network",
      title: "Its name ends in one of the company's DNS suffixes.",
    });
    expect(view.detail).toBe(
      "because company network destinations are switched off",
    );
    expect(row(connection(2, {}).record).company).toBeUndefined();
  });
  it("explains a block, a waiting request and a request line", () => {
    const blocked = row(
      connection(1, {
        decision: "blocked",
        reason: "local_address",
        rule_id: null,
        method: "GET",
        path: "/x",
        injected: true,
      }).record,
    );
    expect(blocked.outcome).toEqual({ label: "Blocked", tone: "deny" });
    expect(blocked.detail).toBe(
      "GET /x · because a local address · credential added",
    );
    const unreadable = row(
      connection(3, {
        decision: "blocked",
        reason: "settings_unreadable",
        rule_id: null,
      }).record,
    );
    expect(unreadable.detail).toBe("because unreadable settings");
    const waiting = row(
      connection(2, { decision: "pending", reason: "no_rule", rule_id: null })
        .record,
    );
    expect(waiting.outcome).toEqual({ label: "Waiting", tone: "warn" });
    expect(waiting.detail).toBe("because no rule yet");
    const merged = row(
      connection(4, {
        decision: null,
        reason: "merged",
        rule_id: null,
        host: null,
        port: null,
        count: 4200,
      }).record,
    );
    expect(merged.detail).toBe(
      "because Puddle merged the oldest records written while requests waited, to keep the log small (their hosts are not kept) · 4200 records",
    );
    const odd = row(
      connection(3, {
        reason: "something_new",
        rule_id: null,
        method: "PUT",
        path: null,
      }).record,
    );
    expect(odd.detail).toBe("PUT · because something_new");
  });
  it("says when the tool in the workspace refused Puddle's certificate, beside the rule that allowed it", () => {
    const refused = row(
      connection(1, {
        decision: "allow",
        reason: "guest_tls_rejected",
        rule_id: 12,
      }).record,
    );
    expect(refused.outcome).toEqual({ label: "Allowed", tone: "allow" });
    expect(refused.detail).toBe(
      "rule 12 · the tool in the workspace didn't accept Puddle's certificate (it may keep its own list of trusted roots)",
    );
    const withoutRule = row(
      connection(2, { reason: "guest_tls_rejected", rule_id: null }).record,
    );
    expect(withoutRule.detail).toBe(
      "the tool in the workspace didn't accept Puddle's certificate (it may keep its own list of trusted roots)",
    );
  });
  it("says when Puddle could not make its certificate, beside the rule that allowed it", () => {
    const failed = row(
      connection(1, {
        decision: "allow",
        reason: "certificate_unavailable",
        rule_id: 12,
      }).record,
    );
    expect(failed.outcome).toEqual({ label: "Allowed", tone: "allow" });
    expect(failed.detail).toBe(
      "rule 12 · Puddle could not make its certificate for this host, so the connection failed on Puddle's side",
    );
  });
  it("says when an allowed connection could not reach its destination", () => {
    const refused = row(
      connection(1, { outcome: "upstream_refused", method: null }).record,
    );
    expect(refused.outcome).toEqual({ label: "Failed", tone: "warn" });
    expect(refused.detail).toBe("rule 1 · the company proxy refused it");
    expect(
      row(connection(2, { outcome: "connect_failed" }).record).detail,
    ).toBe("rule 1 · Puddle could not connect to it");
    expect(row(connection(3, { outcome: "timeout" }).record).detail).toBe(
      "rule 1 · it did not answer in time",
    );
    // Made, or never tried: a plain allow.
    for (const outcome of ["connected", null] as const) {
      const plain = row(connection(4, { outcome }).record);
      expect(plain.outcome).toEqual({ label: "Allowed", tone: "allow" });
      expect(plain.detail).toBe("rule 1");
    }
  });
  it("names the stand-ins that went out unchanged", () => {
    const view = row(
      connection(1, {
        unswapped_stand_ins: ["stand-in:secret:GH_TOKEN"],
        method: "GET",
        path: "/x",
      }).record,
    );
    expect(view.detail).toBe(
      "GET /x · rule 1 · stand-in for GH_TOKEN went out unchanged (check the secret's hosts, and that the tool sends it in a header, not in the URL)",
    );
    const several = row(
      connection(3, {
        unswapped_stand_ins: [
          "stand-in:secret:API_KEY",
          "stand-in:login:work-github",
          "odd",
        ],
      }).record,
    );
    expect(several.detail).toContain(
      "stand-in for API_KEY, login work-github, odd went out unchanged",
    );
    expect(row(connection(2, {}).record).detail).not.toContain("stand-in");
  });
  it("says an answer's real values were put back as stand-ins", () => {
    const view = row(
      connection(1, {
        scrubbed_stand_ins: ["stand-in:secret:GH_TOKEN", "stand-in:login:work"],
      }).record,
    );
    expect(view.detail).toContain(
      "the answer held the real value of GH_TOKEN, login work; the workspace got the stand-in back",
    );
    expect(row(connection(2, {}).record).detail).not.toContain(
      "the answer held",
    );
  });
  it("says a stale stand-in is no longer held, naming a secret but never a token", () => {
    const view = row(
      connection(1, {
        unswapped_stand_ins: [
          "stale-stand-in:secret:OLD_KEY",
          "stale-stand-in:github",
        ],
      }).record,
    );
    expect(view.detail).toContain(
      "OLD_KEY, a GitHub login looks like a stand-in this workspace no longer holds, so it went out unchanged",
    );
    expect(view.detail).not.toContain("went out unchanged (check");
  });
  it("says which kinds of credential the workspace sent that Puddle did not add, never a value", () => {
    const view = row(
      connection(1, {
        foreign_credentials: ["github_token", "git_authorization", "odd_kind"],
      }).record,
    );
    expect(view.detail).toContain(
      "the workspace sent a GitHub token, a login for a Git repository, odd_kind that Puddle did not add; it went out as it was",
    );
    expect(row(connection(2, {}).record).detail).not.toContain("did not add");
  });
  it("says which kinds of token the answer held that Puddle does not hold, never a value", () => {
    const view = row(
      connection(1, {
        answer_credentials: [
          "github_token",
          "jwt_with_refresh_token",
          "odd_kind",
        ],
      }).record,
    );
    expect(view.detail).toContain(
      "the answer held a GitHub token, a sign-in token with a refresh token, odd_kind that Puddle does not hold; the workspace got it as it was",
    );
    expect(row(connection(2, {}).record).detail).not.toContain("does not hold");
  });
  it("names the refused address of a mixed answer, apart from the one connected to", () => {
    const view = row(
      connection(1, {
        resolved_ip: "93.184.216.34",
        refused_address: "10.0.0.8",
        refused_reason: "local_address",
      }).record,
    );
    expect(view.detail).toBe(
      "rule 1 · the answer also held 10.0.0.8 (a local address), which Puddle did not use",
    );
    const raw = row(
      connection(2, {
        refused_address: "127.0.0.1",
        refused_reason: "a_later_reason",
      }).record,
    );
    expect(raw.detail).toContain("127.0.0.1 (a_later_reason)");
    const toggled = row(
      connection(4, {
        refused_address: "127.0.0.1",
        refused_reason: "toggle:loopback",
      }).record,
    );
    expect(toggled.detail).toContain(
      "127.0.0.1 (this computer's own addresses are switched off)",
    );
    expect(row(connection(3, {}).record).detail).not.toContain("answer");
  });
  it("names an address that a rule on the address denied", () => {
    const view = row(
      connection(1, {
        decision: "deny",
        resolved_ip: "10.0.0.9",
        refused_address: "10.0.0.9",
        refused_reason: "ip_rule",
      }).record,
    );
    expect(view.detail).toBe(
      "rule 1 · the answer held 10.0.0.9, which a rule on that address denies, so Puddle did not use it",
    );
  });
  it("shows Puddle's own connections as Puddle, with its own reason", () => {
    const view = row(
      connection(1, {
        workspace_id: null,
        origin: "puddle",
        reason: "puddle_request",
        rule_id: null,
        bytes_down: 2048,
      }).record,
    );
    expect(view.workspace).toBeNull();
    expect(view.detail).toBe(
      "because Puddle's own request · 0 B up, 2.0 KB down",
    );
  });
  it("words the reasons the proxy refuses a Git request or a request on its own", () => {
    const detail = (reason: string) =>
      row(connection(1, { reason, rule_id: null }).record).detail;
    expect(detail("push_denied")).toBe(
      'because the push is not on the workspace\'s push list yet (click "Allow push" in the notice, or turn off "Only push to listed repos" on its Git tab)',
    );
    for (const reason of [
      "push_denied",
      "pull_denied",
      "bad_git_path",
      "ssh_command_unsupported",
      "ssh_channel_refused",
      "credential_rejected",
      "credential_unavailable",
      "credential_ambiguous",
      "git_settings_unavailable",
      "body_too_large",
      "answer_encoding",
      "answer_withheld",
      "bad_request",
      "misdirected_host",
      "expectation_unsupported",
      "head_too_large",
      "request_timeout",
      "protocol_unsupported",
    ]) {
      expect(detail(reason), reason).not.toContain(reason);
    }
    expect(detail("ssh_command_unsupported")).toBe(
      "because Puddle serves only git-upload-pack and git-receive-pack over SSH, and this command was neither",
    );
    expect(detail("ssh_channel_refused")).toBe(
      "because the SSH session asked for something Puddle does not allow (a terminal, a subsystem, forwarding or a second command)",
    );
    expect(detail("bad_git_path")).toBe(
      "because the Git address is not one Puddle reads, or names the repository in more than one way (use owner/repository)",
    );
    expect(detail("a_code_from_a_newer_puddle")).toBe(
      "because a_code_from_a_newer_puddle",
    );
  });
  it.each([
    ["git-receive-pack", "/acme/app", "SSH: push to github.com/acme/app"],
    ["git-upload-pack", "/acme/app", "SSH: fetch from github.com/acme/app"],
    [
      "git-lfs-upload",
      "/acme/app",
      "SSH: Git LFS upload to github.com/acme/app",
    ],
    [
      "git-lfs-download",
      "/acme/app",
      "SSH: Git LFS download from github.com/acme/app",
    ],
  ])("words a %s record as Git over SSH", (method, path, words) => {
    const view = row(
      connection(1, {
        host: "github.com",
        port: 22,
        method,
        path,
        reason: "rule",
        rule_id: null,
        rule_set: "builtin:git",
        injected: true,
      }).record,
    );
    expect(view.destination).toBe("github.com:22");
    expect(view.detail).toBe(
      `${words} · rule set builtin:git · credential added`,
    );
  });
  it("words a refused SSH push with the push list's reason, and leaves HTTP alone", () => {
    const refused = row(
      connection(1, {
        host: "github.com",
        port: 22,
        method: "git-receive-pack",
        path: "/acme/other",
        decision: "blocked",
        reason: "push_denied",
        rule_id: null,
      }).record,
    );
    expect(refused.outcome).toEqual({ label: "Blocked", tone: "deny" });
    expect(refused.detail).toBe(
      'SSH: push to github.com/acme/other · because the push is not on the workspace\'s push list yet (click "Allow push" in the notice, or turn off "Only push to listed repos" on its Git tab)',
    );
    const http = row(
      connection(2, {
        host: "github.com",
        method: "POST",
        path: "/acme/other/git-receive-pack",
        decision: "blocked",
        reason: "push_denied",
        rule_id: null,
      }).record,
    );
    expect(http.detail).toMatch(/^POST \/acme\/other\/git-receive-pack · /);
  });
  it("words a command or a channel request Puddle refused over SSH", () => {
    const command = row(
      connection(1, {
        host: "github.com",
        port: 22,
        method: "ssh-command",
        path: "/",
        decision: "blocked",
        reason: "ssh_command_unsupported",
        rule_id: null,
      }).record,
    );
    expect(command.detail).toMatch(
      /^SSH: command Puddle does not serve · because Puddle serves only/,
    );
    const channel = row(
      connection(2, {
        host: "github.com",
        port: 22,
        method: "ssh-channel",
        path: "/",
        decision: "blocked",
        reason: "ssh_channel_refused",
        rule_id: null,
      }).record,
    );
    expect(channel.detail).toMatch(
      /^SSH: session request · because the SSH session asked/,
    );
  });
  it("shows a listing request of Puddle's that failed as failed, with why", () => {
    const view = row(
      connection(1, {
        workspace_id: null,
        origin: "puddle",
        host: "api.github.com",
        port: 443,
        reason: "puddle_request_failed:unreachable",
        rule_id: null,
      }).record,
    );
    expect(view.outcome).toEqual({ label: "Failed", tone: "warn" });
    expect(view.detail).toBe(
      "because Puddle's own request failed: the host could not be reached",
    );
    const unknown = row(
      connection(2, {
        workspace_id: null,
        origin: "puddle",
        reason: "puddle_request_failed:new_code",
        rule_id: null,
      }).record,
    );
    expect(unknown.detail).toContain("failed: new_code");
  });
  it("names the certificate-check exception by what it is", () => {
    const view = row(
      connection(1, {
        host: "pki.corp.example",
        port: 80,
        decision: "allow",
        reason: "pki_distribution_point",
        rule_id: null,
      }).record,
    );
    expect(view.detail).toContain(
      "because a certificate check address named in your company's root certificates",
    );
  });
  it("leaves the outcome and the address out when the record has none", () => {
    const view = row(
      connection(1, { decision: null, host: null, port: null }).record,
    );
    expect(view.outcome).toBeNull();
    expect(view.destination).toBeNull();
    expect(
      row(connection(1, { host: "a.test", port: null }).record).destination,
    ).toBe("a.test");
  });
  it("shows the three request records", () => {
    const created = row({
      type: "pending_created",
      ts: 1,
      pending,
    } as AuditRecord);
    expect(created).toMatchObject({
      workspace: "shop",
      destination: "evil.example.org:8443",
      outcome: { label: "Waiting" },
      detail: "",
    });
    expect(
      row({
        type: "pending_created",
        ts: 1,
        pending: { ...pending, attempts: 4 },
      } as AuditRecord).detail,
    ).toBe("4 attempts");
    const decided = (state: string, by: string | null, ruleId: number | null) =>
      row({
        type: "pending_decided",
        ts: 1,
        pending: { ...pending, state, decided_by: by, rule_id: ruleId },
      } as AuditRecord);
    expect(decided("allowed", "ui", 9)).toMatchObject({
      outcome: { label: "Allowed", tone: "allow" },
      detail: "by ui, rule 9",
    });
    expect(decided("denied", null, null)).toMatchObject({
      outcome: { label: "Denied", tone: "deny" },
      detail: "by unknown",
    });
    const expired = row({
      type: "pending_expired",
      ts: 1,
      pending,
      reason: "timeout",
    } as unknown as AuditRecord);
    expect(expired).toMatchObject({
      outcome: { label: "Closed", tone: "neutral" },
      detail: "closed: timeout",
    });
    for (const [reason, detail] of [
      ["workspace_deleted", "closed: its workspace was deleted"],
      ["stale", "closed: not repeated for 7 days (Puddle no longer does this)"],
    ] as const) {
      expect(
        row({
          type: "pending_expired",
          ts: 1,
          pending,
          reason,
        } as unknown as AuditRecord),
      ).toMatchObject({ detail });
    }
  });
  it("says a rule made by a workspace create was made by the create", () => {
    expect(
      row({
        ts: 1,
        type: "rule_created",
        rule: { ...rule, created_by: "workspace_create" },
      } as AuditRecord),
    ).toMatchObject({ detail: "allow for workspace shop, by the create" });
  });
  it("shows rule changes by pattern and scope, with no outcome", () => {
    const base = { ts: 1, rule };
    expect(row({ ...base, type: "rule_created" } as AuditRecord)).toMatchObject(
      {
        workspace: "shop",
        destination: ".example.com",
        outcome: null,
        detail: "allow for workspace shop, by ui",
      },
    );
    expect(
      row({
        ...base,
        type: "rule_expired",
        rule: { ...rule, scope: "global", workspace_id: null },
      } as AuditRecord),
    ).toMatchObject({
      workspace: null,
      detail: "allow for every workspace",
    });
    expect(
      row({
        ...base,
        type: "rule_updated",
        actor: "cli",
        before: rule,
      } as unknown as AuditRecord).detail,
    ).toBe("allow for workspace shop, by cli");
    expect(
      row({
        ...base,
        type: "rule_deleted",
        actor: "ui",
        reason: "user",
        rule: { ...rule, workspace_id: null },
      } as unknown as AuditRecord).detail,
    ).toBe("allow for workspace ?, by ui, user");
  });
  it("shows a refusal by a rule on method and path with its rule", () => {
    expect(
      row(
        connection(4, {
          decision: "deny",
          reason: "request_rule",
          rule_id: null,
          request_rule: 7,
          method: "DELETE",
          path: "/repos/a/b",
        }).record,
      ),
    ).toMatchObject({
      outcome: { label: "Denied", tone: "deny" },
      detail: "DELETE /repos/a/b · method and path rule 7",
      requestRule: 7,
    });
    expect(row(connection(6).record).requestRule).toBeUndefined();
    expect(
      row(
        connection(5, {
          decision: "blocked",
          reason: "request_rule_port",
          rule_id: null,
        }).record,
      ).detail,
    ).toContain("because the host has rules on method and path");
  });
  it("cuts a detail around its rule's name, or leaves it whole when the name is missing", () => {
    expect(splitDetail("GET /a · method and path rule 7 · x", 7)).toEqual({
      before: "GET /a · ",
      name: "method and path rule 7",
      after: " · x",
    });
    expect(splitDetail("GET /a", 7)).toEqual({
      before: "GET /a",
      name: "",
      after: "",
    });
  });
  it("shows a workspace operation with who asked, what it touched and how it ended", () => {
    const op = (fields: Record<string, unknown>) =>
      row({
        type: "workspace_op",
        ts: 5,
        workspace_id: "shop",
        outcome: "succeeded",
        actor: "api",
        detail: null,
        ...fields,
      } as AuditRecord);
    expect(op({ op: "create" })).toMatchObject({
      workspace: "shop",
      type: "Workspace operation",
      destination: null,
      outcome: { label: "Succeeded", tone: "allow" },
      detail: "Created, by api",
    });
    expect(
      op({
        op: "delete",
        detail: "deleted; its volume was already gone",
      }).detail,
    ).toBe("Deleted: deleted; its volume was already gone, by api");
    expect(op({ op: "rename", detail: "renamed to Acme shop" }).detail).toBe(
      "Renamed: renamed to Acme shop, by api",
    );
    // A failure says why and is marked as one.
    expect(
      op({ op: "start", outcome: "failed", detail: "no volume" }),
    ).toMatchObject({
      outcome: { label: "Failed", tone: "deny" },
      detail: "Start failed: no volume, by api",
    });
    expect(op({ op: "stop", outcome: "failed" }).detail).toBe(
      "Stop failed, by api",
    );
    expect(
      op({ op: "create", outcome: "failed", detail: "disk full" }).detail,
    ).toBe("Create failed: disk full, by api");
    // An identity change belongs to no workspace.
    expect(
      op({
        op: "identity_created",
        workspace_id: null,
        detail: "Work",
      }),
    ).toMatchObject({
      workspace: null,
      detail: "Identity added: Work, by api",
    });
    for (const kind of [
      "identity_updated",
      "identity_deleted",
      "identity_default_set",
      "identities_changed",
      "identities_reordered",
      "repo_added",
      "repo_switched",
      "repo_removed",
      "root_disk_grown",
      "git_repaired",
    ]) {
      expect(op({ op: kind }).detail).toMatch(/^[A-Z][a-z ]+, by api$/);
    }
  });
  it("words a root disk grow with both sizes, and a failed one with its reason", () => {
    const op = (fields: Record<string, unknown>) =>
      row({
        type: "workspace_op",
        ts: 5,
        workspace_id: "shop",
        op: "root_disk_grown",
        outcome: "succeeded",
        actor: "api",
        detail: null,
        ...fields,
      } as AuditRecord);
    expect(
      op({
        detail: "root disk grown from 10240 MiB to 20480 MiB, while it ran",
      }),
    ).toMatchObject({
      outcome: { label: "Succeeded", tone: "allow" },
      detail:
        "Root disk made bigger: root disk grown from 10240 MiB to 20480 MiB, while it ran, by api",
    });
    expect(
      op({
        outcome: "failed",
        detail: "root disk not grown from 10240 MiB to 20480 MiB: no room",
      }),
    ).toMatchObject({
      outcome: { label: "Failed", tone: "deny" },
      detail:
        "Making the root disk bigger failed: root disk not grown from 10240 MiB to 20480 MiB: no room, by api",
    });
  });
  it("filters on the workspace operation type and its two outcomes", () => {
    const filter = {
      ...NO_FILTER,
      workspace: "shop",
      type: "workspace_op" as const,
      outcome: "failed" as const,
      range: "all" as const,
    };
    expect(toQuery(filter, 1)).toEqual({
      workspace: "shop",
      type: "workspace_op",
      outcome: "failed",
    });
    expect(fromSearch(toSearch(filter))).toEqual(filter);
    expect(outcomeLabel("succeeded")).toBe("Succeeded");
    expect(outcomeLabel("failed")).toBe("Failed");
    expect(typeLabel("workspace_op")).toBe("Workspace operation");
  });
  it("shows the records of rules on method and path", () => {
    const rule = {
      id: 7,
      scope: "workspace",
      workspace_id: "shop",
      host: "api.example.com",
      methods: ["DELETE", "PUT"],
      path: "/repos/**",
      created_at: 1,
      created_by: "api",
    };
    expect(
      row({ type: "request_rule_created", ts: 1, rule } as AuditRecord),
    ).toMatchObject({
      workspace: "shop",
      destination: "api.example.com",
      outcome: null,
      detail: "refuse DELETE, PUT /repos/** for workspace shop, by api",
    });
    expect(
      row({
        type: "request_rule_deleted",
        ts: 2,
        rule: { ...rule, scope: "global", workspace_id: null, methods: [] },
        reason: "user",
        actor: "ui",
      } as AuditRecord).detail,
    ).toBe("refuse any method /repos/** for every workspace, by ui, user");
  });
  it("shows the two records that belong to no host", () => {
    expect(
      row({
        type: "pending_suppressed",
        ts: 1,
        workspace_id: "shop",
        count: 40,
      } as AuditRecord),
    ).toMatchObject({
      workspace: "shop",
      destination: null,
      detail: "40 requests refused for volume (Puddle no longer does this)",
    });
    expect(
      row({
        type: "pending_suppressed",
        ts: 1,
        workspace_id: "shop",
        count: 1,
      } as AuditRecord).detail,
    ).toBe("1 request refused for volume (Puddle no longer does this)");
    expect(
      row({
        type: "audit_trimmed",
        ts: 1,
        deleted_records: 12,
        oldest_ts_kept: null,
      } as AuditRecord),
    ).toMatchObject({ workspace: null, detail: "12 older records deleted" });
  });
});

describe("records and states this build doesn't know", () => {
  it("describes an unknown record type by its name, so the row still reads", () => {
    const view = row({
      type: "quota_changed",
      ts: 9,
      workspace_id: "shop",
    } as unknown as AuditRecord);
    expect(view).toMatchObject({
      ts: 9,
      workspace: "shop",
      type: "quota_changed",
      outcome: null,
    });
    expect(view.detail).toContain('"quota_changed"');
    expect(row({} as unknown as AuditRecord)).toMatchObject({
      ts: 0,
      workspace: null,
      type: "unknown",
    });
  });

  it("shows a decision or pending state it has no word for as it came", () => {
    expect(
      row(connection(1, { decision: "quarantine" } as never).record).outcome,
    ).toEqual({ label: "quarantine", tone: "neutral" });
    expect(
      row({
        type: "pending_decided",
        ts: 1,
        pending: { ...pending, state: "snoozed" },
      } as unknown as AuditRecord).outcome,
    ).toEqual({ label: "snoozed", tone: "neutral" });
  });
});

describe("rule set records (R-43)", () => {
  const set = {
    id: 4,
    name: "Client X",
    description: "",
    created_at: 1,
    created_by: "ui",
  };
  it("shows a set made, renamed or deleted by name", () => {
    for (const type of [
      "rule_set_created",
      "rule_set_updated",
      "rule_set_deleted",
    ]) {
      expect(
        row({
          type,
          ts: 1,
          rule_set: set,
          before: set,
          actor: "api",
        } as AuditRecord),
      ).toMatchObject({
        workspace: null,
        destination: "Client X",
        detail: "user:4, by api",
      });
    }
  });
  it("shows a switch for every workspace or one", () => {
    const switched = (workspace: string | null, enabled: boolean | null) =>
      row({
        type: "rule_set_switched",
        ts: 1,
        set_id: "builtin:github",
        workspace_id: workspace,
        enabled,
        actor: "ui",
      } as AuditRecord);
    expect(switched(null, true)).toMatchObject({
      workspace: null,
      destination: "builtin:github",
      detail: "on for every workspace, by ui",
    });
    expect(switched("api", false).detail).toBe("off for workspace api, by ui");
    expect(switched("api", null).detail).toBe(
      "follows the next level for workspace api, by ui",
    );
  });
  it("shows what an update changed in a built-in set", () => {
    const changed = (added: string[], removed: string[]) =>
      row({
        type: "rule_set_changed",
        ts: 1,
        set_id: "builtin:github",
        added,
        removed,
      } as AuditRecord).detail;
    expect(changed(["a.example"], ["b.example"])).toBe(
      "added a.example; removed b.example",
    );
    expect(changed(["a.example"], [])).toBe("added a.example");
    expect(changed([], ["b.example"])).toBe("removed b.example");
  });
  it("shows why System managed changed, in words", () => {
    const changed = (added: string[], removed: string[]) =>
      row({
        type: "system_managed_changed",
        ts: 1,
        workspace_id: null,
        added,
        removed,
      } as AuditRecord).detail;
    expect(changed(["microsoft_server"], ["code_server"])).toBe(
      "now for Microsoft's VS Code server; no longer for code-server",
    );
    expect(changed(["direct_ssh", "later"], [])).toBe(
      "now for direct SSH, later",
    );
    expect(changed([], ["direct_ssh"])).toBe("no longer for direct SSH");
  });
  it("names the set that decided a connection or a request", () => {
    expect(
      row(connection(1, { rule_id: null, rule_set: "system" }).record).detail,
    ).toBe("System managed");
    expect(
      row(connection(1, { rule_id: 8, rule_set: "user:2" }).record).detail,
    ).toBe("rule set user:2 · rule 8");
    expect(
      row({
        type: "pending_decided",
        ts: 1,
        pending: {
          ...pending,
          state: "allowed",
          decided_by: "system",
          rule_set: "builtin:github",
        },
      } as AuditRecord).detail,
    ).toBe("by system, rule set builtin:github");
    expect(
      row({
        type: "rule_created",
        ts: 1,
        rule: { ...rule, scope: "set", workspace_id: null, set_id: 4 },
      } as AuditRecord).detail,
    ).toBe("allow in rule set 4, by ui");
  });
});

describe("raw records and export lines", () => {
  const { record } = connection(1, { host: 'a"b.test' });
  it("indents the raw record and keeps a hostile host as text", () => {
    expect(rawJson(record)).toContain('\n  "host": "a\\"b.test"');
    expect(JSON.parse(rawJson(record))).toEqual(record);
  });
  it("writes one line per record, exactly as the API sent it", () => {
    const line = jsonLine(record);
    expect(line.endsWith("\n")).toBe(true);
    expect(line.slice(0, -1)).not.toContain("\n");
    expect(JSON.parse(line)).toEqual(record);
  });
  it("names the file after the moment, with no characters a file system refuses", () => {
    expect(exportName(Date.UTC(2026, 9, 7, 12, 0, 5))).toBe(
      "puddle-activity-2026-10-07T12-00-05.jsonl",
    );
  });
});
