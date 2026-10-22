<script lang="ts">
  // A caution line: a warning triangle and the text, on the warning tint
  // (`warning`: an action the user should take, a secret leaving) or on the
  // elevated surface (`note`: good to know). Shared by the import dialog and
  // the new-host-key prompt.
  import type { Snippet } from "svelte";

  let { tone = "warning", testid, children }: { tone?: "warning" | "note"; testid?: string; children: Snippet } = $props();
</script>

<p class="caution" data-tone={tone} data-testid={testid} role="note">
  <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 2.25 14.25 13.25H1.75Z" /><path d="M8 6.5v3M8 11.1v.1" /></svg>
  <span>{@render children()}</span>
</p>

<style>
  .caution {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
    margin: 0;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    color: var(--text-primary);
    font-size: var(--text-sm);
  }
  .caution[data-tone="warning"] { background: color-mix(in srgb, var(--status-warning) 10%, transparent); }
  .caution[data-tone="note"] { background: var(--bg-elevated); color: var(--text-secondary); }
  svg {
    flex: 0 0 auto;
    width: var(--space-4);
    /* As tall as the first line of text: the icon sits centered on it. */
    height: 1lh;
    fill: none;
    stroke: var(--status-warning);
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .caution[data-tone="note"] svg { stroke: var(--text-muted); }
</style>
