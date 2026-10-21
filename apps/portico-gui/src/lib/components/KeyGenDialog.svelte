<script lang="ts">
  // New key pair in ~/.ssh. The name is validated by portico-core (a name,
  // never a path); the passphrase is sent once, as a SecretString on the
  // Rust side, and the fields are cleared right after.
  import { onMount } from "svelte";
  import { asError, sentence, type ErrorSummary, type KeyAlgorithm } from "../api";
  import { backend } from "../backend";
  import { keys } from "../state/keys.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";

  type Size = { label: string; algorithm: KeyAlgorithm };
  type Family = { label: string; base: string; sizes: Size[] };
  const FAMILIES: Family[] = [
    { label: "Ed25519", base: "id_ed25519", sizes: [{ label: "", algorithm: { kind: "ed25519" } }] },
    {
      label: "RSA",
      base: "id_rsa",
      sizes: [
        { label: "3072 bits", algorithm: { kind: "rsa", bits: 3072 } },
        { label: "4096 bits", algorithm: { kind: "rsa", bits: 4096 } },
      ],
    },
    {
      label: "ECDSA",
      base: "id_ecdsa",
      sizes: [
        { label: "P-256", algorithm: { kind: "ecdsa", curve: "p256" } },
        { label: "P-384", algorithm: { kind: "ecdsa", curve: "p384" } },
        { label: "P-521", algorithm: { kind: "ecdsa", curve: "p521" } },
      ],
    },
  ];

  let armed = $state(false);
  let family = $state(0);
  let size = $state(0);
  let name = $state("");
  let nameEdited = $state(false);
  let comment = $state("");
  let passphrase = $state("");
  let confirm = $state("");
  let busy = $state(false);
  let error: ErrorSummary | null = $state(null);
  /** A passphrase was sent, refused with the rest of the form, and cleared:
   * never create the key without one unless the user says so. */
  let passphraseCleared = $state(false);
  let withoutPassphrase = $state(false);

  /** First free name: id_ed25519, id_ed25519_2, … */
  function freeName(base: string): string {
    const taken = new Set(keys.local.map((k) => k.id));
    if (!taken.has(base)) return base;
    for (let n = 2; ; n++) if (!taken.has(`${base}_${n}`)) return `${base}_${n}`;
  }

  const fam = $derived(FAMILIES[family] ?? FAMILIES[0]);
  const current = $derived(fam?.sizes[size] ?? fam?.sizes[0]);
  $effect(() => {
    const base = fam?.base ?? "id_ed25519";
    if (!nameEdited) name = freeName(base);
  });

  onMount(() => {
    backend.keyDefaults().then((d) => (comment ||= d.comment), () => {});
  });

  /** Shortest non-empty passphrase (same rule as the TUI); empty = none. */
  const MIN_PASSPHRASE = 8;
  const passphraseChars = $derived([...passphrase].length);
  const tooShort = $derived(passphraseChars > 0 && passphraseChars < MIN_PASSPHRASE);
  const mismatch = $derived(confirm.length > 0 && passphrase !== confirm);
  const taken = $derived(keys.local.some((k) => k.id === name.trim()));
  const passphraseDecided = $derived(!passphraseCleared || passphrase.length > 0 || withoutPassphrase);
  const canCreate = $derived(
    armed && !busy && name.trim().length > 0 && !mismatch && !tooShort && passphrase === confirm && !taken && passphraseDecided,
  );
  const slow = $derived(current?.algorithm.kind === "rsa");

  async function create() {
    if (!canCreate || !current) return;
    busy = true;
    error = null;
    const secret = passphrase;
    passphrase = "";
    confirm = "";
    try {
      const key = await backend.generateKey({ name: name.trim(), algorithm: current.algorithm, comment: comment.trim(), passphrase: secret || null });
      await keys.refresh();
      keys.select(key.id);
      keys.view = { kind: "list" };
      workspace.notify({ code: "key_created", message: `Created ${key.id}`, hint: `${key.privateDisplay} and ${key.publicDisplay}` }, "info");
    } catch (e) {
      error = asError(e);
      if (secret) {
        passphraseCleared = true;
        withoutPassphrase = false;
      }
    } finally {
      busy = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      // Also from the segmented choices: Enter submits, Space picks.
      e.preventDefault();
      void create();
    }
  }
</script>

<Modal bind:armed onescape={() => (keys.view = { kind: "list" })} labelledBy="keygen-title">
  {#snippet title()}New key{/snippet}

  <div class="form" role="presentation" {onkeydown}>
    <div class="field">
      <span>Algorithm</span>
      <div class="choices">
        <div class="segmented" role="group" aria-label="Algorithm">
          {#each FAMILIES as f, i (f.label)}
            <button type="button" aria-pressed={family === i} onclick={() => ((family = i), (size = 0))}>{f.label}</button>
          {/each}
        </div>
        {#if fam && fam.sizes.length > 1}
          <div class="segmented" role="group" aria-label="Key size">
            {#each fam.sizes as z, i (z.label)}
              <button type="button" aria-pressed={size === i} onclick={() => (size = i)}>{z.label}</button>
            {/each}
          </div>
        {/if}
      </div>
      <span class="help">Ed25519 is short, fast and widely supported. RSA and ECDSA for older servers.</span>
    </div>

    <label class="field">
      <span>File name in ~/.ssh</span>
      <input
        class="mono"
        data-autofocus
        bind:value={name}
        oninput={() => (nameEdited = true)}
        spellcheck="false"
        autocomplete="off"
        aria-invalid={taken || error?.code === "key_name" || error?.code === "key_exists"}
      />
      {#if taken}<span class="help warn">A key with this name exists: Portico never overwrites a key.</span>{/if}
    </label>

    <label class="field">
      <span>Comment</span>
      <input class="mono" bind:value={comment} spellcheck="false" autocomplete="off" aria-invalid={error?.code === "key_comment"} />
    </label>

    <div class="pair">
      <label class="field">
        <span>Passphrase</span>
        <input type="password" bind:value={passphrase} autocomplete="new-password" spellcheck="false" aria-describedby="keygen-passphrase-rule" />
        <span class="help rule" id="keygen-passphrase-rule">
          <span>At least {MIN_PASSPHRASE} characters</span>
          <!-- Capped: never tells the length beyond the minimum. -->
          <span class="count" class:met={passphraseChars >= MIN_PASSPHRASE}>{Math.min(passphraseChars, MIN_PASSPHRASE)} / {MIN_PASSPHRASE} min</span>
        </span>
      </label>
      <label class="field">
        <span>Confirm passphrase</span>
        <input type="password" bind:value={confirm} autocomplete="new-password" spellcheck="false" aria-invalid={mismatch} />
      </label>
    </div>
    {#if mismatch}
      <span class="inline-error" role="alert">The passphrases differ.</span>
    {:else if passphraseCleared && !passphrase}
      <div class="cleared" role="status">
        <span class="soft-warning">Type the passphrase again: it was cleared when the key was refused.</span>
        <label class="check">
          <input type="checkbox" bind:checked={withoutPassphrase} />
          <span>Create it without a passphrase</span>
        </label>
      </div>
    {:else if !passphrase}
      <span class="soft-warning">Without a passphrase, anyone who copies this file can use the key.</span>
    {/if}

    {#if error}
      <div class="inline-error" role="alert"><span>{sentence(error.message)}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}</div>
    {/if}
  </div>

  {#snippet footer()}
    {#if busy}<span class="footer-note busy"><span class="spinner"></span>{slow ? "Generating, RSA takes a few seconds…" : "Generating…"}</span>{/if}
    <button class="secondary" disabled={busy} onclick={() => (keys.view = { kind: "list" })}>Cancel <kbd>Esc</kbd></button>
    <button class="primary" disabled={!canCreate} onclick={create}>Create key <kbd>↵</kbd></button>
  {/snippet}
</Modal>

<style>
  .form { display: grid; gap: var(--space-3); }
  .choices { display: flex; flex-wrap: wrap; gap: var(--space-2); }
  .pair { display: grid; grid-template-columns: 1fr 1fr; gap: var(--space-3); }
  .warn { color: var(--status-warning); }
  .rule { display: flex; justify-content: space-between; gap: var(--space-2); }
  .count { font-variant-numeric: tabular-nums; }
  .count.met { color: var(--text-secondary); }
  .cleared { display: grid; gap: var(--space-2); }
  .busy { display: inline-flex; align-items: center; gap: var(--space-2); }
</style>
