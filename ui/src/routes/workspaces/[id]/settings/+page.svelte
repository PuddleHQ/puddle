<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import Trash2 from "@lucide/svelte/icons/trash-2";
  import InterceptionSetting from "#lib/components/InterceptionSetting.svelte";
  import RootDiskControls from "#lib/components/RootDiskControls.svelte";
  import KeptLoginsList from "#lib/components/KeptLoginsList.svelte";
  import SessionUserSetting from "#lib/components/SessionUserSetting.svelte";
  import StartFolderSetting from "#lib/components/StartFolderSetting.svelte";
  import UnknownFields from "#lib/components/UnknownFields.svelte";
  import { page } from "$app/state";
  import {
    LOCAL_LABELS,
    type LocalCategory,
  } from "#lib/decision/local-labels.ts";
  import { CPUS_NOTE } from "#lib/settings/cpus.ts";
  import { KeptLogins } from "#lib/stores/kept-logins.svelte.ts";
  import { WorkspaceSettings } from "#lib/stores/workspace-settings.svelte.ts";
  import { workspaceActions as actions } from "#lib/stores/workspace-actions.svelte.ts";
  import { workspaces } from "#lib/stores/workspaces.svelte.ts";
  import {
    DIRECT_SSH_HINT,
    DIRECT_SSH_LABEL,
  } from "#lib/workspaces/direct-ssh.ts";
  import {
    canDelete,
    diskLabel,
    diskWarned,
    rootDiskLabel,
  } from "#lib/workspaces/model.ts";
  import {
    ALLOW_OPTIONS,
    CLIPBOARD_OPTIONS,
    SWITCH_OPTIONS,
    cpuChoices,
    fromOnOff,
    memoryChoices,
    onOff,
    sourceLabel,
    type Clipboard,
    type Layer,
    type Option,
    type Source,
  } from "#lib/workspaces/settings.ts";
  import "#lib/theme/controls.css";

  const id = $derived(page.params["id"] ?? "");
  const workspace = $derived(workspaces.list.find((w) => w.id === id));
  const settings = new WorkspaceSettings();
  const kept = new KeptLogins();

  let saved = $state<string | null>(null);
  let problem = $state<string | null>(null);
  let restartHint = $state(false);
  let cpusHint = $state(false);
  let captureHint = $state(false);
  const fid = $props.id();

  // Reload only for another workspace, not on every status change of this one.
  const wid = $derived(workspace?.id);
  $effect(() => {
    if (wid) void settings.load(wid);
  });

  // The kept logins come from Puddle's own record, so the list is cheap to read again when the
  // workspace starts or stops (a login made while it ran is recorded by then).
  const keptId = $derived(workspace?.id);
  const keptStatus = $derived(workspace?.status);
  $effect(() => {
    void keptStatus;
    if (keptId) void kept.load(keptId, true);
  });

  // Direct SSH can change from the connect step too: read the overrides again when it does.
  const directSsh = $derived(workspace?.direct_ssh);
  let lastDirectSsh: boolean | undefined;
  $effect(() => {
    const now = directSsh;
    if (lastDirectSsh !== undefined && now !== lastDirectSsh && wid) {
      void settings.load(wid, true);
    }
    lastDirectSsh = now;
  });

  const CATEGORIES: LocalCategory[] = [
    "loopback",
    "private",
    "company_network",
    "link_local",
    "metadata",
    "special",
  ];

  /** One line of the table: its value in effect, what the dropdown offers and how a choice is saved. */
  interface SettingRow {
    key: string;
    label: string;
    hint?: string;
    value: string;
    options: Option<string>[];
    source: Source;
    /** Saves a choice; false when it was refused, and the dropdown goes back to `value`. */
    pick: (choice: string) => Promise<boolean>;
    /** Follows the global setting (or Puddle's default) again. */
    reset: () => Promise<boolean>;
    resetLabel?: string;
  }
  interface Group {
    /** The anchor of the group, for a link from elsewhere. */
    anchor?: string;
    title: string;
    rows: SettingRow[];
  }

  /** Saves a patch of the overrides and says so; a refusal is shown and returns false. */
  async function save(patch: Partial<Layer>, what: string) {
    saved = null;
    problem = null;
    const result = await settings.change(patch);
    if (result.ok) saved = `${what} saved.`;
    else problem = result.message;
    return result.ok;
  }

  /** A restart note only for a change that waits for the next start of a running workspace. */
  function note(kind: "memory" | "cpus" | "capture" | null, ok: boolean) {
    const running = ok && workspace?.status === "running";
    restartHint = kind === "memory" && running;
    cpusHint = kind === "cpus" && running;
    captureHint = kind === "capture" && running;
  }

  /** On asks the trust text first; off and the global setting save at once. */
  async function saveDirectSsh(value: boolean | null) {
    if (value === true && workspace) {
      actions.requestDirectSsh(workspace, true);
      return false;
    }
    const ok = await save({ direct_ssh: value }, DIRECT_SSH_LABEL);
    if (ok) await workspaces.refresh();
    return ok;
  }

  async function saveTool(
    key: "mise" | "repo_code",
    value: boolean | null,
    what: string,
  ) {
    saved = null;
    problem = null;
    const result = await settings.changeTools({ [key]: value });
    if (result.ok) saved = `${what} saved.`;
    else problem = result.message;
    return result.ok;
  }

  /** A yes-or-no row: `save` takes true, false, or null to follow the global setting again. */
  function yesNo(
    key: string,
    label: string,
    resolved: { value: boolean; source: Source },
    options: Option<string>[],
    saveValue: (value: boolean | null) => Promise<boolean>,
    hint?: string,
  ): SettingRow {
    return {
      key,
      label,
      ...(hint ? { hint } : {}),
      value: onOff(resolved.value),
      options,
      source: resolved.source,
      pick: (choice) => saveValue(fromOnOff(choice)),
      reset: () => saveValue(null),
    };
  }

  const groups = $derived.by((): Group[] => {
    const effective = settings.effective;
    if (!effective) return [];
    const local = CATEGORIES.map((category) =>
      yesNo(
        `local-${category}`,
        LOCAL_LABELS[category],
        effective.local_toggles[category],
        ALLOW_OPTIONS,
        (value) => {
          const current = settings.overrides?.local_toggles;
          return current
            ? save(
                { local_toggles: { ...current, [category]: value } },
                LOCAL_LABELS[category],
              )
            : Promise.resolve(false);
        },
      ),
    );
    const out: Group[] = [
      {
        title: "Resources",
        rows: [
          {
            key: "memory",
            label: "Memory",
            hint: "A change applies at the next restart.",
            value: String(effective.memory.value),
            options: memoryChoices(effective.memory.value),
            source: effective.memory.source,
            pick: async (choice) => {
              const ok = await save({ memory: Number(choice) }, "Memory");
              note("memory", ok);
              return ok;
            },
            reset: async () => {
              const ok = await save({ memory: null }, "Memory");
              note("memory", ok);
              return ok;
            },
          },
          {
            key: "cpus",
            label: "CPUs",
            hint: CPUS_NOTE,
            value: String(effective.cpus.value),
            options: cpuChoices(settings.maxCpus, effective.cpus.value),
            source: effective.cpus.source,
            pick: async (choice) => {
              const ok = await save({ cpus: Number(choice) }, "CPUs");
              note("cpus", ok);
              return ok;
            },
            reset: async () => {
              const ok = await save({ cpus: null }, "CPUs");
              note("cpus", ok);
              return ok;
            },
          },
        ],
      },
      {
        title: "Access",
        rows: [
          yesNo(
            "direct-ssh",
            DIRECT_SSH_LABEL,
            effective.direct_ssh,
            ALLOW_OPTIONS,
            saveDirectSsh,
            `${DIRECT_SSH_HINT} Applies at once.`,
          ),
          {
            key: "clipboard",
            label: "Pages reading the clipboard",
            hint: "When a page in this workspace's browser window reads the clipboard from a script.",
            value: effective.clipboard_read.value,
            options: CLIPBOARD_OPTIONS,
            source: effective.clipboard_read.source,
            pick: (choice) =>
              save({ clipboard_read: choice as Clipboard }, "Clipboard"),
            reset: () => save({ clipboard_read: null }, "Clipboard"),
          },
        ],
      },
      {
        anchor: "local-destinations",
        title: "Local destinations: whether requests to them may be approved",
        rows: local,
      },
      {
        title: "Logins and credentials",
        rows: [
          yesNo(
            "capture",
            "Keep logins made in this workspace",
            effective.capture_logins,
            SWITCH_OPTIONS,
            async (value) => {
              const ok = await save({ capture_logins: value }, "Login capture");
              note("capture", ok);
              return ok;
            },
            "Puddle keeps the real token on this computer and the workspace gets a stand-in. A change applies when the workspace next starts.",
          ),
          yesNo(
            "foreign-warning",
            "Warn when a tool sends a token of its own",
            effective.warn_foreign_credentials,
            SWITCH_OPTIONS,
            (value) =>
              save({ warn_foreign_credentials: value }, "Credential warning"),
            "A notice and a flag in the Activity log name the website and the kind of token, never the token. Applies to the next request.",
          ),
        ],
      },
    ];
    if (settings.globalMise) {
      const tool = (
        key: "mise" | "repo_code",
        label: string,
        what: string,
        hint: string,
      ): SettingRow => {
        const own = settings.tools[key] ?? null;
        return {
          key: `tool-${key}`,
          label,
          hint,
          value: onOff(own ?? true),
          options: SWITCH_OPTIONS,
          source: own === null ? "default" : "workspace",
          pick: (choice) => saveTool(key, fromOnOff(choice), what),
          reset: () => saveTool(key, null, what),
          resetLabel: "Use the default",
        };
      };
      out.push({
        anchor: "tools",
        title: "Tools",
        rows: [
          tool(
            "mise",
            "Manage this workspace's tools with mise",
            "Tools with mise",
            "Turning it off leaves the tools and mise.toml as they are.",
          ),
          tool(
            "repo_code",
            "Let mise run the repository's own code",
            "Running repository code",
            "A repository's mise.toml can run hooks and templates. With this off, mise does not trust that file.",
          ),
        ],
      });
    }
    return out;
  });

  /** A dropdown changed: save it, and put it back when the save was refused. */
  async function choose(row: SettingRow, select: HTMLSelectElement) {
    if (!(await row.pick(select.value))) select.value = row.value;
  }
</script>

{#if workspace}
  {#if settings.status === "loading"}
    <p class="muted">Loading settings&hellip;</p>
  {:else if settings.status === "failed" || !settings.overrides || !settings.effective || !settings.global}
    <div role="alert">
      <p class="muted">Couldn't read this workspace's settings.</p>
      {#if settings.reason}<p class="error">{settings.reason}</p>{/if}
    </div>
  {:else}
    <UnknownFields fields={settings.unknownFields} />

    <div class="status" aria-live="polite">
      {#if saved}<p class="saved">{saved}</p>{/if}
      {#if problem}<p class="error" role="alert">{problem}</p>{/if}
      {#if restartHint}
        <p>The new memory applies the next time the workspace starts.</p>
      {/if}
      {#if cpusHint}
        <p>
          The new CPU count applies the next time the workspace starts; it keeps
          its current count until then.
        </p>
      {/if}
      {#if captureHint}
        <p>The new login setting applies the next time the workspace starts.</p>
      {/if}
    </div>

    <table aria-label="Settings of {workspace.name}">
      <thead>
        <tr>
          <th scope="col">Setting</th>
          <th scope="col">Value</th>
          <th scope="col">From</th>
        </tr>
      </thead>
      {#each groups as group (group.title)}
        <tbody>
          <tr class="group">
            <th scope="colgroup" colspan="3" id={group.anchor}>{group.title}</th
            >
          </tr>
          {#each group.rows as row (row.key)}
            <tr data-setting={row.key}>
              <th scope="row">
                <span id="{fid}-{row.key}">{row.label}</span>
                {#if row.hint}
                  <span class="hint" id="{fid}-{row.key}-hint">{row.hint}</span>
                {/if}
              </th>
              <td>
                <select
                  class="pick"
                  aria-labelledby="{fid}-{row.key}"
                  aria-describedby={row.hint
                    ? `${fid}-${row.key}-hint`
                    : undefined}
                  value={row.value}
                  onchange={(e) => void choose(row, e.currentTarget)}
                >
                  {#each row.options as option (option.value)}
                    <option value={option.value}>{option.label}</option>
                  {/each}
                </select>
              </td>
              <td class="from">
                <span class="chip">{sourceLabel(row.source)}</span>
                {#if row.source === "workspace"}
                  <button
                    type="button"
                    class="reset"
                    aria-label="{row.resetLabel ??
                      'Use the global setting'} again for {row.label}"
                    onclick={() => void row.reset()}
                    >{row.resetLabel ?? "Use global"}</button
                  >
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      {/each}
      <tbody>
        <tr class="group">
          <th scope="colgroup" colspan="3" id="advanced">Advanced</th>
        </tr>
        <tr>
          <th scope="row">Image</th>
          <td colspan="2" class="mono wide" title={workspace.image}
            >{workspace.image.split("@")[0]}</td
          >
        </tr>
        {#if rootDiskLabel(workspace) !== null}
          <tr>
            <th scope="row">Root disk</th>
            <td colspan="2" class:full={diskWarned(workspace, "root")}
              >{rootDiskLabel(workspace)}{diskWarned(workspace, "root")
                ? " (nearly full)"
                : ""}</td
            >
          </tr>
        {/if}
        <tr>
          <th scope="row">Disk</th>
          <td colspan="2" class:full={diskWarned(workspace, "volume")}
            >{diskLabel(workspace)}{diskWarned(workspace, "volume")
              ? " (nearly full)"
              : ""}</td
          >
        </tr>
        <tr>
          <th scope="row">
            Disk space
            <span class="hint"
              >Reclaiming gives disk the workspace no longer uses back to your
              computer. Resetting the root disk (a stopped workspace) removes
              what was installed outside the volume. Making it bigger works for
              a running or a stopped workspace and is kept when it is reset.</span
            >
          </th>
          <td colspan="2"><RootDiskControls {workspace} /></td>
        </tr>
        <tr>
          <td colspan="3"><StartFolderSetting {workspace} /></td>
        </tr>
        <tr>
          <td colspan="3"><SessionUserSetting {workspace} /></td>
        </tr>
        <tr>
          <td colspan="3"><InterceptionSetting id={workspace.id} /></td>
        </tr>
        <tr>
          <td colspan="3">
            <KeptLoginsList
              store={kept}
              workspaceId={workspace.id}
              workspaceLabel={workspace.name}
              reload={() => void kept.load(workspace.id)}
            />
          </td>
        </tr>
      </tbody>
      <tbody class="danger">
        <tr class="group">
          <th scope="colgroup" colspan="3" id="danger">Danger</th>
        </tr>
        <tr>
          <th scope="row">
            Delete workspace
            <span class="hint">
              {#if workspace.status === "volume_missing"}
                The disk is already gone, so deleting only removes the workspace
                from Puddle.
              {:else}
                Deleting removes the workspace and its disk. Puddle first lists
                uncommitted changes, unpushed commits and stashes.
              {/if}
            </span>
          </th>
          <td colspan="2">
            <button
              type="button"
              class="btn deny"
              disabled={!canDelete(workspace)}
              onclick={() => void actions.askDelete(workspace)}
            >
              <Trash2 aria-hidden="true" size={16} />{actions.checking ===
              workspace.id
                ? "Checking…"
                : "Delete workspace…"}
            </button>
            {#if !canDelete(workspace) && workspace.busy === null}
              <p class="why">Stop the workspace before deleting it.</p>
            {/if}
          </td>
        </tr>
      </tbody>
    </table>
  {/if}
{/if}

<style>
  .muted {
    margin: 0 0 var(--space-3);
    color: var(--color-text-muted);
  }
  .status {
    min-height: 1.5rem;
    margin-bottom: var(--space-2);
  }
  .status p {
    margin: 0;
  }
  .saved {
    color: var(--color-success);
  }
  .error {
    color: var(--color-danger);
  }
  table {
    width: 100%;
    max-width: 64rem;
    border-collapse: collapse;
  }
  thead th {
    padding: var(--space-2) var(--space-3);
    color: var(--color-text-muted);
    font-size: var(--text-sm);
    text-align: start;
  }
  tbody th,
  tbody td {
    padding: var(--space-3);
    border-top: 1px solid var(--color-border-subtle);
    text-align: start;
    vertical-align: top;
  }
  tbody th[scope="row"] {
    width: 45%;
    font-weight: 600;
  }
  tr.group th {
    padding-top: var(--space-6);
    border-top: 0;
    border-bottom: 2px solid var(--color-border);
    font-size: var(--text-md);
  }
  .hint {
    display: block;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
    font-weight: 400;
  }
  .pick {
    max-width: 100%;
  }
  .from {
    white-space: nowrap;
  }
  .chip {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-pill);
    font-size: var(--text-sm);
  }
  .reset {
    margin-inline-start: var(--space-2);
    padding: 0;
    border: 0;
    background: none;
    color: var(--color-accent);
    font: inherit;
    font-size: var(--text-sm);
    text-decoration: underline;
    cursor: pointer;
  }
  .wide {
    overflow-wrap: anywhere;
  }
  .full {
    color: var(--color-warning);
    font-weight: 600;
  }
  .danger tr.group th {
    border-bottom-color: var(--color-danger);
    color: var(--color-danger);
  }
  .why {
    margin: var(--space-2) 0 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }

  @media (max-width: 760px) {
    tbody th[scope="row"] {
      width: auto;
    }
    .from {
      white-space: normal;
    }
  }
</style>
