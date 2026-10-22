// The host form ("Add host…", "Edit host…"): its fields as typed, the draft
// sent to the engine, dirty tracking and where an engine error goes.
//
// Pure module (no DOM, no Svelte, type-only imports): `scripts/test-hostform.ts`
// runs it under Node. The engine validates everything; the only checks here
// are those the IPC types need (a port must be a number) and the form's own
// (a password to set must be typed).
import type { ErrorSummary, Host, HostDraft, IdentityChoice, ImportSource, ManagedHost, PasswordChange } from "./api";

export type FieldName = "alias" | "hostname" | "user" | "port" | "identity" | "proxyJump" | "password" | "tags" | "group" | "notes";

/** Edit only: what happens to the remembered password. */
export type PasswordMode = "keep" | "set" | "forget";

/** `identity` select values: "" (none), `keep` (the current file), `key:<id>`. */
export const KEEP_IDENTITY = "keep";
export const keyValue = (id: string) => `key:${id}`;

export interface HostForm {
  alias: string;
  hostname: string;
  user: string;
  /** As typed: parsed by `toDraft`. */
  port: string;
  identity: string;
  /** "" = none. */
  proxyJump: string;
  tags: string[];
  group: string;
  favorite: boolean;
  notes: string;
  passwordMode: PasswordMode;
  /** Never kept beyond the IPC call (the dialog clears it). */
  password: string;
}

/** Under the password field when a password is set or kept (same wording
 * as the TUI): where a remembered password goes (risk 13 of SECURITY.md). */
export const PASSWORD_DESTINATION_NOTE =
  "Sent automatically to the server that answers at this address, after its host key is verified; then tied to that key.";

/** True when the form sets a password (typed) or keeps the remembered one. */
export function sendsPassword(form: HostForm, editing: boolean, remembered = true): boolean {
  if (!editing) return form.password !== "";
  return (form.passwordMode === "keep" && remembered) || (form.passwordMode === "set" && form.password !== "");
}

// ── Shared wording with the TUI (phase 6b closing fixes) ────────────────

/** Replaces the Keep and destination lines once HostName, User or Port changed. */
export const DESTINATION_CHANGED_NOTE =
  "Changing the server or user forgets the remembered password. Choose Change to set one for the new destination.";
export const PASSWORD_REMEMBERED = "A password is remembered";
export const NO_PASSWORD_REMEMBERED = "No password remembered";
/** The "no key" choice of the key picker. */
export const KEY_NONE_LABEL = "none · agent and default keys";
/** The Include line banner and the import report (and the CLI). */
export const INCLUDE_MESSAGE = "Optional: for ssh, scp and git to see these hosts too, add this line at the top of ~/.ssh/config:";
/** Read-only body of a host of ~/.ssh/config. */
export const READ_ONLY_SSH_CONFIG = "Defined in ~/.ssh/config, which Portico never edits. Duplicate it as a Portico host to change it.";
/** Read-only body of a host of the old imports/termius.conf. */
export const READ_ONLY_IMPORTED =
  "Imported by an earlier version of Portico (imports/termius.conf), which it never edits. Duplicate it as a Portico host to change it.";
/** Under the edit form of a Portico host created by an import. */
export const IMPORTED_EDIT_HELP = "Editing it keeps later imports from updating it.";

const SOURCE_LABELS: Record<ImportSource, string> = { termius: "Termius", termix: "Termix" };

/** "Portico", or "Portico · from Termix" for a host an import created. */
export function originLabel(origin: Host["origin"], importedFrom: ImportSource | null | undefined): string {
  if (origin === "portico") return importedFrom ? `Portico · from ${SOURCE_LABELS[importedFrom]}` : "Portico";
  return origin === "imported" ? "Imported" : "~/.ssh/config";
}

/** `user@host:port` of a host as the list resolves it. */
export function accountText(host: Pick<Host, "user" | "hostname" | "port">): string {
  return `${host.user ? `${host.user}@` : ""}${host.hostname}:${host.port}`;
}

/** The delete confirmation (same text as the TUI), cut where the alias and
 * the account are shown in mono. */
export function deleteConfirmation(host: Pick<Host, "alias" | "user" | "hostname" | "port">): { alias: string; account: string; rest: string } {
  return {
    alias: host.alias,
    account: accountText(host),
    rest: "and its tags and notes. Its remembered password is forgotten unless another host uses the same account. Open sessions stay connected. This cannot be undone.",
  };
}

/** The whole delete confirmation as one sentence (tests, accessibility). */
export function deleteConfirmationText(host: Pick<Host, "alias" | "user" | "hostname" | "port">): string {
  const d = deleteConfirmation(host);
  return `Delete ${d.alias} (${d.account}) ${d.rest}`;
}

/** Title of the read-only dialog opened by "Delete host" on a host Portico does not own. */
export const cannotDeleteTitle = (alias: string) => `${alias} can't be deleted here`;

/** True when HostName, User or Port differ from the values the form opened with. */
export function destinationChanged(initial: HostForm, form: HostForm): boolean {
  return form.hostname.trim() !== initial.hostname.trim() || form.user.trim() !== initial.user.trim() || form.port.trim() !== initial.port.trim();
}

/** B1: a password could not be saved because there is no vault (create
 * one) or it is locked (unlock it): the vault dialog to chain before
 * retrying; `null` for any other error. */
export function vaultFix(code: string): "create" | "unlock" | null {
  if (code === "vault_missing") return "create";
  if (code === "vault_locked") return "unlock";
  return null;
}

export function emptyForm(): HostForm {
  return {
    alias: "",
    hostname: "",
    user: "",
    port: "",
    identity: "",
    proxyJump: "",
    tags: [],
    group: "",
    favorite: false,
    notes: "",
    passwordMode: "keep",
    password: "",
  };
}

/** The form of a Portico host, from what hosts.conf itself holds
 * (`ManagedHost`, never the resolved config: an earlier value elsewhere could
 * hide one, and saving would erase it). Its key is preselected when it is a
 * discovered one (`keyId`, mapped in Rust); otherwise "keep" stands for it
 * (the path stays in Rust). With no password remembered, Keep and Forget
 * mean nothing: the form starts on "Change" with an empty field. */
export function formFromHost(host: Host, own: ManagedHost): HostForm {
  return {
    alias: host.alias,
    hostname: own.hostname,
    user: own.user ?? "",
    port: own.port != null ? String(own.port) : "",
    identity: own.keyId ? keyValue(own.keyId) : own.identityFile ? KEEP_IDENTITY : "",
    proxyJump: own.proxyJump ?? "",
    tags: [...host.tags],
    group: host.group ?? "",
    favorite: host.favorite,
    notes: host.notes ?? "",
    passwordMode: own.passwordRemembered ? "keep" : "set",
    password: "",
  };
}

/** True when saving would change something (the password counts once typed or forgotten). */
export function isDirty(initial: HostForm, form: HostForm): boolean {
  const same = (a: string[], b: string[]) => a.length === b.length && a.every((t, i) => t === b[i]);
  return (
    form.alias.trim() !== initial.alias.trim() ||
    form.hostname.trim() !== initial.hostname.trim() ||
    form.user.trim() !== initial.user.trim() ||
    form.port.trim() !== initial.port.trim() ||
    form.identity !== initial.identity ||
    form.proxyJump !== initial.proxyJump ||
    !same(form.tags, initial.tags) ||
    form.group.trim() !== initial.group.trim() ||
    form.favorite !== initial.favorite ||
    form.notes !== initial.notes ||
    form.passwordMode !== initial.passwordMode ||
    form.password !== ""
  );
}

const MAX_U32 = 4294967295;
const PORT_HINT = "A port is a number from 1 to 65535.";

/** `null` for an empty field; `undefined` when it is not a number. Range
 * checks are the engine's (`host_invalid_port`). */
export function parsePort(text: string): number | null | undefined {
  const t = text.trim();
  if (!t) return null;
  if (!/^\d{1,10}$/.test(t)) return undefined;
  const n = Number(t);
  return n <= MAX_U32 ? n : undefined;
}

export type DraftResult = { ok: true; draft: HostDraft } | { ok: false; field: FieldName; error: ErrorSummary };

/** The IPC draft. `editing`: the dialog edits an existing host (password
 * choices, "keep" key); creating, a typed password is remembered.
 * `remembered`: a password is remembered for the host (edit); without one,
 * an empty "Change" field changes nothing. */
export function toDraft(form: HostForm, editing: boolean, remembered = true): DraftResult {
  const port = parsePort(form.port);
  if (port === undefined) {
    return { ok: false, field: "port", error: { code: "host_invalid_port", message: "the port must be a number", hint: PORT_HINT } };
  }
  let password: PasswordChange = { kind: "keep" };
  if (!editing) {
    if (form.password) password = { kind: "set", value: form.password };
  } else if (form.passwordMode === "forget") {
    password = { kind: "forget" };
  } else if (form.passwordMode === "set") {
    if (!form.password && !remembered) {
      password = { kind: "keep" };
    } else if (!form.password) {
      return {
        ok: false,
        field: "password",
        error: { code: "host_password_empty", message: "type the new password", hint: "Or choose Keep to leave the remembered one as it is." },
      };
    } else {
      password = { kind: "set", value: form.password };
    }
  }

  let identity: IdentityChoice = { kind: "none" };
  if (form.identity === KEEP_IDENTITY) identity = editing ? { kind: "keep" } : { kind: "none" };
  else if (form.identity.startsWith("key:")) identity = { kind: "key", id: form.identity.slice(4) };
  const optional = (text: string) => (text.trim() ? text.trim() : null);
  return {
    ok: true,
    draft: {
      alias: form.alias.trim(),
      hostname: form.hostname.trim(),
      user: optional(form.user),
      port,
      identity,
      proxyJump: form.proxyJump || null,
      tags: [...form.tags],
      group: optional(form.group),
      favorite: form.favorite,
      notes: form.notes.trim() ? form.notes : null,
      password,
    },
  };
}

/** Engine error codes → the field they are shown under (by code, never by message). */
const FIELD_OF: Record<string, FieldName> = {
  host_invalid_alias: "alias",
  host_alias_taken: "alias",
  host_alias: "alias",
  host_invalid_hostname: "hostname",
  invalid_host: "hostname",
  host_invalid_user: "user",
  host_invalid_port: "port",
  host_invalid_identity_file: "identity",
  key_unknown: "identity",
  key_public_only: "identity",
  host_invalid_proxy_jump: "proxyJump",
  host_proxy_jump_cycle: "proxyJump",
  host_tag: "tags",
  host_tags: "tags",
  host_group: "group",
  host_notes: "notes",
  host_password_empty: "password",
  secrets: "password",
  vault_locked: "password",
  vault_missing: "password",
  vault_corrupt: "password",
  vault_version: "password",
};

/** The field an error belongs to, or null (shown under the form: a
 * hand-edited hosts.conf, a host in use, a read-only host…). */
export function fieldOf(code: string): FieldName | null {
  return Object.hasOwn(FIELD_OF, code) ? (FIELD_OF[code] ?? null) : null;
}

/** Codes whose message starts with a host alias (`nas is already defined…`):
 * shown as written, never capitalized (an alias is a name). */
const NAMED: ReadonlySet<string> = new Set(["host_alias_taken", "host_read_only", "host_in_use", "host_not_duplicable"]);

/** Whether `sentence()` may capitalize this error's message. */
export const capitalizable = (code: string): boolean => !NAMED.has(code);

/** Fields in form order: the first one with an error takes the focus. */
export const FIELD_ORDER: FieldName[] = ["alias", "hostname", "user", "port", "identity", "proxyJump", "password", "tags", "group", "notes"];

/** Aliases Portico writes: `[a-z0-9_.-]`, 64 at most, not starting with `-` or `.`. */
export function suggestAlias(alias: string, taken: string[]): string {
  const base =
    alias
      .toLowerCase()
      .replace(/[^a-z0-9_.-]+/g, "-")
      .replace(/^[-.]+/, "")
      .slice(0, 55) || "host";
  const used = new Set(taken.map((a) => a.toLowerCase()));
  for (let n = 1; ; n++) {
    const candidate = n === 1 ? `${base}-copy` : `${base}-copy-${n}`;
    if (!used.has(candidate)) return candidate;
  }
}
