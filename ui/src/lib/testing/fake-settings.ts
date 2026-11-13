// SPDX-License-Identifier: GPL-3.0-or-later
// A stand-in for the API's global settings and consents endpoints, in the shape openapi-fetch
// returns them. Like the API it refuses Microsoft's server without a granted consent. Not
// shipped (tests import it).
import type { components } from "#lib/api/schema.d.ts";
import { FakeInterception } from "./fake-interception.ts";
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
    company_network: null,
  },
  memory: null,
  cpus: null,
  reconnection_grace: null,
  wildcards_reach_local: null,
  zoom_hotkeys: null,
  direct_ssh: null,
  capture_logins: null,
  warn_foreign_credentials: null,
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
  notify_disk_full: null,
  disk_warning_percent: null,
  notify_unexpected_stop: null,
  notify_low_disk_space: null,
  disk_warn_free_gb: null,
  disk_fail_free_gb: null,
  ssh_agent_pipe: null,
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
  tools: S["GlobalTools"] = { mise: null };
  /** The user's own app icon, as the PNG text the API would send; `null` until one is stored. */
  customIcon: string | null = null;
  consent: S["Consent"] = { state: "not_asked" };
  /** The names no workspace decrypts. */
  neverDecrypt: string[] = [];
  /** The proxy sign-ins stored (names only; the password is kept apart). */
  proxySignIns: S["StoredSignIn"][] = [];
  /** The network report the proxy field offers proxies from; none answers 503. */
  health: S["NetworkHealth"] | null = null;
  /** The passwords PUT, by proxy, for a test that checks what was sent. */
  proxyPasswords = new Map<string, string>();
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
  /** The global interception setting, answered on `/api/interception`. */
  interception = new FakeInterception();

  reset(): void {
    this.interception.reset();
    const fresh = new FakeSettings();
    this.layer = fresh.layer;
    this.unknown = [];
    this.vscode = fresh.vscode;
    this.ui = fresh.ui;
    this.tools = fresh.tools;
    this.customIcon = null;
    this.consent = fresh.consent;
    this.neverDecrypt = [];
    this.learnedLimits = { ttl_secs: 43_200, cap: 1024 };
    this.proxySignIns = [];
    this.proxyPasswords = new Map();
    this.health = null;
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
      tools: this.tools,
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
          company_network: this.pick(l.local_toggles.company_network),
        },
        wildcards_reach_local: this.pick(l.wildcards_reach_local),
        reconnection_grace: {
          value: l.reconnection_grace ?? 300,
          source: l.reconnection_grace === null ? "default" : "global",
        },
        zoom_hotkeys: this.pick(l.zoom_hotkeys, true),
        direct_ssh: this.pick(l.direct_ssh),
        capture_logins: this.pick(l.capture_logins, true),
        warn_foreign_credentials: this.pick(l.warn_foreign_credentials, true),
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
      case "/api/interception":
        return this.interception.GET(path);
      case "/api/interception/never-decrypt":
        return this.reply(200, { never_decrypt: this.neverDecrypt });
      case "/api/interception/learned-limits":
        return this.reply(200, this.learnedLimits);
      case "/api/proxy-sign-ins":
        return this.reply(200, { sign_ins: this.proxySignIns });
      case "/api/network-health":
        // The proxy field's suggestions: a report only when a test gives one.
        return this.health
          ? this.reply(200, this.health)
          : this.reply(503, undefined, "unavailable", "no report");
      case "/api/app-icons":
        return this.reply(200, {
          built_in: (
            ["duck", "duck_pond", "duck_night", "duck_sun"] as const
          ).map((choice) => ({ choice, png_base64: `${choice}-png` })),
          custom_png_base64: this.customIcon,
        } satisfies S["AppIcons"]);
      case "/api/health":
        return this.reply(200, {
          version: this.version,
          api_version: "0.3.0",
          ui_build_id: null,
        });
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
    if (path === "/api/interception") {
      return this.interception.PUT(path, init as never);
    }
    if (path === "/api/interception/learned-limits") {
      this.learnedLimits = init.body as S["LearnedLimits"];
      return this.reply(200, this.learnedLimits);
    }
    if (path === "/api/proxy-sign-ins") {
      const b = init.body as S["ProxySignInRequest"];
      this.proxyPasswords.set(b.proxy, b.password);
      this.proxySignIns = [
        ...this.proxySignIns.filter((s) => s.proxy !== b.proxy),
        { proxy: b.proxy, user: b.user },
      ].sort((a, c) => a.proxy.localeCompare(c.proxy));
      return this.reply(200, { sign_ins: this.proxySignIns });
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
    this.tools = { mise: null, ...b.tools };
    return this.reply(200, this.view());
  };

  DELETE = async (
    path: string,
    init?: { params?: { path?: { proxy?: string } } },
  ) => {
    this.calls.push(`DELETE ${path}`);
    if (this.down) throw new TypeError("down");
    const refused = this.refusal(`DELETE ${path}`);
    if (refused) return refused;
    if (path === "/api/proxy-sign-ins/{proxy}") {
      const proxy = init?.params?.path?.proxy;
      this.proxySignIns = this.proxySignIns.filter((s) => s.proxy !== proxy);
      this.proxyPasswords.delete(proxy ?? "");
      return { response: { status: 204, ok: true } as Response };
    }
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
