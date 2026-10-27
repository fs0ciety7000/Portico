<script lang="ts">
  // The Sync panel (phase 11, docs/SYNC.md §6): status and what to do about
  // it, conflicts, received changes to confirm, issues, devices. Keys: 1–5
  // switch tabs, `s` syncs now.
  import { fly } from "svelte/transition";
  import { sentence, type ErrorSummary } from "../api";
  import { backend } from "../backend";
  import { dur } from "../motion";
  import { ago, explainAlert, explainFolder, explainWait, subjectLabel, syncLabel } from "../syncview";
  import { sync, type SyncTab } from "../state/sync.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";
  import SyncConflicts from "./SyncConflicts.svelte";
  import SyncReceived from "./SyncReceived.svelte";

  let { tab }: { tab: SyncTab } = $props();

  let armed = $state(false);
  let busy: string | null = $state(null);
  let error: ErrorSummary | null = $state(null);

  const status = $derived(sync.status);
  const head = $derived(syncLabel(status, sync.now));
  const issueCount = $derived(sync.issues.length);
  const TABS: { id: SyncTab; label: string }[] = [
    { id: "overview", label: "Overview" },
    { id: "conflicts", label: "Conflicts" },
    { id: "received", label: "Received" },
    { id: "issues", label: "Issues" },
    { id: "devices", label: "Devices" },
  ];
  const countOf = (id: SyncTab) =>
    id === "conflicts" ? sync.conflicts.length : id === "received" ? sync.received : id === "issues" ? issueCount : id === "devices" ? sync.devices.length : 0;

  function go(id: SyncTab) {
    sync.view = { kind: "panel", tab: id };
  }

  async function run(label: string, action: () => Promise<unknown>, done?: string) {
    busy = label;
    error = null;
    const failed = await sync.act(action as () => Promise<void>);
    busy = null;
    if (failed) error = failed;
    else if (done) workspace.notify({ code: "sync", message: done, hint: null }, "info");
  }

  const syncNow = () => run("Syncing…", () => backend.syncNow());
  const acknowledge = () => run("Acknowledging…", () => backend.syncAcknowledgeAlerts(), "Alert acknowledged");
  const setRemember = (on: boolean) => run("Saving…", () => backend.syncSetOptions(on), on ? "The key is remembered on this device" : "The key is no longer remembered here");
  const skip = (key: string) => run("Skipping…", () => backend.syncSkipOnThisDevice(key), "Skipped on this device");

  function onkey(key: string): boolean {
    const n = Number(key);
    const t = TABS[n - 1];
    if (t) {
      go(t.id);
      return true;
    }
    if (key === "s" && sync.enabled && !busy) {
      void syncNow();
      return true;
    }
    return false;
  }

  const stamp = (secs: number | null) => (secs ? ago(secs, sync.now) : "never");
</script>

<Modal bind:armed onescape={() => sync.close()} {onkey} labelledBy="sync-panel-title" size="wide" scroll={false}>
  {#snippet title()}Sync{/snippet}

  <nav class="tabs" aria-label="Sync sections">
    {#each TABS as t, i (t.id)}
      <button class="tab" aria-current={tab === t.id ? "page" : undefined} onclick={() => go(t.id)} title={`${t.label} (${i + 1})`}>
        {t.label}
        {#if countOf(t.id) && t.id !== "devices"}<span class="badge" data-kind={t.id}>{countOf(t.id)}</span>{/if}
      </button>
    {/each}
  </nav>

  <div class="content">
    {#key tab}
      <div class="pane" in:fly={{ y: 6, duration: dur("base") }}>
        {#if tab === "overview"}
          <section class="status" data-tone={head.tone} data-testid="sync-overview">
            <div class="headline">
              <span class="dot" class:busy={head.tone === "busy"}></span>
              <strong>{head.label}</strong>
            </div>
            <dl class="facts">
              <dt>Folder</dt><dd>{sync.overview?.folder ?? "—"}</dd>
              <dt>This device</dt><dd>{sync.overview?.device ?? "—"}</dd>
            </dl>
          </section>

          {#if status?.state === "alert"}
            {@const why = explainAlert(status.kind, status.device)}
            <section class="callout" data-tone="error" role="alert">
              <strong>{why.title}</strong>
              <p>{why.body}</p>
              <p class="hint">{why.hint}</p>
              {#if status.kind === "passphrase_changed"}
                <div class="row"><button class="secondary small" disabled={!!busy} onclick={acknowledge}>I expected it, acknowledge</button></div>
              {/if}
            </section>
          {:else if status?.state === "folder_unavailable"}
            {@const why = explainFolder(status.reason)}
            <section class="callout" data-tone="warning">
              <strong>{why.title}</strong>
              <p>{why.body}</p>
              <p class="hint">{why.hint}</p>
            </section>
          {:else if status?.state === "waiting"}
            <section class="callout" data-tone="quiet">
              <p>{explainWait(status.reason, status.device)} Portico checks again every 30 seconds.</p>
            </section>
          {:else if status?.state === "locked"}
            <section class="callout" data-tone="warning">
              <strong>The sync key is not on this device</strong>
              <p>This device does not remember the key: enter the sync passphrase to sync again (Unlock, below).</p>
            </section>
          {:else if status?.state === "error"}
            <section class="callout" data-tone="error" role="alert">
              <strong>{sentence(status.summary.message)}</strong>
              {#if status.summary.hint}<p class="hint">{status.summary.hint}</p>{/if}
            </section>
          {/if}

          {#if sync.conflicts.length || sync.received || issueCount}
            <section class="todo">
              {#if sync.conflicts.length}<button class="link" onclick={() => go("conflicts")}><span class="n warning">{sync.conflicts.length}</span> {sync.conflicts.length === 1 ? "conflict" : "conflicts"} to settle</button>{/if}
              {#if sync.received}<button class="link" onclick={() => go("received")}><span class="n accent">{sync.received}</span> received {sync.received === 1 ? "change" : "changes"} to confirm</button>{/if}
              {#if issueCount}<button class="link" onclick={() => go("issues")}><span class="n">{issueCount}</span> {issueCount === 1 ? "issue" : "issues"}</button>{/if}
            </section>
          {/if}

          <section class="group">
            <span class="caption">This device</span>
            <label class="check">
              <input type="checkbox" checked={sync.overview?.rememberKey ?? true} disabled={!!busy || status?.state === "locked"} onchange={(e) => void setRemember(e.currentTarget.checked)} />
              Remember the key on this device
            </label>
            <div class="row">
              <button class="secondary small" onclick={() => sync.open({ kind: "form", form: "rename" })}>Rename device…</button>
              <button class="secondary small" onclick={() => sync.open({ kind: "form", form: "change" })}>Change passphrase…</button>
              <button class="secondary small" onclick={() => sync.open({ kind: "form", form: "reset" })} disabled={status?.state === "locked"}>Reset passphrase…</button>
              <button class="secondary small danger" onclick={() => sync.open({ kind: "form", form: "disable" })}>Turn sync off…</button>
            </div>
          </section>
        {:else if tab === "conflicts"}
          <SyncConflicts />
        {:else if tab === "received"}
          <SyncReceived />
        {:else if tab === "issues"}
          {#if sync.lists === "loading"}
            <div class="skeleton" aria-busy="true"></div>
          {:else if !sync.issues.length}
            <p class="empty">No issue. Every received change could be applied.</p>
          {:else}
            <ul class="list" aria-label="Issues">
              {#each sync.issues as issue (issue.key)}
                <li class="issue" data-tone={issue.warning ? "warning" : "error"}>
                  <div class="issue-text">
                    <span class="subject mono">{subjectLabel(issue.subject)}</span>
                    <strong>{sentence(issue.summary.message)}</strong>
                    {#if issue.summary.hint}<span class="hint">{issue.summary.hint}</span>{/if}
                    <span class="code mono">{issue.summary.code}</span>
                  </div>
                  {#if issue.skippable}
                    <button class="secondary small" disabled={!!busy} onclick={() => skip(issue.key)} title="Keep this record as it is on this device; the other devices keep theirs">Skip on this device</button>
                  {/if}
                </li>
              {/each}
            </ul>
          {/if}
        {:else if tab === "devices"}
          {#if sync.lists === "loading"}
            <div class="skeleton" aria-busy="true"></div>
          {:else if !sync.devices.length}
            <p class="empty">No device yet.</p>
          {:else}
            <ul class="list" aria-label="Devices">
              {#each sync.devices as d (d.id)}
                <li class="device" class:inactive={d.stale || d.revoked}>
                  <span class="mono name">{d.label}</span>
                  {#if d.this_device}<span class="pill">this device</span>{/if}
                  {#if d.revoked}<span class="pill">removed</span>{:else if d.stale}<span class="pill">inactive</span>{/if}
                  <span class="when">published {stamp(d.published_at)} · seen {d.this_device ? "now" : stamp(d.last_seen)}</span>
                </li>
              {/each}
            </ul>
          {/if}
        {/if}
      </div>
    {/key}
    {#if sync.lists === "error" && sync.listError}
      <div class="inline-error" role="alert"><span>{sentence(sync.listError.message)}</span>{#if sync.listError.hint}<span class="hint">{sync.listError.hint}</span>{/if}</div>
    {/if}
    {#if error}
      <div class="inline-error" role="alert"><span>{sentence(error.message)}</span>{#if error.hint}<span class="hint">{error.hint}</span>{/if}</div>
    {/if}
  </div>

  {#snippet footer()}
    {#if busy}<span class="footer-note busy"><span class="spinner"></span>{busy}</span>{:else}<span class="footer-note"><kbd>1</kbd>–<kbd>5</kbd> sections</span>{/if}
    <button class="secondary" onclick={() => sync.close()}>Close <kbd>Esc</kbd></button>
    {#if status?.state === "locked"}
      <button class="primary" onclick={() => sync.open({ kind: "form", form: "unlock" })}>Unlock…</button>
    {:else}
      <button class="primary" disabled={!!busy || !sync.enabled} onclick={syncNow}>Sync now <kbd>s</kbd></button>
    {/if}
  {/snippet}
</Modal>

<style>
  .tabs { display: flex; flex-wrap: wrap; gap: var(--space-1); border-bottom: 1px solid var(--border-subtle); }
  .tabs .tab {
    position: relative;
    height: var(--size-control-sm);
    padding: 0 var(--space-3);
    border: 0;
    border-radius: var(--radius-sm) var(--radius-sm) 0 0;
    background: transparent;
    color: var(--text-secondary);
  }
  .tabs .tab:hover { color: var(--text-primary); }
  .tabs .tab[aria-current="page"] { color: var(--text-primary); box-shadow: inset 0 -2px 0 var(--accent); }
  .badge {
    display: inline-grid;
    place-items: center;
    min-width: var(--size-badge-sm);
    height: var(--size-badge-sm);
    padding: 0 var(--space-1);
    border-radius: var(--radius-pill);
    background: var(--bg-elevated);
    color: var(--text-secondary);
    font-family: var(--font-mono);
    font-size: var(--text-xs);
    font-variant-numeric: tabular-nums;
  }
  .badge[data-kind="conflicts"] { color: var(--status-warning); }
  .badge[data-kind="received"] { color: var(--status-authenticating); }
  /* A steady height: switching sections does not resize the dialog. */
  .content { display: grid; flex: 1 1 auto; align-content: start; gap: var(--space-3); min-height: min(440px, 55vh); overflow-y: auto; padding-top: var(--space-3); }
  .pane { display: grid; gap: var(--space-4); align-content: start; }
  .status { display: grid; gap: var(--space-2); }
  .headline { display: flex; align-items: center; gap: var(--space-2); }
  .headline strong { color: var(--text-primary); font-family: var(--font-display); font-size: var(--text-lg); font-weight: 600; }
  .dot { width: var(--size-dot); height: var(--size-dot); border-radius: 50%; background: var(--status-idle); }
  .status[data-tone="quiet"] .dot { background: var(--status-connected); }
  .status[data-tone="busy"] .dot { background: var(--status-connecting); }
  .status[data-tone="warning"] .dot { background: var(--status-warning); }
  .status[data-tone="error"] .dot { background: var(--status-error); }
  .dot.busy { animation: sync-pulse var(--motion-loop) var(--ease-standard) infinite; }
  @keyframes sync-pulse { 50% { opacity: 0.4; } }
  .facts { margin: 0; }
  .callout {
    --c: var(--border-strong);
    display: grid;
    gap: var(--space-1);
    padding: var(--space-3) var(--space-4);
    border-left: 2px solid var(--c);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  .callout strong { color: var(--text-primary); font-weight: 550; }
  .callout[data-tone="error"] { --c: var(--status-error); }
  .callout[data-tone="warning"] { --c: var(--status-warning); }
  .callout .hint, .hint { color: var(--text-muted); font-size: var(--text-sm); }
  .row { display: flex; flex-wrap: wrap; gap: var(--space-2); margin-top: var(--space-1); }
  .todo { display: flex; flex-wrap: wrap; gap: var(--space-2); }
  .link {
    height: var(--size-control-sm);
    padding: 0 var(--space-3);
    border: 1px solid var(--border-subtle);
    background: transparent;
    color: var(--text-primary);
  }
  .link:hover { background: var(--bg-elevated); }
  .n { font-family: var(--font-mono); font-variant-numeric: tabular-nums; }
  .n.warning { color: var(--status-warning); }
  .n.accent { color: var(--status-authenticating); }
  .group { display: grid; gap: var(--space-2); }
  .caption { color: var(--text-muted); font-size: var(--text-xs); letter-spacing: var(--tracking-caps); text-transform: uppercase; }
  .row .danger { color: var(--status-error); }
  .list { display: grid; gap: var(--space-2); margin: 0; padding: 0; list-style: none; }
  .issue {
    --c: var(--status-error);
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: var(--space-3);
    padding: var(--space-3) var(--space-4);
    border-left: 2px solid var(--c);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  .issue[data-tone="warning"] { --c: var(--status-warning); }
  .issue-text { display: grid; gap: var(--space-1); min-width: 0; }
  .issue strong { color: var(--text-primary); font-weight: 550; }
  .subject { color: var(--text-secondary); font-size: var(--text-sm); }
  .code { color: var(--text-muted); font-size: var(--text-xs); }
  .mono { font-family: var(--font-mono); }
  .device {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: var(--space-2);
    min-height: var(--size-row);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  .device .name { color: var(--text-primary); font-size: var(--text-sm); }
  .device.inactive .name { color: var(--text-muted); }
  .device .when { margin-left: auto; color: var(--text-muted); font-size: var(--text-xs); font-variant-numeric: tabular-nums; }
  .pill { padding: 0 var(--space-2); border: 1px solid var(--border-strong); border-radius: var(--radius-pill); color: var(--text-secondary); font-size: var(--text-xs); }
  .empty { color: var(--text-muted); }
  .skeleton { height: calc(3 * var(--size-row)); border-radius: var(--radius-md); background: var(--bg-elevated); animation: sync-pulse var(--motion-loop) var(--ease-standard) infinite; }
  .busy { display: inline-flex; align-items: center; gap: var(--space-2); }
</style>
