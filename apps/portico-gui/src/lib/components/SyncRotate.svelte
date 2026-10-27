<script lang="ts">
  // "Rotate keys…" and "Forget <device>…" (phase 11, S2c-1, docs/SYNC.md
  // §13.8): forgetting is always a rotation (Q2). Steps:
  //   1. what happens: for a forget, FIRST "remove its access to your cloud
  //      folder" (RR7) and the records it last wrote; then the devices that
  //      will need the new passphrase, those on Portico < 0.7 that will be
  //      cut off (Q5), and the server passwords to change (F2);
  //   2. the current passphrase, or "I no longer know it" behind the
  //      reset-level confirmation (type `reset`, Q6);
  //   3. the new passphrase (generator, meter, "I saved it…"), refused by
  //      the core when it opens older keys (`sync_passphrase_reused`);
  //   4. the result: "Keys rotated. Enter the new passphrase on: …", or
  //      "Finishing the key rotation…" while the folder does not answer.
  // The final step is a security action: danger button, `y` only (never
  // Enter). The new passphrase is cleared when sent; the current one once
  // the rotation is done, refused as wrong, or the dialog closes. A
  // generated one is dropped in Rust with the dialog.
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { fly } from "svelte/transition";
  import { asError, type ErrorSummary, type ForgetPreview, type GeneratedPassphrase, type RotationOutcome, type RotationPreview } from "../api";
  import { backend } from "../backend";
  import { dur } from "../motion";
  import { ago, listText, RESET_WORD, subjectLabel } from "../syncview";
  import { sync, TIMEOUT } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Caution from "./Caution.svelte";
  import CodeText from "./CodeText.svelte";
  import Modal from "./Modal.svelte";
  import SyncPassphraseFields, { newPassphrase } from "./SyncPassphraseFields.svelte";

  let { device: initial }: { device: { id: string; label: string } | null } = $props();
  const device = untrack(() => initial);

  type Step = "loading" | "about" | "auth" | "new" | "done";
  let step = $state<Step>("loading");
  let armed = $state(false);
  let busy = $state<string | null>(null);
  let error = $state<ErrorSummary | null>(null);
  let loadError = $state<ErrorSummary | null>(null);
  let preview = $state<RotationPreview | null>(null);
  let forget = $state<ForgetPreview | null>(null);
  let outcome = $state<RotationOutcome | null>(null);
  // Secrets: cleared once sent, and with the dialog.
  let current = $state("");
  let unknown = $state(false);
  let resetTyped = $state("");
  let wrong = $state(0);
  let passphrase = $state("");
  let confirm = $state("");
  let generated = $state<GeneratedPassphrase | null>(null);
  let wroteDown = $state(false);
  let passReady = $state(false);
  let saved = $state(false);
  let rememberKey = $state(untrack(() => sync.overview?.rememberKey ?? true));
  let root = $state<HTMLDivElement | undefined>();

  const others = $derived(preview?.devices.filter((d) => !d.this_device && d.id !== device?.id) ?? []);
  const needNew = $derived(forget ? forget.enter_new_passphrase_on : others.map((d) => d.label));
  const outdated = $derived((preview?.outdated ?? []).filter((label) => label !== device?.label));
  const waiting = $derived(others.filter((d) => d.waiting).map((d) => d.label));
  const exposed = $derived([...new Set([...(preview?.exposed_accounts ?? []), ...(forget?.exposed_accounts ?? [])])]);
  const refused = $derived(error?.code === "sync_passphrase_weak");
  const authReady = $derived(unknown ? resetTyped.trim().toLowerCase() === RESET_WORD : current.length > 0);
  const finalReady = $derived(armed && !busy && passReady && saved && authReady);
  const verb = $derived(device ? "Forget and rotate keys" : "Rotate keys");

  onMount(() => {
    void load();
  });

  async function load() {
    step = "loading";
    loadError = null;
    try {
      const [rotation, forgotten] = await Promise.all([backend.syncRotationPreview(), device ? backend.syncForgetPreview(device.id) : Promise.resolve(null)]);
      preview = rotation;
      forget = forgotten;
      step = "about";
    } catch (e) {
      loadError = asError(e);
      if (loadError.code === TIMEOUT) void sync.resync();
    }
  }

  onDestroy(() => {
    clearSecrets();
    void backend.syncForgetGenerated().catch(() => {});
  });

  function clearSecrets() {
    current = "";
    resetTyped = "";
    passphrase = "";
    confirm = "";
  }

  // A new step takes the focus to its first field (the Modal does it for the first one).
  $effect(() => {
    void step;
    if (!untrack(() => armed)) return;
    void tick().then(() => root?.closest(".modal")?.querySelector<HTMLElement>("[data-autofocus]")?.focus());
  });

  function next() {
    if (busy) return;
    error = null;
    if (step === "about") step = "auth";
    else if (step === "auth" && authReady) step = "new";
  }

  function back() {
    if (busy) return;
    error = null;
    if (step === "new") step = "auth";
    else if (step === "auth") step = "about";
  }

  function close() {
    clearSecrets();
    if (step === "done") sync.open({ kind: "panel", tab: device ? "devices" : "overview" });
    else sync.back();
  }

  async function submit() {
    if (!finalReady) return;
    const authorization = unknown ? ({ kind: "reset_confirmed" } as const) : ({ kind: "current", value: current } as const);
    const request = { deviceIds: device ? [device.id] : [], authorization, newPassphrase: newPassphrase(generated, passphrase), rememberKey };
    // The new passphrase is typed again after a refusal; the current one
    // stays for that retry only (cleared on success, on a wrong one, and
    // with the dialog).
    passphrase = "";
    confirm = "";
    error = null;
    busy = "Deriving the new keys… (a few seconds)";
    try {
      outcome = await backend.syncRotateKeys(request);
      clearSecrets();
      generated = null;
      step = "done";
      const result = outcome.status;
      busy = null;
      await sync.act(async () => result);
    } catch (e) {
      busy = null;
      const failed = asError(e);
      // A generated passphrase serves once: generate it again.
      if (generated) {
        generated = null;
        wroteDown = false;
      }
      saved = false;
      if (failed.code === "sync_wrong_passphrase") {
        wrong++;
        current = "";
        step = "auth";
        return;
      }
      error = failed;
      // Refused new passphrase: back to its (now empty) field.
      void tick().then(() => root?.querySelector<HTMLElement>("[data-autofocus]")?.focus());
      if (failed.code === TIMEOUT || failed.code === "sync_not_pending" || failed.code === "sync_device_unknown") void sync.resync();
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key !== "Enter" || e.target instanceof HTMLButtonElement) return;
    if (step === "about" || step === "auth") {
      e.preventDefault();
      next();
    } else if (step === "done") {
      e.preventDefault();
      close();
    }
  }

  function onkey(key: string): boolean {
    if (step === "new" && key === "y") {
      void submit();
      return true;
    }
    if ((step === "about" || step === "done") && key === "Enter") {
      if (step === "about") next();
      else close();
      return true;
    }
    return false;
  }

  const titles = $derived<Record<Step, string>>({
    loading: device ? `Forget ${device.label}` : "Rotate sync keys",
    about: device ? `Forget ${device.label}` : "Rotate sync keys",
    auth: "Current passphrase",
    new: "New passphrase",
    done: outcome?.status.state === "waiting" ? "Finishing the key rotation…" : "Keys rotated",
  });
  const stepNo = $derived(step === "about" ? 1 : step === "auth" ? 2 : step === "new" ? 3 : 0);
</script>

<Modal bind:armed onescape={close} {onkey} labelledBy="sync-rotate-title" size="wide" tone={step === "new" ? "danger" : "default"}>
  {#snippet title()}
    <span class="title-row">
      {titles[step]}
      {#if stepNo}<span class="step-no">Step {stepNo} of 3</span>{/if}
    </span>
  {/snippet}

  <div class="rotate" role="presentation" {onkeydown} bind:this={root} data-testid="sync-rotate">
    {#key step}
      <div class="step" in:fly={{ y: 6, duration: dur("base") }}>
        {#if step === "loading"}
          {#if loadError}
            <div class="inline-error" role="alert"><span><CodeText text={loadError.message} /></span>{#if loadError.hint}<span class="hint"><CodeText text={loadError.hint} capitalize={false} /></span>{/if}</div>
          {:else}
            <div class="skeleton" aria-busy="true"></div>
          {/if}
        {:else if step === "about"}
          {#if device && forget}
            <Caution testid="sync-forget-cloud">
              <strong>Also remove {forget.label}'s access to your cloud folder</strong> (its Dropbox, iCloud or OneDrive session, or its
              Syncthing share): only that closes the remaining window. Without it, the lost device can still put old files back.
            </Caution>
            <p>
              Portico removes <span class="mono">{forget.label}</span> from the device list and replaces the sync keys. It can no longer
              read anything written from now on; what it could read before stays readable to it.
              {#if forget.last_seen}Last seen here {ago(forget.last_seen, sync.now)}.{/if}
            </p>
            {#if forget.last_written.length}
              <section class="group">
                <span class="caption">It last wrote {forget.last_written.length} {forget.last_written.length === 1 ? "record" : "records"} · they stay</span>
                <ul class="records">
                  {#each forget.last_written as subject, i (i)}<li class="mono">{subjectLabel(subject)}</li>{/each}
                </ul>
                <p class="note">Review them afterwards if you do not trust what it wrote.</p>
              </section>
            {/if}
          {:else}
            <p>
              Portico replaces the sync keys and the passphrase. Anyone who copied the folder or knows the old passphrase cannot read
              what is written from now on. Your hosts, snippets and settings stay as they are.
            </p>
          {/if}
          <section class="group">
            <span class="caption">Enter the new passphrase afterwards on</span>
            {#if needNew.length}
              <ul class="devices">
                {#each needNew as label (label)}<li class="mono">{label}</li>{/each}
              </ul>
              <p class="note">Until then they stop syncing; nothing is lost on them.</p>
            {:else}
              <p class="note">No other device: only this one uses the sync.</p>
            {/if}
          </section>
          {#if outdated.length}
            <Caution testid="sync-rotate-outdated">
              <strong>{listText(outdated)} {outdated.length === 1 ? "runs" : "run"} Portico older than 0.7</strong> and will stop syncing
              after this. Update Portico there first, or accept that {outdated.length === 1 ? "it stops" : "they stop"} until updated.
            </Caution>
          {/if}
          {#if waiting.length}
            <Caution tone="note">
              Some data of {listText(waiting)} has not arrived yet: what only {waiting.length === 1 ? "it holds" : "they hold"} reaches
              the others once {waiting.length === 1 ? "it types" : "they type"} the new passphrase.
            </Caution>
          {/if}
          {#if exposed.length}
            <Caution>Passwords of these servers were shared through sync: change them on the servers afterwards. <span class="mono">{exposed.join(", ")}</span></Caution>
          {/if}
        {:else if step === "auth"}
          <p>The current sync passphrase authorizes the new keys.</p>
          {#if !unknown}
            <label class="field">
              <span>Current passphrase</span>
              <input data-autofocus type="password" bind:value={current} autocomplete="current-password" spellcheck="false" aria-invalid={wrong > 0} />
            </label>
            {#if wrong > 0}
              <span class="inline-error" role="alert">Wrong passphrase, try again (attempt {wrong + 1}). Check Caps Lock and the keyboard layout.</span>
            {/if}
          {/if}
          <label class="check">
            <input type="checkbox" bind:checked={unknown} onchange={() => ((current = ""), (resetTyped = ""), (wrong = 0))} data-testid="sync-rotate-unknown" />
            I no longer know it
          </label>
          {#if unknown}
            <Caution>
              This device remembers the sync key, so it can authorize new keys without the passphrase, like a reset. Every other device
              will need the new passphrase.
            </Caution>
            <label class="field">
              <span>Type <span class="mono">{RESET_WORD}</span> to confirm</span>
              <input data-autofocus bind:value={resetTyped} spellcheck="false" autocomplete="off" />
            </label>
          {/if}
        {:else if step === "new"}
          <p>Choose the passphrase every device will use from now on. It must differ from every earlier one.</p>
          <SyncPassphraseFields bind:value={passphrase} bind:confirm bind:generated bind:wroteDown bind:ready={passReady} label="New passphrase" autofocus savedOutside {refused} />
          <label class="check">
            <input type="checkbox" bind:checked={rememberKey} />
            Remember the new key on this device
          </label>
          <div class="recovery">
            <label class="check">
              <input type="checkbox" bind:checked={saved} data-testid="sync-rotate-saved" />
              I saved it. A lost passphrase cannot be recovered unless a device remembers the key.
            </label>
          </div>
        {:else if step === "done" && outcome}
          {#if outcome.status.state === "waiting"}
            <section class="callout" data-tone="quiet" data-testid="sync-rotate-pending">
              <strong>The new keys are saved on this device</strong>
              <p>Portico writes them to the sync folder as soon as it answers. Nothing goes back to the old keys.</p>
            </section>
          {:else}
            <section class="callout" data-tone="ok" data-testid="sync-rotate-done">
              <strong>The new keys are in use on this device</strong>
              {#if outcome.forgotten.length}<p>Forgotten: <span class="mono">{listText(outcome.forgotten)}</span>. It can no longer read new changes.</p>{/if}
            </section>
          {/if}
          <section class="group">
            <span class="caption">Enter the new passphrase on</span>
            {#if outcome.enter_new_passphrase_on.length}
              <ul class="devices">
                {#each outcome.enter_new_passphrase_on as label (label)}<li class="mono">{label}</li>{/each}
              </ul>
              <p class="note">They stop syncing until then. Portico shows them “Keys changed — enter new passphrase”.</p>
            {:else}
              <p class="note">No other device.</p>
            {/if}
          </section>
          {#if outcome.exposed_accounts.length}
            <Caution testid="sync-rotate-exposed">
              <strong>Change these server passwords:</strong> <span class="mono">{outcome.exposed_accounts.join(", ")}</span>. They were
              shared through sync; new keys do not hide what was already readable.
            </Caution>
          {/if}
          {#if device}
            <p class="note">If you have not yet, remove {device.label}'s access to your cloud folder.</p>
          {/if}
        {/if}
      </div>
    {/key}
    {#if error}
      <div class="inline-error" role="alert"><span><CodeText text={error.message} /></span>{#if error.hint}<span class="hint"><CodeText text={error.hint} capitalize={false} /></span>{/if}</div>
    {/if}
  </div>

  {#snippet footer()}
    {#if busy}<span class="footer-note busy"><span class="spinner"></span>{busy}</span>{/if}
    {#if step === "done"}
      <button class="primary" data-autofocus onclick={close}>Done <kbd>↵</kbd></button>
    {:else}
      <button class="secondary" disabled={!!busy} onclick={close}>Cancel <kbd>Esc</kbd></button>
      {#if step === "auth" || step === "new"}<button class="secondary" disabled={!!busy} onclick={back}>Back</button>{/if}
      {#if step === "loading" && loadError}
        <button class="primary" onclick={load}>Try again</button>
      {:else if step === "about"}
        <button class="primary" data-autofocus disabled={!armed} onclick={next}>Continue <kbd>↵</kbd></button>
      {:else if step === "auth"}
        <button class="primary" disabled={!authReady} onclick={next}>Continue <kbd>↵</kbd></button>
      {:else if step === "new"}
        <button class="danger" disabled={!finalReady} onclick={submit} data-testid="sync-rotate-submit">{verb} <kbd>y</kbd></button>
      {/if}
    {/if}
  {/snippet}
</Modal>

<style>
  .rotate { display: grid; gap: var(--space-3); }
  .step { display: grid; gap: var(--space-3); }
  .title-row { display: inline-flex; align-items: baseline; gap: var(--space-3); }
  .step-no { color: var(--text-muted); font-size: var(--text-xs); font-weight: 450; letter-spacing: var(--tracking-caps); text-transform: uppercase; }
  .group { display: grid; gap: var(--space-1); }
  .caption { color: var(--text-muted); font-size: var(--text-xs); letter-spacing: var(--tracking-caps); text-transform: uppercase; }
  .devices, .records { display: flex; flex-wrap: wrap; gap: var(--space-1) var(--space-2); margin: 0; padding: 0; list-style: none; }
  .devices li, .records li {
    padding: 0 var(--space-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-pill);
    color: var(--text-primary);
    font-size: var(--text-sm);
    line-height: var(--size-badge);
  }
  .records li { color: var(--text-secondary); }
  .note { color: var(--text-muted); font-size: var(--text-sm); }
  .mono { font-family: var(--font-mono); }
  strong { color: var(--text-primary); font-weight: 550; }
  .callout {
    --c: var(--border-strong);
    display: grid;
    gap: var(--space-1);
    padding: var(--space-3) var(--space-4);
    border-left: 2px solid var(--c);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  .callout[data-tone="ok"] { --c: var(--status-connected); }
  .callout[data-tone="quiet"] { --c: var(--status-connecting); }
  .recovery { padding-top: var(--space-2); border-top: 1px solid var(--border-subtle); }
  .skeleton { height: calc(4 * var(--size-row)); border-radius: var(--radius-md); background: var(--bg-elevated); animation: rotate-pulse var(--motion-loop) var(--ease-standard) infinite; }
  @keyframes rotate-pulse { 50% { opacity: 0.4; } }
  .busy { display: inline-flex; align-items: center; gap: var(--space-2); }
  .hint { color: var(--text-secondary); }
</style>
