<script lang="ts">
  // Where a host comes from (phase 6b): "Portico" (editable), "Imported"
  // (Termius import), "~/.ssh/config" (read-only, with a lock). Quiet: a
  // hairline pill in secondary text, never a status color.
  import type { HostOrigin } from "../api";

  let { origin, compact = false }: { origin: HostOrigin; compact?: boolean } = $props();

  const LABELS: Record<HostOrigin, string> = { portico: "Portico", imported: "Imported", ssh_config: "~/.ssh/config" };
  const TITLES: Record<HostOrigin, string> = {
    portico: "Created in Portico (hosts.conf): editable",
    imported: "Imported from Termius: replaced by the next import",
    ssh_config: "Defined in ~/.ssh/config: read-only in Portico",
  };
</script>

<span class="origin" class:compact class:portico={origin === "portico"} data-origin={origin} title={TITLES[origin]}>
  {#if origin === "ssh_config"}
    <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="3.5" y="7" width="9" height="6.5" rx="1.5" /><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2" /></svg>
  {/if}
  {LABELS[origin]}
</span>

<style>
  .origin {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-1);
    height: 20px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    color: var(--text-secondary);
    font-family: var(--font-ui);
    font-size: var(--text-xs);
    font-weight: 500;
    letter-spacing: 0.02em;
    white-space: nowrap;
  }
  .origin[data-origin="ssh_config"] { font-family: var(--font-mono); color: var(--text-muted); }
  /* Portico's own hosts: the hairline takes the accent, the text stays calm. */
  .origin.portico { border-color: color-mix(in srgb, var(--accent) 55%, var(--border-strong)); }
  .origin.compact { height: 16px; padding: 0 var(--space-1); letter-spacing: normal; }
  svg { width: 11px; height: 11px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linejoin: round; }
</style>
