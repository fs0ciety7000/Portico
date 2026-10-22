// Demo (browser preview only): keys, ssh-agent, copy-id, vault, snippets.
//
// Scenarios: `id_ed25519_work` is encrypted (passphrase "correct horse"),
// its certificate expires in 3 days; `id_rsa_legacy` has an expired
// certificate and is refused by db-prod at verification; `deploy_ci` is a
// public key only. web-01 already has the keys. The vault starts locked
// (passphrase "correct horse"), or absent with `?vault=absent`. Never used inside the desktop app.
import type { AgentKey, CopyIdOutcome, EngineEvent, ErrorSummary, KeyGenRequest, LocalKey, PromptAnswer, PromptKind, Snippet, SnippetDraft, VaultStatus } from "./api";

export interface DemoHooks {
  emit(e: EngineEvent): void;
  ask(session: number, kind: PromptKind): Promise<PromptAnswer>;
  sleep(ms: number): Promise<void>;
  nextId(): number;
  aliasOf(session: number): string | undefined;
  tagsOf(alias: string): string[];
  /** Types into a demo terminal; false when it is gone. */
  type(terminal: number, data: string): boolean;
}

const NOW = Math.floor(Date.now() / 1000);
const DAY = 86_400;
const PASS = "correct horse";

const fail = (code: string, message: string, hint: string | null = null): ErrorSummary => ({ code, message, hint });

// Messages and hints as portico-core and src-tauri word them (lowercase
// messages, absolute paths), so the demo shows what the app shows.
const SSH_DIR = "/home/demo/.ssh";
const VAULT_FILE = "/home/demo/.local/share/portico/vault.toml";
const unknownKey = (id: string) => fail("key_unknown", `no key named ${id} in ~/.ssh`, "Refresh the list: the key may have been moved or deleted.");
const snippetNotFound = (id: string) => fail("snippet_not_found", `unknown snippet ${id}`, "It may have been deleted from another window: reload the snippets.");
const wrongVaultPassphrase = () => fail("vault_wrong_passphrase", "wrong vault passphrase", "Check the master passphrase (keyboard layout, Caps Lock).");
const vaultTooShort = () =>
  fail("vault_passphrase_too_short", "the vault passphrase must have at least 8 characters", "Choose a longer master passphrase (several words are easy to remember).");
const notAccepted = (alias: string) =>
  fail(
    "copy_id_not_accepted",
    `the key was installed on ${alias} but logging in with it failed`,
    "If you dismissed the passphrase, verify again. Otherwise the server may ignore ~/.ssh/authorized_keys (AuthorizedKeysFile, StrictModes or ownership).",
  );

const local: LocalKey[] = [
  {
    id: "deploy_ci",
    privateDisplay: "~/.ssh/deploy_ci",
    publicDisplay: "~/.ssh/deploy_ci.pub",
    algorithm: "ssh-ed25519",
    fingerprint: "SHA256:Zb2kq7m0XcQe4J1vT9pRyLs8NwHfA3uD6oGiK5aEtUc",
    comment: "ci@build-runner",
    hasPrivate: false,
    certificate: null,
  },
  {
    id: "id_ed25519",
    privateDisplay: "~/.ssh/id_ed25519",
    publicDisplay: "~/.ssh/id_ed25519.pub",
    algorithm: "ssh-ed25519",
    fingerprint: "SHA256:8xPq3VnJ2kLwR5tY7uI9oA1sD4fG6hJ0kZ2xC3vB5nM",
    comment: "demo@workstation",
    hasPrivate: true,
    certificate: null,
  },
  {
    id: "id_ed25519_work",
    privateDisplay: "~/.ssh/id_ed25519_work",
    publicDisplay: "~/.ssh/id_ed25519_work.pub",
    algorithm: "ssh-ed25519",
    fingerprint: "SHA256:Qm7tR2wX9yZ4aB6cD8eF0gH1iJ3kL5mN7oP9qR1sT3u",
    comment: "demo@corp",
    hasPrivate: true,
    certificate: {
      keyId: "demo@corp",
      principals: ["demo", "deploy"],
      validAfter: NOW - 4 * DAY,
      validBefore: NOW + 3 * DAY,
      validity: { state: "expires_soon", expires_in_secs: 3 * DAY },
    },
  },
  {
    id: "id_rsa_legacy",
    privateDisplay: "~/.ssh/id_rsa_legacy",
    publicDisplay: "~/.ssh/id_rsa_legacy.pub",
    algorithm: "rsa-sha2-512",
    fingerprint: "SHA256:vW3xY5zA7bC9dE1fG3hI5jK7lM9nO1pQ3rS5tU7vW9x",
    comment: "old laptop",
    hasPrivate: true,
    certificate: {
      keyId: "legacy",
      principals: ["ops"],
      validAfter: NOW - 400 * DAY,
      validBefore: NOW - 35 * DAY,
      validity: { state: "expired", since_secs: 35 * DAY },
    },
  },
];

const agent: AgentKey[] = [
  { algorithm: "ssh-ed25519", fingerprint: "SHA256:8xPq3VnJ2kLwR5tY7uI9oA1sD4fG6hJ0kZ2xC3vB5nM", comment: "demo@workstation", certificate: null },
  {
    algorithm: "ssh-ed25519-cert-v01@openssh.com",
    fingerprint: "SHA256:Hc4dE6fG8hI0jK2lM4nO6pQ8rS0tU2vW4xY6zA8bC0d",
    comment: "vault-issued",
    certificate: {
      keyId: "demo@vault",
      principals: ["demo"],
      validAfter: NOW - DAY,
      validBefore: NOW + 20 * DAY,
      validity: { state: "valid", expires_in_secs: 20 * DAY },
    },
  },
];

// `?vault=absent` starts without a vault (to see the create flow).
let vaultStatus: VaultStatus = new URLSearchParams(window.location.search).get("vault") === "absent" ? "absent" : "locked";

const snippets: Snippet[] = [];
function addSnippet(d: SnippetDraft) {
  snippets.push({ ...d, placeholders: placeholders(d.command) });
}
addSnippet({ id: "disk-usage", name: "Disk usage", command: "df -h", description: "Free space on every mounted filesystem", scope: { kind: "global" }, run: true });
addSnippet({ id: "tail-app-log", name: "Tail app log", command: "tail -n {{lines}} -f ~/logs/{{file}}", description: null, scope: { kind: "hosts", values: ["web-01", "staging", "bastion"] }, run: true });
addSnippet({ id: "restart-service", name: "Restart a service", command: "sudo systemctl restart {{service}} && systemctl status {{service}} --no-pager", description: "Asks for sudo on the server", scope: { kind: "tags", values: ["prod"] }, run: false });
addSnippet({
  id: "pg-activity",
  name: "Postgres activity",
  command: "psql -c \"select pid, usename, state, query\n  from pg_stat_activity\n  where state <> 'idle';\"",
  description: null,
  scope: { kind: "hosts", values: ["db-prod", "bastion"] },
  run: false,
});

function placeholders(command: string): string[] {
  const out: string[] = [];
  for (const m of command.matchAll(/\{\{\s*([^{}]+?)\s*\}\}/g)) if (m[1] && !out.includes(m[1])) out.push(m[1]);
  return out;
}

function applies(s: Snippet, alias: string, tags: string[]): boolean {
  if (s.scope.kind === "global") return true;
  if (s.scope.kind === "hosts") return s.scope.values.includes(alias);
  return s.scope.values.some((t) => tags.includes(t));
}

export function demoKeys(h: DemoHooks) {
  return {
    listKeys: async (): Promise<LocalKey[]> => {
      await h.sleep(220);
      return local.map((k) => ({ ...k }));
    },
    keyDefaults: async () => ({ comment: "demo@workstation" }),
    async generateKey(r: KeyGenRequest): Promise<LocalKey> {
      if (!/^[A-Za-z0-9._-]{1,64}$/.test(r.name) || r.name.startsWith(".")) {
        throw fail("key_name", `invalid key file name "${r.name}": only letters, digits, dot, dash and underscore`, "Use letters, digits, dot, dash or underscore, like id_ed25519_work.");
      }
      if (local.some((k) => k.id === r.name)) throw fail("key_exists", `${SSH_DIR}/${r.name} already exists`, "Pick another name: Portico never overwrites a key.");
      await h.sleep(r.algorithm.kind === "rsa" ? 1800 : 500);
      const algorithm = r.algorithm.kind === "ed25519" ? "ssh-ed25519" : r.algorithm.kind === "rsa" ? "rsa-sha2-512" : `ecdsa-sha2-nist${r.algorithm.curve}`;
      const key: LocalKey = {
        id: r.name,
        privateDisplay: `~/.ssh/${r.name}`,
        publicDisplay: `~/.ssh/${r.name}.pub`,
        algorithm,
        fingerprint: `SHA256:${btoa(r.name + algorithm).replace(/=+$/, "").padEnd(43, "x").slice(0, 43)}`,
        comment: r.comment,
        hasPrivate: true,
        certificate: null,
      };
      local.push(key);
      local.sort((a, b) => a.id.localeCompare(b.id));
      return key;
    },
    publicKeyText: async (id: string) => {
      const k = local.find((x) => x.id === id);
      if (!k) throw unknownKey(id);
      return `${k.algorithm} AAAAC3NzaC1lZDI1NTE5AAAAIDemoDemoDemoDemoDemoDemoDemoDemoDemo ${k.comment}`;
    },
    listAgentKeys: async (): Promise<AgentKey[]> => {
      await h.sleep(160);
      return agent.map((k) => ({ ...k }));
    },
    async agentAdd(id: string, passphrase: string | null, lifetime: number | null): Promise<AgentKey> {
      const k = local.find((x) => x.id === id);
      if (!k) throw unknownKey(id);
      if (lifetime !== null && (lifetime < 1 || lifetime > 7 * DAY)) {
        throw fail("agent_lifetime", `an agent lifetime of ${lifetime} s is outside 1..=${7 * DAY} s`, "Pick one of the offered durations, or until removed.");
      }
      await h.sleep(300);
      if (k.id === "id_ed25519_work") {
        if (!passphrase) throw fail("key_needs_passphrase", `${SSH_DIR}/${k.id} is encrypted`);
        if (passphrase !== PASS) throw fail("key_wrong_passphrase", `wrong passphrase for ${SSH_DIR}/${k.id}`, "Try again.");
      }
      const added: AgentKey = { algorithm: k.algorithm, fingerprint: k.fingerprint, comment: "", certificate: null };
      if (!agent.some((a) => a.fingerprint === k.fingerprint)) agent.push(added);
      return added;
    },
    async agentRemove(fingerprint: string) {
      const i = agent.findIndex((a) => a.fingerprint === fingerprint);
      if (i < 0) throw fail("agent_no_identity", `no identity ${fingerprint} in ssh-agent`, "Refresh the list.");
      agent.splice(i, 1);
    },
    async copyId(session: number, id: string): Promise<CopyIdOutcome> {
      const alias = h.aliasOf(session) ?? "";
      await h.sleep(900);
      if (!local.some((k) => k.id === id)) throw unknownKey(id);
      return alias === "web-01" ? "already_present" : "added";
    },
    async verifyKeyLogin(session: number, id: string) {
      const alias = h.aliasOf(session) ?? "";
      const probe = h.nextId();
      h.emit({ type: "session", id: probe, event: { kind: "started", alias, probe: true } });
      h.emit({ type: "session", id: probe, event: { kind: "phase", phase: "connecting", hop: 1, hops: 1, target: alias } });
      await h.sleep(700);
      const end = (ok: boolean) =>
        h.emit({ type: "session", id: probe, event: ok ? { kind: "phase", phase: "closed", reason: "user_requested" } : { kind: "phase", phase: "failed", error: fail("auth_failed", "authentication failed") } });
      if (id === "id_ed25519_work") {
        h.emit({ type: "session", id: probe, event: { kind: "phase", phase: "authenticating", method: "public_key" } });
        for (let attempt = 1; ; attempt++) {
          const a = await h.ask(probe, { kind: "key_passphrase", key_path: `${SSH_DIR}/id_ed25519_work`, attempt, attempts: 3 });
          if (a.kind === "secret" && a.value === PASS) break;
          if (a.kind !== "secret" || attempt === 3) {
            end(false);
            throw notAccepted(alias);
          }
        }
      }
      await h.sleep(600);
      if (alias === "db-prod" && id === "id_rsa_legacy") {
        end(false);
        throw notAccepted(alias);
      }
      end(true);
    },
    vaultStatus: async () => vaultStatus,
    async vaultCreate(passphrase: string) {
      await h.sleep(500);
      if (vaultStatus !== "absent") throw fail("vault_exists", `a vault already exists at ${VAULT_FILE}`, "A vault already exists: unlock it instead of creating a new one.");
      if ([...passphrase].length < 8) throw vaultTooShort();
      vaultStatus = "unlocked";
    },
    async vaultUnlock(passphrase: string) {
      await h.sleep(500);
      if (passphrase !== PASS) throw wrongVaultPassphrase();
      vaultStatus = "unlocked";
    },
    async vaultLock() {
      vaultStatus = "locked";
    },
    async vaultChangePassphrase(old: string, next: string) {
      await h.sleep(700);
      if (old !== PASS) throw wrongVaultPassphrase();
      if ([...next].length < 8) throw vaultTooShort();
    },
    listSnippets: async (): Promise<Snippet[]> => {
      await h.sleep(120);
      return snippets.map((s) => ({ ...s }));
    },
    snippetsFor: async (alias: string): Promise<Snippet[]> => snippets.filter((s) => applies(s, alias, h.tagsOf(alias))).map((s) => ({ ...s })),
    async saveSnippet(d: SnippetDraft): Promise<Snippet> {
      if (!d.name.trim()) throw fail("snippet_invalid", "invalid snippet name: is empty");
      if (!d.command.trim()) throw fail("snippet_invalid", "invalid snippet command: is empty");
      // eslint-disable-next-line no-control-regex
      if (/[\u0000-\u0008\u000b-\u001f\u007f]/.test(d.command)) {
        throw fail("snippet_invalid", "invalid snippet command: contains control characters", "Commands may use several lines and tabs, but no escape or other control characters.");
      }
      const id = d.id || d.name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "") || "snippet";
      const saved: Snippet = { ...d, id, name: d.name.trim(), placeholders: placeholders(d.command) };
      const i = snippets.findIndex((s) => s.id === id);
      if (i >= 0) snippets[i] = saved;
      else snippets.push(saved);
      return saved;
    },
    async deleteSnippet(id: string) {
      const i = snippets.findIndex((s) => s.id === id);
      if (i < 0) throw snippetNotFound(id);
      snippets.splice(i, 1);
    },
    async runSnippet(terminal: number, id: string, values: Record<string, string>, bracketed: boolean, run: boolean | null) {
      const s = snippets.find((x) => x.id === id);
      if (!s) throw snippetNotFound(id);
      let text = s.command;
      for (const name of s.placeholders) {
        const value = values[name];
        if (!value) throw fail("snippet_missing_value", `no value for placeholder {{${name}}}`, "Fill in every placeholder before sending the snippet.");
        if (/[\n\r\t]/.test(value)) throw fail("snippet_value", `the value of {{${name}}} contains a line break`, "Values are typed on a single line: remove line breaks, tabs and control characters.");
        text = text.replace(new RegExp(`\\{\\{\\s*${name.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")}\\s*\\}\\}`, "g"), value);
      }
      const execute = run ?? s.run;
      if (text.includes("\n") && !execute && !bracketed) {
        throw fail(
          "snippet_needs_paste_mode",
          "a multi-line snippet cannot be pasted without bracketed paste",
          "The remote shell does not accept multi-line pastes: run the snippet, or paste it from a shell with bracketed paste.",
        );
      }
      // The demo shell has no bracketed paste: one line, Enter if run.
      if (!h.type(terminal, text.replace(/\n\s*/g, " ") + (execute ? "\r" : ""))) {
        throw fail("terminal_closed", "this terminal is closed", "Run the snippet in an open terminal tab.");
      }
    },
  };
}
