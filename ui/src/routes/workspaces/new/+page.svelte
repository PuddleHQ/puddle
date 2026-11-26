<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import CreateWorkspaceForm from "#lib/components/CreateWorkspaceForm.svelte";

  // A repository to start from arrives as `?url=...&identity=...` ("Create a workspace for this").
  // Taken once: the form keeps editing its own rows after that.
  const prefill = $derived.by(() => {
    const url = page.url.searchParams.get("url");
    const identity = Number(page.url.searchParams.get("identity"));
    if (!url) return null;
    return {
      url,
      identity: Number.isInteger(identity) && identity > 0 ? identity : null,
    };
  });
</script>

<CreateWorkspaceForm
  {prefill}
  onCreated={(workspace) =>
    void goto(
      prefill
        ? `/workspaces/${encodeURIComponent(workspace.id)}`
        : "/workspaces",
    )}
  onCancel={() => void goto("/workspaces")}
/>
