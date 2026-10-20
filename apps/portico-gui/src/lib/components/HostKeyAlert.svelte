<script lang="ts">
  // Changed host key: blocking, red, informative. There is deliberately no
  // way to accept the new key from here.
  import { displayPath } from "../api";
  import { shellQuote } from "../keys";
  import type { Alert } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";

  let { alert, onclose }: { alert: Alert; onclose: () => void } = $props();

  let armed = $state(false);
  let copied = $state(false);

  const target = $derived(alert.port === 22 ? alert.host : `[${alert.host}]:${alert.port}`);
  const file = $derived(displayPath(alert.known_hosts_file));
  // Quoted for the shell; `-f` when the key lives in another file than the default.
  const fix = $derived(
    `ssh-keygen -R ${shellQuote(target)}${file === "~/.ssh/known_hosts" ? "" : ` -f ${shellQuote(alert.known_hosts_file)}`}`,
  );
  /** Offer a one-click copy only for plain host names. */
  const copyable = $derived(/^[A-Za-z0-9._:[\]-]+$/.test(target));

  async function copy() {
    try {
      await navigator.clipboard.writeText(fix);
      copied = true;
      setTimeout(() => (copied = false), 1600);
    } catch {
      // Clipboard unavailable: the command stays selectable.
    }
  }

  function close() {
    if (armed) onclose();
  }
</script>

<Modal tone="danger" bind:armed onescape={close} labelledBy="host-key-alert">
  {#snippet title()}
    <span class="banner">Host key changed</span>
    <span class="title-host"> · {alert.alias}</span>
  {/snippet}

  <p class="lead">The key presented by <span class="mono">{target}</span> does not match <span class="mono">known_hosts</span>.</p>
  <p class="danger">Someone could be intercepting this connection (man-in-the-middle). Portico refused to connect.</p>
  <dl>
    <dt>Key type</dt><dd>{alert.algorithm}</dd>
    <dt>Expected</dt><dd class="fingerprint">{alert.expected_fingerprint}</dd>
    <dt>Received</dt><dd class="fingerprint received">{alert.fingerprint}</dd>
    <dt>Recorded in</dt><dd>{file}:{alert.known_hosts_line}</dd>
  </dl>
  <p>Only if the server's administrator confirms the new key, remove the old one, then connect again:</p>
  <div class="fix">
    <code>{fix}</code>
    {#if copyable}
      <button class="copy" onclick={copy} aria-label="Copy command">{copied ? "Copied" : "Copy"}</button>
    {/if}
  </div>

  {#snippet footer()}
    <button class="primary" data-autofocus disabled={!armed} onclick={close}>Close <kbd>Esc</kbd></button>
  {/snippet}
</Modal>

<style>
  .banner { color: var(--status-error); }
  .lead { color: var(--text-primary); }
  .danger { color: var(--status-error); }
  .received { color: var(--status-error) !important; font-weight: 600; }
  .fix {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    padding: var(--space-2) var(--space-2) var(--space-2) var(--space-4);
    background: var(--bg-base);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
  }
  code {
    flex: 1;
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    color: var(--text-primary);
    user-select: text;
  }
  .copy {
    height: 26px !important;
    padding: 0 var(--space-3) !important;
    border: 1px solid var(--border-strong);
    background: transparent;
    color: var(--text-secondary);
    font-size: var(--text-sm) !important;
  }
  .copy:hover { background: var(--bg-elevated); color: var(--text-primary); }
</style>
