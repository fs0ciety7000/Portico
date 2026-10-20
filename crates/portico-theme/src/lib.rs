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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Theme {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
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

/// Elevation shadows (GUI), identical in every dark theme.
pub const SHADOWS: [(&str, &str); 2] = [
    ("raised", "0 12px 32px rgb(0 0 0 / 0.4)"),
    ("overlay", "0 24px 64px rgb(0 0 0 / 0.5)"),
];

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

/// **Obsidian**: the default. Near-black mineral, marble text, brass accent.
pub const OBSIDIAN: Theme = Theme {
    id: "obsidian",
    name: "Obsidian",
    description: "Near-black mineral surfaces, marble text, brushed-brass accent.",
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

/// All built-in themes; the first one is the default.
pub const THEMES: [Theme; 3] = [OBSIDIAN, BASALT, TRAVERTINE];

impl Theme {
    pub fn by_id(id: &str) -> Option<&'static Theme> {
        THEMES.iter().find(|t| t.id == id)
    }

    pub fn default_theme() -> &'static Theme {
        &THEMES[0]
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
    for (k, v) in SHADOWS {
        let _ = writeln!(css, "  --shadow-{k}: {v};");
    }
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
        ] {
            let _ = writeln!(css, "  --{name}: {};", c.to_hex());
        }
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
        let _ = writeln!(css, "  color-scheme: dark;");
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

    #[test]
    fn ids_are_unique() {
        for (i, a) in THEMES.iter().enumerate() {
            assert!(THEMES.iter().skip(i + 1).all(|b| b.id != a.id));
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
