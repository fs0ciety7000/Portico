<script lang="ts" module>
  import type { Status } from "../api";

  export interface Command {
    id: string;
    title: string;
    hint?: string;
    /** The hint is technical data (`user@host`): mono. */
    mono?: boolean;
    /** A status dot before the title (host picker). */
    status?: { status: Status; busy: boolean };
    /** A short note after the title ("reuses connection"). */
    note?: string;
    shortcut?: string;
    run: () => void;
  }
</script>

<script lang="ts">
  import { fade, scale } from "svelte/transition";
  import { dur } from "../motion";

  let {
    open = $bindable(false),
    commands,
    label = "Command palette",
    placeholder = "Type a command…",
    empty = "No command found",
  }: {
    open?: boolean;
    commands: Command[];
    /** Accessible name (the palette, or a picker built on it). */
    label?: string;
    placeholder?: string;
    /** Shown when nothing matches. */
    empty?: string;
  } = $props();

  let query = $state("");
  let active = $state(0);
  let input: HTMLInputElement | undefined = $state();

  const filtered = $derived(
    commands.filter((c) => c.title.toLowerCase().includes(query.trim().toLowerCase())),
  );

  $effect(() => {
    if (open) {
      query = "";
      active = 0;
      queueMicrotask(() => input?.focus());
    }
  });

  function run(cmd: Command | undefined) {
    if (!cmd) return;
    open = false;
    cmd.run();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Escape") open = false;
    else if (e.key === "ArrowDown") active = Math.min(active + 1, filtered.length - 1);
    else if (e.key === "ArrowUp") active = Math.max(active - 1, 0);
    else if (e.key === "Enter") run(filtered[active]);
    else return;
    e.preventDefault();
  }
</script>

{#if open}
  <div class="scrim" transition:fade={{ duration: dur("fast") }} onclick={() => (open = false)} role="presentation"></div>
  <div class="palette" role="dialog" aria-modal="true" aria-label={label} transition:scale={{ start: 0.97, duration: dur("base") }}>
    <input bind:this={input} bind:value={query} {onkeydown} {placeholder} spellcheck="false" oninput={() => (active = 0)} />
    <ul>
      {#each filtered as cmd, i (cmd.id)}
        <li class:active={i === active} onmouseenter={() => (active = i)} onclick={() => run(cmd)} onkeydown={() => {}} role="option" aria-selected={i === active}>
          {#if cmd.status}<span class="dot" data-status={cmd.status.status} class:busy={cmd.status.busy}></span>{/if}
          <span class="title">{cmd.title}{#if cmd.note}<span class="note">{cmd.note}</span>{/if}</span>
          {#if cmd.hint}<span class="hint" class:mono={cmd.mono}>{cmd.hint}</span>{/if}
          {#if cmd.shortcut}<kbd>{cmd.shortcut}</kbd>{/if}
        </li>
      {:else}
        <li class="empty">{empty}</li>
      {/each}
    </ul>
  </div>
{/if}

<style>
  .scrim {
    position: fixed;
    inset: 0;
    background: color-mix(in srgb, var(--bg-base) 55%, transparent);
    backdrop-filter: blur(2px);
    z-index: 10;
  }
  .palette {
    position: fixed;
    top: 14vh;
    left: 50%;
    width: min(560px, calc(100vw - 32px));
    transform: translateX(-50%);
    z-index: 11;
    background: var(--bg-overlay);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-lg);
    box-shadow: var(--shadow-overlay);
    overflow: hidden;
  }
  input {
    width: 100%;
    height: 48px;
    padding: 0 var(--space-4);
    border: 0;
    border-bottom: 1px solid var(--border-subtle);
    outline: 0;
    background: transparent;
    color: var(--text-primary);
    font: inherit;
    font-size: var(--text-md);
  }
  ul { list-style: none; margin: 0; padding: var(--space-2); max-height: calc(36px * 8 + var(--space-2) * 2); overflow-y: auto; }
  li {
    display: flex;
    align-items: center;
    gap: var(--space-3);
    height: 36px;
    padding: 0 var(--space-3);
    border-radius: var(--radius-md);
  }
  li.active { background: var(--bg-selected); }
  .title { flex: 1; min-width: 0; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .note { margin-left: var(--space-2); color: var(--text-muted); font-size: var(--text-xs); }
  .hint { flex: 0 1 auto; min-width: 0; overflow: hidden; color: var(--text-muted); font-size: var(--text-sm); text-overflow: ellipsis; white-space: nowrap; }
  .hint.mono { font-family: var(--font-mono); font-size: var(--text-xs); }
  .dot {
    --c: var(--status-idle);
    flex: none;
    width: var(--size-dot);
    height: var(--size-dot);
    border-radius: 50%;
    background: var(--c);
  }
  .dot[data-status="connecting"] { --c: var(--status-connecting); }
  .dot[data-status="authenticating"] { --c: var(--status-authenticating); }
  .dot[data-status="connected"] { --c: var(--status-connected); }
  .dot[data-status="warning"] { --c: var(--status-warning); }
  .dot[data-status="error"] { --c: var(--status-error); }
  .dot.busy { animation: pulse var(--motion-loop) var(--ease-standard) infinite; }
  @keyframes pulse {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--c) 60%, transparent); }
    100% { box-shadow: 0 0 0 4px transparent; }
  }
  .empty { color: var(--text-muted); }
</style>
