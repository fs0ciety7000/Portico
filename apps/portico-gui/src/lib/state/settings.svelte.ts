// Theme, keymap and "Copy on select" from settings.toml (read and written
// by the core).
//
// Theme model (phase 10): `theme` is a theme id or `auto` (the default),
// `dark_theme` the dark theme `auto` uses. `auto` paints Marble while the
// system is light (`prefers-color-scheme`, followed live) and the dark
// theme otherwise. Choosing a dark theme makes it the dark theme of `auto`;
// a run of `t` presses (no other shortcut in between, like the TUI) does
// not, for the themes it only passes through.
//
// Theme without a flash: the setting is cached in browser storage and
// resolved by `applyCachedTheme()` in main.ts, before the first paint;
// without a cache, tokens.css itself follows the system until then.
// settings.toml stays the reference and is applied as soon as it is read.
// An unknown theme name falls back to `auto`, with a notice.
import type { ThemeInfo } from "../api";
import { backend } from "../backend";
import { keymap } from "./keymap.svelte";
import { workspace } from "./workspace.svelte";

/** The setting that follows the system. */
export const AUTO = "auto";
const THEME_CACHE = "portico.theme";
const DARK_CACHE = "portico.darkTheme";
/** Default dark theme (the first theme of portico-theme). */
const DEFAULT_DARK = "obsidian";
/** The light theme `auto` uses. */
const LIGHT_THEME = "marble";
/** Themes known before `list_themes` answers (cached ids are checked against the tokens). */
const ID = /^[a-z0-9-]{1,64}$/;
/** The "Theme · …" notice of `t` (as short as the TUI's). */
const CYCLE_TOAST_MS = 1500;
/** Problems listed in the notice before "and n more". */
const LISTED = 3;
/** A notice that lists entries stays longer than the usual 4 s. */
const LIST_TOAST_MS = 9000;

const lightQuery = (): MediaQueryList | null =>
  typeof matchMedia === "function" ? matchMedia("(prefers-color-scheme: light)") : null;

/** The theme to paint: `selection` is an id or `auto`. */
export function resolveTheme(selection: string, dark: string, systemLight: boolean): string {
  if (selection !== AUTO) return selection;
  return systemLight ? LIGHT_THEME : dark;
}

function setThemeAttribute(id: string) {
  const root = document.documentElement;
  root.dataset.theme = id;
  // An id with no token block would leave the page without colors.
  if (!getComputedStyle(root).getPropertyValue("--bg-base").trim()) root.dataset.theme = DEFAULT_DARK;
}

function read(key: string): string | null {
  try {
    const v = localStorage.getItem(key);
    return v && ID.test(v) ? v : null;
  } catch {
    return null;
  }
}

/** Called by main.ts before mounting: the cached setting, resolved now. */
export function applyCachedTheme() {
  const selection = read(THEME_CACHE) ?? AUTO;
  const dark = read(DARK_CACHE) ?? DEFAULT_DARK;
  setThemeAttribute(resolveTheme(selection, dark, lightQuery()?.matches ?? false));
}

function cache(selection: string, dark: string) {
  try {
    localStorage.setItem(THEME_CACHE, selection);
    localStorage.setItem(DARK_CACHE, dark);
  } catch {
    // Not cached: tokens.css follows the system at the next start.
  }
}

class SettingsState {
  themes: ThemeInfo[] = $state([]);
  /** `auto` or a theme id, as saved. */
  selection: string = $state(read(THEME_CACHE) ?? AUTO);
  /** The dark theme `auto` uses. */
  darkTheme: string = $state(read(DARK_CACHE) ?? DEFAULT_DARK);
  /** The system appearance (`prefers-color-scheme`), followed live. */
  systemLight: boolean = $state(lightQuery()?.matches ?? false);
  /** The theme painted now (`data-theme`). */
  themeId: string = $derived(resolveTheme(this.selection, this.darkTheme, this.systemLight));
  theme: ThemeInfo | undefined = $derived(this.themes.find((t) => t.id === this.themeId));
  auto: boolean = $derived(this.selection === AUTO);
  /** `Auto · Marble`, or the theme's name. */
  label: string = $derived(this.theme ? (this.auto ? `Auto · ${this.theme.name}` : this.theme.name) : "");
  /** A finished mouse selection in a terminal is copied (`[gui] copy_on_select`). */
  copyOnSelect: boolean = $state(false);
  /** The settings dialog is open. */
  open: boolean = $state(false);
  /** Inside a run of `t` presses (see the header); any other shortcut ends it. */
  #cycling = false;

  constructor() {
    // The system flips (macOS appearance, GNOME dark style): `auto` follows
    // at once, without a reload.
    lightQuery()?.addEventListener("change", (e) => {
      this.systemLight = e.matches;
      this.#paint();
    });
  }

  #isDark(id: string) {
    return this.themes.some((t) => t.id === id && t.appearance === "dark");
  }

  /** Reads settings.toml: theme, keymap; one notice for what was ignored. */
  async init(themes: ThemeInfo[]) {
    this.themes = themes;
    await this.#read();
  }

  /** settings.toml changed outside this view (folder sync, phase 11):
   * theme and keymaps apply live, received keymaps checked like local
   * ones (invalid entries ignored and reported, F18). Nothing is written. */
  async reload() {
    if (!this.themes.length) return;
    await this.#read();
  }

  async #read() {
    const themes = this.themes;
    let settings;
    try {
      settings = await backend.getSettings();
    } catch (e) {
      workspace.notify(e);
      this.#paint();
      return;
    }
    if (settings.error) {
      // Invalid file: defaults everywhere (the core never rewrites it).
      this.#set(AUTO, DEFAULT_DARK);
      keymap.load({});
      workspace.notify(settings.error, "error", LIST_TOAST_MS);
      return;
    }
    this.copyOnSelect = settings.copyOnSelect;
    const problems: string[] = [];
    const wanted = settings.theme ?? AUTO;
    const selection = wanted === AUTO || themes.some((t) => t.id === wanted) ? wanted : AUTO;
    if (selection !== wanted) problems.push(`theme = "${wanted}": unknown theme, Auto is used`);
    let dark = settings.darkTheme ?? DEFAULT_DARK;
    if (!this.#isDark(dark)) {
      problems.push(`dark_theme = "${dark}": not a dark theme, ${themes.find((t) => t.id === DEFAULT_DARK)?.name ?? DEFAULT_DARK} is used`);
      dark = DEFAULT_DARK;
    }
    this.#set(selection, dark);
    problems.push(...keymap.load(settings.keymap));
    if (problems.length) {
      const shown = problems.slice(0, LISTED).join("\n");
      const more = problems.length > LISTED ? `\nand ${problems.length - LISTED} more` : "";
      workspace.notify(
        { code: "settings_ignored", message: `settings.toml: ${problems.length} ${problems.length === 1 ? "entry" : "entries"} ignored`, hint: shown + more },
        "warning",
        LIST_TOAST_MS,
      );
    }
  }

  #set(selection: string, dark: string) {
    this.selection = selection;
    this.darkTheme = dark;
    this.#paint();
  }

  /** `data-theme`, the cache and the window (title bar, background) follow `themeId`. */
  #paint() {
    const id = this.themeId;
    setThemeAttribute(id);
    cache(this.selection, this.darkTheme);
    backend.setWindowTheme(id).catch(() => {
      // Cosmetic: the title bar keeps its look, the page is themed anyway.
    });
  }

  /** Applies and remembers a choice (`auto` or a theme id) in settings.toml, through the core. */
  select(id: string, passing = false) {
    if (id === this.selection || (id !== AUTO && !this.themes.some((t) => t.id === id))) return;
    // A dark theme chosen (not only passed through) becomes the dark theme of `auto`.
    const dark = !passing && this.#isDark(id) ? id : this.darkTheme;
    this.#set(id, dark);
    // Saved like the TUI does: a dark theme on screen is `auto`'s if the user stops there.
    const saved = this.#isDark(id) ? id : dark;
    backend.saveTheme(id, saved).catch((e) => workspace.notify(e));
  }

  /** The choices in order: every theme, then Auto (the radio group shows Auto first). */
  get choices(): string[] {
    return [...this.themes.map((t) => t.id), AUTO];
  }

  /** `t`: the next choice (see the header for the dark theme of a run). */
  cycle() {
    const continuing = this.#cycling;
    this.#cycling = true;
    // The theme left at the start of a run is the one the user had settled on.
    if (!continuing && this.#isDark(this.selection)) this.darkTheme = this.selection;
    const list = this.choices;
    const next = list[(list.indexOf(this.selection) + 1) % list.length];
    if (next) this.select(next, true);
    workspace.notify({ code: "theme", message: `Theme · ${this.label}` }, "info", CYCLE_TOAST_MS);
  }

  /** Any shortcut other than `t`: the next `t` starts a new run. */
  endCycle() {
    this.#cycling = false;
  }

  /** Applies and remembers "Copy on select"; back to the old value if it cannot be saved. */
  setCopyOnSelect(on: boolean) {
    if (on === this.copyOnSelect) return;
    this.copyOnSelect = on;
    backend.saveCopyOnSelect(on).catch((e) => {
      this.copyOnSelect = !on;
      workspace.notify(e);
    });
  }
}

export const settings = new SettingsState();
