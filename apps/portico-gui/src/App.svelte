<script lang="ts">
  import { onMount } from "svelte";
  import { fly } from "svelte/transition";
  import {
    api,
    asError,
    isTauri,
    type EngineEvent,
    type Host,
    type SessionView,
    type ThemeInfo,
    type ErrorSummary,
  } from "./lib/api";
  import { demoHosts, demoThemes } from "./lib/demo";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import HostDetails from "./lib/components/HostDetails.svelte";
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
  let toast: ErrorSummary | null = $state(null);
  /** Latest session per host alias. */
  let sessions: Record<string, SessionView> = $state({});
  /** Sessions whose prompt was declined (dialogs arrive in phase 1c). */
  const declined = new Set<number>();
  let toastTimer: ReturnType<typeof setTimeout> | undefined;

  const theme = $derived(themes[themeIndex]);
  $effect(() => {
    if (theme) document.documentElement.dataset.theme = theme.id;
  });

  // Ranking lives in portico-core (same results as the TUI).
  $effect(() => {
    const q = query;
    if (!isTauri()) {
      hosts = allHosts.filter((h) => `${h.alias} ${h.hostname} ${h.tags.map((t) => "#" + t).join(" ")}`.includes(q));
      selected = 0;
      return;
    }
    api.searchHosts(q).then((r) => {
      if (q === query) {
        hosts = r;
        selected = 0;
      }
    }, showError);
  });

  async function load() {
    loading = true;
    try {
      [allHosts, themes] = isTauri() ? await Promise.all([api.listHosts(), api.listThemes()]) : [demoHosts, demoThemes];
      hosts = allHosts;
    } catch (e) {
      showError(e);
    } finally {
      loading = false;
    }
  }

  function showError(e: unknown) {
    toast = asError(e);
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => (toast = null), 4000);
  }

  function connectSelected() {
    const host = hosts[selected];
    if (host) connect(host);
  }

  async function connect(host: Host) {
    const current = sessions[host.alias];
    if (current && current.phase.phase !== "failed" && current.phase.phase !== "closed") return;
    try {
      const id = await api.connect(host.alias);
      sessions[host.alias] = { id, phase: { phase: "resolving" }, latencyMs: null };
    } catch (e) {
      showError(e);
    }
  }

  async function disconnect(host: Host) {
    const s = sessions[host.alias];
    if (s) await api.disconnect(s.id).catch(showError);
  }

  function aliasOf(id: number): string | undefined {
    return Object.keys(sessions).find((a) => sessions[a]?.id === id);
  }

  function onEngineEvent(ev: EngineEvent) {
    if (ev.type === "prompt" && "id" in ev && "session" in ev) {
      // Prompt dialogs land in phase 1c: decline so the session fails precisely.
      if (ev.session !== null) declined.add(ev.session);
      api.respondPrompt(ev.id, { kind: "cancel" }).catch(() => {});
      return;
    }
    if (ev.type !== "session" || !("event" in ev)) return;
    const alias = aliasOf(ev.id);
    const view = alias ? sessions[alias] : undefined;
    if (!view) return;
    const e = ev.event;
    if (e.kind === "phase") {
      const { kind: _kind, ...phase } = e;
      view.phase = phase;
      if (phase.phase === "failed") {
        toast = declined.delete(ev.id)
          ? { code: "prompt", message: `${alias} needs an answer (host key or password)`, hint: "Prompt dialogs arrive in the next phase." }
          : phase.error;
        clearTimeout(toastTimer);
        toastTimer = setTimeout(() => (toast = null), 5000);
      }
    } else if (e.kind === "latency") {
      view.latencyMs = e.rtt.secs * 1000 + e.rtt.nanos / 1e6;
    }
  }

  const cycleTheme = () => (themeIndex = (themeIndex + 1) % Math.max(themes.length, 1));

  const commands: Command[] = $derived([
    { id: "search", title: "Search hosts", shortcut: "/", run: () => searchEl?.focus() },
    { id: "connect", title: "Connect to selected host", shortcut: "↵", run: connectSelected },
    { id: "reload", title: "Reload ~/.ssh/config", shortcut: "⌘R", run: load },
    ...themes.map((t, i) => ({ id: `theme-${t.id}`, title: `Theme: ${t.name}`, hint: t.description, run: () => (themeIndex = i) })),
  ]);

  function onkeydown(e: KeyboardEvent) {
    const mod = e.metaKey || e.ctrlKey;
    if (mod && e.key.toLowerCase() === "k") {
      paletteOpen = !paletteOpen;
      e.preventDefault();
    } else if (mod && e.key.toLowerCase() === "r") {
      load();
      e.preventDefault();
    } else if (!paletteOpen && document.activeElement !== searchEl) {
      if (e.key === "/") {
        searchEl?.focus();
        e.preventDefault();
      } else if (e.key === "j" || e.key === "ArrowDown") selected = Math.min(selected + 1, hosts.length - 1);
      else if (e.key === "k" || e.key === "ArrowUp") selected = Math.max(selected - 1, 0);
      else if (e.key === "Enter") connectSelected();
      else if (e.key === "t") cycleTheme();
      else if (e.key === "d") {
        const host = hosts[selected];
        if (host) disconnect(host);
      }
    }
  }

  onMount(() => {
    load();
    if (!isTauri()) return;
    const unlisten = api.onEngineEvent(onEngineEvent);
    return () => {
      unlisten.then((f) => f());
    };
  });
</script>

<svelte:window {onkeydown} />

<div class="shell">
  <header class="titlebar" data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <img src="/portico-mark.svg" alt="" width="18" height="18" />
      <span class="wordmark">PORTICO</span>
    </div>
    <button class="palette-trigger" onclick={() => (paletteOpen = true)}>
      Search or run a command… <kbd>⌘K</kbd>
    </button>
    <span class="theme-name">{theme?.name ?? ""}</span>
  </header>

  <main>
    <Sidebar {hosts} {sessions} {loading} bind:query bind:selected bind:searchEl onconnect={connect} />
    <HostDetails
      host={hosts[selected]}
      session={sessions[hosts[selected]?.alias ?? ""]}
      onconnect={connect}
      ondisconnect={disconnect}
    />
  </main>

  <footer class="statusbar">
    <span>{allHosts.length} hosts</span>
    <span class="spacer"></span>
    <span><kbd>/</kbd> search</span>
    <span><kbd>↵</kbd> connect</span>
    <span><kbd>d</kbd> disconnect</span>
    <span><kbd>t</kbd> theme</span>
    <span><kbd>⌘K</kbd> commands</span>
  </footer>
</div>

<CommandPalette bind:open={paletteOpen} {commands} />

{#if toast}
  <div class="toast" role="alert" transition:fly={{ y: 12, duration: 180 }}>
    <strong>{toast.message}</strong>
    {#if toast.hint}<span>{toast.hint}</span>{/if}
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
    grid-template-columns: 1fr auto 1fr;
    align-items: center;
    padding: 0 var(--space-4) 0 84px; /* room for macOS traffic lights */
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-base);
  }
  .brand { display: flex; align-items: center; gap: var(--space-2); }
  .wordmark {
    font-family: var(--font-display);
    font-size: var(--text-md);
    letter-spacing: 0.32em;
    color: var(--text-primary);
  }
  .palette-trigger {
    display: flex;
    align-items: center;
    gap: var(--space-5);
    height: 28px;
    padding: 0 var(--space-2) 0 var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-surface);
    color: var(--text-muted);
    font: inherit;
    font-size: var(--text-sm);
    transition: border-color var(--motion-fast) var(--ease-standard);
  }
  .palette-trigger:hover { border-color: var(--border-strong); }
  .theme-name { justify-self: end; color: var(--text-muted); font-size: var(--text-sm); }
  main {
    display: grid;
    grid-template-columns: minmax(280px, 340px) 1fr;
    min-height: 0;
  }
  .statusbar {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    padding: 0 var(--space-4);
    border-top: 1px solid var(--border-subtle);
    background: var(--bg-base);
    color: var(--text-muted);
    font-size: var(--text-xs);
  }
  .spacer { flex: 1; }
  .toast {
    position: fixed;
    right: var(--space-5);
    bottom: calc(28px + var(--space-4));
    display: grid;
    gap: var(--space-1);
    max-width: 380px;
    padding: var(--space-3) var(--space-4);
    background: var(--bg-overlay);
    border: 1px solid color-mix(in srgb, var(--status-error) 45%, transparent);
    border-left: 3px solid var(--status-error);
    border-radius: var(--radius-md);
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.4);
  }
  .toast strong { font-weight: 550; }
  .toast span { color: var(--text-secondary); font-size: var(--text-sm); }
</style>
