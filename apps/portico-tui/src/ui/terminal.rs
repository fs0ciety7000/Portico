//! Renders an `alacritty_terminal` grid into the ratatui buffer.

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

use crate::terminal::TerminalView;
use crate::theme::{Styles, c};

/// Draws the terminal. `focused` controls the cursor and the scroll badge.
pub(crate) fn render(
    view: &TerminalView,
    focused: bool,
    s: Styles,
    frame: &mut Frame<'_>,
    area: Rect,
) {
    let term = view
        .term
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    frame.render_widget(
        TermWidget {
            term: &term,
            theme: s.theme,
            focused,
        },
        area,
    );
    let offset = term.grid().display_offset();
    drop(term);

    if offset > 0 {
        let badge = Line::from(vec![
            Span::styled(format!(" ↑ {offset} lines "), s.key_hint()),
            Span::styled(" Shift-PgDn ", s.muted()),
        ])
        .right_aligned();
        frame.render_widget(Paragraph::new(badge), Rect { height: 1, ..area });
    }
    if let Some(ended) = &view.ended {
        let text = match ended {
            crate::terminal::Ended::Exited(code) => {
                format!(" shell exited ({code}) · Ctrl-Space w to close ")
            }
            crate::terminal::Ended::Killed(sig) => {
                format!(" shell killed by SIG{sig} · Ctrl-Space w to close ")
            }
            crate::terminal::Ended::Closed => {
                " connection closed · Ctrl-Space w to close ".to_owned()
            }
        };
        let line = Line::from(Span::styled(text, s.key_hint())).centered();
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

struct TermWidget<'a> {
    term: &'a alacritty_terminal::Term<crate::terminal::EventProxy>,
    theme: &'static Theme,
    focused: bool,
}

impl Widget for TermWidget<'_> {
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
            let row = indexed.point.line.0 + offset;
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
            let Some(target) = buf.cell_mut((area.x + x, area.y + y)) else {
                continue;
            };
            let mut symbol = String::new();
            symbol.push(if cell.c == '\0' { ' ' } else { cell.c });
            if let Some(extra) = cell.zerowidth() {
                // A hostile stream can stack unbounded combining marks.
                symbol.extend(extra.iter().take(4));
            }
            target.set_symbol(&symbol);
            target.set_style(Style::new().fg(fg).bg(bg).add_modifier(modifier));
        }

        // Cursor: only in the live view, when the program shows it.
        let cursor = content.cursor;
        if self.focused
            && content.display_offset == 0
            && content.mode.contains(TermMode::SHOW_CURSOR)
            && cursor.shape != CursorShape::Hidden
        {
            let (Ok(y), Ok(x)) = (
                u16::try_from(cursor.point.line.0),
                u16::try_from(cursor.point.column.0),
            ) else {
                return;
            };
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
