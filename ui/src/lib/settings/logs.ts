// SPDX-License-Identifier: AGPL-3.0-or-later
// "Export log bundle" on the Settings screen: asks the service for the bundle and hands the file to
// the browser's download. The service builds it and cuts the secrets out; nothing is read here.
import { api } from "#lib/api/client.ts";
import { readFailure } from "#lib/stores/read-failure.ts";
import { fileNameFrom, save } from "./backup.ts";

export type LogBundleResult =
  { ok: true; name: string; bytes: number } | { ok: false; reason: string };

/** Makes a log bundle and starts its download. */
export async function exportLogBundle(): Promise<LogBundleResult> {
  try {
    const { data, error, response } = await api.POST("/api/logs/bundle", {
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
