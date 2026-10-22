<script lang="ts">
  // Group, with the existing groups as suggestions (↑ ↓ pick, ↵ takes one;
  // ↵ without a suggestion is the dialog's primary action).
  import { sentence, type ErrorSummary, type Host } from "../api";
  import { groupNames } from "../hostlist";

  let {
    value = $bindable(""),
    error = $bindable(),
    groups,
    onenter,
  }: {
    value?: string;
    error?: ErrorSummary;
    /** Hosts whose groups are suggested. */
    groups: Host[];
    onenter: () => void;
  } = $props();

  const uid = $props.id();
  const labelId = `group-label-${uid}`;
  let focused = $state(false);
  let suggestion = $state(-1);

  const allGroups = $derived(groupNames(groups));
  const suggestions = $derived.by(() => {
    const q = value.trim().toLowerCase();
    return allGroups.filter((g) => g.toLowerCase() !== q && (!q || g.toLowerCase().includes(q))).slice(0, 6);
  });
  const showSuggestions = $derived(focused && suggestions.length > 0);

  function onkeydown(e: KeyboardEvent) {
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
      else onenter();
    }
  }

  function pick(name: string) {
    value = name;
    suggestion = -1;
    error = undefined;
  }
</script>

<div class="field group-field">
  <span id={labelId}>Group</span>
  <input
    bind:value
    aria-labelledby={labelId}
    aria-invalid={!!error}
    aria-autocomplete="list"
    aria-expanded={showSuggestions}
    placeholder="None"
    spellcheck="false"
    autocomplete="off"
    onfocus={() => (focused = true)}
    onblur={() => (focused = false)}
    oninput={() => {
      suggestion = -1;
      error = undefined;
    }}
    {onkeydown}
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
  {#if error}
    <p class="inline-error"><span>{sentence(error.message)}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}</p>
  {:else}
    <span class="help">Groups order the sidebar. Search them with <span class="mono">@group</span>.</span>
  {/if}
</div>

<style>
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
</style>
