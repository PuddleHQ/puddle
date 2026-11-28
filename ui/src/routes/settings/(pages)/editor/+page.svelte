<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import SettingsGroup from "#lib/components/SettingsGroup.svelte";
  import ValueRow from "#lib/components/ValueRow.svelte";
  import { saveLayer } from "#lib/settings/apply.ts";
  import { requireSaver } from "#lib/settings/context.ts";
  import {
    EXTENSIONS_DEFAULT,
    FONT_DEFAULT,
    CUSTOM_SETTINGS_DEFAULT,
    KEYBINDINGS_DEFAULT,
    THEME_DEFAULT,
    editorOf,
    extensionsText,
    fontSizeText,
    parseExtensions,
    parseFontSize,
    parseCustomSettings,
    parseKeybindings,
    parseTheme,
    type EditorField,
    type EditorSettings,
  } from "#lib/settings/editor.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";

  const saver = requireSaver();

  /** Saves one field of the editor group; the group goes as a whole, so the others stay as stored. */
  function save<K extends EditorField>(
    id: string,
    stored: EditorSettings,
    key: K,
    value: EditorSettings[K],
  ) {
    return saveLayer(saver, id, "editor", { ...stored, [key]: value }, stored);
  }
</script>

{#if store.view}
  {@const view = store.view}
  {@const stored = editorOf(view.workspace_defaults)}
  <SettingsGroup title="Editor defaults">
    <ValueRow
      id="set-editor-theme"
      label="Editor colour theme"
      desc="The theme's name as the editor lists it. Empty follows the system colour scheme. {THEME_DEFAULT}."
      how="A workspace can use its own theme on its settings page. The editor keeps any theme you pick inside it: a default is only applied again while you have not changed that setting yourself."
      value={stored.colour_theme ?? ""}
      width="wide"
      placeholder="Default Dark Modern"
      parse={parseTheme}
      onsave={(name) => save("set-editor-theme", stored, "colour_theme", name)}
    />
    <ValueRow
      id="set-editor-font-size"
      label="Font size (pixels)"
      desc="From 8 to 40. Empty keeps {FONT_DEFAULT.toLowerCase()}."
      value={fontSizeText(stored.font_size)}
      inputmode="numeric"
      parse={parseFontSize}
      onsave={(px) => save("set-editor-font-size", stored, "font_size", px)}
    />
    <ValueRow
      id="set-editor-extensions"
      label="Extensions to install"
      desc="One publisher.name id per line, for example rust-lang.rust-analyzer. Empty installs {EXTENSIONS_DEFAULT.toLowerCase()}."
      how="A workspace that lists its own extensions uses its list instead of this one. An extension you uninstall yourself is not put back."
      value={extensionsText(stored.extensions)}
      mono
      multiline
      parse={parseExtensions}
      onsave={(list) =>
        save(
          "set-editor-extensions",
          stored,
          "extensions",
          list.length === 0 ? null : list,
        )}
    />
    <ValueRow
      id="set-editor-keybindings"
      label="Key bindings"
      desc="A JSON array of key and command objects, the same shape as the editor's keybindings.json. Empty adds {KEYBINDINGS_DEFAULT.toLowerCase()}."
      value={stored.keybindings ?? ""}
      mono
      multiline
      placeholder={'[{"key": "ctrl+alt+t", "command": "workbench.action.terminal.toggleTerminal"}]'}
      parse={parseKeybindings}
      onsave={(text) =>
        save("set-editor-keybindings", stored, "keybindings", text)}
    />
    <ValueRow
      id="set-editor-custom-settings"
      label="Custom editor settings"
      desc="Your own editor settings as a JSON object, the shape of the editor's settings.json without comments, used as defaults for new workspaces. Empty adds {CUSTOM_SETTINGS_DEFAULT.toLowerCase()}."
      how="A setting you change inside a workspace is never overwritten. The theme and font size have their own fields above and are refused here."
      value={stored.settings ?? ""}
      mono
      multiline
      placeholder={'{"editor.tabSize": 2, "files.autoSave": "afterDelay"}'}
      parse={parseCustomSettings}
      onsave={(text) =>
        save("set-editor-custom-settings", stored, "settings", text)}
    />
  </SettingsGroup>
{/if}
