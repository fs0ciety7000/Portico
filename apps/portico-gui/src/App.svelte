<script lang="ts">
  import { onMount, untrack } from "svelte";
  import { fly } from "svelte/transition";
  import { describe, formatRtt, isTauri, sentence, setHome, type Host } from "./lib/api";
  import { backend } from "./lib/backend";
  import { isMac } from "./lib/keys";
  import { dur } from "./lib/motion";
  import { buildRows, relocate, rowHost, type Row } from "./lib/hostlist";
  import { isLive, paneStatus, workspace, type Tab } from "./lib/state/workspace.svelte";
  import { layout, minPanePx, MAX_PANES, panesOf, type Direction, type Layout } from "./lib/panes";
  import PaneDivider from "./lib/components/PaneDivider.svelte";
  import { files } from "./lib/state/files.svelte";
  import { tunnels } from "./lib/state/tunnels.svelte";
  import { keys } from "./lib/state/keys.svelte";
  import { vault } from "./lib/state/vault.svelte";
  import { hosts } from "./lib/state/hosts.svelte";
  import { keymap } from "./lib/state/keymap.svelte";
  import { logs } from "./lib/state/logs.svelte";
  import { settings } from "./lib/state/settings.svelte";
  import { importer } from "./lib/state/importer.svelte";
  import { emptyDraft, NEEDS_PASTE_MODE, scopeLabel, snippets, type SnippetTarget } from "./lib/state/snippets.svelte";
  import type { Snippet } from "./lib/api";
  import KeysDialog from "./lib/components/KeysDialog.svelte";
  import KeyGenDialog from "./lib/components/KeyGenDialog.svelte";
  import AgentAddDialog from "./lib/components/AgentAddDialog.svelte";
  import CopyIdDialog from "./lib/components/CopyIdDialog.svelte";
  import VaultDialog from "./lib/components/VaultDialog.svelte";
  import SnippetsDialog from "./lib/components/SnippetsDialog.svelte";
  import FilesPanel from "./lib/components/FilesPanel.svelte";
  import TunnelsDialog from "./lib/components/TunnelsDialog.svelte";
  import EditHostDialog from "./lib/components/EditHostDialog.svelte";
  import LogsDialog from "./lib/components/LogsDialog.svelte";
  import ImportDialog from "./lib/components/ImportDialog.svelte";
  import RecordingsDialog from "./lib/components/RecordingsDialog.svelte";
  import RecordConfirm from "./lib/components/RecordConfirm.svelte";
  import RecBadge from "./lib/components/RecBadge.svelte";
  import { recordings } from "./lib/state/recordings.svelte";
  import Modal from "./lib/components/Modal.svelte";
  import Sidebar from "./lib/components/Sidebar.svelte";
  import HostDetails from "./lib/components/HostDetails.svelte";
  import TabBar from "./lib/components/TabBar.svelte";
  import Mark from "./lib/components/Mark.svelte";
  import TerminalView from "./lib/components/TerminalView.svelte";
  import PromptDialog from "./lib/components/PromptDialog.svelte";
  import HostKeyAlert from "./lib/components/HostKeyAlert.svelte";
  import CommandPalette, { type Command } from "./lib/components/CommandPalette.svelte";

  /** Search results (ranked by portico-core); the sidebar shows them while a query is typed. */
  let results: Host[] = $state([]);
  let query = $state("");
  let selected = $state(0);
  let loading = $state(true);
  let paletteOpen = $state(false);
  let searchEl: HTMLInputElement | undefined = $state();
  /** Files panel beside the active terminal. */
  let filesOpen = $state(false);
  let tunnelsOpen = $state(false);
  let filesPanel: FilesPanel | undefined = $state();
  /** Terminal components by pane key (copy, paste, focus). */
  const views: Record<number, TerminalView> = $state({});

  const searching = $derived(query.trim().length > 0);
  const rows: Row[] = $derived(buildRows(searching ? results : hosts.all, hosts.recent, hosts.collapsed, searching));
  const selectedRow = $derived(rows[selected]);
  const selectedHost = $derived(rowHost(selectedRow));

  const tab = $derived(workspace.activeTab);
  /** The focused pane of the active tab: files, snippets, logs, copy and paste act on it. */
  const pane = $derived(workspace.activePane);
  const tabSession = $derived(pane ? workspace.sessions[pane.session] : undefined);
  /** Same dot grammar as the pane header and the tab (`paneStatus`). */
  const tabStatus = $derived(paneStatus(pane, tabSession));
  const prompt = $derived(workspace.prompts[0]);
  const alert = $derived(workspace.alerts[0]);
  const conflict = $derived(files.conflicts[0]);
  const editing = $derived(hosts.editing ? hosts.byAlias[hosts.editing] : undefined);
  const modalOpen = $derived(
    !!prompt ||
      !!alert ||
      !!conflict ||
      tunnelsOpen ||
      keys.open ||
      snippets.open ||
      !!vault.dialog ||
      !!editing ||
      hosts.confirmClear ||
      logs.open ||
      importer.open ||
      !!recordings.confirm ||
      recordings.open,
  );
  /** Terminals being recorded (any tab): REC in the status bar. */
  const recCount = $derived(Object.keys(recordings.active).length);
  const recHere = $derived(recordings.isRecording(pane?.terminal ?? null));

  /** Favorite, edit: the active pane's host, else the selected one. */
  const targetAlias = $derived(pane ? pane.alias : selectedHost?.alias);
  const targetHost = $derived(targetAlias ? hosts.byAlias[targetAlias] : undefined);

  // The selection is an index: when the list changes under it (an edit
  // moves the host to a group, a connection adds "Recent"), it follows its
  // row instead of landing on whatever row took its place. A new search
  // forgets it (`anchor = undefined`): the best match gets the selection.
  let anchor: Row | undefined;
  $effect.pre(() => {
    const now = rows;
    untrack(() => {
      const at = relocate(now, anchor);
      if (at >= 0) selected = at;
    });
  });
  $effect(() => {
    anchor = rows[selected];
  });
  // Keep the selection on an existing row.
  $effect(() => {
    if (selected > rows.length - 1) selected = Math.max(0, rows.length - 1);
  });
  /** First host row (the list starts with a section header when grouped). */
  const firstHost = () => Math.max(0, rows.findIndex((r) => r.kind === "host"));

  /** Where snippets run: the active pane's shell. */
  const snippetTarget: SnippetTarget | null = $derived(
    pane
      ? {
          alias: pane.alias,
          terminal: pane.terminal !== null && !pane.end ? pane.terminal : null,
          bracketed: () => views[pane.key]?.bracketedPaste() ?? false,
        }
      : null,
  );

  function openSnippets() {
    snippets.show(pane ? pane.alias : (selectedHost?.alias ?? null));
  }

  /** "Run: …" from the palette: straight to the shell unless it needs values. */
  async function runFromPalette(s: Snippet) {
    if (!pane) return;
    if (s.placeholders.length) {
      snippets.show(pane.alias, { kind: "run", snippet: s, error: null });
      return;
    }
    const error = await snippets.run(snippetTarget, s, {});
    if (!error) return;
    if (error.code === NEEDS_PASTE_MODE) snippets.show(pane.alias, { kind: "run", snippet: s, error });
    else workspace.notify(error);
  }

  // The palette lists the snippets of the active pane's host.
  $effect(() => {
    void paletteOpen;
    void snippets.loadPalette(pane?.alias ?? null);
  });
  const paletteSnippets = $derived(
    pane && snippetTarget?.terminal != null && snippets.palette?.alias === pane.alias ? snippets.palette.list : [],
  );

  /** Effective label of an action (undefined when unbound). */
  const k = (id: string) => keymap.label(id);
  /** "Open Keys, then …": the key that does it from the keys view. */
  const inKeys = (key: string) => (k("app.keys") ? `${k("app.keys")}, ${key}` : undefined);
  const vaultCommands: Command[] = $derived(
    vault.status === "locked"
      ? [{ id: "vault.unlock", title: "Unlock the vault", shortcut: inKeys("v"), run: () => vault.open("unlock", "keys") }]
      : vault.status === "unlocked"
        ? [
            { id: "vault.lock", title: "Lock the vault", shortcut: inKeys("v"), run: () => void vault.lock() },
            { id: "vault.change", title: "Change the vault passphrase", shortcut: inKeys("V"), run: () => vault.open("change", "keys") },
          ]
        : vault.status === "absent"
          ? [{ id: "vault.create", title: "Create a vault for remembered secrets", shortcut: inKeys("V"), run: () => vault.open("create", "keys") }]
          : [],
  );
  const showFiles = $derived(filesOpen && !!pane);

  /** Session the tunnels panel adds to: the active pane's, else the selected host's live one. */
  const tunnelSession = $derived.by(() => {
    if (pane) return isLive(workspace.sessions[pane.session]) ? { id: pane.session, alias: pane.alias } : null;
    const alias = selectedHost?.alias;
    const live = alias ? workspace.byAlias[alias] : undefined;
    return live && alias && isLive(live) ? { id: live.id, alias } : null;
  });

  function toggleFiles() {
    if (!pane) {
      // From the host list: open (or reuse) the host's tab, files beside it.
      const host = selectedHost;
      if (!host) return;
      filesOpen = true;
      void workspace.open(host.alias);
      return;
    }
    filesOpen = !filesOpen;
  }

  // Ranking lives in portico-core (same results as the TUI). Metadata
  // edits change what `*`, `#tag` and `@group` match: search again.
  let lastQuery = "";
  $effect(() => {
    const q = query.trim();
    void hosts.revision;
    const fresh = q !== lastQuery;
    lastQuery = q;
    if (!q) {
      results = [];
      if (fresh) {
        anchor = undefined;
        selected = firstHost();
      }
      return;
    }
    backend.searchHosts(q).then(
      (r) => {
        if (q !== query.trim()) return;
        if (fresh) anchor = undefined;
        results = r.map((h) => hosts.byAlias[h.alias] ?? h);
        if (fresh) selected = 0;
      },
      (e) => workspace.notify(e),
    );
  });

  let initialized = false;
  async function load() {
    loading = true;
    try {
      const [h, t, home] = await Promise.all([backend.listHosts(), backend.listThemes(), backend.homeDir()]);
      setHome(home);
      hosts.set(h);
      if (!initialized) {
        initialized = true;
        await Promise.all([settings.init(t), hosts.loadRecent()]);
        selected = firstHost();
      } else void hosts.loadRecent();
    } catch (e) {
      workspace.notify(e);
    } finally {
      loading = false;
    }
  }

  /** ↵ or double-click on a row: connect a host, fold or unfold a section. */
  function activate(row: Row | undefined) {
    if (!row) return;
    if (row.kind === "header") hosts.toggleSection(row.section.id);
    else void workspace.open(row.host.alias);
  }

  const connectSelected = () => activate(selectedRow);

  /** ← / →: fold or unfold the selected row's section (its header gets the selection). */
  function fold(collapse: boolean) {
    const section = selectedRow?.section;
    if (!section || section.kind === "flat") return;
    hosts.toggleSection(section.id, collapse);
    const at = rows.findIndex((r) => r.key === `header:${section.id}`);
    if (at >= 0 && collapse) selected = at;
  }

  /** Debug panel on the active pane's session, else the selected host's latest. */
  function showLogs() {
    const session = pane ? pane.session : selectedHost ? (workspace.byAlias[selectedHost.alias]?.id ?? null) : null;
    void logs.show(session);
  }

  async function paste() {
    const view = pane ? views[pane.key] : undefined;
    if (!view) return;
    try {
      view.pasteText(await navigator.clipboard.readText());
    } catch {
      workspace.notify({
        code: "clipboard",
        message: "The clipboard is not available here",
        hint: "Paste with Shift-Insert or the middle mouse button.",
      });
    }
  }

  function copy() {
    const text = pane ? views[pane.key]?.copySelection() : "";
    if (text) navigator.clipboard.writeText(text).catch((e) => workspace.notify(e));
  }

  /** The selected row when it is in "Recent" (x removes it). */
  const recentRow = $derived(!pane && selectedRow?.kind === "host" && selectedRow.section.kind === "recent" ? selectedRow.host : undefined);

  // ── Split panes: geometry of the active tab ──
  let stageW = $state(0);
  let stageH = $state(0);
  /** Room between panes and around them (tokens, read once the styles are in). */
  let paneGap = $state(8);
  let panePad = $state(4);
  /** Smallest pane in px: 20 × 5 cells plus the pane's chrome (TerminalView). */
  let paneMin = $state({ w: 0, h: 0 });
  /** Where split panes go: the stage, inset so frames do not touch the window. */
  const paneRect = $derived({ x: panePad, y: panePad, w: Math.max(0, stageW - 2 * panePad), h: Math.max(0, stageH - 2 * panePad) });
  const paneLayout: Layout = $derived.by(() => {
    if (!tab) return { panes: new Map(), dividers: [] };
    // A lone pane keeps the whole stage, frameless (as before panes existed).
    if (panesOf(tab.root).length < 2) return layout(tab.root, { x: 0, y: 0, w: stageW, h: stageH }, 0);
    return layout(tab.root, paneRect, paneGap, tab.zoom);
  });
  /** Every pane of every tab, with its tab. */
  const placed = $derived(workspace.tabs.flatMap((t) => workspace.panesIn(t).map((p) => ({ pane: p, tab: t }))));
  $effect(() => {
    workspace.geometry = { rect: paneRect, limits: { maxPanes: MAX_PANES, min: paneMin, gap: paneGap } };
  });

  /** Reads the tokens and the terminal cell size the pane geometry depends on. */
  function measurePanes() {
    const css = getComputedStyle(document.documentElement);
    const px = (name: string) => Number.parseFloat(css.getPropertyValue(name)) || 0;
    paneGap = px("--space-2");
    panePad = px("--space-1");
    const fontSize = px("--text-base") || 13;
    const ctx = document.createElement("canvas").getContext("2d");
    if (ctx) ctx.font = `${fontSize}px ${css.getPropertyValue("--font-mono").trim()}`;
    const cell = { w: ctx?.measureText("W").width || fontSize * 0.6, h: Math.ceil(fontSize * 1.2) };
    // TerminalView's split chrome: 1 px frame, header, insets of the xterm
    // host, and room for the scrollbar.
    const chrome = {
      w: 2 + px("--space-3") + px("--space-2") + px("--space-3"),
      h: 2 + px("--space-5") + px("--space-1") + px("--space-1") + px("--space-2"),
    };
    paneMin = minPanePx(cell, chrome);
  }

  /** Palette hint of a tab: its pane count, else the remote title. */
  function tabHint(t: Tab): string | undefined {
    const n = panesOf(t.root).length;
    return n > 1 ? `${n} panes` : workspace.focusOf(t)?.title || undefined;
  }

  const paneCount = $derived(tab ? panesOf(tab.root).length : 0);
  /** In a split tab `tab.close` closes the focused pane: its key, unless `pane.close` has one. */
  const closePaneKey = $derived(k("pane.close") ?? k("tab.close"));
  /** Split panes (only with a terminal tab). Titles always say "pane" (as in the TUI). */
  const paneCommands: Command[] = $derived([
    { id: "pane.split-right", title: "Split pane right", hint: pane ? `new pane on ${pane.alias}` : undefined, shortcut: k("pane.split-right"), run: () => void workspace.split("row") },
    { id: "pane.split-down", title: "Split pane down", hint: pane ? `new pane on ${pane.alias}` : undefined, shortcut: k("pane.split-down"), run: () => void workspace.split("column") },
    { id: "pane.split-host", title: "Split pane with host…", hint: "host picker", shortcut: k("pane.split-host"), run: openHostPicker },
    ...(paneCount > 1
      ? [
          { id: "pane.next", title: "Next pane", shortcut: k("pane.next"), run: () => workspace.focusNext() },
          ...(["left", "right", "up", "down"] as const).map((d) => ({
            id: `pane.focus-${d}`,
            title: `Focus pane ${d}`,
            shortcut: k(`pane.focus-${d}`),
            run: () => workspace.focusDirection(d),
          })),
          { id: "pane.divider", title: "Move pane divider…", hint: "arrows, Home / End, ↵ when done", shortcut: k("pane.divider"), run: focusDivider },
          { id: "pane.zoom", title: tab?.zoom != null ? "Unzoom pane" : "Zoom pane", shortcut: k("pane.zoom"), run: () => workspace.toggleZoom() },
          { id: "pane.close", title: "Close pane", hint: pane?.alias, shortcut: closePaneKey, run: () => void workspace.closePane() },
          { id: "tab.close-all", title: "Close tab and its panes", hint: `${paneCount} panes`, shortcut: k("tab.close-all"), run: () => void workspace.close() },
        ]
      : []),
  ]);

  /** A divider has the keyboard: it holds the accent, the focused pane a neutral ring. */
  let dividerFocused = $state(false);

  /** "Move pane divider…": the focused pane's nearest divider takes the keyboard. */
  function focusDivider() {
    const id = workspace.dividerOfFocus();
    if (id === null) return;
    // After the unzoom (if any) has laid the dividers out.
    requestAnimationFrame(() => document.querySelector<HTMLElement>(`.stage [data-divider="${id}"]`)?.focus());
  }

  /** "Split with another host…": the palette, listing hosts. */
  let pickerOpen = $state(false);
  function openHostPicker() {
    if (pane) pickerOpen = true;
  }
  /** Recent hosts first, then favourites, then the rest (configuration order). */
  const hostChoices: Command[] = $derived.by(() => {
    const recent = hosts.recent.flatMap((r) => {
      const h = hosts.byAlias[r.alias];
      return h ? [h] : [];
    });
    const seen = new Set(recent.map((h) => h.alias));
    const favorites = hosts.all.filter((h) => h.favorite && !seen.has(h.alias));
    for (const h of favorites) seen.add(h.alias);
    const ordered = [...recent, ...favorites, ...hosts.all.filter((h) => !seen.has(h.alias))];
    return ordered.map((h) => {
      const session = workspace.byAlias[h.alias];
      const d = describe(session);
      return {
        id: `split-host-${h.alias}`,
        title: h.alias,
        status: { status: d.status, busy: d.busy },
        // The split reuses the host's live session: no new connection, no prompt.
        note: isLive(session) ? "reuses connection" : undefined,
        hint: [h.user && `${h.user}@`, h.hostname].filter(Boolean).join("") || undefined,
        mono: true,
        run: () => void workspace.split(null, h.alias),
      };
    });
  });

  // Ids are the stable action ids of `shortcuts.ts` (bindable in settings.toml).
  const commands: Command[] = $derived([
    {
      id: "hosts.search",
      title: "Search hosts",
      hint: "#tag, @group, * favorites",
      shortcut: k("hosts.search"),
      run: () => ((workspace.active = null), queueMicrotask(() => searchEl?.focus())),
    },
    ...(pane
      ? [
          { id: "tab.new", title: `New tab on ${pane.alias}`, shortcut: k("tab.new"), run: () => void workspace.duplicate() },
          ...(paneCount > 1 ? [] : [{ id: "tab.close", title: "Close tab", shortcut: k("tab.close"), run: () => void workspace.close() }]),
          { id: "tab.copy", title: "Copy selection", shortcut: isMac ? "⌘C" : k("tab.copy"), run: copy },
          { id: "tab.paste", title: "Paste", shortcut: isMac ? "⌘V" : k("tab.paste"), run: () => void paste() },
          ...(recordings.recordable(pane)
            ? [
                recordings.isRecording(pane.terminal)
                  ? { id: "session.record", title: "Stop recording", hint: pane.alias, shortcut: k("session.record"), run: () => void recordings.toggle(pane) }
                  : { id: "session.record", title: "Record session", hint: `${pane.alias} · output only, asks first`, shortcut: k("session.record"), run: () => void recordings.toggle(pane) },
              ]
            : []),
          { id: "tab.reconnect", title: `Reconnect ${pane.alias}`, shortcut: k("tab.reconnect"), run: () => void workspace.reconnect(pane.key) },
          { id: "tab.disconnect", title: `Disconnect ${pane.alias}`, shortcut: k("tab.disconnect"), run: () => void workspace.disconnect(pane.alias) },
          ...paneCommands,
        ]
      : [
          {
            id: "hosts.connect",
            title: selectedHost ? `Connect to ${selectedHost.alias}` : "Connect to selected host",
            shortcut: k("hosts.connect"),
            run: connectSelected,
          },
        ]),
    ...(targetHost
      ? [
          {
            id: "app.favorite",
            title: targetHost.favorite ? `Remove ${targetHost.alias} from favorites` : `Add ${targetHost.alias} to favorites`,
            hint: "Toggle favorite",
            // In a terminal, Mod+D splits the pane.
            shortcut: pane ? undefined : k("app.favorite"),
            run: () => void hosts.toggleFavorite(targetHost.alias),
          },
          {
            id: "hosts.edit",
            title: `Edit ${targetHost.alias}`,
            hint: "tags, group, notes",
            shortcut: pane ? undefined : k("hosts.edit"),
            run: () => (hosts.editing = targetHost.alias),
          },
        ]
      : []),
    { id: "app.logs", title: "Session logs", hint: "debug panel, effective config", shortcut: k("app.logs"), run: showLogs },
    ...(recentRow
      ? [
          {
            id: "hosts.forget-recent",
            title: `Remove ${recentRow.alias} from recent`,
            shortcut: k("hosts.forget-recent"),
            run: () => void hosts.forgetRecent(recentRow.alias),
          },
        ]
      : []),
    ...(hosts.recent.length
      ? [{ id: "hosts.clear-recent", title: "Clear recent", shortcut: pane ? undefined : k("hosts.clear-recent"), run: () => (hosts.confirmClear = true) }]
      : []),
    {
      id: "app.files",
      title: pane ? (showFiles ? "Hide files" : `Files on ${pane.alias}`) : `Files on ${selectedHost?.alias ?? "host"}`,
      shortcut: k("app.files"),
      run: toggleFiles,
    },
    ...(showFiles && pane
      ? [
          { id: "files.upload", title: `Upload files to ${pane.alias}`, shortcut: "u", run: () => filesPanel?.run("upload") },
          { id: "files.mkdir", title: "New folder", shortcut: "n", run: () => filesPanel?.run("mkdir") },
          { id: "files.rename", title: "Rename selected file", shortcut: "r", run: () => filesPanel?.run("rename") },
          { id: "files.delete", title: "Delete selected file", shortcut: "d", run: () => filesPanel?.run("delete") },
          { id: "files.refresh", title: "Refresh files (in the files panel)", shortcut: "Ctrl-R", run: () => filesPanel?.run("refresh") },
          { id: "files.hidden", title: files.showHidden ? "Hide hidden files" : "Show hidden files", shortcut: ".", run: () => filesPanel?.run("hidden") },
          { id: "files.cancel", title: "Cancel the latest transfer", shortcut: "x", run: () => filesPanel?.run("cancel") },
        ]
      : []),
    { id: "app.keys", title: "Keys", hint: "~/.ssh and ssh-agent", shortcut: k("app.keys"), run: () => keys.show() },
    { id: "keys.new", title: "New key", shortcut: k("keys.new") ?? inKeys("n"), run: () => keys.show({ kind: "new" }) },
    ...vaultCommands,
    { id: "app.snippets", title: "Snippets", shortcut: k("app.snippets"), run: openSnippets },
    {
      id: "snippets.new",
      title: "New snippet",
      shortcut: k("snippets.new") ?? (k("app.snippets") ? `${k("app.snippets")}, n` : undefined),
      run: () => snippets.show(pane?.alias ?? null, { kind: "edit", draft: emptyDraft(pane?.alias ?? null) }),
    },
    ...paletteSnippets.map((s) => ({ id: `snippet-${s.id}`, title: `Run: ${s.name}`, hint: scopeLabel(s), run: () => void runFromPalette(s) })),
    {
      id: "app.tunnels",
      title: "Tunnels",
      hint: tunnels.list.length ? `${tunnels.list.length} running` : undefined,
      shortcut: k("app.tunnels"),
      run: () => (tunnelsOpen = true),
    },
    {
      id: "recordings.open",
      title: "Recordings…",
      hint: recCount ? `${recCount} recording now` : "play or delete recorded sessions",
      shortcut: k("recordings.open"),
      run: () => recordings.show(),
    },
    {
      id: "files.download-dir",
      title: "Change download folder",
      hint: files.downloadDir || undefined,
      shortcut: k("files.download-dir"),
      run: () => void files.chooseDownloadDir(),
    },
    {
      id: "app.import-termius",
      title: "Import from Termius…",
      hint: "Termius CSV, JSON or ssh-config export · previewed before writing",
      shortcut: k("app.import-termius"),
      run: () => void importer.start(),
    },
    { id: "tab.hosts", title: "Go to hosts", shortcut: k("tab.hosts"), run: () => (workspace.active = null) },
    ...workspace.tabs.map((t, i) => ({
      id: `tab-${t.key}`,
      title: `Go to ${workspace.focusOf(t)?.alias ?? "tab"}`,
      hint: tabHint(t),
      shortcut: i < 9 ? k(`tab.goto-${i + 1}`) : undefined,
      run: () => (workspace.active = t.key),
    })),
    ...(workspace.tabs.length
      ? [
          { id: "tab.next", title: "Next tab", shortcut: k("tab.next"), run: () => workspace.cycle(1) },
          { id: "tab.prev", title: "Previous tab", shortcut: k("tab.prev"), run: () => workspace.cycle(-1) },
        ]
      : []),
    { id: "app.reload", title: "Reload ~/.ssh/config", shortcut: k("app.reload"), run: load },
    { id: "app.theme", title: "Next theme", shortcut: k("app.theme"), hint: settings.theme?.name, run: () => settings.cycle() },
    ...settings.themes.map((t) => ({ id: `theme-${t.id}`, title: `Theme: ${t.name}`, hint: t.description, run: () => settings.select(t.id) })),
  ]);

  /** Runs a keymap action (ids of `shortcuts.ts`). */
  function runAction(id: string) {
    const goto = /^tab\.goto-(\d)$/.exec(id);
    if (goto?.[1]) return workspace.go(Number(goto[1]) - 1);
    const arrow = /^pane\.focus-(left|right|up|down)$/.exec(id);
    if (arrow?.[1]) return workspace.focusDirection(arrow[1] as Direction);
    switch (id) {
      case "app.palette":
        paletteOpen = !paletteOpen;
        pickerOpen = false;
        return;
      case "tab.new":
        return pane ? void workspace.duplicate() : connectSelected();
      case "tab.close":
        // In a split tab: the focused pane (the last one closes the tab).
        return void (pane ? workspace.closePane() : workspace.close());
      case "tab.close-all":
        return void workspace.close();
      case "tab.hosts":
        workspace.active = null;
        return;
      case "tab.next":
        return workspace.cycle(1);
      case "tab.prev":
        return workspace.cycle(-1);
      case "tab.copy":
        return copy();
      case "tab.paste":
        return void paste();
      case "app.files":
      case "hosts.files":
        return toggleFiles();
      case "app.tunnels":
      case "hosts.tunnels":
        tunnelsOpen = true;
        return;
      case "app.keys":
      case "hosts.keys":
        return keys.show();
      case "app.snippets":
      case "hosts.snippets":
        return openSnippets();
      case "app.logs":
        return showLogs();
      case "app.reload":
        return void load();
      case "app.theme":
        return settings.cycle();
      case "app.favorite":
      case "hosts.favorite":
        if (targetAlias) void hosts.toggleFavorite(targetAlias);
        return;
      case "hosts.edit":
        if (targetAlias) hosts.editing = targetAlias;
        return;
      case "hosts.search":
        return searchEl?.focus();
      case "hosts.down":
        selected = Math.min(selected + 1, rows.length - 1);
        return;
      case "hosts.up":
        selected = Math.max(selected - 1, 0);
        return;
      case "hosts.connect":
        return connectSelected();
      case "hosts.disconnect":
        if (selectedHost) void workspace.disconnect(selectedHost.alias);
        return;
      case "hosts.forget-recent":
        if (recentRow) void hosts.forgetRecent(recentRow.alias);
        return;
      case "hosts.collapse":
        return fold(true);
      case "hosts.expand":
        return fold(false);
      case "pane.split-right":
        return void workspace.split("row");
      case "pane.split-down":
        return void workspace.split("column");
      case "pane.split-host":
        return openHostPicker();
      case "pane.next":
        return workspace.focusNext();
      case "pane.zoom":
        return workspace.toggleZoom();
      case "pane.close":
        return void workspace.closePane();
      case "pane.divider":
        return focusDivider();
      case "session.record":
        return void recordings.toggle(pane);
      case "recordings.open":
        return recordings.show();
      default:
        // Palette-only actions bound in settings.toml run their command.
        commands.find((c) => c.id === id)?.run();
    }
  }

  const isField = (t: EventTarget | null) =>
    t instanceof HTMLInputElement ||
    t instanceof HTMLTextAreaElement ||
    t instanceof HTMLSelectElement ||
    (t instanceof HTMLElement && t.isContentEditable);

  function onkeydown(e: KeyboardEvent) {
    if (modalOpen) return;
    // In a field (search, xterm's textarea) or the palette, plain keys type.
    const id = keymap.match(e, pane ? "terminal" : "hosts", paletteOpen || pickerOpen || isField(e.target));
    if (!id) return;
    e.preventDefault();
    if ((paletteOpen || pickerOpen) && id !== "app.palette") return;
    runAction(id);
  }

  // Leaving a terminal: take the focus out of its (now hidden) textarea.
  $effect(() => {
    if (pane) return;
    const el = document.activeElement;
    if (el instanceof HTMLElement && el.closest(".terminal")) el.blur();
  });

  // Give the keyboard back when an overlay closes: to the files panel when
  // it is open (and just opened), else to the terminal.
  $effect(() => {
    if (!paletteOpen && !pickerOpen && !modalOpen && pane) {
      const view = views[pane.key];
      const panel = showFiles ? filesPanel : undefined;
      queueMicrotask(() => (panel ? panel.focus() : view?.focus()));
    }
  });

  // A notice never covers a dialog: the one showing when a dialog opens is
  // dismissed; one raised while a dialog is open shows above it, and the
  // dialog moves down by the notice's height (`--toast-room`, Modal.svelte).
  let hadModal = false;
  $effect(() => {
    const open = modalOpen;
    if (open && !hadModal) untrack(() => workspace.dismissToast());
    hadModal = open;
  });
  let toastHeight = $state(0);
  /** A terminal is on screen (no dialog): notices go to the stage's top-right corner. */
  const toastOnStage = $derived(!!pane && !modalOpen);
  $effect(() => {
    const room = modalOpen && workspace.toast && toastHeight ? `calc(${toastHeight}px + var(--space-4))` : "0px";
    document.documentElement.style.setProperty("--toast-room", room);
  });

  function closeFiles() {
    filesOpen = false;
  }

  onMount(() => {
    // New aliases (and their metadata) show up once the Include line is in place.
    importer.onimported = () => void load();
    measurePanes();
    // The mono font may arrive after the first measure.
    void document.fonts.ready.then(measurePanes);
    load();
    void files.init();
    void vault.init();
    const unlisten = [
      backend.onEngineEvent((ev) => {
        workspace.onEvent(ev);
        recordings.onEvent(ev);
        hosts.onEvent(ev);
        logs.onEvent(ev);
        if (ev.type === "sftp") files.onEvent(ev.session, ev.event);
        else if (ev.type === "tunnel") tunnels.onEvent(ev.id, ev.session, ev.event);
        else if (ev.type === "session" && ev.event.kind === "phase" && ["closed", "failed"].includes(ev.event.phase)) {
          files.forget(ev.id);
        }
      }),
      // Events were lost: REC is resynchronised from the engine.
      backend.onEngineLagged(() => void recordings.resync()),
      backend.onDragging((d) => (files.dragging = d)),
      backend.onDropped((list) => files.dropped(list, showFiles && pane ? pane.session : null)),
    ];
    return () => {
      for (const u of unlisten) void u.then((f) => f());
    };
  });

  /** "⌥⌘←→" / "Ctrl-Shift-←→": the focus keys of the panes, in one hint. */
  function arrowsLabel(): string | undefined {
    const left = k("pane.focus-left");
    const right = k("pane.focus-right");
    if (left?.endsWith("←") && right === `${left.slice(0, -1)}→`) return `${left}→`;
    return left ?? k("pane.next");
  }

  // Status-bar hints, most useful first: when the bar runs out of room
  // they give way from the end of each list (the palette hint stays last).
  // A split tab speaks of its panes first.
  const tabHints: [string | undefined, string][] = $derived(
    paneCount > 1
      ? [
          [arrowsLabel(), "panes"],
          [closePaneKey, "close pane"],
          [k("pane.split-right"), "split"],
          [k("tab.hosts"), "hosts"],
          [k("app.files"), "files"],
          [k("app.logs"), "logs"],
        ]
      : [
          [k("tab.hosts"), "hosts"],
          [k("tab.new"), "new tab"],
          [k("pane.split-right"), "split"],
          [k("tab.close"), "close"],
          [k("app.files"), "files"],
          [k("app.logs"), "logs"],
          [k("app.snippets"), "snippets"],
        ],
  );
  const hostHints = [
    ["hosts.connect", "connect"],
    ["hosts.search", "search"],
    ["hosts.edit", "edit"],
    ["app.favorite", "favorite"],
    ["hosts.files", "files"],
    ["app.logs", "logs"],
    ["hosts.snippets", "snippets"],
    ["app.theme", "theme"],
  ] as const;
</script>

<svelte:window {onkeydown} />

<div class="shell">
  <header class="titlebar" class:mac={isMac} data-tauri-drag-region>
    <div class="brand" data-tauri-drag-region>
      <Mark size={16} />
      <span class="wordmark">PORTICO</span>
    </div>
    <TabBar hostCount={hosts.all.length} />
    <button class="palette-trigger" onclick={() => (paletteOpen = true)} aria-label="Command palette" title={keymap.tip("Command palette", "app.palette")}>
      <svg viewBox="0 0 16 16" aria-hidden="true"><circle cx="7" cy="7" r="4.5" /><path d="M10.5 10.5 14 14" /></svg>
      {#if k("app.palette")}<kbd>{k("app.palette")}</kbd>{/if}
    </button>
  </header>

  <main class:with-files={showFiles}>
    <div class="hosts" class:hidden={!!pane}>
      <Sidebar
        {rows}
        total={hosts.all.length}
        sessions={workspace.byAlias}
        {loading}
        bind:query
        bind:selected
        bind:searchEl
        onactivate={activate}
        onforget={(alias) => void hosts.forgetRecent(alias)}
        ontoggle={(id) => hosts.toggleSection(id)}
      />
      <HostDetails
        host={selectedHost}
        session={workspace.byAlias[selectedHost?.alias ?? ""]}
        section={selectedRow?.kind === "header" ? selectedRow.section : null}
        onconnect={(h) => workspace.open(h.alias)}
        ondisconnect={(h) => workspace.disconnect(h.alias)}
        onfavorite={(h) => void hosts.toggleFavorite(h.alias)}
        onedit={(h) => (hosts.editing = h.alias)}
        onlogs={showLogs}
      />
    </div>
    <div class="stage" bind:clientWidth={stageW} bind:clientHeight={stageH}>
      <!-- Flat and keyed by pane: a split or a close never remounts a terminal. -->
      {#each placed as { pane: p, tab: t } (p.key)}
        {@const rect = t.key === workspace.active ? paneLayout.panes.get(p.key) : undefined}
        <div
          class="slot"
          class:hidden={!rect}
          style:left={rect ? `${rect.x}px` : null}
          style:top={rect ? `${rect.y}px` : null}
          style:width={rect ? `${rect.w}px` : null}
          style:height={rect ? `${rect.h}px` : null}
        >
          <TerminalView
            bind:this={views[p.key]}
            pane={p}
            visible={!!rect}
            focused={t.focus === p.key}
            split={panesOf(t.root).length > 1}
            zoomed={t.zoom === p.key}
            demoted={dividerFocused && t.key === workspace.active}
            themeId={settings.themeId}
            autofocus={!showFiles}
            onfocusin={() => workspace.focusPane(p.key)}
          />
        </div>
      {/each}
      {#each paneLayout.dividers as d (d.id)}
        <PaneDivider divider={d} onrelease={() => pane && views[pane.key]?.focus()} onfocuschange={(f) => (dividerFocused = f)} />
      {/each}
      {#if workspace.toast && toastOnStage}
        {@render toastView(workspace.toast)}
      {/if}
    </div>
    {#if showFiles && pane}
      {#key pane.session}
        <FilesPanel bind:this={filesPanel} session={pane.session} alias={pane.alias} onclose={closeFiles} onreconnect={() => workspace.reconnect(pane.key)} />
      {/key}
    {/if}
  </main>

  <footer class="statusbar">
    {#if pane}
      <!-- The status (dot, alias, phase, latency) never shrinks; only the
           terminal title does, then the hints give way (see .hints). -->
      <span class="session" data-status={tabStatus.status}>
        <span class="dot" class:busy={tabStatus.busy} class:hollow={tabStatus.hollow}></span>
        <span class="alias" title={pane.alias}>{pane.alias}</span>
        {#if tab && paneCount > 1}
          <span class="pane-pos">pane <span class="mono">{panesOf(tab.root).indexOf(pane.key) + 1}/{paneCount}</span>{tab.zoom !== null ? ", zoomed" : ""}</span>
        {/if}
        {#if pane.end}<span class="ended">{pane.end.kind === "closed" ? "Shell closed" : "Shell exited"}</span>{/if}
        <span class="phase">{pane.end ? `session ${tabStatus.label.toLowerCase()}` : tabStatus.label}</span>
        {#if tabSession?.latencyMs != null && tabSession.phase.phase === "connected" && !tabStatus.failed}
          <span class="rtt mono" class:slow={tabStatus.status === "warning"}>{formatRtt(tabSession.latencyMs)}</span>
        {/if}
        <!-- Split: each pane header shows its own title already. -->
        {#if pane.title && paneCount < 2}<span class="title" title={pane.title}>{pane.title}</span>{/if}
      </span>
      {#if recCount}
        <RecBadge title={recHere ? "This pane's output is being recorded" : `${recCount} ${recCount === 1 ? "terminal is" : "terminals are"} being recorded`} />
      {/if}
      <span class="hints">
        {#if k("app.palette")}<span><kbd>{k("app.palette")}</kbd> commands</span>{/if}
        {#each tabHints as [key, label] (label)}
          {#if key}<span><kbd>{key}</kbd> {label}</span>{/if}
        {/each}
      </span>
    {:else}
      <span class="counts">{hosts.all.length} hosts · {workspace.tabs.length} open</span>
      {#if recCount}
        <RecBadge title={`${recCount} ${recCount === 1 ? "terminal is" : "terminals are"} being recorded`} />
      {/if}
      <span class="hints">
        {#if k("app.palette")}<span><kbd>{k("app.palette")}</kbd> commands</span>{/if}
        {#each hostHints as [id, label] (id)}
          {#if k(id)}<span><kbd>{k(id)}</kbd> {label}</span>{/if}
        {/each}
      </span>
    {/if}
    <span class="theme-name">
      {#if !isTauri()}<span class="demo" title="Browser preview with a simulated engine">Demo</span>{/if}
      {settings.theme?.name ?? ""}
    </span>
  </footer>
</div>

<CommandPalette bind:open={paletteOpen} {commands} />
<CommandPalette
  bind:open={pickerOpen}
  commands={hostChoices}
  label="Split pane with host"
  placeholder={pane ? `Split ${pane.alias}'s pane with…` : "Split pane with…"}
  empty="No host found"
/>

{#if alert}
  {#key alert}
    <HostKeyAlert {alert} onclose={() => workspace.dismissAlert()} />
  {/key}
{:else if prompt}
  {#key prompt.id}
    <PromptDialog
      {prompt}
      alias={workspace.aliasOf(prompt.session)}
      context={keys.copyContext(prompt.session, workspace.aliasOf(prompt.session))}
      onanswer={(a) => workspace.answer(prompt, a)}
    />
  {/key}
{:else if conflict}
  {#key conflict}
    <Modal
      onescape={() => files.answerConflict(null)}
      onkey={(k) => {
        const policy = ({ k: "rename", o: "overwrite", s: "skip" } as const)[k as "k" | "o" | "s"];
        if (policy) files.answerConflict(policy);
        return !!policy;
      }}
      labelledBy="conflict-title"
    >
      {#snippet title()}<span class="mono">{conflict.name}</span> already exists{/snippet}
      <p>
        {conflict.retry.kind === "download" ? "Your download folder" : "The remote folder"} already has a file with this name.
        Keep both to save this one as a numbered copy.
      </p>
      {#snippet footer()}
        <button class="secondary" onclick={() => files.answerConflict("skip")}>Skip <kbd>s</kbd></button>
        <button class="secondary" onclick={() => files.answerConflict("overwrite")}>Overwrite <kbd>o</kbd></button>
        <button class="primary" data-autofocus onclick={() => files.answerConflict("rename")}>Keep both <kbd>k</kbd></button>
      {/snippet}
    </Modal>
  {/key}
{:else if vault.dialog}
  {#key vault.dialog}
    <VaultDialog dialog={vault.dialog} />
  {/key}
{:else if keys.open}
  {#key keys.view}
    {#if keys.view.kind === "new"}
      <KeyGenDialog />
    {:else if keys.view.kind === "agent-add"}
      <AgentAddDialog key={keys.view.key} />
    {:else if keys.view.kind === "copy-pick"}
      <CopyIdDialog pick={keys.view.key} />
    {:else if keys.view.kind === "copy"}
      <CopyIdDialog pick={null} />
    {:else}
      <KeysDialog />
    {/if}
  {/key}
{:else if snippets.open}
  {#key snippets.view.kind}
    <SnippetsDialog target={snippetTarget} />
  {/key}
{:else if tunnelsOpen}
  <TunnelsDialog
    session={tunnelSession?.id ?? null}
    alias={tunnelSession?.alias}
    host={tunnelSession ? hosts.byAlias[tunnelSession.alias] : undefined}
    onclose={() => (tunnelsOpen = false)}
  />
{:else if editing}
  {#key editing.alias}
    <EditHostDialog host={editing} onclose={() => (hosts.editing = null)} />
  {/key}
{:else if hosts.confirmClear}
  <Modal
    onescape={() => (hosts.confirmClear = false)}
    onkey={(key) => {
      if (key === "y") void hosts.clearRecent();
      else if (key === "n") hosts.confirmClear = false;
      else return false;
      return true;
    }}
    labelledBy="clear-recent-title"
  >
    {#snippet title()}Clear recent hosts?{/snippet}
    <p>
      {hosts.recent.length === 1 ? "The recent host leaves" : `The ${hosts.recent.length} recent hosts leave`} the “Recent” section of the
      sidebar. Hosts, favorites and groups stay as they are.
    </p>
    {#snippet footer()}
      <button class="secondary" onclick={() => void hosts.clearRecent()}>Clear <kbd>y</kbd></button>
      <button class="primary" data-autofocus onclick={() => (hosts.confirmClear = false)}>Keep <kbd>n</kbd></button>
    {/snippet}
  </Modal>
{:else if logs.open}
  <LogsDialog />
{:else if importer.open}
  <ImportDialog />
{:else if recordings.confirm}
  {#key recordings.confirm}
    <RecordConfirm alias={recordings.confirm.alias} folder={recordings.confirm.folder} />
  {/key}
{:else if recordings.open}
  <RecordingsDialog themeId={settings.themeId} />
{/if}

{#if workspace.toast && !toastOnStage}
  {@render toastView(workspace.toast)}
{/if}

{#snippet toastView(toast: NonNullable<typeof workspace.toast>)}
  <!-- Above an open dialog, the notice goes to the top, in the room the
       dialog leaves it. Over a terminal it goes to the stage's top-right
       corner, compact: the active pane's input line stays in sight. -->
  <div
    class="toast"
    class:top={modalOpen}
    class:on-stage={toastOnStage}
    data-tone={toast.tone}
    role={toast.tone === "error" ? "alert" : "status"}
    bind:offsetHeight={toastHeight}
    transition:fly={{ y: modalOpen || toastOnStage ? -12 : 12, duration: dur("base") }}
  >
    <svg viewBox="0 0 16 16" aria-hidden="true">
      {#if toast.tone === "error"}<circle cx="8" cy="8" r="6.25" /><path d="M8 4.75v3.75M8 11.1v.1" />
      {:else if toast.tone === "warning"}<path d="M8 2.25 14.25 13.25H1.75Z" /><path d="M8 6.5v3M8 11.1v.1" />
      {:else}<circle cx="8" cy="8" r="6.25" /><path d="M8 7.25v4M8 4.9v.1" />{/if}
    </svg>
    <div class="toast-text">
      <strong>{sentence(toast.message)}</strong>
      {#if toast.hint}<span>{toast.hint}</span>{/if}
    </div>
  </div>
{/snippet}

<style>
  .shell {
    --titlebar-h: 44px;
    --statusbar-h: 28px;
    display: grid;
    grid-template-rows: var(--titlebar-h) 1fr var(--statusbar-h);
    height: 100%;
  }
  .titlebar {
    display: grid;
    grid-template-columns: auto minmax(0, 1fr) auto;
    align-items: center;
    gap: var(--space-4);
    padding: 0 var(--space-3) 0 var(--space-4);
    border-bottom: 1px solid var(--border-subtle);
    background: var(--bg-base);
  }
  .titlebar.mac { padding-left: 84px; /* room for the traffic lights */ }
  .brand { display: flex; align-items: center; gap: var(--space-2); padding-right: var(--space-2); }
  .wordmark {
    font-family: var(--font-display);
    font-size: var(--text-md);
    font-weight: 500;
    letter-spacing: 0.28em;
    color: var(--text-primary);
  }
  .palette-trigger {
    display: flex;
    align-items: center;
    gap: var(--space-2);
    height: 28px;
    padding: 0 var(--space-2);
    border: 1px solid var(--border-subtle);
    border-radius: var(--radius-md);
    background: var(--bg-surface);
    color: var(--text-muted);
    font: inherit;
    transition: border-color var(--motion-fast) var(--ease-standard);
  }
  .palette-trigger:hover { border-color: var(--border-strong); }
  .palette-trigger svg { width: 14px; height: 14px; fill: none; stroke: currentColor; stroke-width: 1.5; stroke-linecap: round; }
  main {
    position: relative;
    display: grid;
    grid-template-columns: minmax(0, 1fr);
    min-height: 0;
  }
  main.with-files { grid-template-columns: minmax(0, 1fr) min(420px, 42%); }
  /* Above the (empty, transparent) terminal stage: it must not take the
     host list's clicks. Hidden while a tab is active. */
  main > .hosts { position: absolute; inset: 0; z-index: 1; }
  .stage { position: relative; min-width: 0; min-height: 0; }
  .slot { position: absolute; inset: 0; }
  .slot.hidden { display: none; }
  .hosts {
    display: grid;
    grid-template-columns: minmax(280px, 340px) 1fr;
    min-height: 0;
  }
  .hosts.hidden { display: none; }
  .statusbar {
    display: flex;
    align-items: center;
    gap: var(--space-4);
    min-width: 0;
    padding: 0 var(--space-4);
    border-top: 1px solid var(--border-subtle);
    background: var(--bg-base);
    color: var(--text-muted);
    font-size: var(--text-xs);
    white-space: nowrap;
  }
  /*
   * Room is shared by priority, from the space actually left (no viewport
   * breakpoint): the session status keeps its full width, the terminal
   * title shrinks to an ellipsis, and the hints take what remains. They
   * lay out right to left (DOM order = priority) and wrap; a wrapped hint
   * falls on a second line that the bar clips, so whole hints disappear,
   * least useful first, never a half-cut one.
   */
  .hints {
    display: flex;
    flex: 1 1 0;
    flex-direction: row-reverse;
    flex-wrap: wrap;
    align-content: flex-start;
    align-self: stretch;
    column-gap: var(--space-4);
    min-width: 0;
    overflow: hidden;
  }
  .hints > span { display: flex; align-items: center; gap: var(--space-1); height: 100%; }
  .counts { flex: 0 0 auto; }
  .theme-name { flex: 0 0 auto; padding-left: var(--space-3); border-left: 1px solid var(--border-subtle); }
  .session {
    --c: var(--status-idle);
    display: flex;
    flex: 0 1 auto;
    align-items: center;
    gap: var(--space-2);
    min-width: 0;
    color: var(--text-secondary);
  }
  .session > * { flex: 0 0 auto; }
  .session[data-status="connecting"] { --c: var(--status-connecting); }
  .session[data-status="authenticating"] { --c: var(--status-authenticating); }
  .session[data-status="connected"] { --c: var(--status-connected); }
  .session[data-status="warning"] { --c: var(--status-warning); }
  .session[data-status="error"] { --c: var(--status-error); }
  .session .dot { flex: 0 0 auto; width: var(--size-dot); height: var(--size-dot); border-radius: 50%; background: var(--c); }
  .session .dot.hollow { background: transparent; box-shadow: inset 0 0 0 var(--size-dot-ring) var(--c); }
  .session .dot.busy { animation: pulse var(--motion-loop) var(--ease-standard) infinite; }
  .session .ended { color: var(--text-primary); }
  .session .alias { max-width: 24ch; overflow: hidden; color: var(--text-primary); font-weight: 550; text-overflow: ellipsis; }
  .session[data-status="error"] .phase { color: var(--status-error); }
  .rtt { color: var(--text-muted); }
  .pane-pos .mono { font-variant-numeric: tabular-nums; }
  .rtt.slow { color: var(--status-warning); }
  .session > .title { flex: 0 1 auto; min-width: 0; overflow: hidden; color: var(--text-muted); text-overflow: ellipsis; }
  .session > * + *:not(.alias)::before { content: "·"; margin-right: var(--space-2); color: var(--text-muted); }
  @keyframes pulse {
    0% { box-shadow: 0 0 0 0 color-mix(in srgb, var(--c) 60%, transparent); }
    100% { box-shadow: 0 0 0 4px transparent; }
  }
  .toast {
    --c: var(--status-error);
    position: fixed;
    right: var(--space-5);
    bottom: calc(28px + var(--space-4));
    display: flex;
    align-items: flex-start;
    gap: var(--space-3);
    max-width: 420px;
    padding: var(--space-3) var(--space-4);
    background: var(--bg-overlay);
    border: 1px solid color-mix(in srgb, var(--c) 45%, transparent);
    border-left: 3px solid var(--c);
    border-radius: var(--radius-md);
    box-shadow: var(--shadow-raised);
    z-index: 30;
  }
  .toast.top {
    top: var(--space-4);
    right: 0;
    bottom: auto;
    left: 0;
    width: fit-content;
    margin-inline: auto;
  }
  /* Below the first row of pane headers, clear of every input line. */
  .toast.on-stage {
    position: absolute;
    top: var(--space-7);
    right: var(--space-3);
    bottom: auto;
    gap: var(--space-2);
    max-width: min(var(--size-card), calc(100% - var(--space-6)));
    padding: var(--space-2) var(--space-3);
  }
  .toast.on-stage .toast-text { gap: 0; }
  .toast[data-tone="warning"] { --c: var(--status-warning); }
  .toast[data-tone="info"] { --c: var(--border-strong); }
  .toast svg {
    flex: 0 0 auto;
    width: 14px;
    height: 14px;
    margin-top: 2px;
    fill: none;
    stroke: var(--c);
    stroke-width: 1.5;
    stroke-linecap: round;
    stroke-linejoin: round;
  }
  .toast[data-tone="info"] svg { stroke: var(--text-secondary); }
  .toast-text { display: grid; gap: var(--space-1); }
  .toast strong { font-weight: 550; }
  .toast span { color: var(--text-secondary); font-size: var(--text-sm); white-space: pre-line; }
  .demo {
    margin-right: var(--space-2);
    padding: 0 var(--space-2);
    border: 1px solid var(--border-strong);
    border-radius: var(--radius-pill);
    letter-spacing: var(--tracking-caps);
    text-transform: uppercase;
  }
</style>
