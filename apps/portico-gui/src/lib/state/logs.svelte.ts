// Debug panel: sessions with logs (live and the last ended ones), the
// records of the selected one (refreshed live while the panel is open) and
// its effective configuration. Every text comes from Rust already escaped
// (`debug.rs`); the panel never renders raw engine events.
import type { EffectiveConfig, EngineEvent, ErrorSummary, LogLevel, LogLine, SessionLogInfo } from "../api";
import { asError } from "../api";
import { backend } from "../backend";

export type LogsTab = "logs" | "config";

const LEVELS: LogLevel[] = ["trace", "debug", "info", "warn", "error"];
export const levelRank = (l: LogLevel): number => LEVELS.indexOf(l);

/** Live refresh at most this often (a burst of records = one fetch). */
const REFRESH_MS = 250;

class Logs {
  open = $state(false);
  tab: LogsTab = $state("logs");
  sessions: SessionLogInfo[] = $state([]);
  sessionsError: ErrorSummary | null = $state(null);
  selected: number | null = $state(null);
  lines: LogLine[] = $state([]);
  /** True until the first fetch of the selected session's records. */
  loading = $state(false);
  error: ErrorSummary | null = $state(null);
  filter = $state("");
  minLevel: LogLevel = $state("debug");
  /** Keep the newest record in view. */
  follow = $state(true);
  config: EffectiveConfig | null = $state(null);
  configFor: string | null = $state(null);
  configLoading = $state(false);
  configError: ErrorSummary | null = $state(null);

  selectedInfo: SessionLogInfo | undefined = $derived(this.sessions.find((s) => s.id === this.selected));

  visible: LogLine[] = $derived.by(() => {
    const min = levelRank(this.minLevel);
    const q = this.filter.trim().toLowerCase();
    return this.lines.filter(
      (l) => levelRank(l.level) >= min && (!q || l.message.toLowerCase().includes(q) || l.target.toLowerCase().includes(q)),
    );
  });

  #linesTimer: ReturnType<typeof setTimeout> | undefined;
  #sessionsTimer: ReturnType<typeof setTimeout> | undefined;
  #request = 0;

  /** Opens the panel, on `session` when given (the active tab's). */
  async show(session: number | null = null) {
    this.open = true;
    this.filter = "";
    this.follow = true;
    await this.refreshSessions();
    const pick =
      (session !== null && this.sessions.some((s) => s.id === session) ? session : null) ??
      (this.selected !== null && this.sessions.some((s) => s.id === this.selected) ? this.selected : null) ??
      [...this.sessions].reverse().find((s) => !s.probe)?.id ??
      this.sessions[this.sessions.length - 1]?.id ??
      null;
    await this.select(pick);
  }

  close() {
    this.open = false;
    clearTimeout(this.#linesTimer);
    clearTimeout(this.#sessionsTimer);
    this.#linesTimer = this.#sessionsTimer = undefined;
  }

  async refreshSessions() {
    try {
      this.sessions = await backend.sessionsWithLogs();
      this.sessionsError = null;
    } catch (e) {
      this.sessionsError = asError(e);
    }
  }

  async select(id: number | null) {
    if (id !== this.selected) {
      this.selected = id;
      this.lines = [];
      this.follow = true;
    }
    if (id === null) return;
    this.loading = true;
    await this.loadLines();
    if (this.tab === "config") await this.loadConfig();
  }

  async loadLines() {
    const id = this.selected;
    if (id === null) return;
    const request = ++this.#request;
    try {
      const lines = await backend.sessionLogs(id);
      if (request !== this.#request) return;
      this.lines = lines;
      this.error = null;
    } catch (e) {
      if (request === this.#request) this.error = asError(e);
    } finally {
      if (request === this.#request) this.loading = false;
    }
  }

  async setTab(tab: LogsTab) {
    this.tab = tab;
    if (tab === "config") await this.loadConfig();
  }

  /** The effective config of the selected session's host (recomputed on demand). */
  async loadConfig(force = false) {
    const alias = this.selectedInfo?.alias ?? null;
    if (!alias || (!force && alias === this.configFor && this.config)) return;
    this.configFor = alias;
    this.configLoading = true;
    try {
      const config = await backend.effectiveConfig(alias);
      if (this.configFor !== alias) return;
      this.config = config;
      this.configError = null;
    } catch (e) {
      if (this.configFor === alias) {
        this.config = null;
        this.configError = asError(e);
      }
    } finally {
      if (this.configFor === alias) this.configLoading = false;
    }
  }

  /** "Copy all": the stable text form, built in Rust. */
  copyText(): Promise<string> {
    return this.selected === null ? Promise.resolve("") : backend.sessionLogsText(this.selected);
  }

  onEvent(ev: EngineEvent) {
    if (!this.open) return;
    if (ev.type === "log" && ev.session === this.selected) {
      this.#linesTimer ??= setTimeout(() => {
        this.#linesTimer = undefined;
        void this.loadLines();
      }, REFRESH_MS);
    } else if (ev.type === "session" && (ev.event.kind === "started" || ev.event.kind === "phase")) {
      this.#sessionsTimer ??= setTimeout(() => {
        this.#sessionsTimer = undefined;
        void this.refreshSessions();
      }, REFRESH_MS);
    }
  }
}

export const logs = new Logs();
