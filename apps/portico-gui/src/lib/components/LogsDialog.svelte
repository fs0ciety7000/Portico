<script lang="ts">
  // Debug panel: sessions on the left (live and the last ended ones), the
  // selected one's records or effective configuration on the right. Texts
  // arrive escaped from Rust (debug.rs); "Copy all" is the `to_line` text
  // built in Rust. Command keywords (ProxyCommand…) are never received.
  import { describe, sentence, type LogLevel, type SessionLogInfo } from "../api";
  import { isMac } from "../keys";
  import { logs } from "../state/logs.svelte";
  import { workspace } from "../state/workspace.svelte";
  import Modal from "./Modal.svelte";

  let armed = $state(false);
  let filterEl: HTMLInputElement | undefined = $state();
  let listEl: HTMLDivElement | undefined = $state();
  let configEl: HTMLDivElement | undefined = $state();
  let sessionsEl: HTMLUListElement | undefined = $state();
  /** Text to select by hand when the clipboard is not available. */
  let fallback: string | null = $state(null);
  let fallbackEl: HTMLTextAreaElement | undefined = $state();
  let copied = $state(false);

  const LEVELS: { id: LogLevel; label: string }[] = [
    { id: "debug", label: "Debug" },
    { id: "info", label: "Info" },
    { id: "warn", label: "Warn" },
    { id: "error", label: "Error" },
  ];

  const info = $derived(logs.selectedInfo);

  const pad = (n: number, w = 2) => String(n).padStart(w, "0");
  const clock = (ms: number) => {
    const d = new Date(ms);
    return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const clockMs = (ms: number) => `${clock(ms)}.${pad(new Date(ms).getMilliseconds(), 3)}`;
  const fullDate = (ms: number) => new Date(ms).toLocaleString();

  /** Short wording of a failure, by error code (the code itself is the tooltip). */
  const FAILURES: Record<string, string> = {
    host_key_changed: "host key changed",
    host_key_new_algorithm: "new host key type",
    host_key_unknown: "unknown host key",
    host_key_revoked: "host key revoked",
    host_cert_invalid: "invalid host certificate",
    auth_failed: "authentication failed",
    auth_disconnected: "dropped during authentication",
    dns: "host name not resolved",
    connect: "unreachable",
    timeout: "timed out",
    proxy_jump_cycle: "ProxyJump cycle",
    proxy_jump_too_deep: "ProxyJump too deep",
    unsupported: "unsupported option",
    invalid_host: "invalid host name",
    config: "ssh config error",
    terminal_refused: "terminal refused",
    cancelled: "cancelled",
    ssh: "SSH error",
  };

  /** Status of a listed session: the live phase when the workspace knows it.
   * "Refused" (the user turned a host key down) is neutral, not an error. */
  function statusOf(s: SessionLogInfo): { status: string; label: string; code?: string } {
    if (s.outcome?.kind === "failed") {
      const code = s.outcome.error.code;
      if (code === "host_key_rejected") return { status: "idle", label: "Refused", code };
      return { status: "error", label: `Failed · ${FAILURES[code] ?? code.replaceAll("_", " ")}`, code };
    }
    if (s.outcome?.kind === "closed") return { status: "idle", label: s.outcome.reason === "remote_closed" ? "Closed by server" : s.outcome.reason === "timeout" ? "Timed out" : "Closed" };
    const view = workspace.sessions[s.id];
    if (!view) return { status: "connecting", label: "Live" };
    const d = describe(view);
    return { status: d.status, label: d.label };
  }

  const configRows = $derived.by(() => {
    const q = logs.filter.trim().toLowerCase();
    const options = logs.config?.options ?? [];
    return q ? options.filter((o) => o.key.includes(q) || (!o.sensitive && o.values.some((v) => v.toLowerCase().includes(q)))) : options;
  });

  // Follow: keep the newest record in view.
  $effect(() => {
    void logs.visible.length;
    if (logs.follow && listEl) queueMicrotask(() => listEl && (listEl.scrollTop = listEl.scrollHeight));
  });

  function onscroll() {
    if (!listEl) return;
    const atBottom = listEl.scrollHeight - listEl.scrollTop - listEl.clientHeight < 8;
    if (logs.follow !== atBottom) logs.follow = atBottom;
  }

  /** Newest first, as listed. */
  const listed = $derived([...logs.sessions].reverse());

  function move(delta: number) {
    const at = listed.findIndex((s) => s.id === logs.selected);
    const next = listed[Math.max(0, Math.min(listed.length - 1, at + delta))];
    if (next) {
      void logs.select(next.id);
      queueMicrotask(() => sessionsEl?.querySelector('[aria-selected="true"]')?.scrollIntoView({ block: "nearest" }));
    }
  }

  async function copyAll() {
    let text: string;
    try {
      text = await logs.copyText();
    } catch (e) {
      workspace.notify(e);
      return;
    }
    try {
      await navigator.clipboard.writeText(text);
      copied = true;
      setTimeout(() => (copied = false), 1600);
    } catch {
      fallback = text;
      queueMicrotask(() => fallbackEl?.select());
    }
  }

  function refresh() {
    void logs.refreshSessions();
    void logs.loadLines();
    if (logs.tab === "config") void logs.loadConfig(true);
  }

  /** `v`: the next minimum level (Debug → Info → Warn → Error → Debug). */
  function cycleLevel() {
    const at = LEVELS.findIndex((l) => l.id === logs.minLevel);
    logs.minLevel = LEVELS[(at + 1) % LEVELS.length]?.id ?? "debug";
  }

  /** Scrolls the records (or the options): page, top, bottom. The bottom
   * turns following back on; anything else stops it (see `onscroll`). */
  function scroll(to: "up" | "down" | "top" | "bottom") {
    const el = logs.tab === "logs" ? listEl : configEl;
    if (!el) return;
    // A page, minus a little overlap to keep the reading place.
    const page = el.clientHeight * 0.9;
    if (to === "up") el.scrollBy({ top: -page });
    else if (to === "down") el.scrollBy({ top: page });
    else if (to === "top") el.scrollTop = 0;
    else {
      el.scrollTop = el.scrollHeight;
      if (logs.tab === "logs") logs.follow = true;
    }
  }

  // Same grammar as the TUI: `y` copies, Ctrl-R refreshes, Tab switches the
  // view, `v` steps the level; `c`, `r`, `1` / `2` stay as aliases.
  function onkey(key: string): boolean {
    switch (key) {
      case "j":
      case "ArrowDown":
        move(1);
        return true;
      case "k":
      case "ArrowUp":
        move(-1);
        return true;
      case "Tab":
        void logs.setTab(logs.tab === "logs" ? "config" : "logs");
        return true;
      case "1":
        void logs.setTab("logs");
        return true;
      case "2":
        void logs.setTab("config");
        return true;
      case "/":
        filterEl?.focus();
        return true;
      case "f":
        logs.follow = !logs.follow;
        return true;
      case "v":
        if (logs.tab === "logs") cycleLevel();
        return true;
      case "y":
      case "c":
        if (logs.tab === "logs" && logs.selected !== null) void copyAll();
        return true;
      case "r":
        refresh();
        return true;
      case "PageUp":
        scroll("up");
        return true;
      case "PageDown":
        scroll("down");
        return true;
      case "Home":
      case "g":
        scroll("top");
        return true;
      case "End":
      case "G":
        scroll("bottom");
        return true;
      default:
        return false;
    }
  }

  function onshortcut(e: KeyboardEvent): boolean {
    if (e.key.toLowerCase() !== "r" || e.shiftKey || e.altKey) return false;
    refresh();
    return true;
  }

  function onescape() {
    if (fallback !== null) fallback = null;
    else logs.close();
  }
</script>

<Modal bind:armed size="full" scroll={false} {onescape} {onkey} {onshortcut} labelledBy="logs-title">
  {#snippet title()}Session logs{#if info}<span class="title-host">· {info.alias}</span>{/if}{/snippet}

  <div class="panel">
    <aside class="sessions">
      <div class="column-title">Sessions <span class="count">{logs.sessions.length}</span></div>
      {#if logs.sessionsError}
        <p class="inline-error"><span>{sentence(logs.sessionsError.message)}</span>{#if logs.sessionsError.hint}<span class="hint">{logs.sessionsError.hint}</span>{/if}</p>
      {:else if logs.sessions.length === 0}
        <p class="note">No session yet. Connect to a host: its connection records show here, and stay after it ends.</p>
      {:else}
        <ul role="listbox" aria-label="Sessions" bind:this={sessionsEl}>
          {#each listed as s (s.id)}
            {@const st = statusOf(s)}
            <li
              role="option"
              aria-selected={s.id === logs.selected}
              class:selected={s.id === logs.selected}
              class:with-probe={s.probe}
              title={`Started ${fullDate(s.started)}${s.ended ? ` · ended ${fullDate(s.ended)}` : ""}`}
              onclick={() => void logs.select(s.id)}
              onkeydown={() => {}}
            >
              <span class="dot" data-status={st.status} class:hollow={s.ended !== null}></span>
              <span class="alias">{s.alias}</span>
              <span class="time mono">{clock(s.started)}</span>
              <span class="outcome" data-status={st.status} title={st.code}>{st.label}</span>
              {#if s.probe}<span class="probe" title="A copy-id verification, not a session you opened">probe</span>{/if}
            </li>
          {/each}
        </ul>
      {/if}
    </aside>

    <section class="main">
      <div class="tabs" role="tablist" aria-label="Debug views">
        <button role="tab" aria-selected={logs.tab === "logs"} class:active={logs.tab === "logs"} title="Tab switches the view (also 1)" onclick={() => void logs.setTab("logs")}>Logs</button>
        <button role="tab" aria-selected={logs.tab === "config"} class:active={logs.tab === "config"} title="Tab switches the view (also 2)" onclick={() => void logs.setTab("config")}>Effective config</button>
        <kbd class="tab-key">Tab</kbd>
        <span class="spacer"></span>
        <label class="filter">
          <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></svg>
          <input bind:this={filterEl} bind:value={logs.filter} placeholder={logs.tab === "logs" ? "Filter messages" : "Filter options"} spellcheck="false" autocomplete="off" aria-label="Filter"
            onkeydown={(e) => {
              if (e.key === "Escape" && logs.filter) {
                logs.filter = "";
                e.stopPropagation();
                e.preventDefault();
              }
            }} />
          <kbd>/</kbd>
        </label>
      </div>

      {#if logs.selected === null}
        <div class="state"><p>Select a session to read its records.</p></div>
      {:else if logs.tab === "logs"}
        <div class="toolbar">
          <div class="segmented" role="group" aria-label="Minimum level" title="Minimum level (v steps through)">
            {#each LEVELS as l (l.id)}
              <button aria-pressed={logs.minLevel === l.id} onclick={() => (logs.minLevel = l.id)}>{l.label}</button>
            {/each}
          </div>
          <kbd class="level-key">v</kbd>
          <button class="small secondary" aria-pressed={logs.follow} class:on={logs.follow} onclick={() => (logs.follow = !logs.follow)} title="Keep the newest record in view (f)">
            <span class="live-dot" class:on={logs.follow && info?.ended === null}></span>Follow <kbd>f</kbd>
          </button>
          <span class="spacer"></span>
          <span class="note count-note">{logs.visible.length === logs.lines.length ? `${logs.lines.length} records` : `${logs.visible.length} of ${logs.lines.length}`}</span>
          <button class="small secondary" onclick={() => void copyAll()} disabled={!logs.lines.length}>{copied ? "Copied" : "Copy all"} <kbd>y</kbd></button>
        </div>

        {#if fallback !== null}
          <div class="fallback">
            <p class="note">The clipboard is not available here: the text is selected, copy it with {isMac ? "⌘C" : "Ctrl-C"}. <kbd>Esc</kbd> hides it.</p>
            <textarea bind:this={fallbackEl} readonly value={fallback} aria-label="Session records as text"></textarea>
          </div>
        {:else if logs.loading}
          <div class="state"><span class="spinner" aria-hidden="true"></span><p>Loading records…</p></div>
        {:else if logs.error}
          <div class="state"><p class="inline-error"><span>{sentence(logs.error.message)}</span>{#if logs.error.hint}<span class="hint">{logs.error.hint}</span>{/if}</p></div>
        {:else if logs.lines.length === 0}
          <div class="state"><p>No record yet for this session.</p></div>
        {:else if logs.visible.length === 0}
          <div class="state"><p>No record matches.</p><p class="note">Clear the filter or lower the level.</p></div>
        {:else}
          <div class="records" role="log" aria-label="Log records" bind:this={listEl} {onscroll} tabindex="-1">
            {#each logs.visible as line, i (i)}
              <div class="record" data-level={line.level}>
                <span class="at mono" title={fullDate(line.at)}>{clockMs(line.at)}</span>
                <span class="level">{line.level.toUpperCase()}</span>
                <span class="target mono">{line.target}</span>
                <span class="message mono">{line.message}</span>
              </div>
            {/each}
          </div>
          <p class="cap note">The engine keeps the last 500 records of each session.</p>
        {/if}
      {:else}
        <div class="config" bind:this={configEl}>
          {#if logs.configLoading && !logs.config}
            <div class="state"><span class="spinner" aria-hidden="true"></span><p>Resolving {info?.alias}…</p></div>
          {:else if logs.configError}
            <div class="state"><p class="inline-error"><span>{sentence(logs.configError.message)}</span>{#if logs.configError.hint}<span class="hint">{logs.configError.hint}</span>{/if}</p></div>
          {:else if logs.config}
            {@const c = logs.config}
            <div class="route">
              <span class="label">Route</span>
              {#if c.routeError}
                <p class="inline-error"><span>{sentence(c.routeError.message)}</span>{#if c.routeError.hint}<span class="hint">{c.routeError.hint}</span>{/if}</p>
              {:else}
                <span class="hops mono">
                  <span class="muted">You</span>
                  {#each c.route as hop, i (i)}
                    <span class="arrow">→</span><span class:final={i === c.route.length - 1}>{hop.user}@{hop.hostname}{#if hop.port !== 22}<span class="muted">:{hop.port}</span>{/if}</span>
                  {/each}
                </span>
              {/if}
            </div>
            {#each c.warnings as w (w.origin + w.message)}
              <p class="soft-warning">{w.message} <span class="muted mono">{w.origin}</span></p>
            {/each}
            {#if configRows.length === 0}
              <div class="state"><p>No option matches.</p></div>
            {:else}
              <div class="options" role="table" aria-label="Effective options">
                {#each configRows as o, i (i)}
                  <div class="option" role="row">
                    <span class="key mono" role="cell">{o.key}</span>
                    <span class="values mono" role="cell">
                      {#if o.sensitive}
                        <span class="hidden" aria-hidden="true">•••</span> <span class="hidden-note">Hidden: may contain a secret · never copied</span>
                      {:else}
                        {o.values.join(" ")}
                      {/if}
                    </span>
                    <span class="origin mono" role="cell">{o.origin ?? (o.command ? "at connect" : "default")}</span>
                  </div>
                {/each}
              </div>
            {/if}
          {/if}
        </div>
      {/if}
    </section>
  </div>

  {#snippet footer()}
    <span class="footer-note"><kbd>j</kbd> <kbd>k</kbd> sessions · <kbd>PgUp</kbd> <kbd>PgDn</kbd> <kbd>g</kbd> <kbd>G</kbd> scroll · <kbd>Ctrl-R</kbd> refresh · texts are shown escaped, as copied</span>
    <button class="secondary" onclick={() => logs.close()}>Close <kbd>Esc</kbd></button>
  {/snippet}
</Modal>

<style>
  .panel {
    display: grid;
    grid-template-columns: minmax(170px, 240px) minmax(0, 1fr);
    gap: var(--space-4);
    flex: 1;
    min-height: 0;
  }
  .sessions {
    display: flex;
    flex-direction: column;
    gap: var(--space-2);
    min-height: 0;
    padding-right: var(--space-3);
    border-right: 1px solid var(--border-subtle);
  }
  .column-title {
    display: flex;
    justify-content: space-between;
    padding: 0 var(--space-2);
    color: var(--text-muted);
    font-size: var(--text-xs);
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }
  .count { font-variant-numeric: tabular-nums; letter-spacing: normal; }
  .sessions ul {
    display: flex;
    flex-direction: column;
    gap: var(--space-1);
    margin: 0;
    padding: 0;
    list-style: none;
    overflow-y: auto;
  }
  .sessions li {
    position: relative;
    display: grid;
    grid-template-columns: 8px minmax(0, 1fr) auto;
    grid-template-areas: "dot alias time" ". outcome probe";
    align-items: center;
    column-gap: var(--space-2);
    padding: var(--space-2) var(--space-3);
    border-radius: var(--radius-md);
    cursor: default;
    transition: background var(--motion-fast) var(--ease-standard);
  }
  .sessions li:hover { background: var(--bg-elevated); }
  .sessions li.selected { background: var(--bg-selected); }
  .sessions li.selected::before {
    content: "";
    position: absolute;
    left: 0;
    top: var(--space-2);
    bottom: var(--space-2);
    width: 2px;
    border-radius: 2px;
    background: var(--accent);
  }
  .dot {
    --c: var(--status-idle);
    grid-area: dot;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--c);
  }
  .dot.hollow { background: transparent; box-shadow: inset 0 0 0 1.5px var(--c); }
  .dot[data-status="connecting"] { --c: var(--status-connecting); }
  .dot[data-status="authenticating"] { --c: var(--status-authenticating); }
  .dot[data-status="connected"] { --c: var(--status-connected); }
  .dot[data-status="warning"] { --c: var(--status-warning); }
  .dot[data-status="error"] { --c: var(--status-error); }
  .alias { grid-area: alias; overflow: hidden; color: var(--text-primary); font-weight: 550; text-overflow: ellipsis; white-space: nowrap; }
  .time { grid-area: time; color: var(--text-muted); font-size: var(--text-xs); }
  .outcome { grid-area: outcome; overflow: hidden; color: var(--text-muted); font-size: var(--text-xs); text-overflow: ellipsis; white-space: nowrap; }
  /* Without a probe badge, the outcome uses the whole second line. */
  .sessions li:not(.with-probe) .outcome { grid-column: 2 / -1; }
  .outcome[data-status="error"] { color: var(--status-error); }
  /* A refused host key was the user's choice: neutral, not an error. */
  .outcome[data-status="idle"] { color: var(--status-idle); }
  .tab-key,
  .level-key { color: var(--text-muted); }
  .probe {
    grid-area: probe;
    padding: 0 var(--space-1);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    color: var(--text-muted);
    font-size: var(--text-xs);
  }

  .main { display: flex; flex-direction: column; gap: var(--space-3); min-width: 0; min-height: 0; }
  .tabs { display: flex; align-items: center; gap: var(--space-1); border-bottom: 1px solid var(--border-subtle); }
  .tabs button[role="tab"] {
    height: 34px;
    padding: 0 var(--space-3);
    border: 0;
    border-radius: 0;
    background: transparent;
    color: var(--text-muted);
    box-shadow: inset 0 -2px 0 transparent;
  }
  .tabs button[role="tab"]:hover { color: var(--text-secondary); }
  .tabs button.active { color: var(--text-primary); box-shadow: inset 0 -2px 0 var(--accent); }
  .spacer { flex: 1; }
  .filter {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 28px;
    margin-bottom: var(--space-1);
    padding: 0 var(--space-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-elevated);
  }
  .filter:focus-within { border-color: var(--accent); }
  .filter svg { width: 12px; height: 12px; fill: none; stroke: var(--text-muted); stroke-width: 1.5; }
  .filter input { width: 22ch; border: 0; outline: 0; background: transparent; color: var(--text-primary); font: inherit; font-size: var(--text-sm); }
  .filter input::placeholder { color: var(--text-muted); }

  .toolbar { display: flex; align-items: center; gap: var(--space-2); white-space: nowrap; }
  .toolbar .segmented { flex-wrap: nowrap; }
  @media (max-width: 1000px) {
    .count-note { display: none; }
    .filter input { width: 14ch; }
  }
  .live-dot { width: 6px; height: 6px; border-radius: 50%; background: var(--text-muted); }
  .live-dot.on { background: var(--status-connected); animation: pulse var(--motion-loop) var(--ease-standard) infinite; }
  @keyframes pulse {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--status-connected) 60%, transparent); }
    100% { box-shadow: 0 0 0 4px transparent; }
  }

  .records {
    flex: 1;
    min-height: 0;
    overflow: auto;
    padding: var(--space-2) 0;
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-base);
    outline: none;
    user-select: text;
  }
  .record {
    display: grid;
    grid-template-columns: max-content 5ch 11ch minmax(0, 1fr);
    gap: var(--space-3);
    /* No padding off the 4 px scale: the line height spaces the records. */
    padding: 0 var(--space-3);
    font-size: var(--text-sm);
    line-height: 1.75;
    /* Off-screen rows skip layout and paint (500 records at most). */
    content-visibility: auto;
    contain-intrinsic-size: auto 24px;
  }
  .record:hover { background: var(--bg-surface); }
  .at { color: var(--text-muted); }
  .level { font-size: var(--text-xs); font-weight: 600; letter-spacing: 0.04em; color: var(--text-muted); }
  .record[data-level="info"] .level { color: var(--text-secondary); }
  .record[data-level="warn"] .level { color: var(--status-warning); }
  .record[data-level="error"] .level { color: var(--status-error); }
  .record[data-level="error"] .message { color: var(--text-primary); }
  .target { overflow: hidden; color: var(--text-muted); text-overflow: ellipsis; white-space: nowrap; }
  .message { color: var(--text-secondary); overflow-wrap: anywhere; }
  .cap { margin: 0; }

  .state {
    display: grid;
    flex: 1;
    place-content: center;
    justify-items: center;
    gap: var(--space-2);
    color: var(--text-secondary);
    text-align: center;
  }
  .fallback { display: flex; flex: 1; flex-direction: column; gap: var(--space-2); min-height: 0; }
  .fallback textarea {
    flex: 1;
    padding: var(--space-2) var(--space-3);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-md);
    background: var(--bg-base);
    color: var(--text-primary);
    font-family: var(--font-mono);
    font-size: var(--text-sm);
    resize: none;
    user-select: text;
  }

  .config { display: flex; flex: 1; flex-direction: column; gap: var(--space-3); min-height: 0; overflow-y: auto; }
  .route { display: grid; grid-template-columns: max-content minmax(0, 1fr); align-items: baseline; gap: var(--space-4); padding: var(--space-3) var(--space-4); background: var(--bg-elevated); border-radius: var(--radius-md); }
  .route .label { color: var(--text-muted); font-size: var(--text-sm); }
  .hops { display: flex; flex-wrap: wrap; gap: var(--space-2); color: var(--text-secondary); font-size: var(--text-sm); }
  .arrow { color: var(--text-muted); }
  .final { color: var(--text-primary); }
  .muted { color: var(--text-muted); }
  .options { display: grid; grid-template-columns: max-content minmax(0, 1fr) max-content; }
  .option { display: contents; }
  .option > span { padding: var(--space-1) var(--space-3); border-bottom: 1px solid var(--border-subtle); font-size: var(--text-sm); }
  .key { color: var(--text-primary); }
  .values { color: var(--text-secondary); overflow-wrap: anywhere; user-select: text; }
  .hidden { letter-spacing: 0.2em; color: var(--text-muted); }
  .hidden-note { color: var(--text-muted); font-family: var(--font-ui); font-size: var(--text-xs); }
  .origin { color: var(--text-muted); text-align: right; white-space: nowrap; }
</style>
