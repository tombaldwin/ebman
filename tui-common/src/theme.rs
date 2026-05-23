//! Theme primitives shared across TUI apps.
//!
//! Each app keeps its own `Theme` struct (the slot list is
//! app-specific — ebman has `health_red` / `status_ready`, pgman has
//! its own DB-state colours). What lives here is the small set of
//! genuinely-shared pieces:
//!
//! - [`IconStyle`] — the glyph-set preference (`Unicode` /
//!   `Ascii` / `Powerline`) every TUI ends up needing.
//! - [`contrast_text_for`] — WCAG-luminance black-or-white picker
//!   for foreground text against a coloured background. Pure
//!   function so the algorithm isn't duplicated per app.

use ratatui::style::Color;

/// Operator's preferred glyph set. `Unicode` is the default; `Ascii`
/// is for terminals that can't render common Unicode glyphs at all
/// (CI logs, some legacy emulators); `Powerline` opts into the
/// patched-font / Nerd Font range (E0B0+ separators, MDI icons) when
/// the user knows their font supports it (or the `font_probe`
/// auto-detected it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconStyle {
    Unicode,
    Ascii,
    /// Opt-in glyph set for terminals using a Powerline-patched or
    /// Nerd Font. Uses U+E0B0+ separators in headers / breadcrumbs /
    /// footers; otherwise behaves like Unicode for icons whose
    /// Powerline equivalent isn't a clear win. When the font isn't
    /// installed the user sees unknown-glyph boxes — opting in is
    /// explicit so that's acceptable.
    Powerline,
}

/// Pick black or white text against a coloured background, whichever
/// gives better contrast. Uses the WCAG perceived-luminance formula
/// (0.299·R + 0.587·G + 0.114·B), thresholded at 140/255 to favour
/// black text on bright pill backgrounds. Falls back to
/// `non_rgb_fallback` for non-RGB `Color` variants
/// (`Color::Black` / `Color::Reset` / etc.) — caller passes whichever
/// foreground their theme uses as the "everyone else" default, so
/// themed pill text stays readable across terminals that re-map the
/// 16-colour palette.
pub fn contrast_text_for(bg: Color, non_rgb_fallback: Color) -> Color {
    match bg {
        Color::Rgb(r, g, b) => {
            let luminance = (299 * r as u32 + 587 * g as u32 + 114 * b as u32) / 1000;
            if luminance > 140 {
                Color::Black
            } else {
                Color::White
            }
        }
        _ => non_rgb_fallback,
    }
}

#[cfg(test)]
mod tests {
    use super::{contrast_text_for, Color};

    #[test]
    fn contrast_text_picks_black_on_bright_rgb() {
        // Bright yellow (luminance ~205) → black text.
        assert_eq!(
            contrast_text_for(Color::Rgb(240, 210, 130), Color::Magenta),
            Color::Black
        );
        // Bright green (~180) → black text.
        assert_eq!(
            contrast_text_for(Color::Rgb(140, 220, 160), Color::Magenta),
            Color::Black
        );
    }

    #[test]
    fn contrast_text_picks_white_on_dark_rgb() {
        // Dark blue (~50) → white text.
        assert_eq!(
            contrast_text_for(Color::Rgb(40, 60, 90), Color::Magenta),
            Color::White
        );
        // Dark red (~75) → white text.
        assert_eq!(
            contrast_text_for(Color::Rgb(170, 30, 40), Color::Magenta),
            Color::White
        );
    }

    #[test]
    fn contrast_text_uses_fallback_for_non_rgb() {
        // The helper must not panic on Color::Black / Color::Reset /
        // 16-palette indices etc., and should return the caller's
        // preferred fallback for them.
        let fallback = Color::Rgb(220, 222, 230);
        assert_eq!(contrast_text_for(Color::Reset, fallback), fallback);
        assert_eq!(contrast_text_for(Color::Black, fallback), fallback);
        assert_eq!(contrast_text_for(Color::Indexed(4), fallback), fallback);
    }
}
