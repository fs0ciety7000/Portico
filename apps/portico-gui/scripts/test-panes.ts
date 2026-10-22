// Checks of the split-pane tree (src/lib/panes.ts), run by `pnpm check`.
// Node's built-in test runner, no dependency.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  clampRatio,
  layout,
  leaf,
  MAX_PANES,
  minPanePx,
  neighbor,
  nextPane,
  panesOf,
  preferredAxis,
  ratioBounds,
  removePane,
  resizePane,
  setRatio,
  splitPane,
  toggleZoom,
  type Axis,
  type Limits,
  type PaneNode,
  type Rect,
  type Split,
} from "../src/lib/panes.ts";

const STAGE: Rect = { x: 0, y: 0, w: 1200, h: 800 };
const LIMITS: Limits = { maxPanes: MAX_PANES, min: { w: 200, h: 100 }, gap: 4 };

let ids = 100;
/** Splits or fails the test. */
function split(root: PaneNode, target: number, axis: Axis, added: number, rect = STAGE, limits = LIMITS): PaneNode {
  const r = splitPane(root, target, axis, added, ids++, rect, limits);
  assert.ok(r.ok, `split ${target} ${axis}: ${r.ok ? "" : r.reason}`);
  return r.root;
}

/**  1 | 2
 *   --+--
 *   3 | 4   (1 split right into 2, then each split down) */
function grid(): PaneNode {
  let t: PaneNode = leaf(1);
  t = split(t, 1, "row", 2);
  t = split(t, 1, "column", 3);
  t = split(t, 2, "column", 4);
  return t;
}

test("split right and down: the new pane goes second, halves share the room", () => {
  const t = split(leaf(1), 1, "row", 2);
  assert.deepEqual(panesOf(t), [1, 2]);
  const { panes, dividers } = layout(t, STAGE, LIMITS.gap);
  assert.deepEqual(panes.get(1), { x: 0, y: 0, w: 598, h: 800 });
  assert.deepEqual(panes.get(2), { x: 602, y: 0, w: 598, h: 800 });
  assert.equal(dividers.length, 1);
  assert.deepEqual(dividers[0]?.rect, { x: 598, y: 0, w: 4, h: 800 });

  const d = split(t, 2, "column", 3);
  assert.deepEqual(panesOf(d), [1, 2, 3]);
  const l = layout(d, STAGE, LIMITS.gap).panes;
  assert.deepEqual(l.get(2), { x: 602, y: 0, w: 598, h: 398 });
  assert.deepEqual(l.get(3), { x: 602, y: 402, w: 598, h: 398 });
  // Pane 1 is untouched.
  assert.deepEqual(l.get(1), { x: 0, y: 0, w: 598, h: 800 });
});

test("a split smaller than the minimum pane is refused", () => {
  const narrow: Rect = { x: 0, y: 0, w: 400, h: 800 };
  // 400 - 4 = 396 → two halves of 198 < 200.
  assert.deepEqual(splitPane(leaf(1), 1, "row", 2, 1, narrow, LIMITS), { ok: false, reason: "too_small" });
  // Down still fits (796 / 2 ≥ 100).
  assert.ok(splitPane(leaf(1), 1, "column", 2, 1, narrow, LIMITS).ok);
  // Exactly the minimum fits.
  assert.ok(splitPane(leaf(1), 1, "row", 2, 1, { ...narrow, w: 404 }, LIMITS).ok);
  // Unknown target.
  assert.deepEqual(splitPane(leaf(1), 9, "row", 2, 1, STAGE, LIMITS), { ok: false, reason: "unknown_pane" });
});

test("at most 8 panes per tab", () => {
  const big: Rect = { x: 0, y: 0, w: 4000, h: 4000 };
  let t: PaneNode = leaf(1);
  for (let p = 2; p <= MAX_PANES; p++) t = split(t, p - 1, p % 2 ? "row" : "column", p, big);
  assert.equal(panesOf(t).length, 8);
  assert.deepEqual(splitPane(t, 1, "row", 9, 1, big, LIMITS), { ok: false, reason: "max_panes" });
});

test("min pane size in px comes from 20 × 5 cells plus chrome", () => {
  assert.deepEqual(minPanePx({ w: 7.8, h: 15.6 }, { w: 24, h: 50 }), { w: 180, h: 128 });
});

test("closing a pane collapses its split: the sibling takes the room", () => {
  const t = grid();
  const without3 = removePane(t, 3);
  assert.ok(without3);
  assert.deepEqual(panesOf(without3), [1, 2, 4]);
  // Pane 1 takes the whole left column again.
  assert.deepEqual(layout(without3, STAGE, LIMITS.gap).panes.get(1), { x: 0, y: 0, w: 598, h: 800 });
  // No split is left with a single child.
  const walk = (n: PaneNode): void => {
    if (n.kind === "split") {
      assert.ok(n.a && n.b);
      walk(n.a);
      walk(n.b);
    }
  };
  walk(without3);
  const one = removePane(removePane(without3, 2)!, 4)!;
  assert.deepEqual(one, leaf(1));
  assert.equal(removePane(one, 1), null);
  // Unknown pane: same tree.
  assert.equal(removePane(t, 42), t);
});

test("focus by direction follows the geometry", () => {
  const t = grid();
  const n = (p: number, d: "left" | "right" | "up" | "down") => neighbor(t, p, d, STAGE, LIMITS.gap);
  assert.equal(n(1, "right"), 2);
  assert.equal(n(1, "down"), 3);
  assert.equal(n(3, "right"), 4);
  assert.equal(n(4, "up"), 2);
  assert.equal(n(4, "left"), 3);
  // Edges of the tab.
  assert.equal(n(1, "left"), null);
  assert.equal(n(1, "up"), null);
  assert.equal(n(4, "right"), null);
  assert.equal(n(4, "down"), null);
});

test("focus by direction prefers the pane facing the middle", () => {
  // 1 | (2 / 3): from 1, → goes to the pane at its vertical middle.
  let t: PaneNode = split(leaf(1), 1, "row", 2);
  t = split(t, 2, "column", 3);
  // Move the right divider up: pane 3 now covers the middle of pane 1.
  const inner = (t as Split).b as Split;
  t = setRatio(t, inner.id, 0.3, STAGE, LIMITS);
  assert.equal(neighbor(t, 1, "right", STAGE, LIMITS.gap), 3);
  t = setRatio(t, inner.id, 0.7, STAGE, LIMITS);
  assert.equal(neighbor(t, 1, "right", STAGE, LIMITS.gap), 2);
  // From either right pane, ← goes back to 1.
  assert.equal(neighbor(t, 2, "left", STAGE, LIMITS.gap), 1);
  assert.equal(neighbor(t, 3, "left", STAGE, LIMITS.gap), 1);
});

test("next pane cycles in reading order", () => {
  const t = grid();
  assert.deepEqual(panesOf(t), [1, 3, 2, 4]);
  assert.equal(nextPane(t, 1, 1), 3);
  assert.equal(nextPane(t, 4, 1), 1);
  assert.equal(nextPane(t, 1, -1), 4);
  assert.equal(nextPane(leaf(7), 7, 1), 7);
});

test("keyboard resize moves the nearest divider across the direction, within limits", () => {
  let t: PaneNode = split(leaf(1), 1, "row", 2);
  const id = (t as Split).id;
  t = resizePane(t, 1, "right", STAGE, LIMITS);
  assert.equal((t as Split).ratio, 0.55);
  t = resizePane(t, 2, "left", STAGE, LIMITS);
  t = resizePane(t, 2, "left", STAGE, LIMITS);
  assert.equal(Math.round((t as Split).ratio * 100), 45);
  // No horizontal divider: ↑ changes nothing.
  assert.equal(resizePane(t, 1, "up", STAGE, LIMITS), t);
  // Clamped: each side keeps 200 px of the 1196 px of room.
  for (let i = 0; i < 40; i++) t = resizePane(t, 1, "left", STAGE, LIMITS);
  assert.equal((t as Split).ratio, 200 / 1196);
  for (let i = 0; i < 40; i++) t = resizePane(t, 1, "right", STAGE, LIMITS);
  assert.equal((t as Split).ratio, 1 - 200 / 1196);
  assert.equal((t as Split).id, id);
});

test("resize of a nested split counts the minimum size of the whole subtree", () => {
  // 1 | (2 | 3): the left divider must leave room for two panes on the right.
  let t: PaneNode = split(leaf(1), 1, "row", 2);
  t = split(t, 2, "row", 3);
  const outer = t as Split;
  const bounds = ratioBounds(outer, STAGE, LIMITS);
  assert.ok(bounds);
  assert.equal(bounds[0], 200 / 1196);
  assert.equal(bounds[1], 1 - (200 + 4 + 200) / 1196);
  assert.equal(clampRatio(outer, 0.99, STAGE, LIMITS), bounds[1]);
  assert.equal(clampRatio(outer, -1, STAGE, LIMITS), bounds[0]);
  // Pane 3's ← resizes its own split (2 | 3), the nearest one.
  const after = resizePane(t, 3, "left", STAGE, LIMITS) as Split;
  assert.equal(after.ratio, outer.ratio);
  assert.equal((after.b as Split).ratio, 0.45);
  // A window too small for the tree: the ratio stays as it is.
  assert.equal(clampRatio(outer, 0.2, { w: 500, h: 800 }, LIMITS), outer.ratio);
});

test("mouse drag sets a ratio, clamped", () => {
  const t = split(leaf(1), 1, "column", 2);
  const id = (t as Split).id;
  assert.equal((setRatio(t, id, 0.25, STAGE, LIMITS) as Split).ratio, 0.25);
  assert.equal((setRatio(t, id, 0.01, STAGE, LIMITS) as Split).ratio, 100 / 796);
  // Unknown id: same tree.
  assert.equal(setRatio(t, 999, 0.3, STAGE, LIMITS), t);
});

test("zoom: the focused pane takes the whole tab; nothing to zoom alone", () => {
  const t = grid();
  assert.equal(toggleZoom(t, null, 3), 3);
  assert.equal(toggleZoom(t, 3, 3), null);
  // Zoomed on another pane: the zoom moves to the focused one.
  assert.equal(toggleZoom(t, 1, 3), 3);
  assert.equal(toggleZoom(leaf(1), null, 1), null);
  const zoomed = layout(t, STAGE, LIMITS.gap, 3);
  assert.deepEqual([...zoomed.panes.keys()], [3]);
  assert.deepEqual(zoomed.panes.get(3), STAGE);
  assert.equal(zoomed.dividers.length, 0);
  // A stale zoom (pane gone) shows the whole tree.
  assert.equal(layout(t, STAGE, LIMITS.gap, 42).panes.size, 4);
});

test("preferred axis: side by side when wider than tall", () => {
  assert.equal(preferredAxis({ w: 1200, h: 800 }), "row");
  assert.equal(preferredAxis({ w: 500, h: 800 }), "column");
});
