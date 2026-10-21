// Application shortcuts and their platform modifier.
//
// macOS: ⌘ never reaches the remote shell, so ⌘T / ⌘W / ⌘1..9 are free.
// Linux: Ctrl-T, Ctrl-W, Ctrl-K… belong to the shell (readline, vim, tmux),
// so inside a terminal the app modifier is Ctrl+Shift, like GNOME Terminal.
// Outside a terminal plain Ctrl also works. Tabs by number use Alt+digit on
// Linux (Ctrl-Shift-2 / -6 are Ctrl-@ and Ctrl-^, used by emacs and vim).
//
// Letters are matched on the produced character (AZERTY, Dvorak, Bépo put
// W, K… elsewhere); digits on the physical key (they need Shift on AZERTY).

export const isMac: boolean = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

/** Label of the app modifier for `<kbd>` hints. */
export const MOD = isMac ? "⌘" : "Ctrl-Shift-";

export function kbd(key: string): string {
  return isMac ? `⌘${key}` : `Ctrl-Shift-${key}`;
}

/** Label of a shortcut that also needs Shift on macOS (⌘⇧F). */
export function kbdShift(key: string): string {
  return isMac ? `⌘⇧${key}` : `Ctrl-Shift-${key}`;
}

/** Label of "go to tab n" (0 = host list). */
export function tabKbd(n: number): string {
  return isMac ? `⌘${n}` : `Alt-${n}`;
}

/** The letter a key event stands for: the typed character on any Latin
 * layout, the physical key otherwise (Cyrillic, Greek…). */
function letterOf(e: KeyboardEvent): string | null {
  const key = e.key.length === 1 ? e.key.toLowerCase() : "";
  if (/^[a-z]$/.test(key)) return key;
  const code = /^Key([A-Z])$/.exec(e.code);
  return code?.[1] ? code[1].toLowerCase() : null;
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
  | { action: "reload" }
  | { action: "files" }
  | { action: "tunnels" }
  | { action: "keys" }
  | { action: "snippets" };

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
  const digit = /^Digit([0-9])$/.exec(e.code);
  const tabMod = isMac
    ? e.metaKey && !e.ctrlKey && !e.altKey
    : e.altKey && !e.ctrlKey && !e.metaKey && !e.shiftKey;
  if (digit?.[1] && tabMod) {
    const n = Number(digit[1]);
    return n === 0 ? { action: "go-home" } : { action: "go-tab", index: n - 1 };
  }
  if (!hasMod(e, inTerminal)) return null;
  if (isMac && e.code === "BracketRight") return { action: "next-tab" };
  if (isMac && e.code === "BracketLeft") return { action: "prev-tab" };
  switch (letterOf(e)) {
    case "k":
      return { action: "palette" };
    case "t":
      return { action: "new-tab" };
    case "w":
      return { action: "close-tab" };
    case "r":
      return inTerminal ? null : { action: "reload" };
    // ⌘F / ⌘P stay free on macOS (find, print): these need Shift there too.
    case "f":
      return isMac && !e.shiftKey ? null : { action: "files" };
    case "p":
      return isMac && !e.shiftKey ? null : { action: "tunnels" };
    // ⌘I / ⌘S stay free on macOS (italic, save): Shift there too.
    case "i":
      return isMac && !e.shiftKey ? null : { action: "keys" };
    case "s":
      return isMac && !e.shiftKey ? null : { action: "snippets" };
    // ⌘C / ⌘V on macOS go through the native Edit menu; Linux needs ours.
    case "c":
      return inTerminal && !isMac ? { action: "copy" } : null;
    case "v":
      return inTerminal && !isMac ? { action: "paste" } : null;
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
 * Text chosen by a remote server (window title, keyboard-interactive
 * messages) must not impersonate Portico or another host, nor reorder
 * itself: strip controls and bidi overrides, cap the length.
 */
// eslint-disable-next-line no-control-regex
const REMOTE_UNSAFE = /[\u0000-\u0009\u000b-\u001f\u007f-\u009f\u200e\u200f\u202a-\u202e\u2066-\u2069]/g;

export function sanitizeRemote(text: string, max: number, keepNewlines = false): string {
  const flat = keepNewlines ? text : text.replace(/\n/g, " ");
  const clean = flat.replace(REMOTE_UNSAFE, "").trim();
  return clean.length > max ? `${clean.slice(0, max)}…` : clean;
}

export const sanitizeTitle = (title: string): string => sanitizeRemote(title, 256);

/**
 * Splits text into chunks of at most `size` UTF-16 units without cutting a
 * surrogate pair (a lone surrogate is not valid JSON for the IPC).
 */
export function chunks(text: string, size: number): string[] {
  const out: string[] = [];
  let at = 0;
  while (at < text.length) {
    let end = Math.min(at + size, text.length);
    const last = text.charCodeAt(end - 1);
    if (end < text.length && last >= 0xd800 && last <= 0xdbff) end--;
    out.push(text.slice(at, end));
    at = end;
  }
  return out;
}

/** POSIX single-quoting for a command the user will paste in a shell. */
export function shellQuote(s: string): string {
  return `'${s.replaceAll("'", "'\\''")}'`;
}
