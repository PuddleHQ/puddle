// SPDX-License-Identifier: GPL-3.0-or-later
import { sentence } from "#lib/rules/model.ts";
import {
  globalSettings,
  type SaveResult,
} from "#lib/stores/global-settings.svelte.ts";
import {
  applyTheme,
  browserStorage,
  isThemeChoice,
  loadTheme,
  saveTheme,
  type ThemeChoice,
} from "./theme.ts";

/**
 * The reactive theme choice. The stored choice lives in Puddle's settings (the API's address
 * changes on every launch, so the page can't keep it); localStorage only holds a hint that is
 * applied before the first paint. `init()` once in the root layout, then `sync()`.
 */
class ThemeState {
  choice = $state<ThemeChoice>("system");
  /** Counts the choices made, so `sync()` can tell one came in while it read. */
  #chosen = 0;

  init(): void {
    this.choice = loadTheme(browserStorage());
    applyTheme(document.documentElement, this.choice);
  }

  /** Applies the choice stored in the settings, once they are read. */
  async sync(): Promise<void> {
    const chosen = this.#chosen;
    await globalSettings.load(true);
    const view = globalSettings.view;
    if (!view) return; // not readable: the hint stays
    if (this.#chosen !== chosen) return; // chosen meanwhile: newer than what was read
    const stored = view.ui.theme ?? "system";
    if (isThemeChoice(stored)) this.#apply(stored);
  }

  /** Applies a choice at once and stores it in the settings; on failure it still applies this time and the result says why it was not saved. */
  async set(choice: ThemeChoice): Promise<SaveResult> {
    this.preview(choice);
    if (!globalSettings.view) await globalSettings.load(true);
    if (!globalSettings.view) {
      return {
        ok: false,
        message: globalSettings.reason ?? "Settings aren't loaded.",
      };
    }
    const result = await globalSettings.save({ ui: { theme: choice } });
    return result.ok
      ? result
      : { ok: false, message: sentence(result.message) };
  }

  /** Applies a choice without storing it: the Settings screen shows it until Save or Discard. */
  preview(choice: ThemeChoice): void {
    this.#chosen++;
    this.#apply(choice);
  }

  #apply(choice: ThemeChoice): void {
    this.choice = choice;
    saveTheme(browserStorage(), choice);
    applyTheme(document.documentElement, choice);
  }
}

export const theme = new ThemeState();
