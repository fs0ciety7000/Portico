<script lang="ts">
  // Tunnels of every session, forwards suggested by ssh_config for the
  // current host (never started on their own), and the -L / -R form. A bind
  // beyond loopback needs an explicit, armed confirmation.
  import { fly } from "svelte/transition";
  import type { Host } from "../api";
  import { backend } from "../backend";
  import { dur } from "../motion";
  import { tunnels } from "../state/tunnels.svelte";
  import Modal from "./Modal.svelte";

  let {
    session,
    alias,
    host,
    onclose,
  }: { session: number | null; alias: string | undefined; host: Host | undefined; onclose: () => void } = $props();

  let spec = $state("-L ");
  let error: string | null = $state(null);
  let busy = $state(false);
  let armed = $state(false);
  /** Exposed spec waiting for confirmation. */
  let confirm: { spec: string; display: string } | null = $state(null);
  let input: HTMLInputElement | undefined = $state();
  let cancelButton: HTMLButtonElement | undefined = $state();

  // The dialog armed when it opened; the exposure confirmation arms again on
  // its own, after a pause without keystroke or click, so a double click or a
  // held Enter on "Start" can never confirm it unread.
  const CONFIRM_ARM_DELAY = 400;
  let confirmArmed = $state(false);
  $effect(() => {
    if (!confirm) return;
    confirmArmed = false;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const restart = () => {
      confirmArmed = false;
      clearTimeout(timer);
      timer = setTimeout(() => (confirmArmed = true), CONFIRM_ARM_DELAY);
    };
    restart();
    queueMicrotask(() => cancelButton?.focus());
    window.addEventListener("keydown", restart, true);
    window.addEventListener("pointerdown", restart, true);
    return () => {
      clearTimeout(timer);
      window.removeEventListener("keydown", restart, true);
      window.removeEventListener("pointerdown", restart, true);
    };
  });

  $effect(() => {
    void tunnels.sync();
  });

  const suggestions = $derived(
    session === null || !host
      ? []
      : [...host.localForwards.map((f) => `-L ${f}`), ...host.remoteForwards.map((f) => `-R ${f}`)],
  );

  /** Suggestions in their canonical form (same text as running tunnels). */
  let checked: { spec: string; display: string }[] = $state([]);
  $effect(() => {
    const list = suggestions;
    void Promise.all(
      list.map((spec) => backend.checkForward(spec).then((c) => ({ spec, display: c.display }), () => null)),
    ).then((r) => {
      if (list === suggestions) checked = r.filter((x): x is { spec: string; display: string } => x !== null);
    });
  });
  const offered = $derived(
    checked.filter((c) => !tunnels.list.some((t) => t.session === session && t.status !== "failed" && t.display === c.display)),
  );

  async function submit(text: string) {
    if (session === null || busy) return;
    error = null;
    busy = true;
    try {
      const check = await backend.checkForward(text);
      if (check.exposed) {
        confirm = { spec: text, display: check.display };
        return;
      }
      if (await tunnels.start(session, text, false)) spec = "-L ";
    } catch (e) {
      error = (e as { message?: string }).message ?? String(e);
    } finally {
      busy = false;
    }
  }

  async function expose() {
    const c = confirm;
    if (!c || session === null || !armed || !confirmArmed) return;
    confirm = null;
    if (await tunnels.start(session, c.spec, true)) spec = "-L ";
    queueMicrotask(() => input?.focus());
  }

  function cancelConfirm() {
    confirm = null;
    queueMicrotask(() => input?.focus());
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      void submit(spec);
    }
  }
</script>

<Modal onescape={() => (confirm ? cancelConfirm() : onclose())} bind:armed labelledBy="tunnels-title">
  {#snippet title()}Tunnels{#if alias}<span class="title-host"> · {alias}</span>{/if}{/snippet}

  {#if tunnels.list.length || offered.length}
    <ul class="tunnels" inert={confirm !== null}>
      {#each tunnels.list as t (t.id)}
        <li data-status={t.status} in:fly={{ y: 6, duration: dur("base") }}>
          <span class="dot" class:busy={t.status === "starting"}></span>
          <div class="main">
            <span class="spec mono">{t.display}</span>
            <span class="sub">
              <span>{t.alias}</span>
              {#if t.status === "failed"}
                <span class="err">{t.error?.message}{t.error?.hint ? ` — ${t.error.hint}` : ""}</span>
              {:else if t.bind}
                <span class="mono">{t.bind}</span>
                {#if t.connections}<span>{t.connections} {t.connections === 1 ? "connection" : "connections"}</span>{/if}
              {:else}
                <span>Starting…</span>
              {/if}
              {#if t.exposed}<span class="exposed">exposed</span>{/if}
            </span>
          </div>
          <button class="secondary small" onclick={() => tunnels.stop(t.id)}>{t.status === "failed" ? "Dismiss" : "Stop"}</button>
        </li>
      {/each}
      {#each offered as s (s.spec)}
        <li class="suggested">
          <span class="dot hollow"></span>
          <div class="main">
            <span class="spec mono">{s.display}</span>
            <span class="sub"><span>Declared in ssh_config · not started</span></span>
          </div>
          <button class="secondary small" onclick={() => submit(s.spec)} disabled={busy}>Start</button>
        </li>
      {/each}
    </ul>
  {:else}
    <div class="empty">
      <strong>No tunnels yet</strong>
      <span>Forward a local port to a service the server can reach, or the other way round.</span>
    </div>
  {/if}

  {#if confirm}
    <div class="confirm" role="alert" transition:fly={{ y: 6, duration: dur("base") }}>
      <strong><span class="mono">{confirm.display}</span> listens beyond this machine</strong>
      <span>Anyone who can reach this computer on the network can use the tunnel.</span>
    </div>
  {:else if session !== null}
    <div class="form">
      <input
        bind:this={input}
        bind:value={spec}
        class="mono"
        placeholder="-L 8080:db.internal:5432"
        spellcheck="false"
        aria-label="Tunnel"
        aria-invalid={!!error}
        data-autofocus
        {onkeydown}
      />
      <span class="help">
        {#if error}<span class="err">{error}</span>
        {:else}<span class="mono">-L</span> local port → host:port via the server · <span class="mono">-R</span> server port → here{/if}
      </span>
    </div>
  {:else}
    <p class="help">Connect to a host to add a tunnel.</p>
  {/if}

  {#snippet footer()}
    {#if confirm}
      <!-- Re-armed on appearance and after every click: a double click on
           "Start" lands on a disabled button. -->
      <button class="secondary" bind:this={cancelButton} onclick={cancelConfirm}>Cancel <kbd>Esc</kbd></button>
      <button class="warning" disabled={!armed || !confirmArmed} onclick={expose}>Expose tunnel</button>
    {:else}
      <button class="secondary" onclick={onclose}>Close <kbd>Esc</kbd></button>
      {#if session !== null}
        <button class="primary" disabled={busy || spec.trim().length < 4} onclick={() => submit(spec)}>Start <kbd>↵</kbd></button>
      {/if}
    {/if}
  {/snippet}
</Modal>

<style>
  .tunnels { display: grid; gap: var(--space-1); margin: 0; padding: 0; list-style: none; }
  li {
    --c: var(--status-connecting);
    display: grid;
    grid-template-columns: 8px minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-3);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  li[data-status="listening"] { --c: var(--status-connected); }
  li[data-status="failed"] { --c: var(--status-error); }
  li.suggested { --c: var(--status-idle); background: transparent; border: 1px dashed var(--border-subtle); }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--c); }
  .dot.hollow { background: transparent; box-shadow: inset 0 0 0 1.5px var(--c); }
  .dot.busy { animation: pulse var(--motion-loop) var(--ease-standard) infinite; }
  @keyframes pulse {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--c) 60%, transparent); }
    100% { box-shadow: 0 0 0 4px transparent; }
  }
  .main { display: grid; gap: 2px; min-width: 0; }
  .spec { color: var(--text-primary); font-size: var(--text-sm); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
  .sub { display: flex; flex-wrap: wrap; gap: var(--space-2); color: var(--text-muted); font-size: var(--text-xs); }
  .sub > * + *::before { content: "·"; margin-right: var(--space-2); }
  .err { color: var(--status-error); }
  .exposed { color: var(--status-warning); }
  .empty { display: grid; gap: var(--space-1); padding: var(--space-3) 0; }
  .empty strong { color: var(--text-primary); font-weight: 550; }
  .form { display: grid; gap: var(--space-2); margin-top: var(--space-2); }
  input {
    height: 36px;
    padding: 0 var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
    color: var(--text-primary);
    font-size: var(--text-md);
    outline: none;
    transition: border-color var(--motion-fast) var(--ease-standard);
  }
  input:focus { border-color: var(--accent); }
  input[aria-invalid="true"] { border-color: var(--status-error); }
  .help { color: var(--text-muted); font-size: var(--text-xs); }
  .confirm {
    display: grid;
    gap: var(--space-1);
    margin-top: var(--space-2);
    padding: var(--space-3) var(--space-4);
    border: 1px solid color-mix(in srgb, var(--status-warning) 45%, transparent);
    border-left: 3px solid var(--status-warning);
    border-radius: var(--radius-md);
    background: color-mix(in srgb, var(--status-warning) 6%, var(--bg-elevated));
  }
  .confirm strong { color: var(--text-primary); font-weight: 550; }
  :global(.modal button.small) { height: 26px; padding: 0 var(--space-3); font-size: var(--text-sm); }
  :global(.modal button.warning) {
    border: 0;
    background: var(--status-warning);
    color: var(--text-on-accent);
    font-weight: 600;
  }
</style>
