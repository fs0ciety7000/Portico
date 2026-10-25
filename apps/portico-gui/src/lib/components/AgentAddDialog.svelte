<script lang="ts">
  // ssh-add: lifetime, and the key's passphrase when the engine says it is
  // encrypted (`key_needs_passphrase`) or wrong (`key_wrong_passphrase`).
  // The passphrase field is cleared as soon as it is sent.
  import { asError, sentence, type ErrorSummary, type LocalKey } from "../api";
  import { backend } from "../backend";
  import { keys } from "../state/keys.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";
  import { isWindowsWebview } from "../webview";

  let { key }: { key: LocalKey } = $props();

  const LIFETIMES: { label: string; secs: number | null }[] = [
    { label: "Until removed", secs: null },
    { label: "1 hour", secs: 3600 },
    { label: "8 hours", secs: 8 * 3600 },
  ];

  // The Windows agent stores keys (registry, DPAPI) until they are
  // removed, across reboots, and may refuse a lifetime.
  const windowsAgent = isWindowsWebview(navigator.userAgent);

  let armed = $state(false);
  let lifetime = $state(0);
  let needsPassphrase = $state(false);
  let attempt = $state(0);
  let passphrase = $state("");
  let busy = $state(false);
  let error: ErrorSummary | null = $state(null);
  let field: HTMLInputElement | undefined = $state();

  async function add() {
    if (!armed || busy) return;
    busy = true;
    error = null;
    const secret = needsPassphrase ? passphrase : null;
    passphrase = "";
    try {
      await backend.agentAdd(key.id, secret, LIFETIMES[lifetime]?.secs ?? null);
      await keys.refresh();
      keys.view = { kind: "list" };
      workspace.notify({ code: "agent_added", message: `Added ${key.id} to ssh-agent`, hint: null }, "info");
    } catch (e) {
      const err = asError(e);
      if (err.code === "key_needs_passphrase") {
        needsPassphrase = true;
      } else if (err.code === "key_wrong_passphrase") {
        needsPassphrase = true;
        attempt++;
      } else {
        error = err;
      }
      queueMicrotask(() => field?.focus());
    } finally {
      busy = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      // Also from the segmented choices: Enter submits, Space picks.
      e.preventDefault();
      void add();
    }
  }
</script>

<Modal bind:armed onescape={() => (keys.view = { kind: "list" })} labelledBy="agent-add-title">
  {#snippet title()}Add to ssh-agent<span class="title-host">· {key.id}</span>{/snippet}

  <div class="form" role="presentation" {onkeydown}>
    <dl>
      <dt>Key</dt><dd>{key.privateDisplay}</dd>
      <dt>Fingerprint</dt><dd class="fingerprint">{key.fingerprint}</dd>
    </dl>
    <div class="field">
      <span>Keep it in the agent</span>
      <div class="segmented" role="group" aria-label="Lifetime">
        {#each LIFETIMES as l, i (l.label)}
          <button type="button" aria-pressed={lifetime === i} data-autofocus={i === 0 && !needsPassphrase ? "" : undefined} onclick={() => (lifetime = i)}>{l.label}</button>
        {/each}
      </div>
      {#if windowsAgent}
        <span class="note">The Windows agent keeps keys, across restarts, until they are removed, and may refuse a lifetime.</span>
      {/if}
    </div>
    {#if needsPassphrase}
      <label class="field">
        <span>Passphrase of {key.id}</span>
        <input bind:this={field} type="password" bind:value={passphrase} autocomplete="off" spellcheck="false" aria-invalid={attempt > 0} />
      </label>
      {#if attempt > 0}
        <span class="inline-error" role="alert">Wrong passphrase, try again ({attempt + 1}).</span>
      {:else}
        <span class="note">This key is encrypted. Its passphrase is used once, to hand the key to the agent.</span>
      {/if}
    {/if}
    {#if error}
      <div class="inline-error" role="alert"><span>{sentence(error.message)}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}</div>
    {/if}
  </div>

  {#snippet footer()}
    <button class="secondary" onclick={() => (keys.view = { kind: "list" })}>Cancel <kbd>Esc</kbd></button>
    <button class="primary" disabled={!armed || busy || (needsPassphrase && !passphrase)} onclick={add}>
      {#if busy}<span class="spinner"></span>{/if}Add <kbd>↵</kbd>
    </button>
  {/snippet}
</Modal>

<style>
  .form { display: grid; gap: var(--space-3); }
</style>
