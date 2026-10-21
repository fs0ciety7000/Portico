<script lang="ts">
  // "Edit <alias>": tags (chips), group (with the existing groups as
  // suggestions) and notes, stored in hosts.toml by the core, which
  // validates and normalizes them (errors come back per field, with a hint).
  import { untrack } from "svelte";
  import { asError, sentence, type ErrorSummary, type Host } from "../api";
  import { backend } from "../backend";
  import { groupNames } from "../hostlist";
  import { isMac } from "../keys";
  import { hosts } from "../state/hosts.svelte";
  import Modal from "./Modal.svelte";

  let { host, onclose }: { host: Host; onclose: () => void } = $props();

  /** Same bound as the core (`MAX_NOTES_CHARS`), for the counter only. */
  const NOTES_MAX = 2000;

  let armed = $state(false);
  // Snapshot at opening: edits from elsewhere do not rewrite the form.
  const initial = untrack(() => ({ tags: [...host.tags], group: host.group ?? "", notes: host.notes ?? "" }));
  let tags: string[] = $state([...initial.tags]);
  let tagText = $state("");
  let group = $state(initial.group);
  let notes = $state(initial.notes);
  let busy = $state(false);
  let errors: { tags?: ErrorSummary; group?: ErrorSummary; notes?: ErrorSummary; other?: ErrorSummary } = $state({});
  let tagInput: HTMLInputElement | undefined = $state();
  let groupFocused = $state(false);
  let suggestion = $state(-1);

  const allGroups = $derived(groupNames(hosts.all));
  const suggestions = $derived.by(() => {
    const q = group.trim().toLowerCase();
    return allGroups.filter((g) => g.toLowerCase() !== q && (!q || g.toLowerCase().includes(q))).slice(0, 6);
  });
  const showSuggestions = $derived(groupFocused && suggestions.length > 0);
  const saveKey = isMac ? "⌘S" : "Ctrl-S";

  /** Splits typed or pasted text into tags (space, comma, Enter; `#` optional). */
  function commit(text: string) {
    for (const raw of text.split(/[\s,]+/)) {
      const tag = raw.replace(/^#/, "").trim();
      if (tag && !tags.some((t) => t.toLowerCase() === tag.toLowerCase())) tags.push(tag);
    }
    tagText = "";
    errors.tags = undefined;
  }

  function removeTag(i: number) {
    tags.splice(i, 1);
    errors.tags = undefined;
    tagInput?.focus();
  }

  function onTagKey(e: KeyboardEvent) {
    if (e.key === " " || e.key === ",") {
      e.preventDefault();
      commit(tagText);
    } else if (e.key === "Enter") {
      e.preventDefault();
      if (tagText.trim()) commit(tagText);
      else void save();
    } else if (e.key === "Backspace" && !tagText && tags.length) {
      e.preventDefault();
      tags.pop();
    }
  }

  function onTagPaste(e: ClipboardEvent) {
    const text = e.clipboardData?.getData("text") ?? "";
    if (/[\s,]/.test(text)) {
      e.preventDefault();
      commit(tagText + text);
    }
  }

  function onGroupKey(e: KeyboardEvent) {
    if (showSuggestions && e.key === "ArrowDown") {
      suggestion = Math.min(suggestion + 1, suggestions.length - 1);
      e.preventDefault();
    } else if (showSuggestions && e.key === "ArrowUp") {
      suggestion = Math.max(suggestion - 1, -1);
      e.preventDefault();
    } else if (e.key === "Enter") {
      e.preventDefault();
      const picked = suggestion >= 0 ? suggestions[suggestion] : undefined;
      if (picked && showSuggestions) pick(picked);
      else void save();
    }
  }

  function pick(name: string) {
    group = name;
    suggestion = -1;
    errors.group = undefined;
  }

  const same = (a: string[], b: string[]) => a.length === b.length && a.every((t, i) => t === b[i]);

  async function save() {
    if (!armed || busy) return;
    if (tagText.trim()) commit(tagText);
    busy = true;
    errors = {};
    const alias = host.alias;
    // One field at a time: an error stays next to its field.
    const steps: [keyof typeof errors, () => Promise<unknown>][] = [];
    if (!same(tags, initial.tags)) steps.push(["tags", () => backend.setTags(alias, [...tags])]);
    if (group.trim() !== initial.group) steps.push(["group", () => backend.setGroup(alias, group.trim() || null)]);
    if (notes !== initial.notes) steps.push(["notes", () => backend.setNotes(alias, notes.trim() ? notes : null)]);
    try {
      for (const [field, run] of steps) {
        try {
          await run();
        } catch (e) {
          const error = asError(e);
          const known = { host_tag: "tags", host_tags: "tags", host_group: "group", host_notes: "notes" } as const;
          errors[known[error.code as keyof typeof known] ?? field] = error;
          return;
        }
      }
      onclose();
    } finally {
      busy = false;
    }
  }

  function onshortcut(e: KeyboardEvent): boolean {
    if (e.key.toLowerCase() === "s" || e.key === "Enter") {
      void save();
      return true;
    }
    return false;
  }
</script>

<Modal bind:armed onescape={onclose} {onshortcut} footerWrap={false} labelledBy="edit-host-title">
  {#snippet title()}Edit <span class="mono">{host.alias}</span>{/snippet}

  <div class="field">
    <span id="edit-tags-label">Tags</span>
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="chips" class:invalid={!!errors.tags} onclick={() => tagInput?.focus()}>
      {#each tags as tag, i (tag)}
        <span class="chip">
          #{tag}
          <button class="chip-remove" tabindex="-1" aria-label={`Remove tag ${tag}`} onclick={() => removeTag(i)}>
            <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m5 5 6 6M11 5l-6 6" /></svg>
          </button>
        </span>
      {/each}
      <input
        bind:this={tagInput}
        bind:value={tagText}
        data-autofocus
        class="chip-input mono"
        placeholder={tags.length ? "" : "prod, db, eu-west"}
        aria-labelledby="edit-tags-label"
        aria-invalid={!!errors.tags}
        spellcheck="false"
        autocomplete="off"
        onkeydown={onTagKey}
        onpaste={onTagPaste}
        onblur={() => tagText.trim() && commit(tagText)}
      />
    </div>
    {#if errors.tags}
      <p class="inline-error"><span>{sentence(errors.tags.message)}</span>{#if errors.tags.hint}<span class="hint">{errors.tags.hint}</span>{/if}</p>
    {:else}
      <span class="help">Space, comma or <kbd>↵</kbd> adds a tag; <kbd>Backspace</kbd> removes the last one. Search them with <span class="mono">#tag</span>.</span>
    {/if}
  </div>

  <div class="field group-field">
    <span id="edit-group-label">Group</span>
    <input
      bind:value={group}
      aria-labelledby="edit-group-label"
      aria-invalid={!!errors.group}
      aria-autocomplete="list"
      aria-expanded={showSuggestions}
      placeholder="None"
      spellcheck="false"
      autocomplete="off"
      onfocus={() => (groupFocused = true)}
      onblur={() => (groupFocused = false)}
      oninput={() => {
        suggestion = -1;
        errors.group = undefined;
      }}
      onkeydown={onGroupKey}
    />
    {#if showSuggestions}
      <ul class="suggestions" role="listbox" aria-label="Existing groups">
        {#each suggestions as name, i (name)}
          <li role="option" aria-selected={i === suggestion}>
            <!-- mousedown: picked before the field loses the focus -->
            <button tabindex="-1" class:active={i === suggestion} onmousedown={(e) => (e.preventDefault(), pick(name))}>{name}</button>
          </li>
        {/each}
      </ul>
    {/if}
    {#if errors.group}
      <p class="inline-error"><span>{sentence(errors.group.message)}</span>{#if errors.group.hint}<span class="hint">{errors.group.hint}</span>{/if}</p>
    {:else}
      <span class="help">Groups order the sidebar. Search them with <span class="mono">@group</span>.</span>
    {/if}
  </div>

  <label class="field">
    <span>Notes</span>
    <textarea bind:value={notes} aria-invalid={!!errors.notes} spellcheck="true" rows="4" oninput={() => (errors.notes = undefined)}></textarea>
    {#if errors.notes}
      <p class="inline-error"><span>{sentence(errors.notes.message)}</span>{#if errors.notes.hint}<span class="hint">{errors.notes.hint}</span>{/if}</p>
    {:else}
      <span class="help counter" class:over={notes.length > NOTES_MAX}>{notes.length} / {NOTES_MAX}</span>
    {/if}
  </label>

  {#if errors.other}
    <p class="inline-error"><span>{sentence(errors.other.message)}</span>{#if errors.other.hint}<span class="hint">{errors.other.hint}</span>{/if}</p>
  {/if}

  {#snippet footer()}
    <span class="footer-note">Portico never edits ~/.ssh/config: these live in hosts.toml</span>
    <button class="secondary" onclick={onclose}>Cancel <kbd>Esc</kbd></button>
    <button class="primary" disabled={!armed || busy} onclick={() => void save()}>
      {#if busy}<span class="spinner" aria-hidden="true"></span>{/if}Save <kbd>{saveKey}</kbd>
    </button>
  {/snippet}
</Modal>

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
    gap: 2px;
    height: 24px;
    padding: 0 2px 0 var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    color: var(--text-secondary);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
  }
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
  .group-field { position: relative; }
  .suggestions {
    position: absolute;
    top: calc(var(--space-5) + 34px);
    left: 0;
    right: 0;
    z-index: 2;
    margin: 0;
    padding: var(--space-1);
    list-style: none;
    background: var(--bg-overlay);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-raised);
  }
  .suggestions button {
    width: 100%;
    height: 28px;
    justify-content: flex-start;
    border: 0;
    background: transparent;
    color: var(--text-secondary);
  }
  .suggestions button:hover,
  .suggestions button.active { background: var(--bg-selected); color: var(--text-primary); }
  .counter { justify-self: end; font-variant-numeric: tabular-nums; }
  .counter.over { color: var(--status-error); }
  /* Wraps beside the buttons rather than pushing them to a second line. */
  .footer-note { flex: 1 1 auto; min-width: 0; white-space: normal; }
</style>
