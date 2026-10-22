// Demo (browser preview only): "Import hosts…" with a simulated export.
// Never used inside the desktop app, where the picker, the format detection
// and the plan (key material included) live in Rust.
//
// URL switches: `?import=termix` (a Termix JSON export with two private
// keys, one of them encrypted, and a host edited since an earlier import),
// `?import=cancel` (the picker is dismissed), `?import=error` (an
// unrecognized file), `?import=included` (the Include line is already in
// ~/.ssh/config), `?import=empty` (every record is skipped). Without a
// switch: the synthetic Termius template of portico-core's tests. A second
// import in the same page says how many hosts it updates.
import type { Backend } from "./backend";
import type { ImportApplyRequest, ImportNotice, ImportPreview, ImportReport, PlannedHost, PlannedKey } from "./api";

const mode = new URLSearchParams(location.search).get("import");
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const TARGET = "~/.config/portico/hosts.conf";
const INCLUDE = "Include ~/.config/portico/hosts.conf";
const KEYS_DIR = "~/.ssh/portico/";

const expired = () => ({ code: "import_expired", message: "this import preview is no longer available", hint: "Choose the export file again." });

const host = (
  source: string,
  alias: string,
  label: string,
  hostname: string,
  user: string,
  port: number | null,
  group: string | null,
  tags: string[],
  record: number,
  extra: Partial<PlannedHost> = {},
): PlannedHost => ({
  alias,
  label,
  hostname,
  user,
  port,
  group,
  tags,
  notes: alias === label ? null : `${source}: ${label}`,
  record,
  favorite: false,
  key: null,
  update: importedCount > 0,
  sourceId: `label:${alias}`,
  ...extra,
});

const notice = (code: string, record: number | null, label: string | null, message: string): ImportNotice => ({ code, record, label, message });

function emptyPreview(prefix: boolean): ImportPreview {
  return {
    token: Date.now(),
    fileName: "switches.csv",
    source: "Termius",
    format: "Termius CSV",
    prefixText: "termius-",
    records: 3,
    prefix,
    hosts: [],
    skipped: [
      notice("unsupported_protocol", 2, "core-switch", "the protocol is not SSH"),
      notice("unsupported_protocol", 3, "edge-switch", "the protocol is not SSH"),
      notice("missing_hostname", 4, "printer", "no host name or address"),
    ],
    warnings: [],
    secrets: { passwords: 0, keys: 0, keysInExport: 0 },
    secretsAdvice: null,
    identityFiles: [],
    target: TARGET,
    includeLine: INCLUDE,
    replaces: null,
    keys: [],
    keysDir: KEYS_DIR,
  };
}

function termiusPreview(prefix: boolean): ImportPreview {
  const p = (alias: string) => (prefix ? `termius-${alias}` : alias);
  const h = (...args: Parameters<typeof host> extends [string, ...infer R] ? R : never) => host("Termius", ...args);
  const hosts = [
    h(p("web-eu-1"), "web eu 1", "203.0.113.10", "deploy", null, "Production/EU", ["web", "nginx"], 2),
    h(p("db-eu-1"), "db-eu-1", "203.0.113.11", "postgres", 2222, "Production/EU", ["db", "postgres"], 3),
    h("termius-at-198.51.100.7", "AT 198.51.100.7", "198.51.100.7", "root", 38444, "Production", [], 4),
    h(p("web-eu-1-2"), "web eu 1", "staging.example.test", "ubuntu", null, "Staging", ["web-serveur"], 5),
    h(p("v6-box"), "v6-box", "2001:db8::10", "ops", null, null, ["ipv6"], 7),
    h(p("bad-port"), "bad-port", "192.0.2.60", "root", null, "Lab", [], 9),
  ];
  // `web-01` exists in the demo's ssh config: without the prefix, a
  // Termius host of that name is skipped.
  const collision = prefix ? [] : [notice("alias_collision", 11, "web-01", "`web-01` is already defined in your ssh config (import with the termius- prefix to keep both)")];
  if (prefix) hosts.push(h("termius-web-01", "web-01", "198.51.100.30", "deploy", null, "Production", ["web"], 11));
  return {
    token: Date.now(),
    fileName: "termius_template.csv",
    source: "Termius",
    format: "Termius CSV",
    prefixText: "termius-",
    records: 10,
    prefix,
    hosts,
    skipped: [
      notice("unsupported_protocol", 6, "legacy-switch", "the protocol is not SSH"),
      notice("missing_hostname", 8, "no-host", "no host name or address"),
      notice("invalid_hostname", 10, "inject", "the host name contains a space, line break, `#`, quote or another character not allowed in a host name"),
      ...collision,
    ],
    warnings: [
      notice("alias_renamed", 5, "web eu 1", `\`${p("web-eu-1")}\` is already used in this import: imported as \`${p("web-eu-1-2")}\``),
      notice("tag_normalized", 5, "web eu 1", "tag `Web Serveur` imported as `web-serveur`"),
      notice("invalid_port", 9, "bad-port", "the port is not between 1 and 65535: left out (22 applies)"),
      ...(prefix ? [] : [notice("alias_looks_like_host", 4, "AT 198.51.100.7", "`at-198.51.100.7` looks like another host's name: imported as `termius-at-198.51.100.7`")]),
    ],
    secrets: { passwords: 2, keys: 1, keysInExport: 0 },
    secretsAdvice:
      "2 passwords and 1 key ignored (never imported). The export holds them in clear text: delete it once the import is done, and keep secrets in your keychain or ssh-agent.",
    identityFiles: [],
    target: TARGET,
    includeLine: INCLUDE,
    replaces: importedCount || null,
    keys: [],
    keysDir: KEYS_DIR,
  };
}

const ED25519 = "SHA256:q1Xv9mB3rTzK0pW7nLc2fYhJ8sDgE4uRaO6iVbN5kMw";
const RSA = "SHA256:Zk4pN7wQe2LmT9vB1xCyR6uHsJ3aGdF8oIcE0nVtWbM";

function termixPreview(prefix: boolean): ImportPreview {
  const p = (alias: string) => (prefix ? `termix-${alias}` : alias);
  const h = (...args: Parameters<typeof host> extends [string, ...infer R] ? R : never) => host("Termix", ...args);
  const hosts = [
    h(p("web-eu-1"), "Web EU 1", "203.0.113.10", "deploy", null, "Production/EU", ["web"], 1, { key: ED25519, favorite: true, sourceId: "1" }),
    h(p("web-eu-2"), "Web EU 2", "203.0.113.12", "deploy", null, "Production/EU", ["web"], 2, { key: ED25519, sourceId: "2" }),
    h(p("db-eu-1"), "DB EU 1", "203.0.113.11", "postgres", 2222, "Production/EU", ["db"], 3, { key: ED25519, sourceId: "3" }),
    h(p("bastion"), "Bastion", "198.51.100.4", "ops", null, "Edge", [], 4, { key: RSA, sourceId: "4" }),
    h(p("nas"), "NAS", "192.168.1.20", "admin", 2222, "Home", ["lab"], 6, { sourceId: "6" }),
  ];
  const keys: PlannedKey[] = [
    { id: ED25519, algorithm: "ssh-ed25519", fingerprint: ED25519, format: "open_ssh", encrypted: false, aliases: [p("web-eu-1"), p("web-eu-2"), p("db-eu-1")], local: null },
    { id: RSA, algorithm: "ssh-rsa", fingerprint: RSA, format: "pem", encrypted: true, aliases: [p("bastion")], local: null },
  ];
  return {
    token: Date.now(),
    fileName: "termix_export.json",
    source: "Termix",
    format: "Termix JSON",
    prefixText: "termix-",
    records: 7,
    prefix,
    hosts,
    skipped: prefix
      ? []
      : [notice("edited_since_import", 5, "Grafana", "this host was imported earlier, then edited: left as it is (import with the termix- prefix to keep both)")],
    warnings: [
      notice("ignored_setting", 4, "Bastion", "`tunnelConnections` (2) was not imported: Portico imports hosts only"),
      notice("key_unreadable", 7, "Old router", "private key not imported: a PuTTY key (convert it with puttygen to OpenSSH)"),
    ],
    secrets: { passwords: 1, keys: 1, keysInExport: 3 },
    secretsAdvice:
      "1 password and 1 key ignored (never imported). The export holds 3 private keys in clear text or encrypted: delete it once the import is done, and keep secrets in your keychain or ssh-agent.",
    identityFiles: [],
    target: TARGET,
    includeLine: INCLUDE,
    replaces: importedCount || null,
    keys,
    keysDir: KEYS_DIR,
  };
}

function preview(prefix: boolean): ImportPreview {
  if (mode === "empty") return emptyPreview(prefix);
  if (mode === "termix") return termixPreview(prefix);
  return termiusPreview(prefix);
}

let pending: ImportPreview | null = null;
let imported = false;
/** Hosts written by the last demo import (a second one updates them). */
let importedCount = 0;

export const demoImport: Pick<Backend, "importPick" | "importReplan" | "importApply" | "importCancel"> = {
  async importPick(prefix) {
    await sleep(700);
    if (mode === "cancel") return null;
    if (mode === "error") {
      throw {
        code: "import_unknown_format",
        message: "notes.txt is not a Termius or Termix export Portico recognizes",
        hint: "Use a Termius CSV (import template header), a termius-local-export hosts.json or credentials.csv, the output of `termius export-ssh-config`, or a Termix JSON export. See docs/IMPORT.md.",
      };
    }
    pending = preview(prefix);
    return pending;
  },
  async importReplan(token, prefix) {
    await sleep(200);
    if (pending?.token !== token) throw expired();
    pending = preview(prefix);
    return pending;
  },
  async importApply(request: ImportApplyRequest): Promise<ImportReport> {
    await sleep(500);
    const plan = pending;
    if (!plan || plan.token !== request.token) throw expired();
    // Same checks as Rust (the plan stays pending when refused).
    if (request.prefix !== plan.prefix) throw { code: "import_stale", message: "this preview does not match the prefix choice", hint: "Toggle the prefix again, then import." };
    if (request.keys.some((id) => !plan.keys.some((k) => k.id === id))) {
      throw { code: "import_key_unknown", message: "a selected key is not in this export", hint: "Choose the export file again." };
    }
    pending = null;
    const again = imported;
    imported = true;
    importedCount = plan.hosts.length;
    const chosen = plan.keys.filter((k) => request.keys.includes(k.id));
    const firstAlias = (k: PlannedKey) => plan.hosts.find((h) => h.key === k.id)?.alias ?? "key";
    return {
      formatLabel: plan.format,
      source: plan.source,
      target: plan.target,
      unchanged: false,
      imported: plan.hosts.map((h) => h.alias),
      updated: plan.hosts.filter((h) => h.update).map((h) => h.alias),
      skipped: plan.skipped,
      warnings: plan.warnings,
      secrets: { ...plan.secrets, keys: plan.secrets.keys + plan.keys.length - chosen.length },
      secretsAdvice: plan.secretsAdvice,
      identityFiles: plan.identityFiles,
      metaUpdated: again ? 0 : plan.hosts.length,
      includeLine: plan.includeLine,
      includeActive: mode === "included",
      keys: {
        imported: chosen.filter((k) => !k.local).map((k) => `${KEYS_DIR}${firstAlias(k)}`),
        reused: chosen.flatMap((k) => (k.local ? [k.local] : [])),
        skipped: plan.keys.length - chosen.length,
      },
    };
  },
  async importCancel() {
    pending = null;
  },
};
