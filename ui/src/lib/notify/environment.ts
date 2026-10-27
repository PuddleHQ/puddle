// SPDX-License-Identifier: GPL-3.0-or-later
// Notices for two background failures a workspace's tabs cannot show unless they are open: a
// change to its variables or secrets that did not reach it while it ran, and a kept port forward
// that could not be opened again at a start. Each says why and links to the tab that fixes it.
import { asEnvFailed } from "#lib/environment/events.ts";
import { asPortForwardFailed } from "#lib/workspaces/ports.ts";
import type { Notifier } from "./notifier.ts";

const tab = (workspace: string, slug: string) =>
  `/workspaces/${encodeURIComponent(workspace)}/${slug}`;

export class EnvironmentNotices {
  readonly #notifier: Notifier;

  constructor(notifier: Notifier) {
    this.#notifier = notifier;
  }

  /** Raises the notice for one of the two events; false for any other event. */
  handle(raw: unknown): boolean {
    const env = asEnvFailed(raw);
    if (env) {
      this.#notifier.notify({
        key: `env:${env.workspace}`,
        tone: "warning",
        title: `A change to the environment of ${env.workspace} did not reach it.`,
        detail: env.reason,
        link: {
          href: tab(env.workspace, "environment"),
          label: "Open the environment",
        },
      });
      return true;
    }
    const port = asPortForwardFailed(raw);
    if (port) {
      this.#notifier.notify({
        key: `port:${port.workspace}:${port.guest_port}`,
        tone: "warning",
        title: `Port ${port.guest_port} of ${port.workspace} could not be forwarded to 127.0.0.1:${port.host_port}.`,
        detail: port.reason,
        link: { href: tab(port.workspace, "ports"), label: "Open the ports" },
      });
      return true;
    }
    return false;
  }
}
