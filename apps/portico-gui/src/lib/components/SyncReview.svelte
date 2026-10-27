<script lang="ts">
  // One received change, reviewed alone (phase 11): a host's destination
  // (F1: current next to received, confirm `y` or reject `n`) or a
  // snippet's full command (F8: monospace, never truncated, numbered lines,
  // tabs and control characters drawn visibly; confirmable only once its
  // end has been on screen, S1c review A1/A2). The decision carries the
  // hash of what is shown; when the content changes meanwhile, the state
  // module closes this view (`sync_pending_changed`).
  import { tick } from "svelte";
  import { type ErrorSummary } from "../api";
  import { backend } from "../backend";
  import { HOST_FIELDS, commandLines, commandStats, hostField, scrolledToEnd } from "../syncview";
  import { sync } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import CodeText from "./CodeText.svelte";
  import Modal from "./Modal.svelte";

  let { view }: { view: { kind: "host"; alias: string; hash: string } | { kind: "snippet"; id: string; hash: string } } = $props();

  let armed = $state(false);
  let busy = $state(false);
  let error: ErrorSummary | null = $state(null);

  const host = $derived(view.kind === "host" ? sync.pendingHosts.find((p) => p.alias === view.alias && p.destination_hash === view.hash) : undefined);
  const snippet = $derived(view.kind === "snippet" ? sync.snippets.find((s) => s.id === view.id && s.hash === view.hash) : undefined);
  const lines = $derived(snippet ? commandLines(snippet.snippet.command) : []);
  const digits = $derived(String(lines.length).length);

  // The command's end must have been on screen before it can be confirmed:
  // a payload can hide below the fold of an overlay scrollbar.
  let box: HTMLPreElement | undefined = $state();
  let seenEnd = $state(false);
  let atEnd = $state(true);

  function measure() {
    if (!box) return;
    atEnd = scrolledToEnd(box.scrollTop, box.clientHeight, box.scrollHeight);
    if (atEnd) seenEnd = true;
  }

  // A new content (another hash): seen again from the top. (A primitive:
  // a reload with the same content does not reset the gate.)
  const shownHash = $derived(snippet?.hash);
  $effect(() => {
    void shownHash;
    seenEnd = false;
    atEnd = true;
    void tick().then(measure);
  });

  $effect(() => {
    if (!box) return;
    const observer = new ResizeObserver(measure);
    observer.observe(box);
    return () => observer.disconnect();
  });

  function more() {
    box?.scrollBy({ top: box.clientHeight * 0.8 });
  }

  const confirmable = $derived(!busy && armed && (view.kind === "host" ? !!host : !!snippet && seenEnd));

  async function decide(confirm: boolean) {
    if (busy || !armed) return;
    if (confirm && !confirmable) return;
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

  // `y` / `n`, as every confirmation of the app; Enter stays unbound.
  function onkey(key: string): boolean {
    if (key === "y") void decide(true);
    else if (key === "n" && view.kind === "host" && host) void decide(false);
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
    <p>Changed by <span class="mono">{snippet.devices.join(", ") || "another device"}</span>. It runs only once you confirm this exact command on this device.</p>
    <div class="command-head">
      <span class="caption">Command</span>
      <span class="stats" data-testid="sync-snippet-stats">{commandStats(snippet.snippet.command)}</span>
    </div>
    <div class="command-box" class:clipped={!atEnd}>
      <!-- Focusable so the keyboard can scroll a long command. -->
      <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
      <pre class="command" tabindex="0" aria-label="Full command" data-testid="sync-snippet-command" bind:this={box} onscroll={measure} style:--ln-width={`${digits}ch`}>{#each lines as line, i (i)}<span class="line"><span class="ln" aria-hidden="true">{i + 1}</span><span class="lc">{#each line as piece, j (j)}{#if piece.mark === "→"}<span class="ctl tab" data-mark={piece.mark} title="Tab">{piece.text}</span>{:else if piece.mark}<span class="ctl" data-mark={piece.mark} title={`Control character U+${(piece.text.codePointAt(0) ?? 0).toString(16).toUpperCase().padStart(4, "0")}`}></span>{:else}{piece.text}{/if}{/each}</span></span>{/each}</pre>
      <div class="fade" aria-hidden="true"></div>
      {#if !atEnd}
        <button class="more" tabindex="-1" onclick={more} data-testid="sync-snippet-more">↓ more</button>
      {/if}
    </div>
    {#if snippet.snippet.description}<p class="note">{snippet.snippet.description}</p>{/if}
    {#if snippet.snippet.run}
      <p class="runs"><span class="mark" aria-hidden="true">▲</span>Runs as soon as it is sent (Enter is added).</p>
    {:else}
      <p class="note">Pasted only: you press Enter yourself.</p>
    {/if}
  {:else}
    <p class="note">This change is no longer waiting for a confirmation.</p>
  {/if}
  {#if error}
    <div class="inline-error" role="alert"><span><CodeText text={error.message} /></span>{#if error.hint}<span class="hint"><CodeText text={error.hint} capitalize={false} /></span>{/if}</div>
  {/if}

  {#snippet footer()}
    {#if snippet && !seenEnd}<span class="footer-note" data-testid="sync-snippet-gate">Scroll to the end of the command to confirm it.</span>{/if}
    <button class="secondary" onclick={() => sync.back()}>Back <kbd>Esc</kbd></button>
    {#if view.kind === "host" && host}
      <button class="secondary" disabled={busy} onclick={() => decide(false)}>Reject <kbd>n</kbd></button>
    {/if}
    {#if host || snippet}
      <button class="primary" disabled={!confirmable} onclick={() => decide(true)} data-testid="sync-review-confirm">Confirm <kbd>y</kbd></button>
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
  .command-head { display: flex; align-items: baseline; justify-content: space-between; gap: var(--space-3); }
  .caption { color: var(--text-muted); font-size: var(--text-xs); letter-spacing: var(--tracking-caps); text-transform: uppercase; }
  .stats { color: var(--text-secondary); font-size: var(--text-sm); font-variant-numeric: tabular-nums; }
  .command-box { position: relative; }
  .command {
    max-height: 40vh;
    margin: 0;
    padding: var(--space-3) var(--space-4) var(--space-3) var(--space-2);
    overflow: auto;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-base);
    color: var(--text-primary);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    line-height: 1.5;
    tab-size: 4;
    user-select: text;
  }
  .command:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .line { display: flex; gap: var(--space-3); min-height: 1lh; }
  /* Numbered, so a blank line (an extra Enter) is visible. Not copied. */
  .ln { flex: 0 0 auto; width: var(--ln-width); color: var(--text-muted); text-align: right; user-select: none; }
  .lc { flex: 1 1 auto; min-width: 0; white-space: pre-wrap; word-break: break-all; }
  /* Tabs and control characters: a muted mark, drawn (not copied). */
  .ctl::before { content: attr(data-mark); color: var(--text-muted); }
  .fade {
    position: absolute;
    inset: auto 1px 1px 1px;
    height: var(--space-7);
    border-radius: 0 0 var(--radius-md) var(--radius-md);
    background: linear-gradient(to bottom, transparent, var(--bg-base));
    opacity: 0;
    pointer-events: none;
    transition: opacity var(--motion-fast) var(--ease-standard);
  }
  .clipped .fade { opacity: 1; }
  .command-box .more {
    position: absolute;
    right: var(--space-4);
    bottom: var(--space-2);
    height: var(--size-badge-sm);
    padding: 0 var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    background: var(--bg-overlay);
    color: var(--text-secondary);
    font-size: var(--text-xs);
  }
  .command-box .more:hover { color: var(--text-primary); }
  .runs { display: flex; align-items: baseline; gap: var(--space-2); color: var(--text-primary); font-size: var(--text-sm); }
  .runs .mark { color: var(--status-warning); }
  .hint { color: var(--text-secondary); }
</style>
