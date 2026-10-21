//! Bridges `portico-theme` tokens to ratatui styles.

use portico_theme::{Rgb, Theme};
use ratatui::style::{Color, Modifier, Style};

pub(crate) fn c(rgb: Rgb) -> Color {
    Color::Rgb(rgb.0, rgb.1, rgb.2)
}

/// Pre-computed styles for one theme.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Styles {
    pub(crate) theme: &'static Theme,
}

impl Styles {
    pub(crate) fn new(theme: &'static Theme) -> Self {
        Self { theme }
    }

    pub(crate) fn base(self) -> Style {
        let p = &self.theme.palette;
        Style::new().bg(c(p.bg_base)).fg(c(p.text_primary))
    }
    pub(crate) fn surface(self) -> Style {
        let p = &self.theme.palette;
        Style::new().bg(c(p.bg_surface)).fg(c(p.text_primary))
    }
    pub(crate) fn overlay(self) -> Style {
        let p = &self.theme.palette;
        Style::new().bg(c(p.bg_overlay)).fg(c(p.text_primary))
    }
    pub(crate) fn border(self) -> Style {
        Style::new().fg(c(self.theme.palette.border_subtle))
    }
    pub(crate) fn border_focus(self) -> Style {
        Style::new().fg(c(self.theme.palette.accent))
    }
    pub(crate) fn secondary(self) -> Style {
        Style::new().fg(c(self.theme.palette.text_secondary))
    }
    pub(crate) fn muted(self) -> Style {
        Style::new().fg(c(self.theme.palette.text_muted))
    }
    pub(crate) fn accent(self) -> Style {
        Style::new().fg(c(self.theme.palette.accent))
    }
    pub(crate) fn title(self) -> Style {
        Style::new()
            .fg(c(self.theme.palette.text_primary))
            .add_modifier(Modifier::BOLD)
    }
    /// Background only: spans keep their meaning colours (status dot,
    /// favourite star, muted metadata) on the selected row.
    pub(crate) fn selected(self) -> Style {
        Style::new().bg(c(self.theme.palette.bg_selected))
    }
    /// Selection of a pane that does not have the focus: visible, but
    /// quieter than [`Self::selected`].
    pub(crate) fn elevated(self) -> Style {
        Style::new().bg(c(self.theme.palette.bg_elevated))
    }
    /// Header / chrome surface.
    pub(crate) fn chrome(self) -> Style {
        Style::new()
            .bg(c(self.theme.palette.bg_surface))
            .fg(c(self.theme.palette.text_primary))
    }
    /// Status-bar mode pill (HOSTS, TERMINAL…).
    pub(crate) fn mode_pill(self) -> Style {
        Style::new()
            .bg(c(self.theme.palette.accent))
            .fg(c(self.theme.palette.text_on_accent))
            .add_modifier(Modifier::BOLD)
    }
    /// Transient PREFIX mode: inverted so it never looks like TERMINAL.
    pub(crate) fn prefix_pill(self) -> Style {
        Style::new()
            .bg(c(self.theme.palette.bg_elevated))
            .fg(c(self.theme.palette.accent_strong))
            .add_modifier(Modifier::BOLD | Modifier::REVERSED)
    }
    /// Key cap of the primary action in a footer.
    pub(crate) fn key_primary(self) -> Style {
        Style::new()
            .bg(c(self.theme.palette.accent))
            .fg(c(self.theme.palette.text_on_accent))
    }
    pub(crate) fn border_strong(self) -> Style {
        Style::new().fg(c(self.theme.palette.border_strong))
    }
    pub(crate) fn key_hint(self) -> Style {
        Style::new()
            .fg(c(self.theme.palette.text_primary))
            .bg(c(self.theme.palette.bg_elevated))
    }
    #[allow(clippy::unused_self)] // keeps every style accessor on `Styles`
    pub(crate) fn status(self, rgb: Rgb) -> Style {
        Style::new().fg(c(rgb))
    }
}
