<script lang="ts">
  // One received change, reviewed alone (phase 11): a host's destination
  // (F1: current next to received, confirm or reject) or a snippet's full
  // command (F8: monospace, never truncated, scrollable; confirm). The
  // decision carries the hash of what is shown; when the content changes
  // meanwhile, the state module closes this view (`sync_pending_changed`).
  import { sentence, type ErrorSummary } from "../api";
  import { backend } from "../backend";
  import { HOST_FIELDS, hostField } from "../syncview";
  import { sync } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";

  let { view }: { view: { kind: "host"; alias: string; hash: string } | { kind: "snippet"; id: string; hash: string } } = $props();

  let armed = $state(false);
  let busy = $state(false);
  let error: ErrorSummary | null = $state(null);

  const host = $derived(view.kind === "host" ? sync.pendingHosts.find((p) => p.alias === view.alias && p.destination_hash === view.hash) : undefined);
  const snippet = $derived(view.kind === "snippet" ? sync.snippets.find((s) => s.id === view.id && s.hash === view.hash) : undefined);

  async function decide(confirm: boolean) {
    if (busy || !armed) return;
    busy = true;
    error = null;
    let failed: ErrorSummary | null;
    if (view.kind === "host") {
      const list = [{ alias: view.alias, destinationHash: view.hash }];
      failed = await sync.act(() => (confirm ? backend.syncConfirmHosts(list) : backend.syncRejectHosts(list)));
    } else {
      const { id, hash } = view;
      failed = await sync.act(() => backend.confirmSyncedSnippet(id, hash));
    }
    busy = false;
    if (failed) {
      error = failed;
      return;
    }
    const name = view.kind === "host" ? view.alias : (snippet?.snippet.name ?? "snippet");
    workspace.notify({ code: "sync_confirmed", message: confirm ? `${name} confirmed` : `${name} rejected on this device`, hint: null }, "info");
    sync.back();
  }

  function onkey(key: string): boolean {
    if (key === "c") void decide(true);
    else if (key === "r" && view.kind === "host") void decide(false);
    else return false;
    return true;
  }
</script>

<Modal bind:armed onescape={() => sync.back()} {onkey} labelledBy="sync-review-title" size="wide">
  {#snippet title()}
    {#if view.kind === "host"}{host?.new ? "New host" : "Changed host"} <span class="mono">{view.alias}</span>
    {:else}Received snippet{#if snippet}<span class="title-host">{snippet.snippet.name}</span>{/if}{/if}
  {/snippet}

  {#if view.kind === "host" && host}
    <p>
      {host.new ? "Added" : "Changed"} by <span class="mono">{host.devices.join(", ") || "another device"}</span>.
      {#if host.renamed_from}It was <span class="mono">{host.renamed_from}</span> there: here it is a new host.{/if}
      Confirm only if you expect this destination: connecting to it sends your keys' signatures and, once you type it, your password.
    </p>
    <div class="dest" role="table">
      <div class="tr th" role="row"><span role="columnheader"></span><span role="columnheader">On this device</span><span role="columnheader">Received</span></div>
      {#each HOST_FIELDS as [key, label] (key)}
        {@const before = hostField(host.current, key)}
        {@const after = hostField(host.incoming, key)}
        <div class="tr" role="row" class:differs={before !== after}>
          <span role="rowheader" class="field-name">{label}</span>
          <span role="cell" class="mono before">{host.current ? before : "—"}</span>
          <span role="cell" class="mono after">{after}</span>
        </div>
      {/each}
    </div>
    <p class="note">Rejecting keeps it out of hosts.conf on this device only; a different destination later asks again.</p>
  {:else if view.kind === "snippet" && snippet}
    <p>Changed by sync on another device. It runs only once you confirm this exact command on this device.</p>
    <!-- Focusable so the keyboard can scroll a long command. -->
    <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
    <pre class="command" tabindex="0" aria-label="Full command" data-testid="sync-snippet-command">{snippet.snippet.command}</pre>
    {#if snippet.snippet.description}<p class="note">{snippet.snippet.description}</p>{/if}
    <p class="note">{snippet.snippet.run ? "Runs as soon as it is sent (Enter is added)." : "Pasted only: you press Enter yourself."}</p>
  {:else}
    <p class="note">This change is no longer waiting for a confirmation.</p>
  {/if}
  {#if error}
    <div class="inline-error" role="alert"><span>{sentence(error.message)}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}</div>
  {/if}

  {#snippet footer()}
    <button class="secondary" onclick={() => sync.back()}>Back <kbd>Esc</kbd></button>
    {#if view.kind === "host" && host}
      <button class="secondary" disabled={busy} onclick={() => decide(false)}>Reject <kbd>r</kbd></button>
    {/if}
    {#if host || snippet}
      <button class="primary" disabled={busy || !armed} onclick={() => decide(true)}>Confirm <kbd>c</kbd></button>
    {/if}
  {/snippet}
</Modal>

<style>
  .mono { font-family: var(--font-mono); }
  .dest { display: grid; gap: 1px; padding: var(--space-2); border-radius: var(--radius-md); background: var(--bg-elevated); }
  .tr { display: grid; grid-template-columns: 12ch minmax(0, 1fr) minmax(0, 1fr); gap: var(--space-3); padding: var(--space-1) var(--space-2); }
  .th { color: var(--text-muted); font-size: var(--text-xs); }
  .field-name { color: var(--text-muted); font-size: var(--text-sm); }
  .before, .after { min-width: 0; font-size: var(--text-sm); word-break: break-all; }
  .before { color: var(--text-muted); }
  .after { color: var(--text-secondary); }
  .tr.differs { border-radius: var(--radius-sm); background: var(--bg-overlay); box-shadow: inset 2px 0 0 var(--status-warning); }
  .tr.differs .after { color: var(--text-primary); }
  .command {
    max-height: 40vh;
    margin: 0;
    padding: var(--space-3) var(--space-4);
    overflow: auto;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-base);
    color: var(--text-primary);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    line-height: 1.5;
    white-space: pre-wrap;
    word-break: break-all;
    user-select: text;
  }
  .command:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .hint { color: var(--text-secondary); }
</style>
