// Scrollback search of a terminal pane (phase 9, ADR "Recherche dans le
// scrollback"): the search itself, kept pure (no DOM, no Svelte, the xterm
// buffer behind an interface) so `scripts/test-terminal.ts` runs it under
// Node. Wording shared with the TUI.
//
// - The query is literal text, never a regex: it is compared with
//   `indexOf` / `lastIndexOf`, nothing is interpreted.
// - Smart case: case-insensitive unless the query has an uppercase letter.
// - Matches are found on the text of each logical line (wrapped rows
//   joined), cached until the buffer changes: a full 10 000-line scrollback
//   is a few `indexOf` per keystroke. (xterm's search addon was measured at
//   20 to 55 ms per keystroke there: its reverse search converts every
//   row's cells, and it decorates up to 1 000 matches from the top.)
// - The counter numbers matches from the bottom (the most recent output is
//   1), the direction `Enter` walks: `3 of 17`, `No match`, `999+` from
//   1 000 matches on (counting stops there).

/** Counting stops here: the counter shows `999+`. */
export const MATCH_CAP = 1000;
/** Wording of the bar (same as the TUI). */
export const SEARCH_LABEL = "Search";
export const NO_MATCH = "No match";
/** Keys of the bar, in the status bar while it is open (as in the TUI). */
export const SEARCH_HINT_KEYS: readonly (readonly [string, string])[] = [
  ["↵", "older"],
  ["⇧↵", "newer"],
  ["Esc", "close"],
];
/** The same keys as one line: the placeholder while the query is empty. */
export const SEARCH_HINTS = SEARCH_HINT_KEYS.map(([key, label]) => `${key} ${label}`).join(" · ");

/** Smart case: an uppercase letter anywhere makes the search case-sensitive. */
export function isCaseSensitive(query: string): boolean {
  return /\p{Lu}/u.test(query);
}

export interface Counter {
  text: string;
  /** No match: the warning colour. */
  none: boolean;
}

/**
 * The counter next to the query: `rank` is the current match's position
 * from the bottom (1 = most recent, 0 = none), `total` the matches counted
 * (at most `MATCH_CAP`).
 */
export function counter(query: string, rank: number, total: number): Counter {
  if (!query) return { text: "", none: false };
  if (total <= 0) return { text: NO_MATCH, none: true };
  if (total >= MATCH_CAP) return { text: "999+", none: false };
  if (rank <= 0 || rank > total) return { text: `${total} ${total === 1 ? "match" : "matches"}`, none: false };
  return { text: `${rank} of ${total}`, none: false };
}

// ── Colours: xterm decorations want opaque `#RRGGBB` ──────────────────────

type Rgba = [number, number, number, number];

function parseColor(text: string): Rgba | null {
  const c = text.trim().toLowerCase();
  const hex = /^#([0-9a-f]{3,8})$/.exec(c)?.[1];
  if (hex) {
    if (hex.length === 3 || hex.length === 4) {
      const d = [...hex].map((h) => Number.parseInt(h + h, 16));
      return [d[0] ?? 0, d[1] ?? 0, d[2] ?? 0, hex.length === 4 ? (d[3] ?? 255) / 255 : 1];
    }
    if (hex.length === 6 || hex.length === 8) {
      const d = [0, 2, 4, 6].map((i) => Number.parseInt(hex.slice(i, i + 2) || "ff", 16));
      return [d[0] ?? 0, d[1] ?? 0, d[2] ?? 0, (d[3] ?? 255) / 255];
    }
    return null;
  }
  const fn = /^rgba?\(\s*([\d.]+)[\s,]+([\d.]+)[\s,]+([\d.]+)\s*(?:[,/]\s*([\d.]+%?)\s*)?\)$/.exec(c);
  if (fn) {
    const alphaText = fn[4];
    const alpha = alphaText === undefined ? 1 : alphaText.endsWith("%") ? Number.parseFloat(alphaText) / 100 : Number.parseFloat(alphaText);
    return [Number(fn[1]), Number(fn[2]), Number(fn[3]), Math.min(1, Math.max(0, alpha))];
  }
  return null;
}

const hex2 = (n: number) => Math.round(Math.min(255, Math.max(0, n))).toString(16).padStart(2, "0");

/**
 * `color` (a token: `#rgb`, `#rrggbbaa`, `rgb()`…) laid over `base`, as
 * `#rrggbb`; null when either cannot be read.
 */
export function opaqueHex(color: string, base: string): string | null {
  const top = parseColor(color);
  const under = parseColor(base);
  if (!top || !under) return null;
  const [r, g, b, a] = top;
  const mix = (t: number, u: number) => t * a + u * (1 - a);
  return `#${hex2(mix(r, under[0]))}${hex2(mix(g, under[1]))}${hex2(mix(b, under[2]))}`;
}

/** Highlight colours read from the theme tokens, as opaque `#rrggbb`. */
export interface SearchColors {
  /** `--search-match`: every match in view. */
  match: string;
  /** `--search-current`: the current match (the accent). */
  current: string;
  /** `--text-primary`: text on a match. */
  text: string;
  /** `--text-on-accent`: text on the current match. */
  onAccent: string;
}

/**
 * Colours of one match's decoration. The foreground is forced too: the
 * remote's own colour could be unreadable on the tint, and light text on
 * the accent would be (as in the TUI).
 */
export function decorationColors(isCurrent: boolean, colors: SearchColors): { backgroundColor: string; foregroundColor: string } {
  return isCurrent
    ? { backgroundColor: colors.current, foregroundColor: colors.onAccent }
    : { backgroundColor: colors.match, foregroundColor: colors.text };
}

// ── The buffer, as xterm's public API shows it ────────────────────────────

export interface CellLike {
  getWidth(): number;
  getChars(): string;
  getCode(): number;
}
export interface LineLike {
  readonly isWrapped: boolean;
  readonly length: number;
  translateToString(trimRight?: boolean, startColumn?: number, endColumn?: number): string;
  getCell(x: number): CellLike | undefined;
}
export interface BufferLike {
  readonly length: number;
  getLine(y: number): LineLike | undefined;
}

/** A buffer position (0-based row and cell). */
export interface Pos {
  row: number;
  col: number;
}

/** Part of a match on one row (cells `col` … `col + width - 1`). */
export interface Span {
  row: number;
  col: number;
  width: number;
  /** Where the match starts (its identity). */
  start: Pos;
}

interface Logical {
  /** First row of the logical line. */
  start: number;
  /** Rows it covers. */
  rows: number;
  text: string;
  lower: string;
  /** String offset where each row starts. */
  offsets: number[];
}

/** Text of logical lines, kept until the buffer changes (`clear`). */
export class LineTexts {
  #byStart = new Map<number, Logical>();
  #buffer: () => BufferLike;

  constructor(buffer: () => BufferLike) {
    this.#buffer = buffer;
  }

  clear() {
    this.#byStart.clear();
  }

  get buffer(): BufferLike {
    return this.#buffer();
  }

  /**
   * Reads up to `rows` rows going up from `row` (a warm-up in slices, so
   * the first keystroke does not pay for the whole scrollback); returns the
   * row to continue from, -1 when done.
   */
  warm(row: number, rows: number): number {
    let y = Math.min(row, this.#buffer().length - 1);
    const stop = y - rows;
    while (y >= 0 && y > stop) {
      const line = this.at(y);
      if (!line) return -1;
      y = line.start - 1;
    }
    return y;
  }

  /** The logical line that contains `row`. */
  at(row: number): Logical | null {
    const buf = this.#buffer();
    let start = row;
    while (start > 0 && buf.getLine(start)?.isWrapped) start--;
    const known = this.#byStart.get(start);
    if (known) return known;
    if (!buf.getLine(start)) return null;
    // Wrapped rows untrimmed, the last one trimmed; a wide character
    // pushed to the next row leaves no blank (as xterm's own search).
    const parts: string[] = [];
    const offsets = [0];
    for (let y = start; ; y++) {
      const line = buf.getLine(y);
      if (!line) break;
      const next = buf.getLine(y + 1);
      const wraps = !!next?.isWrapped;
      let part = line.translateToString(!wraps);
      if (wraps && next) {
        const last = line.getCell(line.length - 1);
        if (last && last.getCode() === 0 && last.getWidth() === 1 && next.getCell(0)?.getWidth() === 2) part = part.slice(0, -1);
      }
      parts.push(part);
      if (!wraps) break;
      offsets.push((offsets[offsets.length - 1] ?? 0) + part.length);
    }
    const text = parts.join("");
    const logical = { start, rows: offsets.length, text, lower: text.toLowerCase(), offsets };
    this.#byStart.set(start, logical);
    return logical;
  }
}

/** Buffer position of string offset `index` in a logical line. */
function positionOf(buf: BufferLike, line: Logical, index: number): Pos {
  let k = 0;
  while (k + 1 < line.offsets.length && index >= (line.offsets[k + 1] ?? Infinity)) k++;
  const row = line.start + k;
  let rest = index - (line.offsets[k] ?? 0);
  const cells = buf.getLine(row);
  if (!cells) return { row, col: rest };
  for (let x = 0; x < cells.length; x++) {
    const cell = cells.getCell(x);
    if (!cell) break;
    if (!cell.getWidth()) continue;
    if (rest <= 0) return { row, col: x };
    rest -= cell.getCode() === 0 ? 1 : cell.getChars().length;
  }
  return { row, col: cells.length };
}

const compare = (a: Pos, b: Pos) => a.row - b.row || a.col - b.col;

interface Needle {
  text: string;
  caseSensitive: boolean;
}

function needleOf(query: string): Needle {
  const caseSensitive = isCaseSensitive(query);
  return { text: caseSensitive ? query : query.toLowerCase(), caseSensitive };
}

const hayOf = (line: Logical, n: Needle) => (n.caseSensitive ? line.text : line.lower);

/** Every occurrence (overlapping ones too) in a logical line, in order. */
function occurrences(line: Logical, n: Needle): number[] {
  const hay = hayOf(line, n);
  const out: number[] = [];
  for (let at = hay.indexOf(n.text); at >= 0; at = hay.indexOf(n.text, at + 1)) out.push(at);
  return out;
}

/** The last match starting before `limit` (at it too when `inclusive`), scanning up. */
function lastBefore(texts: LineTexts, n: Needle, limit: Pos, inclusive: boolean): Pos | null {
  const buf = texts.buffer;
  let row = Math.min(limit.row, buf.length - 1);
  while (row >= 0) {
    const line = texts.at(row);
    if (!line) return null;
    const hay = hayOf(line, n);
    for (let at = hay.lastIndexOf(n.text); at >= 0; at = at > 0 ? hay.lastIndexOf(n.text, at - 1) : -1) {
      const pos = positionOf(buf, line, at);
      const c = compare(pos, limit);
      if (c < 0 || (inclusive && c === 0)) return pos;
    }
    row = line.start - 1;
  }
  return null;
}

/** The first match starting after `limit` (at it too when `inclusive`), scanning down. */
function firstAfter(texts: LineTexts, n: Needle, limit: Pos, inclusive: boolean): Pos | null {
  const buf = texts.buffer;
  let row = Math.max(limit.row, 0);
  while (row < buf.length) {
    const line = texts.at(row);
    if (!line) return null;
    for (const at of occurrences(line, n)) {
      const pos = positionOf(buf, line, at);
      const c = compare(pos, limit);
      if (c > 0 || (inclusive && c === 0)) return pos;
    }
    row = line.start + line.rows;
  }
  return null;
}

const TOP: Pos = { row: 0, col: -1 };
const bottomOf = (buf: BufferLike): Pos => ({ row: buf.length - 1, col: Number.MAX_SAFE_INTEGER });

/**
 * The next match going up: the last one before `from` (at it too when
 * `inclusive`), else, wrapping, the bottom-most one; null without any.
 */
export function matchAbove(texts: LineTexts, query: string, from: Pos, inclusive: boolean): Pos | null {
  if (!query) return null;
  const n = needleOf(query);
  return lastBefore(texts, n, from, inclusive) ?? lastBefore(texts, n, bottomOf(texts.buffer), true);
}

/** The next match going down, wrapping to the top-most one; null without any. */
export function matchBelow(texts: LineTexts, query: string, from: Pos): Pos | null {
  if (!query) return null;
  const n = needleOf(query);
  return firstAfter(texts, n, from, false) ?? firstAfter(texts, n, TOP, false);
}

/**
 * Matches counted from the bottom (stops at `MATCH_CAP`), and the rank of
 * the one starting at `current` (1 = bottom-most; 0 when it is not a
 * match or lies beyond the cap).
 */
export function countMatches(texts: LineTexts, query: string, current: Pos | null): { total: number; rank: number } {
  if (!query) return { total: 0, rank: 0 };
  const n = needleOf(query);
  const buf = texts.buffer;
  let total = 0;
  let rank = 0;
  let row = buf.length - 1;
  while (row >= 0 && total < MATCH_CAP) {
    const line = texts.at(row);
    if (!line) break;
    const found = occurrences(line, n);
    const holdsCurrent = current !== null && current.row >= line.start && current.row < line.start + line.rows;
    for (let i = found.length - 1; i >= 0 && total < MATCH_CAP; i--) {
      total++;
      if (holdsCurrent && rank === 0 && current && compare(positionOf(buf, line, found[i] ?? 0), current) === 0) rank = total;
    }
    row = line.start - 1;
  }
  return { total, rank };
}

/** The cells of the match at string offset `at`, one span per row. */
function spansOf(buf: BufferLike, line: Logical, at: number, length: number): Span[] {
  const start = positionOf(buf, line, at);
  const end = positionOf(buf, line, at + length);
  const spans: Span[] = [];
  for (let row = start.row; row <= end.row; row++) {
    const from = row === start.row ? start.col : 0;
    const to = row === end.row ? end.col : (buf.getLine(row)?.length ?? from);
    if (to > from) spans.push({ row, col: from, width: to - from, start });
  }
  return spans;
}

/** Every match touching rows `top` … `bottom` (the view), clipped to it. */
export function visibleSpans(texts: LineTexts, query: string, top: number, bottom: number): Span[] {
  if (!query) return [];
  const n = needleOf(query);
  const buf = texts.buffer;
  const out: Span[] = [];
  let row = Math.max(top, 0);
  const last = Math.min(bottom, buf.length - 1);
  while (row <= last) {
    const line = texts.at(row);
    if (!line) break;
    for (const at of occurrences(line, n)) {
      for (const span of spansOf(buf, line, at, n.text.length)) {
        if (span.row >= top && span.row <= bottom) out.push(span);
      }
    }
    row = line.start + line.rows;
  }
  return out;
}
