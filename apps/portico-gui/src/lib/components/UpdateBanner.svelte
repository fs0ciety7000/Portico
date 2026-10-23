<script lang="ts">
  // The one-time update question (0.3.0): a discreet banner over the host
  // details while `[updates] check` is absent. "Check daily" writes true,
  // "Not now" writes false (never asked again); both change later in
  // Settings. Nothing reaches the network before "Check daily".
  import { fly } from "svelte/transition";
  import { dur } from "../motion";
  import { UPDATE_QUESTION, updates } from "../state/updates.svelte";
</script>

<div class="banner" role="region" aria-label="Update check" data-testid="update-question" transition:fly={{ y: -6, duration: dur("base") }}>
  <svg class="icon" viewBox="0 0 16 16" aria-hidden="true"><path d="M8 12.5V4M4.5 7.5 8 4l3.5 3.5" /></svg>
  <p class="message">{UPDATE_QUESTION}</p>
  <div class="actions">
    <button class="secondary small" disabled={updates.saving} onclick={() => void updates.setCheck(false)} data-testid="update-not-now">Not now</button>
    <button class="primary small" disabled={updates.saving} onclick={() => void updates.setCheck(true)} data-testid="update-check-daily">Check daily</button>
  </div>
</div>

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin-bottom: var(--space-5);
    padding: var(--space-2) var(--space-3) var(--space-2) var(--space-4);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-surface);
    color: var(--text-secondary);
  }
  .icon {
    flex: none;
    width: var(--size-badge-sm);
    height: var(--size-badge-sm);
    fill: none;
    stroke: var(--text-muted);
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .message { flex: 1 1 0; min-width: 0; margin: 0; }
  .actions { display: flex; flex: none; gap: var(--space-2); }
  button {
    display: inline-flex;
    align-items: center;
    height: var(--size-control-sm);
    padding: 0 var(--space-3);
    border-radius: var(--radius-md);
    font: inherit;
    font-size: var(--text-sm);
    transition: background var(--motion-fast) var(--ease-standard), transform var(--motion-fast) var(--ease-standard);
  }
  button:active:not(:disabled) { transform: scale(0.97); }
  button:disabled { opacity: 0.5; }
  .secondary { border: 1px solid var(--border-strong); background: transparent; color: var(--text-primary); }
  .secondary:hover:not(:disabled) { background: var(--bg-elevated); }
  .primary { border: 0; background: var(--accent); color: var(--text-on-accent); font-weight: 600; }
  .primary:hover:not(:disabled) { background: var(--accent-strong); }
  /* Narrow window: the question on one line above, the buttons below it. */
  @media (max-width: 1100px) {
    .banner { flex-wrap: wrap; }
    .message { flex-basis: calc(100% - var(--size-badge-sm) - var(--space-3)); }
    .actions { margin-left: calc(var(--size-badge-sm) + var(--space-3)); }
  }
</style>
