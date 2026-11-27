// SPDX-License-Identifier: AGPL-3.0-or-later
// "Back up now" on the Settings screen: asks the service for a backup and hands the file to the
// browser's download. The file is built by the service; nothing of it is read or kept here.
import { api } from "#lib/api/client.ts";
import { readFailure } from "#lib/stores/read-failure.ts";

export type BackupResult =
  { ok: true; name: string; bytes: number } | { ok: false; reason: string };

/** What a download is called when the service names none. */
const FALLBACK_NAME = "puddle-backup.tar.gz";

/** The file name the service sent in `Content-Disposition`, or a plain default. */
export function fileNameFrom(disposition: string | null): string {
  const match = /filename="([^"/\\]+)"/.exec(disposition ?? "");
  return match?.[1] ?? FALLBACK_NAME;
}

/** A size as a person reads it: `1.5 MB`, `312 KB`, `40 bytes`. */
export function sizeText(bytes: number): string {
  if (bytes >= 1024 * 1024) return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} bytes`;
}

/** How long the download address of a backup stays valid after the click. */
const REVOKE_AFTER_MS = 30_000;

/** Offers `blob` to the user as a download named `name`. */
export function save(blob: Blob, name: string): void {
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  // Some web views start the download after the click returns and drop it if the address is gone.
  setTimeout(() => URL.revokeObjectURL(url), REVOKE_AFTER_MS);
}

/** Makes a backup and starts its download. */
export async function backUpNow(): Promise<BackupResult> {
  try {
    const { data, error, response } = await api.POST("/api/backup", {
      parseAs: "blob",
    });
    if (!data) {
      return { ok: false, reason: readFailure(error, response) };
    }
    const name = fileNameFrom(response.headers.get("content-disposition"));
    save(data, name);
    return { ok: true, name, bytes: data.size };
  } catch {
    return { ok: false, reason: "Puddle's service isn't answering." };
  }
}
