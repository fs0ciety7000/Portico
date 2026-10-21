// Effective GUI keymap: defaults from `shortcuts.ts`, overridden by
// `[keymap.gui]` in settings.toml. Labels are reactive so the palette,
// tooltips and hints follow a reload of the settings.
import { isMac } from "../keys";
import {
  actionById,
  defaultBindings,
  eventSignature,
  formatCombo,
  isModified,
  matchTables,
  resolveKeymap,
  type Combo,
  type Context,
  type KeyLike,
  type Platform,
} from "../shortcuts";

export const platform: Platform = isMac ? "mac" : "linux";

class Keymap {
  bindings: Record<string, Combo[]> = $state(defaultBindings(platform));
  #tables = matchTables(defaultBindings(platform), platform);

  /** Applies overrides; returns the entries that were ignored. */
  load(overrides: Record<string, string>): string[] {
    const { bindings, problems } = resolveKeymap(overrides, platform);
    this.bindings = bindings;
    this.#tables = matchTables(bindings, platform);
    return problems;
  }

  /** First combination of `id` as shown to the user, or undefined when unbound. */
  label(id: string): string | undefined {
    const combo = this.bindings[id]?.[0];
    const action = actionById(id);
    return combo && action ? formatCombo(combo, platform, action.scope) : undefined;
  }

  /** `Files (⌘⇧F)` for tooltips. */
  tip(text: string, id: string): string {
    const l = this.label(id);
    return l ? `${text} (${l})` : text;
  }

  /**
   * The action a key event triggers in `context`. `typing`: the focus is in
   * a text field, so keys without Ctrl, Alt or ⌘ are left to it.
   */
  match(e: KeyLike, context: Context, typing = false): string | null {
    if (typing && !isModified(e) && !/^F\d+$/.test(e.key)) return null;
    const s = eventSignature(e);
    return s ? (this.#tables[context].get(s) ?? null) : null;
  }
}

export const keymap = new Keymap();
