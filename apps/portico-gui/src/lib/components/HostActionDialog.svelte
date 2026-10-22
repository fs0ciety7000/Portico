<script lang="ts">
  // Phase 6b, the short host dialogs: the read-only notice ("Edit host…" on
  // a host Portico does not own, duplicate offered), "Duplicate as Portico
  // host…" (a new alias) and the armed "Delete host" confirmation.
  import { untrack } from "svelte";
  import { asError, sentence, type ErrorSummary, type Host } from "../api";
  import { backend } from "../backend";
  import { capitalizable, suggestAlias } from "../hostform";
  import { hosts, type HostDialog } from "../state/hosts.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";
  import OriginBadge from "./OriginBadge.svelte";

  let { dialog, host }: { dialog: Extract<HostDialog, { kind: "read-only" | "duplicate" | "delete" }>; host: Host } = $props();

  /** Shared wording with the TUI. */
  const READ_ONLY = "Defined in ~/.ssh/config, which Portico never edits. Duplicate it as a Portico host to change it.";
  const READ_ONLY_IMPORTED = "Imported from Termius: the next import replaces it, and Portico never edits it. Duplicate it as a Portico host to change it.";

  let armed = $state(false);
  let busy = $state(false);
  let error: ErrorSummary | null = $state(null);
  let newAlias = $state(untrack(() => suggestAlias(host.alias, hosts.all.map((h) => h.alias))));

  const close = () => (hosts.dialog = null);

  function toDuplicate() {
    hosts.dialog = { kind: "duplicate", alias: host.alias };
  }

  function toMetadata() {
    hosts.dialog = null;
    hosts.editing = host.alias;
  }

  async function duplicate() {
    if (!armed || busy) return;
    busy = true;
    error = null;
    try {
      const alias = await backend.duplicateHost(host.alias, newAlias.trim());
      hosts.changed(alias);
      // Straight to the copy's form: duplicating is for changing it.
      // (the form starts from what hosts.conf holds, read once written).
      const managed = await backend.managedHost(alias);
      hosts.dialog = managed ? { kind: "edit", alias, managed } : null;
      workspace.notify({ code: "host_duplicated", message: `${host.alias} duplicated as ${alias}`, hint: null }, "info");
    } catch (e) {
      error = asError(e);
    } finally {
      busy = false;
    }
  }

  async function remove() {
    if (!armed || busy) return;
    busy = true;
    error = null;
    try {
      await backend.deleteHost(host.alias);
      hosts.dialog = null;
      hosts.changed();
      workspace.notify({ code: "host_deleted", message: `${host.alias} deleted`, hint: null }, "info");
    } catch (e) {
      error = asError(e);
    } finally {
      busy = false;
    }
  }

  function onkey(key: string): boolean {
    if (dialog.kind === "read-only") {
      if (key === "d" || key === "Enter") toDuplicate();
      else if (key === "e") toMetadata();
      else return false;
      return true;
    }
    if (dialog.kind === "delete") {
      if (key === "y") void remove();
      else if (key === "n") close();
      else return false;
      return true;
    }
    return false;
  }
</script>

{#snippet errorView()}
  {#if error}
    <p class="inline-error" role="alert"><span>{capitalizable(error.code) ? sentence(error.message) : error.message}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}</p>
  {/if}
{/snippet}

{#if dialog.kind === "read-only"}
  <Modal bind:armed onescape={close} {onkey} labelledBy="host-readonly-title">
    {#snippet title()}<span class="title-line">Edit <span class="mono">{host.alias}</span><OriginBadge origin={host.origin} /></span>{/snippet}
    <p class="lead">{host.origin === "imported" ? READ_ONLY_IMPORTED : READ_ONLY}</p>
    <p class="note">Tags, group, favorite and notes stay editable for every host: they live in <span class="mono">hosts.toml</span>.</p>
    <dl>
      <dt>Defined in</dt><dd>{host.source}</dd>
    </dl>
    {#snippet footer()}
      <button class="secondary" onclick={toMetadata} title="Edit tags, group and notes (Esc closes)">Tags and notes <kbd>e</kbd></button>
      <button class="primary" data-autofocus onclick={toDuplicate}>Duplicate as Portico host… <kbd>d</kbd></button>
    {/snippet}
  </Modal>
{:else if dialog.kind === "duplicate"}
  <Modal bind:armed onescape={close} labelledBy="host-duplicate-title">
    {#snippet title()}Duplicate <span class="mono">{host.alias}</span> as a Portico host{/snippet}
    <p class="lead">
      Copies its HostName, User, Port, key and jump host into Portico's <span class="mono">hosts.conf</span>, with its tags and group. The original stays
      as it is{host.origin === "ssh_config" ? " in ~/.ssh/config" : ""}.
    </p>
    <label class="field">
      <span>New alias</span>
      <input
        class="mono"
        data-autofocus
        bind:value={newAlias}
        spellcheck="false"
        autocomplete="off"
        aria-invalid={!!error}
        oninput={() => (error = null)}
        onkeydown={(e) => {
          if (e.key === "Enter") {
            e.preventDefault();
            void duplicate();
          }
        }}
      />
      {#if !error}<span class="help">Lowercase letters, digits, <span class="mono">_ . -</span>; not an alias in use.</span>{/if}
    </label>
    {@render errorView()}
    {#snippet footer()}
      <button class="secondary" onclick={close}>Cancel <kbd>Esc</kbd></button>
      <button class="primary" disabled={!armed || busy} onclick={() => void duplicate()}>
        {#if busy}<span class="spinner" aria-hidden="true"></span>{/if}Duplicate <kbd>↵</kbd>
      </button>
    {/snippet}
  </Modal>
{:else}
  <Modal bind:armed tone="danger" onescape={close} {onkey} labelledBy="host-delete-title">
    {#snippet title()}Delete <span class="mono">{host.alias}</span>?{/snippet}
    <p class="lead">
      Removes it from Portico's <span class="mono">hosts.conf</span>, with its tags, notes and remembered password. Open sessions stay connected;
      <span class="mono">~/.ssh/config</span> is not touched.
    </p>
    <dl>
      <dt>Host</dt><dd>{host.user ? `${host.user}@` : ""}{host.hostname}:{host.port}</dd>
    </dl>
    {@render errorView()}
    {#snippet footer()}
      <button class="secondary danger" disabled={!armed || busy} onclick={() => void remove()}>
        {#if busy}<span class="spinner" aria-hidden="true"></span>{/if}Delete <kbd>y</kbd>
      </button>
      <button class="primary" data-autofocus onclick={close}>Keep <kbd>n</kbd></button>
    {/snippet}
  </Modal>
{/if}

<style>
  .title-line { display: inline-flex; align-items: center; gap: var(--space-2); flex-wrap: wrap; }
  .lead { color: var(--text-primary); line-height: 1.5; }
  :global(.modal) button.secondary.danger { color: var(--status-error); border-color: color-mix(in srgb, var(--status-error) 45%, var(--border-strong)); }
</style>
