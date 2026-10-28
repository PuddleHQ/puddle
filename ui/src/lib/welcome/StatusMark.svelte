<!-- SPDX-License-Identifier: GPL-3.0-or-later -->
<script lang="ts">
  import { STATUS_LABEL, type DoctorStatus } from "./doctor.ts";

  // The round symbol in front of a check. The word for it is in the text for screen readers: the
  // colour and the symbol are never the only signal.
  let { status }: { status: DoctorStatus } = $props();

  // The tick and the cross are drawn, not typed: the bundled fonts have no ✓ or ✕, and a character
  // from a system font makes the browser search the system's fonts the first time it is shown. On
  // Windows that search waits on the system font cache, and with a busy system disk it held the
  // page for seconds, on the first screen of a fresh install.
  const TEXT: Partial<Record<DoctorStatus, string>> = {
    info: "i",
    warn: "!",
    skipped: "–",
  };
</script>

<span class="mark {status}" aria-hidden="true">
  {#if status === "ok"}
    <svg viewBox="0 0 16 16"><path d="M3.5 8.5l3 3 6-7" /></svg>
  {:else if status === "fail"}
    <svg viewBox="0 0 16 16"><path d="M4.5 4.5l7 7m0-7l-7 7" /></svg>
  {:else}
    {TEXT[status]}
  {/if}
</span>
<span class="visually-hidden">{STATUS_LABEL[status]}: </span>

<style>
  .mark {
    flex: none;
    display: grid;
    place-items: center;
    width: 1.5rem;
    height: 1.5rem;
    border-radius: 50%;
    border: 2px solid currentColor;
    font-size: var(--text-sm);
    font-weight: 800;
    line-height: 1;
  }
  svg {
    width: 1rem;
    height: 1rem;
    fill: none;
    stroke: currentColor;
    stroke-width: 2.25;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .ok {
    color: var(--color-success);
  }
  .info,
  .skipped {
    color: var(--color-text-muted);
  }
  .warn {
    color: var(--color-warning);
  }
  .fail {
    color: var(--color-danger);
  }
</style>
