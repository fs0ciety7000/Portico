// Platform and text helpers for keyboard input. The shortcut table, its
// parsing and matching live in `shortcuts.ts` (pure, also used to generate
// docs/KEYBINDINGS.md) and `state/keymap.svelte.ts` (effective keymap).
//
// macOS: ⌘ never reaches the remote shell, so ⌘T / ⌘W / ⌘1..9 are free.
// Linux: Ctrl-T, Ctrl-W, Ctrl-K… belong to the shell (readline, vim, tmux),
// so inside a terminal the app modifier is Ctrl+Shift, like GNOME Terminal.

export const isMac: boolean = /Mac|iPhone|iPad/.test(navigator.platform || navigator.userAgent);

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
