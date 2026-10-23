<script lang="ts">
  // Settings (phase 9): the theme and "Copy on select", both written to
  // settings.toml by the core (only their own keys change). Each change
  // applies at once; Esc or Done closes.
  import { settings } from "../state/settings.svelte";
  import { keymap } from "../state/keymap.svelte";
  import Modal from "./Modal.svelte";

  const themeKey = $derived(keymap.label("app.theme"));
  const close = () => (settings.open = false);
  let group: HTMLDivElement | undefined = $state();

  /** A radio group: the arrows select the previous / next theme and move the focus (one tab stop). */
  function onThemeKey(e: KeyboardEvent) {
    const step = { ArrowLeft: -1, ArrowUp: -1, ArrowRight: 1, ArrowDown: 1 }[e.key];
    if (step === undefined || e.altKey || e.ctrlKey || e.metaKey) return;
    e.preventDefault();
    const list = settings.themes;
    const at = list.findIndex((t) => t.id === settings.themeId);
    const next = list[(at + step + list.length) % list.length];
    if (!next) return;
    settings.select(next.id);
    group?.querySelector<HTMLElement>(`[data-id="${next.id}"]`)?.focus();
  }
</script>

<Modal onescape={close} labelledBy="settings-title">
  {#snippet title()}Settings{/snippet}
  <div class="rows">
    <div class="field">
      <span id="theme-label">Theme</span>
      <!-- svelte-ignore a11y_interactive_supports_focus: the radios inside take the focus (roving tabindex) -->
      <div class="themes" role="radiogroup" aria-labelledby="theme-label" bind:this={group} onkeydown={onThemeKey}>
        {#each settings.themes as t (t.id)}
          <button
            type="button"
            role="radio"
            class="theme"
            class:on={t.id === settings.themeId}
            aria-checked={t.id === settings.themeId}
            tabindex={t.id === settings.themeId ? 0 : -1}
            data-id={t.id}
            data-autofocus={t.id === settings.themeId ? "" : undefined}
            title={t.description}
            onclick={() => settings.select(t.id)}
          >
            <span class="swatch" data-theme={t.id}></span>{t.name}
          </button>
        {/each}
      </div>
      {#if themeKey}<p class="note">Also <kbd>{themeKey}</kbd> in the host list.</p>{/if}
    </div>
    <div class="field">
      <span>Terminal</span>
      <label class="check">
        <input type="checkbox" checked={settings.copyOnSelect} onchange={(e) => settings.setCopyOnSelect(e.currentTarget.checked)} data-testid="copy-on-select" />
        Copy on select
      </label>
      <p class="note">A finished mouse selection goes to the clipboard, as a copy would.</p>
    </div>
  </div>
  {#snippet footer()}
    <span class="footer-note">Saved in settings.toml</span>
    <button class="primary" onclick={close}>Done <kbd>Esc</kbd></button>
  {/snippet}
</Modal>

<style>
  .rows { display: grid; gap: var(--space-5); }
  .themes { display: flex; flex-wrap: wrap; gap: var(--space-2); }
  .theme {
    display: inline-flex;
    align-items: center;
    gap: var(--space-2);
    height: var(--size-control);
    padding: 0 var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-secondary);
    font: inherit;
    transition: border-color var(--motion-fast) var(--ease-standard), color var(--motion-fast) var(--ease-standard);
  }
  .theme:hover { color: var(--text-primary); background: var(--bg-elevated); }
  .theme.on {
    border-color: var(--accent);
    background: var(--bg-selected);
    box-shadow: inset var(--size-bar) 0 0 0 var(--accent);
    color: var(--text-primary);
  }
  /* Each theme's own accent, read from its token block. */
  .swatch {
    width: var(--space-3);
    height: var(--space-3);
    border-radius: 50%;
    background: var(--accent);
    box-shadow: inset 0 0 0 1px var(--border-strong);
  }
  .note { color: var(--text-muted); font-size: var(--text-sm); }
</style>
