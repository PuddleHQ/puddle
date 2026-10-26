// SPDX-License-Identifier: GPL-3.0-or-later
import { sentence } from "#lib/rules/model.ts";
import {
  globalSettings,
  type SaveResult,
} from "#lib/stores/global-settings.svelte.ts";
import {
  applyPalette,
  customFrom,
  isPaletteChoice,
  loadPalette,
  NO_CUSTOM,
  savePalette,
  type Custom,
  type PaletteChoice,
} from "./palette.ts";
import { browserStorage } from "./theme.ts";

/**
 * The reactive palette, kept the way the theme is: the stored choice and colours live in Puddle's
 * settings and a localStorage hint is applied before the first paint. `init()` once in the root
 * layout, then `sync()`.
 */
class PaletteStore {
  choice = $state<PaletteChoice>("pond");
  custom = $state.raw<Custom>({ ...NO_CUSTOM });
  /** Counts the changes made, so `sync()` can tell one came in while it read. */
  #changed = 0;

  init(): void {
    const hint = loadPalette(browserStorage());
    this.#apply(hint.choice, hint.custom);
  }

  /** Applies the palette stored in the settings, once they are read. */
  async sync(): Promise<void> {
    const changed = this.#changed;
    await globalSettings.load(true);
    const view = globalSettings.view;
    if (!view) return; // not readable: the hint stays
    if (this.#changed !== changed) return; // changed meanwhile: newer than what was read
    const stored = view.ui.palette ?? "pond";
    this.#apply(
      isPaletteChoice(stored) ? stored : "pond",
      customFrom({
        accent: view.ui.custom_accent,
        background: view.ui.custom_background,
        surface: view.ui.custom_surface,
        text: view.ui.custom_text,
      }),
    );
  }

  /** Applies a palette at once and stores the choice in the settings (the colours stay as they are stored). */
  async set(choice: PaletteChoice): Promise<SaveResult> {
    this.preview(choice, this.custom);
    if (!globalSettings.view) await globalSettings.load(true);
    if (!globalSettings.view) {
      return {
        ok: false,
        message: globalSettings.reason ?? "Settings aren't loaded.",
      };
    }
    const result = await globalSettings.save({ ui: { palette: choice } });
    return result.ok
      ? result
      : { ok: false, message: sentence(result.message) };
  }

  /** Applies a palette without storing it: the Settings screen shows it until Save or Discard. */
  preview(choice: PaletteChoice, custom: Custom): void {
    this.#changed++;
    this.#apply(choice, custom);
  }

  #apply(choice: PaletteChoice, custom: Custom): void {
    this.choice = choice;
    this.custom = custom;
    savePalette(browserStorage(), { choice, custom });
    applyPalette(document.documentElement, choice, custom);
  }
}

export const palette = new PaletteStore();
