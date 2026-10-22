// Host inventory as the UI shows it: hosts from ~/.ssh/config with their
// Portico metadata (favorite, tags, group, notes: hosts.toml, written by
// the core), recent hosts, folded sidebar sections, the editor and the
// "clear recent" confirmation. Metadata changes arrive as `host_meta`
// events (from this window or another UI) and update hosts in place.
//
// Phase 6b: hosts created in Portico (its own hosts.conf). The host dialogs
// (create, edit, read-only notice, duplicate, delete), the Include line
// banner, and `hosts_changed`, on which the inventory is reloaded (App
// keeps the selection, or moves it to `pendingSelect`).
import type { EngineEvent, ErrorSummary, Host, HostMeta, ManagedHost, ManagedInclude, RecentHost } from "../api";
import type { HostForm } from "../hostform";

import { backend } from "../backend";
import { workspace } from "./workspace.svelte";

/** See `Hosts.parked`. `form` and `initial` are `HostForm`s (hostform.ts). */
export interface ParkedHostForm {
  form: HostForm;
  initial: HostForm;
  /** The error that sent the user to the vault (shown again on "Not now"). */
  error: ErrorSummary;
  outcome: "pending" | "ready" | "declined";
}

export type HostDialog =
  | { kind: "create" }
  /** `managed`: what hosts.conf holds, read when the dialog opens (the form starts from it). */
  | { kind: "edit"; alias: string; managed: ManagedHost }
  /** A host of ~/.ssh/config or of an import: never edited, duplicate
   * offered. `action: "delete"`: opened by "Delete host" (titled so). */
  | { kind: "read-only"; alias: string; action?: "edit" | "delete" }
  | { kind: "duplicate"; alias: string }
  | { kind: "delete"; alias: string };

/** The Include line whose banner was dismissed (per machine, browser storage). */
const BANNER_KEY = "portico.include.dismissed";

function loadDismissed(): string | null {
  try {
    return localStorage.getItem(BANNER_KEY);
  } catch {
    return null;
  }
}

/** Folded sections are a per-machine convenience (browser storage). */
const COLLAPSED_KEY = "portico.sidebar.collapsed";

function loadCollapsed(): Record<string, boolean> {
  try {
    const raw = localStorage.getItem(COLLAPSED_KEY);
    const parsed: unknown = raw ? JSON.parse(raw) : {};
    if (parsed && typeof parsed === "object") {
      return Object.fromEntries(Object.entries(parsed).filter(([, v]) => v === true)) as Record<string, boolean>;
    }
  } catch {
    // Storage unavailable or corrupt: everything unfolded.
  }
  return {};
}

/** How many recent hosts are asked for (the sidebar keeps those still configured). */
const RECENT_FETCHED = 20;

class Hosts {
  all: Host[] = $state([]);
  recent: RecentHost[] = $state([]);
  collapsed: Record<string, boolean> = $state(loadCollapsed());
  /** Alias whose metadata editor is open. */
  editing: string | null = $state(null);
  /** "Clear recent?" confirmation shown. */
  confirmClear = $state(false);
  /** Bumped on every metadata change (search results depend on it). */
  revision = $state(0);

  byAlias: Record<string, Host> = $derived(Object.fromEntries(this.all.map((h) => [h.alias, h])));

  /** The open host dialog (phase 6b). */
  dialog: HostDialog | null = $state(null);
  /** Bumped by every "Edit host…" (a slow read never opens a stale dialog). */
  private editRun = 0;
  /** The Include line for hosts.conf, and whether ~/.ssh/config has it. */
  include: ManagedInclude | null = $state(null);
  dismissed: string | null = $state(loadDismissed());
  /** Alias to select once the reloaded list has it (after a create, rename, duplicate). */
  pendingSelect: string | null = $state(null);
  /** Reloads the inventory (set by App: the same `load` as at startup). */
  onchanged: (() => void) | null = null;

  hasPortico = $derived(this.all.some((h) => h.origin === "portico"));
  /** "Add this line" banner: Portico hosts exist, OpenSSH does not see them yet. */
  showBanner = $derived(this.hasPortico && !!this.include && !this.include.active && this.dismissed !== this.include.line);

  set(list: Host[]) {
    this.all = list;
    this.revision++;
    if (this.hasPortico) void this.loadInclude();
  }

  async loadInclude() {
    try {
      this.include = await backend.managedInclude();
    } catch (e) {
      workspace.notify(e);
    }
  }

  dismissBanner() {
    if (!this.include) return;
    this.dismissed = this.include.line;
    try {
      localStorage.setItem(BANNER_KEY, this.include.line);
    } catch {
      // Not remembered: it comes back next time.
    }
  }

  /** "Edit host…": the host dialog for a Portico host, once its values in
   * hosts.conf are read; the read-only notice otherwise. */
  async openEdit(alias: string) {
    const host = this.byAlias[alias];
    if (!host) return;
    if (host.origin !== "portico") {
      this.dialog = { kind: "read-only", alias };
      return;
    }
    const run = ++this.editRun;
    try {
      const managed = await backend.managedHost(alias);
      // Another dialog opened meanwhile: that one wins.
      if (run !== this.editRun || this.dialog) return;
      if (managed) this.dialog = { kind: "edit", alias, managed };
      else {
        workspace.notify({ code: "host_not_found", message: `${alias} is no longer in Portico's hosts.conf`, hint: "Reload the host list." });
        this.changed();
      }
    } catch (e) {
      if (run === this.editRun) workspace.notify(e);
    }
  }

  /** "Delete host": only a Portico host; the others are read-only. */
  openDelete(alias: string) {
    const host = this.byAlias[alias];
    if (!host) return;
    this.dialog = host.origin === "portico" ? { kind: "delete", alias } : { kind: "read-only", alias, action: "delete" };
  }

  /** B1: the host form waiting on the vault dialog (a password could not be
   * saved: no vault, or a locked one). The form, typed password included,
   * is parked here while the vault dialog has the screen, and given back
   * when it closes: `ready` (created or unlocked: the form saves again) or
   * `declined` ("Not now": the form shows the error under the password).
   * Never outlives the host dialog. */
  parked: ParkedHostForm | null = null;

  park(form: ParkedHostForm) {
    this.parked = form;
  }

  /** The parked form for the dialog now showing, once the vault dialog
   * answered (taken: given back once). */
  unpark(): ParkedHostForm | null {
    const parked = this.parked;
    if (!parked || parked.outcome === "pending") return null;
    this.parked = null;
    return parked;
  }

  /** The host dialog closed: nothing parked survives it. */
  dropParked() {
    if (this.parked) this.parked.form.password = "";
    this.parked = null;
  }

  /** hosts.conf changed: reload, then select `alias` when given. */
  changed(alias?: string) {
    if (alias) this.pendingSelect = alias;
    this.onchanged?.();
  }

  async remove(alias: string) {
    try {
      await backend.deleteHost(alias);
      this.dialog = null;
      this.changed();
      workspace.notify({ code: "host_deleted", message: `${alias} deleted`, hint: null }, "info");
    } catch (e) {
      this.dialog = null;
      workspace.notify(e);
    }
  }

  async loadRecent() {
    try {
      this.recent = await backend.recentHosts(RECENT_FETCHED);
    } catch (e) {
      workspace.notify(e);
    }
  }

  onEvent(ev: EngineEvent) {
    if (ev.type === "host_meta") this.#apply(ev.alias, ev.meta);
    else if (ev.type === "recent_changed") void this.loadRecent();
    else if (ev.type === "hosts_changed") this.onchanged?.();
  }

  #apply(alias: string, meta: HostMeta) {
    const host = this.all.find((h) => h.alias === alias);
    if (!host) return;
    host.tags = meta.tags;
    host.group = meta.group;
    host.favorite = meta.favorite;
    host.notes = meta.notes;
    this.revision++;
  }

  async toggleFavorite(alias: string) {
    const host = this.byAlias[alias];
    if (!host) return;
    try {
      this.#apply(alias, await backend.setFavorite(alias, !host.favorite));
      workspace.notify({ code: "favorite", message: host.favorite ? `${alias} added to favorites` : `${alias} removed from favorites`, hint: null }, "info");
    } catch (e) {
      workspace.notify(e);
    }
  }

  async forgetRecent(alias: string) {
    try {
      await backend.forgetRecent(alias);
      this.recent = this.recent.filter((r) => r.alias !== alias);
    } catch (e) {
      workspace.notify(e);
    }
  }

  async clearRecent() {
    this.confirmClear = false;
    try {
      await backend.clearRecent();
      this.recent = [];
      workspace.notify({ code: "recent_cleared", message: "Recent hosts cleared", hint: null }, "info");
    } catch (e) {
      workspace.notify(e);
    }
  }

  toggleSection(id: string, collapse = !this.collapsed[id]) {
    if (collapse) this.collapsed[id] = true;
    else delete this.collapsed[id];
    try {
      localStorage.setItem(COLLAPSED_KEY, JSON.stringify(this.collapsed));
    } catch {
      // Not remembered: fine.
    }
  }
}

export const hosts = new Hosts();
