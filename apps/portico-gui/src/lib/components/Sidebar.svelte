<script lang="ts">
  // Host list: search, then either the ranked results (flat) or the
  // sections (Recent, groups, Other) whose headers are rows too: the
  // keyboard moves over both, ↵ / ← / → fold a section.
  import { describe, type SessionView } from "../api";
  import type { Row } from "../hostlist";
  import { keymap } from "../state/keymap.svelte";
  import OriginBadge from "./OriginBadge.svelte";

  let {
    rows,
    total,
    sessions = {},
    query = $bindable(""),
    selected = $bindable(0),
    loading = false,
    searchEl = $bindable(),
    onactivate,
    onforget,
    ontoggle,
    onadd,
  }: {
    rows: Row[];
    /** Hosts in the config (empty-state wording). */
    total: number;
    sessions?: Record<string, SessionView>;
    query?: string;
    selected?: number;
    loading?: boolean;
    searchEl?: HTMLInputElement;
    /** ↵ or double-click: connect a host, fold or unfold a section. */
    onactivate: (row: Row) => void;
    /** "Remove from recent" on a Recent row. */
    onforget: (alias: string) => void;
    ontoggle: (sectionId: string) => void;
    /** "Add host…" (phase 6b). */
    onadd: () => void;
  } = $props();

  let listEl: HTMLUListElement | undefined = $state();

  const flat = $derived(rows.every((r) => r.section.kind === "flat"));
  const flatTitle = $derived(rows[0]?.section.title ?? (query ? "Results" : "Hosts"));

  // Keep the selected row visible when navigating with the keyboard.
  $effect(() => {
    const row = listEl?.children[selected] as HTMLElement | undefined;
    row?.scrollIntoView({ block: "nearest" });
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) {
      selected = Math.min(selected + 1, rows.length - 1);
      e.preventDefault();
    } else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
      selected = Math.max(selected - 1, 0);
      e.preventDefault();
    } else if (e.key === "Enter") {
      const row = rows[selected];
      if (row) onactivate(row);
    } else if (e.key === "Escape") {
      if (query) query = "";
      else searchEl?.blur();
    }
  }

  const forgetTip = $derived(keymap.tip("Remove from recent", "hosts.forget-recent"));
</script>

<aside class="sidebar">
  <div class="search-row">
    <label class="search">
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></svg>
      <input
        bind:this={searchEl}
        bind:value={query}
        {onkeydown}
        placeholder="Search · #tag @group *"
        title="Fuzzy search · #tag, @group, * favorites"
        spellcheck="false"
        autocomplete="off"
        aria-label="Search hosts"
      />
      {#if keymap.label("hosts.search")}<kbd>{keymap.label("hosts.search")}</kbd>{/if}
    </label>
    <button class="add" onclick={onadd} title={keymap.tip("Add host…", "host.add")} aria-label="Add host">
      <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 3.5v9M3.5 8h9" /></svg>
    </button>
  </div>

  {#if flat || loading}
    <div class="section-title static">{flatTitle} <span class="count">{loading ? "" : rows.length}</span></div>
  {/if}

  {#if loading}
    <ul class="list" aria-busy="true">
      {#each [0, 1, 2, 3, 4] as i (i)}
        <li class="skeleton" style:--i={i}></li>
      {/each}
    </ul>
  {:else if rows.length === 0}
    <div class="empty">
      {#if query}
        <p>No host matches this search.</p>
        <p class="muted"><kbd>Esc</kbd> clears it · <span class="mono">#tag</span>, <span class="mono">@group</span> and <span class="mono">*</span> narrow it</p>
      {:else if total === 0}
        <p>No hosts yet.</p>
        <p class="muted">
          Add one here{#if keymap.label("host.add")} with <kbd>{keymap.label("host.add")}</kbd>{/if}, or add a <span class="mono">Host</span> block to
          <span class="mono">~/.ssh/config</span> and reload{#if keymap.label("app.reload")} with <kbd>{keymap.label("app.reload")}</kbd>{/if}.
        </p>
      {/if}
    </div>
  {:else}
    <ul class="list" role="listbox" aria-label="Hosts" bind:this={listEl}>
      {#each rows as row, i (row.key)}
        {#if row.kind === "header"}
          <li
            class="header"
            class:selected={i === selected}
            role="option"
            aria-selected={i === selected}
            aria-label={`${row.section.title}, ${row.section.count} ${row.section.count === 1 ? "host" : "hosts"}, ${row.section.collapsed ? "folded" : "unfolded"}`}
            onclick={() => {
              selected = i;
              ontoggle(row.section.id);
            }}
            onkeydown={() => {}}
          >
            <svg class="chevron" class:collapsed={row.section.collapsed} viewBox="0 0 16 16" aria-hidden="true"><path d="m4.5 6 3.5 3.5L11.5 6" /></svg>
            <span class="section-name">{row.section.title}</span>
            <span class="count">{row.section.count}</span>
          </li>
        {:else}
          {@const host = row.host}
          {@const st = describe(sessions[host.alias])}
          <li
            role="option"
            aria-selected={i === selected}
            class:selected={i === selected}
            class:recent={row.section.kind === "recent"}
            onclick={() => (selected = i)}
            ondblclick={() => onactivate(row)}
            onkeydown={() => {}}
          >
            <span class="dot" data-status={st.status} class:busy={st.busy}></span>
            <span class="name">
              <span class="alias" title={host.alias}>{host.alias}</span>
              {#if host.favorite}<span class="star" aria-label="favorite" title="Favorite">★</span>{/if}
              {#if host.origin !== "ssh_config"}<OriginBadge origin={host.origin} compact />{/if}
            </span>
            <span class="target mono">{host.user ? `${host.user}@` : ""}{host.hostname}</span>
            {#if row.section.kind === "recent"}
              <button
                class="forget"
                tabindex="-1"
                title={forgetTip}
                aria-label={`Remove ${host.alias} from recent`}
                onclick={(e) => {
                  e.stopPropagation();
                  onforget(host.alias);
                }}
              >
                <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m5 5 6 6M11 5l-6 6" /></svg>
              </button>
            {/if}
          </li>
        {/if}
      {/each}
    </ul>
  {/if}
</aside>

<style>
  .sidebar {
    display: flex;
    flex-direction: column;
    gap: var(--space-3);
    padding: var(--space-4) var(--space-3);
    background: var(--bg-surface);
    border-right: 1px solid var(--border-subtle);
    min-height: 0;
  }
  .search {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 34px;
    padding: 0 var(--space-3);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    border: 1px solid var(--border-subtle);
    transition: border-color var(--motion-fast) var(--ease-standard),
      box-shadow var(--motion-fast) var(--ease-standard);
  }
  .search:focus-within {
    border-color: var(--accent);
    box-shadow: 0 0 0 3px color-mix(in srgb, var(--accent) 18%, transparent);
  }
  .search svg {
    width: 14px;
    height: 14px;
    fill: none;
    stroke: var(--text-muted);
    stroke-width: 1.5;
  }
  .search input {
    flex: 1;
    min-width: 0;
    border: 0;
    outline: 0;
    background: transparent;
    color: var(--text-primary);
    font: inherit;
  }
  .search input::placeholder { color: var(--text-muted); }
  .search-row { display: flex; gap: var(--space-2); }
  .search-row .search { flex: 1; min-width: 0; }
  /* "Add host…": a quiet square beside the search, the accent on hover and focus. */
  .add {
    display: grid;
    flex: 0 0 auto;
    place-items: center;
    width: 34px;
    height: 34px;
    padding: 0;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-secondary);
    transition: color var(--motion-fast) var(--ease-standard), border-color var(--motion-fast) var(--ease-standard),
      transform var(--motion-fast) var(--ease-standard);
  }
  .add:hover { color: var(--accent); border-color: var(--border-strong); }
  .add:active { transform: scale(0.95); }
  .add:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .add svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; }

  .section-title {
    display: flex;
    justify-content: space-between;
    padding: 0 var(--space-2);
    color: var(--text-muted);
    font-size: var(--text-xs);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .list {
    list-style: none;
    margin: 0;
    padding: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  li {
    position: relative;
    display: grid;
    /* The alias takes what it needs, up to the whole row (then an
       ellipsis); `user@host` gets the rest, right-aligned. */
    grid-template-columns: 8px minmax(0, max-content) minmax(0, 1fr);
    align-items: center;
    gap: var(--space-2);
    flex: 0 0 34px;
    height: 34px;
    padding: 0 var(--space-3);
    border-radius: var(--radius-md);
    cursor: default;
    transition: background var(--motion-fast) var(--ease-standard);
  }
  li:hover { --row-bg: var(--bg-elevated); background: var(--row-bg); }
  li.selected { --row-bg: var(--bg-selected); background: var(--row-bg); }
  li.selected::before {
    content: "";
    position: absolute;
    left: 0;
    top: 8px;
    bottom: 8px;
    width: 2px;
    border-radius: 2px;
    background: var(--accent);
  }
  /* Section headers: spaced capitals, muted; space above marks the break. */
  li.header {
    grid-template-columns: 12px 1fr auto;
    flex-basis: 28px;
    height: 28px;
    margin-top: var(--space-2);
    padding: 0 var(--space-2);
    color: var(--text-muted);
    font-size: var(--text-xs);
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }
  li.header:first-child { margin-top: 0; }
  li.header:hover { color: var(--text-secondary); background: transparent; }
  li.header.selected { color: var(--text-secondary); background: var(--bg-selected); }
  .section-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .count { font-variant-numeric: tabular-nums; letter-spacing: normal; }
  .chevron {
    width: 12px;
    height: 12px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
    transition: transform var(--motion-fast) var(--ease-standard);
  }
  .chevron.collapsed { transform: rotate(-90deg); }
  .dot {
    --c: var(--status-idle);
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--c);
  }
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
  /* The star stays outside the truncated alias: always visible. */
  .name { display: flex; align-items: center; gap: var(--space-1); min-width: 0; }
  .alias { min-width: 0; overflow: hidden; font-weight: 550; text-overflow: ellipsis; white-space: nowrap; }
  .star { flex: 0 0 auto; color: var(--accent); font-size: var(--text-xs); }
  .target {
    color: var(--text-muted);
    font-size: var(--text-xs);
    text-align: right;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  /* Over the end of the row (no column of its own: `user@host` lines up
     with the other sections); shown on hover and on the selected row. */
  .forget {
    position: absolute;
    top: 50%;
    right: var(--space-2);
    display: grid;
    place-items: center;
    width: 20px;
    height: 20px;
    margin-top: -10px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-sm);
    /* The row's own background, fading over the end of `user@host`. */
    background: var(--row-bg, var(--bg-surface));
    box-shadow: calc(-1 * var(--space-2)) 0 var(--space-2) var(--space-1) var(--row-bg, var(--bg-surface));
    color: var(--text-muted);
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--motion-fast) var(--ease-standard), background var(--motion-fast) var(--ease-standard);
  }
  li:hover .forget,
  li.selected .forget { opacity: 1; pointer-events: auto; }
  .forget:hover { background: var(--bg-overlay); color: var(--text-primary); }
  .forget svg { width: 12px; height: 12px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; }
  .empty { display: grid; gap: var(--space-1); padding: var(--space-2); color: var(--text-secondary); }
  .empty p { margin: 0; }
  .muted { color: var(--text-muted); font-size: var(--text-sm); }

  .skeleton {
    height: 34px;
    background: linear-gradient(90deg, var(--bg-elevated), var(--bg-selected), var(--bg-elevated));
    background-size: 200% 100%;
    animation: shimmer var(--motion-loop) var(--ease-standard) infinite;
    /* Rows start one after the other: a short stagger, from the tokens. */
    animation-delay: calc(var(--i, 0) * var(--motion-fast) * 2 / 3);
  }
  @keyframes shimmer {
    from { background-position: 100% 0; }
    to { background-position: -100% 0; }
  }
</style>
