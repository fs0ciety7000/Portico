<script lang="ts">
  // "Import hosts…" (Termius or Termix, detected from the content): the file
  // is chosen in the native picker (Rust), the plan is previewed here (hosts,
  // private keys, skipped records, warnings, ignored secrets), nothing is
  // written before "Import". Private keys are imported only when ticked
  // (none by default); an optional passphrase encrypts the copies written to
  // ~/.ssh/portico/, crosses the IPC once, and is cleared right after. A
  // re-import that points an alias at another server shows old → new and is
  // written only when its "Accept the new destination" box is ticked (none
  // by default; Rust checks the aliases against the plan). The
  // report ends with the Include line the user adds to ~/.ssh/config:
  // Portico never edits it.
  import { codeSpans, sentence, type ImportNotice, type PlannedHost, type PlannedKey } from "../api";
  import {
    applyLabel,
    destinationText,
    dialogTitle,
    hostsToWrite,
    keyHosts,
    keysToWrite,
    keyType,
    noticeExplanation,
    passphraseError,
    plural,
    reportKeysLine,
    shortFingerprint,
    splitDestinationChanges,
    splitNotices,
    UNVERIFIED_FINGERPRINT,
    wantsPassphrase,
  } from "../importform";
  import { COPIED_FEEDBACK_MS } from "../motion";
  import { importer } from "../state/importer.svelte";
  import { keymap } from "../state/keymap.svelte";
  import Modal from "./Modal.svelte";

  let armed = $state(false);
  let copied = $state(false);
  let includeEl: HTMLInputElement | undefined = $state();
  /** Never kept beyond the IPC call (cleared in `apply`, `close`). */
  let passphrase = $state("");
  let confirm = $state("");

  const view = $derived(importer.view);
  const kind = $derived(view.kind);
  const preview = $derived(view.kind === "preview" || view.kind === "applying" ? view.preview : null);
  const busy = $derived(view.kind === "applying" || (view.kind === "preview" && view.busy));
  const taken = $derived(preview ? preview.skipped.filter((n) => n.code === "alias_collision").length : 0);
  const reload = $derived(keymap.label("app.reload"));
  const source = $derived(preview?.source ?? (view.kind === "done" ? view.report.source : null));
  const selected = $derived(importer.selected);
  const writing = $derived(preview ? keysToWrite(preview.keys, selected) : []);
  const askPassphrase = $derived(preview ? wantsPassphrase(preview.keys, selected) : false);
  const mismatch = $derived(askPassphrase ? passphraseError(passphrase, confirm) : null);
  // Destination changes show on their host row, not again as warnings; in
  // the report they are answered (skipped ones get their own section,
  // accepted ones are updates).
  const warnings = $derived(preview ? splitNotices(splitDestinationChanges(preview.warnings).other) : { keys: [], other: [] });
  const accepted = $derived(importer.redirect);
  const writable = $derived(preview ? hostsToWrite(preview.hosts, accepted) : []);
  const updates = $derived(writable.filter((h) => h.update).length);
  const changes = $derived(preview ? preview.hosts.filter((h) => h.previous !== null).length : 0);
  const pendingChanges = $derived(changes - accepted.length);
  /** JSON exports number records by position, the others by line. */
  const unit = $derived((preview?.format ?? (view.kind === "done" ? view.report.formatLabel : "")).toLowerCase().includes("json") ? "#" : "line ");

  const address = (h: PlannedHost) => destinationText(h);
  const where = (n: ImportNotice) => [n.record !== null ? `${unit}${n.record}` : null, n.label && !n.label.startsWith("SHA256:") ? n.label : null].filter(Boolean).join(" · ");
  /** `~/…/portico/hosts.conf`: the folder that matters (already `~/…` from Rust). */
  const shortTarget = (path: string) => {
    const parts = path.split("/");
    return parts.length > 4 ? [parts[0], "…", ...parts.slice(-2)].join("/") : path;
  };

  // The passphrase goes away with the keys that needed it.
  $effect(() => {
    if (!askPassphrase && (passphrase || confirm)) {
      passphrase = "";
      confirm = "";
    }
  });

  // The footer changes with the view (picking → preview → done, error):
  // its default button takes the focus, so ↵ always does what it shows.
  // Keyed on the kind only: a replan (prefix toggle) keeps the focus where
  // it is.
  $effect(() => {
    if (!armed || (kind !== "preview" && kind !== "done" && kind !== "error")) return;
    queueMicrotask(() => document.querySelector<HTMLElement>(".modal footer [data-autofocus]")?.focus());
  });

  const canApply = $derived(!!preview && !busy && armed && writable.length > 0 && !mismatch);

  async function apply() {
    if (!canApply) return;
    try {
      await importer.apply(passphrase);
    } finally {
      // Crossed the IPC once: nothing keeps it here.
      passphrase = "";
      confirm = "";
    }
  }

  function close() {
    passphrase = "";
    confirm = "";
    importer.close();
  }

  async function copyInclude(line: string) {
    try {
      await navigator.clipboard.writeText(line);
      copied = true;
      setTimeout(() => (copied = false), COPIED_FEEDBACK_MS);
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
    // 1–9: tick or untick the n-th key.
    if (view.kind === "preview" && /^[1-9]$/.test(key)) {
      const k = view.preview.keys[Number(key) - 1];
      if (!k) return false;
      importer.toggleKey(k.id);
      return true;
    }
    if (view.kind === "done" && key === "c") {
      void copyInclude(view.report.includeLine);
      return true;
    }
    // ↵ outside a button (the focus was elsewhere): the primary action.
    if (key === "Enter") {
      if (view.kind === "preview") void apply();
      else if (view.kind === "done") close();
      else if (view.kind === "error") void importer.start();
      else return false;
      return true;
    }
    return false;
  }

  /** ↵ in a checkbox or a passphrase field imports, like everywhere else in the dialog. */
  function onEnter(e: KeyboardEvent) {
    if (e.key !== "Enter" || e.isComposing) return;
    e.preventDefault();
    void apply();
  }
</script>

{#snippet message(text: string)}
  {#each codeSpans(sentence(text)) as part, i (i)}{#if part.code}<span class="mono code">{part.text}</span>{:else}{part.text}{/if}{/each}
{/snippet}

{#snippet notices(title: string, list: ImportNotice[], tone: "muted" | "warning")}
  {#if list.length}
    <section>
      <h3>{title} <span class="count">{list.length}</span></h3>
      <ul class="notices" data-tone={tone}>
        {#each list as n, i (i)}
          {@const explain = noticeExplanation(n.code)}
          <li data-code={n.code}>
            <span class="where mono">{where(n)}</span>
            <span class="message">{@render message(n.message)}{#if explain}<span class="explain">{explain}</span>{/if}</span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}
{/snippet}

{#snippet caution(text: string, tone: "warning" | "note")}
  <p class="caution" data-tone={tone} role="note">
    <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 2.25 14.25 13.25H1.75Z" /><path d="M8 6.5v3M8 11.1v.1" /></svg>
    <span>{text}</span>
  </p>
{/snippet}

{#snippet keyRow(k: PlannedKey, n: number)}
  {@const on = selected.includes(k.id)}
  <li class:on>
    <label class="key">
      <input type="checkbox" checked={on} disabled={busy} onchange={() => importer.toggleKey(k.id)} onkeydown={onEnter} aria-describedby={`key-${n}-hosts`} />
      <span class="type">{keyType(k.algorithm)}</span>
      <span class="fp mono" title={k.fingerprint}>{shortFingerprint(k.fingerprint)}</span>
      <span class="badges">
        {#if !k.fingerprintVerified}<span class="badge warn" title="The export only claims this fingerprint: the key is written, never matched with a local one.">{UNVERIFIED_FINGERPRINT}</span>
        {:else if k.encrypted}<span class="badge">encrypted</span>{/if}
        {#if k.local}<span class="badge reuse" title={k.local}>already in ~/.ssh · reused</span>{/if}
      </span>
      {#if n <= 9}<kbd class="digit">{n}</kbd>{/if}
    </label>
    <span class="key-hosts" id={`key-${n}-hosts`}>Used by <span class="mono">{keyHosts(k.aliases)}</span></span>
  </li>
{/snippet}

<Modal bind:armed size="wide" footerWrap={false} onescape={close} {onkey} labelledBy="import-title">
  {#snippet title()}
    {dialogTitle(view.kind, source)}
    {#if preview}<span class="title-host mono">{preview.fileName}</span>{/if}
  {/snippet}

  {#if view.kind === "picking"}
    <p class="waiting"><span class="spinner" aria-hidden="true"></span> Choose a Termius or Termix export in the file dialog…</p>
    <p class="note">
      Termius: a CSV in the layout of its import template, a termius-local-export <span class="mono">hosts.json</span> or
      <span class="mono">credentials.csv</span>, or the output of <span class="mono">termius export-ssh-config</span>. Termix: its JSON
      export. The format is recognized from the content.
    </p>
  {:else if view.kind === "error"}
    <div class="inline-error" role="alert">
      <span>{sentence(view.error.message)}</span>
      {#if view.error.hint}<span class="hint">{view.error.hint}</span>{/if}
    </div>
  {:else if preview}
    <p class="summary">
      <strong>{plural(writable.length, "host")}</strong> to import from {plural(preview.records, "record")}{#if preview.skipped.length}, {preview.skipped.length} skipped{/if}{#if updates}, {updates} updated from an earlier import{/if}{#if pendingChanges > 0}, <span class="warn-text">{plural(pendingChanges, "destination change")} skipped unless accepted</span>{/if}
      <span class="muted">· {preview.format}</span>
    </p>
    <div class="prefix">
      <label class="check">
        <input type="checkbox" checked={importer.prefix} disabled={busy} onchange={(e) => void importer.setPrefix(e.currentTarget.checked)} />
        Prefix aliases with <span class="mono">{preview.prefixText}</span> (keeps both when a name is taken) <kbd>p</kbd>
      </label>
      {#if taken}<span class="taken">{taken} {taken === 1 ? "name" : "names"} already taken</span>{/if}
    </div>
    {#if preview.secretsAdvice}{@render caution(preview.secretsAdvice, "warning")}{/if}
    {#if preview.hosts.length}
      <section>
        <h3>Hosts <span class="count">{preview.hosts.length}</span></h3>
        <ul class="hosts">
          {#each preview.hosts as h (h.alias)}
            {@const on = accepted.includes(h.alias)}
            <li class:changed={h.previous !== null} class:held={h.previous !== null && !on} data-alias={h.alias}>
              <span class="alias mono">{h.alias}</span>
              <span class="address mono">{address(h)}</span>
              <span class="meta">
                {#if h.previous}<span class="badge warn">destination changed</span>
                {:else if h.update}<span class="state">update</span>{/if}
                {#if h.group}<span class="group">{h.group}</span>{/if}
                {#each h.tags as t (t)}<span class="tag">#{t}</span>{/each}
              </span>
              {#if h.previous}
                <div class="redirect">
                  <span class="move mono">
                    <span class="was">{destinationText(h.previous)}</span>
                    <svg viewBox="0 0 16 16" aria-label="becomes"><path d="M3 8h10M9.5 4.5 13 8l-3.5 3.5" /></svg>
                    <span class="now">{address(h)}</span>
                  </span>
                  <label class="check accept">
                    <input type="checkbox" checked={on} disabled={busy} onchange={() => importer.toggleRedirect(h.alias)} onkeydown={onEnter} />
                    Accept the new destination
                  </label>
                  <span class="redirect-help">
                    {#if on}{h.previous.identityFile ? `Pointed at the new server; its key ${h.previous.identityFile} is not kept.` : "Pointed at the new server."}
                    {:else}Skipped: <span class="mono">{h.alias}</span> keeps pointing at the server it was imported for.{/if}
                  </span>
                </div>
              {/if}
            </li>
          {/each}
        </ul>
      </section>
    {:else}
      <div class="empty">
        <strong>Nothing to import</strong>
        <span>Every record was skipped (see below). Try the <span class="mono">{preview.prefixText}</span> prefix (<kbd>p</kbd>) or another export.</span>
      </div>
    {/if}
    {#if preview.keys.length}
      <section class="keys-section" aria-labelledby="import-keys-title">
        <h3 id="import-keys-title">Private keys <span class="count">{preview.keys.length}</span></h3>
        <p class="keys-intro">None is imported unless you tick it. Without a key, the hosts use your agent and default keys.</p>
        <ul class="keys">
          {#each preview.keys as k, i (k.id)}{@render keyRow(k, i + 1)}{/each}
        </ul>
        {@render caution(
          `A ticked key is written to ${preview.keysDir}, readable only by you, and set as the IdentityFile of its hosts. A key already in ~/.ssh is reused instead.`,
          writing.length ? "warning" : "note",
        )}
        {#if askPassphrase}
          <div class="passphrase">
            <label class="field">
              <span>Passphrase for the written copies <span class="muted">(optional)</span></span>
              <input type="password" bind:value={passphrase} autocomplete="new-password" disabled={busy} onkeydown={onEnter} aria-invalid={!!mismatch} />
            </label>
            <label class="field">
              <span>Confirm</span>
              <input type="password" bind:value={confirm} autocomplete="new-password" disabled={busy} onkeydown={onEnter} aria-invalid={!!mismatch} />
            </label>
            {#if mismatch}
              <p class="inline-error pass-help" role="alert"><span>{mismatch}</span></p>
            {:else}
              <span class="help pass-help">Encrypts the keys that are not encrypted in the export. Empty: written without one. Never stored by Portico.</span>
            {/if}
          </div>
        {/if}
        {@render notices("Keys not offered", warnings.keys, "warning")}
      </section>
    {/if}
    {@render notices("Skipped", preview.skipped, "muted")}
    {@render notices("Warnings", warnings.other, "warning")}
    {#if preview.identityFiles.length}
      <section>
        <h3>Identity files not imported <span class="count">{preview.identityFiles.length}</span></h3>
        <ul class="notices" data-tone="muted">
          {#each preview.identityFiles as f, i (i)}<li><span class="message mono">{f}</span></li>{/each}
        </ul>
      </section>
    {/if}
    {#if importer.error}
      <div class="inline-error" role="alert">
        <span>{sentence(importer.error.message)}</span>
        {#if importer.error.hint}<span class="hint">{importer.error.hint}</span>{/if}
      </div>
    {/if}
  {:else if view.kind === "done"}
    {@const r = view.report}
    {@const keysLine = reportKeysLine(r)}
    {@const skipped = splitDestinationChanges(r.skipped)}
    {@const reportWarnings = splitDestinationChanges(r.warnings).other}
    <p class="summary">
      {#if r.unchanged}
        <strong>{plural(r.imported.length, "host")}</strong> already imported: nothing changed.
      {:else}
        <strong>{plural(r.imported.length, "host")}</strong> written to <span class="mono">{r.target}</span>{#if r.updated.length}, {r.updated.length} of them updated{/if}.
      {/if}
      {#if r.metaUpdated}<span class="muted">Groups, tags and notes updated for {plural(r.metaUpdated, "host")}.</span>{/if}
    </p>
    {#if r.updated.length}
      <section>
        <h3>Updated <span class="count">{r.updated.length}</span></h3>
        <p class="aliases mono">{r.updated.join(", ")}</p>
      </section>
    {/if}
    {#if keysLine}
      <section class="report-keys">
        <h3>Private keys</h3>
        <p class="summary">{keysLine}</p>
        {#if r.keys.imported.length || r.keys.reused.length}
          <ul class="paths">
            {#each r.keys.imported as path (path)}<li><span class="mono">{path}</span><span class="muted">written · 0600</span></li>{/each}
            {#each r.keys.reused as path (path)}<li><span class="mono">{path}</span><span class="muted">reused</span></li>{/each}
          </ul>
        {/if}
      </section>
    {/if}
    {@render notices("Destination changes skipped", skipped.changes, "warning")}
    {@render notices("Skipped", skipped.other, "muted")}
    {@render notices("Warnings", reportWarnings, "warning")}
    {#if r.includeActive}
      <p class="ok">~/.ssh/config already includes this file: the hosts are in your list.</p>
    {:else}
      <div class="field">
        <p class="step"><strong>Optional:</strong> for <span class="mono">ssh</span>, <span class="mono">scp</span> and <span class="mono">git</span> to see these hosts too, add this line at the top of <span class="mono">~/.ssh/config</span>. Portico never edits that file.</p>
        <div class="include">
          <input class="mono" readonly value={r.includeLine} bind:this={includeEl} aria-label="Include line" onfocus={(e) => e.currentTarget.select()} />
          <button class="secondary small" onclick={() => void copyInclude(r.includeLine)}>{copied ? "Copied" : "Copy"} <kbd>c</kbd></button>
        </div>
        <span class="help">The hosts are already in Portico's list{#if reload}{" "}(reload <kbd>{reload}</kbd> if not){/if}.</span>
      </div>
    {/if}
    {#if r.secretsAdvice}{@render caution(r.secretsAdvice, "warning")}{/if}
  {/if}

  {#snippet footer()}
    {#if view.kind === "done"}
      <button class="primary" data-autofocus onclick={close}>Done <kbd>↵</kbd></button>
    {:else if view.kind === "error"}
      <button class="secondary" onclick={close}>Close <kbd>Esc</kbd></button>
      <button class="primary" data-autofocus onclick={() => void importer.start()}>Choose another file <kbd>↵</kbd></button>
    {:else if preview}
      <span class="footer-note" title={preview.target}>
        {#if preview.replaces}
          Updates {plural(preview.replaces, "host")} of an earlier import in <span class="mono">{shortTarget(preview.target)}</span>
        {:else}
          Writes Portico's <span class="mono">{shortTarget(preview.target)}</span> · <span class="mono">~/.ssh/config</span> is never edited
        {/if}
      </span>
      <button class="secondary" onclick={close}>Cancel <kbd>Esc</kbd></button>
      <button class="primary" data-autofocus disabled={!canApply} onclick={() => void apply()}>
        {#if view.kind === "applying"}<span class="spinner" aria-hidden="true"></span>{/if}
        {applyLabel(preview, selected, accepted)}
        <kbd>↵</kbd>
      </button>
    {:else}
      <button class="secondary" onclick={close}>Cancel <kbd>Esc</kbd></button>
    {/if}
  {/snippet}
</Modal>

<style>
  .mono { font-family: var(--font-mono); font-size: var(--text-sm); }
  .code { color: var(--text-primary); }
  .muted { color: var(--text-muted); }
  .summary { color: var(--text-secondary); }
  .summary strong { color: var(--text-primary); font-weight: 550; }
  .waiting { display: flex; align-items: center; gap: var(--space-2); color: var(--text-primary); }
  .prefix { display: flex; flex-wrap: wrap; align-items: center; column-gap: var(--space-3); row-gap: var(--space-1); }
  .prefix .check { flex: 0 1 auto; }
  .taken { color: var(--text-secondary); font-size: var(--text-sm); }
  .empty { display: grid; gap: var(--space-1); padding: var(--space-3) 0; color: var(--text-secondary); }
  .empty strong { color: var(--text-primary); font-weight: 550; }
  .ok { color: var(--text-secondary); font-size: var(--text-sm); }
  .step { margin: 0; color: var(--text-primary); }
  .step strong { font-weight: 550; }
  .caution {
    display: flex;
    gap: var(--space-2);
    align-items: flex-start;
    margin: 0;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    color: var(--text-primary);
    font-size: var(--text-sm);
  }
  .caution[data-tone="warning"] { background: color-mix(in srgb, var(--status-warning) 10%, transparent); }
  .caution[data-tone="note"] { background: var(--bg-elevated); color: var(--text-secondary); }
  .caution svg {
    flex: 0 0 auto;
    width: var(--space-4);
    /* As tall as the first line of text: the icon sits centered on it. */
    height: 1lh;
    fill: none;
    stroke: var(--status-warning);
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .caution[data-tone="note"] svg { stroke: var(--text-muted); }
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
  /* One grid for every row: alias, address and chips line up across rows. */
  .hosts {
    display: grid;
    grid-template-columns: minmax(0, 2fr) minmax(0, 3fr) auto;
    column-gap: var(--space-3);
  }
  .hosts li {
    grid-column: 1 / -1;
    display: grid;
    grid-template-columns: subgrid;
    align-items: center;
    min-height: 34px;
    padding: 0 var(--space-2);
    border-radius: var(--radius-md);
  }
  .hosts li:hover { background: var(--bg-elevated); }
  .alias { color: var(--text-primary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .address { color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .meta { display: flex; gap: var(--space-1); justify-content: flex-end; flex-wrap: wrap; }
  /* Same chips as the host details. */
  .group, .tag, .state, .badge {
    padding: calc(var(--space-1) / 2) var(--space-2);
    color: var(--text-secondary);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .group, .state {
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    letter-spacing: 0.08em;
    text-transform: uppercase;
  }
  .tag, .badge {
    border-radius: var(--radius-pill);
    border: 1px solid var(--border-strong);
  }
  .keys-intro { margin: 0; color: var(--text-secondary); font-size: var(--text-sm); }
  .keys { display: grid; gap: calc(var(--space-1) / 2); }
  .keys li {
    display: grid;
    gap: calc(var(--space-1) / 2);
    padding: var(--space-1) var(--space-2) var(--space-2);
    border-radius: var(--radius-md);
    transition: background var(--motion-fast) var(--ease-standard);
  }
  .keys li:hover { background: var(--bg-elevated); }
  /* Ticked: the selection's surface and accent bar, as in lists. */
  .keys li.on { background: var(--bg-selected); box-shadow: inset 2px 0 0 var(--accent); }
  .key {
    display: grid;
    grid-template-columns: auto minmax(7ch, auto) minmax(0, 1fr) auto auto;
    align-items: center;
    column-gap: var(--space-3);
    min-height: 34px;
    cursor: pointer;
  }
  .key input { margin: 0; accent-color: var(--accent); }
  .type { color: var(--text-primary); font-weight: 550; }
  .fp { color: var(--text-secondary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .badges { display: flex; gap: var(--space-1); justify-content: flex-end; flex-wrap: wrap; }
  .badge.reuse { color: var(--text-primary); border-color: var(--accent); }
  /* Meaning, so the status color: a destination change, an unverified fingerprint. */
  .badge.warn { color: var(--status-warning); border-color: var(--status-warning); }
  .warn-text { color: var(--status-warning); }
  .hosts li.changed { padding-block: var(--space-1); box-shadow: inset 2px 0 0 var(--status-warning); }
  .hosts li.held .alias, .hosts li.held .address { color: var(--text-muted); }
  .redirect {
    grid-column: 1 / -1;
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    column-gap: var(--space-4);
    row-gap: var(--space-1);
    padding: var(--space-1) 0 var(--space-1);
  }
  .move { display: inline-flex; align-items: center; gap: var(--space-2); min-width: 0; }
  .move .was { color: var(--text-secondary); }
  .move .now { color: var(--text-primary); }
  .move svg {
    flex: 0 0 auto;
    width: var(--space-4);
    height: var(--space-4);
    fill: none;
    stroke: var(--text-muted);
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .accept { font-size: var(--text-sm); }
  .redirect-help { flex-basis: 100%; color: var(--text-muted); font-size: var(--text-xs); }
  .redirect-help .mono { font-size: inherit; }
  .digit { justify-self: end; }
  /* Under the type, after the checkbox column. */
  .key-hosts {
    padding-left: calc(var(--space-3) + var(--space-4));
    color: var(--text-muted);
    font-size: var(--text-sm);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .key-hosts .mono { color: var(--text-secondary); font-size: inherit; }
  .passphrase {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--space-2) var(--space-4);
    padding: var(--space-2) 0 0;
  }
  .pass-help { grid-column: 1 / -1; }
  @media (max-width: 720px) {
    .passphrase { grid-template-columns: minmax(0, 1fr); }
    .key { grid-template-columns: auto minmax(7ch, auto) minmax(0, 1fr) auto; }
    .badges { grid-column: 2 / -1; justify-content: flex-start; }
    .digit { display: none; }
  }
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
  .explain { display: block; color: var(--text-muted); font-size: var(--text-xs); }
  .aliases { margin: 0; padding: 0 var(--space-2); color: var(--text-secondary); }
  .paths li {
    display: flex;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-1) var(--space-2);
    font-size: var(--text-sm);
  }
  .paths .mono { color: var(--text-primary); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .include { display: flex; gap: var(--space-2); align-items: center; }
  .include input { flex: 1; min-width: 0; }
  .check kbd { margin-left: var(--space-1); }
  /* The note gives way to the buttons: it wraps, they never do. */
  .footer-note { flex: 1 1 auto; min-width: 0; }
  .footer-note .mono { font-size: inherit; }
</style>
