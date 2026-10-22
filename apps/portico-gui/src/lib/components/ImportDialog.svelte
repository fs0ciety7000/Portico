<script lang="ts">
  // "Import from Termius…": the file is chosen in the native picker (Rust),
  // the plan is previewed here (hosts, skipped records, warnings, ignored
  // secrets), nothing is written before "Import". The report ends with the
  // Include line the user adds to ~/.ssh/config: Portico never edits it.
  import { displayPath, sentence, type ImportNotice, type PlannedHost } from "../api";
  import { importer } from "../state/importer.svelte";
  import Modal from "./Modal.svelte";

  let armed = $state(false);
  let copied = $state(false);
  let includeEl: HTMLInputElement | undefined = $state();

  const view = $derived(importer.view);
  const preview = $derived(view.kind === "preview" || view.kind === "applying" ? view.preview : null);
  const busy = $derived(view.kind === "applying" || (view.kind === "preview" && view.busy));

  const plural = (n: number, word: string) => `${n} ${word}${n === 1 ? "" : "s"}`;
  const address = (h: PlannedHost) => `${h.user ? `${h.user}@` : ""}${h.hostname}${h.port ? `:${h.port}` : ""}`;
  const where = (n: ImportNotice) => [n.record !== null ? `line ${n.record}` : null, n.label].filter(Boolean).join(" · ");

  async function copyInclude(line: string) {
    try {
      await navigator.clipboard.writeText(line);
      copied = true;
      setTimeout(() => (copied = false), 1600);
    } catch {
      // No clipboard here: select the line for a manual copy.
      includeEl?.focus();
      includeEl?.select();
    }
  }

  function onkey(key: string): boolean {
    if (view.kind === "preview" && key === "p") {
      void importer.setPrefix(!importer.prefix);
      return true;
    }
    if (view.kind === "done" && key === "c") {
      void copyInclude(view.report.includeLine);
      return true;
    }
    return false;
  }
</script>

{#snippet notices(title: string, list: ImportNotice[], tone: "muted" | "warning")}
  {#if list.length}
    <section>
      <h3>{title} <span class="count">{list.length}</span></h3>
      <ul class="notices" data-tone={tone}>
        {#each list as n, i (i)}
          <li>
            <span class="where mono">{where(n)}</span>
            <span class="message">{sentence(n.message)}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}
{/snippet}

<Modal bind:armed size="wide" onescape={() => importer.close()} {onkey} labelledBy="import-title">
  {#snippet title()}
    {view.kind === "done" ? "Imported from Termius" : "Import from Termius"}
    {#if preview}<span class="title-host mono">{preview.fileName}</span>{/if}
  {/snippet}

  {#if view.kind === "picking"}
    <p class="waiting"><span class="spinner" aria-hidden="true"></span> Choose a Termius export in the file dialog…</p>
    <p class="note">
      A CSV in the layout of Termius' import template, a termius-local-export <span class="mono">hosts.json</span> or
      <span class="mono">credentials.csv</span>, or the output of <span class="mono">termius export-ssh-config</span>.
    </p>
  {:else if view.kind === "error"}
    <div class="inline-error" role="alert">
      <span>{sentence(view.error.message)}</span>
      {#if view.error.hint}<span class="hint">{view.error.hint}</span>{/if}
    </div>
  {:else if preview}
    <p class="summary">
      <strong>{plural(preview.hosts.length, "host")}</strong> to import from {plural(preview.records, "record")}{#if preview.skipped.length}, {preview.skipped.length} skipped{/if}
      <span class="muted">· {preview.format}</span>
    </p>
    {#if preview.secretsAdvice}
      <p class="secrets" role="note">
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 2.25 14.25 13.25H1.75Z" /><path d="M8 6.5v3M8 11.1v.1" /></svg>
        <span>{preview.secretsAdvice}</span>
      </p>
    {/if}
    {#if preview.hosts.length}
      <section>
        <h3>Hosts <span class="count">{preview.hosts.length}</span></h3>
        <ul class="hosts">
          {#each preview.hosts as h (h.alias)}
            <li>
              <span class="alias mono">{h.alias}</span>
              <span class="address mono">{address(h)}</span>
              <span class="meta">
                {#if h.group}<span class="group">{h.group}</span>{/if}
                {#each h.tags as t (t)}<span class="tag">#{t}</span>{/each}
              </span>
            </li>
          {/each}
        </ul>
      </section>
    {:else}
      <p class="empty">No host of this export can be imported.</p>
    {/if}
    {@render notices("Skipped", preview.skipped, "muted")}
    {@render notices("Warnings", preview.warnings, "warning")}
    {#if preview.identityFiles.length}
      <section>
        <h3>Identity files not imported <span class="count">{preview.identityFiles.length}</span></h3>
        <ul class="notices" data-tone="muted">
          {#each preview.identityFiles as f, i (i)}<li><span class="message mono">{f}</span></li>{/each}
        </ul>
      </section>
    {/if}
    <label class="check">
      <input type="checkbox" checked={importer.prefix} disabled={busy} onchange={(e) => void importer.setPrefix(e.currentTarget.checked)} />
      Prefix every alias with <span class="mono">termius-</span> (keeps hosts that share a name with yours) <kbd>p</kbd>
    </label>
    {#if importer.error}
      <div class="inline-error" role="alert">
        <span>{sentence(importer.error.message)}</span>
        {#if importer.error.hint}<span class="hint">{importer.error.hint}</span>{/if}
      </div>
    {/if}
  {:else if view.kind === "done"}
    {@const r = view.report}
    <p class="summary">
      {#if r.unchanged}
        <strong>{plural(r.imported.length, "host")}</strong> already imported: nothing changed.
      {:else}
        <strong>{plural(r.imported.length, "host")}</strong> written to <span class="mono">{displayPath(r.target)}</span>.
      {/if}
      {#if r.metaUpdated}<span class="muted">Groups, tags and notes updated for {plural(r.metaUpdated, "host")}.</span>{/if}
    </p>
    {#if r.includeActive}
      <p class="ok">~/.ssh/config already includes this file: the hosts are in your list.</p>
    {:else}
      <div class="field">
        <span>Add this line at the top of <span class="mono">~/.ssh/config</span>. Portico never edits that file.</span>
        <div class="include">
          <input class="mono" readonly value={r.includeLine} bind:this={includeEl} aria-label="Include line" onfocus={(e) => e.currentTarget.select()} />
          <button class="secondary small" onclick={() => void copyInclude(r.includeLine)}>{copied ? "Copied" : "Copy"} <kbd>c</kbd></button>
        </div>
        <span class="help">Then reload the host list (the hosts appear once the line is in place).</span>
      </div>
    {/if}
    {#if r.secretsAdvice}
      <p class="secrets" role="note">
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 2.25 14.25 13.25H1.75Z" /><path d="M8 6.5v3M8 11.1v.1" /></svg>
        <span>{r.secretsAdvice}</span>
      </p>
    {/if}
  {/if}

  {#snippet footer()}
    {#if view.kind === "done"}
      <button class="primary" data-autofocus onclick={() => importer.close()}>Done</button>
    {:else if view.kind === "error"}
      <button class="secondary" onclick={() => importer.close()}>Close</button>
      <button class="primary" data-autofocus onclick={() => void importer.start()}>Choose another file</button>
    {:else if preview}
      <span class="footer-note">Writes <span class="mono">{displayPath(preview.target)}</span> only</span>
      <button class="secondary" onclick={() => importer.close()}>Cancel</button>
      <button class="primary" data-autofocus disabled={busy || !armed} onclick={() => void importer.apply()}>
        {#if view.kind === "applying"}<span class="spinner" aria-hidden="true"></span>{/if}
        Import {plural(preview.hosts.length, "host")}
      </button>
    {:else}
      <button class="secondary" onclick={() => importer.close()}>Cancel</button>
    {/if}
  {/snippet}
</Modal>

<style>
  .mono { font-family: var(--font-mono); font-size: var(--text-sm); }
  .muted { color: var(--text-muted); }
  .summary { color: var(--text-secondary); }
  .summary strong { color: var(--text-primary); font-weight: 550; }
  .waiting { display: flex; align-items: center; gap: var(--space-2); color: var(--text-primary); }
  .empty { color: var(--text-muted); }
  .ok { color: var(--status-connected); font-size: var(--text-sm); }
  .secrets {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--status-warning) 10%, transparent);
    color: var(--text-primary);
    font-size: var(--text-sm);
  }
  .secrets svg {
    flex: 0 0 auto;
    width: 16px;
    height: 16px;
    margin-top: 1px;
    fill: none;
    stroke: var(--status-warning);
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  section { display: grid; gap: var(--space-1); }
  h3 {
    margin: var(--space-2) 0 0;
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: 550;
    letter-spacing: 0.06em;
    text-transform: uppercase;
  }
  .count { margin-left: var(--space-1); font-variant-numeric: tabular-nums; }
  ul { margin: 0; padding: 0; list-style: none; }
  .hosts { display: grid; }
  .hosts li {
    display: grid;
    grid-template-columns: minmax(0, 2fr) minmax(0, 3fr) auto;
    align-items: center;
    gap: var(--space-3);
    min-height: 30px;
    padding: 0 var(--space-2);
    border-radius: var(--radius-md);
  }
  .hosts li:hover { background: var(--bg-elevated); }
  .alias { color: var(--text-primary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .address { color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meta { display: flex; gap: var(--space-1); justify-content: flex-end; flex-wrap: wrap; }
  .group, .tag {
    padding: 0 var(--space-2);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    color: var(--text-muted);
    font-size: var(--text-xs);
    line-height: 20px;
  }
  .group { color: var(--text-secondary); }
  .notices li {
    display: grid;
    grid-template-columns: minmax(0, 1fr) minmax(0, 3fr);
    gap: var(--space-3);
    padding: var(--space-1) var(--space-2);
    font-size: var(--text-sm);
  }
  .notices li:has(> :only-child) { grid-template-columns: minmax(0, 1fr); }
  .where { color: var(--text-muted); font-size: var(--text-xs); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .notices[data-tone="muted"] .message { color: var(--text-secondary); }
  .notices[data-tone="warning"] .message { color: var(--text-primary); }
  .notices[data-tone="warning"] li { box-shadow: inset 2px 0 0 var(--status-warning); }
  .include { display: flex; gap: var(--space-2); align-items: center; }
  .include input { flex: 1; min-width: 0; }
  .check kbd { margin-left: auto; }
</style>
