// Workspace state derived from engine events: sessions, terminal tabs,
// pending prompts and alerts. Components read it; user actions call the
// methods below, which call the backend.
import { asError, durationMs, type EngineEvent, type ErrorSummary, type HostKeyChanged, type Phase, type Prompt, type SessionView } from "../api";
import { backend } from "../backend";

export interface SessionInfo extends SessionView {
  alias: string;
}

export type TabEnd = { kind: "exited"; status: number | null; signal: string | null } | { kind: "closed" };

export interface Tab {
  /** Local, stable key (survives reconnections). */
  key: number;
  alias: string;
  session: number;
  /** Set once the PTY is open. */
  terminal: number | null;
  /** Remote title (OSC 0/2), sanitized; shown after the alias, never instead. */
  title: string;
  end: TabEnd | null;
  /** Opening failed for a reason other than the session itself. */
  error: ErrorSummary | null;
  /** Bumped to (re)open a terminal in this tab. */
  generation: number;
}

export interface Alert extends HostKeyChanged {
  alias: string;
}

const LIVE: Phase["phase"][] = ["resolving", "connecting", "handshake", "verifying_host_key", "authenticating", "connected"];

export const isLive = (s: SessionInfo | undefined): boolean => !!s && LIVE.includes(s.phase.phase);

let nextKey = 1;

class Workspace {
  sessions: Record<number, SessionInfo> = $state({});
  tabs: Tab[] = $state([]);
  /** Active tab key; null shows the host list. */
  active: number | null = $state(null);
  prompts: Prompt[] = $state([]);
  alerts: Alert[] = $state([]);
  toast: ErrorSummary | null = $state(null);
  #toastTimer: ReturnType<typeof setTimeout> | undefined;

  /** Latest session per host alias (host list, details). */
  byAlias: Record<string, SessionInfo> = $derived.by(() => {
    const out: Record<string, SessionInfo> = {};
    for (const s of Object.values(this.sessions)) {
      const prev = out[s.alias];
      if (!prev || prev.id < s.id) out[s.alias] = s;
    }
    return out;
  });

  activeTab: Tab | undefined = $derived(this.tabs.find((t) => t.key === this.active));

  notify(e: unknown) {
    this.toast = asError(e);
    clearTimeout(this.#toastTimer);
    this.#toastTimer = setTimeout(() => (this.toast = null), 5000);
  }

  // ── Tabs ────────────────────────────────────────────────────────────

  /** Enter on a host: focus its live tab, or open one (reusing a live session). */
  async open(alias: string) {
    const live = this.tabs.find((t) => t.alias === alias && !t.end && isLive(this.sessions[t.session]));
    if (live) {
      this.active = live.key;
      return;
    }
    await this.#newTab(alias);
  }

  /** New tab on the same host as the active one. */
  async duplicate() {
    const tab = this.activeTab;
    if (tab) await this.#newTab(tab.alias);
  }

  async #newTab(alias: string) {
    const current = this.byAlias[alias];
    const session = isLive(current) && current ? current.id : await this.#connect(alias);
    if (session === null) return;
    const tab: Tab = { key: nextKey++, alias, session, terminal: null, title: "", end: null, error: null, generation: 0 };
    this.tabs.push(tab);
    this.active = tab.key;
  }

  async #connect(alias: string): Promise<number | null> {
    try {
      const id = await backend.connect(alias);
      this.sessions[id] ??= { id, alias, phase: { phase: "resolving" }, latencyMs: null };
      return id;
    } catch (e) {
      this.notify(e);
      return null;
    }
  }

  /** Reopens a shell in an ended or failed tab (new session if needed). */
  async reconnect(key: number) {
    const tab = this.tabs.find((t) => t.key === key);
    if (!tab) return;
    if (!isLive(this.sessions[tab.session])) {
      const session = await this.#connect(tab.alias);
      if (session === null) return;
      tab.session = session;
    }
    tab.terminal = null;
    tab.end = null;
    tab.error = null;
    tab.generation++;
  }

  async close(key: number | undefined = this.active ?? undefined) {
    const index = this.tabs.findIndex((t) => t.key === key);
    const tab = this.tabs[index];
    if (!tab) return;
    this.tabs.splice(index, 1);
    if (this.active === key) {
      const next = this.tabs[Math.min(index, this.tabs.length - 1)];
      this.active = next ? next.key : null;
    }
    if (tab.terminal !== null && !tab.end) await backend.closeTerminal(tab.terminal).catch(() => {});
    // The last tab of a session disconnects it (also cancels a connection in flight).
    if (!this.tabs.some((t) => t.session === tab.session) && isLive(this.sessions[tab.session])) {
      await backend.disconnect(tab.session).catch((e) => this.notify(e));
    }
  }

  go(index: number) {
    const tab = this.tabs[index];
    if (tab) this.active = tab.key;
  }

  cycle(delta: number) {
    // Position 0 is the host list.
    const keys: (number | null)[] = [null, ...this.tabs.map((t) => t.key)];
    const at = keys.indexOf(this.active);
    this.active = keys[(at + delta + keys.length) % keys.length] ?? null;
  }

  async disconnect(alias: string) {
    const s = this.byAlias[alias];
    if (s && isLive(s)) await backend.disconnect(s.id).catch((e) => this.notify(e));
  }

  // ── Prompts ─────────────────────────────────────────────────────────

  async answer(prompt: Prompt, answer: Parameters<typeof backend.respondPrompt>[1]) {
    this.prompts = this.prompts.filter((p) => p.id !== prompt.id);
    await backend.respondPrompt(prompt.id, answer).catch((e) => this.notify(e));
  }

  dismissAlert() {
    this.alerts.shift();
  }

  aliasOf(session: number | null): string | undefined {
    return session === null ? undefined : this.sessions[session]?.alias;
  }

  // ── Engine events ───────────────────────────────────────────────────

  onEvent(ev: EngineEvent) {
    switch (ev.type) {
      case "session": {
        const e = ev.event;
        if (e.kind === "started") {
          this.sessions[ev.id] ??= { id: ev.id, alias: e.alias, phase: { phase: "resolving" }, latencyMs: null };
          return;
        }
        const s = this.sessions[ev.id];
        if (!s) return;
        if (e.kind === "phase") {
          const { kind: _kind, ...phase } = e;
          s.phase = phase;
          if (phase.phase !== "connected") s.latencyMs = null;
        } else if (e.kind === "latency") {
          s.latencyMs = durationMs(e.rtt);
        } else if (e.kind === "host_key_changed") {
          const { kind: _kind, ...alert } = e;
          this.alerts.push({ ...alert, alias: s.alias });
        }
        return;
      }
      case "terminal": {
        const tab = this.tabs.find((t) => t.terminal === ev.id);
        if (!tab) return;
        const e = ev.event;
        if (e.kind === "exited") tab.end = { kind: "exited", status: e.status, signal: e.signal };
        else if (e.kind === "closed") tab.end ??= { kind: "closed" };
        return;
      }
      case "prompt":
        if (!this.prompts.some((p) => p.id === ev.id)) {
          this.prompts.push({ id: ev.id, session: ev.session, kind: ev.kind });
        }
        return;
      case "prompt_resolved":
        this.prompts = this.prompts.filter((p) => p.id !== ev.id);
        return;
    }
  }
}

export const workspace = new Workspace();
