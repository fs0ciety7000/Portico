<script lang="ts">
  // Sync forms (phase 11): unlock (Locked), change the passphrase (F2: it
  // does not protect against a passphrase already leaked), reset it with the
  // remembered key (extra confirmation, blocking alert elsewhere), rename
  // this device, turn sync off (forget the key by default; local data
  // stays). Passphrase fields are cleared as soon as they are sent; a
  // generated passphrase is dropped in Rust when the form closes. Reset is
  // destructive: `y` (or its button), never Enter.
  // S2c-1: "Turn sync off" offers "Leave the device list" (R18: without new
  // keys, the device could still read the current sync), on by default with
  // "Remove this device's file" (§13.4.5); "Join again as a new device" (a
  // removed device) turns sync off keeping the folder picked, then opens the
  // wizard on it; "Restore key file" rewrites bundle.toml from this
  // device's verified copy (danger, `y` only).
  import { onDestroy, onMount, untrack } from "svelte";
  import { type ErrorSummary, type GeneratedPassphrase, type RotationPreview } from "../api";
  import { backend } from "../backend";
  import { sync, type SyncForm } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import CodeText from "./CodeText.svelte";
  import Caution from "./Caution.svelte";
  import Modal from "./Modal.svelte";
  import { listText } from "../syncview";
  import SyncPassphraseFields, { newPassphrase } from "./SyncPassphraseFields.svelte";

  let { form: initial }: { form: SyncForm } = $props();
  const form = untrack(() => initial);

  let armed = $state(false);
  let busy = $state<string | null>(null);
  let error = $state<ErrorSummary | null>(null);
  let wrong = $state(0);
  let old = $state("");
  let passphrase = $state("");
  let confirm = $state("");
  let generated = $state<GeneratedPassphrase | null>(null);
  let wroteDown = $state(false);
  let passReady = $state(false);
  let resetTyped = $state("");
  let name = $state(untrack(() => sync.overview?.device?.replace(/\s*\([0-9a-f]{8}\)$/, "") ?? ""));
  let forgetKey = $state(true);
  let removeMyFile = $state(false);
  let leaveRoster = $state(false);
  let preview = $state<RotationPreview | null>(null);
  const outdated = $derived(preview?.outdated ?? []);

  // The Q5 warning of "Leave the device list" names the devices on Portico < 0.7.
  onMount(() => {
    if (form === "disable") backend.syncRotationPreview().then((p) => (preview = p), () => {});
  });

  const RESET_WORD = "reset";
  const ready = $derived.by(() => {
    if (busy || !armed) return false;
    switch (form) {
      case "unlock":
        return passphrase.length > 0;
      case "change":
        return old.length > 0 && passReady;
      case "reset":
        return passReady && resetTyped.trim().toLowerCase() === RESET_WORD;
      case "rename":
        return name.trim().length > 0;
      default:
        return true;
    }
  });

  function clear() {
    old = "";
    passphrase = "";
    confirm = "";
  }

  async function submit() {
    if (!ready) return;
    error = null;
    let action: () => Promise<unknown>;
    let done: string;
    switch (form) {
      case "unlock": {
        const secret = passphrase;
        action = () => backend.syncUnlock(secret);
        done = "Sync unlocked";
        busy = "Deriving the key…";
        break;
      }
      case "change": {
        const [previous, next] = [old, newPassphrase(generated, passphrase)];
        action = () => backend.syncChangePassphrase(previous, next);
        done = "Sync passphrase changed";
        busy = "Deriving the keys…";
        break;
      }
      case "reset": {
        const next = newPassphrase(generated, passphrase);
        action = () => backend.syncResetPassphrase(next);
        done = "Sync passphrase reset";
        busy = "Deriving the key…";
        break;
      }
      case "rename": {
        const value = name.trim();
        action = () => backend.syncRenameDevice(value);
        done = "Device renamed";
        busy = "Saving…";
        break;
      }
      case "rejoin": {
        action = () => backend.syncDisable(true, false, false, true);
        done = "Sync is off on this device";
        busy = "Turning sync off…";
        break;
      }
      case "restore": {
        action = () => backend.syncRestoreBundle();
        done = "Key file restored";
        busy = "Restoring the key file…";
        break;
      }
      default: {
        const options = [forgetKey, removeMyFile, leaveRoster, false] as const;
        action = () => backend.syncDisable(...options);
        done = "Sync is off on this device";
        busy = "Turning sync off…";
      }
    }
    clear();
    const failed = await sync.act(action as () => Promise<void>);
    busy = null;
    if (generated) {
      // A generated passphrase serves once.
      generated = null;
      wroteDown = false;
    }
    if (failed) {
      if (failed.code === "sync_wrong_passphrase") wrong++;
      else error = failed;
      return;
    }
    workspace.notify({ code: "sync", message: done, hint: form === "disable" || form === "rejoin" ? "Your hosts, snippets and settings stay as they are." : null }, "info");
    if (form === "disable") sync.close();
    else if (form === "rejoin") await openWizardOnFolder();
    else sync.open({ kind: "panel", tab: "overview" });
  }

  /** "Join again": the wizard on the same folder (kept picked in Rust). */
  async function openWizardOnFolder() {
    const display = folderShown;
    try {
      const probe = await backend.syncProbeAgain();
      sync.open({ kind: "wizard", rejoin: { display: display ?? "Sync folder", probe } });
    } catch {
      sync.open({ kind: "wizard" });
    }
  }
  const folderShown = untrack(() => sync.overview?.folder ?? null);

  onDestroy(() => void backend.syncForgetGenerated().catch(() => {}));

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && form !== "reset" && form !== "restore" && !(e.target instanceof HTMLButtonElement)) {
      e.preventDefault();
      void submit();
    }
  }

  function onkey(key: string): boolean {
    if ((form !== "reset" && form !== "restore") || key !== "y") return false;
    void submit();
    return true;
  }

  const titles: Record<SyncForm, string> = {
    unlock: "Unlock sync",
    change: "Change the sync passphrase",
    reset: "Reset the sync passphrase",
    rename: "Rename this device",
    disable: "Turn sync off on this device",
    rejoin: "Join again as a new device",
    restore: "Restore the key file",
  };
  const actions: Record<SyncForm, string> = {
    unlock: "Unlock",
    change: "Change",
    reset: "Reset passphrase",
    rename: "Rename",
    disable: "Turn off",
    rejoin: "Turn off and join again",
    restore: "Restore key file",
  };
  const danger = (f: SyncForm) => f === "reset" || f === "restore";
</script>

<Modal bind:armed onescape={() => (clear(), sync.back())} {onkey} labelledBy="sync-form-title" tone={danger(form) ? "danger" : "default"}>
  {#snippet title()}{titles[form]}{/snippet}

  <div class="form" role="presentation" {onkeydown}>
    {#if form === "unlock"}
      <p>This device does not remember the sync key. Enter the sync passphrase to read the folder again.</p>
      <label class="field">
        <span>Sync passphrase</span>
        <input data-autofocus type="password" bind:value={passphrase} autocomplete="current-password" spellcheck="false" aria-invalid={wrong > 0} />
      </label>
    {:else if form === "change"}
      <p>Every device keeps working; the others need the new passphrase only to join again or unlock.</p>
      <p class="caution">
        Changing it does not protect the folder against someone who already knows the old passphrase and copied the folder.
        If it leaked, turn sync off everywhere and create a new sync folder.
      </p>
      <label class="field">
        <span>Current passphrase</span>
        <input data-autofocus type="password" bind:value={old} autocomplete="current-password" spellcheck="false" aria-invalid={wrong > 0} />
      </label>
      <SyncPassphraseFields bind:value={passphrase} bind:confirm bind:generated bind:wroteDown bind:ready={passReady} label="New passphrase" />
    {:else if form === "reset"}
      <p>
        For a lost passphrase: this device remembers the sync key, so it can set a new passphrase without the old one.
        Every other device shows a blocking alert until someone acknowledges it there.
      </p>
      <SyncPassphraseFields bind:value={passphrase} bind:confirm bind:generated bind:wroteDown bind:ready={passReady} label="New passphrase" autofocus />
      <label class="field">
        <span>Type <span class="mono">{RESET_WORD}</span> to confirm</span>
        <input bind:value={resetTyped} spellcheck="false" autocomplete="off" />
      </label>
    {:else if form === "rename"}
      <p>The other devices show the new name next to this device's short id.</p>
      <label class="field">
        <span>Device name</span>
        <input data-autofocus bind:value={name} maxlength="64" spellcheck="false" />
      </label>
    {:else if form === "rejoin"}
      <p>
        This device was removed from the sync. Portico turns sync off here and forgets its keys, then opens the wizard on the same
        folder: join it again as a new device, with the passphrase the other devices use now.
      </p>
      <p><strong>Your hosts, snippets and settings stay here, unchanged.</strong> When you join, each difference waits for your review.</p>
      <p class="note">Folder: <span class="mono">{folderShown ?? "—"}</span></p>
    {:else if form === "restore"}
      <p>
        Portico rewrites the sync folder's key file (bundle.toml) from the copy this device verified. Use it when the key file was
        damaged or replaced and nobody changed the keys.
      </p>
      <p class="note">If another device really changed the keys, Portico refuses and says so: enter the new passphrase instead.</p>
    {:else}
      <p>Portico stops syncing on this device. <strong>Your hosts, snippets and settings stay here, unchanged.</strong> Other devices keep syncing.</p>
      <label class="check"><input type="checkbox" bind:checked={forgetKey} data-autofocus /> Forget the sync key on this device</label>
      <label class="check"><input type="checkbox" bind:checked={removeMyFile} onchange={() => (leaveRoster = removeMyFile)} /> Remove this device's file from the folder</label>
      <label class="check"><input type="checkbox" bind:checked={leaveRoster} data-testid="sync-leave-roster" /> Leave the device list</label>
      {#if leaveRoster}
        <p class="note" data-testid="sync-leave-note">
          Removed from the list without changing the keys{forgetKey ? "" : ": while it keeps the key, this device can still read the current sync"}.
          To exclude it for real, forget it from another device.
        </p>
        {#if outdated.length}
          <Caution>{listText(outdated)} {outdated.length === 1 ? "runs" : "run"} Portico older than 0.7 and will stop syncing after this: update Portico there.</Caution>
        {/if}
      {/if}
      <p class="note">Received snippets you have not confirmed stay blocked until you review them.</p>
    {/if}

    {#if wrong > 0}
      <span class="inline-error" role="alert">Wrong passphrase, try again (attempt {wrong + 1}). Check Caps Lock and the keyboard layout.</span>
    {/if}
    {#if error}
      <div class="inline-error" role="alert"><span><CodeText text={error.message} /></span>{#if error.hint}<span class="hint"><CodeText text={error.hint} capitalize={false} /></span>{/if}</div>
    {/if}
  </div>

  {#snippet footer()}
    {#if busy}<span class="footer-note busy"><span class="spinner"></span>{busy}</span>{/if}
    <button class="secondary" disabled={!!busy} onclick={() => (clear(), sync.back())}>Cancel <kbd>Esc</kbd></button>
    {#if danger(form)}
      <button class="danger" disabled={!ready} onclick={submit}>{actions[form]} <kbd>y</kbd></button>
    {:else}
      <button class="primary" disabled={!ready} onclick={submit}>{actions[form]} <kbd>↵</kbd></button>
    {/if}
  {/snippet}
</Modal>

<style>
  .form { display: grid; gap: var(--space-3); }
  .caution { padding-left: var(--space-3); border-left: 2px solid var(--status-warning); color: var(--text-secondary); font-size: var(--text-sm); }
  .mono { font-family: var(--font-mono); }
  strong { color: var(--text-primary); font-weight: 550; }
  .busy { display: inline-flex; align-items: center; gap: var(--space-2); }
  .hint { color: var(--text-secondary); }
</style>
