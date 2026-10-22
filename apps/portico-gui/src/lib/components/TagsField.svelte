<script lang="ts">
  // Tags as chips (the metadata editor and the host dialog). Each change is
  // checked with the core's own rules (`check_tags`, no write): a refused
  // chip is marked at once, with the core's message and hint.
  import { asError, sentence, tagPosition, type ErrorSummary } from "../api";
  import { backend } from "../backend";

  let {
    tags = $bindable([]),
    error = $bindable(),
    label = "Tags",
    autofocus = false,
    onenter,
  }: {
    tags?: string[];
    error?: ErrorSummary;
    label?: string;
    autofocus?: boolean;
    /** ↵ in the empty field (the dialog's primary action). */
    onenter: () => void;
  } = $props();

  const uid = $props.id();
  const labelId = `tags-label-${uid}`;
  let text = $state("");
  let input: HTMLInputElement | undefined = $state();
  /** Index of the chip the core refused (from the error's position). */
  let badTag: number | null = $state(null);
  let chipEls: HTMLElement[] = $state([]);
  let checks = 0;

  /** Shows an error about the tags (from `check_tags` or a save). */
  export function setError(e: unknown) {
    error = asError(e);
    const position = tagPosition(e);
    badTag = position !== null && position <= tags.length ? position - 1 : null;
  }

  /** Turns text still in the field into chips (before a save). */
  export function commitPending() {
    if (text.trim()) commit(text);
  }

  export function focus() {
    input?.focus();
  }

  async function check() {
    const run = ++checks;
    const list = [...tags];
    try {
      await backend.checkTags(list);
      if (run !== checks) return;
      error = undefined;
      badTag = null;
    } catch (e) {
      if (run === checks) setError(e);
    }
  }

  /** Splits typed or pasted text into tags (space, comma, Enter; `#` optional). */
  function commit(raw: string) {
    let added = false;
    for (const part of raw.split(/[\s,]+/)) {
      const tag = part.replace(/^#/, "").trim();
      if (tag && !tags.some((t) => t.toLowerCase() === tag.toLowerCase())) {
        tags.push(tag);
        added = true;
      }
    }
    text = "";
    if (added) void check();
  }

  /** Removes a chip; the focus goes to `focusNext` (a chip index), else the field. */
  function removeTag(i: number, focusNext: number | null = null) {
    tags.splice(i, 1);
    if (error) void check();
    else badTag = null;
    const next = focusNext !== null && focusNext >= 0 && focusNext < tags.length ? focusNext : null;
    queueMicrotask(() => (next !== null ? chipEls[next]?.focus() : input?.focus()));
  }

  function focusChip(i: number) {
    chipEls[i]?.focus();
  }

  function onTagKey(e: KeyboardEvent) {
    if (e.key === " " || e.key === ",") {
      e.preventDefault();
      commit(text);
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (text.trim()) commit(text);
      else onenter();
    } else if (e.key === "Backspace" && !text && tags.length) {
      e.preventDefault();
      removeTag(tags.length - 1);
    } else if ((e.key === "ArrowLeft" || e.key === "ArrowRight") && !text && tags.length) {
      // From the empty field, the arrows walk onto the chips.
      e.preventDefault();
      focusChip(e.key === "ArrowLeft" ? tags.length - 1 : 0);
    }
  }

  function onChipKey(e: KeyboardEvent, i: number) {
    if (e.key === "ArrowLeft") {
      e.preventDefault();
      focusChip(Math.max(0, i - 1));
    } else if (e.key === "ArrowRight") {
      e.preventDefault();
      if (i + 1 < tags.length) focusChip(i + 1);
      else input?.focus();
    } else if (e.key === "Backspace") {
      e.preventDefault();
      removeTag(i, i > 0 ? i - 1 : 0);
    } else if (e.key === "Delete") {
      e.preventDefault();
      removeTag(i, i);
    } else if (e.key === "Home") {
      e.preventDefault();
      focusChip(0);
    } else if (e.key === "End" || (e.key.length === 1 && !e.ctrlKey && !e.metaKey && !e.altKey)) {
      // Typing on a chip goes back to the field (and types there).
      input?.focus();
    }
  }

  function onTagPaste(e: ClipboardEvent) {
    const pasted = e.clipboardData?.getData("text") ?? "";
    if (/[\s,]/.test(pasted)) {
      e.preventDefault();
      commit(text + pasted);
    }
  }
</script>

<div class="field">
  <span id={labelId}>{label}</span>
  <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
  <div class="chips" class:invalid={!!error} onclick={() => input?.focus()}>
    {#each tags as tag, i (tag)}
      <!-- Reached with ← / → from the empty field (not a Tab stop). -->
      <span
        class="chip"
        class:invalid={badTag === i}
        role="button"
        tabindex="-1"
        aria-label={`Tag ${tag}${badTag === i ? ", refused" : ""}: Backspace or Delete removes it`}
        bind:this={chipEls[i]}
        onkeydown={(e) => onChipKey(e, i)}
      >
        #{tag}
        <button class="chip-remove" tabindex="-1" aria-label={`Remove tag ${tag}`} onclick={() => removeTag(i)}>
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m5 5 6 6M11 5l-6 6" /></svg>
        </button>
      </span>
    {/each}
    <input
      bind:this={input}
      bind:value={text}
      data-autofocus={autofocus ? true : undefined}
      class="chip-input mono"
      placeholder={tags.length ? "" : "prod, db, eu-west"}
      aria-labelledby={labelId}
      aria-invalid={!!error}
      spellcheck="false"
      autocomplete="off"
      onkeydown={onTagKey}
      onpaste={onTagPaste}
      onblur={() => text.trim() && commit(text)}
    />
  </div>
  {#if error}
    <p class="inline-error"><span>{sentence(error.message)}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}</p>
  {:else}
    <span class="help">
      Space, comma or <kbd>↵</kbd> adds a tag; <kbd>←</kbd> <kbd>→</kbd> select one, <kbd>Backspace</kbd> or <kbd>Del</kbd> removes it.
      Search them with <span class="mono">#tag</span>.
    </span>
  {/if}
</div>

<style>
  .chips {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-1);
    min-height: 34px;
    padding: var(--space-1) var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    cursor: text;
    transition: border-color var(--motion-fast) var(--ease-standard);
  }
  .chips:focus-within { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .chips.invalid { border-color: var(--status-error); }
  .chip {
    display: inline-flex;
    align-items: center;
    gap: var(--size-line-gap);
    height: 24px;
    padding: 0 2px 0 var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    color: var(--text-secondary);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }
  .chip { outline: 2px solid transparent; outline-offset: 1px; transition: outline-color var(--motion-fast) var(--ease-standard); }
  /* Keyboard focus on a chip: the accent ring (a refused chip keeps its red border inside it). */
  .chip:focus { outline-color: var(--accent); color: var(--text-primary); }
  /* The tag the core refused. */
  .chip.invalid { border-color: var(--status-error); color: var(--status-error); }
  .chips .chip-remove {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    border-radius: var(--radius-pill);
    background: transparent;
    color: var(--text-muted);
  }
  .chips .chip-remove:hover { background: var(--bg-overlay); color: var(--text-primary); }
  .chip-remove svg { width: 10px; height: 10px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; }
  /* Beats the shared `.modal .field input` rule: the chip box is the field. */
  .field .chips input.chip-input:not([type="checkbox"]):not([type="radio"]) {
    flex: 1;
    min-width: 8ch;
    height: 24px;
    padding: 0 var(--space-1);
    border: 0;
    outline: none;
    box-shadow: none;
    background: transparent;
    color: var(--text-primary);
    font-size: var(--text-sm);
  }
  .field .chips input.chip-input:not([type="checkbox"]):not([type="radio"]):focus { border: 0; box-shadow: none; }
</style>
