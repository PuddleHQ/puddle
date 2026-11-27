<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import Ellipsis from "@lucide/svelte/icons/ellipsis";
  import { DropdownMenu } from "bits-ui";
  import { ruleName, type Rule } from "#lib/rules/model.ts";
  import "#lib/theme/controls.css";

  // The one "..." button of a rule's row: Edit, Change expiry (Renew once it has lapsed) and Delete.
  // The dialogs these open give focus back to the button, so the menu hands it over as their anchor.
  let {
    rule,
    expired,
    onEdit,
    onExpiry,
    onDelete,
  }: {
    rule: Rule;
    expired: boolean;
    onEdit?: ((rule: Rule, anchor: HTMLElement) => void) | undefined;
    onExpiry: (rule: Rule, anchor: HTMLElement) => void;
    onDelete: (rule: Rule, anchor: HTMLElement) => void;
  } = $props();

  let trigger = $state<HTMLElement | null>(null);
  const name = $derived(ruleName(rule));
</script>

<DropdownMenu.Root>
  <DropdownMenu.Trigger class="btn more-trigger" bind:ref={trigger}>
    <Ellipsis aria-hidden="true" size={16} /><span class="visually-hidden">
      More about {name}
    </span>
  </DropdownMenu.Trigger>
  <DropdownMenu.Portal>
    <DropdownMenu.Content class="row-menu" align="end" sideOffset={4}>
      {#if onEdit}
        <DropdownMenu.Item
          class="row-menu-item"
          onSelect={() => onEdit(rule, trigger!)}
        >
          Edit rule<span class="visually-hidden">: {name}</span>
        </DropdownMenu.Item>
      {/if}
      <DropdownMenu.Item
        class="row-menu-item"
        onSelect={() => onExpiry(rule, trigger!)}
      >
        {expired ? "Renew" : "Change expiry"}<span class="visually-hidden">
          : {name}
        </span>
      </DropdownMenu.Item>
      <DropdownMenu.Item
        class="row-menu-item"
        onSelect={() => onDelete(rule, trigger!)}
      >
        Delete rule<span class="visually-hidden">: {name}</span>
      </DropdownMenu.Item>
    </DropdownMenu.Content>
  </DropdownMenu.Portal>
</DropdownMenu.Root>
