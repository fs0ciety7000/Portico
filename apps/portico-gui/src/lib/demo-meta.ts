// Demo (browser preview only): host metadata, recent hosts, session logs,
// effective config and settings. Never used inside the desktop app.
//
// URL switches: `?keymap=custom` loads a keymap with two invalid entries
// (notice), `?theme=<id>` starts with a saved theme (an unknown one shows
// the fallback notice), `?recent=none` starts without recent hosts.
import type {
  EffectiveConfig,
  EngineEvent,
  ErrorSummary,
  Host,
  HostMeta,
  LogLevel,
  LogLine,
  RecentHost,
  SessionLogInfo,
  Settings,
} from "./api";

export interface MetaHooks {
  emit(e: EngineEvent): void;
  hosts: Host[];
}

const params = new URLSearchParams(location.search);
const fail = (code: string, message: string, hint: string | null = null): ErrorSummary => ({ code, message, hint });
const MINUTE = 60_000;
const NOW = Date.now();

/** Same rules as `portico_core::hosts::normalize_tags` (demo only). */
export function normalizeTags(tags: string[]): string[] {
  const out: string[] = [];
  tags.forEach((raw, i) => {
    const tag = raw.trim().replace(/^#/, "").trim();
    if (!tag) return;
    const hint = "Tags use letters, digits, `_`, `.` and `-`, up to 32 characters.";
    if (!/^[A-Za-z0-9_.-]+$/.test(tag)) {
      throw { ...fail("host_tag", `invalid tag ${i + 1}: use only letters, digits, underscore, dot and dash`, hint), position: i + 1 };
    }
    if (tag.length > 32) throw { ...fail("host_tag", `invalid tag ${i + 1}: longer than 32 characters`, hint), position: i + 1 };
    if (!out.some((t) => t.toLowerCase() === tag.toLowerCase())) out.push(tag);
  });
  if (out.length > 16) throw fail("host_tags", "too many tags (at most 16)", "Keep at most 16 tags per host.");
  return out;
}

interface DemoLogSession extends SessionLogInfo {
  lines: LogLine[];
}

export function demoMeta(hooks: MetaHooks) {
  const { emit, hosts } = hooks;
  let recent: RecentHost[] =
    params.get("recent") === "none"
      ? []
      : [
          { alias: "db-prod", last: NOW - 12 * MINUTE, count: 14 },
          { alias: "web-01", last: NOW - 95 * MINUTE, count: 31 },
          { alias: "decommissioned-01", last: NOW - 3 * 1440 * MINUTE, count: 2 },
          { alias: "grafana", last: NOW - 4 * 1440 * MINUTE, count: 5 },
        ];
  const logSessions: DemoLogSession[] = [];
  let theme: string | null = params.get("theme");
  let copyOnSelect = params.get("copy") === "on";
  const keymap: Record<string, string> =
    params.get("keymap") === "custom" ? { "app.files": "Mod+Shift+O", "tab.new": "Mod+W", "app.zoom": "Mod+=" } : {};

  // An earlier failed session and a copy-id probe, so the panel is not empty.
  const seed = (id: number, alias: string, probe: boolean, ago: number, lines: [LogLevel, string, string][], outcome: SessionLogInfo["outcome"]) => {
    const started = NOW - ago;
    logSessions.push({
      id,
      alias,
      probe,
      started,
      ended: started + 2400,
      outcome,
      lines: lines.map(([level, target, message], i) => ({ at: started + i * 180, level, target, message })),
    });
  };
  seed(
    9001,
    "staging",
    false,
    42 * MINUTE,
    [
      ["info", "config", "resolved staging.example.com:22 as deploy (~/.ssh/conf.d/staging:1)"],
      ["debug", "tcp", "connecting to 203.0.113.40:22"],
      ["info", "kex", "kex curve25519-sha256 · host key ssh-ed25519 · cipher chacha20-poly1305@openssh.com"],
      ["error", "known_hosts", "host key for staging.example.com has CHANGED (~/.ssh/known_hosts:14)"],
      ["warn", "session", "connection refused before authentication"],
    ],
    { kind: "failed", error: fail("host_key_changed", "host key for staging.example.com has CHANGED", "Do not connect. Verify the new key with the server administrator.") },
  );
  seed(
    9002,
    "web-01",
    true,
    30 * MINUTE,
    [
      ["info", "copy-id", "verifying login with id_ed25519_work"],
      ["debug", "auth", "offering public key SHA256:q2Xn8kD0bT1sV9cXr4mJ7yPwZ3aLhE5uGfN6oKiRtYs"],
      ["info", "auth", "authenticated with publickey"],
    ],
    { kind: "closed", reason: "user_requested" },
  );

  const sessionOf = (id: number) => logSessions.find((s) => s.id === id);

  function edit(alias: string, change: (h: Host) => void): HostMeta {
    const host = hosts.find((h) => h.alias === alias);
    if (!host) throw fail("host_alias", `invalid host alias: unknown`);
    change(host);
    const meta: HostMeta = { tags: [...host.tags], group: host.group, favorite: host.favorite, notes: host.notes };
    emit({ type: "host_meta", alias, meta });
    return meta;
  }

  return {
    /** Called by the demo engine: a session started / logged / ended. */
    sessionStarted(id: number, alias: string, probe = false) {
      logSessions.push({ id, alias, probe, started: Date.now(), ended: null, outcome: null, lines: [] });
    },
    log(id: number, level: LogLevel, target: string, message: string) {
      const s = sessionOf(id);
      if (!s) return;
      s.lines.push({ at: Date.now(), level, target, message });
      emit({ type: "log", session: id });
    },
    sessionEnded(id: number, outcome: SessionLogInfo["outcome"]) {
      const s = sessionOf(id);
      if (!s || s.ended !== null) return;
      s.ended = Date.now();
      s.outcome = outcome;
    },
    connected(alias: string) {
      const prev = recent.find((r) => r.alias === alias);
      recent = [{ alias, last: Date.now(), count: (prev?.count ?? 0) + 1 }, ...recent.filter((r) => r.alias !== alias)];
      emit({ type: "recent_changed" });
    },

    backend: {
      async setFavorite(alias: string, favorite: boolean) {
        return edit(alias, (h) => (h.favorite = favorite));
      },
      async checkTags(tags: string[]) {
        return normalizeTags(tags);
      },
      async setTags(alias: string, tags: string[]) {
        const clean = normalizeTags(tags);
        return edit(alias, (h) => (h.tags = clean));
      },
      async setGroup(alias: string, group: string | null) {
        const g = group?.trim() || null;
        if (g && g.length > 64) throw fail("host_group", "invalid group: longer than 64 characters", "A group is one line of at most 64 visible characters.");
        return edit(alias, (h) => (h.group = g));
      },
      async setNotes(alias: string, notes: string | null) {
        const n = notes?.trim() ? notes : null;
        if (n && n.length > 2000) throw fail("host_notes", "invalid notes: longer than 2000 characters", "Notes hold at most 2000 characters of plain text.");
        return edit(alias, (h) => (h.notes = n));
      },
      async recentHosts(limit: number) {
        return recent.slice(0, limit);
      },
      async clearRecent() {
        recent = [];
        emit({ type: "recent_changed" });
      },
      async forgetRecent(alias: string) {
        const before = recent.length;
        recent = recent.filter((r) => r.alias !== alias);
        if (recent.length !== before) emit({ type: "recent_changed" });
        return recent.length !== before;
      },
      async sessionsWithLogs(): Promise<SessionLogInfo[]> {
        return [...logSessions].sort((a, b) => a.started - b.started).map(({ lines: _lines, ...info }) => info);
      },
      async sessionLogs(session: number) {
        const s = sessionOf(session);
        if (!s) throw fail("unknown_session", "unknown session", null);
        return [...s.lines];
      },
      async sessionLogsText(session: number) {
        const s = sessionOf(session);
        if (!s) throw fail("unknown_session", "unknown session", null);
        return s.lines.map((l) => `${new Date(l.at).toISOString()} ${l.level.toUpperCase().padEnd(5)} ${l.target}: ${l.message}\n`).join("");
      },
      async effectiveConfig(alias: string): Promise<EffectiveConfig> {
        const host = hosts.find((h) => h.alias === alias);
        if (!host) throw fail("unknown_host", `no host named ${alias} in ~/.ssh/config`, "Reload the config: the host may have been renamed.");
        const line = Number(host.source.split(":").pop() ?? 1);
        const file = host.source.replace(/:\d+$/, "");
        const at = (n: number) => `${file}:${line + n}`;
        const route = [...host.proxyJump, host.alias].map((a) => {
          const h = hosts.find((x) => x.alias === a);
          return { alias: a, user: h?.user ?? "demo", hostname: h?.hostname ?? a, port: h?.port ?? 22 };
        });
        return {
          alias,
          options: [
            { key: "host", values: [alias], origin: at(0), command: false, sensitive: false },
            { key: "hostname", values: [host.hostname], origin: at(1), command: false, sensitive: false },
            { key: "user", values: [host.user ?? "demo"], origin: at(2), command: false, sensitive: false },
            { key: "port", values: [String(host.port)], origin: host.port === 22 ? null : at(3), command: false, sensitive: false },
            ...(host.proxyJump.length ? [{ key: "proxyjump", values: [host.proxyJump.join(",")], origin: at(4), command: false, sensitive: false }] : []),
            { key: "identityfile", values: ["~/.ssh/id_ed25519_work"], origin: "~/.ssh/config:40", command: false, sensitive: false },
            ...(alias === "staging" ? [{ key: "localcommand", values: [], origin: at(5), command: false, sensitive: true }] : []),
            { key: "serveraliveinterval", values: ["30"], origin: "~/.ssh/config:42", command: false, sensitive: false },
            { key: "stricthostkeychecking", values: ["ask"], origin: null, command: false, sensitive: false },
            { key: "connecttimeout", values: ["15"], origin: null, command: false, sensitive: false },
            { key: "serveralivecountmax", values: ["3"], origin: null, command: false, sensitive: false },
          ],
          route,
          routeError: null,
          warnings: alias === "staging" ? [{ origin: "~/.ssh/conf.d/staging:7", message: "Match exec is not supported: the block is ignored" }] : [],
        };
      },
      async getSettings(): Promise<Settings> {
        return { theme, keymap, copyOnSelect, error: null };
      },
      async saveTheme(id: string) {
        theme = id;
      },
      async saveCopyOnSelect(on: boolean) {
        copyOnSelect = on;
      },
      // The demo never opens anything: the dialog closes, a notice says so.
      async openUrl(_url: string) {
        throw { code: "demo_link", message: "links are not opened in the demo", hint: "In the desktop app the system browser opens it." };
      },
    },
  };
}
