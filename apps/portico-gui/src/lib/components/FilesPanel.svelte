<script lang="ts">
  // SFTP explorer docked beside the terminal of the active session.
  // Keyboard (same grammar as the TUI): j/k ↑↓ move · ↵ open folder /
  // download file · ⌫ parent · n new folder · r F2 rename · d Del F8 delete ·
  // u upload · . hidden · Ctrl-R refresh · x cancel the latest transfer ·
  // Esc back to the terminal. Names shown are `display` (made safe in Rust).
  // Until the session is connected the panel shows its status instead.
  import { onMount } from "svelte";
  import { fly } from "svelte/transition";
  import { flip } from "svelte/animate";
  import { describe, formatEta, formatSize, displayPath, sentence, type RemoteEntry } from "../api";
  import { dur } from "../motion";
  import { crumbs, displaySafe, files, type Transfer } from "../state/files.svelte";
  import { isLive, workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";

  let {
    session,
    alias,
    onclose,
    onreconnect,
  }: { session: number; alias: string; onclose: () => void; onreconnect: () => void } = $props();

  const info = $derived(workspace.sessions[session]);
  const connected = $derived(info?.phase.phase === "connected");
  const status = $derived(describe(info));
  /** Why the session is not usable (failed, closed), with what to do. */
  const ended = $derived.by(() => {
    if (!info || isLive(info)) return null;
    if (info.phase.phase === "failed") return info.phase.error;
    return { code: "session_closed", message: "The connection to this host is closed", hint: "Reconnect to browse its files again." };
  });

  /** A changed host key is a hard refusal: never offer to connect again. */
  const refused = $derived(ended?.code === "host_key_changed");

  // Opening mutates state: never inside a $derived. Only a connected
  // session is asked for its files.
  $effect(() => {
    if (connected) files.open(session, alias);
  });
  const ex = $derived(files.explorers[session]);
  const shown = $derived(ex ? files.visible(ex) : []);
  const path = $derived(ex ? crumbs(ex.cwd) : []);
  const queue = $derived(files.transfers.filter((t) => t.session === session));
  const running = $derived(queue.filter((t) => t.status === "queued" || t.status === "running").length);

  let list: HTMLDivElement | undefined = $state();
  let crumbsEl: HTMLElement | undefined = $state();
  // A deep path overflows the bar: keep its end (the current folder) in view.
  $effect(() => {
    void path;
    if (crumbsEl) crumbsEl.scrollLeft = crumbsEl.scrollWidth;
  });
  /** Inline editor: new folder, or rename of `target`. */
  let editing: { kind: "mkdir" } | { kind: "rename"; target: RemoteEntry } | null = $state(null);
  let draft = $state("");
  let confirmDelete: RemoteEntry | null = $state(null);

  export function focus() {
    list?.focus();
  }

  /** Palette entry points (same as the panel's keys). */
  export function run(action: "upload" | "mkdir" | "rename" | "delete" | "refresh" | "hidden" | "cancel") {
    if (!ex?.cwd && action !== "hidden" && action !== "cancel") return;
    if (action === "upload") void files.pickAndUpload(session);
    else if (action === "mkdir") startEdit("mkdir");
    else if (action === "rename") startEdit("rename");
    else if (action === "delete") {
      if (selectedEntry) confirmDelete = selectedEntry;
    } else if (action === "refresh") files.refresh(session);
    else if (action === "hidden") files.showHidden = !files.showHidden;
    else cancelLatest();
  }

  /** The most recent transfer still moving. */
  function cancelLatest() {
    const t = [...queue].reverse().find((x) => x.status === "running" || x.status === "queued");
    if (t) files.cancel(t.id);
  }

  // The panel takes the keyboard when it appears.
  onMount(() => {
    requestAnimationFrame(() => list?.focus());
  });

  const selectedEntry = $derived(shown.find((e) => e.path === ex?.selected));

  // Keep the selected row in view.
  $effect(() => {
    const sel = ex?.selected;
    if (!sel || !list) return;
    const row = list.querySelector<HTMLElement>(`[data-path="${CSS.escape(sel)}"]`);
    row?.scrollIntoView({ block: "nearest" });
  });

  /** Focuses (and selects) an inline editor as soon as it exists. */
  function takeFocus(node: HTMLInputElement) {
    // Synchronous: keys typed right after `n` / F2 must land here.
    node.focus();
    node.select();
  }

  function startEdit(kind: "mkdir" | "rename") {
    if (kind === "rename") {
      if (!selectedEntry) return;
      editing = { kind, target: selectedEntry };
      // A name that needed cleaning for display is not offered for editing.
      draft = selectedEntry.display === selectedEntry.name ? selectedEntry.name : "";
    } else {
      editing = { kind };
      draft = "";
    }
  }

  function validName(name: string): boolean {
    return !!name && name !== "." && name !== ".." && !name.includes("/") && !/[\u0000-\u001f\u007f]/.test(name);
  }

  async function commitEdit() {
    const e = editing;
    const name = draft.trim();
    editing = null;
    if (!e || !validName(name)) {
      if (e && name) {
        workspace.notify({ code: "invalid_name", message: "A name cannot contain / or be . or ..", hint: null });
      }
      list?.focus();
      return;
    }
    if (e.kind === "mkdir") await files.mkdir(session, name);
    else await files.rename(session, e.target, name);
    list?.focus();
  }

  function editKey(e: KeyboardEvent) {
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      void commitEdit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      editing = null;
      list?.focus();
    }
  }

  function onkeydown(e: KeyboardEvent) {
    const key = e.key;
    const handled = () => {
      e.preventDefault();
      e.stopPropagation();
    };
    // Ctrl-R refreshes the panel here (elsewhere it reloads ssh_config).
    if (e.ctrlKey && !e.metaKey && !e.altKey && key.toLowerCase() === "r") return (handled(), files.refresh(session));
    if (e.metaKey || e.ctrlKey || e.altKey) return;
    if (!connected) {
      if (key === "Enter" && ended && !refused) (handled(), onreconnect());
      else if (key === "Escape") (handled(), onclose());
      return;
    }
    if (key === "j" || key === "ArrowDown") (handled(), files.move(session, 1));
    else if (key === "k" || key === "ArrowUp") (handled(), files.move(session, -1));
    else if (key === "Home") (handled(), files.move(session, -1e9));
    else if (key === "End") (handled(), files.move(session, 1e9));
    else if (key === "Enter" || key === "ArrowRight") {
      handled();
      if (selectedEntry) files.activate(session, selectedEntry);
    } else if (key === "Backspace" || key === "ArrowLeft") (handled(), files.up(session));
    else if (key === "n") (handled(), startEdit("mkdir"));
    else if (key === "F2" || key === "r") (handled(), startEdit("rename"));
    else if (key === "Delete" || key === "F8" || key === "d") {
      handled();
      if (selectedEntry) confirmDelete = selectedEntry;
    } else if (key === "u") (handled(), void files.pickAndUpload(session));
    else if (key === ".") (handled(), (files.showHidden = !files.showHidden));
    else if (key === "x") (handled(), cancelLatest());
    else if (key === "Escape") (handled(), onclose());
  }

  async function deleteConfirmed(target: RemoteEntry) {
    confirmDelete = null;
    await files.remove(session, target);
    list?.focus();
  }

  const DAY = 86_400_000;
  const MONTHS = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
  function when(secs: number | null): string {
    if (secs === null) return "";
    const d = new Date(secs * 1000);
    const age = Date.now() - d.getTime();
    if (age >= 0 && age < 60_000) return "just now";
    if (age >= 0 && age < 3_600_000) return `${Math.floor(age / 60_000)} min ago`;
    if (age >= 0 && age < DAY) return `${Math.floor(age / 3_600_000)} h ago`;
    const day = `${d.getDate()} ${MONTHS[d.getMonth()]}`;
    return d.getFullYear() === new Date().getFullYear() ? day : `${day} ${d.getFullYear()}`;
  }

  function percent(t: Transfer): number | null {
    return t.total ? Math.min(100, (t.done / t.total) * 100) : null;
  }

  function detail(t: Transfer): string {
    if (t.status === "queued") return "Starting…";
    if (t.status === "done") return formatSize(t.total ?? t.done);
    if (t.status === "skipped") return "Skipped";
    if (t.status === "cancelled") return "Cancelled";
    if (t.status === "failed") return t.error?.message ?? "Failed";
    const parts = [t.total ? `${formatSize(t.done)} of ${formatSize(t.total)}` : formatSize(t.done)];
    if (t.rate > 0) {
      parts.push(`${formatSize(t.rate)}/s`);
      if (t.total) parts.push(formatEta((t.total - t.done) / t.rate));
    }
    return parts.join(" · ");
  }
</script>

<aside class="files" aria-label="Files on {alias}" transition:fly={{ x: 16, duration: dur("slow") }}>
  <header>
    <div class="heading">
      <h2>Files</h2>
      <span class="alias">{alias}</span>
    </div>
    <div class="tools">
      <button class="icon" title="Upload files (u)" aria-label="Upload files" onclick={() => files.pickAndUpload(session)} disabled={!ex?.cwd}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 11V3M4.5 6.5 8 3l3.5 3.5M3 13h10" /></svg>
      </button>
      <button class="icon" title="New folder (n)" aria-label="New folder" onclick={() => startEdit("mkdir")} disabled={!ex?.cwd}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M2 4.5h4l1.5 1.5H14v6.5H2zM8 8v3M6.5 9.5h3" /></svg>
      </button>
      <button class="icon" title="Refresh (Ctrl-R)" aria-label="Refresh" onclick={() => files.refresh(session)} disabled={!ex?.cwd}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M13 8a5 5 0 1 1-1.5-3.55M13 3v3h-3" /></svg>
      </button>
      <button class="icon" title="Close (Esc)" aria-label="Close files" onclick={onclose}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4 4l8 8M12 4l-8 8" /></svg>
      </button>
    </div>
  </header>

  <nav class="crumbs mono" aria-label="Path" bind:this={crumbsEl}>
    {#each path as c, i (c.path)}
      {#if i > 1}<span class="sep">/</span>{/if}
      <button class:current={i === path.length - 1} onclick={() => (files.go(session, c.path), list?.focus())}>{c.label}</button>
    {/each}
    {#if ex?.loading && ex.entries.length}<span class="busy" aria-label="Loading"></span>{/if}
  </nav>

  <div
    class="list"
    role="listbox"
    aria-label="Remote files"
    tabindex="0"
    bind:this={list}
    {onkeydown}
  >
    {#if editing?.kind === "mkdir"}
      <div class="row editing">
        <svg class="kind" viewBox="0 0 16 16" aria-hidden="true"><path d="M2 4.5h4l1.5 1.5H14v6.5H2z" /></svg>
        <input bind:value={draft} placeholder="Folder name" aria-label="New folder name" spellcheck="false" use:takeFocus onkeydown={editKey} onblur={() => (editing = null)} />
      </div>
    {/if}

    {#if ended || ex?.error?.code === "session_ended"}
      {@const err = ended ?? ex?.error}
      <div class="state error" role="alert">
        <strong>{sentence(err?.message ?? "")}</strong>
        {#if err?.hint}<span>{err.hint}</span>{/if}
        {#if !refused}
          <div class="actions">
            <button class="secondary" onclick={onreconnect}>Reconnect <kbd>↵</kbd></button>
          </div>
        {/if}
      </div>
    {:else if !connected}
      <div class="state connecting" data-status={status.status} aria-live="polite">
        <span class="dot" class:busy={status.busy}></span>
        <strong>{status.label}</strong>
        <span>Files appear once the session is ready.</span>
      </div>
    {:else if ex?.error}
      <div class="state error" role="alert">
        <strong>{sentence(ex.error.message)}</strong>
        {#if ex.error.hint}<span>{ex.error.hint}</span>{/if}
        <div class="actions">
          <button class="secondary" onclick={() => files.refresh(session)}>Retry <kbd>Ctrl-R</kbd></button>
          {#if ex.cwd && ex.cwd !== "/"}<button class="secondary" onclick={() => files.up(session)}>Parent folder <kbd>⌫</kbd></button>{/if}
        </div>
      </div>
    {:else if ex?.loading && !ex.entries.length}
      <div class="skeleton" aria-label="Loading">
        {#each [72, 54, 86, 40, 64, 48] as w, i (i)}
          <div class="bar" style:--w="{w}%" style:--i={i}></div>
        {/each}
      </div>
    {:else if !shown.length && editing?.kind !== "mkdir"}
      <div class="state empty">
        <svg viewBox="0 0 32 32" aria-hidden="true"><path d="M4 9h9l3 3h12v13H4z" /></svg>
        <strong>{ex?.entries.length ? "Only hidden files here" : "This folder is empty"}</strong>
        <span>{ex?.entries.length ? "Press . to show them." : "Drop files here or upload them."}</span>
        {#if !ex?.entries.length}
          <button class="secondary" onclick={() => files.pickAndUpload(session)}>Upload files <kbd>u</kbd></button>
        {/if}
      </div>
    {:else}
      {#each shown as entry (entry.path)}
        <div
          class="row"
          class:selected={entry.path === ex?.selected && editing?.kind !== "mkdir"}
          role="option"
          aria-selected={entry.path === ex?.selected}
          tabindex="-1"
          data-path={entry.path}
          onclick={() => {
            if (ex) ex.selected = entry.path;
            list?.focus();
          }}
          ondblclick={() => files.activate(session, entry)}
          onkeydown={() => {}}
          animate:flip={{ duration: dur("base") }}
        >
          <svg class="kind" class:dir={entry.kind === "dir"} viewBox="0 0 16 16" aria-hidden="true">
            {#if entry.kind === "dir"}<path d="M2 4.5h4l1.5 1.5H14v6.5H2z" />
            {:else if entry.kind === "symlink"}<path d="M6.5 9.5 9.5 6.5M7 4.5l1-1a2.5 2.5 0 0 1 3.5 3.5l-1 1M9 11.5l-1 1A2.5 2.5 0 0 1 4.5 9l1-1" />
            {:else}<path d="M4 2h5l3 3v9H4zM9 2v3h3" />{/if}
          </svg>
          {#if editing?.kind === "rename" && editing.target.path === entry.path}
            <input bind:value={draft} aria-label="New name" spellcheck="false" use:takeFocus onkeydown={editKey} onblur={() => (editing = null)} />
          {:else}
            <span class="name" title={entry.display}>{entry.display}{entry.kind === "dir" ? "/" : ""}</span>
            <span class="meta">
              {#if entry.kind === "file" || entry.kind === "other"}<span class="size mono">{formatSize(entry.size)}</span>{/if}
              <span class="date">{when(entry.modified)}</span>
            </span>
            <span class="row-actions">
              {#if entry.kind === "file"}
                <button class="icon small" tabindex="-1" title="Download (↵)" aria-label="Download {entry.display}" onclick={(e) => (e.stopPropagation(), files.download(session, entry))}>
                  <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 3v8M4.5 7.5 8 11l3.5-3.5M3 13h10" /></svg>
                </button>
              {/if}
              <button class="icon small" tabindex="-1" title="Rename (r)" aria-label="Rename {entry.display}" onclick={(e) => (e.stopPropagation(), ex && (ex.selected = entry.path), startEdit("rename"))}>
                <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 13h2.5L12.5 6 10 3.5 3 10.5zM9 4.5 11.5 7" /></svg>
              </button>
              <button class="icon small" tabindex="-1" title="Delete (d)" aria-label="Delete {entry.display}" onclick={(e) => (e.stopPropagation(), (confirmDelete = entry))}>
                <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M3 4.5h10M6.5 4.5V3h3v1.5M4.5 4.5l.5 8.5h6l.5-8.5" /></svg>
              </button>
            </span>
          {/if}
        </div>
      {/each}
    {/if}

    {#if files.dragging && ex?.cwd}
      <div class="drop" transition:fly={{ y: 6, duration: dur("fast") }}>
        <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M8 11V3M4.5 6.5 8 3l3.5 3.5M3 13h10" /></svg>
        <strong>Drop to upload</strong>
        <span class="mono">{displaySafe(ex.cwd)}</span>
      </div>
    {/if}
  </div>

  <footer>
    {#if queue.length}
      <div class="queue-head">
        <span>{running ? `Transferring ${running}` : "Transfers"}</span>
        {#if queue.length > running}<button class="link" onclick={() => files.clearDone()}>Clear finished</button>{/if}
      </div>
      <ul class="queue">
        {#each queue.slice(-4) as t (t.id)}
          {@const pct = percent(t)}
          <li data-status={t.status} in:fly={{ y: 6, duration: dur("base") }}>
            <span class="dir" aria-label={t.direction}>{t.direction === "upload" ? "↑" : "↓"}</span>
            <div class="t-main">
              <div class="t-line">
                <span class="t-name">{t.name}</span>
                {#if t.status === "running" && pct !== null}<span class="pct mono">{Math.floor(pct)}%</span>{/if}
                {#if t.status === "done"}<svg class="ok" viewBox="0 0 16 16" aria-label="Done"><path d="M3.5 8.5 6.5 11.5 12.5 4.5" /></svg>{/if}
              </div>
              {#if t.status === "running" || t.status === "queued"}
                <div class="track" class:indeterminate={pct === null}><div class="fill" style:transform="scaleX({(pct ?? 0) / 100})"></div></div>
              {/if}
              <span class="t-detail">{detail(t)}</span>
            </div>
            {#if t.status === "running" || t.status === "queued"}
              <button class="icon small" title="Cancel (x)" aria-label="Cancel {t.name}" onclick={() => files.cancel(t.id)}>
                <svg viewBox="0 0 16 16" aria-hidden="true"><path d="M4.5 4.5l7 7M11.5 4.5l-7 7" /></svg>
              </button>
            {/if}
          </li>
        {/each}
      </ul>
    {/if}
    <div class="dest">
      <span>Downloads to <span class="mono">{files.downloadDir ? displayPath(files.downloadDir) : "Downloads"}</span></span>
      <button class="link" onclick={() => files.chooseDownloadDir()}>Change</button>
    </div>
  </footer>
</aside>

{#if confirmDelete}
  {@const target = confirmDelete}
  <Modal
    tone="danger"
    onescape={() => ((confirmDelete = null), list?.focus())}
    onkey={(k) => {
      if (k === "y") void deleteConfirmed(target);
      else if (k === "n") (confirmDelete = null), list?.focus();
      return k === "y" || k === "n";
    }}
  >
    {#snippet title()}Delete {target.kind === "dir" ? "folder" : "file"}?{/snippet}
    <p><span class="mono">{target.display}</span> will be removed from <strong>{alias}</strong>. This cannot be undone.</p>
    {#if target.kind === "dir"}<p>Only empty folders can be deleted.</p>{/if}
    {#snippet footer()}
      <!-- Focus on Keep: Enter never deletes, `y` does. -->
      <button class="secondary" data-autofocus onclick={() => ((confirmDelete = null), list?.focus())}>Keep <kbd>Esc</kbd></button>
      <button class="danger" onclick={() => deleteConfirmed(target)}>Delete <kbd>y</kbd></button>
    {/snippet}
  </Modal>
{/if}

<style>
  .files {
    display: grid;
    grid-template-rows: auto auto minmax(0, 1fr) auto;
    min-height: 0;
    background: var(--bg-surface);
    border-left: 1px solid var(--border-subtle);
  }
  header {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-3) var(--space-2) var(--space-4);
  }
  .heading { display: flex; align-items: baseline; gap: var(--space-2); min-width: 0; }
  h2 { margin: 0; font-size: var(--text-md); font-weight: 600; }
  .alias { color: var(--text-muted); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .tools { display: flex; gap: var(--space-1); }
  button.icon {
    display: grid;
    place-items: center;
    width: 28px;
    height: 28px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-secondary);
    transition: background var(--motion-fast) var(--ease-standard), color var(--motion-fast) var(--ease-standard);
  }
  button.icon.small { width: 24px; height: 24px; }
  button.icon:hover:not(:disabled) { background: var(--bg-elevated); color: var(--text-primary); }
  button.icon:disabled { opacity: 0.4; }
  button.icon svg, .kind, .state svg, .drop svg, .ok {
    width: 16px;
    height: 16px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: var(--size-line-gap);
    padding: 0 var(--space-3) var(--space-2) var(--space-3);
    border-bottom: 1px solid var(--border-subtle);
    font-size: var(--text-sm);
    overflow-x: auto;
    scrollbar-width: none;
    white-space: nowrap;
  }
  .crumbs button {
    unicode-bidi: isolate;
    padding: 2px var(--space-1);
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-muted);
    font: inherit;
    transition: color var(--motion-fast) var(--ease-standard), background var(--motion-fast) var(--ease-standard);
  }
  .crumbs button:hover { color: var(--text-primary); background: var(--bg-elevated); }
  .crumbs button.current { color: var(--text-primary); }
  .sep { color: var(--border-strong); }
  .busy {
    width: 6px;
    height: 6px;
    margin-left: var(--space-2);
    border-radius: 50%;
    background: var(--accent);
    animation: breathe var(--motion-loop) var(--ease-standard) infinite;
  }
  .list {
    position: relative;
    overflow-y: auto;
    padding: var(--space-2);
    outline: none;
    scrollbar-width: thin;
  }
  /* With rows, focus shows as the selection's accent bar; without, on the
     action the keyboard would trigger (Reconnect, Retry, Upload). */
  .list:focus-visible .state .actions button:first-child,
  .list:focus-visible .state.empty button {
    box-shadow: 0 0 0 2px var(--accent);
  }
  .row {
    position: relative;
    display: grid;
    grid-template-columns: 16px minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-3);
    height: 34px;
    padding: 0 var(--space-2) 0 var(--space-3);
    border-radius: var(--radius-md);
    color: var(--text-secondary);
    cursor: default;
    transition: background var(--motion-fast) var(--ease-standard);
  }
  .row:hover { background: var(--bg-elevated); }
  .row.selected { background: var(--bg-selected); color: var(--text-primary); }
  .list:focus .row.selected::before {
    content: "";
    position: absolute;
    left: 0;
    top: 8px;
    bottom: 8px;
    width: var(--size-bar);
    border-radius: 2px;
    background: var(--accent);
  }
  .kind { color: var(--text-muted); }
  .kind.dir { color: var(--text-secondary); }
  .name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .row:has(.kind.dir) .name { color: var(--text-primary); font-weight: 550; }
  .meta {
    display: grid;
    grid-template-columns: 8ch 11ch;
    gap: var(--space-3);
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .size { grid-column: 1; text-align: right; }
  .date { grid-column: 2; text-align: right; }
  .row-actions {
    position: absolute;
    right: var(--space-2);
    display: flex;
    gap: var(--size-line-gap);
    padding-left: var(--space-3);

    opacity: 0;
    transition: opacity var(--motion-fast) var(--ease-standard);
  }
  .row:hover .row-actions, .row:focus-within .row-actions { opacity: 1; }
  /* The actions take the place of size and date. */
  .meta { transition: opacity var(--motion-fast) var(--ease-standard); }
  .row:hover .meta { opacity: 0; }
  .row.editing { background: var(--bg-selected); grid-template-columns: 16px 1fr; }
  .row input {
    width: 100%;
    height: 26px;
    padding: 0 var(--space-2);
    border: 1px solid var(--accent);
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font: inherit;
    outline: none;
  }
  .state {
    display: grid;
    justify-items: center;
    gap: var(--space-2);
    padding: var(--space-7) var(--space-5);
    text-align: center;
    color: var(--text-muted);
  }
  .state strong { color: var(--text-primary); font-weight: 550; }
  .state svg { width: 32px; height: 32px; stroke-width: 1.25; color: var(--border-strong); }
  .state.error { justify-items: start; text-align: left; padding: var(--space-5) var(--space-3); }
  .state.error strong { color: var(--status-error); }
  .state.connecting { --c: var(--status-connecting); }
  .state.connecting[data-status="authenticating"] { --c: var(--status-authenticating); }
  .state.connecting[data-status="idle"] { --c: var(--status-idle); }
  .state.connecting strong { color: var(--text-primary); }
  .state .dot { width: 8px; height: 8px; border-radius: 50%; background: var(--c); }
  .state .dot.busy { animation: breathe var(--motion-loop) var(--ease-standard) infinite; }
  .state .actions { display: flex; gap: var(--space-2); margin-top: var(--space-2); }
  button.secondary {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: 28px;
    padding: 0 var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-primary);
    font: inherit;
    transition: background var(--motion-fast) var(--ease-standard);
  }
  button.secondary:hover { background: var(--bg-elevated); }
  .skeleton { display: grid; gap: var(--space-3); padding: var(--space-3); }
  .bar {
    width: var(--w);
    height: 12px;
    border-radius: var(--radius-sm);
    background: var(--bg-elevated);
    animation: breathe var(--motion-loop) var(--ease-standard) infinite;
    animation-delay: calc(var(--i) * var(--motion-fast) * 0.6);
  }
  @keyframes breathe {
    0%, 100% { opacity: 0.45; }
    50% { opacity: 1; }
  }
  .drop {
    position: absolute;
    inset: var(--space-2);
    display: grid;
    place-content: center;
    justify-items: center;
    gap: var(--space-2);
    border: 1px dashed var(--accent);
    border-radius: var(--radius-lg);
    background: var(--bg-overlay);
    color: var(--accent);
    pointer-events: none;
  }
  .drop strong { color: var(--text-primary); font-weight: 550; }
  .drop .mono { color: var(--text-muted); font-size: var(--text-sm); unicode-bidi: isolate; }
  footer {
    display: grid;
    gap: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border-top: 1px solid var(--border-subtle);
    font-size: var(--text-sm);
  }
  .queue-head { display: flex; justify-content: space-between; color: var(--text-secondary); font-weight: 550; }
  .queue { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
  .queue li { display: grid; grid-template-columns: 16px minmax(0, 1fr) auto; align-items: start; gap: var(--space-2); }
  .queue .dir { color: var(--text-muted); text-align: center; }
  .t-main { display: grid; gap: 4px; min-width: 0; }
  .t-line { display: flex; justify-content: space-between; gap: var(--space-2); }
  .t-name { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-primary); }
  .pct { color: var(--text-secondary); font-size: var(--text-xs); }
  .t-detail { color: var(--text-muted); font-size: var(--text-xs); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  li[data-status="failed"] .t-detail { color: var(--status-error); }
  li[data-status="done"] .ok { color: var(--status-connected); }
  .track { height: 3px; overflow: hidden; border-radius: var(--radius-pill); background: var(--bg-elevated); }
  .fill {
    height: 100%;
    background: var(--accent);
    transform-origin: left;
    transition: transform var(--motion-base) var(--ease-standard);
  }
  .track.indeterminate .fill {
    width: 30%;
    transform: none !important;
    animation: slide var(--motion-loop) var(--ease-standard) infinite;
  }
  @keyframes slide {
    0% { translate: -100% 0; }
    100% { translate: 340% 0; }
  }
  .dest { display: flex; justify-content: space-between; gap: var(--space-2); color: var(--text-muted); }
  .dest .mono { color: var(--text-secondary); }
  button.link {
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--accent);
    font: inherit;
  }
  button.link:hover { color: var(--accent-strong); }
</style>
