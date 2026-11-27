// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { GlobalSettings } from "#lib/stores/global-settings.svelte.ts";
import { saveToggle } from "./apply.ts";
import { SettingsSaver } from "./saver.svelte.ts";

type Doc = Record<string, unknown> & { revision: number };

describe("two local-destination switches changed within one round trip", () => {
  it("both persist, and each row says Saved once its change is saved", async () => {
    let doc: Doc = {
      revision: 1,
      workspace_defaults: {
        local_toggles: { loopback: false, private: false },
      },
      vscode_server: {},
      ui: {},
      tools: {},
      limits: {},
    };
    const bodies: Doc[] = [];
    const answer = (data: unknown) =>
      Promise.resolve({
        data,
        error: undefined,
        response: new Response(null, { status: 200 }),
      });
    const api = {
      GET: (path: string) =>
        answer(path === "/api/settings" ? structuredClone(doc) : {}),
      PUT: async (_path: string, { body }: { body: Doc }) => {
        bodies.push(structuredClone(body));
        await new Promise((r) => setTimeout(r, 5));
        doc = { ...structuredClone(body), revision: doc.revision + 1 };
        return answer(structuredClone(doc));
      },
    };
    const store = new GlobalSettings(api as never);
    await store.load();
    const saver = new SettingsSaver(store);
    const first = saveToggle(saver, "a", "loopback", true, false);
    const second = saveToggle(saver, "b", "private", true, false);
    expect(await first).toBe(true);
    expect(await second).toBe(true);
    const sent = bodies.map(
      (b) => (b.workspace_defaults as { local_toggles: unknown }).local_toggles,
    );
    expect(sent).toEqual([
      { loopback: true, private: false },
      { loopback: true, private: true },
    ]);
    expect(saver.rows["a"]).toMatchObject({ kind: "saved" });
    expect(saver.rows["b"]).toMatchObject({ kind: "saved" });
  });
});
