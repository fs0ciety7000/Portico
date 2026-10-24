<script lang="ts">
  // Settings (phase 9): the theme, "Copy on select" and (0.3.0) the daily
  // update check, all written to settings.toml by the core (only their own
  // keys change). Each change applies at once; Esc or Done closes.
  import { AUTO, settings } from "../state/settings.svelte";
  import { ago, updates } from "../state/updates.svelte";
  import { keymap } from "../state/keymap.svelte";
  import Modal from "./Modal.svelte";

  const themeKey = $derived(keymap.label("app.theme"));
  const close = () => (settings.open = false);
  let group: HTMLDivElement | undefined = $state();

  /** Auto first, then every theme. */
  const options = $derived([
    { id: AUTO, name: "Auto", description: "Follows the system: Marble when it is light, your dark theme when it is dark." },
    ...settings.themes,
  ]);
  const darkName = $derived(settings.themes.find((t) => t.id === settings.darkTheme)?.name ?? settings.darkTheme);

  /** A radio group: the arrows select the previous / next choice and move the focus (one tab stop). */
  function onThemeKey(e: KeyboardEvent) {
    const step = { ArrowLeft: -1, ArrowUp: -1, ArrowRight: 1, ArrowDown: 1 }[e.key];
    if (step === undefined || e.altKey || e.ctrlKey || e.metaKey) return;
    e.preventDefault();
    const list = options;
    const at = list.findIndex((t) => t.id === settings.selection);
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
        {#each options as t (t.id)}
          <button
            type="button"
            role="radio"
            class="theme"
            class:on={t.id === settings.selection}
            aria-checked={t.id === settings.selection}
            tabindex={t.id === settings.selection ? 0 : -1}
            data-id={t.id}
            data-autofocus={t.id === settings.selection ? "" : undefined}
            title={t.description}
            onclick={() => settings.select(t.id)}
          >
            {#if t.id === AUTO}
              <!-- Both halves of Auto: the light theme's accent and the dark theme's. -->
              <span class="swatch split" aria-hidden="true"><span data-theme="marble"></span><span data-theme={settings.darkTheme}></span></span>
            {:else}
              <span class="swatch" data-theme={t.id}></span>
            {/if}{t.name}
          </button>
        {/each}
      </div>
      <p class="note" data-testid="theme-note">
        Auto uses Marble while the system is light, {darkName} while it is dark.
        {#if themeKey}Also <kbd>{themeKey}</kbd> in the host list.{/if}
      </p>
    </div>
    <div class="field">
      <span>Terminal</span>
      <label class="check">
        <input type="checkbox" checked={settings.copyOnSelect} onchange={(e) => settings.setCopyOnSelect(e.currentTarget.checked)} data-testid="copy-on-select" />
        Copy on select
      </label>
      <p class="note">A finished mouse selection goes to the clipboard, as a copy would.</p>
    </div>
    {#if updates.status}
      {@const checked = updates.on ? ago(updates.status.checked_at) : null}
      <div class="field">
        <span>Updates</span>
        <label class="check">
          <input type="checkbox" checked={updates.on} disabled={updates.saving} onchange={(e) => void updates.setCheck(e.currentTarget.checked)} data-testid="update-check" />
          Check for updates daily
        </label>
        <p class="note">
          Portico contacts github.com once a day and sends nothing else. Portico never downloads or installs anything itself.{checked
            ? ` Last checked ${checked}.`
            : ""}
        </p>
      </div>
    {/if}
  </div>
  {#snippet footer()}
    <span class="footer-note">Saved in settings.toml</span>
    <button class="primary" onclick={close}>Done <kbd>Esc</kbd></button>
  {/snippet}
</Modal>

<style>
  .rows { display: grid; gap: var(--space-5); }
  /* Auto on its own line (it follows the system), the four themes under it. */
  .themes {
    display: grid;
    grid-template-columns: repeat(4, auto);
    justify-content: start;
    gap: var(--space-2);
  }
  .theme[data-id="auto"] { grid-column: 1 / -1; justify-self: start; }
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
  /* Auto: two half discs, each in its own theme's accent. */
  .swatch.split {
    display: inline-flex;
    overflow: hidden;
    background: none;
  }
  .swatch.split > span { flex: 1 1 50%; background: var(--accent); }
  .note { color: var(--text-muted); font-size: var(--text-sm); }
</style>
