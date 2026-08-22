//! The embedded SSM shell pane and its vt100 colour mapping.
//!
//! Moved verbatim out of `src/ui.rs` (5,046 lines) in the 0.31 split;
//! visibility widened to `pub(crate)` where a sibling needs it, and
//! nothing else changed. `use super::*` picks up the shared vocabulary
//! the way `detail.rs` and `overlays.rs` already do.

use super::*;

/// Render an embedded shell pane: a 1-row title at the top, a 1-row footer
/// hint at the bottom, and the vt100 screen contents filling the middle.
/// We resize the PTY to match the available space and iterate the parser's
/// screen cell-by-cell so xterm colours / bold / reverse propagate through
/// to the ratatui buffer.
pub(crate) fn draw_shell(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(shell) = app.current_shell.as_ref() else {
        return;
    };
    let theme = &app.theme;
    let footer_rows: u16 = 1;
    let outer_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(footer_rows)])
        .split(area);
    // Bordered block holds the shell content with a title bar — gives
    // the subprocess natural breathing room (the border eats 1 row at top
    // and bottom, 1 col at left and right) and keeps the pane label
    // visible without crowding the first line of output.
    let title_text = format!(" ⌥ {}    F12 detach    ^D / exit close ", shell.label);
    let block = rounded_block(theme, true)
        .border_style(Style::default().fg(theme.title))
        .title(Span::styled(
            title_text,
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        ));
    let body = block.inner(outer_chunks[0]);
    f.render_widget(block, outer_chunks[0]);
    // Resize the PTY to fit the available body area so the subprocess
    // gets a sensible TIOCSWINSZ on terminal resize.
    shell.resize(body.height, body.width);

    // Lock the parser and walk the visible cells. We render into the
    // ratatui buffer directly because that's the cheapest way to preserve
    // the per-cell style information.
    let mut cursor_pos: Option<(u16, u16)> = None;
    if let Ok(parser) = shell.parser.lock() {
        let screen = parser.screen();
        let (cur_row, cur_col) = screen.cursor_position();
        let buf = f.buffer_mut();
        for row in 0..body.height {
            for col in 0..body.width {
                let cell = screen.cell(row, col);
                let target_x = body.x + col;
                let target_y = body.y + row;
                if target_x >= buf.area.x.saturating_add(buf.area.width)
                    || target_y >= buf.area.y.saturating_add(buf.area.height)
                {
                    continue;
                }
                let target = &mut buf[(target_x, target_y)];
                match cell {
                    Some(c) => {
                        let sym = c.contents();
                        target.set_symbol(if sym.is_empty() { " " } else { &sym });
                        let mut style = Style::default();
                        style = style.fg(vt100_color_to_ratatui(c.fgcolor()));
                        style = style.bg(vt100_color_to_ratatui(c.bgcolor()));
                        let mut mods = Modifier::empty();
                        if c.bold() {
                            mods |= Modifier::BOLD;
                        }
                        if c.italic() {
                            mods |= Modifier::ITALIC;
                        }
                        if c.underline() {
                            mods |= Modifier::UNDERLINED;
                        }
                        if c.inverse() {
                            mods |= Modifier::REVERSED;
                        }
                        style = style.add_modifier(mods);
                        target.set_style(style);
                    }
                    None => {
                        target.set_symbol(" ");
                        target.set_style(Style::default());
                    }
                }
            }
        }
        // Translate vt100's cursor into screen coords for the real cursor.
        if cur_row < body.height && cur_col < body.width && !screen.hide_cursor() {
            cursor_pos = Some((body.x + cur_col, body.y + cur_row));
        }
    }

    // Real terminal cursor at the vt100 cursor position so the user can
    // see where they're typing and follow visual editors (vim, less, etc.).
    if let Some((cx, cy)) = cursor_pos {
        f.set_cursor_position((cx, cy));
    }

    let footer = Line::from(Span::styled(
        " SHELL  keys forwarded to subprocess  ·  F12 detach  ·  ^D / exit closes ",
        Style::default().fg(theme.muted),
    ));
    f.render_widget(Paragraph::new(footer), outer_chunks[1]);
}

/// Map a vt100 cell colour to a ratatui Color. vt100 distinguishes
/// `Default` (terminal default) from indexed 256-colour and RGB; we
/// pass each through to the closest ratatui equivalent so true-colour
/// content (modern shells, vim themes) renders faithfully.
pub(crate) fn vt100_color_to_ratatui(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

// `OverlaySize`, `centered_overlay`, `centered_rect`, and `overlay_dims`
// live in the shared `tui-common` crate. Re-exported from
// `crate::overlay` in `lib.rs`; call sites continue to use
// `OverlaySize::Picker` etc. through the `use` statement at the top
// of this file.
