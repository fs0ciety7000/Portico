// SFTP explorer state: one remote folder view per session, the transfer
// queue (all sessions) and the conflict questions. Local files never appear
// here as paths: uploads are tokens handed out by the Rust side.
import { asError, type Direction, type ErrorSummary, type OnConflict, type RemoteEntry, type SftpEvent, type Upload } from "../api";
import { backend } from "../backend";
import { workspace } from "./workspace.svelte";

export interface Explorer {
  session: number;
  alias: string;
  cwd: string;
  entries: RemoteEntry[];
  loading: boolean;
  error: ErrorSummary | null;
  /** Path of the selected row. */
  selected: string | null;
}

export type TransferStatus = "queued" | "running" | "done" | "skipped" | "cancelled" | "failed";

/** How to run the same transfer again (after a conflict question). */
type Retry =
  | { kind: "download"; remotePath: string }
  | { kind: "upload"; token: number; remoteDir: string };

export interface Transfer {
  id: number;
  session: number;
  direction: Direction;
  name: string;
  done: number;
  total: number | null;
  status: TransferStatus;
  /** `performance.now()` when the bytes started moving. */
  started: number;
  /** Bytes per second, smoothed. */
  rate: number;
  error: ErrorSummary | null;
  destination: string | null;
  retry: Retry;
}

export interface Conflict {
  session: number;
  name: string;
  retry: Retry;
}

const parentOf = (path: string): string => {
  const trimmed = path.replace(/\/+$/, "");
  const at = trimmed.lastIndexOf("/");
  return at <= 0 ? "/" : trimmed.slice(0, at);
};

export const joinRemote = (dir: string, name: string): string => (dir.endsWith("/") ? `${dir}${name}` : `${dir}/${name}`);

/** `/srv/app/releases` → `["/", "srv", "app", "releases"]` with their paths. */
/** Controls and bidi overrides, as in `portico_core::sftp::display_safe`. */
const UNSAFE_CHARS = /[\p{Cc}\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]/gu;

/**
 * Text chosen by the server (a remote path) made safe to show: a name like
 * `invoice\u202Efdp.sh` cannot disguise itself. The raw text is kept for
 * operations.
 */
export function displaySafe(text: string): string {
  return text.replace(UNSAFE_CHARS, "\uFFFD");
}

export function crumbs(path: string): { label: string; path: string }[] {
  const out = [{ label: "/", path: "/" }];
  let acc = "";
  for (const part of path.split("/").filter(Boolean)) {
    acc += `/${part}`;
    out.push({ label: displaySafe(part), path: acc });
  }
  return out;
}

class Files {
  explorers: Record<number, Explorer> = $state({});
  transfers: Transfer[] = $state([]);
  conflicts: Conflict[] = $state([]);
  downloadDir: string = $state("");
  showHidden = $state(false);
  /** A drag with files is over the window. */
  dragging = $state(false);
  /** Latest navigation per session: older answers are dropped. */
  #nav = new Map<number, number>();

  async init() {
    try {
      this.downloadDir = await backend.downloadDir();
    } catch {
      // Shown as "Downloads" until the user picks a folder.
    }
  }

  /** Opens (or returns) the explorer of `session`, at the remote home. */
  open(session: number, alias: string): Explorer {
    const existing = this.explorers[session];
    if (existing) return existing;
    this.explorers[session] = { session, alias, cwd: "", entries: [], loading: true, error: null, selected: null };
    const ex = this.explorers[session];
    backend.sftpHome(session).then(
      (home) => this.go(session, home),
      (e) => {
        ex.loading = false;
        ex.error = asError(e);
      },
    );
    return ex;
  }

  visible(ex: Explorer): RemoteEntry[] {
    return this.showHidden ? ex.entries : ex.entries.filter((e) => !e.name.startsWith("."));
  }

  async go(session: number, path: string) {
    const ex = this.explorers[session];
    if (!ex) return;
    const keep = ex.cwd === path ? ex.selected : null;
    const seq = (this.#nav.get(session) ?? 0) + 1;
    this.#nav.set(session, seq);
    const current = () => this.#nav.get(session) === seq;
    ex.loading = true;
    ex.error = null;
    try {
      const entries = await backend.sftpList(session, path);
      // A later navigation won the race: drop this answer.
      if (!current()) return;
      ex.cwd = path;
      ex.entries = entries;
      const shown = this.visible(ex);
      ex.selected = shown.some((e) => e.path === keep) ? keep : (shown[0]?.path ?? null);
    } catch (e) {
      if (!current()) return;
      ex.error = asError(e);
      // The failed folder becomes the current one: "retry" and "parent"
      // then act on it.
      ex.cwd = path;
      ex.entries = [];
    } finally {
      if (current()) ex.loading = false;
    }
  }

  refresh(session: number) {
    const ex = this.explorers[session];
    if (ex?.cwd) void this.go(session, ex.cwd);
  }

  up(session: number) {
    const ex = this.explorers[session];
    if (ex?.cwd && ex.cwd !== "/") {
      const from = ex.cwd;
      void this.go(session, parentOf(from)).then(() => {
        // Land on the folder we came from.
        if (ex.entries.some((e) => e.path === from)) ex.selected = from;
      });
    }
  }

  move(session: number, delta: number) {
    const ex = this.explorers[session];
    if (!ex) return;
    const shown = this.visible(ex);
    if (!shown.length) return;
    const at = shown.findIndex((e) => e.path === ex.selected);
    const next = Math.min(Math.max(at + delta, 0), shown.length - 1);
    ex.selected = shown[next]?.path ?? null;
  }

  /** Folders open, files download. */
  activate(session: number, entry: RemoteEntry) {
    if (entry.kind === "dir" || entry.kind === "symlink") void this.go(session, entry.path);
    else void this.download(session, entry);
  }

  async mkdir(session: number, name: string) {
    const ex = this.explorers[session];
    if (!ex) return;
    const path = joinRemote(ex.cwd, name);
    await this.#op(session, () => backend.sftpMkdir(session, path));
    ex.selected = path;
  }

  async rename(session: number, entry: RemoteEntry, name: string) {
    const ex = this.explorers[session];
    if (!ex || name === entry.name) return;
    const to = joinRemote(ex.cwd, name);
    await this.#op(session, () => backend.sftpRename(session, entry.path, to));
    ex.selected = to;
  }

  async remove(session: number, entry: RemoteEntry) {
    const ex = this.explorers[session];
    if (!ex) return;
    const shown = this.visible(ex);
    const at = shown.findIndex((e) => e.path === entry.path);
    await this.#op(session, () => backend.sftpRemove(session, entry.path));
    // Select the neighbour, like a file manager.
    const rest = this.visible(ex);
    ex.selected = rest[Math.min(at, rest.length - 1)]?.path ?? null;
  }

  async #op(session: number, run: () => Promise<void>) {
    try {
      await run();
    } catch (e) {
      workspace.notify(e);
    }
    await this.go(session, this.explorers[session]?.cwd ?? "/");
  }

  // ── Transfers ───────────────────────────────────────────────────────

  async download(session: number, entry: RemoteEntry, onConflict: OnConflict = "ask") {
    await this.#start(session, entry.display, "download", { kind: "download", remotePath: entry.path }, onConflict);
  }

  async upload(session: number, files: Upload[], onConflict: OnConflict = "ask") {
    const ex = this.explorers[session];
    if (!ex) return;
    for (const f of files) {
      await this.#start(session, f.display, "upload", { kind: "upload", token: f.token, remoteDir: ex.cwd }, onConflict, f.size);
    }
  }

  async pickAndUpload(session: number) {
    try {
      const files = await backend.pickUploads();
      if (files.length) await this.upload(session, files);
    } catch (e) {
      workspace.notify(e);
    }
  }

  /** Files dropped on the window go to the explorer that is showing. */
  dropped(files: Upload[], session: number | null) {
    if (!files.length) return;
    if (session !== null && this.explorers[session]) {
      void this.upload(session, files);
      return;
    }
    workspace.notify(
      { code: "drop", message: "Open Files to upload", hint: "Open a host's files, then drop again." },
      "info",
    );
  }

  async #start(session: number, name: string, direction: Direction, retry: Retry, onConflict: OnConflict, size: number | null = null) {
    try {
      const id =
        retry.kind === "download"
          ? await backend.download(session, retry.remotePath, onConflict)
          : await backend.upload(session, retry.token, retry.remoteDir, onConflict);
      // The first event may have been handled already (it can overtake the reply).
      if (!this.transfers.some((t) => t.id === id)) {
        this.transfers.push({
          id,
          session,
          direction,
          name,
          done: 0,
          total: size,
          status: "queued",
          started: performance.now(),
          rate: 0,
          error: null,
          destination: null,
          retry,
        });
      } else {
        const t = this.transfers.find((x) => x.id === id);
        if (t) {
          t.name = name;
          t.retry = retry;
        }
      }
      this.#settleEarly(id);
    } catch (e) {
      workspace.notify(e);
    }
  }

  cancel(id: number) {
    backend.cancelTransfer(id).catch(() => {});
  }

  /** Removes finished rows. */
  clearDone() {
    this.transfers = this.transfers.filter((t) => t.status === "queued" || t.status === "running");
  }

  answerConflict(policy: OnConflict | null) {
    const c = this.conflicts.shift();
    if (!c || !policy) return;
    if (c.retry.kind === "download") {
      void this.#start(c.session, c.name, "download", c.retry, policy);
    } else {
      void this.#start(c.session, c.name, "upload", c.retry, policy);
    }
  }

  async chooseDownloadDir() {
    try {
      const dir = await backend.chooseDownloadDir();
      if (dir) this.downloadDir = dir;
    } catch (e) {
      workspace.notify(e);
    }
  }

  /** Events of transfers whose id the view does not know yet. */
  #early = new Map<number, { session: number; event: SftpEvent }[]>();

  #settleEarly(id: number) {
    const queued = this.#early.get(id);
    this.#early.delete(id);
    for (const q of queued ?? []) this.onEvent(q.session, q.event);
  }

  onEvent(session: number, ev: SftpEvent) {
    if (ev.kind === "ready" || ev.kind === "closed") return;
    const t = this.transfers.find((x) => x.id === ev.transfer);
    if (!t) {
      const list = this.#early.get(ev.transfer) ?? [];
      list.push({ session, event: ev });
      this.#early.set(ev.transfer, list);
      return;
    }
    switch (ev.kind) {
      case "transfer_started":
        t.status = "running";
        t.total = ev.total;
        t.started = performance.now();
        return;
      case "transfer_progress": {
        const secs = (performance.now() - t.started) / 1000;
        t.done = ev.done;
        t.total = ev.total;
        if (secs > 0.2) t.rate = t.rate ? t.rate * 0.7 + (ev.done / secs) * 0.3 : ev.done / secs;
        return;
      }
      case "transfer_finished":
        t.status = "done";
        t.done = t.total ?? t.done;
        t.destination = ev.destination;
        if (t.direction === "upload") this.#refreshShowing(t.session);
        return;
      case "transfer_skipped":
        t.status = "skipped";
        return;
      case "transfer_cancelled":
        t.status = "cancelled";
        return;
      case "transfer_failed":
        if (ev.error.code === "destination_exists") {
          // Ask instead of failing: the row is replaced by the answer.
          this.transfers = this.transfers.filter((x) => x.id !== t.id);
          this.conflicts.push({ session: t.session, name: t.name, retry: t.retry });
        } else {
          t.status = "failed";
          t.error = ev.error;
        }
        return;
    }
  }

  #refreshShowing(session: number) {
    const ex = this.explorers[session];
    if (ex && !ex.loading) void this.go(session, ex.cwd);
  }

  /** The session ended: its explorer can not be used any more. */
  forget(session: number) {
    delete this.explorers[session];
    this.conflicts = this.conflicts.filter((c) => c.session !== session);
  }
}

export const files = new Files();
