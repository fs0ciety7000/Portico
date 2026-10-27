<script lang="ts">
  import { fly } from "svelte/transition";
  import { COPIED_FEEDBACK_MS, dur } from "../motion";
  import { describe, type Host, type SessionView } from "../api";
  import StatusBadge from "./StatusBadge.svelte";
  import Mark from "./Mark.svelte";
  import { keymap } from "../state/keymap.svelte";
  import { hosts } from "../state/hosts.svelte";
  import OriginBadge from "./OriginBadge.svelte";
  import { INCLUDE_MESSAGE, READ_ONLY_IMPORTED } from "../hostform";
  import { updates } from "../state/updates.svelte";
  import UpdateBanner from "./UpdateBanner.svelte";
  import { sync } from "../state/sync.svelte";
  import { destination } from "../syncview";

  let {
    host,
    session,
    section = null,
    onconnect,
    ondisconnect,
    onfavorite,
    onedit,
    onlogs,
    onduplicate,
    ondelete,
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
    /** "Duplicate as Portico host…" (phase 6b). */
    onduplicate: (host: Host) => void;
    /** "Delete host" (a Portico host). */
    ondelete: (host: Host) => void;
  } = $props();

  const status = $derived(describe(session));
  const live = $derived(status.busy || session?.phase.phase === "connected");
  /** A Portico host: the whole host is edited, not only its metadata. */
  const editKey = $derived(host?.origin === "portico" ? (keymap.label("hosts.edit") ?? keymap.label("host.edit")) : keymap.label("hosts.edit"));

  let copied = $state(false);
  let includeEl: HTMLInputElement | undefined = $state();

  async function copyInclude(line: string) {
    try {
      await navigator.clipboard.writeText(line);
      copied = true;
      setTimeout(() => (copied = false), COPIED_FEEDBACK_MS);
    } catch {
      // No clipboard here: select the line for a manual copy.
      includeEl?.focus();
      includeEl?.select();
    }
  }
</script>

<section class="details">
  {#if updates.ask}
    <UpdateBanner />
  {/if}
  {#if hosts.showBanner && hosts.include}
    {@const line = hosts.include.line}
    <div class="banner" role="note" aria-label="Include line for Portico hosts" transition:fly={{ y: -6, duration: dur("base") }}>
      <svg class="info" viewBox="0 0 16 16" aria-hidden="true"><circle cx="8" cy="8" r="6" /><path d="M8 7.25v4M8 4.9v.1" /></svg>
      <div class="banner-body">
        <p class="message" title={INCLUDE_MESSAGE}>{INCLUDE_MESSAGE.slice(0, -"~/.ssh/config:".length)}<span class="mono">~/.ssh/config</span>:</p>
        <div class="include">
          <input class="mono" readonly value={line} bind:this={includeEl} aria-label="Include line" onfocus={(e) => e.currentTarget.select()} />
          <button class="secondary small" title={line} aria-label="Copy the Include line" onclick={() => void copyInclude(line)}>{copied ? "Copied" : "Copy"}</button>
        </div>
        <p class="small more">Portico never edits that file. Your Portico hosts already work here without it.</p>
      </div>
      <button class="dismiss" onclick={() => hosts.dismissBanner()} aria-label="Dismiss" title="Dismiss">
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m5 5 6 6M11 5l-6 6" /></svg>
      </button>
    </div>
  {/if}
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
              <OriginBadge origin={host.origin} importedFrom={host.importedFrom} />
            </div>
            <p class="target mono">
              {host.user ?? "(local user)"}@{host.hostname}<span class="port">:{host.port}</span>
            </p>
          </div>
          <div class="actions">
            <StatusBadge status={status.status} label={status.label} latencyMs={session?.latencyMs ?? null} />
            <button
              class="secondary"
              onclick={() => onedit(host)}
              title={host.origin === "portico" ? keymap.tip("Edit host…", "host.edit") : keymap.tip("Edit tags, group and notes", "hosts.edit")}
            >
              Edit{#if editKey} <kbd>{editKey}</kbd>{/if}
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

        {#if sync.pendingByAlias[host.alias]}
          {@const change = sync.pendingByAlias[host.alias]}
          {#if change}
            <div class="received" data-testid="host-received">
              <span>
                Changed by <span class="mono">{change.devices.join(", ") || "another device"}</span>: the new destination
                <span class="mono">{destination(change.incoming)}</span> waits for your confirmation. Until then this device keeps the one above.
              </span>
              <button class="secondary" onclick={() => sync.reviewHost(change)}>Review…</button>
            </div>
          {/if}
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
          <dt>Defined in</dt>
          <dd class="defined">
            <span class="mono muted">{host.source}</span>
            {#if host.origin === "portico"}
              <span class="origin-note">Portico's own file, editable here.</span>
              <button class="link" onclick={() => ondelete(host)}>Delete host{#if keymap.label("host.delete")} <kbd>{keymap.label("host.delete")}</kbd>{/if}</button>
            {:else}
              <span class="origin-note">{host.origin === "imported" ? READ_ONLY_IMPORTED : "Read-only: Portico never edits ~/.ssh/config."}</span>
              <button class="link" onclick={() => onduplicate(host)}>
                Duplicate as Portico host…{#if keymap.label("host.duplicate")} <kbd>{keymap.label("host.duplicate")}</kbd>{/if}
              </button>
            {/if}
          </dd>
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
    display: flex;
    flex-direction: column;
    padding: var(--space-6) var(--space-7);
    overflow-y: auto;
    min-height: 0;
  }
  .details > .placeholder { flex: 1 1 auto; height: auto; }
  /* The Include line (phase 6b): optional information, quiet (no accent). */
  .banner {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    gap: var(--space-3);
    align-items: start;
    margin-bottom: var(--space-5);
    padding: var(--space-3) var(--space-3) var(--space-3) var(--space-4);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-surface);
    color: var(--text-muted);
  }
  .banner p { margin: 0; }
  .banner .mono { color: var(--text-secondary); }
  .banner-body { display: grid; gap: var(--space-2); min-width: 0; }
  .banner .info {
    width: var(--size-badge-sm);
    height: var(--size-badge-sm);
    margin-top: calc(var(--space-1) / 2);
    fill: none;
    stroke: var(--text-muted);
    stroke-width: 1.5;
    stroke-linecap: round;
  }
  .include { display: flex; gap: var(--space-2); align-items: center; max-width: 72ch; }
  .include input {
    flex: 1;
    min-width: 0;
    height: var(--size-control-sm);
    padding: 0 var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font-size: var(--text-sm);
    outline: none;
  }
  .include input:focus { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .details :global(button.secondary.small) { height: var(--size-control-sm); padding: 0 var(--space-3); font-size: var(--text-sm); }
  .small { font-size: var(--text-xs); }
  /* Narrow window (880): one line. The message (whole in its tooltip) and
     Copy; the Include line itself is in Copy's tooltip and the palette. */
  @media (max-width: 1100px) {
    .banner { align-items: center; padding-block: var(--space-2); }
    .banner .info { margin-top: 0; }
    .banner-body { display: flex; align-items: center; gap: var(--space-3); }
    .banner .message { flex: 1 1 0; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .include { flex: none; }
    .include input { display: none; }
    .banner .more { display: none; }
  }
  .dismiss {
    display: grid;
    place-items: center;
    width: var(--space-5);
    height: var(--space-5);
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    transition: background var(--motion-fast) var(--ease-standard), color var(--motion-fast) var(--ease-standard);
  }
  .dismiss:hover { background: var(--bg-elevated); color: var(--text-primary); }
  .dismiss svg { width: var(--size-icon-sm); height: var(--size-icon-sm); fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; }
  .defined { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-1) var(--space-3); word-break: normal; }
  .defined .mono { word-break: break-all; }
  .origin-note { color: var(--text-muted); font-size: var(--text-sm); }
  .defined button.link { margin-left: 0; height: var(--space-5); font-size: var(--text-sm); color: var(--text-secondary); }
  .defined button.link:hover { color: var(--text-primary); }
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
    height: var(--size-control);
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
  /* Shared with the banners above the details (UpdateBanner). */
  .details :global(button.secondary) {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--size-control);
    padding: 0 var(--space-4);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-primary);
    font: inherit;
    transition: background var(--motion-fast) var(--ease-standard);
  }
  .details :global(button.secondary:hover:not(:disabled)) { background: var(--bg-elevated); }
  .details :global(button.secondary:disabled) { opacity: 0.5; }
  .hint {
    margin-top: var(--space-5);
    padding: var(--space-3) var(--space-4);
    border-left: var(--size-bar) solid var(--status-error);

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
  button.star svg { width: var(--size-badge-sm); height: var(--size-badge-sm); fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linejoin: round; }
  button.star.on { color: var(--accent); }
  button.star.on svg { fill: currentColor; }
  .tags { display: flex; flex-wrap: wrap; gap: var(--space-2); margin-top: var(--space-4); }
  .group {
    padding: calc(var(--space-1) / 2) var(--space-2);
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
    padding: calc(var(--space-1) / 2) var(--space-2);
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
  /* A received destination (sync, F1): a status-coloured hairline, not an alarm. */
  .received {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    margin-top: var(--space-4);
    padding: var(--space-3) var(--space-4);
    border-left: 2px solid var(--status-warning);
    border-radius: var(--radius-md);
    background: var(--bg-surface);
    color: var(--text-secondary);
    font-size: var(--text-sm);
  }
  .received button { flex: 0 0 auto; margin-left: auto; }
</style>
