<script lang="ts">
  // Connection status pill. Colour = meaning; animation only while in flight.
  import { formatRtt, type Status } from "../api";

  let { status = "idle", label, latencyMs = null }: {
    status?: Status;
    label?: string;
    latencyMs?: number | null;
  } = $props();

  const labels: Record<Status, string> = {
    idle: "Disconnected",
    connecting: "Connecting",
    authenticating: "Authenticating",
    connected: "Connected",
    warning: "Degraded",
    error: "Failed",
  };
  const busy = $derived(status === "connecting" || status === "authenticating");
</script>

<span class="badge" data-status={status} role="status" aria-live="polite">
  <span class="dot" class:busy></span>
  <span class="label">{label ?? labels[status]}</span>
  {#if (status === "connected" || status === "warning") && latencyMs !== null}
    <span class="latency mono">{formatRtt(latencyMs)}</span>
  {/if}
</span>

<style>
  .badge {
    --c: var(--status-idle);
    display: inline-flex;
    max-width: 100%;
    min-width: 0;
    overflow: hidden;
    white-space: nowrap;
    align-items: center;
    gap: var(--space-2);
    height: 24px;
    padding: 0 var(--space-3) 0 var(--space-2);
    border-radius: var(--radius-pill);
    border: 1px solid color-mix(in srgb, var(--c) 35%, transparent);
    background: color-mix(in srgb, var(--c) 10%, transparent);
    color: var(--text-secondary);
    font-size: var(--text-sm);
    transition: border-color var(--motion-base) var(--ease-standard),
      background var(--motion-base) var(--ease-standard);
  }
  [data-status="connecting"] { --c: var(--status-connecting); }
  [data-status="authenticating"] { --c: var(--status-authenticating); }
  [data-status="connected"] { --c: var(--status-connected); }
  [data-status="warning"] { --c: var(--status-warning); }
  [data-status="error"] { --c: var(--status-error); }

  /* A failure reads in the status colour, like the pane header's phase. */
  [data-status="error"] .label { color: var(--status-error); }
  .label { min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .dot, .latency { flex: none; }
  .dot {
    width: var(--size-dot);
    height: var(--size-dot);
    border-radius: 50%;
    background: var(--c);
    box-shadow: 0 0 0 0 color-mix(in srgb, var(--c) 60%, transparent);
  }
  .dot.busy {
    animation: pulse var(--motion-loop) var(--ease-standard) infinite;
  }
  .latency {
    color: var(--text-muted);
    font-size: var(--text-xs);
  }
  @keyframes pulse {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--c) 60%, transparent); }
    100% { box-shadow: 0 0 0 6px transparent; }
  }
</style>
