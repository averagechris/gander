//! UI-toolkit-agnostic derived theme core.
//!
//! This module owns the pure RGB palette, color math, and palette→semantic-slot
//! derivation shared by terminal and future non-terminal renderers. It has no
//! dependency on ratatui/crossterm or terminal color types.

/// Contrast target for primary foreground text (WCAG AAA normal text).
pub(crate) const FOREGROUND_CONTRAST: f64 = 7.0;
/// Contrast target for standard chrome text slots (WCAG AA normal text).
pub(crate) const TEXT_CONTRAST: f64 = 4.5;
/// Contrast target for deliberately de-emphasized text and gutter bars
/// (WCAG AA large-text/graphics level).
pub(crate) const MUTED_CONTRAST: f64 = 3.0;
/// Contrast target for colored semantic text sitting on *transient* highlight
/// surfaces. See docs/theme.md for the full contract.
pub(crate) const HIGHLIGHT_TEXT_CONTRAST: f64 = 3.0;

/// An 8-bit RGB color used for theme math.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub(crate) const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub(crate) const fn hex(value: u32) -> Self {
        Self {
            r: (value >> 16) as u8,
            g: (value >> 8) as u8,
            b: value as u8,
        }
    }
}

pub(crate) const BLACK: Rgb = Rgb::new(0, 0, 0);
pub(crate) const WHITE: Rgb = Rgb::new(255, 255, 255);

/// WCAG 2.x relative luminance of an sRGB color, in `0.0..=1.0`.
pub(crate) fn relative_luminance(color: Rgb) -> f64 {
    fn channel(value: u8) -> f64 {
        let c = f64::from(value) / 255.0;
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }
    0.2126 * channel(color.r) + 0.7152 * channel(color.g) + 0.0722 * channel(color.b)
}

/// WCAG contrast ratio between two colors, in `1.0..=21.0`.
pub(crate) fn contrast_ratio(a: Rgb, b: Rgb) -> f64 {
    let la = relative_luminance(a);
    let lb = relative_luminance(b);
    let (hi, lo) = if la >= lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}

/// Linear per-channel blend: `alpha` of `top` over `bottom`.
pub(crate) fn blend(top: Rgb, bottom: Rgb, alpha: f64) -> Rgb {
    let alpha = alpha.clamp(0.0, 1.0);
    let mix = |t: u8, b: u8| -> u8 {
        (f64::from(t) * alpha + f64::from(b) * (1.0 - alpha)).round() as u8
    };
    Rgb::new(
        mix(top.r, bottom.r),
        mix(top.g, bottom.g),
        mix(top.b, bottom.b),
    )
}

/// Repair `candidate` until it reaches `min` contrast against `background`.
pub(crate) fn guard_contrast(candidate: Rgb, background: Rgb, min: f64) -> Rgb {
    if contrast_ratio(candidate, background) >= min {
        return candidate;
    }
    let white_ratio = contrast_ratio(WHITE, background);
    let black_ratio = contrast_ratio(BLACK, background);
    let endpoint = if white_ratio >= min && black_ratio >= min {
        if relative_luminance(candidate) >= relative_luminance(background) {
            WHITE
        } else {
            BLACK
        }
    } else if white_ratio >= black_ratio {
        WHITE
    } else {
        BLACK
    };
    for step in 1..=64u32 {
        let repaired = blend(endpoint, candidate, f64::from(step) / 64.0);
        if contrast_ratio(repaired, background) >= min {
            return repaired;
        }
    }
    endpoint
}

/// A readability constraint on a surface: this text color must keep at least
/// this ratio on top of it.
pub(crate) type SurfaceConstraint = (Rgb, f64);

pub(crate) fn satisfies(surface: Rgb, constraints: &[SurfaceConstraint]) -> bool {
    constraints
        .iter()
        .all(|(text, min)| contrast_ratio(*text, surface) >= *min)
}

/// A tinted surface pulled back toward the background until every text color
/// that renders on it keeps its required contrast.
pub(crate) fn guarded_surface(
    hue: Rgb,
    background: Rgb,
    alpha: f64,
    constraints: &[SurfaceConstraint],
) -> Rgb {
    let surface = blend(hue, background, alpha);
    if satisfies(surface, constraints) {
        return surface;
    }
    for step in 1..=64u32 {
        let softened = blend(background, surface, f64::from(step) / 64.0);
        if satisfies(softened, constraints) {
            return softened;
        }
    }
    background
}

/// Light or dark presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ThemeKind {
    Dark,
    Light,
}

/// The small base palette every chrome slot is derived from.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BasePalette {
    pub background: Rgb,
    pub foreground: Rgb,
    pub accent: Rgb,
    pub positive: Rgb,
    pub negative: Rgb,
    pub info: Rgb,
}

impl BasePalette {
    pub(crate) fn for_kind(kind: ThemeKind) -> Self {
        match kind {
            ThemeKind::Dark => Self {
                background: Rgb::hex(0x0d1117),
                foreground: Rgb::hex(0xe6edf3),
                accent: Rgb::hex(0xd29922),
                positive: Rgb::hex(0x3fb950),
                negative: Rgb::hex(0xf85149),
                info: Rgb::hex(0x58a6ff),
            },
            ThemeKind::Light => Self {
                background: Rgb::hex(0xffffff),
                foreground: Rgb::hex(0x1f2328),
                accent: Rgb::hex(0x9a6700),
                positive: Rgb::hex(0x1a7f37),
                negative: Rgb::hex(0xcf222e),
                info: Rgb::hex(0x0969da),
            },
        }
    }
}

/// Luminance boundary between "dark background" and "light background".
const LIGHT_BACKGROUND_LUMINANCE: f64 = 0.179;

pub(crate) fn kind_for_background(background: Rgb) -> ThemeKind {
    if relative_luminance(background) > LIGHT_BACKGROUND_LUMINANCE {
        ThemeKind::Light
    } else {
        ThemeKind::Dark
    }
}

/// Pure RGB semantic slots derived from a base palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ThemeSlots {
    pub foreground: Rgb,
    pub muted: Rgb,
    pub subtle: Rgb,
    pub accent: Rgb,
    pub warning: Rgb,
    pub info: Rgb,
    pub detail: Rgb,
    pub secondary: Rgb,
    pub positive: Rgb,
    pub negative: Rgb,
    pub selection_bg: Rgb,
    pub range_bg: Rgb,
    pub added_line_bg: Rgb,
    pub removed_line_bg: Rgb,
    pub added_word_fg: Rgb,
    pub added_word_bg: Rgb,
    pub removed_word_fg: Rgb,
    pub removed_word_bg: Rgb,
    pub gutter_added_fg: Rgb,
    pub gutter_removed_fg: Rgb,
}

impl ThemeSlots {
    /// Derive RGB slots for `palette` against the caller-selected effective
    /// contrast background.
    pub(crate) fn derive(palette: BasePalette, bg: Rgb) -> Self {
        let foreground = guard_contrast(palette.foreground, bg, FOREGROUND_CONTRAST);
        let subtle = guard_contrast(blend(foreground, bg, 0.72), bg, TEXT_CONTRAST);
        let muted = guard_contrast(blend(foreground, bg, 0.50), bg, MUTED_CONTRAST);
        let accent = guard_contrast(palette.accent, bg, TEXT_CONTRAST);
        let warning = guard_contrast(
            blend(palette.accent, palette.negative, 0.45),
            bg,
            TEXT_CONTRAST,
        );
        let info = guard_contrast(palette.info, bg, TEXT_CONTRAST);
        let detail = guard_contrast(
            blend(palette.info, palette.positive, 0.5),
            bg,
            TEXT_CONTRAST,
        );
        let secondary = guard_contrast(
            blend(palette.negative, palette.info, 0.5),
            bg,
            TEXT_CONTRAST,
        );
        let positive = guard_contrast(palette.positive, bg, TEXT_CONTRAST);
        let negative = guard_contrast(palette.negative, bg, TEXT_CONTRAST);

        let highlight_constraints = highlight_constraints(TextSlots {
            foreground,
            muted,
            subtle,
            accent,
            warning,
            info,
            detail,
            secondary,
            positive,
            negative,
        });
        let added_line_constraints = [(foreground, TEXT_CONTRAST), (positive, TEXT_CONTRAST)];
        let removed_line_constraints = [(foreground, TEXT_CONTRAST), (negative, TEXT_CONTRAST)];
        let emphasis_constraints = [(foreground, TEXT_CONTRAST)];

        Self {
            foreground,
            muted,
            subtle,
            accent,
            warning,
            info,
            detail,
            secondary,
            positive,
            negative,
            selection_bg: guarded_surface(foreground, bg, 0.16, &highlight_constraints),
            range_bg: guarded_surface(palette.info, bg, 0.35, &highlight_constraints),
            added_line_bg: guarded_surface(palette.positive, bg, 0.15, &added_line_constraints),
            removed_line_bg: guarded_surface(palette.negative, bg, 0.15, &removed_line_constraints),
            added_word_fg: foreground,
            added_word_bg: guarded_surface(palette.positive, bg, 0.40, &emphasis_constraints),
            removed_word_fg: foreground,
            removed_word_bg: guarded_surface(palette.negative, bg, 0.40, &emphasis_constraints),
            gutter_added_fg: guard_contrast(palette.positive, bg, MUTED_CONTRAST),
            gutter_removed_fg: guard_contrast(palette.negative, bg, MUTED_CONTRAST),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct TextSlots {
    pub foreground: Rgb,
    pub muted: Rgb,
    pub subtle: Rgb,
    pub accent: Rgb,
    pub warning: Rgb,
    pub info: Rgb,
    pub detail: Rgb,
    pub secondary: Rgb,
    pub positive: Rgb,
    pub negative: Rgb,
}

pub(crate) fn highlight_constraints(text: TextSlots) -> [SurfaceConstraint; 10] {
    [
        (text.foreground, TEXT_CONTRAST),
        (text.subtle, TEXT_CONTRAST),
        (text.muted, HIGHLIGHT_TEXT_CONTRAST),
        (text.accent, HIGHLIGHT_TEXT_CONTRAST),
        (text.warning, HIGHLIGHT_TEXT_CONTRAST),
        (text.info, HIGHLIGHT_TEXT_CONTRAST),
        (text.detail, HIGHLIGHT_TEXT_CONTRAST),
        (text.secondary, HIGHLIGHT_TEXT_CONTRAST),
        (text.positive, HIGHLIGHT_TEXT_CONTRAST),
        (text.negative, HIGHLIGHT_TEXT_CONTRAST),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_surface_contract(slots: ThemeSlots, context: &str) {
        let all_surfaces = [
            slots.selection_bg,
            slots.range_bg,
            slots.added_line_bg,
            slots.removed_line_bg,
            slots.added_word_bg,
            slots.removed_word_bg,
        ];
        for surface in all_surfaces {
            assert!(
                contrast_ratio(slots.foreground, surface) >= TEXT_CONTRAST,
                "{context}: foreground on {surface:?} = {}",
                contrast_ratio(slots.foreground, surface)
            );
        }
        assert_eq!(slots.added_word_fg, slots.foreground, "{context}");
        assert_eq!(slots.removed_word_fg, slots.foreground, "{context}");
        assert!(
            contrast_ratio(slots.positive, slots.added_line_bg) >= TEXT_CONTRAST,
            "{context}: positive on added_line_bg = {}",
            contrast_ratio(slots.positive, slots.added_line_bg)
        );
        assert!(
            contrast_ratio(slots.negative, slots.removed_line_bg) >= TEXT_CONTRAST,
            "{context}: negative on removed_line_bg = {}",
            contrast_ratio(slots.negative, slots.removed_line_bg)
        );
        for highlight in [slots.selection_bg, slots.range_bg] {
            assert!(
                contrast_ratio(slots.subtle, highlight) >= TEXT_CONTRAST,
                "{context}: subtle on {highlight:?} = {}",
                contrast_ratio(slots.subtle, highlight)
            );
            for slot in [
                slots.muted,
                slots.accent,
                slots.warning,
                slots.info,
                slots.detail,
                slots.secondary,
                slots.positive,
                slots.negative,
            ] {
                assert!(
                    contrast_ratio(slot, highlight) >= HIGHLIGHT_TEXT_CONTRAST,
                    "{context}: {slot:?} on {highlight:?} = {}",
                    contrast_ratio(slot, highlight)
                );
            }
        }
    }

    #[test]
    fn blend_is_exact_at_edges_and_midpoint() {
        let a = Rgb::new(10, 200, 30);
        let b = Rgb::new(240, 4, 90);
        assert_eq!(blend(a, b, 0.0), b);
        assert_eq!(blend(a, b, 1.0), a);
        assert_eq!(blend(a, b, 0.5), Rgb::new(125, 102, 60));
    }

    #[test]
    fn contrast_ratio_matches_wcag_reference_values() {
        assert!((contrast_ratio(WHITE, BLACK) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(BLACK, WHITE) - 21.0).abs() < 1e-9);
        assert!((contrast_ratio(WHITE, WHITE) - 1.0).abs() < 1e-9);
        let gray = Rgb::new(128, 128, 128);
        assert!((contrast_ratio(gray, WHITE) - 3.9497).abs() < 1e-3);
    }

    #[test]
    fn guard_contrast_picks_an_endpoint_that_can_meet_the_target() {
        let gray = Rgb::new(128, 128, 128);
        let repaired = guard_contrast(Rgb::new(200, 200, 200), gray, 4.5);
        assert!(
            contrast_ratio(repaired, gray) >= 4.5,
            "got {}",
            contrast_ratio(repaired, gray)
        );
        assert!(relative_luminance(repaired) < relative_luminance(gray));
        let repaired = guard_contrast(Rgb::new(25, 25, 25), BLACK, 4.5);
        assert!(contrast_ratio(repaired, BLACK) >= 4.5);
    }

    #[test]
    fn dark_and_light_defaults_keep_every_text_slot_readable() {
        for kind in [ThemeKind::Dark, ThemeKind::Light] {
            let palette = BasePalette::for_kind(kind);
            let slots = ThemeSlots::derive(palette, palette.background);
            assert!(contrast_ratio(slots.foreground, palette.background) >= FOREGROUND_CONTRAST);
            for slot in [
                slots.subtle,
                slots.accent,
                slots.warning,
                slots.info,
                slots.detail,
                slots.secondary,
                slots.positive,
                slots.negative,
            ] {
                assert!(
                    contrast_ratio(slot, palette.background) >= TEXT_CONTRAST,
                    "kind={kind:?} slot={slot:?}"
                );
            }
            assert!(contrast_ratio(slots.muted, palette.background) >= MUTED_CONTRAST);
            assert!(contrast_ratio(slots.gutter_added_fg, palette.background) >= MUTED_CONTRAST);
            assert!(contrast_ratio(slots.gutter_removed_fg, palette.background) >= MUTED_CONTRAST);
            assert_surface_contract(slots, &format!("kind={kind:?}"));
        }
    }

    #[test]
    fn derives_slots_for_detected_backgrounds() {
        for detected in [
            BLACK,
            WHITE,
            Rgb::new(128, 128, 128),
            Rgb::hex(0x00a000),
            Rgb::hex(0x0000b8),
            Rgb::hex(0x0070f8),
        ] {
            let palette = BasePalette::for_kind(kind_for_background(detected));
            let slots = ThemeSlots::derive(palette, detected);
            let fg_target = FOREGROUND_CONTRAST
                .min(contrast_ratio(WHITE, detected).max(contrast_ratio(BLACK, detected)));
            assert!(contrast_ratio(slots.foreground, detected) >= fg_target);
            assert_surface_contract(slots, &format!("detected={detected:?}"));
        }
    }

    #[test]
    fn auto_kind_picks_light_and_dark_from_detected_luminance() {
        assert_eq!(kind_for_background(WHITE), ThemeKind::Light);
        assert_eq!(
            kind_for_background(Rgb::new(128, 128, 128)),
            ThemeKind::Light
        );
        assert_eq!(kind_for_background(BLACK), ThemeKind::Dark);
    }
}
