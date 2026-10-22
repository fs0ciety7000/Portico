<script lang="ts">
  // "Record this terminal?": the opt-in of the ADR "Enregistrement de
  // sessions, asciicast v2". Armed like every confirmation (Modal: no
  // answer before a pause without keystrokes), so a user still typing in
  // the shell can never start a recording by accident. The folder is the
  // display string built in Rust (`recordings_folder`).
  import { recordings } from "../state/recordings.svelte";
  import Modal from "./Modal.svelte";

  let { alias, folder }: { alias: string; folder: string } = $props();
  let armed = $state(false);

  function record() {
    if (armed) void recordings.start();
  }
</script>

<Modal
  bind:armed
  onescape={() => (recordings.confirm = null)}
  onkey={(k) => (k === "y" || k === "Enter" ? (record(), true) : k === "n" ? ((recordings.confirm = null), true) : false)}
  labelledBy="record-title"
>
  {#snippet title()}Record this terminal?<span class="title-host">· {alias}</span>{/snippet}
  <p>
    Everything shown on screen is recorded — including secrets printed by commands and anything the remote echoes. What you type is
    never recorded.
  </p>
  <p>Saved to <span class="path mono">{folder}</span>, readable only by you.</p>
  {#snippet footer()}
    <span class="footer-note">asciicast v2 · <span class="rec">REC</span> stays on while it records</span>
    <button class="secondary" onclick={() => (recordings.confirm = null)}>Cancel <kbd>Esc</kbd></button>
    <!-- aria-disabled, not disabled: the dialog focuses it as it arms (a
         disabled button cannot take the focus); record() checks the arming. -->
    <button class="primary" data-autofocus aria-disabled={!armed} onclick={record}>Record <kbd>↵</kbd></button>
  {/snippet}
</Modal>

<style>
  /* The path moves to the next line as a whole; only a path wider than the
     dialog breaks inside. */
  .path { color: var(--text-primary); font-size: var(--text-sm); overflow-wrap: anywhere; }
  button[aria-disabled="true"] { opacity: 0.5; }
  .rec { color: var(--status-error); font-weight: 600; letter-spacing: var(--tracking-caps); }
</style>
