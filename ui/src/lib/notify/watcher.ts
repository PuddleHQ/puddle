// SPDX-License-Identifier: AGPL-3.0-or-later
// Turns the shell's event stream into notices: out-of-memory kills, a workspace that stopped
// or crashed without being asked to, a network that got worse, and a drive that runs low on room.
//
// "Without being asked": a stop is expected when the stream said the workspace was stopping,
// draining or being removed since it last ran. A stop with none of those, or a crash, is not.
// Statuses are tracked here from the events, seeded from the workspace list; a workspace never
// seen before can't be called unexpected.
import { workspaces } from "#lib/stores/workspaces.svelte.ts";
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import {
  problems,
  problemSummary,
  type NetworkHealth,
} from "#lib/network/model.ts";
import { asWorkspaceEvent } from "#lib/workspaces/events.ts";
import { globalSettings } from "#lib/stores/global-settings.svelte.ts";
import { identities } from "#lib/stores/identities.svelte.ts";
import type { LiveSource } from "#lib/stores/live.svelte.ts";
import { CredentialNotices, type IdentityLookup } from "./credentials.ts";
import { DiskSpaceNotices } from "./disk-space.ts";
import { LearnedNotices } from "#lib/interception/learned.ts";
import { EnvironmentNotices } from "./environment.ts";
import { ForeignCredentialNotices } from "./foreign-credentials.ts";
import { UnknownTokenNotices } from "./unknown-tokens.ts";
import { InterceptionChoiceNotice } from "./interception.ts";
import { LoginNotices } from "./logins.ts";
import { InAppNotifier, type Notifier } from "./notifier.ts";
import { ProblemNotices } from "./problems.ts";

type Status = components["schemas"]["WorkspaceStatus"];

const RUNNING: readonly Status[] = ["starting", "running", "paused"];
const NETWORK_KEY = "network";
const stopKey = (name: string) => `stop:${name}`;
const oomKey = (name: string) => `oom:${name}`;
const diskKey = (name: string, disk: string) => `disk:${name}:${disk}`;
const sshKey = (name: string) => `ssh:${name}`;

export interface WatcherDeps {
  source?: LiveSource;
  notifier?: Notifier;
  api?: Pick<ApiClient, "GET" | "POST" | "PUT" | "DELETE">;
  /** What the sign-in notice asks of the identities; the app's own by default. */
  identities?: IdentityLookup;
  /** Whether the free-space notice is wanted (Settings, Notifications); the stored choice by default. */
  lowDiskSpaceNotice?: () => boolean;
}

export class NoticeWatcher {
  readonly #source: LiveSource | undefined;
  readonly #notifier: Notifier;
  readonly #api: Pick<ApiClient, "GET" | "POST" | "PUT" | "DELETE">;
  readonly #credentials: CredentialNotices;
  readonly #logins: LoginNotices;
  readonly #foreign: ForeignCredentialNotices;
  readonly #unknownTokens: UnknownTokenNotices;
  readonly #environment: EnvironmentNotices;
  readonly #learned: LearnedNotices;
  readonly #problems: ProblemNotices;
  readonly #interceptionChoice: InterceptionChoiceNotice;
  readonly #diskSpace: DiskSpaceNotices;
  readonly #status = new Map<string, Status>();
  readonly #expected = new Set<string>();
  /** The summary the network notice last said, so an unchanged problem is not announced again. */
  #network: string | null = null;

  constructor(deps: WatcherDeps = {}) {
    this.#source = deps.source;
    this.#notifier = deps.notifier ?? new InAppNotifier();
    this.#api = deps.api ?? defaultApi;
    this.#credentials = new CredentialNotices({
      notifier: this.#notifier,
      identities: deps.identities ?? identities,
      api: this.#api,
    });
    this.#logins = new LoginNotices(this.#notifier, this.#api);
    this.#foreign = new ForeignCredentialNotices(this.#notifier);
    this.#unknownTokens = new UnknownTokenNotices(this.#notifier);
    this.#environment = new EnvironmentNotices(this.#notifier);
    this.#learned = new LearnedNotices(this.#notifier, this.#api);
    this.#problems = new ProblemNotices(this.#notifier, this.#api);
    this.#interceptionChoice = new InterceptionChoiceNotice(
      this.#notifier,
      this.#api,
    );
    this.#diskSpace = new DiskSpaceNotices(
      this.#notifier,
      deps.lowDiskSpaceNotice ??
        (() => globalSettings.view?.ui.notify_low_disk_space ?? true),
    );
  }

  /** Reads the statuses the events will be compared with; never throws. */
  async seed(): Promise<void> {
    void this.#problems.refresh();
    void this.#interceptionChoice.refresh();
    try {
      const { data } = await this.#api.GET("/api/workspaces");
      if (!data) return;
      for (const w of data.workspaces) {
        // An event that already told us more recent news wins over the list.
        if (!this.#status.has(w.id)) this.#status.set(w.id, w.status);
      }
    } catch {
      // Without a list, the events alone still work for workspaces they name.
    }
  }

  /** Applies one stream event. */
  handle(raw: unknown): void {
    if (
      this.#credentials.handle(raw) ||
      this.#logins.handle(raw) ||
      this.#foreign.handle(raw) ||
      this.#unknownTokens.handle(raw) ||
      this.#environment.handle(raw) ||
      this.#learned.handle(raw) ||
      this.#problems.handle(raw) ||
      this.#diskSpace.handle(raw)
    ) {
      return;
    }
    if (this.#sshFailed(raw)) return;
    const event = asWorkspaceEvent(raw);
    if (!event) return;
    const id = event.workspace;
    const name = workspaces.labelOf(id);
    switch (event.type) {
      case "oom_kill":
        this.#notifier.notify({
          key: oomKey(id),
          tone: "warning",
          title: `${name} ran out of memory and a process was killed.`,
          detail: `${event.process} (process ${event.pid}) was stopped. Give the workspace more memory in its settings; it applies at the next restart.`,
          link: {
            href: `/workspaces/${encodeURIComponent(id)}/settings`,
            label: "Workspace settings",
          },
        });
        return;
      case "disk_nearly_full":
        this.#notifier.notify({
          key: diskKey(name, event.disk),
          tone: "warning",
          title: `${name}: the ${event.disk === "root" ? "root disk" : "volume"} is ${event.percent} % full.`,
          detail:
            event.disk === "root"
              ? "Free space in the workspace, or reset the root disk from the workspace page: packages reinstall on the next start and the volume is untouched."
              : "Delete what you no longer need in the workspace; the volume cannot grow.",
          link: {
            href: `/workspaces/${encodeURIComponent(name)}`,
            label: "Open workspace",
          },
        });
        return;
      case "workspace_progress":
        if (event.step === "stopping" || event.step === "removing") {
          this.#expected.add(id);
        }
        return;
      case "status_changed":
        this.#onStatus(id, event.status);
        return;
    }
  }

  /** Direct SSH failed or recovered in a way the `ssh` client can't show; false when the event is neither. */
  #sshFailed(raw: unknown): boolean {
    if (typeof raw !== "object" || raw === null) return false;
    const type = (raw as { type?: unknown }).type;
    if (type === "workspace_ssh_recovered") {
      const { workspace } = raw as { workspace?: unknown };
      if (typeof workspace === "string") {
        this.#notifier.resolve(sshKey(workspace));
      }
      return true;
    }
    if (type !== "workspace_ssh_failed") return false;
    const { workspace, reason } = raw as {
      workspace?: unknown;
      reason?: unknown;
    };
    if (typeof workspace !== "string" || typeof reason !== "string")
      return true;
    this.#notifier.notify({
      key: sshKey(workspace),
      tone: "warning",
      title: `Direct SSH to ${workspaces.labelOf(workspace)} failed.`,
      detail: `${reason} The workspace itself keeps running.`,
      link: {
        href: `/workspaces/${encodeURIComponent(workspace)}`,
        label: "Open workspace",
      },
    });
    return true;
  }

  #onStatus(id: string, status: Status): void {
    const name = workspaces.labelOf(id);
    const before = this.#status.get(id);
    this.#status.set(id, status);
    if (!RUNNING.includes(status) && status !== "draining") {
      // The endpoint it reported on is gone; the next start opens a new one.
      this.#notifier.resolve(sshKey(id));
    }
    if (status === "draining") {
      this.#expected.add(id);
      return;
    }
    if (RUNNING.includes(status) || status === "created") {
      this.#expected.delete(id);
      this.#notifier.resolve(stopKey(id));
      return;
    }
    const wasUp =
      before !== undefined && [...RUNNING, "draining"].includes(before);
    if (status === "crashed") {
      this.#notifier.notify({
        key: stopKey(id),
        tone: "warning",
        title: `${name} crashed.`,
        detail:
          "Its machine stopped working. Start it again from the workspace list.",
        link: {
          href: `/workspaces/${encodeURIComponent(id)}`,
          label: "Open workspace",
        },
      });
    } else if (status === "volume_missing") {
      this.#notifier.notify({
        key: stopKey(id),
        tone: "warning",
        title: `${name} can't start: its disk is missing.`,
        detail:
          "Restore the volume and start it again, or delete the workspace.",
        link: {
          href: `/workspaces/${encodeURIComponent(id)}`,
          label: "Open workspace",
        },
      });
    } else if (status === "stopped" && wasUp && !this.#expected.has(id)) {
      this.#notifier.notify({
        key: stopKey(id),
        tone: "warning",
        title: `${name} stopped without being asked to.`,
        detail:
          "Nothing in Puddle stopped it. Start it again from the workspace list.",
        link: {
          href: `/workspaces/${encodeURIComponent(id)}`,
          label: "Open workspace",
        },
      });
    }
  }

  /** The network got worse or better: raises the notice while there is a problem, withdraws it after. */
  network(report: NetworkHealth | null): void {
    if (!report) return;
    const list = problems(report);
    if (list.length === 0) {
      this.#network = null;
      this.#notifier.resolve(NETWORK_KEY);
      return;
    }
    const title = `Network trouble: ${problemSummary(list)}`;
    if (title === this.#network) return;
    this.#network = title;
    this.#notifier.notify({
      key: NETWORK_KEY,
      tone: "warning",
      title,
      link: { href: "/settings/network-health", label: "Network health" },
    });
  }

  /** Listens to the stream; returns the function that stops it. */
  start(): () => void {
    void this.seed();
    const unsubscribe = this.#source?.subscribe({
      event: (e) => this.handle(e),
      resync: () => void this.seed(),
    });
    const stopCredentials = this.#credentials.start();
    return () => {
      unsubscribe?.();
      stopCredentials();
    };
  }
}
