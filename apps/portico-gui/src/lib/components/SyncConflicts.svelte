<script lang="ts">
  // Conflicts (phase 11): concurrent edits of one record, never settled
  // silently. Each shows its sides field by field (differences marked), a
  // choice preselected — Union for tags, Keep both for snippets — and
  // "Resolve". Choosing a side is a write: it syncs to every device.
  import { sentence, type ErrorSummary, type SyncConflict, type SyncResolution } from "../api";
  import { backend } from "../backend";
  import { ago, conflictRows, defaultChoice, subjectLabel } from "../syncview";
  import { sync } from "../state/sync.svelte";

  /** The choice per conflict id (preselected from `defaultChoice`). */
  let choices: Record<string, SyncResolution> = $state({});
  let busy: string | null = $state(null);
  let errors: Record<string, ErrorSummary> = $state({});

  const choiceOf = (c: SyncConflict): SyncResolution => choices[c.id] ?? defaultChoice(c.sides);
  const same = (a: SyncResolution, b: SyncResolution) => JSON.stringify(a) === JSON.stringify(b);
  const isTags = (c: SyncConflict) => c.sides.some((s) => s.value.type === "tags");
  const isSnippet = (c: SyncConflict) => c.sides.some((s) => s.value.type === "snippet");

  async function resolve(c: SyncConflict) {
    busy = c.id;
    const failed = await sync.act(() => backend.syncResolve(c.id, choiceOf(c)));
    busy = null;
    if (failed) errors = { ...errors, [c.id]: failed };
    else {
      const { [c.id]: _, ...rest } = errors;
      errors = rest;
    }
  }

  const sideTitle = (devices: string[], local: boolean) => (local ? "This device" : devices[0] ?? "Another device");
</script>

{#if sync.lists === "loading" && !sync.conflicts.length}
  <div class="skeleton" aria-busy="true"></div>
{:else if !sync.conflicts.length}
  <p class="empty">No conflict. Edits made on two devices at once show up here, side by side.</p>
{:else}
  <ul class="conflicts" aria-label="Conflicts">
    {#each sync.conflicts as c (c.id)}
      {@const rows = conflictRows(c.sides)}
      {@const choice = choiceOf(c)}
      <li class="conflict">
        <div class="head">
          <strong class="mono">{subjectLabel(c.subject)}</strong>
          <span class="note">{c.sides.length} versions</span>
        </div>
        <div class="table" role="table" style:--cols={c.sides.length}>
          <div class="tr th" role="row">
            <span role="columnheader"></span>
            {#each c.sides as side (side.index)}
              <span role="columnheader" class="side-head">
                <span class="who">{sideTitle(side.devices, side.local)}</span>
                <span class="when">{side.devices.join(", ")} · {ago(side.at, sync.now)}</span>
              </span>
            {/each}
          </div>
          {#each rows as row (row.field)}
            <div class="tr" role="row" class:differs={row.differs}>
              <span role="rowheader" class="field-name">{row.field}</span>
              {#each row.values as v, i (i)}<span role="cell" class="mono value">{v}</span>{/each}
            </div>
          {/each}
        </div>
        <div class="choose" role="radiogroup" aria-label={`Resolve ${subjectLabel(c.subject)}`}>
          {#if isTags(c)}
            <label class="radio"><input type="radio" name={c.id} checked={choice.type === "union"} onchange={() => (choices[c.id] = { type: "union" })} /> Union of all tags</label>
          {/if}
          {#if isSnippet(c)}
            {#each c.sides as side (side.index)}
              {#if side.value.type === "snippet"}
                <label class="radio">
                  <input type="radio" name={c.id} checked={same(choice, { type: "keep_both", keep: side.index })} onchange={() => (choices[c.id] = { type: "keep_both", keep: side.index })} />
                  Keep both · {side.local ? "this device's" : `${sideTitle(side.devices, side.local)}'s`} keeps the id, the other becomes a copy
                </label>
              {/if}
            {/each}
          {/if}
          {#each c.sides as side (side.index)}
            <label class="radio">
              <input type="radio" name={c.id} checked={same(choice, { type: "pick", index: side.index })} onchange={() => (choices[c.id] = { type: "pick", index: side.index })} />
              Keep {side.local ? "this device's" : `${sideTitle(side.devices, side.local)}'s`}{side.value.type === "deleted" ? " (delete it)" : ""}
            </label>
          {/each}
          <button class="primary small" disabled={!!busy} onclick={() => resolve(c)}>
            {#if busy === c.id}<span class="spinner"></span>{/if}Resolve
          </button>
        </div>
        {#if errors[c.id]}
          {@const e = errors[c.id]}
          <div class="inline-error" role="alert"><span>{sentence(e?.message ?? "")}</span>{#if e?.hint}<span class="hint">{e.hint}</span>{/if}</div>
        {/if}
      </li>
    {/each}
  </ul>
{/if}

<style>
  .conflicts { display: grid; gap: var(--space-3); margin: 0; padding: 0; list-style: none; }
  .conflict { display: grid; gap: var(--space-3); padding: var(--space-3) var(--space-4); border: 1px solid var(--border-subtle); border-radius: var(--radius-md); }
  .head { display: flex; align-items: baseline; gap: var(--space-2); }
  .head strong { color: var(--text-primary); font-weight: 550; }
  .table { display: grid; gap: 1px; overflow-x: auto; border-radius: var(--radius-sm); }
  .tr { display: grid; grid-template-columns: minmax(9ch, max-content) repeat(var(--cols), minmax(0, 1fr)); gap: var(--space-3); padding: var(--space-1) var(--space-2); }
  .th { color: var(--text-muted); font-size: var(--text-xs); }
  .side-head { display: grid; }
  .who { color: var(--text-primary); font-size: var(--text-sm); font-weight: 550; }
  .when { color: var(--text-muted); font-size: var(--text-xs); }
  .field-name { color: var(--text-muted); font-size: var(--text-sm); }
  .value { min-width: 0; color: var(--text-secondary); font-size: var(--text-sm); white-space: pre-wrap; word-break: break-word; }
  .tr.differs { border-radius: var(--radius-sm); background: var(--bg-elevated); box-shadow: inset 2px 0 0 var(--status-warning); }
  .tr.differs .value { color: var(--text-primary); }
  .choose { display: grid; gap: var(--space-2); }
  .choose button { justify-self: end; }
  .radio { display: inline-flex; align-items: center; gap: var(--space-2); color: var(--text-secondary); font-size: var(--text-sm); }
  /* Drawn here: WebKitGTK paints an unchecked native radio white. */
  .radio input {
    appearance: none;
    flex: 0 0 auto;
    width: 14px;
    height: 14px;
    margin: 0;
    border: 1px solid var(--border-strong);
    border-radius: 50%;
    background: var(--bg-elevated);
    transition: box-shadow var(--motion-fast) var(--ease-standard), border-color var(--motion-fast) var(--ease-standard);
  }
  .radio input:hover { border-color: var(--accent); }
  .radio input:checked { border-color: var(--accent); box-shadow: inset 0 0 0 3px var(--bg-elevated), inset 0 0 0 7px var(--accent); }
  .radio input:focus-visible { outline: 2px solid var(--accent); outline-offset: 1px; }
  .mono { font-family: var(--font-mono); }
  .note, .empty { color: var(--text-muted); font-size: var(--text-sm); }
  .hint { color: var(--text-secondary); }
  .skeleton { height: calc(3 * var(--size-row)); border-radius: var(--radius-md); background: var(--bg-elevated); }
</style>
