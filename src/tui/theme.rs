//! Derived terminal colors and OSC 11 background detection.
//!
//! Semantic render helpers receive [`AppTheme`] explicitly and resolve chrome
//! before writing cells. Syntax styles and user diff overrides retain their
//! literal named/RGB/indexed colors.

use ratatui::style::{Color, Modifier, Style};

use crate::config::{DiffThemeConfig, ThemeConfig, ThemeModeConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rgb {
    pub(crate) red: u8,
    pub(crate) green: u8,
    pub(crate) blue: u8,
}

impl Rgb {
    pub(super) const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }

    fn color(self, truecolor: bool) -> Color {
        if truecolor {
            Color::Rgb(self.red, self.green, self.blue)
        } else {
            Color::Indexed(nearest_indexed(self.red, self.green, self.blue))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThemeKind {
    Dark,
    Light,
}

/// The four annotation-channel colors planned by M17. Keeping this semantic
/// mapping in the foundation prevents that UI from inventing new ad-hoc hues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ChannelColors {
    pub(crate) onboarding: Color,
    pub(crate) delegation: Color,
    pub(crate) collaboration: Color,
    pub(crate) note: Color,
}

/// Every current TUI chrome role, derived from a compact base palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppTheme {
    pub(crate) kind: ThemeKind,
    pub(crate) background: Color,
    pub(crate) foreground: Color,
    pub(crate) muted: Color,
    pub(crate) subtle: Color,
    pub(crate) accent: Color,
    pub(crate) warning: Color,
    pub(crate) info: Color,
    pub(crate) positive: Color,
    pub(crate) negative: Color,
    pub(crate) secondary: Color,
    pub(crate) selection_bg: Color,
    pub(crate) range_bg: Color,
    pub(crate) positive_bg: Color,
    pub(crate) negative_bg: Color,
    pub(crate) positive_emphasis_bg: Color,
    pub(crate) negative_emphasis_bg: Color,
    pub(crate) added_word: Style,
    pub(crate) removed_word: Style,
    pub(crate) gutter_added: Style,
    pub(crate) gutter_removed: Style,
    pub(crate) channels: ChannelColors,
    transparent: bool,
    truecolor: bool,
}

#[derive(Debug, Clone, Copy)]
struct BasePalette {
    background: Rgb,
    foreground: Rgb,
    accent: Rgb,
    added: Rgb,
    removed: Rgb,
    info: Rgb,
}

impl BasePalette {
    const fn dark() -> Self {
        Self {
            background: Rgb::new(0x0d, 0x11, 0x17),
            foreground: Rgb::new(0xf0, 0xf6, 0xfc),
            accent: Rgb::new(0xd2, 0x99, 0x22),
            added: Rgb::new(0x3f, 0xb9, 0x50),
            removed: Rgb::new(0xf8, 0x51, 0x49),
            info: Rgb::new(0x58, 0xa6, 0xff),
        }
    }

    const fn light() -> Self {
        Self {
            background: Rgb::new(0xff, 0xff, 0xff),
            foreground: Rgb::new(0x1f, 0x23, 0x28),
            accent: Rgb::new(0x9a, 0x67, 0x00),
            added: Rgb::new(0x1a, 0x7f, 0x37),
            removed: Rgb::new(0xcf, 0x22, 0x2e),
            info: Rgb::new(0x09, 0x69, 0xda),
        }
    }
}

impl Default for AppTheme {
    fn default() -> Self {
        Self::derive(ThemeKind::Dark, None, true, true)
    }
}

impl AppTheme {
    pub(crate) fn resolve(
        config: ThemeConfig,
        diff: &DiffThemeConfig,
        detected_background: Option<Rgb>,
        truecolor: bool,
    ) -> Self {
        let kind = match config.mode {
            ThemeModeConfig::Dark => ThemeKind::Dark,
            ThemeModeConfig::Light => ThemeKind::Light,
            ThemeModeConfig::Auto => detected_background
                .map(theme_kind_for_background)
                .unwrap_or(ThemeKind::Dark),
        };
        let mut theme = Self::derive(kind, detected_background, config.transparent, truecolor);
        theme.apply_diff_overrides(diff);
        theme
    }

    fn derive(
        kind: ThemeKind,
        detected_background: Option<Rgb>,
        transparent: bool,
        truecolor: bool,
    ) -> Self {
        let base = match kind {
            ThemeKind::Dark => BasePalette::dark(),
            ThemeKind::Light => BasePalette::light(),
        };
        // Transparent mode contrasts against the terminal's actual queried
        // background. Opaque mode owns and paints the palette background.
        let background = if transparent {
            detected_background.unwrap_or(base.background)
        } else {
            base.background
        };
        let foreground = guard_contrast(base.foreground, background, 4.5, base.foreground);
        let accent = guard_contrast(base.accent, background, 4.5, foreground);
        let positive = guard_contrast(base.added, background, 4.5, foreground);
        let negative = guard_contrast(base.removed, background, 4.5, foreground);
        let info = guard_contrast(base.info, background, 4.5, foreground);
        let muted = guard_contrast(
            blend(foreground, background, 0.50),
            background,
            4.5,
            foreground,
        );
        let subtle = guard_contrast(
            blend(foreground, background, 0.68),
            background,
            4.5,
            foreground,
        );
        let secondary_hue = blend(base.removed, base.info, 0.48);
        let secondary = guard_contrast(secondary_hue, background, 4.5, foreground);
        let selection_bg = guarded_surface(base.accent, background, 0.22, foreground, 4.5);
        let range_bg = guarded_surface(base.info, background, 0.34, foreground, 4.5);
        let positive_bg = guarded_surface(base.added, background, 0.15, positive, 4.5);
        let negative_bg = guarded_surface(base.removed, background, 0.15, negative, 4.5);
        let positive_emphasis_bg = guarded_surface(base.added, background, 0.40, positive, 4.5);
        let negative_emphasis_bg = guarded_surface(base.removed, background, 0.40, negative, 4.5);

        let background_color = background.color(truecolor);
        let effective_background = if transparent {
            // Nothing is painted in transparent mode: terminal content still
            // sits on the exact OSC 11 background, even when foregrounds are
            // downgraded to xterm-256.
            background
        } else {
            color_rgb(background_color).unwrap_or(background)
        };
        let text_color = |rgb: Rgb, minimum: f64| {
            resolve_text_color(rgb, effective_background, minimum, truecolor)
        };
        let foreground_color = text_color(foreground, 4.5);
        let accent = text_color(accent, 4.5);
        let warning = accent;
        let info = text_color(info, 4.5);
        let muted = text_color(muted, 4.5);
        let positive_color = text_color(positive, 4.5);
        let negative_color = text_color(negative, 4.5);
        let positive_bg_color = resolve_surface_color(
            positive_bg,
            color_rgb(positive_color).unwrap(),
            4.5,
            truecolor,
        );
        let negative_bg_color = resolve_surface_color(
            negative_bg,
            color_rgb(negative_color).unwrap(),
            4.5,
            truecolor,
        );
        let positive_emphasis_bg_color = resolve_surface_color(
            positive_emphasis_bg,
            color_rgb(positive_color).unwrap(),
            4.5,
            truecolor,
        );
        let negative_emphasis_bg_color = resolve_surface_color(
            negative_emphasis_bg,
            color_rgb(negative_color).unwrap(),
            4.5,
            truecolor,
        );
        Self {
            kind,
            background: background_color,
            foreground: foreground_color,
            muted,
            subtle: text_color(subtle, 4.5),
            accent,
            warning,
            info,
            positive: positive_color,
            negative: negative_color,
            gutter_added: Style::default().fg(positive_color),
            gutter_removed: Style::default().fg(negative_color),
            secondary: text_color(secondary, 4.5),
            selection_bg: resolve_surface_color(
                selection_bg,
                color_rgb(foreground_color).unwrap(),
                4.5,
                truecolor,
            ),
            range_bg: resolve_surface_color(
                range_bg,
                color_rgb(foreground_color).unwrap(),
                4.5,
                truecolor,
            ),
            positive_bg: positive_bg_color,
            negative_bg: negative_bg_color,
            positive_emphasis_bg: positive_emphasis_bg_color,
            negative_emphasis_bg: negative_emphasis_bg_color,
            added_word: Style::default()
                .bg(positive_emphasis_bg_color)
                .add_modifier(Modifier::BOLD),
            removed_word: Style::default()
                .bg(negative_emphasis_bg_color)
                .add_modifier(Modifier::BOLD),
            channels: ChannelColors {
                onboarding: accent,
                delegation: warning,
                collaboration: info,
                note: muted,
            },
            transparent,
            truecolor,
        }
    }

    pub(crate) fn base_style(self) -> Style {
        let style = Style::default().fg(self.foreground);
        if self.transparent {
            style
        } else {
            style.bg(self.background)
        }
    }

    pub(crate) fn literal_style(self, spec: &str) -> Style {
        let mut style = parse_style_spec(spec);
        style.fg = style.fg.map(|color| self.downgrade_explicit(color));
        style.bg = style.bg.map(|color| self.downgrade_explicit(color));
        style
    }

    fn downgrade_explicit(self, color: Color) -> Color {
        match color {
            Color::Rgb(red, green, blue) if !self.truecolor => {
                Color::Indexed(nearest_indexed(red, green, blue))
            }
            other => other,
        }
    }

    fn apply_diff_overrides(&mut self, diff: &DiffThemeConfig) {
        let defaults = DiffThemeConfig::default();
        if diff.added_line_bg != defaults.added_line_bg
            && let Some(color) = self.literal_line_background(&diff.added_line_bg)
        {
            self.positive_bg = color;
        }
        if diff.removed_line_bg != defaults.removed_line_bg
            && let Some(color) = self.literal_line_background(&diff.removed_line_bg)
        {
            self.negative_bg = color;
        }
        if diff.added_word != defaults.added_word {
            self.added_word = self.literal_style(&diff.added_word);
        }
        if diff.removed_word != defaults.removed_word {
            self.removed_word = self.literal_style(&diff.removed_word);
        }
        if diff.gutter_added != defaults.gutter_added {
            self.gutter_added = self.literal_style(&diff.gutter_added);
        }
        if diff.gutter_removed != defaults.gutter_removed {
            self.gutter_removed = self.literal_style(&diff.gutter_removed);
        }
    }

    fn literal_line_background(self, spec: &str) -> Option<Color> {
        let style = self.literal_style(spec);
        style.bg.or(style.fg)
    }
}

fn parse_style_spec(spec: &str) -> Style {
    let mut style = Style::default();
    let mut background = false;
    for token in spec.split_whitespace() {
        if token == "on" {
            background = true;
            continue;
        }
        if let Some(color) = spec_color(token) {
            style = if background {
                style.bg(color)
            } else {
                style.fg(color)
            };
            background = false;
            continue;
        }
        style = match token {
            "bold" => style.add_modifier(Modifier::BOLD),
            "dim" => style.add_modifier(Modifier::DIM),
            "italic" => style.add_modifier(Modifier::ITALIC),
            "underlined" | "underline" => style.add_modifier(Modifier::UNDERLINED),
            _ => style,
        };
    }
    style
}

fn spec_color(token: &str) -> Option<Color> {
    match token {
        "black" => Some(Color::Black),
        "blue" => Some(Color::Blue),
        "cyan" => Some(Color::Cyan),
        "dark-gray" | "dark-grey" => Some(Color::DarkGray),
        "gray" | "grey" => Some(Color::Gray),
        "green" => Some(Color::Green),
        "magenta" => Some(Color::Magenta),
        "red" => Some(Color::Red),
        "white" => Some(Color::White),
        "yellow" => Some(Color::Yellow),
        _ => {
            if let Some(hex) = token.strip_prefix('#') {
                if hex.len() != 6 {
                    return None;
                }
                let value = u32::from_str_radix(hex, 16).ok()?;
                return Some(Color::Rgb(
                    (value >> 16) as u8,
                    (value >> 8) as u8,
                    value as u8,
                ));
            }
            token.parse::<u8>().ok().map(Color::Indexed)
        }
    }
}

fn color_rgb(color: Color) -> Option<Rgb> {
    match color {
        Color::Black => Some(Rgb::new(0, 0, 0)),
        Color::Red => Some(Rgb::new(255, 0, 0)),
        Color::Green => Some(Rgb::new(0, 255, 0)),
        Color::Yellow => Some(Rgb::new(255, 255, 0)),
        Color::Blue => Some(Rgb::new(0, 0, 255)),
        Color::Magenta => Some(Rgb::new(255, 0, 255)),
        Color::Cyan => Some(Rgb::new(0, 255, 255)),
        Color::Gray => Some(Rgb::new(192, 192, 192)),
        Color::DarkGray => Some(Rgb::new(128, 128, 128)),
        Color::White => Some(Rgb::new(255, 255, 255)),
        Color::Rgb(red, green, blue) => Some(Rgb::new(red, green, blue)),
        Color::Indexed(index) => Some(indexed_rgb(index)),
        _ => None,
    }
}

fn indexed_rgb(index: u8) -> Rgb {
    const ANSI: [Rgb; 16] = [
        Rgb::new(0, 0, 0),
        Rgb::new(128, 0, 0),
        Rgb::new(0, 128, 0),
        Rgb::new(128, 128, 0),
        Rgb::new(0, 0, 128),
        Rgb::new(128, 0, 128),
        Rgb::new(0, 128, 128),
        Rgb::new(192, 192, 192),
        Rgb::new(128, 128, 128),
        Rgb::new(255, 0, 0),
        Rgb::new(0, 255, 0),
        Rgb::new(255, 255, 0),
        Rgb::new(0, 0, 255),
        Rgb::new(255, 0, 255),
        Rgb::new(0, 255, 255),
        Rgb::new(255, 255, 255),
    ];
    match index {
        0..=15 => ANSI[index as usize],
        16..=231 => {
            let index = index - 16;
            let level = |component: u8| [0, 95, 135, 175, 215, 255][component as usize];
            Rgb::new(level(index / 36), level(index % 36 / 6), level(index % 6))
        }
        _ => {
            let value = 8 + (index - 232) * 10;
            Rgb::new(value, value, value)
        }
    }
}

fn blend(foreground: Rgb, background: Rgb, opacity: f64) -> Rgb {
    let opacity = opacity.clamp(0.0, 1.0);
    let channel = |front: u8, back: u8| {
        (f64::from(front).mul_add(opacity, f64::from(back) * (1.0 - opacity)))
            .round()
            .clamp(0.0, 255.0) as u8
    };
    Rgb::new(
        channel(foreground.red, background.red),
        channel(foreground.green, background.green),
        channel(foreground.blue, background.blue),
    )
}

fn relative_luminance(color: Rgb) -> f64 {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.red) + 0.7152 * linear(color.green) + 0.0722 * linear(color.blue)
}

fn contrast_ratio(left: Rgb, right: Rgb) -> f64 {
    let (lighter, darker) = {
        let left = relative_luminance(left);
        let right = relative_luminance(right);
        if left >= right {
            (left, right)
        } else {
            (right, left)
        }
    };
    (lighter + 0.05) / (darker + 0.05)
}

fn best_contrast_endpoint(background: Rgb) -> Rgb {
    let black = Rgb::new(0, 0, 0);
    let white = Rgb::new(255, 255, 255);
    if contrast_ratio(black, background) >= contrast_ratio(white, background) {
        black
    } else {
        white
    }
}

fn guard_contrast(candidate: Rgb, background: Rgb, minimum: f64, _fallback: Rgb) -> Rgb {
    if contrast_ratio(candidate, background) >= minimum {
        return candidate;
    }
    let endpoint = best_contrast_endpoint(background);
    (1..=255)
        .map(|step| blend(endpoint, candidate, f64::from(step) / 255.0))
        .find(|color| contrast_ratio(*color, background) >= minimum)
        .unwrap_or(endpoint)
}

fn resolve_text_color(candidate: Rgb, background: Rgb, minimum: f64, truecolor: bool) -> Color {
    let guarded = guard_contrast(
        candidate,
        background,
        minimum,
        best_contrast_endpoint(background),
    );
    if truecolor {
        return Color::Rgb(guarded.red, guarded.green, guarded.blue);
    }
    let index = nearest_contrasting_index(guarded, background, minimum);
    Color::Indexed(index)
}

fn resolve_surface_color(candidate: Rgb, text: Rgb, minimum: f64, truecolor: bool) -> Color {
    if truecolor {
        return Color::Rgb(candidate.red, candidate.green, candidate.blue);
    }
    let index = nearest_contrasting_index(candidate, text, minimum);
    Color::Indexed(index)
}

fn nearest_contrasting_index(candidate: Rgb, against: Rgb, minimum: f64) -> u8 {
    let distance = |color: Rgb| {
        let red = u32::from(candidate.red.abs_diff(color.red));
        let green = u32::from(candidate.green.abs_diff(color.green));
        let blue = u32::from(candidate.blue.abs_diff(color.blue));
        red * red + green * green + blue * blue
    };
    (0..=u8::MAX)
        .filter(|index| contrast_ratio(indexed_rgb(*index), against) >= minimum)
        .min_by_key(|index| distance(indexed_rgb(*index)))
        .unwrap_or_else(|| {
            let endpoint = best_contrast_endpoint(against);
            nearest_indexed(endpoint.red, endpoint.green, endpoint.blue)
        })
}

fn guarded_surface(hue: Rgb, background: Rgb, opacity: f64, text: Rgb, minimum: f64) -> Rgb {
    let candidate = blend(hue, background, opacity);
    if contrast_ratio(text, candidate) >= minimum {
        return candidate;
    }
    // Pull a colored surface back toward the base until its text is readable.
    (1..=255)
        .map(|step| blend(background, candidate, f64::from(step) / 255.0))
        .find(|surface| contrast_ratio(text, *surface) >= minimum)
        .unwrap_or(background)
}

fn theme_kind_for_background(background: Rgb) -> ThemeKind {
    if relative_luminance(background) > 0.35 {
        ThemeKind::Light
    } else {
        ThemeKind::Dark
    }
}

/// Nearest xterm-256 color, considering the 6x6x6 cube and grayscale ramp.
pub(crate) fn nearest_indexed(red: u8, green: u8, blue: u8) -> u8 {
    fn cube_component(value: u8) -> (u8, u8) {
        let levels = [0u8, 95, 135, 175, 215, 255];
        let (index, level) = levels
            .into_iter()
            .enumerate()
            .min_by_key(|(_, level)| value.abs_diff(*level))
            .expect("fixed palette is non-empty");
        (index as u8, level)
    }
    fn distance(a: Rgb, b: Rgb) -> u32 {
        let red = u32::from(a.red.abs_diff(b.red));
        let green = u32::from(a.green.abs_diff(b.green));
        let blue = u32::from(a.blue.abs_diff(b.blue));
        red * red + green * green + blue * blue
    }

    let (ri, rv) = cube_component(red);
    let (gi, gv) = cube_component(green);
    let (bi, bv) = cube_component(blue);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;
    let spread = red.max(green).max(blue) - red.min(green).min(blue);
    if spread >= 12 {
        if cube_index == 16 {
            let maximum = red.max(green).max(blue);
            return if red == maximum {
                52
            } else if green == maximum {
                22
            } else {
                17
            };
        }
        return cube_index;
    }

    let gray = (u16::from(red) + u16::from(green) + u16::from(blue)) / 3;
    let gray_step = ((gray.saturating_sub(8)).div_ceil(10)).min(23) as u8;
    let gray_value = 8 + 10 * gray_step;
    let gray_index = 232 + gray_step;
    if distance(
        Rgb::new(red, green, blue),
        Rgb::new(gray_value, gray_value, gray_value),
    ) < distance(Rgb::new(red, green, blue), Rgb::new(rv, gv, bv))
    {
        gray_index
    } else {
        cube_index
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(color: Color) -> Rgb {
        color_rgb(color).unwrap_or_else(|| panic!("expected color value, got {color:?}"))
    }

    #[test]
    fn blending_is_deterministic_at_edges_and_midpoint() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert_eq!(blend(white, black, 0.0), black);
        assert_eq!(blend(white, black, 1.0), white);
        assert_eq!(blend(white, black, 0.5), Rgb::new(128, 128, 128));
    }

    #[test]
    fn contrast_uses_wcag_luminance_and_guard_reaches_minimum() {
        let black = Rgb::new(0, 0, 0);
        let white = Rgb::new(255, 255, 255);
        assert!((contrast_ratio(black, white) - 21.0).abs() < 0.001);
        let guarded = guard_contrast(Rgb::new(25, 25, 25), black, 4.5, white);
        assert!(contrast_ratio(guarded, black) >= 4.5);
    }

    #[test]
    fn dark_and_light_derivations_keep_text_slots_readable() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let theme = AppTheme::derive(kind, None, false, true);
            let background = rgb(theme.background);
            for color in [
                theme.foreground,
                theme.muted,
                theme.subtle,
                theme.accent,
                theme.info,
                theme.positive,
                theme.negative,
            ] {
                assert!(
                    contrast_ratio(rgb(color), background) >= 4.5,
                    "{kind:?} {color:?}"
                );
            }
            assert_eq!(theme.channels.onboarding, theme.accent);
            assert_eq!(theme.channels.delegation, theme.warning);
            assert_eq!(theme.channels.collaboration, theme.info);
            assert_eq!(theme.channels.note, theme.muted);
        }
    }

    #[test]
    fn auto_detection_selects_light_or_dark_and_transparent_uses_terminal_bg() {
        let light = Rgb::new(250, 250, 250);
        let dark = Rgb::new(10, 10, 10);
        assert_eq!(theme_kind_for_background(light), ThemeKind::Light);
        assert_eq!(theme_kind_for_background(dark), ThemeKind::Dark);
        let theme = AppTheme::resolve(
            ThemeConfig {
                mode: ThemeModeConfig::Auto,
                transparent: true,
            },
            &DiffThemeConfig::default(),
            Some(light),
            true,
        );
        assert_eq!(theme.kind, ThemeKind::Light);
        assert_eq!(theme.background, Color::Rgb(250, 250, 250));
    }

    #[test]
    fn xterm_downgrade_covers_derived_and_explicit_rgb_colors() {
        let theme = AppTheme::derive(ThemeKind::Dark, None, false, false);
        assert!(matches!(theme.accent, Color::Indexed(_)));
        assert!(matches!(
            theme.literal_style("#a1b2c3").fg,
            Some(Color::Indexed(_))
        ));
        assert_eq!(nearest_indexed(63, 185, 80), 71);
    }

    #[test]
    fn custom_diff_colors_remain_literal_and_materialize_by_role() {
        let diff = DiffThemeConfig {
            added_line_bg: "#ffffff".to_owned(),
            gutter_added: "#010101".to_owned(),
            ..DiffThemeConfig::default()
        };
        let theme = AppTheme::resolve(
            ThemeConfig {
                mode: ThemeModeConfig::Dark,
                transparent: false,
            },
            &diff,
            None,
            true,
        );
        assert_eq!(theme.positive_bg, Color::Rgb(255, 255, 255));
        assert_eq!(theme.gutter_added.fg, Some(Color::Rgb(1, 1, 1)));
        assert_eq!(theme.added_word.bg, Some(theme.positive_emphasis_bg));
    }

    #[test]
    fn legacy_word_specs_preserve_foreground_background_and_modifiers() {
        let resolve = |added_word: &str| {
            AppTheme::resolve(
                ThemeConfig {
                    mode: ThemeModeConfig::Dark,
                    transparent: false,
                },
                &DiffThemeConfig {
                    added_word: added_word.to_owned(),
                    ..DiffThemeConfig::default()
                },
                None,
                true,
            )
            .added_word
        };

        let bare = resolve("#00a000");
        assert_eq!(bare.fg, Some(Color::Rgb(0, 160, 0)));
        assert!(bare.bg.is_none());

        let modifiers = resolve("bold italic");
        assert!(modifiers.fg.is_none());
        assert!(modifiers.bg.is_none());
        assert!(modifiers.add_modifier.contains(Modifier::BOLD));
        assert!(modifiers.add_modifier.contains(Modifier::ITALIC));

        let background = resolve("bold on #00a000");
        assert!(background.fg.is_none());
        assert!(background.bg.is_some());
        assert!(background.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn line_background_keeps_bare_color_compatibility() {
        let theme = AppTheme::derive(ThemeKind::Dark, None, true, true);
        assert_eq!(
            theme.literal_line_background("#00a000"),
            Some(Color::Rgb(0, 160, 0))
        );
        assert_eq!(
            theme.literal_line_background("bold on #123456"),
            Some(Color::Rgb(0x12, 0x34, 0x56))
        );
        assert_eq!(theme.literal_line_background("bold"), None);
    }

    #[test]
    fn literal_syntax_colors_do_not_collide_with_chrome_or_old_markers() {
        let theme = AppTheme::derive(ThemeKind::Light, None, true, true);
        assert_eq!(theme.literal_style("red").fg, Some(Color::Red));
        assert_eq!(theme.literal_style("yellow").fg, Some(Color::Yellow));
        for marker in [
            "#010201", "#020101", "#010202", "#020102", "#010203", "#020103",
        ] {
            let expected = spec_color(marker).unwrap();
            assert_eq!(theme.literal_style(marker).fg, Some(expected), "{marker}");
            let diff_theme = AppTheme::resolve(
                ThemeConfig::default(),
                &DiffThemeConfig {
                    added_word: marker.to_owned(),
                    ..DiffThemeConfig::default()
                },
                None,
                true,
            );
            assert_eq!(diff_theme.added_word.fg, Some(expected), "diff {marker}");
        }
    }

    #[test]
    fn contrast_targets_hold_for_detected_backgrounds_and_xterm_downgrade() {
        let backgrounds = [
            Rgb::new(0, 0, 0),
            Rgb::new(255, 255, 255),
            Rgb::new(0, 160, 0),
            Rgb::new(117, 117, 117),
            Rgb::new(12, 80, 180),
            Rgb::new(0, 0, 184),
        ];
        for truecolor in [true, false] {
            for background in backgrounds {
                let theme = AppTheme::resolve(
                    ThemeConfig {
                        mode: ThemeModeConfig::Auto,
                        transparent: true,
                    },
                    &DiffThemeConfig::default(),
                    Some(background),
                    truecolor,
                );
                for foreground in [
                    theme.foreground,
                    theme.muted,
                    theme.subtle,
                    theme.accent,
                    theme.info,
                    theme.positive,
                    theme.negative,
                ] {
                    assert!(
                        contrast_ratio(rgb(foreground), background) >= 4.5,
                        "truecolor={truecolor} bg={background:?} fg={foreground:?}"
                    );
                }
                assert!(contrast_ratio(rgb(theme.positive), rgb(theme.positive_bg)) >= 4.5);
                assert!(contrast_ratio(rgb(theme.negative), rgb(theme.negative_bg)) >= 4.5);
            }
        }

        let opaque = AppTheme::resolve(
            ThemeConfig {
                mode: ThemeModeConfig::Dark,
                transparent: false,
            },
            &DiffThemeConfig::default(),
            Some(Rgb::new(0, 0, 184)),
            false,
        );
        let painted = rgb(opaque.background);
        assert!(contrast_ratio(rgb(opaque.foreground), painted) >= 4.5);
    }
}
