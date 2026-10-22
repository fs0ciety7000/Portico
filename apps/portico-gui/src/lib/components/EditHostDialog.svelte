<script lang="ts">
  // "Edit <alias>": tags (chips), group (with the existing groups as
  // suggestions) and notes, stored in hosts.toml by the core, which
  // validates and normalizes them (errors come back per field, with a hint).
  // A host of ~/.ssh/config or of an import keeps its directives read-only;
  // a Portico host opens the full host dialog instead (HostDialog.svelte).
  import { untrack } from "svelte";
  import { asError, sentence, type ErrorSummary, type Host } from "../api";
  import { backend } from "../backend";
  import { isMac } from "../keys";
  import { hosts } from "../state/hosts.svelte";
  import GroupField from "./GroupField.svelte";
  import Modal from "./Modal.svelte";
  import TagsField from "./TagsField.svelte";

  let { host, onclose }: { host: Host; onclose: () => void } = $props();

  /** Same bound as the core (`MAX_NOTES_CHARS`), for the counter only. */
  const NOTES_MAX = 2000;

  let armed = $state(false);
  // Snapshot at opening: edits from elsewhere do not rewrite the form.
  const initial = untrack(() => ({ tags: [...host.tags], group: host.group ?? "", notes: host.notes ?? "" }));
  let tags: string[] = $state([...initial.tags]);
  let group = $state(initial.group);
  let notes = $state(initial.notes);
  let busy = $state(false);
  let errors: { tags?: ErrorSummary; group?: ErrorSummary; notes?: ErrorSummary; other?: ErrorSummary } = $state({});
  let tagsField: TagsField | undefined = $state();
  const saveKey = isMac ? "⌘S" : "Ctrl-S";

  const same = (a: string[], b: string[]) => a.length === b.length && a.every((t, i) => t === b[i]);

  async function save() {
    if (!armed || busy) return;
    tagsField?.commitPending();
    busy = true;
    errors = {};
    const alias = host.alias;
    // One field at a time: an error stays next to its field.
    const steps: [keyof typeof errors, () => Promise<unknown>][] = [];
    if (!same(tags, initial.tags)) steps.push(["tags", () => backend.setTags(alias, [...tags])]);
    if (group.trim() !== initial.group) steps.push(["group", () => backend.setGroup(alias, group.trim() || null)]);
    if (notes !== initial.notes) steps.push(["notes", () => backend.setNotes(alias, notes.trim() ? notes : null)]);
    try {
      for (const [field, run] of steps) {
        try {
          await run();
        } catch (e) {
          const error = asError(e);
          const known = { host_tag: "tags", host_tags: "tags", host_group: "group", host_notes: "notes" } as const;
          const at = known[error.code as keyof typeof known] ?? field;
          if (at === "tags") tagsField?.setError(e);
          else errors[at] = error;
          return;
        }
      }
      onclose();
    } finally {
      busy = false;
    }
  }

  function onshortcut(e: KeyboardEvent): boolean {
    if (e.key.toLowerCase() === "s" || e.key === "Enter") {
      void save();
      return true;
    }
    return false;
  }
</script>

<Modal bind:armed onescape={onclose} {onshortcut} footerWrap={false} labelledBy="edit-host-title">
  {#snippet title()}Edit <span class="mono">{host.alias}</span>{/snippet}

  <TagsField bind:this={tagsField} bind:tags bind:error={errors.tags} autofocus onenter={() => void save()} />

  <GroupField bind:value={group} bind:error={errors.group} groups={hosts.all} onenter={() => void save()} />

  <label class="field notes-field">
    <span>Notes</span>
    <textarea bind:value={notes} aria-invalid={!!errors.notes} spellcheck="true" rows="4" oninput={() => (errors.notes = undefined)}></textarea>
    {#if errors.notes}
      <p class="inline-error"><span>{sentence(errors.notes.message)}</span>{#if errors.notes.hint}<span class="hint">{errors.notes.hint}</span>{/if}</p>
    {:else}
      <span class="help counter" class:over={notes.length > NOTES_MAX}>{notes.length} / {NOTES_MAX}</span>
    {/if}
  </label>

  {#if errors.other}
    <p class="inline-error"><span>{sentence(errors.other.message)}</span>{#if errors.other.hint}<span class="hint">{errors.other.hint}</span>{/if}</p>
  {/if}

  {#snippet footer()}
    <span class="footer-note">Portico never edits <span class="mono">~/.ssh/config</span>: these live in <span class="mono">hosts.toml</span>.</span>
    <button class="secondary" onclick={onclose}>Cancel <kbd>Esc</kbd></button>
    <button class="primary" disabled={!armed || busy} onclick={() => void save()}>
      {#if busy}<span class="spinner" aria-hidden="true"></span>{/if}Save <kbd>{saveKey}</kbd>
    </button>
  {/snippet}
</Modal>

<style>
  /* Notes are prose: the UI font, not the shared mono textarea. */
  .field.notes-field textarea { font-family: var(--font-ui); font-size: var(--text-base); }
  .counter { justify-self: end; font-variant-numeric: tabular-nums; }
  .counter.over { color: var(--status-error); }
  /* Wraps beside the buttons rather than pushing them to a second line. */
  .footer-note { flex: 1 1 auto; min-width: 0; white-space: normal; }
</style>
