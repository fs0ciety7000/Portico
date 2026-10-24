<script lang="ts">
  // Playback of a recording in its own xterm.js instance (never a live
  // terminal's). Read-only by construction (`createPlayerTerminal`):
  // `disableStdin`, inert links, window operations off, no addon. Nothing
  // listens to the emulator's data, binary, title or bell events: CSI
  // replies (DSR, DA), mouse reports and focus events have nowhere to go,
  // and the modes a recording sets stay in this instance.
  //
  // Frames arrive already sanitised from Rust (one PlaybackSanitizer for
  // the whole playback); pause, speed, skip and seek are decided there too.
  // Output text is written to the emulator and nowhere else (no log, no
  // notice). Keys (`playerKey`, same as the TUI): Space play/pause (replay
  // when ended) · + / - speed · → skip the silence · ← back 5 s · Home
  // restart · q / Esc close (handled by the dialog).
  import { onMount } from "svelte";
  import { Terminal } from "@xterm/xterm";
  import "@xterm/xterm/css/xterm.css";
  import { asError, formatSize, sentence, type ErrorSummary, type PlaybackFrame, type PlaybackInfo, type Recording } from "../api";
  import { backend } from "../backend";
  import {
    createPlayerTerminal,
    formatClock,
    formatSpeed,
    frameTime,
    MAX_SPEED,
    MIN_SPEED,
    PlayerFeed,
    playerFontSize,
    playerKey,
    seekTime,
    stepSpeed,
  } from "../recording";

  let { recording, themeId }: { recording: Recording; themeId: string } = $props();

  /** Rewind step of ← (seconds). */
  const BACK_STEP = 5;
  /** Line height of the player's emulator (as in `playerOptions`). */
  const LINE_HEIGHT = 1.2;

  let host: HTMLDivElement;
  let screen: HTMLDivElement;
  let term: Terminal | undefined;
  let feed: PlayerFeed | undefined;
  let info: PlaybackInfo | null = $state(null);
  let error: ErrorSummary | null = $state(null);
  /** The file could not be played further (what came before was). */
  let streamError: ErrorSummary | null = $state(null);
  let time = $state(0);
  let seen = $state(0);
  let paused = $state(false);
  let speed = $state(1);
  let ended = $state(false);
  let closed = false;

  const total = $derived(Math.max(recording.duration ?? 0, seen, time));
  const progress = $derived(total > 0 ? Math.min(1, time / total) : 0);

  /** Same rule as the live terminal (`--terminal-min-contrast`). */
  function minContrast(): number {
    const n = Number(getComputedStyle(document.documentElement).getPropertyValue("--terminal-min-contrast").trim());
    return Number.isFinite(n) && n >= 1 ? n : 1;
  }

  function readTheme(): Record<string, string> {
    const css = getComputedStyle(document.documentElement);
    const v = (name: string) => css.getPropertyValue(name).trim();
    const ansi = (i: number) => v(`--ansi-${i}`);
    return {
      background: v("--bg-base"),
      foreground: v("--text-primary"),
      cursor: v("--text-muted"),
      cursorAccent: v("--bg-base"),
      selectionBackground: v("--accent-selection"),
      black: ansi(0), red: ansi(1), green: ansi(2), yellow: ansi(3),
      blue: ansi(4), magenta: ansi(5), cyan: ansi(6), white: ansi(7),
      brightBlack: ansi(8), brightRed: ansi(9), brightGreen: ansi(10), brightYellow: ansi(11),
      brightBlue: ansi(12), brightMagenta: ansi(13), brightCyan: ansi(14), brightWhite: ansi(15),
    };
  }

  /** Font size: the base size, smaller (down to `--text-xs`) when the
   * recorded width or height does not fit the screen (`playerFontSize`);
   * below that, the screen scrolls to keep the cursor row in view. */
  function fontFor(cols: number, rows: number): { family: string; size: number } {
    const css = getComputedStyle(document.documentElement);
    const family = css.getPropertyValue("--font-mono").trim();
    const base = Number.parseFloat(css.getPropertyValue("--text-base"));
    const min = Math.min(base, Number.parseFloat(css.getPropertyValue("--text-xs")));
    const ctx = document.createElement("canvas").getContext("2d");
    if (ctx) ctx.font = `${base}px ${family}`;
    const box = getComputedStyle(screen);
    const padX = Number.parseFloat(box.paddingLeft) + Number.parseFloat(box.paddingRight);
    const padY = Number.parseFloat(box.paddingTop) + Number.parseFloat(box.paddingBottom);
    const size = playerFontSize({
      base,
      min,
      cellW: ctx?.measureText("W").width ?? 0,
      cellH: Math.ceil(base * LINE_HEIGHT),
      cols,
      rows,
      width: screen.clientWidth - padX,
      height: screen.clientHeight - padY,
    });
    return { family, size };
  }

  /** A recording taller than the screen even at the smallest size: keep
   * the cursor row in view (the screen scrolls, like a terminal). */
  let follow = 0;
  function keepCursorInView() {
    if (follow || !term) return;
    follow = requestAnimationFrame(() => {
      follow = 0;
      const rowsEl = host.querySelector<HTMLElement>(".xterm-screen");
      if (!term || !rowsEl || screen.scrollHeight <= screen.clientHeight) return;
      const rowH = rowsEl.clientHeight / Math.max(1, term.rows);
      const top = rowsEl.offsetTop + term.buffer.active.cursorY * rowH;
      if (top < screen.scrollTop) screen.scrollTop = top;
      else if (top + rowH > screen.scrollTop + screen.clientHeight) screen.scrollTop = top + rowH - screen.clientHeight;
    });
  }

  function onFrame(frame: PlaybackFrame) {
    if (closed) return;
    feed?.apply(frame);
    const t = frameTime(frame);
    if (t !== null) {
      time = t;
      seen = Math.max(seen, t);
    }
    if (frame.kind === "reset") ended = false;
    else if (frame.kind === "end") ended = true;
    else if (frame.kind === "error") {
      streamError = frame.error;
      ended = true;
    }
  }

  async function control(c: Parameters<typeof backend.playbackControl>[1]) {
    if (!info) return;
    try {
      const s = await backend.playbackControl(info.id, c);
      paused = s.paused;
      speed = s.speed;
    } catch (e) {
      error = asError(e);
    }
  }

  export function toggle() {
    if (ended) {
      // From the end: play again from the start.
      ended = false;
      void control({ kind: "seek", time: 0 }).then(() => control({ kind: "resume" }));
    } else void control({ kind: paused ? "resume" : "pause" });
  }
  export function faster(dir: 1 | -1) {
    void control({ kind: "speed", value: stepSpeed(speed, dir) });
  }
  export function skip() {
    void control({ kind: "skip" });
  }
  export function seek(to: number) {
    ended = false;
    void control({ kind: "seek", time: Math.max(0, to) });
  }
  export function back() {
    seek(time - BACK_STEP);
  }

  /** Keys of the player; true when handled (Modal's `onkey`). Closing
   * (`q`, Esc) belongs to the dialog. */
  export function key(k: string): boolean {
    switch (playerKey(k)) {
      case "toggle":
        toggle();
        return true;
      case "faster":
        faster(1);
        return true;
      case "slower":
        faster(-1);
        return true;
      case "skip":
        skip();
        return true;
      case "back":
        back();
        return true;
      case "restart":
        seek(0);
        return true;
      default:
        return false;
    }
  }

  function onBar(e: MouseEvent) {
    const bar = e.currentTarget as HTMLElement;
    const box = bar.getBoundingClientRect();
    seek(seekTime((e.clientX - box.left) / box.width, total));
  }

  onMount(() => {
    let id: number | null = null;
    /** Parsed before the playback id was known (frames can overtake the reply). */
    let early = 0;
    // The emulator: created at once with the listed size, resized from the
    // header and from `resize` frames.
    const font = fontFor(recording.width ?? 80, recording.height ?? 24);
    term = createPlayerTerminal(Terminal, {
      fontFamily: font.family,
      fontSize: font.size,
      theme: readTheme(),
      minimumContrastRatio: minContrast(),
      cols: recording.width ?? 80,
      rows: recording.height ?? 24,
    });
    term.open(host);
    // Keys never reach the emulator (it has no input anyway): the dialog
    // handles them. A click into it gives the focus back to the dialog.
    term.attachCustomKeyEventHandler(() => false);
    const cursor = term.onCursorMove(keepCursorInView);
    const refocus = () => host.closest<HTMLElement>("[role=dialog]")?.focus();
    host.addEventListener("focusin", refocus);
    feed = new PlayerFeed(term, (total) => {
      if (id === null) early = total;
      else backend.playbackAck(id, total).catch(() => {});
    });
    void (async () => {
      try {
        const opened = await backend.playRecording(recording.name, 1, onFrame);
        id = opened.id;
        if (closed) return void backend.playbackClose(opened.id).catch(() => {});
        if (early) backend.playbackAck(opened.id, early).catch(() => {});
        info = opened;
        paused = opened.paused;
        speed = opened.speed;
      } catch (e) {
        if (!closed) error = asError(e);
      }
    })();
    return () => {
      closed = true;
      cursor.dispose();
      cancelAnimationFrame(follow);
      feed?.dispose();
      host.removeEventListener("focusin", refocus);
      if (id !== null) void backend.playbackClose(id).catch(() => {});
      term?.dispose();
      term = undefined;
    };
  });

  // The header's size, once known (the listing may not have had it).
  $effect(() => {
    if (info && term) term.resize(Math.max(1, info.cols), Math.max(1, info.rows));
  });

  $effect(() => {
    void themeId;
    if (term) {
      term.options.theme = readTheme();
      term.options.minimumContrastRatio = minContrast();
    }
  });
</script>

<div class="player">
  <div class="screen" class:dimmed={!!error} bind:this={screen}>
    <div class="xterm-host" bind:this={host} aria-label="Recording playback" role="img"></div>
    {#if error}
      <div class="failure" role="alert">
        <strong>{sentence(error.message)}</strong>
        {#if error.hint}<span>{error.hint}</span>{/if}
      </div>
    {:else if !info}
      <div class="loading" aria-busy="true"><span class="spinner"></span> Opening the recording…</div>
    {/if}
  </div>

  {#if streamError}
    <div class="stream-error" role="status">
      <strong>{sentence(streamError.message)}</strong>
      {#if streamError.hint}<span>{streamError.hint}</span>{/if}
    </div>
  {/if}

  <div class="controls">
    <button class="secondary small play" onclick={toggle} disabled={!info} aria-label={ended ? "Replay" : paused ? "Play" : "Pause"}>
      <svg viewBox="0 0 16 16" aria-hidden="true">
        {#if ended}<path d="M3.5 8a4.5 4.5 0 1 0 1.3-3.2M3.5 2.75v2.5H6" />
        {:else if paused}<path d="M5 3.5v9l7-4.5Z" />
        {:else}<path d="M5.5 3.5v9M10.5 3.5v9" />{/if}
      </svg>
      <kbd>Space</kbd>
    </button>
    <div
      class="bar"
      role="slider"
      tabindex="-1"
      aria-label="Position"
      aria-valuemin={0}
      aria-valuemax={Math.round(total)}
      aria-valuenow={Math.round(time)}
      aria-valuetext={`${formatClock(time)} of ${formatClock(total)}`}
      onclick={onBar}
      onkeydown={() => {}}
    >
      <span class="track"><span class="fill" style:transform={`scaleX(${progress})`}></span></span>
    </div>
    <!-- Text, not only the icon: the state is read without decoding it. -->
    {#if info && (ended || paused)}<span class="state">{ended ? "Ended" : "Paused"}</span>{/if}
    <span class="clock mono">{formatClock(time)} / {formatClock(total)}</span>
    <span class="speed">
      <button class="secondary small" onclick={() => faster(-1)} disabled={!info || speed <= MIN_SPEED} aria-label="Slower">−</button>
      <span class="mono value">{formatSpeed(speed)}</span>
      <button class="secondary small" onclick={() => faster(1)} disabled={!info || speed >= MAX_SPEED} aria-label="Faster">+</button>
    </span>
    <button class="secondary small" onclick={back} disabled={!info} aria-label="Back 5 seconds">↺ 5 s <kbd>←</kbd></button>
    <button class="secondary small" onclick={() => seek(0)} disabled={!info} aria-label="Restart">Restart <kbd>Home</kbd></button>
    <button class="secondary small" onclick={skip} disabled={!info || ended}>Skip idle <kbd>→</kbd></button>
  </div>
  <p class="meta">
    <span>{formatSize(recording.size)}</span>
    {#if info}<span class="mono">{info.cols}×{info.rows}</span>{/if}
    {#if ended}
      <span class="keys"><kbd>Space</kbd> replay · <kbd>q</kbd> back to list</span>
    {:else}
      <span class="keys"><kbd>+</kbd> <kbd>−</kbd> speed · <kbd>q</kbd> back to list</span>
    {/if}
  </p>
</div>

<style>
  /* A container: the controls and the key hints follow the dialog's width. */
  .player { display: flex; flex-direction: column; gap: var(--space-3); flex: 1 1 auto; min-height: 0; container-type: inline-size; }
  .screen {
    position: relative;
    flex: 1 1 auto;
    min-height: 0;
    padding: var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-base);
    overflow: auto;
  }
  .xterm-host { width: max-content; min-width: 100%; transition: opacity var(--motion-base) var(--ease-standard); }
  .screen.dimmed .xterm-host { opacity: 0.4; }
  .xterm-host :global(.xterm-viewport) { background: transparent !important; }
  /* Read-only: no text cursor over the screen. */
  .xterm-host :global(.xterm-screen) { cursor: default; }
  .loading,
  .failure {
    position: absolute;
    inset: 0;
    display: grid;
    place-content: center;
    gap: var(--space-1);
    text-align: center;
    color: var(--text-secondary);
    font-size: var(--text-sm);
  }
  .loading { grid-auto-flow: column; align-items: center; gap: var(--space-2); }
  .failure strong { color: var(--status-error); font-weight: 550; font-size: var(--text-base); }
  .stream-error {
    display: grid;
    gap: var(--space-1);
    padding: var(--space-2) var(--space-3);
    border: 1px solid color-mix(in srgb, var(--status-warning) 45%, transparent);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--status-warning) 6%, var(--bg-elevated));
    font-size: var(--text-sm);
  }
  .stream-error strong { color: var(--text-primary); font-weight: 550; }
  .controls {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    min-width: 0;
  }
  .play svg { width: var(--size-icon-sm); height: var(--size-icon-sm); fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; stroke-linejoin: round; }
  .bar {
    display: flex;
    align-items: center;
    flex: 1 1 auto;
    min-width: var(--space-7);
    height: var(--space-5);
    cursor: pointer;
  }
  .track {
    position: relative;
    width: 100%;
    height: var(--space-1);
    border-radius: var(--radius-pill);
    background: var(--bg-elevated);
    overflow: hidden;
  }
  .fill {
    position: absolute;
    inset: 0;
    background: var(--accent);
    transform-origin: left;
    transition: transform var(--motion-fast) linear;
  }
  .bar:hover .track { background: var(--bg-selected); }
  .state { flex: none; color: var(--text-secondary); font-size: var(--text-sm); }
  .clock { flex: none; color: var(--text-secondary); font-size: var(--text-sm); font-variant-numeric: tabular-nums; }
  .speed { display: inline-flex; flex: none; align-items: center; gap: var(--space-1); }
  .speed .value { min-width: 4ch; text-align: center; color: var(--text-primary); font-size: var(--text-sm); font-variant-numeric: tabular-nums; }
  .speed button { padding: 0 var(--space-2); }
  .meta {
    display: flex;
    flex-wrap: wrap;
    gap: var(--space-1) var(--space-3);
    margin: 0;
    color: var(--text-muted);
    font-size: var(--text-xs);
  }
  .meta .mono { color: var(--text-secondary); }
  .meta .keys { margin-left: auto; }
  @container (max-width: 640px) {
    .meta .keys { display: none; }
  }
</style>
