// GUI command table and keymap: stable action ids, default combinations,
// parsing of `settings.toml` overrides (`[keymap.gui]`), conflict and
// reserved-key checks, labels and key-event matching.
//
// Pure module (no DOM, no Svelte, no imports): `scripts/gen-keybindings.ts`
// runs it under Node to generate docs/KEYBINDINGS.md, so keep it free of
// TypeScript-only runtime syntax (enums, namespaces).
//
// Combination syntax (case-insensitive, `+` between parts):
//   Mod+Shift+O   Mod = ⌘ on macOS, Ctrl elsewhere (Ctrl+Shift once a
//                 terminal has the focus: plain Ctrl-letters belong to the
//                 shell, vim, tmux)
//   Alt+3, Ctrl+Tab, F5, Enter, Shift+E, /
//   none          removes the binding
// Modifiers: Mod, Ctrl, Alt (Option), Shift, Cmd (Meta, Super). Keys: a–z,
// 0–9, F1–F12, Enter, Esc, Tab, Space, Backspace, Delete, Insert, Up, Down,
// Left, Right, Home, End, PageUp, PageDown, and / [ ] . , ; ' - = ` \ ? *

export type Platform = "mac" | "linux";
/** Where an action listens: everywhere (terminal included), only in the
 * host list, or only in a terminal tab (split panes). */
export type Scope = "global" | "hosts" | "terminal";
/** What has the keyboard when a key is pressed. */
export type Context = "hosts" | "terminal";

export interface Combo {
  mod: boolean;
  ctrl: boolean;
  alt: boolean;
  shift: boolean;
  meta: boolean;
  /** Normalized: `k`, `5`, `f5`, `enter`, `up`, `/`… */
  key: string;
}

export interface ActionDef {
  id: string;
  /** Palette / tooltip wording (English UI). */
  title: string;
  /** Wording in docs/KEYBINDINGS.md (French). */
  doc: string;
  scope: Scope;
  /** Default combinations, same on both platforms or per platform. */
  defaults: string[] | { mac: string[]; linux: string[] };
  /** Cannot be rebound (copy/paste of the terminal). */
  fixed?: boolean;
}

const both = (...combos: string[]) => combos;
const per = (mac: string[], linux: string[]) => ({ mac, linux });

const tabGoto: ActionDef[] = [1, 2, 3, 4, 5, 6, 7, 8, 9].map((n) => ({
  id: `tab.goto-${n}`,
  title: `Go to tab ${n}`,
  doc: `Aller à l'onglet ${n}`,
  scope: "global",
  defaults: per([`Mod+${n}`], [`Alt+${n}`]),
}));

const arrows = ["left", "right", "up", "down"] as const;
const towards = { left: "à gauche", right: "à droite", up: "vers le haut", down: "vers le bas" } as const;
const arrowKey = { left: "Left", right: "Right", up: "Up", down: "Down" } as const;

/** Split panes (ADR "Split panes", docs/STACK.md): only in a terminal tab.
 * Palette titles always say "pane" (same wording as the TUI). */
const panes: ActionDef[] = [
  { id: "pane.split-right", title: "Split pane right", doc: "Diviser à droite (même session : un canal PTY de plus)", scope: "terminal", defaults: both("Mod+D") },
  { id: "pane.split-down", title: "Split pane down", doc: "Diviser en bas (même session)", scope: "terminal", defaults: per(["Mod+Shift+D"], ["Mod+O"]) },
  { id: "pane.split-host", title: "Split pane with host…", doc: "Diviser avec un autre hôte (sélecteur d'hôtes ; à droite si le panneau est plus large que haut, sinon en bas)", scope: "terminal", defaults: both() },
  ...arrows.map(
    (d): ActionDef => ({
      id: `pane.focus-${d}`,
      title: `Focus pane ${d}`,
      doc: `Focus au panneau voisin ${towards[d]}`,
      scope: "terminal",
      // Linux: not Ctrl-Alt-arrows, which GNOME, Cinnamon and Xfce keep for workspaces.
      defaults: per([`Mod+Alt+${arrowKey[d]}`], [`Mod+${arrowKey[d]}`]),
    }),
  ),
  {
    id: "pane.divider",
    title: "Move pane divider…",
    doc: "Prendre la séparation la plus proche du panneau actif : flèches pour la déplacer, `Home`/`End` jusqu'aux limites, `↵` ou `Esc` pour rendre le clavier au panneau",
    scope: "terminal",
    defaults: both("Mod+Alt+R"),
  },
  { id: "pane.next", title: "Next pane", doc: "Panneau suivant (ordre de lecture)", scope: "terminal", defaults: per(["Mod+Alt+N"], ["Ctrl+Alt+N"]) },
  { id: "pane.zoom", title: "Zoom pane", doc: "Zoom on/off : le panneau actif occupe tout l'onglet", scope: "terminal", defaults: both("Mod+Shift+Enter") },
  // Phase 9. Linux: Mod+F would be Ctrl-Shift-F in a terminal, which is
  // `app.files` (Mod+Shift+F) there; Ctrl-Alt-F is free (like `pane.next`).
  { id: "terminal.search", title: "Search scrollback", doc: "Rechercher dans le scrollback du panneau actif (texte littéral, casse ignorée sauf majuscule ; `↵` ou `↑` plus ancien, `Shift-↵` ou `↓` plus récent, `Esc` ferme et revient en bas ; touches : tableau « Terminal (GUI) : recherche, liens, menu »)", scope: "terminal", defaults: per(["Mod+F"], ["Ctrl+Alt+F"]) },
  { id: "pane.close", title: "Close pane", doc: "Fermer le panneau actif et son canal PTY (le dernier panneau d'une session la déconnecte). Sans raccourci par défaut : dans un onglet divisé, `tab.close` ferme le panneau actif", scope: "terminal", defaults: both() },
];

/**
 * Every bindable action. Palette commands use these ids; an action with no
 * default combination can still be bound in settings.toml (it then runs
 * its palette command).
 */
export const ACTIONS: ActionDef[] = [
  { id: "app.palette", title: "Command palette", doc: "Palette de commandes", scope: "global", defaults: both("Mod+K") },
  { id: "app.files", title: "Files (SFTP)", doc: "Fichiers (SFTP) de l'hôte, à côté du terminal (ouvre ou ferme le panneau)", scope: "global", defaults: both("Mod+Shift+F") },
  { id: "app.tunnels", title: "Tunnels", doc: "Tunnels", scope: "global", defaults: both("Mod+Shift+P") },
  { id: "app.keys", title: "Keys", doc: "Clés (locales, agent, vault)", scope: "global", defaults: both("Mod+Shift+I") },
  { id: "app.snippets", title: "Snippets", doc: "Snippets de l'hôte (la palette liste aussi « Run: <nom> »)", scope: "global", defaults: both("Mod+Shift+S") },
  { id: "app.logs", title: "Session logs", doc: "Journaux de session et config effective (panneau debug)", scope: "global", defaults: both("Mod+Shift+L") },
  { id: "app.favorite", title: "Toggle favorite", doc: "Favori on/off de l'hôte sélectionné (depuis un onglet : palette, « Add <alias> to favorites » ; dans un terminal, `Mod+D` divise le panneau)", scope: "hosts", defaults: both("Mod+D") },
  { id: "tab.new", title: "New tab", doc: "Nouvel onglet sur le même hôte (depuis la liste : se connecter)", scope: "global", defaults: both("Mod+T") },
  { id: "tab.close", title: "Close tab", doc: "Fermer l'onglet ; dans un onglet divisé, le panneau actif (le dernier panneau ferme l'onglet ; le dernier onglet d'un hôte le déconnecte ; pendant la connexion : annuler)", scope: "global", defaults: both("Mod+W") },
  // Linux: Mod+Shift+W is Ctrl-Shift-W in a terminal, i.e. `tab.close` there.
  { id: "tab.close-all", title: "Close tab and its panes", doc: "Fermer l'onglet et tous ses panneaux", scope: "global", defaults: per(["Mod+Shift+W"], ["Mod+Alt+W"]) },
  { id: "tab.next", title: "Next tab", doc: "Onglet suivant", scope: "global", defaults: per(["Ctrl+Tab", "Mod+]"], ["Ctrl+Tab"]) },
  { id: "tab.prev", title: "Previous tab", doc: "Onglet précédent", scope: "global", defaults: per(["Ctrl+Shift+Tab", "Mod+["], ["Ctrl+Shift+Tab"]) },
  { id: "tab.hosts", title: "Go to hosts", doc: "Liste des hôtes", scope: "global", defaults: per(["Mod+0"], ["Alt+0"]) },
  ...tabGoto,
  { id: "tab.copy", title: "Copy selection", doc: "Copier la sélection du terminal (macOS : menu Édition, `⌘C`)", scope: "global", defaults: per([], ["Ctrl+Shift+C"]), fixed: true },
  { id: "tab.paste", title: "Paste", doc: "Coller (macOS : `⌘V` ; Linux aussi `Shift-Insert`, clic milieu)", scope: "global", defaults: per([], ["Ctrl+Shift+V"]), fixed: true },
  { id: "tab.reconnect", title: "Reconnect", doc: "Reconnecter l'hôte de l'onglet", scope: "global", defaults: both() },
  { id: "tab.disconnect", title: "Disconnect", doc: "Déconnecter l'hôte de l'onglet", scope: "global", defaults: both() },
  { id: "app.reload", title: "Reload ~/.ssh/config", doc: "Recharger `~/.ssh/config` et `hosts.toml`", scope: "hosts", defaults: both("Mod+R") },
  { id: "hosts.search", title: "Search hosts", doc: "Rechercher (flou, `#tag`, `*` favoris, `@groupe`)", scope: "hosts", defaults: both("/") },
  { id: "hosts.down", title: "Next host", doc: "Hôte suivant", scope: "hosts", defaults: both("J", "Down") },
  { id: "hosts.up", title: "Previous host", doc: "Hôte précédent", scope: "hosts", defaults: both("K", "Up") },
  { id: "hosts.connect", title: "Connect", doc: "Se connecter (ouvre un onglet terminal, ou y revient ; aussi double-clic)", scope: "hosts", defaults: both("Enter") },
  { id: "hosts.edit", title: "Edit host", doc: "Éditer tags, groupe et notes de l'hôte sélectionné ; un hôte Portico ouvre le formulaire complet (depuis un onglet : palette, « Edit <alias> »)", scope: "hosts", defaults: both("E") },
  // Hosts created in Portico (phase 6b, ADR "Hôtes créés dans Portico"). Same
  // palette wording as the TUI. Linux: not Mod+N, which the search field
  // keeps for "next host" (Ctrl-N).
  { id: "host.add", title: "Add host…", doc: "Ajouter un hôte (dans `hosts.conf` de Portico, jamais `~/.ssh/config`) ; aussi le bouton « + » de la liste", scope: "global", defaults: both("Mod+Shift+N") },
  { id: "host.edit", title: "Edit host…", doc: "Modifier un hôte Portico ; sur un hôte de `~/.ssh/config` ou importé : lecture seule, duplication proposée", scope: "hosts", defaults: both("Shift+E") },
  { id: "host.duplicate", title: "Duplicate as Portico host…", doc: "Dupliquer l'hôte sélectionné comme hôte Portico (nouvel alias), puis le modifier", scope: "hosts", defaults: both("Shift+D") },
  // macOS: ⌘⌫ as in Finder; never while typing in a field (App.svelte).
  { id: "host.delete", title: "Delete host", doc: "Supprimer un hôte Portico (confirmation `y` / `n`) ; refusé pour les autres", scope: "hosts", defaults: per(["Mod+Backspace"], ["Delete"]) },
  { id: "hosts.disconnect", title: "Disconnect selected host", doc: "Se déconnecter (hôte sélectionné)", scope: "hosts", defaults: both("D") },
  { id: "hosts.files", title: "Files on selected host", doc: "Fichiers (SFTP) de l'hôte sélectionné (se connecte au besoin)", scope: "hosts", defaults: both("F") },
  { id: "hosts.tunnels", title: "Tunnels", doc: "Tunnels", scope: "hosts", defaults: both("P") },
  { id: "hosts.keys", title: "Keys", doc: "Clés", scope: "hosts", defaults: both("I") },
  { id: "hosts.snippets", title: "Snippets of selected host", doc: "Snippets de l'hôte sélectionné", scope: "hosts", defaults: both("S") },
  { id: "hosts.favorite", title: "Toggle favorite", doc: "Favori on/off de l'hôte sélectionné (second raccourci de `app.favorite`, hors champ de recherche)", scope: "hosts", defaults: both("*") },
  { id: "hosts.forget-recent", title: "Remove from recent", doc: "Retirer de « Recent » (sur une ligne de la section Recent)", scope: "hosts", defaults: both("X") },
  { id: "hosts.collapse", title: "Collapse group", doc: "Replier le groupe de l'hôte sélectionné", scope: "hosts", defaults: both("Left") },
  { id: "hosts.expand", title: "Expand group", doc: "Déplier le groupe", scope: "hosts", defaults: both("Right") },
  { id: "hosts.clear-recent", title: "Clear recent", doc: "Vider « Recent » (confirmation)", scope: "hosts", defaults: both("Shift+X") },
  { id: "app.theme", title: "Next theme", doc: "Thème suivant (mémorisé dans `settings.toml`)", scope: "hosts", defaults: both("T") },
  ...panes,
  // Recording (ADR "Enregistrement de sessions, asciicast v2"). Linux: Ctrl-Shift-R
  // in a terminal; `app.reload` (Mod+R) only listens in the host list.
  { id: "session.record", title: "Record session", doc: "Enregistrer le panneau actif (asciicast v2, confirmation : ce qui s'affiche est enregistré, jamais ce qui est tapé) ; arrêter s'il l'est déjà (sans confirmation). `REC` sur le panneau, l'onglet et la barre d'état", scope: "terminal", defaults: both("Mod+Shift+R") },
  { id: "recordings.open", title: "Recordings…", doc: "Enregistrements : liste, lecture et suppression (touches : tableau « Enregistrements (GUI) »). Sans raccourci par défaut dans un terminal : palette", scope: "global", defaults: both() },
  // Same id and key as the TUI's host list.
  { id: "hosts.recordings", title: "Recordings…", doc: "Enregistrements (second raccourci de `recordings.open`, depuis la liste des hôtes)", scope: "hosts", defaults: both("V") },
  // ⌘, as on macOS. Linux: Ctrl-Alt-, (Ctrl-Shift-, types `<` in a
  // terminal; Ctrl-Alt is free there, like `terminal.search`).
  { id: "app.settings", title: "Settings…", doc: "Réglages : thème, « Copy on select » (enregistrés dans `settings.toml`)", scope: "global", defaults: per(["Mod+,"], ["Ctrl+Alt+,"]) },
  { id: "keys.new", title: "New key", doc: "Nouvelle clé", scope: "global", defaults: both() },
  { id: "snippets.new", title: "New snippet", doc: "Nouveau snippet", scope: "global", defaults: both() },
  { id: "files.download-dir", title: "Change download folder", doc: "Changer de dossier de téléchargement", scope: "global", defaults: both() },
  // The id predates Termix: kept, so a rebound key still applies.
  // Free on both platforms and in a terminal (Linux: Ctrl-Shift-M), checked by test-shortcuts.
  { id: "app.import-termius", title: "Import hosts…", doc: "Importer des hôtes Termius ou Termix (format détecté ; aperçu, clés privées seulement si cochées, puis hôtes Portico dans `hosts.conf`, voir `docs/IMPORT.md`)", scope: "global", defaults: both("Mod+Shift+M") },
];

const BY_ID = new Map(ACTIONS.map((a) => [a.id, a]));

export function actionById(id: string): ActionDef | undefined {
  return BY_ID.get(id);
}

export function defaultCombos(action: ActionDef, platform: Platform): string[] {
  return Array.isArray(action.defaults) ? action.defaults : action.defaults[platform];
}

// ── Parsing ──────────────────────────────────────────────────────────────

const NAMED: Record<string, string> = {
  enter: "enter",
  return: "enter",
  esc: "escape",
  escape: "escape",
  tab: "tab",
  space: "space",
  backspace: "backspace",
  delete: "delete",
  del: "delete",
  insert: "insert",
  ins: "insert",
  up: "up",
  arrowup: "up",
  down: "down",
  arrowdown: "down",
  left: "left",
  arrowleft: "left",
  right: "right",
  arrowright: "right",
  home: "home",
  end: "end",
  pageup: "pageup",
  pgup: "pageup",
  pagedown: "pagedown",
  pgdn: "pagedown",
  slash: "/",
  comma: ",",
  period: ".",
};
const SYMBOLS = "/[].,;'-=`\\?*";

function normalizeKey(raw: string): string | null {
  const k = raw.toLowerCase();
  if (/^[a-z0-9]$/.test(k)) return k;
  if (/^f([1-9]|1[0-2])$/.test(k)) return k;
  if (k.length === 1 && SYMBOLS.includes(k)) return k;
  return NAMED[k] ?? null;
}

/** `Mod+Shift+O` → Combo, or null when unreadable. */
export function parseCombo(text: string): Combo | null {
  const parts = text.trim().split("+").map((p) => p.trim());
  const keyPart = parts.pop();
  if (!keyPart) return null;
  const key = normalizeKey(keyPart);
  if (!key) return null;
  const combo: Combo = { mod: false, ctrl: false, alt: false, shift: false, meta: false, key };
  for (const part of parts) {
    const flag = (
      {
        mod: "mod",
        ctrl: "ctrl",
        control: "ctrl",
        alt: "alt",
        option: "alt",
        opt: "alt",
        shift: "shift",
        cmd: "meta",
        command: "meta",
        meta: "meta",
        super: "meta",
      } as const
    )[part.toLowerCase() as "mod"];
    if (!flag || combo[flag]) return null;
    combo[flag] = true;
  }
  return combo;
}

const isSymbol = (key: string) => key.length === 1 && SYMBOLS.includes(key);
const hasModifier = (c: Combo) => c.mod || c.ctrl || c.alt || c.meta;

// ── Concrete key events ──────────────────────────────────────────────────

/** Signature of a key event: modifiers then key (`CS:k`). */
function sig(ctrl: boolean, alt: boolean, shift: boolean, meta: boolean, key: string): string {
  return `${ctrl ? "C" : ""}${alt ? "A" : ""}${shift ? "S" : ""}${meta ? "M" : ""}:${key}`;
}

/** Every event signature `combo` answers to in `context`. */
export function signatures(combo: Combo, platform: Platform, context: Context): string[] {
  let ctrl = combo.ctrl;
  let meta = combo.meta;
  const shifts: boolean[] = [combo.shift];
  if (combo.mod) {
    if (platform === "mac") meta = true;
    else {
      ctrl = true;
      // Linux: Ctrl+Shift works everywhere, plain Ctrl only outside a terminal.
      if (!combo.shift) {
        if (context === "terminal") shifts.splice(0, 1, true);
        else shifts.push(true);
      }
    }
  }
  // A symbol may need Shift on some layouts (`/`, `?`): Shift is not part of it.
  if (isSymbol(combo.key) && !shifts.includes(!combo.shift)) shifts.push(!combo.shift);
  return shifts.map((shift) => sig(ctrl, combo.alt, shift, meta, combo.key));
}

/** Contexts an action of `scope` listens in. */
export const contextsOf = (scope: Scope): Context[] =>
  scope === "global" ? ["hosts", "terminal"] : scope === "terminal" ? ["terminal"] : ["hosts"];

const MAC_MENU = ["c", "v", "x", "a", "z", "q", "h", "m"];

/** Why `combo` cannot be bound in `scope`, or null. */
export function reservedReason(combo: Combo, platform: Platform, scope: Scope): string | null {
  if (scope !== "hosts" && !hasModifier(combo) && !/^f\d+$/.test(combo.key)) {
    return "needs Mod, Ctrl or Alt (plain keys belong to the terminal)";
  }
  if (!hasModifier(combo) && (combo.key === "escape" || combo.key === "tab")) return "reserved for dialogs and focus";
  for (const context of contextsOf(scope)) {
    for (const s of signatures(combo, platform, context)) {
      const [mods = "", key = ""] = s.split(":");
      if (platform === "mac" && mods.replace("S", "") === "M" && MAC_MENU.includes(key)) {
        return "reserved by macOS (Edit menu, quit, hide)";
      }
      if (context === "terminal") {
        if (platform === "linux" && mods === "CS" && (key === "c" || key === "v")) return "reserved for terminal copy and paste";
        if (mods === "S" && key === "insert") return "reserved for terminal paste";
        if (mods === "C" && key !== "tab") return "Ctrl alone belongs to the shell in a terminal (use Mod)";
      }
    }
  }
  return null;
}

// ── Resolution of settings.toml overrides ───────────────────────────────

export interface ResolvedKeymap {
  /** Effective combinations per action id. */
  bindings: Record<string, Combo[]>;
  /** One line per ignored entry, e.g. `tab.new = "Mod+W": already used by tab.close`. */
  problems: string[];
}

function conflictOf(
  id: string,
  combos: Combo[],
  bindings: Record<string, Combo[]>,
  platform: Platform,
): string | null {
  const scope = BY_ID.get(id)?.scope ?? "global";
  for (const context of contextsOf(scope)) {
    const mine = new Set(combos.flatMap((c) => signatures(c, platform, context)));
    for (const [other, list] of Object.entries(bindings)) {
      if (other === id) continue;
      const otherScope = BY_ID.get(other)?.scope ?? "global";
      if (!contextsOf(otherScope).includes(context)) continue;
      if (list.some((c) => signatures(c, platform, context).some((s) => mine.has(s)))) return other;
    }
  }
  return null;
}

export function defaultBindings(platform: Platform): Record<string, Combo[]> {
  const out: Record<string, Combo[]> = {};
  for (const a of ACTIONS) {
    out[a.id] = defaultCombos(a, platform).flatMap((t) => {
      const c = parseCombo(t);
      return c ? [c] : [];
    });
  }
  return out;
}

/** Applies `[keymap.gui]` on top of the defaults; invalid entries are
 * ignored (the default stays) and reported. */
export function resolveKeymap(overrides: Record<string, string>, platform: Platform): ResolvedKeymap {
  const problems: string[] = [];
  const pending = new Map<string, { text: string; combos: Combo[] }>();
  for (const [id, text] of Object.entries(overrides)) {
    const entry = `${id} = "${text}"`;
    const action = BY_ID.get(id);
    if (!action) {
      problems.push(`${entry}: unknown action`);
      continue;
    }
    if (action.fixed) {
      problems.push(`${entry}: this shortcut cannot be changed`);
      continue;
    }
    if (text.trim().toLowerCase() === "none") {
      pending.set(id, { text, combos: [] });
      continue;
    }
    const combo = parseCombo(text);
    if (!combo) {
      problems.push(`${entry}: unreadable combination`);
      continue;
    }
    const reserved = reservedReason(combo, platform, action.scope);
    if (reserved) {
      problems.push(`${entry}: ${reserved}`);
      continue;
    }
    pending.set(id, { text, combos: [combo] });
  }
  const defaults = defaultBindings(platform);
  // Drop conflicting overrides one at a time (a dropped one restores a
  // default, which may conflict in turn) until the table is consistent.
  for (;;) {
    const bindings: Record<string, Combo[]> = { ...defaults };
    for (const [id, p] of pending) bindings[id] = p.combos;
    let dropped = false;
    for (const [id, p] of pending) {
      const other = conflictOf(id, p.combos, bindings, platform);
      if (other) {
        problems.push(`${id} = "${p.text}": already used by ${other}`);
        pending.delete(id);
        dropped = true;
        break;
      }
    }
    if (!dropped) return { bindings, problems };
  }
}

// ── Labels ───────────────────────────────────────────────────────────────

const KEY_LABEL: Record<string, string> = {
  enter: "↵",
  escape: "Esc",
  tab: "Tab",
  space: "Space",
  backspace: "Backspace",
  delete: "Del",
  insert: "Insert",
  up: "↑",
  down: "↓",
  left: "←",
  right: "→",
  home: "Home",
  end: "End",
  pageup: "PgUp",
  pagedown: "PgDn",
};

/**
 * What the UI shows: `⌘⇧F` on macOS, `Ctrl-Shift-F` on Linux. A global
 * (or terminal) `Mod` shortcut shows Ctrl-Shift on Linux (it works
 * everywhere, terminal included); in the host list it shows plain Ctrl.
 */
export function formatCombo(c: Combo, platform: Platform, scope: Scope): string {
  const any = hasModifier(c) || c.shift;
  const key = KEY_LABEL[c.key] ?? (/^f\d+$/.test(c.key) ? c.key.toUpperCase() : any ? c.key.toUpperCase() : c.key);
  if (platform === "mac") {
    if (c.ctrl) {
      // Word style when Ctrl is involved (Ctrl-Shift-Tab).
      const words = ["Ctrl", c.alt && "Opt", (c.mod || c.meta) && "Cmd", c.shift && "Shift"].filter(Boolean);
      return `${words.join("-")}-${key}`;
    }
    return `${c.alt ? "⌥" : ""}${c.mod || c.meta ? "⌘" : ""}${c.shift ? "⇧" : ""}${key}`;
  }
  const shift = c.shift || (c.mod && scope !== "hosts");
  const words = [(c.ctrl || c.mod) && "Ctrl", c.alt && "Alt", shift && "Shift", c.meta && "Super"].filter(Boolean);
  return words.length ? `${words.join("-")}-${key}` : key;
}

// ── Matching ─────────────────────────────────────────────────────────────

/** The parts of a KeyboardEvent the matcher reads. */
export interface KeyLike {
  key: string;
  code: string;
  ctrlKey: boolean;
  altKey: boolean;
  shiftKey: boolean;
  metaKey: boolean;
}

const EVENT_NAMED: Record<string, string> = {
  Enter: "enter",
  Escape: "escape",
  Tab: "tab",
  " ": "space",
  Backspace: "backspace",
  Delete: "delete",
  Insert: "insert",
  ArrowUp: "up",
  ArrowDown: "down",
  ArrowLeft: "left",
  ArrowRight: "right",
  Home: "home",
  End: "end",
  PageUp: "pageup",
  PageDown: "pagedown",
};

/**
 * The key an event stands for. Letters: the typed character on any Latin
 * layout (AZERTY, Dvorak, Bépo), the physical key otherwise (Cyrillic…, or
 * Option on macOS). Digits: the physical key (Shift on AZERTY).
 */
export function eventKey(e: KeyLike): string | null {
  const typed = e.key.length === 1 ? e.key.toLowerCase() : "";
  const digit = /^(?:Digit|Numpad)([0-9])$/.exec(e.code);
  // A symbol typed on a digit key (`/` is Shift-7 in German) is that
  // symbol; with Ctrl, Alt or ⌘ the digit key is the digit (Alt-1 on AZERTY).
  if (digit?.[1] && !(isSymbol(typed) && !isModified(e))) return digit[1];
  const named = EVENT_NAMED[e.key];
  if (named) return named;
  if (/^F([1-9]|1[0-2])$/.test(e.key)) return e.key.toLowerCase();
  if (/^[a-z]$/.test(typed)) return typed;
  const letter = /^Key([A-Z])$/.exec(e.code);
  if (letter?.[1]) return letter[1].toLowerCase();
  if (typed && SYMBOLS.includes(typed)) return typed;
  return null;
}

export function eventSignature(e: KeyLike): string | null {
  const key = eventKey(e);
  return key ? sig(e.ctrlKey, e.altKey, e.shiftKey, e.metaKey, key) : null;
}

/** Lookup tables: event signature → action id, per context. */
export function matchTables(bindings: Record<string, Combo[]>, platform: Platform): Record<Context, Map<string, string>> {
  const tables: Record<Context, Map<string, string>> = { hosts: new Map(), terminal: new Map() };
  for (const [id, combos] of Object.entries(bindings)) {
    const scope = BY_ID.get(id)?.scope ?? "global";
    for (const context of contextsOf(scope)) {
      for (const c of combos) for (const s of signatures(c, platform, context)) tables[context].set(s, id);
    }
  }
  return tables;
}

/** True when the event carries a modifier that makes it a shortcut (not typing). */
export const isModified = (e: KeyLike): boolean => e.ctrlKey || e.altKey || e.metaKey;
