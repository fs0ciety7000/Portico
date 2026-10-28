<script lang="ts">
  // Keys changed on another device (phase 11, S2c-1, docs/SYNC.md §13.3.5):
  // "Enter the NEW passphrase. If you did not change the keys on another
  // device, type nothing." On a device that does not remember the key, the
  // previous passphrase comes first (RR4): it keeps where local changes came
  // from; skipping it is allowed, and every local difference then waits for
  // a decision in Received. A device file that claims newer keys when
  // nobody changed them can be removed here (RR5, the file named, its
  // content fingerprint sent back). Passphrases are cleared once sent.
  import { onDestroy, tick, untrack } from "svelte";
  import { fly } from "svelte/transition";
  import { asError, type ErrorSummary, type NewerEpochFile } from "../api";
  import SyncNewerFiles from "./SyncNewerFiles.svelte";
  import { backend } from "../backend";
  import { dur } from "../motion";
  import { ADOPT_UNSURE_AFTER_R1, NEWER_FILE_HINT, newKeysTitle, REUSED, reusedHint, TYPE_NOTHING } from "../syncview";
  import { sync, TIMEOUT } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import CodeText from "./CodeText.svelte";
  import Modal from "./Modal.svelte";

  type Step = "previous" | "new";
  let step = $state<Step>(untrack(() => (sync.overview?.rememberKey === false ? "previous" : "new")));
  let armed = $state(false);
  let busy = $state<string | null>(null);
  let error = $state<ErrorSummary | null>(null);
  let wrong = $state(0);
  let skipped = $state(false);
  let passphrase = $state("");
  let discarding = $state<NewerEpochFile | null>(null);
  let newer = $state<SyncNewerFiles | undefined>();
  let root = $state<HTMLDivElement | undefined>();

  const status = $derived(sync.status);
  const who = $derived(status?.state === "alert" ? status.device : null);
  /** A reused passphrase: an old one, said under the field (B3). */
  const reused = $derived(error?.code === REUSED ? error : null);

  onDestroy(() => (passphrase = ""));

  // Adopted elsewhere (the other Portico app), or the alert changed: nothing to do here.
  $effect(() => {
    const s = status;
    if (busy || !s) return;
    if (!(s.state === "alert" && s.kind === "keys_rotated_elsewhere")) {
      untrack(() => {
        passphrase = "";
        sync.open({ kind: "panel", tab: "overview" });
      });
    }
  });

  $effect(() => {
    void step;
    if (!untrack(() => armed)) return;
    void tick().then(() => root?.closest(".modal")?.querySelector<HTMLElement>("[data-autofocus]")?.focus());
  });

  function close() {
    passphrase = "";
    sync.back();
  }

  function skip() {
    passphrase = "";
    wrong = 0;
    error = null;
    skipped = true;
    step = "new";
  }

  async function submit() {
    if (!passphrase || busy || !armed) return;
    const secret = passphrase;
    const previous = step === "previous";
    passphrase = "";
    error = null;
    busy = previous ? "Opening the previous keys…" : "Deriving the new keys… (a few seconds)";
    try {
      const result = await backend.syncUnlock(secret);
      if (previous) {
        // In memory only: the new passphrase is still needed (usually).
        busy = null;
        wrong = 0;
        if (result.state === "alert" && result.kind === "keys_rotated_elsewhere") {
          step = "new";
          await sync.act(async () => result);
          return;
        }
      }
      busy = null;
      await sync.act(async () => result);
      workspace.notify({ code: "sync", message: "The new sync keys are in use", hint: skipped ? "Local changes of unknown origin wait for you in Sync, Received." : null }, "info");
      sync.open({ kind: "panel", tab: sync.held.length ? "received" : "overview" });
    } catch (e) {
      busy = null;
      const failed = asError(e);
      if (failed.code === "sync_wrong_passphrase") {
        wrong++;
        return;
      }
      error = failed;
      if (failed.code === TIMEOUT || failed.code === "sync_device_forgotten") void sync.resync();
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && e.target instanceof HTMLInputElement) {
      e.preventDefault();
      void submit();
    }
  }

  function onkey(key: string): boolean {
    if (discarding && key === "y") {
      void newer?.remove();
      return true;
    }
    return false;
  }
</script>

<Modal bind:armed onescape={() => (discarding ? (discarding = null) : close())} {onkey} labelledBy="sync-adopt-title" top>
  {#snippet title()}Enter the new passphrase{/snippet}

  <div class="adopt" role="presentation" {onkeydown} bind:this={root} data-testid="sync-adopt">
    <section class="lead">
      <strong class="title">{newKeysTitle(who)}</strong>
      <p>Enter the NEW passphrase chosen there.</p>
      <p class="r1" data-testid="sync-adopt-r1"><span class="mark" aria-hidden="true">▲</span>{TYPE_NOTHING}</p>
      <p class="unsure">{ADOPT_UNSURE_AFTER_R1}</p>
      {#if sync.newerFiles.length}<p class="unsure">{NEWER_FILE_HINT}</p>{/if}
    </section>

    {#key step}
      <div class="step" in:fly={{ y: 6, duration: dur("base") }}>
        {#if step === "previous"}
          <p>
            This device does not remember the key. Type the <em>previous</em> passphrase first: Portico then knows which local changes
            came from where. Without it, every local difference waits for your decision, one by one.
          </p>
          <label class="field">
            <span>Previous passphrase</span>
            <input data-autofocus type="password" bind:value={passphrase} autocomplete="current-password" spellcheck="false" aria-invalid={wrong > 0} />
          </label>
          {#if wrong > 0}
            <span class="inline-error" role="alert">This passphrase does not open the previous keys (attempt {wrong + 1}). Check it, or continue without it.</span>
          {/if}
        {:else}
          <label class="field">
            <span>New passphrase</span>
            <input
              data-autofocus
              type="password"
              bind:value={passphrase}
              autocomplete="current-password"
              spellcheck="false"
              aria-invalid={wrong > 0 || !!reused}
              aria-describedby={reused ? "sync-adopt-reused" : undefined}
              oninput={() => reused && (error = null)}
            />
          </label>
          {#if reused}
            <div class="inline-error" role="alert" id="sync-adopt-reused" data-testid="sync-pass-problem">
              <span><CodeText text={reused.message} /></span>
              <span class="hint">{reusedHint("adoption")}</span>
            </div>
          {:else if wrong > 0}
            <span class="inline-error" role="alert">Wrong passphrase, try again (attempt {wrong + 1}). It is the one chosen on the other device.</span>
          {/if}
          {#if skipped}<p class="note">Without the previous passphrase: local differences will wait in Received.</p>{/if}
        {/if}
      </div>
    {/key}

    {#if sync.newerFiles.length}
      <div class="newer-wrap"><SyncNewerFiles bind:this={newer} bind:asking={discarding} bind:busy onerror={(e) => (error = e)} /></div>
    {/if}

    {#if error && !reused}
      <div class="inline-error" role="alert">
        <span><CodeText text={error.message} /></span>
        {#if error.hint}<span class="hint"><CodeText text={error.hint} capitalize={false} /></span>{/if}
        {#if error.code === "sync_lineage_unknown" || error.code === "sync_device_forgotten"}
          <span><button class="secondary small" onclick={() => sync.open({ kind: "form", form: "rejoin" })}>Turn sync off and join again…</button></span>
        {/if}
      </div>
    {/if}
  </div>

  {#snippet footer()}
    {#if busy}<span class="footer-note busy"><span class="spinner"></span>{busy}</span>{/if}
    <button class="secondary" disabled={!!busy} onclick={close}>Not now <kbd>Esc</kbd></button>
    {#if step === "previous"}
      <button class="secondary" disabled={!!busy} onclick={skip} data-testid="sync-adopt-skip">I don't remember it</button>
      <button class="primary" disabled={!passphrase || !!busy || !armed} onclick={submit}>Continue <kbd>↵</kbd></button>
    {:else}
      <button class="primary" disabled={!passphrase || !!busy || !armed} onclick={submit}>Use the new keys <kbd>↵</kbd></button>
    {/if}
  {/snippet}
</Modal>

<style>
  .adopt { display: grid; gap: var(--space-3); }
  .lead { display: grid; gap: var(--space-1); }
  .lead .title { font-family: var(--font-display); font-size: var(--text-md); font-weight: 600; }
  .r1 { display: flex; gap: var(--space-2); color: var(--text-primary); font-weight: 550; }
  .r1 .mark { color: var(--status-warning); font-size: var(--text-xs); line-height: inherit; }
  .unsure { color: var(--text-secondary); font-size: var(--text-sm); }
  .step { display: grid; gap: var(--space-3); }
  strong { color: var(--text-primary); font-weight: 550; }
  em { color: var(--text-primary); font-style: normal; font-weight: 550; }
  .note { color: var(--text-muted); font-size: var(--text-sm); }
  .newer-wrap { padding-top: var(--space-3); border-top: 1px solid var(--border-subtle); }
  .busy { display: inline-flex; align-items: center; gap: var(--space-2); }
  .hint { color: var(--text-secondary); }
</style>
