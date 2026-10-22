// Demo (browser preview only): hosts created in Portico (phase 6b). Create,
// edit, duplicate and delete mutate the demo inventory with rules close to
// `portico_core::managed_hosts` (same codes, messages and hints), then emit
// `hosts_changed` and `host_meta`. Never used inside the desktop app.
//
// URL switches: `?include=active` (the Include line is already in
// ~/.ssh/config: no banner), `?banner=none` (no Portico host at startup).
import type { EngineEvent, ErrorSummary, Host, HostDraft, LocalKey, ManagedInclude } from "./api";
import type { Backend } from "./backend";
import { normalizeTags } from "./demo-meta";

const params = new URLSearchParams(location.search);
const MANAGED_FILE = "~/.config/portico/hosts.conf";
const INCLUDE: ManagedInclude = { line: `Include ${MANAGED_FILE}`, active: params.get("include") === "active" };

const fail = (code: string, message: string, hint: string | null): ErrorSummary => ({ code, message, hint });

/** Hints of `ManagedHostError::code_and_hint` (same text as the engine). */
const HINTS = {
  alias: "Use lowercase letters, digits, `_`, `.` and `-` (64 at most), not starting with `-` or `.`.",
  hostname: "Use a DNS name or an IP address: letters, digits, `.`, `-`, `_` and `:`.",
  user: "Use letters, digits, `.`, `-`, `_`, `@` and `+` (64 at most), not starting with `-`.",
  port: "A port is a number from 1 to 65535.",
  identity: "Pick one of your local private keys: a regular file under your home directory.",
  jump: "Pick an existing host as jump host.",
  cycle: "A host cannot reach itself through its jump hosts: pick another jump host.",
  taken: "Pick another alias: Portico never shadows a host defined elsewhere (aliases are compared ignoring case).",
  readOnly: "Portico never rewrites your ssh config: duplicate it as a Portico host to change it here.",
  inUse: "Change the jump host of those hosts first.",
};

export interface HostsHooks {
  emit(e: EngineEvent): void;
  hosts: Host[];
  keys(): Promise<LocalKey[]>;
}

const NO_FORWARDS = { localForwards: [], remoteForwards: [], dynamicForwards: [] as string[], warnings: [] as string[] };

/** Portico hosts of the demo (unless `?banner=none`), and one imported host. */
export function demoPorticoHosts(): Host[] {
  if (params.get("banner") === "none") return [];
  return [
    {
      ...NO_FORWARDS,
      alias: "nas",
      hostname: "192.168.1.20",
      user: "admin",
      port: 2222,
      proxyJump: [],
      identityFiles: ["~/.ssh/id_ed25519"],
      tags: ["home", "backup"],
      group: null,
      favorite: false,
      notes: "Synology, nightly snapshots at 03:00.",
      source: `${MANAGED_FILE}:5`,
      origin: "portico",
      managed: { hostname: "192.168.1.20", user: "admin", port: 2222, identityFile: "~/.ssh/id_ed25519", proxyJump: null },
    },
    {
      ...NO_FORWARDS,
      alias: "ci-runner",
      hostname: "10.0.9.4",
      user: "ci",
      port: 22,
      proxyJump: ["bastion"],
      identityFiles: [],
      tags: ["ci"],
      group: null,
      favorite: false,
      notes: null,
      source: `${MANAGED_FILE}:11`,
      origin: "portico",
      managed: { hostname: "10.0.9.4", user: "ci", port: null, identityFile: null, proxyJump: "bastion" },
    },
    {
      ...NO_FORWARDS,
      alias: "web-eu-1",
      hostname: "203.0.113.10",
      user: "deploy",
      port: 22,
      proxyJump: [],
      identityFiles: [],
      tags: ["web", "nginx"],
      group: null,
      favorite: false,
      notes: "Termius: web eu 1",
      source: "~/.config/portico/imports/termius.conf:4",
      origin: "imported",
      managed: null,
    },
  ];
}

export function demoHostsBackend(hooks: HostsHooks): Pick<Backend, "createHost" | "updateHost" | "deleteHost" | "duplicateHost" | "managedInclude"> {
  const { emit, hosts } = hooks;
  const find = (alias: string) => hosts.find((h) => h.alias.toLowerCase() === alias.toLowerCase());
  const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
  let nextLine = 17;

  function checkAlias(raw: string, except: string | null): string {
    const alias = raw.trim();
    if (!alias) throw fail("host_invalid_alias", "invalid host alias: empty", HINTS.alias);
    if (!/^[a-z0-9_][a-z0-9_.-]{0,63}$/.test(alias) || alias === "none") {
      throw fail("host_invalid_alias", "invalid host alias: use lowercase letters, digits, `_`, `.` and `-`", HINTS.alias);
    }
    const other = find(alias);
    if (other && other.alias !== except) {
      throw fail("host_alias_taken", `${other.alias} is already defined in ${other.source}`, HINTS.taken);
    }
    return alias;
  }

  function checkJump(jump: string | null, alias: string, editing: string | null): string | null {
    if (!jump) return null;
    const target = find(jump);
    if (!target) throw fail("host_invalid_proxy_jump", `jump host ${jump} is not a known host`, HINTS.jump);
    // Walk the chain: back to this host (or its old name) is a loop.
    const self = (a: string) => a.toLowerCase() === alias.toLowerCase() || a === editing;
    const chain = [alias, target.alias];
    let at: Host | undefined = target;
    while (!self(chain[chain.length - 1] ?? "") && at?.proxyJump[0] && chain.length < 32) {
      chain.push(at.proxyJump[0]);
      at = find(at.proxyJump[0]);
    }
    if (self(chain[chain.length - 1] ?? "")) throw fail("host_proxy_jump_cycle", `ProxyJump loop: ${chain.join(" → ")}`, HINTS.cycle);
    return target.alias;
  }

  async function build(draft: HostDraft, existing: Host | null): Promise<Host> {
    const alias = checkAlias(draft.alias, existing?.alias ?? null);
    const hostname = draft.hostname.trim();
    if (!hostname) throw fail("host_invalid_hostname", "invalid host name: empty", HINTS.hostname);
    if (!/^[A-Za-z0-9._:-]+$/.test(hostname) || hostname.startsWith("-")) {
      throw fail("host_invalid_hostname", "invalid host name: use letters, digits, `.`, `-`, `_` and `:`", HINTS.hostname);
    }
    const user = draft.user?.trim() || null;
    if (user && (!/^[A-Za-z0-9._@+-]{1,64}$/.test(user) || user.startsWith("-"))) {
      throw fail("host_invalid_user", "invalid user name: use letters, digits, `.`, `-`, `_`, `@` and `+`", HINTS.user);
    }
    if (draft.port !== null && (draft.port < 1 || draft.port > 65535)) {
      throw fail("host_invalid_port", `invalid port ${draft.port}: use 1 to 65535`, HINTS.port);
    }
    let identityFile: string | null = null;
    if (draft.identity.kind === "keep") identityFile = existing?.managed?.identityFile ?? null;
    else if (draft.identity.kind === "key") {
      const id = draft.identity.id;
      const key = (await hooks.keys()).find((k) => k.id === id);
      if (!key) throw fail("key_unknown", `no key named ${id} in ~/.ssh`, "Refresh the list: the key may have been moved or deleted.");
      if (!key.hasPrivate) throw fail("key_public_only", `${id} has no private key here`, "Only the .pub file is in ~/.ssh: this key cannot be used from this machine.");
      identityFile = key.privateDisplay;
    }
    const jump = checkJump(draft.proxyJump, alias, existing?.alias ?? null);
    const tags = normalizeTags(draft.tags);
    const group = draft.group?.trim() || null;
    if (group && group.length > 64) throw fail("host_group", "invalid group: longer than 64 characters", "A group is one line of at most 64 visible characters.");
    if (draft.notes && draft.notes.length > 2000) throw fail("host_notes", "notes are too long (2000 characters at most)", "Notes hold at most 2000 characters of plain text.");
    if (draft.password.kind === "set" && params.get("vault") === "locked") {
      throw fail("vault_locked", "the vault is locked", "Unlock the vault (Keys, then v) to remember passwords.");
    }
    return {
      ...NO_FORWARDS,
      alias,
      hostname,
      user,
      port: draft.port ?? 22,
      proxyJump: jump ? [jump] : [],
      identityFiles: identityFile ? [identityFile] : [],
      tags,
      group,
      favorite: draft.favorite,
      notes: draft.notes?.trim() ? draft.notes : null,
      source: existing?.source ?? `${MANAGED_FILE}:${(nextLine += 6)}`,
      origin: "portico",
      managed: { hostname, user, port: draft.port, identityFile, proxyJump: jump },
    };
  }

  function changed(host: Host | null) {
    emit({ type: "hosts_changed" });
    if (host) emit({ type: "host_meta", alias: host.alias, meta: { tags: host.tags, group: host.group, favorite: host.favorite, notes: host.notes } });
  }

  function readOnly(host: Host): never {
    throw fail("host_read_only", `${host.alias} is defined in ${host.source.replace(/:\d+$/, "")}, which Portico does not edit`, HINTS.readOnly);
  }

  return {
    async createHost(draft) {
      await sleep(250);
      const host = await build(draft, null);
      // Portico hosts are read first: they lead the list, as in the engine.
      const at = hosts.findIndex((h) => h.origin !== "portico");
      hosts.splice(at < 0 ? hosts.length : at, 0, host);
      changed(host);
      return host.alias;
    },
    async updateHost(alias, draft) {
      await sleep(250);
      const existing = find(alias);
      if (!existing) throw fail("host_not_found", `no host named ${alias}`, "Reload the host list.");
      if (existing.origin !== "portico") readOnly(existing);
      const host = await build(draft, existing);
      if (host.alias !== existing.alias) {
        // Portico hosts jumping through it follow the rename; others refuse it.
        const outside = hosts.find((h) => h.origin !== "portico" && h.proxyJump.includes(existing.alias));
        if (outside) throw fail("host_in_use", `${existing.alias} is the jump host of ${outside.alias}`, HINTS.inUse);
        for (const h of hosts) {
          if (h.proxyJump[0] === existing.alias) {
            h.proxyJump = [host.alias];
            if (h.managed) h.managed.proxyJump = host.alias;
          }
        }
      }
      hosts.splice(hosts.indexOf(existing), 1, host);
      changed(host);
      return host.alias;
    },
    async deleteHost(alias) {
      await sleep(200);
      const existing = find(alias);
      if (!existing) throw fail("host_not_found", `no host named ${alias}`, "Reload the host list.");
      if (existing.origin !== "portico") readOnly(existing);
      const by = hosts.find((h) => h.proxyJump.includes(existing.alias));
      if (by) throw fail("host_in_use", `${existing.alias} is the jump host of ${by.alias}`, HINTS.inUse);
      hosts.splice(hosts.indexOf(existing), 1);
      changed(null);
    },
    async duplicateHost(alias, newAlias) {
      await sleep(250);
      const source = find(alias);
      if (!source) throw fail("host_not_found", `no host named ${alias}`, "Reload the host list.");
      if (source.proxyJump.length > 1) {
        throw fail(
          "host_not_duplicable",
          `${alias} cannot become a Portico host: it goes through a chain of jump hosts`,
          "Portico hosts only hold HostName, User, Port, one IdentityFile and one jump host: keep this host in ~/.ssh/config.",
        );
      }
      const keys = await hooks.keys();
      const key = keys.find((k) => k.hasPrivate && source.identityFiles.includes(k.privateDisplay));
      const host = await build(
        {
          alias: newAlias,
          hostname: source.hostname,
          user: source.user,
          port: source.port === 22 ? null : source.port,
          identity: key ? { kind: "key", id: key.id } : { kind: "none" },
          proxyJump: source.proxyJump[0] ?? null,
          tags: source.tags,
          group: source.group,
          favorite: false,
          notes: null,
          password: { kind: "keep" },
        },
        null,
      );
      const at = hosts.findIndex((h) => h.origin !== "portico");
      hosts.splice(at < 0 ? hosts.length : at, 0, host);
      changed(host);
      return host.alias;
    },
    async managedInclude() {
      return { ...INCLUDE };
    },
  };
}
