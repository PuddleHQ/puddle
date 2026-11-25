// SPDX-License-Identifier: AGPL-3.0-or-later
// What the image a new workspace gets by default says it is: one line the host reads from the
// image's own labels. Nothing about the image is written in the page.
import { api as defaultApi, type ApiClient } from "#lib/api/client.ts";

export class DefaultImage {
  /** The line, or `null` while the host has not read the image's labels (or they say nothing). */
  description = $state<string | null>(null);
  readonly #api: Pick<ApiClient, "GET">;

  constructor(api: Pick<ApiClient, "GET"> = defaultApi) {
    this.#api = api;
  }

  /** Reads the line. A failed read keeps what is on screen: the line is a nicety, never an error. */
  async load(): Promise<void> {
    try {
      const { data } = await this.#api.GET("/api/workspaces/default-image");
      if (data) this.description = data.description;
    } catch {
      // The form works without the line.
    }
  }
}

export const defaultImage = new DefaultImage();
