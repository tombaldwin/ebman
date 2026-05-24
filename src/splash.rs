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

#[cfg(test)]
mod tests {
    use super::{
        splash_pixel, splash_scene_lines, splash_shows_scene, SPLASH_FRAME_0, SPLASH_FRAME_1,
        SPLASH_FRAME_10, SPLASH_FRAME_11, SPLASH_FRAME_12, SPLASH_FRAME_13, SPLASH_FRAME_2,
        SPLASH_FRAME_3, SPLASH_FRAME_4, SPLASH_FRAME_5, SPLASH_FRAME_6, SPLASH_FRAME_7,
        SPLASH_FRAME_8, SPLASH_FRAME_9,
    };

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
