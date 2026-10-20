// Simulated engine so the UI can be designed in a plain browser (`pnpm dev`)
// without the Tauri runtime. Never used inside the desktop app.
//
// Scenarios: `bastion` connects through the agent, `web-01` asks to trust a
// new host key, `db-prod` goes through a jump host and asks for a password
// (anything but "wrong" works), `staging` presents a changed host key.
import type { UnlistenFn } from "@tauri-apps/api/event";
import type { EngineEvent, ErrorSummary, Host, Phase, PromptAnswer, PromptKind, ThemeInfo } from "./api";
import type { Backend } from "./backend";

const base = { identityFiles: [], localForwards: [], remoteForwards: [], warnings: [], group: null };

export const demoHosts: Host[] = [
  { ...base, alias: "db-prod", hostname: "10.0.4.12", user: "postgres", port: 22, proxyJump: ["bastion"], tags: ["prod", "db"], favorite: true, source: "~/.ssh/config:12" },
  { ...base, alias: "bastion", hostname: "bastion.example.com", user: "ops", port: 2222, proxyJump: [], tags: ["infra"], favorite: false, source: "~/.ssh/config:1" },
  { ...base, alias: "web-01", hostname: "web-01.internal", user: "deploy", port: 22, proxyJump: ["bastion"], tags: ["prod", "web"], favorite: false, source: "~/.ssh/config:5", localForwards: ["8080 localhost:80"] },
  { ...base, alias: "staging", hostname: "staging.example.com", user: "deploy", port: 22, proxyJump: [], tags: ["staging"], favorite: false, source: "~/.ssh/conf.d/staging:1" },
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
  await sleep(250);
  const hops = host.proxyJump.length + 1;
  for (let hop = 1; hop <= hops; hop++) {
    phase(id, { phase: "connecting", hop, hops, target: hop < hops ? (host.proxyJump[hop - 1] ?? "") : host.alias });
    await sleep(450);
  }
  phase(id, { phase: "handshake" });
  await sleep(250);
  phase(id, { phase: "verifying_host_key" });
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
    await sleep(400);
  }
  s.connected = true;
  phase(id, { phase: "connected" });
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
    case "uptime":
      out(" 17:42:08 up 41 days,  3:12,  1 user,  load average: 0.08, 0.11, 0.09\r\n");
      break;
    case "exit":
      out("logout\r\n");
      terminals.delete(id);
      emit({ type: "terminal", id, session: t.session, event: { kind: "exited", status: 0, signal: null } });
      emit({ type: "terminal", id, session: t.session, event: { kind: "closed" } });
      return;
    default:
      out(`-bash: ${word}: command not found\r\n`);
  }
  out(promptOf(t));
}

export const demoBackend: Backend = {
  listHosts: async () => demoHosts,
  searchHosts: async (q) =>
    demoHosts.filter((h) => `${h.alias} ${h.hostname} ${h.tags.map((t) => "#" + t).join(" ")}`.includes(q)),
  listThemes: async () => demoThemes,
  homeDir: async () => "/home/demo",
  async connect(alias) {
    const host = demoHosts.find((h) => h.alias === alias);
    if (!host) throw { code: "unknown_host", message: `no host named ${alias}`, hint: null } satisfies ErrorSummary;
    const id = nextId++;
    sessions.set(id, { alias, connected: false, closed: false, waiters: [] });
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
    const t: DemoTerminal = { session, host, onData, line: "" };
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
    emit({ type: "terminal", id, session: t.session, event: { kind: "closed" } });
  },
  async onEngineEvent(cb): Promise<UnlistenFn> {
    listeners.add(cb);
    return () => listeners.delete(cb);
  },
};
