// Demo (browser preview only): "Import from Termius…" with a simulated
// export (the synthetic template of portico-core's tests). Never used inside
// the desktop app, where the picker and the plan live in Rust.
//
// URL switches: `?import=cancel` (the picker is dismissed), `?import=error`
// (an unrecognized file), `?import=included` (the Include line is already
// in ~/.ssh/config), `?import=empty` (every record is skipped). A second
// import in the same page says how many hosts it replaces.
import type { Backend } from "./backend";
import type { ImportNotice, ImportPreview, ImportReport, PlannedHost } from "./api";

const mode = new URLSearchParams(location.search).get("import");
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const TARGET = "/home/demo/.config/portico/imports/termius.conf";
const INCLUDE = "Include ~/.config/portico/imports/termius.conf";

const host = (alias: string, label: string, hostname: string, user: string, port: number | null, group: string | null, tags: string[], record: number): PlannedHost => ({
  alias,
  label,
  hostname,
  user,
  port,
  group,
  tags,
  notes: alias === label ? null : `Termius: ${label}`,
  record,
});

const notice = (code: string, record: number, label: string, message: string): ImportNotice => ({ code, record, label, message });

function emptyPreview(prefix: boolean): ImportPreview {
  return {
    token: Date.now(),
    fileName: "switches.csv",
    format: "Termius CSV",
    records: 3,
    prefix,
    hosts: [],
    skipped: [
      notice("unsupported_protocol", 2, "core-switch", "the protocol is not SSH"),
      notice("unsupported_protocol", 3, "edge-switch", "the protocol is not SSH"),
      notice("missing_hostname", 4, "printer", "no host name or address"),
    ],
    warnings: [],
    secrets: { passwords: 0, keys: 0 },
    secretsAdvice: null,
    identityFiles: [],
    target: TARGET,
    includeLine: INCLUDE,
    replaces: importedCount || null,
  };
}

function preview(prefix: boolean): ImportPreview {
  if (mode === "empty") return emptyPreview(prefix);
  const p = (alias: string) => (prefix ? `termius-${alias}` : alias);
  const hosts = [
    host(p("web-eu-1"), "web eu 1", "203.0.113.10", "deploy", null, "Production/EU", ["web", "nginx"], 2),
    host(p("db-eu-1"), "db-eu-1", "203.0.113.11", "postgres", 2222, "Production/EU", ["db", "postgres"], 3),
    host("termius-at-198.51.100.7", "AT 198.51.100.7", "198.51.100.7", "root", 38444, "Production", [], 4),
    host(p("web-eu-1-2"), "web eu 1", "staging.example.test", "ubuntu", null, "Staging", ["web-serveur"], 5),
    host(p("v6-box"), "v6-box", "2001:db8::10", "ops", null, null, ["ipv6"], 7),
    host(p("bad-port"), "bad-port", "192.0.2.60", "root", null, "Lab", [], 9),
  ];
  // `web-01` exists in the demo's ssh config: without the prefix, a
  // Termius host of that name is skipped.
  const collision = prefix ? [] : [notice("alias_collision", 11, "web-01", "`web-01` is already defined in your ssh config (import with the termius- prefix to keep both)")];
  if (prefix) hosts.push(host("termius-web-01", "web-01", "198.51.100.30", "deploy", null, "Production", ["web"], 11));
  return {
    token: Date.now(),
    fileName: "termius_template.csv",
    format: "Termius CSV",
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
    secrets: { passwords: 2, keys: 1 },
    secretsAdvice:
      "2 passwords and 1 key ignored (never imported). The export holds them in clear text: delete it once the import is done, and keep secrets in your keychain or ssh-agent.",
    identityFiles: [],
    target: TARGET,
    includeLine: INCLUDE,
    replaces: importedCount || null,
  };
}

let pending: ImportPreview | null = null;
let imported = false;
/** Hosts written by the last demo import (a second one replaces them). */
let importedCount = 0;

export const demoImport: Pick<Backend, "importTermiusPick" | "importTermiusReplan" | "importApply" | "importCancel"> = {
  async importTermiusPick(prefix) {
    await sleep(700);
    if (mode === "cancel") return null;
    if (mode === "error") {
      throw {
        code: "import_unknown_format",
        message: "notes.txt is not a Termius export Portico recognizes",
        hint: "Use a CSV with the Termius import template header (Groups,Label,Tags,Hostname/IP,Protocol,Port,Username,…), a termius-local-export hosts.json or credentials.csv, or the output of `termius export-ssh-config`. See docs/IMPORT.md.",
      };
    }
    pending = preview(prefix);
    return pending;
  },
  async importTermiusReplan(token, prefix) {
    await sleep(200);
    if (pending?.token !== token) throw { code: "import_expired", message: "this import preview is no longer available", hint: "Choose the export file again." };
    pending = preview(prefix);
    return pending;
  },
  async importApply(token): Promise<ImportReport> {
    await sleep(500);
    const plan = pending;
    if (!plan || plan.token !== token) throw { code: "import_expired", message: "this import preview is no longer available", hint: "Choose the export file again." };
    pending = null;
    const again = imported;
    imported = true;
    importedCount = plan.hosts.length;
    return {
      formatLabel: plan.format,
      target: plan.target,
      unchanged: again,
      imported: plan.hosts.map((h) => h.alias),
      skipped: plan.skipped,
      warnings: plan.warnings,
      secrets: plan.secrets,
      secretsAdvice: plan.secretsAdvice,
      identityFiles: plan.identityFiles,
      metaUpdated: again ? 0 : plan.hosts.length,
      includeLine: plan.includeLine,
      includeActive: mode === "included",
    };
  },
  async importCancel() {
    pending = null;
  },
};
