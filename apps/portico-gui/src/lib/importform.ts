// The import dialog's logic ("Import hosts…"): which private keys are
// ticked, whether the passphrase field shows, the request sent to Rust, and
// the texts of the preview and the report.
//
// Pure module (no DOM, no Svelte, type-only imports): `scripts/test-import.ts`
// runs it under Node. Rust checks every key id against the plan again; the
// passphrase becomes a SecretString there and is cleared here after the call.
import type { ImportApplyRequest, ImportNotice, ImportPreview, ImportReport, PlannedKey } from "./api";

export const plural = (n: number, word: string, many = `${word}s`) => `${n} ${n === 1 ? word : many}`;

/** `SHA256:abcdefghij…`: enough to tell keys apart, the whole one in a tooltip. */
export function shortFingerprint(fingerprint: string, chars = 10): string {
  const at = fingerprint.indexOf(":");
  const [algo, digest] = at < 0 ? ["", fingerprint] : [fingerprint.slice(0, at + 1), fingerprint.slice(at + 1)];
  return digest.length > chars ? `${algo}${digest.slice(0, chars)}…` : fingerprint;
}

const TYPES: Record<string, string> = {
  "ssh-ed25519": "Ed25519",
  "ssh-rsa": "RSA",
  "ecdsa-sha2-nistp256": "ECDSA P-256",
  "ecdsa-sha2-nistp384": "ECDSA P-384",
  "ecdsa-sha2-nistp521": "ECDSA P-521",
  "sk-ssh-ed25519@openssh.com": "Ed25519-SK",
  "sk-ecdsa-sha2-nistp256@openssh.com": "ECDSA-SK",
  "ssh-dss": "DSA",
};

/** `Ed25519`, `RSA`, `ECDSA P-256`… (the algorithm name otherwise). */
export function keyType(algorithm: string): string {
  return Object.hasOwn(TYPES, algorithm) ? (TYPES[algorithm] ?? algorithm) : algorithm;
}

/** The ids of `selected` the preview still holds (a replan may drop some), in plan order. */
export function keepSelected(keys: PlannedKey[], selected: readonly string[]): string[] {
  const set = new Set(selected);
  return keys.filter((k) => set.has(k.id)).map((k) => k.id);
}

/** Ticks or unticks `id` (only an id of the preview). */
export function toggleKey(keys: PlannedKey[], selected: readonly string[], id: string): string[] {
  if (!keys.some((k) => k.id === id)) return keepSelected(keys, selected);
  const set = new Set(selected);
  if (set.has(id)) set.delete(id);
  else set.add(id);
  return keepSelected(keys, [...set]);
}

/** Ticked keys that will be written to ~/.ssh/portico (not reused). */
export function keysToWrite(keys: PlannedKey[], selected: readonly string[]): PlannedKey[] {
  const set = new Set(selected);
  return keys.filter((k) => set.has(k.id) && !k.local);
}

/** The passphrase field shows once an unencrypted key is to be written:
 * an encrypted one keeps its own, a reused one is not written. */
export function wantsPassphrase(keys: PlannedKey[], selected: readonly string[]): boolean {
  return keysToWrite(keys, selected).some((k) => !k.encrypted);
}

/** `null` when the passphrase can be used (empty: none), the error otherwise. */
export function passphraseError(passphrase: string, confirm: string): string | null {
  if (!passphrase && !confirm) return null;
  if (passphrase !== confirm) return "The passphrases do not match.";
  return null;
}

/** The request for `import_apply`. The passphrase rides along only when the
 * field shows and is filled (Rust ignores it otherwise too). */
export function applyRequest(preview: ImportPreview, selected: readonly string[], passphrase: string): ImportApplyRequest {
  const keys = keepSelected(preview.keys, selected);
  return {
    token: preview.token,
    prefix: preview.prefix,
    keys,
    passphrase: passphrase && wantsPassphrase(preview.keys, keys) ? passphrase : null,
  };
}

/** The hosts using a key, shortened: `web, db and 3 more`. */
export function keyHosts(aliases: string[], shown = 3): string {
  if (aliases.length === 0) return "no host imported";
  if (aliases.length <= shown) return aliases.join(", ");
  return `${aliases.slice(0, shown).join(", ")} and ${aliases.length - shown} more`;
}

/** The primary button of the preview. */
export function applyLabel(preview: ImportPreview, selected: readonly string[]): string {
  const hosts = plural(preview.hosts.length, "host");
  const keys = keepSelected(preview.keys, selected).length;
  return keys ? `Import ${hosts} and ${plural(keys, "key")}` : `Import ${hosts}`;
}

/** What a notice code means beyond its message, when the message alone is not enough. */
const EXPLAIN: Record<string, string> = {
  edited_since_import: "The host was changed by hand since it was imported: it is left as is.",
  key_unreadable: "Its text is never shown, here or in the logs.",
  key_not_used: "The host does not log in with a key: the key is not offered.",
  key_skipped: "Selected, but not imported.",
};

export function noticeExplanation(code: string): string | null {
  return Object.hasOwn(EXPLAIN, code) ? (EXPLAIN[code] ?? null) : null;
}

/** Notices that concern keys (shown under the key list), the others. */
export function splitNotices(list: ImportNotice[]): { keys: ImportNotice[]; other: ImportNotice[] } {
  const keyCodes = new Set(["key_unreadable", "key_not_used", "key_skipped"]);
  return { keys: list.filter((n) => keyCodes.has(n.code)), other: list.filter((n) => !keyCodes.has(n.code)) };
}

/** One line on the keys of a report, `null` when the export had none. */
export function reportKeysLine(report: ImportReport): string | null {
  const { imported, reused, skipped } = report.keys;
  if (!imported.length && !reused.length && !skipped) return null;
  const parts: string[] = [];
  if (imported.length) parts.push(`${plural(imported.length, "key")} imported`);
  if (reused.length) parts.push(`${reused.length} reused from ~/.ssh`);
  if (skipped) parts.push(`${skipped} not imported`);
  return `${parts.join(", ")}.`;
}

/** Title of the dialog: the source once known. */
export function dialogTitle(kind: "picking" | "preview" | "applying" | "done" | "error", source: string | null): string {
  if (kind === "done") return source ? `Imported from ${source}` : "Imported";
  return source ? `Import from ${source}` : "Import hosts";
}
