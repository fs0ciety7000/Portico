<script lang="ts">
  // `REC`: the terminal's output is being recorded. In the error colour (a
  // sensitive state: colour is reserved for meaning), with a steady dot;
  // shown from the `started` event to the `stopped` one only.
  import { fade } from "svelte/transition";
  import { dur } from "../motion";

  let { compact = false, title = "Recording this terminal's output" }: { compact?: boolean; title?: string } = $props();
</script>

<span class="rec" class:compact {title} role="status" aria-label="Recording" transition:fade={{ duration: dur("fast") }}>
  <span class="dot" aria-hidden="true"></span>REC
</span>

<style>
  .rec {
    display: inline-flex;
    flex: none;
    align-items: center;
    gap: var(--space-1);
    height: calc(var(--space-4) + var(--space-1));
    padding: 0 var(--space-2);
    border: 1px solid color-mix(in srgb, var(--status-error) 50%, transparent);
    border-radius: var(--radius-pill);
    background: color-mix(in srgb, var(--status-error) 10%, transparent);
    color: var(--status-error);
    font-size: var(--text-xs);
    font-weight: 600;
    letter-spacing: var(--tracking-caps);
    line-height: 1;
  }
  .rec.compact { height: var(--space-4); padding: 0 var(--space-1); }
  .dot {
    width: var(--size-dot);
    height: var(--size-dot);
    border-radius: 50%;
    background: var(--status-error);
  }
</style>
