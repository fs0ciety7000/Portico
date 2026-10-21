// Host inventory as the UI shows it: hosts from ~/.ssh/config with their
// Portico metadata (favorite, tags, group, notes: hosts.toml, written by
// the core), recent hosts, folded sidebar sections, the editor and the
// "clear recent" confirmation. Metadata changes arrive as `host_meta`
// events (from this window or another UI) and update hosts in place.
import type { EngineEvent, Host, HostMeta, RecentHost } from "../api";
import { backend } from "../backend";
import { workspace } from "./workspace.svelte";

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

  set(list: Host[]) {
    this.all = list;
    this.revision++;
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
