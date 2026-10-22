<script lang="ts">
  // The handle between two split panes. Keyboard first: "Move pane
  // divider…" (`pane.divider`) gives the focused pane's nearest divider the
  // keyboard (as do a click and Tab); it then moves with the arrows along
  // its axis, Home / End to the limits. The mouse drags it. Both are
  // clamped to the minimum pane size by the workspace. Enter or Esc gives
  // the keyboard back to the focused pane. While it has the focus it holds
  // the only accent (`onfocuschange`: the pane gives up its own).
  import { RESIZE_STEP, type Divider } from "../panes";
  import { workspace } from "../state/workspace.svelte";

  let {
    divider,
    onrelease,
    onfocuschange,
  }: { divider: Divider; onrelease: () => void; onfocuschange?: (focused: boolean) => void } = $props();

  let el: HTMLDivElement | undefined = $state();
  let dragging = $state(false);
  /** Vertical line between panes side by side (`row`). */
  const vertical = $derived(divider.axis === "row");

  function ratioAt(e: PointerEvent): number {
    const stage = el?.parentElement?.getBoundingClientRect();
    if (!stage) return divider.ratio;
    const { span, rect } = divider;
    const gap = vertical ? rect.w : rect.h;
    const room = (vertical ? span.w : span.h) - gap;
    if (room <= 0) return divider.ratio;
    const at = vertical ? e.clientX - stage.left - span.x : e.clientY - stage.top - span.y;
    return (at - gap / 2) / room;
  }

  function onpointerdown(e: PointerEvent) {
    if (e.button !== 0) return;
    e.preventDefault();
    el?.setPointerCapture(e.pointerId);
    el?.focus();
    dragging = true;
  }

  function onpointermove(e: PointerEvent) {
    if (dragging) workspace.setRatio(divider.id, ratioAt(e));
  }

  function onpointerup(e: PointerEvent) {
    if (!dragging) return;
    dragging = false;
    el?.releasePointerCapture(e.pointerId);
    onrelease();
  }

  function onkeydown(e: KeyboardEvent) {
    const less = vertical ? "ArrowLeft" : "ArrowUp";
    const more = vertical ? "ArrowRight" : "ArrowDown";
    if (e.key === less) workspace.setRatio(divider.id, divider.ratio - RESIZE_STEP);
    else if (e.key === more) workspace.setRatio(divider.id, divider.ratio + RESIZE_STEP);
    else if (e.key === "Home") workspace.setRatio(divider.id, 0);
    else if (e.key === "End") workspace.setRatio(divider.id, 1);
    else if (e.key === "Enter" || e.key === "Escape") onrelease();
    else return;
    e.preventDefault();
    e.stopPropagation();
  }
</script>

<!-- A focusable separator with a value is an interactive widget in WAI-ARIA
     (window splitter pattern); Svelte's check treats every separator as static. -->
<!-- svelte-ignore a11y_no_noninteractive_tabindex, a11y_no_noninteractive_element_interactions -->
<div
  bind:this={el}
  class="divider"
  class:vertical
  class:dragging
  role="separator"
  tabindex="0"
  data-divider={divider.id}
  aria-orientation={vertical ? "vertical" : "horizontal"}
  aria-label={vertical ? "Resize the panes side by side" : "Resize the stacked panes"}
  aria-valuemin={0}
  aria-valuemax={100}
  aria-valuenow={Math.round(divider.ratio * 100)}
  style:left="{divider.rect.x}px"
  style:top="{divider.rect.y}px"
  style:width="{divider.rect.w}px"
  style:height="{divider.rect.h}px"
  {onpointerdown}
  {onpointermove}
  {onpointerup}
  onpointercancel={onpointerup}
  onlostpointercapture={() => (dragging = false)}
  {onkeydown}
  onfocus={() => onfocuschange?.(true)}
  onblur={() => onfocuschange?.(false)}
></div>

<style>
  .divider {
    position: absolute;
    z-index: 1;
    display: grid;
    place-items: center;
    outline: none;
    cursor: row-resize;
    touch-action: none;
  }
  .divider.vertical { cursor: col-resize; }
  /* The line: invisible at rest (the gap already separates the panes), a
     quiet line on hover, the accent while dragged or focused. */
  .divider::before {
    content: "";
    width: 100%;
    height: 1px;
    border-radius: var(--radius-pill);
    background: var(--border-strong);
    opacity: 0;
    transition: opacity var(--motion-fast) var(--ease-standard),
      background var(--motion-fast) var(--ease-standard);
  }
  .divider.vertical::before { width: 1px; height: 100%; }
  .divider:hover::before { opacity: 1; }
  .divider.dragging::before,
  .divider:focus-visible::before {
    opacity: 1;
    background: var(--accent);
  }
  /* Grip: a short pill in the middle, shown with the focus ring. */
  .divider::after {
    content: "";
    position: absolute;
    width: var(--space-6);
    height: var(--space-1);
    border-radius: var(--radius-pill);
    background: var(--accent);
    opacity: 0;
    transform: scale(0.9);
    transition: opacity var(--motion-fast) var(--ease-standard),
      transform var(--motion-fast) var(--ease-standard);
  }
  .divider.vertical::after { width: var(--space-1); height: var(--space-6); }
  .divider:focus-visible::after,
  .divider.dragging::after {
    opacity: 1;
    transform: scale(1);
  }
</style>
