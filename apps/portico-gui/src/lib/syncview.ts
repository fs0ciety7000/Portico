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
      return { label: status.device ? `Waiting for ${status.device}` : "Waiting for the sync folder", tone: "busy" };
    case "folder_unavailable":
      return { label: "Sync folder unavailable", tone: "warning" };
    case "alert":
      return { label: "Sync alert", tone: "error" };
    case "error":
      return { label: "Sync failed", tone: "error" };
    default:
      return { label: "Sync", tone: "muted" };
  }
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
        title: "The sync keys were replaced on another device",
        body: `${d} uses newer sync keys than this device knows.`,
        hint: "Unlock with the current passphrase, or update Portico if this version is older.",
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
