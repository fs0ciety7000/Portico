<script lang="ts">
  // One terminal tab: xterm.js (WebGL, DOM fallback) on the engine's PTY.
  //
  // Data plane: output arrives as raw bytes; each chunk is acknowledged once
  // xterm.js has parsed it (flow control, see src-tauri/src/terminals.rs).
  // Input goes through a single send loop (order is preserved, one IPC call
  // in flight), pasted text is sanitized.
  import { onMount, untrack } from "svelte";
  import { fade } from "svelte/transition";
  import { Terminal, type ITheme } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { WebglAddon } from "@xterm/addon-webgl";
  import "@xterm/xterm/css/xterm.css";
  import { asError, describe } from "../api";
  import { backend } from "../backend";
  import { appShortcut, chunks, kbd, sanitizePaste, sanitizeTitle } from "../keys";
  import { dur } from "../motion";
  import { isLive, workspace, type Tab } from "../state/workspace.svelte";
  import StatusBadge from "./StatusBadge.svelte";

  let {
    tab,
    active,
    themeId,
    autofocus = true,
  }: {
    tab: Tab;
    active: boolean;
    themeId: string;
    /** False while another panel (files) owns the keyboard: no automatic focus. */
    autofocus?: boolean;
  } = $props();

  /** Acknowledge parsed output at least this often (bytes). */
  const ACK_EVERY = 128 * 1024;
  /** Largest input sent in one IPC call (matches the backend's cap). */
  const INPUT_CHUNK = 64 * 1024;

  let host: HTMLDivElement;
  let term: Terminal | undefined;
  let fit: FitAddon | undefined;
  let destroyed = false;

  const session = $derived(workspace.sessions[tab.session]);
  const status = $derived(describe(session));
  const failed = $derived(session?.phase.phase === "failed" ? session.phase.error : null);
  const error = $derived(failed ?? tab.error);
  const waiting = $derived(tab.terminal === null && !tab.end && !error && isLive(session));
  const cancelled = $derived(tab.terminal === null && !error && !isLive(session) && !tab.end);
  /** A changed host key: retrying would only show the same refusal. */
  const keyChanged = $derived(error?.code === "host_key_changed");

  // ── Theme from tokens (the 16 ANSI colours are part of each theme) ──
  function readTheme(): ITheme {
    const css = getComputedStyle(document.documentElement);
    const v = (name: string) => css.getPropertyValue(name).trim();
    const ansi = (i: number) => v(`--ansi-${i}`);
    return {
      background: v("--bg-base"),
      foreground: v("--text-primary"),
      cursor: v("--accent"),
      cursorAccent: v("--bg-base"),
      selectionBackground: `${v("--accent")}55`,
      selectionInactiveBackground: `${v("--text-muted")}44`,
      scrollbarSliderBackground: `${v("--text-muted")}33`,
      scrollbarSliderHoverBackground: `${v("--text-muted")}66`,
      black: ansi(0), red: ansi(1), green: ansi(2), yellow: ansi(3),
      blue: ansi(4), magenta: ansi(5), cyan: ansi(6), white: ansi(7),
      brightBlack: ansi(8), brightRed: ansi(9), brightGreen: ansi(10), brightYellow: ansi(11),
      brightBlue: ansi(12), brightMagenta: ansi(13), brightCyan: ansi(14), brightWhite: ansi(15),
    };
  }

  // ── Input: one send loop, order preserved between text and binary ──
  let queue: { data: string; binary: boolean }[] = [];
  let flushScheduled = false;
  let sending = false;

  function send(data: string, binary: boolean) {
    // Before the id is known only automatic replies (DA, DSR…) get here:
    // keep them, the shell may wait for them.
    if (tab.end) return;
    const last = queue[queue.length - 1];
    if (last && last.binary === binary) last.data += data;
    else queue.push({ data, binary });
    schedule();
  }

  function schedule() {
    if (flushScheduled || sending || tab.terminal === null) return;
    flushScheduled = true;
    requestAnimationFrame(() => {
      flushScheduled = false;
      void flush();
    });
  }

  async function flush() {
    if (sending) return;
    sending = true;
    try {
      while (queue.length && tab.terminal !== null && !destroyed) {
        const id = tab.terminal;
        const items = queue;
        queue = [];
        for (const { data, binary } of items) {
          for (const part of chunks(data, INPUT_CHUNK)) {
            try {
              await backend.terminalInput(id, part, binary);
            } catch (e) {
              queue = []; // never send the rest out of context
              const err = asError(e);
              if (err.code !== "terminal_closed") workspace.notify(err);
              return;
            }
          }
        }
      }
    } finally {
      sending = false;
    }
  }

  function paste(text: string) {
    term?.paste(sanitizePaste(text));
  }

  // ── Output + flow control (counters belong to one opening) ──
  interface Flow {
    generation: number;
    id: number | null;
    parsed: number;
    acked: number;
  }

  function ack(flow: Flow, force: boolean) {
    if (flow.id === null || flow.parsed === flow.acked) return;
    if (force || flow.parsed - flow.acked >= ACK_EVERY) {
      flow.acked = flow.parsed;
      backend.terminalAck(flow.id, flow.parsed).catch(() => {});
    }
  }

  function write(flow: Flow, bytes: Uint8Array) {
    if (!term || flow.generation !== tab.generation) return;
    term.write(bytes, () => {
      if (flow.generation !== tab.generation) return;
      flow.parsed += bytes.length;
      ack(flow, false);
      // Idle again: settle the count so the engine never waits on us.
      queueMicrotask(() => ack(flow, true));
    });
  }

  /** Size in cells and pixels, for the PTY. */
  function size() {
    const box = host.querySelector(".xterm-screen")?.getBoundingClientRect();
    return {
      cols: term?.cols ?? 80,
      rows: term?.rows ?? 24,
      width: Math.round(box?.width ?? 0),
      height: Math.round(box?.height ?? 0),
    };
  }

  let sentSize = { cols: 0, rows: 0 };

  function pushSize() {
    if (!term || tab.terminal === null || tab.end) return;
    const s = size();
    if (s.cols === sentSize.cols && s.rows === sentSize.rows) return;
    sentSize = { cols: s.cols, rows: s.rows };
    backend.terminalResize(tab.terminal, s.cols, s.rows, s.width, s.height).catch(() => {});
  }

  async function open() {
    if (!term || !fit) return;
    const flow: Flow = { generation: tab.generation, id: null, parsed: 0, acked: 0 };
    if (flow.generation > 0) {
      term.write("\r\n\x1b[2m──── reconnecting ────\x1b[0m\r\n");
    }
    queue = [];
    fitNow();
    const s = size();
    try {
      const id = await backend.openTerminal(tab.session, s, (b) => write(flow, b));
      if (destroyed || flow.generation !== tab.generation) {
        backend.closeTerminal(id).catch(() => {});
        return;
      }
      flow.id = id;
      sentSize = { cols: s.cols, rows: s.rows };
      workspace.attach(tab, id);
      ack(flow, true);
      // The window may have changed while we were connecting.
      fitNow();
      pushSize();
      schedule();
      if (active && autofocus) term.focus();
    } catch (e) {
      if (destroyed || flow.generation !== tab.generation) return;
      const err = asError(e);
      // The session's own failure is shown from its phase.
      if (err.code !== "session_ended") tab.error = err;
    }
  }

  // ── Size ──
  let resizeTimer: ReturnType<typeof setTimeout> | undefined;
  function fitNow() {
    if (!fit || !term || !active || host.clientWidth === 0) return;
    fit.fit();
  }
  function onResize() {
    clearTimeout(resizeTimer);
    resizeTimer = setTimeout(() => {
      fitNow();
      pushSize();
    }, 50);
  }

  /** xterm measures its cells once: wait (briefly) for the bundled font. */
  async function fontsReady(family: string, size: number) {
    const timeout = new Promise((resolve) => setTimeout(resolve, 1000));
    await Promise.race([document.fonts.load(`${size}px ${family}`).catch(() => {}), timeout]);
  }

  onMount(() => {
    const css = getComputedStyle(document.documentElement);
    const fontFamily = css.getPropertyValue("--font-mono").trim();
    const fontSize = Number.parseFloat(css.getPropertyValue("--text-base")) || 13;
    void fontsReady(fontFamily, fontSize).then(() => {
      if (!destroyed) create(fontFamily, fontSize);
    });
    const observer = new ResizeObserver(onResize);
    observer.observe(host);
    return () => {
      destroyed = true;
      observer.disconnect();
      clearTimeout(resizeTimer);
      term?.dispose();
      term = undefined;
    };
  });

  function create(fontFamily: string, fontSize: number) {
    term = new Terminal({
      fontFamily,
      fontSize,
      lineHeight: 1.2,
      cursorBlink: true,
      cursorStyle: "bar",
      cursorInactiveStyle: "outline",
      scrollback: 10000,
      allowProposedApi: false,
      // Option types | [ ] { } ~ on non-US Mac layouts: keep it a normal key.
      macOptionIsMeta: false,
      // OSC 8 links stay inert until clickable links get their own,
      // confirmed, http(s)-only handler (post-MVP).
      linkHandler: { activate: () => {}, allowNonHttpProtocols: false },
      theme: readTheme(),
    });
    fit = new FitAddon();
    term.loadAddon(fit);
    term.open(host);
    try {
      const webgl = new WebglAddon();
      // GPU context lost (sleep, driver reset): fall back to the DOM renderer.
      webgl.onContextLoss(() => webgl.dispose());
      term.loadAddon(webgl);
    } catch {
      // No WebGL2: the DOM renderer is already active.
    }

    term.attachCustomKeyEventHandler((e) => {
      if (appShortcut(e, true)) return false; // handled by the app (bubbles to window)
      // A hidden view never takes input (focus may linger after switching).
      if (!active) {
        e.preventDefault();
        return false;
      }
      if (tab.terminal !== null && !tab.end) return true;
      // No shell behind this view: Enter (re)connects, Esc cancels or goes
      // back to the hosts, the rest is swallowed (also by the browser: no
      // stray line break later).
      e.preventDefault();
      if (e.type !== "keydown") return false;
      if (e.key === "Enter" && !waiting && !keyChanged) void workspace.reconnect(tab.key);
      if (e.key === "Escape") {
        if (waiting) void workspace.close(tab.key);
        else workspace.active = null;
      }
      return false;
    });
    term.onData((d) => send(d, false));
    term.onBinary((d) => send(d, true));
    term.onTitleChange((t) => (tab.title = sanitizeTitle(t)));

    // Every paste (menu, Shift-Insert, middle click) is sanitized first.
    const onPaste = (e: ClipboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const text = e.clipboardData?.getData("text/plain");
      if (text) paste(text);
    };
    host.addEventListener("paste", onPaste, true);

    void open();
    if (active && autofocus) requestAnimationFrame(() => term?.focus());
  }

  // (Re)open whenever the tab asks for a new generation.
  $effect(() => {
    void tab.generation;
    untrack(() => void open());
  });

  $effect(() => {
    void themeId;
    if (term) term.options.theme = readTheme();
  });

  $effect(() => {
    if (active && term) {
      const focus = autofocus;
      requestAnimationFrame(() => {
        onResize();
        if (focus) term?.focus();
      });
    }
  });

  export function copySelection(): string {
    return term?.getSelection() ?? "";
  }
  export function pasteText(text: string) {
    paste(text);
  }
  /** DECSET 2004 as tracked by xterm.js (snippets are wrapped when on). */
  export function bracketedPaste(): boolean {
    return term?.modes.bracketedPasteMode ?? false;
  }
  export function focus() {
    term?.focus();
  }

  const endLabel = $derived.by(() => {
    const end = tab.end;
    if (!end) return "";
    if (end.kind === "closed") return isLive(session) ? "Shell closed" : "Connection closed";
    if (end.signal) return `Shell killed by SIG${end.signal}`;
    return `Shell exited${end.status ? ` with status ${end.status}` : ""}`;
  });
  const endIsError = $derived(!!tab.end && tab.end.kind === "exited" && (!!tab.end.signal || !!tab.end.status));
</script>

<section class="terminal" class:hidden={!active} aria-hidden={!active}>
  <div class="xterm-host" class:dimmed={!!tab.end || !!failed} bind:this={host}></div>

  {#if waiting || cancelled}
    <div class="overlay" transition:fade={{ duration: dur("fast") }}>
      <div class="card">
        <h2>{tab.alias}</h2>
        <StatusBadge status={status.status} label={status.label} latencyMs={session?.latencyMs ?? null} />
        {#if waiting}
          <p class="muted">Press <kbd>Esc</kbd> or <kbd>{kbd("W")}</kbd> to cancel</p>
        {:else}
          <div class="actions">
            <button class="secondary" onclick={() => workspace.close(tab.key)}>Close <kbd>{kbd("W")}</kbd></button>
            <button class="primary" onclick={() => workspace.reconnect(tab.key)}>Connect <kbd>↵</kbd></button>
          </div>
        {/if}
      </div>
    </div>
  {:else if error}
    <div class="overlay" transition:fade={{ duration: dur("fast") }}>
      <div class="card failure" class:neutral={error.code === "host_key_rejected"}>
        <h2>{tab.alias}</h2>
        <StatusBadge status={status.status} label={status.label} />
        {#if keyChanged}
          <!-- The blocking alert already showed the details. -->
          <p class="message">Host key changed: possible man-in-the-middle.</p>
          <p class="hint">
            Portico refused to connect. Fix <code>known_hosts</code> only once the administrator confirms the new key.
          </p>
        {:else}
          <p class="message">{error.message}</p>
          {#if error.hint}<p class="hint">{error.hint}</p>{/if}
        {/if}
        <div class="actions">
          {#if keyChanged}
            <button class="secondary" onclick={() => workspace.reconnect(tab.key)}>Retry</button>
            {#if workspace.hasAlert(tab.session)}
              <button class="secondary" onclick={() => workspace.showAlert(tab.session)}>Show details</button>
            {/if}
            <button class="primary" onclick={() => workspace.close(tab.key)}>Close <kbd>{kbd("W")}</kbd></button>
          {:else}
            <button class="secondary" onclick={() => (workspace.active = null)}>Hosts <kbd>Esc</kbd></button>
            <button class="secondary" onclick={() => workspace.close(tab.key)}>Close <kbd>{kbd("W")}</kbd></button>
            <button class="primary" onclick={() => workspace.reconnect(tab.key)}>Retry <kbd>↵</kbd></button>
          {/if}
        </div>
      </div>
    </div>
  {:else if tab.end}
    <div class="ended" data-status={status.status} class:error={endIsError} transition:fade={{ duration: dur("fast") }}>
      <span class="dot" class:hollow={!isLive(session)}></span>
      <span class="what">{endLabel}</span>
      {#if isLive(session)}<span class="muted-inline">· session still connected</span>{/if}
      <span class="spacer"></span>
      <button class="link" onclick={() => workspace.reconnect(tab.key)}>Reconnect <kbd>↵</kbd></button>
      <button class="link" onclick={() => workspace.close(tab.key)}>Close <kbd>{kbd("W")}</kbd></button>
    </div>
  {/if}
</section>

<style>
  .terminal {
    position: relative;
    height: 100%;
    min-height: 0;
    background: var(--bg-base);
  }
  .terminal.hidden { display: none; }
  .xterm-host {
    position: absolute;
    inset: var(--space-3) var(--space-2) var(--space-2) var(--space-4);
    transition: opacity var(--motion-base) var(--ease-standard);
  }
  .xterm-host.dimmed { opacity: 0.55; }
  .xterm-host :global(.xterm) { height: 100%; }
  .xterm-host :global(.xterm-viewport) { background: transparent !important; }

  .overlay {
    position: absolute;
    inset: 0;
    display: grid;
    place-items: center;
    background: color-mix(in srgb, var(--bg-base) 70%, transparent);
  }
  .card {
    display: grid;
    justify-items: center;
    gap: var(--space-3);
    min-width: 320px;
    max-width: 540px;
    padding: var(--space-6) var(--space-6) var(--space-5);
    text-align: center;
  }
  .card h2 {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 600;
    font-size: var(--text-display-sm);
    letter-spacing: -0.015em;
  }
  .card.failure {
    background: var(--bg-overlay);
    border: 1px solid var(--border-subtle);
    border-top: 2px solid var(--status-error);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-overlay);
  }
  .card.failure.neutral { border-top-color: var(--border-strong); }
  .message { margin: var(--space-2) 0 0; color: var(--text-primary); }
  .hint { margin: 0; color: var(--text-secondary); font-size: var(--text-sm); }
  .hint code { font-family: var(--font-mono); color: var(--text-primary); }
  .muted { margin: 0; color: var(--text-muted); font-size: var(--text-sm); }
  .actions { display: flex; flex-wrap: wrap; justify-content: center; gap: var(--space-2); margin-top: var(--space-3); }

  .ended {
    --c: var(--status-idle);
    position: absolute;
    left: var(--space-4);
    right: var(--space-4);
    bottom: var(--space-4);
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 40px;
    padding: 0 var(--space-2) 0 var(--space-4);
    background: var(--bg-overlay);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-raised);
    color: var(--text-secondary);
  }
  .ended[data-status="connected"] { --c: var(--status-connected); }
  .ended[data-status="warning"] { --c: var(--status-warning); }
  .ended[data-status="error"] { --c: var(--status-error); }
  .ended .dot { width: 7px; height: 7px; margin-right: var(--space-1); border-radius: 50%; background: var(--c); }
  .ended .dot.hollow { background: transparent; box-shadow: inset 0 0 0 1.5px var(--c); }
  .ended.error .what { color: var(--status-warning); }
  .muted-inline { color: var(--text-muted); font-size: var(--text-sm); }
  .spacer { flex: 1; }

  button {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: 32px;
    padding: 0 var(--space-4);
    border-radius: var(--radius-md);
    font: inherit;
    transition: background var(--motion-fast) var(--ease-standard),
      transform var(--motion-fast) var(--ease-standard);
  }
  button:active { transform: scale(0.97); }
  button.primary {
    border: 0;
    background: var(--accent);
    color: var(--text-on-accent);
    font-weight: 600;
  }
  button.primary:hover { background: var(--accent-strong); }
  button.primary kbd {
    background: color-mix(in srgb, var(--text-on-accent) 12%, transparent);
    border-color: transparent;
    color: inherit;
  }
  button.secondary {
    border: 1px solid var(--border-strong);
    background: transparent;
    color: var(--text-primary);
  }
  button.secondary:hover { background: var(--bg-elevated); }
  button.link {
    height: 28px;
    padding: 0 var(--space-2);
    border: 0;
    background: transparent;
    color: var(--text-secondary);
  }
  button.link:hover { background: var(--bg-elevated); color: var(--text-primary); }
</style>
