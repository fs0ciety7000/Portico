// Demo stand-in for session recording (browser preview and design captures
// only, never in the desktop app): REC toggling on demo terminals, a list
// with a fake (invalid) third-party file, and a playable demo cast streamed
// like the Rust side does (pause, speed, skip, seek, end).
//
// In a demo terminal, `flood` simulates a burst the disk cannot follow: a
// recording of that terminal stops with `queue_full` (the stop notice).
import type { EngineEvent, ErrorSummary, PlaybackControl, PlaybackFrame, Recording } from "./api";
import type { Backend } from "./backend";
import { clampSpeed } from "./recording";

interface Deps {
  emit: (e: EngineEvent) => void;
  /** Alias and session of an open demo terminal. */
  terminal: (id: number) => { alias: string; session: number } | undefined;
}

const FOLDER = "~/.local/share/portico/recordings/";
const ABS = "/home/demo/.local/share/portico/recordings/";
const IDLE_CAP = 2;
/** Same rule as `portico_core::recording::validate_name`. */
const NAME = /^(?!\.)(?!.*\.\.)[A-Za-z0-9._-]+\.cast$/;

const P = "\x1b[38;5;108mdeploy@web-01\x1b[0m:\x1b[38;5;110m~/app\x1b[0m$ ";
type Ev = [number, "o" | "r", string];
/** A short deploy check on web-01 (with the DSR / DA queries shells send:
 * the player has no reply path). */
const DEMO_CAST: Ev[] = [
  [0.0, "o", "\x1b[6n\x1b[c"],
  [0.05, "o", `Last login: Wed Oct 22 09:12:40 2026 from 10.0.0.2\r\n${P}`],
  [1.1, "o", "g"], [1.18, "o", "i"], [1.26, "o", "t"], [1.34, "o", " "], [1.42, "o", "l"], [1.5, "o", "o"], [1.58, "o", "g"],
  [1.66, "o", " "], [1.74, "o", "-"], [1.82, "o", "3"], [1.9, "o", " "], [1.98, "o", "-"], [2.06, "o", "-"], [2.14, "o", "o"],
  [2.22, "o", "n"], [2.3, "o", "e"], [2.38, "o", "l"], [2.46, "o", "i"], [2.54, "o", "n"], [2.62, "o", "e"],
  [3.1, "o", "\r\n\x1b[33m9f2c1ab\x1b[0m (\x1b[1;36mHEAD -> \x1b[1;32mmain\x1b[0m) Pin the TLS ciphers of the edge proxy\r\n\x1b[33m41d07e3\x1b[0m Retry the health check twice before failing\r\n\x1b[33mc0ffee1\x1b[0m Add the deploy lock\r\n" + P],
  [4.4, "o", "./deploy.sh --check\r\n"],
  [4.9, "o", "\x1b[2m[1/4]\x1b[0m Resolving release \x1b[1m2026.10.22-1\x1b[0m … \x1b[32mok\x1b[0m\r\n"],
  [5.6, "o", "\x1b[2m[2/4]\x1b[0m Checking disk space on /srv … \x1b[32m44G free\x1b[0m\r\n"],
  [6.5, "o", "\x1b[2m[3/4]\x1b[0m Health of web-01, web-02 … \x1b[32m200 200\x1b[0m\r\n"],
  [12.5, "o", "\x1b[2m[4/4]\x1b[0m Migrations pending … \x1b[33m1 (add_index_orders_created_at)\x1b[0m\r\n"],
  [13.1, "o", "\r\n\x1b[1;32m✓\x1b[0m Ready to deploy. Run \x1b[1m./deploy.sh\x1b[0m to roll out.\r\n" + P],
  [14.2, "r", "100x28"],
  [14.6, "o", "uptime\r\n 09:13:02 up 41 days,  3:12,  1 user,  load average: 0.08, 0.11, 0.09\r\n" + P],
  [16.0, "o", "exit\r\nlogout\r\n"],
];
const DEMO_DURATION = 16.0;

const now = Date.UTC(2026, 9, 22, 9, 13);
const baseList: Recording[] = [
  { name: "web-01-20261022T091240Z.cast", alias: "web-01", started: now - 60_000, size: 3_412, duration: DEMO_DURATION, width: 92, height: 26, valid: true },
  { name: "db-prod-20261021T163005Z.cast", alias: "db-prod", started: now - 17 * 3600_000, size: 1_874_321, duration: 1_284.6, width: 132, height: 40, valid: true },
  { name: "bastion-20261019T080112Z.cast", alias: "bastion", started: now - 3 * 86_400_000, size: 92_160, duration: 312.2, width: 120, height: 34, valid: true },
  // A third-party file whose header is not asciicast v2: its title is
  // whatever it says, so the file name is always shown beside it.
  { name: "imported-session.cast", alias: "imported-session", started: null, size: 18_204, duration: null, width: null, height: null, valid: false },
];

function invalidName(name: string): ErrorSummary {
  return { code: "recording_invalid_name", message: `"${name.slice(0, 64)}" is not a recording name`, hint: "Pick a recording from the list: only names ending in .cast are accepted." };
}

function stamp(at: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${at.getUTCFullYear()}${p(at.getUTCMonth() + 1)}${p(at.getUTCDate())}T${p(at.getUTCHours())}${p(at.getUTCMinutes())}${p(at.getUTCSeconds())}Z`;
}

export function demoRecording(deps: Deps) {
  const list: Recording[] = [...baseList];
  /** Terminal → recording in progress. */
  const active = new Map<number, { name: string; session: number; started: number; bytes: number }>();
  let nextPlayback = 1;
  const playbacks = new Map<number, { control: (c: PlaybackControl) => { paused: boolean; speed: number }; close: () => void }>();

  function stop(terminal: number, reason: "requested" | "terminal_closed" | "queue_full") {
    const rec = active.get(terminal);
    if (!rec) return;
    active.delete(terminal);
    const secs = (Date.now() - rec.started) / 1000;
    const entry = list.find((r) => r.name === rec.name);
    if (entry) {
      entry.duration = secs;
      entry.size = rec.bytes;
    }
    deps.emit({
      type: "recording",
      terminal,
      session: rec.session,
      state: { state: "stopped", reason, name: rec.name, path: ABS + rec.name, bytes: rec.bytes, duration: { secs: Math.floor(secs), nanos: 0 } },
    });
  }

  function play(name: string, speed0: number, onFrame: (f: PlaybackFrame) => void) {
    const id = nextPlayback++;
    const events = name === baseList[0]?.name ? DEMO_CAST : DEMO_CAST.slice(0, 4);
    let speed = clampSpeed(speed0);
    let paused = false;
    let closed = false;
    let index = 0;
    let pos = 0;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let ended = false;

    const send = (e: Ev) => {
      pos = e[0];
      if (e[1] === "o") onFrame({ kind: "output", data: e[2], size: new TextEncoder().encode(e[2]).length, time: e[0] });
      else {
        const [cols = 80, rows = 24] = e[2].split("x").map(Number);
        onFrame({ kind: "resize", cols, rows, time: e[0] });
      }
    };
    const schedule = () => {
      clearTimeout(timer);
      if (closed || paused || ended) return;
      const next = events[index];
      if (!next) {
        ended = true;
        onFrame({ kind: "end", time: pos });
        return;
      }
      const gap = Math.min(IDLE_CAP, Math.max(0, next[0] - pos)) / speed;
      timer = setTimeout(() => {
        index++;
        send(next);
        schedule();
      }, gap * 1000);
    };
    const seek = (t: number) => {
      clearTimeout(timer);
      ended = false;
      if (t < pos) {
        onFrame({ kind: "reset", time: 0 });
        index = 0;
        pos = 0;
      }
      let batch = "";
      while (index < events.length && (events[index]?.[0] ?? Infinity) <= t) {
        const e = events[index++];
        if (!e) break;
        if (e[1] === "o") batch += e[2];
        else {
          if (batch) onFrame({ kind: "output", data: batch, size: batch.length, time: e[0] });
          batch = "";
          send(e);
        }
      }
      if (batch) onFrame({ kind: "output", data: batch, size: batch.length, time: t });
      pos = Math.max(pos, t);
      schedule();
    };
    playbacks.set(id, {
      control(c) {
        switch (c.kind) {
          case "pause":
            paused = true;
            clearTimeout(timer);
            break;
          case "resume":
            paused = false;
            schedule();
            break;
          case "speed":
            speed = clampSpeed(c.value);
            schedule();
            break;
          case "skip": {
            const next = events[index];
            if (next && !paused) {
              clearTimeout(timer);
              index++;
              send(next);
              schedule();
            }
            break;
          }
          case "seek":
            seek(Math.max(0, c.time));
            break;
        }
        return { paused, speed };
      },
      close() {
        closed = true;
        clearTimeout(timer);
      },
    });
    // `?player=paused`: a still frame for captures (half-way, paused).
    const still = new URLSearchParams(location.search).get("player") === "paused";
    setTimeout(() => {
      if (still) {
        paused = true;
        seek(13.5);
      } else schedule();
    }, 60);
    return { id, paused: still, speed };
  }

  const backend: Pick<
    Backend,
    | "recordingStart"
    | "recordingStop"
    | "recordingStatus"
    | "recordingsFolder"
    | "listRecordings"
    | "deleteRecording"
    | "playRecording"
    | "playbackControl"
    | "playbackAck"
    | "playbackClose"
    | "onEngineLagged"
  > = {
    async recordingStart(terminal) {
      const t = deps.terminal(terminal);
      if (!t) throw { code: "terminal_closed", message: "this terminal is closed", hint: null } satisfies ErrorSummary;
      if (active.has(terminal)) {
        throw { code: "recording_already_active", message: `terminal ${terminal} is already being recorded`, hint: "This terminal is already recorded: stop the current recording first." } satisfies ErrorSummary;
      }
      const at = new Date();
      const name = `${t.alias}-${stamp(at)}.cast`;
      active.set(terminal, { name, session: t.session, started: at.getTime(), bytes: 180 });
      list.unshift({ name, alias: t.alias, started: at.getTime(), size: 180, duration: 0, width: 92, height: 26, valid: true });
      // The engine creates the file first, then emits `started`.
      setTimeout(() => deps.emit({ type: "recording", terminal, session: t.session, state: { state: "started", name, path: ABS + name } }), 120);
    },
    async recordingStop(terminal) {
      if (!active.has(terminal)) throw { code: "recording_not_active", message: `terminal ${terminal} is not being recorded`, hint: null } satisfies ErrorSummary;
      setTimeout(() => stop(terminal, "requested"), 60);
    },
    recordingStatus: async (terminal) => active.has(terminal),
    // `?folder=long`: a data folder outside the home directory (no `~`).
    recordingsFolder: async () =>
      new URLSearchParams(location.search).get("folder") === "long" ? "/srv/workstations/shared-profiles/ops-team/.local/share/portico/recordings/" : FOLDER,
    async listRecordings() {
      await new Promise((r) => setTimeout(r, new URLSearchParams(location.search).get("recordings") === "slow" ? 1500 : 200));
      if (new URLSearchParams(location.search).get("recordings") === "none") return [];
      return list.map((r) => ({ ...r }));
    },
    async deleteRecording(name) {
      if (!NAME.test(name)) throw invalidName(name);
      if ([...active.values()].some((r) => r.name === name)) {
        throw { code: "recording_active", message: `recording ${name} is still being written`, hint: "Stop the recording first, then delete it." } satisfies ErrorSummary;
      }
      const at = list.findIndex((r) => r.name === name);
      if (at < 0) throw { code: "recording_not_found", message: `recording ${name} does not exist`, hint: "It may have been deleted meanwhile: refresh the list." } satisfies ErrorSummary;
      list.splice(at, 1);
    },
    async playRecording(name, speed, onFrame) {
      if (!NAME.test(name)) throw invalidName(name);
      const entry = list.find((r) => r.name === name);
      if (!entry) throw { code: "recording_not_found", message: `recording ${name} does not exist`, hint: "It may have been deleted meanwhile: refresh the list." } satisfies ErrorSummary;
      if (!entry.valid) throw { code: "recording_invalid_header", message: `invalid asciicast header in ${name}: not a JSON object`, hint: "This is not an asciicast v2 file, or it is damaged." } satisfies ErrorSummary;
      const p = play(name, speed, onFrame);
      return { id: p.id, name, title: entry.alias, cols: entry.width ?? 80, rows: entry.height ?? 24, paused: p.paused, speed: p.speed };
    },
    async playbackControl(id, control) {
      const p = playbacks.get(id);
      if (!p) throw { code: "playback_closed", message: "this playback is closed", hint: null } satisfies ErrorSummary;
      return p.control(control);
    },
    playbackAck: async () => {},
    async playbackClose(id) {
      playbacks.get(id)?.close();
      playbacks.delete(id);
    },
    onEngineLagged: async () => () => {},
  };

  return {
    backend,
    /** The demo terminal printed `bytes`. */
    output(terminal: number, bytes: number) {
      const rec = active.get(terminal);
      if (rec) rec.bytes += bytes;
    },
    terminalClosed(terminal: number) {
      stop(terminal, "terminal_closed");
    },
    /** `flood` in a demo terminal: the writer could not keep up. */
    flood(terminal: number) {
      if (active.has(terminal)) setTimeout(() => stop(terminal, "queue_full"), 400);
    },
  };
}
