// Session recording in the GUI: pure logic (no DOM, no Svelte, no runtime
// import) so `scripts/test-recording.ts` runs it under Node.
//
// - The REC state reducer: driven by `recording` engine events only (the
//   IPC reply of `recording_start` never turns REC on), plus an explicit
//   resync after a lagged event stream.
// - The player's xterm.js options and construction: a read-only emulator
//   with no reply path (ADR "Enregistrement de sessions, asciicast v2").
// - Playback speed and time formatting, the player's keys and font size.
// - The wording shared with the TUI (consent, notices, invalid file): the
//   same sentences in both UIs, checked by scripts/test-recording.ts.
import type { ErrorSummary, PlaybackFrame, RecordingState, RecordingStopReason } from "./api";
import type { ToastTone } from "./state/workspace.svelte";

// ── REC state ───────────────────────────────────────────────────────────

/** A terminal being recorded. */
export interface ActiveRecording {
  /** File name (the id for the recordings list). */
  name: string;
  session: number;
}

/** Terminal id → recording, for every terminal with `REC` on. */
export type RecMap = Readonly<Record<number, ActiveRecording>>;

/** A notice for a recording that stopped: `ms` is how long it shows. */
export type StopNotice = ErrorSummary & { tone: ToastTone; ms: number };

/** How long a recording that stopped on its own (disk, size) is told. */
export const STOP_NOTICE_MS = 6000;
/** How long "Recording saved" shows (the usual notice duration). */
export const SAVED_NOTICE_MS = 4000;

/** Shared with the TUI: what a user-requested stop says (neutral). */
export const SAVED_HINT = "v from the host list opens Recordings";
export function savedMessage(name: string): string {
  return `Recording saved · ${name}`;
}

/** What the user is told when a recording stops: a neutral "saved" notice
 * after their request, a warning or an error when it stopped on its own,
 * nothing when its terminal closed. */
export function stopNotice(reason: RecordingStopReason, name = ""): StopNotice | null {
  const notice = (n: Omit<StopNotice, "ms">): StopNotice => ({ ...n, ms: STOP_NOTICE_MS });
  switch (reason) {
    case "requested":
      return { code: "recording_saved", message: savedMessage(name), hint: SAVED_HINT, tone: "info", ms: SAVED_NOTICE_MS };
    case "queue_full":
      return notice({
        code: "recording_queue_full",
        message: "Recording stopped: the disk could not keep up",
        hint: "The file is complete up to that point. Record again if you still need it.",
        tone: "warning",
      });
    case "write_failed":
      return notice({
        code: "recording_write_failed",
        message: "Recording stopped: the file could not be written",
        hint: "Check the free disk space; the file keeps what was written before.",
        tone: "error",
      });
    case "size_limit":
      return notice({
        code: "recording_size_limit",
        message: "Recording stopped: size limit reached (512 MiB)",
        hint: "The file is complete up to that point. Record again to continue in a new file.",
        tone: "warning",
      });
    default:
      return null;
  }
}

/**
 * Next REC state after a `recording` event. `Started` turns REC on for the
 * terminal, `Stopped` turns it off (only if it is the same recording: a
 * late `Stopped` never hides a newer one). An entry with no name comes
 * from a resync (its `Started` was lost): any `Stopped` of that terminal
 * clears it, or REC would stay on forever. Never mutates `map`.
 */
export function reduceRecording(
  map: RecMap,
  terminal: number,
  session: number,
  state: RecordingState,
): { map: RecMap; notice: StopNotice | null } {
  if (state.state === "started") {
    return { map: { ...map, [terminal]: { name: state.name, session } }, notice: null };
  }
  const current = map[terminal];
  const next: Record<number, ActiveRecording> = { ...map };
  if (!current || current.name === "" || current.name === state.name) delete next[terminal];
  return { map: next, notice: stopNotice(state.reason, state.name) };
}

/** The engine says `terminal` is not recorded (`recording_not_active` on a
 * stop): REC goes off here too, whatever event was lost. */
export function forgetRecording(map: RecMap, terminal: number): RecMap {
  if (!(terminal in map)) return map;
  const next: Record<number, ActiveRecording> = { ...map };
  delete next[terminal];
  return next;
}

/**
 * Resync after a lagged event stream: `status` is what the engine says for
 * each terminal still open (`recording_status`). A terminal recorded
 * without a known name keeps an empty one; a terminal no longer open or
 * not recorded loses REC.
 */
export function resyncRecording(map: RecMap, status: ReadonlyMap<number, { recording: boolean; session: number }>): RecMap {
  const next: Record<number, ActiveRecording> = {};
  for (const [terminal, s] of status) {
    if (!s.recording) continue;
    next[terminal] = map[terminal] ?? { name: "", session: s.session };
  }
  return next;
}

// ── Wording shared with the TUI ─────────────────────────────────────────

/** Title of the start confirmation; a split tab names the pane. */
export function consentTitle(split: boolean): string {
  return split ? "Record this pane?" : "Record this terminal?";
}

/** Body of the start confirmation, in two parts around the folder (shown
 * in mono between them). `consentBody` joins them. */
export const CONSENT_LEAD =
  "Everything the terminal shows is recorded, including the commands you type (the shell echoes them) and any secret a command prints. Keystrokes themselves are not stored, so a password typed at a prompt that does not echo stays out. Saved to ";
export const CONSENT_TAIL = ", readable only by you.";
export function consentBody(folder: string): string {
  return `${CONSENT_LEAD}${folder}${CONSENT_TAIL}`;
}

/** Confirmation keys: only `y` records, once armed; `Esc` and `n` cancel;
 * every other key (Enter included) does nothing. */
export function confirmKey(key: string, armed: boolean): "record" | "cancel" | null {
  if (key === "Escape" || key === "n") return "cancel";
  if (key === "y") return armed ? "record" : null;
  return null;
}

/** Under the list when an invalid file is selected. */
export const INVALID_RECORDING_TEXT = "Not a valid asciicast v2 recording: it cannot be played, it can be deleted.";
/** The delete confirmation. */
export const DELETE_RECORDING_TEXT = "The file is removed from the recordings folder. This cannot be undone.";
/** Refusal to delete a recording still being written (asked up front). */
export const STILL_WRITING: ErrorSummary = {
  code: "recording_active",
  message: "This recording is still being written",
  hint: "Stop the recording first",
};

/** Delete of `name`: refused up front while it is being written. */
export function deleteRefusal(name: string, active: ReadonlySet<string>): ErrorSummary | null {
  return active.has(name) ? STILL_WRITING : null;
}

const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"] as const;

/** `22 Oct 13:34` in local time, 24 h; the year only when it is not the
 * current one (`3 Jan 2025 09:05`). */
export function formatWhen(ms: number | null, now: Date = new Date()): string {
  if (ms === null || !Number.isFinite(ms)) return "Unknown date";
  const d = new Date(ms);
  const two = (n: number) => String(n).padStart(2, "0");
  const year = d.getFullYear() === now.getFullYear() ? "" : ` ${d.getFullYear()}`;
  return `${d.getDate()} ${MONTHS[d.getMonth()] ?? ""}${year} ${two(d.getHours())}:${two(d.getMinutes())}`;
}

// ── Speed and time ──────────────────────────────────────────────────────

/** Same bounds as `portico_core::recording` (MIN/MAX_PLAYBACK_SPEED). */
export const MIN_SPEED = 0.5;
export const MAX_SPEED = 8;
/** Steps of `+` / `-`. */
export const SPEEDS = [0.5, 1, 2, 4, 8] as const;

export function clampSpeed(speed: number): number {
  return Number.isFinite(speed) ? Math.min(MAX_SPEED, Math.max(MIN_SPEED, speed)) : 1;
}

/** The next step up (`dir` 1) or down (-1) from `speed`, clamped. */
export function stepSpeed(speed: number, dir: 1 | -1): number {
  const s = clampSpeed(speed);
  if (dir > 0) return SPEEDS.find((v) => v > s + 1e-9) ?? MAX_SPEED;
  return [...SPEEDS].reverse().find((v) => v < s - 1e-9) ?? MIN_SPEED;
}

/** `×0.5`, `×1`, `×8`. */
export function formatSpeed(speed: number): string {
  return `×${clampSpeed(speed)}`;
}

/** `0:07`, `12:30`, `1:02:03` (playback clock). */
export function formatClock(seconds: number): string {
  const s = Number.isFinite(seconds) ? Math.max(0, Math.floor(seconds)) : 0;
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = String(s % 60).padStart(2, "0");
  return h ? `${h}:${String(m).padStart(2, "0")}:${sec}` : `${m}:${sec}`;
}

/** `0:42`, `3:05`, `1:20:00` (list column, a clock like the player's). */
export function formatDuration(seconds: number | null): string {
  if (seconds === null || !Number.isFinite(seconds)) return "—";
  return formatClock(Math.round(seconds));
}

/** Seek target for a click at `fraction` of the bar. */
export function seekTime(fraction: number, total: number): number {
  if (!Number.isFinite(fraction) || !Number.isFinite(total) || total <= 0) return 0;
  return Math.min(1, Math.max(0, fraction)) * total;
}

// ── The player's keys and font ──────────────────────────────────────────

export type PlayerKey = "toggle" | "faster" | "slower" | "skip" | "back" | "restart" | "close";

/** Keys of the player (same as the TUI): Space play/pause (replay when
 * ended) · `+`/`-` speed · → skip idle · ← back 5 s · Home restart ·
 * `q`/Esc close. */
export function playerKey(key: string): PlayerKey | null {
  switch (key) {
    case " ":
      return "toggle";
    case "+":
    case "=":
      return "faster";
    case "-":
    case "_":
      return "slower";
    case "ArrowRight":
      return "skip";
    case "ArrowLeft":
      return "back";
    case "Home":
      return "restart";
    case "q":
    case "Escape":
      return "close";
    default:
      return null;
  }
}

/** Hint of an ended playback (shared with the TUI). */
export const ENDED_HINT = "Space replay · q back to list";

/** What the player's font size depends on (px). */
export interface FontRoom {
  base: number;
  min: number;
  /** Cell width and height at `base` (height includes the line height). */
  cellW: number;
  cellH: number;
  cols: number;
  rows: number;
  width: number;
  height: number;
}

/**
 * The player's font size: `base` when the recorded size fits, smaller so
 * both the width and the height fit, never below `min` (beyond that the
 * screen scrolls to keep the cursor row in view). Unknown room: `base`.
 */
export function playerFontSize(r: FontRoom): number {
  const fit = (room: number, cells: number, cell: number) =>
    room > 0 && cells > 0 && cell > 0 && cells * cell > room ? (r.base * room) / (cells * cell) : r.base;
  const size = Math.min(fit(r.width, r.cols, r.cellW), fit(r.height, r.rows, r.cellH));
  return Number.isFinite(size) ? Math.max(r.min, Math.min(r.base, Math.floor(size))) : r.base;
}

// ── The player's emulator ───────────────────────────────────────────────

/** Every xterm.js window operation (CSI t) off: a recording can neither
 * move, resize, raise nor query the window. */
export const PLAYER_WINDOW_OPTIONS = {
  restoreWin: false,
  minimizeWin: false,
  setWinPosition: false,
  setWinSizePixels: false,
  raiseWin: false,
  lowerWin: false,
  refreshWin: false,
  setWinSizeChars: false,
  maximizeWin: false,
  fullscreenWin: false,
  getWinState: false,
  getWinPosition: false,
  getWinSizePixels: false,
  getScreenSizePixels: false,
  getCellSizePixels: false,
  getWinSizeChars: false,
  getScreenSizeChars: false,
  getIconTitle: false,
  getWinTitle: false,
  pushTitle: false,
  popTitle: false,
  setWinLines: false,
} as const;

/** What the player's options depend on (read from the tokens). */
export interface PlayerLook {
  fontFamily: string;
  fontSize: number;
  // xterm.js ITheme, kept opaque here.
  theme: Record<string, string>;
  /** `--terminal-min-contrast` (1 = off); absent = off. */
  minimumContrastRatio?: number;
  cols: number;
  rows: number;
}

/**
 * Options of the player's xterm.js instance. Read-only by construction:
 * `disableStdin` drops every reply the emulator would send (DSR, DA,
 * mouse reports, focus events), links do nothing, every window option is
 * off, no proposed API.
 */
export function playerOptions(look: PlayerLook) {
  return {
    fontFamily: look.fontFamily,
    fontSize: look.fontSize,
    lineHeight: 1.2,
    cols: Math.max(1, Math.min(1000, Math.floor(look.cols) || 80)),
    rows: Math.max(1, Math.min(1000, Math.floor(look.rows) || 24)),
    theme: look.theme,
    minimumContrastRatio: Math.max(1, Math.min(21, look.minimumContrastRatio ?? 1)),
    disableStdin: true,
    cursorBlink: false,
    cursorStyle: "bar" as const,
    cursorInactiveStyle: "none" as const,
    scrollback: 1000,
    allowProposedApi: false,
    allowTransparency: false,
    ignoreBracketedPasteMode: true,
    screenReaderMode: false,
    linkHandler: { activate: () => {}, allowNonHttpProtocols: false },
    windowOptions: { ...PLAYER_WINDOW_OPTIONS },
  };
}

export type PlayerOptions = ReturnType<typeof playerOptions>;

/** The parts of an xterm.js `Terminal` the player uses. Nothing here reads
 * input: there is no `onData`, `onBinary`, `onTitleChange` or `onBell`. */
export interface PlayerTerminal {
  write(data: string, callback?: () => void): void;
  resize(cols: number, rows: number): void;
  reset(): void;
  dispose(): void;
}

/**
 * Creates the player's emulator: a new instance every time (never a live
 * terminal's), with `playerOptions`, no addon and no event handler. The
 * caller only opens it on its element.
 */
export function createPlayerTerminal<T extends PlayerTerminal>(Ctor: new (options: PlayerOptions) => T, look: PlayerLook): T {
  return new Ctor(playerOptions(look));
}

/** Flow control of one playback: acknowledges parsed bytes in batches. */
export const PLAYER_ACK_EVERY = 64 * 1024;

/**
 * Applies frames to the player's emulator and acknowledges the bytes it
 * has parsed (`ack(total)`). The output was sanitised in Rust; it is never
 * logged or shown anywhere else. Returns the playback time after `frame`.
 */
export class PlayerFeed {
  parsed = 0;
  acked = 0;
  #disposed = false;
  // Plain fields: Node runs this module with type stripping only (no
  // parameter properties).
  readonly term: PlayerTerminal;
  readonly ack: (total: number) => void;

  constructor(term: PlayerTerminal, ack: (total: number) => void) {
    this.term = term;
    this.ack = ack;
  }

  apply(frame: PlaybackFrame): void {
    if (this.#disposed) return;
    switch (frame.kind) {
      case "output":
        this.term.write(frame.data, () => {
          if (this.#disposed) return;
          this.parsed += frame.size;
          this.#ack(false);
          queueMicrotask(() => this.#ack(true));
        });
        return;
      case "resize":
        this.term.resize(Math.max(1, Math.min(1000, frame.cols)), Math.max(1, Math.min(1000, frame.rows)));
        return;
      case "reset":
        this.term.reset();
        return;
      default:
        return;
    }
  }

  #ack(force: boolean) {
    if (this.parsed === this.acked) return;
    if (force || this.parsed - this.acked >= PLAYER_ACK_EVERY) {
      this.acked = this.parsed;
      this.ack(this.parsed);
    }
  }

  dispose() {
    this.#disposed = true;
  }
}

/** Time a frame stands for (null for an error). */
export function frameTime(frame: PlaybackFrame): number | null {
  return frame.kind === "error" ? null : frame.time;
}
