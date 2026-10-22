// Simulated engine so the UI can be designed in a plain browser (`pnpm dev`)
// without the Tauri runtime. Never used inside the desktop app.
//
// Scenarios: `bastion` connects through the agent, `web-01` asks to trust a
// new host key, `db-prod` goes through a jump host and asks for a password
// (anything but "wrong" works), `staging` presents a changed host key.
import type { UnlistenFn } from "@tauri-apps/api/event";
import type { EngineEvent, ErrorSummary, Host, HostOrigin, Phase, PromptAnswer, PromptKind, RemoteEntry, ThemeInfo, TunnelInfo, Upload } from "./api";
import type { Backend } from "./backend";
import { demoHostsBackend, demoPorticoHosts } from "./demo-hosts";
import { demoKeys } from "./demo-keys";
import { demoMeta } from "./demo-meta";
import { demoImport } from "./demo-import";
import { demoRecording } from "./demo-recording";

const base = {
  identityFiles: [],
  localForwards: [],
  remoteForwards: [],
  dynamicForwards: [] as string[],
  warnings: [],
  group: null as string | null,
  notes: null as string | null,
  origin: "ssh_config" as HostOrigin,
  managed: null,
};

// `?groups=none`: no group (with `?recent=none`, the flat list of earlier phases).
const grouped = new URLSearchParams(location.search).get("groups") !== "none";
const group = (name: string) => (grouped ? name : null);

const created = demoPorticoHosts();

export const demoHosts: Host[] = [
  // hosts.conf is read before ~/.ssh/config (as if included at its top).
  ...created.filter((h) => h.origin === "portico"),
  {
    ...base,
    alias: "db-prod",
    hostname: "10.0.4.12",
    user: "postgres",
    port: 22,
    proxyJump: ["bastion"],
    tags: ["prod", "db"],
    favorite: true,
    group: group("Production"),
    notes: "Primary PostgreSQL 16. Maintenance window: Sunday 02:00–04:00 UTC.\nPage the DBA on call before any restart.",
    source: "~/.ssh/config:12",
  },
  { ...base, alias: "bastion", hostname: "bastion.example.com", user: "ops", port: 2222, proxyJump: [], tags: ["infra"], favorite: false, group: group("Infrastructure"), source: "~/.ssh/config:1", dynamicForwards: ["1080"] },
  { ...base, alias: "web-01", hostname: "web-01.internal", user: "deploy", port: 22, proxyJump: ["bastion"], tags: ["prod", "web"], favorite: false, group: group("Production"), source: "~/.ssh/config:5", localForwards: ["8080 localhost:80"] },
  { ...base, alias: "web-02", hostname: "web-02.internal", user: "deploy", port: 22, proxyJump: ["bastion"], tags: ["prod", "web"], favorite: false, group: group("Production"), source: "~/.ssh/config:8" },
  { ...base, alias: "grafana", hostname: "grafana.internal", user: "ops", port: 22, proxyJump: ["bastion"], tags: ["monitoring"], favorite: true, group: group("Infrastructure"), source: "~/.ssh/config:20" },
  { ...base, alias: "staging", hostname: "staging.example.com", user: "deploy", port: 22, proxyJump: [], tags: ["staging"], favorite: false, source: "~/.ssh/conf.d/staging:1" },
  { ...base, alias: "lab-pi", hostname: "192.168.1.40", user: "pi", port: 22, proxyJump: [], tags: ["home"], favorite: false, source: "~/.ssh/config:30" },
  // `?long=1`: a host with a long alias (ellipsis in the list, wrapped title).
  ...(new URLSearchParams(location.search).get("long") === "1"
    ? [{ ...base, alias: "analytics-warehouse-replica-eu-west-3-standby", hostname: "10.0.7.31", user: "analyst", port: 22, proxyJump: ["bastion"], tags: ["analytics"], favorite: true, group: group("Production"), source: "~/.ssh/config:44" }]
    : []),
  // The Termius import file, included at the end of ~/.ssh/config.
  ...created.filter((h) => h.origin === "imported"),
];

export const demoThemes: ThemeInfo[] = [
  { id: "obsidian", name: "Obsidian", description: "Near-black mineral surfaces, marble text, brushed-brass accent." },
  { id: "basalt", name: "Basalt", description: "Cool volcanic grey-blue, steel accent." },
  { id: "travertine", name: "Travertine Night", description: "Warm dark stone, terracotta accent." },
];

type Listener = (e: EngineEvent) => void;
const listeners = new Set<Listener>();
const emit = (e: EngineEvent) => queueMicrotask(() => listeners.forEach((l) => l(e)));
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));

let nextId = 1;
const pending = new Map<number, (a: PromptAnswer) => void>();

interface DemoSession {
  alias: string;
  connected: boolean;
  closed: boolean;
  waiters: ((ok: boolean) => void)[];
}
const sessions = new Map<number, DemoSession>();

interface DemoTerminal {
  session: number;
  host: Host;
  onData: (b: Uint8Array) => void;
  line: string;
}
const terminals = new Map<number, DemoTerminal>();
const enc = new TextEncoder();

function phase(id: number, p: Phase) {
  emit({ type: "session", id, event: { kind: "phase", ...p } });
  if (p.phase === "failed") meta.sessionEnded(id, { kind: "failed", error: p.error });
  else if (p.phase === "closed") meta.sessionEnded(id, { kind: "closed", reason: p.reason as "user_requested" });
}

function ask(session: number, kind: PromptKind): Promise<PromptAnswer> {
  const id = nextId++;
  emit({ type: "prompt", id, session, kind });
  return new Promise((resolve) => pending.set(id, resolve));
}

function fail(id: number, error: ErrorSummary) {
  const s = sessions.get(id);
  if (s) {
    s.closed = true;
    s.waiters.splice(0).forEach((w) => w(false));
  }
  phase(id, { phase: "failed", error });
}

async function run(id: number, host: Host) {
  const s = sessions.get(id);
  if (!s) return;
  phase(id, { phase: "resolving" });
  meta.log(id, "info", "config", `resolved ${host.hostname}:${host.port} as ${host.user ?? "user"} (${host.source})`);
  await sleep(250);
  const hops = host.proxyJump.length + 1;
  for (let hop = 1; hop <= hops; hop++) {
    const target = hop < hops ? (host.proxyJump[hop - 1] ?? "") : host.alias;
    phase(id, { phase: "connecting", hop, hops, target });
    meta.log(id, "debug", hops > 1 ? "jump" : "tcp", hops > 1 ? `hop ${hop}/${hops}: opening ${target}` : `connecting to ${host.hostname}:${host.port}`);
    await sleep(450);
  }
  phase(id, { phase: "handshake" });
  meta.log(id, "info", "kex", "kex curve25519-sha256 · host key ssh-ed25519 · cipher chacha20-poly1305@openssh.com");
  await sleep(250);
  phase(id, { phase: "verifying_host_key" });
  meta.log(id, "debug", "known_hosts", `checking ${host.hostname} in ~/.ssh/known_hosts`);
  await sleep(300);
  if (host.alias === "staging") {
    emit({
      type: "session",
      id,
      event: {
        kind: "host_key_changed",
        host: host.hostname,
        port: host.port,
        algorithm: "ssh-ed25519",
        fingerprint: "SHA256:0y5GvtXOmKA2vClfRftsHeW8EJw3nIcc2nhnFInw5aw",
        expected_fingerprint: "SHA256:EI4OTHhovxsOvDDQdV9+C7hTMLB4P0osWS0GNVqL/mo",
        known_hosts_file: "~/.ssh/known_hosts",
        known_hosts_line: 14,
      },
    });
    return fail(id, {
      code: "host_key_changed",
      message: `host key for ${host.hostname} has CHANGED`,
      hint: "Do not connect. Verify the new key with the server administrator; if legitimate, remove the old line from known_hosts.",
    });
  }
  if (host.alias === "web-01") {
    const a = await ask(id, {
      kind: "unknown_host_key",
      host: host.hostname,
      port: host.port,
      algorithm: "ssh-ed25519",
      fingerprint: "SHA256:q2Xn8kD0bT1sV9cXr4mJ7yPwZ3aLhE5uGfN6oKiRtYs",
      other_algorithms: [],
    });
    if (a.kind !== "accept_host_key") {
      return fail(id, { code: "host_key_rejected", message: `host key for ${host.hostname} was not trusted`, hint: null });
    }
  }
  if (host.alias === "db-prod") {
    phase(id, { phase: "authenticating", method: "password" });
    for (let attempt = 1; ; attempt++) {
      const a = await ask(id, { kind: "password", user: host.user ?? "user", host: host.hostname, attempt, attempts: 3 });
      if (a.kind !== "secret") {
        s.closed = true;
        s.waiters.splice(0).forEach((w) => w(false));
        return phase(id, { phase: "closed", reason: "user_requested" });
      }
      if (a.value !== "wrong") break;
      if (attempt === 3) {
        return fail(id, {
          code: "auth_failed",
          message: `authentication failed for ${host.user}@${host.hostname}`,
          hint: "Check User, IdentityFile and your agent (ssh-add -l).",
        });
      }
    }
  } else {
    phase(id, { phase: "authenticating", method: "agent" });
    meta.log(id, "debug", "auth", "offering agent key SHA256:q2Xn8kD0bT1sV9cXr4mJ7yPwZ3aLhE5uGfN6oKiRtYs (id_ed25519_work)");
    await sleep(400);
  }
  meta.log(id, "info", "auth", host.alias === "db-prod" ? "authenticated with password" : "authenticated with publickey (agent)");
  meta.log(id, "warn", "banner", "server banner: \\r\\nAuthorized use only\\u{202e}");
  s.connected = true;
  phase(id, { phase: "connected" });
  meta.connected(host.alias);
  meta.log(id, "info", "session", "connected");
  s.waiters.splice(0).forEach((w) => w(true));
  const rtt = host.alias === "db-prod" ? 312 : 18;
  emit({ type: "session", id, event: { kind: "latency", rtt: { secs: 0, nanos: rtt * 1e6 } } });
}

function promptOf(t: DemoTerminal): string {
  return `\x1b[38;5;108m${t.host.user ?? "user"}@${t.host.alias}\x1b[0m:\x1b[38;5;110m~\x1b[0m$ `;
}

function exec(id: number, t: DemoTerminal, cmd: string) {
  const out = (s: string) => t.onData(enc.encode(s));
  const word = cmd.trim().split(/\s+/)[0] ?? "";
  switch (word) {
    case "":
      break;
    case "ls":
      out("\x1b[1;34mapp\x1b[0m  \x1b[1;34mlogs\x1b[0m  deploy.sh  README.md  \x1b[32mrestart\x1b[0m\r\n");
      break;
    case "colors":
      for (let i = 0; i < 16; i++) out(`\x1b[48;5;${i}m  ${String(i).padStart(2)}  \x1b[0m${i === 7 ? "\r\n" : ""}`);
      out("\r\n");
      break;
    case "df":
      out("Filesystem      Size  Used Avail Use% Mounted on\r\n/dev/vda1        78G   31G   44G  42% /\r\ntmpfs           3.9G     0  3.9G   0% /dev/shm\r\n");
      break;
    case "uptime":
      out(" 17:42:08 up 41 days,  3:12,  1 user,  load average: 0.08, 0.11, 0.09\r\n");
      break;
    case "flood":
      for (let i = 0; i < 40; i++) out(`\x1b[2m${String(i).padStart(4, "0")}\x1b[0m GET /api/orders?page=${i} 200 ${12 + (i % 7)} ms\r\n`);
      rec.flood(id);
      break;
    case "exit":
      out("logout\r\n");
      terminals.delete(id);
      rec.terminalClosed(id);
      emit({ type: "terminal", id, session: t.session, event: { kind: "exited", status: 0, signal: null } });
      emit({ type: "terminal", id, session: t.session, event: { kind: "closed" } });
      return;
    default:
      out(`-bash: ${word}: command not found\r\n`);
  }
  out(promptOf(t));
}

// ── Files ────────────────────────────────────────────────────────────────

const DAY = 86_400;
const now = Math.floor(Date.UTC(2026, 9, 21, 9, 30) / 1000);
type DemoNode = { kind: RemoteEntry["kind"]; size: number; modified: number };
const tree = new Map<string, DemoNode>();
function put(path: string, kind: RemoteEntry["kind"], size = 0, age = 0) {
  tree.set(path, { kind, size, modified: now - age });
}
for (const home of ["/home/deploy", "/home/ops", "/var/lib/postgresql"]) {
  put(home, "dir");
  put(`${home}/app`, "dir", 0, DAY * 2);
  put(`${home}/app/releases`, "dir", 0, DAY * 2);
  put(`${home}/app/current`, "symlink", 0, DAY * 2);
  put(`${home}/app/package.json`, "file", 1_874, DAY * 9);
  put(`${home}/logs`, "dir", 0, 3600);
  put(`${home}/logs/app.log`, "file", 12_582_912, 120);
  put(`${home}/logs/error.log`, "file", 348_160, 3600);
  put(`${home}/backup-2026-10-19.tar.gz`, "file", 505_413_632, DAY * 2);
  put(`${home}/config.yml`, "file", 1_203, DAY * 30);
  put(`${home}/deploy.sh`, "file", 2_150, DAY * 400);
  put(`${home}/README.md`, "file", 4_096, DAY * 400);
  put(`${home}/.bashrc`, "file", 3_771, DAY * 900);
}
for (const dir of ["/", "/home", "/var", "/var/lib", "/root", "/etc"]) put(dir, "dir", 0, DAY * 120);

const demoHomes: Record<string, string> = { "db-prod": "/var/lib/postgresql", bastion: "/home/ops" };
const notFound = (path: string): ErrorSummary => ({ code: "not_found", message: `${path}: no such file or directory`, hint: null });
const parentOf = (path: string) => (path === "/" ? "" : path.replace(/\/[^/]+$/, "") || "/");
const nameOf = (path: string) => path.split("/").pop() ?? path;

function list(path: string): RemoteEntry[] {
  if (path === "/root") {
    throw { code: "permission_denied", message: "/root: permission denied", hint: "Your user cannot read this folder." } satisfies ErrorSummary;
  }
  if (tree.get(path)?.kind !== "dir") throw notFound(path);
  const out: RemoteEntry[] = [];
  for (const [p, n] of tree) {
    if (p !== path && parentOf(p) === path) {
      const name = nameOf(p);
      out.push({ name, display: name, path: p, kind: n.kind, size: n.size, mode: n.kind === "dir" ? 0o755 : 0o644, modified: n.modified });
    }
  }
  return out.sort((a, b) => Number(b.kind === "dir") - Number(a.kind === "dir") || a.name.localeCompare(b.name));
}

const uploads = new Map<number, { name: string; size: number }>();
const transfers = new Map<number, { cancelled: boolean }>();
const local = new Set<string>(["config.yml"]);

async function simulate(session: number, direction: "upload" | "download", name: string, total: number, onConflict: string, done: (final: string) => void) {
  const id = nextId++;
  const sftp = 1;
  const send = (event: Extract<EngineEvent, { type: "sftp" }>["event"]) => emit({ type: "sftp", id: sftp, session, event });
  transfers.set(id, { cancelled: false });
  void (async () => {
    await sleep(60);
    const exists = direction === "download" ? local.has(name) : false;
    if (exists && onConflict === "ask") {
      send({ kind: "transfer_failed", transfer: id, error: { code: "destination_exists", message: `${name} already exists`, hint: null } });
      return;
    }
    if (exists && onConflict === "skip") return send({ kind: "transfer_skipped", transfer: id });
    const final = exists && onConflict === "rename" ? name.replace(/(\.[^.]*)?$/, " (1)$1") : name;
    send({ kind: "transfer_started", transfer: id, direction, name, total });
    // ~6 s for the large archive, instant for small files.
    const steps = total > 50_000_000 ? 60 : 4;
    for (let i = 1; i <= steps; i++) {
      await sleep(100);
      if (transfers.get(id)?.cancelled) return send({ kind: "transfer_cancelled", transfer: id });
      send({ kind: "transfer_progress", transfer: id, done: Math.round((total * i) / steps), total });
    }
    done(final);
    send({ kind: "transfer_finished", transfer: id, destination: final });
  })();
  return id;
}

let downloadDir = "/home/demo/Downloads";
const tunnels = new Map<number, TunnelInfo>();
let dropListener: ((files: Upload[]) => void) | null = null;
let dragListener: ((dragging: boolean) => void) | null = null;
let installed = false;

/** Starts the simulation (once): the only side effects of this module.
 * Called by `backend.ts` in development or a `--mode demo` build only. */
export function installDemo(): Backend {
  if (installed) return demoBackend;
  installed = true;
  // Browser preview: an HTML5 drop stands in for the native one.
  window.addEventListener("dragover", (e) => {
    e.preventDefault();
    dragListener?.(true);
  });
  window.addEventListener("dragleave", (e) => {
    if (!e.relatedTarget) dragListener?.(false);
  });
  window.addEventListener("drop", (e) => {
    e.preventDefault();
    dragListener?.(false);
    const files = [...(e.dataTransfer?.files ?? [])].map((f) => {
      const token = nextId++;
      uploads.set(token, { name: f.name, size: f.size });
      return { token, display: f.name, size: f.size };
    });
    if (files.length) dropListener?.(files);
  });
  return demoBackend;
}

const keyDemo = demoKeys({
  emit,
  ask,
  sleep: (ms) => sleep(ms).then(() => {}),
  nextId: () => nextId++,
  aliasOf: (session) => sessions.get(session)?.alias,
  tagsOf: (alias) => demoHosts.find((h) => h.alias === alias)?.tags ?? [],
  type(terminal, data) {
    if (!terminals.has(terminal)) return false;
    void demoBackend.terminalInput(terminal, data, false);
    return true;
  },
});

const meta = demoMeta({ emit, hosts: demoHosts });

const rec = demoRecording({
  emit,
  terminal: (id) => {
    const t = terminals.get(id);
    return t ? { alias: t.host.alias, session: t.session } : undefined;
  },
});

/** Rough stand-in for portico-core's search: words, `#tag`, `@group`, `*`. */
function demoSearch(q: string): Host[] {
  const words = q.toLowerCase().split(/\s+/).filter(Boolean);
  return demoHosts.filter((h) =>
    words.every((w) => {
      if (w === "*") return h.favorite;
      if (w.startsWith("#")) return h.tags.some((t) => t.toLowerCase().startsWith(w.slice(1)));
      if (w.startsWith("@")) return (h.group ?? "").toLowerCase().includes(w.slice(1));
      return `${h.alias} ${h.hostname} ${h.user ?? ""}`.toLowerCase().includes(w);
    }),
  );
}

const managed = demoHostsBackend({ emit, hosts: demoHosts, keys: () => keyDemo.listKeys() });

export const demoBackend: Backend = {
  ...keyDemo,
  ...meta.backend,
  ...demoImport,
  ...rec.backend,
  ...managed,
  // Copies: the UI's state never aliases the simulation's own objects.
  listHosts: async () => demoHosts.map((h) => structuredClone(h)),
  searchHosts: async (q) => demoSearch(q),
  listThemes: async () => demoThemes,
  homeDir: async () => "/home/demo",
  async connect(alias) {
    const host = demoHosts.find((h) => h.alias === alias);
    if (!host) throw { code: "unknown_host", message: `no host named ${alias}`, hint: null } satisfies ErrorSummary;
    const id = nextId++;
    sessions.set(id, { alias, connected: false, closed: false, waiters: [] });
    meta.sessionStarted(id, alias);
    emit({ type: "session", id, event: { kind: "started", alias } });
    void run(id, host);
    return id;
  },
  async disconnect(id) {
    const s = sessions.get(id);
    if (!s || s.closed) return;
    s.closed = true;
    s.waiters.splice(0).forEach((w) => w(false));
    for (const [tid, t] of terminals) {
      if (t.session === id) {
        terminals.delete(tid);
        rec.terminalClosed(tid);
        emit({ type: "terminal", id: tid, session: id, event: { kind: "closed" } });
      }
    }
    phase(id, { phase: "disconnecting" });
    await sleep(150);
    phase(id, { phase: "closed", reason: "user_requested" });
  },
  async respondPrompt(id, answer) {
    const resolve = pending.get(id);
    pending.delete(id);
    emit({ type: "prompt_resolved", id });
    resolve?.(answer);
  },
  async openTerminal(session, _size, onData) {
    const s = sessions.get(session);
    if (!s || s.closed) throw { code: "session_ended", message: "session ended", hint: null } satisfies ErrorSummary;
    if (!s.connected && !(await new Promise<boolean>((r) => s.waiters.push(r)))) {
      throw { code: "session_ended", message: "session ended", hint: null } satisfies ErrorSummary;
    }
    const host = demoHosts.find((h) => h.alias === s.alias);
    if (!host) throw { code: "unknown_host", message: "unknown host", hint: null } satisfies ErrorSummary;
    const id = nextId++;
    const t: DemoTerminal = {
      session,
      host,
      // Output counts towards a demo recording's size.
      onData: (b) => {
        rec.output(id, b.length);
        onData(b);
      },
      line: "",
    };
    terminals.set(id, t);
    setTimeout(() => {
      onData(enc.encode(`\x1b]0;${host.user}@${host.alias}: ~\x07`));
      onData(enc.encode(`Welcome to Ubuntu 24.04.1 LTS (GNU/Linux 6.8.0-45-generic x86_64)\r\n\r\n`));
      onData(enc.encode(`Last login: Tue Oct 20 17:41:55 2026 from 10.0.0.2\r\n${promptOf(t)}`));
    }, 30);
    return id;
  },
  async terminalInput(id, data) {
    const t = terminals.get(id);
    if (!t) return;
    for (const ch of data) {
      if (ch === "\r") {
        t.onData(enc.encode("\r\n"));
        const cmd = t.line;
        t.line = "";
        exec(id, t, cmd);
      } else if (ch === "\x7f") {
        if (t.line) {
          t.line = t.line.slice(0, -1);
          t.onData(enc.encode("\b \b"));
        }
      } else if (ch === "\x03") {
        t.line = "";
        t.onData(enc.encode(`^C\r\n${promptOf(t)}`));
      } else if (ch >= " ") {
        t.line += ch;
        t.onData(enc.encode(ch));
      }
    }
  },
  terminalResize: async () => {},
  terminalAck: async () => {},
  async closeTerminal(id) {
    const t = terminals.get(id);
    if (!t) return;
    terminals.delete(id);
    rec.terminalClosed(id);
    emit({ type: "terminal", id, session: t.session, event: { kind: "closed" } });
  },
  async onEngineEvent(cb): Promise<UnlistenFn> {
    listeners.add(cb);
    return () => listeners.delete(cb);
  },
  async sftpHome(session) {
    const s = sessions.get(session);
    return (s && demoHosts.find((h) => h.alias === s.alias)?.user && demoHomes[s.alias]) || "/home/deploy";
  },
  async sftpList(_session, path) {
    await sleep(path.includes("logs") ? 600 : 180);
    return list(path);
  },
  async sftpMkdir(_session, path) {
    if (tree.has(path)) throw { code: "remote_failure", message: `${path} already exists`, hint: null } satisfies ErrorSummary;
    put(path, "dir");
  },
  async sftpRename(_session, from, to) {
    for (const [p, n] of [...tree]) {
      if (p === from || p.startsWith(`${from}/`)) {
        tree.delete(p);
        tree.set(to + p.slice(from.length), n);
      }
    }
  },
  async sftpRemove(_session, path) {
    if ([...tree.keys()].some((p) => parentOf(p) === path && p !== path)) {
      throw { code: "remote_failure", message: `${nameOf(path)} is not empty`, hint: "Remove what it contains first." } satisfies ErrorSummary;
    }
    tree.delete(path);
  },
  async pickUploads() {
    const token = nextId++;
    uploads.set(token, { name: "release-notes.md", size: 18_432 });
    return [{ token, display: "release-notes.md", size: 18_432 }];
  },
  async upload(session, token, remoteDir, onConflict) {
    const file = uploads.get(token);
    if (!file) throw { code: "unknown_upload", message: "this file is no longer available", hint: "Choose it again." } satisfies ErrorSummary;
    const path = `${remoteDir.replace(/\/$/, "")}/${file.name}`;
    if (tree.has(path) && onConflict === "ask") {
      const id = nextId++;
      emit({ type: "sftp", id: 1, session, event: { kind: "transfer_failed", transfer: id, error: { code: "destination_exists", message: `${file.name} already exists`, hint: null } } });
      return id;
    }
    return simulate(session, "upload", file.name, file.size, onConflict === "ask" ? "overwrite" : onConflict, () => put(path, "file", file.size));
  },
  async download(session, remotePath, onConflict) {
    const node = tree.get(remotePath);
    if (!node) throw notFound(remotePath);
    return simulate(session, "download", nameOf(remotePath), node.size, onConflict, (final) => local.add(final));
  },
  async cancelTransfer(id) {
    const t = transfers.get(id);
    if (t) t.cancelled = true;
  },
  downloadDir: async () => downloadDir,
  async chooseDownloadDir() {
    downloadDir = "/home/demo/Projects/incidents";
    return downloadDir;
  },
  async checkForward(spec) {
    // -D [bind:]port: a SOCKS5 proxy (same exposure rule as -L).
    const d = /^\s*-?D\s+(?:([^\s:]*):)?(\d+)\s*$/i.exec(spec);
    if (d) {
      const bind = d[1] === undefined ? "localhost" : d[1] === "" || d[1] === "*" ? "0.0.0.0" : d[1];
      return { display: `SOCKS · ${bind}:${d[2]}`, exposed: !["localhost", "127.0.0.1", "::1"].includes(bind), dynamic: true };
    }
    const m = /^\s*-?([LR])\s+(?:([^\s:]+):)?(\d+)[:\s]([^\s:]+):(\d+)\s*$/i.exec(spec);
    if (!m) throw { code: "invalid_forward", message: "expected [bind:]port:host:hostport", hint: "For example -L 8080:db.internal:5432, -R 9000:localhost:3000 or -D 1080 (SOCKS)." } satisfies ErrorSummary;
    const bind = m[2] ?? "localhost";
    return {
      display: `-${(m[1] ?? "L").toUpperCase()} ${bind}:${m[3]} → ${m[4]}:${m[5]}`,
      exposed: !["localhost", "127.0.0.1", "::1"].includes(bind),
      dynamic: false,
    };
  },
  listTunnels: async () => [...tunnels.values()],
  async startTunnel(session, spec, allowExposed) {
    const check = await demoBackend.checkForward(spec);
    if (check.exposed && !allowExposed) throw { code: "tunnel_exposed", message: "this tunnel listens beyond this machine", hint: "Confirm to expose it." } satisfies ErrorSummary;
    const id = nextId++;
    tunnels.set(id, { id, session, display: check.display, exposed: check.exposed });
    void (async () => {
      await sleep(250);
      if (spec.includes(":5433")) {
        tunnels.delete(id);
        return emit({ type: "tunnel", id, session, event: { kind: "failed", error: { code: "port_in_use", message: "127.0.0.1:5433 is already in use", hint: "Pick another local port." } } });
      }
      const bind = (check.display.startsWith("SOCKS · ") ? check.display.slice(8) : check.display.split(" ")[1] ?? "").replace("localhost", "127.0.0.1");
      emit({ type: "tunnel", id, session, event: { kind: "listening", bind } });
      await sleep(900);
      emit({ type: "tunnel", id, session, event: { kind: "connection_opened", peer: "127.0.0.1:53412" } });
    })();
    return id;
  },
  async stopTunnel(id) {
    const t = tunnels.get(id);
    if (!t) throw { code: "unknown_tunnel", message: "this tunnel is not running", hint: null } satisfies ErrorSummary;
    tunnels.delete(id);
    emit({ type: "tunnel", id, session: t.session, event: { kind: "stopped" } });
  },
  async onDropped(cb) {
    dropListener = cb;
    return () => (dropListener = null);
  },
  async onDragging(cb) {
    dragListener = cb;
    return () => (dragListener = null);
  },
};
