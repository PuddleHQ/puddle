// SPDX-License-Identifier: AGPL-3.0-or-later
// The fakes the rules page tests share. A test file mocks the three rule stores with the
// factories below (vi.mock must stay in the file that uses it); nothing here imports a mocked
// module, so a store factory can load this file without a cycle.
import * as requestRules from "#lib/testing/fake-request-rules.ts";
import * as inbox from "#lib/testing/fake-inbox.ts";
import * as fakes from "#lib/testing/fake-rules.ts";
import * as sets from "#lib/testing/fake-rule-sets.ts";

export const h = {
  requestApi: new requestRules.FakeRequestRules(),
  requestRule: requestRules.requestRule,
  api: new fakes.FakeRules(),
  setsApi: new sets.FakeRuleSets(),
  source: new inbox.FakeSource(),
  rule: fakes.rule,
  builtIn: sets.builtIn,
  mine: sets.mine,
  systemHost: sets.systemHost,
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

export function requestRulesMock(
  mod: typeof import("#lib/stores/request-rules.svelte.ts"),
) {
  return {
    ...mod,
    requestRulesStore: new mod.RequestRulesStore({
      api: h.requestApi as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
}

export function rulesMock(mod: typeof import("#lib/stores/rules.svelte.ts")) {
  return {
    ...mod,
    rulesStore: new mod.RulesStore({
      api: h.api as never,
      source: h.source,
      pollMs: 60_000,
    }),
  };
}
