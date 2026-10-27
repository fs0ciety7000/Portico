// Browser features of the Windows webview (WebView2) that a desktop app
// must not expose. Pure (no DOM global at load time), tested under Node by
// scripts/test-webview.ts.
//
// WebView2 keeps its browser accelerator keys on, and Tauri does not expose
// the switch (`AreBrowserAcceleratorKeysEnabled`): F5 or Ctrl-R outside a
// terminal would reload the page, which closes every session; Alt-Left would
// navigate back; Ctrl-P, Ctrl-F open the print dialog and the find bar. The
// webview only acts on a key whose `keydown` was not cancelled, so the app
// cancels these (capture phase, so a dialog's own handler cannot hide them).
// A terminal still gets them: xterm.js reads the key and sends it to the
// remote side whether or not the event was cancelled (F5 reaches `htop`).
// WebKitGTK and WKWebView have no such keys: nothing changes on Linux or macOS.

// Its default context menu (Back, Refresh, Save as, Print, Inspect) is
// cancelled the same way outside text fields, where Cut / Copy / Paste stay;
// the terminal shows its own menu.

/** The fields of a `KeyboardEvent` this module reads. */
export interface KeyLike {
  key: string;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
  metaKey: boolean;
}

/** WebView2, from the user agent (`Windows NT` in every Windows build). */
export function isWindowsWebview(userAgent: string): boolean {
  return /\bWindows NT\b/.test(userAgent);
}

/** Keys that reload, navigate, print, find, or open the caret browsing prompt. */
const BARE = new Set(["F3", "F5", "F7", "BrowserBack", "BrowserForward", "BrowserRefresh", "BrowserSearch"]);
/**
 * Ctrl (with or without Shift) + these letters: find (F, G), print (P),
 * reload (R), and the browser's own new window, open, save, source,
 * history and downloads (N, O, S, U, H, J), cancelled even where WebView2
 * ignores them today. None is a text editing key (A, C, V, X, Y, Z stay).
 */
const CTRL = new Set(["f", "g", "h", "j", "n", "o", "p", "r", "s", "u"]);

/**
 * Cancels, for the whole window, what WebView2 would do as a browser: the
 * keys above and the default context menu outside text fields. Returns the
 * cleanup. Only called on Windows (`isWindowsWebview`).
 */
export function guardBrowserFeatures(target: Window): () => void {
  const onKey = (e: KeyboardEvent) => {
    if (isBrowserAccelerator(e)) e.preventDefault();
  };
  const onMenu = (e: MouseEvent) => {
    const field = e.target instanceof Element && e.target.closest("input, textarea, [contenteditable]");
    if (!field) e.preventDefault();
  };
  // Capture: a dialog that stops a key's propagation still has it cancelled.
  target.addEventListener("keydown", onKey, true);
  target.addEventListener("contextmenu", onMenu, true);
  return () => {
    target.removeEventListener("keydown", onKey, true);
    target.removeEventListener("contextmenu", onMenu, true);
  };
}

/** A key WebView2 would act on as a browser (reload, back, print, find…). */
export function isBrowserAccelerator(e: KeyLike): boolean {
  if (e.metaKey) return false;
  if (BARE.has(e.key)) return !e.altKey;
  if (e.altKey && !e.ctrlKey && (e.key === "ArrowLeft" || e.key === "ArrowRight")) return true;
  if (e.ctrlKey && !e.altKey && CTRL.has(e.key.toLowerCase())) return true;
  return false;
}
