<script lang="ts">
  // One terminal (a pane of a tab): xterm.js (WebGL, DOM fallback) on the engine's PTY.
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
  import { asError, formatRtt, sentence } from "../api";
  import { backend } from "../backend";
  import { chunks, sanitizePaste, sanitizeTitle } from "../keys";
  import { keymap } from "../state/keymap.svelte";
  import { logs } from "../state/logs.svelte";
  import { dur } from "../motion";
  import { isLive, paneStatus, workspace, type Pane } from "../state/workspace.svelte";
  import StatusBadge from "./StatusBadge.svelte";

  let {
    pane,
    visible,
    focused,
    split = false,
    zoomed = false,
    demoted = false,
    themeId,
    autofocus = true,
    onfocusin,
  }: {
    pane: Pane;
    /** On screen: its tab is active and no other pane is zoomed. */
    visible: boolean;
    /** The focused pane of its tab (the only one that takes keystrokes). */
    focused: boolean;
    /** Its tab shows several panes: a status header, the accent edge when focused, dimmed otherwise. */
    split?: boolean;
    /** Takes the whole tab while its siblings are hidden. */
    zoomed?: boolean;
    /** A divider has the keyboard: the focused pane gives up its accent
     * (one accent at a time), keeping a neutral ring. */
    demoted?: boolean;
    themeId: string;
    /** False while another panel (files) owns the keyboard: no automatic focus. */
    autofocus?: boolean;
    /** The keyboard (or a click) entered this pane. */
    onfocusin?: () => void;
  } = $props();
  /** Visible and focused: takes the keyboard. */
  const active = $derived(visible && focused);
  // A lone pane closes with its tab; in a split tab, `tab.close` closes the
  // focused pane (unless `pane.close` has a key of its own).
  const closeKey = $derived(split ? (keymap.label("pane.close") ?? keymap.label("tab.close")) : keymap.label("tab.close"));
  const closeLabel = $derived(split ? "Close pane" : "Close");
  const logsKey = $derived(keymap.label("app.logs"));
  const zoomKey = $derived(keymap.label("pane.zoom"));

  /** Acknowledge parsed output at least this often (bytes). */
  const ACK_EVERY = 128 * 1024;
  /** Largest input sent in one IPC call (matches the backend's cap). */
  const INPUT_CHUNK = 64 * 1024;

  let host: HTMLDivElement;
  let term: Terminal | undefined;
  let fit: FitAddon | undefined;
  let destroyed = false;

  const session = $derived(workspace.sessions[pane.session]);
  /** Status, label, dot fill: the same grammar as the tab and the status bar. */
  const status = $derived(paneStatus(pane, session));
  const failed = $derived(session?.phase.phase === "failed" ? session.phase.error : null);
  const error = $derived(failed ?? pane.error);
  const waiting = $derived(pane.terminal === null && !pane.end && !error && isLive(session));
  const cancelled = $derived(pane.terminal === null && !error && !isLive(session) && !pane.end);
  /** A changed host key: retrying would only show the same refusal. */
  const keyChanged = $derived(error?.code === "host_key_changed");

  // ── Theme from tokens (the 16 ANSI colours are part of each theme) ──
  /** `hideCursor`: an overlay (connecting, failure) covers the terminal: no
   * accent cursor blinking behind it. */
  function readTheme(hideCursor: boolean): ITheme {
    const css = getComputedStyle(document.documentElement);
    const v = (name: string) => css.getPropertyValue(name).trim();
    const ansi = (i: number) => v(`--ansi-${i}`);
    return {
      background: v("--bg-base"),
      foreground: v("--text-primary"),
      cursor: hideCursor ? `${v("--bg-base")}00` : v("--accent"),
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
    if (pane.end) return;
    const last = queue[queue.length - 1];
    if (last && last.binary === binary) last.data += data;
    else queue.push({ data, binary });
    schedule();
  }

  function schedule() {
    if (flushScheduled || sending || pane.terminal === null) return;
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
      while (queue.length && pane.terminal !== null && !destroyed) {
        const id = pane.terminal;
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

  // The cursor blinks while something happens and goes solid after a few
  // quiet seconds: a blinking cursor repaints forever, which costs real CPU
  // with software rendering (docs/PERF.md) for no information.
  const BLINK_WHILE_ACTIVE_MS = 5000;
  let blinkTimer: ReturnType<typeof setTimeout> | undefined;

  function wake() {
    if (!term) return;
    if (!term.options.cursorBlink) term.options.cursorBlink = true;
    clearTimeout(blinkTimer);
    blinkTimer = setTimeout(() => {
      if (term) term.options.cursorBlink = false;
    }, BLINK_WHILE_ACTIVE_MS);
  }

  function write(flow: Flow, bytes: Uint8Array) {
    if (!term || flow.generation !== pane.generation) return;
    wake();
    term.write(bytes, () => {
      if (flow.generation !== pane.generation) return;
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
    if (!term || pane.terminal === null || pane.end) return;
    const s = size();
    if (s.cols === sentSize.cols && s.rows === sentSize.rows) return;
    sentSize = { cols: s.cols, rows: s.rows };
    backend.terminalResize(pane.terminal, s.cols, s.rows, s.width, s.height).catch(() => {});
  }

  async function open() {
    if (!term || !fit) return;
    const flow: Flow = { generation: pane.generation, id: null, parsed: 0, acked: 0 };
    if (flow.generation > 0) {
      term.write("\r\n\x1b[2m──── reconnecting ────\x1b[0m\r\n");
    }
    queue = [];
    fitNow();
    const s = size();
    try {
      const id = await backend.openTerminal(pane.session, s, (b) => write(flow, b));
      if (destroyed || flow.generation !== pane.generation) {
        backend.closeTerminal(id).catch(() => {});
        return;
      }
      flow.id = id;
      sentSize = { cols: s.cols, rows: s.rows };
      workspace.attach(pane, id);
      ack(flow, true);
      // The window may have changed while we were connecting.
      fitNow();
      pushSize();
      schedule();
      if (active && autofocus) term.focus();
    } catch (e) {
      if (destroyed || flow.generation !== pane.generation) return;
      const err = asError(e);
      // The session's own failure is shown from its phase.
      if (err.code !== "session_ended") pane.error = err;
    }
  }

  // ── Size ──
  let resizeTimer: ReturnType<typeof setTimeout> | undefined;
  function fitNow() {
    if (!fit || !term || !visible || host.clientWidth === 0) return;
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
      clearTimeout(blinkTimer);
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
      // Only the focused pane of a split tab shows a cursor (see the effect below).
      cursorInactiveStyle: split && !focused ? "none" : "outline",
      scrollback: 10000,
      allowProposedApi: false,
      // Option types | [ ] { } ~ on non-US Mac layouts: keep it a normal key.
      macOptionIsMeta: false,
      // OSC 8 links stay inert until clickable links get their own,
      // confirmed, http(s)-only handler (post-MVP).
      linkHandler: { activate: () => {}, allowNonHttpProtocols: false },
      theme: readTheme(covered),
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
      if (e.type === "keydown" && keymap.match(e, "terminal")) return false; // handled by the app (bubbles to window)
      // A hidden view never takes input (focus may linger after switching).
      if (!active) {
        e.preventDefault();
        return false;
      }
      if (pane.terminal !== null && !pane.end) return true;
      // No shell behind this view: Enter (re)connects, Esc cancels or goes
      // back to the hosts, the rest is swallowed (also by the browser: no
      // stray line break later).
      e.preventDefault();
      if (e.type !== "keydown") return false;
      if (e.key === "Enter" && !waiting && !keyChanged) void workspace.reconnect(pane.key);
      if (e.key === "Escape") {
        if (waiting) void workspace.closePane(pane.key);
        // In a split tab the other panes stay: Esc does not leave them.
        else if (!split) workspace.active = null;
      }
      return false;
    });
    term.onData((d) => {
      wake();
      send(d, false);
    });
    term.onBinary((d) => send(d, true));
    term.onTitleChange((t) => (pane.title = sanitizeTitle(t)));

    // Every paste (menu, Shift-Insert, middle click) is sanitized first.
    const onPaste = (e: ClipboardEvent) => {
      e.preventDefault();
      e.stopPropagation();
      const text = e.clipboardData?.getData("text/plain");
      if (text) paste(text);
    };
    host.addEventListener("paste", onPaste, true);

    void open();
    wake();
    if (active && autofocus) requestAnimationFrame(() => term?.focus());
  }

  // (Re)open whenever the tab asks for a new generation.
  $effect(() => {
    void pane.generation;
    untrack(() => void open());
  });

  /** An overlay covers the terminal. */
  const covered = $derived(waiting || cancelled || !!error);

  $effect(() => {
    void themeId;
    const hide = covered;
    if (term) term.options.theme = readTheme(hide);
  });

  // Inactive panes of a split tab: no cursor at all (the accent one is the
  // focused pane's); a lone terminal keeps xterm's outline when unfocused.
  $effect(() => {
    const style = split && !focused ? "none" : "outline";
    if (term && term.options.cursorInactiveStyle !== style) term.options.cursorInactiveStyle = style;
  });

  $effect(() => {
    if (visible && term) requestAnimationFrame(onResize);
  });

  $effect(() => {
    if (active && term) {
      const focus = autofocus;
      requestAnimationFrame(() => {
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
    const end = pane.end;
    if (!end) return "";
    if (end.kind === "closed") return isLive(session) ? "Shell closed" : "Connection closed";
    if (end.signal) return `Shell killed by SIG${end.signal}`;
    return `Shell exited${end.status ? ` with status ${end.status}` : ""}`;
  });
  const endIsError = $derived(!!pane.end && pane.end.kind === "exited" && (!!pane.end.signal || !!pane.end.status));
</script>

<section
  class="terminal"
  class:hidden={!visible}
  class:split
  class:focused
  class:demoted
  aria-hidden={!visible}
  aria-busy={waiting}
  aria-label={split ? `${pane.alias}, pane${focused ? ' (focused)' : ''}` : undefined}
  data-pane={pane.key}
  {onfocusin}
>
  {#if split}
    <!-- Per-pane status: the status bar only speaks for the focused one.
         The alias gives way first (ellipsis), never the dot or the phase. -->
    <header class="head" data-status={status.status}>
      <span class="dot" class:busy={status.busy} class:hollow={status.hollow}></span>
      <span class="alias" title={pane.alias}>{pane.alias}</span>
      <span class="phase">{pane.end ? endLabel : status.label}</span>
      {#if session?.latencyMs != null && session.phase.phase === "connected" && !pane.end && !status.failed}
        <span class="rtt mono" class:slow={status.status === "warning"}>{formatRtt(session.latencyMs)}</span>
      {/if}
      {#if pane.title}<span class="title" title={pane.title}>{pane.title}</span>{/if}
      {#if zoomed}<span class="zoom" title={zoomKey ? `Zoom (${zoomKey})` : 'Zoom'}>Zoom</span>{/if}
    </header>
  {/if}
  <div class="xterm-host" class:dimmed={!!pane.end || !!failed} class:idle={split && !focused} bind:this={host}></div>

  {#if waiting || cancelled}
    <div class="overlay" transition:fade={{ duration: dur("fast") }}>
      <div class="card">
        <h2>{pane.alias}</h2>
        <StatusBadge status={status.status} label={status.label} latencyMs={session?.latencyMs ?? null} />
        {#if waiting}
          <p class="muted wide">Press <kbd>Esc</kbd>{#if closeKey} or <kbd>{closeKey}</kbd>{/if} to cancel</p>
          <p class="muted narrow">Esc to cancel</p>
        {:else}
          <div class="actions">
            <button class="secondary" onclick={() => workspace.closePane(pane.key)}>{closeLabel}{#if closeKey} <kbd>{closeKey}</kbd>{/if}</button>
            <button class="primary" onclick={() => workspace.reconnect(pane.key)}>Connect <kbd>↵</kbd></button>
          </div>
        {/if}
      </div>
      {#if split}
        <!-- A short pane: one line instead of the card. -->
        <div class="compact" data-status={status.status}>
          <span class="dot" class:busy={status.busy} class:hollow={status.hollow}></span>
          <span class="message">{status.label}</span>
          {#if waiting}
            <span class="muted-inline">Esc to cancel</span>
          {:else}
            <button class="link" onclick={() => workspace.reconnect(pane.key)}>Connect <kbd>↵</kbd></button>
            <button class="link" onclick={() => workspace.closePane(pane.key)}>{closeLabel}</button>
          {/if}
        </div>
      {/if}
    </div>
  {:else if error}
    <div class="overlay" transition:fade={{ duration: dur("fast") }}>
      <div class="card failure" class:neutral={error.code === "host_key_rejected"}>
        <h2>{pane.alias}</h2>
        <StatusBadge status={status.status} label={status.label} />
        {#if keyChanged}
          <!-- The blocking alert already showed the details. -->
          <p class="message">Host key changed: possible man-in-the-middle.</p>
          <p class="hint">
            Portico refused to connect. Fix <code>known_hosts</code> only once the administrator confirms the new key.
          </p>
        {:else}
          <p class="message">{sentence(error.message)}</p>
          {#if error.hint}<p class="hint">{error.hint}</p>{/if}
        {/if}
        <!-- Same entry point as the host details: the connection records. -->
        <button class="link" onclick={() => void logs.show(pane.session)}>
          Show session logs{#if logsKey} <kbd>{logsKey}</kbd>{/if}
        </button>
        <div class="actions">
          {#if keyChanged}
            <button class="secondary" onclick={() => workspace.reconnect(pane.key)}>Retry</button>
            {#if workspace.hasAlert(pane.session)}
              <button class="secondary" onclick={() => workspace.showAlert(pane.session)}>Show details</button>
            {/if}
            <button class="primary" onclick={() => workspace.closePane(pane.key)}>{closeLabel}{#if closeKey} <kbd>{closeKey}</kbd>{/if}</button>
          {:else}
            <!-- In a split tab, the other panes stay: no way out to the hosts from here. -->
            {#if !split}<button class="secondary" onclick={() => (workspace.active = null)}>Hosts <kbd>Esc</kbd></button>{/if}
            <button class="secondary" onclick={() => workspace.closePane(pane.key)}>{closeLabel}{#if closeKey} <kbd>{closeKey}</kbd>{/if}</button>
            <button class="primary" onclick={() => workspace.reconnect(pane.key)}>Retry <kbd>↵</kbd></button>
          {/if}
        </div>
      </div>
      {#if split}
        <!-- A short pane: one line, the hint in its tooltip. -->
        {@const text = keyChanged ? "Host key changed: possible man-in-the-middle." : sentence(error.message)}
        {@const tip = keyChanged ? "Fix known_hosts only once the administrator confirms the new key." : error.hint}
        <div class="compact failure" data-status={status.status} title={tip ? `${text}\n${tip}` : text}>
          <span class="dot"></span>
          <span class="message">{text}</span>
          <button class="link" onclick={() => workspace.reconnect(pane.key)}>Retry{#if !keyChanged} <kbd>↵</kbd>{/if}</button>
          <button class="link" onclick={() => workspace.closePane(pane.key)}>{closeLabel}</button>
        </div>
      {/if}
    </div>
  {:else if pane.end}
    <div class="ended" data-status={status.status} class:error={endIsError} transition:fade={{ duration: dur("fast") }}>
      <span class="dot" class:hollow={!isLive(session)}></span>
      <span class="what">{endLabel}</span>
      {#if isLive(session)}<span class="muted-inline">· session still connected</span>{/if}
      <span class="spacer"></span>
      <button class="link" onclick={() => workspace.reconnect(pane.key)}>Reconnect <kbd>↵</kbd></button>
      <button class="link" onclick={() => workspace.closePane(pane.key)}>{closeLabel}{#if closeKey} <kbd>{closeKey}</kbd>{/if}</button>
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

  /* ── Split panes: one framed surface per terminal. The focused one gets
     the accent edge (top bar + ring); the others are dimmed. The chrome
     sizes here are the ones App.svelte counts in the minimum pane size. ── */
  .terminal.split {
    --pane-head: calc(var(--space-5) + var(--space-1));
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    overflow: hidden;
    container-type: size;
    transition: border-color var(--motion-fast) var(--ease-standard);
  }
  .terminal.split.focused { border-color: color-mix(in srgb, var(--accent) 55%, transparent); }
  /* A divider has the keyboard: it carries the accent, the pane keeps a neutral ring. */
  .terminal.split.focused.demoted { border-color: var(--border-strong); }
  .terminal.split .xterm-host {
    inset: calc(var(--pane-head) + var(--space-1)) var(--space-2) var(--space-2) var(--space-3);
  }
  .xterm-host.idle { opacity: 0.6; }
  .xterm-host.idle.dimmed { opacity: 0.4; }
  .head {
    --c: var(--status-idle);
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--pane-head);
    min-width: 0;
    padding: 0 var(--space-3);
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-surface);
    color: var(--text-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
    transition: color var(--motion-fast) var(--ease-standard),
      box-shadow var(--motion-fast) var(--ease-standard);
  }
  .focused .head { color: var(--text-secondary); box-shadow: inset 0 2px 0 0 var(--accent); }
  .focused.demoted .head { box-shadow: none; }
  .head[data-status="connecting"] { --c: var(--status-connecting); }
  .head[data-status="authenticating"] { --c: var(--status-authenticating); }
  .head[data-status="connected"] { --c: var(--status-connected); }
  .head[data-status="warning"] { --c: var(--status-warning); }
  .head[data-status="error"] { --c: var(--status-error); }
  /* Room goes to the status first: dot, phase, latency and badge never
     shrink; the alias gives way (ellipsis, at most 45%), then the title. */
  .head > * { flex: none; }
  .head .dot { width: var(--size-dot); height: var(--size-dot); border-radius: 50%; background: var(--c); }
  .head .dot.hollow { background: transparent; box-shadow: inset 0 0 0 var(--size-dot-ring) var(--c); }
  .head .dot.busy { animation: pulse var(--motion-loop) var(--ease-standard) infinite; }
  .head .alias {
    flex: 0 1 auto;
    min-width: 0;
    max-width: 45%;
    overflow: hidden;
    color: var(--text-secondary);
    font-weight: 550;
    text-overflow: ellipsis;
  }
  .focused .head .alias { color: var(--text-primary); }
  .head .rtt { font-variant-numeric: tabular-nums; }
  .head .rtt.slow { color: var(--status-warning); }
  .head[data-status="error"] .phase { color: var(--status-error); }
  .head .title { flex: 1 1 0; min-width: 0; overflow: hidden; text-overflow: ellipsis; }
  .head > * + *:not(.alias):not(.zoom)::before { content: "·"; margin-right: var(--space-2); color: var(--text-muted); }
  .head .zoom {
    margin-left: auto;
    padding: 0 var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    color: var(--text-secondary);
    letter-spacing: var(--tracking-caps);
    text-transform: uppercase;
  }
  @keyframes pulse {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--c) 60%, transparent); }
    100% { box-shadow: 0 0 0 4px transparent; }
  }
  /* Cards and bars fit a small pane. */
  .terminal.split .overlay { top: var(--pane-head); overflow: auto; }
  .terminal.split .card {
    min-width: 0;
    max-width: min(var(--size-card), calc(100% - var(--space-5)));
    padding: var(--space-4);
    gap: var(--space-2);
  }
  .terminal.split .card h2 { font-size: var(--text-xl); }
  .terminal.split .ended { left: var(--space-2); right: var(--space-2); bottom: var(--space-2); }
  .muted.narrow,
  .compact { display: none; }
  /* A narrow pane (two side by side at 880 px): shorter wording, no keys. */
  @container (max-width: 520px) {
    .terminal.split .ended .muted-inline { display: none; }
    .terminal.split .card .link kbd,
    .terminal.split .card .actions kbd { display: none; }
    .terminal.split .muted.wide { display: none; }
    .terminal.split .muted.narrow { display: block; }
  }
  /* A narrow pane hides the phase: the dot's colour carries it. */
  @container (max-width: 320px) {
    .head .phase { display: none; }
  }
  /* A short pane (four at 880 px): the card becomes one line. */
  @container (max-height: 260px) {
    .terminal.split .card { display: none; }
    .terminal.split .compact { display: flex; }
  }
  .compact {
    --c: var(--status-idle);
    align-items: center;
    gap: var(--space-2);
    width: calc(100% - var(--space-3));
    max-width: var(--size-card);
    min-width: 0;
    height: var(--space-6);
    padding: 0 var(--space-2) 0 var(--space-3);
    background: var(--bg-overlay);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-raised);
    color: var(--text-secondary);
    font-size: var(--text-sm);
    white-space: nowrap;
  }
  .compact[data-status="connecting"] { --c: var(--status-connecting); }
  .compact[data-status="authenticating"] { --c: var(--status-authenticating); }
  .compact[data-status="connected"] { --c: var(--status-connected); }
  .compact[data-status="warning"] { --c: var(--status-warning); }
  .compact[data-status="error"] { --c: var(--status-error); }
  .compact > * { flex: none; }
  .compact .dot { width: var(--size-dot); height: var(--size-dot); border-radius: 50%; background: var(--c); }
  .compact .dot.hollow { background: transparent; box-shadow: inset 0 0 0 var(--size-dot-ring) var(--c); }
  .compact .dot.busy { animation: pulse var(--motion-loop) var(--ease-standard) infinite; }
  /* The message takes what the actions leave, cut with an ellipsis (the hint is in the tooltip). */
  .compact .message { flex: 1 1 auto; min-width: 3ch; margin: 0; overflow: hidden; color: var(--text-primary); text-overflow: ellipsis; }
  .compact.failure .message { color: var(--status-error); }
  .compact .link { height: var(--space-5); padding: 0 var(--space-1); gap: var(--space-1); }
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
    max-width: var(--size-card);
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
  .ended .dot { width: var(--size-dot); height: var(--size-dot); margin-right: var(--space-1); border-radius: 50%; background: var(--c); }
  .ended .dot.hollow { background: transparent; box-shadow: inset 0 0 0 var(--size-dot-ring) var(--c); }
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
