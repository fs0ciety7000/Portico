<script lang="ts">
  // Keys view: local key pairs, then ssh-agent identities, then the vault.
  // Keys: j/k ↑↓ move · n new · a add to agent · x remove from agent ·
  // c copy to a host · y copy the public key · v / V vault · Ctrl-R refresh.
  // Only the lists scroll: the vault strip stays visible at any height.
  import { fly } from "svelte/transition";
  import { sentence, type AgentKey, type Certificate, type ErrorSummary, type LocalKey } from "../api";
  import { dur } from "../motion";
  import { algorithmLabel, keys, relative, shortDate } from "../state/keys.svelte";
  import { vault } from "../state/vault.svelte";
  import Modal from "./Modal.svelte";

  let listEl: HTMLDivElement | undefined = $state();
  let removeError: ErrorSummary | null = $state(null);
  let removing = $state(false);

  type Row = { kind: "local"; key: LocalKey } | { kind: "agent"; key: AgentKey };
  const current: Row | undefined = $derived.by(() => {
    const i = keys.selected;
    const local = keys.local[i];
    if (local) return { kind: "local", key: local };
    const agent = keys.agent[i - keys.local.length];
    return agent ? { kind: "agent", key: agent } : undefined;
  });

  $effect(() => {
    void keys.selected;
    listEl?.querySelector(".row.selected")?.scrollIntoView({ block: "nearest" });
  });

  function act(key: string): boolean {
    const row = current;
    switch (key) {
      case "n":
        keys.view = { kind: "new" };
        return true;
      case "a":
        if (row?.kind === "local" && row.key.hasPrivate && !keys.inAgent(row.key.fingerprint)) keys.view = { kind: "agent-add", key: row.key };
        return true;
      case "x":
        if (row?.kind === "agent") {
          removeError = null;
          keys.view = { kind: "agent-remove", key: row.key };
        } else if (row?.kind === "local") {
          const agentKey = keys.agent.find((a) => a.fingerprint === row.key.fingerprint);
          if (agentKey) {
            removeError = null;
            keys.view = { kind: "agent-remove", key: agentKey };
          }
        }
        return true;
      case "c":
        if (row?.kind === "local") keys.view = { kind: "copy-pick", key: row.key };
        return true;
      case "y":
        if (row?.kind === "local") void keys.copyPublic(row.key);
        return true;
      case "v":
        if (vault.status === "locked") vault.open("unlock", "keys");
        else if (vault.status === "unlocked") void vault.lock();
        else if (vault.status === "absent") vault.open("create", "keys");
        return vault.inUse;
      case "V":
        if (vault.status === "unlocked") vault.open("change", "keys");
        else if (vault.status === "absent") vault.open("create", "keys");
        return vault.inUse;
      default:
        return false;
    }
  }

  function onListKey(e: KeyboardEvent) {
    const last = Math.max(keys.rows - 1, 0);
    if (e.key === "ArrowDown" || (e.key === "j" && !e.ctrlKey)) keys.selected = Math.min(keys.selected + 1, last);
    else if (e.key === "ArrowUp" || (e.key === "k" && !e.ctrlKey)) keys.selected = Math.max(keys.selected - 1, 0);
    else if (e.key === "Home") keys.selected = 0;
    else if (e.key === "End") keys.selected = last;
    else if (e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "r") void keys.refresh();
    else return;
    e.preventDefault();
  }

  async function confirmRemove(key: AgentKey) {
    if (removing) return;
    removing = true;
    removeError = await keys.removeFromAgent(key);
    removing = false;
    if (!removeError) keys.view = { kind: "list" };
  }

  function certBadge(cert: Certificate): { tone: "ok" | "warning" | "error"; text: string } {
    const v = cert.validity;
    switch (v.state) {
      case "valid":
        return { tone: "ok", text: cert.validBefore === null ? "certificate · no expiry" : `certificate · until ${shortDate(cert.validBefore)}` };
      case "expires_soon":
        return { tone: "warning", text: `certificate expires in ${relative(v.expires_in_secs)}` };
      case "expired":
        return { tone: "error", text: "certificate expired" };
      case "not_yet_valid":
        return { tone: "warning", text: `certificate valid in ${relative(v.starts_in_secs)}` };
    }
  }

  const vaultLabel = $derived(
    vault.status === "locked" ? "Vault locked" : vault.status === "unlocked" ? "Vault unlocked" : "No vault yet",
  );
</script>

{#if keys.view.kind === "agent-remove"}
  {@const key = keys.view.key}
  <Modal onescape={() => (keys.view = { kind: "list" })} onkey={(k) => (k === "y" ? (void confirmRemove(key), true) : k === "n" ? ((keys.view = { kind: "list" }), true) : false)} labelledBy="agent-remove-title">
    {#snippet title()}Remove from ssh-agent?{/snippet}
    <p>The agent forgets this identity; the key file in <span class="mono">~/.ssh</span> stays.</p>
    <dl>
      <dt>Comment</dt><dd>{key.comment || "—"}</dd>
      <dt>Fingerprint</dt><dd class="fingerprint">{key.fingerprint}</dd>
    </dl>
    {#if removeError}
      <div class="inline-error" role="alert"><span>{sentence(removeError.message)}</span>{#if removeError.hint}<span class="hint">{removeError.hint}</span>{/if}</div>
    {/if}
    {#snippet footer()}
      <button class="secondary" data-autofocus onclick={() => (keys.view = { kind: "list" })}>Keep <kbd>n</kbd></button>
      <button class="primary" disabled={removing} onclick={() => confirmRemove(key)}>Remove <kbd>y</kbd></button>
    {/snippet}
  </Modal>
{:else}
  <Modal size="wide" scroll={false} footerWrap={false} onescape={() => keys.close()} onkey={act} labelledBy="keys-title">
    {#snippet title()}Keys<span class="title-host">· ~/.ssh and ssh-agent</span><span class="title-hint"><kbd>Ctrl-R</kbd> refresh</span>{/snippet}

    <div class="lists" bind:this={listEl} tabindex="0" role="listbox" aria-label="Keys" data-autofocus onkeydown={onListKey}>
      <section>
        <h3>Local keys {#if keys.localState === "ready"}<span class="count">{keys.local.length}</span>{/if}</h3>
        {#if keys.localState === "loading"}
          {#each [0, 1, 2] as i (i)}<div class="skeleton" style:animation-delay="calc({i} * var(--motion-fast))"></div>{/each}
        {:else if keys.localState === "error" && keys.localError}
          <div class="state error"><strong>{sentence(keys.localError.message)}</strong>{#if keys.localError.hint}<span>{keys.localError.hint}</span>{/if}</div>
        {:else if !keys.local.length}
          <div class="state"><strong>No keys in ~/.ssh yet</strong><span>Create one with <kbd>n</kbd>: Ed25519 is a good default.</span></div>
        {:else}
          {#each keys.local as key, i (key.id)}
            {@const cert = key.certificate ? certBadge(key.certificate) : null}
            <div
              class="row"
              class:selected={keys.selected === i}
              role="option"
              aria-selected={keys.selected === i}
              tabindex="-1"
              onclick={() => (keys.selected = i)}
              onkeydown={() => {}}
              in:fly={{ y: 6, duration: dur("base") }}
            >
              <div class="line">
                <span class="name mono">{key.id}</span>
                <span class="algo">{algorithmLabel(key.algorithm)}</span>
                {#if keys.inAgent(key.fingerprint)}<span class="badge agent">in agent</span>{/if}
                {#if !key.hasPrivate}<span class="badge">public only</span>{/if}
                {#if cert}<span class="badge" data-tone={cert.tone}>{cert.text}</span>{/if}
              </div>
              <div class="line sub">
                <span class="fp mono">{key.fingerprint}</span>
                {#if key.comment}<span class="comment">{key.comment}</span>{/if}
              </div>
            </div>
          {/each}
        {/if}
      </section>

      <section>
        <h3>ssh-agent {#if keys.agentState === "ready"}<span class="count">{keys.agent.length}</span>{/if}</h3>
        {#if keys.agentState === "loading"}
          <div class="skeleton"></div>
        {:else if keys.agentState === "error" && keys.agentError}
          <div class="state warning"><strong>{sentence(keys.agentError.message)}</strong>{#if keys.agentError.hint}<span>{keys.agentError.hint}</span>{/if}</div>
        {:else if !keys.agent.length}
          <div class="state"><strong>The agent holds no identity</strong><span>Select a local key and press <kbd>a</kbd> to add it.</span></div>
        {:else}
          {#each keys.agent as key, j (key.fingerprint + (key.certificate ? ":cert" : ""))}
            {@const i = keys.local.length + j}
            {@const cert = key.certificate ? certBadge(key.certificate) : null}
            <div class="row" class:selected={keys.selected === i} role="option" aria-selected={keys.selected === i} tabindex="-1" onclick={() => (keys.selected = i)} onkeydown={() => {}}>
              <div class="line">
                <span class="name">{key.comment || "No comment"}</span>
                <span class="algo">{algorithmLabel(key.algorithm)}</span>
                {#if cert}<span class="badge" data-tone={cert.tone}>{cert.text}</span>{/if}
              </div>
              <div class="line sub"><span class="fp mono">{key.fingerprint}</span></div>
            </div>
          {/each}
        {/if}
      </section>
    </div>

    {#if keys.publicLine && current?.kind === "local" && current.key.id === keys.publicLine.id}
      <div class="public-line">
        <span class="note">Public key of <span class="mono">{keys.publicLine.id}</span>: select the line and copy it.</span>
        <pre class="mono" tabindex="-1">{keys.publicLine.text}</pre>
      </div>
    {/if}

    {#if vault.inUse}
      <div class="vault" data-status={vault.status}>
        <span class="dot"></span>
        <span class="vault-label">{vaultLabel}</span>
        <span class="vault-sub">
          {#if vault.status === "absent"}Remembered passwords and passphrases need an encrypted vault (no system keyring here).
          {:else if vault.status === "locked"}Remembered secrets are unavailable until you unlock it.
          {:else}Remembered secrets are available.{/if}
        </span>
        <span class="vault-actions">
          {#if vault.status === "locked"}<button class="secondary small" onclick={() => act("v")}>Unlock <kbd>v</kbd></button>
          {:else if vault.status === "unlocked"}
            <button class="secondary small" onclick={() => act("v")}>Lock <kbd>v</kbd></button>
            <button class="secondary small" onclick={() => act("V")}>Change passphrase <kbd>V</kbd></button>
          {:else}<button class="secondary small" onclick={() => act("V")}>Create vault <kbd>V</kbd></button>{/if}
        </span>
      </div>
    {/if}

    {#snippet footer()}
      {#if current?.kind === "local"}
        <button class="secondary small" onclick={() => act("y")}>Copy key <kbd>y</kbd></button>
        <button class="secondary small" onclick={() => act("c")}>Copy to host… <kbd>c</kbd></button>
        {#if keys.inAgent(current.key.fingerprint)}
          <button class="secondary small" onclick={() => act("x")}>Remove from agent <kbd>x</kbd></button>
        {:else if current.key.hasPrivate}
          <button class="secondary small" disabled={keys.agentState === "error"} onclick={() => act("a")}>Add to agent <kbd>a</kbd></button>
        {/if}
      {:else if current?.kind === "agent"}
        <button class="secondary small" onclick={() => act("x")}>Remove from agent <kbd>x</kbd></button>
      {/if}
      <button class="primary" onclick={() => act("n")}>New key <kbd>n</kbd></button>
    {/snippet}
  </Modal>
{/if}

<style>
  /* The lists scroll; the public key line and the vault stay in view. */
  .lists {
    display: grid;
    align-content: start;
    gap: var(--space-4);
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    outline: none;
    border-radius: var(--radius-md);
  }
  /* Focus shows on the selected row, not around the whole list. */
  .lists:focus-visible { outline: none; }
  .lists:focus-visible .row.selected { box-shadow: inset 0 0 0 var(--size-bar) var(--accent); }
  .title-hint {
    float: right;
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: 450;
    line-height: normal;
    margin-top: var(--space-1);
  }
  .public-line { flex: none; display: grid; gap: var(--space-1); }
  .public-line pre {
    margin: 0;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font-size: var(--text-xs);
    white-space: nowrap;
    overflow-x: auto;
    user-select: all;
    scrollbar-width: thin;
  }
  section { display: grid; gap: var(--space-1); }
  h3 {
    margin: 0 0 var(--space-1);
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: 550;
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .count { margin-left: var(--space-1); color: var(--text-muted); font-weight: 450; }
  .row {
    position: relative;
    display: grid;
    gap: var(--size-line-gap);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    cursor: default;
    transition: background var(--motion-fast) var(--ease-standard);
  }
  .row:hover { background: var(--bg-elevated); }
  .row.selected { background: var(--bg-selected); }
  .row.selected::before {
    content: "";
    position: absolute;
    left: 0;
    top: var(--space-2);
    bottom: var(--space-2);
    width: var(--size-bar);
    border-radius: var(--radius-pill);
    background: var(--accent);
  }
  .line { display: flex; align-items: center; gap: var(--space-2); min-width: 0; }
  .name { color: var(--text-primary); font-weight: 550; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .name.mono { font-size: var(--text-sm); }
  .algo { color: var(--text-muted); font-size: var(--text-xs); letter-spacing: 0.06em; }
  .sub { color: var(--text-muted); font-size: var(--text-xs); }
  .fp { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; color: var(--text-secondary); }
  .comment { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .comment::before { content: "·"; margin-right: var(--space-2); }
  .badge {
    --c: var(--text-muted);
    display: inline-flex;
    align-items: center;
    height: var(--space-5);
    padding: 0 var(--space-2);
    border: 1px solid color-mix(in srgb, var(--c) 45%, transparent);
    border-radius: var(--radius-pill);
    color: var(--c);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .badge.agent { --c: var(--text-secondary); }
  .badge[data-tone="ok"] { --c: var(--text-secondary); }
  .badge[data-tone="warning"] { --c: var(--status-warning); }
  .badge[data-tone="error"] { --c: var(--status-error); }
  .state { display: grid; gap: var(--space-1); padding: var(--space-2) var(--space-3); color: var(--text-secondary); font-size: var(--text-sm); }
  .state strong { color: var(--text-primary); font-weight: 550; font-size: var(--text-base); }
  .state.error strong { color: var(--status-error); }
  .state.warning strong { color: var(--status-warning); }
  .skeleton {
    height: 44px;
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    animation: shimmer var(--motion-loop) var(--ease-standard) infinite;
  }
  @keyframes shimmer { 50% { opacity: 0.45; } }
  .vault {
    --c: var(--status-idle);
    display: grid;
    flex: none;
    grid-template-columns: var(--space-2) auto minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-2) var(--space-3);
    margin-top: var(--space-2);
    padding: var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
  }
  .vault[data-status="unlocked"] { --c: var(--status-connected); }
  .vault[data-status="locked"] { --c: var(--status-warning); }
  .vault .dot { width: var(--space-2); height: var(--space-2); border-radius: 50%; background: var(--c); }
  .vault-label { color: var(--text-primary); font-weight: 550; }
  .vault-sub { color: var(--text-muted); font-size: var(--text-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .vault-actions { display: flex; gap: var(--space-2); }
  @media (max-width: 900px) {
    .vault { grid-template-columns: var(--space-2) minmax(0, 1fr) auto; }
    .vault-sub { display: none; }
  }
</style>
