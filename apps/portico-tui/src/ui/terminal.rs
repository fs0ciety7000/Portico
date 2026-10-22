//! Renders an `alacritty_terminal` grid into the ratatui buffer, with the
//! scrollback search bar and its highlights when the pane has one open:
//! every visible match on `search_match`, the current one on
//! `search_current` (the accent) with `text_on_accent`.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::TermMode;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::vte::ansi::{Color as TermColor, CursorShape, NamedColor};
use portico_theme::Theme;
use ratatui::Frame;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Widget};

use crate::term_search::{ScrollSearch, counter_text};
use crate::terminal::TerminalView;
use crate::theme::{Styles, c};
use crate::ui::fmt;

/// Draws the terminal. `focused` controls the cursor; `banner` the
/// one-line notice once the shell ended (`close`: the close-pane key).
pub(crate) fn render(
    view: &TerminalView,
    focused: bool,
    banner: bool,
    close: &str,
    s: Styles,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    let term = view
        .term
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let search = view.search.as_ref();
    frame.render_widget(
        TermWidget {
            term: &term,
            theme: s.theme,
            focused,
            skip_rows: 0,
            search,
        },
        area,
    );
    let offset = term.grid().display_offset();
    let bar_on_top = search.is_some_and(|search| search.bar_on_top(&term));
    drop(term);

    if let Some(search) = search {
        let y = if bar_on_top {
            area.y
        } else {
            area.bottom().saturating_sub(1)
        };
        render_search_bar(
            search,
            focused,
            s,
            frame,
            Rect {
                y,
                height: 1,
                ..area
            },
        );
        // The bar replaces the banner and, on top, the scroll badge.
        if bar_on_top {
            return;
        }
    }
    if offset > 0 {
        let mut spans = vec![Span::styled(
            format!(" ↑ {offset} lines  "),
            s.muted().patch(s.chrome()),
        )];
        // While searching, the keys belong to the bar (Esc goes back down).
        if search.is_none() {
            spans.push(Span::styled(" Shift-PgDn ", s.key_hint()));
        }
        let badge = Line::from(spans).right_aligned();
        frame.render_widget(Paragraph::new(badge), Rect { height: 1, ..area });
    }
    if let Some(ended) = view.ended.as_ref().filter(|_| banner && search.is_none()) {
        let text = match ended {
            crate::terminal::Ended::Exited(code) => {
                format!(" shell exited ({code}) · Ctrl-Space {close} to close ")
            }
            crate::terminal::Ended::Killed(sig) => {
                format!(" shell killed by SIG{sig} · Ctrl-Space {close} to close ")
            }
            crate::terminal::Ended::Closed => {
                format!(" connection closed · ↵ reconnect · Ctrl-Space {close} to close ")
            }
        };
        // A failing exit status is worth noticing; a clean one is not.
        let style = match ended {
            crate::terminal::Ended::Exited(0) | crate::terminal::Ended::Closed => s.key_hint(),
            _ => s.key_hint().patch(s.status(s.theme.status.warning)),
        };
        let line = Line::from(Span::styled(text, style)).centered();
        let y = area.bottom().saturating_sub(1);
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                y,
                height: 1,
                ..area
            },
        );
    }
}

/// Draws a grid that is not a live terminal (the player): same cell
/// mapping, `skip_rows` top rows left out (a grid taller than `area`
/// keeps its cursor in view), the cursor shown when `cursor` is set.
pub(crate) fn render_grid<T: EventListener>(
    term: &alacritty_terminal::Term<T>,
    cursor: bool,
    skip_rows: u16,
    s: Styles,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    frame.render_widget(
        TermWidget {
            term,
            theme: s.theme,
            focused: cursor,
            skip_rows: i32::from(skip_rows),
            search: None,
        },
        area,
    );
}

/// `▍Search  query▏                 3 of 17 ` on the chrome surface. The
/// query keeps its end when it does not fit; the counter is muted, or in
/// the warning colour for `No match`, absent while the query is empty.
fn render_search_bar(
    search: &ScrollSearch,
    focused: bool,
    s: Styles,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    let width = usize::from(area.width);
    let counter = counter_text(search.count).map(|(text, warn)| {
        let style = if warn {
            s.status(s.theme.status.warning)
        } else {
            s.muted()
        };
        (format!("{text} "), style)
    });
    let counter_width = counter.as_ref().map_or(0, |(t, _)| t.chars().count());
    // `▍` + ` Search  ` + cursor + one space before the counter.
    let fixed = 1 + 9 + 1 + 1;
    let room = width.saturating_sub(fixed + counter_width).max(1);
    let query = if search.query.chars().count() > room {
        let tail: String = {
            let chars: Vec<char> = search.query.chars().collect();
            let skip = chars.len().saturating_sub(room.saturating_sub(1));
            chars.get(skip..).unwrap_or_default().iter().collect()
        };
        format!("…{tail}")
    } else {
        search.query.clone()
    };
    let mut spans = vec![
        Span::styled("▍", s.accent()),
        Span::styled(" Search  ", s.secondary()),
        Span::styled(query, s.primary()),
    ];
    if focused {
        spans.push(Span::styled(" ", s.key_primary()));
    }
    let used: usize = spans.iter().map(Span::width).sum();
    if let Some((text, style)) = counter
        && used + counter_width < width
    {
        spans.push(Span::raw(" ".repeat(width - used - counter_width)));
        spans.push(Span::styled(text, style));
    }
    let line = Line::from(spans);
    let line = if width < 12 {
        Line::from(Span::styled(fmt::truncate("Search", width), s.secondary()))
    } else {
        line
    };
    frame.render_widget(Paragraph::new(line).style(s.chrome()), area);
}

struct TermWidget<'a, T> {
    term: &'a alacritty_terminal::Term<T>,
    theme: &'static Theme,
    focused: bool,
    /// Grid rows above the drawn area.
    skip_rows: i32,
    /// The pane's open search: highlights (live terminals only).
    search: Option<&'a ScrollSearch>,
}

impl<T: EventListener> Widget for TermWidget<'_, T> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let p = &self.theme.palette;
        let base = Style::new().bg(c(p.bg_base)).fg(c(p.text_primary));
        buf.set_style(area, base);

        let content = self.term.renderable_content();
        let offset = i32::try_from(content.display_offset).unwrap_or(0);
        let palette = Palette {
            theme: self.theme,
            colors: content.colors,
        };
        let cols = u16::try_from(self.term.columns()).unwrap_or(u16::MAX);
        for indexed in content.display_iter {
            let cell = indexed.cell;
            if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let row = indexed.point.line.0 + offset - self.skip_rows;
            let (Ok(y), Ok(x)) = (u16::try_from(row), u16::try_from(indexed.point.column.0)) else {
                continue;
            };
            if x >= area.width.min(cols) || y >= area.height {
                continue;
            }
            let mut fg = palette.resolve(cell.fg, true);
            let mut bg = palette.resolve(cell.bg, false);
            let mut modifier = Modifier::empty();
            if cell.flags.contains(Flags::BOLD) {
                modifier |= Modifier::BOLD;
            }
            if cell.flags.contains(Flags::ITALIC) {
                modifier |= Modifier::ITALIC;
            }
            if cell.flags.intersects(Flags::ALL_UNDERLINES) {
                modifier |= Modifier::UNDERLINED;
            }
            if cell.flags.contains(Flags::DIM) {
                modifier |= Modifier::DIM;
            }
            if cell.flags.contains(Flags::STRIKEOUT) {
                modifier |= Modifier::CROSSED_OUT;
            }
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            if cell.flags.contains(Flags::HIDDEN) {
                fg = bg;
            }
            if let Some(search) = self.search {
                let point = indexed.point;
                if search.current.as_ref().is_some_and(|m| m.contains(&point)) {
                    bg = c(p.search_current);
                    fg = c(p.text_on_accent);
                } else if search.visible.iter().any(|m| m.contains(&point)) {
                    bg = c(p.search_match);
                }
            }
            let Some(target) = buf.cell_mut((area.x + x, area.y + y)) else {
                continue;
            };
            let mut symbol = String::new();
            symbol.push(cell_char(cell.c));
            if let Some(extra) = cell.zerowidth() {
                // A hostile stream can stack unbounded combining marks.
                symbol.extend(extra.iter().take(4).filter(|c| !c.is_control()));
            }
            target.set_symbol(&symbol);
            target.set_style(Style::new().fg(fg).bg(bg).add_modifier(modifier));
        }

        // Cursor: only in the live view, when the program shows it.
        let cursor = content.cursor;
        // While searching, the bar has the only cursor.
        if self.focused
            && self.search.is_none()
            && content.display_offset == 0
            && content.mode.contains(TermMode::SHOW_CURSOR)
            && cursor.shape != CursorShape::Hidden
        {
            let (Ok(y), Ok(x)) = (
                u16::try_from(cursor.point.line.0 - self.skip_rows),
                u16::try_from(cursor.point.column.0),
            ) else {
                return;
            };
            if x >= area.width || y >= area.height {
                return;
            }
            if let Some(cell) = buf.cell_mut((area.x + x, area.y + y)) {
                let accent = c(p.accent);
                match cursor.shape {
                    CursorShape::Underline => {
                        cell.set_style(
                            Style::new()
                                .add_modifier(Modifier::UNDERLINED)
                                .underline_color(accent),
                        );
                    }
                    _ => {
                        cell.set_style(Style::new().bg(accent).fg(c(p.text_on_accent)));
                    }
                }
            }
        }
    }
}

struct Palette<'a> {
    theme: &'static Theme,
    colors: &'a alacritty_terminal::term::color::Colors,
}

impl Palette<'_> {
    /// Colours here are chosen by the remote program (SGR), not by Portico:
    /// they bypass the design tokens on purpose, except the 16 named colours
    /// and the default fg/bg, which map to the theme.
    fn resolve(&self, color: TermColor, foreground: bool) -> Color {
        let p = &self.theme.palette;
        match color {
            TermColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
            TermColor::Indexed(i) => self.indexed(usize::from(i)),
            TermColor::Named(named) => {
                // OSC 4/10/11 overrides set by the remote program win.
                if let Some(rgb) = self.colors[named] {
                    return Color::Rgb(rgb.r, rgb.g, rgb.b);
                }
                match named {
                    NamedColor::Foreground | NamedColor::BrightForeground => c(p.text_primary),
                    NamedColor::DimForeground => c(p.text_secondary),
                    NamedColor::Background => c(p.bg_base),
                    NamedColor::Cursor => c(p.accent),
                    other => {
                        let idx = other as usize;
                        if idx < 16 {
                            self.ansi(idx)
                        } else if (NamedColor::DimBlack as usize..=NamedColor::DimWhite as usize)
                            .contains(&idx)
                        {
                            dim(self.ansi(idx - NamedColor::DimBlack as usize))
                        } else if foreground {
                            c(p.text_primary)
                        } else {
                            c(p.bg_base)
                        }
                    }
                }
            }
        }
    }

    fn ansi(&self, idx: usize) -> Color {
        self.theme
            .ansi
            .0
            .get(idx)
            .map_or(Color::Reset, |rgb| c(*rgb))
    }

    fn indexed(&self, idx: usize) -> Color {
        if let Some(rgb) = self.colors[idx] {
            return Color::Rgb(rgb.r, rgb.g, rgb.b);
        }
        match idx {
            0..16 => self.ansi(idx),
            16..232 => {
                // 6×6×6 cube, xterm levels.
                let i = idx - 16;
                let level = |v: usize| -> u8 {
                    if v == 0 {
                        0
                    } else {
                        u8::try_from(55 + v * 40).unwrap_or(255)
                    }
                };
                Color::Rgb(level(i / 36), level((i / 6) % 6), level(i % 6))
            }
            _ => {
                let v = u8::try_from(8 + (idx.saturating_sub(232)) * 10).unwrap_or(255);
                Color::Rgb(v, v, v)
            }
        }
    }
}

fn dim(color: Color) -> Color {
    match color {
        Color::Rgb(r, g, b) => {
            let f = |v: u8| u8::try_from(u16::from(v) * 2 / 3).unwrap_or(v);
            Color::Rgb(f(r), f(g), f(b))
        }
        other => other,
    }
}

/// What a grid cell draws. alacritty keeps a `\t` in the cell a tab
/// started from (for copy), and an empty cell is `\0`: both draw as a
/// space, as does any other control character (ratatui refuses them).
fn cell_char(c: char) -> char {
    if c.is_control() { ' ' } else { c }
}

#[cfg(test)]
mod tests {
    use super::cell_char;

    #[test]
    fn tabs_and_controls_draw_as_spaces() {
        assert_eq!(cell_char('\t'), ' ');
        assert_eq!(cell_char('\0'), ' ');
        assert_eq!(cell_char('\u{9b}'), ' ');
        assert_eq!(cell_char('é'), 'é');
    }
}
