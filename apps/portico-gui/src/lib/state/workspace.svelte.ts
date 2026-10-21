// Workspace state derived from engine events: sessions, terminal tabs,
// pending prompts and alerts. Components read it; user actions call the
// methods below, which call the backend.
import { asError, durationMs, type EngineEvent, type ErrorSummary, type HostKeyChanged, type Phase, type Prompt, type SessionView } from "../api";
import { backend } from "../backend";
import { vault } from "./vault.svelte";

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

/** How long a notice stays (design system: 4 s). */
const TOAST_MS = 4000;

class Workspace {
  sessions: Record<number, SessionInfo> = $state({});
  tabs: Tab[] = $state([]);
  /** Active tab key; null shows the host list. */
  active: number | null = $state(null);
  prompts: Prompt[] = $state([]);
  alerts: Alert[] = $state([]);
  toast: (ErrorSummary & { tone: "error" | "info" }) | null = $state(null);
  #toastTimer: ReturnType<typeof setTimeout> | undefined;
  /** Last changed-key alert per session ("Show details" on the failure card). */
  #alertBySession = new Map<number, Alert>();
  /** End of terminals whose id the view does not know yet (the event can
   * overtake the `open_terminal` reply). */
  #earlyEnds = new Map<number, TabEnd>();
  /** Probe sessions (copy-id verification) by id → alias: never listed as
   * the host's session, but their prompts and alerts name the host. */
  #probes = new Map<number, string>();
  /** Callers waiting for a session to settle (connected / failed / closed). */
  #settle = new Map<number, ((error: ErrorSummary | null) => void)[]>();

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

  /** `ms`: longer for a notice that lists things (ignored settings). */
  notify(e: unknown, tone: "error" | "info" = "error", ms = TOAST_MS) {
    this.toast = { ...asError(e), tone };
    clearTimeout(this.#toastTimer);
    this.#toastTimer = setTimeout(() => (this.toast = null), ms);
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

  /** Opens a session without a tab (copy-id); prompts show as usual. */
  connectQuiet(alias: string): Promise<number | null> {
    return this.#connect(alias);
  }

  /** Resolves once session `id` is connected (null) or ended (its error). */
  settled(id: number): Promise<ErrorSummary | null> {
    const now = this.#outcome(this.sessions[id]?.phase);
    if (now !== undefined) return Promise.resolve(now);
    return new Promise((resolve) => {
      const list = this.#settle.get(id) ?? [];
      list.push(resolve);
      this.#settle.set(id, list);
    });
  }

  /** undefined while still connecting. */
  #outcome(phase: Phase | undefined): ErrorSummary | null | undefined {
    if (!phase) return { code: "session_ended", message: "the session ended", hint: null };
    switch (phase.phase) {
      case "connected":
        return null;
      case "failed":
        return phase.error;
      case "closed":
      case "disconnecting":
        return { code: "cancelled", message: "the connection was cancelled", hint: null };
      default:
        return undefined;
    }
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

  /** Reopens a shell in an ended or failed tab: same session if it is still
   * up, else the host's live session, else a new one. */
  async reconnect(key: number) {
    const tab = this.tabs.find((t) => t.key === key);
    if (!tab) return;
    if (!isLive(this.sessions[tab.session])) {
      const live = this.byAlias[tab.alias];
      const session = live && isLive(live) ? live.id : await this.#connect(tab.alias);
      if (session === null) return;
      if (!this.tabs.some((t) => t.key === key)) {
        // Closed meanwhile: do not leave the new session behind.
        if (!this.tabs.some((t) => t.session === session)) await backend.disconnect(session).catch(() => {});
        return;
      }
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
    // Idempotent; also releases a channel whose server never sent CLOSE.
    if (tab.terminal !== null) await backend.closeTerminal(tab.terminal).catch(() => {});
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

  /** Sends the answer; nothing here keeps a reference to a secret after
   * the call (JS strings cannot be wiped: the least we can do is let them
   * be collected as soon as the IPC has serialized them). */
  answer(prompt: Prompt, answer: Parameters<typeof backend.respondPrompt>[1]): Promise<void> {
    this.prompts = this.prompts.filter((p) => p.id !== prompt.id);
    const remembered = answer.kind === "secret" && answer.remember;
    return backend
      .respondPrompt(prompt.id, answer)
      .catch((e) => this.notify(e))
      .then(() => {
        // Without an unlocked vault the secret could not be remembered.
        if (remembered) vault.afterRemember();
      });
  }

  dismissAlert() {
    this.alerts.shift();
  }

  /** Shows a session's changed-key alert again. */
  showAlert(session: number) {
    const alert = this.#alertBySession.get(session);
    if (alert && !this.alerts.includes(alert)) this.alerts.push(alert);
  }

  hasAlert(session: number): boolean {
    return this.#alertBySession.has(session);
  }

  /** Called by a view once its terminal id is known. */
  attach(tab: Tab, terminal: number) {
    tab.terminal = terminal;
    const end = this.#earlyEnds.get(terminal);
    if (end) {
      this.#earlyEnds.delete(terminal);
      tab.end = end;
    }
  }

  aliasOf(session: number | null): string | undefined {
    return session === null ? undefined : (this.sessions[session]?.alias ?? this.#probes.get(session));
  }

  // ── Engine events ───────────────────────────────────────────────────

  onEvent(ev: EngineEvent) {
    switch (ev.type) {
      case "session": {
        const e = ev.event;
        if (e.kind === "started") {
          if (e.probe) this.#probes.set(ev.id, e.alias);
          else this.sessions[ev.id] ??= { id: ev.id, alias: e.alias, phase: { phase: "resolving" }, latencyMs: null };
          return;
        }
        const probe = this.#probes.get(ev.id);
        if (probe !== undefined) {
          // A changed host key is never filtered, whoever connected.
          if (e.kind === "host_key_changed") {
            const { kind: _kind, ...rest } = e;
            this.alerts.push({ ...rest, alias: probe });
          } else if (e.kind === "phase" && (e.phase === "closed" || e.phase === "failed")) {
            this.#probes.delete(ev.id);
          }
          return;
        }
        const s = this.sessions[ev.id];
        if (!s) return;
        if (e.kind === "phase") {
          const { kind: _kind, ...phase } = e;
          s.phase = phase;
          if (phase.phase !== "connected") s.latencyMs = null;
          const outcome = this.#outcome(phase);
          const waiting = this.#settle.get(ev.id);
          if (outcome !== undefined && waiting) {
            this.#settle.delete(ev.id);
            for (const resolve of waiting) resolve(outcome);
          }
        } else if (e.kind === "latency") {
          s.latencyMs = durationMs(e.rtt);
        } else if (e.kind === "host_key_changed") {
          const { kind: _kind, ...rest } = e;
          const alert = { ...rest, alias: s.alias };
          this.#alertBySession.set(ev.id, alert);
          this.alerts.push(alert);
        }
        return;
      }
      case "terminal": {
        const e = ev.event;
        if (e.kind === "opened") return;
        const tab = this.tabs.find((t) => t.terminal === ev.id);
        const end: TabEnd = e.kind === "exited" ? { kind: "exited", status: e.status, signal: e.signal } : { kind: "closed" };
        if (!tab) {
          // Possibly a view still waiting for its id: keep the first end.
          if (!this.#earlyEnds.has(ev.id)) this.#earlyEnds.set(ev.id, end);
          return;
        }
        if (e.kind === "exited") tab.end = end;
        else tab.end ??= end;
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
