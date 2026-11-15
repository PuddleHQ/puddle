<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import {
    checkHosts,
    hostList,
    type HostCheck,
    type RepositoryHost,
  } from "#lib/workspaces/hosts.ts";

  // Under the repository addresses of a form: an info box naming the hosts the create will allow
  // for the workspace, and an error box naming the rule that blocks one. `blocked` tells the form
  // to grey out its button. The answer is asked for after the typing stops.
  let {
    urls,
    workspace = undefined,
    check = checkHosts,
    onBlocked = undefined,
    delay = 300,
  }: {
    urls: readonly string[];
    workspace?: string | undefined;
    check?: HostCheck;
    onBlocked?: (blocked: boolean) => void;
    delay?: number;
  } = $props();

  let hosts = $state<RepositoryHost[]>([]);

  const wanted = $derived(
    urls.map((u) => u.trim()).filter((u) => u.startsWith("https://")),
  );
  const key = $derived(JSON.stringify(wanted));

  $effect(() => {
    const asked: string[] = JSON.parse(key);
    if (asked.length === 0) {
      hosts = [];
      return;
    }
    let live = true;
    const timer = setTimeout(async () => {
      const answer = await check(asked, workspace);
      if (live) hosts = answer ?? [];
    }, delay);
    return () => {
      live = false;
      clearTimeout(timer);
    };
  });

  const fresh = $derived(hosts.filter((h) => h.state === "new"));
  const refused = $derived(hosts.filter((h) => h.state === "blocked"));
  $effect(() => {
    onBlocked?.(refused.length > 0);
  });
</script>

{#each refused as host (host.host)}
  <p class="error" role="alert" data-testid="hosts-blocked">
    {host.blocked_by ?? `${host.host} is blocked by a rule.`}
  </p>
{/each}
{#if fresh.length > 0}
  <p class="hint" data-testid="hosts-new">
    Creating this allows {hostList(fresh.map((h) => h.host))} for this workspace,
    so the clone does not wait for you. You can remove the rule on the Rules screen.
  </p>
{/if}
