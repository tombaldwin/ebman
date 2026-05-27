//! Pixel-art splash scene — a beanstalk growing out of its pot
//! (the `ebman` = Elastic *Beanstalk* gag: watch the thing sprout).
//! Used by the boot splash and the `:about` overlay.
//!
//! Each glyph in a frame is a palette key, not a literal. The
//! renderer ([`splash_scene_lines`]) paints every non-`.` key as a
//! **two-cell** `██` block coloured via [`splash_pixel`] — two cells
//! wide so each logical pixel is roughly square (terminal cells are
//! ~1:2). The `.` key is transparent (rendered as two blank cells).
//!
//! 14 frames take the env from bare pot to full bloom: 8 keyframes
//! from the JSON design source (indices 0, 1, 3, 5, 7, 9, 11, 13)
//! interleaved with 6 hand-drawn in-betweens (2, 4, 6, 8, 10, 12)
//! that smooth the largest visual jumps. The pot stays rooted across
//! every frame so the growing motion reads against a fixed anchor.
//!
//! Palette keys: `#` outline (dark green) · `G` leaf · `L` leaf
//! highlight · `F` bud · `P` pot · `T` soil. All frames are 20×20.

const SPLASH_FRAME_0: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_1: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: sprout extends to a 2-pixel stem before the wings appear.
const SPLASH_FRAME_2: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_3: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "........G#G.........",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: wings shed, stem grows another two rows.
const SPLASH_FRAME_4: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_5: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: small leaf bud forming at the top of the stem before
// the full cluster fills in.
const SPLASH_FRAME_6: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "........###.........",
    ".......#GGG#........",
    "........###.........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_7: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: upper sprout begins to climb above the lower cluster.
const SPLASH_FRAME_8: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_9: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: small upper bud forms before the branched upper cluster.
const SPLASH_FRAME_10: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    "........###.........",
    ".......#GGG#........",
    "........###.........",
    ".........#..........",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_11: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    "......##.#.##.......",
    ".....#GG#G#GG#......",
    ".....#GLGGGLG#......",
    "......##.#.##.......",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: stem extends one row above the upper cluster before
// the bud forms its cap.
const SPLASH_FRAME_12: &[&str] = &[
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    "......##.#.##.......",
    ".....#GG#G#GG#......",
    ".....#GLGGGLG#......",
    "......##.#.##.......",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_13: &[&str] = &[
    "....................",
    ".........##.........",
    "........#FF#........",
    ".........##.........",
    ".........#..........",
    "......##.#.##.......",
    ".....#GG#G#GG#......",
    ".....#GLGGGLG#......",
    "......##.#.##.......",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

/// Map a pixel-art palette key to an RGB colour. `None` = a
/// transparent cell (rendered as two blank cells).
fn splash_pixel(key: char) -> Option<(u8, u8, u8)> {
    Some(match key {
        '#' => (28, 107, 46),   // outline (dark green)
        'G' => (76, 192, 106),  // leaf body
        'L' => (139, 224, 156), // leaf highlight
        'F' => (255, 208, 36),  // bud (yellow)
        'P' => (184, 115, 46),  // pot (brown)
        'T' => (107, 67, 33),   // soil (dark brown)
        _ => return None,
    })
}

/// Number of art rows in the splash scene (every frame is the same
/// height). Used by callers to size the splash card / about popup.
pub const SPLASH_SCENE_ROWS: usize = SPLASH_FRAME_0.len();

/// Number of art columns per frame. Every frame is exactly this
/// wide; the renderer relies on that to avoid per-frame jitter when
/// alignment recentres. Doubled (×2) at render time so logical
/// pixels are roughly square in the terminal cell grid.
pub const SPLASH_SCENE_COLS: usize = 20;

/// Build the coloured lines for the splash scene at `frame`. Each
/// non-transparent pixel is a **two-cell** `██` block in its palette
/// colour, so the logical pixels are roughly square. Used by the
/// boot splash and the `:about` overlay.
///
/// 14 frames advance at ≈180 ms each (6 30 ms ticks); the grow phase
/// (empty pot → bud) takes ~2.5 s so it fits inside the boot splash's
/// 3 s minimum duration. After the grow phase finishes the final
/// bud-blossom frame lingers for an extra ~2 s
/// (`FINAL_FRAME_HOLD_TICKS`) before wrapping back to the empty pot
/// — gives the bloom time to land visually in the looping `:about`
/// view rather than snapping straight back to the empty pot.
pub fn splash_scene_lines(frame: u64) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::style::Color;
    const FRAMES: [&[&str]; 14] = [
        SPLASH_FRAME_0,
        SPLASH_FRAME_1,
        SPLASH_FRAME_2,
        SPLASH_FRAME_3,
        SPLASH_FRAME_4,
        SPLASH_FRAME_5,
        SPLASH_FRAME_6,
        SPLASH_FRAME_7,
        SPLASH_FRAME_8,
        SPLASH_FRAME_9,
        SPLASH_FRAME_10,
        SPLASH_FRAME_11,
        SPLASH_FRAME_12,
        SPLASH_FRAME_13,
    ];
    // 14 frames × 6 ticks × 30 ms = 2520 ms for the grow phase, well
    // inside the 3 s SPLASH_MIN_DURATION (so the boot splash always
    // lands on the bud frame). Then 67 ticks × 30 ms ≈ 2 s of hold on
    // the last frame before the cycle wraps — only visible in the
    // looping `:about` view since the boot splash dismisses during
    // the hold anyway. Total cycle: 4530 ms.
    const TICKS_PER_FRAME: usize = 6;
    const FINAL_FRAME_HOLD_TICKS: usize = 67;
    let grow_ticks = FRAMES.len() * TICKS_PER_FRAME;
    let cycle_ticks = grow_ticks + FINAL_FRAME_HOLD_TICKS;
    let tick = (frame as usize) % cycle_ticks;
    let scene_idx = if tick < grow_ticks {
        tick / TICKS_PER_FRAME
    } else {
        // Hold phase: stay on the final bloom frame.
        FRAMES.len() - 1
    };
    let scene = FRAMES[scene_idx];
    // The pixel→`██` rendering loop lives in tui-common::splash so
    // pgman (and any future sibling) shares the same machinery. We
    // wrap our local 6-key palette in a closure that hands back a
    // ratatui Color so the generic renderer doesn't have to know
    // about RGB tuples.
    tui_common::splash::render_frame(
        scene,
        |key| splash_pixel(key).map(|(r, g, b)| Color::Rgb(r, g, b)),
        SPLASH_SCENE_COLS,
    )
}

/// Pure: whether the boot splash has room for the pixel-art scene.
/// Below this it falls back to the compact text-only card. Scene is
/// 40 cells wide (20 px × 2) and 20 rows tall; the threshold is the
/// card chrome budget (+ borders / padding) on top.
pub fn splash_shows_scene(w: u16, h: u16) -> bool {
    w >= 48 && h >= 30
}

/// Spinner animation frames used by the boot-time splash. Each frame
/// advances every ~3 frames in [`draw_splash`].
const SPLASH_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Draw one splash frame to the terminal. Composes the
/// [`splash_scene_lines`] beanstalk-growth scene (when the terminal
/// has room) with the tagline + byline + connecting-to-AWS spinner.
/// In Powerline mode (resolved by `font_probe` before this runs)
/// pills the tagline + byline + a `v{VERSION}` tab on the top
/// border; falls back to plain text otherwise.
///
/// Frame counter drives both the spinner cycle and the one-shot
/// border-glow easing (cyan → magenta → cyan over ~1s). Called from
/// `main()`'s splash loop until the initial AWS context lands.
pub fn draw_splash(
    terminal: &mut crate::Tui,
    frame: u64,
    icons: &str,
) -> color_eyre::eyre::Result<()> {
    use ratatui::layout::{Alignment, Constraint, Direction, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

    terminal.draw(|f| {
        let area = f.area();
        let powerline = icons == "powerline";
        let show_scene = splash_shows_scene(area.width, area.height);
        let (card_w, card_h): (u16, u16) = if show_scene { (46, 30) } else { (52, 9) };
        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(card_h),
                Constraint::Min(0),
            ])
            .split(area);
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(card_w),
                Constraint::Min(0),
            ])
            .split(v[1]);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(""));
        if show_scene {
            lines.extend(splash_scene_lines(frame));
            lines.push(Line::from(""));
        }
        if powerline {
            let tag_bg = Color::Rgb(35, 45, 60);
            let tag_fg = Color::Rgb(170, 180, 200);
            let cloud = "\u{f0c2}"; // fa-cloud, stable across Nerd Font releases
            lines.push(
                Line::from(vec![
                    Span::styled("\u{e0b6}", Style::default().fg(tag_bg)),
                    Span::styled(
                        format!(" {cloud}  k9s-style TUI for AWS Elastic Beanstalk "),
                        Style::default().fg(tag_fg).bg(tag_bg),
                    ),
                    Span::styled("\u{e0b4}", Style::default().fg(tag_bg)),
                ])
                .alignment(Alignment::Center),
            );
            let by_bg = Color::Rgb(50, 40, 75);
            let by_fg = Color::Rgb(220, 195, 245);
            lines.push(
                Line::from(vec![
                    Span::styled("\u{e0b6}", Style::default().fg(by_bg)),
                    Span::styled(
                        " by Tom Baldwin · Polymorphism Ltd ",
                        Style::default().fg(by_fg).bg(by_bg),
                    ),
                    Span::styled("\u{e0b4}", Style::default().fg(by_bg)),
                ])
                .alignment(Alignment::Center),
            );
        } else {
            lines.push(
                Line::from(Span::styled(
                    "k9s-style TUI for AWS Elastic Beanstalk",
                    Style::default().fg(Color::Rgb(150, 155, 170)),
                ))
                .alignment(Alignment::Center),
            );
            lines.push(
                Line::from(Span::styled(
                    "by Tom Baldwin · Polymorphism Ltd",
                    Style::default().fg(Color::Rgb(180, 140, 230)),
                ))
                .alignment(Alignment::Center),
            );
        }
        lines.push(Line::from(""));
        // Spinner: advance every 3 frames → ~10 fps spin at 30 ms ticks.
        // Dots: advance every 8 frames → ~240 ms per dot.
        let spinner = SPLASH_SPINNER[(frame as usize / 3) % SPLASH_SPINNER.len()];
        let dots = ".".repeat((frame as usize / 8) % 4);
        lines.push(
            Line::from(Span::styled(
                format!("{spinner} connecting to AWS{dots}"),
                Style::default()
                    .fg(Color::Rgb(255, 200, 120))
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center),
        );

        // One-shot border glow: cyan → magenta → cyan over the first
        // ~1 s of splash time, then settles to cyan.
        const BORDER_SPOTLIGHT_FRAMES: f64 = 33.0;
        let border_phase = (frame as f64 / BORDER_SPOTLIGHT_FRAMES).clamp(0.0, 1.0);
        let border_glow = if border_phase < 0.5 {
            border_phase * 2.0
        } else {
            (1.0 - border_phase) * 2.0
        };
        let border_hue = 180.0 + border_glow * 120.0;
        let (br, bg, bb) = hsl_to_rgb(border_hue, 0.60, 0.65);
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(br, bg, bb)));
        if powerline {
            let tab_bg = Color::Rgb(60, 50, 80);
            let tab_fg = Color::Rgb(220, 200, 250);
            let version = format!(" v{} ", env!("CARGO_PKG_VERSION"));
            let title = Line::from(vec![
                Span::styled("\u{e0b6}", Style::default().fg(tab_bg)),
                Span::styled(
                    version,
                    Style::default()
                        .fg(tab_fg)
                        .bg(tab_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("\u{e0b4}", Style::default().fg(tab_bg)),
            ]);
            block = block.title(title).title_alignment(Alignment::Center);
        }
        f.render_widget(Paragraph::new(lines).block(block), h[1]);
    })?;
    Ok(())
}

/// Standard HSL → RGB. `h` in degrees 0-360, `s` and `l` in 0.0-1.0.
/// Used by [`draw_splash`] for the boot border-glow easing. Crate-
/// internal — no external callers.
pub(crate) fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = h / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = if h_prime < 1.0 {
        (c, x, 0.0)
    } else if h_prime < 2.0 {
        (x, c, 0.0)
    } else if h_prime < 3.0 {
        (0.0, c, x)
    } else if h_prime < 4.0 {
        (0.0, x, c)
    } else if h_prime < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = l - c / 2.0;
    let to_u8 = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
}

#[cfg(test)]
mod tests {
    use super::{
        hsl_to_rgb, splash_pixel, splash_scene_lines, splash_shows_scene, SPLASH_FRAME_0,
        SPLASH_FRAME_1, SPLASH_FRAME_10, SPLASH_FRAME_11, SPLASH_FRAME_12, SPLASH_FRAME_13,
        SPLASH_FRAME_2, SPLASH_FRAME_3, SPLASH_FRAME_4, SPLASH_FRAME_5, SPLASH_FRAME_6,
        SPLASH_FRAME_7, SPLASH_FRAME_8, SPLASH_FRAME_9,
    };

    #[test]
    fn hsl_to_rgb_red() {
        let (r, g, b) = hsl_to_rgb(0.0, 1.0, 0.5);
        assert_eq!((r, g, b), (255, 0, 0));
    }

    #[test]
    fn hsl_to_rgb_cyan_and_magenta() {
        let (r, g, b) = hsl_to_rgb(180.0, 1.0, 0.5);
        assert_eq!((r, g, b), (0, 255, 255));
        let (r, g, b) = hsl_to_rgb(300.0, 1.0, 0.5);
        assert_eq!((r, g, b), (255, 0, 255));
    }

    #[test]
    fn hsl_to_rgb_clamps_to_valid_range() {
        // u8 enforces 0..=255 by type, so additionally assert that
        // moderate-saturation mid-lightness inputs produce visible
        // (non-collapsed) outputs across the wheel, and that hue is
        // wrapped modulo 360 (h=-30 should equal h=330).
        for h in [-30.0, 0.0, 90.0, 180.0, 270.0, 360.0, 720.0] {
            let (r, g, b) = hsl_to_rgb(h, 0.7, 0.65);
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            assert!(max > min, "hue {h} collapsed to greyscale");
        }
        assert_eq!(hsl_to_rgb(-30.0, 0.7, 0.65), hsl_to_rgb(330.0, 0.7, 0.65));
        assert_eq!(hsl_to_rgb(0.0, 0.7, 0.65), hsl_to_rgb(360.0, 0.7, 0.65));
        // Zero saturation collapses to greyscale at lightness * 255.
        let (r, g, b) = hsl_to_rgb(123.0, 0.0, 0.5);
        assert_eq!(r, g);
        assert_eq!(g, b);
    }

    const ALL_SPLASH_FRAMES: [&[&str]; 14] = [
        SPLASH_FRAME_0,
        SPLASH_FRAME_1,
        SPLASH_FRAME_2,
        SPLASH_FRAME_3,
        SPLASH_FRAME_4,
        SPLASH_FRAME_5,
        SPLASH_FRAME_6,
        SPLASH_FRAME_7,
        SPLASH_FRAME_8,
        SPLASH_FRAME_9,
        SPLASH_FRAME_10,
        SPLASH_FRAME_11,
        SPLASH_FRAME_12,
        SPLASH_FRAME_13,
    ];

    /// Mirrors `splash_scene_lines`'s internal constants so the cycle
    /// probes match the actual frame stride / hold duration.
    const TICKS_PER_FRAME: u64 = 6;
    /// Total number of frames in the animation cycle. Mirrors the
    /// FRAMES array length in `splash_scene_lines`.
    const FRAME_COUNT: u64 = 14;
    /// Hold-on-last-frame tick count. Mirrors `FINAL_FRAME_HOLD_TICKS`.
    const FINAL_FRAME_HOLD_TICKS: u64 = 67;

    #[test]
    fn splash_shows_scene_gates_on_terminal_size() {
        // Roomy → scene.
        assert!(splash_shows_scene(120, 50));
        assert!(splash_shows_scene(48, 30)); // exact threshold
                                             // Too narrow / too short → text-only fallback.
        assert!(!splash_shows_scene(47, 50));
        assert!(!splash_shows_scene(120, 29));
        assert!(!splash_shows_scene(40, 20));
    }

    #[test]
    fn splash_pixel_maps_keys_and_treats_dot_as_transparent() {
        // Every key used in the art frames must resolve to a colour
        // (apart from `.`, which is transparent by design).
        for frame in ALL_SPLASH_FRAMES {
            for row in frame {
                for ch in row.chars() {
                    if ch != '.' {
                        assert!(
                            splash_pixel(ch).is_some(),
                            "art key '{ch}' has no palette entry"
                        );
                    }
                }
            }
        }
        assert_eq!(splash_pixel('.'), None);
    }

    #[test]
    fn all_splash_frames_have_identical_dimensions() {
        // Frames must agree on row count *and* column count — the
        // pot anchor is meant to stay rooted while only the plant
        // above it changes, so any frame-to-frame size shift would
        // re-centre and read as a jitter.
        let h = SPLASH_FRAME_0.len();
        for f in ALL_SPLASH_FRAMES {
            assert_eq!(f.len(), h, "frame height mismatch");
            for row in f {
                assert_eq!(
                    row.chars().count(),
                    super::SPLASH_SCENE_COLS,
                    "row width mismatch in frame: {row:?}"
                );
            }
        }
    }

    #[test]
    fn splash_scene_frames_render_identical_width() {
        // Every frame must render to the same total cell width, or
        // centre-alignment shifts the sprite sideways between frames
        // (a visible jitter).
        let line_w = |l: &ratatui::text::Line| -> usize {
            l.spans.iter().map(|s| s.content.chars().count()).sum()
        };
        // Probe one frame from each cycle slot; the sampled tick
        // offsets span the full N-frame cycle.
        let widths: Vec<usize> = (0..FRAME_COUNT)
            .map(|i| {
                let ls = splash_scene_lines(i * TICKS_PER_FRAME);
                line_w(&ls[0])
            })
            .collect();
        assert!(
            widths.iter().all(|&w| w == widths[0]),
            "frames render at differing widths: {widths:?}"
        );
    }

    #[test]
    fn splash_scene_lines_renders_full_scene_and_animates() {
        // Include the foreground colour as well as the glyph — the
        // animation is mostly composition (more pixels appear as the
        // beanstalk grows), so a glyph-only compare may miss subtle
        // recolours; a (color, glyph) compare catches both.
        let render = |ls: &[ratatui::text::Line]| {
            ls.iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| format!("{:?}{}", s.style.fg, s.content))
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        // Every frame renders every art row.
        let f0 = splash_scene_lines(0);
        assert_eq!(f0.len(), SPLASH_FRAME_0.len());
        // Probe several cycle slots — at least one pair must differ.
        let r0 = render(&f0);
        let r5 = render(&splash_scene_lines(5 * TICKS_PER_FRAME));
        let r13 = render(&splash_scene_lines(13 * TICKS_PER_FRAME));
        assert!(
            r0 != r5 || r0 != r13,
            "frames should differ — scene is not animating"
        );
        // The full cycle wraps back to frame 0 after the grow phase
        // PLUS the hold-on-last-frame tail.
        let cycle_ticks = FRAME_COUNT * TICKS_PER_FRAME + FINAL_FRAME_HOLD_TICKS;
        assert_eq!(render(&splash_scene_lines(cycle_ticks)), r0);
        // Square pixels: a painted cell is the two-block "██" (sampled
        // from the last frame which has the most pixels lit).
        assert!(splash_scene_lines(13 * TICKS_PER_FRAME)
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.content.as_ref() == "██"));
    }

    #[test]
    fn last_frame_holds_for_two_seconds_before_wrapping() {
        // Pin the hold behaviour: every tick inside the hold window
        // renders the bud-blossom frame, but the very next tick after
        // the hold ends wraps back to the empty-pot frame.
        let render = |ls: &[ratatui::text::Line]| {
            ls.iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| format!("{:?}{}", s.style.fg, s.content))
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        let grow_ticks = FRAME_COUNT * TICKS_PER_FRAME;
        let bud_frame = render(&splash_scene_lines(grow_ticks - 1));
        // First tick of the hold window — still bud.
        assert_eq!(render(&splash_scene_lines(grow_ticks)), bud_frame);
        // Middle of the hold window — still bud.
        assert_eq!(
            render(&splash_scene_lines(grow_ticks + FINAL_FRAME_HOLD_TICKS / 2)),
            bud_frame
        );
        // Last tick of the hold window — still bud.
        assert_eq!(
            render(&splash_scene_lines(grow_ticks + FINAL_FRAME_HOLD_TICKS - 1)),
            bud_frame
        );
        // One tick past the hold window — wrapped back to frame 0.
        let pot_frame = render(&splash_scene_lines(0));
        assert_eq!(
            render(&splash_scene_lines(grow_ticks + FINAL_FRAME_HOLD_TICKS)),
            pot_frame
        );
    }

    #[test]
    fn last_frame_hold_is_approximately_two_seconds() {
        // Pin the design intent: the hold is ~2 s in wall-clock time
        // (animation tick = 30 ms). If a future bump knocks the hold
        // below 1.5 s or above 2.5 s, that's almost certainly a bug.
        let hold_ms = FINAL_FRAME_HOLD_TICKS * 30;
        assert!(
            (1500..=2500).contains(&hold_ms),
            "hold is {hold_ms} ms — should sit near 2 s"
        );
    }

    #[test]
    fn splash_grow_phase_completes_within_min_duration() {
        // Pin the speed-up: the GROW phase (empty pot → bud) should
        // finish in less than the splash's 3 s minimum duration so
        // the boot splash always lands on the final-bloom frame
        // before the table replaces it. The hold-on-last-frame tail
        // continues past 3 s but only the looping `:about` view ever
        // sees it — the boot splash dismisses during the hold.
        let grow_ms = FRAME_COUNT * TICKS_PER_FRAME * 30;
        assert!(
            grow_ms < 3000,
            "grow phase is {grow_ms} ms — exceeds 3 s splash duration; bump TICKS_PER_FRAME down"
        );
    }
}
