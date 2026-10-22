// Checks of the recording logic (src/lib/recording.ts) and of the player's
// read-only guarantees, run by `pnpm check`. Node's built-in test runner;
// xterm.js runs headless here (no `open()`, no DOM).
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import xterm from "@xterm/xterm";
import {
  clampSpeed,
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
  const r = reduceRecording(map, 7, 1, stopped("a.cast", "requested"));
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

test("stop reasons other than requested and terminal_closed give a notice", () => {
  assert.equal(stopNotice("requested"), null);
  assert.equal(stopNotice("terminal_closed"), null);
  assert.equal(stopNotice("queue_full")?.message, "Recording stopped: the disk could not keep up");
  assert.equal(stopNotice("write_failed")?.message, "Recording stopped: the file could not be written");
  assert.equal(stopNotice("size_limit")?.message, "Recording stopped: size limit reached (512 MiB)");
  assert.equal(stopNotice("write_failed")?.tone, "error");
  const { notice } = reduceRecording({ 3: { name: "a.cast", session: 1 } }, 3, 1, stopped("a.cast", "size_limit"));
  assert.equal(notice?.code, "recording_size_limit");
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
  assert.equal(formatDuration(42.4), "42 s");
  assert.equal(formatDuration(185), "3 min 05");
  assert.equal(formatDuration(4800), "1 h 20");
  assert.equal(seekTime(0.5, 20), 10);
  assert.equal(seekTime(-1, 20), 0);
  assert.equal(seekTime(2, 20), 20);
  assert.equal(seekTime(0.5, 0), 0);
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
