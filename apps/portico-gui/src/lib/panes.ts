// Split panes: the tree of terminals inside one tab (ADR "Split panes",
// docs/STACK.md, phase 6).
//
// A node is either a leaf (one terminal, by pane key) or a split: two
// children side by side (`row`, "split right") or stacked (`column`, "split
// down"), sharing the room by `ratio` (the first child's part, 0–1).
//
// Pure module (no DOM, no Svelte, no imports): `scripts/test-panes.ts` runs
// it under Node, so keep it free of TypeScript-only runtime syntax. Every
// operation returns a new tree; geometry is in CSS pixels, from the stage
// rectangle, the gap between panes and the minimum pane size (20 columns ×
// 5 rows, converted to pixels by the caller from the terminal's cell size).

export type Axis = "row" | "column";
export type Direction = "left" | "right" | "up" | "down";

export interface Leaf {
  kind: "leaf";
  pane: number;
}

export interface Split {
  kind: "split";
  /** Stable id (divider key, mouse drag). */
  id: number;
  axis: Axis;
  /** Part of the first child, along `axis` (gap excluded). */
  ratio: number;
  a: PaneNode;
  b: PaneNode;
}

export type PaneNode = Leaf | Split;

export interface Rect {
  x: number;
  y: number;
  w: number;
  h: number;
}

export interface Size {
  w: number;
  h: number;
}

export interface Limits {
  /** Panes per tab. */
  maxPanes: number;
  /** Smallest pane, in px (20 × 5 cells plus the pane's own chrome). */
  min: Size;
  /** Room between two panes (the divider), in px. */
  gap: number;
}

/** At most this many panes per tab (ADR). */
export const MAX_PANES = 8;
/** Smallest pane, in terminal cells (ADR). */
export const MIN_COLS = 20;
export const MIN_ROWS = 5;
/** Keyboard resize: share of the split moved per key press. */
export const RESIZE_STEP = 0.05;

/** Why a split was refused. */
export type SplitRefusal = "max_panes" | "too_small" | "unknown_pane";

export type SplitResult = { ok: true; root: PaneNode } | { ok: false; reason: SplitRefusal };

export interface Divider {
  id: number;
  axis: Axis;
  ratio: number;
  /** The strip between the two children (where the handle goes). */
  rect: Rect;
  /** The whole split (drag: pointer position → ratio). */
  span: Rect;
}

export interface Layout {
  panes: Map<number, Rect>;
  dividers: Divider[];
}

export const leaf = (pane: number): Leaf => ({ kind: "leaf", pane });

/** Pane keys, in reading order (left to right, top to bottom). */
export function panesOf(node: PaneNode): number[] {
  return node.kind === "leaf" ? [node.pane] : [...panesOf(node.a), ...panesOf(node.b)];
}

export function contains(node: PaneNode, pane: number): boolean {
  return node.kind === "leaf" ? node.pane === pane : contains(node.a, pane) || contains(node.b, pane);
}

const along = (axis: Axis, s: Size): number => (axis === "row" ? s.w : s.h);

/** The two children's rectangles: the first gets `ratio` of the room left by the gap. */
function halves(rect: Rect, axis: Axis, ratio: number, gap: number): [Rect, Rect, Rect] {
  if (axis === "row") {
    const room = Math.max(0, rect.w - gap);
    const first = Math.round(room * ratio);
    return [
      { x: rect.x, y: rect.y, w: first, h: rect.h },
      { x: rect.x + first + gap, y: rect.y, w: room - first, h: rect.h },
      { x: rect.x + first, y: rect.y, w: gap, h: rect.h },
    ];
  }
  const room = Math.max(0, rect.h - gap);
  const first = Math.round(room * ratio);
  return [
    { x: rect.x, y: rect.y, w: rect.w, h: first },
    { x: rect.x, y: rect.y + first + gap, w: rect.w, h: room - first },
    { x: rect.x, y: rect.y + first, w: rect.w, h: gap },
  ];
}

/**
 * Where each pane goes in `rect`. With `zoom` (a pane of the tree), that
 * pane takes the whole rectangle and the others are left out.
 */
export function layout(node: PaneNode, rect: Rect, gap: number, zoom: number | null = null): Layout {
  const out: Layout = { panes: new Map(), dividers: [] };
  if (zoom !== null && contains(node, zoom)) {
    out.panes.set(zoom, rect);
    return out;
  }
  const walk = (n: PaneNode, r: Rect) => {
    if (n.kind === "leaf") {
      out.panes.set(n.pane, r);
      return;
    }
    const [a, b, strip] = halves(r, n.axis, n.ratio, gap);
    out.dividers.push({ id: n.id, axis: n.axis, ratio: n.ratio, rect: strip, span: r });
    walk(n.a, a);
    walk(n.b, b);
  };
  walk(node, rect);
  return out;
}

/** Smallest rectangle the subtree fits in. */
export function minSize(node: PaneNode, limits: Limits): Size {
  if (node.kind === "leaf") return limits.min;
  const a = minSize(node.a, limits);
  const b = minSize(node.b, limits);
  return node.axis === "row"
    ? { w: a.w + limits.gap + b.w, h: Math.max(a.h, b.h) }
    : { w: Math.max(a.w, b.w), h: a.h + limits.gap + b.h };
}

function replace(node: PaneNode, pane: number, by: PaneNode): PaneNode {
  if (node.kind === "leaf") return node.pane === pane ? by : node;
  const a = replace(node.a, pane, by);
  const b = replace(node.b, pane, by);
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

/**
 * Splits `target` in two along `axis`; `added` goes second (right or
 * below). Refused when the tab is full or either half would be smaller
 * than the minimum pane.
 */
export function splitPane(
  root: PaneNode,
  target: number,
  axis: Axis,
  added: number,
  splitId: number,
  rect: Rect,
  limits: Limits,
): SplitResult {
  if (!contains(root, target)) return { ok: false, reason: "unknown_pane" };
  if (panesOf(root).length >= limits.maxPanes) return { ok: false, reason: "max_panes" };
  const at = layout(root, rect, limits.gap).panes.get(target);
  if (!at) return { ok: false, reason: "unknown_pane" };
  const room = along(axis, at) - limits.gap;
  if (room / 2 < along(axis, limits.min)) return { ok: false, reason: "too_small" };
  const split: Split = { kind: "split", id: splitId, axis, ratio: 0.5, a: leaf(target), b: leaf(added) };
  return { ok: true, root: replace(root, target, split) };
}

/** The axis that gives the larger halves: side by side when wider than tall. */
export const preferredAxis = (r: Size): Axis => (r.w >= r.h ? "row" : "column");

/**
 * Removes a pane; its sibling takes the parent's place (the tree never
 * keeps a split with one child). Null when it was the last pane.
 */
export function removePane(root: PaneNode, pane: number): PaneNode | null {
  if (root.kind === "leaf") return root.pane === pane ? null : root;
  if (root.a.kind === "leaf" && root.a.pane === pane) return root.b;
  if (root.b.kind === "leaf" && root.b.pane === pane) return root.a;
  const a = removePane(root.a, pane);
  const b = removePane(root.b, pane);
  if (a === root.a && b === root.b) return root;
  if (!a) return b;
  if (!b) return a;
  return { ...root, a, b };
}

/** Next (or previous, `delta` < 0) pane in reading order, wrapping around. */
export function nextPane(root: PaneNode, pane: number, delta: number): number {
  const list = panesOf(root);
  const at = list.indexOf(pane);
  if (at < 0) return list[0] ?? pane;
  const n = list.length;
  return list[(((at + delta) % n) + n) % n] ?? pane;
}

/**
 * The pane next to `pane` in `dir`: among the panes across the nearest
 * edge, the one facing the middle of the current pane, else the one that
 * overlaps it most. Null at the edge of the tab.
 */
export function neighbor(root: PaneNode, pane: number, dir: Direction, rect: Rect, gap: number): number | null {
  const rects = layout(root, rect, gap).panes;
  const from = rects.get(pane);
  if (!from) return null;
  const horizontal = dir === "left" || dir === "right";
  // Tolerance: rounding of ratios may shift an edge by a pixel.
  const slack = gap + 1;
  let best: { pane: number; distance: number; facing: boolean; overlap: number } | null = null;
  for (const [key, r] of rects) {
    if (key === pane) continue;
    let distance: number;
    if (dir === "right") distance = r.x - (from.x + from.w);
    else if (dir === "left") distance = from.x - (r.x + r.w);
    else if (dir === "down") distance = r.y - (from.y + from.h);
    else distance = from.y - (r.y + r.h);
    if (distance < -1) continue;
    const [lo, hi, rlo, rhi] = horizontal ? [from.y, from.y + from.h, r.y, r.y + r.h] : [from.x, from.x + from.w, r.x, r.x + r.w];
    const overlap = Math.min(hi, rhi) - Math.max(lo, rlo);
    if (overlap <= 0) continue;
    const middle = (lo + hi) / 2;
    const facing = rlo <= middle && middle < rhi;
    const near = Math.max(0, distance) <= slack;
    if (!near) continue;
    if (
      !best ||
      distance < best.distance - 1 ||
      (Math.abs(distance - best.distance) <= 1 && (Number(facing) > Number(best.facing) || (facing === best.facing && overlap > best.overlap)))
    ) {
      best = { pane: key, distance, facing, overlap };
    }
  }
  return best ? best.pane : null;
}

/** Bounds of a split's ratio so both children keep their minimum size; null when nothing fits. */
export function ratioBounds(split: Split, span: Size, limits: Limits): [number, number] | null {
  const room = along(split.axis, span) - limits.gap;
  if (room <= 0) return null;
  const lo = along(split.axis, minSize(split.a, limits)) / room;
  const hi = 1 - along(split.axis, minSize(split.b, limits)) / room;
  return lo <= hi ? [lo, hi] : null;
}

/** `ratio` within the split's bounds (unchanged when no ratio fits: the window is too small for the tree). */
export function clampRatio(split: Split, ratio: number, span: Size, limits: Limits): number {
  const bounds = ratioBounds(split, span, limits);
  if (!bounds) return split.ratio;
  return Math.min(bounds[1], Math.max(bounds[0], ratio));
}

function withRatio(node: PaneNode, id: number, rect: Rect, limits: Limits, next: (s: Split) => number): PaneNode {
  if (node.kind === "leaf") return node;
  if (node.id === id) {
    const ratio = clampRatio(node, next(node), rect, limits);
    return ratio === node.ratio ? node : { ...node, ratio };
  }
  const [ra, rb] = halves(rect, node.axis, node.ratio, limits.gap);
  const a = withRatio(node.a, id, ra, limits, next);
  const b = withRatio(node.b, id, rb, limits, next);
  return a === node.a && b === node.b ? node : { ...node, a, b };
}

/** Sets a split's ratio (mouse drag), clamped to the minimum pane size. */
export function setRatio(root: PaneNode, id: number, ratio: number, rect: Rect, limits: Limits): PaneNode {
  return withRatio(root, id, rect, limits, () => ratio);
}

/** The nearest split above `pane` along `axis`, if any. */
export function enclosingSplit(root: PaneNode, pane: number, axis: Axis): Split | null {
  let found: Split | null = null;
  const walk = (n: PaneNode): boolean => {
    if (n.kind === "leaf") return n.pane === pane;
    if (walk(n.a) || walk(n.b)) {
      if (!found && n.axis === axis) found = n;
      return true;
    }
    return false;
  };
  walk(root);
  return found;
}

/**
 * Keyboard resize: moves the nearest divider around `pane` that runs
 * across `dir` (a vertical one for ←/→) one step towards `dir`, within the
 * minimum pane size. Unchanged when there is none.
 */
export function resizePane(root: PaneNode, pane: number, dir: Direction, rect: Rect, limits: Limits, step = RESIZE_STEP): PaneNode {
  const axis: Axis = dir === "left" || dir === "right" ? "row" : "column";
  const split = enclosingSplit(root, pane, axis);
  if (!split) return root;
  const delta = dir === "right" || dir === "down" ? step : -step;
  return withRatio(root, split.id, rect, limits, (s) => s.ratio + delta);
}

/**
 * Zoom on/off for the focused pane: a zoomed pane takes the whole tab.
 * Nothing to zoom with a single pane.
 */
export function toggleZoom(root: PaneNode, zoom: number | null, focus: number): number | null {
  if (panesOf(root).length < 2) return null;
  return zoom === focus ? null : focus;
}

/** Pixel size of the minimum pane, from the terminal cell size and the pane's chrome. */
export function minPanePx(cell: Size, chrome: Size): Size {
  return { w: Math.ceil(MIN_COLS * cell.w + chrome.w), h: Math.ceil(MIN_ROWS * cell.h + chrome.h) };
}
