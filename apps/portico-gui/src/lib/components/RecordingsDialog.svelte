<script lang="ts">
  // Recordings: the files of the recordings folder (newest first), played
  // in a read-only player, deleted after an armed confirmation.
  // Keys: j/k ↑↓ move · ↵ play · Del (or x) delete · Ctrl-R refresh · Esc close.
  // A recording still being written cannot be deleted: refused up front
  // (`deleteRefusal`), its Delete button is disabled.
  //
  // The file name is always shown next to the title: a third-party file
  // sets any title it likes. Requests carry the listed `name` only.
  import { fly } from "svelte/transition";
  import { formatSize, sentence } from "../api";
  import { dur } from "../motion";
  import { DELETE_RECORDING_TEXT, formatDuration, formatWhen, INVALID_RECORDING_TEXT, playerKey } from "../recording";
  import { keymap } from "../state/keymap.svelte";
  import { recordings } from "../state/recordings.svelte";
  import Modal from "./Modal.svelte";
  import PlayerView from "./PlayerView.svelte";
  import RecBadge from "./RecBadge.svelte";

  let { themeId }: { themeId: string } = $props();

  let listEl: HTMLDivElement | undefined = $state();
  let player: PlayerView | undefined = $state();
  let armed = $state(false);

  const current = $derived(recordings.list[recordings.selected]);
  const active = $derived(recordings.activeNames);
  const recordKey = $derived(keymap.label("session.record"));
  const now = new Date();

  $effect(() => {
    void recordings.selected;
    listEl?.querySelector(".row.selected")?.scrollIntoView({ block: "nearest" });
  });

  /** Player keys; `q` goes back to the list like Esc. */
  function playerKeys(k: string): boolean {
    if (playerKey(k) === "close") {
      recordings.backToList();
      return true;
    }
    return player?.key(k) ?? false;
  }

  function act(key: string): boolean {
    switch (key) {
      case "Enter":
        recordings.play(current);
        return true;
      case "Delete":
      case "x":
        recordings.askDelete(current);
        return true;
      default:
        return false;
    }
  }

  function onListKey(e: KeyboardEvent) {
    const last = Math.max(recordings.list.length - 1, 0);
    if (e.key === "ArrowDown" || (e.key === "j" && !e.ctrlKey)) recordings.selected = Math.min(recordings.selected + 1, last);
    else if (e.key === "ArrowUp" || (e.key === "k" && !e.ctrlKey)) recordings.selected = Math.max(recordings.selected - 1, 0);
    else if (e.key === "Home") recordings.selected = 0;
    else if (e.key === "End") recordings.selected = last;
    else if (e.ctrlKey && !e.metaKey && e.key.toLowerCase() === "r") void recordings.refresh(true);
    else return;
    e.preventDefault();
  }

  function confirmDelete() {
    if (armed) void recordings.confirmDelete();
  }
</script>

{#if recordings.view.kind === "player"}
  {@const r = recordings.view.recording}
  {#key r.name}
    <Modal size="full" scroll={false} onescape={() => recordings.backToList()} onkey={playerKeys} labelledBy="player-title">
      {#snippet title()}{r.alias}<span class="title-host mono">· {r.name}</span>{/snippet}
      <PlayerView bind:this={player} recording={r} {themeId} />
      {#snippet footer()}
        <span class="footer-note">Read-only playback · links, clipboard and replies are off</span>
        <button class="secondary" onclick={() => recordings.backToList()}>Back to list <kbd>Esc</kbd></button>
      {/snippet}
    </Modal>
  {/key}
{:else if recordings.view.kind === "delete"}
  {@const v = recordings.view}
  <Modal
    bind:armed
    onescape={() => recordings.backToList()}
    onkey={(k) => (k === "y" ? (confirmDelete(), true) : k === "n" ? (recordings.backToList(), true) : false)}
    labelledBy="recording-delete-title"
  >
    {#snippet title()}Delete this recording?{/snippet}
    <p>{DELETE_RECORDING_TEXT}</p>
    <dl>
      <dt>Title</dt><dd>{v.recording.alias}</dd>
      <dt>File</dt><dd>{v.recording.name}</dd>
      <dt>Size</dt><dd>{formatSize(v.recording.size)}</dd>
    </dl>
    {#if v.error}
      <div class="inline-error" role="alert"><span>{sentence(v.error.message)}</span>{#if v.error.hint}<span class="hint">{v.error.hint}</span>{/if}</div>
    {/if}
    {#snippet footer()}
      <button class="secondary" data-autofocus onclick={() => recordings.backToList()}>Keep <kbd>n</kbd></button>
      <button class="danger" disabled={!armed || v.busy} onclick={confirmDelete}>Delete <kbd>y</kbd></button>
    {/snippet}
  </Modal>
{:else}
  <Modal size="wide" scroll={false} footerWrap={false} onescape={() => recordings.close()} onkey={act} labelledBy="recordings-title">
    {#snippet title()}Recordings<span class="title-hint"><kbd>Ctrl-R</kbd> refresh</span>{/snippet}

    <div class="lists" bind:this={listEl} tabindex="0" role="listbox" aria-label="Recordings" data-autofocus onkeydown={onListKey}>
      {#if recordings.listState === "loading"}
        {#each [0, 1, 2] as i (i)}<div class="skeleton" style:animation-delay="calc({i} * var(--motion-fast))"></div>{/each}
      {:else if recordings.listState === "error" && recordings.listError}
        <div class="state error">
          <strong>{sentence(recordings.listError.message)}</strong>
          {#if recordings.listError.hint}<span>{recordings.listError.hint}</span>{/if}
        </div>
      {:else if !recordings.list.length}
        <div class="state">
          <strong>No recordings yet</strong>
          <span
            >In a terminal, press {#if recordKey}<kbd>{recordKey}</kbd>{/if} (Record session): what the terminal shows is saved until you stop
            it.</span
          >
        </div>
      {:else}
        {#each recordings.list as r, i (r.name)}
          {@const writing = active.has(r.name)}
          <div
            class="row"
            class:selected={recordings.selected === i}
            class:invalid={!r.valid}
            role="option"
            aria-selected={recordings.selected === i}
            tabindex="-1"
            onclick={() => (recordings.selected = i)}
            ondblclick={() => recordings.play(r)}
            onkeydown={() => {}}
            in:fly={{ y: 6, duration: dur("base") }}
          >
            <div class="line">
              <span class="name">{r.alias}</span>
              {#if writing}<RecBadge compact role="img" title="Still being written" />{/if}
              {#if !r.valid}<span class="badge" data-tone="warning" title="Not an asciicast v2 file, or damaged">invalid</span>{/if}
              <span class="when">{formatWhen(r.started, now)}</span>
            </div>
            <div class="line sub">
              <span class="file mono">{r.name}</span>
              <span class="num mono">{formatDuration(r.duration)}</span>
              <span class="num mono">{formatSize(r.size)}</span>
              {#if r.width && r.height}<span class="num mono">{r.width}×{r.height}</span>{/if}
            </div>
          </div>
        {/each}
      {/if}
    </div>
    {#if recordings.listState === "ready" && current && !current.valid}
      <p class="invalid-note" role="status">{INVALID_RECORDING_TEXT}</p>
    {/if}

    {#snippet footer()}
      <!-- The folder (display string from Rust) gives way first: an ellipsis
           at its start keeps the end of the path in view. -->
      <span class="footer-note folder" title={recordings.folder ?? undefined}>
        {#if recordings.folder}<span class="path mono"><bdi>{recordings.folder}</bdi></span><span aria-hidden="true">·</span>{/if}<span class="rest">readable only by you · asciicast v2</span>
      </span>
      {#if current}
        <button
          class="secondary small"
          disabled={active.has(current.name)}
          title={active.has(current.name) ? "Stop the recording first" : undefined}
          onclick={() => recordings.askDelete(current)}>Delete <kbd>Del</kbd></button
        >
        <button class="primary" disabled={!current.valid} onclick={() => recordings.play(current)}>Play <kbd>↵</kbd></button>
      {:else}
        <button class="secondary" onclick={() => recordings.close()}>Close <kbd>Esc</kbd></button>
      {/if}
    {/snippet}
  </Modal>
{/if}

<style>
  /* width 0: its text never widens the dialog (grid min-content); it takes the room left. */
  .folder { display: flex; flex: 1 1 auto; width: 0; min-width: 0; gap: var(--space-1); white-space: nowrap; overflow: hidden; }
  .folder .rest { flex: none; }
  .folder .path { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; direction: rtl; text-align: left; }
  .lists {
    display: grid;
    align-content: start;
    gap: var(--space-1);
    flex: 1 1 auto;
    min-height: 0;
    overflow-y: auto;
    outline: none;
    border-radius: var(--radius-md);
  }
  .lists:focus-visible { outline: none; }
  /* The selected row shows the bar and the selected background, focused or
     not; the ring is only for a row focused without being selected. */
  .row:focus-visible:not(.selected) { outline: none; box-shadow: inset 0 0 0 var(--size-bar) var(--accent); }
  .title-hint {
    float: right;
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    color: var(--text-muted);
    font-size: var(--text-xs);
    font-weight: 450;
    line-height: normal;
    margin-top: var(--space-1);
  }
  .row {
    position: relative;
    display: grid;
    gap: var(--size-line-gap);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    cursor: default;
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
    width: var(--size-bar);
    border-radius: var(--radius-pill);
    background: var(--accent);
  }
  .line { display: flex; align-items: center; gap: var(--space-2); min-width: 0; }
  .name { color: var(--text-primary); font-weight: 550; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .row.invalid .name { color: var(--text-secondary); }
  .when { margin-left: auto; flex: none; color: var(--text-muted); font-size: var(--text-xs); font-variant-numeric: tabular-nums; }
  .sub { color: var(--text-muted); font-size: var(--text-xs); }
  .file { flex: 0 1 auto; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; color: var(--text-secondary); }
  .num { flex: none; font-variant-numeric: tabular-nums; }
  .sub > * + *::before { content: "·"; margin-right: var(--space-2); color: var(--text-muted); }
  .badge {
    --c: var(--text-muted);
    display: inline-flex;
    flex: none;
    align-items: center;
    height: var(--space-5);
    padding: 0 var(--space-2);
    border: 1px solid color-mix(in srgb, var(--c) 45%, transparent);
    border-radius: var(--radius-pill);
    color: var(--c);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  .badge[data-tone="warning"] { --c: var(--status-warning); }
  .invalid-note { flex: none; margin: 0; padding: 0 var(--space-3); color: var(--status-warning); font-size: var(--text-sm); }
  .state { display: grid; gap: var(--space-1); padding: var(--space-2) var(--space-3); color: var(--text-secondary); font-size: var(--text-sm); }
  .state strong { color: var(--text-primary); font-weight: 550; font-size: var(--text-base); }
  .state.error strong { color: var(--status-error); }
  .skeleton {
    height: var(--space-7);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    animation: shimmer var(--motion-loop) var(--ease-standard) infinite;
  }
  @keyframes shimmer { 50% { opacity: 0.45; } }
</style>
