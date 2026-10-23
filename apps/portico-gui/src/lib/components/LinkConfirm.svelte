<script lang="ts">
  // "Open this link?" (phase 9): every terminal link is confirmed. The full
  // URL in mono, wrapped, its host emphasised (punycode for an
  // internationalized one); a caution when an OSC 8 text names something
  // else. Enter opens once the dialog is armed (Modal), Esc cancels. With a
  // caution, Cancel has the focus and Open is no longer the primary action:
  // Enter acts on the focused button only.
  import { caution, splitAtHost } from "../links";
  import { links, type PendingLink } from "../state/links.svelte";
  import Caution from "./Caution.svelte";
  import Modal from "./Modal.svelte";

  let { link }: { link: PendingLink } = $props();
  let armed = $state(false);
  const parts = $derived(splitAtHost(link.href, link.host));

  function open() {
    if (armed) void links.open();
  }
</script>

<Modal
  bind:armed
  onescape={() => links.cancel()}
  onkey={(k) => {
    if (k !== "Enter" || link.misleading) return false;
    open();
    return true;
  }}
  labelledBy="link-title"
>
  {#snippet title()}Open this link?{/snippet}
  <p class="url mono" data-testid="link-url">{parts[0]}<strong>{parts[1]}</strong>{parts[2]}</p>
  {#if link.misleading}
    <Caution testid="link-caution">{caution(link.misleading, link.host)}</Caution>
  {/if}
  {#snippet footer()}
    <span class="footer-note">Opens in your default browser</span>
    {#if link.misleading}
      <button class="secondary" data-autofocus onclick={() => links.cancel()}>Cancel <kbd>Esc</kbd></button>
      <button class="secondary" aria-disabled={!armed} onclick={open} data-testid="link-open">Open</button>
    {:else}
      <button class="secondary" onclick={() => links.cancel()}>Cancel <kbd>Esc</kbd></button>
      <button class="primary" data-autofocus aria-disabled={!armed} onclick={open} data-testid="link-open">Open <kbd>↵</kbd></button>
    {/if}
  {/snippet}
</Modal>

<style>
  .url {
    margin: 0 0 var(--space-3);
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-secondary);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    overflow-wrap: anywhere;
    white-space: pre-wrap;
    max-height: 12lh;
    overflow-y: auto;
  }
  .url:last-child { margin-bottom: 0; }
  .url strong { color: var(--text-primary); font-weight: 600; }
  button[aria-disabled="true"] { opacity: 0.5; }
</style>
