// SPDX-License-Identifier: GPL-3.0-or-later
// A stand-in for the API's global settings and consents endpoints, in the shape openapi-fetch
// returns them. Like the API it refuses Microsoft's server without a granted consent. Not
// shipped (tests import it).
import type { components } from "#lib/api/schema.d.ts";
import { needsMicrosoftConsent } from "#lib/settings/model.ts";

type S = components["schemas"];
type Body = S["GlobalSettingsRequest"];

const noLayer = (): S["SettingsLayer"] => ({
  clipboard_read: null,
  local_toggles: {
    link_local: null,
    loopback: null,
    metadata: null,
    private: null,
    special: null,
  },
  memory: null,
  cpus: null,
  reconnection_grace: null,
  wildcards_reach_local: null,
  zoom_hotkeys: null,
  direct_ssh: null,
  capture_logins: null,
});

const noUi = (): S["UiPrefs"] => ({
  theme: null,
  density: null,
  palette: null,
  custom_accent: null,
  custom_background: null,
  custom_surface: null,
  custom_text: null,
  notifications: null,
  sound: null,
  close_behaviour: null,
  minimize_to_tray: null,
  app_icon: null,
  notify_only_unfocused: null,
  badge: null,
  notify_oom: null,
  notify_unexpected_stop: null,
  confirm_global_rules: null,
  inbox_shortcuts: null,
  start_at_login: null,
});

export class FakeSettings {
  layer = noLayer();
  /** Fields the stored document has that Puddle doesn't know. */
  unknown: string[] = [];
  vscode: S["VsCodeServer"] = {
    server: null,
    telemetry: null,
    auto_update: null,
  };
  ui: S["UiPrefs"] = noUi();
  /** The user's own app icon, as the PNG text the API would send; `null` until one is stored. */
  customIcon: string | null = null;
  consent: S["Consent"] = { state: "not_asked" };
  /** The names no workspace decrypts. */
  neverDecrypt: string[] = [];
  /** How long and how many learned hosts are passed through. */
  learnedLimits: S["LearnedLimits"] = { ttl_secs: 43_200, cap: 1024 };
  calls: string[] = [];
  bodies: unknown[] = [];
  down = false;
  /** Refuse the next call to a path (`"PUT /api/settings"`). */
  refuse = new Map<
    string,
    { status: number; error: string; message: string }
  >();
  version = "0.1.0";

  /** Back to a fresh backend, keeping the object (a module mock holds on to it). */
  reset(): void {
    const fresh = new FakeSettings();
    this.layer = fresh.layer;
    this.unknown = [];
    this.vscode = fresh.vscode;
    this.ui = fresh.ui;
    this.customIcon = null;
    this.consent = fresh.consent;
    this.neverDecrypt = [];
    this.learnedLimits = { ttl_secs: 43_200, cap: 1024 };
    this.calls = [];
    this.bodies = [];
    this.down = false;
    this.refuse = new Map();
    this.version = fresh.version;
  }

  private reply(
    status: number,
    data?: unknown,
    error = "invalid",
    message = "",
  ) {
    const response = { status, ok: status < 400 } as Response;
    return status < 400
      ? { data, response }
      : { error: { error, message }, response };
  }

  private refusal(key: string) {
    const r = this.refuse.get(key);
    if (!r) return null;
    this.refuse.delete(key);
    return this.reply(r.status, undefined, r.error, r.message);
  }

  private pick(v: boolean | null, fallback = false) {
    return {
      value: v ?? fallback,
      source: v === null ? "default" : "global",
    } as const;
  }

  private view(): S["GlobalSettingsView"] {
    const l = this.layer;
    return {
      workspace_defaults: l,
      vscode_server: this.vscode,
      ui: this.ui,
      unknown_fields: this.unknown,
      effective: {
        memory: {
          value: l.memory ?? 8192,
          source: l.memory === null ? "default" : "global",
        },
        cpus: {
          value: l.cpus ?? 1,
          source: l.cpus === null ? "default" : "global",
        },
        local_toggles: {
          loopback: this.pick(l.local_toggles.loopback),
          private: this.pick(l.local_toggles.private),
          link_local: this.pick(l.local_toggles.link_local),
          metadata: this.pick(l.local_toggles.metadata),
          special: this.pick(l.local_toggles.special),
        },
        wildcards_reach_local: this.pick(l.wildcards_reach_local),
        reconnection_grace: {
          value: l.reconnection_grace ?? 300,
          source: l.reconnection_grace === null ? "default" : "global",
        },
        zoom_hotkeys: this.pick(l.zoom_hotkeys, true),
        direct_ssh: this.pick(l.direct_ssh),
        capture_logins: this.pick(l.capture_logins, true),
        clipboard_read: {
          value: l.clipboard_read ?? "ask",
          source: l.clipboard_read === null ? "default" : "global",
        },
      },
    };
  }

  private consents(): S["Consents"] {
    return {
      telemetry: { state: "not_asked" },
      crash_reports: { state: "not_asked" },
      vscode_server: this.consent,
    };
  }

  GET = async (path: string) => {
    this.calls.push(`GET ${path}`);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal(`GET ${path}`);
    if (refused) return refused;
    switch (path) {
      case "/api/settings":
        return this.reply(200, this.view());
      case "/api/consents":
        return this.reply(200, this.consents());
      case "/api/interception/never-decrypt":
        return this.reply(200, { never_decrypt: this.neverDecrypt });
      case "/api/interception/learned-limits":
        return this.reply(200, this.learnedLimits);
      case "/api/app-icons":
        return this.reply(200, {
          built_in: (
            ["duck", "duck_pond", "duck_night", "duck_sun"] as const
          ).map((choice) => ({ choice, png_base64: `${choice}-png` })),
          custom_png_base64: this.customIcon,
        } satisfies S["AppIcons"]);
      case "/api/health":
        return this.reply(200, { version: this.version, api_version: "0.3.0" });
    }
    throw new Error(`unexpected GET ${path}`);
  };

  PUT = async (
    path: string,
    init: { params?: { path?: { kind?: string } }; body: unknown },
  ) => {
    this.calls.push(`PUT ${path}`);
    this.bodies.push(init.body);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal(`PUT ${path}`);
    if (refused) return refused;
    if (path === "/api/interception/learned-limits") {
      this.learnedLimits = init.body as S["LearnedLimits"];
      return this.reply(200, this.learnedLimits);
    }
    if (path === "/api/interception/never-decrypt") {
      this.neverDecrypt = [
        ...(init.body as S["GlobalNeverDecrypt"]).never_decrypt,
      ].sort();
      return this.reply(200, { never_decrypt: this.neverDecrypt });
    }
    if (path === "/api/app-icons/custom") {
      if ((init.body as File).size === 0) {
        return this.reply(422, undefined, "invalid", "the file is empty");
      }
      this.customIcon = "own-png";
      return this.reply(200, { png_base64: this.customIcon });
    }
    if (path === "/api/consents/{kind}") {
      const b = init.body as S["ConsentRequest"];
      this.consent =
        b.decision === "granted"
          ? {
              state: "granted",
              at: 1_700_000_000_000,
              terms_version: b.terms_version,
            }
          : {
              state: "declined",
              at: 1_700_000_000_000,
              terms_version: b.terms_version,
            };
      // Like the API: a record that does not cover today's terms takes Microsoft's server back to
      // code-server in the same save.
      if (
        this.vscode.server === "microsoft" &&
        needsMicrosoftConsent(this.consent)
      ) {
        this.vscode = { ...this.vscode, server: "code_server" };
      }
      return this.reply(200, this.consents());
    }
    const b = init.body as Body;
    if (b.ui?.app_icon === "custom" && this.customIcon === null) {
      return this.reply(
        422,
        undefined,
        "invalid",
        "ui.app_icon: give Puddle an image before choosing your own icon",
      );
    }
    if (
      b.vscode_server?.server === "microsoft" &&
      this.consent.state !== "granted"
    ) {
      return this.reply(
        422,
        undefined,
        "invalid",
        "Microsoft's server needs the user's consent first",
      );
    }
    this.layer = { ...noLayer(), ...b.workspace_defaults };
    this.vscode = {
      server: null,
      telemetry: null,
      auto_update: null,
      ...b.vscode_server,
    };
    this.ui = { ...noUi(), ...b.ui };
    return this.reply(200, this.view());
  };

  DELETE = async (path: string) => {
    this.calls.push(`DELETE ${path}`);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal(`DELETE ${path}`);
    if (refused) return refused;
    if (this.ui.app_icon === "custom") {
      return this.reply(
        409,
        undefined,
        "conflict",
        "your own image is the icon in use; choose another icon and save first",
      );
    }
    this.customIcon = null;
    return { response: { status: 204, ok: true } as Response };
  };
}
