<script lang="ts">
  // "Sync…" when sync is off (phase 11, docs/SYNC.md §6): what it does, the
  // folder (native picker, in Rust), then create (first device) or join
  // (review before anything is written), and a last recovery warning to
  // acknowledge. Passphrase fields are cleared as soon as they are sent.
  import { onMount } from "svelte";
  import { fly } from "svelte/transition";
  import { asError, sentence, type ErrorSummary, type GeneratedPassphrase, type JoinBulkChoice, type JoinPreview, type PickedFolder } from "../api";
  import { backend } from "../backend";
  import { dur } from "../motion";
  import { JOIN_TTL_MS, probeVerdict } from "../syncview";
  import { sync } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";
  import SyncPassphraseFields, { newPassphrase } from "./SyncPassphraseFields.svelte";

  type Step = "intro" | "folder" | "create" | "join" | "review" | "done";

  let step: Step = $state("intro");
  let armed = $state(false);
  let busy: string | null = $state(null);
  let error: ErrorSummary | null = $state(null);
  let picked = $state<PickedFolder | null>(null);
  // Passphrases: cleared once sent.
  let passphrase = $state("");
  let confirm = $state("");
  let generated = $state<GeneratedPassphrase | null>(null);
  let wroteDown = $state(false);
  let passReady = $state(false);
  let deviceName = $state("");
  let rememberKey = $state(true);
  let preview = $state<JoinPreview | null>(null);
  let previewAt = 0;
  let bulk: JoinBulkChoice = $state("review_each");
  let understood = $state(false);
  let joined = $state(false);

  onMount(() => {
    backend.syncDefaultDeviceName().then(
      (name) => (deviceName ||= name),
      () => {},
    );
  });

  const verdict = $derived(picked ? probeVerdict(picked.probe) : null);
  const nameOk = $derived(deviceName.trim().length > 0);

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
    if (!passReady || !nameOk || busy) return;
    const secret = newPassphrase(generated, passphrase);
    forget();
    busy = "Deriving the key… (a few seconds)";
    error = null;
    try {
      const status = await backend.syncCreate(secret, deviceName.trim(), rememberKey);
      generated = null;
      await sync.act(async () => status);
      joined = false;
      step = "done";
    } catch (e) {
      error = asError(e);
      // A generated passphrase serves once: generate it again.
      if (generated) {
        generated = null;
        wroteDown = false;
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
      bulk = "review_each";
      step = "review";
    } catch (e) {
      error = asError(e);
    } finally {
      busy = null;
    }
  }

  async function join() {
    if (!preview || busy) return;
    if (Date.now() - previewAt > JOIN_TTL_MS) return expired();
    busy = "Joining…";
    error = null;
    try {
      const status = await backend.syncJoinCommit(preview.token, bulk);
      preview = null;
      await sync.act(async () => status);
      joined = true;
      step = "done";
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

  function finish() {
    if (!understood) return;
    sync.close();
    workspace.notify({ code: "sync_on", message: "Sync is on", hint: joined ? "Received hosts wait for your confirmation in Sync, Received." : null }, "info");
    if (joined && sync.received) sync.open({ kind: "panel", tab: "received" });
  }

  function close() {
    forget();
    if (step === "done") return finish();
    sync.close();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key !== "Enter" || e.target instanceof HTMLButtonElement || !armed) return;
    e.preventDefault();
    if (step === "create") void create();
    else if (step === "join") void review();
    else if (step === "review") void join();
    else if (step === "done") finish();
  }

  const titles: Record<Step, string> = {
    intro: "Sync with your other devices",
    folder: "Sync folder",
    create: "Create the sync",
    join: "Join the sync",
    review: "What joining brings",
    done: "Before you go",
  };
  const BULK: { id: JoinBulkChoice; label: string; help: string }[] = [
    { id: "review_each", label: "Review each", help: "Every conflict waits for you in Sync, Conflicts." },
    { id: "prefer_remote", label: "Prefer the folder's", help: "Conflicts take the other devices' values." },
    { id: "prefer_local", label: "Prefer this device's", help: "Conflicts keep what this device has." },
  ];
</script>

<Modal bind:armed onescape={close} labelledBy="sync-wizard-title" size="wide">
  {#snippet title()}{titles[step]}{/snippet}

  <div class="wizard" role="presentation" {onkeydown}>
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
            <strong>{sentence(verdict.title)}</strong>
            {#if verdict.body}<p>{verdict.body}</p>{/if}
            {#if verdict.hint}<p class="hint">{verdict.hint}</p>{/if}
          </div>
        {:else if step === "create"}
          <p>Choose the passphrase every device will use to join. It never leaves this machine; only what it encrypts does.</p>
          <SyncPassphraseFields bind:value={passphrase} bind:confirm bind:generated bind:wroteDown bind:ready={passReady} autofocus />
          {@render device()}
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
          {#if preview.conflicts}
            <span class="caption">Conflicts with what this device has</span>
            <div class="segmented" role="group" aria-label="Conflicts">
              {#each BULK as b (b.id)}
                <button aria-pressed={bulk === b.id} onclick={() => (bulk = b.id)}>{b.label}</button>
              {/each}
            </div>
            <p class="note">{BULK.find((b) => b.id === bulk)?.help}</p>
          {/if}
          <p class="note">Received hosts stay off ~/.ssh until you confirm each destination, whatever you choose here.</p>
        {:else if step === "done"}
          <div class="recovery" data-testid="sync-recovery">
            <strong>Keep the passphrase safe</strong>
            <p>
              Lose it and every device that does not remember the key cannot read the sync folder again: by design, nobody can
              recover it. A device that remembers the key can still set a new passphrase.
            </p>
            <p>Your local hosts, snippets and settings are never deleted, even when you turn sync off.</p>
          </div>
          <label class="check"><input type="checkbox" bind:checked={understood} data-autofocus /> I understand</label>
        {/if}
      </div>
    {/key}
    {#if error}
      <div class="inline-error" role="alert"><span>{sentence(error.message)}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}</div>
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
      <button class="primary" disabled={!passReady || !nameOk || !!busy} onclick={create}>Create sync <kbd>↵</kbd></button>
    {:else if step === "join"}
      <button class="secondary" disabled={!!busy} onclick={() => ((step = "folder"), forget())}>Back</button>
      <button class="primary" disabled={!passphrase || !nameOk || !!busy} onclick={review}>Review <kbd>↵</kbd></button>
    {:else if step === "review"}
      <button class="secondary" disabled={!!busy} onclick={() => ((preview = null), (step = "join"))}>Back</button>
      <button class="primary" data-autofocus disabled={!!busy} onclick={join}>Join <kbd>↵</kbd></button>
    {:else}
      <button class="primary" disabled={!understood} onclick={finish}>Done <kbd>↵</kbd></button>
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
  .verdict[data-tone="ok"], .verdict[data-tone="join"] { --c: var(--accent); }
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
