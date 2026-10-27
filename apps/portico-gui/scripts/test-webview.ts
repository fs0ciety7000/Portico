// Checks of the WebView2 browser key filter (src/lib/webview.ts), run by `pnpm check`.
// Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { test } from "node:test";
import { isBrowserAccelerator, isWindowsWebview, type KeyLike } from "../src/lib/webview.ts";

const key = (k: string, mods: Partial<Omit<KeyLike, "key">> = {}): KeyLike => ({
  key: k,
  ctrlKey: false,
  shiftKey: false,
  altKey: false,
  metaKey: false,
  ...mods,
});

test("WebView2 is told apart from WebKitGTK and WKWebView", () => {
  assert.ok(
    isWindowsWebview(
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36 Edg/130.0.0.0",
    ),
  );
  assert.ok(!isWindowsWebview("Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko)"));
  assert.ok(!isWindowsWebview("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 (KHTML, like Gecko)"));
});

test("reload, navigation, print and find are browser keys", () => {
  for (const k of [
    key("F5"),
    key("F5", { ctrlKey: true }),
    key("F5", { shiftKey: true }),
    key("F3"),
    key("F3", { shiftKey: true }),
    key("F7"),
    key("BrowserBack"),
    key("BrowserRefresh"),
    key("r", { ctrlKey: true }),
    key("R", { ctrlKey: true, shiftKey: true }),
    key("p", { ctrlKey: true }),
    key("f", { ctrlKey: true }),
    key("g", { ctrlKey: true }),
    key("ArrowLeft", { altKey: true }),
    key("ArrowRight", { altKey: true }),
  ]) {
    assert.ok(isBrowserAccelerator(k), JSON.stringify(k));
  }
});

test("editing keys, window keys and AltGr characters are left alone", () => {
  for (const k of [
    key("a", { ctrlKey: true }),
    key("c", { ctrlKey: true }),
    key("v", { ctrlKey: true }),
    key("x", { ctrlKey: true }),
    key("z", { ctrlKey: true }),
    key("y", { ctrlKey: true }),
    key("C", { ctrlKey: true, shiftKey: true }),
    key("V", { ctrlKey: true, shiftKey: true }),
    key("F4", { altKey: true }),
    key("F1"),
    key("F12"),
    key("Tab", { ctrlKey: true }),
    key("ArrowLeft"),
    key("ArrowLeft", { ctrlKey: true }),
    key("ArrowLeft", { ctrlKey: true, altKey: true }),
    // AltGr arrives as Ctrl+Alt on Windows: AltGr+F types a character on some layouts.
    key("f", { ctrlKey: true, altKey: true }),
    key("r", { metaKey: true }),
    key("r"),
    key("F5", { altKey: true }),
  ]) {
    assert.ok(!isBrowserAccelerator(k), JSON.stringify(k));
  }
});
