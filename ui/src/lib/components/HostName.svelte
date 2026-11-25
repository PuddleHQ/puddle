<!-- SPDX-License-Identifier: AGPL-3.0-or-later -->
<script lang="ts">
  import { hostParts } from "#lib/format/host.ts";

  // How Puddle draws a host name everywhere: the registrable part bold (`api.`**`stripe.com`**),
  // port 443 grey, a leading `*.` as a wildcard mark.
  let {
    host,
    port = null,
    registrable,
  }: {
    /** `api.example.com`, `*.example.com` or `example.com:8443`. */
    host: string;
    port?: number | null;
    /** The registrable domain when the service said it. */
    registrable?: string | undefined;
  } = $props();

  const parts = $derived(
    hostParts(host, {
      port,
      ...(registrable === undefined ? {} : { registrable }),
    }),
  );
</script>

<span class="host mono"
  >{#if parts.wildcard}<span
      class="wild"
      title="This host and every subdomain of it">*.</span
    >{/if}{#if parts.lead}<span class="lead">{parts.lead}</span>{/if}<b
    class="main">{parts.main}</b
  >{#if parts.port !== null}<span class="port" class:usual={parts.usualPort}
      >:{parts.port}</span
    >{/if}</span
>

<style>
  .host {
    overflow-wrap: anywhere;
  }
  .main {
    font-weight: 700;
  }
  .lead {
    font-weight: 400;
  }
  .wild {
    color: var(--color-warning);
    font-weight: 700;
  }
  .port {
    color: var(--color-text);
  }
  .port.usual {
    color: var(--color-text-muted);
    opacity: 0.7;
  }
</style>
