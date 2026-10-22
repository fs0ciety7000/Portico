// The import dialog's logic ("Import hosts…"): which private keys are
// ticked, which destination changes are accepted, whether the passphrase
// field shows, the request sent to Rust, and the texts of the preview and
// the report.
//
// Pure module (no DOM, no Svelte, type-only imports): `scripts/test-import.ts`
// runs it under Node. Rust checks every key id against the plan again; the
// passphrase becomes a SecretString there and is cleared here after the call.
import type { IgnoredSecrets, ImportApplyRequest, ImportDestination, ImportNotice, ImportPreview, ImportReport, PlannedHost, PlannedKey, WrittenKey } from "./api";

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

/** The footer's reason while the passphrases differ (Import is disabled). */
export const PASSPHRASE_MISMATCH_NOTE = "Passphrases differ";

/** `user@host:port` of a destination (the port only when not 22). */
export function destinationText(d: Pick<ImportDestination, "hostname" | "user" | "port">): string {
  return `${d.user ? `${d.user}@` : ""}${d.hostname}${d.port ? `:${d.port}` : ""}`;
}

/** Shown next to a key whose fingerprint the export only claims. */
export const UNVERIFIED_FINGERPRINT = "fingerprint not verified (encrypted key)";

/** Hosts whose re-import points the alias at another server. */
export function destinationChanges(hosts: PlannedHost[]): PlannedHost[] {
  return hosts.filter((h) => h.previous !== null);
}

/** The accepted aliases the preview still holds as destination changes, in plan order. */
export function keepRedirects(hosts: PlannedHost[], accepted: readonly string[]): string[] {
  const set = new Set(accepted);
  return destinationChanges(hosts)
    .filter((h) => set.has(h.alias))
    .map((h) => h.alias);
}

/** Accepts or refuses the destination change of `alias` (unticked by default). */
export function toggleRedirect(hosts: PlannedHost[], accepted: readonly string[], alias: string): string[] {
  if (!destinationChanges(hosts).some((h) => h.alias === alias)) return keepRedirects(hosts, accepted);
  const set = new Set(accepted);
  if (set.has(alias)) set.delete(alias);
  else set.add(alias);
  return keepRedirects(hosts, [...set]);
}

/** The hosts the import writes: every one, except a destination change not accepted. */
export function hostsToWrite(hosts: PlannedHost[], accepted: readonly string[]): PlannedHost[] {
  const set = new Set(keepRedirects(hosts, accepted));
  return hosts.filter((h) => h.previous === null || set.has(h.alias));
}

/** "Updates N": hosts of an earlier import the import writes (in-place
 * updates, plus the destination changes accepted), as
 * `ImportPlan::updates` counts them. */
export function updatesCount(hosts: PlannedHost[], accepted: readonly string[]): number {
  return hostsToWrite(hosts, accepted).filter((h) => h.update).length;
}

/** The address column of a preview row: a destination change keeps showing
 * where the alias points today (`previous`) until it is accepted. */
export function rowAddress(h: PlannedHost, accepted: readonly string[]): string {
  return h.previous && !accepted.includes(h.alias) ? destinationText(h.previous) : destinationText(h);
}

/** Under an accepted destination change (same wording as the CLI). */
export function redirectNote(previous: ImportDestination): string {
  return previous.identityFile ? `Pointed at the new server; key ${previous.identityFile} no longer used.` : "Pointed at the new server.";
}

/** The request for `import_apply`. The passphrase rides along only when the
 * field shows and is filled (Rust ignores it otherwise too). */
export function applyRequest(preview: ImportPreview, selected: readonly string[], passphrase: string, accepted: readonly string[] = []): ImportApplyRequest {
  const keys = keepSelected(preview.keys, selected);
  return {
    token: preview.token,
    prefix: preview.prefix,
    keys,
    redirect: keepRedirects(preview.hosts, accepted),
    passphrase: passphrase && wantsPassphrase(preview.keys, keys) ? passphrase : null,
  };
}

/** The hosts using a key, shortened: `web, db and 3 more`. */
export function keyHosts(aliases: string[], shown = 3): string {
  if (aliases.length === 0) return "no host imported";
  if (aliases.length <= shown) return aliases.join(", ");
  return `${aliases.slice(0, shown).join(", ")} and ${aliases.length - shown} more`;
}

/** The primary button of the preview (a destination change counts once accepted). */
export function applyLabel(preview: ImportPreview, selected: readonly string[], accepted: readonly string[] = []): string {
  const hosts = plural(hostsToWrite(preview.hosts, accepted).length, "host");
  const keys = keepSelected(preview.keys, selected).length;
  return keys ? `Import ${hosts} and ${plural(keys, "key")}` : `Import ${hosts}`;
}

/** What a notice code means beyond its message, when the message alone is not enough. */
const EXPLAIN: Record<string, string> = {
  key_unreadable: "Its text is never shown, here or in the logs.",
  key_not_used: "The host does not log in with a key: the key is not offered.",
  key_skipped: "Selected, but not imported.",
  destination_changed: "Re-import and tick “Accept the new destination” to point the alias there.",
};

export function noticeExplanation(code: string): string | null {
  return Object.hasOwn(EXPLAIN, code) ? (EXPLAIN[code] ?? null) : null;
}

/** Notices that concern keys (shown under the key list), the others. */
export function splitNotices(list: ImportNotice[]): { keys: ImportNotice[]; other: ImportNotice[] } {
  const keyCodes = new Set(["key_unreadable", "key_not_used", "key_skipped"]);
  return { keys: list.filter((n) => keyCodes.has(n.code)), other: list.filter((n) => !keyCodes.has(n.code)) };
}

/** Destination changes (skipped unless accepted), the other notices. */
export function splitDestinationChanges(list: ImportNotice[]): { changes: ImportNotice[]; other: ImportNotice[] } {
  return {
    changes: list.filter((n) => n.code === "destination_changed"),
    other: list.filter((n) => n.code !== "destination_changed"),
  };
}

/** What was not imported, by cause, as the engine words it
 * (`IgnoredSecrets::advice`): "Not imported: 1 password, 2 unticked keys,
 * 1 unreadable key."; `null` when nothing was left out. The desktop app
 * shows the engine's own text: this one serves the demo. */
export function ignoredSecretsAdvice(secrets: IgnoredSecrets): string | null {
  const { passwords, keys, untickedKeys, unreadableKeys } = secrets;
  const other = Math.max(0, keys - untickedKeys - unreadableKeys);
  const otherWord = untickedKeys > 0 || unreadableKeys > 0 ? "other key" : "key";
  const parts = (
    [
      [passwords, "password"],
      [untickedKeys, "unticked key"],
      [unreadableKeys, "unreadable key"],
      [other, otherWord],
    ] as const
  )
    .filter(([n]) => n > 0)
    .map(([n, word]) => plural(n, word));
  return parts.length ? `Not imported: ${parts.join(", ")}.` : null;
}

/** The action on the export, as the engine words it
 * (`IgnoredSecrets::delete_export_advice`): "Delete hosts.json now: it holds
 * 1 password and 4 private keys (in clear text or encrypted)."; `null` when
 * it held no secret. Demo only (the desktop app shows the engine's text). */
export function deleteExportAdvice(secrets: IgnoredSecrets, fileName: string): string | null {
  const keys = Math.max(secrets.keysInExport, secrets.keys);
  const held = (
    [
      [secrets.passwords, "password"],
      [keys, "private key"],
    ] as const
  )
    .filter(([n]) => n > 0)
    .map(([n, word]) => plural(n, word));
  return held.length ? `Delete ${fileName} now: it holds ${held.join(" and ")} (in clear text or encrypted).` : null;
}

/** The short version of the delete action, for the report's footer note. */
export function shortDeleteAdvice(fileName: string): string {
  return `Delete ${fileName} now: it holds secrets.`;
}

/** A key file the import wrote: "written · 0600 · encrypted" (or "· no
 * passphrase"), then "used by a, b" (same wording as the CLI). */
export function writtenKeyLine(key: WrittenKey): string {
  const parts = ["written", "0600"];
  if (key.encrypted !== null) parts.push(key.encrypted ? "encrypted" : "no passphrase");
  if (key.usedBy.length) parts.push(`used by ${key.usedBy.join(", ")}`);
  return parts.join(" · ");
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
