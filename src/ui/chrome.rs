//! Shared drawing vocabulary: blocks, pills, glyphs, colours and the
//! small formatters every panel reuses.
//!
//! Moved verbatim out of `src/ui.rs` (5,046 lines) in the 0.31 split;
//! visibility widened to `pub(crate)` where a sibling needs it, and
//! nothing else changed. `use super::*` picks up the shared vocabulary
//! the way `detail.rs` and `overlays.rs` already do.

use super::*;

pub(crate) const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
pub(crate) const ASCII_SPINNER: &[&str] = &["|", "/", "-", "\\"];

pub(crate) fn rounded_block(theme: &Theme, active: bool) -> Block<'static> {
    let color = if active {
        theme.border_active
    } else {
        theme.border_idle
    };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
}

pub(crate) fn titled_block(
    theme: &Theme,
    raw_title: &str,
    active: bool,
    accent: Color,
) -> Block<'static> {
    let trimmed = raw_title.trim();
    let decorated = match theme.icons {
        IconStyle::Ascii => format!("[ {trimmed} ]"),
        // U+E0B6 / U+E0B4: rounded powerline left/right caps frame the title
        // like a tab on a folder. Renders as boxes when the font isn't
        // installed; documented in the config description.
        IconStyle::Powerline => format!(" {trimmed} "),
        IconStyle::Unicode => format!("[ ◆ {trimmed} ◆ ]"),
    };
    rounded_block(theme, active).title(Span::styled(
        decorated,
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    ))
}

pub(crate) fn pill(text: &str, fg: Color, bg: Color) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    )
}

/// Glyph for the pending-actions pill, gated on the active icon style.
/// `⏳` (U+23F3) is unicode-only — operators on `icons = "ascii"`
/// terminals saw box-tofu before this; falls back to a `*` tag now.
pub(crate) fn pending_glyph(theme: &Theme) -> &'static str {
    match theme.icons {
        IconStyle::Ascii => "* ",
        _ => "⏳ ",
    }
}

/// Pick the ascii fallback for a decorative glyph when the operator's
/// font can't render unicode (`icons = "ascii"` — the mode's contract
/// is "stays readable when the font lacks the glyphs", so raw literals
/// in draw paths are stragglers).
pub(crate) fn glyph<'a>(icons: IconStyle, unicode: &'a str, ascii: &'a str) -> &'a str {
    match icons {
        IconStyle::Ascii => ascii,
        _ => unicode,
    }
}

/// Glyph for the multi-select-active pill.
pub(crate) fn multi_select_glyph(theme: &Theme) -> &'static str {
    match theme.icons {
        IconStyle::Ascii => "+ ",
        _ => "▶ ",
    }
}

/// Glyph for the incident banner pill. `🚨` is unicode-only; ascii
/// terminals get a loud `!!` tag instead of tofu.
pub(crate) fn incident_glyph(theme: &Theme) -> &'static str {
    match theme.icons {
        IconStyle::Ascii => "!! ",
        _ => "🚨 ",
    }
}

pub(crate) fn health_dot(health: &str, theme: &Theme) -> Span<'static> {
    let c = health_color(health, theme);
    let glyph = match theme.icons {
        IconStyle::Ascii => "*",
        // U+F111 Nerd-Font solid circle reads identically to U+25CF in
        // Powerline-patched fonts but is part of the Nerd Font set, which
        // gives a tiny consistency win when the rest of the chrome uses
        // private-use glyphs.
        IconStyle::Powerline => "\u{f111}",
        IconStyle::Unicode => "●",
    };
    Span::styled(glyph, Style::default().fg(c).add_modifier(Modifier::BOLD))
}

pub(crate) fn spinner(elapsed_ms: u128, icons: IconStyle) -> &'static str {
    match icons {
        // Powerline-targeted fonts include the braille range, so the same
        // animation reads well without needing a separate frame set.
        IconStyle::Unicode | IconStyle::Powerline => {
            SPINNER_FRAMES[(elapsed_ms / 100) as usize % SPINNER_FRAMES.len()]
        }
        IconStyle::Ascii => ASCII_SPINNER[(elapsed_ms / 100) as usize % ASCII_SPINNER.len()],
    }
}

pub(crate) fn tab_icon(t: DetailTab, icons: IconStyle) -> &'static str {
    match (icons, t) {
        (IconStyle::Unicode, DetailTab::Health) => "♥",
        (IconStyle::Unicode, DetailTab::Events) => "⚡",
        (IconStyle::Unicode, DetailTab::Instances) => "▣",
        (IconStyle::Unicode, DetailTab::Metrics) => "▆",
        (IconStyle::Unicode, DetailTab::Queue) => "✉",
        (IconStyle::Unicode, DetailTab::Logs) => "≣",
        (IconStyle::Unicode, DetailTab::Config) => "⚙",
        // Powerline / Nerd Font Material Design glyphs. Each is distinct so
        // the tab strip remains readable even when icons collapse onto a
        // single line in the boot splash / detail header.
        (IconStyle::Powerline, DetailTab::Health) => "\u{f02d1}", // heart-pulse
        (IconStyle::Powerline, DetailTab::Events) => "\u{f0e7}",  // flash
        (IconStyle::Powerline, DetailTab::Instances) => "\u{f048b}", // server
        (IconStyle::Powerline, DetailTab::Metrics) => "\u{f0680}", // chart-line
        (IconStyle::Powerline, DetailTab::Queue) => "\u{f01ee}",  // email-outline
        (IconStyle::Powerline, DetailTab::Logs) => "\u{f021a}",   // text-box
        (IconStyle::Powerline, DetailTab::Config) => "\u{f0493}", // cog
        // ASCII fallbacks: one letter per tab so each is distinguishable.
        (IconStyle::Ascii, DetailTab::Health) => "H",
        (IconStyle::Ascii, DetailTab::Events) => "E",
        (IconStyle::Ascii, DetailTab::Instances) => "I",
        (IconStyle::Ascii, DetailTab::Metrics) => "M",
        (IconStyle::Ascii, DetailTab::Queue) => "Q",
        (IconStyle::Ascii, DetailTab::Logs) => "L",
        (IconStyle::Ascii, DetailTab::Config) => "C",
    }
}

pub(crate) fn micro_bar(value: i64, max: i64, width: usize) -> String {
    // `value < 0` and the `full.min(width)` below are both redundant
    // given the `clamp(0.0, 1.0)`: a negative value clamps to 0.0 and
    // yields no glyphs, and `frac <= 1.0` bounds `full` by `width`. The
    // 2026-08-26 sweep reports both as survivable and is right.
    //
    // Kept anyway, and not as an oversight: they are belt-and-braces on
    // float arithmetic feeding a `usize` loop count, and deleting them
    // to move a mutation score would trade a real safety margin for a
    // number. `max <= 0` is NOT redundant — it guards the divide.
    if max <= 0 || width == 0 || value < 0 {
        return String::new();
    }
    let frac = (value as f64 / max as f64).clamp(0.0, 1.0);
    let total_eighths = (frac * (width as f64) * 8.0).round() as usize;
    let full = total_eighths / 8;
    let rem = total_eighths % 8;
    let mut out = String::new();
    for _ in 0..full.min(width) {
        out.push('█');
    }
    if full < width && rem > 0 {
        out.push(match rem {
            1 => '▏',
            2 => '▎',
            3 => '▍',
            4 => '▌',
            5 => '▋',
            6 => '▊',
            7 => '▉',
            _ => ' ',
        });
    }
    out
}

pub(crate) const SPARKLINE_WIDTH: usize = 10;
/// How wide each divider fill string is. Ratatui truncates per-column, so any
/// value ≥ max column width works.
pub(crate) const DIVIDER_FILL_WIDTH: usize = 200;

/// Pure helper: pick a (start, end) window of indices to render such that
/// `cursor` is inside `[start, end)` and `end - start <= budget`. Window
/// stays as low as possible (anchor to top when items fit, slide down only
/// when the cursor passes the visible area). Used by the saved-configs
/// overlay's scroll logic and tested directly.
pub(crate) fn visible_window(cursor: usize, total: usize, budget: usize) -> (usize, usize) {
    if total == 0 {
        return (0, 0);
    }
    let budget = budget.max(1).min(total);
    if total <= budget {
        return (0, total);
    }
    // Slide so the cursor stays inside. If cursor is in the upper portion,
    // anchor to 0; if in the lower portion, end at total; otherwise centre.
    let half = budget / 2;
    let start = cursor.saturating_sub(half);
    let start = start.min(total - budget);
    (start, start + budget)
}

pub(crate) fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let prefix: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{prefix}…")
}

/// Pad a string to at least `width` chars with spaces. Uses
/// char-count rather than byte-count because Region / env names
/// can contain non-ASCII (rare but legal).
pub(crate) fn pad_right(s: &str, width: usize) -> String {
    let n = s.chars().count();
    if n >= width {
        s.to_string()
    } else {
        let mut out = String::with_capacity(width);
        out.push_str(s);
        for _ in 0..(width - n) {
            out.push(' ');
        }
        out
    }
}

pub(crate) fn humanize_duration(secs: u64) -> String {
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
    } else {
        format!("{}d{}h", secs / 86_400, (secs % 86_400) / 3600)
    }
}

pub(crate) fn kv<'a>(key: &'a str, value: &'a str, theme: &Theme) -> Vec<Span<'a>> {
    vec![
        Span::styled(format!("{key}: "), Style::default().fg(theme.muted)),
        Span::styled(
            value.to_string(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ]
}

pub(crate) fn sep(theme: &Theme) -> Span<'static> {
    // U+E0B1 — thin powerline separator — reads as a real divider in
    // Powerline-patched fonts and falls back to a tofu box otherwise.
    let glyph = if theme.icons == IconStyle::Powerline {
        "  \u{e0b1}  "
    } else {
        "  •  "
    };
    Span::styled(glyph, Style::default().fg(theme.muted))
}

/// Pure: ASCII-case-insensitive "is `s` any of these?" predicate. Cheap
/// alternative to `s.to_lowercase().as_str()` matching against a fixed
/// option list — saves a per-call `String` allocation in the table-row
/// render hot path, where `health` / `status` strings come from AWS in
/// known-case form anyway.
pub(crate) fn ieq_any(s: &str, options: &[&str]) -> bool {
    options.iter().any(|o| s.eq_ignore_ascii_case(o))
}

/// Cursor / row-selection marker prepended to highlighted rows in lists +
/// tables. Powerline-mode users get the filled U+E0B0 right-triangle so
/// the marker matches the rest of the ribbon aesthetic; everyone else gets
/// the half-block ▌ that doesn't need a patched font.
pub(crate) fn cursor_marker(theme: &Theme) -> &'static str {
    if theme.icons == IconStyle::Powerline {
        "\u{e0b0} "
    } else {
        "▌ "
    }
}

/// Insertion-point caret glyph used as the blinking cursor in the command
/// bar / filter bar / quick-jump bar / picker / typed-name confirm. ASCII
/// stays on `_` (no Unicode needed in low-feature terminals); everything
/// else uses U+258E (a thin vertical block) which actually reads as a
/// terminal cursor rather than an underscore character.
pub(crate) fn caret_glyph(theme: &Theme) -> &'static str {
    if theme.icons == IconStyle::Ascii {
        "_"
    } else {
        "\u{258e}"
    }
}

/// Render a single-line text input as `before-caret` + caret glyph +
/// `after-caret`, so the blinking caret sits at `cursor_col` (a char
/// offset) instead of always at the end. Shared by the `TextInput`-backed
/// input renderers (quickjump / palette / …) now that those inputs
/// support mid-string cursor movement. `cursor_col` past the end clamps
/// to the end (caret after all text).
pub(crate) fn input_caret_spans(
    text: &str,
    cursor_col: usize,
    text_style: Style,
    caret_style: Style,
    theme: &Theme,
) -> Vec<Span<'static>> {
    let byte = text
        .char_indices()
        .nth(cursor_col)
        .map(|(i, _)| i)
        .unwrap_or(text.len());
    let (before, after) = text.split_at(byte);
    vec![
        Span::styled(before.to_string(), text_style),
        Span::styled(caret_glyph(theme), caret_style),
        Span::styled(after.to_string(), text_style),
    ]
}

/// Pure: chevron used in the non-Powerline group-banner row to mark the
/// start of an app section (`── ▶ app-name ──`). Powerline mode renders
/// its own ribbon and never calls this — but we return a sensible glyph
/// anyway so the helper is total.
pub(crate) fn separator_glyph(icons: IconStyle) -> &'static str {
    match icons {
        IconStyle::Ascii => ">",
        // U+25B6 BLACK RIGHT-POINTING TRIANGLE — BMP, single-cell in every
        // standard monospace font. Mirrors the Powerline E0B0 wedge in
        // intent (forward direction, calls attention to the section break).
        _ => "▶",
    }
}

/// Warning glyph — `⚠ ` in unicode/powerline modes, `! ` in ascii so
/// `icons = "ascii"` operators don't get box-tofu instead. Caller
/// includes the trailing space.
pub(crate) fn warn_glyph(icons: IconStyle) -> &'static str {
    match icons {
        IconStyle::Ascii => "! ",
        _ => "⚠ ",
    }
}

/// Hint / suggestion glyph — `💡 ` (lightbulb) in unicode/powerline,
/// `? ` in ascii. Used by context-aware footer hints (`:why` / `:alarms`
/// suggestions when the status slot is empty).
pub(crate) fn hint_glyph(icons: IconStyle) -> &'static str {
    match icons {
        IconStyle::Ascii => "? ",
        _ => "💡 ",
    }
}

/// "Newer platform version available" glyph — `↑` in unicode/powerline,
/// `^` in ascii. Flags stale platforms in the envs-table PLATFORM column.
pub(crate) fn stale_glyph(icons: IconStyle) -> &'static str {
    match icons {
        IconStyle::Ascii => "^",
        _ => "↑",
    }
}

/// Severity-stripe glyph for toast notification bodies. Half-block
/// `▎` in unicode/powerline, `|` in ascii.
pub(crate) fn stripe_glyph(icons: IconStyle) -> &'static str {
    match icons {
        IconStyle::Ascii => "|",
        _ => "▎",
    }
}

pub(crate) fn sparkline_for(
    samples: Option<&std::collections::VecDeque<String>>,
    theme: &Theme,
    pulse_last: bool,
) -> Line<'static> {
    let Some(samples) = samples else {
        return Line::from(Span::raw(" ".repeat(SPARKLINE_WIDTH)));
    };
    let pad = SPARKLINE_WIDTH.saturating_sub(samples.len());
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(SPARKLINE_WIDTH);
    if pad > 0 {
        spans.push(Span::raw(" ".repeat(pad)));
    }
    let start = samples.len().saturating_sub(SPARKLINE_WIDTH);
    let visible: Vec<&String> = samples.iter().skip(start).collect();
    let visible_len = visible.len();
    for (i, h) in visible.iter().enumerate() {
        let color = health_color(h, theme);
        // Two-tone styling so the cell reads as a coloured bar under
        // the row-highlight's `Modifier::REVERSED`. fg=full bright,
        // bg=darker shade — the swap flips to (darker fg, bright bg)
        // on the selected row, painting the bar in the darker shade.
        // Bar shape: `▇` is the lower 7/8 block, so the top 1/8 sliver
        // shows the bg colour as a darker cap (or a brighter cap on
        // the inverted highlighted row). Uniform across the bar — the
        // earlier dim-leading-third gradient added confusion without
        // operational signal (everything inside a 5-min window is
        // "recent" enough).
        let darker = scale_rgb(color, 0.6);
        let style = Style::default().fg(color).bg(darker);
        // Pulse the rightmost cell when the caller flagged a fresh
        // health transition — swap the block to a full-height `█` and
        // bold it so the change visually pops on the refresh that
        // landed it.
        let (glyph, style) = if pulse_last && i + 1 == visible_len {
            (
                "█",
                style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK),
            )
        } else {
            ("▇", style)
        };
        spans.push(Span::styled(glyph, style));
    }
    Line::from(spans)
}

/// Pure: scale an `Rgb` colour towards black by `factor` (clamped 0..=1).
/// Non-RGB inputs (e.g. terminal-named `Color::Red`) pass through unchanged
/// because there's no portable "darken by N%" for those. Used by the
/// sparkline two-tone styling so fg+bg pairs read as distinct shades on
/// both highlighted and unhighlighted rows.
pub(crate) fn scale_rgb(color: Color, factor: f32) -> Color {
    let factor = factor.clamp(0.0, 1.0);
    if let Color::Rgb(r, g, b) = color {
        Color::Rgb(
            (r as f32 * factor) as u8,
            (g as f32 * factor) as u8,
            (b as f32 * factor) as u8,
        )
    } else {
        color
    }
}

pub(crate) fn health_style(health: &str, theme: &Theme) -> Style {
    Style::default()
        .fg(health_color(health, theme))
        .add_modifier(Modifier::BOLD)
}

/// Pure: map an EB health bucket name (any case) to the theme's
/// corresponding palette colour. Allocation-free — extracted so the
/// per-row hot path doesn't pay a `to_lowercase` per cell.
pub(crate) fn health_color(health: &str, theme: &Theme) -> Color {
    if ieq_any(health, &["green", "ok"]) {
        theme.health_green
    } else if ieq_any(health, &["yellow", "warning"]) {
        theme.health_yellow
    } else if ieq_any(health, &["red", "severe", "degraded"]) {
        theme.health_red
    } else if ieq_any(health, &["grey", "gray", "info", "no data", "pending"]) {
        theme.health_grey
    } else {
        theme.text
    }
}

pub(crate) fn redact(value: &str, on: bool) -> String {
    if !on || value.is_empty() || value == "—" {
        return value.to_string();
    }
    // Preserve length using full-block shaded characters.
    "▓".repeat(value.chars().count())
}
