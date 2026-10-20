<script lang="ts">
  // One terminal tab: xterm.js (WebGL, DOM fallback) on the engine's PTY.
  //
  // Data plane: output arrives as raw bytes; each chunk is acknowledged once
  // xterm.js has parsed it (flow control, see src-tauri/src/terminals.rs).
  // Input is batched per animation frame and pasted text is sanitized.
  import { fade } from "svelte/transition";
  import { Terminal, type ITheme } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { WebglAddon } from "@xterm/addon-webgl";
  import "@xterm/xterm/css/xterm.css";
  import { onMount, untrack } from "svelte";
  import { asError, describe } from "../api";
  import { backend } from "../backend";
  import { appShortcut, kbd, sanitizePaste, sanitizeTitle } from "../keys";
  import { isLive, workspace, type Tab } from "../state/workspace.svelte";
  import StatusBadge from "./StatusBadge.svelte";

  let { tab, active, themeId }: { tab: Tab; active: boolean; themeId: string } = $props();

  /** Acknowledge parsed output at least this often (bytes). */
  const ACK_EVERY = 128 * 1024;
  /** Largest input sent in one IPC call (a big paste is split). */
  const INPUT_CHUNK = 64 * 1024;

  let host: HTMLDivElement;
  let term: Terminal | undefined;
  let fit: FitAddon | undefined;
  let destroyed = false;

  const session = $derived(workspace.sessions[tab.session]);
  const status = $derived(describe(session));
  const failed = $derived(session?.phase.phase === "failed" ? session.phase.error : null);
  const waiting = $derived(tab.terminal === null && !tab.end && !failed && !tab.error && isLive(session));
  const cancelled = $derived(tab.terminal === null && !failed && !tab.error && !isLive(session) && !tab.end);

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

  // ── Input: batched per frame, order preserved between text and binary ──
  let queue: { data: string; binary: boolean }[] = [];
  let flushScheduled = false;

  function send(data: string, binary: boolean) {
    if (tab.terminal === null || tab.end) return;
    const last = queue[queue.length - 1];
    if (last && last.binary === binary) last.data += data;
    else queue.push({ data, binary });
    if (!flushScheduled) {
      flushScheduled = true;
      requestAnimationFrame(flush);
    }
  }

  async function flush() {
    flushScheduled = false;
    const items = queue;
    queue = [];
    const id = tab.terminal;
    if (id === null) return;
    for (const { data, binary } of items) {
      for (let at = 0; at < data.length; at += INPUT_CHUNK) {
        try {
          await backend.terminalInput(id, data.slice(at, at + INPUT_CHUNK), binary);
        } catch {
          return; // closed: the terminal event will say why
        }
      }
    }
  }

  function paste(text: string) {
    term?.paste(sanitizePaste(text));
  }

  // ── Output + flow control ──
  let parsed = 0;
  let acked = 0;

  function ack(force: boolean) {
    const id = tab.terminal;
    if (id === null || parsed === acked) return;
    if (force || parsed - acked >= ACK_EVERY) {
      acked = parsed;
      backend.terminalAck(id, parsed).catch(() => {});
    }
  }

  function write(bytes: Uint8Array, generation: number) {
    if (!term || generation !== tab.generation) return;
    term.write(bytes, () => {
      parsed += bytes.length;
      ack(false);
      // Idle again: settle the count so the engine never waits on us.
      queueMicrotask(() => ack(true));
    });
  }

  async function open() {
    if (!term || !fit) return;
    const generation = tab.generation;
    const session = tab.session;
    if (generation > 0) {
      term.write("\r\n\x1b[2m──── reconnecting ────\x1b[0m\r\n");
    }
    parsed = 0;
    acked = 0;
    fitNow();
    try {
      const id = await backend.openTerminal(session, term.cols, term.rows, (b) => write(b, generation));
      if (destroyed || generation !== tab.generation) {
        backend.closeTerminal(id).catch(() => {});
        return;
      }
      tab.terminal = id;
      ack(true);
      if (active) term.focus();
    } catch (e) {
      if (destroyed || generation !== tab.generation) return;
      const err = e as { code?: string };
      // The session's own failure is shown from its phase.
      if (err.code !== "session_ended") tab.error = asError(e);
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
      if (term && tab.terminal !== null && !tab.end) {
        backend.terminalResize(tab.terminal, term.cols, term.rows).catch(() => {});
      }
    }, 50);
  }

  onMount(() => {
    const css = getComputedStyle(document.documentElement);
    term = new Terminal({
      fontFamily: css.getPropertyValue("--font-mono").trim(),
      fontSize: 13,
      lineHeight: 1.2,
      cursorBlink: true,
      cursorStyle: "bar",
      cursorInactiveStyle: "outline",
      scrollback: 10000,
      allowProposedApi: false,
      macOptionIsMeta: true,
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
      // No shell behind this view: Enter (re)connects, Esc cancels, the rest
      // is swallowed (also by the browser: no stray line break later).
      e.preventDefault();
      if (e.type === "keydown" && e.key === "Enter" && !waiting) workspace.reconnect(tab.key);
      if (e.type === "keydown" && e.key === "Escape" && waiting) workspace.close(tab.key);
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

    const observer = new ResizeObserver(onResize);
    observer.observe(host);

    return () => {
      destroyed = true;
      observer.disconnect();
      host.removeEventListener("paste", onPaste, true);
      clearTimeout(resizeTimer);
      term?.dispose();
      term = undefined;
    };
  });

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
      requestAnimationFrame(() => {
        onResize();
        term?.focus();
      });
    }
  });

  export function copySelection(): string {
    return term?.getSelection() ?? "";
  }
  export function pasteText(text: string) {
    paste(text);
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
    <div class="overlay" transition:fade={{ duration: 120 }}>
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
  {:else if failed || tab.error}
    {@const error = failed ?? tab.error}
    <div class="overlay" transition:fade={{ duration: 120 }}>
      <div class="card failure" class:neutral={error?.code === "host_key_rejected"}>
        <h2>{tab.alias}</h2>
        <StatusBadge status={status.status} label={status.label} />
        {#if error?.code === "host_key_changed"}
          <!-- The blocking alert already showed the details. -->
          <p class="message">Host key changed: possible man-in-the-middle.</p>
          <p class="hint">Portico refused to connect. Fix <code>known_hosts</code> only once the administrator confirms the new key.</p>
        {:else}
          <p class="message">{error?.message}</p>
          {#if error?.hint}<p class="hint">{error.hint}</p>{/if}
        {/if}
        <div class="actions">
          <button class="secondary" onclick={() => workspace.close(tab.key)}>Close <kbd>{kbd("W")}</kbd></button>
          <button class="primary" onclick={() => workspace.reconnect(tab.key)}>Retry <kbd>↵</kbd></button>
        </div>
      </div>
    </div>
  {:else if tab.end}
    <div class="ended" class:error={endIsError} transition:fade={{ duration: 120 }}>
      <span class="dot"></span>
      <span>{endLabel}</span>
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
    max-width: 520px;
    padding: var(--space-6) var(--space-6) var(--space-5);
    text-align: center;
  }
  .card h2 {
    margin: 0;
    font-family: var(--font-display);
    font-weight: 500;
    font-size: 30px;
    letter-spacing: 0.01em;
  }
  .card.failure {
    background: var(--bg-overlay);
    border: 1px solid var(--border-subtle);
    border-top: 2px solid var(--status-error);
    border-radius: var(--radius-lg);
    box-shadow: 0 16px 48px rgb(0 0 0 / 0.45);
  }
  .card.failure.neutral { border-top-color: var(--border-strong); }
  .message { margin: var(--space-2) 0 0; color: var(--text-primary); }
  .hint { margin: 0; color: var(--text-secondary); font-size: var(--text-sm); }
  .hint code { font-family: var(--font-mono); color: var(--text-primary); }
  .muted { margin: 0; color: var(--text-muted); font-size: var(--text-sm); }
  .actions { display: flex; gap: var(--space-2); margin-top: var(--space-3); }

  .ended {
    --c: var(--status-idle);
    position: absolute;
    left: var(--space-4);
    right: var(--space-4);
    bottom: var(--space-4);
    display: flex;
    align-items: center;
    gap: var(--space-3);
    height: 40px;
    padding: 0 var(--space-2) 0 var(--space-4);
    background: var(--bg-overlay);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    box-shadow: 0 12px 32px rgb(0 0 0 / 0.4);
    color: var(--text-secondary);
  }
  .ended.error { --c: var(--status-warning); }
  .ended .dot { width: 7px; height: 7px; border-radius: 50%; background: var(--c); }
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
