// SPDX-License-Identifier: AGPL-3.0-or-later
// A stand-in for `GET/PUT /api/interception`, in the shape openapi-fetch returns. Not shipped
// (tests import it).
import type { components } from "#lib/api/schema.d.ts";

type View = components["schemas"]["InterceptionSettingsView"];
type Body = components["schemas"]["InterceptionSettingsRequest"];

export class FakeInterception {
  view: View = FakeInterception.fresh();
  calls: string[] = [];
  bodies: Body[] = [];
  /** Answers every call with a failure. */
  refuse = false;
  /** Fails like a service that is not there. */
  down = false;

  static fresh(): View {
    return {
      mode: "known_only",
      chosen: false,
      learn_from_rejections: true,
      notice_seen: false,
      curated: [],
    };
  }

  reset(): void {
    this.view = FakeInterception.fresh();
    this.calls = [];
    this.bodies = [];
    this.refuse = false;
    this.down = false;
  }

  private answer() {
    const response = (status: number) =>
      ({ status, ok: status < 400 }) as Response;
    return this.refuse
      ? {
          error: { error: "failing", message: "no" },
          response: response(500),
        }
      : { data: this.view, response: response(200) };
  }

  GET = async (path: string) => {
    this.calls.push(`GET ${path}`);
    if (this.down) throw new TypeError("down");
    return this.answer();
  };

  PUT = async (path: string, init: { body: Body }) => {
    this.calls.push(`PUT ${path}`);
    if (this.down) throw new TypeError("down");
    this.bodies.push(init.body);
    if (!this.refuse) {
      const b = init.body;
      if (b.mode) {
        this.view.mode = b.mode;
        this.view.chosen = true;
        this.view.notice_seen = true;
      }
      if (b.learn_from_rejections != null)
        this.view.learn_from_rejections = b.learn_from_rejections;
      if (b.notice_seen != null) this.view.notice_seen = b.notice_seen;
    }
    return this.answer();
  };
}
