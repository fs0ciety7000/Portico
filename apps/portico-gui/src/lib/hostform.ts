// The host form ("Add host…", "Edit host…"): its fields as typed, the draft
// sent to the engine, dirty tracking and where an engine error goes.
//
// Pure module (no DOM, no Svelte, type-only imports): `scripts/test-hostform.ts`
// runs it under Node. The engine validates everything; the only checks here
// are those the IPC types need (a port must be a number) and the form's own
// (a password to set must be typed).
import type { ErrorSummary, Host, HostDraft, IdentityChoice, LocalKey, PasswordChange } from "./api";

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

/** The form of a Portico host, from its own directives (never inherited
 * values). Its key is preselected when it is a discovered one; otherwise
 * "keep" stands for it (the path stays in Rust). */
export function formFromHost(host: Host, keys: LocalKey[]): HostForm {
  const own = host.managed;
  const file = own?.identityFile ?? null;
  const key = file ? keys.find((k) => k.hasPrivate && k.privateDisplay === file) : undefined;
  return {
    alias: host.alias,
    hostname: own?.hostname ?? host.hostname,
    user: own?.user ?? "",
    port: own?.port != null ? String(own.port) : "",
    identity: key ? keyValue(key.id) : file ? KEEP_IDENTITY : "",
    proxyJump: own?.proxyJump ?? "",
    tags: [...host.tags],
    group: host.group ?? "",
    favorite: host.favorite,
    notes: host.notes ?? "",
    passwordMode: "keep",
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
 * choices, "keep" key); creating, a typed password is remembered. */
export function toDraft(form: HostForm, editing: boolean): DraftResult {
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
    if (!form.password) {
      return {
        ok: false,
        field: "password",
        error: { code: "host_password_empty", message: "type the new password", hint: "Or choose Keep to leave the remembered one as it is." },
      };
    }
    password = { kind: "set", value: form.password };
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
