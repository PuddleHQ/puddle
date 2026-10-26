// SPDX-License-Identifier: GPL-3.0-or-later
// The app icon on the Settings screen: the choices and their names, and the three calls that
// read the icons, store the user's own image and remove it. Which icon is in use is the
// `ui.app_icon` setting, saved with the rest; the image itself is stored at once.
import { api } from "#lib/api/client.ts";
import type { components } from "#lib/api/schema.d.ts";
import { readFailure } from "#lib/stores/read-failure.ts";

type S = components["schemas"];

export type AppIconChoice = NonNullable<S["UiPrefs"]["app_icon"]>;
export type AppIcons = S["AppIcons"];

/** The built-in icons by the name the screen shows. */
export const ICON_NAMES: Record<Exclude<AppIconChoice, "custom">, string> = {
  duck: "Duck",
  duck_pond: "Duck on a pond",
  duck_night: "Duck at night",
  duck_sun: "Duck in the sun",
};

/** The largest file the service takes (it says so too; this saves the trip). */
export const MAX_ICON_BYTES = 8 * 1024 * 1024;

export const isIconChoice = (v: unknown): v is AppIconChoice =>
  v === "custom" || (typeof v === "string" && v in ICON_NAMES);

export type IconResult<T> =
  { ok: true; value: T } | { ok: false; reason: string };

const unreachable = (): { ok: false; reason: string } => ({
  ok: false,
  reason: "Puddle's service isn't answering.",
});

export function pngUrl(base64: string): string {
  return `data:image/png;base64,${base64}`;
}

export async function loadIcons(): Promise<IconResult<AppIcons>> {
  try {
    const { data, error, response } = await api.GET("/api/app-icons");
    return data
      ? { ok: true, value: data }
      : { ok: false, reason: readFailure(error, response) };
  } catch {
    return unreachable();
  }
}

/** Sends the file; the service checks it, resizes it and answers with what it stored. */
export async function uploadIcon(file: File): Promise<IconResult<string>> {
  if (file.size > MAX_ICON_BYTES) {
    return { ok: false, reason: "That file is larger than 8 MB." };
  }
  try {
    const { data, error, response } = await api.PUT("/api/app-icons/custom", {
      // The file goes as it is; the typed body is a byte list, which the serializer skips.
      body: file as unknown as number[],
      bodySerializer: (body: unknown) => body as BodyInit,
      headers: { "Content-Type": "application/octet-stream" },
    });
    return data
      ? { ok: true, value: data.png_base64 }
      : { ok: false, reason: readFailure(error, response) };
  } catch {
    return unreachable();
  }
}

export async function removeIcon(): Promise<IconResult<null>> {
  try {
    const { error, response } = await api.DELETE("/api/app-icons/custom");
    return response.ok
      ? { ok: true, value: null }
      : { ok: false, reason: readFailure(error, response) };
  } catch {
    return unreachable();
  }
}
