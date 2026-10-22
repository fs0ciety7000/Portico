<script lang="ts">
  // "Add host…" / "Edit host…" (phase 6b): a host of Portico's own
  // hosts.conf. The engine validates every field; its errors come back by
  // code and show under their field, with the hint. The key is picked among
  // the keys the engine discovered (by id): the webview never names a path.
  // The password goes to the engine once, then the field is cleared.
  import { onMount, untrack } from "svelte";
  import { asError, sentence, type ErrorSummary, type Host, type LocalKey } from "../api";
  import { backend } from "../backend";
  import {
    capitalizable,
    emptyForm,
    fieldOf,
    FIELD_ORDER,
    formFromHost,
    isDirty,
    KEEP_IDENTITY,
    keyValue,
    toDraft,
    type FieldName,
    type HostForm,
    type PasswordMode,
  } from "../hostform";
  import { isMac } from "../keys";
  import { hosts } from "../state/hosts.svelte";
  import { workspace } from "../state/workspace.svelte";
  import GroupField from "./GroupField.svelte";
  import Modal from "./Modal.svelte";
  import OriginBadge from "./OriginBadge.svelte";
  import TagsField from "./TagsField.svelte";

  let { host }: { host?: Host } = $props();

  const editing = untrack(() => !!host);
  /** The alias the host had when the dialog opened (a rename changes the form's). */
  const original = untrack(() => host?.alias ?? "");
  /** Same bound as the core (`MAX_NOTES_CHARS`), for the counter only. */
  const NOTES_MAX = 2000;

  let armed = $state(false);
  let keys: LocalKey[] = $state([]);
  let keysLoading = $state(true);
  let keysError: ErrorSummary | null = $state(null);
  // Snapshot at opening (keys refine the key choice once listed).
  let initial: HostForm = $state(untrack(() => (host ? formFromHost(host, []) : emptyForm())));
  let form: HostForm = $state(structuredClone($state.snapshot(initial)));
  let errors: Partial<Record<FieldName, ErrorSummary>> = $state({});
  let formError: ErrorSummary | null = $state(null);
  let busy = $state(false);
  let confirmDiscard = $state(false);
  let tagsField: TagsField | undefined = $state();
  let body: HTMLDivElement | undefined = $state();
  let keepEditing: HTMLButtonElement | undefined = $state();

  const saveKey = isMac ? "⌘S" : "Ctrl-S";
  const dirty = $derived(isDirty(initial, form));
  const usableKeys = $derived(keys.filter((k) => k.hasPrivate));
  /** Jump hosts: every other host (a cycle is the engine's to refuse, with its chain). */
  const jumpChoices = $derived.by(() => {
    const list = hosts.all.map((h) => h.alias).filter((a) => a !== original);
    if (form.proxyJump && !list.includes(form.proxyJump)) list.unshift(form.proxyJump);
    return list;
  });

  onMount(() => {
    backend.listKeys().then(
      (list) => {
        keys = list;
        keysLoading = false;
        if (!host) return;
        // The current key, now that the discovered ones are known.
        const refined = formFromHost(host, list).identity;
        if (form.identity === initial.identity) form.identity = refined;
        initial.identity = refined;
      },
      (e) => {
        keysLoading = false;
        keysError = asError(e);
      },
    );
  });

  // The discard question takes the keyboard (y / n, or ↵ on "Keep editing").
  $effect(() => {
    if (confirmDiscard) queueMicrotask(() => keepEditing?.focus());
  });

  /** An error message as shown: a sentence, unless it starts with an alias. */
  const say = (e: ErrorSummary) => (capitalizable(e.code) ? sentence(e.message) : e.message);

  function clear(field: FieldName) {
    if (errors[field]) errors[field] = undefined;
    formError = null;
  }

  function focusField(field: FieldName) {
    queueMicrotask(() => {
      if (field === "tags") return tagsField?.focus();
      body?.querySelector<HTMLElement>(`[data-field="${field}"]`)?.focus();
    });
  }

  function showError(e: unknown) {
    const error = asError(e);
    const field = fieldOf(error.code);
    if (field === "tags") tagsField?.setError(e);
    else if (field) errors[field] = error;
    else formError = error;
    if (field) focusField(field);
  }

  async function save() {
    if (!armed || busy || confirmDiscard) return;
    tagsField?.commitPending();
    errors = {};
    formError = null;
    const result = toDraft(form, editing);
    if (!result.ok) {
      errors[result.field] = result.error;
      focusField(result.field);
      return;
    }
    busy = true;
    try {
      const alias = editing ? await backend.updateHost(original, result.draft) : await backend.createHost(result.draft);
      hosts.dialog = null;
      hosts.changed(alias);
      workspace.notify({ code: "host_saved", message: editing ? `${alias} saved` : `${alias} added to your hosts`, hint: null }, "info");
    } catch (e) {
      showError(e);
    } finally {
      // The password crossed the IPC once: nothing keeps it here.
      form.password = "";
      if (result.draft.password.kind === "set") result.draft.password = { kind: "keep" };
      busy = false;
    }
  }

  function close() {
    form.password = "";
    hosts.dialog = null;
  }

  function onescape() {
    if (confirmDiscard) confirmDiscard = false;
    else if (dirty && !busy) confirmDiscard = true;
    else close();
  }

  function onshortcut(e: KeyboardEvent): boolean {
    if (e.key.toLowerCase() === "s" || e.key === "Enter") {
      void save();
      return true;
    }
    return false;
  }

  function onkey(key: string): boolean {
    if (!confirmDiscard) return false;
    if (key === "y") close();
    else if (key === "n") confirmDiscard = false;
    else return false;
    return true;
  }

  /** ↵ in a single-line field or a select saves. */
  function onEnter(e: KeyboardEvent) {
    if (e.key !== "Enter" || e.isComposing) return;
    e.preventDefault();
    void save();
  }

  function setPasswordMode(mode: PasswordMode) {
    form.passwordMode = mode;
    form.password = "";
    clear("password");
    if (mode === "set") focusField("password");
  }

  const passwordModes: [PasswordMode, string][] = [
    ["keep", "Keep"],
    ["set", "Change"],
    ["forget", "Forget"],
  ];
</script>

{#snippet fieldError(field: FieldName, help: string | null)}
  {#if errors[field]}
    {@const e = errors[field]}
    <p class="inline-error" id={`host-${field}-error`}><span>{say(e)}</span>{#if e.hint}<span class="hint">{e.hint}</span>{/if}</p>
  {:else if help}
    <span class="help">{help}</span>
  {/if}
{/snippet}

<Modal bind:armed {onescape} {onshortcut} {onkey} size="wide" footerWrap={false} labelledBy="host-dialog-title">
  {#snippet title()}
    {#if editing}
      <span class="title-line">Edit <span class="mono">{original}</span><OriginBadge origin="portico" /></span>
    {:else}
      Add host
    {/if}
  {/snippet}

  <div class="form" bind:this={body}>
    <div class="grid">
      <label class="field">
        <span>Alias</span>
        <input
          class="mono"
          data-field="alias"
          data-autofocus
          bind:value={form.alias}
          placeholder="web-01"
          spellcheck="false"
          autocomplete="off"
          aria-invalid={!!errors.alias}
          oninput={() => clear("alias")}
          onkeydown={onEnter}
        />
        {@render fieldError("alias", "The name you connect with: ssh <alias>.")}
      </label>
      <label class="field">
        <span>HostName</span>
        <input
          class="mono"
          data-field="hostname"
          bind:value={form.hostname}
          placeholder="203.0.113.10 or web-01.example.com"
          spellcheck="false"
          autocomplete="off"
          aria-invalid={!!errors.hostname}
          oninput={() => clear("hostname")}
          onkeydown={onEnter}
        />
        {@render fieldError("hostname", "DNS name or IP address.")}
      </label>
      <label class="field">
        <span>User</span>
        <input
          class="mono"
          data-field="user"
          bind:value={form.user}
          placeholder="Your local user"
          spellcheck="false"
          autocomplete="off"
          aria-invalid={!!errors.user}
          oninput={() => clear("user")}
          onkeydown={onEnter}
        />
        {@render fieldError("user", null)}
      </label>
      <label class="field">
        <span>Port</span>
        <input
          class="mono port"
          data-field="port"
          bind:value={form.port}
          placeholder="22"
          inputmode="numeric"
          spellcheck="false"
          autocomplete="off"
          aria-invalid={!!errors.port}
          oninput={() => clear("port")}
          onkeydown={onEnter}
        />
        {@render fieldError("port", null)}
      </label>
      <label class="field">
        <span>Key</span>
        <span class="select">
          <select data-field="identity" bind:value={form.identity} aria-invalid={!!errors.identity} onchange={() => clear("identity")} onkeydown={onEnter}>
            <option value="">None · agent and default keys</option>
            {#if form.identity === KEEP_IDENTITY || initial.identity === KEEP_IDENTITY}
              <option value={KEEP_IDENTITY}>{host?.managed?.identityFile ?? "Current key"} (current)</option>
            {/if}
            {#each usableKeys as k (k.id)}
              <option value={keyValue(k.id)}>{k.id} · {k.algorithm}</option>
            {/each}
          </select>
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4.5 6 3.5 3.5L11.5 6" /></svg>
        </span>
        {#if errors.identity}
          {@render fieldError("identity", null)}
        {:else if keysLoading}
          <span class="help"><span class="spinner" aria-hidden="true"></span> Looking for keys in ~/.ssh…</span>
        {:else if keysError}
          <span class="help warn">{sentence(keysError.message)}</span>
        {:else}
          <span class="help">{usableKeys.length ? "Keys found in ~/.ssh." : "No private key in ~/.ssh: create one in Keys."}</span>
        {/if}
      </label>
      <label class="field">
        <span>ProxyJump</span>
        <span class="select">
          <select data-field="proxyJump" bind:value={form.proxyJump} aria-invalid={!!errors.proxyJump} onchange={() => clear("proxyJump")} onkeydown={onEnter}>
            <option value="">None · direct connection</option>
            {#each jumpChoices as alias (alias)}
              <option value={alias}>{alias}</option>
            {/each}
          </select>
          <svg viewBox="0 0 16 16" aria-hidden="true"><path d="m4.5 6 3.5 3.5L11.5 6" /></svg>
        </span>
        {@render fieldError("proxyJump", "Reach this host through another one.")}
      </label>
    </div>

    <div class="field password-field">
      <span id="host-password-label">Password</span>
      {#if editing}
        <div class="segmented" role="group" aria-labelledby="host-password-label">
          {#each passwordModes as [mode, label] (mode)}
            <button type="button" aria-pressed={form.passwordMode === mode} onclick={() => setPasswordMode(mode)}>{label}</button>
          {/each}
        </div>
      {/if}
      {#if !editing || form.passwordMode === "set"}
        <input
          type="password"
          data-field="password"
          bind:value={form.password}
          placeholder={editing ? "New password" : "Optional"}
          autocomplete="new-password"
          aria-labelledby="host-password-label"
          aria-invalid={!!errors.password}
          oninput={() => clear("password")}
          onkeydown={onEnter}
        />
      {/if}
      {#if errors.password}
        {@render fieldError("password", null)}
      {:else if editing && form.passwordMode === "forget"}
        <span class="help">The remembered password is forgotten when you save.</span>
      {:else if editing && form.passwordMode === "keep"}
        <span class="help">The remembered password, if any, stays as it is.</span>
      {:else}
        <span class="help">Remembered in your keyring (or the vault), never written to a file. Offered only once the host key is verified.</span>
      {/if}
    </div>

    <TagsField bind:this={tagsField} bind:tags={form.tags} bind:error={errors.tags} onenter={() => void save()} />

    <div class="grid">
      <GroupField bind:value={form.group} bind:error={errors.group} groups={hosts.all} onenter={() => void save()} />
      <div class="field">
        <span>Favorite</span>
        <label class="check favorite">
          <input type="checkbox" bind:checked={form.favorite} onkeydown={onEnter} />
          Pin to favorites <span class="mono muted">*</span>
        </label>
      </div>
    </div>

    <label class="field notes-field">
      <span>Notes</span>
      <textarea data-field="notes" bind:value={form.notes} aria-invalid={!!errors.notes} spellcheck="true" rows="3" oninput={() => clear("notes")}></textarea>
      {#if errors.notes}
        {@render fieldError("notes", null)}
      {:else}
        <span class="help counter" class:over={form.notes.length > NOTES_MAX}>{form.notes.length} / {NOTES_MAX}</span>
      {/if}
    </label>

    {#if formError}
      <p class="inline-error form-error" role="alert"><span>{say(formError)}</span>{#if formError.hint}<span class="hint">{formError.hint}</span>{/if}</p>
    {/if}
  </div>

  {#snippet footer()}
    {#if confirmDiscard}
      <span class="footer-note discard" role="alert">Discard your changes?</span>
      <button class="secondary" onclick={close}>Discard <kbd>y</kbd></button>
      <button class="primary" bind:this={keepEditing} onclick={() => (confirmDiscard = false)}>Keep editing <kbd>n</kbd></button>
    {:else}
      <span class="footer-note">Saved in Portico's <span class="mono">hosts.conf</span>. <span class="mono">~/.ssh/config</span> is never edited.</span>
      <button class="secondary" onclick={onescape}>Cancel <kbd>Esc</kbd></button>
      <button class="primary" disabled={!armed || busy} onclick={() => void save()}>
        {#if busy}<span class="spinner" aria-hidden="true"></span>{/if}{editing ? "Save" : "Add host"} <kbd>↵</kbd>
      </button>
    {/if}
  {/snippet}
</Modal>

<style>
  .form { display: grid; gap: var(--space-4); }
  .grid {
    display: grid;
    grid-template-columns: repeat(2, minmax(0, 1fr));
    gap: var(--space-4) var(--space-4);
    align-items: start;
  }
  @media (max-width: 720px) {
    .grid { grid-template-columns: minmax(0, 1fr); }
  }
  .title-line { display: inline-flex; align-items: center; gap: var(--space-2); }
  .field input.port { max-width: 12ch; }
  /* Native select, drawn like the inputs (the popup stays the system's). */
  .select { position: relative; display: grid; }
  .select select {
    appearance: none;
    height: 34px;
    padding: 0 calc(var(--space-3) + 16px) 0 var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    outline: none;
    text-overflow: ellipsis;
    transition: border-color var(--motion-fast) var(--ease-standard);
  }
  .select select:focus { border-color: var(--accent); box-shadow: 0 0 0 1px var(--accent); }
  .select select[aria-invalid="true"] { border-color: var(--status-error); }
  .select select option { background: var(--bg-overlay); color: var(--text-primary); }
  .select svg {
    position: absolute;
    right: var(--space-3);
    top: 50%;
    width: 12px;
    height: 12px;
    margin-top: -6px;
    fill: none;
    stroke: var(--text-muted);
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
    pointer-events: none;
  }
  .password-field { gap: var(--space-2); }
  .password-field input { max-width: 40ch; }
  .favorite { height: 34px; }
  .muted { color: var(--text-muted); }
  .warn { color: var(--status-warning); }
  .help .spinner { width: 10px; height: 10px; vertical-align: -1px; }
  /* Notes are prose: the UI font, not the shared mono textarea. */
  .field.notes-field textarea { font-family: var(--font-ui); font-size: var(--text-base); min-height: calc(3 * 1.5em + 2 * var(--space-2)); }
  .counter { justify-self: end; font-variant-numeric: tabular-nums; }
  .counter.over { color: var(--status-error); }
  .form-error {
    padding: var(--space-2) var(--space-3);
    border-left: 2px solid var(--status-error);
    border-radius: var(--radius-sm);
    background: color-mix(in srgb, var(--status-error) 8%, transparent);
  }
  .footer-note { flex: 1 1 auto; min-width: 0; white-space: normal; }
  .footer-note.discard { color: var(--text-primary); font-size: var(--text-sm); }
</style>
