<script lang="ts">
  // Tabs in the title bar: Hosts, then one tab per terminal with its live
  // status dot. The alias always comes first; the remote title follows.
  import { flip } from "svelte/animate";
  import { fly } from "svelte/transition";
  import { describe } from "../api";
  import { kbd } from "../keys";
  import { isLive, workspace } from "../state/workspace.svelte";

  let { hostCount }: { hostCount: number } = $props();

  let strip: HTMLDivElement | undefined = $state();

  // Keep the active tab in view when there are many.
  $effect(() => {
    const key = workspace.active;
    strip?.querySelector<HTMLElement>(`[data-key="${key ?? "home"}"]`)?.scrollIntoView({ block: "nearest", inline: "nearest" });
  });

  function dotOf(tab: (typeof workspace.tabs)[number]) {
    const session = workspace.sessions[tab.session];
    if (tab.end || (!isLive(session) && session?.phase.phase !== "failed")) return { status: "idle", busy: false, hollow: true };
    const d = describe(session);
    return { status: d.status, busy: d.busy, hollow: false };
  }
</script>

<div class="strip" role="tablist" aria-label="Workspace" bind:this={strip} data-tauri-drag-region>
  <button
    class="tab home"
    role="tab"
    data-key="home"
    aria-selected={workspace.active === null}
    title={`Hosts (${kbd("0")})`}
    onclick={() => (workspace.active = null)}
  >
    <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 6.5 8 2.5l5.5 4" /><path d="M4 6.5v7h8v-7" /></svg>
    <span>Hosts</span>
    <span class="count">{hostCount}</span>
  </button>

  {#each workspace.tabs as tab, i (tab.key)}
    {@const dot = dotOf(tab)}
    <div
      class="tab"
      role="tab"
      tabindex="0"
      data-key={tab.key}
      aria-selected={workspace.active === tab.key}
      title={tab.title ? `${tab.alias} — ${tab.title}` : tab.alias}
      animate:flip={{ duration: 180 }}
      in:fly={{ y: -4, duration: 180 }}
      onclick={() => (workspace.active = tab.key)}
      onkeydown={(e) => e.key === "Enter" && (workspace.active = tab.key)}
      onauxclick={(e) => e.button === 1 && workspace.close(tab.key)}
    >
      <span class="dot" data-status={dot.status} class:busy={dot.busy} class:hollow={dot.hollow}></span>
      <span class="alias">{tab.alias}</span>
      {#if tab.title && workspace.active === tab.key}<span class="remote">{tab.title}</span>{/if}
      <span class="slot">
        {#if i < 9}<span class="index">{i + 1}</span>{/if}
        <button
          class="close"
          aria-label={`Close ${tab.alias}`}
          tabindex="-1"
          onclick={(e) => {
            e.stopPropagation();
            workspace.close(tab.key);
          }}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4.5 4.5 7 7m0-7-7 7" /></svg>
        </button>
      </span>
    </div>
  {/each}
</div>

<style>
  .strip {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    overflow-x: auto;
    scrollbar-width: none;
  }
  .strip::-webkit-scrollbar { display: none; }

  .tab {
    position: relative;
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    flex: 0 0 auto;
    max-width: 320px;
    height: 30px;
    padding: 0 var(--space-2) 0 var(--space-3);
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-muted);
    font: inherit;
    font-size: var(--text-sm);
    cursor: default;
    transition: background var(--motion-fast) var(--ease-standard),
      color var(--motion-fast) var(--ease-standard);
  }
  .tab:hover { background: var(--bg-surface); color: var(--text-secondary); }
  .tab[aria-selected="true"] {
    background: var(--bg-selected);
    color: var(--text-primary);
  }
  .tab[aria-selected="true"]::after {
    content: "";
    position: absolute;
    left: var(--space-3);
    right: var(--space-3);
    bottom: 0;
    height: 2px;
    border-radius: 2px;
    background: var(--accent);
  }
  .home { padding-right: var(--space-3); }
  .home svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
    stroke-linejoin: round;
  }
  .count, .index {
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    color: var(--text-muted);
  }
  .alias {
    flex: 0 1 auto;
    min-width: 3ch;
    max-width: 160px;
    font-weight: 550;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .remote {
    flex: 0 100 auto;
    min-width: 0;
    color: var(--text-muted);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  /* Tab number and close button share one slot: no jump on hover. */
  .slot { display: grid; place-items: center; width: 18px; height: 18px; }
  .slot > * { grid-area: 1 / 1; }

  .close {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    opacity: 0;
    transition: opacity var(--motion-fast) var(--ease-standard),
      background var(--motion-fast) var(--ease-standard);
  }
  .close svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; }
  .tab:hover .close, .tab[aria-selected="true"] .close { opacity: 1; }
  .tab:hover .index, .tab[aria-selected="true"] .index { opacity: 0; }
  .close:hover { background: var(--bg-elevated); color: var(--text-primary); }

  .dot {
    --c: var(--status-idle);
    flex: 0 0 auto;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--c);
  }
  .dot.hollow { background: transparent; box-shadow: inset 0 0 0 1.5px var(--c); }
  .dot[data-status="connecting"] { --c: var(--status-connecting); }
  .dot[data-status="authenticating"] { --c: var(--status-authenticating); }
  .dot[data-status="connected"] { --c: var(--status-connected); }
  .dot[data-status="warning"] { --c: var(--status-warning); }
  .dot[data-status="error"] { --c: var(--status-error); }
  .dot.busy { animation: pulse 1.2s var(--ease-standard) infinite; }
  @keyframes pulse {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--c) 60%, transparent); }
    100% { box-shadow: 0 0 0 5px transparent; }
  }
</style>
