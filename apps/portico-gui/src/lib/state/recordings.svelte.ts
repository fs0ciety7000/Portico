// Session recording state: which terminals show `REC` (from `recording`
// engine events only, see `reduceRecording`), the start confirmation, and
// the Recordings dialog (list, delete confirmation, player).
//
// Requests name recordings by the `name` a listing gave (never a path);
// errors are shown from their `ErrorSummary`, never matched on text.
import { asError, type EngineEvent, type ErrorSummary, type Recording } from "../api";
import { backend } from "../backend";
import { deleteRefusal, forgetRecording, reduceRecording, resyncRecording, type RecMap } from "../recording";
import { workspace, type Pane } from "./workspace.svelte";

export type ListState = "loading" | "ready" | "error";

/** What the Recordings dialog shows. */
export type RecordingsView =
  | { kind: "list" }
  | { kind: "delete"; recording: Recording; error: ErrorSummary | null; busy: boolean }
  | { kind: "player"; recording: Recording };

class Recordings {
  /** Terminal id → recording, between `Started` and `Stopped`. */
  active: RecMap = $state({});
  /** Pane waiting for the start confirmation; `pane` is its place in a
   * split tab (`n/N`), null when the tab has one pane. */
  confirm: { terminal: number; alias: string; folder: string; pane: { index: number; count: number } | null } | null = $state(null);
  open = $state(false);
  view: RecordingsView = $state({ kind: "list" });
  list: Recording[] = $state([]);
  listState: ListState = $state("loading");
  listError: ErrorSummary | null = $state(null);
  selected = $state(0);
  /** The recordings folder for display (from Rust). */
  folder: string | null = $state(null);

  isRecording(terminal: number | null): boolean {
    return terminal !== null && terminal in this.active;
  }

  /** A pane whose shell is open and running. */
  recordable(pane: Pane | undefined): pane is Pane & { terminal: number } {
    return !!pane && pane.terminal !== null && !pane.end;
  }

  /** Names being written right now (shown in the list, delete refused). */
  get activeNames(): Set<string> {
    return new Set(Object.values(this.active).map((r) => r.name));
  }

  /** "Record session" / "Stop recording" on the focused pane. */
  async toggle(pane: Pane | undefined) {
    if (!this.recordable(pane)) return;
    if (this.isRecording(pane.terminal)) return this.stop(pane.terminal);
    try {
      const folder = await backend.recordingsFolder();
      const tab = workspace.tabOf(pane.key);
      const panes = tab ? workspace.panesIn(tab) : [];
      const index = panes.findIndex((p) => p.key === pane.key);
      this.confirm = {
        terminal: pane.terminal,
        alias: pane.alias,
        folder,
        pane: panes.length > 1 && index >= 0 ? { index: index + 1, count: panes.length } : null,
      };
    } catch (e) {
      workspace.notify(e);
    }
  }

  /** "Record" in the confirmation. REC turns on with the `started` event. */
  async start() {
    const c = this.confirm;
    this.confirm = null;
    if (!c) return;
    try {
      await backend.recordingStart(c.terminal);
    } catch (e) {
      workspace.notify(e);
    }
  }

  /** No confirmation: REC turns off with the `stopped` event. */
  async stop(terminal: number) {
    try {
      await backend.recordingStop(terminal);
    } catch (e) {
      const err = asError(e);
      // Already stopped: nothing to say, but REC goes off here even if its
      // `stopped` event was lost (an entry from a resync).
      if (err.code === "recording_not_active") this.active = forgetRecording(this.active, terminal);
      else workspace.notify(err);
    }
  }

  onEvent(ev: EngineEvent) {
    if (ev.type !== "recording") return;
    const { map, notice } = reduceRecording(this.active, ev.terminal, ev.session, ev.state);
    this.active = map;
    if (notice) {
      const { tone, ms, ...summary } = notice;
      workspace.notify(summary, tone, ms);
    }
    // A finished file shows up in an open list.
    if (ev.state.state === "stopped" && this.open && this.view.kind === "list") void this.refresh(false);
  }

  /** The event stream lagged: ask the engine about every open terminal. */
  async resync() {
    const panes = Object.values(workspace.panes).filter((p) => p.terminal !== null && !p.end);
    const status = new Map<number, { recording: boolean; session: number }>();
    await Promise.all(
      panes.map(async (p) => {
        if (p.terminal === null) return;
        const recording = await backend.recordingStatus(p.terminal).catch(() => false);
        status.set(p.terminal, { recording, session: p.session });
      }),
    );
    this.active = resyncRecording(this.active, status);
  }

  // ── Recordings dialog ───────────────────────────────────────────────

  show() {
    this.open = true;
    this.view = { kind: "list" };
    void this.refresh(true);
    if (this.folder === null) {
      backend.recordingsFolder().then(
        (f) => (this.folder = f),
        () => {},
      );
    }
  }

  close() {
    this.open = false;
    this.view = { kind: "list" };
  }

  async refresh(skeleton: boolean) {
    if (skeleton) this.listState = "loading";
    try {
      const keep = this.list[this.selected]?.name;
      this.list = await backend.listRecordings();
      this.listError = null;
      this.listState = "ready";
      const at = keep ? this.list.findIndex((r) => r.name === keep) : -1;
      this.selected = at >= 0 ? at : Math.min(this.selected, Math.max(0, this.list.length - 1));
    } catch (e) {
      this.listError = asError(e);
      this.listState = "error";
    }
  }

  play(recording: Recording | undefined) {
    if (recording?.valid) this.view = { kind: "player", recording };
  }

  /** A recording still being written is refused at once (not asked, then failed). */
  askDelete(recording: Recording | undefined) {
    if (!recording) return;
    const refusal = deleteRefusal(recording.name, this.activeNames);
    if (refusal) return workspace.notify(refusal, "warning");
    this.view = { kind: "delete", recording, error: null, busy: false };
  }

  /** Deletes the confirmed recording; a refusal (still written…) stays in the dialog. */
  async confirmDelete() {
    const v = this.view;
    if (v.kind !== "delete" || v.busy) return;
    v.busy = true;
    try {
      await backend.deleteRecording(v.recording.name);
      this.view = { kind: "list" };
      await this.refresh(false);
    } catch (e) {
      if (this.view === v) {
        v.error = asError(e);
        v.busy = false;
      }
    }
  }

  backToList() {
    this.view = { kind: "list" };
    void this.refresh(false);
  }
}

export const recordings = new Recordings();
