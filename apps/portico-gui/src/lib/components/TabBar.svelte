<script lang="ts">
  // Tabs in the title bar: Hosts (pinned), then one tab per terminal with its
  // live status dot. A tab speaks for its focused pane: its alias first, its
  // remote title on the active tab only (unless split: alias and pane count
  // are enough there), and the number of panes when the tab is split.
  //
  // Dot grammar (`paneStatus`, shared with the pane header and the status
  // bar): the colour is the status, a failure is a filled error dot, the
  // hollow dot means "no shell attached".
  //
  // Keyboard: one tab stop (roving tabindex); ←/→ move, Home/End jump,
  // Delete closes.
  import { flip } from "svelte/animate";
  import { fly } from "svelte/transition";
  import { keymap } from "../state/keymap.svelte";
  import { dur } from "../motion";
  import { panesOf } from "../panes";
  import { paneStatus, workspace, type Pane } from "../state/workspace.svelte";
  import { recordings } from "../state/recordings.svelte";
  import RecBadge from "./RecBadge.svelte";

  let { hostCount }: { hostCount: number } = $props();

  let strip: HTMLDivElement | undefined = $state();
  let fadeStart = $state(false);
  let fadeEnd = $state(false);

  function updateFades() {
    if (!strip) return;
    fadeStart = strip.scrollLeft > 0;
    fadeEnd = strip.scrollLeft < strip.scrollWidth - strip.clientWidth - 1;
  }

  // Keep the active tab in view, once its final width is laid out (the
  // remote title appears on activation).
  $effect(() => {
    const key = workspace.active;
    void workspace.activePane?.title;
    void workspace.tabs.length;
    requestAnimationFrame(() => {
      strip
        ?.querySelector<HTMLElement>(`[data-key="${key ?? "home"}"]`)
        ?.scrollIntoView({ block: "nearest", inline: "nearest" });
      updateFades();
    });
  });

  function dotOf(pane: Pane | undefined) {
    return paneStatus(pane, pane ? workspace.sessions[pane.session] : undefined);
  }

  function focusTab(index: number) {
    const keys = [null, ...workspace.tabs.map((t) => t.key)];
    const i = Math.max(0, Math.min(index, keys.length - 1));
    workspace.active = keys[i] ?? null;
    requestAnimationFrame(() =>
      document.querySelector<HTMLElement>(`.tab[data-key="${keys[i] ?? "home"}"]`)?.focus(),
    );
  }

  function onkeydown(e: KeyboardEvent) {
    const keys = [null, ...workspace.tabs.map((t) => t.key)];
    const at = keys.indexOf(workspace.active);
    if (e.key === "ArrowRight") focusTab(at + 1);
    else if (e.key === "ArrowLeft") focusTab(at - 1);
    else if (e.key === "Home") focusTab(0);
    else if (e.key === "End") focusTab(keys.length - 1);
    else if ((e.key === "Delete" || e.key === "Backspace") && workspace.active !== null) {
      void workspace.close();
      focusTab(at - 1);
    } else return;
    e.preventDefault();
  }
</script>

<div class="bar" role="tablist" aria-label="Workspace">
  <button
    class="tab home"
    role="tab"
    data-key="home"
    aria-selected={workspace.active === null}
    tabindex={workspace.active === null ? 0 : -1}
    title={keymap.tip("Hosts", "tab.hosts")}
    onclick={() => (workspace.active = null)}
    {onkeydown}
  >
    <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2.5 6.5 8 2.5l5.5 4" /><path d="M4 6.5v7h8v-7" /></svg>
    <span>Hosts</span>
    <span class="count">{hostCount}</span>
  </button>

  <div
    class="strip"
    class:fade-start={fadeStart}
    class:fade-end={fadeEnd}
    role="presentation"
    bind:this={strip}
    onscroll={updateFades}
    data-tauri-drag-region
  >
    {#each workspace.tabs as tab, i (tab.key)}
      {@const focus = workspace.focusOf(tab)}
      {@const alias = focus?.alias ?? ""}
      {@const title = focus?.title ?? ""}
      {@const count = panesOf(tab.root).length}
      {@const dot = dotOf(focus)}
      {@const selected = workspace.active === tab.key}
      <!-- Any pane of the tab recording: REC on the tab (the header says which). -->
      {@const rec = workspace.panesIn(tab).some((p) => recordings.isRecording(p.terminal))}
      <div
        class="tab"
        role="tab"
        tabindex={selected ? 0 : -1}
        data-key={tab.key}
        aria-selected={selected}
        title={count > 1 ? `${alias} · ${count} panes` : title ? `${alias} — ${title}` : alias}
        animate:flip={{ duration: dur("base") }}
        in:fly={{ y: -4, duration: dur("base") }}
        onclick={() => (workspace.active = tab.key)}
        {onkeydown}
        onauxclick={(e) => e.button === 1 && workspace.close(tab.key)}
      >
        <span class="dot" data-status={dot.status} class:busy={dot.busy} class:hollow={dot.hollow}></span>
        <span class="alias">{alias}</span>
        {#if rec}<RecBadge compact title="A pane of this tab is being recorded" />{/if}
        {#if title && selected && count < 2}<span class="remote">{title}</span>{/if}
        {#if count > 1}
          <span class="panes" aria-label={`${count} panes`}>
            <svg viewBox="0 0 16 16" aria-hidden="true"><rect x="2.5" y="3" width="11" height="10" rx="1.5" /><path d="M8 3v10" /></svg>
            {count}
          </span>
        {/if}
        <span class="slot">
          {#if i < 9}<span class="index">{i + 1}</span>{/if}
          <button
            class="close"
            aria-label={`Close ${alias}`}
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
</div>

<style>
  .bar {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
  }
  .strip {
    display: flex;
    align-items: center;
    gap: var(--space-1);
    min-width: 0;
    /* Room for the focus ring inside the scroll container. */
    padding-block: var(--space-1);
    overflow-x: auto;
    scrollbar-width: none;
  }
  .strip::-webkit-scrollbar { display: none; }
  /* Hidden tabs on either side are hinted by a fade. */
  .strip.fade-start { mask-image: linear-gradient(90deg, transparent 0, #000 var(--space-5)); }
  .strip.fade-end { mask-image: linear-gradient(90deg, #000 calc(100% - var(--space-5)), transparent); }
  .strip.fade-start.fade-end {
    mask-image: linear-gradient(90deg, transparent 0, #000 var(--space-5), #000 calc(100% - var(--space-5)), transparent);
  }

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
  .tab:focus-visible { outline-offset: -2px; }
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
  .home { flex: 0 0 auto; padding-right: var(--space-3); }
  .home svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
    stroke-linejoin: round;
  }
  .panes {
    display: inline-flex;
    flex: 0 0 auto;
    align-items: center;
    gap: var(--space-1);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
    color: var(--text-muted);
  }
  .panes svg { width: var(--size-icon-sm); height: var(--size-icon-sm); fill: none; stroke: currentColor; stroke-width: 1.5; }
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
    max-width: 140px;
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
  .close svg { width: var(--size-icon-sm); height: var(--size-icon-sm); fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; }
  .tab:hover .close, .tab[aria-selected="true"] .close { opacity: 1; }
  .tab:hover .index, .tab[aria-selected="true"] .index { opacity: 0; }
  .close:hover { background: var(--bg-elevated); color: var(--text-primary); }

  .dot {
    --c: var(--status-idle);
    flex: 0 0 auto;
    width: var(--size-dot);
    height: var(--size-dot);
    border-radius: 50%;
    background: var(--c);
  }
  .dot.hollow { background: transparent; box-shadow: inset 0 0 0 var(--size-dot-ring) var(--c); }
  .dot[data-status="connecting"] { --c: var(--status-connecting); }
  .dot[data-status="authenticating"] { --c: var(--status-authenticating); }
  .dot[data-status="connected"] { --c: var(--status-connected); }
  .dot[data-status="warning"] { --c: var(--status-warning); }
  .dot[data-status="error"] { --c: var(--status-error); }
  .dot.busy { animation: pulse var(--motion-loop) var(--ease-standard) infinite; }
  @keyframes pulse {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--c) 60%, transparent); }
    100% { box-shadow: 0 0 0 5px transparent; }
  }
</style>
