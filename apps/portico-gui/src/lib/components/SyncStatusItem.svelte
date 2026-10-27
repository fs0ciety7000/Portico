<script lang="ts">
  // Status-bar item of the folder sync (phase 11): a monochrome glyph and a
  // label (`Synced 2 min ago`, `Syncing…`, `Waiting for laptop (3f9c0ae1)`,
  // `Sync folder unavailable`, `Sync locked`, `Sync off`). Only the counts
  // (conflicts, received changes to confirm, alerts) carry a status colour.
  import { syncLabel } from "../syncview";
  import { sync } from "../state/sync.svelte";

  const view = $derived(syncLabel(sync.status, sync.now));
  const status = $derived(sync.status);
  const conflicts = $derived(status?.state === "idle" ? Math.max(status.conflicts, sync.conflictCount) : sync.conflictCount);
  const issues = $derived(status?.state === "idle" ? status.issues : 0);
  const received = $derived(sync.received);
  const alert = $derived(status?.state === "alert" || status?.state === "error");
  const title = $derived(
    [
      view.label,
      sync.overview?.folder ? `Folder: ${sync.overview.folder}` : null,
      conflicts ? `${conflicts} ${conflicts === 1 ? "conflict" : "conflicts"}` : null,
      received ? `${received} received ${received === 1 ? "change" : "changes"} to confirm` : null,
      issues ? `${issues} ${issues === 1 ? "issue" : "issues"}` : null,
      status?.state === "disabled" ? "Set up an encrypted folder sync" : "Open the Sync panel",
    ]
      .filter(Boolean)
      .join(" · "),
  );
</script>

<button class="sync" data-tone={view.tone} onclick={() => sync.show(conflicts ? "conflicts" : received ? "received" : "overview")} {title} data-testid="sync-status">
  <svg viewBox="0 0 16 16" aria-hidden="true" class:spin={view.tone === "busy" && status?.state === "syncing"}>
    {#if status?.state === "locked"}
      <rect x="3.5" y="7" width="9" height="6.5" rx="1.5" /><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" />
    {:else if status?.state === "disabled" || !status}
      <path d="M3.2 6.2a5 5 0 0 1 8.6-1.7M12.8 9.8a5 5 0 0 1-8.6 1.7" /><path d="M2.5 13.5 13.5 2.5" />
    {:else if alert || status?.state === "folder_unavailable"}
      <path d="M8 2.25 14.25 13.25H1.75Z" /><path d="M8 6.5v3M8 11.1v.1" />
    {:else}
      <path d="M3.2 6.2a5 5 0 0 1 8.6-1.7M12.8 9.8a5 5 0 0 1-8.6 1.7" /><path d="M12.2 2.2v2.6H9.6M3.8 13.8v-2.6h2.6" />
    {/if}
  </svg>
  <span class="label">{view.label}</span>
  {#if alert}<span class="count error" aria-label="alert">!</span>{/if}
  {#if conflicts}<span class="count warning" aria-label={`${conflicts} conflicts`}>{conflicts}</span>{/if}
  {#if received}<span class="count accent" aria-label={`${received} to confirm`}>{received}</span>{/if}
</button>

<style>
  .sync {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-1);
    height: calc(var(--statusbar-h, 28px) - var(--space-2));
    padding: 0 var(--space-2);
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-secondary);
    font: inherit;
    transition: background var(--motion-fast) var(--ease-standard), color var(--motion-fast) var(--ease-standard);
  }
  .sync:hover { background: var(--bg-elevated); color: var(--text-primary); }
  .sync:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .sync[data-tone="muted"] { color: var(--text-muted); }
  svg { width: var(--size-icon-xs); height: var(--size-icon-xs); fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }
  svg.spin { animation: sync-spin var(--motion-loop) linear infinite; }
  @keyframes sync-spin { to { transform: rotate(360deg); } }
  .label { white-space: nowrap; }
  .count {
    display: inline-grid;
    place-items: center;
    min-width: var(--size-badge-sm);
    height: var(--size-badge-sm);
    padding: 0 var(--space-1);
    border-radius: var(--radius-pill);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    line-height: 1;
  }
  .count.warning { color: var(--status-warning); box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--status-warning) 55%, transparent); }
  .count.error { color: var(--status-error); box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--status-error) 55%, transparent); }
  .count.accent { color: var(--status-authenticating); box-shadow: inset 0 0 0 1px color-mix(in srgb, var(--status-authenticating) 55%, transparent); }
</style>
