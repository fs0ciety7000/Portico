<script module lang="ts">
  /** A row of the menu; `separator` draws a rule. */
  export interface MenuItem {
    id: string;
    label?: string;
    shortcut?: string | undefined;
    disabled?: boolean;
  }
</script>

<script lang="ts">
  // Right-click menu of a terminal pane (phase 9): drawn from the tokens
  // (no OS menu), keyboard navigable (↑ ↓, Home / End, ↵, Esc), closed by
  // a click outside, the window losing focus or the focus leaving it.
  // Placed at the pointer, kept inside the pane.
  import { onMount } from "svelte";
  import { scale } from "svelte/transition";
  import { dur } from "../motion";

  let {
    x,
    y,
    items,
    onpick,
    onclose,
  }: { x: number; y: number; items: MenuItem[]; onpick: (id: string) => void; onclose: () => void } = $props();

  let menu: HTMLDivElement;
  let left = $state(0);
  let top = $state(0);

  const enabled = () => [...menu.querySelectorAll<HTMLButtonElement>("button:not([disabled])")];

  function move(step: number) {
    const list = enabled();
    if (!list.length) return;
    const at = list.indexOf(document.activeElement as HTMLButtonElement);
    const next = at < 0 ? (step > 0 ? 0 : list.length - 1) : (at + step + list.length) % list.length;
    list[next]?.focus();
  }

  function onkeydown(e: KeyboardEvent) {
    // The menu has the keyboard: nothing reaches the terminal or the app.
    e.stopPropagation();
    const keys: Record<string, () => void> = {
      ArrowDown: () => move(1),
      ArrowUp: () => move(-1),
      Home: () => enabled()[0]?.focus(),
      End: () => enabled().at(-1)?.focus(),
      Escape: onclose,
      Tab: onclose,
    };
    const run = keys[e.key];
    if (run) {
      e.preventDefault();
      run();
    }
  }

  onMount(() => {
    // Inside the pane: flip left / up when the pointer is near an edge.
    const parent = menu.offsetParent as HTMLElement | null;
    const w = menu.offsetWidth;
    const h = menu.offsetHeight;
    const pw = parent?.clientWidth ?? w;
    const ph = parent?.clientHeight ?? h;
    left = x + w > pw ? Math.max(0, x - w) : x;
    top = y + h > ph ? Math.max(0, y - h) : y;
    enabled()[0]?.focus();
    const outside = (e: PointerEvent) => {
      if (!menu.contains(e.target as Node)) onclose();
    };
    const blur = () => onclose();
    window.addEventListener("pointerdown", outside, true);
    window.addEventListener("blur", blur);
    return () => {
      window.removeEventListener("pointerdown", outside, true);
      window.removeEventListener("blur", blur);
    };
  });
</script>

<div
  class="menu"
  role="menu"
  tabindex="-1"
  data-testid="terminal-menu"
  bind:this={menu}
  style:left={`${left}px`}
  style:top={`${top}px`}
  {onkeydown}
  onfocusout={(e) => {
    if (!menu.contains(e.relatedTarget as Node | null) && e.relatedTarget !== null) onclose();
  }}
  oncontextmenu={(e) => e.preventDefault()}
  transition:scale={{ start: 0.97, duration: dur("fast") }}
>
  {#each items as item (item.id)}
    {#if item.id === "separator"}
      <div class="rule" role="separator"></div>
    {:else}
      <button type="button" role="menuitem" disabled={item.disabled} onclick={() => onpick(item.id)}>
        <span class="label">{item.label}</span>
        {#if item.shortcut}<kbd>{item.shortcut}</kbd>{/if}
      </button>
    {/if}
  {/each}
</div>

<style>
  .menu {
    position: absolute;
    z-index: 5;
    display: grid;
    min-width: var(--size-menu);
    padding: var(--space-1);
    background: var(--bg-overlay);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-overlay);
    transform-origin: top left;
    outline: none;
  }
  button {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    height: var(--size-control-sm);
    padding: 0 var(--space-2) 0 var(--space-3);
    border: 0;
    border-radius: var(--radius-sm);
    background: transparent;
    color: var(--text-primary);
    font: inherit;
    text-align: left;
    transition: background var(--motion-fast) var(--ease-standard);
  }
  button:hover:not(:disabled),
  button:focus-visible {
    background: var(--bg-selected);
    outline: none;
    box-shadow: inset var(--size-bar) 0 0 0 var(--accent);
  }
  button:disabled { color: var(--text-muted); }
  .label { flex: 1; }
  kbd { color: var(--text-muted); }
  .rule {
    height: 1px;
    margin: var(--space-1) var(--space-2);
    background: var(--border-subtle);
  }
</style>
