// Folder sync (phase 11): wording and pure helpers shared by the status
// bar, the panel, the wizard and their tests (`scripts/test-sync.ts`).
//
// Pure module (no DOM, no Svelte): it runs under Node for the tests. The
// vocabulary matches the TUI's (docs/SYNC.md §6).
import type { AlertKind, ConflictSide, FolderProbe, FolderReason, SyncHostValue, SyncStatus, SyncSubject, SyncValue, WaitReason } from "./api";

/** Tone of the status-bar item: the colour carries meaning only. */
export type SyncTone = "muted" | "quiet" | "busy" | "warning" | "error";

/** `just now`, `2 min ago`, `3 h ago`, `4 d ago` (Unix seconds). */
export function ago(thenSecs: number, nowMs: number): string {
  const secs = Math.max(0, Math.floor(nowMs / 1000) - thenSecs);
  if (secs < 45) return "just now";
  const mins = Math.round(secs / 60);
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.round(secs / 3600);
  if (hours < 24) return `${hours} h ago`;
  return `${Math.round(secs / 86400)} d ago`;
}

/** Status-bar label and tone of a sync status. */
export function syncLabel(status: SyncStatus | null, nowMs: number): { label: string; tone: SyncTone } {
  if (!status) return { label: "Sync", tone: "muted" };
  switch (status.state) {
    case "disabled":
      return { label: "Sync off", tone: "muted" };
    case "locked":
      return { label: "Sync locked", tone: "warning" };
    case "idle":
      return { label: `Synced ${ago(status.last_pass, nowMs)}`, tone: "quiet" };
    case "syncing":
      return { label: "Syncing…", tone: "busy" };
    case "waiting":
      // The name only: the id is in the item's title (B14).
      if (status.reason === "rotation_pending") return { label: ROTATION_PENDING_LABEL, tone: "busy" };
      if (status.reason === "needs_new_keys") return { label: status.device ? `New keys needed on ${deviceName(status.device)}` : "New keys needed", tone: "warning" };
      return { label: status.device ? `Waiting for ${deviceName(status.device)}` : "Waiting for the sync folder", tone: "busy" };
    case "folder_unavailable":
      return { label: "Sync folder unavailable", tone: "warning" };
    case "alert":
      switch (status.kind) {
        case "keys_rotated_elsewhere":
          return { label: "Keys changed — enter new passphrase", tone: "warning" };
        case "rotation_conflict":
          return { label: "Key conflict", tone: "error" };
        case "device_forgotten":
          return { label: "Removed from sync", tone: "error" };
        case "revoked_writes":
          return { label: "Changes from a removed device", tone: "error" };
        default:
          return { label: "Sync alert", tone: "error" };
      }
    case "error":
      return { label: "Sync failed", tone: "error" };
    default:
      return { label: "Sync", tone: "muted" };
  }
}

/** `studio-mac (3f9c0ae1)` → `studio-mac`: where room is short. */
export function deviceName(label: string): string {
  return label.replace(/\s*\([0-9a-f]{8}\)$/, "") || label;
}

/** The status item's counts as words (wide windows): `2 conflicts · 3 to review`. */
export function countsText(conflicts: number, toReview: number): string {
  return [conflicts ? `${conflicts} ${conflicts === 1 ? "conflict" : "conflicts"}` : null, toReview ? `${toReview} to review` : null]
    .filter(Boolean)
    .join(" · ");
}

/** One explanation: what happened, and what to do. */
export interface Explanation {
  title: string;
  body: string;
  hint: string;
}

const someDevice = (device: string | null) => device ?? "another device";

/** Every alert, explained with the device named and an actionable hint. */
export function explainAlert(kind: AlertKind, device: string | null): Explanation {
  const d = someDevice(device);
  switch (kind) {
    case "rollback":
      return {
        title: "An older copy of a sync file came back",
        body: `The sync folder holds an older version of ${d}'s data than this device already saw. Portico ignored it.`,
        hint: "Check the folder's version history (cloud provider, git): something restored an old file. Nothing changed here.",
      };
    case "fork":
      return {
        title: "One device published two different versions",
        body: `${d} published two different snapshots under the same number: two machines may share one sync identity (a cloned disk or a restored backup).`,
        hint: "On the copied machine, turn sync off and on again so it joins as a new device.",
      };
    case "restored":
      return {
        title: "This device rejoined under a new identity",
        body: "This device's sync state was older than what it had already published (restored from a backup), so it rejoined under a new id. Nothing local was lost.",
        hint: "Nothing to do. The old identity shows as inactive in Devices.",
      };
    case "bundle_tampered":
      return {
        title: "The sync folder's key file was modified",
        body: "bundle.toml changed without the sync key. Portico stopped reading the folder.",
        hint: "Restore bundle.toml from the folder's history, or turn sync off and create a new sync folder.",
      };
    case "keys_rotated_elsewhere":
      return {
        title: newKeysTitle(device),
        body: "Enter the NEW passphrase chosen there. Nothing is merged until then; your local data stays as it is.",
        hint: ADOPT_UNSURE,
      };
    case "rotation_conflict":
      return {
        title: "Two devices changed the keys at the same time",
        body: `${d} and this device each replaced the sync keys. Nothing is merged until you choose which keys to keep.`,
        hint: "Use the other device's keys (with its passphrase), or keep this device's keys.",
      };
    case "device_forgotten":
      return {
        title: "This device was removed from sync",
        body: `${device ? `${device} removed this device` : "Another device removed this device"} from the device list and changed the keys. It no longer receives changes. Your local hosts, snippets and settings stay here.`,
        hint: "Turn sync off, or join again as a new device with the new passphrase.",
      };
    case "revoked_writes":
      return {
        title: "Changes from a removed device",
        body: `${d} relays changes written by a device that was removed. They are not applied.`,
        hint: `Enter the new passphrase on ${d}. Values held on this device wait in Received.`,
      };
    case "passphrase_changed":
      return {
        title: "The sync passphrase was changed elsewhere",
        body: `The passphrase was changed or reset on ${d}. If nobody you trust did it, someone with access to the folder and a device key may have.`,
        hint: "Acknowledge if you expected it. Otherwise turn sync off on this device and create a new sync folder.",
      };
    case "third_party_claim":
      return {
        title: "A device claims changes this device never made",
        body: `${d}'s data says this device made changes it never made. Portico did not merge it.`,
        hint: "Check that device. If you don't recognise it, turn sync off and create a new sync folder.",
      };
    case "merge_refused":
      return {
        title: "A device's data was not merged",
        body: `${d}'s data would exceed the sync limits (records or size). It was not merged.`,
        hint: "Remove unneeded hosts or snippets on that device, then sync again.",
      };
    case "divergent":
      return {
        title: "A device sent two values for one change",
        body: `${d} published inconsistent data. Portico kept what it had.`,
        hint: "Sync that device again. If it persists, turn sync off and on there.",
      };
    case "dropped_values":
      return {
        title: "A device removed data without replacing it",
        body: `${d}'s data drops values it had published before, without a deletion. Portico kept them.`,
        hint: "Sync that device again; update Portico there if it is older.",
      };
    default:
      return { title: "Sync alert", body: "Something in the sync folder needs a look.", hint: "Open Issues for the details." };
  }
}

/** Why the folder cannot be used now. */
export function explainFolder(reason: FolderReason): Explanation {
  switch (reason) {
    case "missing":
      return {
        title: "The sync folder is gone",
        body: "The folder (or its portico-sync/ inside) is missing. Portico never recreates it on its own.",
        hint: "Reconnect the drive or restore the folder, or turn sync off.",
      };
    case "not_mounted":
      return {
        title: "The sync folder did not answer",
        body: "Reading the folder took too long: a stalled network drive, or files still downloading.",
        hint: "Check the drive or the sync client, then Sync now.",
      };
    case "permission_denied":
      return {
        title: "Portico cannot read the sync folder",
        body: "The system refused access to the folder.",
        hint: "Give Portico access to the folder (macOS: Privacy & Security, Files and Folders).",
      };
    case "bundle_missing":
      return {
        title: "bundle.toml is missing",
        body: "portico-sync/ is there but its key file is not: it may still be downloading. Nothing is written meanwhile.",
        hint: "Wait for the sync client to finish, then Sync now.",
      };
    case "unsafe":
      return {
        title: "The sync folder is not safe to use",
        body: "The folder is a link or a junction, or it sits inside a private folder (~/.ssh, Portico's own data).",
        hint: "Choose a plain folder that your sync client manages.",
      };
    case "too_many_entries":
      return {
        title: "Too many files in the sync folder",
        body: "portico-sync/devices/ holds more files than Portico reads in one pass. Nothing was deleted.",
        hint: "Remove stray files from portico-sync/devices/.",
      };
    default:
      return { title: "Sync folder unavailable", body: "", hint: "" };
  }
}

export function explainWait(reason: WaitReason, device: string | null): string {
  const d = someDevice(device);
  switch (reason) {
    case "partial_sync":
      return `${d}'s file is still uploading or incomplete.`;
    case "placeholder":
      return `${d}'s file is a cloud placeholder that is not downloaded yet.`;
    case "deferred":
      return `${d}'s data refers to changes that are not in the folder yet.`;
    case "busy":
      return "The other Portico app (TUI or GUI) is syncing right now.";
    case "needs_new_keys":
      return `${d} still uses the old keys: its changes are not merged until someone types the new passphrase there.`;
    case "rotation_pending":
      return "The new keys are saved on this device. Portico writes them to the sync folder as soon as it answers; nothing goes back to the old keys.";
    default:
      return "Waiting for the sync folder.";
  }
}

/** The wizard's reading of a probed folder. */
export function probeVerdict(probe: FolderProbe): { tone: "ok" | "join" | "wait" | "error"; title: string; body: string; hint: string | null } {
  switch (probe.kind) {
    case "usable":
      return { tone: "ok", title: "Ready for a new sync", body: "Portico will create portico-sync/ here, with this device as the first one.", hint: null };
    case "existing_bundle":
      return {
        tone: "join",
        title: `A Portico sync with ${probe.devices} ${probe.devices === 1 ? "device" : "devices"}`,
        body: "Join it with its passphrase. Nothing is written before you review what it brings.",
        hint: null,
      };
    case "incomplete":
      return {
        tone: "wait",
        title: "Not downloaded yet",
        body: "portico-sync/ is here without its key file (bundle.toml): your sync client may still be downloading it.",
        hint: "Wait a moment, then Check again.",
      };
    case "missing":
      return { tone: "error", title: "This folder does not exist", body: "", hint: "Choose another folder." };
    case "refused":
      return { tone: "error", title: probe.summary.message, body: "", hint: probe.summary.hint };
    default:
      return { tone: "error", title: "Unknown folder state", body: "", hint: null };
  }
}

/** Meter: a 0–1 fill and a word. `minimum` = the core's 60 bits. */
export const MIN_BITS = 60;
export function strengthView(bits: number, acceptable: boolean): { fill: number; word: string; tone: "weak" | "fair" | "good" } {
  const fill = Math.max(0.04, Math.min(1, bits / (MIN_BITS * 1.6)));
  if (!acceptable) return { fill, word: bits < MIN_BITS / 2 ? "Weak" : "Not enough", tone: "weak" };
  return bits >= MIN_BITS * 1.25 ? { fill, word: "Strong", tone: "good" } : { fill, word: "Acceptable", tone: "fair" };
}

/** `web-01`, `web-01 · tags`, `snippet deploy`, `setting theme`. */
export function subjectLabel(subject: SyncSubject | null): string {
  if (!subject) return "Sync";
  switch (subject.type) {
    case "host":
      return subject.alias;
    case "host_meta":
      return `${subject.alias} · ${subject.field}`;
    case "snippet":
      return `snippet ${subject.id}`;
    case "setting":
      return `setting ${subject.key}`;
    case "other":
      return `${subject.kind} (newer Portico)`;
    default:
      return "Sync";
  }
}

/** The fields a host destination is made of, in display order. */
export const HOST_FIELDS = [
  ["hostname", "HostName"],
  ["user", "User"],
  ["port", "Port"],
  ["identity_file", "IdentityFile"],
  ["proxy_jump", "ProxyJump"],
] as const satisfies readonly (readonly [keyof SyncHostValue, string])[];

export function hostField(value: SyncHostValue | null, field: keyof SyncHostValue): string {
  if (!value) return "—";
  const v = value[field];
  return v === null || v === "" ? "—" : String(v);
}

/** `deploy@web-01.internal:2222 via bastion`. */
export function destination(value: SyncHostValue): string {
  const port = value.port !== null && value.port !== 22 ? `:${value.port}` : "";
  const user = value.user ? `${value.user}@` : "";
  const jump = value.proxy_jump ? ` via ${value.proxy_jump}` : "";
  return `${user}${value.hostname}${port}${jump}`;
}

/** Field rows of a conflict: one value per side, and whether they differ. */
export function conflictRows(sides: ConflictSide[]): { field: string; values: string[]; differs: boolean }[] {
  const kinds = new Set(sides.map((s) => s.value.type));
  const rows: { field: string; values: string[] }[] = [];
  const of = (v: SyncValue, f: (v: SyncValue) => string) => f(v);
  if (kinds.has("host")) {
    for (const [key, label] of HOST_FIELDS) {
      rows.push({ field: label, values: sides.map((s) => of(s.value, (v) => (v.type === "host" ? hostField(v.value, key) : v.type === "deleted" ? "(deleted)" : "—"))) });
    }
  } else if (kinds.has("snippet")) {
    const fields = [
      ["Name", (x: { name: string }) => x.name],
      ["Command", (x: { command: string }) => x.command],
      ["Description", (x: { description: string | null }) => x.description ?? "—"],
      ["Runs on ↵", (x: { run: boolean }) => (x.run ? "yes" : "no, pasted only")],
    ] as const;
    for (const [label, get] of fields) {
      rows.push({
        field: label,
        values: sides.map((s) => (s.value.type === "snippet" ? (get as (x: unknown) => string)(s.value.value) : s.value.type === "deleted" ? "(deleted)" : "—")),
      });
    }
  } else {
    rows.push({ field: "Value", values: sides.map((s) => valueText(s.value)) });
  }
  return rows.map((r) => ({ ...r, differs: new Set(r.values).size > 1 }));
}

export function valueText(value: SyncValue): string {
  switch (value.type) {
    case "text":
      return value.value || "(empty)";
    case "bool":
      return value.value ? "on" : "off";
    case "tags":
      return value.value.length ? value.value.map((t) => `#${t}`).join(" ") : "(no tags)";
    case "host":
      return destination(value.value);
    case "snippet":
      return value.value.name;
    case "deleted":
      return "(deleted)";
    default:
      return "";
  }
}

/** The default choice of a conflict: Union for tags, Keep both for
 * snippets (keeping this device's side under the id), else this device's
 * side (or the first). */
export function defaultChoice(sides: ConflictSide[]): { type: "pick"; index: number } | { type: "union" } | { type: "keep_both"; keep: number } {
  const local = sides.find((s) => s.local)?.index ?? sides[0]?.index ?? 0;
  if (sides.length && sides.every((s) => s.value.type === "tags" || s.value.type === "deleted") && sides.some((s) => s.value.type === "tags")) {
    return { type: "union" };
  }
  if (sides.some((s) => s.value.type === "snippet")) return { type: "keep_both", keep: local };
  return { type: "pick", index: local };
}

/** The join token lives 10 minutes (core: F14). */
export const JOIN_TTL_MS = 10 * 60_000;

/** `Valid for 10 more min` … `Valid for 1 more min`, then `Expired`. */
export function validityLeft(sinceMs: number, nowMs: number): string {
  const left = JOIN_TTL_MS - Math.max(0, nowMs - sinceMs);
  if (left <= 0) return "Expired";
  return `Valid for ${Math.ceil(left / 60_000)} more min`;
}

// ── A received command, shown before it may run (F8, S1c review A1/A2) ──

/** One piece of a command line: plain text, or an invisible character
 * drawn as a visible mark (`→` for a tab, `␍` `␛`… for other controls). The
 * real character stays in `text` (copied as is); `mark` is only drawn. */
export interface CommandPiece {
  text: string;
  mark: string | null;
}

const controlMark = (c: string): string => {
  if (c === "\t") return "→";
  const code = c.codePointAt(0) ?? 0;
  return code === 0x7f ? "␡" : String.fromCodePoint(0x2400 + code);
};

/** The command cut in lines (a trailing newline makes a last, empty line:
 * it is an extra Enter, so it is shown and numbered), each cut at its tabs
 * and other control characters. */
export function commandLines(command: string): CommandPiece[][] {
  return command.split("\n").map((line) => {
    const pieces: CommandPiece[] = [];
    for (const part of line.split(/([\u0000-\u0009\u000b-\u001f\u007f])/)) {
      if (!part) continue;
      pieces.push(part.length === 1 && /[\u0000-\u0009\u000b-\u001f\u007f]/.test(part) ? { text: part, mark: controlMark(part) } : { text: part, mark: null });
    }
    return pieces;
  });
}

/** `1 line · 12 characters`, `200 lines · 8,400 characters`. */
export function commandStats(command: string): string {
  const lines = commandLines(command).length;
  const chars = [...command].length;
  return `${lines.toLocaleString("en-US")} ${lines === 1 ? "line" : "lines"} · ${chars.toLocaleString("en-US")} ${chars === 1 ? "character" : "characters"}`;
}

/** The end of a scrollable command is on screen (or it does not scroll). */
export function scrolledToEnd(scrollTop: number, clientHeight: number, scrollHeight: number): boolean {
  return scrollTop + clientHeight >= scrollHeight - 1;
}

// ── Key rotation and forgetting a device (S2c-1) ─────────────────────────

/** `a, b and c` (labels as given). */
export function listText(items: string[]): string {
  if (items.length <= 1) return items[0] ?? "";
  return `${items.slice(0, -1).join(", ")} and ${items[items.length - 1]}`;
}

/** `Old keys (rotation 1) · this sync · not in use`. */
export function keyEntryLabel(entry: { bundle: string; epoch: number; this_bundle: boolean }): string {
  const sync = entry.this_bundle ? "this sync" : `another sync (${entry.bundle.slice(0, 8)})`;
  return `Old keys (rotation ${entry.epoch}) · ${sync} · not in use`;
}

/** The status says a rotation, adoption or conflict needs this device. */
export function keysNeedYou(status: SyncStatus | null): "adopt" | "conflict" | "forgotten" | null {
  if (status?.state !== "alert") return null;
  if (status.kind === "keys_rotated_elsewhere") return "adopt";
  if (status.kind === "rotation_conflict") return "conflict";
  if (status.kind === "device_forgotten") return "forgotten";
  return null;
}

/** The word typed for the reset-level confirmation (Q6), as for "Reset passphrase". */
export const RESET_WORD = "reset";

// ── Shared security texts: word for word as in the TUI (S2c-1 review B8) ──

/** Status-bar label and result title of a rotation still on its way (one wording everywhere). */
export const ROTATION_PENDING_LABEL = "Finishing the key rotation…";

/** The adoption's title: `studio-mac (3f9c0ae1) reports new keys`. */
export function newKeysTitle(device: string | null): string {
  return `${device ?? "Another device"} reports new keys`;
}

/** The adoption's line (§13.3.2, R1). */
export const TYPE_NOTHING = "If you did not change the keys on another device, type nothing.";

/** What to do when nobody here changed the keys (B4). */
export const ADOPT_UNSURE =
  "If you did not change the keys on another device, type nothing and ask who did: someone with the old passphrase or a lost device may have. Close this, then forget that device from a device you trust.";

/** The same, under the R1 line (the adoption dialog): R1 already says its first sentence. */
export const ADOPT_UNSURE_AFTER_R1 = "Ask who did: someone with the old passphrase or a lost device may have. Close this, then forget that device from a device you trust.";

/** Only when the core lists files that claim newer keys (RR5). */
export const NEWER_FILE_HINT = "If nobody changed the keys, remove the file that claims newer keys (below).";

/** Rotate keys, step 1. */
export const ROTATE_ABOUT =
  "Rotating makes new keys under a new passphrase. Use it when a passphrase or a device may have leaked: a device without the new passphrase cannot read anything synced from now on. What was synced before stays readable to whoever had the old keys.";

/** Forget a device, step 1. */
export function forgetAbout(label: string): string {
  return `Forgetting ${label} rotates the keys: it cannot read anything synced from now on. What it already synced stays readable to it.`;
}

/** The cloud folder reminder of a forget (RR7). */
export function cloudTitle(label: string): string {
  return `Also remove ${label}'s access to your cloud folder`;
}
export const CLOUD_TEXT =
  "Its Dropbox, iCloud or OneDrive session, or its Syncthing share: only that closes the remaining window. Without it, the lost device can still put old files back.";

/** The forget result: `Forgotten: X. It cannot read what is synced from now on.` */
export function forgottenResult(labels: string[]): string {
  return `Forgotten: ${listText(labels)}. ${labels.length === 1 ? "It cannot" : "They cannot"} read what is synced from now on.`;
}

/** The forget result's RR7 reminder. */
export function cloudReminder(labels: string[]): string {
  return `${cloudTitle(listText(labels))}, if not done yet: only that closes the remaining window.`;
}

/** R17: "Keep this device's keys" in a rotation conflict. */
export const KEEP_MINE_TEXT =
  "This device rotates the keys again from its own. The other rotation's removals are lost: if the other device removed a device, remove it again here. The other devices then see a conflict and choose in turn.";

/** One-line costs of the two conflict choices (B2). */
export function useCost(name: string): string {
  return `Type the passphrase chosen on ${name}. This device switches to its keys.`;
}
export function keepCost(name: string): string {
  return `Devices ${name} removed stay in the list: remove them again here. Portico cannot see which: check on ${name}.`;
}

/** R18: "Leave the device list" without a rotation. */
export const LEAVE_NOTE =
  "Leaves the list without changing the keys: unless its key is forgotten too, this device can still read the current sync until the next rotation. To really exclude a device, forget it from another device.";

/** Removing an old key from this keychain. */
export const OLD_KEY_CONSEQUENCE = "Files sealed only with it can no longer be read here.";

/** A held value in full (A3): a snippet with its command, what Keep would share. */
export function heldText(value: SyncValue): string {
  return value.type === "snippet" ? `${value.value.name} — ${value.value.command}` : valueText(value);
}

/** Values held from removed devices (B7), adapted to the buttons. */
export const HELD_TITLE = "From removed devices";
export const HELD_NOTE = "Held until you decide, one by one.";
export const HELD_EXPLAIN =
  "Keep as mine (m) keeps it as this device's value and shares it. Take the synced value (t) takes the value the other devices hold, or removes it here if they hold none.";

/** `sync_passphrase_reused`, said for the dialog it happened in (B3): by code, never by message. */
export function reusedHint(dialog: "rotation" | "adoption"): string {
  return dialog === "rotation"
    ? "Portico accepted this passphrase before. Choose a new one, or generate one."
    : "This is an old passphrase, not a new one. If nobody gave you a new passphrase, nobody changed the keys: type nothing, close, and check your other devices.";
}
export const REUSED = "sync_passphrase_reused";

/** `workstation (a07d51c2) — update Portico first` for a device on Portico < 0.7 (B9). */
export function deviceToUpdate(label: string, outdated: readonly string[]): string {
  return outdated.includes(label) ? `${label} — update Portico first` : label;
}

// ── Arming (S2c-1 review A1–A3) ──

/** A final `y` acts only this long after the dialog became ready (a Space
 * on "I saved it" then `y` at once is ignored), as in PromptDialog. */
export const Y_DELAY = 150;
export function yArmed(readyAt: number | null, nowMs: number): boolean {
  return readyAt !== null && nowMs - readyAt >= Y_DELAY;
}

/** Held values: decisions wait this long after the list changed (the TUI's ARM_DELAY). */
export const HELD_ARM_MS = 400;
export function heldArmed(changedAt: number, nowMs: number): boolean {
  return nowMs - changedAt >= HELD_ARM_MS;
}

/** The Sync panel's `y` answers only the question on screen (A1). */
export function panelConfirm(tab: string, discarding: boolean, removingKey: boolean): "discard" | "remove" | null {
  if (tab === "overview" && discarding) return "discard";
  if (tab === "devices" && removingKey) return "remove";
  return null;
}

/** After `sync_folder_timeout` on a rotation (A4): what the status and the
 * key epoch in use, read again, say. `pending` / `done`: it happened (show the result); `failed`:
 * it did not (try again); `unknown`: still checking (no new passphrase). */
export function rotationAfterTimeout(status: SyncStatus | null, epochBefore: number | null, epochAfter: number | null): "pending" | "done" | "failed" | "unknown" {
  if (!status || status.state === "folder_unavailable" || status.state === "syncing") return "unknown";
  if (status.state === "waiting" && status.reason === "rotation_pending") return "pending";
  // The epoch in use (SyncOverview.epoch) moved: the keys changed.
  if (epochBefore === null || epochAfter === null) return "unknown";
  return epochAfter > epochBefore ? "done" : "failed";
}

