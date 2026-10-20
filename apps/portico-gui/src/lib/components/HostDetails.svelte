<script lang="ts">
  import { fly } from "svelte/transition";
  import { describe, type Host, type SessionView } from "../api";
  import StatusBadge from "./StatusBadge.svelte";
  import { kbd } from "../keys";

  let {
    host,
    session,
    onconnect,
    ondisconnect,
  }: {
    host: Host | undefined;
    session: SessionView | undefined;
    onconnect: (host: Host) => void;
    ondisconnect: (host: Host) => void;
  } = $props();

  const status = $derived(describe(session));
  const live = $derived(status.busy || session?.phase.phase === "connected");
</script>

<section class="details">
  {#if host}
    {#key host.alias}
      <div class="content" in:fly={{ y: 6, duration: 180 }}>
        <header>
          <div>
            <h1>{host.alias}</h1>
            <p class="target mono">
              {host.user ?? "(local user)"}@{host.hostname}<span class="port">:{host.port}</span>
            </p>
          </div>
          <div class="actions">
            <StatusBadge status={status.status} label={status.label} latencyMs={session?.latencyMs ?? null} />
            {#if live}
              <button class="secondary" onclick={() => ondisconnect(host)}>
                Disconnect <kbd>D</kbd>
              </button>
            {:else}
              <button class="primary" onclick={() => onconnect(host)}>
                Connect <kbd>↵</kbd>
              </button>
            {/if}
          </div>
        </header>

        {#if host.tags.length}
          <div class="tags">
            {#each host.tags as tag (tag)}<span class="tag">#{tag}</span>{/each}
          </div>
        {/if}

        <dl>
          {#if host.proxyJump.length}
            <dt>Route</dt>
            <dd class="route">
              <span>You</span>
              {#each host.proxyJump as hop (hop)}<span class="arrow">→</span><span>{hop}</span>{/each}
              <span class="arrow">→</span><span class="final">{host.alias}</span>
            </dd>
          {/if}
          {#each host.identityFiles as f (f)}
            <dt>Identity</dt><dd class="mono">{f}</dd>
          {/each}
          {#each host.localForwards as f (f)}
            <dt>Forward −L</dt><dd class="mono">{f}</dd>
          {/each}
          {#each host.remoteForwards as f (f)}
            <dt>Forward −R</dt><dd class="mono">{f}</dd>
          {/each}
          <dt>Defined in</dt><dd class="mono muted">{host.source}</dd>
        </dl>

        {#if session?.phase.phase === "failed" && session.phase.error.hint}
          <p class="hint">{session.phase.error.hint}</p>
        {/if}

        {#each host.warnings as w (w)}
          <p class="warning">⚠ {w}</p>
        {/each}
      </div>
    {/key}
  {:else}
    <div class="placeholder">
      <img src="/portico-mark.svg" alt="" width="72" height="72" />
      <p>Select a host to see its effective configuration.</p>
      <p class="muted"><kbd>↑</kbd> <kbd>↓</kbd> to move · <kbd>↵</kbd> to connect · <kbd>{kbd("K")}</kbd> commands</p>
    </div>
  {/if}
</section>

<style>
  .details {
    padding: var(--space-6) var(--space-7);
    overflow-y: auto;
    min-height: 0;
  }
  header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    gap: var(--space-5);
  }
  h1 {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 500;
    font-size: 34px;
    letter-spacing: 0.01em;
  }
  .target { margin: var(--space-1) 0 0; color: var(--text-secondary); }
  .port { color: var(--text-muted); }
  .actions { display: flex; align-items: center; gap: var(--space-3); }

  button.primary {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: 32px;
    padding: 0 var(--space-4);
    border: 0;
    border-radius: var(--radius-md);
    background: var(--accent);
    color: var(--text-on-accent);
    font: inherit;
    font-weight: 600;
    transition: background var(--motion-fast) var(--ease-standard),
      transform var(--motion-fast) var(--ease-standard);
  }
  button.primary:hover { background: var(--accent-strong); }
  button.primary:active { transform: scale(0.97); }
  button.secondary {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: 32px;
    padding: 0 var(--space-4);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-primary);
    font: inherit;
    transition: background var(--motion-fast) var(--ease-standard);
  }
  button.secondary:hover { background: var(--bg-elevated); }
  .hint {
    margin-top: var(--space-5);
    padding: var(--space-3) var(--space-4);
    border-left: 2px solid var(--status-error);
    background: color-mix(in srgb, var(--status-error) 8%, transparent);
    color: var(--text-secondary);
    border-radius: var(--radius-sm);
  }
  button.primary kbd {
    background: color-mix(in srgb, var(--text-on-accent) 12%, transparent);
    border-color: transparent;
    color: inherit;
  }

  .tags { display: flex; gap: var(--space-2); margin-top: var(--space-4); }
  .tag {
    padding: 2px var(--space-2);
    border-radius: var(--radius-pill);
    border: 1px solid var(--border-strong);
    color: var(--text-secondary);
    font-size: var(--text-xs);
  }

  dl {
    display: grid;
    grid-template-columns: 120px 1fr;
    gap: var(--space-3) var(--space-4);
    margin: var(--space-6) 0 0;
    padding-top: var(--space-5);
    border-top: 1px solid var(--border-subtle);
  }
  dt { color: var(--text-muted); font-size: var(--text-sm); }
  dd { margin: 0; color: var(--text-secondary); word-break: break-all; }
  .muted { color: var(--text-muted); }
  .route { display: flex; flex-wrap: wrap; gap: var(--space-2); }
  .arrow { color: var(--text-muted); }
  .final { color: var(--text-primary); }
  .warning { color: var(--status-warning); }

  .placeholder {
    height: 100%;
    display: grid;
    place-content: center;
    justify-items: center;
    gap: var(--space-2);
    color: var(--text-secondary);
    text-align: center;
  }
  .placeholder img { opacity: 0.85; margin-bottom: var(--space-3); }
</style>
