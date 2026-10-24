//! # portico-theme
//!
//! Portico's design tokens, defined once in Rust and consumed by:
//! - the TUI, directly (`ratatui::style::Color::Rgb` from [`Rgb`]);
//! - the GUI, through generated CSS custom properties
//!   (`cargo run -p portico-theme --bin export-tokens`), checked in at
//!   `apps/portico-gui/src/lib/styles/tokens.css` and verified by a test.
//!
//! Identity: *architectural minimalism*. Deep mineral backgrounds, marble
//! text, one restrained metallic accent, colour reserved for meaning
//! (connection status). See `.claude/skills/ui-design-premium/SKILL.md`.
//!
//! Three dark themes and one light one (**Marble**). The `auto` setting
//! ([`AUTO`]) follows the system: Marble when it is light, the user's dark
//! theme otherwise ([`Theme::resolve`]). Every theme passes the same
//! contrast and ΔE tests (see the tests at the bottom).

// Colour literals are grouped by RGB byte (`0xRR_GG_BB`) on purpose.
#![allow(clippy::mistyped_literal_suffixes)]

use std::fmt::Write as _;

use serde::Serialize;

/// 24-bit colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    pub const fn hex(v: u32) -> Self {
        // Truncation is intended: we take the three low bytes.
        #[allow(clippy::cast_possible_truncation)]
        Self((v >> 16) as u8, (v >> 8) as u8, v as u8)
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }
}

/// Semantic surface / text / accent palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Palette {
    /// App background (behind everything, terminal background).
    pub bg_base: Rgb,
    /// Panels, sidebar.
    pub bg_surface: Rgb,
    /// Cards, hovered rows, inputs.
    pub bg_elevated: Rgb,
    /// Modals, command palette, popovers.
    pub bg_overlay: Rgb,
    /// Selected row background.
    pub bg_selected: Rgb,
    pub border_subtle: Rgb,
    pub border_strong: Rgb,
    pub text_primary: Rgb,
    pub text_secondary: Rgb,
    pub text_muted: Rgb,
    /// Signature accent (focus ring, active tab underline, primary button).
    pub accent: Rgb,
    pub accent_strong: Rgb,
    pub text_on_accent: Rgb,
    /// Terminal scrollback search: background of every visible match (a
    /// quiet mineral tint, kept apart from the accent-tinted selection so
    /// a match never reads as selected text; terminal text stays AAA).
    pub search_match: Rgb,
    /// Terminal scrollback search: background of the current match (the
    /// accent; its text is `text_on_accent`).
    pub search_current: Rgb,
}

/// Status colours: the only place where saturated colour is allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StatusPalette {
    pub idle: Rgb,
    pub connecting: Rgb,
    pub authenticating: Rgb,
    pub connected: Rgb,
    pub warning: Rgb,
    pub error: Rgb,
}

/// ANSI 16 colours for terminal views (0–7 normal, 8–15 bright).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct AnsiPalette(pub [Rgb; 16]);

/// Light or dark: decides the CSS `color-scheme`, the shadows, the scrim
/// and which theme `auto` picks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Appearance {
    Dark,
    Light,
}

impl Appearance {
    /// The CSS `color-scheme` value.
    pub const fn css(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub appearance: Appearance,
    pub palette: Palette,
    pub status: StatusPalette,
    pub ansi: AnsiPalette,
}

/// Spacing scale (GUI px; TUI uses 1 cell ≈ 8px horizontally).
pub const SPACING: [(&str, u16); 8] = [
    ("0", 0),
    ("1", 4),
    ("2", 8),
    ("3", 12),
    ("4", 16),
    ("5", 24),
    ("6", 32),
    ("7", 48),
];

/// Corner radii (GUI).
pub const RADII: [(&str, u16); 4] = [("sm", 4), ("md", 8), ("lg", 12), ("pill", 999)];

/// Motion durations in ms. Nothing in Portico animates longer than `slow`.
pub const MOTION: [(&str, u16); 4] = [("instant", 0), ("fast", 120), ("base", 180), ("slow", 260)];

/// Period of looping "in flight" indicators (status pulse, skeletons): the
/// only animations allowed to exceed `slow`.
pub const MOTION_LOOP_MS: u16 = 1200;

/// Elevation shadows (GUI) of the dark themes: deep and black.
pub const SHADOWS_DARK: [(&str, &str); 2] = [
    ("raised", "0 12px 32px rgb(0 0 0 / 0.4)"),
    ("overlay", "0 24px 64px rgb(0 0 0 / 0.5)"),
];

/// Elevation shadows of a light theme: a warm graphite, soft and wide (a
/// black shadow on marble reads as dirt), with a tight contact shadow so an
/// overlay keeps a crisp edge.
pub const SHADOWS_LIGHT: [(&str, &str); 2] = [
    (
        "raised",
        "0 1px 2px rgb(43 36 28 / 0.08), 0 10px 28px rgb(43 36 28 / 0.12)",
    ),
    (
        "overlay",
        "0 1px 3px rgb(43 36 28 / 0.10), 0 24px 56px rgb(43 36 28 / 0.18)",
    ),
];

/// Modal scrims (GUI, `--scrim` behind dialogs, `--scrim-soft` behind the
/// command palette). Dark: the base colour dims the app. Light: the base
/// colour would only wash it out, so a light graphite veil separates the
/// dialog instead.
pub const SCRIMS_DARK: [(&str, &str); 2] = [
    (
        "scrim",
        "color-mix(in srgb, var(--bg-base) 72%, transparent)",
    ),
    (
        "scrim-soft",
        "color-mix(in srgb, var(--bg-base) 55%, transparent)",
    ),
];
pub const SCRIMS_LIGHT: [(&str, &str); 2] = [
    ("scrim", "rgb(43 36 28 / 0.22)"),
    ("scrim-soft", "rgb(43 36 28 / 0.12)"),
];

impl Appearance {
    pub const fn shadows(self) -> [(&'static str, &'static str); 2] {
        match self {
            Self::Dark => SHADOWS_DARK,
            Self::Light => SHADOWS_LIGHT,
        }
    }

    pub const fn scrims(self) -> [(&'static str, &'static str); 2] {
        match self {
            Self::Dark => SCRIMS_DARK,
            Self::Light => SCRIMS_LIGHT,
        }
    }
}

/// Standard easing (decelerate) and emphasized easing (spring-like overshoot-free).
pub const EASING_STANDARD: &str = "cubic-bezier(0.2, 0, 0, 1)";
pub const EASING_EMPHASIZED: &str = "cubic-bezier(0.3, 0, 0, 1.0)";

/// Font stacks (GUI). Monospace is also the terminal font.
pub const FONT_UI: &str = "\"Inter Variable\", Inter, -apple-system, BlinkMacSystemFont, \"Segoe UI\", system-ui, sans-serif";
pub const FONT_MONO: &str =
    "\"JetBrains Mono\", \"SF Mono\", \"Cascadia Code\", Menlo, \"DejaVu Sans Mono\", monospace";
/// Display face for the wordmark and large titles only: a crisp grotesque,
/// distinct from the UI face by its proportions and tracking.
pub const FONT_DISPLAY: &str =
    "\"Geist Sans\", Geist, \"Inter Variable\", Inter, -apple-system, system-ui, sans-serif";

/// Type scale (GUI px). `display*` are for host titles in the display face.
pub const TYPE_SCALE: [(&str, u16); 8] = [
    ("xs", 11),
    ("sm", 12),
    ("base", 13),
    ("md", 14),
    ("lg", 16),
    ("xl", 20),
    ("display-sm", 30),
    ("display", 34),
];

/// Component sizes (GUI, CSS lengths), exported as `--size-<name>`:
/// - `dot`: the status dot, everywhere (tabs, pane headers, lists, status bar);
/// - `dot-ring`: the ring of a hollow dot ("no shell attached");
/// - `icon-sm`: small inline icons (pane count, close crosses);
/// - `card`: the widest connection or failure card over a terminal;
/// - `bar`: the accent bar of a selected row (and the focus ring width);
/// - `line-gap`: between the two lines of a list row (title, then details);
/// - `row`: a list row, a form control (input, select) or a checkbox line;
/// - `control`: a button or a toggle of a dialog;
/// - `control-sm`: a compact control (a small button, an inline input);
/// - `badge`: a badge or pill (origin, count);
/// - `badge-sm`: a compact badge, a small icon box;
/// - `icon-xs`: the smallest inline icons (lock of an origin badge);
/// - `menu`: the minimum width of a context menu.
pub const SIZES: [(&str, &str); 13] = [
    ("dot", "6px"),
    ("dot-ring", "1.5px"),
    ("icon-sm", "12px"),
    ("card", "540px"),
    ("bar", "2px"),
    ("line-gap", "2px"),
    ("row", "34px"),
    ("control", "32px"),
    ("control-sm", "30px"),
    ("badge", "20px"),
    ("badge-sm", "16px"),
    ("icon-xs", "11px"),
    ("menu", "224px"),
];

/// Alpha of the terminal selection (`--accent-selection`): the theme's
/// accent at about one third, so selected text stays readable.
pub const ACCENT_SELECTION_ALPHA: u8 = 0x55;

/// Letter spacing of small capitals (badges such as ZOOM and DEMO, section labels).
pub const TRACKING_CAPS: &str = "0.08em";

/// **Obsidian**: the default. Near-black mineral, marble text, brass accent.
pub const OBSIDIAN: Theme = Theme {
    id: "obsidian",
    name: "Obsidian",
    description: "Near-black mineral surfaces, marble text, brushed-brass accent.",
    appearance: Appearance::Dark,
    palette: Palette {
        bg_base: Rgb::hex(0x0b_0c_0e),
        bg_surface: Rgb::hex(0x11_13_16),
        bg_elevated: Rgb::hex(0x17_1a_1e),
        bg_overlay: Rgb::hex(0x1c_20_25),
        bg_selected: Rgb::hex(0x22_25_2b),
        border_subtle: Rgb::hex(0x23_27_2d),
        border_strong: Rgb::hex(0x34_39_41),
        text_primary: Rgb::hex(0xec_e9_e2),
        text_secondary: Rgb::hex(0xa8_a5_9d),
        text_muted: Rgb::hex(0x6c_6a_65),
        accent: Rgb::hex(0xc8_a9_6b),
        accent_strong: Rgb::hex(0xdd_c0_85),
        text_on_accent: Rgb::hex(0x14_11_0b),
        search_match: Rgb::hex(0x3a_3d_42),
        search_current: Rgb::hex(0xc8_a9_6b),
    },
    status: StatusPalette {
        idle: Rgb::hex(0x6c_6a_65),
        connecting: Rgb::hex(0x8a_aa_d6),
        authenticating: Rgb::hex(0xb3_9d_e0),
        connected: Rgb::hex(0x7f_c8_a0),
        warning: Rgb::hex(0xf2_c1_4e),
        error: Rgb::hex(0xe5_7c_70),
    },
    ansi: AnsiPalette([
        Rgb::hex(0x1c_20_25),
        Rgb::hex(0xe5_7c_70),
        Rgb::hex(0x7f_c8_a0),
        Rgb::hex(0xe0_b4_6a),
        Rgb::hex(0x8a_aa_d6),
        Rgb::hex(0xb3_9d_e0),
        Rgb::hex(0x7c_c4_c4),
        Rgb::hex(0xc9_c5_bc),
        Rgb::hex(0x4a_4f_57),
        Rgb::hex(0xf0_96_8b),
        Rgb::hex(0x9b_dc_b8),
        Rgb::hex(0xec_c8_86),
        Rgb::hex(0xa8_c2_e8),
        Rgb::hex(0xc9_b6_ee),
        Rgb::hex(0x99_da_da),
        Rgb::hex(0xf4_f1_ea),
    ]),
};

/// **Basalt**: cool blue-grey stone, steel accent.
pub const BASALT: Theme = Theme {
    id: "basalt",
    name: "Basalt",
    description: "Cool volcanic grey-blue, steel accent. Calm and technical.",
    appearance: Appearance::Dark,
    palette: Palette {
        bg_base: Rgb::hex(0x0d_10_14),
        bg_surface: Rgb::hex(0x12_16_1c),
        bg_elevated: Rgb::hex(0x18_1d_24),
        bg_overlay: Rgb::hex(0x1e_24_2c),
        bg_selected: Rgb::hex(0x23_2a_34),
        border_subtle: Rgb::hex(0x24_2b_34),
        border_strong: Rgb::hex(0x35_3e_4a),
        text_primary: Rgb::hex(0xe4_e8_ee),
        text_secondary: Rgb::hex(0x9b_a4_b0),
        text_muted: Rgb::hex(0x63_6c_78),
        accent: Rgb::hex(0xb4_bc_c6),
        accent_strong: Rgb::hex(0xcc_d3_db),
        text_on_accent: Rgb::hex(0x0b_12_1a),
        search_match: Rgb::hex(0x30_40_5a),
        search_current: Rgb::hex(0xb4_bc_c6),
    },
    status: StatusPalette {
        idle: Rgb::hex(0x63_6c_78),
        connecting: Rgb::hex(0x6f_c3_d3),
        authenticating: Rgb::hex(0xb4_a2_e6),
        connected: Rgb::hex(0x82_cf_a8),
        warning: Rgb::hex(0xe6_bb_6e),
        error: Rgb::hex(0xea_80_78),
    },
    ansi: AnsiPalette([
        Rgb::hex(0x1e_24_2c),
        Rgb::hex(0xea_80_78),
        Rgb::hex(0x82_cf_a8),
        Rgb::hex(0xe6_bb_6e),
        Rgb::hex(0x8f_b3_d9),
        Rgb::hex(0xb4_a2_e6),
        Rgb::hex(0x7e_c9_d1),
        Rgb::hex(0xc5_cc_d6),
        Rgb::hex(0x4b_55_62),
        Rgb::hex(0xf2_9a_92),
        Rgb::hex(0x9e_e0_bf),
        Rgb::hex(0xf0_cd_8c),
        Rgb::hex(0xae_ca_e8),
        Rgb::hex(0xcb_bd_f0),
        Rgb::hex(0x9c_dc_e3),
        Rgb::hex(0xf2_f5_f9),
    ]),
};

/// **Travertine Night**: warm dark stone, terracotta accent.
pub const TRAVERTINE: Theme = Theme {
    id: "travertine",
    name: "Travertine Night",
    description: "Warm dark stone, terracotta accent. Soft on the eyes at night.",
    appearance: Appearance::Dark,
    palette: Palette {
        bg_base: Rgb::hex(0x12_10_0e),
        bg_surface: Rgb::hex(0x18_15_12),
        bg_elevated: Rgb::hex(0x1f_1b_17),
        bg_overlay: Rgb::hex(0x26_21_1c),
        bg_selected: Rgb::hex(0x2c_26_20),
        border_subtle: Rgb::hex(0x2c_26_20),
        border_strong: Rgb::hex(0x40_37_2e),
        text_primary: Rgb::hex(0xef_e6_da),
        text_secondary: Rgb::hex(0xb0_a4_95),
        text_muted: Rgb::hex(0x75_6b_60),
        accent: Rgb::hex(0xc9_9a_74),
        accent_strong: Rgb::hex(0xdc_b0_8e),
        text_on_accent: Rgb::hex(0x1a_0f_09),
        search_match: Rgb::hex(0x3e_41_3a),
        search_current: Rgb::hex(0xc9_9a_74),
    },
    status: StatusPalette {
        idle: Rgb::hex(0x75_6b_60),
        connecting: Rgb::hex(0x94_ae_cf),
        authenticating: Rgb::hex(0xbc_a3_d6),
        connected: Rgb::hex(0x97_c7_8f),
        warning: Rgb::hex(0xe3_b5_63),
        error: Rgb::hex(0xe0_74_66),
    },
    ansi: AnsiPalette([
        Rgb::hex(0x26_21_1c),
        Rgb::hex(0xe0_74_66),
        Rgb::hex(0x97_c7_8f),
        Rgb::hex(0xe3_b5_63),
        Rgb::hex(0x94_ae_cf),
        Rgb::hex(0xbc_a3_d6),
        Rgb::hex(0x8e_c4_b5),
        Rgb::hex(0xd1_c6_b8),
        Rgb::hex(0x57_4d_43),
        Rgb::hex(0xee_8f_80),
        Rgb::hex(0xae_da_a6),
        Rgb::hex(0xef_ca_80),
        Rgb::hex(0xae_c6_e2),
        Rgb::hex(0xd0_bb_e6),
        Rgb::hex(0xa8_d8_ca),
        Rgb::hex(0xf7_f0_e6),
    ]),
};

/// **Marble**: the light theme. Warm polished marble (never pure white),
/// graphite text, a deep bronze accent; statuses and ANSI colours darkened
/// for a light ground. Measured contrasts are in the `docs/STACK.md` decision log.
///
/// ANSI: the remote paints on `bg-base`, so every colour that carries text
/// stays ≥ 4.5:1 on it, bright ones included (they gain chroma, not
/// lightness). `white` (7) is a readable stone grey, as in most light
/// terminal palettes. Exception, by design: `bright white` (15) stays a
/// light marble so white-on-colour segments (prompts, status lines) keep
/// working; bright white text on the default background is then faint,
/// which is the remote's choice.
pub const MARBLE: Theme = Theme {
    id: "marble",
    name: "Marble",
    description: "Warm light marble, graphite text, deep bronze accent. For daylight.",
    appearance: Appearance::Light,
    palette: Palette {
        bg_base: Rgb::hex(0xf4_f1_ec),
        bg_surface: Rgb::hex(0xee_ea_e4),
        bg_elevated: Rgb::hex(0xe7_e2_da),
        bg_overlay: Rgb::hex(0xfa_f8_f5),
        bg_selected: Rgb::hex(0xe2_db_cf),
        border_subtle: Rgb::hex(0xdd_d7_cd),
        border_strong: Rgb::hex(0xc8_bf_b2),
        text_primary: Rgb::hex(0x1f_1d_1a),
        text_secondary: Rgb::hex(0x5a_55_4d),
        text_muted: Rgb::hex(0x7b_74_6a),
        accent: Rgb::hex(0x80_60_3a),
        accent_strong: Rgb::hex(0x68_4d_2e),
        text_on_accent: Rgb::hex(0xfb_f8_f3),
        search_match: Rgb::hex(0xcf_d8_de),
        search_current: Rgb::hex(0x80_60_3a),
    },
    status: StatusPalette {
        idle: Rgb::hex(0x7b_74_6a),
        connecting: Rgb::hex(0x2a_64_b0),
        authenticating: Rgb::hex(0x74_48_b5),
        connected: Rgb::hex(0x25_70_4a),
        warning: Rgb::hex(0x98_54_00),
        error: Rgb::hex(0xb5_36_2c),
    },
    ansi: AnsiPalette([
        Rgb::hex(0x2b_28_24),
        Rgb::hex(0xb0_35_2b),
        Rgb::hex(0x2a_6e_47),
        Rgb::hex(0x83_5a_00),
        Rgb::hex(0x2c_5c_a3),
        Rgb::hex(0x84_41_9c),
        Rgb::hex(0x1a_6b_72),
        Rgb::hex(0x57_52_4a),
        Rgb::hex(0x6e_68_5f),
        Rgb::hex(0xc4_3c_2c),
        Rgb::hex(0x1d_78_45),
        Rgb::hex(0x8f_63_00),
        Rgb::hex(0x33_66_b8),
        Rgb::hex(0x94_46_ad),
        Rgb::hex(0x12_77_83),
        Rgb::hex(0xfb_f9_f6),
    ]),
};

/// ANSI indices of a light theme exempt from the "readable on the
/// background" rule: `bright white` (see [`MARBLE`]).
pub const ANSI_LIGHT_EXEMPT: [usize; 1] = [15];

/// All built-in themes; the first one is the default dark theme.
pub const THEMES: [Theme; 4] = [OBSIDIAN, BASALT, TRAVERTINE, MARBLE];

/// The theme setting that follows the system appearance.
pub const AUTO: &str = "auto";

impl Theme {
    pub fn by_id(id: &str) -> Option<&'static Theme> {
        THEMES.iter().find(|t| t.id == id)
    }

    pub fn default_theme() -> &'static Theme {
        &THEMES[0]
    }

    pub const fn is_light(&self) -> bool {
        matches!(self.appearance, Appearance::Light)
    }

    /// The light theme `auto` uses.
    pub fn light_theme() -> &'static Theme {
        THEMES
            .iter()
            .find(|t| t.is_light())
            .unwrap_or_else(Self::default_theme)
    }

    /// The dark theme `auto` uses: `dark_theme` when it names a dark
    /// theme, the default otherwise.
    pub fn auto_dark(dark_theme: Option<&str>) -> &'static Theme {
        dark_theme
            .and_then(Self::by_id)
            .filter(|t| !t.is_light())
            .unwrap_or_else(Self::default_theme)
    }

    /// The theme to paint for a setting (`theme`: an id, [`AUTO`] or
    /// absent = auto), the remembered dark theme and the system appearance.
    /// `None` for an unknown id (the UIs warn and fall back to `auto`).
    pub fn resolve(
        theme: Option<&str>,
        dark_theme: Option<&str>,
        system: Appearance,
    ) -> Option<&'static Theme> {
        match theme {
            None | Some(AUTO) => Some(match system {
                Appearance::Light => Self::light_theme(),
                Appearance::Dark => Self::auto_dark(dark_theme),
            }),
            Some(id) => Self::by_id(id),
        }
    }
}

/// Generates the GUI token stylesheet: shared scales on `:root`, one
/// `[data-theme=…]` block per theme (Obsidian is also the `:root` default).
pub fn export_css() -> String {
    let mut css = String::from(
        "/* GENERATED by `cargo run -p portico-theme --bin export-tokens`. Do not edit. */\n\n:root {\n",
    );
    for (k, v) in SPACING {
        let _ = writeln!(css, "  --space-{k}: {v}px;");
    }
    for (k, v) in RADII {
        let _ = writeln!(css, "  --radius-{k}: {v}px;");
    }
    for (k, v) in MOTION {
        let _ = writeln!(css, "  --motion-{k}: {v}ms;");
    }
    let _ = writeln!(css, "  --motion-loop: {MOTION_LOOP_MS}ms;");
    for (k, v) in TYPE_SCALE {
        let _ = writeln!(css, "  --text-{k}: {v}px;");
    }
    for (k, v) in SIZES {
        let _ = writeln!(css, "  --size-{k}: {v};");
    }
    let _ = writeln!(css, "  --tracking-caps: {TRACKING_CAPS};");
    let _ = writeln!(css, "  --ease-standard: {EASING_STANDARD};");
    let _ = writeln!(css, "  --ease-emphasized: {EASING_EMPHASIZED};");
    let _ = writeln!(css, "  --font-ui: {FONT_UI};");
    let _ = writeln!(css, "  --font-mono: {FONT_MONO};");
    let _ = writeln!(css, "  --font-display: {FONT_DISPLAY};");
    css.push_str("}\n");

    for (i, theme) in THEMES.iter().enumerate() {
        let selector = if i == 0 {
            format!(":root,\n[data-theme=\"{}\"]", theme.id)
        } else {
            format!("[data-theme=\"{}\"]", theme.id)
        };
        let _ = writeln!(
            css,
            "\n/* {} — {} */\n{selector} {{",
            theme.name, theme.description
        );
        let p = &theme.palette;
        for (name, c) in [
            ("bg-base", p.bg_base),
            ("bg-surface", p.bg_surface),
            ("bg-elevated", p.bg_elevated),
            ("bg-overlay", p.bg_overlay),
            ("bg-selected", p.bg_selected),
            ("border-subtle", p.border_subtle),
            ("border-strong", p.border_strong),
            ("text-primary", p.text_primary),
            ("text-secondary", p.text_secondary),
            ("text-muted", p.text_muted),
            ("accent", p.accent),
            ("accent-strong", p.accent_strong),
            ("text-on-accent", p.text_on_accent),
            ("search-match", p.search_match),
            ("search-current", p.search_current),
        ] {
            let _ = writeln!(css, "  --{name}: {};", c.to_hex());
        }
        let _ = writeln!(
            css,
            "  --accent-selection: {}{ACCENT_SELECTION_ALPHA:02x};",
            p.accent.to_hex()
        );
        let s = &theme.status;
        for (name, c) in [
            ("idle", s.idle),
            ("connecting", s.connecting),
            ("authenticating", s.authenticating),
            ("connected", s.connected),
            ("warning", s.warning),
            ("error", s.error),
        ] {
            let _ = writeln!(css, "  --status-{name}: {};", c.to_hex());
        }
        for (i, c) in theme.ansi.0.iter().enumerate() {
            let _ = writeln!(css, "  --ansi-{i}: {};", c.to_hex());
        }
        for (k, v) in theme.appearance.shadows() {
            let _ = writeln!(css, "  --shadow-{k}: {v};");
        }
        for (k, v) in theme.appearance.scrims() {
            let _ = writeln!(css, "  --{k}: {v};");
        }
        let _ = writeln!(css, "  color-scheme: {};", theme.appearance.css());
        css.push_str("}\n");
    }
    css
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WCAG relative luminance.
    fn luminance(c: Rgb) -> f64 {
        let ch = |v: u8| {
            let s = f64::from(v) / 255.0;
            if s <= 0.039_28 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(c.0) + 0.7152 * ch(c.1) + 0.0722 * ch(c.2)
    }

    fn contrast(a: Rgb, b: Rgb) -> f64 {
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    #[test]
    fn themes_meet_contrast_targets() {
        for t in THEMES {
            let p = t.palette;
            for bg in [
                p.bg_base,
                p.bg_surface,
                p.bg_elevated,
                p.bg_overlay,
                p.bg_selected,
            ] {
                assert!(
                    contrast(p.text_primary, bg) >= 7.0,
                    "{} primary text (AAA)",
                    t.id
                );
                assert!(
                    contrast(p.text_secondary, bg) >= 4.5,
                    "{} secondary text (AA)",
                    t.id
                );
            }
            assert!(
                contrast(p.text_muted, p.bg_base) >= 3.0,
                "{} muted text",
                t.id
            );
            assert!(
                contrast(p.text_on_accent, p.accent) >= 4.5,
                "{} on-accent",
                t.id
            );
            // Scrollback search: terminal text stays AAA on a match, the
            // current match reads like the selection.
            assert!(
                contrast(p.text_primary, p.search_match) >= 7.0,
                "{} text on search match (AAA)",
                t.id
            );
            assert!(
                contrast(p.text_on_accent, p.search_current) >= 4.5,
                "{} text on current search match",
                t.id
            );
            let s = t.status;
            for c in [
                s.connecting,
                s.authenticating,
                s.connected,
                s.warning,
                s.error,
            ] {
                assert!(contrast(c, p.bg_surface) >= 4.5, "{} status colour", t.id);
            }
        }
    }

    /// CIE L*a*b* from sRGB (D65).
    // Standard colour-science notation (x, y, z, r, g, b…) reads best here.
    #[allow(clippy::many_single_char_names)]
    fn lab(c: Rgb) -> (f64, f64, f64) {
        let lin = |v: u8| {
            let s = f64::from(v) / 255.0;
            if s <= 0.040_45 {
                s / 12.92
            } else {
                ((s + 0.055) / 1.055).powf(2.4)
            }
        };
        let (r, g, b) = (lin(c.0), lin(c.1), lin(c.2));
        let x = (0.4124 * r + 0.3576 * g + 0.1805 * b) / 0.950_47;
        let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
        let z = (0.0193 * r + 0.1192 * g + 0.9505 * b) / 1.088_83;
        let f = |t: f64| {
            if t > 0.008_856 {
                t.cbrt()
            } else {
                7.787 * t + 16.0 / 116.0
            }
        };
        let (fx, fy, fz) = (f(x), f(y), f(z));
        (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
    }

    fn delta_e(a: Rgb, b: Rgb) -> f64 {
        let (l1, a1, b1) = lab(a);
        let (l2, a2, b2) = lab(b);
        ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
    }

    /// `fg` at `alpha` over an opaque `bg` (how `--accent-selection` paints).
    fn blend(fg: Rgb, bg: Rgb, alpha: u8) -> Rgb {
        let mix = |f: u8, b: u8| {
            let (a, f, b) = (u32::from(alpha), u32::from(f), u32::from(b));
            u8::try_from((f * a + b * (255 - a) + 127) / 255).unwrap_or(u8::MAX)
        };
        Rgb(mix(fg.0, bg.0), mix(fg.1, bg.1), mix(fg.2, bg.2))
    }

    #[test]
    fn blend_matches_the_alpha_formula() {
        let white = Rgb::hex(0xff_ff_ff);
        let black = Rgb::hex(0x00_00_00);
        assert_eq!(blend(white, black, 0xff), white);
        assert_eq!(blend(white, black, 0x00), black);
        assert_eq!(blend(white, black, 0x55), Rgb::hex(0x55_55_55));
    }

    /// Colour = meaning: the accent must never be mistaken for a status.
    #[test]
    fn accent_is_distinct_from_statuses() {
        for t in THEMES {
            let s = t.status;
            for (name, c) in [
                ("connecting", s.connecting),
                ("authenticating", s.authenticating),
                ("connected", s.connected),
                ("warning", s.warning),
                ("error", s.error),
            ] {
                let d = delta_e(t.palette.accent, c);
                assert!(
                    d >= 20.0,
                    "{}: accent too close to {name} (ΔE {d:.1})",
                    t.id
                );
            }
        }
    }

    /// A search match must be seen on the terminal background and must not
    /// read as the selection; the current one is the accent.
    #[test]
    fn search_tokens_stand_out() {
        for t in THEMES {
            let p = t.palette;
            assert_eq!(p.search_current, p.accent, "{}", t.id);
            let selection = blend(p.accent, p.bg_base, ACCENT_SELECTION_ALPHA);
            for (name, bg) in [
                ("bg-base", p.bg_base),
                ("bg-selected", p.bg_selected),
                ("selection", selection),
            ] {
                let d = delta_e(p.search_match, bg);
                assert!(
                    d >= 10.0,
                    "{}: search match too close to {name} (ΔE {d:.1})",
                    t.id
                );
            }
            let d = delta_e(p.search_match, p.search_current);
            assert!(d >= 20.0, "{}: match and current match (ΔE {d:.1})", t.id);
        }
    }

    /// A light theme's terminal: every ANSI colour that carries text is
    /// AA on the terminal background, and so is the default foreground.
    /// Dark themes keep their own rule (0 and 8 are surfaces and dim text).
    #[test]
    fn light_ansi_colours_are_readable_on_the_terminal_background() {
        let light: Vec<_> = THEMES.iter().filter(|t| t.is_light()).collect();
        assert!(!light.is_empty());
        for t in light {
            let bg = t.palette.bg_base;
            for (i, c) in t.ansi.0.iter().enumerate() {
                if ANSI_LIGHT_EXEMPT.contains(&i) {
                    continue;
                }
                let r = contrast(*c, bg);
                assert!(r >= 4.5, "{}: ansi {i} {} at {r:.2}:1", t.id, c.to_hex());
            }
            // Bright white stays light: white-on-colour segments still work.
            for i in ANSI_LIGHT_EXEMPT {
                let c = t.ansi.0.get(i).copied().unwrap_or(bg);
                assert!(contrast(c, t.ansi.0[0]) >= 7.0, "{}: ansi {i}", t.id);
            }
        }
    }

    /// Dark themes: the 14 text colours (all but black and bright black,
    /// which are surfaces and dim text there) read on the background.
    #[test]
    fn dark_ansi_colours_are_readable_on_the_terminal_background() {
        for t in THEMES.iter().filter(|t| !t.is_light()) {
            for (i, c) in t.ansi.0.iter().enumerate() {
                if i == 0 || i == 8 {
                    continue;
                }
                let r = contrast(*c, t.palette.bg_base);
                assert!(r >= 4.5, "{}: ansi {i} at {r:.2}:1", t.id);
            }
        }
    }

    /// A light theme's elevation reads the other way round: dialogs are the
    /// lightest plane, hover, inputs and selection sink into the stone.
    #[test]
    fn light_surfaces_step_in_lightness() {
        for t in THEMES.iter().filter(|t| t.is_light()) {
            let p = t.palette;
            let l = |c| lab(c).0;
            // Overlay above base, then surface, elevated, selected below.
            assert!(l(p.bg_overlay) > l(p.bg_base), "{}", t.id);
            assert!(l(p.bg_base) > l(p.bg_surface), "{}", t.id);
            assert!(l(p.bg_surface) > l(p.bg_elevated), "{}", t.id);
            assert!(l(p.bg_elevated) > l(p.bg_selected), "{}", t.id);
            // Never pure white: marble, not paper.
            assert_ne!(p.bg_overlay, Rgb::hex(0xff_ff_ff));
            assert!(
                contrast(p.accent, p.bg_base) >= 4.5,
                "{} accent as text",
                t.id
            );
            assert!(contrast(p.text_muted, p.bg_selected) >= 3.0, "{}", t.id);
        }
    }

    #[test]
    fn auto_follows_the_system() {
        use Appearance::{Dark, Light};
        let id = |t: Option<&Theme>| t.map(|t| t.id);
        assert_eq!(THEMES.iter().filter(|t| t.is_light()).count(), 1);
        assert!(!Theme::default_theme().is_light());
        assert_eq!(id(Theme::resolve(None, None, Light)), Some("marble"));
        assert_eq!(id(Theme::resolve(None, None, Dark)), Some("obsidian"));
        assert_eq!(
            id(Theme::resolve(Some(AUTO), Some("basalt"), Dark)),
            Some("basalt")
        );
        assert_eq!(
            id(Theme::resolve(Some(AUTO), Some("basalt"), Light)),
            Some("marble")
        );
        // A light or unknown "dark theme" falls back to the default.
        assert_eq!(
            id(Theme::resolve(Some(AUTO), Some("marble"), Dark)),
            Some("obsidian")
        );
        assert_eq!(
            id(Theme::resolve(Some(AUTO), Some("nope"), Dark)),
            Some("obsidian")
        );
        // An explicit theme ignores the system.
        assert_eq!(
            id(Theme::resolve(Some("travertine"), None, Light)),
            Some("travertine")
        );
        assert_eq!(
            id(Theme::resolve(Some("marble"), None, Dark)),
            Some("marble")
        );
        assert_eq!(id(Theme::resolve(Some("solarized"), None, Dark)), None);
        assert!(Theme::by_id(AUTO).is_none(), "auto is not a theme id");
    }

    #[test]
    fn each_theme_exports_its_appearance() {
        let css = export_css();
        for theme in THEMES {
            let block = css
                .split(&format!("[data-theme=\"{}\"]", theme.id))
                .nth(1)
                .and_then(|b| b.split('}').next())
                .unwrap_or_default();
            let scheme = format!("color-scheme: {};", theme.appearance.css());
            assert!(block.contains(&scheme), "{}: {scheme}", theme.id);
            for (k, v) in theme.appearance.shadows() {
                assert!(
                    block.contains(&format!("--shadow-{k}: {v};")),
                    "{}",
                    theme.id
                );
            }
            assert!(block.contains("--scrim:"), "{}", theme.id);
            assert!(block.contains("--scrim-soft:"), "{}", theme.id);
        }
    }

    #[test]
    fn ids_are_unique() {
        for (i, a) in THEMES.iter().enumerate() {
            assert!(THEMES.iter().skip(i + 1).all(|b| b.id != a.id));
        }
    }

    #[test]
    fn every_theme_exports_its_selection_from_its_accent() {
        let css = export_css();
        for theme in THEMES {
            let block = css
                .split(&format!("[data-theme=\"{}\"]", theme.id))
                .nth(1)
                .unwrap_or_default();
            let line = format!(
                "--accent-selection: {}{ACCENT_SELECTION_ALPHA:02x};",
                theme.palette.accent.to_hex()
            );
            assert!(block.contains(&line), "{}: missing `{line}`", theme.id);
        }
        assert!(css.contains("--size-bar: 2px;"));
        assert!(css.contains("--size-line-gap: 2px;"));
        for line in [
            "--size-row: 34px;",
            "--size-control: 32px;",
            "--size-control-sm: 30px;",
            "--size-menu: 224px;",
            "--size-badge: 20px;",
            "--size-badge-sm: 16px;",
            "--size-icon-xs: 11px;",
        ] {
            assert!(css.contains(line), "missing `{line}`");
        }
    }

    #[test]
    fn generated_css_is_up_to_date() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../apps/portico-gui/src/lib/styles/tokens.css"
        );
        let on_disk = std::fs::read_to_string(path).unwrap_or_default();
        assert!(
            on_disk == export_css(),
            "tokens.css is stale: run `cargo run -p portico-theme --bin export-tokens`"
        );
    }
}
