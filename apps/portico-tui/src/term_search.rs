//! Scrollback search in a terminal pane (phase 9, `Ctrl-Space /`).
//!
//! The query is literal text: it is escaped before reaching alacritty's
//! [`RegexSearch`], which applies smart case (case-insensitive unless the
//! query has an uppercase letter). Matches are `alacritty_terminal` grid
//! points; the search runs over the whole grid (screen + scrollback) on the
//! UI loop, bounded by the terminal's scrollback (measured: see
//! `docs/STACK.md`, decision log).
//!
//! Invariants:
//! - while a search is open, output never moves the view: the reader task
//!   holds it ([`hold_before`] / [`hold_after`]), so the display offset
//!   grows by exactly the lines that scrolled in, and [`ScrollSearch`]
//!   shifts its points by that much on the next refresh;
//! - the last row of the pane belongs to the search bar: a match is only
//!   "shown" above it (when the current match can only be on the grid's
//!   bottom line, the view is drawn one row higher, see
//!   [`ScrollSearch::lift`]).

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::term::Term;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::search::{Match, RegexIter, RegexSearch};

/// Beyond this many matches the counter reads `999+`.
pub(crate) const MAX_COUNTED: usize = 999;
/// Longest query kept (characters): bounds the regex size.
pub(crate) const MAX_QUERY: usize = 256;

/// The query as a regex that matches it literally.
pub(crate) fn escape_literal(query: &str) -> String {
    let mut out = String::with_capacity(query.len() * 2);
    for c in query.chars() {
        // regex-syntax meta characters (all may be escaped).
        if matches!(
            c,
            '\\' | '.'
                | '+'
                | '*'
                | '?'
                | '('
                | ')'
                | '|'
                | '['
                | ']'
                | '{'
                | '}'
                | '^'
                | '$'
                | '#'
                | '&'
                | '-'
                | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// How many matches the scrollback holds, and which one is current.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Count {
    /// `index` counts from the bottom, the way `↵` walks: the most recent
    /// match is 1 (`None`: no current match). Same order as the GUI.
    Exact { index: Option<usize>, total: usize },
    /// More than [`MAX_COUNTED`].
    Many,
}

/// The counter text, and whether it is a warning (`No match`). `None`
/// while the query is empty.
pub(crate) fn counter_text(count: Option<Count>) -> Option<(String, bool)> {
    Some(match count? {
        Count::Exact { total: 0, .. } => ("No match".to_owned(), true),
        Count::Exact {
            index: Some(index),
            total,
        } => (format!("{index} of {total}"), false),
        Count::Exact { index: None, total } => (
            format!("{total} {}", if total == 1 { "match" } else { "matches" }),
            false,
        ),
        Count::Many => (format!("{MAX_COUNTED}+"), false),
    })
}

/// Search state of one pane, while its bar is open.
#[derive(Debug, Clone)]
pub(crate) struct ScrollSearch {
    pub(crate) query: String,
    /// Boxed: the lazy DFAs are large, and a pane is moved around.
    regex: Option<Box<RegexSearch>>,
    /// Where incremental search starts (bottom-right of the shown area
    /// when the bar opened), in grid coordinates.
    anchor: Point,
    pub(crate) current: Option<Match>,
    /// Every match, top to bottom, while there are at most
    /// [`MAX_COUNTED`] (`None`: more, or no query).
    all: Option<Vec<Match>>,
    /// Matches in the viewport, for highlighting.
    pub(crate) visible: Vec<Match>,
    /// `None` while the query is empty.
    pub(crate) count: Option<Count>,
    /// Display offset at the last refresh: a rise is output scrolling in.
    last_offset: usize,
}

impl ScrollSearch {
    /// Opens on the current view.
    pub(crate) fn open<T: EventListener>(term: &Term<T>) -> Self {
        let offset = term.grid().display_offset();
        Self {
            query: String::new(),
            regex: None,
            anchor: shown_bottom_right(term),
            current: None,
            all: None,
            visible: Vec::new(),
            count: None,
            last_offset: offset,
        }
    }

    /// Replaces the query, then jumps to the nearest match going up from
    /// the anchor.
    pub(crate) fn set_query<T: EventListener>(&mut self, term: &mut Term<T>, query: &str) {
        self.follow_output(term);
        self.query = query.chars().take(MAX_QUERY).collect();
        self.regex = if self.query.is_empty() {
            None
        } else {
            // A pattern too large for the DFA simply finds nothing.
            RegexSearch::new(&escape_literal(&self.query))
                .ok()
                .map(Box::new)
        };
        self.current = None;
        self.scan(term);
        let anchor = self.anchor;
        self.jump(term, anchor, Direction::Left);
    }

    /// Next older (`up`) or newer match, wrapping around.
    pub(crate) fn step<T: EventListener>(&mut self, term: &mut Term<T>, up: bool) {
        self.follow_output(term);
        let origin = match (&self.current, up) {
            (Some(m), true) => m.start().sub(term, Boundary::None, 1),
            (Some(m), false) => m.end().add(term, Boundary::None, 1),
            (None, _) => self.anchor,
        };
        let direction = if up {
            Direction::Left
        } else {
            Direction::Right
        };
        self.jump(term, origin, direction);
    }

    /// After output or a resize: follows the shift, rescans, drops a
    /// current match that no longer reads the same. Never scrolls.
    pub(crate) fn refresh<T: EventListener>(&mut self, term: &Term<T>) {
        self.follow_output(term);
        self.scan(term);
        let current = self.current.take();
        self.current = match (&self.all, self.regex.as_mut(), current) {
            (_, _, None) | (_, None, _) => None,
            (Some(all), _, Some(current)) => all.contains(&current).then_some(current),
            (None, Some(regex), Some(current)) => {
                let in_grid = current.start().line >= term.topmost_line()
                    && current.end().line <= term.bottommost_line();
                let found = in_grid
                    .then(|| term.regex_search_right(regex, *current.start(), *current.end()))
                    .flatten();
                found.filter(|m| *m == current)
            }
        };
        self.recount(term);
    }

    /// The grid was resized: lines may have reflowed and alacritty is
    /// back on the live view. The anchor is the bottom of the shown area
    /// again; the current match is found again by its rank (the same
    /// occurrence, top to bottom), else the nearest going up, and shown.
    pub(crate) fn resized<T: EventListener>(&mut self, term: &mut Term<T>) {
        let rank = match self.count {
            Some(Count::Exact { index, .. }) => index,
            _ => None,
        };
        self.anchor = shown_bottom_right(term);
        self.last_offset = term.grid().display_offset();
        self.scan(term);
        let again = rank.and_then(|rank| {
            let all = self.all.as_ref()?;
            all.get(all.len().checked_sub(rank)?).cloned()
        });
        match again {
            Some(m) => {
                reveal(term, &m);
                self.current = Some(m);
                self.last_offset = term.grid().display_offset();
                self.recount(term);
            }
            None if self.current.is_some() => {
                let anchor = self.anchor;
                self.jump(term, anchor, Direction::Left);
            }
            None => self.recount(term),
        }
    }

    /// Rows to draw the view higher by: 1 when the current match can only
    /// be on the pane's last row (the live view's bottom line, under the
    /// bar), so it shows just above the bar and the top row goes; else 0.
    pub(crate) fn lift<T>(&self, term: &Term<T>) -> u16 {
        let offset = i32::try_from(term.grid().display_offset()).unwrap_or(i32::MAX);
        let last = i32::try_from(term.screen_lines()).unwrap_or(1) - 1;
        let under_bar = self
            .current
            .as_ref()
            .is_some_and(|m| m.end().line.0.saturating_add(offset) >= last && last > 0);
        u16::from(under_bar)
    }

    fn jump<T: EventListener>(&mut self, term: &mut Term<T>, origin: Point, direction: Direction) {
        let found = match (&self.all, self.regex.as_mut()) {
            (_, None) => None,
            // Up to 999 matches, all known: no scan at all.
            (Some(all), Some(_)) => pick(all, origin, direction),
            // Dense matches: the next one is near.
            (None, Some(regex)) => term.search_next(regex, origin, direction, Side::Left, None),
        };
        if let Some(m) = &found {
            reveal(term, m);
        }
        self.current = found;
        self.last_offset = term.grid().display_offset();
        self.recount(term);
    }

    /// Output scrolled `n` lines in: every point moves `n` lines up.
    fn follow_output<T>(&mut self, term: &Term<T>) {
        let offset = term.grid().display_offset();
        let shift = offset.saturating_sub(self.last_offset);
        self.last_offset = offset;
        if shift == 0 {
            return;
        }
        let shift = i32::try_from(shift).unwrap_or(i32::MAX);
        let up = |p: Point| Point::new(Line(p.line.0.saturating_sub(shift)), p.column);
        let top = term.topmost_line();
        let moved =
            |m: &Match| Some(up(*m.start())..=up(*m.end())).filter(|m| m.start().line >= top);
        self.anchor = up(self.anchor);
        if self.anchor.line < top {
            self.anchor = Point::new(top, Column(0));
        }
        self.current = self.current.take().and_then(|m| moved(&m));
        if let Some(all) = &mut self.all {
            *all = all.iter().filter_map(moved).collect();
        }
    }

    /// Every match of the grid, top to bottom, up to [`MAX_COUNTED`]
    /// (`all` is `None` beyond). The lines that contain the query's
    /// longest ASCII run are kept first (a plain substring test, much
    /// cheaper than the DFA); the DFA then runs on those lines alone.
    fn scan<T>(&mut self, term: &Term<T>) {
        self.all = None;
        let Some(regex) = self.regex.as_mut() else {
            return;
        };
        let last = term.last_column();
        let run = longest_ascii_run(&self.query);
        let blocks = if run.is_empty() {
            vec![(term.topmost_line(), term.bottommost_line())]
        } else {
            candidate_lines(term, run, !self.query.chars().any(char::is_uppercase))
        };
        let mut all = Vec::new();
        for (first, end) in blocks {
            let (start, end) = (Point::new(first, Column(0)), Point::new(end, last));
            for m in RegexIter::new(start, end, Direction::Right, term, regex) {
                if all.len() == MAX_COUNTED {
                    return;
                }
                all.push(m);
            }
        }
        self.all = Some(all);
    }

    /// Visible matches and the counter, from the last scan.
    fn recount<T>(&mut self, term: &Term<T>) {
        self.visible.clear();
        let Some(regex) = self.regex.as_mut() else {
            self.count = None;
            return;
        };
        let offset = i32::try_from(term.grid().display_offset()).unwrap_or(0);
        let rows = i32::try_from(term.screen_lines()).unwrap_or(1);
        let (top, bottom) = (Line(-offset), Line(rows - 1 - offset));
        if let Some(all) = &self.all {
            self.visible.extend(
                all.iter()
                    .filter(|m| m.end().line >= top && m.start().line <= bottom)
                    .cloned(),
            );
            let index = self
                .current
                .as_ref()
                .and_then(|c| all.iter().rposition(|m| m == c))
                .map(|i| all.len() - i);
            self.count = Some(Count::Exact {
                index,
                total: all.len(),
            });
        } else {
            let (start, end) = (
                Point::new(top, Column(0)),
                Point::new(bottom, term.last_column()),
            );
            self.visible
                .extend(RegexIter::new(start, end, Direction::Right, term, regex));
            self.count = Some(Count::Many);
        }
    }
}

/// The match `search_next` would find among `all` (top to bottom): going
/// left, the last one starting at or before `origin`; going right, the
/// first one starting at or after it; wrapping around.
fn pick(all: &[Match], origin: Point, direction: Direction) -> Option<Match> {
    match direction {
        Direction::Left => all
            .iter()
            .rev()
            .find(|m| *m.start() <= origin)
            .or_else(|| all.last()),
        Direction::Right => all
            .iter()
            .find(|m| *m.start() >= origin)
            .or_else(|| all.first()),
    }
    .cloned()
}

/// The longest run of ASCII characters in `query` (empty: none). Every
/// match of the query contains it.
fn longest_ascii_run(query: &str) -> &str {
    query
        .split(|c: char| !c.is_ascii())
        .max_by_key(|run| run.len())
        .unwrap_or_default()
}

/// Lines of the grid (wrapped rows joined, as the DFA reads them: matches
/// never cross a hard line break) that contain the ASCII `query`, as
/// `(first row, last row)`. `insensitive` is the whole query's smart case
/// (alacritty's: insensitive unless it has an uppercase letter); then the
/// only non-ASCII characters that fold onto ASCII letters (`ſ`, the Kelvin
/// sign) are folded too, so no line with a match is left out.
fn candidate_lines<T>(term: &Term<T>, query: &str, insensitive: bool) -> Vec<(Line, Line)> {
    let needle = if insensitive {
        query.to_ascii_lowercase()
    } else {
        query.to_owned()
    };
    let fold = |c: char| match c {
        _ if !insensitive => c,
        '\u{17f}' => 's',
        '\u{212a}' => 'k',
        _ => c.to_ascii_lowercase(),
    };
    let grid = term.grid();
    let (top, bottom) = (term.topmost_line().0, term.bottommost_line().0);
    let mut blocks = Vec::new();
    let mut text = String::new();
    let mut first = top;
    for line in top..=bottom {
        // `line` is within topmost..=bottommost: always a row of the grid.
        #[allow(clippy::indexing_slicing)]
        let row = &grid[Line(line)][..];
        text.extend(
            row.iter()
                .filter(|cell| {
                    !cell
                        .flags
                        .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                })
                .map(|cell| fold(cell.c)),
        );
        let wrapped = row
            .last()
            .is_some_and(|cell| cell.flags.contains(Flags::WRAPLINE));
        if !wrapped || line == bottom {
            if text.contains(needle.as_str()) {
                blocks.push((Line(first), Line(line)));
            }
            text.clear();
            first = line.saturating_add(1);
        }
    }
    blocks
}

/// Rows of the pane above the search bar.
fn shown_rows<T>(term: &Term<T>) -> i32 {
    (i32::try_from(term.screen_lines()).unwrap_or(1) - 1).max(1)
}

/// Bottom-right cell of the area above the bar, in grid coordinates.
fn shown_bottom_right<T>(term: &Term<T>) -> Point {
    let offset = i32::try_from(term.grid().display_offset()).unwrap_or(0);
    Point::new(Line(shown_rows(term) - 1 - offset), term.last_column())
}

/// Scrolls so `m` is shown above the bar: no move when it already is,
/// else centred (as far as the scrollback allows).
fn reveal<T: EventListener>(term: &mut Term<T>, m: &Match) {
    let offset = i32::try_from(term.grid().display_offset()).unwrap_or(0);
    let rows = shown_rows(term);
    let (top, bottom) = (m.start().line.0, m.end().line.0);
    if top + offset >= 0 && bottom + offset < rows {
        return;
    }
    let history = i32::try_from(term.history_size()).unwrap_or(i32::MAX);
    let target = (rows / 2 - top).clamp(0, history);
    term.scroll_display(Scroll::Delta(target - offset));
}

/// What [`hold_before`] did to the view, for [`hold_after`].
#[derive(Debug, Clone, Copy)]
pub(crate) enum Hold {
    /// Scrolled back already: alacritty keeps such a view still.
    Still,
    /// Live view lifted one line for the parse.
    Lifted,
    /// Live view, empty scrollback: the history growth is the shift.
    Grow(usize),
}

/// Before parsing output while a search is open. alacritty keeps a
/// scrolled-back view still (its offset rises as lines scroll in) but the
/// live view follows output; lifting it one line for the parse makes the
/// offset rise by exactly the lines that scrolled in.
pub(crate) fn hold_before<T: EventListener>(term: &mut Term<T>) -> Hold {
    if term.grid().display_offset() > 0 {
        Hold::Still
    } else if term.history_size() > 0 {
        term.scroll_display(Scroll::Delta(1));
        Hold::Lifted
    } else {
        Hold::Grow(0)
    }
}

/// After the parse: puts the lifted line back, or follows the growth.
pub(crate) fn hold_after<T: EventListener>(term: &mut Term<T>, hold: Hold) {
    match hold {
        Hold::Still => {}
        Hold::Lifted => term.scroll_display(Scroll::Delta(-1)),
        Hold::Grow(before) => {
            let grown = term.history_size().saturating_sub(before);
            term.scroll_display(Scroll::Delta(i32::try_from(grown).unwrap_or(i32::MAX)));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term::Config;
    use alacritty_terminal::vte::ansi::Processor;

    use std::fmt::Write as _;

    use super::*;
    use crate::terminal::GridSize;

    /// Smart case, as alacritty applies it (`RegexSearch::new`): an
    /// uppercase letter makes the search case-sensitive.
    fn case_sensitive(query: &str) -> bool {
        query.chars().any(char::is_uppercase)
    }

    fn term(cols: u16, rows: u16, scrollback: usize) -> Term<VoidListener> {
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        Term::new(config, &GridSize { cols, rows }, VoidListener)
    }

    fn feed(term: &mut Term<VoidListener>, text: &str) {
        let mut parser: Processor = Processor::new();
        parser.advance(term, text.replace('\n', "\r\n").as_bytes());
    }

    fn query(search: &mut ScrollSearch, term: &mut Term<VoidListener>, q: &str) {
        search.set_query(term, q);
    }

    /// Text of a match.
    fn text(term: &Term<VoidListener>, m: &Match) -> String {
        term.bounds_to_string(*m.start(), *m.end())
    }

    #[test]
    fn ascii_runs() {
        assert_eq!(longest_ascii_run("journal-é"), "journal-");
        assert_eq!(longest_ascii_run("é-ab-ü-abcd"), "-abcd");
        assert_eq!(longest_ascii_run("éü"), "");
    }

    #[test]
    fn literal_escaping() {
        assert_eq!(escape_literal("a.*[b"), r"a\.\*\[b");
        assert_eq!(escape_literal("x-y ~#&"), r"x\-y \~\#\&");
        assert_eq!(escape_literal("é ok"), "é ok");
        for q in [".*[", "(a|b)", "\\d+", "{1}", "^$", "-~#&", "a b"] {
            assert!(RegexSearch::new(&escape_literal(q)).is_ok(), "{q}");
        }

        let mut t = term(40, 6, 100);
        feed(&mut t, "abc\nfoo.*[bar\nfooXXbar\n");
        let mut s = ScrollSearch::open(&t);
        query(&mut s, &mut t, ".*[");
        let current = s.current.clone().unwrap();
        assert_eq!(text(&t, &current), ".*[");
        assert_eq!(
            s.count,
            Some(Count::Exact {
                index: Some(1),
                total: 1
            })
        );
        // `.` is not a wildcard.
        query(&mut s, &mut t, "foo..bar");
        assert_eq!(
            s.count,
            Some(Count::Exact {
                index: None,
                total: 0
            })
        );
        assert_eq!(counter_text(s.count), Some(("No match".into(), true)));
    }

    #[test]
    fn smart_case() {
        assert!(!case_sensitive("error"));
        assert!(case_sensitive("Error"));
        let mut t = term(40, 6, 100);
        feed(&mut t, "Error one\nerror two\nERROR three\n");
        let mut s = ScrollSearch::open(&t);
        query(&mut s, &mut t, "error");
        // The most recent match is current, and is 1.
        assert_eq!(
            s.count,
            Some(Count::Exact {
                index: Some(1),
                total: 3
            })
        );
        query(&mut s, &mut t, "Error");
        assert_eq!(
            s.count,
            Some(Count::Exact {
                index: Some(1),
                total: 1
            })
        );
    }

    #[test]
    fn steps_go_up_then_wrap() {
        let mut t = term(30, 6, 100);
        feed(&mut t, "hit 1\nx\nhit 2\nx\nhit 3\n");
        let mut s = ScrollSearch::open(&t);
        query(&mut s, &mut t, "hit");
        let index = |s: &ScrollSearch| match s.count {
            Some(Count::Exact { index, .. }) => index,
            _ => None,
        };
        // Nearest going up from the bottom: the newest.
        // Numbered from the bottom: the newest is 1.
        assert_eq!(index(&s), Some(1));
        assert_eq!(counter_text(s.count), Some(("1 of 3".into(), false)));
        s.step(&mut t, true);
        assert_eq!(index(&s), Some(2));
        s.step(&mut t, true);
        assert_eq!(index(&s), Some(3));
        s.step(&mut t, true);
        assert_eq!(index(&s), Some(1), "wraps to the newest");
        s.step(&mut t, false);
        assert_eq!(index(&s), Some(3), "and back down to the oldest");
    }

    #[test]
    fn jumps_into_the_scrollback_and_output_does_not_move_it() {
        let mut t = term(30, 6, 1000);
        let mut out = String::from("needle here\n");
        for i in 0..50 {
            let _ = writeln!(out, "line {i}");
        }
        feed(&mut t, &out);
        let mut s = ScrollSearch::open(&t);
        query(&mut s, &mut t, "needle");
        let offset = t.grid().display_offset();
        assert!(offset > 0, "scrolled back to the match");
        let shown = |t: &Term<VoidListener>, s: &ScrollSearch| {
            let m = s.current.clone().unwrap();
            m.start().line.0 + i32::try_from(t.grid().display_offset()).unwrap()
        };
        let row = shown(&t, &s);
        assert!((0..5).contains(&row), "above the bar: {row}");

        // More output: the same text stays on the same row.
        let mut parser: Processor = Processor::new();
        let hold = hold_before(&mut t);
        parser.advance(&mut t, b"more 1\r\nmore 2\r\nmore 3\r\n");
        hold_after(&mut t, hold);
        s.refresh(&t);
        assert_eq!(shown(&t, &s), row);
        assert_eq!(text(&t, &s.current.clone().unwrap()), "needle");
        assert_eq!(
            s.count,
            Some(Count::Exact {
                index: Some(1),
                total: 1
            })
        );
    }

    /// A match on the live view's bottom line (under the bar) cannot be
    /// scrolled into view: the view is drawn one row higher instead.
    #[test]
    fn a_match_under_the_bar_lifts_the_view() {
        let mut t = term(30, 6, 1000);
        feed(&mut t, "a\nb\nc\nd\ne\n$ find me");
        let mut s = ScrollSearch::open(&t);
        query(&mut s, &mut t, "find");
        let current = s.current.clone().unwrap();
        assert_eq!(current.start().line.0, 5, "on the bottom line");
        assert_eq!(t.grid().display_offset(), 0);
        assert_eq!(s.lift(&t), 1);
        // Shown above the bar: no lift.
        let mut above = term(30, 6, 1000);
        feed(&mut above, "a\nb\nfind me\n$ ");
        let mut on_screen = ScrollSearch::open(&above);
        query(&mut on_screen, &mut above, "find");
        assert!(on_screen.current.is_some());
        assert_eq!(on_screen.lift(&above), 0);
        query(&mut s, &mut t, "zzz");
        assert_eq!(s.lift(&t), 0, "no current match");
    }

    #[test]
    fn the_live_view_is_held_too() {
        let mut t = term(30, 6, 1000);
        feed(&mut t, "a\nb\nc\nd\ne\nf\ng\nfind me\n");
        let mut s = ScrollSearch::open(&t);
        query(&mut s, &mut t, "find");
        assert_eq!(t.grid().display_offset(), 0, "on screen: no scroll");
        let before = s.current.clone().unwrap();
        let mut parser: Processor = Processor::new();
        for chunk in ["x1\r\nx2\r\n", "x3\r\n"] {
            let hold = hold_before(&mut t);
            parser.advance(&mut t, chunk.as_bytes());
            hold_after(&mut t, hold);
        }
        s.refresh(&t);
        assert_eq!(t.grid().display_offset(), 3, "held, not followed");
        let after = s.current.clone().unwrap();
        assert_eq!(after.start().line.0, before.start().line.0 - 3);
        assert_eq!(text(&t, &after), "find");
    }

    #[test]
    fn counter_caps_at_999() {
        let mut t = term(80, 24, 2000);
        let out = "x ".repeat(39).trim_end().to_owned() + "\n";
        feed(&mut t, &out.repeat(40));
        let mut s = ScrollSearch::open(&t);
        query(&mut s, &mut t, "x");
        assert_eq!(s.count, Some(Count::Many));
        assert_eq!(counter_text(s.count), Some(("999+".into(), false)));
        query(&mut s, &mut t, "");
        assert_eq!(counter_text(s.count), None, "nothing while empty");
        assert!(s.current.is_none() && s.visible.is_empty());
    }

    #[test]
    fn the_line_filter_keeps_wrapped_and_folded_matches() {
        // 10 columns: `abcdefghij` + `klm` wraps, the match spans the wrap.
        let mut t = term(10, 6, 100);
        feed(&mut t, "abcdefghijklm\nzz\nxxſtop\n");
        let mut s = ScrollSearch::open(&t);
        query(&mut s, &mut t, "ijk");
        let m = s.current.clone().unwrap();
        assert_ne!(m.start().line, m.end().line, "across the wrap");
        assert_eq!(
            s.count,
            Some(Count::Exact {
                index: Some(1),
                total: 1
            })
        );
        // The DFA folds `ſ` onto `s`: the filter must not drop that line.
        query(&mut s, &mut t, "stop");
        assert_eq!(
            s.count,
            Some(Count::Exact {
                index: Some(1),
                total: 1
            })
        );
        // A non-ASCII query skips the filter.
        query(&mut s, &mut t, "ſtop");
        assert_eq!(
            s.count,
            Some(Count::Exact {
                index: Some(1),
                total: 1
            })
        );
    }

    #[test]
    fn dense_matches_still_step_and_wrap() {
        let mut t = term(80, 24, 2000);
        feed(&mut t, &"x x x x x x x x x x\n".repeat(150));
        let mut s = ScrollSearch::open(&t);
        query(&mut s, &mut t, "x");
        assert_eq!(s.count, Some(Count::Many));
        let first = s.current.clone().unwrap();
        s.step(&mut t, true);
        let second = s.current.clone().unwrap();
        assert!(second.start() < first.start());
        s.step(&mut t, false);
        assert_eq!(s.current, Some(first));
        assert!(!s.visible.is_empty());
    }

    #[test]
    fn visible_matches_are_listed_for_highlighting() {
        let mut t = term(30, 6, 100);
        feed(&mut t, "ab ab\nzz\nab\n");
        let mut s = ScrollSearch::open(&t);
        query(&mut s, &mut t, "ab");
        assert_eq!(s.visible.len(), 3);
        assert!(s.visible.contains(s.current.as_ref().unwrap()));
    }

    /// Search over a full scrollback (10 000 lines of 120 columns), to be
    /// read in release: `cargo test --release -p portico-tui
    /// full_scrollback_timing -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing, run by hand in release"]
    #[allow(clippy::print_stdout)] // the report is the point of this test
    fn full_scrollback_timing() {
        let mut t = term(120, 40, crate::terminal::SCROLLBACK);
        let mut out = String::new();
        for i in 0..crate::terminal::SCROLLBACK + 40 {
            let _ = writeln!(
                out,
                "{i:05} drwxr-xr-x  2 deploy deploy 4096 Oct 22 10:{:02} /var/log/service-{i}",
                i % 60
            );
        }
        feed(&mut t, &out);
        assert_eq!(t.history_size(), crate::terminal::SCROLLBACK);
        for q in [
            "needle-not-there",
            "service-42",
            "deploy",
            "Oct",
            "journal-é",
        ] {
            let mut s = ScrollSearch::open(&t);
            let start = Instant::now();
            query(&mut s, &mut t, q);
            let typed = start.elapsed();
            let start = Instant::now();
            s.step(&mut t, true);
            let step = start.elapsed();
            let start = Instant::now();
            s.refresh(&t);
            let refresh = start.elapsed();
            println!(
                "{q:>18}: type {typed:?} · step {step:?} · refresh {refresh:?} · {:?}",
                s.count
            );
        }
    }
}
