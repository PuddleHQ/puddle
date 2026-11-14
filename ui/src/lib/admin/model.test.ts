// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import {
  auditText,
  canDelete,
  inviteLabel,
  inviteLink,
  isLoopback,
  sizeText,
  stateText,
  workspacesText,
  type AuditLine,
} from "./model.ts";
import { user } from "#lib/testing/fake-admin.ts";

describe("sizeText", () => {
  it("reads MiB below a GiB and GiB above", () => {
    expect(sizeText(0)).toBe("0 MiB");
    expect(sizeText(1023)).toBe("1023 MiB");
    expect(sizeText(1024)).toBe("1 GiB");
    expect(sizeText(1536)).toBe("1.5 GiB");
  });
});

describe("workspacesText", () => {
  it("counts running and stopped, or says none", () => {
    expect(workspacesText(user())).toBe("none");
    expect(workspacesText(user({ workspaces_running: 2 }))).toBe("2 running");
    expect(workspacesText(user({ workspaces_stopped: 1 }))).toBe("1 stopped");
    expect(
      workspacesText(user({ workspaces_running: 2, workspaces_stopped: 1 })),
    ).toBe("2 running, 1 stopped");
  });
});

describe("account states", () => {
  it("names each state", () => {
    expect(stateText(user({ state: "invited" }))).toBe("Invited");
    expect(stateText(user({ state: "active" }))).toBe("Active");
    expect(stateText(user({ state: "deleting" }))).toBe("Being deleted");
  });

  it("lets the owner act on another user who is not already going", () => {
    expect(canDelete(user())).toBe(true);
    expect(canDelete(user({ state: "invited" }))).toBe(true);
    expect(canDelete(user({ state: "deleting" }))).toBe(false);
    expect(canDelete(user({ role: "owner" }))).toBe(false);
  });

  it("calls a re-invite of an active account a password reset", () => {
    expect(inviteLabel(user({ state: "invited" }))).toBe("New invite link");
    expect(inviteLabel(user())).toBe("Reset password");
  });
});

describe("the invite link", () => {
  it("keeps the secret after the #", () => {
    expect(inviteLink("https://puddle.example", "abc")).toBe(
      "https://puddle.example/invite#abc",
    );
  });

  it("knows an address the invited person cannot reach", () => {
    expect(isLoopback("http://127.0.0.1:4000")).toBe(true);
    expect(isLoopback("http://localhost:4000")).toBe(true);
    expect(isLoopback("http://puddle.localhost")).toBe(true);
    expect(isLoopback("http://[::1]:4000")).toBe(true);
    expect(isLoopback("https://puddle.example")).toBe(false);
    expect(isLoopback("not an address")).toBe(false);
  });
});

describe("auditText", () => {
  const line = (action: AuditLine["action"]): AuditLine => ({
    action,
    actor: "Owen",
    target: "Sam",
    target_id: 2,
    at: 1,
  });
  it("says who did what to whom", () => {
    expect(auditText(line("user_created"))).toBe("Owen made the account Sam");
    expect(auditText(line("invite_reissued"))).toBe(
      "Owen gave Sam a new invite link",
    );
    expect(auditText(line("user_delete_started"))).toBe(
      "Owen started deleting Sam",
    );
    expect(auditText(line("user_deleted"))).toBe("Sam was deleted");
  });
});
