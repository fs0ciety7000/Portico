<script lang="ts">
  // Engine prompts: new host key (TOFU), password, key passphrase,
  // keyboard-interactive. Secrets live in this component only until they are
  // sent, then the fields are cleared.
  import { displayPath, sendsRememberedPassword, type Prompt, type PromptAnswer } from "../api";
  import { sanitizeRemote } from "../keys";
  import { hosts } from "../state/hosts.svelte";
  import { vault } from "../state/vault.svelte";
  import Caution from "./Caution.svelte";
  import Modal from "./Modal.svelte";

  let {
    prompt,
    alias,
    context = null,
    onanswer,
  }: {
    prompt: Prompt;
    alias: string | undefined;
    /** What asked for it when it is not a terminal (`Copy id_ed25519 to web-01 · Verifying`). */
    context?: string | null;
    onanswer: (a: PromptAnswer) => void;
  } = $props();

  let armed = $state(false);
  let armedAt = $state(0);
  /** A bare `y` counts only well after arming (no "yesterday" typed on). */
  const Y_DELAY = 150;
  let secret = $state("");
  let remember = $state(false);
  let confirm = $state("");
  // Keyboard-interactive answers, one per prompt.
  let answers: string[] = $state([]);

  const kind = $derived(prompt.kind);
  const unusual = $derived(kind.kind === "unknown_host_key" && kind.other_algorithms.length > 0);

  function reply(answer: PromptAnswer) {
    if (!armed) return;
    onanswer(answer);
    secret = "";
    answers = [];
    confirm = "";
  }

  const cancel = () => reply({ kind: "cancel" });

  function submit() {
    switch (kind.kind) {
      case "unknown_host_key":
        if (!unusual || confirm.trim().toLowerCase() === "yes") reply({ kind: "accept_host_key" });
        return;
      case "password":
      case "key_passphrase":
        reply({ kind: "secret", value: secret, remember });
        return;
      case "keyboard_interactive":
        reply({ kind: "answers", values: kind.prompts.map((_, i) => answers[i] ?? "") });
        return;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      // Host keys: Enter never trusts a key unless it was typed "yes".
      if (kind.kind !== "unknown_host_key" || unusual) submit();
      return;
    }
    if (kind.kind === "unknown_host_key" && !unusual && !e.metaKey && !e.ctrlKey) {
      if (e.key === "y" && performance.now() - armedAt >= Y_DELAY) submit();
      else if (e.key === "n") cancel();
    }
  }

  /** `host:port` in prose (the bracket form is for known_hosts only). */
  const target = $derived(kind.kind === "unknown_host_key" ? `${kind.host}:${kind.port}` : "");
  /** The account whose remembered password the key would unlock: the user
   * of the host this prompt is for, when it answers at that address. */
  const account = $derived.by(() => {
    if (kind.kind !== "unknown_host_key") return null;
    const own = alias ? hosts.byAlias[alias] : undefined;
    const match = own && own.hostname === kind.host && own.port === kind.port ? own : hosts.all.find((h) => h.hostname === kind.host && h.port === kind.port);
    return match?.user ?? null;
  });
</script>

<Modal bind:armed bind:armedAt onescape={cancel}>
  {#snippet title()}
    {#if kind.kind === "unknown_host_key"}{unusual ? "Unusual host key" : "New host key"}
    {:else if kind.kind === "password"}Password
    {:else if kind.kind === "key_passphrase"}Key passphrase
    {:else}Authentication{/if}
    {#if alias}<span class="title-host">· {alias}</span>{/if}
  {/snippet}

  <div class="content" role="presentation" {onkeydown}>
    {#if context}<p class="context">{context}</p>{/if}
    {#if kind.kind === "unknown_host_key"}
      <p class="lead">First connection to <span class="mono">{target}</span>.</p>
      <p>Verify this fingerprint with the server's administrator before trusting it.</p>
      <dl>
        <dt>Key type</dt><dd>{kind.algorithm}</dd>
        <dt>Fingerprint</dt><dd class="fingerprint">{kind.fingerprint}</dd>
      </dl>
      {#if kind.sends_remembered_password}
        <Caution testid="sends-password">{sendsRememberedPassword(account, kind.host, kind.port)}</Caution>
      {/if}
      {#if unusual}
        <p class="warning">
          This host is already known with another key type ({kind.other_algorithms.join(", ")}). This is unusual.
        </p>
        <label class="field">
          <span>Type <strong>yes</strong> to trust it</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input data-autofocus bind:value={confirm} autocomplete="off" spellcheck="false" />
        </label>
      {:else}
        <button class="focus-sink" data-autofocus aria-label="Host key decision" tabindex="-1"></button>
      {/if}
    {:else if kind.kind === "password" || kind.kind === "key_passphrase"}
      <p class="lead">
        {#if kind.kind === "password"}Sign in as <span class="mono">{kind.user}</span> on
          <span class="mono">{kind.host}</span>.
        {:else}Unlock <span class="mono">{displayPath(kind.key_path)}</span>.{/if}
      </p>
      {#if kind.attempt > 1}
        <p class="retry" role="alert">
          Incorrect {kind.kind === "password" ? "password" : "passphrase"}, try again ({kind.attempt}/{kind.attempts}).
        </p>
      {/if}
      <label class="field">
        <span>{kind.kind === "password" ? "Password" : "Passphrase"}</span>
        <input data-autofocus type="password" bind:value={secret} autocomplete="off" spellcheck="false" />
      </label>
      <label class="check">
        <input type="checkbox" bind:checked={remember} />
        <span>{vault.inUse ? "Remember in Portico's encrypted vault" : "Remember in the system keyring"}</span>
      </label>
      {#if remember && vault.status === "absent"}
        <p class="remember-note">There is no vault yet: Portico will offer to create one, and remembers secrets from then on.</p>
      {:else if remember && vault.status === "locked"}
        <p class="remember-note">The vault is locked: Portico will offer to unlock it so this can be remembered next time.</p>
      {/if}
    {:else}
      {@const message = [kind.name, kind.instructions].map((t) => sanitizeRemote(t, 512, true)).filter(Boolean)}
      {#if message.length}
        <!-- Text written by the server, not by Portico. -->
        <div class="server">
          <span class="server-label">Message from {alias ?? "the server"}</span>
          {#each message as text, i (i)}<p>{text}</p>{/each}
        </div>
      {/if}
      {#each kind.prompts as p, i (i)}
        <label class="field">
          <span>{sanitizeRemote(p.text, 128) || `Answer ${i + 1}`}</span>
          <input
            data-autofocus={i === 0 ? "" : undefined}
            type={p.echo ? "text" : "password"}
            bind:value={answers[i]}
            autocomplete="off"
            spellcheck="false"
          />
        </label>
      {/each}
    {/if}
  </div>

  {#snippet footer()}
    {#if kind.kind === "unknown_host_key" && !unusual}
      <button class="secondary" disabled={!armed} onclick={cancel}>Refuse <kbd>N</kbd></button>
      <button class="primary" disabled={!armed} onclick={submit}>Trust &amp; remember <kbd>Y</kbd></button>
    {:else if kind.kind === "unknown_host_key"}
      <button class="secondary" disabled={!armed} onclick={cancel}>Refuse <kbd>Esc</kbd></button>
      <button class="primary" disabled={!armed || confirm.trim().toLowerCase() !== "yes"} onclick={submit}>
        Trust <kbd>↵</kbd>
      </button>
    {:else}
      <button class="secondary" disabled={!armed} onclick={cancel}>Cancel <kbd>Esc</kbd></button>
      <button class="primary" disabled={!armed} onclick={submit}>Continue <kbd>↵</kbd></button>
    {/if}
  {/snippet}
</Modal>

<style>
  .content { display: grid; gap: var(--space-3); }
  .lead { color: var(--text-primary); }
  .context {
    margin-top: calc(-1 * var(--space-2));
    color: var(--text-muted);
    font-size: var(--text-sm);
  }
  .fingerprint { font-weight: 600; letter-spacing: 0.01em; }
  .warning { color: var(--status-warning); }
  .retry { color: var(--status-error); font-size: var(--text-sm); }
  .field { display: grid; gap: var(--space-1); }
  .field span { color: var(--text-muted); font-size: var(--text-sm); }
  .field input {
    height: var(--size-row);
    padding: 0 var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font: inherit;
    font-family: var(--font-mono);
    outline: none;
    transition: border-color var(--motion-fast) var(--ease-standard);
  }
  .field input:focus { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .server {
    display: grid;
    gap: var(--space-1);
    padding: var(--space-3) var(--space-4);
    border-left: var(--size-bar) solid var(--border-strong);

    background: var(--bg-elevated);
    border-radius: var(--radius-sm);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
  }
  .server-label { color: var(--text-muted); font-size: var(--text-xs); letter-spacing: 0.04em; }
  .remember-note { color: var(--text-muted); font-size: var(--text-xs); }
  .focus-sink {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0 !important;
    border: 0;
    opacity: 0;
  }
</style>
