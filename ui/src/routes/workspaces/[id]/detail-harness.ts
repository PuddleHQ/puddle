// SPDX-License-Identifier: AGPL-3.0-or-later
// The fakes, the address and the navigation spy the workspace detail tests share. A test file
// imports them in a `vi.hoisted` call and mocks the stores with the factories below (vi.mock
// must stay in the file that uses it); nothing here imports a mocked module.
import { vi } from "vitest";
import * as inbox from "#lib/testing/fake-inbox.ts";
import * as ws from "#lib/testing/fake-workspaces.ts";
import * as rules from "#lib/testing/fake-rules.ts";
import * as requestRules from "#lib/testing/fake-request-rules.ts";

export const url = {
  pathname: "/workspaces/w-demo",
  id: "w-demo",
};

export const goto = vi.fn();

export const h = {
  inbox: new inbox.FakeInbox(),
  source: new inbox.FakeSource(),
  request: inbox.request,
  api: new ws.FakeWorkspaces(),
  workspace: ws.workspace,
  rules: new rules.FakeRules(),
  rule: rules.rule,
  requestRules: new requestRules.FakeRequestRules(),
  requestRule: requestRules.requestRule,
};

export function pendingMock(
  mod: typeof import("#lib/stores/pending.svelte.ts"),
) {
  return {
    ...mod,
    pending: new mod.PendingStore({
      api: h.inbox as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
}

export function workspacesMock(
  mod: typeof import("#lib/stores/workspaces.svelte.ts"),
) {
  return {
    ...mod,
    workspaces: new mod.WorkspaceStore({
      api: h.api as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
}

export function rulesMock(mod: typeof import("#lib/stores/rules.svelte.ts")) {
  return {
    ...mod,
    rulesStore: new mod.RulesStore({
      api: h.rules as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
}

export function requestRulesMock(
  mod: typeof import("#lib/stores/request-rules.svelte.ts"),
) {
  return {
    ...mod,
    requestRulesStore: new mod.RequestRulesStore({
      api: h.requestRules as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
}
