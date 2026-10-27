<script lang="ts">
  // Device files that claim newer keys (phase 11, S2c-1, RR5): "This file
  // claims newer keys. If nobody changed the keys, remove it." Each file is
  // named; removing it asks first (Keep / Remove file, `y` once asked) and
  // sends back the fingerprint of the file shown (another content is
  // refused with `sync_not_pending`). Shared by the adoption dialog and the
  // Sync panel's Overview.
  import { type ErrorSummary, type NewerEpochFile } from "../api";
  import { backend } from "../backend";
  import { sync } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";

  let {
    asking = $bindable(null),
    busy = $bindable(null),
    onerror,
  }: {
    /** The file whose removal is being confirmed (the dialog maps `y` to `remove`). */
    asking?: NewerEpochFile | null;
    busy?: string | null;
    onerror: (error: ErrorSummary | null) => void;
  } = $props();

  export async function remove() {
    const file = asking;
    if (!file || busy) return;
    busy = "Removing the file…";
    onerror(null);
    const failed = await sync.act(() => backend.syncDiscardNewerFile(file.device, file.fingerprint));
    busy = null;
    asking = null;
    if (failed) onerror(failed);
    else workspace.notify({ code: "sync", message: `Removed ${file.file}`, hint: "Its device publishes again with the keys it really has." }, "info");
  }
</script>

{#if sync.newerFiles.length}
  <section class="newer" aria-labelledby="sync-newer-title" data-testid="sync-newer">
    <span class="caption" id="sync-newer-title">{sync.newerFiles.length === 1 ? "A file claims newer keys" : "Files claim newer keys"}</span>
    <p class="note">If nobody changed the keys, remove it: a lost or copied device may have put it there.</p>
    <ul class="list">
      {#each sync.newerFiles as f (f.fingerprint)}
        <li class="file">
          <div class="file-text">
            <span class="mono path">{f.file}</span>
            <span class="note">{f.label} · announces keys {f.epoch}</span>
          </div>
          {#if asking?.fingerprint === f.fingerprint}
            <span class="confirm">
              <button class="secondary small" onclick={() => (asking = null)}>Keep</button>
              <button class="danger small" disabled={!!busy} onclick={remove} data-testid="sync-newer-remove">Remove file <kbd>y</kbd></button>
            </span>
          {:else}
            <button class="secondary small" disabled={!!busy} onclick={() => (asking = f)}>Remove file…</button>
          {/if}
        </li>
      {/each}
    </ul>
  </section>
{/if}

<style>
  .newer { display: grid; gap: var(--space-2); }
  .caption { color: var(--text-muted); font-size: var(--text-xs); letter-spacing: var(--tracking-caps); text-transform: uppercase; }
  .note { color: var(--text-muted); font-size: var(--text-sm); }
  .mono { font-family: var(--font-mono); }
  .list { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
  .file {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-left: 2px solid var(--status-warning);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  .file-text { display: grid; gap: var(--space-1); min-width: 0; }
  .path { color: var(--text-primary); font-size: var(--text-sm); word-break: break-all; }
  .confirm { display: inline-flex; gap: var(--space-2); }
</style>
