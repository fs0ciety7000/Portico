// Links in a terminal pane (phase 9, ADR "Liens du terminal"): what can be
// activated and what the confirmation shows. Pure (no DOM, no Svelte):
// `scripts/test-terminal.ts` runs it under Node. The Rust command
// `open_url` checks the same rules again before opening anything.
//
// - Plain `http://` / `https://` URLs (xterm's web-links addon) and OSC 8
//   hyperlinks; any other scheme is never activatable.
// - Only `⌘-click` (macOS) / `Ctrl-click` (elsewhere) activates; a plain
//   click keeps selecting text.
// - Always confirmed; the host is emphasised; an OSC 8 text that names
//   another host gets a caution.

/** Longest URL that can be opened, in characters. */
export const MAX_LINK_CHARS = 2048;
export const TOO_LONG = "Link too long to open";

export type LinkCheck =
  | { ok: true; href: string; host: string }
  | { ok: false; reason: "too_long" | "refused" };

// eslint-disable-next-line no-control-regex
const CONTROL_OR_SPACE = /[\u0000-\u0020\u007f-\u00a0\u1680\u2000-\u200f\u2028-\u202f\u205f-\u206f\u3000\ufeff]/;

/** Whether `raw` may be offered for opening, and in which (normalized) form. */
export function checkLink(raw: string): LinkCheck {
  if ([...raw].length > MAX_LINK_CHARS) return { ok: false, reason: "too_long" };
  if (CONTROL_OR_SPACE.test(raw)) return { ok: false, reason: "refused" };
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return { ok: false, reason: "refused" };
  }
  if (url.protocol !== "http:" && url.protocol !== "https:") return { ok: false, reason: "refused" };
  if (url.username || url.password) return { ok: false, reason: "refused" };
  if (!url.hostname) return { ok: false, reason: "refused" };
  if ([...url.href].length > MAX_LINK_CHARS) return { ok: false, reason: "too_long" };
  return { ok: true, href: url.href, host: url.hostname };
}

/** The URL in three parts, the host in the middle (emphasised). */
export function splitAtHost(href: string, host: string): [string, string, string] {
  const at = href.indexOf("//");
  if (at < 0 || href.slice(at + 2, at + 2 + host.length) !== host) return [href, "", ""];
  const start = at + 2;
  return [href.slice(0, start), host, href.slice(start + host.length)];
}

/** Host named by a visible text (`example.com`, `https://example.com/x`), or null. */
function hostOf(text: string): string | null {
  const t = text.trim();
  if (!t || /\s/.test(t)) return null;
  try {
    const url = new URL(/^[a-z][a-z0-9+.-]*:\/\//i.test(t) ? t : `https://${t}`);
    // A bare word is no host ("docs", "here").
    return url.hostname.includes(".") || url.hostname.startsWith("[") ? url.hostname : null;
  } catch {
    return null;
  }
}

/**
 * The OSC 8 visible text differs from where the link goes: true unless the
 * text is the URL itself or names the same host.
 */
export function textMisleads(text: string | null, href: string, host: string): boolean {
  const t = text?.trim() ?? "";
  if (!t || t === href || `${t}/` === href) return false;
  return hostOf(t)?.toLowerCase() !== host.toLowerCase();
}

/** Visible text as shown in the caution: one line, capped. */
export function shownText(text: string, max = 120): string {
  // eslint-disable-next-line no-control-regex
  const flat = text.replace(/[\u0000-\u001f\u007f-\u009f\u200e\u200f\u202a-\u202e\u2066-\u2069]/g, " ").replace(/\s+/g, " ").trim();
  return flat.length > max ? `${flat.slice(0, max)}…` : flat;
}

export function caution(text: string, host: string): string {
  return `The text shown was “${shownText(text)}” but the link goes to ${host}.`;
}

/** The activation gesture: ⌘ on macOS, Ctrl elsewhere (no other modifier). */
export function isOpenGesture(e: { metaKey: boolean; ctrlKey: boolean; altKey: boolean; shiftKey: boolean }, mac: boolean): boolean {
  if (e.altKey || e.shiftKey) return false;
  return mac ? e.metaKey && !e.ctrlKey : e.ctrlKey && !e.metaKey;
}

export const openHint = (mac: boolean): string => (mac ? "⌘-click to open" : "Ctrl-click to open");
