<script lang="ts">
  // Vault: unlock (startup, keys view, after "remember"), create, change the
  // passphrase. Passphrases go to Rust as SecretString; fields are cleared
  // as soon as they are sent.
  import type { ErrorSummary } from "../api";
  import { vault, type VaultMode, type VaultReason } from "../state/vault.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";

  let { mode, reason }: { mode: VaultMode; reason: VaultReason } = $props();

  /** Minimum length of a new master passphrase (portico-core). */
  const MIN = 8;

  let armed = $state(false);
  let previous = $state("");
  let passphrase = $state("");
  let confirm = $state("");
  let busy = $state(false);
  let wrong = $state(0);
  let error: ErrorSummary | null = $state(null);

  const creating = $derived(mode !== "unlock");
  const tooShort = $derived(creating && passphrase.length > 0 && [...passphrase].length < MIN);
  const mismatch = $derived(creating && confirm.length > 0 && passphrase !== confirm);
  const ready = $derived(
    armed && !busy && passphrase.length > 0 && (!creating || (!tooShort && passphrase === confirm)) && (mode !== "change" || previous.length > 0),
  );

  async function submit() {
    if (!ready) return;
    busy = true;
    error = null;
    const [secret, old] = [passphrase, previous];
    passphrase = "";
    confirm = "";
    previous = "";
    const failed = await vault.submit(mode, secret, old);
    busy = false;
    if (!failed) {
      vault.dialog = null;
      const message = mode === "unlock" ? "Vault unlocked" : mode === "create" ? "Vault created and unlocked" : "Vault passphrase changed";
      workspace.notify({ code: "vault", message, hint: null }, "info");
      return;
    }
    if (failed.code === "vault_wrong_passphrase") wrong++;
    else error = failed;
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && !(e.target instanceof HTMLButtonElement)) {
      e.preventDefault();
      void submit();
    }
  }

  const heading = $derived(mode === "unlock" ? "Unlock the vault" : mode === "create" ? "Create a vault" : "Change the vault passphrase");
</script>

<Modal bind:armed onescape={() => vault.close()} labelledBy="vault-title">
  {#snippet title()}{heading}{/snippet}

  <div class="form" role="presentation" {onkeydown}>
    {#if reason === "remember"}
      <p class="lead">
        {mode === "create" ? "There is no vault yet, so this secret was not remembered." : "The vault is locked, so this secret was not remembered."}
      </p>
    {/if}
    {#if mode === "unlock"}
      <p>Portico keeps remembered passwords and passphrases in an encrypted vault on this machine. Enter its master passphrase.</p>
    {:else if mode === "create"}
      <p>
        No system keyring is available here: remembered passwords and passphrases go to an encrypted vault on this machine.
        Its master passphrase cannot be recovered.
      </p>
    {/if}

    {#if mode === "change"}
      <label class="field">
        <span>Current passphrase</span>
        <input data-autofocus type="password" bind:value={previous} autocomplete="current-password" spellcheck="false" aria-invalid={wrong > 0} />
      </label>
    {/if}
    <label class="field">
      <span>{mode === "unlock" ? "Master passphrase" : mode === "change" ? "New passphrase" : "Passphrase"}</span>
      <input
        data-autofocus={mode !== "change" ? "" : undefined}
        type="password"
        bind:value={passphrase}
        autocomplete={creating ? "new-password" : "current-password"}
        spellcheck="false"
        aria-invalid={(mode === "unlock" && wrong > 0) || tooShort}
      />
      {#if creating}<span class="help">At least {MIN} characters; a few words are easy to remember.</span>{/if}
    </label>
    {#if creating}
      <label class="field">
        <span>Confirm</span>
        <input type="password" bind:value={confirm} autocomplete="new-password" spellcheck="false" aria-invalid={mismatch} />
      </label>
    {/if}

    {#if wrong > 0}
      <span class="inline-error" role="alert">Wrong passphrase, try again (attempt {wrong + 1}). Check Caps Lock and the keyboard layout.</span>
    {:else if mismatch}
      <span class="inline-error" role="alert">The passphrases differ.</span>
    {:else if tooShort}
      <span class="soft-warning">Too short: at least {MIN} characters.</span>
    {/if}
    {#if error}
      <div class="inline-error" role="alert"><span>{error.message}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}</div>
    {/if}
    {#if reason === "startup"}
      <p class="note">Continue without it and Portico neither uses nor saves remembered secrets until you unlock it (Keys, <kbd>v</kbd>).</p>
    {/if}
  </div>

  {#snippet footer()}
    {#if busy}<span class="footer-note busy"><span class="spinner"></span>Deriving the key…</span>{/if}
    <button class="secondary" disabled={busy} onclick={() => vault.close()}>
      {reason === "startup" ? "Continue without" : reason === "remember" ? "Not now" : "Cancel"} <kbd>Esc</kbd>
    </button>
    <button class="primary" disabled={!ready} onclick={submit}>
      {mode === "unlock" ? "Unlock" : mode === "create" ? "Create vault" : "Change"} <kbd>↵</kbd>
    </button>
  {/snippet}
</Modal>

<style>
  .form { display: grid; gap: var(--space-3); }
  .lead { color: var(--text-primary); }
  .busy { display: inline-flex; align-items: center; gap: var(--space-2); }
</style>
