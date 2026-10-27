<script lang="ts">
  // Received changes waiting for this device's confirmation (phase 11):
  // hosts (F1) — each destination visible, current next to received, ticked
  // one by one (nothing preselected) — and snippets changed by sync (F8),
  // reviewed one by one with their full command. Decisions carry the hash of
  // what is shown: content changed since is refused and shown again.
  import { type ErrorSummary, type PendingHostChange } from "../api";
  import { backend } from "../backend";
  import { HOST_FIELDS, hostField } from "../syncview";
  import { sync } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import CodeText from "./CodeText.svelte";

  const keyOf = (p: PendingHostChange) => `${p.alias}\u0000${p.destination_hash}`;
  let ticked: Record<string, boolean> = $state({});
  let busy: string | null = $state(null);
  let error: ErrorSummary | null = $state(null);

  // A destination that changed (another device published again) drops its tick.
  $effect(() => {
    const live = new Set(sync.pendingHosts.map(keyOf));
    for (const k of Object.keys(ticked)) if (!live.has(k)) delete ticked[k];
  });

  const chosen = $derived(sync.pendingHosts.filter((p) => ticked[keyOf(p)]));

  async function decide(confirm: boolean) {
    const list = chosen.map((p) => ({ alias: p.alias, destinationHash: p.destination_hash }));
    if (!list.length) return;
    busy = confirm ? "Confirming…" : "Rejecting…";
    error = null;
    const failed = await sync.act(() => (confirm ? backend.syncConfirmHosts(list) : backend.syncRejectHosts(list)));
    busy = null;
    if (failed) {
      error = failed.code === "sync_not_pending" ? { ...failed, hint: "A destination changed meanwhile: check the list again, nothing was written." } : failed;
      return;
    }
    ticked = {};
    workspace.notify(
      {
        code: "sync_hosts",
        message: `${list.length} ${list.length === 1 ? "host" : "hosts"} ${confirm ? "confirmed" : "rejected on this device"}`,
        hint: confirm ? "Written to Portico's hosts.conf." : "Kept out of hosts.conf here; a new destination asks again.",
      },
      "info",
    );
  }
</script>

<section class="block" aria-labelledby="received-hosts">
  <div class="block-head">
    <span class="caption" id="received-hosts">Hosts</span>
    <span class="note">Added to Portico's hosts.conf only once you confirm each destination.</span>
  </div>
  {#if !sync.pendingHosts.length}
    <p class="empty">No received host waits for a confirmation.</p>
  {:else}
    <ul class="list">
      {#each sync.pendingHosts as p (keyOf(p))}
        <li class="host" class:on={ticked[keyOf(p)]}>
          <label class="pick">
            <input type="checkbox" bind:checked={ticked[keyOf(p)]} aria-label={`Select ${p.alias}`} />
            <span class="alias mono">{p.alias}</span>
            <span class="pill" data-kind={p.new ? "new" : "changed"}>{p.new ? "new" : "changed"}</span>
            <span class="from">from {p.devices.join(", ") || "another device"}</span>
          </label>
          {#if p.renamed_from}<p class="note">Renamed from <span class="mono">{p.renamed_from}</span> on that device: a new host here.</p>{/if}
          <div class="dest" role="table" aria-label={`${p.alias} destination`}>
            <div class="tr th" role="row"><span role="columnheader"></span><span role="columnheader">On this device</span><span role="columnheader">Received</span></div>
            {#each HOST_FIELDS as [key, label] (key)}
              {@const before = hostField(p.current, key)}
              {@const after = hostField(p.incoming, key)}
              <div class="tr" role="row" class:differs={before !== after}>
                <span role="rowheader" class="field-name">{label}</span>
                <span role="cell" class="mono before">{p.current ? before : "—"}</span>
                <span role="cell" class="mono after">{after}</span>
              </div>
            {/each}
          </div>
        </li>
      {/each}
    </ul>
    <div class="actions" data-testid="sync-received-actions">
      <span class="note">{chosen.length ? `${chosen.length} selected` : "Check hosts to confirm or reject them."}</span>
      <button class="secondary small" disabled={!chosen.length || !!busy} onclick={() => decide(false)}>Reject selected</button>
      <button class="primary small" disabled={!chosen.length || !!busy} onclick={() => decide(true)}>
        {#if busy}<span class="spinner"></span>{/if}Confirm selected
      </button>
    </div>
  {/if}
  {#if error}
    <div class="inline-error" role="alert"><span><CodeText text={error.message} /></span>{#if error.hint}<span class="hint"><CodeText text={error.hint} capitalize={false} /></span>{/if}</div>
  {/if}
</section>

<section class="block" aria-labelledby="received-snippets">
  <div class="block-head">
    <span class="caption" id="received-snippets">Snippets</span>
    <span class="note">Run only once you have read the command.</span>
  </div>
  {#if !sync.snippets.length}
    <p class="empty">No received snippet waits for a confirmation.</p>
  {:else}
    <ul class="list">
      {#each sync.snippets as s (s.id + s.hash)}
        <li class="snippet">
          <span class="name">{s.snippet.name}</span>
          <span class="pill" data-kind="changed">{s.snippet.run ? "Runs when sent" : "Pasted only"}</span>
          <button class="secondary small" onclick={() => sync.reviewSnippet(s)}>Review command…</button>
        </li>
      {/each}
    </ul>
  {/if}
</section>

<style>
  .block { display: grid; gap: var(--space-2); }
  .block-head { display: flex; flex-wrap: wrap; align-items: baseline; gap: var(--space-3); }
  .caption { color: var(--text-muted); font-size: var(--text-xs); letter-spacing: var(--tracking-caps); text-transform: uppercase; }
  .note, .empty { color: var(--text-muted); font-size: var(--text-sm); }
  .list { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
  .host {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    transition: border-color var(--motion-fast) var(--ease-standard);
  }
  .host.on { border-color: var(--accent); }
  .pick { display: flex; flex-wrap: wrap; align-items: center; gap: var(--space-2); }
  /* Same box as `.check` (Modal.svelte): WebKitGTK paints native ones white. */
  .pick input {
    appearance: none;
    display: inline-grid;
    flex: 0 0 auto;
    place-content: center;
    width: 14px;
    height: 14px;
    margin: 0;
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
  }
  .pick input::after {
    content: "";
    width: 7px;
    height: 4px;
    margin-top: -2px;
    border: solid var(--text-on-accent);
    border-width: 0 0 1.5px 1.5px;
    transform: rotate(-45deg) scale(0);
    transition: transform var(--motion-fast) var(--ease-standard);
  }
  .pick input:checked { border-color: var(--accent); background: var(--accent); }
  .pick input:checked::after { transform: rotate(-45deg) scale(1); }
  .pick input:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .alias { color: var(--text-primary); font-weight: 550; }
  .from { color: var(--text-muted); font-size: var(--text-sm); }
  .pill { padding: 0 var(--space-2); border: 1px solid var(--border-strong); border-radius: var(--radius-pill); color: var(--text-secondary); font-size: var(--text-xs); }
  .pill[data-kind="new"] { border-color: color-mix(in srgb, var(--accent) 55%, var(--border-strong)); }
  .dest { display: grid; gap: 1px; }
  .tr { display: grid; grid-template-columns: 12ch minmax(0, 1fr) minmax(0, 1fr); gap: var(--space-3); padding: 2px var(--space-2); }
  .th { color: var(--text-muted); font-size: var(--text-xs); }
  .field-name { color: var(--text-muted); font-size: var(--text-sm); }
  .before, .after { min-width: 0; font-size: var(--text-sm); word-break: break-all; }
  .before { color: var(--text-muted); }
  .after { color: var(--text-secondary); }
  .tr.differs { border-radius: var(--radius-sm); background: var(--bg-elevated); box-shadow: inset 2px 0 0 var(--status-warning); }
  .tr.differs .after { color: var(--text-primary); }
  .mono { font-family: var(--font-mono); }
  /* Always reachable under a long list: it sticks to the panel's bottom. */
  .actions {
    position: sticky;
    bottom: 0;
    z-index: 1;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: flex-end;
    gap: var(--space-2);
    padding: var(--space-2) 0;
    border-top: 1px solid var(--border-subtle);
    background: var(--bg-overlay);
  }
  .actions .note { margin-right: auto; }
  .snippet {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    min-height: var(--size-row);
    padding: var(--space-1) var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
  }
  .snippet .name { color: var(--text-primary); font-weight: 550; }
  .snippet button { margin-left: auto; }
  .hint { color: var(--text-secondary); }
</style>
