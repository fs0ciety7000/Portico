<script lang="ts">
  // The one-time update question (0.3.0): a discreet banner over the host
  // details while `[updates] check` is absent. "Check daily" writes true,
  // "Not now" writes false (never asked again); both change later in
  // Settings. Nothing reaches the network before "Check daily". Quiet like
  // the Include banner: same icon, both answers the same weight (no accent:
  // neither is the expected one). The buttons use HostDetails' shared
  // `secondary small` style.
  import { fly } from "svelte/transition";
  import { dur } from "../motion";
  import { UPDATE_QUESTION, updates } from "../state/updates.svelte";
</script>

<div class="banner" role="region" aria-label="Update check" data-testid="update-question" transition:fly={{ y: -6, duration: dur("base") }}>
  <svg class="info" viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="6" /><path d="M8 7.25v4M8 4.9v.1" /></svg>
  <p class="message">{UPDATE_QUESTION}</p>
  <div class="actions">
    <button class="secondary small" disabled={updates.saving} onclick={() => void updates.setCheck(false)} data-testid="update-not-now">Not now</button>
    <button class="secondary small" disabled={updates.saving} onclick={() => void updates.setCheck(true)} data-testid="update-check-daily">Check daily</button>
  </div>
</div>

<style>
  .banner {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin-bottom: var(--space-5);
    padding: var(--space-3) var(--space-3) var(--space-3) var(--space-4);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-surface);
    color: var(--text-secondary);
  }
  .info {
    flex: none;
    width: var(--size-badge-sm);
    height: var(--size-badge-sm);
    fill: none;
    stroke: var(--text-muted);
    stroke-width: 1.5;
    stroke-linecap: round;
  }
  .message { flex: 1 1 0; min-width: 0; margin: 0; }
  .actions { display: flex; flex: none; gap: var(--space-2); }
  /* Narrow window: the question on one line above, the buttons below it. */
  @media (max-width: 1100px) {
    .banner { flex-wrap: wrap; padding-block: var(--space-2); }
    .message { flex-basis: calc(100% - var(--size-badge-sm) - var(--space-3)); }
    .actions { margin-left: calc(var(--size-badge-sm) + var(--space-3)); }
  }
</style>
