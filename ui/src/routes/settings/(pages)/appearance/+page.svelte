<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import AppIconSettings from "#lib/components/AppIconSettings.svelte";
  import PaletteSettings from "#lib/components/PaletteSettings.svelte";
  import SettingRow from "#lib/components/SettingRow.svelte";
  import SelectRow from "#lib/components/SelectRow.svelte";
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import { requireSaver } from "#lib/settings/context.ts";
  import { saveUi } from "#lib/settings/apply.ts";
  import type { AppIconChoice } from "#lib/settings/app-icon.ts";
  import type { Ui } from "#lib/settings/model.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";
  import { density } from "#lib/theme/density.svelte.ts";
  import { isDensityChoice } from "#lib/theme/density.ts";
  import { palette } from "#lib/theme/palette.svelte.ts";
  import {
    customFrom,
    isPaletteChoice,
    NO_CUSTOM,
    type Custom,
    type CustomColour,
    type PaletteChoice,
  } from "#lib/theme/palette.ts";
  import { theme } from "#lib/theme/theme.svelte.ts";
  import { isThemeChoice } from "#lib/theme/theme.ts";

  const saver = requireSaver();

  const COLOUR_FIELD = {
    accent: "custom_accent",
    background: "custom_background",
    surface: "custom_surface",
    text: "custom_text",
  } as const;

  /** The custom colours a settings document holds. */
  function storedColours(ui: Ui): Custom {
    return customFrom({
      accent: ui.custom_accent,
      background: ui.custom_background,
      surface: ui.custom_surface,
      text: ui.custom_text,
    });
  }

  /** Puts the theme, density and colours the settings hold on screen: after a save, an undo or a refusal. */
  function showStored() {
    const ui = store.view?.ui;
    if (!ui) return;
    const stored = ui.theme ?? "system";
    if (isThemeChoice(stored)) theme.preview(stored);
    const density_ = ui.density ?? "comfortable";
    if (isDensityChoice(density_)) density.preview(density_);
    const stored_palette = ui.palette ?? "pond";
    palette.preview(
      isPaletteChoice(stored_palette) ? stored_palette : "pond",
      storedColours(ui),
    );
  }

  // What is stored is what shows, so an undo (or a refused save) puts the look back too.
  $effect(() => {
    void store.view?.ui;
    showStored();
  });

  async function chooseTheme(value: string) {
    const ui = store.view?.ui;
    if (!ui || !isThemeChoice(value)) return false;
    theme.preview(value);
    const ok = await saveUi(
      saver,
      "set-theme",
      "theme",
      value,
      ui.theme ?? "system",
    );
    if (!ok) showStored();
    return ok;
  }

  async function chooseDensity(value: string) {
    const ui = store.view?.ui;
    if (!ui || !isDensityChoice(value)) return false;
    density.preview(value);
    const ok = await saveUi(
      saver,
      "set-density",
      "density",
      value,
      ui.density ?? "comfortable",
    );
    if (!ok) showStored();
    return ok;
  }

  async function choosePalette(choice: PaletteChoice) {
    const ui = store.view?.ui;
    if (!ui) return;
    palette.preview(choice, storedColours(ui));
    const ok = await saveUi(
      saver,
      "set-palette",
      "palette",
      choice,
      ui.palette ?? "pond",
    );
    if (!ok) showStored();
  }

  /** A colour being dragged: shown, not stored. */
  function previewColour(colour: CustomColour, value: string) {
    const ui = store.view?.ui;
    const shown = ui ? storedColours(ui) : { ...NO_CUSTOM };
    palette.preview(palette.choice, customFrom({ ...shown, [colour]: value }));
  }

  async function chooseColour(colour: CustomColour, value: string | null) {
    const ui = store.view?.ui;
    if (!ui) return;
    const field = COLOUR_FIELD[colour];
    const ok = await saveUi(
      saver,
      "set-palette",
      field,
      value,
      ui[field] ?? null,
    );
    if (!ok) showStored();
  }

  async function chooseIcon(choice: AppIconChoice) {
    const ui = store.view?.ui;
    if (!ui) return;
    await saveUi(
      saver,
      "set-app-icon",
      "app_icon",
      choice,
      ui.app_icon ?? "duck",
    );
  }
</script>

{#if store.view}
  {@const view = store.view}
  <SettingsGroup title="Appearance">
    <SelectRow
      id="set-theme"
      label="Theme"
      desc="Follows your system unless you pick one."
      value={theme.choice}
      options={[
        { value: "system", label: "System" },
        { value: "light", label: "Light" },
        { value: "dark", label: "Dark" },
      ]}
      onchange={chooseTheme}
    />
    <SelectRow
      id="set-density"
      label="Density"
      desc="Compact fits more on screen; buttons stay easy to hit."
      value={density.choice}
      options={[
        { value: "comfortable", label: "Comfortable" },
        { value: "compact", label: "Compact" },
      ]}
      onchange={chooseDensity}
    />
    <PaletteSettings
      choice={palette.choice}
      custom={palette.custom}
      theme={theme.choice}
      onchoose={(choice) => void choosePalette(choice)}
      onpreview={previewColour}
      onpick={(colour, value) => void chooseColour(colour, value)}
    />
    <SettingRow
      id="set-app-icon"
      label="App icon"
      desc="On Puddle's window, the taskbar and the tray. The duck is the default."
      plain
      stacked
    >
      <AppIconSettings
        choice={view.ui.app_icon ?? "duck"}
        stored={view.ui.app_icon ?? "duck"}
        onchoose={(choice) => void chooseIcon(choice)}
      />
    </SettingRow>
  </SettingsGroup>
{/if}
