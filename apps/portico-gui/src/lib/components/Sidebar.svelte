<script lang="ts">
  import { describe, type Host, type SessionView } from "../api";

  let {
    hosts,
    sessions = {},
    query = $bindable(""),
    selected = $bindable(0),
    loading = false,
    searchEl = $bindable(),
    onconnect,
  }: {
    hosts: Host[];
    sessions?: Record<string, SessionView>;
    query?: string;
    selected?: number;
    loading?: boolean;
    searchEl?: HTMLInputElement;
    onconnect: (host: Host) => void;
  } = $props();

  let listEl: HTMLUListElement | undefined = $state();

  // Keep the selected row visible when navigating with the keyboard.
  $effect(() => {
    const row = listEl?.children[selected] as HTMLElement | undefined;
    row?.scrollIntoView({ block: "nearest" });
  });

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) {
      selected = Math.min(selected + 1, hosts.length - 1);
      e.preventDefault();
    } else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) {
      selected = Math.max(selected - 1, 0);
      e.preventDefault();
    } else if (e.key === "Enter") {
      const host = hosts[selected];
      if (host) onconnect(host);
    } else if (e.key === "Escape") {
      if (query) query = "";
      else searchEl?.blur();
    }
  }
</script>

<aside class="sidebar">
  <label class="search">
    <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></svg>
    <input
      bind:this={searchEl}
      bind:value={query}
      {onkeydown}
      placeholder="Search hosts, #tags…"
      spellcheck="false"
      autocomplete="off"
      aria-label="Search hosts"
    />
    <kbd>⌘K</kbd>
  </label>

  <div class="section-title">Hosts <span class="count">{hosts.length}</span></div>

  {#if loading}
    <ul class="list" aria-busy="true">
      {#each [0, 1, 2, 3, 4] as i (i)}
        <li class="skeleton" style:animation-delay="{i * 80}ms"></li>
      {/each}
    </ul>
  {:else if hosts.length === 0}
    <p class="empty">{query ? "No host matches this search." : "No hosts in ~/.ssh/config yet."}</p>
  {:else}
    <ul class="list" role="listbox" aria-label="Hosts" bind:this={listEl}>
      {#each hosts as host, i (host.alias)}
        {@const st = describe(sessions[host.alias])}
        <li
          role="option"
          aria-selected={i === selected}
          class:selected={i === selected}
          onclick={() => (selected = i)}
          ondblclick={() => onconnect(host)}
          onkeydown={() => {}}
        >
          <span class="dot" data-status={st.status} class:busy={st.busy}></span>
          <span class="alias">{host.alias}{#if host.favorite}<span class="star" aria-label="favorite"> ★</span>{/if}</span>
          <span class="target mono">{host.user ? `${host.user}@` : ""}{host.hostname}</span>
        </li>
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
    grid-template-columns: 8px auto 1fr;
    align-items: center;
    gap: var(--space-2);
    height: 34px;
    padding: 0 var(--space-3);
    border-radius: var(--radius-md);
    cursor: default;
    transition: background var(--motion-fast) var(--ease-standard);
  }
  li:hover { background: var(--bg-elevated); }
  li.selected { background: var(--bg-selected); }
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
  .dot.busy { animation: blink 1s var(--ease-standard) infinite alternate; }
  @keyframes blink { to { opacity: 0.35; } }
  .alias { font-weight: 550; }
  .star { color: var(--accent); font-size: var(--text-xs); }
  .target {
    color: var(--text-muted);
    font-size: var(--text-xs);
    text-align: right;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .empty { color: var(--text-muted); padding: var(--space-2); }

  .skeleton {
    height: 34px;
    background: linear-gradient(90deg, var(--bg-elevated), var(--bg-selected), var(--bg-elevated));
    background-size: 200% 100%;
    animation: shimmer 1.4s var(--ease-standard) infinite;
  }
  @keyframes shimmer {
    from { background-position: 100% 0; }
    to { background-position: -100% 0; }
  }
</style>
