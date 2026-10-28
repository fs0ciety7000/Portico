<script lang="ts">
  // A rotation conflict (phase 11, S2c-1, docs/SYNC.md §13.3.7): two devices
  // changed the keys at the same time. Two explicit choices, nothing
  // preselected (no primary action until one is chosen), each card saying
  // what it costs:
  //   - "Use <device>'s keys" (`u`): its passphrase; refused (E2) while those
  //     keys still include a device this one removed: the card is then
  //     disabled with the refusal, and the focus goes to the other card;
  //   - "Keep this device's keys" (`o`): a new rotation from this device's
  //     keys (R17: the other rotation's removals are lost, the other devices
  //     see a conflict too): new passphrase, "I saved it", danger, `y` only.
  // ←/→ move between the cards (a radio group). Offered only while the
  // conflict is authentic: `sync_not_pending` closes the dialog and reloads.
  // A `sync_folder_timeout` is no refusal (A4): "Result unknown — Portico is
  // checking", and nothing is asked again until the status is known.
  // Passphrases are cleared once sent.
  import { onDestroy, tick, untrack } from "svelte";
  import { fly } from "svelte/transition";
  import { asError, type ErrorSummary, type GeneratedPassphrase } from "../api";
  import { backend } from "../backend";
  import { dur } from "../motion";
  import { deviceName, KEEP_MINE_TEXT, keepCost, REUSED, reusedHint, useCost, yArmed } from "../syncview";
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
  /** The E2 refusal: the other device's keys still include a removed device. */
  let useRefused = $state<ErrorSummary | null>(null);
  /** A timeout: the result is not known yet (A4). */
  let checking = $state(false);
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
  let readyAt = $state<number | null>(null);

  const status = $derived(sync.status);
  // The other device as last named by the alert: kept while the status
  // reads "folder unavailable" after a timeout.
  const named = untrack(() => (sync.status?.state === "alert" ? sync.status.device : null));
  const other = $derived(status?.state === "alert" ? status.device : named);
  const otherName = $derived(other ? deviceName(other) : "the other device");
  const refused = $derived(error?.code === "sync_passphrase_weak");
  const problem = $derived(error?.code === REUSED ? { message: error.message, hint: reusedHint("rotation") } : null);
  const ready = $derived(
    armed && !busy && !checking && (choice === "use" ? !useRefused && theirs.length > 0 : choice === "keep" ? passReady && saved : false),
  );
  $effect(() => {
    readyAt = ready ? performance.now() : null;
  });

  function clearSecrets() {
    theirs = "";
    passphrase = "";
    confirm = "";
  }

  onDestroy(() => {
    clearSecrets();
    void backend.syncForgetGenerated().catch(() => {});
  });

  // Settled elsewhere (the other Portico app, a pass, or the answer that
  // timed out): nothing to choose any more. After a timeout the core says
  // "folder unavailable" until the operation ends: still unknown.
  $effect(() => {
    const s = status;
    if (busy || !s) return;
    const conflict = s.state === "alert" && s.kind === "rotation_conflict";
    const wasChecking = untrack(() => checking);
    if (wasChecking && (s.state === "folder_unavailable" || s.state === "syncing")) return;
    untrack(() => {
      if (conflict && wasChecking) {
        // The folder answered and the conflict is still there: the choice did not apply.
        checking = false;
        error = { code: TIMEOUT, message: "nothing was changed", hint: "The folder answered and the conflict is still there: choose again." };
      } else if (!conflict) {
        clearSecrets();
        if (wasChecking) workspace.notify({ code: "sync", message: "The key conflict is settled", hint: "The Sync panel shows the current state." }, "info");
        sync.open({ kind: "panel", tab: "overview" });
      }
    });
  });

  const cards = () => [...(root?.querySelectorAll<HTMLElement>(".choice") ?? [])];

  function focusCard(which: "use" | "keep") {
    void tick().then(() => cards()[which === "use" ? 0 : 1]?.focus());
  }

  function focusBody() {
    void tick().then(() => root?.querySelector<HTMLElement>(".choice-body [data-autofocus]")?.focus());
  }

  function pick(next: Choice, focus = true) {
    if (busy || checking) return;
    if (next === "use" && useRefused) return;
    if (next !== choice) {
      clearSecrets();
      error = null;
      wrong = 0;
      choice = next;
    }
    if (focus) focusBody();
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
      const failed = asError(e);
      if (generated) {
        generated = null;
        wroteDown = false;
      }
      saved = false;
      if (failed.code === TIMEOUT) {
        // No refusal: it may still apply. Read again; ask nothing meanwhile.
        checking = true;
        busy = null;
        await sync.resync();
        return;
      }
      busy = null;
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
      if (failed.code === "sync_rotation_conflict" && !kept) {
        // E2: their keys cannot be used; only keeping this device's keys remains.
        useRefused = failed;
        choice = null;
        focusCard("keep");
        return;
      }
      error = failed;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter" && choice === "use" && e.target instanceof HTMLInputElement) {
      e.preventDefault();
      void submit();
    }
  }

  /** ←/→ (↑/↓) in the radio group: the other card, chosen. */
  function oncardkey(e: KeyboardEvent, which: "use" | "keep") {
    if (!["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"].includes(e.key)) return;
    e.preventDefault();
    const next = which === "use" ? "keep" : useRefused ? "keep" : "use";
    pick(next, false);
    focusCard(next);
  }

  function onkey(key: string): boolean {
    if (choice === "keep" && key === "y") {
      if (ready && yArmed(readyAt, performance.now())) void submit();
      return true;
    }
    if (key === "u" && !useRefused) {
      pick("use");
      return true;
    }
    if (key === "o") {
      pick("keep");
      return true;
    }
    return false;
  }
</script>

<Modal bind:armed onescape={close} {onkey} labelledBy="sync-conflict-title" size="wide" top tone={choice === "keep" ? "danger" : "default"}>
  {#snippet title()}Key conflict{/snippet}

  <div class="conflict" role="presentation" {onkeydown} bind:this={root} data-testid="sync-key-conflict">
    <p>
      <strong>{other ?? "Another device"}</strong> and this device each changed the sync keys at the same time. Nothing is merged until
      you choose which keys every device keeps.
    </p>

    <div class="choices" role="radiogroup" aria-label="Which keys to keep">
      <button
        class="choice"
        role="radio"
        aria-checked={choice === "use"}
        tabindex={choice === "keep" || useRefused ? -1 : 0}
        disabled={!!useRefused || checking}
        onclick={() => pick("use")}
        onkeydown={(e) => oncardkey(e, "use")}
        data-autofocus={useRefused ? undefined : ""}
        data-testid="sync-conflict-use"
      >
        <span class="choice-title">Use {otherName}'s keys <kbd>u</kbd></span>
        {#if useRefused}
          <span class="choice-help refused" data-testid="sync-conflict-refused">Refused: <CodeText text={useRefused.message} capitalize={false} /></span>
        {:else}
          <span class="choice-help">{useCost(otherName)}</span>
        {/if}
      </button>
      <button
        class="choice"
        role="radio"
        aria-checked={choice === "keep"}
        tabindex={choice === "keep" || useRefused ? 0 : -1}
        disabled={checking}
        onclick={() => pick("keep")}
        onkeydown={(e) => oncardkey(e, "keep")}
        data-autofocus={useRefused ? "" : undefined}
        data-testid="sync-conflict-keep"
      >
        <span class="choice-title">Keep this device's keys <kbd>o</kbd></span>
        <span class="choice-help">{keepCost(otherName)}</span>
      </button>
    </div>

    {#if checking}
      <section class="callout" role="status" data-testid="sync-conflict-checking">
        <strong><span class="spinner"></span> Result unknown — Portico is checking</strong>
        <p>The sync folder did not answer in time, so your choice may still apply. Portico reads the folder again before asking anything.</p>
      </section>
    {:else if choice}
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
            <Caution testid="sync-keep-mine-r17">{KEEP_MINE_TEXT}</Caution>
            <SyncPassphraseFields
              bind:value={passphrase}
              bind:confirm
              bind:generated
              bind:wroteDown
              bind:ready={passReady}
              label="New passphrase"
              autofocus
              savedOutside
              {refused}
              {problem}
              onedit={() => problem && (error = null)}
            />
            <label class="check"><input type="checkbox" bind:checked={rememberKey} /> Remember the new key on this device</label>
            <label class="check">
              <input type="checkbox" bind:checked={saved} data-testid="sync-conflict-saved" />
              I saved it. A lost passphrase cannot be recovered unless a device remembers the key.
            </label>
          {/if}
        </div>
      {/key}
    {:else}
      <p class="note">Choose one: <kbd>u</kbd> uses {otherName}'s keys, <kbd>o</kbd> keeps this device's.</p>
    {/if}

    {#if error && !problem}
      <div class="inline-error" role="alert" data-testid="sync-conflict-error">
        <span><CodeText text={error.message} /></span>
        {#if error.hint}<span class="hint"><CodeText text={error.hint} capitalize={false} /></span>{/if}
      </div>
    {/if}
  </div>

  {#snippet footer()}
    {#if busy}<span class="footer-note busy"><span class="spinner"></span>{busy}</span>{/if}
    <button class="secondary" disabled={!!busy} onclick={close}>Not now <kbd>Esc</kbd></button>
    {#if checking}
      <button class="secondary" onclick={() => void sync.resync()}>Check again</button>
    {:else if choice === "keep"}
      <button class="danger" disabled={!ready} onclick={submit} data-testid="sync-conflict-submit">Keep this device's keys <kbd>y</kbd></button>
    {:else if choice === "use"}
      <button class="primary" disabled={!ready} onclick={submit} data-testid="sync-conflict-submit">Use {otherName}'s keys <kbd>↵</kbd></button>
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
  .choices .choice:focus-visible { outline: 2px solid var(--accent); outline-offset: 2px; }
  .choices .choice:disabled { opacity: 1; }
  .choices .choice:disabled .choice-title { color: var(--text-muted); }
  .choice-help.refused { color: var(--status-error); }
  .choice-title { display: inline-flex; align-items: center; gap: var(--space-2); }
  .callout {
    display: grid;
    gap: var(--space-1);
    padding: var(--space-3) var(--space-4);
    border-left: 2px solid var(--status-connecting);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  .callout strong { display: inline-flex; align-items: center; gap: var(--space-2); }
  .note { color: var(--text-muted); font-size: var(--text-sm); }
  .choice-title { color: var(--text-primary); font-weight: 550; }
  .choice-help { color: var(--text-muted); font-size: var(--text-sm); }
  .choice-body { display: grid; gap: var(--space-3); }
  .busy { display: inline-flex; align-items: center; gap: var(--space-2); }
  .hint { color: var(--text-secondary); }
</style>
