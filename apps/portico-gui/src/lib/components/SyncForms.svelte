<script lang="ts">
  // Sync forms (phase 11): unlock (Locked), change the passphrase (F2: it
  // does not protect against a passphrase already leaked), reset it with the
  // remembered key (extra confirmation, blocking alert elsewhere), rename
  // this device, turn sync off (forget the key by default; local data
  // stays). Passphrase fields are cleared as soon as they are sent.
  import { untrack } from "svelte";
  import { sentence, type ErrorSummary, type GeneratedPassphrase } from "../api";
  import { backend } from "../backend";
  import { sync, type SyncForm } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";
  import SyncPassphraseFields, { newPassphrase } from "./SyncPassphraseFields.svelte";

  let { form: initial }: { form: SyncForm } = $props();
  const form = untrack(() => initial);

  let armed = $state(false);
  let busy: string | null = $state(null);
  let error: ErrorSummary | null = $state(null);
  let wrong = $state(0);
  let old = $state("");
  let passphrase = $state("");
  let confirm = $state("");
  let generated: GeneratedPassphrase | null = $state(null);
  let wroteDown = $state(false);
  let passReady = $state(false);
  let resetTyped = $state("");
  let name = $state(untrack(() => sync.overview?.device?.replace(/\s*\([0-9a-f]{8}\)$/, "") ?? ""));
  let forgetKey = $state(true);
  let removeMyFile = $state(false);

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
      default: {
        const options = [forgetKey, removeMyFile] as const;
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
    workspace.notify({ code: "sync", message: done, hint: form === "disable" ? "Your hosts, snippets and settings stay as they are." : null }, "info");
    if (form === "disable") sync.close();
    else sync.open({ kind: "panel", tab: "overview" });
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !(e.target instanceof HTMLButtonElement)) {
      e.preventDefault();
      void submit();
    }
  }

  const titles: Record<SyncForm, string> = {
    unlock: "Unlock sync",
    change: "Change the sync passphrase",
    reset: "Reset the sync passphrase",
    rename: "Rename this device",
    disable: "Turn sync off on this device",
  };
  const actions: Record<SyncForm, string> = { unlock: "Unlock", change: "Change", reset: "Reset passphrase", rename: "Rename", disable: "Turn off" };
</script>

<Modal bind:armed onescape={() => (clear(), sync.back())} labelledBy="sync-form-title" tone={form === "reset" ? "danger" : "default"}>
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
    {:else}
      <p>Portico stops syncing on this device. <strong>Your hosts, snippets and settings stay here, unchanged.</strong> Other devices keep syncing.</p>
      <label class="check"><input type="checkbox" bind:checked={forgetKey} data-autofocus /> Forget the sync key on this device</label>
      <label class="check"><input type="checkbox" bind:checked={removeMyFile} /> Remove this device's file from the folder</label>
      <p class="note">Received snippets you have not confirmed stay blocked until you review them.</p>
    {/if}

    {#if wrong > 0}
      <span class="inline-error" role="alert">Wrong passphrase, try again (attempt {wrong + 1}). Check Caps Lock and the keyboard layout.</span>
    {/if}
    {#if error}
      <div class="inline-error" role="alert"><span>{sentence(error.message)}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}</div>
    {/if}
  </div>

  {#snippet footer()}
    {#if busy}<span class="footer-note busy"><span class="spinner"></span>{busy}</span>{/if}
    <button class="secondary" disabled={!!busy} onclick={() => (clear(), sync.back())}>Cancel <kbd>Esc</kbd></button>
    <button class="primary" disabled={!ready} onclick={submit}>{actions[form]} <kbd>↵</kbd></button>
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
