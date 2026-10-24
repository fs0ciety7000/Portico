//! Bridges `portico-theme` tokens to ratatui styles, and holds the theme
//! setting ([`ThemeState`]: a theme, or `auto` resolved against the
//! terminal background detected at startup, see `appearance.rs`).

use portico_theme::{AUTO, Appearance, Rgb, THEMES, Theme};
use ratatui::style::{Color, Modifier, Style};

/// The theme setting: `auto` (the default) or one theme, the dark theme
/// `auto` uses and the appearance detected at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThemeState {
    /// `None` = auto.
    pub(crate) fixed: Option<&'static Theme>,
    /// The dark theme of `auto` (the last dark theme chosen).
    pub(crate) dark: &'static Theme,
    /// The terminal background (dark until detected).
    pub(crate) system: Appearance,
    /// Inside a run of `t` presses (no other key in between): the themes
    /// passed through do not become the dark theme of `auto`.
    pub(crate) cycling: bool,
}

impl Default for ThemeState {
    fn default() -> Self {
        Self {
            fixed: None,
            dark: Theme::default_theme(),
            system: Appearance::Dark,
            cycling: false,
        }
    }
}

impl ThemeState {
    /// The theme painted now.
    pub(crate) fn theme(&self) -> &'static Theme {
        self.fixed.unwrap_or(match self.system {
            Appearance::Light => Theme::light_theme(),
            Appearance::Dark => self.dark,
        })
    }

    pub(crate) fn is_auto(&self) -> bool {
        self.fixed.is_none()
    }

    /// `Auto · Marble`, or the theme's name.
    pub(crate) fn label(&self) -> String {
        if self.is_auto() {
            format!("Auto · {}", self.theme().name)
        } else {
            self.theme().name.to_owned()
        }
    }

    /// `t`: every theme in order, then Auto, then the first theme again.
    ///
    /// The dark theme of `auto` is the dark theme in use when a run of `t`
    /// presses starts (the one the user settled on), never one only passed
    /// through: from Basalt, `t t t` gives Travertine, Marble, then
    /// "Auto · Basalt".
    pub(crate) fn next(self) -> Self {
        let dark = match self.fixed {
            Some(t) if !self.cycling && !t.is_light() => t,
            _ => self.dark,
        };
        let at = self
            .fixed
            .and_then(|t| THEMES.iter().position(|o| o.id == t.id));
        let fixed = match at {
            None => THEMES.first(),
            Some(i) => THEMES.get(i.saturating_add(1)),
        };
        Self {
            fixed,
            dark,
            system: self.system,
            cycling: true,
        }
    }

    /// The values written to `settings.toml`: `theme` and the dark theme of
    /// `auto` (always written, so passing through a theme never leaves it
    /// as `auto`'s dark theme on disk).
    pub(crate) fn to_settings(self) -> (&'static str, &'static str) {
        let theme = self.fixed.map_or(AUTO, |t| t.id);
        match self.fixed {
            // A dark theme saved now is the one `auto` gets if the user
            // stops here (the next run of `t` starts from it).
            Some(t) if !t.is_light() => (theme, t.id),
            _ => (theme, self.dark.id),
        }
    }
}

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
    /// Body text: the primary colour without weight (titles add bold).
    pub(crate) fn primary(self) -> Style {
        Style::new().fg(c(self.theme.palette.text_primary))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_the_detected_background() {
        let mut state = ThemeState::default();
        assert_eq!(state.theme().id, "obsidian");
        assert_eq!(state.label(), "Auto · Obsidian");
        state.system = Appearance::Light;
        assert_eq!(state.theme().id, "marble");
        assert_eq!(state.label(), "Auto · Marble");
        assert_eq!(state.to_settings(), ("auto", "obsidian"));
    }

    #[test]
    fn cycling_goes_through_every_theme_then_auto() {
        let mut seen = Vec::new();
        let mut state = ThemeState::default();
        for _ in 0..=THEMES.len() {
            state = state.next();
            seen.push(state.label());
        }
        assert_eq!(
            seen,
            [
                "Obsidian",
                "Basalt",
                "Travertine Night",
                "Marble",
                "Auto · Obsidian"
            ]
        );
    }

    #[test]
    fn auto_keeps_the_dark_theme_settled_on_not_the_ones_passed() {
        let basalt = ThemeState {
            fixed: Theme::by_id("basalt"),
            ..ThemeState::default()
        };
        // t t t from Basalt: Travertine, Marble, Auto · Basalt.
        let travertine = basalt.next();
        assert_eq!(travertine.to_settings(), ("travertine", "travertine"));
        let marble = travertine.next();
        assert_eq!(marble.to_settings(), ("marble", "basalt"));
        let auto = marble.next();
        assert_eq!(auto.label(), "Auto · Basalt");
        assert_eq!(auto.to_settings(), ("auto", "basalt"));
        // Settled on Travertine (another key ended the run), then t t.
        let settled = ThemeState {
            cycling: false,
            ..travertine
        };
        let auto = settled.next().next();
        assert_eq!(auto.label(), "Auto · Travertine Night");
        // On a light terminal Auto paints Marble whatever the dark theme.
        let light = ThemeState {
            system: Appearance::Light,
            ..auto
        };
        assert_eq!(light.label(), "Auto · Marble");
    }
}
