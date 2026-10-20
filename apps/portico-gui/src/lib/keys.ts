// Application shortcuts and their platform modifier.
//
// macOS: ⌘ never reaches the remote shell, so ⌘T / ⌘W / ⌘1..9 are free.
// Linux: Ctrl-T, Ctrl-W, Ctrl-K… belong to the shell (readline, vim, tmux),
// so inside a terminal the app modifier is Ctrl+Shift, like GNOME Terminal.
// Outside a terminal plain Ctrl also works.

export const isMac: boolean = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

/** Label of the app modifier for `<kbd>` hints. */
export const MOD = isMac ? "⌘" : "Ctrl-Shift-";

export function kbd(key: string): string {
  return isMac ? `⌘${key}` : `Ctrl-Shift-${key}`;
}

/** True when `e` uses the app modifier (`inTerminal`: stricter on Linux). */
export function hasMod(e: KeyboardEvent, inTerminal: boolean): boolean {
  if (isMac) return e.metaKey && !e.ctrlKey;
  if (e.metaKey || e.altKey || !e.ctrlKey) return false;
  return inTerminal ? e.shiftKey : true;
}

export type AppShortcut =
  | { action: "palette" }
  | { action: "new-tab" }
  | { action: "close-tab" }
  | { action: "go-tab"; index: number }
  | { action: "go-home" }
  | { action: "next-tab" }
  | { action: "prev-tab" }
  | { action: "copy" }
  | { action: "paste" }
  | { action: "reload" };

/**
 * Maps a keydown to an app shortcut, or null when the key belongs to the
 * focused view (and, in a terminal, to the remote program).
 */
export function appShortcut(e: KeyboardEvent, inTerminal: boolean): AppShortcut | null {
  if (e.type !== "keydown") return null;
  // Ctrl-Tab / Ctrl-Shift-Tab cycle tabs everywhere (rarely used by shells).
  if (e.ctrlKey && !e.metaKey && !e.altKey && e.key === "Tab") {
    return { action: e.shiftKey ? "prev-tab" : "next-tab" };
  }
  if (!hasMod(e, inTerminal)) return null;
  const digit = /^Digit([0-9])$/.exec(e.code);
  if (digit?.[1]) {
    const n = Number(digit[1]);
    return n === 0 ? { action: "go-home" } : { action: "go-tab", index: n - 1 };
  }
  switch (e.code) {
    case "KeyK":
      return { action: "palette" };
    case "KeyT":
      return { action: "new-tab" };
    case "KeyW":
      return { action: "close-tab" };
    case "KeyR":
      return inTerminal ? null : { action: "reload" };
    // ⌘C / ⌘V on macOS go through the native Edit menu; Linux needs ours.
    case "KeyC":
      return inTerminal && !isMac ? { action: "copy" } : null;
    case "KeyV":
      return inTerminal && !isMac ? { action: "paste" } : null;
    case "BracketRight":
      return { action: "next-tab" };
    case "BracketLeft":
      return { action: "prev-tab" };
    default:
      return null;
  }
}

/**
 * Pasted text may come from anywhere: remove ESC and C1 controls so it can
 * never close bracketed paste or inject terminal sequences.
 */
export function sanitizePaste(text: string): string {
  // eslint-disable-next-line no-control-regex
  return text.replace(/[\u001b\u0080-\u009f]/g, "");
}

/**
 * A remote title must not impersonate another host or reorder text: strip
 * controls and bidi overrides, cap the length.
 */
export function sanitizeTitle(title: string): string {
  // eslint-disable-next-line no-control-regex
  const clean = title.replace(/[\u0000-\u001f\u007f-\u009f‎‏‪-‮⁦-⁩]/g, "").trim();
  return clean.length > 256 ? clean.slice(0, 256) : clean;
}
