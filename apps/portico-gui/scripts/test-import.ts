// Checks of the import dialog logic (src/lib/importform.ts), run by `pnpm check`.
// Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { ImportNotice, ImportPreview, ImportReport, PlannedKey } from "../src/lib/api.ts";
import {
  applyLabel,
  applyRequest,
  dialogTitle,
  keepSelected,
  keyHosts,
  keysToWrite,
  keyType,
  noticeExplanation,
  passphraseError,
  reportKeysLine,
  shortFingerprint,
  splitNotices,
  toggleKey,
  wantsPassphrase,
} from "../src/lib/importform.ts";

const key = (id: string, extra: Partial<PlannedKey> = {}): PlannedKey => ({
  id,
  algorithm: "ssh-ed25519",
  fingerprint: id,
  format: "open_ssh",
  encrypted: false,
  aliases: ["web"],
  local: null,
  ...extra,
});

const PLAIN = key("SHA256:plainplainplainplain");
const LOCKED = key("SHA256:lockedlockedlocked", { algorithm: "ssh-rsa", format: "pem", encrypted: true, aliases: ["bastion"] });
const LOCAL = key("SHA256:locallocallocal", { local: "~/.ssh/id_ed25519" });
const KEYS = [PLAIN, LOCKED, LOCAL];

const preview = (keys: PlannedKey[] = KEYS, hosts = 3): ImportPreview => ({
  token: 7,
  fileName: "termix_export.json",
  source: "Termix",
  format: "Termix JSON",
  prefixText: "termix-",
  records: hosts,
  prefix: false,
  hosts: Array.from({ length: hosts }, (_, i) => ({
    alias: `h${i}`,
    label: `h${i}`,
    hostname: "203.0.113.1",
    user: null,
    port: null,
    group: null,
    tags: [],
    notes: null,
    record: i + 1,
    favorite: false,
    key: null,
    update: false,
    sourceId: String(i),
  })),
  skipped: [],
  warnings: [],
  secrets: { passwords: 0, keys: 0, keysInExport: keys.length },
  secretsAdvice: null,
  identityFiles: [],
  target: "~/.config/portico/hosts.conf",
  includeLine: "Include ~/.config/portico/hosts.conf",
  replaces: null,
  keys,
  keysDir: "~/.ssh/portico/",
});

test("no key is ticked by default; toggling keeps plan order and only plan ids", () => {
  assert.deepEqual(keepSelected(KEYS, []), []);
  let selected = toggleKey(KEYS, [], LOCKED.id);
  selected = toggleKey(KEYS, selected, PLAIN.id);
  assert.deepEqual(selected, [PLAIN.id, LOCKED.id], "plan order");
  assert.deepEqual(toggleKey(KEYS, selected, PLAIN.id), [LOCKED.id], "untick");
  assert.deepEqual(toggleKey(KEYS, selected, "SHA256:stranger"), selected, "an id the plan does not hold is ignored");
  // A replan keeps the ticks of keys still there.
  assert.deepEqual(keepSelected([LOCKED], selected), [LOCKED.id]);
});

test("the passphrase field shows only for an unencrypted key that will be written", () => {
  assert.equal(wantsPassphrase(KEYS, []), false);
  assert.equal(wantsPassphrase(KEYS, [LOCKED.id]), false, "encrypted: written as it is");
  assert.equal(wantsPassphrase(KEYS, [LOCAL.id]), false, "reused: nothing written");
  assert.equal(wantsPassphrase(KEYS, [PLAIN.id]), true);
  assert.equal(wantsPassphrase(KEYS, [LOCKED.id, PLAIN.id]), true);
  assert.deepEqual(
    keysToWrite(KEYS, [PLAIN.id, LOCAL.id]).map((k) => k.id),
    [PLAIN.id],
  );
});

test("passphrase confirmation", () => {
  assert.equal(passphraseError("", ""), null, "optional");
  assert.equal(passphraseError("abc", "abc"), null);
  assert.match(passphraseError("abc", "abd") ?? "", /do not match/);
  assert.match(passphraseError("", "abc") ?? "", /do not match/);
});

test("the apply request: token, prefix, ticked ids, passphrase only when it applies", () => {
  const p = preview();
  assert.deepEqual(applyRequest(p, [], "secret"), { token: 7, prefix: false, keys: [], passphrase: null });
  assert.deepEqual(applyRequest(p, [PLAIN.id, "SHA256:stranger"], "secret"), { token: 7, prefix: false, keys: [PLAIN.id], passphrase: "secret" });
  assert.equal(applyRequest(p, [LOCKED.id], "secret").passphrase, null, "an encrypted key keeps its own");
  assert.equal(applyRequest(p, [PLAIN.id], "").passphrase, null, "empty: none");
});

test("key texts", () => {
  assert.equal(keyType("ssh-ed25519"), "Ed25519");
  assert.equal(keyType("ecdsa-sha2-nistp256"), "ECDSA P-256");
  assert.equal(keyType("ssh-rsa"), "RSA");
  assert.equal(keyType("toString"), "toString", "no prototype lookup");
  assert.equal(shortFingerprint("SHA256:q1Xv9mB3rTzK0pW7nLc2fYhJ8sDgE4uRaO6iVbN5kMw"), "SHA256:q1Xv9mB3rT…");
  assert.equal(shortFingerprint("SHA256:short"), "SHA256:short");
  assert.equal(keyHosts(["a", "b"]), "a, b");
  assert.equal(keyHosts(["a", "b", "c", "d", "e"]), "a, b, c and 2 more");
  assert.equal(keyHosts([]), "no host imported");
  assert.equal(applyLabel(preview(), []), "Import 3 hosts");
  assert.equal(applyLabel(preview(KEYS, 1), [PLAIN.id]), "Import 1 host and 1 key");
  assert.equal(applyLabel(preview(), [PLAIN.id, LOCKED.id]), "Import 3 hosts and 2 keys");
  assert.equal(dialogTitle("picking", null), "Import hosts");
  assert.equal(dialogTitle("preview", "Termix"), "Import from Termix");
  assert.equal(dialogTitle("done", "Termius"), "Imported from Termius");
});

test("notices: new codes explained, key notices apart", () => {
  assert.match(noticeExplanation("edited_since_import") ?? "", /changed by hand.*left as is/i);
  assert.ok(noticeExplanation("key_unreadable"));
  assert.ok(noticeExplanation("key_not_used"));
  assert.ok(noticeExplanation("key_skipped"));
  assert.equal(noticeExplanation("ignored_setting"), null, "the message says it all");
  assert.equal(noticeExplanation("alias_renamed"), null);
  assert.equal(noticeExplanation("__proto__"), null);
  const n = (code: string): ImportNotice => ({ code, record: 1, label: null, message: "m" });
  const split = splitNotices([n("key_unreadable"), n("tag_normalized"), n("key_skipped"), n("ignored_setting")]);
  assert.deepEqual(split.keys.map((x) => x.code), ["key_unreadable", "key_skipped"]);
  assert.deepEqual(split.other.map((x) => x.code), ["tag_normalized", "ignored_setting"]);
});

test("report: keys imported, reused, skipped", () => {
  const report = (keys: ImportReport["keys"]): ImportReport => ({
    formatLabel: "Termix JSON",
    source: "Termix",
    target: "~/.config/portico/hosts.conf",
    unchanged: false,
    imported: ["web"],
    updated: [],
    skipped: [],
    warnings: [],
    secrets: { passwords: 0, keys: 0, keysInExport: 2 },
    secretsAdvice: null,
    identityFiles: [],
    metaUpdated: 0,
    includeLine: "Include x",
    includeActive: false,
    keys,
  });
  assert.equal(reportKeysLine(report({ imported: [], reused: [], skipped: 0 })), null);
  assert.equal(reportKeysLine(report({ imported: ["~/.ssh/portico/web"], reused: ["~/.ssh/id_ed25519"], skipped: 1 })), "1 key imported, 1 reused from ~/.ssh, 1 not imported.");
  assert.equal(reportKeysLine(report({ imported: [], reused: [], skipped: 2 })), "2 not imported.");
});
