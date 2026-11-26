<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, tick, untrack } from "svelte";
  import { goto } from "$app/navigation";
  import RepoPicker from "./RepoPicker.svelte";
  import RepositoryHostsNotice from "./RepositoryHostsNotice.svelte";
  import { allowedNotice, type HostCheck } from "#lib/workspaces/hosts.ts";
  import { covering, type Identity } from "#lib/identities/model.ts";
  import { previewIdentity, type Preview } from "#lib/identities/preview.ts";
  import { clearDraft, saveDraft, savedDraft } from "#lib/workspaces/draft.ts";
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
  import { session } from "#lib/session/session.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import {
    globalSettings as globalSettingsStore,
    type GlobalSettings,
  } from "#lib/stores/global-settings.svelte.ts";
  import {
    defaultImage as defaultImageStore,
    type DefaultImage,
  } from "#lib/stores/default-image.svelte.ts";
  import { cpuLabel } from "#lib/settings/cpus.ts";
  import { inDesktopApp, pickFolder } from "#lib/desktop/folder-picker.ts";
  import {
    workspaces,
    type Field,
    type RowRefusal,
    type NewWorkspace,
    type WorkspaceStore,
  } from "#lib/stores/workspaces.svelte.ts";
  import {
    nameProblem,
    parseMemoryGib,
    isIncompleteAddress,
    resolveRepoAddress,
    suggestName,
    type Workspace,
  } from "#lib/workspaces/model.ts";
  import {
    checkRows,
    clashes,
    rowProblems,
    directoryFor,
    cleanPath,
    folderOf,
    MAX_REPOSITORIES,
    newRow,
    startFolderProblem,
    type RowDraft,
    type RowProblems,
  } from "#lib/workspaces/repo-rows.ts";
  import "#lib/theme/controls.css";

  let {
    store = workspaces,
    identities = identityStore,
    prefill = null,
    onCreated,
    onCancel,
    settings = globalSettingsStore,
    hostCheck = undefined,
    defaultImage = defaultImageStore,
  }: {
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
    /** Called when Cancel is pressed, after the draft was dropped. */
    onCancel?: () => void;
    /** The global settings, which say how many CPUs a workspace may have and gets by default. */
    settings?: Pick<GlobalSettings, "view" | "load">;
    /** For tests: the host check. */
    hostCheck?: HostCheck;
    /** What the default image is, read each time the form opens (the host may only know it after the first download). */
    defaultImage?: Pick<DefaultImage, "description" | "load">;
  } = $props();
  let hostBlocked = $state(false);

  type FormField = Exclude<Field, "repo_url">;

  // An unsent draft from the last visit comes back; a repository handed in replaces its rows.
  const kept = untrack(() => prefill) ? null : savedDraft();
  let nextKey = Math.max(0, ...(kept?.rows.map((r) => r.key) ?? [])) + 1;
  const fresh = (url = "") => newRow(nextKey++, url);

  let rows = $state<RowDraft[]>(kept ? structuredClone(kept.rows) : [fresh()]);
  let name = $state(kept?.name ?? "");
  let nameTouched = $state(kept?.nameTouched ?? false);
  let image = $state(kept?.image ?? "");
  let memoryGib = $state(kept?.memoryGib ?? "");
  /** The count the slider was moved to; `null` while it is still on the default. */
  let cpusChosen = $state<number | null>(kept?.cpusChosen ?? null);
  let startFolder = $state(kept?.startFolder ?? "");
  let errors = $state<Partial<Record<FormField, string>>>({});
  /** What each row was refused for, by the row's key: after a submit, or the service's words. */
  let rowErrors = $state<Record<number, RowProblems>>({});
  let working = $state(false);
  let taken: typeof prefill = null;

  // Reads the identities when the screen opens, so the ones that cover an address can be offered.
  // The CPU limits are read each time the screen opens: the maximum can change in Settings.
  onMount(() => {
    void identities.ensureLoaded();
    void settings.load(true);
    void defaultImage.load();
  });

  // The draft follows every edit, so leaving the screen keeps what was typed.
  $effect(() => {
    const draft = $state.snapshot({
      rows,
      name,
      nameTouched,
      image,
      memoryGib,
      cpusChosen,
      startFolder,
    });
    if (!done) saveDraft(draft);
  });
  const limits = $derived(settings.view?.effective_limits ?? null);
  /** What the slider shows: the count it was moved to, else what a new workspace gets by default. */
  const cpusShown = $derived(
    cpusChosen ?? settings.view?.effective.cpus.value ?? 1,
  );

  // A repository handed in by "Create a workspace for this" becomes the only row, once.
  $effect(() => {
    if (prefill && prefill !== taken) {
      taken = prefill;
      const row = fresh(prefill.url);
      row.listedBy = prefill.identity;
      rows = [row];
      rowErrors = {};
    }
  });

  /** The https:// address a row's field means, or what was typed while it names none. */
  function addressOf(row: RowDraft): string {
    const resolved = resolveRepoAddress(row.url);
    return "url" in resolved ? resolved.url : row.url.trim();
  }

  // A definite refusal shows at the keystroke that causes it, with no timer and no host call. Only
  // the half-typed shorthand ("sharkdp" on the way to "sharkdp/fd") waits until the field is left.
  /** The rows whose address field was left, by row key. */
  let left = $state<Record<number, boolean>>({});
  /** The refusal of a non-empty address; an empty one waits for Create. */
  function addressProblem(row: RowDraft): string | undefined {
    if (row.url.trim() === "") return undefined;
    const resolved = resolveRepoAddress(row.url);
    if (!("message" in resolved)) return undefined;
    if (isIncompleteAddress(resolved.message) && !left[row.key])
      return undefined;
    return resolved.message;
  }

  // What two rows share is flagged as you type, before any submit.
  const live = $derived(clashes(rows));
  const problem = (row: RowDraft, field: keyof RowProblems) =>
    rowErrors[row.key]?.[field] ??
    (field === "url" ? addressProblem(row) : undefined) ??
    live.get(row.key)?.[field];

  /** "Will clone ..." when the field means another address than the one typed. */
  function willClone(row: RowDraft): string | null {
    const resolved = resolveRepoAddress(row.url);
    return "url" in resolved && resolved.url !== row.url.trim()
      ? `Will clone ${resolved.url}`
      : null;
  }

  const offeredFor = (row: RowDraft) =>
    matchingUrl(identities.identities, addressOf(row), row.listedBy);
  /**
   * What the form chooses itself for a row: the identity that listed the repository, else the one
   * that covers its owner best (as the host does when none is chosen).
   */
  function chosenForYou(row: RowDraft): number | null {
    const parsed = parseRepoUrl(addressOf(row));
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
      const parsed = parseRepoUrl(addressOf(row));
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
    const folder =
      first?.source === "folder" && first.url.trim() === ""
        ? folderOf(first.localPath)
        : null;
    name = suggestName(folder === null ? (first?.url ?? "") : `/${folder}`);
  });

  /** Drops the draft: what was sent, or what Cancel gives up. */
  let done = false;
  function discard() {
    done = true;
    clearDraft();
  }

  // Focus starts on the first address, as the dialog did.
  onMount(() => {
    const first = rows[0];
    if (first) document.getElementById(`new-ws-r${first.key}-url`)?.focus();
  });

  const focus = (id: string) => document.getElementById(id)?.focus();
  const rowId = (row: RowDraft, part: string) => `new-ws-r${row.key}-${part}`;

  async function addRow() {
    if (rows.length >= MAX_REPOSITORIES) return;
    const row = fresh();
    rows = [...rows, row];
    await tick();
    focus(rowId(row, "url"));
  }

  // The folder dialog exists only in the desktop app; a cancel leaves the field as it was.
  const desktop = inDesktopApp();
  let choosing = $state(false);
  async function chooseFolder(row: RowDraft) {
    choosing = true;
    try {
      const folder = await pickFolder("Folder of the checkout");
      if (folder !== null) {
        row.localPath = folder;
        rowErrors = { ...rowErrors, [row.key]: {} };
      }
    } catch {
      toasts.push("The folder dialog could not open. Type the path instead.", {
        tone: "error",
      });
    } finally {
      choosing = false;
    }
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
    const label = nameProblem(name, null, workspaces.list);
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
        const local = row.source === "folder" ? cleanPath(row.localPath) : "";
        return {
          url: addressOf(row),
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
      // Without the limits the slider is not shown, and the workspace follows the global default.
      ...(limits === null ? {} : { cpus: cpusShown }),
    };
    working = true;
    const result = await store.create(body);
    working = false;
    if (result.ok) {
      discard();
      warnAboutIdentities(result.value);
      const allowed = allowedNotice(result.value.allowed_hosts);
      if (allowed) toasts.push(allowed, { ms: 10_000 });
      onCreated?.(result.value);
      return;
    }
    await refuse(result.message, result.field ?? "form", result.at);
  }

  /** Puts the service's refusal where it belongs: at a row when it names one, else at its field. */
  async function refuse(message: string, field: Field, at?: RowRefusal) {
    const row = at ? rows[at.row - 1] : undefined;
    if (at && row) {
      rowErrors = { [row.key]: { [at.input]: message } };
      errors = {};
      await showAndFocus(rowId(row, at.input));
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

<section class="screen" aria-labelledby="new-ws-title">
  <h1 id="new-ws-title">New workspace</h1>
  <p class="lead">
    Puddle clones your repositories into a fresh workspace with its own disk.
  </p>
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
              onchange={() => {
                rowErrors = { ...rowErrors, [row.key]: {} };
              }}
              data-testid="source-address"
            />
            Clone from an address
          </label>
          {#if session.mayUseServerFolders}
            <label>
              <input
                type="radio"
                name={rowId(row, "source")}
                value="folder"
                bind:group={row.source}
                onchange={() => {
                  rowErrors = { ...rowErrors, [row.key]: {} };
                }}
                data-testid="source-folder"
              />
              From a folder on this computer
            </label>
          {/if}
        </div>

        {#if row.source === "folder"}
          {@const localError = problem(row, "localPath")}
          <div class="field">
            <label for={rowId(row, "localPath")}>Folder of the checkout</label>
            <div class="with-action">
              <input
                class="input-fill"
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
              {#if desktop}
                <button
                  type="button"
                  class="btn"
                  data-testid="choose-folder"
                  disabled={choosing}
                  onclick={() => chooseFolder(row)}>Choose folder</button
                >
              {/if}
            </div>
            {#if localError}
              <p class="error" id={rowId(row, "localPath-error")} role="alert">
                {localError}
              </p>
            {:else}
              <p class="hint" id={rowId(row, "localPath-hint")}>
                Puddle copies your commits and branches from the .git folder.
                Uncommitted changes, untracked files such as .env, and hooks
                stay on this computer.
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
              left = { ...left, [row.key]: false };
            }}
            onblur={() => (left = { ...left, [row.key]: true })}
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
            <p class="hint" id={rowId(row, "url-hint")} aria-live="polite">
              {willClone(row) ??
                (row.source === "folder"
                  ? "Left empty, the checkout's own origin is used. An SSH origin on GitHub, GitLab, Bitbucket or Azure DevOps is turned into its HTTPS address; for any other host, give the HTTPS address here."
                  : "An HTTPS URL, owner/repo for GitHub, or an SSH address of GitHub, GitLab, Bitbucket or Azure DevOps (git@github.com:you/project.git); the repository is cloned over HTTPS.")}
            </p>
          {/if}
        </div>

        {#if row.source === "address"}
          <RepoPicker
            identities={identities.identities}
            onPick={(repo, by) => picked(row, repo, by)}
          />
        {/if}

        {#if tableCannotHold(addressOf(row))}
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
          What Puddle shows for this workspace; you can change it later.
        </p>
      {/if}
    </div>

    <fieldset class="resources">
      <legend>Resources</legend>
      <p class="hint">
        The defaults suit most work; change them here or later in the
        workspace's settings.
      </p>
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
          aria-describedby={errors.image
            ? "new-ws-image-error"
            : image.trim() === "" && defaultImage.description
              ? "new-ws-image-hint"
              : undefined}
        />
        {#if errors.image}
          <p class="error" id="new-ws-image-error" role="alert">
            {errors.image}
          </p>
        {:else if image.trim() === "" && defaultImage.description}
          <p class="hint" id="new-ws-image-hint">
            Default: {defaultImage.description}
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
      {#if limits}
        <div class="field">
          <label for="new-ws-cpus">CPUs</label>
          <div class="slider">
            <input
              class="input-fill"
              id="new-ws-cpus"
              type="range"
              min="1"
              max={limits.max_cpus}
              step="1"
              value={cpusShown}
              aria-valuetext={cpuLabel(cpusShown)}
              aria-invalid={errors.cpus ? "true" : undefined}
              aria-describedby="new-ws-cpus-hint"
              oninput={(e) => {
                cpusChosen = Number(e.currentTarget.value);
                const { cpus: _gone, ...rest } = errors;
                errors = rest;
              }}
            />
            <output for="new-ws-cpus">{cpusShown}</output>
          </div>
          <p
            class={errors.cpus ? "error" : "hint"}
            id="new-ws-cpus-hint"
            role={errors.cpus ? "alert" : undefined}
          >
            {errors.cpus ??
              `This computer has ${limits.host_cpus}. The count is the workspace's own and applies from its first start; you can change it later. JetBrains Gateway needs at least 4.`}
          </p>
        </div>
      {/if}
    </fieldset>

    {#if errors.form}
      <p class="error" role="alert">{errors.form}</p>
    {/if}

    <div class="actions">
      <button
        type="button"
        class="btn"
        onclick={() => {
          discard();
          onCancel?.();
        }}>Cancel</button
      >
      <button
        type="submit"
        class="btn primary"
        disabled={working || hostBlocked}
      >
        {working ? "Creating…" : "Create workspace"}
      </button>
    </div>

    <!-- Below the buttons: the answer arrives after the typing stops, and a note above them
         would push the button away from a press already under way. -->
    <RepositoryHostsNotice
      urls={rows.map(addressOf)}
      {...hostCheck ? { check: hostCheck } : {}}
      onBlocked={(b) => (hostBlocked = b)}
    />
  </form>
</section>

<style>
  .screen {
    max-width: 44rem;
  }
  .lead {
    margin: 0 0 var(--space-4);
    color: var(--color-text-muted);
  }
  .screen :global(form) {
    display: grid;
    gap: var(--space-4);
  }
  .screen :global(fieldset) {
    display: grid;
    gap: var(--space-2);
    margin: 0;
    padding: 0;
    border: 0;
  }
  .screen :global(legend) {
    padding: 0;
    margin-bottom: var(--space-1);
    font-weight: 600;
  }
  .screen :global(.field) {
    display: grid;
    gap: var(--space-1);
  }
  .screen :global(.field > label),
  .screen :global(.choice) {
    display: flex;
    align-items: center;
    gap: var(--space-2);
  }
  .screen :global(.field > label) {
    font-weight: 600;
  }
  .screen :global(.hint) {
    margin: 0;
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  .screen :global(.error) {
    margin: 0;
    color: var(--color-danger);
    font-size: var(--text-sm);
  }
  .screen :global(.actions) {
    display: flex;
    justify-content: flex-end;
    gap: var(--space-2);
  }
  .resources {
    gap: var(--space-3);
  }
  .slider {
    display: flex;
    align-items: center;
    gap: var(--space-3);
  }
  .slider output {
    min-width: 2.5ch;
    font-variant-numeric: tabular-nums;
    text-align: right;
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
  .with-action {
    display: flex;
    gap: var(--space-2);
    align-items: stretch;
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
