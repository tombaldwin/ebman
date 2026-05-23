//! Centred-overlay sizing for ratatui popups.
//!
//! Every TUI ends up with the same set of modal-overlay sizing
//! decisions (small confirms, picker / palette, text-dump panel,
//! wide log-viewer / diff). Without a single source of truth each
//! overlay tends to pick its own `centered_rect(W, H)` values and
//! the result feels visually inconsistent.
//!
//! The `OverlaySize` enum + `centered_overlay` helper here centralise
//! the size table. Apps consume the four named categories rather than
//! raw percentages — `centered_overlay(OverlaySize::Picker, frame)`
//! says what the overlay *is* (a picker), not how big it should be.

use ratatui::layout::{Constraint, Direction, Layout, Rect};

/// Standard overlay size categories. Single source of truth so every
/// overlay reads the same proportions and the TUI feels coherent
/// across modal modes.
///
/// Categories are picked by content shape, not by overlay name, so a
/// future overlay can route to whichever category fits without
/// negotiating with the rest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverlaySize {
    /// Small modal — Y/N confirm, brief menu / picker. Sized so the
    /// content takes the eye but the surrounding context is still
    /// visible (the operator hasn't lost where they were).
    Small,
    /// Picker / palette / input + list. Just enough room for a
    /// dozen rows of fuzzy-match candidates.
    Picker,
    /// Read-only text dump (events, alarms, history, help, …).
    /// Comfortable reading width without dominating the screen.
    Text,
    /// Wide content — log tail, diagnostics, side-by-side diffs,
    /// the bug-report payload. These need horizontal space the
    /// table-shape Text category doesn't have.
    Wide,
}

/// Pick a centered rect for a given overlay category. The size
/// values live in [`overlay_dims`] so re-tuning every overlay is a
/// single-line change.
pub fn centered_overlay(size: OverlaySize, area: Rect) -> Rect {
    let (w, h) = overlay_dims(size);
    centered_rect(w, h, area)
}

/// Pure helper: percent dimensions per category. Extracted for
/// testability and so the size table is grep-able in one place.
pub fn overlay_dims(size: OverlaySize) -> (u16, u16) {
    match size {
        OverlaySize::Small => (50, 40),
        OverlaySize::Picker => (60, 60),
        OverlaySize::Text => (70, 70),
        OverlaySize::Wide => (85, 80),
    }
}

/// Lower-level helper: centre a `percent_x × percent_y` rect inside
/// `r`. Exposed so callers who genuinely need a non-standard size
/// can still use the same centring math; the named-category path
/// above is what most overlays should reach for.
pub fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let v = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(v[1])[1]
}

#[cfg(test)]
mod tests {
    use super::{overlay_dims, OverlaySize};

    #[test]
    fn overlay_dims_ordering_makes_sense() {
        // Each category is strictly bigger than the previous one on at
        // least one axis. If a future change accidentally inverts this
        // (e.g. Small > Picker), the visual hierarchy breaks.
        let (sw, sh) = overlay_dims(OverlaySize::Small);
        let (pw, ph) = overlay_dims(OverlaySize::Picker);
        let (tw, th) = overlay_dims(OverlaySize::Text);
        let (ww, wh) = overlay_dims(OverlaySize::Wide);
        assert!(sw <= pw && sh <= ph, "Picker not smaller than Small");
        assert!(pw <= tw && ph <= th, "Text not bigger than Picker");
        assert!(tw <= ww && th <= wh, "Wide not bigger than Text");
    }

    #[test]
    fn overlay_dims_are_within_legal_percent_range() {
        // Categories are stored as integer percentages; > 100 would
        // panic ratatui's Layout, < 10 would render an invisibly tiny
        // popup. Pin both ends.
        for size in [
            OverlaySize::Small,
            OverlaySize::Picker,
            OverlaySize::Text,
            OverlaySize::Wide,
        ] {
            let (w, h) = overlay_dims(size);
            assert!((10..=100).contains(&w), "{size:?} width {w} out of range");
            assert!((10..=100).contains(&h), "{size:?} height {h} out of range");
        }
    }
}
