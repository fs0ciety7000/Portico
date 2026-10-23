<script lang="ts">
  // Settings (phase 9): the theme and "Copy on select", both written to
  // settings.toml by the core (only their own keys change). Each change
  // applies at once; Esc or Done closes.
  import { settings } from "../state/settings.svelte";
  import { keymap } from "../state/keymap.svelte";
  import Modal from "./Modal.svelte";

  const themeKey = $derived(keymap.label("app.theme"));
  const close = () => (settings.open = false);
</script>

<Modal onescape={close} labelledBy="settings-title">
  {#snippet title()}Settings{/snippet}
  <div class="rows">
    <div class="field">
      <span id="theme-label">Theme</span>
      <div class="themes" role="radiogroup" aria-labelledby="theme-label">
        {#each settings.themes as t (t.id)}
          <button
            type="button"
            role="radio"
            class="theme"
            class:on={t.id === settings.themeId}
            aria-checked={t.id === settings.themeId}
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
    height: 32px;
    padding: 0 var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: transparent;
    color: var(--text-secondary);
    font: inherit;
    transition: border-color var(--motion-fast) var(--ease-standard), color var(--motion-fast) var(--ease-standard);
  }
  .theme:hover { color: var(--text-primary); background: var(--bg-elevated); }
  .theme.on { border-color: var(--accent); color: var(--text-primary); }
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
