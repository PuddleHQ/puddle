// SPDX-License-Identifier: AGPL-3.0-or-later
// The fakes the inbox page tests share. A test file mocks the three stores with the factories
// below (vi.mock must stay in the file that uses it); nothing here imports a mocked module.
import * as m from "#lib/testing/fake-inbox.ts";
import * as sets from "#lib/testing/fake-rule-sets.ts";
import * as agent from "#lib/testing/fake-ssh-agent.ts";

export const h = {
  agentApi: new agent.FakeSshAgent(),
  ask: agent.ask,
  inbox: new m.FakeInbox(),
  setsApi: new sets.FakeRuleSets(),
  source: new m.FakeSource(),
  request: m.request,
  mine: sets.mine,
};

export function ruleSetsMock(
  mod: typeof import("#lib/stores/rule-sets.svelte.ts"),
) {
  return {
    ...mod,
    ruleSets: new mod.RuleSetsStore({
      api: h.setsApi as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
}

export function sshAgentAsksMock(
  mod: typeof import("#lib/stores/ssh-agent-asks.svelte.ts"),
) {
  return {
    ...mod,
    sshAgentAsks: new mod.SshAgentAsksStore(
      h.agentApi as never,
      h.source,
      () => {},
    ),
  };
}

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
