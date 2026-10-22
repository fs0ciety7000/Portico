// Checks of the recording logic (src/lib/recording.ts) and of the player's
// read-only guarantees, run by `pnpm check`. Node's built-in test runner;
// xterm.js runs headless here (no `open()`, no DOM).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import xterm from "@xterm/xterm";
import {
  CONSENT_LEAD,
  CONSENT_TAIL,
  DELETE_RECORDING_TEXT,
  ENDED_HINT,
  INVALID_RECORDING_TEXT,
  SAVED_HINT,
  SAVED_NOTICE_MS,
  STILL_WRITING,
  STOP_NOTICE_MS,
  clampSpeed,
  confirmKey,
  consentBody,
  consentTitle,
  deleteRefusal,
  forgetRecording,
  formatWhen,
  playerFontSize,
  playerKey,
  createPlayerTerminal,
  formatClock,
  formatDuration,
  formatSpeed,
  MAX_SPEED,
  MIN_SPEED,
  PLAYER_WINDOW_OPTIONS,
  PlayerFeed,
  playerOptions,
  reduceRecording,
  resyncRecording,
  seekTime,
  stepSpeed,
  stopNotice,
  type PlayerLook,
  type RecMap,
} from "../src/lib/recording.ts";
import type { PlaybackFrame, RecordingState } from "../src/lib/api.ts";

const { Terminal } = xterm as unknown as typeof import("@xterm/xterm");

const look: PlayerLook = { fontFamily: "monospace", fontSize: 13, theme: {}, cols: 80, rows: 24 };
const started = (name: string): RecordingState => ({ state: "started", name, path: `/x/${name}` });
const stopped = (name: string, reason: Extract<RecordingState, { state: "stopped" }>["reason"]): RecordingState => ({
  state: "stopped",
  reason,
  name,
  path: `/x/${name}`,
  bytes: 10,
  duration: { secs: 1, nanos: 0 },
});

// ── REC state ───────────────────────────────────────────────────────────

test("REC turns on with started and off with stopped, from events only", () => {
  let map: RecMap = {};
  ({ map } = reduceRecording(map, 7, 1, started("a.cast")));
  assert.deepEqual(map, { 7: { name: "a.cast", session: 1 } });
  // Another terminal is independent.
  ({ map } = reduceRecording(map, 8, 1, started("b.cast")));
  const r = reduceRecording(map, 7, 1, stopped("a.cast", "terminal_closed"));
  assert.deepEqual(r.map, { 8: { name: "b.cast", session: 1 } });
  assert.equal(r.notice, null);
  // The reducer never mutates its input.
  assert.ok(7 in map);
});

test("a late stopped never hides a newer recording of the same terminal", () => {
  let map: RecMap = {};
  ({ map } = reduceRecording(map, 7, 1, started("new.cast")));
  const r = reduceRecording(map, 7, 1, stopped("old.cast", "terminal_closed"));
  assert.deepEqual(r.map, { 7: { name: "new.cast", session: 1 } });
});

test("a user-requested stop says where the file is, neutral, for 4 s", () => {
  const n = stopNotice("requested", "alpha-20261022-133400.cast");
  assert.deepEqual(n, {
    code: "recording_saved",
    message: "Recording saved · alpha-20261022-133400.cast",
    hint: "v from the host list opens Recordings",
    tone: "info",
    ms: 4000,
  });
  assert.equal(SAVED_HINT, "v from the host list opens Recordings");
  assert.equal(SAVED_NOTICE_MS, 4000);
  const { notice } = reduceRecording({ 3: { name: "a.cast", session: 1 } }, 3, 1, stopped("a.cast", "requested"));
  assert.equal(notice?.message, "Recording saved · a.cast");
});

test("stop reasons other than requested and terminal_closed give a notice", () => {
  assert.equal(stopNotice("terminal_closed"), null);
  assert.equal(stopNotice("queue_full")?.ms, STOP_NOTICE_MS);
  assert.equal(stopNotice("queue_full")?.message, "Recording stopped: the disk could not keep up");
  assert.equal(stopNotice("write_failed")?.message, "Recording stopped: the file could not be written");
  assert.equal(stopNotice("size_limit")?.message, "Recording stopped: size limit reached (512 MiB)");
  assert.equal(stopNotice("write_failed")?.tone, "error");
  const { notice } = reduceRecording({ 3: { name: "a.cast", session: 1 } }, 3, 1, stopped("a.cast", "size_limit"));
  assert.equal(notice?.code, "recording_size_limit");
});

test("a stopped after a resync clears an unnamed REC", () => {
  // The `started` event was lost: the resync only knows "recording".
  let map = resyncRecording({}, new Map([[5, { recording: true, session: 2 }]]));
  assert.deepEqual(map, { 5: { name: "", session: 2 } });
  // Its `stopped` carries the real name: REC goes off all the same.
  ({ map } = reduceRecording(map, 5, 2, stopped("b-20261022.cast", "requested")));
  assert.deepEqual(map, {});
  // A named entry still ignores a late `stopped` of an older file.
  ({ map } = reduceRecording(map, 5, 2, started("new.cast")));
  ({ map } = reduceRecording(map, 5, 2, stopped("old.cast", "requested")));
  assert.deepEqual(map, { 5: { name: "new.cast", session: 2 } });
});

test("a stop the engine answers recording_not_active drops the local REC", () => {
  const map: RecMap = { 5: { name: "", session: 2 }, 6: { name: "c.cast", session: 2 } };
  assert.deepEqual(forgetRecording(map, 5), { 6: { name: "c.cast", session: 2 } });
  assert.ok(5 in map, "never mutates its input");
  assert.equal(forgetRecording(map, 9), map);
  // The state module uses it on that error code.
  const src = readFileSync(new URL("../src/lib/state/recordings.svelte.ts", import.meta.url), "utf8");
  assert.match(src, /recording_not_active"\) this\.active = forgetRecording\(this\.active, terminal\)/);
});

test("resync after a lag keeps what the engine confirms, nothing else", () => {
  const map: RecMap = { 1: { name: "a.cast", session: 9 }, 2: { name: "b.cast", session: 9 } };
  const status = new Map([
    [1, { recording: true, session: 9 }],
    [2, { recording: false, session: 9 }],
    [3, { recording: true, session: 4 }],
  ]);
  assert.deepEqual(resyncRecording(map, status), { 1: { name: "a.cast", session: 9 }, 3: { name: "", session: 4 } });
});

// ── Speed and time ──────────────────────────────────────────────────────

test("speed is clamped to 0.5–8 and steps through the presets", () => {
  assert.equal(clampSpeed(100), MAX_SPEED);
  assert.equal(clampSpeed(0.01), MIN_SPEED);
  assert.equal(clampSpeed(Number.NaN), 1);
  assert.equal(clampSpeed(Number.POSITIVE_INFINITY), 1);
  assert.equal(stepSpeed(1, 1), 2);
  assert.equal(stepSpeed(8, 1), 8);
  assert.equal(stepSpeed(0.5, -1), 0.5);
  assert.equal(stepSpeed(3, -1), 2);
  assert.equal(stepSpeed(3, 1), 4);
  assert.equal(formatSpeed(0.5), "×0.5");
  assert.equal(formatSpeed(20), "×8");
});

test("clock, duration and seek formatting", () => {
  assert.equal(formatClock(0), "0:00");
  assert.equal(formatClock(7.9), "0:07");
  assert.equal(formatClock(750), "12:30");
  assert.equal(formatClock(3723), "1:02:03");
  assert.equal(formatClock(Number.NaN), "0:00");
  assert.equal(formatDuration(null), "—");
  assert.equal(formatDuration(42.4), "0:42");
  assert.equal(formatDuration(185), "3:05");
  assert.equal(formatDuration(4800), "1:20:00");
  assert.equal(seekTime(0.5, 20), 10);
  assert.equal(seekTime(-1, 20), 0);
  assert.equal(seekTime(2, 20), 20);
  assert.equal(seekTime(0.5, 0), 0);
});

// ── Wording shared with the TUI ─────────────────────────────────────────

test("consent: shared title and body, word for word", () => {
  assert.equal(consentTitle(false), "Record this terminal?");
  assert.equal(consentTitle(true), "Record this pane?");
  assert.equal(
    consentBody("~/.local/share/portico/recordings"),
    "Everything the terminal shows is recorded, including the commands you type (the shell echoes them) and any secret a command prints. Keystrokes themselves are not stored, so a password typed at a prompt that does not echo stays out. Saved to ~/.local/share/portico/recordings, readable only by you.",
  );
  assert.equal(`${CONSENT_LEAD}X${CONSENT_TAIL}`, consentBody("X"));
});

test("confirmation keys: only y records, once armed; Esc and n cancel; Enter does nothing", () => {
  assert.equal(confirmKey("y", true), "record");
  assert.equal(confirmKey("y", false), null);
  assert.equal(confirmKey("Enter", true), null);
  assert.equal(confirmKey(" ", true), null);
  assert.equal(confirmKey("Y", true), null);
  assert.equal(confirmKey("n", true), "cancel");
  assert.equal(confirmKey("Escape", false), "cancel");
  const src = readFileSync(new URL("../src/lib/components/RecordConfirm.svelte", import.meta.url), "utf8");
  assert.ok(src.includes("Record <kbd>y</kbd>"), "button label Record y");
  assert.ok(!src.includes("data-autofocus"), "the Record button is not focused (Enter never records)");
  assert.ok(!src.includes('"Enter" ?'), "no Enter answer");
});

test("list wording: invalid file, delete confirmation, still being written", () => {
  assert.equal(INVALID_RECORDING_TEXT, "Not a valid asciicast v2 recording: it cannot be played, it can be deleted.");
  assert.equal(DELETE_RECORDING_TEXT, "The file is removed from the recordings folder. This cannot be undone.");
  assert.deepEqual(deleteRefusal("a.cast", new Set(["a.cast"])), STILL_WRITING);
  assert.equal(STILL_WRITING.message, "This recording is still being written");
  assert.equal(STILL_WRITING.hint, "Stop the recording first");
  assert.equal(deleteRefusal("b.cast", new Set(["a.cast"])), null);
});

test("dates: local, 24 h, the year only when it is not this year", () => {
  const now = new Date(2026, 9, 22, 18, 0);
  assert.equal(formatWhen(new Date(2026, 9, 22, 13, 34).getTime(), now), "22 Oct 13:34");
  assert.equal(formatWhen(new Date(2026, 0, 3, 9, 5).getTime(), now), "3 Jan 09:05");
  assert.equal(formatWhen(new Date(2025, 11, 31, 23, 59).getTime(), now), "31 Dec 2025 23:59");
  assert.equal(formatWhen(null, now), "Unknown date");
});

// ── The player's keys and font ──────────────────────────────────────────

test("player keys match the TUI; q and Esc close", () => {
  assert.equal(playerKey(" "), "toggle");
  assert.equal(playerKey("+"), "faster");
  assert.equal(playerKey("-"), "slower");
  assert.equal(playerKey("ArrowRight"), "skip");
  assert.equal(playerKey("ArrowLeft"), "back");
  assert.equal(playerKey("Home"), "restart");
  assert.equal(playerKey("q"), "close");
  assert.equal(playerKey("Escape"), "close");
  assert.equal(playerKey("Enter"), null);
  assert.equal(ENDED_HINT, "Space replay · q back to list");
});

test("player font: the smaller of the width and height fits, floored at --text-xs", () => {
  const room = { base: 13, min: 11, cellW: 8, cellH: 16, cols: 80, rows: 24 };
  // Fits: base size.
  assert.equal(playerFontSize({ ...room, width: 800, height: 500 }), 13);
  // Too narrow: 700 / (80 × 8) × 13 = 14.2 → fits; 600 px → floor(12.19).
  assert.equal(playerFontSize({ ...room, width: 600, height: 500 }), 12);
  // Too short (the old width-only rule kept 13 here): 24 rows × 16 > 340.
  assert.equal(playerFontSize({ ...room, width: 800, height: 340 }), 11);
  assert.equal(playerFontSize({ ...room, width: 800, height: 368 }), 12);
  // Far too small: never below the floor (the screen scrolls instead).
  assert.equal(playerFontSize({ ...room, width: 200, height: 100 }), 11);
  // Unknown room: base.
  assert.equal(playerFontSize({ ...room, width: 0, height: 0 }), 13);
});

// ── The player's emulator ───────────────────────────────────────────────

test("player options: read-only, inert links, every window option off", () => {
  const o = playerOptions(look);
  assert.equal(o.disableStdin, true);
  assert.equal(o.allowProposedApi, false);
  for (const [key, value] of Object.entries(o.windowOptions)) assert.equal(value, false, key);
  assert.deepEqual(Object.keys(o.windowOptions).sort(), Object.keys(PLAYER_WINDOW_OPTIONS).sort());
  assert.equal(Object.keys(PLAYER_WINDOW_OPTIONS).length, 22);
  assert.equal(o.linkHandler.allowNonHttpProtocols, false);
  // A link activation does nothing (no window.open, no IPC).
  assert.equal(o.linkHandler.activate(), undefined);
  // Sizes come from the header, within the core's bounds.
  assert.deepEqual([playerOptions({ ...look, cols: 5000, rows: 0 }).cols, playerOptions({ ...look, cols: 5000, rows: 0 }).rows], [1000, 24]);
});

test("the player is a new instance with no handler and no addon", () => {
  const calls: string[] = [];
  class Spy {
    options: unknown;
    constructor(options: unknown) {
      this.options = options;
      calls.push("new");
    }
    onData() { calls.push("onData"); }
    onBinary() { calls.push("onBinary"); }
    onTitleChange() { calls.push("onTitleChange"); }
    onBell() { calls.push("onBell"); }
    loadAddon() { calls.push("loadAddon"); }
    write() {}
    resize() {}
    reset() {}
    dispose() {}
  }
  const a = createPlayerTerminal(Spy, look);
  const b = createPlayerTerminal(Spy, look);
  assert.notEqual(a, b);
  assert.deepEqual(calls, ["new", "new"]);
  assert.equal((a.options as { disableStdin: boolean }).disableStdin, true);
});

test("PlayerView wires nothing to the emulator's input, title or bell", () => {
  const src = readFileSync(new URL("../src/lib/components/PlayerView.svelte", import.meta.url), "utf8");
  // Comments mention the event names: check the code only.
  const code = src.replace(/<!--[\s\S]*?-->/g, "").replace(/\/\/.*$/gm, "").replace(/\/\*[\s\S]*?\*\//g, "");
  for (const forbidden of [".onData", ".onBinary", ".onTitleChange", ".onBell", "loadAddon", "addon-", "terminalInput", "clipboard", "console."]) {
    assert.ok(!code.includes(forbidden), `PlayerView must not use ${forbidden}`);
  }
  assert.ok(code.includes("createPlayerTerminal("), "the player is built by createPlayerTerminal");
  assert.ok(!/new\s+Terminal\s*\(/.test(code), "no other Terminal construction");
});

/** Plays frames into a real (headless) xterm.js player through the same
 * feed the view uses; every backend call is recorded. */
async function playHeadless(frames: PlaybackFrame[]) {
  const ipc: string[] = [];
  const backendSpy = new Proxy(
    {},
    { get: (_t, name) => (...args: unknown[]) => (ipc.push(`${String(name)}(${args.map(String).join(",")})`), Promise.resolve()) },
  ) as { playbackAck: (id: number, total: number) => Promise<void> };
  const term = createPlayerTerminal(Terminal, look);
  const feed = new PlayerFeed(term, (total) => void backendSpy.playbackAck(1, total));
  for (const f of frames) feed.apply(f);
  await new Promise<void>((resolve) => term.write("", resolve));
  await new Promise((r) => setTimeout(r, 20));
  return { term, ipc, feed };
}

test("DSR and DA queries in a recording produce no IPC write", async () => {
  const queries = "\x1b[6n\x1b[c\x1b[>c\x1b[5n";
  // Control: a normal emulator answers them (so the test means something).
  const live = new Terminal({ allowProposedApi: false });
  const replies: string[] = [];
  live.onData((d) => replies.push(d));
  await new Promise<void>((resolve) => live.write(queries, resolve));
  assert.ok(replies.length >= 2, "a live terminal replies to DSR / DA");

  const { term, ipc } = await playHeadless([
    { kind: "output", data: "hello ", size: 6, time: 0 },
    { kind: "output", data: queries, size: queries.length, time: 0.1 },
  ]);
  // Only acknowledgements went out: no input, no reply.
  assert.ok(ipc.every((c) => c.startsWith("playbackAck(")), ipc.join(" "));
  assert.ok(!ipc.some((c) => c.startsWith("terminalInput")));
  // Even a listener attached afterwards hears nothing: disableStdin drops replies.
  const heard: string[] = [];
  term.onData((d) => heard.push(d));
  term.onBinary((d) => heard.push(d));
  await new Promise<void>((resolve) => term.write(queries, resolve));
  assert.deepEqual(heard, []);
  term.dispose();
  live.dispose();
});

test("modes set by a recording stay inside the player", async () => {
  const live = new Terminal({ allowProposedApi: false });
  const { term } = await playHeadless([
    { kind: "output", data: "\x1b[?1000h\x1b[?1006h\x1b[?2004h\x1b[?1004h", size: 24, time: 0 },
  ]);
  assert.equal(term.modes.mouseTrackingMode, "vt200");
  assert.equal(term.modes.bracketedPasteMode, true);
  assert.equal(term.modes.sendFocusMode, true);
  assert.equal(live.modes.mouseTrackingMode, "none");
  assert.equal(live.modes.bracketedPasteMode, false);
  assert.equal(live.modes.sendFocusMode, false);
  term.dispose();
  live.dispose();
});

test("the feed acknowledges parsed bytes, resizes and resets", async () => {
  const big = "x".repeat(70 * 1024);
  const { term, ipc, feed } = await playHeadless([
    { kind: "output", data: big, size: big.length, time: 0 },
    { kind: "resize", cols: 5000, rows: 30, time: 1 },
  ]);
  assert.equal(feed.parsed, big.length);
  assert.equal(ipc.at(-1), `playbackAck(1,${big.length})`);
  assert.equal(term.cols, 1000);
  assert.equal(term.rows, 30);
  feed.apply({ kind: "reset", time: 0 });
  feed.dispose();
  // After dispose, nothing is applied or acknowledged.
  feed.apply({ kind: "output", data: "late", size: 4, time: 2 });
  await new Promise((r) => setTimeout(r, 20));
  assert.equal(feed.parsed, big.length);
  term.dispose();
});

// ── Source checks of the view (no DOM here) ─────────────────────────────

const source = (path: string) => readFileSync(new URL(`../src/${path}`, import.meta.url), "utf8");

test("a paste only reaches the active pane, like keys", () => {
  const src = source("lib/components/TerminalView.svelte");
  const handler = src.slice(src.indexOf("const onPaste"), src.indexOf('host.addEventListener("paste"'));
  assert.match(handler, /if \(!active\) return;\s*const text/);
});

test("no hard-coded selection colour or font fallback; tokens instead", () => {
  for (const path of ["App.svelte", "lib/components/TerminalView.svelte", "lib/components/PlayerView.svelte"]) {
    const src = source(path);
    assert.ok(!/\|\|\s*13\b/.test(src), `${path}: no "|| 13" fallback`);
    assert.ok(!/\}55`/.test(src), `${path}: no alpha suffix on the accent`);
  }
  assert.match(source("lib/components/PlayerView.svelte"), /v\("--accent-selection"\)/);
  assert.match(source("lib/components/TerminalView.svelte"), /v\("--accent-selection"\)/);
  const tokens = source("lib/styles/tokens.css");
  assert.match(tokens, /--size-bar: 2px;/);
  assert.match(tokens, /--accent-selection: #[0-9a-f]{8};/);
  // One danger button style, in base.css.
  for (const path of ["lib/components/RecordingsDialog.svelte", "lib/components/SnippetsDialog.svelte", "lib/components/FilesPanel.svelte"]) {
    assert.ok(!source(path).includes(".modal button.danger"), path);
  }
  assert.match(source("lib/styles/base.css"), /\.modal button\.danger/);
  assert.ok(!source("lib/state/recordings.svelte.ts").includes("6000"));
});

test("REC is a live region only in the status bar", () => {
  assert.match(source("lib/components/RecBadge.svelte"), /role = "img"/);
  const app = source("App.svelte");
  assert.equal((app.match(/<RecBadge role="status"/g) ?? []).length, 2);
  assert.match(source("lib/components/RecordingsDialog.svelte"), /<RecBadge compact role="img"/);
});
