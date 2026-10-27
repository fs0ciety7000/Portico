// Checks of the folder sync wording and helpers (src/lib/syncview.ts), run
// by `pnpm check`. Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { test } from "node:test";
import type { AlertKind, ConflictSide, FolderReason, SyncStatus } from "../src/lib/api.ts";
import { ago, conflictRows, defaultChoice, destination, explainAlert, explainFolder, probeVerdict, strengthView, syncLabel } from "../src/lib/syncview.ts";

const NOW = 1_800_000_000_000;
const secs = NOW / 1000;

test("relative times read like the TUI's", () => {
  assert.equal(ago(secs - 10, NOW), "just now");
  assert.equal(ago(secs - 120, NOW), "2 min ago");
  assert.equal(ago(secs - 3 * 3600, NOW), "3 h ago");
  assert.equal(ago(secs - 2 * 86400, NOW), "2 d ago");
  // A clock ahead of ours never shows a negative time.
  assert.equal(ago(secs + 500, NOW), "just now");
});

test("every status has a label and a tone", () => {
  const cases: [SyncStatus, string, string][] = [
    [{ state: "disabled" }, "Sync off", "muted"],
    [{ state: "locked" }, "Sync locked", "warning"],
    [{ state: "idle", last_pass: secs - 120, devices: 2, conflicts: 0, issues: 0, pending_confirmations: 0 }, "Synced 2 min ago", "quiet"],
    [{ state: "syncing" }, "Syncing…", "busy"],
    [{ state: "waiting", reason: "placeholder", device: "laptop (3f9c0ae1)" }, "Waiting for laptop (3f9c0ae1)", "busy"],
    [{ state: "folder_unavailable", reason: "missing" }, "Sync folder unavailable", "warning"],
    [{ state: "alert", kind: "fork", device: null }, "Sync alert", "error"],
    [{ state: "error", summary: { code: "sync_corrupt", message: "x", hint: null } }, "Sync failed", "error"],
  ];
  for (const [status, label, tone] of cases) assert.deepEqual(syncLabel(status, NOW), { label, tone });
});

test("alerts name the device and say what to do", () => {
  const kinds: AlertKind[] = [
    "rollback",
    "fork",
    "restored",
    "bundle_tampered",
    "keys_rotated_elsewhere",
    "passphrase_changed",
    "third_party_claim",
    "merge_refused",
    "divergent",
    "dropped_values",
  ];
  for (const kind of kinds) {
    const e = explainAlert(kind, "laptop (3f9c0ae1)");
    assert.ok(e.title && e.hint, kind);
  }
  assert.match(explainAlert("passphrase_changed", "laptop (3f9c0ae1)").body, /laptop \(3f9c0ae1\)/);
  assert.match(explainAlert("fork", null).body, /another device/);
  const reasons: FolderReason[] = ["missing", "not_mounted", "permission_denied", "bundle_missing", "unsafe", "too_many_entries"];
  for (const r of reasons) assert.ok(explainFolder(r).hint, r);
});

test("probe verdicts", () => {
  assert.equal(probeVerdict({ kind: "usable" }).tone, "ok");
  assert.equal(probeVerdict({ kind: "existing_bundle", devices: 1 }).title, "A Portico sync with 1 device");
  assert.equal(probeVerdict({ kind: "incomplete" }).tone, "wait");
  const refused = probeVerdict({ kind: "refused", summary: { code: "sync_folder_unsafe", message: "inside ~/.ssh", hint: "Choose another" } });
  assert.deepEqual([refused.tone, refused.title, refused.hint], ["error", "inside ~/.ssh", "Choose another"]);
});

test("the meter follows the core's verdict, not its own guess", () => {
  assert.equal(strengthView(80, false).tone, "weak");
  assert.equal(strengthView(62, true).tone, "fair");
  assert.equal(strengthView(90, true).tone, "good");
  assert.ok(strengthView(0, false).fill > 0);
  assert.equal(strengthView(500, true).fill, 1);
});

const side = (index: number, value: ConflictSide["value"], local = false): ConflictSide => ({ index, devices: [local ? "me (00000000)" : "laptop (3f9c0ae1)"], local, at: secs, value });

test("tags preselect the union, snippets keep both, other records this device's side", () => {
  assert.deepEqual(defaultChoice([side(0, { type: "tags", value: ["a"] }, true), side(1, { type: "tags", value: ["b"] })]), { type: "union" });
  const snippet = { name: "x", command: "ls", description: null, run: true };
  assert.deepEqual(defaultChoice([side(0, { type: "snippet", value: snippet }), side(1, { type: "snippet", value: { ...snippet, command: "ls -l" } }, true)]), {
    type: "keep_both",
    keep: 1,
  });
  assert.deepEqual(defaultChoice([side(0, { type: "text", value: "a" }), side(1, { type: "text", value: "b" }, true)]), { type: "pick", index: 1 });
});

test("conflict rows mark the fields that differ", () => {
  const host = { hostname: "a.example", user: "ops", port: null, identity_file: null, proxy_jump: null };
  const rows = conflictRows([side(0, { type: "host", value: host }, true), side(1, { type: "host", value: { ...host, port: 2222 } })]);
  const port = rows.find((r) => r.field === "Port");
  assert.deepEqual(port, { field: "Port", values: ["—", "2222"], differs: true });
  assert.equal(rows.find((r) => r.field === "HostName")?.differs, false);
  const deleted = conflictRows([side(0, { type: "host", value: host }), side(1, { type: "deleted" })]);
  assert.equal(deleted[0]?.values[1], "(deleted)");
});

test("destinations read like ssh", () => {
  assert.equal(destination({ hostname: "h", user: "u", port: 22, identity_file: null, proxy_jump: "b" }), "u@h via b");
  assert.equal(destination({ hostname: "h", user: null, port: 2222, identity_file: null, proxy_jump: null }), "h:2222");
});
