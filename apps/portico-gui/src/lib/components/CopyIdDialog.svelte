<script lang="ts">
  // ssh-copy-id: pick a host, then Connecting · Installing · Verifying.
  // The flow itself lives in `keys` (it survives the prompts that replace
  // this dialog for a moment). Portico never edits ~/.ssh/config: the
  // IdentityFile line is offered as text to copy.
  import { fly } from "svelte/transition";
  import { describe, sentence, type Host, type LocalKey } from "../api";
  import { backend } from "../backend";
  import { COPIED_FEEDBACK_MS, dur } from "../motion";
  import { COPY_STEP_LABEL, keys, type CopyStep } from "../state/keys.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";

  let { pick }: { pick: LocalKey | null } = $props();

  // ── Host picker ──
  let query = $state("");
  let hosts: Host[] = $state([]);
  let cursor = $state(0);
  let listEl: HTMLUListElement | undefined = $state();

  $effect(() => {
    if (!pick) return;
    const q = query;
    backend.searchHosts(q).then((r) => {
      if (q === query) {
        hosts = r;
        cursor = 0;
      }
    }, (e) => workspace.notify(e));
  });

  $effect(() => {
    void cursor;
    listEl?.children[cursor]?.scrollIntoView({ block: "nearest" });
  });

  function start(host: Host | undefined) {
    if (pick && host) void keys.startCopy(pick, host.alias);
  }

  function onSearchKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown") cursor = Math.min(cursor + 1, hosts.length - 1);
    else if (e.key === "ArrowUp") cursor = Math.max(cursor - 1, 0);
    else if (e.key === "Enter") start(hosts[cursor]);
    else return;
    e.preventDefault();
  }

  // ── Progress ──
  const run = $derived(keys.copy);
  const STEPS: { step: CopyStep; label: string }[] = (["connecting", "installing", "verifying"] as const).map((step) => ({
    step,
    label: COPY_STEP_LABEL[step],
  }));
  const ORDER: Record<CopyStep, number> = { connecting: 0, installing: 1, verifying: 2 };

  function stateOf(step: CopyStep): "done" | "active" | "failed" | "pending" | "skipped" {
    const r = run;
    if (!r) return "pending";
    if (step === "verifying" && !r.key.hasPrivate) return r.step === "done" ? "skipped" : "pending";
    if (r.step === "done") return "done";
    if (r.step === "failed") {
      const at = r.failedAt ?? "connecting";
      return step === at ? "failed" : ORDER[step] < ORDER[at] ? "done" : "pending";
    }
    const now = ORDER[r.step];
    return ORDER[step] < now ? "done" : ORDER[step] === now ? "active" : "pending";
  }

  // The footer changes with the outcome (Hide → Done / Retry): its new
  // default button takes the focus the removed one had.
  $effect(() => {
    const step = run?.step;
    if (step === "done" || step === "failed") {
      queueMicrotask(() => document.querySelector<HTMLElement>(".modal footer [data-autofocus]")?.focus());
    }
  });

  const identity = $derived(run ? `Host ${run.alias}\n    IdentityFile ${run.key.privateDisplay}` : "");
  let copied = $state(false);
  async function copyIdentity() {
    try {
      await navigator.clipboard.writeText(identity);
      copied = true;
      setTimeout(() => (copied = false), COPIED_FEEDBACK_MS);
    } catch {
      // The text stays selectable.
    }
  }

  /** Retrying makes sense unless the host key changed (nothing to retry). */
  const canRetry = $derived(run?.step === "failed" && run.error?.code !== "host_key_changed");

  function onkey(k: string): boolean {
    if (!run) return false;
    if (k === "Enter" || k === "r") {
      if (canRetry) keys.retryCopy();
      else if (run.step === "done" && k === "Enter") keys.dismissCopy();
      else return false;
      return true;
    }
    if (k === "c" && run.step === "done") {
      void copyIdentity();
      return true;
    }
    return false;
  }
</script>

{#if pick}
  <Modal onescape={() => (keys.view = { kind: "list" })} labelledBy="copy-pick-title">
    {#snippet title()}Copy to host<span class="title-host">· {pick.id}</span>{/snippet}
    <p>Adds the public key to <span class="mono">~/.ssh/authorized_keys</span> on the host, then logs in with it to check.</p>
    <label class="field">
      <span>Host</span>
      <input data-autofocus bind:value={query} onkeydown={onSearchKey} placeholder="Search hosts, #tags…" spellcheck="false" autocomplete="off" aria-label="Search hosts" />
    </label>
    {#if hosts.length}
      <ul class="hosts" role="listbox" aria-label="Hosts" bind:this={listEl}>
        {#each hosts as host, i (host.alias)}
          {@const st = describe(workspace.byAlias[host.alias])}
          <li role="option" aria-selected={cursor === i} class:selected={cursor === i} data-status={st.status} onclick={() => start(host)} onmouseenter={() => (cursor = i)} onkeydown={() => {}}>
            <span class="dot"></span>
            <span class="alias">{host.alias}</span>
            <span class="target mono">{host.user ? `${host.user}@` : ""}{host.hostname}</span>
          </li>
        {/each}
      </ul>
    {:else}
      <p class="note">{query ? "No host matches this search." : "No hosts in ~/.ssh/config."}</p>
    {/if}
    {#snippet footer()}
      <button class="secondary" onclick={() => (keys.view = { kind: "list" })}>Back <kbd>Esc</kbd></button>
      <button class="primary" disabled={!hosts[cursor]} onclick={() => start(hosts[cursor])}>Copy <kbd>↵</kbd></button>
    {/snippet}
  </Modal>
{:else if run}
  <Modal onescape={() => keys.hideCopy()} {onkey} labelledBy="copy-title">
    {#snippet title()}Copy {run.key.id}<span class="title-host">· {run.alias}</span>{/snippet}

    <ol class="steps" aria-live="polite">
      {#each STEPS as s (s.step)}
        {@const st = stateOf(s.step)}
        <li data-state={st}>
          <span class="mark" aria-hidden="true">
            {#if st === "done"}<svg viewBox="0 0 16 16"><path d="M3.5 8.5 6.5 11.5 12.5 5" /></svg>
            {:else if st === "failed"}<svg viewBox="0 0 16 16"><path d="M4.5 4.5 11.5 11.5M11.5 4.5 4.5 11.5" /></svg>
            {:else if st === "active"}<span class="spinner"></span>
            {:else}<span class="ring"></span>{/if}
          </span>
          <span class="label">{s.label}</span>
          <span class="detail">
            {#if s.step === "connecting" && st === "active"}{describe(workspace.byAlias[run.alias]).label}
            {:else if s.step === "installing" && st === "done" && run.outcome === "already_present"}already in authorized_keys
            {:else if s.step === "verifying" && st === "active"}logging in with {run.key.id} only
            {:else if s.step === "verifying" && st === "skipped"}skipped: the private key is not on this machine{/if}
          </span>
        </li>
      {/each}
    </ol>

    {#if run.step === "done"}
      <div class="result" in:fly={{ y: 6, duration: dur("base") }}>
        <strong>
          {#if run.verified}{run.outcome === "already_present" ? "Already installed and verified" : "Installed and verified"}
          {:else}{run.outcome === "already_present" ? "Already installed" : "Installed"}{/if}
        </strong>
        <span>
          {#if run.verified}You can now log in to <span class="mono">{run.alias}</span> with <span class="mono">{run.key.id}</span>.
          {:else}<span class="mono">{run.key.id}</span> is authorized on <span class="mono">{run.alias}</span>; it was not verified from here.{/if}
        </span>
      </div>
      <div class="identity">
        <span class="note">To always use this key, add to <span class="mono">~/.ssh/config</span> (Portico never edits it):</span>
        <pre class="mono">{identity}</pre>
      </div>
    {:else if run.step === "failed" && run.error}
      <div class="inline-error" role="alert" in:fly={{ y: 6, duration: dur("base") }}>
        <span>{sentence(run.error.message)}</span>
        {#if run.error.hint}<span class="hint">{run.error.hint}</span>{/if}
      </div>
    {/if}

    {#snippet footer()}
      {#if run.step === "done"}
        <button class="secondary" onclick={copyIdentity}>{copied ? "Copied" : "Copy config lines"} <kbd>c</kbd></button>
        <button class="primary" data-autofocus onclick={() => keys.dismissCopy()}>Done <kbd>↵</kbd></button>
      {:else if run.step === "failed"}
        <button class="secondary" onclick={() => keys.dismissCopy()}>Close <kbd>Esc</kbd></button>
        {#if canRetry}
          <button class="primary" data-autofocus onclick={() => keys.retryCopy()}>Retry <kbd>r</kbd> <kbd>↵</kbd></button>
        {/if}
      {:else}
        <span class="footer-note">The copy goes on in the background; a notice tells how it ended.</span>
        <button class="secondary" data-autofocus onclick={() => keys.hideCopy()}>Hide <kbd>Esc</kbd></button>
      {/if}
    {/snippet}
  </Modal>
{/if}

<style>
  .hosts { display: grid; gap: var(--size-line-gap); margin: 0; padding: 0; list-style: none; max-height: calc(34px * 6); overflow-y: auto; }
  .hosts li {
    --c: var(--status-idle);
    position: relative;
    display: grid;
    grid-template-columns: 8px auto minmax(0, 1fr);
    align-items: center;
    gap: var(--space-3);
    height: 34px;
    padding: 0 var(--space-3);
    border-radius: var(--radius-md);
  }
  .hosts li[data-status="connected"] { --c: var(--status-connected); }
  .hosts li[data-status="connecting"] { --c: var(--status-connecting); }
  .hosts li[data-status="authenticating"] { --c: var(--status-authenticating); }
  .hosts li[data-status="warning"] { --c: var(--status-warning); }
  .hosts li[data-status="error"] { --c: var(--status-error); }
  .hosts li.selected { background: var(--bg-selected); }
  .hosts li.selected::before {
    content: "";
    position: absolute;
    left: 0;
    top: var(--space-2);
    bottom: var(--space-2);
    width: var(--size-bar);
    border-radius: var(--radius-pill);
    background: var(--accent);
  }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--c); }
  .alias { color: var(--text-primary); font-weight: 550; }
  .target { justify-self: end; color: var(--text-muted); font-size: var(--text-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .steps { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
  .steps li {
    --c: var(--text-muted);
    display: grid;
    grid-template-columns: 16px auto minmax(0, 1fr);
    align-items: center;
    gap: var(--space-3);
    color: var(--text-muted);
  }
  .steps li[data-state="active"] { --c: var(--status-connecting); color: var(--text-primary); }
  .steps li[data-state="done"] { --c: var(--status-connected); color: var(--text-secondary); }
  .steps li[data-state="failed"] { --c: var(--status-error); color: var(--text-primary); }
  .mark { display: grid; place-items: center; width: 16px; height: 16px; color: var(--c); }
  .mark svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.75; stroke-linecap: round; stroke-linejoin: round; }
  .ring { width: 8px; height: 8px; border-radius: 50%; box-shadow: inset 0 0 0 1.5px var(--border-strong); }
  .label { font-weight: 550; }
  .detail { color: var(--text-muted); font-size: var(--text-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .result {
    display: grid;
    gap: var(--space-1);
    padding: var(--space-3) var(--space-4);
    border: 1px solid color-mix(in srgb, var(--status-connected) 40%, transparent);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--status-connected) 6%, var(--bg-elevated));
  }
  .result strong { color: var(--text-primary); font-weight: 600; }
  .identity { display: grid; gap: var(--space-2); }
  pre {
    margin: 0;
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font-size: var(--text-sm);
    user-select: text;
    overflow-x: auto;
  }
</style>
