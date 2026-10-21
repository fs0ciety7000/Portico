// Checks of the keymap logic (src/lib/shortcuts.ts), run by `pnpm check`.
// Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  ACTIONS,
  defaultBindings,
  eventSignature,
  formatCombo,
  matchTables,
  parseCombo,
  resolveKeymap,
  type KeyLike,
  type Platform,
} from "../src/lib/shortcuts.ts";

const key = (k: string, code: string, mods: Partial<KeyLike> = {}): KeyLike => ({
  key: k,
  code,
  ctrlKey: false,
  altKey: false,
  shiftKey: false,
  metaKey: false,
  ...mods,
});

test("action ids are unique and defaults parse", () => {
  const ids = new Set<string>();
  for (const a of ACTIONS) {
    assert.ok(!ids.has(a.id), a.id);
    ids.add(a.id);
  }
  for (const p of ["mac", "linux"] as Platform[]) {
    for (const [id, combos] of Object.entries(defaultBindings(p))) {
      const action = ACTIONS.find((a) => a.id === id);
      const declared = Array.isArray(action?.defaults) ? action.defaults : action?.defaults[p];
      assert.equal(combos.length, declared?.length, `${p} ${id}`);
    }
  }
});

test("defaults have no conflict on either platform", () => {
  for (const p of ["mac", "linux"] as Platform[]) {
    assert.deepEqual(resolveKeymap({}, p).problems, []);
    const tables = matchTables(defaultBindings(p), p);
    assert.ok(tables.hosts.size > 0 && tables.terminal.size > 0);
  }
});

test("combinations parse and print per platform", () => {
  const c = parseCombo("mod+shift+o");
  assert.ok(c);
  assert.equal(formatCombo(c, "mac", "global"), "⌘⇧O");
  assert.equal(formatCombo(c, "linux", "global"), "Ctrl-Shift-O");
  const k = parseCombo("Mod+K");
  assert.ok(k);
  assert.equal(formatCombo(k, "linux", "global"), "Ctrl-Shift-K");
  assert.equal(formatCombo(k, "linux", "hosts"), "Ctrl-K");
  assert.equal(formatCombo(parseCombo("Ctrl+Shift+Tab")!, "mac", "global"), "Ctrl-Shift-Tab");
  assert.equal(formatCombo(parseCombo("Enter")!, "linux", "hosts"), "↵");
  for (const bad of ["", "Mod+", "Hyper+K", "Mod+Mod+K", "Ctrl+Shift", "K+Mod", "Mod+ü"]) {
    assert.equal(parseCombo(bad), null, bad);
  }
});

test("Mod on Linux: Ctrl in the host list, Ctrl-Shift in a terminal", () => {
  const t = matchTables(defaultBindings("linux"), "linux");
  assert.equal(t.hosts.get(eventSignature(key("k", "KeyK", { ctrlKey: true }))!), "app.palette");
  assert.equal(t.terminal.get(eventSignature(key("k", "KeyK", { ctrlKey: true }))!), undefined);
  assert.equal(t.terminal.get(eventSignature(key("K", "KeyK", { ctrlKey: true, shiftKey: true }))!), "app.palette");
  assert.equal(t.terminal.get(eventSignature(key("D", "KeyD", { ctrlKey: true, shiftKey: true }))!), "app.favorite");
  assert.equal(t.terminal.get(eventSignature(key("1", "Digit1", { altKey: true }))!), "tab.goto-1");
  // AZERTY: the letter typed counts, digits by physical key.
  assert.equal(t.hosts.get(eventSignature(key("w", "KeyZ", { ctrlKey: true }))!), "tab.close");
  assert.equal(t.hosts.get(eventSignature(key("&", "Digit1", { altKey: true }))!), "tab.goto-1");
  // `/` needs Shift on some layouts.
  assert.equal(t.hosts.get(eventSignature(key("/", "Digit7", { shiftKey: true }))!), "hosts.search");
  // Plain keys never act in a terminal.
  assert.equal(t.terminal.get(eventSignature(key("e", "KeyE"))!), undefined);
});

test("overrides replace defaults; invalid entries are reported", () => {
  const ok = resolveKeymap({ "app.files": "Mod+Shift+O", "app.reload": "none", "hosts.clear-recent": "F9" }, "linux");
  assert.deepEqual(ok.problems, []);
  assert.equal(ok.bindings["app.files"]?.[0]?.key, "o");
  assert.deepEqual(ok.bindings["app.reload"], []);
  assert.equal(ok.bindings["hosts.clear-recent"]?.[0]?.key, "f9");

  const bad = resolveKeymap(
    {
      "app.zoom": "Mod+=",
      "app.files": "Mod+Shift+",
      "tab.new": "Mod+W",
      "app.logs": "Ctrl+Shift+C",
      "app.keys": "Ctrl+R",
      "app.tunnels": "P",
      "tab.copy": "Mod+Shift+Y",
      "hosts.edit": "Esc",
    },
    "linux",
  );
  const text = bad.problems.join("\n");
  assert.match(text, /app\.zoom = "Mod\+=": unknown action/);
  assert.match(text, /app\.files = "Mod\+Shift\+": unreadable/);
  assert.match(text, /tab\.new = "Mod\+W": already used by tab\.close/);
  assert.match(text, /app\.logs = "Ctrl\+Shift\+C": reserved for terminal copy/);
  assert.match(text, /app\.keys = "Ctrl\+R": Ctrl alone belongs to the shell/);
  assert.match(text, /app\.tunnels = "P": needs Mod/);
  assert.match(text, /tab\.copy = .*cannot be changed/);
  assert.match(text, /hosts\.edit = "Esc": reserved/);
  assert.equal(bad.problems.length, 8);
  // Ignored entries keep the default.
  assert.equal(bad.bindings["tab.new"]?.[0]?.key, "t");

  const mac = resolveKeymap({ "app.logs": "Mod+Q", "app.keys": "Mod+Shift+K" }, "mac");
  assert.match(mac.problems.join("\n"), /app\.logs = "Mod\+Q": reserved by macOS/);
  // Distinct from ⌘K on macOS, but the same keys as Mod+K in a Linux terminal.
  assert.equal(mac.problems.length, 1);
  assert.match(resolveKeymap({ "app.keys": "Mod+Shift+K" }, "linux").problems.join(), /already used by app\.palette/);
});

test("swapping two shortcuts works", () => {
  const r = resolveKeymap({ "tab.new": "Mod+W", "tab.close": "Mod+T" }, "mac");
  assert.deepEqual(r.problems, []);
  assert.equal(r.bindings["tab.new"]?.[0]?.key, "w");
});
