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
// import in the same page says how many hosts it updates, and the export now
// points one host at another server (a destination change, skipped unless
// accepted); `?reimport=changed` shows that re-import on the first pick.
import type { Backend } from "./backend";
import type { IgnoredSecrets, ImportApplyRequest, ImportDestination, ImportNotice, ImportPreview, ImportReport, PlannedHost, PlannedKey } from "./api";
import { deleteExportAdvice, destinationText, ignoredSecretsAdvice, keepRedirects } from "./importform";

/** The secrets part of a demo plan, with the engine's two texts. */
function secretsOf(secrets: IgnoredSecrets, fileName: string): Pick<ImportPreview, "secrets" | "secretsAdvice" | "deleteExportAdvice"> {
  return { secrets, secretsAdvice: ignoredSecretsAdvice(secrets), deleteExportAdvice: deleteExportAdvice(secrets, fileName) };
}

const params = new URLSearchParams(location.search);
const mode = params.get("import");
/** A re-import: hosts update an earlier import, one of them moved. */
const reimport = () => importedCount > 0 || params.get("reimport") === "changed";
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
  update: reimport(),
  previous: null,
  sourceId: `label:${alias}`,
  ...extra,
});

const notice = (code: string, record: number | null, label: string | null, message: string): ImportNotice => ({ code, record, label, message });

/** On a re-import, `h` now points at `to`: it carries where it points today
 * (`previous`) and the plan warns, as the engine does. */
function moved(h: PlannedHost, to: string, before: ImportDestination, warnings: ImportNotice[]): PlannedHost {
  if (!reimport()) return h;
  warnings.push(
    notice(
      "destination_changed",
      h.record,
      h.label,
      `\`${h.alias}\` was imported earlier for ${destinationText(before)}: the export now points it elsewhere (skipped unless you accept this change)`,
    ),
  );
  return { ...h, hostname: to, previous: before };
}

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
    ...secretsOf({ passwords: 0, keys: 0, untickedKeys: 0, unreadableKeys: 0, keysInExport: 0 }, "switches.csv"),
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
  const changes: ImportNotice[] = [];
  const hosts = [
    h(p("web-eu-1"), "web eu 1", "203.0.113.10", "deploy", null, "Production/EU", ["web", "nginx"], 2),
    moved(
      h(p("db-eu-1"), "db-eu-1", "203.0.113.11", "postgres", 2222, "Production/EU", ["db", "postgres"], 3),
      "198.51.100.77",
      { hostname: "203.0.113.11", user: "postgres", port: 2222, identityFile: null },
      changes,
    ),
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
      ...changes,
    ],
    ...secretsOf({ passwords: 2, keys: 1, untickedKeys: 0, unreadableKeys: 0, keysInExport: 0 }, "termius_template.csv"),
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
/** An encrypted OpenSSH key: its fingerprint is only claimed by the export. */
const ECDSA = "SHA256:Hc8sLq2VtR5mX0bNwJ7pKf3yUe9aZgD1oTiC6nWrQvE";

function termixPreview(prefix: boolean): ImportPreview {
  const p = (alias: string) => (prefix ? `termix-${alias}` : alias);
  const h = (...args: Parameters<typeof host> extends [string, ...infer R] ? R : never) => host("Termix", ...args);
  const changes: ImportNotice[] = [];
  const hosts = [
    h(p("web-eu-1"), "Web EU 1", "203.0.113.10", "deploy", null, "Production/EU", ["web"], 1, { key: ED25519, favorite: true, sourceId: "1" }),
    moved(
      h(p("web-eu-2"), "Web EU 2", "203.0.113.12", "deploy", null, "Production/EU", ["web"], 2, { key: ED25519, sourceId: "2" }),
      "198.51.100.212",
      { hostname: "203.0.113.12", user: "deploy", port: null, identityFile: `${KEYS_DIR}${p("web-eu-1")}` },
      changes,
    ),
    h(p("db-eu-1"), "DB EU 1", "203.0.113.11", "postgres", 2222, "Production/EU", ["db"], 3, { key: ED25519, sourceId: "3" }),
    h(p("bastion"), "Bastion", "198.51.100.4", "ops", null, "Edge", [], 4, { key: RSA, sourceId: "4" }),
    h(p("nas"), "NAS", "192.168.1.20", "admin", 2222, "Home", ["lab"], 6, { key: ECDSA, sourceId: "6" }),
  ];
  const keys: PlannedKey[] = [
    { id: ED25519, algorithm: "ssh-ed25519", fingerprint: ED25519, format: "open_ssh", encrypted: false, fingerprintVerified: true, aliases: [p("web-eu-1"), p("web-eu-2"), p("db-eu-1")], local: null },
    { id: RSA, algorithm: "ssh-rsa", fingerprint: RSA, format: "pem", encrypted: true, fingerprintVerified: true, aliases: [p("bastion")], local: null },
    { id: ECDSA, algorithm: "ecdsa-sha2-nistp256", fingerprint: ECDSA, format: "open_ssh", encrypted: true, fingerprintVerified: false, aliases: [p("nas")], local: null },
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
      ...changes,
    ],
    ...secretsOf({ passwords: 1, keys: 1, untickedKeys: 0, unreadableKeys: 1, keysInExport: 4 }, "termix_export.json"),
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
        hint: "Use a CSV with the Termius import template header (Groups,Label,Tags,Hostname/IP,Protocol,Port,Username,…), a termius-local-export hosts.json or credentials.csv, the output of `termius export-ssh-config`, or a Termix JSON export. See Portico's import guide.",
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
    if (keepRedirects(plan.hosts, request.redirect).length !== request.redirect.length) {
      throw { code: "import_redirect_unknown", message: "an accepted destination change is not in this preview", hint: "Choose the export file again." };
    }
    pending = null;
    const again = imported;
    imported = true;
    // A destination change not accepted is skipped, as the engine does.
    const written = plan.hosts.filter((h) => h.previous === null || request.redirect.includes(h.alias));
    const held = plan.hosts
      .filter((h) => h.previous !== null && !request.redirect.includes(h.alias))
      .map((h) =>
        notice(
          "destination_changed",
          h.record,
          h.label,
          `this alias was imported earlier for ${destinationText(h.previous ?? h)}: not pointed elsewhere without your consent`,
        ),
      );
    importedCount = written.length;
    const chosen = plan.keys.filter((k) => request.keys.includes(k.id));
    const firstAlias = (k: PlannedKey) => written.find((h) => h.key === k.id)?.alias ?? "key";
    const unticked = plan.keys.length - chosen.length;
    const secrets = { ...plan.secrets, keys: plan.secrets.keys + unticked, untickedKeys: unticked };
    const keyFile = (k: PlannedKey) => {
      const usedBy = written.filter((h) => h.key === k.id).map((h) => h.alias);
      return { path: `${KEYS_DIR}${usedBy[0] ?? "key"}`, encrypted: k.encrypted || !!request.passphrase, usedBy };
    };
    return {
      formatLabel: plan.format,
      source: plan.source,
      fileName: plan.fileName,
      deleteExportAdvice: deleteExportAdvice(secrets, plan.fileName),
      target: plan.target,
      unchanged: false,
      imported: written.map((h) => h.alias),
      updated: written.filter((h) => h.update).map((h) => h.alias),
      skipped: [...plan.skipped, ...held],
      // Like the engine: the plan's warnings, destination changes included.
      warnings: plan.warnings,
      secrets,
      secretsAdvice: ignoredSecretsAdvice(secrets),
      identityFiles: plan.identityFiles,
      metaUpdated: again ? 0 : plan.hosts.length,
      includeLine: plan.includeLine,
      includeActive: mode === "included",
      keys: {
        imported: chosen.filter((k) => !k.local).map((k) => `${KEYS_DIR}${firstAlias(k)}`),
        written: chosen.filter((k) => !k.local).map(keyFile),
        reused: chosen.flatMap((k) => (k.local ? [k.local] : [])),
        skipped: plan.keys.length - chosen.length,
      },
    };
  },
  async importCancel() {
    pending = null;
  },
};
