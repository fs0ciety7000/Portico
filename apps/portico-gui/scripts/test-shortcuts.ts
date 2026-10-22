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
  // Mod+D: favorite in the host list, split right in a terminal.
  assert.equal(t.hosts.get(eventSignature(key("d", "KeyD", { ctrlKey: true }))!), "app.favorite");
  assert.equal(t.terminal.get(eventSignature(key("D", "KeyD", { ctrlKey: true, shiftKey: true }))!), "pane.split-right");
  assert.equal(t.terminal.get(eventSignature(key("1", "Digit1", { altKey: true }))!), "tab.goto-1");
  // AZERTY: the letter typed counts, digits by physical key.
  assert.equal(t.hosts.get(eventSignature(key("w", "KeyZ", { ctrlKey: true }))!), "tab.close");
  assert.equal(t.hosts.get(eventSignature(key("&", "Digit1", { altKey: true }))!), "tab.goto-1");
  // `/` needs Shift on some layouts.
  assert.equal(t.hosts.get(eventSignature(key("/", "Digit7", { shiftKey: true }))!), "hosts.search");
  // Plain keys never act in a terminal.
  assert.equal(t.terminal.get(eventSignature(key("e", "KeyE"))!), undefined);
});

test("shared host-list keys: `*` toggles a favorite, Shift-X clears recent", () => {
  for (const p of ["mac", "linux"] as Platform[]) {
    const t = matchTables(defaultBindings(p), p);
    // `*` is Shift-8 on US layouts, unshifted on others.
    assert.equal(t.hosts.get(eventSignature(key("*", "Digit8", { shiftKey: true }))!), "hosts.favorite", p);
    assert.equal(t.hosts.get(eventSignature(key("*", "NumpadMultiply"))!), "hosts.favorite", p);
    assert.equal(t.terminal.get(eventSignature(key("*", "Digit8", { shiftKey: true }))!), undefined, p);
    assert.equal(t.hosts.get(eventSignature(key("X", "KeyX", { shiftKey: true }))!), "hosts.clear-recent", p);
    assert.equal(t.hosts.get(eventSignature(key("x", "KeyX"))!), "hosts.forget-recent", p);
  }
  assert.equal(formatCombo(parseCombo("Shift+X")!, "linux", "hosts"), "Shift-X");
  assert.equal(formatCombo(parseCombo("Shift+X")!, "mac", "hosts"), "⇧X");
});

test("overrides replace defaults; invalid entries are reported", () => {
  const ok = resolveKeymap({ "app.files": "Mod+Shift+U", "app.reload": "none", "hosts.clear-recent": "F9" }, "linux");
  assert.deepEqual(ok.problems, []);
  assert.equal(ok.bindings["app.files"]?.[0]?.key, "u");
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

test("pane commands: terminal only, distinct per platform, need a modifier", () => {
  const mac = matchTables(defaultBindings("mac"), "mac");
  const linux = matchTables(defaultBindings("linux"), "linux");
  const ev = (k: string, code: string, mods: Partial<KeyLike>) => eventSignature(key(k, code, mods))!;
  // ⌘D / ⌘⇧D on macOS; Ctrl-Shift-D / Ctrl-Shift-O on Linux (Ctrl-Shift-D is also Mod+Shift+D there).
  assert.equal(mac.terminal.get(ev("d", "KeyD", { metaKey: true })), "pane.split-right");
  assert.equal(mac.terminal.get(ev("D", "KeyD", { metaKey: true, shiftKey: true })), "pane.split-down");
  assert.equal(linux.terminal.get(ev("O", "KeyO", { ctrlKey: true, shiftKey: true })), "pane.split-down");
  assert.equal(linux.terminal.get(ev("E", "KeyE", { ctrlKey: true, shiftKey: true })), undefined);
  // Focus: ⌘⌥ arrows on macOS; Mod+arrows (Ctrl-Shift-arrows in a terminal) on Linux, never
  // Ctrl-Alt-arrows (workspace switching in GNOME, Cinnamon, Xfce).
  assert.equal(mac.terminal.get(ev("ArrowLeft", "ArrowLeft", { metaKey: true, altKey: true })), "pane.focus-left");
  assert.equal(linux.terminal.get(ev("ArrowDown", "ArrowDown", { ctrlKey: true, shiftKey: true })), "pane.focus-down");
  for (const d of ["ArrowLeft", "ArrowRight", "ArrowUp", "ArrowDown"]) {
    assert.equal(linux.terminal.get(ev(d, d, { ctrlKey: true, altKey: true })), undefined, d);
    assert.equal(linux.terminal.get(ev(d, d, { ctrlKey: true, altKey: true, shiftKey: true })), undefined, d);
  }
  assert.equal(formatCombo(parseCombo("Mod+Left")!, "linux", "terminal"), "Ctrl-Shift-←");
  // Divider: ⌥⌘R / Ctrl-Alt-Shift-R gives the nearest divider the keyboard; no resize-* defaults left.
  assert.equal(mac.terminal.get(ev("®", "KeyR", { metaKey: true, altKey: true })), "pane.divider");
  assert.equal(linux.terminal.get(ev("R", "KeyR", { ctrlKey: true, altKey: true, shiftKey: true })), "pane.divider");
  assert.equal(formatCombo(parseCombo("Mod+Alt+R")!, "mac", "terminal"), "⌥⌘R");
  assert.ok(!ACTIONS.some((a) => a.id.startsWith("pane.resize-")));
  // Option changes the typed character on macOS: the physical key counts.
  assert.equal(mac.terminal.get(ev("˜", "KeyN", { metaKey: true, altKey: true })), "pane.next");
  // Zoom.
  assert.equal(mac.terminal.get(ev("Enter", "Enter", { metaKey: true, shiftKey: true })), "pane.zoom");
  assert.equal(linux.terminal.get(ev("Enter", "Enter", { ctrlKey: true, shiftKey: true })), "pane.zoom");
  // Never in the host list.
  for (const t of [mac, linux]) {
    for (const id of t.hosts.values()) assert.ok(!id.startsWith("pane."), id);
  }
  // Labels: Ctrl-Shift on Linux, as for the global shortcuts.
  assert.equal(formatCombo(parseCombo("Mod+O")!, "linux", "terminal"), "Ctrl-Shift-O");
  assert.equal(formatCombo(parseCombo("Mod+Alt+Left")!, "mac", "terminal"), "⌥⌘←");
  // A terminal action needs a modifier, like a global one.
  assert.match(resolveKeymap({ "pane.zoom": "Z" }, "linux").problems.join(), /needs Mod/);
  // Rebinding a pane command onto a host-list key is fine (other context).
  assert.deepEqual(resolveKeymap({ "pane.next": "Mod+R" }, "mac").problems, []);
  // On Linux, Mod+R in a terminal is Ctrl-Shift-R: `session.record`'s key.
  assert.deepEqual(resolveKeymap({ "pane.next": "Mod+R" }, "linux").problems, ['pane.next = "Mod+R": already used by session.record']);
});

test("recording: Mod+Shift+R in a terminal only; Recordings is `v` in the host list only", () => {
  const mac = matchTables(defaultBindings("mac"), "mac");
  const linux = matchTables(defaultBindings("linux"), "linux");
  const ev = (k: string, code: string, mods: Partial<KeyLike>) => eventSignature(key(k, code, mods))!;
  assert.equal(mac.terminal.get(ev("R", "KeyR", { metaKey: true, shiftKey: true })), "session.record");
  assert.equal(linux.terminal.get(ev("R", "KeyR", { ctrlKey: true, shiftKey: true })), "session.record");
  // The host list keeps Mod+R (reload), Ctrl-Shift-R included on Linux.
  assert.equal(linux.hosts.get(ev("R", "KeyR", { ctrlKey: true, shiftKey: true })), "app.reload");
  assert.equal(mac.hosts.get(ev("R", "KeyR", { metaKey: true, shiftKey: true })), undefined);
  assert.equal(formatCombo(parseCombo("Mod+Shift+R")!, "linux", "terminal"), "Ctrl-Shift-R");
  assert.deepEqual(defaultBindings("linux")["recordings.open"], []);
  assert.deepEqual(defaultBindings("mac")["recordings.open"], []);
  // `v` (same id and key as the TUI), next to the host editor keys
  // (Shift-E, Shift-D, Mod+Shift+N, ⌘⌫ / Del) without touching them.
  for (const [p, t] of [["mac", mac], ["linux", linux]] as const) {
    assert.equal(t.hosts.get(ev("v", "KeyV", {})), "hosts.recordings", p);
    assert.equal(t.terminal.get(ev("v", "KeyV", {})), undefined, p);
    assert.equal(t.hosts.get(ev("V", "KeyV", { shiftKey: true })), undefined, p);
    assert.equal(t.hosts.get(ev("E", "KeyE", { shiftKey: true })), "host.edit", p);
    assert.equal(t.hosts.get(ev("D", "KeyD", { shiftKey: true })), "host.duplicate", p);
    assert.equal(t.hosts.get(ev("N", "KeyN", { ctrlKey: p === "linux", metaKey: p === "mac", shiftKey: true })), "host.add", p);
    assert.deepEqual(resolveKeymap({}, p).problems, [], p);
  }
  assert.equal(formatCombo(parseCombo("V")!, "linux", "hosts"), "v");
  assert.equal(ACTIONS.find((a) => a.id === "hosts.recordings")?.title, "Recordings…");
});

test("closing: Mod+W closes the focused pane (or the tab), close-all the whole tab; pane.close has no key", () => {
  const mac = matchTables(defaultBindings("mac"), "mac");
  const linux = matchTables(defaultBindings("linux"), "linux");
  const ev = (k: string, code: string, mods: Partial<KeyLike>) => eventSignature(key(k, code, mods))!;
  assert.equal(mac.terminal.get(ev("w", "KeyW", { metaKey: true })), "tab.close");
  assert.equal(mac.terminal.get(ev("W", "KeyW", { metaKey: true, shiftKey: true })), "tab.close-all");
  assert.equal(linux.terminal.get(ev("W", "KeyW", { ctrlKey: true, shiftKey: true })), "tab.close");
  // Mod+Shift+W would be Ctrl-Shift-W too in a Linux terminal: close-all takes Alt instead.
  assert.equal(linux.terminal.get(ev("W", "KeyW", { ctrlKey: true, altKey: true, shiftKey: true })), "tab.close-all");
  assert.equal(linux.hosts.get(ev("w", "KeyW", { ctrlKey: true, altKey: true })), "tab.close-all");
  // The old Ctrl-Shift-Q is gone (it quits other apps); pane.close stays bindable.
  assert.equal(linux.terminal.get(ev("Q", "KeyQ", { ctrlKey: true, shiftKey: true })), undefined);
  for (const p of ["mac", "linux"] as Platform[]) assert.deepEqual(defaultBindings(p)["pane.close"], [], p);
  assert.deepEqual(resolveKeymap({ "pane.close": "Mod+Shift+X" }, "linux").problems, []);
});

test("palette titles of pane commands say \"pane\"", () => {
  for (const a of ACTIONS.filter((a) => a.id.startsWith("pane."))) assert.match(a.title, /\bpane\b/, a.id);
});

test("host commands (phase 6b): shared titles, keys without conflict", () => {
  const titles = Object.fromEntries(ACTIONS.filter((a) => a.id.startsWith("host.")).map((a) => [a.id, a.title]));
  assert.deepEqual(titles, {
    "host.add": "Add host…",
    "host.edit": "Edit host…",
    "host.duplicate": "Duplicate as Portico host…",
    "host.delete": "Delete host",
  });
  const mac = matchTables(defaultBindings("mac"), "mac");
  const linux = matchTables(defaultBindings("linux"), "linux");
  const ev = (k: string, code: string, mods: Partial<KeyLike> = {}) => eventSignature(key(k, code, mods))!;
  // Add host: everywhere, a terminal included.
  assert.equal(mac.hosts.get(ev("N", "KeyN", { metaKey: true, shiftKey: true })), "host.add");
  assert.equal(linux.terminal.get(ev("N", "KeyN", { ctrlKey: true, shiftKey: true })), "host.add");
  // Ctrl-N stays "next host" in the search field (Sidebar).
  assert.equal(linux.hosts.get(ev("n", "KeyN", { ctrlKey: true })), undefined);
  for (const t of [mac, linux]) {
    assert.equal(t.hosts.get(ev("E", "KeyE", { shiftKey: true })), "host.edit");
    assert.equal(t.hosts.get(ev("e", "KeyE")), "hosts.edit");
    assert.equal(t.hosts.get(ev("D", "KeyD", { shiftKey: true })), "host.duplicate");
    assert.equal(t.hosts.get(ev("d", "KeyD")), "hosts.disconnect");
  }
  assert.equal(mac.hosts.get(ev("Backspace", "Backspace", { metaKey: true })), "host.delete");
  assert.equal(linux.hosts.get(ev("Delete", "Delete")), "host.delete");
  assert.equal(formatCombo(parseCombo("Mod+Backspace")!, "mac", "hosts"), "⌘Backspace");
});

test("Import hosts… has a free default shortcut, everywhere (phase 6b fixes)", () => {
  const action = ACTIONS.find((a) => a.id === "app.import-termius");
  assert.equal(action?.title, "Import hosts…");
  assert.equal(action?.scope, "global");
  for (const p of ["mac", "linux"] as Platform[]) {
    const combos = defaultBindings(p)["app.import-termius"] ?? [];
    assert.ok(combos.length > 0, `${p}: a default`);
    // No other action answers to it, in the host list or in a terminal.
    assert.deepEqual(resolveKeymap({}, p).problems, []);
  }
  const mac = matchTables(defaultBindings("mac"), "mac");
  const linux = matchTables(defaultBindings("linux"), "linux");
  const ev = (k: string, code: string, mods: Partial<KeyLike> = {}) => eventSignature(key(k, code, mods))!;
  assert.equal(mac.hosts.get(ev("M", "KeyM", { metaKey: true, shiftKey: true })), "app.import-termius");
  assert.equal(mac.terminal.get(ev("M", "KeyM", { metaKey: true, shiftKey: true })), "app.import-termius");
  assert.equal(linux.hosts.get(ev("M", "KeyM", { ctrlKey: true, shiftKey: true })), "app.import-termius");
  assert.equal(linux.terminal.get(ev("M", "KeyM", { ctrlKey: true, shiftKey: true })), "app.import-termius");
  // Plain Ctrl-M (Enter in a shell) stays the terminal's.
  assert.equal(linux.terminal.get(ev("m", "KeyM", { ctrlKey: true })), undefined);
});
