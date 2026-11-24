<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { onMount, tick } from "svelte";
  import Plus from "@lucide/svelte/icons/plus";
  import {
    auditText,
    canDelete,
    inviteLabel,
    sizeText,
    stateText,
    workspacesText,
    type AdminUser,
  } from "#lib/admin/model.ts";
  import AuditRetention from "#lib/components/AuditRetention.svelte";
  import CreateUserDialog from "#lib/components/CreateUserDialog.svelte";
  import DeleteUserDialog from "#lib/components/DeleteUserDialog.svelte";
  import Toast from "#lib/components/Toast.svelte";
  import InviteLinkDialog from "#lib/components/InviteLinkDialog.svelte";
  import { absoluteTime, relativeTime } from "#lib/format/relative-time.ts";
  import { admin as store } from "#lib/stores/admin.svelte.ts";
  import { toasts } from "#lib/stores/toasts.svelte.ts";
  import "#lib/theme/controls.css";

  let heading = $state<HTMLElement>();
  let createOpen = $state(false);
  let creating = $state(false);
  let createError = $state<string | null>(null);
  let deleting = $state<AdminUser | null>(null);
  let deleteOpen = $state(false);
  let removing = $state(false);
  let deleteError = $state<string | null>(null);
  // The invite secret lives here only while its dialog is open.
  let invite = $state<{
    name: string;
    secret: string;
    expiresAt: number;
  } | null>(null);
  let inviteOpen = $state(false);

  onMount(() => {
    heading?.focus();
    void store.load();
    // The reaper finishes a deletion in the background: look again while one is under way.
    const timer = setInterval(() => {
      if (store.users.some((user) => user.state === "deleting")) {
        void store.load();
      }
    }, 5000);
    return () => clearInterval(timer);
  });

  function show(name: string, secret: string, expiresAt: number) {
    invite = { name, secret, expiresAt };
    inviteOpen = true;
  }

  async function create(name: string) {
    creating = true;
    createError = null;
    const result = await store.create(name);
    creating = false;
    if (!result.ok) {
      createError = result.message;
      return;
    }
    createOpen = false;
    show(
      result.invite.user.name,
      result.invite.invite_secret,
      result.invite.expires_at,
    );
  }

  async function reissue(user: AdminUser) {
    const result = await store.reissue(user.id);
    if (!result.ok) {
      toasts.push(result.message, { tone: "error" });
      return;
    }
    show(user.name, result.invite.invite_secret, result.invite.expires_at);
  }

  function askDelete(user: AdminUser) {
    deleting = user;
    deleteError = null;
    deleteOpen = true;
  }

  async function confirmDelete(typed: string) {
    const user = deleting;
    if (!user) return;
    removing = true;
    deleteError = null;
    const result = await store.remove(user.id, typed);
    removing = false;
    if (!result.ok) {
      deleteError = result.message;
      return;
    }
    deleteOpen = false;
    toasts.push(`Deleting ${user.name}.`);
    await tick();
    heading?.focus();
  }

  // Nothing keeps the secret once its dialog is gone.
  const forgetInvite = () => (invite = null);
</script>

<div class="head">
  <div>
    <h1 tabindex="-1" bind:this={heading}>Admin</h1>
    <p class="sub">
      The accounts on this Puddle and what the server holds in total. You see
      counts and sizes, never what a user keeps: not their workspaces' names or
      contents, repositories, requests, rules, secrets or passwords.
    </p>
  </div>
  <div class="acts">
    <button
      type="button"
      class="btn primary"
      onclick={() => (createOpen = true)}
    >
      <Plus aria-hidden="true" size={16} />Add user
    </button>
  </div>
</div>

{#if store.status === "loading"}
  <p class="muted">Loading&hellip;</p>
{:else if store.status === "failed"}
  <div role="status">
    <p class="muted">Couldn't read the admin data.</p>
    {#if store.reason}<p class="error">{store.reason}</p>{/if}
    <button type="button" class="btn" onclick={() => void store.load()}
      >Try again</button
    >
  </div>
{:else}
  {#if store.stats}
    {@const stats = store.stats}
    <section aria-labelledby="stats-heading">
      <h2 id="stats-heading">This server</h2>
      <dl class="stats">
        <div>
          <dt>Users</dt>
          <dd>{stats.users}</dd>
        </div>
        <div>
          <dt>Workspaces</dt>
          <dd>{stats.workspaces}</dd>
        </div>
        <div>
          <dt>Running</dt>
          <dd>{stats.running}</dd>
        </div>
        <div>
          <dt>Disk in use</dt>
          <dd>{sizeText(stats.disk_used_mib)}</dd>
        </div>
        <div>
          <dt>Memory given</dt>
          <dd>{sizeText(stats.memory_mib)}</dd>
        </div>
        <div>
          <dt>Processors</dt>
          <dd>{stats.host_cpus}</dd>
        </div>
      </dl>
      <p class="muted">
        There are no per-user limits: every account may use the whole computer.
      </p>
    </section>
  {/if}

  <section aria-labelledby="users-heading">
    <h2 id="users-heading">Users</h2>
    {#if store.users.length <= 1}
      <p class="empty">
        Nobody else has an account yet. Add a user to give someone their own
        isolated workspaces on this Puddle.
      </p>
    {/if}
    <ul class="rows" aria-label="Users">
      {#each store.users as user (user.id)}
        <li class="row">
          <div class="main">
            <div class="titleline">
              <h3>{user.name}</h3>
              {#if user.role === "owner"}<span class="chip">Owner</span>{/if}
              <span class="chip {user.state}">{stateText(user)}</span>
            </div>
            <dl class="facts">
              <div>
                <dt>Created</dt>
                <dd title={absoluteTime(user.created_at)}>
                  {relativeTime(user.created_at, Date.now())}
                </dd>
              </div>
              <div>
                <dt>Last sign-in</dt>
                <dd
                  title={user.last_sign_in === null
                    ? undefined
                    : absoluteTime(user.last_sign_in)}
                >
                  {user.last_sign_in === null
                    ? "never"
                    : relativeTime(user.last_sign_in, Date.now())}
                </dd>
              </div>
              <div>
                <dt>Devices</dt>
                <dd>{user.devices}</dd>
              </div>
              <div>
                <dt>Workspaces</dt>
                <dd>{workspacesText(user)}</dd>
              </div>
              <div>
                <dt>Disk</dt>
                <dd>{sizeText(user.disk_used_mib)}</dd>
              </div>
            </dl>
          </div>
          {#if canDelete(user)}
            <div class="acts">
              <button
                type="button"
                class="btn"
                aria-label="{inviteLabel(user)} for {user.name}"
                onclick={() => reissue(user)}>{inviteLabel(user)}</button
              >
              <button
                type="button"
                class="btn deny"
                aria-label="Delete {user.name}"
                onclick={() => askDelete(user)}>Delete</button
              >
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  </section>

  <section aria-labelledby="log-heading">
    <h2 id="log-heading">Activity log</h2>
    <AuditRetention />
  </section>

  <section aria-labelledby="audit-heading">
    <h2 id="audit-heading">What you did</h2>
    {#if store.audit.length === 0}
      <p class="muted">Nothing yet.</p>
    {:else}
      <ul class="audit" aria-label="Admin audit, newest first">
        {#each store.audit as line, i (i)}
          <li>
            {auditText(line)}
            <span class="muted" title={absoluteTime(line.at)}
              >{relativeTime(line.at, Date.now())}</span
            >
          </li>
        {/each}
      </ul>
    {/if}
  </section>
{/if}

<Toast />
<CreateUserDialog
  bind:open={createOpen}
  working={creating}
  error={createError}
  onCreate={create}
/>
{#if invite}
  <InviteLinkDialog
    bind:open={inviteOpen}
    onClose={forgetInvite}
    name={invite.name}
    secret={invite.secret}
    expiresAt={invite.expiresAt}
    origin={window.location.origin}
  />
{/if}
{#if deleting}
  <DeleteUserDialog
    bind:open={deleteOpen}
    name={deleting.name}
    working={removing}
    error={deleteError}
    onConfirm={confirmDelete}
    onCancel={() => (deleteError = null)}
  />
{/if}

<style>
  .head {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    flex-wrap: wrap;
    gap: var(--space-4);
    margin-bottom: var(--space-4);
  }
  h2 {
    font-size: var(--text-lg);
    margin: var(--space-4) 0 var(--space-2);
  }
  h3 {
    font-size: var(--text-md);
    margin: 0;
    overflow-wrap: anywhere;
  }
  .sub,
  .muted {
    margin: var(--space-1) 0 0;
    color: var(--color-text-muted);
  }
  .sub {
    max-width: var(--page-reading-width);
  }
  .error {
    color: var(--color-danger);
  }
  .empty {
    padding: var(--space-4);
    border: 1px dashed var(--color-border);
    border-radius: var(--radius-md);
    color: var(--color-text-muted);
  }
  .acts {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    gap: var(--space-2);
  }
  dl {
    margin: 0;
  }
  dt {
    color: var(--color-text-muted);
    font-size: var(--text-sm);
  }
  dd {
    margin: 0;
    font-weight: 600;
  }
  .stats,
  .facts {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-2) var(--space-6);
  }
  .rows,
  .audit {
    display: grid;
    gap: var(--space-3);
    margin: 0;
    padding: 0;
    list-style: none;
  }
  .audit {
    gap: var(--space-1);
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-4);
    background: var(--color-surface);
    border: 1px solid var(--color-border-subtle);
    border-radius: var(--radius-md);
  }
  .main {
    display: grid;
    gap: var(--space-2);
    min-width: 0;
    flex: 1 1 18rem;
  }
  .titleline {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
  }
  .chip {
    padding: 0 var(--space-2);
    border: 1px solid var(--color-border);
    border-radius: var(--radius-md);
    font-size: var(--text-sm);
  }
</style>
