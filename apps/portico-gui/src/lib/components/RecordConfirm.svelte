<script lang="ts">
  // "Record this terminal?" ("Record this pane?" in a split tab): the opt-in
  // of the ADR "Enregistrement de sessions, asciicast v2". Same wording and
  // keys as the TUI (`consentTitle`, `CONSENT_LEAD`…, `confirmKey`): only
  // `y` records, once armed (Modal: no answer before a pause without
  // keystrokes), so a user still typing in the shell can never start a
  // recording by accident; `n` and Esc cancel; Enter does nothing. The
  // folder is the display string built in Rust (`recordings_folder`).
  import { CONSENT_LEAD, CONSENT_TAIL, confirmKey, consentTitle } from "../recording";
  import { recordings } from "../state/recordings.svelte";
  import Modal from "./Modal.svelte";

  let { alias, folder, pane = null }: { alias: string; folder: string; pane?: { index: number; count: number } | null } = $props();
  let armed = $state(false);

  function record() {
    if (armed) void recordings.start();
  }

  function onkey(k: string): boolean {
    const answer = confirmKey(k, armed);
    if (answer === "record") record();
    else if (answer === "cancel") recordings.confirm = null;
    // `y` before the arming is swallowed too (never typed into the shell).
    return answer !== null || k === "y";
  }

  // Keyboard activation of the Record button is off: `y` is the only key
  // that records (a focused button would take Enter or Space).
  function noKeyActivation(e: KeyboardEvent) {
    if (e.key === "Enter" || e.key === " ") e.preventDefault();
  }
</script>

<Modal bind:armed onescape={() => (recordings.confirm = null)} {onkey} labelledBy="record-title">
  {#snippet title()}{consentTitle(!!pane)}<span class="title-host">· {alias}{#if pane}{" · pane "}<span class="pane-pos mono">{pane.index}/{pane.count}</span>{/if}</span>{/snippet}
  <p>{CONSENT_LEAD}<span class="path mono">{folder}</span>{CONSENT_TAIL}</p>
  {#snippet footer()}
    <span class="footer-note">asciicast v2 · <span class="rec">REC</span> stays on while it records</span>
    <button class="secondary" onclick={() => (recordings.confirm = null)}>Cancel <kbd>Esc</kbd></button>
    <!-- aria-disabled, not disabled: it stays reachable (Tab) and announced;
         record() checks the arming. Not autofocused: Enter never records. -->
    <button class="primary" aria-disabled={!armed} aria-keyshortcuts="y" onkeydown={noKeyActivation} onclick={record}>Record <kbd>y</kbd></button>
  {/snippet}
</Modal>

<style>
  /* The path moves to the next line as a whole; only a path wider than the
     dialog breaks inside. */
  .path { color: var(--text-primary); font-size: var(--text-sm); overflow-wrap: anywhere; }
  .pane-pos { font-size: var(--text-sm); }
  button[aria-disabled="true"] { opacity: 0.5; }
  .rec { color: var(--status-error); font-weight: 600; letter-spacing: var(--tracking-caps); }
</style>
