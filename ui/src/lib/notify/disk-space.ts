// SPDX-License-Identifier: AGPL-3.0-or-later
// The in-app notice for free space: raised when the drive that holds Puddle's data crosses the
// warning or the failure line, withdrawn when it recovers. The notice follows the same switch as
// the system notification (Settings, Notifications); a recovery is never announced.
import { asDiskSpace, gbFromMib } from "#lib/settings/disk-space.ts";
import type { Notifier } from "./notifier.ts";

const KEY = "disk-space";

export class DiskSpaceNotices {
  readonly #notifier: Notifier;
  readonly #enabled: () => boolean;

  /** `enabled` says whether the user wants this notice (read at each event, so a change applies at once). */
  constructor(notifier: Notifier, enabled: () => boolean) {
    this.#notifier = notifier;
    this.#enabled = enabled;
  }

  /** Raises or withdraws the notice for a `disk_space_changed` event; false for any other event. */
  handle(raw: unknown): boolean {
    const event = asDiskSpace(raw);
    if (!event) return false;
    if (event.level === "ok") {
      this.#notifier.resolve(KEY);
      return true;
    }
    if (!this.#enabled()) return true;
    const left = `${gbFromMib(event.free_mib)} GB are left on the drive that holds Puddle's data.`;
    this.#notifier.notify({
      key: KEY,
      tone: "warning",
      title:
        event.level === "critical"
          ? "Disk space is almost gone."
          : "Disk space is running low.",
      detail:
        event.level === "critical"
          ? `${left} Free space now: a build or a tool in a workspace can stop halfway when the drive is full.`
          : `${left} Delete workspaces you no longer use and files you do not need before it runs out.`,
      link: { href: "/settings#s-disk", label: "Disk space settings" },
    });
    return true;
  }
}
