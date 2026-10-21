<script lang="ts">
  import { onMount } from "svelte";
  import { fly } from "svelte/transition";
  import { describe, formatRtt, isTauri, setHome, type Host, type ThemeInfo } from "./lib/api";
  import { backend } from "./lib/backend";
  import { appShortcut, isMac, kbd, kbdShift, tabKbd } from "./lib/keys";
  import { dur } from "./lib/motion";
  import { workspace } from "./lib/state/workspace.svelte";
  import { files } from "./lib/state/files.svelte";
  import { tunnels } from "./lib/state/tunnels.svelte";
  import FilesPanel from "./lib/components/FilesPanel.svelte";
  import TunnelsDialog from "./lib/components/TunnelsDialog.svelte";
  import Modal from "./lib/components/Modal.svelte";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import HostDetails from "./lib/components/HostDetails.svelte";
  import TabBar from "./lib/components/TabBar.svelte";
  import Mark from "./lib/components/Mark.svelte";
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
  /** Files panel beside the active terminal. */
  let filesOpen = $state(false);
  let tunnelsOpen = $state(false);
  let filesPanel: FilesPanel | undefined = $state();
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
  const conflict = $derived(files.conflicts[0]);
  const modalOpen = $derived(!!prompt || !!alert || !!conflict || tunnelsOpen);
  const showFiles = $derived(filesOpen && !!tab);

  /** Session the tunnels panel adds to: the active tab's, else the selected host's live one. */
  const tunnelSession = $derived.by(() => {
    if (tab) return { id: tab.session, alias: tab.alias };
    const alias = hosts[selected]?.alias;
    const live = alias ? workspace.byAlias[alias] : undefined;
    return live && alias && ["connected", "authenticating", "handshake", "connecting", "resolving", "verifying_host_key"].includes(live.phase.phase)
      ? { id: live.id, alias }
      : null;
  });

  function toggleFiles() {
    if (!tab) {
      // From the host list: open (or reuse) the host's tab, files beside it.
      const host = hosts[selected];
      if (!host) return;
      filesOpen = true;
      void workspace.open(host.alias);
      return;
    }
    filesOpen = !filesOpen;
  }

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
    ...(tab
      ? [
          { id: "new-tab", title: `New tab on ${tab.alias}`, shortcut: kbd("T"), run: () => void workspace.duplicate() },
          { id: "close-tab", title: "Close tab", shortcut: kbd("W"), run: () => void workspace.close() },
          { id: "copy", title: "Copy selection", shortcut: isMac ? "⌘C" : kbd("C"), run: copy },
          { id: "paste", title: "Paste", shortcut: isMac ? "⌘V" : kbd("V"), run: () => void paste() },
          { id: "reconnect", title: `Reconnect ${tab.alias}`, run: () => void workspace.reconnect(tab.key) },
          { id: "disconnect", title: `Disconnect ${tab.alias}`, run: () => void workspace.disconnect(tab.alias) },
        ]
      : [{ id: "connect", title: "Connect to selected host", shortcut: "↵", run: connectSelected }]),
    {
      id: "files",
      title: tab ? (showFiles ? "Hide files" : `Files on ${tab.alias}`) : `Files on ${hosts[selected]?.alias ?? "host"}`,
      shortcut: kbdShift("F"),
      run: toggleFiles,
    },
    ...(showFiles && tab
      ? [{ id: "upload", title: `Upload files to ${tab.alias}`, run: () => void files.pickAndUpload(tab.session) }]
      : []),
    { id: "tunnels", title: "Tunnels", hint: tunnels.list.length ? `${tunnels.list.length} running` : undefined, shortcut: kbdShift("P"), run: () => (tunnelsOpen = true) },
    { id: "download-dir", title: "Change download folder", hint: files.downloadDir || undefined, run: () => void files.chooseDownloadDir() },
    { id: "home", title: "Go to hosts", shortcut: tabKbd(0), run: () => (workspace.active = null) },
    ...workspace.tabs.map((t, i) => ({
      id: `tab-${t.key}`,
      title: `Go to ${t.alias}`,
      hint: t.title || undefined,
      shortcut: i < 9 ? tabKbd(i + 1) : undefined,
      run: () => (workspace.active = t.key),
    })),
    ...(workspace.tabs.length
      ? [
          { id: "next-tab", title: "Next tab", shortcut: "Ctrl-Tab", run: () => workspace.cycle(1) },
          { id: "prev-tab", title: "Previous tab", shortcut: "Ctrl-Shift-Tab", run: () => workspace.cycle(-1) },
        ]
      : []),
    { id: "reload", title: "Reload ~/.ssh/config", shortcut: isMac ? "⌘R" : "Ctrl-R", run: load },
    { id: "theme", title: "Next theme", shortcut: "t", hint: theme?.name, run: cycleTheme },
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
        case "files": toggleFiles(); break;
        case "tunnels": tunnelsOpen = true; break;
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
    else if (e.key === "f") toggleFiles();
    else if (e.key === "p") tunnelsOpen = true;
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

  // Give the keyboard back when an overlay closes: to the files panel when
  // it is open (and just opened), else to the terminal.
  $effect(() => {
    if (!paletteOpen && !modalOpen && tab) {
      const view = views[tab.key];
      const panel = showFiles ? filesPanel : undefined;
      queueMicrotask(() => (panel ? panel.focus() : view?.focus()));
    }
  });

  function closeFiles() {
    filesOpen = false;
  }

  onMount(() => {
    load();
    void files.init();
    const unlisten = [
      backend.onEngineEvent((ev) => {
        workspace.onEvent(ev);
        if (ev.type === "sftp") files.onEvent(ev.session, ev.event);
        else if (ev.type === "tunnel") tunnels.onEvent(ev.id, ev.session, ev.event);
        else if (ev.type === "session" && ev.event.kind === "phase" && ["closed", "failed"].includes(ev.event.phase)) {
          files.forget(ev.id);
        }
      }),
      backend.onDragging((d) => (files.dragging = d)),
      backend.onDropped((list) => files.dropped(list, showFiles && tab ? tab.session : null)),
    ];
    return () => {
      for (const u of unlisten) void u.then((f) => f());
    };
  });
</script>

<svelte:window {onkeydown} />

<div class="shell">
  <header class="titlebar" class:mac={isMac} data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <Mark size={16} />
      <span class="wordmark">PORTICO</span>
    </div>
    <TabBar hostCount={allHosts.length} />
    <button class="palette-trigger" onclick={() => (paletteOpen = true)} aria-label="Command palette">
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></svg>
      <kbd>{kbd("K")}</kbd>
    </button>
  </header>

  <main class:with-files={showFiles}>
    <div class="hosts" class:hidden={!!tab}>
      <Sidebar {hosts} sessions={workspace.byAlias} {loading} bind:query bind:selected bind:searchEl onconnect={(h) => workspace.open(h.alias)} />
      <HostDetails
        host={hosts[selected]}
        session={workspace.byAlias[hosts[selected]?.alias ?? ""]}
        onconnect={(h) => workspace.open(h.alias)}
        ondisconnect={(h) => workspace.disconnect(h.alias)}
      />
    </div>
    <div class="stage">
      {#each workspace.tabs as t (t.key)}
        <TerminalView bind:this={views[t.key]} tab={t} active={workspace.active === t.key} themeId={theme?.id ?? ""} autofocus={!showFiles} />
      {/each}
    </div>
    {#if showFiles && tab}
      {#key tab.session}
        <FilesPanel bind:this={filesPanel} session={tab.session} alias={tab.alias} onclose={closeFiles} />
      {/key}
    {/if}
  </main>

  <footer class="statusbar">
    {#if tab}
      <span class="session" data-status={tabStatus.status}>
        <span class="dot" class:busy={tabStatus.busy} class:hollow={!!tab.end}></span>
        <span class="alias">{tab.alias}</span>
        {#if tab.end}<span class="ended">{tab.end.kind === "closed" ? "Shell closed" : "Shell exited"}</span>{/if}
        <span>{tab.end ? `session ${tabStatus.label.toLowerCase()}` : tabStatus.label}</span>
        {#if tabSession?.latencyMs != null && tabSession.phase.phase === "connected"}
          <span class="rtt mono" class:slow={tabStatus.status === "warning"}>{formatRtt(tabSession.latencyMs)}</span>
        {/if}
        {#if tab.title}<span class="title">{tab.title}</span>{/if}
      </span>
      <span class="spacer"></span>
      <span class="hints">
        <span class="optional"><kbd>{kbd("T")}</kbd> new tab</span>
        <span class="optional"><kbd>{kbd("W")}</kbd> close</span>
        <span class="optional"><kbd>{kbdShift("F")}</kbd> files</span>
        <span class="optional"><kbd>{tabKbd(0)}</kbd> hosts</span>
        <span><kbd>{kbd("K")}</kbd> commands</span>
      </span>
    {:else}
      <span>{allHosts.length} hosts · {workspace.tabs.length} open</span>
      <span class="spacer"></span>
      <span class="hints">
        <span><kbd>/</kbd> search</span>
        <span><kbd>↵</kbd> connect</span>
        <span class="optional"><kbd>d</kbd> disconnect</span>
        <span class="optional"><kbd>f</kbd> files</span>
        <span class="optional"><kbd>p</kbd> tunnels</span>
        <span class="optional"><kbd>t</kbd> theme</span>
        <span><kbd>{kbd("K")}</kbd> commands</span>
      </span>
    {/if}
    <span class="theme-name">
      {#if !isTauri()}<span class="demo" title="Browser preview with a simulated engine">Demo</span>{/if}
      {theme?.name ?? ""}
    </span>
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
{:else if conflict}
  {#key conflict}
    <Modal onescape={() => files.answerConflict(null)} labelledBy="conflict-title">
      {#snippet title()}<span class="mono">{conflict.name}</span> already exists{/snippet}
      <p>
        {conflict.retry.kind === "download" ? "Your download folder" : "The remote folder"} already has a file with this name.
        Keep both to save this one as a numbered copy.
      </p>
      {#snippet footer()}
        <button class="secondary" onclick={() => files.answerConflict("skip")}>Skip</button>
        <button class="secondary" onclick={() => files.answerConflict("overwrite")}>Replace</button>
        <button class="primary" data-autofocus onclick={() => files.answerConflict("rename")}>Keep both</button>
      {/snippet}
    </Modal>
  {/key}
{:else if tunnelsOpen}
  <TunnelsDialog
    session={tunnelSession?.id ?? null}
    alias={tunnelSession?.alias}
    host={allHosts.find((h) => h.alias === tunnelSession?.alias)}
    onclose={() => (tunnelsOpen = false)}
  />
{/if}

{#if workspace.toast}
  {@const toast = workspace.toast}
  <div class="toast" data-tone={toast.tone} role={toast.tone === "error" ? "alert" : "status"} transition:fly={{ y: 12, duration: dur("base") }}>
    <svg viewBox="0 0 16 16" aria-hidden="true">
      {#if toast.tone === "error"}<circle cx="8" cy="8" r="6.25" /><path d="M8 4.75v3.75M8 11.1v.1" />
      {:else}<circle cx="8" cy="8" r="6.25" /><path d="M8 7.25v4M8 4.9v.1" />{/if}
    </svg>
    <div class="toast-text">
      <strong>{toast.message}</strong>
      {#if toast.hint}<span>{toast.hint}</span>{/if}
    </div>
  </div>
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
    font-weight: 500;
    letter-spacing: 0.28em;
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
  main {
    position: relative;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    min-height: 0;
  }
  main.with-files { grid-template-columns: minmax(0, 1fr) min(420px, 42%); }
  main > .hosts { position: absolute; inset: 0; }
  .stage { position: relative; min-width: 0; min-height: 0; }
  .stage > :global(*) { position: absolute; inset: 0; }
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
  .session .dot.hollow { background: transparent; box-shadow: inset 0 0 0 1.5px var(--c); }
  .session .dot.busy { animation: pulse var(--motion-loop) var(--ease-standard) infinite; }
  .session .ended { color: var(--text-primary); }
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
    --c: var(--status-error);
    position: fixed;
    right: var(--space-5);
    bottom: calc(28px + var(--space-4));
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    max-width: 420px;
    padding: var(--space-3) var(--space-4);
    background: var(--bg-overlay);
    border: 1px solid color-mix(in srgb, var(--c) 45%, transparent);
    border-left: 3px solid var(--c);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-raised);
    z-index: 30;
  }
  .toast[data-tone="info"] { --c: var(--border-strong); }
  .toast svg {
    flex: 0 0 auto;
    width: 14px;
    height: 14px;
    margin-top: 2px;
    fill: none;
    stroke: var(--c);
    stroke-width: 1.5;
    stroke-linecap: round;
  }
  .toast[data-tone="info"] svg { stroke: var(--text-secondary); }
  .toast-text { display: grid; gap: var(--space-1); }
  .toast strong { font-weight: 550; }
  .toast span { color: var(--text-secondary); font-size: var(--text-sm); }
  .demo {
    margin-right: var(--space-2);
    padding: 0 var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
</style>
