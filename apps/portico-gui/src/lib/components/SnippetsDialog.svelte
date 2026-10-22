<script lang="ts">
  // Snippets: filterable list (scope shown), run in the active terminal
  // (placeholders form, errors inline), editor, delete confirmation.
  // List keys: j/k ↑↓ move · / filter · ↵ run · n new · e edit · d delete
  // (Ctrl-N / Ctrl-E / Ctrl-D everywhere, the filter included).
  // A paste-only multi-line snippet the core refused (`snippet_needs_paste_mode`)
  // runs only with `r`, after the dialog re-arms: never with the Enter that
  // caused the refusal (same grammar as the TUI).
  import { fly } from "svelte/transition";
  import { sentence, type ErrorSummary, type Snippet, type SnippetDraft } from "../api";
  import { dur } from "../motion";
  import { isMac } from "../keys";
  import { emptyDraft, NEEDS_PASTE_MODE, scopeLabel, snippets, type SnippetTarget } from "../state/snippets.svelte";
  import Modal from "./Modal.svelte";

  let { target }: { target: SnippetTarget | null } = $props();

  // ── List ──
  let filterEl: HTMLInputElement | undefined = $state();
  let listEl: HTMLDivElement | undefined = $state();

  const shown = $derived.by(() => {
    const q = snippets.filter.trim().toLowerCase();
    const list = q ? snippets.all.filter((s) => `${s.name} ${s.command} ${s.description ?? ""}`.toLowerCase().includes(q)) : snippets.all;
    // Applicable first, file order otherwise.
    return [...list].sort((a, b) => Number(applies(b)) - Number(applies(a)));
  });
  const current = $derived(shown[snippets.selected]);
  const canRun = $derived(!!target && target.terminal !== null);

  function applies(s: Snippet): boolean {
    return snippets.applicable.includes(s.id);
  }

  $effect(() => {
    void snippets.selected;
    listEl?.querySelector(".row.selected")?.scrollIntoView({ block: "nearest" });
  });

  function startRun(s: Snippet | undefined) {
    if (!s || !canRun || !applies(s)) return;
    if (s.placeholders.length) snippets.view = { kind: "run", snippet: s, error: null };
    else void runNow(s, {}, null);
  }

  function act(k: string): boolean {
    switch (k) {
      case "Enter":
        startRun(current);
        return true;
      case "n":
        draft = emptyDraft(snippets.alias);
        snippets.view = { kind: "edit", draft: { ...draft } };
        return true;
      case "e":
        if (current) {
          const { placeholders: _p, ...rest } = $state.snapshot(current);
          snippets.view = { kind: "edit", draft: rest };
        }
        return true;
      case "d":
        if (current) {
          deleteError = null;
          snippets.view = { kind: "delete", snippet: current };
        }
        return true;
      case "/":
        filterEl?.focus();
        return true;
      default:
        return false;
    }
  }

  function onListKey(e: KeyboardEvent) {
    const last = Math.max(shown.length - 1, 0);
    if (e.key === "ArrowDown" || e.key === "j") snippets.selected = Math.min(snippets.selected + 1, last);
    else if (e.key === "ArrowUp" || e.key === "k") snippets.selected = Math.max(snippets.selected - 1, 0);
    else if (e.key === "Home") snippets.selected = 0;
    else if (e.key === "End") snippets.selected = last;
    else return;
    e.preventDefault();
  }

  /** `Ctrl-N` / `Ctrl-E` / `Ctrl-D`: aliases of n / e / d, from anywhere in
   * the list view (the filter field included). */
  function onListShortcut(e: KeyboardEvent): boolean {
    if (!e.ctrlKey || e.metaKey || e.altKey || e.shiftKey) return false;
    const k = e.key.toLowerCase();
    return k === "n" || k === "e" || k === "d" ? act(k) : false;
  }

  function onFilterKey(e: KeyboardEvent) {
    if (e.key === "ArrowDown") snippets.selected = Math.min(snippets.selected + 1, Math.max(shown.length - 1, 0));
    else if (e.key === "ArrowUp") snippets.selected = Math.max(snippets.selected - 1, 0);
    else if (e.key === "Enter") startRun(current);
    else if (e.key === "Escape" && snippets.filter) {
      snippets.filter = "";
      e.stopPropagation();
    } else if (e.key === "Escape") {
      listEl?.focus();
      e.stopPropagation();
    } else return;
    e.preventDefault();
  }

  const preview = (s: Snippet) => {
    const lines = s.command.split("\n");
    return lines.length > 1 ? `${lines[0]} …` : s.command;
  };

  // ── Run ──
  let values: Record<string, string> = $state({});
  let running = $state(false);
  /** The run dialog is armed (re-armed after each error). */
  let runArmed = $state(false);

  async function runNow(s: Snippet, vals: Record<string, string>, run: boolean | null) {
    if (running) return;
    running = true;
    const error = await snippets.run(target, s, vals, run);
    running = false;
    if (!error) {
      values = {};
      snippets.close();
    } else {
      snippets.view = { kind: "run", snippet: s, error };
    }
  }

  function onRunKey(e: KeyboardEvent, s: Snippet, pasteMode: boolean) {
    if (e.key === "Enter" && !(e.target instanceof HTMLButtonElement)) {
      e.preventDefault();
      // After a paste-mode refusal, Enter (what was refused) does nothing.
      if (!pasteMode) void runNow(s, $state.snapshot(values), null);
    }
  }

  /** The placeholder field an error is about, when it can be told from the
   * values themselves (empty, or holding a line break / control character);
   * null when it cannot. Never read from the message text. */
  function invalidField(s: Snippet, error: ErrorSummary | null): string | null {
    if (!error) return null;
    const bad = (v: string | undefined) =>
      // eslint-disable-next-line no-control-regex
      error.code === "snippet_missing_value" ? !v : error.code === "snippet_value" && /[\u0000-\u001f\u007f]/.test(v ?? "");
    const fields = s.placeholders.filter((name) => bad(values[name]));
    return fields.length === 1 ? (fields[0] ?? null) : null;
  }

  // ── Edit ──
  let draft: SnippetDraft = $state(emptyDraft(null));
  let scopeText = $state("");
  let saveError: ErrorSummary | null = $state(null);
  let saving = $state(false);

  $effect(() => {
    const v = snippets.view;
    if (v.kind === "edit") {
      draft = v.draft;
      scopeText = v.draft.scope.kind === "global" ? "" : v.draft.scope.values.join(", ");
      saveError = null;
    }
  });

  function setScope(kind: "global" | "hosts" | "tags") {
    draft.scope = kind === "global" ? { kind } : { kind, values: [] };
  }

  async function save() {
    if (saving) return;
    const values = scopeText.split(/[\s,]+/).map((v) => v.replace(/^#/, "")).filter(Boolean);
    const scope = draft.scope.kind === "global" ? draft.scope : { kind: draft.scope.kind, values };
    saving = true;
    saveError = await snippets.save({ ...$state.snapshot(draft), scope, description: draft.description?.trim() || null });
    saving = false;
  }

  function onEditKey(e: KeyboardEvent) {
    const saveKey = e.key.toLowerCase() === "s" && (e.ctrlKey || (isMac && e.metaKey)) && !e.altKey && !e.shiftKey;
    const submit =
      saveKey || (e.key === "Enter" && ((isMac ? e.metaKey : e.ctrlKey) || !(e.target instanceof HTMLTextAreaElement || e.target instanceof HTMLButtonElement)));
    if (submit) {
      e.preventDefault();
      void save();
    }
  }

  // ── Delete ──
  let deleteError: ErrorSummary | null = $state(null);
  async function confirmDelete(s: Snippet) {
    deleteError = await snippets.remove(s);
    snippets.selected = Math.min(snippets.selected, Math.max(shown.length - 1, 0));
  }

  const back = () => (snippets.view = { kind: "list" });
</script>

{#if snippets.view.kind === "run"}
  {@const s = snippets.view.snippet}
  {@const error = snippets.view.error}
  {@const pasteMode = error?.code === NEEDS_PASTE_MODE}
  {@const invalid = invalidField(s, error)}
  <!-- A new error remounts the dialog: it re-arms (400 ms without a key)
       before `r` can run the refused snippet. -->
  {#key error}
    <Modal
      bind:armed={runArmed}
      onescape={back}
      onkey={(k) =>
        pasteMode
          ? k === "r" && !running
            ? (void runNow(s, $state.snapshot(values), true), true)
            : false
          : k === "Enter" && canRun
            ? (void runNow(s, $state.snapshot(values), null), true)
            : false}
      labelledBy="snippet-run-title"
    >
      {#snippet title()}{pasteMode || s.run ? "Run" : "Paste"} {s.name}{#if target}<span class="title-host">· {target.alias}</span>{/if}{/snippet}
      <div class="form" role="presentation" onkeydown={(e) => onRunKey(e, s, pasteMode)}>
        <pre class="command mono">{s.command}</pre>
        {#if !pasteMode}
          {#each s.placeholders as name, i (name)}
            <label class="field">
              <span class="mono">{`{{${name}}}`}</span>
              <!-- svelte-ignore a11y_autofocus -->
              <input class="mono" data-autofocus={i === 0 ? "" : undefined} bind:value={values[name]} spellcheck="false" autocomplete="off" aria-invalid={invalid === name} />
            </label>
          {/each}
        {/if}
        {#if pasteMode && error}
          <!-- A safeguard, not a failure. -->
          <div class="guard" role="status" in:fly={{ y: 6, duration: dur("base") }}>
            <strong>{sentence(error.message)}</strong>{#if error.hint}<span>{error.hint}</span>{/if}
          </div>
        {:else if error}
          <div class="inline-error" role="alert" in:fly={{ y: 6, duration: dur("base") }}>
            <span>{sentence(error.message)}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}
          </div>
        {/if}
        {#if pasteMode || !s.placeholders.length}<div class="focus-sink" data-autofocus tabindex="-1" role="presentation"></div>{/if}
      </div>
      {#snippet footer()}
        <span class="footer-note">{pasteMode || s.run ? "Runs every line (Enter is sent)" : "Pastes it, without Enter"}</span>
        <button class="secondary" onclick={back}>Cancel <kbd>Esc</kbd></button>
        {#if pasteMode}
          <button class="secondary" disabled={running || !runArmed} onclick={() => runNow(s, $state.snapshot(values), true)}>Run it <kbd>r</kbd></button>
        {:else}
          <button class="primary" disabled={running || !canRun} onclick={() => runNow(s, $state.snapshot(values), null)}>{s.run ? "Run" : "Paste"} <kbd>↵</kbd></button>
        {/if}
      {/snippet}
    </Modal>
  {/key}
{:else if snippets.view.kind === "edit"}
  <Modal size="wide" onescape={back} labelledBy="snippet-edit-title">
    {#snippet title()}{draft.id ? "Edit snippet" : "New snippet"}{/snippet}
    <div class="form" role="presentation" onkeydown={onEditKey}>
      <label class="field">
        <span>Name</span>
        <input data-autofocus bind:value={draft.name} spellcheck="false" autocomplete="off" />
      </label>
      <label class="field">
        <span>Command</span>
        <textarea bind:value={draft.command} spellcheck="false" autocomplete="off" rows="5"></textarea>
        <span class="help"><span class="mono">{"{{name}}"}</span> asks for a value before running · several lines are sent as one paste</span>
      </label>
      <label class="field">
        <span>Description <span class="optional">optional</span></span>
        <input value={draft.description ?? ""} oninput={(e) => (draft.description = e.currentTarget.value)} spellcheck="false" autocomplete="off" />
      </label>
      <div class="field">
        <span>Available on</span>
        <div class="scope">
          <div class="segmented" role="group" aria-label="Scope">
            <button type="button" aria-pressed={draft.scope.kind === "global"} onclick={() => setScope("global")}>Every host</button>
            <button type="button" aria-pressed={draft.scope.kind === "hosts"} onclick={() => setScope("hosts")}>Hosts</button>
            <button type="button" aria-pressed={draft.scope.kind === "tags"} onclick={() => setScope("tags")}>Tags</button>
          </div>
          {#if draft.scope.kind !== "global"}
            <input class="mono" bind:value={scopeText} placeholder={draft.scope.kind === "hosts" ? "web-01, staging" : "prod, db"} spellcheck="false" autocomplete="off" aria-label={draft.scope.kind === "hosts" ? "Host aliases" : "Tags"} />
          {/if}
        </div>
      </div>
      <label class="check">
        <input type="checkbox" bind:checked={draft.run} />
        <span>Run it (send Enter). Unchecked, the command is pasted for you to review.</span>
      </label>
      {#if saveError}
        <div class="inline-error" role="alert"><span>{sentence(saveError.message)}</span>{#if saveError.hint}<span class="hint">{saveError.hint}</span>{/if}</div>
      {/if}
    </div>
    {#snippet footer()}
      <button class="secondary" onclick={back}>Cancel <kbd>Esc</kbd></button>
      <button class="primary" disabled={saving || !draft.name.trim() || !draft.command.trim()} onclick={save}>Save <kbd>{isMac ? "⌘S" : "Ctrl-S"}</kbd></button>
    {/snippet}
  </Modal>
{:else if snippets.view.kind === "delete"}
  {@const s = snippets.view.snippet}
  <Modal onescape={back} onkey={(k) => (k === "y" ? (void confirmDelete(s), true) : k === "n" ? (back(), true) : false)} labelledBy="snippet-delete-title">
    {#snippet title()}Delete “{s.name}”?{/snippet}
    <p>It is removed from <span class="mono">snippets.toml</span>. This cannot be undone.</p>
    <pre class="command mono">{s.command}</pre>
    {#if deleteError}
      <div class="inline-error" role="alert"><span>{sentence(deleteError.message)}</span>{#if deleteError.hint}<span class="hint">{deleteError.hint}</span>{/if}</div>
    {/if}
    {#snippet footer()}
      <button class="secondary" data-autofocus onclick={back}>Keep <kbd>n</kbd></button>
      <button class="danger" onclick={() => confirmDelete(s)}>Delete <kbd>y</kbd></button>
    {/snippet}
  </Modal>
{:else}
  <Modal size="wide" onescape={() => snippets.close()} onkey={act} onshortcut={onListShortcut} labelledBy="snippets-title">
    {#snippet title()}Snippets{#if snippets.alias}<span class="title-host">· {snippets.alias}</span>{/if}{/snippet}

    <label class="filter">
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></svg>
      <input bind:this={filterEl} bind:value={snippets.filter} onkeydown={onFilterKey} oninput={() => (snippets.selected = 0)} placeholder="Filter snippets" spellcheck="false" autocomplete="off" aria-label="Filter snippets" />
      <kbd>/</kbd>
    </label>

    {#if !canRun}
      <div class="banner" role="status">
        <strong>No open terminal</strong>
        <span>
          {#if snippets.alias}<kbd>Esc</kbd>, then <kbd>↵</kbd> to {target ? "reconnect" : "connect"} <span class="mono">{snippets.alias}</span>.
          {:else}Open a terminal tab to run a snippet.{/if}
          You can still create and edit snippets.
        </span>
      </div>
    {/if}

    <div class="list" bind:this={listEl} tabindex="0" role="listbox" aria-label="Snippets" data-autofocus onkeydown={onListKey}>
      {#if snippets.state === "loading"}
        {#each [0, 1, 2] as i (i)}<div class="skeleton" style:animation-delay="calc({i} * var(--motion-fast))"></div>{/each}
      {:else if snippets.state === "error" && snippets.error}
        <div class="state error"><strong>{sentence(snippets.error.message)}</strong>{#if snippets.error.hint}<span>{snippets.error.hint}</span>{/if}</div>
      {:else if !snippets.all.length}
        <div class="state"><strong>No snippets yet</strong><span>Save the commands you type often with <kbd>n</kbd>; <span class="mono">{"{{placeholders}}"}</span> are asked for at run time.</span></div>
      {:else if !shown.length}
        <div class="state"><strong>No snippet matches “{snippets.filter}”</strong></div>
      {:else}
        {#each shown as s, i (s.id)}
          {@const ok = applies(s)}
          <div class="row" class:selected={snippets.selected === i} class:dim={!ok} role="option" aria-selected={snippets.selected === i} tabindex="-1" onclick={() => (snippets.selected = i)} ondblclick={() => startRun(s)} onkeydown={() => {}}>
            <div class="line">
              <span class="name">{s.name}</span>
              <span class="badge" class:global={s.scope.kind === "global"}>{scopeLabel(s)}</span>
              {#if !s.run}<span class="badge">paste only</span>{/if}
              {#if !ok && snippets.alias}<span class="not-here">not for {snippets.alias}</span>{/if}
            </div>
            <div class="line sub">
              <span class="cmd mono">{preview(s)}</span>
              {#if s.description}<span class="desc">{s.description}</span>{/if}
            </div>
          </div>
        {/each}
      {/if}
    </div>

    {#snippet footer()}
      {#if current}
        <button class="secondary" onclick={() => act("d")}>Delete <kbd>d</kbd></button>
        <button class="secondary" onclick={() => act("e")}>Edit <kbd>e</kbd></button>
      {/if}
      <button class="secondary" onclick={() => act("n")}>New <kbd>n</kbd></button>
      <button class="primary" disabled={!current || !canRun || !applies(current)} title={!canRun ? "Open a terminal first" : undefined} onclick={() => act("Enter")}>
        {current && !current.run ? "Paste" : "Run"} <kbd>↵</kbd>
      </button>
    {/snippet}
  </Modal>
{/if}

<style>
  .form { display: grid; gap: var(--space-3); }
  .filter {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 34px;
    padding: 0 var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-muted);
  }
  .filter:focus-within { border-color: var(--accent); }
  .filter svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; }
  .filter input { flex: 1; min-width: 0; border: 0; outline: 0; background: transparent; color: var(--text-primary); font: inherit; }
  .banner {
    display: grid;
    gap: 2px;
    padding: var(--space-2) var(--space-3);
    border-left: 2px solid var(--border-strong);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    font-size: var(--text-sm);
  }
  .banner strong { color: var(--text-primary); font-weight: 550; }
  .list { display: grid; gap: 2px; outline: none; border-radius: var(--radius-md); }
  /* Focus shows on the selected row, not around the whole list. */
  .list:focus-visible { outline: none; }
  .list:focus-visible .row.selected { box-shadow: inset 0 0 0 2px var(--accent); }
  .row {
    position: relative;
    display: grid;
    gap: 2px;
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
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
    width: 2px;
    border-radius: var(--radius-pill);
    background: var(--accent);
  }
  .row.dim .name, .row.dim .cmd { color: var(--text-muted); }
  .line { display: flex; align-items: center; gap: var(--space-2); min-width: 0; }
  .name { color: var(--text-primary); font-weight: 550; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .sub { font-size: var(--text-xs); color: var(--text-muted); }
  .cmd { color: var(--text-secondary); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; font-size: var(--text-sm); }
  .desc { white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
  .desc::before { content: "·"; margin-right: var(--space-2); }
  .not-here { color: var(--text-muted); font-size: var(--text-xs); }
  .badge {
    display: inline-flex;
    align-items: center;
    height: var(--space-5);
    padding: 0 var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    color: var(--text-secondary);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .badge.global { color: var(--text-muted); border-color: var(--border-subtle); }
  .state { display: grid; gap: var(--space-1); padding: var(--space-3); color: var(--text-secondary); font-size: var(--text-sm); }
  .state strong { color: var(--text-primary); font-weight: 550; font-size: var(--text-base); }
  .state.error strong { color: var(--status-error); }
  .skeleton {
    height: 44px;
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    animation: shimmer var(--motion-loop) var(--ease-standard) infinite;
  }
  @keyframes shimmer { 50% { opacity: 0.45; } }
  .command {
    margin: 0;
    padding: var(--space-3) var(--space-4);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font-size: var(--text-sm);
    white-space: pre-wrap;
    overflow-wrap: anywhere;
    max-height: calc(8 * 1.5em);
    overflow-y: auto;
    user-select: text;
  }
  .guard {
    display: grid;
    gap: var(--space-1);
    padding: var(--space-3) var(--space-4);
    border: 1px solid color-mix(in srgb, var(--status-warning) 45%, transparent);
    border-left: 2px solid var(--status-warning);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--status-warning) 6%, var(--bg-elevated));
    font-size: var(--text-sm);
  }
  .guard strong { color: var(--status-warning); font-weight: 550; }
  .guard span { color: var(--text-secondary); }
  .scope { display: flex; flex-wrap: wrap; gap: var(--space-2); align-items: center; }
  .scope input { flex: 1; min-width: 180px; }
  .optional { color: var(--text-muted); font-weight: 450; }
  .focus-sink { position: absolute; width: 1px; height: 1px; padding: 0 !important; border: 0; opacity: 0; }
  :global(.modal button.danger) {
    border: 0;
    background: var(--status-error);
    color: var(--text-on-accent);
    font-weight: 600;
  }
</style>
