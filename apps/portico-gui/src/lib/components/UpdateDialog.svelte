<script lang="ts">
  // "Portico 0.3.1 is available" (0.3.0): notify only. The release notes
  // open through `open_url` (the page comes from the core, validated as a
  // Portico release on github.com, and again in Rust); the update command
  // for this install is copied, never run. Close (Esc) leaves the badge.
  import { COPIED_FEEDBACK_MS } from "../motion";
  import { updates } from "../state/updates.svelte";
  import Modal from "./Modal.svelte";

  const available = $derived(updates.available);
  const instruction = $derived(updates.instruction);
  let copied = $state(false);
  let commandEl: HTMLElement | undefined = $state();

  async function copy(text: string) {
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
      setTimeout(() => (copied = false), COPIED_FEEDBACK_MS);
    } catch {
      // No clipboard here: select the command for a manual copy.
      const range = document.createRange();
      if (commandEl) {
        range.selectNodeContents(commandEl);
        getSelection()?.removeAllRanges();
        getSelection()?.addRange(range);
      }
    }
  }
</script>

{#if available}
  <Modal onescape={() => updates.close()} labelledBy="update-title">
    {#snippet title()}Portico {available.version} is available{/snippet}
    <div class="body" data-testid="update-dialog">
      <p>You have {updates.status?.current}.</p>
      <p class="how">{instruction?.text ?? "Download it from the release page."}</p>
      {#if instruction?.command}
        {@const command = instruction.command}
        <div class="command">
          <code class="mono" bind:this={commandEl}>{command}</code>
          <button class="secondary" onclick={() => void copy(command)} data-testid="update-copy">{copied ? "Copied" : "Copy"}</button>
        </div>
      {/if}
      <p class="note">Portico never downloads or installs anything itself.</p>
    </div>
    {#snippet footer()}
      <button class="secondary" onclick={() => void updates.openNotes()} title={available.url} data-testid="update-notes">Release notes</button>
      <button class="primary" data-autofocus onclick={() => updates.close()}>Close <kbd>Esc</kbd></button>
    {/snippet}
  </Modal>
{/if}

<style>
  .body { display: grid; gap: var(--space-3); }
  .body p { margin: 0; }
  .how { color: var(--text-secondary); }
  .command { display: flex; align-items: center; gap: var(--space-2); }
  .command code {
    flex: 1;
    min-width: 0;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
    user-select: all;
  }
  .note { color: var(--text-muted); font-size: var(--text-sm); }
</style>
