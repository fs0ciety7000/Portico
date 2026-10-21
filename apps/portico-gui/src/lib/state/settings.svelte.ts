// Theme and keymap from settings.toml (read and written by the core).
//
// Theme without a flash: the last applied theme is cached in browser
// storage and set by `applyCachedTheme()` in main.ts, before the first
// paint; settings.toml stays the reference and is applied as soon as it is
// read. An unknown theme name falls back to the default, with a notice.
import type { ThemeInfo } from "../api";
import { backend } from "../backend";
import { keymap } from "./keymap.svelte";
import { workspace } from "./workspace.svelte";

const THEME_CACHE = "portico.theme";
const DEFAULT_THEME = "obsidian";
/** Problems listed in the notice before "and n more". */
const LISTED = 3;
/** A notice that lists entries stays longer than the usual 4 s. */
const LIST_TOAST_MS = 9000;

function setThemeAttribute(id: string) {
  const root = document.documentElement;
  root.dataset.theme = id;
  // An id with no token block would leave the page without colors.
  if (!getComputedStyle(root).getPropertyValue("--bg-base").trim()) root.dataset.theme = DEFAULT_THEME;
}

/** Called by main.ts before mounting: the cached theme, if any. */
export function applyCachedTheme() {
  try {
    const id = localStorage.getItem(THEME_CACHE);
    if (id && /^[a-z0-9-]{1,64}$/.test(id)) setThemeAttribute(id);
  } catch {
    // No storage: the default theme until settings.toml is read.
  }
}

function cache(id: string) {
  try {
    localStorage.setItem(THEME_CACHE, id);
  } catch {
    // Not cached: a short flash at the next start, nothing more.
  }
}

class SettingsState {
  themes: ThemeInfo[] = $state([]);
  themeId: string = $state(document.documentElement.dataset.theme ?? DEFAULT_THEME);
  theme: ThemeInfo | undefined = $derived(this.themes.find((t) => t.id === this.themeId));

  /** Reads settings.toml: theme, keymap; one notice for what was ignored. */
  async init(themes: ThemeInfo[]) {
    this.themes = themes;
    let settings;
    try {
      settings = await backend.getSettings();
    } catch (e) {
      workspace.notify(e);
      return;
    }
    if (settings.error) {
      // Invalid file: defaults everywhere (the core never rewrites it).
      this.#apply(DEFAULT_THEME);
      keymap.load({});
      workspace.notify(settings.error, "error", LIST_TOAST_MS);
      return;
    }
    const problems: string[] = [];
    const wanted = settings.theme;
    if (wanted && themes.some((t) => t.id === wanted)) this.#apply(wanted);
    else {
      this.#apply(DEFAULT_THEME);
      if (wanted) problems.push(`theme = "${wanted}": unknown theme, ${this.theme?.name ?? DEFAULT_THEME} is used`);
    }
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

  #apply(id: string) {
    this.themeId = id;
    setThemeAttribute(id);
    cache(id);
  }

  /** Applies and remembers a theme (settings.toml, through the core). */
  select(id: string) {
    if (id === this.themeId || !this.themes.some((t) => t.id === id)) return;
    this.#apply(id);
    backend.saveTheme(id).catch((e) => workspace.notify(e));
  }

  cycle() {
    const at = this.themes.findIndex((t) => t.id === this.themeId);
    const next = this.themes[(at + 1) % Math.max(this.themes.length, 1)];
    if (next) this.select(next.id);
  }
}

export const settings = new SettingsState();
