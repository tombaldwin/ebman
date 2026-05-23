//! Generic pixel-art frame renderer for ratatui splash / about scenes.
//!
//! Each app brings its own frame data (`&[&str]` per frame) and its
//! own palette function (`char → Option<Color>`); the rendering loop
//! is shared. Logical pixels render as **two-cell** `██` blocks so
//! they're roughly square in the terminal cell grid; transparent
//! cells (palette returns `None`) render as `  ` (two spaces).
//!
//! Used by ebman's beanstalk-growth splash. Pgman could lift its
//! own splash onto this once the renderer pays off there too.

use ratatui::layout::Alignment;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Render one frame of pixel art into a Vec of centre-aligned
/// [`Line`]s. Each frame row contributes one Line; each visual column
/// is two cells wide (`██` block or two spaces) so pixels are roughly
/// square against typical 2:1 terminal cell aspect.
///
/// `palette_fn` maps each glyph in the frame to a colour. Returning
/// `None` paints that cell transparent (two blank cells). Out-of-bounds
/// columns (when a row is shorter than `cols`) render as the result of
/// `palette_fn(' ')` — passing a palette that maps space → None gives
/// the conventional "pad with transparent" behaviour.
pub fn render_frame(
    frame_rows: &[&str],
    palette_fn: impl Fn(char) -> Option<Color>,
    cols: usize,
) -> Vec<Line<'static>> {
    frame_rows
        .iter()
        .map(|row| {
            let chars: Vec<char> = row.chars().collect();
            let spans: Vec<Span> = (0..cols)
                .map(|col| {
                    let key = chars.get(col).copied().unwrap_or(' ');
                    match palette_fn(key) {
                        Some(color) => Span::styled("██", Style::default().fg(color)),
                        None => Span::raw("  "),
                    }
                })
                .collect();
            Line::from(spans).alignment(Alignment::Center)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{render_frame, Color};

    #[test]
    fn renders_double_cell_pixels_per_glyph() {
        // Two-row frame, three columns. Palette maps '#' to red, ' '
        // (and anything else) to transparent.
        let frame = &["#.#", ".#."];
        let palette = |c: char| -> Option<Color> {
            if c == '#' {
                Some(Color::Rgb(255, 0, 0))
            } else {
                None
            }
        };
        let lines = render_frame(frame, palette, 3);
        assert_eq!(lines.len(), 2, "one line per row");
        // Every span is exactly two cells wide — the █-block convention.
        for line in &lines {
            assert_eq!(line.spans.len(), 3, "one span per column");
            for span in &line.spans {
                assert_eq!(
                    span.content.chars().count(),
                    2,
                    "every cell renders to exactly two terminal cells",
                );
            }
        }
        // First row: ██ in red, transparent, ██ in red.
        assert_eq!(lines[0].spans[0].content.as_ref(), "██");
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(255, 0, 0)));
        assert_eq!(lines[0].spans[1].content.as_ref(), "  ");
        assert_eq!(lines[0].spans[1].style.fg, None);
    }

    #[test]
    fn pads_short_rows_via_palette_default() {
        // Row is two chars, cols = 5 → the last 3 columns get the
        // default ' ' character. Palette returns None for space, so
        // those three cells render transparent.
        let frame = &["##"];
        let palette = |c: char| -> Option<Color> {
            if c == '#' {
                Some(Color::Rgb(0, 255, 0))
            } else {
                None
            }
        };
        let lines = render_frame(frame, palette, 5);
        assert_eq!(lines[0].spans.len(), 5);
        for (i, span) in lines[0].spans.iter().enumerate() {
            if i < 2 {
                assert_eq!(span.content.as_ref(), "██");
            } else {
                assert_eq!(
                    span.content.as_ref(),
                    "  ",
                    "col {i} should be transparent pad"
                );
            }
        }
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let lines = render_frame(&[], |_| Some(Color::Black), 10);
        assert!(lines.is_empty());
    }
}
