// SPDX-License-Identifier: AGPL-3.0-or-later
import { describe, expect, it } from "vitest";
import { report } from "#lib/testing/fake-network.ts";
import {
  canTrust,
  devCertificateLine,
  MORE_DETAILS,
  PURPOSE,
  rootsLine,
  trustNote,
} from "./certificates.ts";

const roots = (over: Partial<ReturnType<typeof report>["roots"]>) => ({
  roots: { ...report().roots, ...over },
});

describe("the Trust button", () => {
  it("is offered where Windows can be asked and has not been answered yes", () => {
    expect(canTrust("needs_trust")).toBe(true);
    expect(canTrust("trust_declined")).toBe(true);
    for (const status of [
      "trusted",
      "trust_failed",
      "unsupported",
      "not_checked",
      undefined,
    ] as const) {
      expect(canTrust(status), String(status)).toBe(false);
    }
  });
});

describe("the note under the Trust button", () => {
  const press = (over: Partial<Parameters<typeof trustNote>[0]>) => ({
    trusting: false,
    error: null,
    result: null,
    status: "needs_trust" as const,
    ...over,
  });

  it("says what Windows is doing while it waits, then what came of it", () => {
    expect(trustNote(press({ trusting: true }))).toMatch(
      /Security Warning.*taskbar/,
    );
    expect(trustNote(press({}))).toBe("");
    expect(
      trustNote(
        press({ result: { waiting: false, detail: null }, status: "trusted" }),
      ),
    ).toBe("Development certificate trusted.");
    expect(
      trustNote(
        press({
          result: { waiting: false, detail: "os error 5" },
          status: "trust_failed",
        }),
      ),
    ).toBe("Development certificate: couldn't be trusted. Details: os error 5");
  });

  it("says when an earlier dialog is still open, and why a press failed", () => {
    expect(
      trustNote(press({ result: { waiting: true, detail: null } })),
    ).toMatch(/still waiting for your answer.*another/);
    expect(trustNote(press({ error: "Puddle refused that." }))).toBe(
      "Puddle refused that.",
    );
  });
});

describe("the development certificate line", () => {
  it("says every other state in words, and which of them warns", () => {
    const cases = [
      ["trusted", "ok", "Development certificate trusted", /restart Firefox/],
      [
        "needs_trust",
        "info",
        "Development certificate: not trusted yet",
        /doesn't trust/,
      ],
      [
        "trust_declined",
        "warn",
        "Development certificate: not trusted",
        /answered No/,
      ],
      [
        "trust_failed",
        "warn",
        "Development certificate: couldn't be trusted",
        /didn.t let Puddle/,
      ],
      [
        "unsupported",
        "info",
        "Development certificate: not trusted on this system yet",
        /can't add its localhost certificate/,
      ],
    ] as const;
    for (const [status, tone, title, text] of cases) {
      const line = devCertificateLine(status);
      expect(line.tone, status).toBe(tone);
      expect(line.title, status).toBe(title);
      expect(line.text, status).toMatch(text);
    }
  });

  it("says what skipping costs, and what to do, in every state that warns", () => {
    for (const status of [
      "needs_trust",
      "trust_declined",
      "trust_failed",
      "unsupported",
    ] as const) {
      expect(devCertificateLine(status).impact, status).toMatch(
        /browser .*warn|not secure/,
      );
    }
    expect(devCertificateLine("needs_trust").remedy).toMatch(
      /asks once.*Security Warning[^.]*choose Yes.*only this certificate.*skip/is,
    );
    expect(devCertificateLine("trusted").remedy).toBeUndefined();
  });

  it("keeps every short text short and free of jargon", () => {
    for (const status of [
      "trusted",
      "needs_trust",
      "trust_declined",
      "trust_failed",
      "unsupported",
      undefined,
    ] as const) {
      const line = devCertificateLine(status);
      for (const t of [line.text, line.impact, line.remedy]) {
        if (t === undefined) continue;
        expect(t.split(/(?<=\.)\s/).length, t).toBeLessThanOrEqual(3);
        expect(t).not.toMatch(/development server|HTTPS development/i);
      }
    }
  });

  it("explains the purpose and the Windows question in advance", () => {
    expect(PURPOSE).toMatch(/forwards that page.*https:\/\/localhost/);
    expect(PURPOSE).toMatch(/Nothing else needs it/);
    const all = MORE_DETAILS.join(" ");
    expect(all).toMatch(/Security Warning that names the certificate/);
    expect(all).toMatch(/your Windows user only/);
    expect(all).toMatch(/Manage user certificates/);
    expect(all).toMatch(/If you skip/);
  });

  it("says plainly that nothing was looked at yet", () => {
    for (const status of ["not_checked", undefined] as const) {
      const line = devCertificateLine(status);
      expect(line.tone).toBe("skipped");
      expect(line.title).toBe("Development certificate: not checked yet");
      expect(line.text).toMatch(/hasn't looked/);
      expect(line.impact).toMatch(/browser warns/);
    }
  });
});

describe("the company roots line", () => {
  it("waits for the report, and says when it never came", () => {
    expect(rootsLine(null, false).text).toMatch(/reading/);
    const failed = rootsLine(null, true);
    expect(failed.title).toBe("Company certificates: not read");
    expect(failed.text).toMatch(/standard certificates/);
  });

  it("says the stores were not read yet", () => {
    const line = rootsLine(roots({ synced: false, roots: 0 }), false);
    expect(line.tone).toBe("skipped");
    expect(line.title).toBe("Company certificates: not read yet");
  });

  it("warns when a certificate store could not be read, whatever else was found", () => {
    const one = rootsLine(
      roots({
        synced: true,
        roots: 2,
        unreadable_stores: ["LocalMachine\\Root: access denied"],
      }),
      false,
    );
    expect(one.tone).toBe("warn");
    expect(one.title).toBe("1 certificate store couldn't be read");
    expect(one.text).toMatch(/Network health/);
    const two = rootsLine(
      roots({ synced: true, roots: 0, unreadable_stores: ["a", "b"] }),
      false,
    );
    expect(two.title).toBe("2 certificate stores couldn't be read");
  });

  it("says none were found", () => {
    const line = rootsLine(roots({ synced: true, roots: 0 }), false);
    expect(line.tone).toBe("info");
    expect(line.title).toBe("No company root certificates found");
  });

  it("counts the roots workspaces get", () => {
    const one = rootsLine(roots({ synced: true, roots: 1 }), false);
    expect(one.title).toBe("1 company root certificate found");
    expect(one.tone).toBe("ok");
    const two = rootsLine(roots({ synced: true, roots: 2 }), false);
    expect(two.title).toBe("2 company root certificates found");
    expect(two.text).toMatch(/Added to every workspace/);
  });
});
