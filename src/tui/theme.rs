//! Derived terminal theme (roadmap M19).
//!
//! A small [`BasePalette`] (background/foreground/accent/diff hues/info) is
//! expanded into the semantic chrome slots of an [`AppTheme`] via
//! contrast-guarded blending. All TUI chrome colors resolve through the
//! theme *before* cells are written; user-provided `[diff.theme]` and
//! `[syntax.theme]` specs remain literal values and are never reinterpreted
//! as semantic markers. The intentional exception is changed-word emphasis:
//! its derived tint keeps syntax backgrounds literal but overrides the
//! foreground with the theme foreground to satisfy contrast.
//!
//! Light/dark auto-detection queries the terminal background with OSC 11
//! through `terminal-colorsaurus` (DA1-fenced, bounded timeout, termios
//! restored on every path). The query runs exactly once, in `tui::run`,
//! before Gander enables crossterm raw mode or starts the crossterm event
//! reader. Replies that arrive only after any non-`Detected` query outcome
//! are contained by [`crate::tui::osc_guard`];
//! the guarantees and the consciously accepted edge-case deltas of this
//! design are documented in docs/theme.md.
//!
//! Contrast uses the WCAG 2.x relative-luminance formula. Guarantees are
//! checked in the final output color space: truecolor slots against the
//! effective background, xterm-256 slots against the quantized background
//! that is actually painted (opaque) or the detected terminal background
//! (transparent), see [`AppTheme::resolve`].

use std::time::Duration;

use crate::theme::{
    BasePalette, FOREGROUND_CONTRAST, MUTED_CONTRAST, SurfaceConstraint, TEXT_CONTRAST, TextSlots,
    ThemeKind, ThemeSlots, blend, guarded_surface, highlight_constraints, kind_for_background,
    satisfies,
};
pub(crate) use crate::theme::{Rgb, contrast_ratio};
use ratatui::style::{Color, Modifier, Style};

fn color(rgb: Rgb) -> Color {
    Color::Rgb(rgb.r, rgb.g, rgb.b)
}

/// The derived theme with every semantic chrome slot resolved to a final
/// output color (`Color::Rgb` in truecolor, `Color::Indexed` on xterm-256).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppTheme {
    pub kind: ThemeKind,
    transparent: bool,
    /// The background all text-slot contrast is guaranteed against: the
    /// painted background when opaque, otherwise the detected (or assumed)
    /// terminal background.
    pub(crate) contrast_background: Rgb,
    /// Base surface color painted in opaque mode (and used for popup fills).
    pub background: Color,
    pub foreground: Color,
    /// Strongly de-emphasized text: hints, line numbers, dividers.
    pub muted: Color,
    /// Secondary body text.
    pub subtle: Color,
    /// Selected rows, headlines, comment markers. M17 channel: onboarding.
    pub accent: Color,
    /// Caution chrome (flag medium priority, …). M17 channel: delegation.
    pub warning: Color,
    /// Informational chrome, hunk headers, notices. M17 channel: collaboration.
    pub info: Color,
    /// Locations, keys, metadata columns.
    pub detail: Color,
    /// Secondary chrome: badges, section headings, artifact borders.
    pub secondary: Color,
    /// Added/viewed/resolved cues.
    pub positive: Color,
    /// Removed/error/todo cues.
    pub negative: Color,
    /// Cursor-row surface.
    pub selection_bg: Color,
    /// Active range-selection surface.
    pub range_bg: Color,
    /// Added-line surface (default when `[diff.theme]` leaves it unset).
    pub added_line_bg: Color,
    /// Removed-line surface.
    pub removed_line_bg: Color,
    /// Changed-word emphasis on added lines (bold + emphasis surface).
    pub added_word: Style,
    /// Changed-word emphasis on removed lines.
    pub removed_word: Style,
    /// Gutter bar for added lines.
    pub gutter_added: Style,
    /// Gutter bar for removed lines.
    pub gutter_removed: Style,
}

impl Default for AppTheme {
    fn default() -> Self {
        Self::resolve(crate::config::ThemeModeConfig::Dark, true, true, None)
    }
}

impl AppTheme {
    /// Derive the theme.
    ///
    /// * `mode`: explicit dark/light, or auto (uses `detected`, falling back
    ///   to dark so undetected terminals keep Gander's historical look).
    /// * `transparent`: suppress painting the base background; contrast is
    ///   then guaranteed against the detected terminal background (or the
    ///   palette background when detection was unavailable).
    /// * `truecolor`: when false, every slot quantizes to xterm-256 and the
    ///   contrast guarantees are re-established in indexed space.
    /// * `detected`: terminal background from the OSC 11 query, if any.
    pub(crate) fn resolve(
        mode: crate::config::ThemeModeConfig,
        transparent: bool,
        truecolor: bool,
        detected: Option<Rgb>,
    ) -> Self {
        let kind = match mode {
            crate::config::ThemeModeConfig::Dark => ThemeKind::Dark,
            crate::config::ThemeModeConfig::Light => ThemeKind::Light,
            crate::config::ThemeModeConfig::Auto => {
                detected.map(kind_for_background).unwrap_or(ThemeKind::Dark)
            }
        };
        let palette = BasePalette::for_kind(kind);

        // The background text sits on: painted palette background when
        // opaque; the real terminal background when transparent. In
        // xterm-256 opaque mode the painted background is itself quantized,
        // so the contrast space quantizes with it.
        let painted = if truecolor {
            palette.background
        } else {
            xterm_rgb(nearest_indexed(palette.background))
        };
        let contrast_background = if transparent {
            detected.unwrap_or(palette.background)
        } else {
            painted
        };
        let bg = contrast_background;

        let slots = ThemeSlots::derive(palette, bg);

        // Final text colors first: in indexed mode the colors that actually
        // sit on surfaces are the quantized ones, so surface constraints
        // must be built from the final output space.
        let text = |rgb: Rgb, min: f64| -> (Color, Rgb) {
            if truecolor {
                (color(rgb), rgb)
            } else {
                let index = nearest_contrasting_indexed(rgb, bg, min);
                (Color::Indexed(index), xterm_rgb(index))
            }
        };
        let (foreground_color, foreground_final) = text(slots.foreground, FOREGROUND_CONTRAST);
        let (muted_color, muted_final) = text(slots.muted, MUTED_CONTRAST);
        let (subtle_color, subtle_final) = text(slots.subtle, TEXT_CONTRAST);
        let (accent_color, accent_final) = text(slots.accent, TEXT_CONTRAST);
        let (warning_color, warning_final) = text(slots.warning, TEXT_CONTRAST);
        let (info_color, info_final) = text(slots.info, TEXT_CONTRAST);
        let (detail_color, detail_final) = text(slots.detail, TEXT_CONTRAST);
        let (secondary_color, secondary_final) = text(slots.secondary, TEXT_CONTRAST);
        let (positive_color, positive_final) = text(slots.positive, TEXT_CONTRAST);
        let (negative_color, negative_final) = text(slots.negative, TEXT_CONTRAST);

        // Per-combination surface constraints (contract in docs/theme.md):
        // persistent diff-line surfaces guarantee AA (4.5:1) for the primary
        // foreground and for the semantic foreground that renders on them;
        // transient highlight surfaces (cursor row, active range) guarantee
        // AA for foreground/subtle text and the WCAG 1.4.11 non-text level
        // (3.0:1) for every colored slot and muted text that can appear
        // there. Word emphasis renders its text in the primary foreground,
        // so its surface only needs the foreground guarantee.
        let highlight_constraints = highlight_constraints(TextSlots {
            foreground: foreground_final,
            muted: muted_final,
            subtle: subtle_final,
            accent: accent_final,
            warning: warning_final,
            info: info_final,
            detail: detail_final,
            secondary: secondary_final,
            positive: positive_final,
            negative: negative_final,
        });
        let added_line_constraints = [
            (foreground_final, TEXT_CONTRAST),
            (positive_final, TEXT_CONTRAST),
        ];
        let removed_line_constraints = [
            (foreground_final, TEXT_CONTRAST),
            (negative_final, TEXT_CONTRAST),
        ];
        let emphasis_constraints = [(foreground_final, TEXT_CONTRAST)];

        let surface = |hue: Rgb, alpha: f64, constraints: &[SurfaceConstraint]| -> Color {
            let derived = guarded_surface(hue, bg, alpha, constraints);
            if truecolor {
                color(derived)
            } else {
                Color::Indexed(quantize_surface(derived, bg, constraints))
            }
        };
        let selection_bg = surface(slots.foreground, 0.16, &highlight_constraints);
        let range_bg = surface(palette.info, 0.35, &highlight_constraints);
        let added_line_bg = surface(palette.positive, 0.15, &added_line_constraints);
        let removed_line_bg = surface(palette.negative, 0.15, &removed_line_constraints);
        let added_word_bg = surface(palette.positive, 0.40, &emphasis_constraints);
        let removed_word_bg = surface(palette.negative, 0.40, &emphasis_constraints);

        let background = if truecolor {
            color(palette.background)
        } else {
            Color::Indexed(nearest_indexed(palette.background))
        };

        Self {
            kind,
            transparent,
            contrast_background: bg,
            background,
            foreground: foreground_color,
            muted: muted_color,
            subtle: subtle_color,
            accent: accent_color,
            warning: warning_color,
            info: info_color,
            detail: detail_color,
            secondary: secondary_color,
            positive: positive_color,
            negative: negative_color,
            selection_bg,
            range_bg,
            added_line_bg,
            removed_line_bg,
            // Emphasized words render in the primary foreground on the
            // stronger tint: the tint stays punchy while the text keeps the
            // full AA guarantee regardless of the line's semantic color.
            added_word: Style::default()
                .fg(foreground_color)
                .add_modifier(Modifier::BOLD)
                .bg(added_word_bg),
            removed_word: Style::default()
                .fg(foreground_color)
                .add_modifier(Modifier::BOLD)
                .bg(removed_word_bg),
            gutter_added: Style::default().fg(text(slots.gutter_added_fg, MUTED_CONTRAST).0),
            gutter_removed: Style::default().fg(text(slots.gutter_removed_fg, MUTED_CONTRAST).0),
        }
    }

    /// Whether the base background is painted (`false` in transparent mode).
    #[allow(dead_code)]
    pub(crate) fn paints_background(&self) -> bool {
        !self.transparent
    }

    /// Style for the root fill and popup clears: always sets the themed
    /// foreground so otherwise-unstyled chrome cannot bypass it; sets the
    /// background only in opaque mode.
    pub(crate) fn base_style(&self) -> Style {
        let style = Style::default().fg(self.foreground);
        if self.transparent {
            style
        } else {
            style.bg(self.background)
        }
    }

    /// M17 channel color language: onboarding → accent.
    #[allow(dead_code)]
    pub(crate) fn channel_onboarding(&self) -> Color {
        self.accent
    }

    /// M17 channel color language: delegation → warning.
    #[allow(dead_code)]
    pub(crate) fn channel_delegation(&self) -> Color {
        self.warning
    }

    /// M17 channel color language: collaboration → info.
    #[allow(dead_code)]
    pub(crate) fn channel_collaboration(&self) -> Color {
        self.info
    }

    /// M17 channel color language: note → muted.
    #[allow(dead_code)]
    pub(crate) fn channel_note(&self) -> Color {
        self.muted
    }

    /// Single semantic color mapping used by every annotation surface.
    pub(crate) fn channel_color(&self, channel: crate::state::Channel) -> Color {
        match channel {
            crate::state::Channel::Onboarding => self.channel_onboarding(),
            crate::state::Channel::Delegation => self.channel_delegation(),
            crate::state::Channel::Collaboration => self.channel_collaboration(),
            crate::state::Channel::Note => self.channel_note(),
        }
    }
}

/// RGB value of a color the theme emitted (`Rgb` or `Indexed`); used by
/// tests to assert contrast in the final output space.
#[cfg(test)]
pub(crate) fn final_rgb(color: Color) -> Option<Rgb> {
    match color {
        Color::Rgb(r, g, b) => Some(Rgb::new(r, g, b)),
        Color::Indexed(index) => Some(xterm_rgb(index)),
        _ => None,
    }
}

/// Nearest xterm-256 color index for an RGB value, considering both the
/// 6x6x6 color cube (16..=231) and the grayscale ramp (232..=255).
pub(crate) fn nearest_indexed(color: Rgb) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    fn cube_component(value: u8) -> (u8, u8) {
        let mut best = (0u8, u16::MAX);
        for (index, level) in LEVELS.into_iter().enumerate() {
            let distance = u16::from(value.abs_diff(level));
            if distance < best.1 {
                best = (index as u8, distance);
            }
        }
        (best.0, LEVELS[best.0 as usize])
    }

    let Rgb {
        r: red,
        g: green,
        b: blue,
    } = color;
    let (ri, rv) = cube_component(red);
    let (gi, gv) = cube_component(green);
    let (bi, bv) = cube_component(blue);
    let cube_index = 16 + 36 * ri + 6 * gi + bi;

    // Only near-gray colors may land on the grayscale ramp: dark tints are
    // often numerically closer to a gray, but flattening the hue defeats
    // the point of a green/red cue.
    let spread = red.max(green).max(blue) - red.min(green).min(blue);
    if spread >= 12 {
        if cube_index == 16 {
            // A dark tint should stay a tint: bump the dominant channel to
            // the first cube level instead of flattening to black.
            let max = red.max(green).max(blue);
            return if red == max {
                16 + 36
            } else if green == max {
                16 + 6
            } else {
                16 + 1
            };
        }
        return cube_index;
    }

    let cube_distance = color_distance(color, Rgb::new(rv, gv, bv));
    // Grayscale ramp: 8, 18, ..., 238.
    let gray = (u16::from(red) + u16::from(green) + u16::from(blue)) / 3;
    let gray_step = ((gray.saturating_sub(8)).div_ceil(10)).min(23) as u8;
    let gray_value = 8 + 10 * gray_step;
    let gray_index = 232 + gray_step;
    let gray_distance = color_distance(color, Rgb::new(gray_value, gray_value, gray_value));

    if gray_distance < cube_distance {
        gray_index
    } else {
        cube_index
    }
}

fn color_distance(a: Rgb, b: Rgb) -> u32 {
    let dr = u32::from(a.r.abs_diff(b.r));
    let dg = u32::from(a.g.abs_diff(b.g));
    let db = u32::from(a.b.abs_diff(b.b));
    dr * dr + dg * dg + db * db
}

/// The RGB value an xterm-256 index renders as (standard palette).
pub(crate) fn xterm_rgb(index: u8) -> Rgb {
    // The 16 base colors vary by terminal; these are the common defaults.
    // The theme never derives indices below 16, this arm only serves
    // user-provided indexed values in tests/diagnostics.
    const BASE16: [u32; 16] = [
        0x000000, 0x800000, 0x008000, 0x808000, 0x000080, 0x800080, 0x008080, 0xc0c0c0, 0x808080,
        0xff0000, 0x00ff00, 0xffff00, 0x0000ff, 0xff00ff, 0x00ffff, 0xffffff,
    ];
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    match index {
        0..=15 => Rgb::hex(BASE16[index as usize]),
        16..=231 => {
            let value = index - 16;
            Rgb::new(
                LEVELS[(value / 36) as usize],
                LEVELS[((value / 6) % 6) as usize],
                LEVELS[(value % 6) as usize],
            )
        }
        232..=255 => {
            let gray = 8 + 10 * (index - 232);
            Rgb::new(gray, gray, gray)
        }
    }
}

/// Nearest xterm-256 index that still holds `min` contrast against
/// `background` (checked in indexed space). Falls back to the
/// highest-contrast index when the palette cannot reach the target.
pub(crate) fn nearest_contrasting_indexed(color: Rgb, background: Rgb, min: f64) -> u8 {
    let mut best: Option<(u8, u32)> = None;
    let mut best_contrast: (u8, f64) = (16, 0.0);
    for index in 16..=255u8 {
        let candidate = xterm_rgb(index);
        let ratio = contrast_ratio(candidate, background);
        if ratio > best_contrast.1 {
            best_contrast = (index, ratio);
        }
        if ratio < min {
            continue;
        }
        let distance = color_distance(color, candidate);
        if best.is_none_or(|(_, best_distance)| distance < best_distance) {
            best = Some((index, distance));
        }
    }
    best.map(|(index, _)| index).unwrap_or(best_contrast.0)
}

/// Quantize a surface color while keeping every text color that renders on
/// it readable in indexed space. First soften along the surface→background
/// blend path; if no quantized point on that path satisfies the
/// constraints, search the whole xterm palette for the closest index that
/// does, and only then fall back to the index with the best worst-case
/// margin. The returned index is always verified against the constraints
/// it reports satisfying — never assumed.
fn quantize_surface(surface: Rgb, background: Rgb, constraints: &[SurfaceConstraint]) -> u8 {
    for step in 0..=64u32 {
        let softened = blend(background, surface, f64::from(step) / 64.0);
        let index = nearest_indexed(softened);
        if satisfies(xterm_rgb(index), constraints) {
            return index;
        }
    }
    let worst_margin = |candidate: Rgb| -> f64 {
        constraints
            .iter()
            .map(|(text, min)| contrast_ratio(*text, candidate) - min)
            .fold(f64::INFINITY, f64::min)
    };
    let mut nearest_ok: Option<(u8, u32)> = None;
    let mut best_fallback: (u8, f64) = (16, f64::NEG_INFINITY);
    for index in 16..=255u8 {
        let candidate = xterm_rgb(index);
        let margin = worst_margin(candidate);
        if margin > best_fallback.1 {
            best_fallback = (index, margin);
        }
        if margin < 0.0 {
            continue;
        }
        let distance = color_distance(surface, candidate);
        if nearest_ok.is_none_or(|(_, best_distance)| distance < best_distance) {
            nearest_ok = Some((index, distance));
        }
    }
    nearest_ok
        .map(|(index, _)| index)
        .unwrap_or(best_fallback.0)
}

/// Outcome of the one-shot OSC 11 background query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BackgroundDetection {
    /// The terminal reported its background color. The reply was fully
    /// consumed, so nothing solicited can arrive later (a late DA1 fence
    /// reply is parsed and discarded by crossterm itself).
    Detected(Rgb),
    /// The terminal answered the DA1 fence without answering OSC 11 (or
    /// was ruled out up front). Usually conclusive, but the query library
    /// can return this without a fully validated fence — e.g. a reply
    /// fragmented immediately after its ESC — so callers still arm
    /// containment (docs/theme.md).
    Unsupported,
    /// The query failed without the DA1 fence confirming anything (timeout,
    /// I/O error, malformed reply). A late reply may still arrive, so the
    /// event loop arms [`crate::tui::osc_guard::OscTailGuard`].
    Inconclusive,
}

/// Bounded wall-clock budget for the OSC 11 + DA1 startup query.
/// Supported terminals answer in milliseconds; unsupported terminals are
/// detected through the DA1 fence well before this elapses. The full budget
/// is only spent on links where both replies are slow (for example SSH with
/// very high latency).
pub(crate) const OSC_QUERY_TIMEOUT: Duration = Duration::from_secs(1);

/// Query the terminal background color once via OSC 11.
///
/// Safety properties (all provided by `terminal-colorsaurus`):
/// * runs against `/dev/tty` (or the Windows console) with its own raw-mode
///   guard, restored on success, error, and unwind;
/// * DA1-fenced: replies are answered in order, so an "unsupported" verdict
///   is usually conclusive — but not guaranteed (see
///   [`BackgroundDetection::Unsupported`]), which is why callers arm
///   containment for every outcome except a parsed reply;
/// * bounded waiting (deadline across all reads) and no busy loops — a
///   zero-byte read (EOF/HUP) terminates the read immediately;
/// * on non-Unix/non-Windows platforms it returns `Unsupported` without
///   touching any terminal state or consuming input.
///
/// Callers must invoke this before the crossterm event reader starts.
pub(crate) fn detect_terminal_background(timeout: Duration) -> BackgroundDetection {
    let mut options = terminal_colorsaurus::QueryOptions::default();
    options.timeout = timeout;
    match terminal_colorsaurus::background_color(options) {
        Ok(color) => {
            let (r, g, b) = color.scale_to_8bit();
            BackgroundDetection::Detected(Rgb::new(r, g, b))
        }
        Err(terminal_colorsaurus::Error::UnsupportedTerminal(_)) => {
            BackgroundDetection::Unsupported
        }
        Err(_) => BackgroundDetection::Inconclusive,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ThemeModeConfig;
    use crate::theme::{BLACK, HIGHLIGHT_TEXT_CONTRAST, WHITE};

    fn ratio(color: Color, against: Rgb) -> f64 {
        contrast_ratio(
            final_rgb(color).expect("theme emits concrete colors"),
            against,
        )
    }

    /// The per-combination surface contract (docs/theme.md): every fg/bg
    /// pair the renderer can actually produce meets its documented target,
    /// asserted in the final output color space.
    fn assert_surface_contract(theme: &AppTheme, context: &str) {
        let surface_rgb = |color: Color| final_rgb(color).expect("surface is concrete");
        let fg = final_rgb(theme.foreground).unwrap();
        let all_surfaces = [
            theme.selection_bg,
            theme.range_bg,
            theme.added_line_bg,
            theme.removed_line_bg,
            theme.added_word.bg.expect("word bg"),
            theme.removed_word.bg.expect("word bg"),
        ];
        // Primary foreground: AA everywhere.
        for surface in all_surfaces {
            let surface = surface_rgb(surface);
            assert!(
                contrast_ratio(fg, surface) >= TEXT_CONTRAST,
                "{context}: foreground on {surface:?} = {}",
                contrast_ratio(fg, surface)
            );
        }
        // Word emphasis renders in the primary foreground.
        assert_eq!(theme.added_word.fg, Some(theme.foreground), "{context}");
        assert_eq!(theme.removed_word.fg, Some(theme.foreground), "{context}");
        // Persistent line surfaces: AA for their semantic foreground.
        let positive = final_rgb(theme.positive).unwrap();
        let negative = final_rgb(theme.negative).unwrap();
        assert!(
            contrast_ratio(positive, surface_rgb(theme.added_line_bg)) >= TEXT_CONTRAST,
            "{context}: positive on added_line_bg = {}",
            contrast_ratio(positive, surface_rgb(theme.added_line_bg))
        );
        assert!(
            contrast_ratio(negative, surface_rgb(theme.removed_line_bg)) >= TEXT_CONTRAST,
            "{context}: negative on removed_line_bg = {}",
            contrast_ratio(negative, surface_rgb(theme.removed_line_bg))
        );
        // Transient highlights: AA for subtle text, non-text minimum for
        // every colored slot and muted text.
        for highlight in [theme.selection_bg, theme.range_bg] {
            let highlight = surface_rgb(highlight);
            let subtle = final_rgb(theme.subtle).unwrap();
            assert!(
                contrast_ratio(subtle, highlight) >= TEXT_CONTRAST,
                "{context}: subtle on {highlight:?} = {}",
                contrast_ratio(subtle, highlight)
            );
            for slot in [
                theme.muted,
                theme.accent,
                theme.warning,
                theme.info,
                theme.detail,
                theme.secondary,
                theme.positive,
                theme.negative,
            ] {
                let slot = final_rgb(slot).unwrap();
                assert!(
                    contrast_ratio(slot, highlight) >= HIGHLIGHT_TEXT_CONTRAST,
                    "{context}: {slot:?} on {highlight:?} = {}",
                    contrast_ratio(slot, highlight)
                );
            }
        }
    }

    #[test]
    fn dark_and_light_defaults_keep_every_text_slot_readable() {
        for mode in [ThemeModeConfig::Dark, ThemeModeConfig::Light] {
            for truecolor in [true, false] {
                let theme = AppTheme::resolve(mode, false, truecolor, None);
                let bg = theme.contrast_background;
                assert!(ratio(theme.foreground, bg) >= FOREGROUND_CONTRAST);
                for slot in [
                    theme.subtle,
                    theme.accent,
                    theme.warning,
                    theme.info,
                    theme.detail,
                    theme.secondary,
                    theme.positive,
                    theme.negative,
                ] {
                    assert!(
                        ratio(slot, bg) >= TEXT_CONTRAST,
                        "mode={mode:?} truecolor={truecolor} slot={slot:?} ratio={}",
                        ratio(slot, bg)
                    );
                }
                assert!(ratio(theme.muted, bg) >= MUTED_CONTRAST);
                for gutter in [theme.gutter_added, theme.gutter_removed] {
                    assert!(ratio(gutter.fg.expect("gutter fg"), bg) >= MUTED_CONTRAST);
                }
                assert_surface_contract(&theme, &format!("mode={mode:?} truecolor={truecolor}"));
            }
        }
    }

    #[test]
    fn contrast_holds_for_detected_backgrounds_after_quantization() {
        // Required matrix: black, white, middle gray, #00a000, #0000b8,
        // plus the palette defaults (covered above). Assert numeric ratios
        // in the final output space for truecolor and xterm-256, opaque
        // and transparent.
        let backgrounds = [
            BLACK,
            WHITE,
            Rgb::new(128, 128, 128),
            Rgb::hex(0x00a000),
            Rgb::hex(0x0000b8),
            // Regression: bright blue whose quantized neighborhood used to
            // slip past the unchecked quantize_surface fallback.
            Rgb::hex(0x0070f8),
        ];
        for detected in backgrounds {
            for truecolor in [true, false] {
                for transparent in [true, false] {
                    let theme = AppTheme::resolve(
                        ThemeModeConfig::Auto,
                        transparent,
                        truecolor,
                        Some(detected),
                    );
                    // Transparent themes must guarantee contrast against the
                    // *detected terminal* background — the color really
                    // behind the glyphs — not a quantized approximation
                    // that is never painted.
                    if transparent {
                        assert_eq!(theme.contrast_background, detected);
                    }
                    let bg = theme.contrast_background;
                    // Some backgrounds (middle gray) cannot physically reach
                    // the AAA target: the guarantee is then the best
                    // achievable endpoint (see guard_contrast).
                    let fg_target = FOREGROUND_CONTRAST
                        .min(contrast_ratio(WHITE, bg).max(contrast_ratio(BLACK, bg)));
                    assert!(
                        ratio(theme.foreground, bg) >= fg_target,
                        "detected={detected:?} truecolor={truecolor} transparent={transparent} fg ratio={}",
                        ratio(theme.foreground, bg)
                    );
                    for slot in [
                        theme.subtle,
                        theme.accent,
                        theme.warning,
                        theme.info,
                        theme.detail,
                        theme.secondary,
                        theme.positive,
                        theme.negative,
                    ] {
                        assert!(
                            ratio(slot, bg) >= TEXT_CONTRAST,
                            "detected={detected:?} truecolor={truecolor} transparent={transparent} slot={slot:?} ratio={}",
                            ratio(slot, bg)
                        );
                    }
                    assert!(ratio(theme.muted, bg) >= MUTED_CONTRAST);
                    assert_surface_contract(
                        &theme,
                        &format!(
                            "detected={detected:?} truecolor={truecolor} transparent={transparent}"
                        ),
                    );
                    if !truecolor {
                        // Quantized slots really are indexed.
                        assert!(matches!(theme.foreground, Color::Indexed(_)));
                        assert!(matches!(theme.accent, Color::Indexed(_)));
                    }
                }
            }
        }
    }

    #[test]
    fn auto_mode_picks_light_and_dark_from_detected_luminance() {
        let light = AppTheme::resolve(ThemeModeConfig::Auto, true, true, Some(WHITE));
        assert_eq!(light.kind, ThemeKind::Light);
        // Middle gray sits above the black-text crossover: light.
        let gray = AppTheme::resolve(
            ThemeModeConfig::Auto,
            true,
            true,
            Some(Rgb::new(128, 128, 128)),
        );
        assert_eq!(gray.kind, ThemeKind::Light);
        let dark = AppTheme::resolve(ThemeModeConfig::Auto, true, true, Some(BLACK));
        assert_eq!(dark.kind, ThemeKind::Dark);
        let fallback = AppTheme::resolve(ThemeModeConfig::Auto, true, true, None);
        assert_eq!(fallback.kind, ThemeKind::Dark);
    }

    #[test]
    fn explicit_modes_ignore_detection() {
        let theme = AppTheme::resolve(ThemeModeConfig::Dark, false, true, Some(WHITE));
        assert_eq!(theme.kind, ThemeKind::Dark);
        let theme = AppTheme::resolve(ThemeModeConfig::Light, false, true, Some(BLACK));
        assert_eq!(theme.kind, ThemeKind::Light);
    }

    #[test]
    fn transparent_mode_only_suppresses_the_base_background() {
        let transparent = AppTheme::resolve(ThemeModeConfig::Dark, true, true, None);
        assert!(!transparent.paints_background());
        assert_eq!(transparent.base_style().bg, None);
        assert_eq!(transparent.base_style().fg, Some(transparent.foreground));
        // Surfaces still paint.
        assert!(matches!(transparent.selection_bg, Color::Rgb(..)));
        assert!(matches!(transparent.added_line_bg, Color::Rgb(..)));

        let opaque = AppTheme::resolve(ThemeModeConfig::Dark, false, true, None);
        assert!(opaque.paints_background());
        assert_eq!(opaque.base_style().bg, Some(opaque.background));
    }

    #[test]
    fn xterm_quantization_stays_hue_faithful_and_indexed() {
        assert_eq!(nearest_indexed(Rgb::hex(0x3fb950)), 71);
        // Dark green tint may not flatten to black or gray.
        let index = nearest_indexed(Rgb::hex(0x12261e));
        let rgb = xterm_rgb(index);
        assert!(rgb.g > rgb.r && rgb.g >= rgb.b, "index {index} rgb {rgb:?}");
        // Pure grays use the ramp.
        let gray_index = nearest_indexed(Rgb::new(120, 120, 120));
        assert!((232..=255).contains(&gray_index));
    }

    #[test]
    fn channel_colors_map_to_their_semantic_slots() {
        let theme = AppTheme::default();
        assert_eq!(theme.channel_onboarding(), theme.accent);
        assert_eq!(theme.channel_delegation(), theme.warning);
        assert_eq!(theme.channel_collaboration(), theme.info);
        assert_eq!(theme.channel_note(), theme.muted);
        assert_eq!(
            theme.channel_color(crate::state::Channel::Onboarding),
            theme.accent
        );
        assert_eq!(
            theme.channel_color(crate::state::Channel::Delegation),
            theme.warning
        );
        assert_eq!(
            theme.channel_color(crate::state::Channel::Collaboration),
            theme.info
        );
        assert_eq!(
            theme.channel_color(crate::state::Channel::Note),
            theme.muted
        );
        // The four channels stay visually distinguishable.
        let colors = [
            theme.channel_onboarding(),
            theme.channel_delegation(),
            theme.channel_collaboration(),
            theme.channel_note(),
        ];
        for (i, a) in colors.iter().enumerate() {
            for b in colors.iter().skip(i + 1) {
                assert_ne!(a, b);
            }
        }
    }
}
