<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import UnknownFields from "#lib/components/UnknownFields.svelte";
  import { onDestroy, onMount } from "svelte";
  import { beforeNavigate } from "$app/navigation";
  import DirectSshDialog from "#lib/components/DirectSshDialog.svelte";
  import MicrosoftServerDialog from "#lib/components/MicrosoftServerDialog.svelte";
  import AppIconSettings from "#lib/components/AppIconSettings.svelte";
  import GlobalNeverDecrypt from "#lib/components/GlobalNeverDecrypt.svelte";
  import LearnedLimitsSetting from "#lib/components/LearnedLimitsSetting.svelte";
  import Toast from "#lib/components/Toast.svelte";
  import EnvironmentTable from "#lib/components/EnvironmentTable.svelte";
  import { EnvironmentStore } from "#lib/stores/environment.svelte.ts";
  import { SECRET_NEVER_IN } from "#lib/environment/model.ts";
  import { api } from "#lib/api/client.ts";
  import { LOCAL_LABELS, type LocalCategory } from "#lib/decision/local.ts";
  import { CPU_COUNTS, CPUS_NOTE, cpuLabel } from "#lib/settings/cpus.ts";
  import {
    EMPTY_DRAFT,
    isDirty,
    restartNames,
    RESTART_NOTE,
    savePatch,
    setLayer,
    setServer,
    setToggle,
    setUi,
    type Draft,
    type LayerKey,
  } from "#lib/settings/draft.ts";
  import type { AppIconChoice } from "#lib/settings/app-icon.ts";
  import {
    CLOSE_OPTIONS,
    LOCAL_EXPLAIN,
    memorySizes,
    microsoftChosenToast,
    needsMicrosoftConsent,
    parseGrace,
    serverNote,
    serverOf,
    type CloseBehaviour,
    type GlobalView,
    type Layer,
    type ServerChoice,
    type Ui,
  } from "#lib/settings/model.ts";
  import { globalSettings as store } from "#lib/stores/global-settings.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
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
  import PaletteSettings from "#lib/components/PaletteSettings.svelte";
  import { theme } from "#lib/theme/theme.svelte.ts";
  import {
    DIRECT_SSH_HINT,
    DIRECT_SSH_LABEL,
    TRUST_WORDS_GLOBAL,
  } from "#lib/workspaces/direct-ssh.ts";
  import { isThemeChoice } from "#lib/theme/theme.ts";
  import "#lib/theme/controls.css";

  const SECTIONS: [string, string][] = [
    ["s-app", "Appearance"],
    ["s-icon", "App icon"],
    ["s-notify", "Notifications"],
    ["s-inbox", "Inbox"],
    ["s-ws", "Workspaces"],
    ["s-net", "Network"],
    ["s-vsc", "Browser VS Code"],
    ["s-git", "Git and credentials"],
    ["s-env", "Environment"],
    ["s-priv", "Privacy"],
    ["s-about", "About"],
  ];
  const CATEGORIES: LocalCategory[] = [
    "loopback",
    "private",
    "link_local",
    "metadata",
    "special",
  ];

  let saved = $state<string | null>(null);
  let problem = $state<string | null>(null);
  let graceError = $state<string | null>(null);
  let trustOpen = $state(false);
  let popupOpen = $state(false);
  let popupBusy = $state(false);
  let withdrawing = $state(false);
  let popupError = $state<string | null>(null);
  let version = $state<string | null>(null);
  let versionAsked = $state(false);
  let notices = $state<string | null>(null);
  let noticesFailed = $state(false);

  const env = new EnvironmentStore();
  onMount(() => env.start({ kind: "global" }));

  onMount(() => {
    void store.load(store.view !== null);
    void api
      .GET("/api/health")
      .then((r) => (version = r.data?.version ?? null))
      .catch(() => undefined)
      .finally(() => (versionAsked = true));
  });

  /** The edits not saved yet; Save sends them as one change, Discard drops them. */
  let draft = $state.raw<Draft>(EMPTY_DRAFT);
  const dirty = $derived(isDirty(draft));
  let saving = $state(false);

  /** Replaces the draft after an edit; any earlier "saved" or "refused" line is no longer true. */
  function edit(next: Draft) {
    draft = next;
    saved = null;
    problem = null;
  }

  /** Puts the theme and density the settings hold back on screen (an unsaved preview ends). */
  function showStored() {
    const view = store.view;
    if (!view) return;
    const stored = view.ui.theme ?? "system";
    if (isThemeChoice(stored)) theme.preview(stored);
    const density_ = view.ui.density ?? "comfortable";
    if (isDensityChoice(density_)) density.preview(density_);
    const stored_palette = view.ui.palette ?? "pond";
    palette.preview(
      isPaletteChoice(stored_palette) ? stored_palette : "pond",
      storedColours(view.ui),
    );
  }

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

  /** The palette's colours as the screen shows them: an unsaved edit (a reset is one too) over what is stored. */
  function shownColours(): Custom {
    const view = store.view;
    const colours = view ? storedColours(view.ui) : { ...NO_CUSTOM };
    for (const key of Object.keys(COLOUR_FIELD) as CustomColour[]) {
      const field = COLOUR_FIELD[key];
      if (field in draft.ui)
        colours[key] = customFrom({ [key]: draft.ui[field] })[key];
    }
    return colours;
  }

  function discard() {
    draft = EMPTY_DRAFT;
    graceError = null;
    saved = null;
    problem = null;
    showStored();
  }

  async function saveAll() {
    const view = store.view;
    if (!view || !dirty || graceError || saving) return;
    saving = true;
    saved = null;
    problem = null;
    const names = restartNames(draft);
    const result = await store.save(savePatch(view, draft));
    saving = false;
    if (result.ok) {
      draft = EMPTY_DRAFT;
      saved =
        names.length > 0
          ? `Settings saved. ${names.join(" and ")} apply when a workspace next starts.`
          : "Settings saved.";
    } else {
      // The draft stays, so nothing typed is lost; Discard goes back to what is stored.
      problem = result.message;
    }
  }

  beforeNavigate((navigation) => {
    if (!dirty || navigation.willUnload) return;
    navigation.cancel();
    problem = "You have unsaved changes. Save or discard them first.";
  });

  onDestroy(() => {
    // Leaving with a previewed theme must not leave it on screen.
    if (isDirty(draft)) showStored();
  });

  const layerValue = <K extends LayerKey>(
    key: K,
    stored: NonNullable<Layer[K]>,
  ) => (draft.layer[key] ?? stored) as NonNullable<Layer[K]>;
  const editLayer = <K extends LayerKey>(
    key: K,
    value: NonNullable<Layer[K]>,
    stored: NonNullable<Layer[K]>,
  ) => edit(setLayer(draft, key, value, stored));
  /** The preferences that are plain on/off switches. */
  type BoolPref =
    | "notifications"
    | "minimize_to_tray"
    | "notify_only_unfocused"
    | "sound"
    | "badge"
    | "notify_oom"
    | "notify_unexpected_stop"
    | "confirm_global_rules"
    | "inbox_shortcuts";
  /** What the control shows: the unsaved edit, else what is stored, else Puddle's default. */
  const uiValue = <K extends keyof Ui>(key: K, fallback: NonNullable<Ui[K]>) =>
    (draft.ui[key] ?? store.view?.ui[key] ?? fallback) as NonNullable<Ui[K]>;

  const editUi = <K extends keyof Ui>(
    key: K,
    value: NonNullable<Ui[K]>,
    stored: NonNullable<Ui[K]>,
  ) => edit(setUi(draft, key, value, stored));

  function chooseTheme(select: HTMLSelectElement) {
    const value = select.value;
    const view = store.view;
    if (!isThemeChoice(value) || !view) return;
    theme.preview(value);
    edit(setUi(draft, "theme", value, view.ui.theme ?? "system"));
  }

  function chooseDensity(select: HTMLSelectElement) {
    const value = select.value;
    const view = store.view;
    if (!isDensityChoice(value) || !view) return;
    density.preview(value);
    edit(setUi(draft, "density", value, view.ui.density ?? "comfortable"));
  }

  function choosePalette(choice: PaletteChoice) {
    const view = store.view;
    if (!view) return;
    palette.preview(choice, shownColours());
    edit(setUi(draft, "palette", choice, view.ui.palette ?? "pond"));
  }

  function chooseColour(colour: CustomColour, value: string | null) {
    const view = store.view;
    if (!view) return;
    const field = COLOUR_FIELD[colour];
    const next = setUi(draft, field, value, view.ui[field] ?? null);
    const shown = { ...shownColours(), [colour]: value } satisfies Custom;
    palette.preview(palette.choice, customFrom(shown));
    edit(next);
  }

  function chooseIcon(choice: AppIconChoice) {
    const view = store.view;
    if (!view) return;
    edit(setUi(draft, "app_icon", choice, view.ui.app_icon ?? "duck"));
  }

  function chooseServer(select: HTMLSelectElement) {
    const view = store.view;
    const consent = store.consents?.vscode_server;
    if (!view || !consent) return;
    const choice = select.value as ServerChoice;
    if (choice === "microsoft" && needsMicrosoftConsent(consent)) {
      select.value = shownServer(view);
      popupError = null;
      popupOpen = true;
      return;
    }
    edit(setServer(draft, "server", choice, serverOf(view)));
  }

  async function accept(telemetry: boolean) {
    popupBusy = true;
    popupError = null;
    const result = await store.grantMicrosoft(telemetry);
    popupBusy = false;
    if (result.ok) {
      popupOpen = false;
      // The popup saved the server and the telemetry choice itself: no draft of those remains.
      draft = { ...draft, server: {} };
      saved = "Microsoft's server chosen.";
      toasts.push(microsoftChosenToast(telemetry), { ms: 12_000 });
    } else {
      popupError = result.message;
    }
  }

  async function withdraw() {
    withdrawing = true;
    problem = null;
    const result = await store.withdrawMicrosoft();
    withdrawing = false;
    if (result.ok) {
      // The API put the server choice back to code-server: no draft of it remains.
      draft = { ...draft, server: {} };
      saved =
        "Consent withdrawn. Workspaces use code-server when they next start.";
    } else {
      problem = result.message;
    }
  }

  function editGrace(input: HTMLInputElement, stored: number) {
    const parsed = parseGrace(input.value);
    if (!parsed.ok) {
      graceError = parsed.message;
      return;
    }
    graceError = null;
    editLayer("reconnection_grace", parsed.secs, stored);
  }

  /** The server the select shows: the draft's choice, else the stored one. */
  const shownServer = (view: GlobalView): ServerChoice =>
    draft.server.server ?? serverOf(view);

  async function loadNotices() {
    if (notices !== null || noticesFailed) return;
    try {
      notices = (await import("../../../THIRD-PARTY-NOTICES.txt?raw")).default;
    } catch {
      noticesFailed = true;
    }
  }

  const when = (ms: number) =>
    new Intl.DateTimeFormat(undefined, { dateStyle: "medium" }).format(ms);
</script>

{#snippet uiToggle(
  key: BoolPref,
  id: string,
  label: string,
  desc: string,
  fallback: boolean,
  disabled = false,
)}
  <div class="setting">
    <div class="grow">
      <label for={id}>{label}</label>
      <p class="desc" id="{id}-desc">{desc}</p>
    </div>
    <input
      {id}
      type="checkbox"
      aria-describedby="{id}-desc"
      {disabled}
      checked={uiValue(key, fallback)}
      onchange={(e) =>
        store.view &&
        editUi(key, e.currentTarget.checked, store.view.ui[key] ?? fallback)}
    />
  </div>
{/snippet}

<h1 tabindex="-1">Settings</h1>
<p class="sub">
  Global values. A workspace can override most of them in its own Settings tab.
</p>

{#if store.status === "loading" && !store.view}
  <p class="muted">Loading settings&hellip;</p>
{:else if store.status === "newer"}
  <p class="error" role="alert">
    These settings were saved by a newer Puddle. Update Puddle to change them.
  </p>
{:else if !store.view || !store.consents}
  <div role="alert">
    <p class="muted">Couldn't read Puddle's settings.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
  </div>
{:else}
  {@const view = store.view}
  {@const eff = view.effective}
  {@const server = shownServer(view)}
  {@const consent = store.consents.vscode_server}
  <UnknownFields fields={view.unknown_fields} />
  <div class="layout">
    <nav aria-label="Settings sections">
      {#each SECTIONS as [id, name] (id)}
        <a href="#{id}">{name}</a>
      {/each}
    </nav>
    <div class="sections">
      <section class="card" id="s-app" aria-labelledby="h-app">
        <h2 id="h-app">Appearance</h2>
        <div class="setting">
          <div class="grow">
            <label for="set-theme">Theme</label>
            <p class="desc">Follows your system unless you pick one.</p>
          </div>
          <select
            id="set-theme"
            value={theme.choice}
            onchange={(e) => chooseTheme(e.currentTarget)}
          >
            <option value="system">System</option>
            <option value="light">Light</option>
            <option value="dark">Dark</option>
          </select>
        </div>
        <div class="setting">
          <div class="grow">
            <label for="set-density">Density</label>
            <p class="desc">
              Compact tightens the spacing so more fits on screen. Buttons and
              fields stay easy to hit.
            </p>
          </div>
          <select
            id="set-density"
            value={density.choice}
            onchange={(e) => chooseDensity(e.currentTarget)}
          >
            <option value="comfortable">Comfortable</option>
            <option value="compact">Compact</option>
          </select>
        </div>
        <PaletteSettings
          choice={palette.choice}
          custom={palette.custom}
          theme={theme.choice}
          onchoose={choosePalette}
          onpick={chooseColour}
        />
      </section>

      <section class="card" id="s-icon" aria-labelledby="h-icon">
        <h2 id="h-icon">App icon</h2>
        <AppIconSettings
          choice={draft.ui.app_icon ?? view.ui.app_icon ?? "duck"}
          stored={view.ui.app_icon ?? "duck"}
          onchoose={chooseIcon}
        />
      </section>

      <section class="card" id="s-notify" aria-labelledby="h-notify">
        <h2 id="h-notify">Notifications</h2>
        {@render uiToggle(
          "notifications",
          "set-notify",
          "System notifications for new requests",
          "At most one notification every 10 seconds per workspace, naming what it asked for. Clicking it opens the request.",
          true,
        )}
        {@render uiToggle(
          "notify_only_unfocused",
          "set-unfocused",
          "Only when Puddle is not the window in front",
          "Off: a notification also shows while you are looking at Puddle.",
          true,
          !uiValue("notifications", true),
        )}
        {@render uiToggle(
          "sound",
          "set-sound",
          "Sound",
          "Play the system notification sound.",
          false,
        )}
        {@render uiToggle(
          "badge",
          "set-badge",
          "Show the number of waiting requests",
          "On the tray icon, the taskbar and the window title.",
          true,
        )}
        <h3>Also notify when</h3>
        {@render uiToggle(
          "notify_oom",
          "set-oom",
          "A workspace runs out of memory",
          "It was stopped because it used all the memory it was given.",
          true,
        )}
        {@render uiToggle(
          "notify_unexpected_stop",
          "set-stopped",
          "A workspace stops on its own",
          "It stopped or crashed without you asking.",
          true,
        )}
        <div class="setting">
          <div class="grow">
            <label for="set-close">Closing the window</label>
            <p class="desc">
              Keeping Puddle in the tray lets workspaces keep running, so builds
              carry on. Quitting from the tray menu stops every workspace. A
              desktop with no tray icon (some Linux desktops) minimizes the
              window instead.
            </p>
          </div>
          <select
            id="set-close"
            value={uiValue("close_behaviour", "tray")}
            onchange={(e) =>
              editUi(
                "close_behaviour",
                e.currentTarget.value as CloseBehaviour,
                view.ui.close_behaviour ?? "tray",
              )}
          >
            {#each CLOSE_OPTIONS as option (option.value)}
              <option value={option.value}>{option.label}</option>
            {/each}
          </select>
        </div>
        {@render uiToggle(
          "minimize_to_tray",
          "set-minimize",
          "Minimizing hides Puddle in the tray",
          "Off: minimizing leaves Puddle in the taskbar. Without a tray icon on your desktop, minimizing always does.",
          false,
        )}
      </section>

      <section class="card" id="s-inbox" aria-labelledby="h-inbox">
        <h2 id="h-inbox">Inbox</h2>
        {@render uiToggle(
          "confirm_global_rules",
          "set-confirm-global",
          "Confirm before a rule affects every workspace",
          "Allowing or denying for every workspace asks first. Turn this off to decide at once; you can still undo right after.",
          true,
        )}
        {@render uiToggle(
          "inbox_shortcuts",
          "set-inbox-keys",
          "Keyboard shortcuts in the inbox",
          "A allows, D denies, J and K move between requests. Off by default so a stray key press decides nothing; Tab, Enter and Space always work.",
          false,
        )}
      </section>

      <section class="card" id="s-ws" aria-labelledby="h-ws">
        <h2 id="h-ws">Workspaces</h2>
        <div class="setting">
          <div class="grow">
            <label for="set-memory">Default memory</label>
            <span class="restart">{RESTART_NOTE}</span>
            <p class="desc">
              Used by every workspace that has no value of its own.
            </p>
          </div>
          <select
            id="set-memory"
            value={String(layerValue("memory", eff.memory.value))}
            onchange={(e) =>
              editLayer(
                "memory",
                Number(e.currentTarget.value),
                eff.memory.value,
              )}
          >
            {#each memorySizes(eff.memory.value) as size (size.value)}
              <option value={String(size.value)}>{size.label}</option>
            {/each}
          </select>
        </div>
        <div class="setting">
          <div class="grow">
            <label for="set-cpus">Default CPUs</label>
            <span class="restart">{RESTART_NOTE}</span>
            <p class="desc" id="set-cpus-desc">
              Used by every workspace that has no count of its own. {CPUS_NOTE}
            </p>
          </div>
          <select
            id="set-cpus"
            aria-describedby="set-cpus-desc"
            value={String(layerValue("cpus", eff.cpus.value))}
            onchange={(e) =>
              editLayer("cpus", Number(e.currentTarget.value), eff.cpus.value)}
          >
            {#each CPU_COUNTS as count (count)}
              <option value={String(count)}>{cpuLabel(count)}</option>
            {/each}
          </select>
        </div>
        <div class="setting">
          <div class="grow">
            <label for="set-direct-ssh"
              >{DIRECT_SSH_LABEL} for new workspaces</label
            >
            <p class="desc" id="set-direct-ssh-desc">
              {DIRECT_SSH_HINT} Off by default. A workspace can set its own switch.
            </p>
          </div>
          <input
            id="set-direct-ssh"
            type="checkbox"
            aria-describedby="set-direct-ssh-desc"
            checked={layerValue("direct_ssh", eff.direct_ssh.value)}
            onchange={(e) => {
              const box = e.currentTarget;
              if (box.checked) {
                // Turning it on says the trust text first; the dialog's button sets the draft.
                box.checked = layerValue("direct_ssh", eff.direct_ssh.value);
                trustOpen = true;
                return;
              }
              editLayer("direct_ssh", false, eff.direct_ssh.value);
            }}
          />
        </div>
        <div class="setting">
          <div class="grow">
            <label for="set-capture">Keep logins made in workspaces</label>
            <span class="restart">{RESTART_NOTE}</span>
            <p class="desc" id="set-capture-desc">
              When you sign in to Claude Code, GitHub or Copilot inside a
              workspace, Puddle keeps the real token on this computer and the
              workspace gets a stand-in that works only through Puddle. On by
              default. A workspace can set its own switch; a change takes effect
              when a workspace next starts, and turning it off leaves the logins
              Puddle already kept unused.
            </p>
          </div>
          <input
            id="set-capture"
            type="checkbox"
            aria-describedby="set-capture-desc"
            checked={layerValue("capture_logins", eff.capture_logins.value)}
            onchange={(e) =>
              editLayer(
                "capture_logins",
                e.currentTarget.checked,
                eff.capture_logins.value,
              )}
          />
        </div>
      </section>

      <section class="card" id="s-net" aria-labelledby="h-net">
        <h2 id="h-net">Network</h2>
        <h3>Network health</h3>
        <div class="setting">
          <div class="grow">
            <p class="desc">
              The proxy Puddle found, how it signs in, and the company
              certificates workspaces get.
            </p>
          </div>
          <a class="btn" href="/settings/network-health"
            >Details<span class="visually-hidden">
              about network health</span
            ></a
          >
        </div>
        <h3>System check</h3>
        <div class="setting">
          <div class="grow">
            <p class="desc">
              Checks this computer again: virtualization, the hypervisor, the
              bundled runtime and a test start of a tiny workspace.
            </p>
          </div>
          <a class="btn" href="/welcome/check?from=settings"
            >Run the system check again</a
          >
        </div>
        <h3>Local destinations</h3>
        <p class="desc">
          Off by default. Turning one on makes those destinations
          <b>approvable</b>: each one still needs a rule or your approval.
          Nothing is allowed by turning it on.
        </p>
        {#each CATEGORIES as category (category)}
          <div class="setting">
            <div class="grow">
              <label for="set-local-{category}">{LOCAL_LABELS[category]}</label>
              <p class="desc">{LOCAL_EXPLAIN[category]}</p>
            </div>
            <input
              id="set-local-{category}"
              type="checkbox"
              checked={draft.toggles[category] ??
                eff.local_toggles[category].value}
              onchange={(e) =>
                edit(
                  setToggle(
                    draft,
                    category,
                    e.currentTarget.checked,
                    eff.local_toggles[category].value,
                  ),
                )}
            />
          </div>
        {/each}
        <div class="setting">
          <div class="grow">
            <label for="set-wild">Let suffix rules reach local addresses</label>
            <p class="desc">
              Off: <span class="mono">.example.com</span> never matches a name that
              resolves to a local address; local destinations need an exact rule.
            </p>
          </div>
          <input
            id="set-wild"
            type="checkbox"
            checked={layerValue(
              "wildcards_reach_local",
              eff.wildcards_reach_local.value,
            )}
            onchange={(e) =>
              editLayer(
                "wildcards_reach_local",
                e.currentTarget.checked,
                eff.wildcards_reach_local.value,
              )}
          />
        </div>

        <GlobalNeverDecrypt />
        <LearnedLimitsSetting />
      </section>

      <section class="card" id="s-vsc" aria-labelledby="h-vsc">
        <h2 id="h-vsc">Browser VS Code</h2>
        <div class="setting">
          <div class="grow">
            <label for="set-server">Server</label>
            <p class="desc" id="set-server-note">{serverNote(server)}</p>
            {#if consent.state === "granted"}
              <p class="desc">
                You accepted Microsoft's terms on {when(consent.at)}.
              </p>
              <button
                type="button"
                class="btn"
                disabled={withdrawing}
                onclick={() => void withdraw()}>Withdraw consent</button
              >
            {/if}
          </div>
          <select
            id="set-server"
            aria-describedby="set-server-note"
            value={server}
            onchange={(e) => chooseServer(e.currentTarget)}
          >
            <option value="code_server">code-server (bundled)</option>
            <option value="microsoft">Microsoft's VS Code server</option>
          </select>
        </div>
        {#if server === "microsoft"}
          <div class="setting">
            <div class="grow">
              <label for="set-ms-telemetry">Send Microsoft telemetry</label>
              <p class="desc">
                Off adds <span class="mono">--disable-telemetry</span>.
              </p>
            </div>
            <input
              id="set-ms-telemetry"
              type="checkbox"
              checked={draft.server.telemetry ??
                view.vscode_server.telemetry ??
                false}
              onchange={(e) =>
                edit(
                  setServer(
                    draft,
                    "telemetry",
                    e.currentTarget.checked,
                    view.vscode_server.telemetry ?? false,
                  ),
                )}
            />
          </div>
        {/if}
        <div class="setting">
          <div class="grow">
            <label for="set-update">Update the server automatically</label>
            <p class="desc">
              Only switches when no window of that workspace is open.
            </p>
          </div>
          <input
            id="set-update"
            type="checkbox"
            checked={draft.server.auto_update ??
              view.vscode_server.auto_update ??
              true}
            onchange={(e) =>
              edit(
                setServer(
                  draft,
                  "auto_update",
                  e.currentTarget.checked,
                  view.vscode_server.auto_update ?? true,
                ),
              )}
          />
        </div>
        <div class="setting">
          <div class="grow">
            <label for="set-grace">Reconnection grace (seconds)</label>
            <p class="desc" id="set-grace-desc">
              How long a closed window's session is kept for a reconnect.
              {#if graceError}<span class="error" role="alert"
                  >{graceError}</span
                >{/if}
            </p>
          </div>
          <input
            id="set-grace"
            type="text"
            inputmode="numeric"
            class="narrow"
            aria-describedby="set-grace-desc"
            aria-invalid={graceError ? "true" : undefined}
            value={String(
              layerValue("reconnection_grace", eff.reconnection_grace.value),
            )}
            onchange={(e) =>
              editGrace(e.currentTarget, eff.reconnection_grace.value)}
          />
        </div>
        <div class="setting">
          <div class="grow">
            <label for="set-zoom">Zoom shortcuts</label>
            <p class="desc">Ctrl and plus or minus in workspace windows.</p>
          </div>
          <input
            id="set-zoom"
            type="checkbox"
            checked={layerValue("zoom_hotkeys", eff.zoom_hotkeys.value)}
            onchange={(e) =>
              editLayer(
                "zoom_hotkeys",
                e.currentTarget.checked,
                eff.zoom_hotkeys.value,
              )}
          />
        </div>
        <div class="setting">
          <div class="grow">
            <label for="set-clip">Clipboard reads by pages</label>
            <p class="desc">
              Typing Ctrl+V always works; this is for pages that read the
              clipboard by themselves.
            </p>
          </div>
          <select
            id="set-clip"
            value={layerValue("clipboard_read", eff.clipboard_read.value)}
            onchange={(e) =>
              editLayer(
                "clipboard_read",
                e.currentTarget.value as "ask" | "allow" | "deny",
                eff.clipboard_read.value,
              )}
          >
            <option value="ask">Ask once per workspace</option>
            <option value="allow">Always allow</option>
            <option value="deny">Never allow</option>
          </select>
        </div>
      </section>

      <section class="card" id="s-git" aria-labelledby="h-git">
        <h2 id="h-git">Git and credentials</h2>
        <p>
          The Git name and email, and the credentials Puddle adds to outgoing
          requests, are kept as identities. The secret of a credential never
          enters a workspace.
        </p>
        <p><a href="/identities">Manage identities</a></p>
      </section>

      <section class="card" id="s-env" aria-labelledby="h-env">
        <h2 id="h-env">Environment</h2>
        <p>
          Variables and secrets every workspace gets at its next start, unless
          the workspace has its own of the same name. Changes here are saved at
          once, not with the Save button above. {SECRET_NEVER_IN}
        </p>
        <EnvironmentTable store={env} label="Environment of every workspace" />
      </section>

      <section class="card" id="s-priv" aria-labelledby="h-priv">
        <h2 id="h-priv">Privacy</h2>
        <p>
          Puddle sends nothing about you or your use of it. There is no
          telemetry in this version, and Puddle makes no update check.
        </p>
        <p class="muted">
          Traffic from inside your workspaces (including editor and extension
          telemetry) goes through your rules like any other request.
        </p>
      </section>

      <section class="card" id="s-about" aria-labelledby="h-about">
        <h2 id="h-about">About</h2>
        <p>
          <b>Puddle{version ? ` ${version}` : ""}</b
          >{#if versionAsked && !version}
            <span class="muted">
              (version unavailable: Puddle's service didn't say)</span
            >{/if}
          <span class="muted"> &middot; GPL-3.0-or-later</span>
        </p>
        <details ontoggle={() => void loadNotices()}>
          <summary>Third-party licences</summary>
          {#if notices !== null}
            <!-- A scrollable region must be reachable by keyboard. -->
            <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
            <pre
              class="notices"
              tabindex="0"
              aria-label="Third-party licences">{notices}</pre>
          {:else if noticesFailed}
            <p class="error">Couldn't load the licence texts.</p>
          {:else}
            <p class="muted">Loading&hellip;</p>
          {/if}
        </details>
      </section>

      <div class="savebar" role="group" aria-label="Save settings">
        <div class="status" aria-live="polite">
          {#if problem}
            <p class="error" role="alert">{problem}</p>
          {:else if graceError && dirty}
            <p class="error">Fix the reconnection grace to save.</p>
          {:else if saved}
            <p class="saved">{saved}</p>
          {:else if dirty}
            <p>
              You have unsaved changes.
              {#if restartNames(draft).length > 0}{restartNames(draft).join(
                  " and ",
                )} apply when a workspace next starts.{/if}
            </p>
          {:else}
            <p class="muted">No unsaved changes.</p>
          {/if}
        </div>
        <button
          type="button"
          class="btn"
          disabled={!dirty || saving}
          onclick={discard}>Discard</button
        >
        <button
          type="button"
          class="btn primary"
          disabled={!dirty || saving || graceError !== null}
          onclick={() => void saveAll()}>Save</button
        >
      </div>
    </div>
  </div>
{/if}

<DirectSshDialog
  bind:open={trustOpen}
  words={TRUST_WORDS_GLOBAL}
  onConfirm={() => {
    const view = store.view;
    if (view) editLayer("direct_ssh", true, view.effective.direct_ssh.value);
  }}
/>

<!-- Each page that pushes a toast draws the queue; this one never did, so the toast after
     accepting Microsoft's terms was queued and never shown. -->
<Toast />

<MicrosoftServerDialog
  bind:open={popupOpen}
  busy={popupBusy}
  error={popupError}
  onAccept={(telemetry) => void accept(telemetry)}
/>

<style>
  h1 {
    margin: 0;
  }
  .sub,
  .muted,
  .desc {
    color: var(--color-text-muted);
  }
  .sub {
    margin: 0 0 var(--space-3);
  }
  .status {
    flex: 1;
    min-width: 0;
  }
  .status p,
  .desc {
    margin: 0;
  }
  .savebar {
    position: sticky;
    bottom: 0;
    display: flex;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    background: var(--color-surface-raised);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
  }
  .restart {
    margin-left: var(--space-2);
    font-size: var(--text-sm);
    color: var(--color-text-muted);
  }
  .desc {
    font-size: var(--text-sm);
  }
  .saved {
    color: var(--color-success);
  }
  .error {
    color: var(--color-danger);
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(0, 11rem) minmax(0, 1fr);
    gap: var(--space-5);
    align-items: start;
  }
  nav {
    position: sticky;
    top: var(--space-4);
    display: grid;
    gap: var(--space-1);
  }
  nav a {
    padding: var(--space-1) var(--space-2);
    border-radius: var(--radius-md);
    color: var(--color-text);
    text-decoration: none;
  }
  nav a:hover {
    background: var(--color-surface-raised);
  }
  .sections {
    display: grid;
    gap: var(--space-4);
    max-width: 48rem;
  }
  .card {
    display: grid;
    gap: var(--space-3);
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
    scroll-margin-top: var(--space-4);
  }
  h2 {
    margin: 0;
    font-size: var(--text-md);
  }
  h3 {
    margin: 0;
    font-size: var(--text-base, 1rem);
  }
  .card p {
    margin: 0;
  }
  .setting {
    display: flex;
    align-items: center;
    gap: var(--space-4);
  }
  .grow {
    flex: 1;
    min-width: 0;
  }
  label {
    font-weight: 600;
  }
  input[type="checkbox"] {
    width: 1.5rem;
    height: 1.5rem;
    flex: none;
  }
  select,
  input[type="text"] {
    min-height: var(--control-size);
    max-width: 18rem;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    background: var(--color-bg);
    color: var(--color-text);
    font: inherit;
  }
  input[aria-invalid="true"] {
    border-color: var(--color-danger);
  }
  .narrow {
    width: 6rem;
  }
  .notices {
    max-height: 20rem;
    overflow: auto;
    margin: var(--space-2) 0 0;
    padding: var(--space-3);
    background: var(--color-bg);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    white-space: pre-wrap;
  }
  @media (max-width: 760px) {
    .layout {
      grid-template-columns: minmax(0, 1fr);
    }
    nav {
      position: static;
      display: flex;
      flex-wrap: wrap;
    }
    .setting {
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
