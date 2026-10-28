<script lang="ts">
  // "Sync…" when sync is off (phase 11, docs/SYNC.md §6): what it does, the
  // folder (native picker, in Rust), then create (first device) or join
  // (review before anything is written). The recovery warning comes before
  // the irreversible step, merged into it: "I saved it" gates Create and
  // Join. Passphrase fields are cleared as soon as they are sent; a
  // generated passphrase is dropped in Rust when the wizard closes.
  // S2c-1: "Join again as a new device" opens it on the folder step, the
  // folder still picked in Rust; a bundle this device held before
  // (`JoinPreview.rejoin`) only offers "Review each".
  import { onDestroy, onMount, tick, untrack } from "svelte";
  import { fly } from "svelte/transition";
  import { asError, type ErrorSummary, type GeneratedPassphrase, type JoinBulkChoice, type JoinPreview, type PickedFolder } from "../api";
  import { backend } from "../backend";
  import { dur } from "../motion";
  import { JOIN_TTL_MS, probeVerdict, validityLeft } from "../syncview";
  import { sync } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import CodeText from "./CodeText.svelte";
  import Modal from "./Modal.svelte";
  import SyncPassphraseFields, { newPassphrase } from "./SyncPassphraseFields.svelte";

  type Step = "intro" | "folder" | "create" | "join" | "review";

  let { rejoin = null }: { rejoin?: PickedFolder | null } = $props();

  let step: Step = $state(untrack(() => (rejoin ? "folder" : "intro")));
  let armed = $state(false);
  let busy: string | null = $state(null);
  let error = $state<ErrorSummary | null>(null);
  let picked = $state<PickedFolder | null>(untrack(() => rejoin));
  // Passphrases: cleared once sent.
  let passphrase = $state("");
  let confirm = $state("");
  let generated = $state<GeneratedPassphrase | null>(null);
  let passReady = $state(false);
  /** "I saved it": gates Create (typed or generated) and Join. */
  let saved = $state(false);
  let deviceName = $state("");
  let rememberKey = $state(true);
  let preview = $state<JoinPreview | null>(null);
  let previewAt = $state(0);
  let bulk: JoinBulkChoice = $state("review_each");
  let now = $state(Date.now());
  let root: HTMLDivElement | undefined = $state();

  onMount(() => {
    backend.syncDefaultDeviceName().then(
      (name) => (deviceName ||= name),
      () => {},
    );
    // The review's remaining validity.
    const timer = setInterval(() => (now = Date.now()), 15_000);
    return () => clearInterval(timer);
  });

  // A generated passphrase not used yet: Rust drops it with the dialog.
  onDestroy(() => void backend.syncForgetGenerated().catch(() => {}));

  // A new step takes the focus to its first field or its gating checkbox
  // (the Modal does it for the first step, once armed).
  $effect(() => {
    void step;
    if (!untrack(() => armed)) return;
    void tick().then(() => root?.closest(".modal")?.querySelector<HTMLElement>("[data-autofocus]")?.focus());
  });

  const verdict = $derived(picked ? probeVerdict(picked.probe) : null);
  const nameOk = $derived(deviceName.trim().length > 0);
  const refused = $derived(error?.code === "sync_passphrase_weak");

  async function pick() {
    error = null;
    busy = "Waiting for the folder…";
    try {
      const chosen = await backend.syncPickFolder();
      if (chosen) {
        picked = chosen;
        step = "folder";
      }
    } catch (e) {
      error = asError(e);
    } finally {
      busy = null;
    }
  }

  async function checkAgain() {
    if (!picked) return;
    busy = "Checking the folder…";
    error = null;
    try {
      picked = { ...picked, probe: await backend.syncProbeAgain() };
    } catch (e) {
      error = asError(e);
    } finally {
      busy = null;
    }
  }

  function forget() {
    passphrase = "";
    confirm = "";
  }

  async function create() {
    if (!passReady || !nameOk || !saved || busy) return;
    const secret = newPassphrase(generated, passphrase);
    forget();
    busy = "Deriving the key… (a few seconds)";
    error = null;
    try {
      const status = await backend.syncCreate(secret, deviceName.trim(), rememberKey);
      generated = null;
      await sync.act(async () => status);
      finish(false);
    } catch (e) {
      error = asError(e);
      // A generated passphrase serves once: generate it again.
      if (generated) {
        generated = null;
        saved = false;
      }
      await sync.refresh();
    } finally {
      busy = null;
    }
  }

  async function review() {
    if (!passphrase || !nameOk || busy) return;
    const secret = passphrase;
    forget();
    busy = "Deriving the key… (a few seconds)";
    error = null;
    try {
      preview = await backend.syncJoinPreview(secret, deviceName.trim(), rememberKey);
      previewAt = Date.now();
      now = previewAt;
      bulk = "review_each";
      saved = false;
      step = "review";
    } catch (e) {
      error = asError(e);
    } finally {
      busy = null;
    }
  }

  async function join() {
    if (!preview || !saved || busy) return;
    if (Date.now() - previewAt > JOIN_TTL_MS) return expired();
    busy = "Joining…";
    error = null;
    try {
      const status = await backend.syncJoinCommit(preview.token, preview.rejoin ? "review_each" : bulk);
      preview = null;
      await sync.act(async () => status);
      finish(true);
    } catch (e) {
      const failed = asError(e);
      if (failed.code === "sync_join_expired") return expired();
      error = failed;
      await sync.refresh();
    } finally {
      busy = null;
    }
  }

  /** The review token is single use and lasts 10 minutes: start the review again. */
  function expired() {
    preview = null;
    step = "join";
    error = { code: "sync_join_expired", message: "this review expired", hint: "Enter the passphrase again to see what joining brings now." };
  }

  /** Success: a notice, and the received changes after a join. */
  function finish(joined: boolean) {
    sync.close();
    workspace.notify({ code: "sync_on", message: "Sync is on", hint: joined ? "Received hosts wait for your confirmation in Sync, Received." : null }, "info");
    if (joined && sync.received) sync.open({ kind: "panel", tab: "received" });
  }

  function close() {
    forget();
    sync.close();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key !== "Enter" || e.target instanceof HTMLButtonElement || !armed) return;
    e.preventDefault();
    if (step === "create") void create();
    else if (step === "join") void review();
    else if (step === "review") void join();
  }

  const titles: Record<Step, string> = {
    intro: "Sync with your other devices",
    folder: "Sync folder",
    create: "Create the sync",
    join: "Join the sync",
    review: "What joining brings",
  };
  const BULK: { id: JoinBulkChoice; label: string; help: string }[] = [
    { id: "review_each", label: "Review each", help: "Every conflict waits for you in Sync, Conflicts." },
    { id: "prefer_remote", label: "Prefer the folder's", help: "Conflicts take the other devices' values." },
    { id: "prefer_local", label: "Prefer this device's", help: "Conflicts keep what this device has." },
  ];
</script>

<Modal bind:armed onescape={close} labelledBy="sync-wizard-title" size="wide">
  {#snippet title()}{titles[step]}{/snippet}

  <div class="wizard" role="presentation" {onkeydown} bind:this={root}>
    {#key step}
      <div class="step" in:fly={{ y: 6, duration: dur("base") }}>
        {#if step === "intro"}
          <p class="lead">Keep hosts, tags, snippets and settings the same on every device, through a folder you already sync.</p>
          <ul class="points">
            <li><strong>Only encrypted files leave this machine.</strong> Portico writes them into a folder synced by iCloud Drive, Dropbox, OneDrive, Syncthing, git…</li>
            <li><strong>Private keys and known_hosts are never synced.</strong> Neither are passwords.</li>
            <li><strong>No account, no server.</strong> Portico itself never goes on the network for this.</li>
            <li>Received hosts and snippets wait for your confirmation on each device before they are used.</li>
          </ul>
        {:else if step === "folder" && picked && verdict}
          <div class="folder">
            <span class="caption">Folder</span>
            <code class="path">{picked.display}</code>
          </div>
          <div class="verdict" data-tone={verdict.tone} data-testid="sync-probe">
            <strong><CodeText text={verdict.title} /></strong>
            {#if verdict.body}<p>{verdict.body}</p>{/if}
            {#if verdict.hint}<p class="hint"><CodeText text={verdict.hint} capitalize={false} /></p>{/if}
          </div>
        {:else if step === "create"}
          <p>Choose the passphrase every device will use to join. It never leaves this machine; only what it encrypts does.</p>
          <SyncPassphraseFields bind:value={passphrase} bind:confirm bind:generated bind:ready={passReady} {refused} savedOutside autofocus />
          {@render device()}
          {@render recovery()}
        {:else if step === "join"}
          <p>Enter the sync passphrase chosen on your first device. Nothing is written before you review what joining brings.</p>
          <label class="field">
            <span>Sync passphrase</span>
            <input data-autofocus type="password" bind:value={passphrase} autocomplete="current-password" spellcheck="false" />
          </label>
          {@render device()}
        {:else if step === "review" && preview}
          <div class="counts" data-testid="sync-preview">
            <div><span class="n">{preview.hosts}</span><span>hosts</span></div>
            <div><span class="n">{preview.host_meta}</span><span>tags, groups, notes</span></div>
            <div><span class="n">{preview.snippets}</span><span>snippets</span></div>
            <div><span class="n">{preview.settings}</span><span>settings</span></div>
            <div data-tone={preview.conflicts ? "warning" : undefined}><span class="n">{preview.conflicts}</span><span>conflicts</span></div>
          </div>
          <p class="note">
            From {preview.devices.map((d) => d.label).join(", ") || "no device yet"}.
            {#if preview.needs_newer}<span class="soft-warning">{preview.needs_newer} {preview.needs_newer === 1 ? "record needs" : "records need"} a newer Portico: kept untouched and passed on.</span>{/if}
          </p>
          {#if preview.rejoin}
            <span class="caption">Conflicts with what this device has</span>
            <div class="segmented" role="group" aria-label="Conflicts">
              {#each BULK as b (b.id)}
                <button aria-pressed={b.id === "review_each"} disabled={b.id !== "review_each"}>{b.label}</button>
              {/each}
            </div>
            <p class="note" data-testid="sync-rejoin-note">
              This device synced with this folder before: every local difference waits for your decision in Sync, Received. Nothing is
              settled in bulk.
            </p>
          {:else if preview.conflicts}
            <span class="caption">Conflicts with what this device has</span>
            <div class="segmented" role="group" aria-label="Conflicts">
              {#each BULK as b (b.id)}
                <button aria-pressed={bulk === b.id} onclick={() => (bulk = b.id)}>{b.label}</button>
              {/each}
            </div>
            <p class="note">{BULK.find((b) => b.id === bulk)?.help}</p>
          {/if}
          <p class="note">Received hosts are not used until you confirm each destination, whatever you choose here.</p>
          <p class="note validity" data-testid="sync-validity">{validityLeft(previewAt, now)} · single use</p>
          {@render recovery(true)}
        {/if}
      </div>
    {/key}
    {#if error}
      <div class="inline-error" role="alert"><span><CodeText text={error.message} /></span>{#if error.hint}<span class="hint"><CodeText text={error.hint} capitalize={false} /></span>{/if}</div>
    {/if}
  </div>

  {#snippet footer()}
    {#if busy}<span class="footer-note busy"><span class="spinner"></span>{busy}</span>{/if}
    {#if step === "intro"}
      <button class="secondary" onclick={close}>Not now <kbd>Esc</kbd></button>
      <button class="primary" data-autofocus disabled={!!busy} onclick={pick}>Choose a folder…</button>
    {:else if step === "folder"}
      <button class="secondary" disabled={!!busy} onclick={pick}>Choose another…</button>
      {#if verdict?.tone === "wait"}<button class="secondary" disabled={!!busy} onclick={checkAgain}>Check again</button>{/if}
      {#if verdict?.tone === "ok"}<button class="primary" data-autofocus onclick={() => ((error = null), (step = "create"))}>Create the sync here</button>{/if}
      {#if verdict?.tone === "join"}<button class="primary" data-autofocus onclick={() => ((error = null), (step = "join"))}>Join this sync</button>{/if}
    {:else if step === "create"}
      <button class="secondary" disabled={!!busy} onclick={() => ((step = "folder"), forget())}>Back</button>
      <button class="primary" disabled={!passReady || !nameOk || !saved || !!busy} onclick={create}>Create sync <kbd>↵</kbd></button>
    {:else if step === "join"}
      <button class="secondary" disabled={!!busy} onclick={() => ((step = "folder"), forget())}>Back</button>
      <button class="primary" disabled={!passphrase || !nameOk || !!busy} onclick={review}>Review <kbd>↵</kbd></button>
    {:else if step === "review"}
      <button class="secondary" disabled={!!busy} onclick={() => ((preview = null), (step = "join"))}>Back</button>
      <button class="primary" disabled={!saved || !!busy} onclick={join}>Join <kbd>↵</kbd></button>
    {/if}
  {/snippet}
</Modal>

{#snippet device()}
  <label class="field">
    <span>This device's name</span>
    <input bind:value={deviceName} spellcheck="false" maxlength="64" aria-invalid={!nameOk} />
    <span class="help">Shown to your other devices, with a short id.</span>
  </label>
  <label class="check"><input type="checkbox" bind:checked={rememberKey} /> Remember the key on this device</label>
  <p class="note">{rememberKey ? "Stored in the system keychain: no passphrase at each start." : "Portico asks for the passphrase at each start (shared machines)."}</p>
{/snippet}

{#snippet recovery(gates = false)}
  <div class="recovery" data-testid="sync-recovery">
    <strong>Keep the passphrase safe</strong>
    <p>
      Lose it and every device that does not remember the key cannot read the sync folder again: by design, nobody can recover it. A
      device that remembers the key can still set a new passphrase. Your local hosts, snippets and settings are never deleted.
    </p>
    <label class="check">
      <input type="checkbox" bind:checked={saved} data-autofocus={gates ? "" : undefined} data-testid="sync-saved" />
      I saved it. A lost passphrase cannot be recovered unless a device remembers the key.
    </label>
  </div>
{/snippet}

<style>
  .wizard { display: grid; gap: var(--space-3); }
  .step { display: grid; gap: var(--space-3); }
  .lead { color: var(--text-primary); font-size: var(--text-md); }
  .points { display: grid; gap: var(--space-2); margin: 0; padding-left: var(--space-4); }
  .points strong { color: var(--text-primary); font-weight: 550; }
  .caption { color: var(--text-muted); font-size: var(--text-xs); letter-spacing: var(--tracking-caps); text-transform: uppercase; }
  .folder { display: grid; gap: var(--space-1); }
  .path { color: var(--text-primary); font-family: var(--font-mono); font-size: var(--text-sm); word-break: break-all; }
  .verdict {
    --c: var(--border-strong);
    display: grid;
    gap: var(--space-1);
    padding: var(--space-3) var(--space-4);
    border-left: 2px solid var(--c);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  .verdict strong { color: var(--text-primary); font-weight: 550; }

  .verdict[data-tone="wait"] { --c: var(--status-warning); }
  .verdict[data-tone="error"] { --c: var(--status-error); }
  .verdict .hint { color: var(--text-muted); font-size: var(--text-sm); }
  .counts { display: grid; grid-template-columns: repeat(5, minmax(0, 1fr)); gap: var(--space-2); }
  .counts > div { display: grid; gap: var(--space-1); padding: var(--space-3); border-radius: var(--radius-md); background: var(--bg-elevated); }
  .counts span:not(.n) { color: var(--text-muted); font-size: var(--text-xs); }
  .counts .n { color: var(--text-primary); font-family: var(--font-display); font-size: var(--text-xl); font-weight: 600; font-variant-numeric: tabular-nums; }
  .counts > div[data-tone="warning"] .n { color: var(--status-warning); }
  .recovery {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border-left: 2px solid var(--status-warning);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  .recovery strong { color: var(--text-primary); font-weight: 550; }
  .busy { display: inline-flex; align-items: center; gap: var(--space-2); }
</style>
