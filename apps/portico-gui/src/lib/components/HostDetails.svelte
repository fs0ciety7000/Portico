<script lang="ts">
  import { fly } from "svelte/transition";
  import { dur } from "../motion";
  import { describe, type Host, type SessionView } from "../api";
  import StatusBadge from "./StatusBadge.svelte";
  import Mark from "./Mark.svelte";
  import { keymap } from "../state/keymap.svelte";

  let {
    host,
    session,
    section = null,
    onconnect,
    ondisconnect,
    onfavorite,
    onedit,
    onlogs,
  }: {
    host: Host | undefined;
    session: SessionView | undefined;
    /** A section header is selected instead of a host. */
    section?: { title: string; count: number; collapsed: boolean } | null;
    onconnect: (host: Host) => void;
    ondisconnect: (host: Host) => void;
    onfavorite: (host: Host) => void;
    onedit: (host: Host) => void;
    /** Session logs of this host's latest session. */
    onlogs: () => void;
  } = $props();

  const status = $derived(describe(session));
  const live = $derived(status.busy || session?.phase.phase === "connected");
</script>

<section class="details">
  {#if host}
    {#key host.alias}
      <div class="content" in:fly={{ y: 6, duration: dur("base") }}>
        <header>
          <div class="identity">
            <div class="title-row">
              <h1 title={host.alias}>{host.alias}</h1>
              <button
                class="star"
                class:on={host.favorite}
                aria-pressed={host.favorite}
                title={keymap.tip(host.favorite ? "Remove from favorites" : "Add to favorites", "app.favorite")}
                aria-label={host.favorite ? "Remove from favorites" : "Add to favorites"}
                onclick={() => onfavorite(host)}
              >
                <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m8 1.9 1.8 3.8 4.1.5-3 2.8.8 4.1L8 11.1l-3.7 2 .8-4.1-3-2.8 4.1-.5Z" /></svg>
              </button>
            </div>
            <p class="target mono">
              {host.user ?? "(local user)"}@{host.hostname}<span class="port">:{host.port}</span>
            </p>
          </div>
          <div class="actions">
            <StatusBadge status={status.status} label={status.label} latencyMs={session?.latencyMs ?? null} />
            <button class="secondary" onclick={() => onedit(host)} title={keymap.tip("Edit tags, group and notes", "hosts.edit")}>
              Edit{#if keymap.label("hosts.edit")} <kbd>{keymap.label("hosts.edit")}</kbd>{/if}
            </button>
            {#if live}
              <button class="secondary" onclick={() => ondisconnect(host)}>
                Disconnect{#if keymap.label("hosts.disconnect")} <kbd>{keymap.label("hosts.disconnect")}</kbd>{/if}
              </button>
            {:else}
              <button class="primary" onclick={() => onconnect(host)}>
                Connect{#if keymap.label("hosts.connect")} <kbd>{keymap.label("hosts.connect")}</kbd>{/if}
              </button>
            {/if}
          </div>
        </header>

        {#if host.tags.length || host.group}
          <div class="tags">
            {#if host.group}<span class="group">{host.group}</span>{/if}
            {#each host.tags as tag (tag)}<span class="tag">#{tag}</span>{/each}
          </div>
        {/if}

        {#if host.notes}
          <p class="notes">{host.notes}</p>
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

        {#if session?.phase.phase === "failed"}
          <div class="hint">
            {#if session.phase.error.hint}<p>{session.phase.error.hint}</p>{/if}
            <button class="link" onclick={onlogs}>Show session logs{#if keymap.label("app.logs")} <kbd>{keymap.label("app.logs")}</kbd>{/if}</button>
          </div>
        {/if}

        {#each host.warnings as w (w)}
          <p class="warning">⚠ {w}</p>
        {/each}
      </div>
    {/key}
  {:else if section}
    <div class="placeholder">
      <p class="section-name">{section.title}</p>
      <p>{section.count} {section.count === 1 ? "host" : "hosts"}{section.collapsed ? " · folded" : ""}</p>
      <p class="muted"><kbd>↵</kbd> {section.collapsed ? "unfold" : "fold"} · <kbd>←</kbd> <kbd>→</kbd> fold, unfold · <kbd>↓</kbd> next host</p>
    </div>
  {:else}
    <div class="placeholder">
      <Mark size={56} />
      <p>Select a host to see its effective configuration.</p>
      <p class="muted"><kbd>↑</kbd> <kbd>↓</kbd> to move · <kbd>↵</kbd> to connect{#if keymap.label("app.palette")}{" · "}<kbd>{keymap.label("app.palette")}</kbd> commands{/if}</p>
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
    flex-wrap: wrap;
    justify-content: space-between;
    align-items: flex-start;
    gap: var(--space-4) var(--space-5);
  }
  @media (max-width: 1100px) {
    .details { padding: var(--space-5); }
  }
  /* Long aliases wrap anywhere, two lines at most (full name in the tooltip). */
  h1 {
    display: -webkit-box;
    min-width: 0;
    margin: 0;
    overflow: hidden;
    overflow-wrap: anywhere;
    -webkit-box-orient: vertical;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    font-family: var(--font-display);
    font-weight: 600;
    font-size: var(--text-display);
    letter-spacing: -0.015em;
  }
  .target { margin: var(--space-1) 0 0; color: var(--text-secondary); }
  .port { color: var(--text-muted); }
  .actions { display: flex; flex-shrink: 0; align-items: center; gap: var(--space-3); }

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

  .identity { flex: 1 1 auto; min-width: 0; }
  .title-row { display: flex; align-items: center; gap: var(--space-2); }
  .title-row button.star { flex: 0 0 auto; }
  .target { overflow-wrap: anywhere; }
  button.star {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-muted);
    transition: color var(--motion-fast) var(--ease-standard), background var(--motion-fast) var(--ease-standard),
      transform var(--motion-fast) var(--ease-standard);
  }
  button.star:hover { background: var(--bg-elevated); color: var(--text-secondary); }
  button.star:active { transform: scale(0.92); }
  button.star svg { width: 16px; height: 16px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linejoin: round; }
  button.star.on { color: var(--accent); }
  button.star.on svg { fill: currentColor; }
  .tags { display: flex; flex-wrap: wrap; gap: var(--space-2); margin-top: var(--space-4); }
  .group {
    padding: 2px var(--space-2);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    color: var(--text-secondary);
    font-size: var(--text-xs);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .notes {
    margin: var(--space-4) 0 0;
    max-width: 72ch;
    color: var(--text-secondary);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    user-select: text;
  }
  .hint p { margin: 0 0 var(--space-2); }
  button.link {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: 28px;
    padding: 0 var(--space-2);
    margin-left: calc(-1 * var(--space-2));
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-primary);
    font: inherit;
  }
  button.link:hover { background: var(--bg-elevated); }
  .section-name {
    margin: 0;
    font-size: var(--text-xs);
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .placeholder p { margin: 0; }
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
  .placeholder :global(svg) { opacity: 0.85; margin-bottom: var(--space-3); }
</style>
