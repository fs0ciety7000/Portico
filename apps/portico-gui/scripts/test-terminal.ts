// Checks of the terminal comfort helpers (phase 9): scrollback search
// (src/lib/termsearch.ts) and links (src/lib/links.ts). Run by `pnpm check`.
import assert from "node:assert/strict";
import { test } from "node:test";
import {
  countMatches,
  counter,
  isCaseSensitive,
  LineTexts,
  MATCH_CAP,
  matchAbove,
  matchBelow,
  opaqueHex,
  visibleSpans,
  type BufferLike,
  type LineLike,
} from "../src/lib/termsearch.ts";
import { caution, checkLink, isOpenGesture, MAX_LINK_CHARS, openHint, splitAtHost, textMisleads } from "../src/lib/links.ts";

test("search: smart case", () => {
  assert.equal(isCaseSensitive("error"), false);
  assert.equal(isCaseSensitive("Error"), true);
  assert.equal(isCaseSensitive("ÉTÉ"), true);
  assert.equal(isCaseSensitive("été 42 .*[$"), false);
});

test("search: counter from the bottom, no match, 999+", () => {
  assert.deepEqual(counter("", 1, 5), { text: "", none: false });
  assert.deepEqual(counter("x", 0, 0), { text: "No match", none: true });
  assert.deepEqual(counter("x", 3, 17), { text: "3 of 17", none: false });
  assert.equal(counter("x", 1, 17).text, "1 of 17");
  assert.equal(counter("x", 999, 999).text, "999 of 999");
  assert.equal(counter("x", 3, MATCH_CAP).text, "999+");
  assert.equal(counter("x", 0, MATCH_CAP).text, "999+");
  assert.equal(counter("x", 0, 4).text, "4 matches");
  assert.equal(counter("x", 0, 1).text, "1 match");
});

test("search: token colours become opaque #rrggbb", () => {
  assert.equal(opaqueHex("#c8a96b", "#0b0c0e"), "#c8a96b");
  assert.equal(opaqueHex("#ffffff80", "#000000"), "#808080");
  assert.equal(opaqueHex("#fff8", "#000"), "#888888");
  assert.equal(opaqueHex("rgba(255, 0, 0, 0.5)", "#000000"), "#800000");
  assert.equal(opaqueHex("rgb(255 255 255 / 25%)", "#000000"), "#404040");
  assert.equal(opaqueHex("color-mix(in srgb, red 20%, transparent)", "#000"), null);
  assert.equal(opaqueHex("#c8a96b", ""), null);
});

test("links: http(s) only, no credentials, length capped", () => {
  const ok = checkLink("https://Example.com/docs?q=1#top");
  assert.deepEqual(ok, { ok: true, href: "https://example.com/docs?q=1#top", host: "example.com" });
  assert.deepEqual(checkLink("http://10.0.0.2:8080"), { ok: true, href: "http://10.0.0.2:8080/", host: "10.0.0.2" });
  // An internationalized host is shown (and opened) in punycode.
  assert.equal(checkLink("https://bücher.example/").ok && (checkLink("https://bücher.example/") as { host: string }).host, "xn--bcher-kva.example");
  for (const raw of [
    "file:///etc/passwd",
    "ssh://host",
    "javascript:alert(1)",
    "data:text/html,x",
    "mailto:a@b.c",
    "portico://open",
    "https://user:pass@example.com/",
    "https://user@example.com/",
    "https://exa mple.com/",
    "https://example.com/\nx",
    "https://example.com/\u202e",
    "example.com",
    "",
  ]) {
    assert.deepEqual(checkLink(raw), { ok: false, reason: "refused" }, raw);
  }
  const base = "https://example.com/";
  assert.equal(checkLink(base + "a".repeat(MAX_LINK_CHARS - base.length)).ok, true);
  assert.deepEqual(checkLink(base + "a".repeat(MAX_LINK_CHARS - base.length + 1)), { ok: false, reason: "too_long" });
});

test("links: the host is emphasised, a misleading text is called out", () => {
  assert.deepEqual(splitAtHost("https://example.com:8443/a?b", "example.com"), ["https://", "example.com", ":8443/a?b"]);
  assert.deepEqual(splitAtHost("http://[::1]/", "[::1]"), ["http://", "[::1]", "/"]);
  const href = "https://evil.example/login";
  assert.equal(textMisleads(null, href, "evil.example"), false);
  assert.equal(textMisleads("", href, "evil.example"), false);
  assert.equal(textMisleads(href, href, "evil.example"), false);
  assert.equal(textMisleads("evil.example", href, "evil.example"), false);
  assert.equal(textMisleads("https://EVIL.example/other", href, "evil.example"), false);
  assert.equal(textMisleads("https://bank.example/login", href, "evil.example"), true);
  assert.equal(textMisleads("bank.example", href, "evil.example"), true);
  assert.equal(textMisleads("click here", href, "evil.example"), true);
  assert.equal(
    caution("bank.example\u202e\n", "evil.example"),
    "The text shown was “bank.example” but the link goes to evil.example.",
  );
});

test("links: ⌘-click on macOS, Ctrl-click elsewhere, nothing else", () => {
  const ev = (m: Partial<{ metaKey: boolean; ctrlKey: boolean; altKey: boolean; shiftKey: boolean }>) => ({
    metaKey: false,
    ctrlKey: false,
    altKey: false,
    shiftKey: false,
    ...m,
  });
  assert.equal(isOpenGesture(ev({ metaKey: true }), true), true);
  assert.equal(isOpenGesture(ev({ ctrlKey: true }), true), false);
  assert.equal(isOpenGesture(ev({ ctrlKey: true }), false), true);
  assert.equal(isOpenGesture(ev({ metaKey: true }), false), false);
  assert.equal(isOpenGesture(ev({}), false), false);
  assert.equal(isOpenGesture(ev({ ctrlKey: true, shiftKey: true }), false), false);
  assert.equal(openHint(true), "⌘-click to open");
  assert.equal(openHint(false), "Ctrl-click to open");
});

/** A fake xterm buffer: one entry per row, `\u0001` prefix = wrapped from the row above; `W` = a wide char (2 cells). */
function buffer(rows: string[], cols = 20): BufferLike & { text: string[] } {
  const lines: LineLike[] = rows.map((raw) => {
    const isWrapped = raw.startsWith("\u0001");
    const body = isWrapped ? raw.slice(1) : raw;
    // Cells: a wide char "界" takes two cells (the second has width 0).
    const cells: { ch: string; w: number }[] = [];
    for (const ch of body) {
      if (ch === "界") cells.push({ ch, w: 2 }, { ch: "", w: 0 });
      else cells.push({ ch, w: 1 });
    }
    while (cells.length < cols) cells.push({ ch: "", w: 1 });
    return {
      isWrapped,
      length: cols,
      translateToString(trimRight?: boolean) {
        const t = cells.filter((c) => c.w > 0).map((c) => c.ch || " ").join("");
        return trimRight ? t.replace(/ +$/, "") : t;
      },
      getCell(x: number) {
        const c = cells[x];
        return c && { getWidth: () => c.w, getChars: () => c.ch, getCode: () => (c.ch ? c.ch.codePointAt(0)! : 0) };
      },
    };
  });
  return { length: lines.length, getLine: (y: number) => lines[y], text: rows };
}

test("search going up: nearest older match, wrap, smart case, literal", () => {
  const buf = buffer(["error one", "ok", "ERROR two", "a.*[b", "x error y error", "last"]);
  const texts = new LineTexts(() => buf);
  const bottom = { row: 5, col: 99 };
  // Typing: the nearest match going up from the bottom.
  assert.deepEqual(matchAbove(texts, "error", bottom, true), { row: 4, col: 10 });
  // Enter: strictly before the current one, then the line above…
  assert.deepEqual(matchAbove(texts, "error", { row: 4, col: 10 }, false), { row: 4, col: 2 });
  assert.deepEqual(matchAbove(texts, "error", { row: 4, col: 2 }, false), { row: 2, col: 0 });
  assert.deepEqual(matchAbove(texts, "error", { row: 2, col: 0 }, false), { row: 0, col: 0 });
  // …and wraps around to the bottom-most one.
  assert.deepEqual(matchAbove(texts, "error", { row: 0, col: 0 }, false), { row: 4, col: 10 });
  // An uppercase letter makes it case-sensitive.
  assert.deepEqual(matchAbove(texts, "ERROR", bottom, true), { row: 2, col: 0 });
  // Literal text: regex characters match themselves.
  assert.deepEqual(matchAbove(texts, ".*[", bottom, true), { row: 3, col: 1 });
  assert.equal(matchAbove(texts, "e.r", bottom, true), null);
  assert.equal(matchAbove(texts, "zzz", bottom, true), null);
  assert.equal(matchAbove(texts, "", bottom, true), null);
  // Inclusive: the current match stays while the query grows.
  assert.deepEqual(matchAbove(texts, "error y", { row: 4, col: 2 }, true), { row: 4, col: 2 });
});

test("search going up: wrapped rows and wide characters", () => {
  // "abcdefghij" + "klmno needle" wrapped at 10 columns.
  const buf = buffer(["abcdefghij", "\u0001klmno needle", "界界 needle"], 10);
  const texts = new LineTexts(() => buf);
  // A match across the wrap ("jk") starts on the first row.
  assert.deepEqual(matchAbove(texts, "jk", { row: 2, col: 99 }, true), { row: 0, col: 9 });
  assert.deepEqual(matchAbove(texts, "needle", { row: 1, col: 99 }, true), { row: 1, col: 6 });
  // After two wide characters (4 cells), the match starts at cell 5.
  assert.deepEqual(matchAbove(texts, "needle", { row: 2, col: 99 }, true), { row: 2, col: 5 });
  texts.clear();
  assert.deepEqual(matchAbove(texts, "needle", { row: 2, col: 5 }, false), { row: 1, col: 6 });
});

test("search going down, counting, extents and visible spans", () => {
  const buf = buffer(["error one", "ok", "ERROR two", "a.*[b", "x error y error", "last"]);
  const texts = new LineTexts(() => buf);
  // Shift-Enter: the next newer one, wrapping to the top-most.
  assert.deepEqual(matchBelow(texts, "error", { row: 0, col: 0 }), { row: 2, col: 0 });
  assert.deepEqual(matchBelow(texts, "error", { row: 4, col: 2 }), { row: 4, col: 10 });
  assert.deepEqual(matchBelow(texts, "error", { row: 4, col: 10 }), { row: 0, col: 0 });
  assert.equal(matchBelow(texts, "zzz", { row: 0, col: 0 }), null);
  // Counted from the bottom: the most recent is 1.
  assert.deepEqual(countMatches(texts, "error", { row: 4, col: 10 }), { total: 4, rank: 1 });
  assert.deepEqual(countMatches(texts, "error", { row: 2, col: 0 }), { total: 4, rank: 3 });
  assert.deepEqual(countMatches(texts, "error", { row: 1, col: 0 }), { total: 4, rank: 0 });
  assert.deepEqual(countMatches(texts, "Error", null), { total: 0, rank: 0 });
  assert.deepEqual(countMatches(texts, "", null), { total: 0, rank: 0 });
  // Highlights, clipped to the view.
  assert.deepEqual(
    visibleSpans(texts, "error", 2, 4).map((s) => [s.row, s.col, s.width]),
    [
      [2, 0, 5],
      [4, 2, 5],
      [4, 10, 5],
    ],
  );
});

test("search: a full scrollback stops counting at the cap", () => {
  const rows = Array.from({ length: 10_000 }, (_, i) => `${String(i).padStart(5, "0")} GET /api/orders?page=${i} ${i % 97 === 0 ? "500 Error" : "200 OK"}`);
  const buf = buffer(rows, 60);
  const texts = new LineTexts(() => buf);
  assert.deepEqual(countMatches(texts, "GET", null).total, MATCH_CAP);
  const bottom = { row: 9_999, col: Number.MAX_SAFE_INTEGER };
  const first = matchAbove(texts, "500 error", bottom, true);
  assert.deepEqual(first, { row: 9_991, col: 32 });
  assert.deepEqual(countMatches(texts, "500 error", first), { total: 104, rank: 1 });
  // A wrap from the top-most one goes back to the bottom-most.
  assert.deepEqual(matchAbove(texts, "500 error", { row: 0, col: 0 }, false), first);
  // The warm-up reads everything in slices, from the bottom.
  const cold = new LineTexts(() => buf);
  let next = cold.warm(Number.MAX_SAFE_INTEGER, 4_000);
  assert.equal(next, 5_999);
  next = cold.warm(next, 4_000);
  assert.equal(cold.warm(next, 4_000), -1);
  const t0 = performance.now();
  for (const q of ["5", "50", "500", "zzz", "q"]) {
    countMatches(texts, q, matchAbove(texts, q, bottom, true));
    visibleSpans(texts, q, 9_960, 9_999);
  }
  // Generous bound (CI machines vary): the point is "no full cell walk".
  assert.ok(performance.now() - t0 < 500);
});
