<script lang="ts">
  // A rotation conflict (phase 11, S2c-1, docs/SYNC.md §13.3.7): two devices
  // changed the keys at the same time. Two explicit choices, nothing
  // preselected:
  //   - "Use <device>'s keys": its passphrase; refused (E2) while those keys
  //     still include a device this one removed, the device named;
  //   - "Keep this device's keys": a new rotation from this device's keys
  //     (R17: the other rotation's removals are lost, the other devices see
  //     a conflict too): new passphrase, "I saved it", danger, `y` only.
  // Offered only while the conflict is authentic: `sync_not_pending` closes
  // the dialog and reloads. Passphrases are cleared once sent.
  import { onDestroy, tick, untrack } from "svelte";
  import { fly } from "svelte/transition";
  import { asError, type ErrorSummary, type GeneratedPassphrase } from "../api";
  import { backend } from "../backend";
  import { dur } from "../motion";
  import { deviceName } from "../syncview";
  import { NOT_PENDING, sync, TIMEOUT } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Caution from "./Caution.svelte";
  import CodeText from "./CodeText.svelte";
  import Modal from "./Modal.svelte";
  import SyncPassphraseFields, { newPassphrase } from "./SyncPassphraseFields.svelte";

  type Choice = "use" | "keep" | null;
  let choice = $state<Choice>(null);
  let armed = $state(false);
  let busy = $state<string | null>(null);
  let error = $state<ErrorSummary | null>(null);
  let wrong = $state(0);
  let theirs = $state("");
  let passphrase = $state("");
  let confirm = $state("");
  let generated = $state<GeneratedPassphrase | null>(null);
  let wroteDown = $state(false);
  let passReady = $state(false);
  let saved = $state(false);
  let rememberKey = $state(untrack(() => sync.overview?.rememberKey ?? true));
  let root = $state<HTMLDivElement | undefined>();

  const status = $derived(sync.status);
  const other = $derived(status?.state === "alert" ? status.device : null);
  const otherName = $derived(other ? deviceName(other) : "the other device");
  const refused = $derived(error?.code === "sync_passphrase_weak");
  const ready = $derived(
    armed && !busy && (choice === "use" ? theirs.length > 0 : choice === "keep" ? passReady && saved : false),
  );

  function clearSecrets() {
    theirs = "";
    passphrase = "";
    confirm = "";
  }

  onDestroy(() => {
    clearSecrets();
    void backend.syncForgetGenerated().catch(() => {});
  });

  // Settled elsewhere (the other Portico app, or a pass): nothing to choose any more.
  $effect(() => {
    const s = status;
    if (busy || !s) return;
    if (!(s.state === "alert" && s.kind === "rotation_conflict")) {
      untrack(() => {
        clearSecrets();
        sync.open({ kind: "panel", tab: "overview" });
      });
    }
  });

  $effect(() => {
    void choice;
    if (!untrack(() => armed)) return;
    void tick().then(() => root?.querySelector<HTMLElement>(".choice-body [data-autofocus]")?.focus());
  });

  function pick(next: Choice) {
    if (busy) return;
    clearSecrets();
    error = null;
    wrong = 0;
    choice = next;
  }

  function close() {
    clearSecrets();
    sync.back();
  }

  async function submit() {
    if (!ready || !choice) return;
    const request =
      choice === "use"
        ? ({ kind: "use_other", passphrase: theirs } as const)
        : ({ kind: "keep_mine", newPassphrase: newPassphrase(generated, passphrase), rememberKey } as const);
    const kept = choice === "keep";
    clearSecrets();
    error = null;
    busy = kept ? "Deriving the new keys… (a few seconds)" : "Checking their keys… (a few seconds)";
    try {
      const result = await backend.syncResolveRotationConflict(request);
      busy = null;
      await sync.act(async () => result);
      workspace.notify(
        kept
          ? { code: "sync", message: "This device's keys are kept", hint: "Enter the new passphrase on the other devices." }
          : { code: "sync", message: `Using ${otherName}'s keys`, hint: null },
        "info",
      );
      sync.open({ kind: "panel", tab: "overview" });
    } catch (e) {
      busy = null;
      const failed = asError(e);
      if (generated) {
        generated = null;
        wroteDown = false;
      }
      saved = false;
      if (failed.code === "sync_wrong_passphrase") {
        wrong++;
        return;
      }
      if (failed.code === NOT_PENDING) {
        workspace.notify({ ...failed, message: "the key conflict is over", hint: "Nothing was changed. The Sync panel shows the current state." }, "warning");
        void sync.resync();
        sync.open({ kind: "panel", tab: "overview" });
        return;
      }
      error = failed;
      if (failed.code === TIMEOUT) void sync.resync();
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && choice === "use" && e.target instanceof HTMLInputElement) {
      e.preventDefault();
      void submit();
    }
  }

  function onkey(key: string): boolean {
    if (choice === "keep" && key === "y") {
      void submit();
      return true;
    }
    return false;
  }
</script>

<Modal bind:armed onescape={close} {onkey} labelledBy="sync-conflict-title" size="wide" tone={choice === "keep" ? "danger" : "default"}>
  {#snippet title()}Key conflict{/snippet}

  <div class="conflict" role="presentation" {onkeydown} bind:this={root} data-testid="sync-key-conflict">
    <p>
      <strong>{other ?? "Another device"}</strong> and this device each changed the sync keys at the same time. Nothing is merged until
      you choose which keys every device keeps.
    </p>

    <div class="choices" role="radiogroup" aria-label="Which keys to keep">
      <button class="choice" role="radio" aria-checked={choice === "use"} onclick={() => pick("use")} data-autofocus>
        <span class="choice-title">Use {otherName}'s keys</span>
        <span class="choice-help">Type the passphrase chosen there. This device switches to them.</span>
      </button>
      <button class="choice" role="radio" aria-checked={choice === "keep"} onclick={() => pick("keep")}>
        <span class="choice-title">Keep this device's keys</span>
        <span class="choice-help">Rotate again from this device, with a new passphrase.</span>
      </button>
    </div>

    {#if choice}
      {#key choice}
        <div class="choice-body" in:fly={{ y: 6, duration: dur("base") }}>
          {#if choice === "use"}
            <label class="field">
              <span>{otherName}'s passphrase</span>
              <input data-autofocus type="password" bind:value={theirs} autocomplete="off" spellcheck="false" aria-invalid={wrong > 0} />
            </label>
            {#if wrong > 0}
              <span class="inline-error" role="alert">Wrong passphrase, try again (attempt {wrong + 1}). It is the one chosen on {otherName}.</span>
            {/if}
          {:else}
            <Caution testid="sync-keep-mine-r17">
              The other rotation's changes to the device list are lost: devices it removed are <strong>not</strong> removed by this one —
              remove them again afterwards. The other devices will see a key conflict too, and need this device's new passphrase.
            </Caution>
            <SyncPassphraseFields bind:value={passphrase} bind:confirm bind:generated bind:wroteDown bind:ready={passReady} label="New passphrase" autofocus savedOutside {refused} />
            <label class="check"><input type="checkbox" bind:checked={rememberKey} /> Remember the new key on this device</label>
            <label class="check">
              <input type="checkbox" bind:checked={saved} />
              I saved it. A lost passphrase cannot be recovered unless a device remembers the key.
            </label>
          {/if}
        </div>
      {/key}
    {/if}

    {#if error}
      <div class="inline-error" role="alert" data-testid="sync-conflict-error">
        <span><CodeText text={error.message} /></span>
        {#if error.hint}<span class="hint"><CodeText text={error.hint} capitalize={false} /></span>{/if}
      </div>
    {/if}
  </div>

  {#snippet footer()}
    {#if busy}<span class="footer-note busy"><span class="spinner"></span>{busy}</span>{/if}
    <button class="secondary" disabled={!!busy} onclick={close}>Not now <kbd>Esc</kbd></button>
    {#if choice === "keep"}
      <button class="danger" disabled={!ready} onclick={submit}>Keep this device's keys <kbd>y</kbd></button>
    {:else}
      <button class="primary" disabled={!ready} onclick={submit}>Use {otherName}'s keys <kbd>↵</kbd></button>
    {/if}
  {/snippet}
</Modal>

<style>
  .conflict { display: grid; gap: var(--space-3); }
  strong { color: var(--text-primary); font-weight: 550; }
  .choices { display: grid; grid-template-columns: repeat(auto-fit, minmax(var(--size-menu), 1fr)); gap: var(--space-2); }
  .choices .choice {
    flex-direction: column;
    align-items: flex-start;
    justify-content: center;
    gap: var(--space-1);
    height: auto;
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-secondary);
    text-align: left;
    transition: border-color var(--motion-fast) var(--ease-standard), background var(--motion-fast) var(--ease-standard);
  }
  .choices .choice:hover { background: var(--bg-elevated); }
  .choices .choice[aria-checked="true"] { border-color: var(--accent); background: var(--bg-selected); }
  .choice-title { color: var(--text-primary); font-weight: 550; }
  .choice-help { color: var(--text-muted); font-size: var(--text-sm); }
  .choice-body { display: grid; gap: var(--space-3); }
  .busy { display: inline-flex; align-items: center; gap: var(--space-2); }
  .hint { color: var(--text-secondary); }
</style>
