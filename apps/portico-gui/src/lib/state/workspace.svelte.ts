// Workspace state derived from engine events: sessions, terminal tabs and
// their panes, pending prompts and alerts. Components read it; user actions
// call the methods below, which call the backend.
//
// A tab holds a tree of panes (src/lib/panes.ts, ADR "Split panes" in
// docs/STACK.md); a pane is one terminal: one more PTY channel on a session
// (the same host's, or another host's). Exactly one pane per tab has the
// focus, and only it receives keystrokes.
import { asError, durationMs, type EngineEvent, type ErrorSummary, type HostKeyChanged, type Phase, type Prompt, type SessionView } from "../api";
import { backend } from "../backend";
import {
  layout,
  leaf,
  MAX_PANES,
  neighbor,
  nextPane,
  panesOf,
  preferredAxis,
  removePane,
  resizePane,
  setRatio,
  splitPane,
  toggleZoom,
  type Axis,
  type Direction,
  type Limits,
  type PaneNode,
  type Rect,
  type SplitRefusal,
} from "../panes";
import { vault } from "./vault.svelte";

export interface SessionInfo extends SessionView {
  alias: string;
}

export type TabEnd = { kind: "exited"; status: number | null; signal: string | null } | { kind: "closed" };

/** One terminal: a leaf of a tab's pane tree. */
export interface Pane {
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
  /** Bumped to (re)open a terminal in this pane. */
  generation: number;
}

/** A tab: a tree of panes, one of them focused, maybe one zoomed. */
export interface Tab {
  key: number;
  root: PaneNode;
  /** Key of the focused pane. */
  focus: number;
  /** Key of the pane that takes the whole tab, if any. */
  zoom: number | null;
}

/** Where panes are laid out, as measured by the stage (App.svelte). */
export interface PaneGeometry {
  rect: Rect;
  limits: Limits;
}

/** Why a split was refused, for the notice. */
const REFUSAL: Record<SplitRefusal, ErrorSummary> = {
  max_panes: {
    code: "pane_limit",
    message: `this tab already has ${MAX_PANES} panes`,
    hint: "Close a pane, or open a new tab.",
  },
  too_small: {
    code: "pane_too_small",
    message: "not enough room to split this pane",
    hint: "A pane needs at least 20 columns × 5 rows: enlarge the window, or split a larger pane.",
  },
  unknown_pane: { code: "pane_gone", message: "this pane is closed", hint: null },
};

export interface Alert extends HostKeyChanged {
  alias: string;
}

const LIVE: Phase["phase"][] = ["resolving", "connecting", "handshake", "verifying_host_key", "authenticating", "connected"];

export const isLive = (s: SessionInfo | undefined): boolean => !!s && LIVE.includes(s.phase.phase);

let nextKey = 1;

/** Severity of a notice: icon and border colour. */
export type ToastTone = "error" | "warning" | "info";

/** How long a notice stays (design system: 4 s). */
const TOAST_MS = 4000;

class Workspace {
  sessions: Record<number, SessionInfo> = $state({});
  tabs: Tab[] = $state([]);
  /** Every pane of every tab, by key. */
  panes: Record<number, Pane> = $state({});
  /** Active tab key; null shows the host list. */
  active: number | null = $state(null);
  prompts: Prompt[] = $state([]);
  alerts: Alert[] = $state([]);
  toast: (ErrorSummary & { tone: ToastTone }) | null = $state(null);
  /** Set by the stage once measured: splits check the room they leave. */
  geometry: PaneGeometry | null = null;
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
  /** The focused pane of the active tab. */
  activePane: Pane | undefined = $derived(this.activeTab ? this.panes[this.activeTab.focus] : undefined);

  /** `ms`: longer for a notice that lists things (ignored settings). */
  notify(e: unknown, tone: ToastTone = "error", ms = TOAST_MS) {
    this.toast = { ...asError(e), tone };
    clearTimeout(this.#toastTimer);
    this.#toastTimer = setTimeout(() => (this.toast = null), ms);
  }

  dismissToast() {
    clearTimeout(this.#toastTimer);
    this.toast = null;
  }

  /** Panes of a tab, in reading order. */
  panesIn(tab: Tab): Pane[] {
    return panesOf(tab.root).flatMap((k) => {
      const p = this.panes[k];
      return p ? [p] : [];
    });
  }

  /** The focused pane of a tab. */
  focusOf(tab: Tab): Pane | undefined {
    return this.panes[tab.focus];
  }

  tabOf(pane: number): Tab | undefined {
    return this.tabs.find((t) => panesOf(t.root).includes(pane));
  }

  /** True while some pane shows a terminal of `session`. */
  usesSession(session: number): boolean {
    return Object.values(this.panes).some((p) => p.session === session);
  }

  // ── Tabs ────────────────────────────────────────────────────────────

  /** Enter on a host: focus its live pane (and tab), or open a tab (reusing a live session). */
  async open(alias: string) {
    const live = Object.values(this.panes).find((p) => p.alias === alias && !p.end && isLive(this.sessions[p.session]));
    const tab = live ? this.tabOf(live.key) : undefined;
    if (live && tab) {
      this.active = tab.key;
      this.focusPane(live.key);
      return;
    }
    await this.#newTab(alias);
  }

  /** New tab on the same host as the focused pane. */
  async duplicate() {
    const pane = this.activePane;
    if (pane) await this.#newTab(pane.alias);
  }

  /** The host's live session, else a new connection (null: failed, notified). */
  async #sessionFor(alias: string): Promise<number | null> {
    const current = this.byAlias[alias];
    return isLive(current) && current ? current.id : await this.#connect(alias);
  }

  #newPane(alias: string, session: number): Pane {
    const key = nextKey++;
    const pane: Pane = { key, alias, session, terminal: null, title: "", end: null, error: null, generation: 0 };
    this.panes[key] = pane;
    // The proxied object: the view's mutations must be seen.
    return this.panes[key] ?? pane;
  }

  async #newTab(alias: string) {
    const session = await this.#sessionFor(alias);
    if (session === null) return;
    const pane = this.#newPane(alias, session);
    this.tabs.push({ key: nextKey++, root: leaf(pane.key), focus: pane.key, zoom: null });
    this.active = this.tabs[this.tabs.length - 1]?.key ?? null;
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

  /** Reopens a shell in an ended or failed pane: same session if it is
   * still up, else the host's live session, else a new one. */
  async reconnect(key: number) {
    const pane = this.panes[key];
    if (!pane) return;
    if (!isLive(this.sessions[pane.session])) {
      const live = this.byAlias[pane.alias];
      const session = live && isLive(live) ? live.id : await this.#connect(pane.alias);
      if (session === null) return;
      if (!this.panes[key]) {
        // Closed meanwhile: do not leave the new session behind.
        if (!this.usesSession(session)) await backend.disconnect(session).catch(() => {});
        return;
      }
      pane.session = session;
    }
    pane.terminal = null;
    pane.end = null;
    pane.error = null;
    // The title belonged to the previous shell (OSC 0/2): a new one starts untitled.
    pane.title = "";
    pane.generation++;
  }

  /** Closes a tab and all its panes. */
  async close(key: number | undefined = this.active ?? undefined) {
    const index = this.tabs.findIndex((t) => t.key === key);
    const tab = this.tabs[index];
    if (!tab) return;
    this.tabs.splice(index, 1);
    if (this.active === key) {
      const next = this.tabs[Math.min(index, this.tabs.length - 1)];
      this.active = next ? next.key : null;
    }
    await this.#release(panesOf(tab.root));
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

  // ── Panes ───────────────────────────────────────────────────────────

  /**
   * Splits the focused pane: a new terminal on the same session (one more
   * PTY channel on the same transport), or on `alias`'s session. `axis`
   * null: the axis that gives the larger halves, else the other one. A
   * refusal (8 panes, too small) is notified before anything is opened.
   */
  async split(axis: Axis | null, alias?: string) {
    const tab = this.activeTab;
    const base = this.activePane;
    if (!tab || !base) return;
    const target = base.key;
    const refusal = this.#trySplit(tab, target, this.#axes(tab, target, axis), null);
    if (refusal) return this.notify(REFUSAL[refusal], "warning");
    const host = alias ?? base.alias;
    const session = !alias && isLive(this.sessions[base.session]) ? base.session : await this.#sessionFor(host);
    if (session === null) return;
    // The tab or the pane may have gone (or the window shrunk) while connecting.
    const now = this.tabs.find((t) => t.key === tab.key);
    const failed = now && panesOf(now.root).includes(target) ? this.#trySplit(now, target, this.#axes(now, target, axis), { alias: host, session }) : "unknown_pane";
    if (failed) {
      if (!this.usesSession(session) && isLive(this.sessions[session])) await backend.disconnect(session).catch(() => {});
      if (failed !== "unknown_pane") this.notify(REFUSAL[failed], "warning");
    }
  }

  /** Axes to try, in order. */
  #axes(tab: Tab, target: number, axis: Axis | null): Axis[] {
    if (axis) return [axis];
    const geo = this.geometry;
    // The pane's own shape decides (unzoomed layout).
    const at = geo ? layout(tab.root, geo.rect, geo.limits.gap).panes.get(target) : undefined;
    const preferred = at ? preferredAxis(at) : "row";
    return [preferred, preferred === "row" ? "column" : "row"];
  }

  /**
   * Splits with the first axis that fits, adding a pane for `add`, and
   * focuses it; `add` null only checks. Returns the refusal otherwise.
   */
  #trySplit(tab: Tab, target: number, axes: Axis[], add: { alias: string; session: number } | null): SplitRefusal | null {
    const geo = this.geometry;
    // Not measured yet: only the pane count is checked.
    const rect = geo?.rect ?? { x: 0, y: 0, w: Number.MAX_SAFE_INTEGER, h: Number.MAX_SAFE_INTEGER };
    const limits = geo?.limits ?? { maxPanes: MAX_PANES, min: { w: 0, h: 0 }, gap: 0 };
    let refusal: SplitRefusal | null = null;
    for (const axis of axes) {
      const r = splitPane(tab.root, target, axis, add ? nextKey : -1, nextKey + 1, rect, limits);
      if (r.ok) {
        if (add) {
          const pane = this.#newPane(add.alias, add.session);
          nextKey++; // the split's id
          tab.root = r.root;
          tab.focus = pane.key;
          tab.zoom = null;
        }
        return null;
      }
      refusal ??= r.reason;
      if (r.reason !== "too_small") break;
    }
    return refusal;
  }

  /** Gives the focus to a pane (click, focus commands); a zoom follows it. */
  focusPane(pane: number) {
    const tab = this.tabOf(pane);
    if (!tab || tab.focus === pane) return;
    tab.focus = pane;
    if (tab.zoom !== null) tab.zoom = pane;
  }

  /** Focus to the pane next to the focused one in `dir`. */
  focusDirection(dir: Direction) {
    const tab = this.activeTab;
    const geo = this.geometry;
    if (!tab || !geo) return;
    const next = neighbor(tab.root, tab.focus, dir, geo.rect, geo.limits.gap);
    if (next !== null) this.focusPane(next);
  }

  focusNext(delta = 1) {
    const tab = this.activeTab;
    if (tab) this.focusPane(nextPane(tab.root, tab.focus, delta));
  }

  /** Keyboard resize around the focused pane (moves the nearest divider). */
  resize(dir: Direction) {
    const tab = this.activeTab;
    const geo = this.geometry;
    if (!tab || !geo || tab.zoom !== null) return;
    tab.root = resizePane(tab.root, tab.focus, dir, geo.rect, geo.limits);
  }

  /** Divider drag (or its arrow keys), clamped to the minimum pane size. */
  setRatio(split: number, ratio: number) {
    const tab = this.activeTab;
    const geo = this.geometry;
    if (!tab || !geo) return;
    tab.root = setRatio(tab.root, split, ratio, geo.rect, geo.limits);
  }

  toggleZoom() {
    const tab = this.activeTab;
    if (tab) tab.zoom = toggleZoom(tab.root, tab.zoom, tab.focus);
  }

  /**
   * Closes a pane and its PTY channel; its sibling takes the room. The
   * last pane closes the tab; the last pane of a session disconnects it.
   */
  async closePane(key: number | undefined = this.activePane?.key) {
    if (key === undefined) return;
    const tab = this.tabOf(key);
    if (!tab) return;
    const root = removePane(tab.root, key);
    if (!root) return this.close(tab.key);
    const order = panesOf(tab.root);
    tab.root = root;
    if (tab.focus === key) {
      // The pane before it in reading order, else the first one.
      const left = panesOf(root);
      const before = order.slice(0, order.indexOf(key)).reverse();
      tab.focus = before.find((k) => left.includes(k)) ?? left[0] ?? key;
    }
    if (tab.zoom === key) tab.zoom = null;
    await this.#release([key]);
  }

  /** Forgets panes, closes their terminals, disconnects the sessions left without a pane. */
  async #release(keys: number[]) {
    const gone = keys.flatMap((k) => {
      const p = this.panes[k];
      return p ? [{ terminal: p.terminal, session: p.session }] : [];
    });
    for (const k of keys) delete this.panes[k];
    for (const p of gone) {
      // Idempotent; also releases a channel whose server never sent CLOSE.
      if (p.terminal !== null) await backend.closeTerminal(p.terminal).catch(() => {});
    }
    // The last pane of a session disconnects it (also cancels a connection in flight).
    for (const session of new Set(gone.map((p) => p.session))) {
      if (!this.usesSession(session) && isLive(this.sessions[session])) {
        await backend.disconnect(session).catch((e) => this.notify(e));
      }
    }
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
  attach(pane: Pane, terminal: number) {
    pane.terminal = terminal;
    const end = this.#earlyEnds.get(terminal);
    if (end) {
      this.#earlyEnds.delete(terminal);
      pane.end = end;
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
        const pane = Object.values(this.panes).find((p) => p.terminal === ev.id);
        const end: TabEnd = e.kind === "exited" ? { kind: "exited", status: e.status, signal: e.signal } : { kind: "closed" };
        if (!pane) {
          // Possibly a view still waiting for its id: keep the first end.
          if (!this.#earlyEnds.has(ev.id)) this.#earlyEnds.set(ev.id, end);
          return;
        }
        if (e.kind === "exited") pane.end = end;
        else pane.end ??= end;
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
