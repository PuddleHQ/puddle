<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { tick } from "svelte";
  import { goto } from "$app/navigation";
  import FormDialog from "./FormDialog.svelte";
  import RepoPicker from "./RepoPicker.svelte";
  import { covering, type Identity } from "#lib/identities/model.ts";
  import { previewIdentity, type Preview } from "#lib/identities/preview.ts";
  import { parseRepoUrl } from "#lib/identities/repo.ts";
  import {
    matchingUrl,
    tableCannotHold,
    TABLE_CANNOT_HOLD,
  } from "#lib/repos/model.ts";
  import {
    identities as identityStore,
    type IdentitiesStore,
  } from "#lib/stores/identities.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import {
    workspaces,
    type Field,
    type NewWorkspace,
    type WorkspaceStore,
  } from "#lib/stores/workspaces.svelte.ts";
  import {
    nameProblem,
    parseMemoryGib,
    suggestName,
    type Workspace,
  } from "#lib/workspaces/model.ts";
  import {
    checkRows,
    clashes,
    rowProblems,
    directoryFor,
    MAX_REPOSITORIES,
    newRow,
    refusedRow,
    startFolderProblem,
    type RowDraft,
    type RowProblems,
  } from "#lib/workspaces/repo-rows.ts";
  import "#lib/theme/controls.css";

  let {
    open = $bindable(false),
    store = workspaces,
    identities = identityStore,
    prefill = null,
    onCreated,
  }: {
    open?: boolean;
    store?: Pick<WorkspaceStore, "create">;
    /** The identities to offer; the screens' own store unless a test brings one. */
    identities?: Pick<IdentitiesStore, "identities" | "ensureLoaded"> &
      Partial<Pick<IdentitiesStore, "status">>;
    /**
     * A repository to start from, as "Create a workspace for this" gives it: its address and the
     * identity that listed it. Give a new object each time the form should take one.
     */
    prefill?: { url: string; identity: number | null } | null;
    /** Called with the new workspace (still being created) once the service accepted it. */
    onCreated?: (workspace: Workspace) => void;
  } = $props();

  type FormField = Exclude<Field, "repo_url">;

  let nextKey = 1;
  const fresh = (url = "") => newRow(nextKey++, url);

  let rows = $state<RowDraft[]>([fresh()]);
  let name = $state("");
  let nameTouched = $state(false);
  let image = $state("");
  let memoryGib = $state("");
  let startFolder = $state("");
  let moreOpen = $state(false);
  let errors = $state<Partial<Record<FormField, string>>>({});
  /** What each row was refused for, by the row's key: after a submit, or the service's words. */
  let rowErrors = $state<Record<number, RowProblems>>({});
  let working = $state(false);
  let taken: typeof prefill = null;

  // Reads the identities when the form opens, so the ones that cover an address can be offered.
  $effect(() => {
    if (open) void identities.ensureLoaded();
  });

  // A repository handed in by "Create a workspace for this" becomes the only row, once.
  $effect(() => {
    if (open && prefill && prefill !== taken) {
      taken = prefill;
      const row = fresh(prefill.url);
      row.listedBy = prefill.identity;
      rows = [row];
      rowErrors = {};
    }
  });

  // What two rows share is flagged as you type, before any submit.
  const live = $derived(clashes(rows));
  const problem = (row: RowDraft, field: keyof RowProblems) =>
    rowErrors[row.key]?.[field] ?? live.get(row.key)?.[field];

  const offeredFor = (row: RowDraft) =>
    matchingUrl(identities.identities, row.url, row.listedBy);
  /**
   * What the form chooses itself for a row: the identity that listed the repository, else the one
   * that covers its owner best (as the host does when none is chosen).
   */
  function chosenForYou(row: RowDraft): number | null {
    const parsed = parseRepoUrl(row.url);
    const offered = offeredFor(row);
    if (!parsed.ok || offered.length === 0) return null;
    const listed = offered.find((i) => i.id === row.listedBy);
    return (listed ?? covering(offered, parsed.host, parsed.owner))?.id ?? null;
  }
  const selectedFor = (row: RowDraft): string =>
    row.identity ?? String(chosenForYou(row) ?? "");

  /**
   * What each row will get, as the host will choose it: a row's identity joins the workspace's
   * list for the rows after it. Empty until the identities are read, and for an address that
   * names no repository yet.
   */
  const previews = $derived.by(() => {
    const out: Record<number, Preview> = {};
    if (identities.status && identities.status !== "ready") return out;
    const onList: Identity[] = [];
    for (const row of rows) {
      const parsed = parseRepoUrl(row.url);
      if (!parsed.ok) continue;
      const picked = selectedFor(row);
      const preview = previewIdentity(
        identities.identities,
        onList,
        picked === "" ? null : Number(picked),
        parsed.host,
        parsed.owner,
      );
      out[row.key] = preview;
      const joined = preview.identity;
      if (joined && !onList.some((i) => i.id === joined.id))
        onList.push(joined);
    }
    return out;
  });

  // Until the user types a name, it follows the first repository.
  $effect(() => {
    if (nameTouched) return;
    const first = rows[0];
    // From a folder with no address typed, the folder's name stands in for the address's.
    const fromFolder =
      first?.source === "folder" && first.url.trim() === ""
        ? first.localPath.trim().replace(/[\\/]+(\.git)?[\\/]*$/, "")
        : "";
    name = suggestName(
      fromFolder === ""
        ? (first?.url ?? "")
        : `/${fromFolder.replace(/\\/g, "/")}`,
    );
  });

  function reset() {
    rows = [fresh()];
    name = "";
    nameTouched = false;
    image = "";
    memoryGib = "";
    startFolder = "";
    moreOpen = false;
    errors = {};
    rowErrors = {};
    working = false;
    taken = null;
  }

  const focus = (id: string) => document.getElementById(id)?.focus();
  const rowId = (row: RowDraft, part: string) => `new-ws-r${row.key}-${part}`;

  async function addRow() {
    if (rows.length >= MAX_REPOSITORIES) return;
    const row = fresh();
    rows = [...rows, row];
    await tick();
    focus(rowId(row, "url"));
  }

  async function removeRow(at: number) {
    rows = rows.filter((_, i) => i !== at);
    await tick();
    const next = rows[at];
    if (next) focus(rowId(next, "url"));
    else focus("new-ws-add");
  }

  function picked(row: RowDraft, repo: { url: string }, by: number | null) {
    row.url = repo.url;
    row.listedBy = by;
    row.identity = null;
    if (row === rows[0]) focus("new-ws-name");
    else focus(rowId(row, "directory"));
  }

  function check() {
    const found: Partial<Record<FormField, string>> = {};
    const label = nameProblem(name);
    if (label) found.name = label;
    const folder = startFolderProblem(startFolder);
    if (folder) found.start_folder = folder;
    const memory = parseMemoryGib(memoryGib);
    if (!memory.ok) found.memory_mib = memory.message;
    // What is shown after the submit is what the rows cannot show themselves; two rows that
    // share a directory are flagged as you type (`live`), so the flag clears when one changes.
    const shown: Record<number, RowProblems> = {};
    for (const [key, p] of rowProblems(rows)) shown[key] = p;
    const bad: Record<number, RowProblems> = {};
    for (const [key, p] of checkRows(rows)) bad[key] = p;
    return { found, bad, shown };
  }

  /** The first field with a problem, in the order on screen. */
  function firstBad(
    found: Partial<Record<FormField, string>>,
    bad: Record<number, RowProblems>,
  ): string | null {
    for (const row of rows) {
      if (bad[row.key]?.localPath) return rowId(row, "localPath");
      if (bad[row.key]?.url) return rowId(row, "url");
      if (bad[row.key]?.directory) return rowId(row, "directory");
    }
    if (found.name) return "new-ws-name";
    if (found.start_folder) return "new-ws-start_folder";
    if (found.memory_mib) return "new-ws-memory_mib";
    return null;
  }

  async function showAndFocus(id: string | null) {
    if (id && /start_folder|memory_mib|image/.test(id)) moreOpen = true;
    await tick();
    if (id) focus(id);
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    if (working) return;
    const { found, bad, shown } = check();
    errors = found;
    rowErrors = shown;
    const first = firstBad(found, bad);
    if (first) {
      await showAndFocus(first);
      return;
    }
    const memory = parseMemoryGib(memoryGib);
    const folder = startFolder.trim();
    const body: NewWorkspace = {
      name,
      repositories: rows.map((row) => {
        const identity = selectedFor(row);
        const directory = row.directory.trim();
        const local = row.source === "folder" ? row.localPath.trim() : "";
        return {
          url: row.url.trim(),
          ...(local === "" ? {} : { local_path: local }),
          ...(directory === "" ? {} : { directory }),
          ...(identity === "" ? {} : { identity_id: Number(identity) }),
          ...(row.pull ? {} : { pull: false }),
          ...(row.push ? {} : { push: false }),
        };
      }),
      ...(folder === "" ? {} : { start_folder: folder }),
      ...(image.trim() === "" ? {} : { image: image.trim() }),
      ...(memory.ok && memory.mib !== null ? { memory_mib: memory.mib } : {}),
    };
    working = true;
    const result = await store.create(body);
    working = false;
    if (result.ok) {
      open = false;
      reset();
      warnAboutIdentities(result.value);
      onCreated?.(result.value);
      return;
    }
    await refuse(result.message, result.field ?? "form");
  }

  /** Puts the service's refusal where it belongs: at a row when it names one, else at its field. */
  async function refuse(message: string, field: Field) {
    const at = refusedRow(message);
    const row = at ? rows[at.row - 1] : undefined;
    if (at && row) {
      rowErrors = { [row.key]: { [at.field]: message } };
      errors = {};
      await showAndFocus(rowId(row, at.field));
      return;
    }
    // An address refusal that names no row is the one row's, when there is only one.
    const only = rows.length === 1 ? rows[0] : undefined;
    if (field === "repo_url" && only) {
      rowErrors = { [only.key]: { url: message } };
      errors = {};
      await showAndFocus(rowId(only, "url"));
      return;
    }
    const target = field === "repo_url" ? "form" : field;
    errors = { [target]: message };
    rowErrors = {};
    if (target !== "form") await showAndFocus(`new-ws-${target}`);
  }

  /** Nothing covers some repository, so it got the default identity: say so, and where to change it. */
  function warnAboutIdentities(made: Workspace) {
    const warnings = made.identities.flatMap((i) =>
      i.warning ? [i.warning] : [],
    );
    const [first] = warnings;
    if (!first) return;
    const rest = warnings.length - 1;
    const id = made.id;
    toasts.push(
      rest > 0
        ? `${first} ${rest} more ${rest === 1 ? "repository has" : "repositories have"} the same trouble.`
        : first,
      {
        ms: 20_000,
        action: {
          label: "Open Git tab",
          run: () => goto(`/workspaces/${encodeURIComponent(id)}/git`),
        },
      },
    );
  }
</script>

<FormDialog
  bind:open
  title="New workspace"
  description="Puddle clones your repositories into a fresh workspace with its own disk."
  onClose={reset}
>
  <form onsubmit={submit} novalidate>
    {#if rows.length === 0}
      <p class="hint" id="new-ws-none">
        No repository: this makes an ordinary workspace with an empty disk.
      </p>
    {/if}

    {#each rows as row, at (row.key)}
      {@const offered = offeredFor(row)}
      {@const urlError = problem(row, "url")}
      {@const dirError = problem(row, "directory")}
      <fieldset class="repo">
        <legend>Repository {at + 1}</legend>
        <div
          class="source"
          role="radiogroup"
          aria-label="Where the code comes from"
        >
          <label>
            <input
              type="radio"
              name={rowId(row, "source")}
              value="address"
              bind:group={row.source}
              data-testid="source-address"
            />
            Clone from an address
          </label>
          <label>
            <input
              type="radio"
              name={rowId(row, "source")}
              value="folder"
              bind:group={row.source}
              data-testid="source-folder"
            />
            From a folder on this computer
          </label>
        </div>

        {#if row.source === "folder"}
          {@const localError = problem(row, "localPath")}
          <div class="field">
            <label for={rowId(row, "localPath")}>Folder of the checkout</label>
            <input
              id={rowId(row, "localPath")}
              type="text"
              autocomplete="off"
              spellcheck="false"
              placeholder="/home/you/project or C:\Users\you\project"
              bind:value={row.localPath}
              oninput={() => {
                rowErrors = { ...rowErrors, [row.key]: {} };
              }}
              aria-invalid={localError ? "true" : undefined}
              aria-describedby={localError
                ? rowId(row, "localPath-error")
                : rowId(row, "localPath-hint")}
            />
            {#if localError}
              <p class="error" id={rowId(row, "localPath-error")} role="alert">
                {localError}
              </p>
            {:else}
              <p class="hint" id={rowId(row, "localPath-hint")}>
                Puddle reads only its .git folder, read-only: every commit,
                branch and tag comes along. Uncommitted changes and untracked
                files, such as .env, stay on this computer.
              </p>
            {/if}
          </div>
        {/if}

        <div class="field">
          <label for={rowId(row, "url")}
            >{row.source === "folder"
              ? "Git repository (HTTPS, optional)"
              : "Git repository (HTTPS)"}</label
          >
          <input
            id={rowId(row, "url")}
            type="text"
            inputmode="url"
            autocomplete="off"
            spellcheck="false"
            placeholder="https://github.com/you/project.git"
            bind:value={row.url}
            oninput={() => {
              row.listedBy = null;
              row.identity = null;
              rowErrors = { ...rowErrors, [row.key]: {} };
            }}
            aria-invalid={urlError ? "true" : undefined}
            aria-describedby={urlError
              ? rowId(row, "url-error")
              : rowId(row, "url-hint")}
          />
          {#if urlError}
            <p class="error" id={rowId(row, "url-error")} role="alert">
              {urlError}
            </p>
          {:else}
            <p class="hint" id={rowId(row, "url-hint")}>
              {row.source === "folder"
                ? "Left empty, the checkout's own origin is used (an SSH origin of GitHub, GitLab, Bitbucket or Azure DevOps as HTTPS)."
                : "SSH addresses aren't supported yet: use the HTTPS URL."}
            </p>
          {/if}
        </div>

        {#if row.source === "address"}
          <RepoPicker
            identities={identities.identities}
            onPick={(repo, by) => picked(row, repo, by)}
          />
        {/if}

        {#if tableCannotHold(row.url)}
          <p class="hint warn" data-testid="table-cannot-hold">
            {TABLE_CANNOT_HOLD}
          </p>
        {/if}

        <div class="field">
          <label for={rowId(row, "directory")}>Directory</label>
          <input
            id={rowId(row, "directory")}
            type="text"
            autocomplete="off"
            spellcheck="false"
            placeholder={directoryFor({ ...row, directory: "" })}
            bind:value={row.directory}
            oninput={() => {
              rowErrors = { ...rowErrors, [row.key]: {} };
            }}
            aria-invalid={dirError ? "true" : undefined}
            aria-describedby={dirError
              ? rowId(row, "directory-error")
              : rowId(row, "directory-hint")}
          />
          {#if dirError}
            <p class="error" id={rowId(row, "directory-error")} role="alert">
              {dirError}
            </p>
          {:else}
            <p class="hint" id={rowId(row, "directory-hint")}>
              The folder it is cloned into, on the workspace's disk. It follows
              the address until you type one.
            </p>
          {/if}
        </div>

        <div class="field">
          <label for={rowId(row, "identity")}>Git identity</label>
          <select
            id={rowId(row, "identity")}
            value={selectedFor(row)}
            aria-describedby={previews[row.key]
              ? rowId(row, "identity-preview")
              : undefined}
            onchange={(event) => {
              row.identity = event.currentTarget.value;
            }}
          >
            <option value="">Let Puddle choose</option>
            {#if offered.length > 0}
              <optgroup label="Cover this repository">
                {#each offered as identity (identity.id)}
                  <option value={String(identity.id)}
                    >{identity.label}{identity.id === row.listedBy
                      ? " (listed this repository)"
                      : ""}</option
                  >
                {/each}
              </optgroup>
            {/if}
            {#if identities.identities.length > offered.length}
              <optgroup label="Other identities">
                {#each identities.identities.filter((i) => !offered.some((o) => o.id === i.id)) as identity (identity.id)}
                  <option value={String(identity.id)}>{identity.label}</option>
                {/each}
              </optgroup>
            {/if}
          </select>
          {#if previews[row.key]}
            {@const preview = previews[row.key]!}
            <p
              class="hint"
              class:warn={!preview.covered}
              id={rowId(row, "identity-preview")}
              aria-live="polite"
            >
              {preview.text}
            </p>
          {/if}
        </div>

        <div class="boxes">
          <label class="choice">
            <input
              type="checkbox"
              bind:checked={row.pull}
              aria-describedby={rowId(row, "boxes-hint")}
            />
            Pull
          </label>
          <label class="choice">
            <input
              type="checkbox"
              bind:checked={row.push}
              aria-describedby={rowId(row, "boxes-hint")}
            />
            Push
          </label>
        </div>
        <p class="hint" id={rowId(row, "boxes-hint")}>
          Pull and Push say what the workspace may fetch from and send to this
          repository. You can change them later on its Git tab.
        </p>

        <div>
          <button
            type="button"
            class="btn"
            aria-label="Remove repository {at + 1}"
            onclick={() => removeRow(at)}>Remove</button
          >
        </div>
      </fieldset>
    {/each}

    <div>
      <button
        type="button"
        class="btn"
        id="new-ws-add"
        disabled={rows.length >= MAX_REPOSITORIES}
        onclick={addRow}
        >{rows.length === 0
          ? "Add a repository"
          : "Add another repository"}</button
      >
    </div>

    <div class="field">
      <label for="new-ws-name">Name</label>
      <input
        id="new-ws-name"
        type="text"
        autocomplete="off"
        spellcheck="false"
        bind:value={name}
        oninput={() => {
          nameTouched = true;
        }}
        aria-invalid={errors.name ? "true" : undefined}
        aria-describedby={errors.name
          ? "new-ws-name-error"
          : "new-ws-name-hint"}
      />
      {#if errors.name}
        <p class="error" id="new-ws-name-error" role="alert">{errors.name}</p>
      {:else}
        <p class="hint" id="new-ws-name-hint">
          Lowercase letters, digits and hyphens. It names the disk and can't be
          changed; you can rename the workspace later and keep this as its
          identifier.
        </p>
      {/if}
    </div>

    <details class="more" bind:open={moreOpen}>
      <summary>Start folder, image and memory</summary>
      <div class="field">
        <label for="new-ws-start_folder">Start folder</label>
        <input
          id="new-ws-start_folder"
          type="text"
          autocomplete="off"
          spellcheck="false"
          placeholder="The workspace root"
          bind:value={startFolder}
          oninput={() => {
            const { start_folder: _gone, ...rest } = errors;
            errors = rest;
          }}
          aria-invalid={errors.start_folder || startFolderProblem(startFolder)
            ? "true"
            : undefined}
          aria-describedby="new-ws-start_folder-hint"
        />
        <p
          class={errors.start_folder || startFolderProblem(startFolder)
            ? "error"
            : "hint"}
          id="new-ws-start_folder-hint"
        >
          {errors.start_folder ??
            startFolderProblem(startFolder) ??
            "A folder under the workspace root, such as api/src. New terminals start there and the editor opens it."}
        </p>
      </div>
      <div class="field">
        <label for="new-ws-image">Image</label>
        <input
          id="new-ws-image"
          type="text"
          autocomplete="off"
          spellcheck="false"
          placeholder="Default development image"
          bind:value={image}
          aria-invalid={errors.image ? "true" : undefined}
          aria-describedby={errors.image ? "new-ws-image-error" : undefined}
        />
        {#if errors.image}
          <p class="error" id="new-ws-image-error" role="alert">
            {errors.image}
          </p>
        {/if}
      </div>
      <div class="field">
        <label for="new-ws-memory_mib">Memory (GiB)</label>
        <input
          id="new-ws-memory_mib"
          type="text"
          inputmode="decimal"
          autocomplete="off"
          placeholder="Use the default"
          bind:value={memoryGib}
          aria-invalid={errors.memory_mib ? "true" : undefined}
          aria-describedby="new-ws-memory_mib-hint"
        />
        <p
          class={errors.memory_mib ? "error" : "hint"}
          id="new-ws-memory_mib-hint"
          role={errors.memory_mib ? "alert" : undefined}
        >
          {errors.memory_mib ??
            "You can change it later; it applies at the next start."}
        </p>
      </div>
    </details>

    {#if errors.form}
      <p class="error" role="alert">{errors.form}</p>
    {/if}

    <div class="actions">
      <button
        type="button"
        class="btn"
        onclick={() => {
          open = false;
          reset();
        }}>Cancel</button
      >
      <button type="submit" class="btn primary" disabled={working}>
        {working ? "Creating…" : "Create workspace"}
      </button>
    </div>
  </form>
</FormDialog>

<style>
  .more {
    display: grid;
    gap: var(--space-3);
  }
  summary {
    cursor: pointer;
    font-weight: 600;
  }
  .more .field {
    margin-top: var(--space-3);
  }
  .repo {
    padding: var(--space-3);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  .boxes,
  .source {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-4);
  }
  .source label {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    min-height: var(--control-size);
  }
  .boxes .choice {
    min-height: var(--control-size);
  }
  .warn {
    color: var(--color-warning);
  }
  .btn:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
