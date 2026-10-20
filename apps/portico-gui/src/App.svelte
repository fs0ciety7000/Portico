<script lang="ts">
  import { onMount } from "svelte";
  import { fly } from "svelte/transition";
  import { describe, formatRtt, isTauri, setHome, type Host, type ThemeInfo } from "./lib/api";
  import { backend } from "./lib/backend";
  import { appShortcut, isMac, kbd } from "./lib/keys";
  import { workspace } from "./lib/state/workspace.svelte";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import HostDetails from "./lib/components/HostDetails.svelte";
  import TabBar from "./lib/components/TabBar.svelte";
  import TerminalView from "./lib/components/TerminalView.svelte";
  import PromptDialog from "./lib/components/PromptDialog.svelte";
  import HostKeyAlert from "./lib/components/HostKeyAlert.svelte";
  import CommandPalette, { type Command } from "./lib/components/CommandPalette.svelte";

  let allHosts: Host[] = $state([]);
  let hosts: Host[] = $state([]);
  let themes: ThemeInfo[] = $state([]);
  let themeIndex = $state(0);
  let query = $state("");
  let selected = $state(0);
  let loading = $state(true);
  let paletteOpen = $state(false);
  let searchEl: HTMLInputElement | undefined = $state();
  /** Terminal components by tab key (copy, paste, focus). */
  const views: Record<number, TerminalView> = $state({});

  const theme = $derived(themes[themeIndex]);
  $effect(() => {
    if (theme) document.documentElement.dataset.theme = theme.id;
  });

  const tab = $derived(workspace.activeTab);
  const tabSession = $derived(tab ? workspace.sessions[tab.session] : undefined);
  const tabStatus = $derived(describe(tabSession));
  const prompt = $derived(workspace.prompts[0]);
  const alert = $derived(workspace.alerts[0]);
  const modalOpen = $derived(!!prompt || !!alert);

  // Ranking lives in portico-core (same results as the TUI).
  $effect(() => {
    const q = query;
    backend.searchHosts(q).then((r) => {
      if (q === query) {
        hosts = r;
        selected = 0;
      }
    }, (e) => workspace.notify(e));
  });

  async function load() {
    loading = true;
    try {
      const [h, t, home] = await Promise.all([backend.listHosts(), backend.listThemes(), backend.homeDir()]);
      setHome(home);
      [allHosts, themes] = [h, t];
      hosts = allHosts;
    } catch (e) {
      workspace.notify(e);
    } finally {
      loading = false;
    }
  }

  function connectSelected() {
    const host = hosts[selected];
    if (host) void workspace.open(host.alias);
  }

  const cycleTheme = () => (themeIndex = (themeIndex + 1) % Math.max(themes.length, 1));

  async function paste() {
    const view = tab ? views[tab.key] : undefined;
    if (!view) return;
    try {
      view.pasteText(await navigator.clipboard.readText());
    } catch {
      workspace.notify({
        code: "clipboard",
        message: "The clipboard is not available here",
        hint: "Paste with Shift-Insert or the middle mouse button.",
      });
    }
  }

  function copy() {
    const text = tab ? views[tab.key]?.copySelection() : "";
    if (text) navigator.clipboard.writeText(text).catch((e) => workspace.notify(e));
  }

  const commands: Command[] = $derived([
    { id: "search", title: "Search hosts", shortcut: "/", run: () => ((workspace.active = null), queueMicrotask(() => searchEl?.focus())) },
    { id: "connect", title: "Connect to selected host", shortcut: "↵", run: connectSelected },
    ...(tab
      ? [
          { id: "new-tab", title: `New tab on ${tab.alias}`, shortcut: kbd("T"), run: () => void workspace.duplicate() },
          { id: "close-tab", title: "Close tab", shortcut: kbd("W"), run: () => void workspace.close() },
          { id: "reconnect", title: `Reconnect ${tab.alias}`, run: () => void workspace.reconnect(tab.key) },
          { id: "disconnect", title: `Disconnect ${tab.alias}`, run: () => void workspace.disconnect(tab.alias) },
        ]
      : []),
    { id: "home", title: "Go to hosts", shortcut: kbd("0"), run: () => (workspace.active = null) },
    ...workspace.tabs.map((t, i) => ({
      id: `tab-${t.key}`,
      title: `Go to ${t.alias}`,
      hint: t.title || undefined,
      shortcut: i < 9 ? kbd(String(i + 1)) : undefined,
      run: () => (workspace.active = t.key),
    })),
    { id: "reload", title: "Reload ~/.ssh/config", shortcut: kbd("R"), run: load },
    ...themes.map((t, i) => ({ id: `theme-${t.id}`, title: `Theme: ${t.name}`, hint: t.description, run: () => (themeIndex = i) })),
  ]);

  function onkeydown(e: KeyboardEvent) {
    if (modalOpen) return;
    const inTerminal = !!tab;
    const sc = appShortcut(e, inTerminal) ?? (!isMac && !inTerminal && e.ctrlKey && e.key === "k" ? { action: "palette" as const } : null);
    if (sc) {
      e.preventDefault();
      if (paletteOpen && sc.action !== "palette") return;
      switch (sc.action) {
        case "palette": paletteOpen = !paletteOpen; break;
        case "new-tab": inTerminal ? void workspace.duplicate() : connectSelected(); break;
        case "close-tab": void workspace.close(); break;
        case "go-tab": workspace.go(sc.index); break;
        case "go-home": workspace.active = null; break;
        case "next-tab": workspace.cycle(1); break;
        case "prev-tab": workspace.cycle(-1); break;
        case "copy": copy(); break;
        case "paste": void paste(); break;
        case "reload": void load(); break;
      }
      return;
    }
    if (paletteOpen || inTerminal || document.activeElement === searchEl) return;
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (e.key === "/") {
      searchEl?.focus();
      e.preventDefault();
    } else if (e.key === "j" || e.key === "ArrowDown") selected = Math.min(selected + 1, hosts.length - 1);
    else if (e.key === "k" || e.key === "ArrowUp") selected = Math.max(selected - 1, 0);
    else if (e.key === "Enter") connectSelected();
    else if (e.key === "t") cycleTheme();
    else if (e.key === "d") {
      const host = hosts[selected];
      if (host) void workspace.disconnect(host.alias);
    }
  }

  // Leaving a terminal: take the focus out of its (now hidden) textarea.
  $effect(() => {
    if (tab) return;
    const el = document.activeElement;
    if (el instanceof HTMLElement && el.closest(".terminal")) el.blur();
  });

  // Give the keyboard back to the terminal when an overlay closes.
  $effect(() => {
    if (!paletteOpen && !modalOpen && tab) {
      const view = views[tab.key];
      queueMicrotask(() => view?.focus());
    }
  });

  onMount(() => {
    load();
    const unlisten = backend.onEngineEvent((ev) => workspace.onEvent(ev));
    return () => {
      unlisten.then((f) => f());
    };
  });
</script>

<svelte:window {onkeydown} />

<div class="shell">
  <header class="titlebar" class:mac={isMac} data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <img src="/portico-mark.svg" alt="" width="18" height="18" />
      <span class="wordmark">PORTICO</span>
    </div>
    <TabBar hostCount={allHosts.length} />
    <button class="palette-trigger" onclick={() => (paletteOpen = true)} aria-label="Command palette">
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></svg>
      <kbd>{kbd("K")}</kbd>
    </button>
  </header>

  <main>
    <div class="hosts" class:hidden={!!tab}>
      <Sidebar {hosts} sessions={workspace.byAlias} {loading} bind:query bind:selected bind:searchEl onconnect={(h) => workspace.open(h.alias)} />
      <HostDetails
        host={hosts[selected]}
        session={workspace.byAlias[hosts[selected]?.alias ?? ""]}
        onconnect={(h) => workspace.open(h.alias)}
        ondisconnect={(h) => workspace.disconnect(h.alias)}
      />
    </div>
    {#each workspace.tabs as t (t.key)}
      <TerminalView bind:this={views[t.key]} tab={t} active={workspace.active === t.key} themeId={theme?.id ?? ""} />
    {/each}
  </main>

  <footer class="statusbar">
    {#if tab}
      <span class="session" data-status={tabStatus.status}>
        <span class="dot" class:busy={tabStatus.busy}></span>
        <span class="alias">{tab.alias}</span>
        <span>{tabStatus.label}</span>
        {#if tabSession?.latencyMs != null && tabSession.phase.phase === "connected"}
          <span class="rtt mono" class:slow={tabStatus.status === "warning"}>{formatRtt(tabSession.latencyMs)}</span>
        {/if}
        {#if tab.title}<span class="title">{tab.title}</span>{/if}
      </span>
      <span class="spacer"></span>
      <span class="hints">
        <span class="optional"><kbd>{kbd("T")}</kbd> new tab</span>
        <span class="optional"><kbd>{kbd("W")}</kbd> close</span>
        <span class="optional"><kbd>{kbd("0")}</kbd> hosts</span>
        <span><kbd>{kbd("K")}</kbd> commands</span>
      </span>
    {:else}
      <span>{allHosts.length} hosts · {workspace.tabs.length} open</span>
      <span class="spacer"></span>
      <span class="hints">
        <span><kbd>/</kbd> search</span>
        <span><kbd>↵</kbd> connect</span>
        <span class="optional"><kbd>d</kbd> disconnect</span>
        <span class="optional"><kbd>t</kbd> theme</span>
        <span><kbd>{kbd("K")}</kbd> commands</span>
      </span>
    {/if}
    <span class="theme-name">{theme?.name ?? ""}</span>
  </footer>
</div>

<CommandPalette bind:open={paletteOpen} {commands} />

{#if alert}
  {#key alert}
    <HostKeyAlert {alert} onclose={() => workspace.dismissAlert()} />
  {/key}
{:else if prompt}
  {#key prompt.id}
    <PromptDialog {prompt} alias={workspace.aliasOf(prompt.session)} onanswer={(a) => workspace.answer(prompt, a)} />
  {/key}
{/if}

{#if workspace.toast}
  <div class="toast" role="alert" transition:fly={{ y: 12, duration: 180 }}>
    <strong>{workspace.toast.message}</strong>
    {#if workspace.toast.hint}<span>{workspace.toast.hint}</span>{/if}
  </div>
{/if}

{#if !isTauri()}
  <span class="demo-badge" title="Browser preview with a simulated engine">Demo</span>
{/if}

<style>
  .shell {
    display: grid;
    grid-template-rows: 44px 1fr 28px;
    height: 100%;
  }
  .titlebar {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-4);
    padding: 0 var(--space-3) 0 var(--space-4);
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-base);
  }
  .titlebar.mac { padding-left: 84px; /* room for the traffic lights */ }
  .brand { display: flex; align-items: center; gap: var(--space-2); padding-right: var(--space-2); }
  .wordmark {
    font-family: var(--font-display);
    font-size: var(--text-md);
    letter-spacing: 0.32em;
    color: var(--text-primary);
  }
  .palette-trigger {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 28px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-surface);
    color: var(--text-muted);
    font: inherit;
    transition: border-color var(--motion-fast) var(--ease-standard);
  }
  .palette-trigger:hover { border-color: var(--border-strong); }
  .palette-trigger svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; }
  main { position: relative; min-height: 0; }
  main > :global(*) { position: absolute; inset: 0; }
  .hosts {
    display: grid;
    grid-template-columns: minmax(280px, 340px) 1fr;
    min-height: 0;
  }
  .hosts.hidden { display: none; }
  .statusbar {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    min-width: 0;
    padding: 0 var(--space-4);
    border-top: 1px solid var(--border-subtle);
    background: var(--bg-base);
    color: var(--text-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .spacer { flex: 1; }
  .hints { display: flex; gap: var(--space-4); flex-shrink: 0; }
  /* Narrow windows keep the session info and the palette hint. */
  @media (max-width: 1100px) {
    .hints .optional { display: none; }
  }
  .theme-name { padding-left: var(--space-3); border-left: 1px solid var(--border-subtle); }
  .session {
    --c: var(--status-idle);
    display: flex;
    flex: 0 1 auto;
    overflow: hidden;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    color: var(--text-secondary);
  }
  .session[data-status="connecting"] { --c: var(--status-connecting); }
  .session[data-status="authenticating"] { --c: var(--status-authenticating); }
  .session[data-status="connected"] { --c: var(--status-connected); }
  .session[data-status="warning"] { --c: var(--status-warning); }
  .session[data-status="error"] { --c: var(--status-error); }
  .session .dot { flex: 0 0 auto; width: 6px; height: 6px; border-radius: 50%; background: var(--c); }
  .session .dot.busy { animation: pulse 1.2s var(--ease-standard) infinite; }
  .session .alias { color: var(--text-primary); font-weight: 550; }
  .rtt { color: var(--text-muted); }
  .rtt.slow { color: var(--status-warning); }
  .session .title { color: var(--text-muted); overflow: hidden; text-overflow: ellipsis; }
  .session > * + *:not(.alias)::before { content: "·"; margin-right: var(--space-2); color: var(--text-muted); }
  @keyframes pulse {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--c) 60%, transparent); }
    100% { box-shadow: 0 0 0 4px transparent; }
  }
  .toast {
    position: fixed;
    right: var(--space-5);
    bottom: calc(28px + var(--space-4));
    display: grid;
    gap: var(--space-1);
    max-width: 400px;
    padding: var(--space-3) var(--space-4);
    background: var(--bg-overlay);
    border: 1px solid color-mix(in srgb, var(--status-error) 45%, transparent);
    border-left: 3px solid var(--status-error);
    border-radius: var(--radius-md);
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.4);
    z-index: 30;
  }
  .toast strong { font-weight: 550; }
  .toast span { color: var(--text-secondary); font-size: var(--text-sm); }
  .demo-badge {
    position: fixed;
    left: var(--space-3);
    bottom: calc(28px + var(--space-3));
    padding: 2px var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    color: var(--text-muted);
    font-size: var(--text-xs);
    letter-spacing: 0.08em;
    text-transform: uppercase;
    pointer-events: none;
  }
</style>
