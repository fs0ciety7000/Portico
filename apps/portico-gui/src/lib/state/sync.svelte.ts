// Folder sync (phase 11, S1c): what the views show, derived from the
// engine's events and re-read with queries (`docs/SYNC.md` §10 "Ce que
// S1c attend de S1b").
//
// - No passphrase ever lives here: the dialogs hold their fields and clear
//   them as soon as they are sent.
// - `sync_pending_changed` reloads the received hosts and snippets, and a
//   review screen whose content changed closes itself (the core would
//   refuse its confirmation: `sync_not_pending`).
// - A `sync_folder_timeout` answer may still finish in the background: the
//   UI assumes neither success nor failure and re-reads everything.
import { asError, type EngineEvent, type ErrorSummary, type PendingHostChange, type SyncConflict, type SyncDevice, type SyncIssue, type SyncOverview, type SyncStatus, type UnconfirmedSnippet } from "../api";
import { backend } from "../backend";
import { workspace } from "./workspace.svelte";

export type SyncTab = "overview" | "conflicts" | "received" | "issues" | "devices";

export type SyncForm = "unlock" | "change" | "reset" | "rename" | "disable";

export type SyncView =
  | { kind: "wizard" }
  | { kind: "panel"; tab: SyncTab }
  | { kind: "form"; form: SyncForm }
  | { kind: "host"; alias: string; hash: string; back: SyncView | null }
  | { kind: "snippet"; id: string; hash: string; back: SyncView | null };

/** The status-bar clock ("Synced 2 min ago"). */
const TICK_MS = 30_000;

export const TIMEOUT = "sync_folder_timeout";
export const NOT_PENDING = "sync_not_pending";
export const UNCONFIRMED = "sync_snippet_unconfirmed";

type Load = "idle" | "loading" | "ready" | "error";

class SyncState {
  overview: SyncOverview | null = $state(null);
  status: SyncStatus | null = $derived(this.overview?.status ?? null);
  enabled: boolean = $derived(!!this.status && this.status.state !== "disabled");
  conflicts: SyncConflict[] = $state([]);
  conflictCount = $state(0);
  issues: SyncIssue[] = $state([]);
  devices: SyncDevice[] = $state([]);
  pendingHosts: PendingHostChange[] = $state([]);
  snippets: UnconfirmedSnippet[] = $state([]);
  /** Panel lists: loading / error state (the overview has its own). */
  lists: Load = $state("idle");
  listError: ErrorSummary | null = $state(null);
  view: SyncView | null = $state(null);
  now = $state(Date.now());
  /** Received hosts by alias (sidebar badges). */
  pendingByAlias: Record<string, PendingHostChange> = $derived(Object.fromEntries(this.pendingHosts.map((p) => [p.alias, p])));
  /** Ids of snippets changed by sync and not confirmed here (F8). */
  unconfirmedIds: Set<string> = $derived(new Set(this.snippets.map((s) => s.id)));
  received: number = $derived(this.pendingHosts.length + this.snippets.length);
  #timer: ReturnType<typeof setInterval> | undefined;
  #gen = 0;
  /** Called when snippets.toml changed (the snippets view reloads). */
  onsnippets: (() => void) | null = null;
  /** Called when settings.toml changed (theme and keymaps, live). */
  onsettings: (() => void) | null = null;

  /** At startup: the background task, then everything the views derive. */
  async init() {
    this.#timer ??= setInterval(() => (this.now = Date.now()), TICK_MS);
    await backend.syncStart().catch(() => {});
    await this.refresh();
  }

  /** Status, received hosts and snippets (the latter work even when sync is off: F8). */
  async refresh() {
    const gen = ++this.#gen;
    const [overview, hosts, snippets] = await Promise.allSettled([
      backend.syncStatus(),
      backend.syncPendingHosts(),
      backend.syncedSnippetsUnconfirmed(),
    ]);
    if (gen !== this.#gen) return;
    if (overview.status === "fulfilled") this.overview = overview.value;
    if (hosts.status === "fulfilled") this.pendingHosts = hosts.value;
    if (snippets.status === "fulfilled") this.snippets = snippets.value;
    if (this.enabled) void this.loadConflicts();
    this.#closeStale();
  }

  /** The panel's lists: devices, conflicts, issues. */
  async loadLists() {
    if (!this.enabled) {
      this.devices = [];
      this.conflicts = [];
      this.issues = [];
      this.lists = "ready";
      return;
    }
    if (this.lists !== "ready") this.lists = "loading";
    try {
      const [devices, conflicts, issues] = await Promise.all([backend.syncDevices(), backend.syncConflicts(), backend.syncIssues()]);
      this.devices = devices;
      this.conflicts = conflicts;
      this.conflictCount = conflicts.length;
      this.issues = issues;
      this.lists = "ready";
      this.listError = null;
    } catch (e) {
      this.lists = "error";
      this.listError = asError(e);
      // The status says what happened; the lists load again with the next event.
      if (this.listError.code === TIMEOUT) void this.refresh();
    }
  }

  async loadConflicts() {
    try {
      this.conflicts = await backend.syncConflicts();
      this.conflictCount = this.conflicts.length;
    } catch {
      // Shown by the panel when it loads its lists.
    }
  }

  onEvent(ev: EngineEvent) {
    switch (ev.type) {
      case "sync": {
        const status = ev.status;
        const was = this.status?.state;
        this.overview = this.overview ? { ...this.overview, status } : { status, folder: null, device: null, rememberKey: true };
        // On, off, unlocked: the folder and device changed too.
        if (was === "disabled" || status.state === "disabled" || was === "locked") void this.refresh();
        if (this.view?.kind === "panel") void this.loadLists();
        return;
      }
      case "sync_conflicts":
        this.conflictCount = ev.count;
        void this.loadConflicts();
        return;
      case "sync_pending_changed":
        void this.reloadPending();
        return;
      case "snippets_changed":
        void this.reloadPending();
        this.onsnippets?.();
        return;
      case "settings_changed":
        this.onsettings?.();
        return;
    }
  }

  /** After a lagged event stream: everything is read again. */
  async resync() {
    await this.refresh();
    if (this.view?.kind === "panel") await this.loadLists();
  }

  async reloadPending() {
    const [hosts, snippets] = await Promise.allSettled([backend.syncPendingHosts(), backend.syncedSnippetsUnconfirmed()]);
    if (hosts.status === "fulfilled") this.pendingHosts = hosts.value;
    if (snippets.status === "fulfilled") this.snippets = snippets.value;
    this.#closeStale();
  }

  /** A review showing content that is no longer pending (or changed) closes. */
  #closeStale() {
    const v = this.view;
    if (v?.kind === "host" && !this.pendingHosts.some((p) => p.alias === v.alias && p.destination_hash === v.hash)) {
      this.view = v.back;
      workspace.notify(
        { code: NOT_PENDING, message: `${v.alias} changed again`, hint: this.pendingByAlias[v.alias] ? "Review its new destination." : "Nothing waits for a confirmation any more." },
        "warning",
      );
    } else if (v?.kind === "snippet" && !this.snippets.some((s) => s.id === v.id && s.hash === v.hash)) {
      this.view = v.back;
      workspace.notify(
        { code: NOT_PENDING, message: "this snippet changed again", hint: this.unconfirmedIds.has(v.id) ? "Review its new command." : "Nothing waits for a confirmation any more." },
        "warning",
      );
    }
  }

  open(view: SyncView) {
    this.view = view;
    if (view.kind === "panel") void this.loadLists();
  }

  /** "Sync…": the panel when set up, the wizard otherwise. */
  show(tab: SyncTab = "overview") {
    this.open(this.enabled ? { kind: "panel", tab } : { kind: "wizard" });
  }

  close() {
    this.view = null;
  }

  /** Back from a form or a review: where it was opened from. */
  back() {
    const v = this.view;
    if (v && (v.kind === "host" || v.kind === "snippet")) this.view = v.back;
    else if (v?.kind === "form") this.open({ kind: "panel", tab: "overview" });
    else this.view = null;
  }

  reviewHost(change: PendingHostChange) {
    this.view = { kind: "host", alias: change.alias, hash: change.destination_hash, back: this.view };
  }

  reviewSnippet(snippet: UnconfirmedSnippet) {
    this.view = { kind: "snippet", id: snippet.id, hash: snippet.hash, back: this.view };
  }

  /** A run refused for an unconfirmed snippet: its review, not a bare error. */
  async reviewSnippetById(id: string): Promise<boolean> {
    await this.reloadPending();
    const found = this.snippets.find((s) => s.id === id);
    if (found) this.reviewSnippet(found);
    return !!found;
  }

  /** Runs a command whose answer is a status; null on success. */
  async act(run: () => Promise<SyncStatus | void>): Promise<ErrorSummary | null> {
    try {
      const status = await run();
      if (status && this.overview) this.overview = { ...this.overview, status };
      await this.refresh();
      if (this.view?.kind === "panel") await this.loadLists();
      return null;
    } catch (e) {
      const error = asError(e);
      this.#afterError(error);
      return error;
    }
  }

  /** The 60 s limit: the operation may still finish: re-read, assume nothing. */
  #afterError(error: ErrorSummary) {
    if (error.code === TIMEOUT || error.code === NOT_PENDING) void this.resync();
  }
}

export const sync = new SyncState();
