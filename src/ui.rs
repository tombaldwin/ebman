use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    symbols::Marker,
    text::{Line, Span},
    widgets::{
        Axis, Block, BorderType, Borders, Cell, Chart, Clear, Dataset, GraphType, List, ListItem,
        Padding, Paragraph, Row, Table, Wrap,
    },
    Frame,
};

use crate::aws::Environment;
use crate::theme::{IconStyle, Theme};

use crate::app::{
    Action, ActionFlow, App, ConfirmKind, DetailTab, DisplayRow, LoadState, Mode, Overlay, Scope,
    SortKey, ToastKind, ViewMode, ACTIONS,
};

const SPINNER_FRAMES: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
const ASCII_SPINNER: &[&str] = &["|", "/", "-", "\\"];

fn rounded_block(theme: &Theme, active: bool) -> Block<'static> {
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

fn titled_block(theme: &Theme, raw_title: &str, active: bool, accent: Color) -> Block<'static> {
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

fn pill(text: &str, fg: Color, bg: Color) -> Span<'static> {
    Span::styled(
        format!(" {text} "),
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    )
}

/// Render a chain of pills with Powerline-style triangular bridges in
/// `IconStyle::Powerline`, or a plain pill+sep chain in other styles.
///
/// In Powerline mode each adjacent pair gets a U+E0B0 right-pointing
/// triangle whose `fg` matches the left pill's bg and `bg` matches the
/// right pill's bg — so the colours flow continuously, no gap visible.
/// A trailing arrow with `bg=default` flows the ribbon back to the
/// surrounding background.
///
/// The returned spans are intended to sit at the *end* of a Line; in
/// non-Powerline mode the first sep is omitted so the caller controls the
/// space between any preceding plain-text content and the chain head.
fn pill_chain(items: &[(String, Color, Color)], theme: &Theme) -> Vec<Span<'static>> {
    if items.is_empty() {
        return Vec::new();
    }
    let powerline = theme.icons == IconStyle::Powerline;
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(items.len() * 2 + 1);
    if powerline {
        // Lead-in arrow: default bg → first pill's bg. We use U+E0B2 (LEFT-
        // pointing solid triangle) here, not U+E0B0 — the pill's coloured
        // base needs to sit on the *right* side of the cell (adjacent to
        // the pill), with the empty wedge on the left (adjacent to the
        // preceding plain text). Using E0B0 here would put the base on
        // the left and leave only a thin point touching the pill, which
        // visually reads as a much smaller triangle than the matching
        // trailing E0B0 on the right edge of the chain.
        let (_, _, first_bg) = items[0];
        spans.push(Span::styled("\u{e0b2}", Style::default().fg(first_bg)));
        for (i, (text, fg, bg)) in items.iter().enumerate() {
            spans.push(Span::styled(
                format!(" {text} "),
                Style::default()
                    .fg(*fg)
                    .bg(*bg)
                    .add_modifier(Modifier::BOLD),
            ));
            // Bridge to next pill, or trailing arrow back to default bg.
            let bridge_style = if let Some(next) = items.get(i + 1) {
                Style::default().fg(*bg).bg(next.2)
            } else {
                Style::default().fg(*bg)
            };
            spans.push(Span::styled("\u{e0b0}", bridge_style));
        }
    } else {
        // Non-Powerline: classic pill + bullet separator chain. Caller
        // already injected a leading sep before the first pill — we just
        // emit pills + interleaved separators.
        for (i, (text, fg, bg)) in items.iter().enumerate() {
            if i > 0 {
                spans.push(sep(theme));
            }
            spans.push(Span::styled(
                format!(" {text} "),
                Style::default()
                    .fg(*fg)
                    .bg(*bg)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    spans
}

fn health_dot(health: &str, theme: &Theme) -> Span<'static> {
    let c = match health.to_lowercase().as_str() {
        "green" | "ok" => theme.health_green,
        "yellow" | "warning" => theme.health_yellow,
        "red" | "severe" | "degraded" => theme.health_red,
        "grey" | "gray" | "info" | "no data" | "pending" => theme.health_grey,
        _ => theme.text,
    };
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

fn spinner(elapsed_ms: u128, icons: IconStyle) -> &'static str {
    match icons {
        // Powerline-targeted fonts include the braille range, so the same
        // animation reads well without needing a separate frame set.
        IconStyle::Unicode | IconStyle::Powerline => {
            SPINNER_FRAMES[(elapsed_ms / 100) as usize % SPINNER_FRAMES.len()]
        }
        IconStyle::Ascii => ASCII_SPINNER[(elapsed_ms / 100) as usize % ASCII_SPINNER.len()],
    }
}

fn tab_icon(t: DetailTab, icons: IconStyle) -> &'static str {
    match (icons, t) {
        (IconStyle::Unicode, DetailTab::Events) => "⚡",
        (IconStyle::Unicode, DetailTab::Instances) => "▣",
        (IconStyle::Unicode, DetailTab::Metrics) => "▆",
        (IconStyle::Unicode, DetailTab::Queue) => "✉",
        (IconStyle::Unicode, DetailTab::Logs) => "≣",
        (IconStyle::Unicode, DetailTab::Config) => "⚙",
        // Powerline / Nerd Font Material Design glyphs. Each is distinct so
        // the tab strip remains readable even when icons collapse onto a
        // single line in the boot splash / detail header.
        (IconStyle::Powerline, DetailTab::Events) => "\u{f0e7}", // flash
        (IconStyle::Powerline, DetailTab::Instances) => "\u{f048b}", // server
        (IconStyle::Powerline, DetailTab::Metrics) => "\u{f0680}", // chart-line
        (IconStyle::Powerline, DetailTab::Queue) => "\u{f01ee}", // email-outline
        (IconStyle::Powerline, DetailTab::Logs) => "\u{f021a}",  // text-box
        (IconStyle::Powerline, DetailTab::Config) => "\u{f0493}", // cog
        // ASCII fallbacks: one letter per tab so each is distinguishable.
        (IconStyle::Ascii, DetailTab::Events) => "E",
        (IconStyle::Ascii, DetailTab::Instances) => "I",
        (IconStyle::Ascii, DetailTab::Metrics) => "M",
        (IconStyle::Ascii, DetailTab::Queue) => "Q",
        (IconStyle::Ascii, DetailTab::Logs) => "L",
        (IconStyle::Ascii, DetailTab::Config) => "C",
    }
}

fn micro_bar(value: i64, max: i64, width: usize) -> String {
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

const SPARKLINE_WIDTH: usize = 20;
/// How wide each divider fill string is. Ratatui truncates per-column, so any
/// value ≥ max column width works.
const DIVIDER_FILL_WIDTH: usize = 200;

pub fn draw(f: &mut Frame, app: &mut App) {
    // Shell mode takes the whole screen; nothing else draws.
    if app.mode == Mode::Shell && app.current_shell.is_some() {
        draw_shell(f, f.area(), app);
        return;
    }
    // Background — Dlq / Detail use a full-screen alternative layout; otherwise
    // draw the main header + table + events + footer.
    //
    // The mode check used to also gate this — meaning pressing `?` or `a` in
    // Detail would temporarily render the main table behind the popup
    // because mode transitioned to Help/Action. We now use the state-Option
    // as the source of truth: if a Detail/Dlq view is open, that's the
    // background, regardless of whether a help/action/overlay modal is on
    // top of it.
    if app.dlq.is_some() {
        draw_dlq(f, f.area(), app);
    } else if app.detail.is_some() {
        draw_detail(f, f.area(), app);
    } else {
        let events_height: u16 = if app.events_visible {
            app.events_panel_height
        } else {
            0
        };
        // Header is taller when the user has saved filters — the chip bar
        // needs its own row. Stays at 5 otherwise so we don't waste vertical
        // space on accounts with no saved filters.
        let header_height: u16 = if app.named_filters.is_empty() { 5 } else { 6 };
        let mut constraints: Vec<Constraint> =
            vec![Constraint::Length(header_height), Constraint::Min(3)];
        if events_height > 0 {
            constraints.push(Constraint::Length(events_height));
        }
        constraints.push(Constraint::Length(2));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(f.area());

        draw_header(f, chunks[0], app);
        match app.scope {
            Scope::Envs => draw_table(f, chunks[1], app),
            Scope::Apps => draw_apps_table(f, chunks[1], app),
        }
        if app.events_visible {
            app.events_area = Some(chunks[2]);
            draw_events(f, chunks[2], app);
            draw_footer(f, chunks[3], app);
        } else {
            app.events_area = None;
            draw_footer(f, chunks[2], app);
        }
    }

    // Overlays and modal popups — paint on top of whichever background was
    // drawn above. Keeping these unconditional means a `D`-press from Detail
    // still surfaces the describe overlay; previously the early return swallowed it.
    if app.mode == Mode::Help {
        draw_help(f, f.area(), app);
    }
    if app.mode == Mode::Picker {
        draw_picker(f, f.area(), app);
    }
    if app.mode == Mode::Action {
        draw_action(f, f.area(), app);
    }
    if let Some(overlay) = app.current_overlay.clone() {
        match overlay {
            Overlay::Describe(text) => draw_describe(f, f.area(), app, &text),
            Overlay::Whatsnew(text) => draw_whatsnew(f, f.area(), app, &text),
            Overlay::History(text) => draw_history_overlay(f, f.area(), app, &text),
            Overlay::Alarms { body, .. } => draw_alarms_overlay(f, f.area(), app, &body),
            Overlay::Diff(text) => draw_diff_overlay(f, f.area(), app, &text),
            Overlay::SavedConfigs(text) => draw_saved_configs_overlay(f, f.area(), app, &text),
            Overlay::SavedConfigsInteractive {
                items,
                cursor,
                confirm_delete,
            } => draw_saved_configs_interactive(f, f.area(), app, &items, cursor, confirm_delete),
            Overlay::TextDump { title, body } => {
                draw_text_dump_overlay(f, f.area(), app, &title, &body)
            }
            Overlay::LogTail { .. } => draw_log_tail_overlay(f, f.area(), app),
        }
    }
    if app.mode == Mode::Palette {
        draw_palette(f, f.area(), app);
    }
    if app.mode == Mode::Form {
        draw_form(f, f.area(), app);
    }
    // Toasts render last so they overlay everything else.
    if !app.toasts.is_empty() {
        draw_toasts(f, f.area(), app);
    }
}

fn draw_form(f: &mut Frame, area: Rect, app: &App) {
    use crate::form::{FieldKind, FormState};
    let Some(form) = app.form.as_ref() else {
        return;
    };
    let popup = centered_rect(60, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let outer = titled_block(theme, &form.title, true, theme.title_alt);
    let inner = outer.inner(popup);
    f.render_widget(outer, popup);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // env target banner
            Constraint::Length(1), // separator
            Constraint::Min(1),    // fields
            Constraint::Length(1), // footer hint
        ])
        .split(inner);

    // LocalConfig forms (`:settings`) don't have an AWS target; show the
    // config file path instead so the operator knows where the submit
    // will land.
    let banner = if form.env_name.is_empty() {
        format!(" file: {}", crate::config::config_path().display())
    } else {
        format!(" target: {}", form.env_name)
    };
    f.render_widget(
        Paragraph::new(Span::styled(banner, Style::default().fg(theme.muted))),
        chunks[0],
    );
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(theme.muted),
        )),
        chunks[1],
    );

    if form.state == FormState::Loading {
        f.render_widget(
            Paragraph::new(Span::styled(
                "  loading current values from AWS…",
                Style::default().fg(theme.muted),
            )),
            chunks[2],
        );
    } else {
        // Build the field rows. Each field takes 2-3 lines: label/value
        // row, optional help, optional error.
        let max_label = form
            .fields
            .iter()
            .map(|fld| fld.label.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(8, 32);
        let mut lines: Vec<Line> = Vec::new();
        for (i, fld) in form.fields.iter().enumerate() {
            let is_cursor = i == form.cursor;
            let pointer = if is_cursor { "▶ " } else { "  " };
            let label_style = if is_cursor {
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            let value_style = if is_cursor {
                Style::default().fg(theme.text).bg(theme.row_selected_bg)
            } else {
                Style::default().fg(theme.text)
            };
            // Render the value per kind.
            let value_text: String = match &fld.kind {
                FieldKind::Text | FieldKind::Integer { .. } => {
                    if is_cursor {
                        format!("{}_", fld.value)
                    } else {
                        fld.value.clone()
                    }
                }
                FieldKind::Boolean => {
                    if fld.value == "true" {
                        "[x] true".to_string()
                    } else {
                        "[ ] false".to_string()
                    }
                }
                FieldKind::Select { options } => {
                    // ◀ value ▶ when focused; just value otherwise.
                    let _ = options; // currently unused; keeps the type
                    if is_cursor {
                        format!("◀ {} ▶", fld.value)
                    } else {
                        fld.value.clone()
                    }
                }
            };
            // Trailing in-line validation marker: a single ✗ glyph in
            // health_red next to the value when the field is invalid.
            // The full error message still renders on its own line below;
            // the marker is the eye-catcher that lets the operator scan
            // for the bad field without reading every help line.
            let mut row_spans = vec![
                Span::styled(pointer.to_string(), Style::default().fg(theme.accent)),
                Span::styled(
                    format!("{:<width$}  ", fld.label, width = max_label),
                    label_style,
                ),
                Span::styled(value_text, value_style),
            ];
            if fld.error.is_some() {
                row_spans.push(Span::styled(
                    "  ✗",
                    Style::default()
                        .fg(theme.health_red)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            lines.push(Line::from(row_spans));
            if let Some(help) = &fld.help {
                lines.push(Line::from(Span::styled(
                    format!("     {help}"),
                    Style::default().fg(theme.muted),
                )));
            }
            if let Some(err) = &fld.error {
                lines.push(Line::from(Span::styled(
                    format!("     ⚠ {err}"),
                    Style::default().fg(theme.health_red),
                )));
            }
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[2]);
    }
    let footer = match form.state {
        FormState::Loading => " esc to cancel",
        FormState::Submitting => " submitting…",
        FormState::Ready => " tab/↓↑ field · type to edit · space toggle bool · ←→ cycle select · ^S submit · esc cancel",
    };
    f.render_widget(
        Paragraph::new(Span::styled(footer, Style::default().fg(theme.muted))),
        chunks[3],
    );
}

fn draw_palette(f: &mut Frame, area: Rect, app: &App) {
    let popup = centered_rect(60, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    // Single frame around the whole palette (input + list + footer). The
    // inner layout splits the interior with no internal borders, so the
    // popup reads as one visually-unified widget rather than three stacked
    // boxes.
    let outer = titled_block(theme, "palette", true, theme.title_alt);
    let inner = outer.inner(popup);
    f.render_widget(outer, popup);
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // input
            Constraint::Length(1), // separator
            Constraint::Min(1),    // list
            Constraint::Length(1), // hint
        ])
        .split(inner);

    // Input bar (no border — drawn directly inside the outer frame).
    let input = Paragraph::new(Line::from(vec![
        Span::styled(
            " ❯ ",
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.palette_input.clone(), Style::default().fg(theme.text)),
        Span::styled(
            caret_glyph(theme),
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
    ]));
    f.render_widget(input, layout[0]);

    // Thin horizontal rule between input and list.
    f.render_widget(
        Paragraph::new(Span::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(theme.muted),
        )),
        layout[1],
    );

    // Item list
    let items: Vec<ListItem> = app
        .palette_filtered
        .iter()
        .filter_map(|i| app.palette_items.get(*i))
        .map(|it| {
            let tag_color = match it.kind_tag {
                "cmd" => theme.title,
                "env" => theme.text,
                "view" => theme.title_alt,
                "plugin" => theme.accent,
                _ => theme.muted,
            };
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!(" {:<6}", it.kind_tag),
                    Style::default().fg(tag_color).add_modifier(Modifier::DIM),
                ),
                Span::styled(
                    format!("{:<32}", it.label),
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(it.detail.clone(), Style::default().fg(theme.muted)),
            ]))
        })
        .collect();
    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(cursor_marker(theme));
    let mut state = app.palette_state.clone();
    f.render_stateful_widget(list, layout[2], &mut state);

    // Hint footer
    let hint_count = app.palette_filtered.len();
    let total = app.palette_items.len();
    let hint = Paragraph::new(Span::styled(
        format!(
            " {}/{} matches   ↑/↓ move   ⏎ run   esc cancel",
            hint_count, total,
        ),
        Style::default().fg(theme.muted),
    ));
    f.render_widget(hint, layout[3]);
}

fn draw_toasts(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let width: u16 = 50;
    let toast_h: u16 = 3;
    // Stack from bottom-right; newest at the bottom.
    let n = app.toasts.len() as u16;
    let total_h = n * toast_h;
    if area.height < total_h + 2 || area.width < width + 2 {
        return;
    }
    let x = area.x + area.width.saturating_sub(width + 2);
    let mut y = area.y + area.height.saturating_sub(total_h + 2);
    for t in &app.toasts {
        let rect = Rect {
            x,
            y,
            width,
            height: toast_h,
        };
        // Severity drives all three of: glyph, border colour, title text.
        // Glyph picks vary by icon style so the toast stays readable when
        // the user's font doesn't have Nerd / Powerline glyphs.
        let (border_color, label, glyph) = match (t.kind, theme.icons) {
            (ToastKind::Info, IconStyle::Powerline) => (theme.title, "info", "\u{f05a}"),
            (ToastKind::Success, IconStyle::Powerline) => (theme.health_green, "ok", "\u{f058}"),
            (ToastKind::Error, IconStyle::Powerline) => (theme.health_red, "error", "\u{f057}"),
            (ToastKind::Info, IconStyle::Unicode) => (theme.title, "info", "ⓘ"),
            (ToastKind::Success, IconStyle::Unicode) => (theme.health_green, "ok", "✓"),
            (ToastKind::Error, IconStyle::Unicode) => (theme.health_red, "error", "✗"),
            (ToastKind::Info, IconStyle::Ascii) => (theme.title, "info", "i"),
            (ToastKind::Success, IconStyle::Ascii) => (theme.health_green, "ok", "+"),
            (ToastKind::Error, IconStyle::Ascii) => (theme.health_red, "error", "!"),
        };
        let block = rounded_block(theme, true)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                format!(" {glyph} {label} "),
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ));
        let mut text = t.text.clone();
        // Truncate so it fits one line inside the box. Leave room for the
        // left-edge severity stripe (▎) + leading glyph + space.
        let max = (width as usize).saturating_sub(7);
        if text.chars().count() > max {
            text = text.chars().take(max.saturating_sub(1)).collect::<String>();
            text.push('…');
        }
        // Chunky severity stripe on the left edge of the body. Reads as a
        // notification-card accent bar the way Slack / VS Code toasts look,
        // and keeps the severity signal even at the periphery of vision.
        let para = Paragraph::new(Line::from(vec![
            Span::styled(
                "▎",
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" {glyph} "),
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(text, Style::default().fg(theme.text)),
        ]))
        .block(block);
        f.render_widget(Clear, rect);
        f.render_widget(para, rect);
        y += toast_h;
    }
}

fn draw_saved_configs_interactive(
    f: &mut Frame,
    area: Rect,
    app: &App,
    items: &[(String, String)],
    cursor: usize,
    confirm_delete: bool,
) {
    let popup = centered_rect(60, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let target = app
        .selected_env()
        .map(|e| e.name.clone())
        .unwrap_or_else(|| "—".into());
    // App of the apply-target env. Templates from a different app can't be
    // applied (EB rejects cross-app), so we dim those rows + add a marker
    // so the operator knows before pressing enter.
    let target_app = app.selected_env().map(|e| e.application.clone());
    // popup.height includes the title row + border. Subtract those + uniform
    // padding (1) + the 2 banner lines + the footer line. The remainder is
    // how many item rows we can show before clipping; if items overflow,
    // window them around the cursor.
    let row_budget = popup.height.saturating_sub(8) as usize;
    let (visible_start, visible_end) = visible_window(cursor, items.len(), row_budget);
    let mut lines: Vec<Line> = Vec::with_capacity(row_budget + 6);
    let banner = if confirm_delete {
        let cur_label = items
            .get(cursor)
            .map(|(a, t)| format!("{a}/{t}"))
            .unwrap_or_else(|| "?".into());
        Line::from(Span::styled(
            format!(" delete {cur_label}?  (Y/N)"),
            Style::default()
                .fg(theme.health_red)
                .add_modifier(Modifier::BOLD),
        ))
    } else {
        Line::from(Span::styled(
            format!(" apply target: {target}"),
            Style::default().fg(theme.muted),
        ))
    };
    lines.push(banner);
    if visible_start > 0 {
        lines.push(Line::from(Span::styled(
            format!(" ↑ {visible_start} more above"),
            Style::default().fg(theme.muted),
        )));
    } else {
        lines.push(Line::from(""));
    }
    // Group rows under app-name headers as the cursor walks the visible
    // window. Header lines aren't selectable so the cursor index still
    // maps 1:1 to `items`.
    let mut prev_app: Option<&str> = None;
    // If the first visible item isn't index 0, look back to figure out
    // whether to print its app header. We always emit a header when the
    // current item's app differs from the previous *visible* row.
    if visible_start > 0 {
        prev_app = items.get(visible_start - 1).map(|(a, _)| a.as_str());
    }
    for (i, (app_name, tmpl)) in items
        .iter()
        .enumerate()
        .skip(visible_start)
        .take(visible_end - visible_start)
    {
        if Some(app_name.as_str()) != prev_app {
            lines.push(Line::from(Span::styled(
                app_name.clone(),
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            )));
            prev_app = Some(app_name.as_str());
        }
        let cross_app = target_app
            .as_ref()
            .map(|ta| ta != app_name)
            .unwrap_or(false);
        let marker = if i == cursor { " ▶ " } else { "   " };
        let style = if i == cursor {
            let bg = if confirm_delete {
                theme.row_red_bg
            } else {
                theme.row_selected_bg
            };
            Style::default()
                .fg(theme.text)
                .bg(bg)
                .add_modifier(Modifier::BOLD)
        } else if cross_app {
            // Cross-app templates dimmed — EB rejects applying a template
            // from a different application, so the operator should see
            // before pressing enter that this row isn't a valid apply.
            Style::default().fg(theme.muted)
        } else {
            Style::default().fg(theme.text)
        };
        let suffix = if cross_app {
            "  (different app — apply will fail)"
        } else {
            ""
        };
        let line = Line::from(vec![
            Span::styled(marker.to_string(), Style::default().fg(theme.accent)),
            Span::styled(tmpl.clone(), style),
            Span::styled(suffix.to_string(), Style::default().fg(theme.health_yellow)),
        ]);
        lines.push(line);
    }
    if visible_end < items.len() {
        let more = items.len() - visible_end;
        lines.push(Line::from(Span::styled(
            format!(" ↓ {more} more below"),
            Style::default().fg(theme.muted),
        )));
    }
    lines.push(Line::from(""));
    let footer = if confirm_delete {
        " Y confirm • N / esc cancel "
    } else {
        " j/k move • enter/a apply • i inspect • c create • x delete • ? help • esc close "
    };
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(theme.muted),
    )));
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "saved configurations", true, app.theme.title)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

/// Pure helper: pick a (start, end) window of indices to render such that
/// `cursor` is inside `[start, end)` and `end - start <= budget`. Window
/// stays as low as possible (anchor to top when items fit, slide down only
/// when the cursor passes the visible area). Used by the saved-configs
/// overlay's scroll logic and tested directly.
pub fn visible_window(cursor: usize, total: usize, budget: usize) -> (usize, usize) {
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

fn draw_log_tail_overlay(f: &mut Frame, area: Rect, app: &App) {
    let Some(crate::app::Overlay::LogTail {
        log_group,
        env_name,
        events,
        scroll,
        following,
        filter_input,
        filter_active,
        filter_pattern,
        last_err,
        ..
    }) = app.current_overlay.as_ref()
    else {
        return;
    };
    let popup = centered_rect(85, 80, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    // Format each event as `HH:MM:SS  STREAM_TAIL  message`. Stream names
    // are EB instance ids — keep just the last 8 chars so the line stays
    // scannable.
    let mut lines: Vec<Line> = Vec::with_capacity(events.len());
    for ev in events.iter() {
        if let Some(pat) = filter_pattern.as_ref() {
            if !pat.is_match(&ev.message) {
                continue;
            }
        }
        let dt = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ev.timestamp_ms)
            .unwrap_or_else(chrono::Utc::now);
        let stream_tail: String = ev
            .stream
            .chars()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let ts_style = Style::default().fg(theme.muted);
        let stream_style = Style::default().fg(theme.title_alt);
        let msg_style = Style::default().fg(theme.text);
        lines.push(Line::from(vec![
            Span::styled(format!("{}  ", dt.format("%H:%M:%S")), ts_style),
            Span::styled(format!("{stream_tail}  "), stream_style),
            Span::styled(ev.message.clone(), msg_style),
        ]));
    }
    // popup.height minus borders/padding/title/footer (≈6). Slice the tail
    // when following; otherwise honour `scroll`.
    let body_rows = popup.height.saturating_sub(6) as usize;
    let total = lines.len();
    let start = if *following {
        total.saturating_sub(body_rows)
    } else {
        let max_start = total.saturating_sub(body_rows);
        max_start.saturating_sub(*scroll as usize)
    };
    let visible_lines: Vec<Line> = lines.into_iter().skip(start).take(body_rows).collect();
    let title_text = format!(
        "logs-tail — {env_name} · {} · {} lines{}",
        log_group.rsplit('/').next().unwrap_or(log_group.as_str()),
        events.len(),
        if *following {
            " · following"
        } else {
            " · paused (G to follow)"
        }
    );
    let footer_text = if *filter_active {
        format!(" filter: {filter_input}_ (esc cancel)")
    } else if let Some(err) = last_err {
        format!(" ⚠ {err}")
    } else {
        " j/k scroll · g/G top/follow · / filter · n clear-filter · esc / q close".to_string()
    };
    let mut paragraph_lines = visible_lines;
    paragraph_lines.push(Line::raw(""));
    paragraph_lines.push(Line::from(Span::styled(
        footer_text,
        Style::default().fg(theme.muted),
    )));
    let p = Paragraph::new(paragraph_lines)
        .wrap(Wrap { trim: false })
        .block(
            titled_block(&app.theme, &title_text, true, app.theme.title)
                .padding(Padding::uniform(1)),
        );
    f.render_widget(p, popup);
}

fn draw_text_dump_overlay(f: &mut Frame, area: Rect, app: &App, title: &str, text: &str) {
    let popup = centered_rect(70, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let lines: Vec<Line> = text
        .lines()
        .map(|l| Line::from(Span::styled(l.to_string(), Style::default().fg(theme.text))))
        .collect();
    // Pin the close-hint to the bottom row of the popup so it stays
    // visible even when the body overflows. Body region + 1-row footer
    // both render inside the same titled block.
    let outer = titled_block(&app.theme, title, true, app.theme.title);
    let inner = outer.inner(popup);
    f.render_widget(outer, popup);
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(inner);
    f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[0]);
    f.render_widget(
        Paragraph::new(Span::styled(
            " esc / q to close",
            Style::default().fg(theme.muted),
        )),
        chunks[1],
    );
}

fn draw_saved_configs_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_rect(60, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| {
            let style = if l.starts_with("Application:") {
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD)
            } else if l.trim_start().starts_with('▸') {
                Style::default().fg(theme.text)
            } else if l.starts_with("─") {
                Style::default().fg(theme.muted)
            } else {
                Style::default().fg(theme.text)
            };
            Line::from(Span::styled(l.to_string(), style))
        })
        .collect();
    push_close_hint(&mut lines, &app.theme);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "saved configurations", true, app.theme.title)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

/// Append a one-line `esc / q to close` hint to an overlay's body so the
/// title bar can stay clean. Pushes a blank separator first.
fn push_close_hint(lines: &mut Vec<Line<'static>>, theme: &Theme) {
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " esc / q to close",
        Style::default().fg(theme.muted),
    )));
}

fn draw_diff_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_rect(80, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| {
            let style = if l.starts_with('≠') {
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::BOLD)
            } else if l.starts_with("─") {
                Style::default().fg(theme.muted)
            } else {
                Style::default().fg(theme.text)
            };
            Line::from(Span::styled(l.to_string(), style))
        })
        .collect();
    push_close_hint(&mut lines, &app.theme);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "diff", true, app.theme.title).padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_alarms_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_rect(70, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| {
            // Highlight alarm state at the start of each line.
            let style = if l.starts_with("ALARM") {
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD)
            } else if l.starts_with("OK") {
                Style::default().fg(theme.health_green)
            } else if l.starts_with("INSUFFICIENT") || l.trim_start().starts_with("↳") {
                Style::default().fg(theme.muted)
            } else {
                Style::default().fg(theme.text)
            };
            Line::from(Span::styled(l.to_string(), style))
        })
        .collect();
    push_close_hint(&mut lines, &app.theme);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "alarms", true, app.theme.title).padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_history_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_rect(60, 70, area);
    f.render_widget(Clear, popup);
    let mut lines: Vec<Line> = text
        .lines()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(app.theme.text),
            ))
        })
        .collect();
    push_close_hint(&mut lines, &app.theme);
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "history", true, app.theme.title).padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_whatsnew(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_rect(60, 70, area);
    f.render_widget(Clear, popup);
    let lines: Vec<Line> = text
        .lines()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(app.theme.text),
            ))
        })
        .collect();
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(
            &app.theme,
            "what's new — esc / w / q to close",
            true,
            app.theme.title_alt,
        )
        .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_describe(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_rect(60, 70, area);
    f.render_widget(Clear, popup);
    let lines: Vec<Line> = text
        .lines()
        .map(|l| {
            Line::from(Span::styled(
                l.to_string(),
                Style::default().fg(app.theme.text),
            ))
        })
        .collect();
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(
            &app.theme,
            "describe — esc / D / q to close",
            true,
            app.theme.title_alt,
        )
        .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_header(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(area);

    let profile = app
        .context
        .profile
        .clone()
        .unwrap_or_else(|| "default".into());
    let last = match app.last_refresh {
        Some(t) => format!(
            "{} (every {}s)",
            t.with_timezone(&chrono::Local).format("%H:%M:%S"),
            app.refresh_interval.as_secs()
        ),
        None => format!("— (every {}s)", app.refresh_interval.as_secs()),
    };
    let now = std::time::Instant::now();
    let live_load_visible = app
        .loading_since
        .map(|t| t.elapsed() >= crate::app::LOADING_INDICATOR_THRESHOLD)
        .unwrap_or(false);
    let linger_active = app.loading_visible_until.map(|t| now < t).unwrap_or(false);
    let show_loading = live_load_visible || linger_active;
    // Spinner phase tracks the live load when one is in flight; during the
    // linger window the spinner keeps advancing from the linger's start so
    // the animation doesn't freeze on a single frame for half a second.
    let elapsed_ms = if let Some(t) = app.loading_since {
        t.elapsed().as_millis()
    } else if let Some(until) = app.loading_visible_until {
        let linger_started = until - crate::app::LOADING_INDICATOR_LINGER;
        now.saturating_duration_since(linger_started).as_millis()
    } else {
        0
    };
    // Fixed-width status slot so the rest of line 2 doesn't shift right
    // when the indicator flips between `idle` and `⠋ loading…`. Slot is
    // sized for the longest variant (spinner + " loading…" = ~10 cols);
    // shorter values get left-aligned + space-padded.
    const STATUS_SLOT: usize = 10;
    // The linger window (LOADING_INDICATOR_LINGER) keeps `show_loading`
    // true after the load completes; previously the match arm gated on
    // `LoadState::Loading` so the linger had no visible effect — flipped
    // straight from loading-yellow back to idle-green. Drive the
    // selection off `show_loading` directly so the linger actually
    // smooths over the transition.
    let status: Span<'static> = if matches!(app.load_state, LoadState::Error) {
        let label = format!("{:<width$}", "error", width = STATUS_SLOT);
        Span::styled(label, Style::default().fg(theme.health_red))
    } else if show_loading {
        let label = format!(
            "{:<width$}",
            format!("{} loading…", spinner(elapsed_ms, theme.icons)),
            width = STATUS_SLOT
        );
        Span::styled(label, Style::default().fg(theme.health_yellow))
    } else {
        let label = format!("{:<width$}", "idle", width = STATUS_SLOT);
        Span::styled(label, Style::default().fg(theme.health_green))
    };

    let env_count = app.environments.len().to_string();
    let account = redact(
        &app.context.account_id.clone().unwrap_or_else(|| "—".into()),
        app.redact,
    );
    let caller = redact(
        &app.context
            .caller_arn
            .as_deref()
            .map(short_caller)
            .unwrap_or_else(|| "—".into()),
        app.redact,
    );

    let mut line1 = kv("Account", &account, theme);
    line1.push(sep(theme));
    line1.extend(kv("Region", &app.context.region, theme));
    line1.push(sep(theme));
    line1.extend(kv("Profile", &profile, theme));
    let mut line2 = kv("Caller", &caller, theme);
    line2.push(sep(theme));
    line2.extend(kv("Envs", &env_count, theme));
    // Health-bucket delta since the previous refresh, e.g. "▲1 Red ▼1 Yellow".
    for (bucket, delta) in app.health_delta.iter().chain(app.status_delta.iter()) {
        if *delta == 0 {
            continue;
        }
        let arrow = if *delta > 0 { "▲" } else { "▼" };
        let color = match bucket.to_lowercase().as_str() {
            "red" | "severe" => theme.health_red,
            "yellow" | "warning" => theme.health_yellow,
            "green" | "ok" | "ready" => theme.health_green,
            "updating" | "launching" => theme.health_yellow,
            "terminating" | "terminated" => theme.health_red,
            _ => theme.muted,
        };
        line2.push(Span::raw(" "));
        line2.push(Span::styled(
            format!("{arrow}{} {}", delta.abs(), bucket),
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ));
    }
    line2.push(sep(theme));
    line2.extend(kv("Last", &last, theme));
    line2.push(sep(theme));
    line2.push(Span::raw("Status: "));
    line2.push(status);
    let sort_dir = if app.sort_desc { "↓" } else { "↑" };
    let sort_label = format!("{}{}", app.sort_key.label(), sort_dir);
    line2.push(sep(theme));
    line2.extend(kv("Sort", &sort_label, theme));
    if !app.filter.is_empty() {
        line2.push(sep(theme));
        let filter_text = app.filter.clone();
        line2.push(Span::styled("Filter: ", Style::default().fg(theme.muted)));
        line2.push(Span::styled(
            filter_text,
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    // Collect every contextual pill into a single Vec, then emit them as a
    // Powerline-style chain (triangular bridges flowing between pill bgs)
    // or as classic pill+sep pairs in other icon styles. Building the chain
    // up-front lets pill_chain compute the right-edge transition back to
    // the default bg.
    let mut chain_pills: Vec<(String, Color, Color)> = Vec::new();
    if app.grouped {
        chain_pills.push(("GROUPED".into(), Color::Black, theme.title_alt));
    }
    match app.view_mode {
        ViewMode::Compact => {
            chain_pills.push(("COMPACT".into(), Color::Black, theme.accent));
        }
        ViewMode::Spacious => {
            chain_pills.push(("SPACIOUS".into(), Color::Black, theme.accent));
        }
        ViewMode::Default => {}
    }
    if app.redact {
        chain_pills.push(("REDACT".into(), Color::Black, theme.health_yellow));
    }
    if app.alerts > 0 {
        chain_pills.push((
            format!(
                "! {} alert{}",
                app.alerts,
                if app.alerts == 1 { "" } else { "s" }
            ),
            Color::White,
            theme.health_red,
        ));
    }
    let in_flight: Vec<&str> = app
        .pending_actions
        .iter()
        .filter(|e| e.completed.is_none())
        .map(|e| e.label.as_str())
        .collect();
    if !in_flight.is_empty() {
        chain_pills.push((
            format!("⏳ {}", summarize_in_flight(&in_flight)),
            Color::Black,
            theme.health_yellow,
        ));
    }
    if app.frozen {
        chain_pills.push(("FROZEN".into(), Color::Black, theme.health_grey));
    }
    if app.read_only {
        chain_pills.push(("READ-ONLY".into(), Color::Black, theme.health_green));
    }
    if let Some(release) = app.update_available.as_ref() {
        chain_pills.push((
            format!("UPDATE {} (:update)", release.version),
            Color::Black,
            theme.title_alt,
        ));
    }
    if let Some(exp) = app.sso_expiry {
        let remaining = exp.signed_duration_since(chrono::Utc::now());
        if remaining > chrono::Duration::seconds(0) {
            let mins = remaining.num_minutes();
            let label = if mins >= 60 {
                format!("SSO {}h", remaining.num_hours())
            } else {
                format!("SSO {mins}m")
            };
            let bg = if mins < 15 {
                theme.health_red
            } else if mins < 60 {
                theme.health_yellow
            } else {
                theme.health_grey
            };
            chain_pills.push((label, Color::Black, bg));
        }
    }
    if !chain_pills.is_empty() {
        // In non-Powerline modes pill_chain doesn't add a leading sep, so
        // inject one here to space the chain from the preceding plain text.
        // In Powerline mode the chain starts with a coloured E0B2 wedge that
        // would otherwise sit flush against the preceding text — pad with
        // two spaces so the wedge has visual breathing room.
        if theme.icons != IconStyle::Powerline {
            line2.push(sep(theme));
        } else {
            line2.push(Span::raw("  "));
        }
        line2.extend(pill_chain(&chain_pills, theme));
    }

    // Breadcrumb: region / application / env — gives context at a glance.
    let crumb = breadcrumb_line(app);
    // Saved-filter tab bar — only rendered when the user has saved any.
    // Each chip is the filter name; the chip matching the currently-applied
    // filter is highlighted. The user activates with `:f NAME` or the palette.
    let mut paragraph_lines: Vec<Line> = vec![crumb, Line::from(line1), Line::from(line2)];
    if !app.named_filters.is_empty() {
        let mut chips: Vec<Span> =
            vec![Span::styled("Filters: ", Style::default().fg(theme.muted))];
        if theme.icons == IconStyle::Powerline {
            // Powerline mode: render the chip bar as a ribbon. Active chip
            // uses title_alt as its bg (the same colour the alerts pill
            // uses for "selected"); inactive chips use row_alt_bg so they
            // read as a faint trough behind the active one.
            let pills: Vec<(String, Color, Color)> = app
                .named_filters
                .iter()
                .map(|(name, value)| {
                    let active = !app.filter.is_empty() && value == &app.filter;
                    let (fg, bg) = if active {
                        (Color::Black, theme.title_alt)
                    } else {
                        (theme.muted, theme.row_alt_bg)
                    };
                    (name.to_string(), fg, bg)
                })
                .collect();
            chips.extend(pill_chain(&pills, theme));
        } else {
            for (name, value) in app.named_filters.iter() {
                let active = !app.filter.is_empty() && value == &app.filter;
                chips.push(Span::styled(
                    format!(" {name} "),
                    if active {
                        Style::default()
                            .fg(Color::Black)
                            .bg(theme.title_alt)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.muted)
                    },
                ));
                chips.push(Span::raw(" "));
            }
        }
        paragraph_lines.push(Line::from(chips));
    }
    let info =
        Paragraph::new(paragraph_lines).block(titled_block(theme, "ebman", false, theme.title));
    f.render_widget(info, cols[0]);

    let scope_label = match app.scope {
        Scope::Envs => "Envs",
        Scope::Apps => "Apps",
    };
    let context_panel = Paragraph::new(vec![
        Line::from(vec![
            Span::styled(
                "Elastic Beanstalk  ",
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            ),
            pill(scope_label, Color::Black, theme.title),
        ]),
        Line::from(Span::styled(
            "<tab> scope  <?> help  <:> command  </> filter  <q> quit",
            Style::default().fg(theme.muted),
        )),
    ])
    .alignment(Alignment::Right)
    .block(rounded_block(theme, false));
    f.render_widget(context_panel, cols[1]);
}

fn draw_apps_table(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme.clone();
    let header = Row::new(
        ["NAME", "VERSIONS", "CREATED", "UPDATED", "DESCRIPTION"].map(|h| {
            Cell::from(h).style(
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            )
        }),
    )
    .height(1);

    let now = chrono::Utc::now();
    let rows: Vec<Row> = app
        .applications
        .iter()
        .enumerate()
        .map(|(i, a)| {
            let age = |d: Option<chrono::DateTime<chrono::Utc>>| -> String {
                d.map(|t| humanize_age(now.signed_duration_since(t)))
                    .unwrap_or_else(|| "—".into())
            };
            let r = Row::new(vec![
                Cell::from(a.name.clone())
                    .style(Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
                Cell::from(a.version_count.to_string())
                    .style(Style::default().fg(theme.app_palette[0])),
                Cell::from(age(a.date_created)).style(Style::default().fg(theme.muted)),
                Cell::from(age(a.date_updated)).style(Style::default().fg(theme.muted)),
                Cell::from(a.description.clone()).style(Style::default().fg(theme.text)),
            ]);
            // Selection bg is layered on by Table::row_highlight_style; only
            // apply zebra striping here.
            if i % 2 == 0 {
                r.style(Style::default().bg(theme.row_alt_bg))
            } else {
                r
            }
        })
        .collect();
    let title = format!("Applications  {}", app.applications.len());
    let widths = [
        Constraint::Percentage(22),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Percentage(50),
    ];
    let popup_open = matches!(
        app.mode,
        Mode::Help | Mode::Picker | Mode::Command | Mode::Action | Mode::Filter
    );
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(
            Style::default()
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(cursor_marker(&theme))
        .block(
            titled_block(&theme, &title, !popup_open, theme.title).padding(Padding::horizontal(1)),
        );
    f.render_stateful_widget(table, area, &mut app.app_table_state);
}

fn draw_table(f: &mut Frame, area: Rect, app: &mut App) {
    app.table_area = area;
    let theme = app.theme.clone();
    let compact = app.view_mode == ViewMode::Compact;
    let spacious = app.view_mode == ViewMode::Spacious;
    let row_height: u16 = if spacious { 2 } else { 1 };
    let block_padding: u16 = if spacious { 2 } else { 1 };
    let indexes = app.filtered_indexes();

    // Column set varies by view mode + per-column hide list. The HEALTH dot
    // and TREND glyph share the HEALTH sort key but are addressed separately
    // when hiding (`:cols hide HEALTH` hides the dot; `:cols hide TREND` hides
    // the trend). The NAME column is always shown.
    let mut full = vec![
        ("NAME", SortKey::Name),
        ("APPLICATION", SortKey::App),
        ("TIER", SortKey::App),
        ("STATUS", SortKey::Status),
        ("HEALTH", SortKey::Health),
        ("TREND", SortKey::Health),
        ("PLATFORM", SortKey::Version),
        ("VERSION", SortKey::Version),
        ("CNAME", SortKey::Name),
        ("AGE", SortKey::Age),
    ];
    // REGION column only renders when the user has fanned across regions.
    if !app.multi_regions.is_empty() {
        full.insert(1, ("REGION", SortKey::App));
    }
    if compact {
        // Compact preset hides TREND + PLATFORM regardless of user pref.
        full.retain(|(label, _)| !matches!(*label, "TREND" | "PLATFORM"));
    }
    let columns: Vec<(&'static str, SortKey)> = full
        .into_iter()
        .filter(|(label, _)| {
            // NAME can never be hidden — it's the row identifier.
            if *label == "NAME" {
                return true;
            }
            !app.hidden_cols.contains(*label)
        })
        .collect();
    let sort_marker = if app.sort_desc { " ▼" } else { " ▲" };
    // TREND header advertises the window length (HISTORY_CAP samples × refresh
    // interval) so operators reading the column don't have to guess. Computed
    // once outside the per-column map.
    let trend_window =
        crate::app::humanize_short_age(app.refresh_interval * crate::app::HISTORY_CAP as u32);
    let header_cells: Vec<Cell> = columns
        .iter()
        .map(|(label, key)| {
            // The HEALTH column is rendered as the dot glyph but labelled "●"
            // in the header for the canonical column; sort marker only on it
            // (and the canonical NAME/APPLICATION/STATUS/VERSION/AGE columns).
            let display: std::borrow::Cow<'_, str> = if *label == "HEALTH" {
                "●".into()
            } else if *label == "TREND" {
                format!("TREND ({trend_window})").into()
            } else {
                (*label).into()
            };
            let mut text = display.into_owned();
            let primary_match = matches!(
                (key, app.sort_key),
                (SortKey::Name, SortKey::Name)
                    | (SortKey::App, SortKey::App)
                    | (SortKey::Status, SortKey::Status)
                    | (SortKey::Health, SortKey::Health)
                    | (SortKey::Age, SortKey::Age)
                    | (SortKey::Version, SortKey::Version)
            );
            let show_marker = primary_match && !matches!(*label, "TREND" | "CNAME" | "TIER");
            if show_marker {
                text.push_str(sort_marker);
            }
            Cell::from(text).style(
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            )
        })
        .collect();
    let header = Row::new(header_cells).height(1);

    // Per-application palette colour map is precomputed by App::rebuild_view
    // and stored on the app — rebuilding it here per frame is unnecessary.
    let app_colors = &app.cached_app_colors;

    // Hover only applies while the user is interacting with the table itself.
    let hover = if app.mode == Mode::Normal {
        app.hover_row
    } else {
        None
    };
    let display = app.display_rows();
    let now = chrono::Utc::now();
    let mut env_idx: usize = 0;
    let rows: Vec<Row> = display
        .iter()
        .enumerate()
        .map(|(row_idx, row)| match row {
            DisplayRow::Env(i) => {
                let env_position = env_idx;
                env_idx += 1;
                let e = &app.environments[*i];
                let color = app_colors
                    .get(&e.application)
                    .copied()
                    .unwrap_or(theme.text);
                let age = e
                    .updated
                    .map(|u| humanize_age(now.signed_duration_since(u)))
                    .unwrap_or_else(|| "—".into());

                let display_name = app
                    .aliases
                    .get(&e.name)
                    .cloned()
                    .unwrap_or_else(|| e.name.clone());
                let star = if app.pinned.contains(&e.name) {
                    "★ "
                } else {
                    ""
                };
                let checked = if app.multi_selected.contains(&e.name) {
                    "✓ "
                } else {
                    ""
                };
                // Transient "appeared on this refresh" marker. Stays only
                // for the cycle in which the env was first seen, so it
                // calls out new envs without sticking forever.
                let added_marker = if app.newly_added.contains(&e.name) {
                    "+ "
                } else {
                    ""
                };
                let alert = if app.newly_red.contains(&e.name) {
                    "▲ "
                } else {
                    ""
                };
                // Drift glyph: ◆ if env's configuration was updated in the last
                // 24h (someone deployed / changed options), ◇ if it's been
                // longer than 30 days (sleeping env that may be on stale runtime).
                let (drift_glyph, drift_color) = match e.updated {
                    Some(u) => {
                        let dur = now.signed_duration_since(u);
                        if dur < chrono::Duration::hours(24) && dur > chrono::Duration::zero() {
                            ("◆ ", theme.title_alt)
                        } else if dur > chrono::Duration::days(30) {
                            ("◇ ", theme.muted)
                        } else {
                            ("", theme.text)
                        }
                    }
                    None => ("", theme.text),
                };
                let name_cell = Cell::from(Line::from(vec![
                    Span::styled(
                        checked.to_string(),
                        Style::default()
                            .fg(theme.title_alt)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        star.to_string(),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        alert.to_string(),
                        Style::default()
                            .fg(theme.health_red)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        added_marker.to_string(),
                        Style::default()
                            .fg(theme.health_green)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        drift_glyph.to_string(),
                        Style::default()
                            .fg(drift_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        display_name,
                        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                    ),
                ]));
                let cells: Vec<Cell> = columns
                    .iter()
                    .map(|(label, _)| match *label {
                        "NAME" => name_cell.clone(),
                        "APPLICATION" => Cell::from(e.application.clone())
                            .style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
                        "TIER" => tier_cell(&e.tier, &theme),
                        "STATUS" => status_cell(&e.status, &theme),
                        "HEALTH" => Cell::from(health_dot(&e.health, &theme)),
                        "TREND" => Cell::from(sparkline_for(
                            app.history.get(&e.name),
                            &theme,
                            app.newly_red.contains(&e.name),
                        )),
                        "PLATFORM" => {
                            Cell::from(e.platform.clone()).style(Style::default().fg(theme.muted))
                        }
                        "VERSION" => Cell::from(format_version_label(
                            &e.version_label,
                            theme.app_palette[0],
                            theme.muted,
                        )),
                        "CNAME" => Cell::from(redact(&e.cname, app.redact))
                            .style(Style::default().fg(theme.muted)),
                        "AGE" => Cell::from(age.clone()).style(Style::default().fg(theme.muted)),
                        "REGION" => Cell::from(e.region.clone().unwrap_or_default())
                            .style(Style::default().fg(theme.accent)),
                        _ => Cell::from(""),
                    })
                    .collect();

                // Row tint priority: severity > hover > zebra. Selection is
                // handled by Table::row_highlight_style so it overlays cleanly.
                let is_hover = hover == Some(row_idx);
                let bg = if e.health.eq_ignore_ascii_case("Red")
                    || e.health.eq_ignore_ascii_case("Severe")
                {
                    Some(theme.row_red_bg)
                } else if e.health.eq_ignore_ascii_case("Yellow") {
                    Some(theme.row_yellow_bg)
                } else if is_hover {
                    Some(theme.row_hover_bg)
                } else if env_position.is_multiple_of(2) {
                    Some(theme.row_alt_bg)
                } else {
                    None
                };
                let style = match bg {
                    Some(c) => Style::default().bg(c),
                    None => Style::default(),
                };
                Row::new(cells).style(style).height(row_height)
            }
            DisplayRow::Separator => {
                // Resolve the next app's name + color via the same
                // look-ahead pattern; we use the name for the Powerline
                // ribbon and the color for the dashed fill in other styles.
                let (next_app_name, next_color) = display
                    .iter()
                    .skip(row_idx + 1)
                    .find_map(|r| match r {
                        DisplayRow::Env(i) => {
                            let env = &app.environments[*i];
                            Some((
                                env.application.clone(),
                                app_colors
                                    .get(&env.application)
                                    .copied()
                                    .unwrap_or(theme.muted),
                            ))
                        }
                        _ => None,
                    })
                    .unwrap_or_else(|| (String::new(), theme.muted));
                // Walk forward from this separator until the next one to
                // collect the envs in this group; compute "3 envs · 1 red"
                // style summary so operators see per-app health without
                // scanning rows.
                let group_envs: Vec<&Environment> = display
                    .iter()
                    .skip(row_idx + 1)
                    .map_while(|r| match r {
                        DisplayRow::Env(i) => Some(&app.environments[*i]),
                        DisplayRow::Separator => None,
                    })
                    .collect();
                let summary = summarize_group(&group_envs);
                let dashes = "─".repeat(DIVIDER_FILL_WIDTH);
                let count = columns.len();
                if theme.icons == IconStyle::Powerline && !next_app_name.is_empty() {
                    // Per-app coloured ribbon banner. NAME cell holds a
                    // wedge-pill-wedge ribbon (left E0B2 cap + pill + right
                    // E0B0 cap) so the next-app section starts with its
                    // name visible in its own colour. Remaining cells stay
                    // as dashes in the same colour for visual continuity.
                    let summary_text = summary.clone();
                    let cells: Vec<Cell> = columns
                        .iter()
                        .enumerate()
                        .map(|(i, (label, _))| {
                            if i == 0 && *label == "NAME" {
                                Cell::from(Line::from(vec![
                                    Span::styled("\u{e0b2}", Style::default().fg(next_color)),
                                    Span::styled(
                                        format!(" {next_app_name} "),
                                        Style::default()
                                            .fg(Color::Black)
                                            .bg(next_color)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled("\u{e0b0}", Style::default().fg(next_color)),
                                ]))
                            } else if i == 1 {
                                // Summary lives in the column right after
                                // the name banner — long enough that the
                                // counts have room and short enough that
                                // it doesn't push into PLATFORM.
                                Cell::from(Span::styled(
                                    format!(" {summary_text} "),
                                    Style::default().fg(theme.muted),
                                ))
                            } else {
                                Cell::from(Span::styled(
                                    dashes.clone(),
                                    Style::default().fg(next_color),
                                ))
                            }
                        })
                        .collect();
                    Row::new(cells)
                } else {
                    let cells = (0..count).map(|_| {
                        Cell::from(Span::styled(
                            dashes.clone(),
                            Style::default().fg(next_color),
                        ))
                    });
                    Row::new(cells)
                }
            }
        })
        .collect();

    let title = format!("Environments  {}/{}", indexes.len(), app.environments.len());
    let widths: Vec<Constraint> = columns
        .iter()
        .map(|(label, _)| match *label {
            "NAME" => Constraint::Percentage(14),
            "APPLICATION" => Constraint::Percentage(12),
            "TIER" => Constraint::Length(7),
            "STATUS" => Constraint::Length(10),
            "HEALTH" => Constraint::Length(3),
            "TREND" => Constraint::Length(22),
            "PLATFORM" => Constraint::Percentage(15),
            "VERSION" => Constraint::Percentage(10),
            "CNAME" => Constraint::Percentage(14),
            "AGE" => Constraint::Length(6),
            _ => Constraint::Length(6),
        })
        .collect();
    let popup_open = matches!(
        app.mode,
        Mode::Help | Mode::Picker | Mode::Command | Mode::Action | Mode::Filter
    );
    let block = titled_block(&theme, &title, !popup_open, theme.title)
        .padding(Padding::horizontal(block_padding));
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(
            Style::default()
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(cursor_marker(&theme))
        .block(block);

    // Build the hover preview (if any) before the stateful render, because
    // both touch `app` and the borrow-checker rejects overlapping borrows.
    let hover_preview: Option<(Rect, String)> = hover.and_then(|idx| match display.get(idx)? {
        DisplayRow::Env(i) => {
            let e = &app.environments[*i];
            let alias_part = match app.aliases.get(&e.name) {
                Some(a) => format!("  alias \"{a}\""),
                None => String::new(),
            };
            let preview = format!(
                " ⓘ {}{}  ·  {}  ·  {} / {}  ·  {}  ·  {}",
                e.name,
                alias_part,
                e.application,
                e.status,
                e.health,
                e.platform,
                redact(&e.cname, app.redact),
            );
            let row = Rect {
                x: area.x,
                y: area.y + area.height.saturating_sub(1),
                width: area.width,
                height: 1,
            };
            Some((row, preview))
        }
        _ => None,
    });

    let env_count_total = app.environments.len();
    let env_count_visible = indexes.len();

    f.render_stateful_widget(table, area, &mut app.table_state);

    // Empty-state overlay: friendly message when there are no envs at all,
    // or when a filter has hidden everything. Echoes the live filter text
    // back so the operator can see what's hiding their rows.
    if env_count_visible == 0 {
        let heading: String;
        let hint: String;
        if env_count_total == 0 {
            heading = "no environments in this account / region".to_string();
            hint = "try a different region (r) or profile (p), or check the AWS console (b)"
                .to_string();
        } else if app.filter.is_empty() {
            heading = "no environments match the active view".to_string();
            hint = "press `views` to switch back to default, or `:filters` to drop a saved one"
                .to_string();
        } else {
            heading = format!("no environments match  `{}`", app.filter);
            hint = "press / to edit, or Esc in filter mode to clear".to_string();
        }
        let block_height: u16 = 4;
        let inner = Rect {
            x: area.x + 2,
            y: area
                .y
                .saturating_add(area.height.saturating_sub(block_height) / 2),
            width: area.width.saturating_sub(4),
            height: block_height.min(area.height),
        };
        let lines = vec![
            Line::from(Span::styled(
                heading,
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center),
            Line::from(Span::raw("")),
            Line::from(Span::styled(hint, Style::default().fg(theme.muted)))
                .alignment(Alignment::Center),
        ];
        f.render_widget(Paragraph::new(lines), inner);
    }

    if let Some((row, preview)) = hover_preview {
        let para = Paragraph::new(Span::styled(
            preview,
            Style::default()
                .bg(theme.row_hover_bg)
                .fg(theme.text)
                .add_modifier(Modifier::DIM),
        ));
        f.render_widget(Clear, row);
        f.render_widget(para, row);
    }

    if app.show_minimap {
        draw_minimap(f, area, app);
    }
}

/// Render a small picture-in-picture minimap of all envs in the top-right
/// corner of the table area. Each env is a single coloured cell driven by
/// health (Green / Yellow / Red / Grey). Capped to a 4-row × 30-column box
/// so it doesn't dominate narrow terminals; if there are more envs than
/// fit, the rest are dropped.
fn draw_minimap(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let envs = &app.environments;
    if envs.is_empty() {
        return;
    }
    let max_w: u16 = area.width.saturating_sub(2).min(30);
    let max_h: u16 = 4.min(area.height.saturating_sub(2));
    if max_w == 0 || max_h == 0 {
        return;
    }
    let capacity = (max_w as usize) * (max_h as usize);
    let to_show: Vec<&crate::aws::Environment> = envs.iter().take(capacity).collect();
    let needed_w: u16 = (to_show.len() as u16).min(max_w);
    let rows_needed: u16 = (to_show.len() as u16).div_ceil(max_w);
    let rows: u16 = rows_needed.min(max_h);
    let map_rect = Rect {
        x: area.x + area.width.saturating_sub(needed_w + 2),
        y: area.y + 1,
        width: needed_w + 2,
        height: rows + 2,
    };
    f.render_widget(Clear, map_rect);
    f.render_widget(
        rounded_block(theme, false)
            .border_style(Style::default().fg(theme.muted))
            .title(Span::styled(
                " minimap ",
                Style::default().fg(theme.title_alt),
            )),
        map_rect,
    );
    for (i, e) in to_show.iter().enumerate() {
        let row_idx = (i as u16) / max_w;
        let col_idx = (i as u16) % max_w;
        if row_idx >= rows {
            break;
        }
        let cell = Rect {
            x: map_rect.x + 1 + col_idx,
            y: map_rect.y + 1 + row_idx,
            width: 1,
            height: 1,
        };
        let color = match e.health.to_lowercase().as_str() {
            "green" | "ok" => theme.health_green,
            "yellow" | "warning" => theme.health_yellow,
            "red" | "severe" => theme.health_red,
            _ => theme.health_grey,
        };
        f.render_widget(
            Paragraph::new(Span::styled("█", Style::default().fg(color))),
            cell,
        );
    }
}

fn tier_cell(tier: &str, theme: &Theme) -> Cell<'static> {
    match tier {
        "Worker" => Cell::from(pill("Worker", Color::Black, theme.accent)),
        "Web" => Cell::from(Span::styled("Web", Style::default().fg(theme.muted))),
        other => Cell::from(Span::styled(
            other.to_string(),
            Style::default().fg(theme.muted),
        )),
    }
}

fn status_cell(status: &str, theme: &Theme) -> Cell<'static> {
    Cell::from(status_pill(status, theme))
}

/// Render a status string as a coloured pill. Pulled out of `status_cell`
/// so the Detail header (and any other non-table consumer) can drop the
/// same pill into a Line without the Cell wrapper.
fn status_pill(status: &str, theme: &Theme) -> Span<'static> {
    let lower = status.to_lowercase();
    if lower == "ready" {
        pill("Ready", Color::Black, theme.status_ready)
    } else if matches!(lower.as_str(), "updating" | "launching") {
        pill(status, Color::Black, theme.status_updating)
    } else if matches!(lower.as_str(), "terminating" | "terminated") {
        pill(status, Color::White, theme.status_terminating)
    } else {
        Span::styled(status.to_string(), Style::default().fg(theme.text))
    }
}

fn draw_events(f: &mut Frame, area: Rect, app: &App) {
    let scope_suffix = match app.events_for_env.as_deref() {
        Some(env) => format!(" · {env}"),
        None => " · all envs".to_string(),
    };
    let title = format!("Events  {}{}", app.events.len(), scope_suffix);
    let block =
        titled_block(&app.theme, &title, true, app.theme.title).padding(Padding::horizontal(1));

    if app.events.is_empty() {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  ◌  no events yet",
                Style::default()
                    .fg(app.theme.muted)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "     they appear on the next refresh — ^R now, or wait for the tick",
                Style::default().fg(app.theme.muted),
            )),
        ];
        let p = Paragraph::new(lines).block(block);
        f.render_widget(p, area);
        return;
    }

    let now = chrono::Utc::now();
    let lines: Vec<Line> = app
        .events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let when = match e.at {
                Some(t) => humanize_age(now.signed_duration_since(t)),
                None => "—".into(),
            };
            let sev_style = severity_style(&e.severity, &app.theme);
            let is_cursor = app.events_cursor == Some(i);
            let marker = if is_cursor { "▶ " } else { "  " };
            let marker_style = if is_cursor {
                Style::default()
                    .fg(app.theme.title_alt)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(app.theme.muted)
            };
            Line::from(vec![
                Span::styled(marker, marker_style),
                Span::styled(
                    format!("{:>4} ", when),
                    Style::default().fg(app.theme.muted),
                ),
                Span::styled(format!("{:<5} ", e.severity), sev_style),
                Span::styled(
                    format!("{} ", env_label(e)),
                    Style::default()
                        .fg(app.theme.text)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(e.message.clone()),
            ])
        })
        .collect();

    let para = Paragraph::new(lines)
        .block(block)
        .scroll((app.events_scroll, 0));
    f.render_widget(para, area);
}

fn env_label(e: &crate::aws::Event) -> String {
    if e.env.is_empty() {
        if e.application.is_empty() {
            "—".into()
        } else {
            format!("[{}]", e.application)
        }
    } else if e.application.is_empty() {
        format!("[{}]", e.env)
    } else {
        format!("[{}/{}]", e.application, e.env)
    }
}

fn severity_style(s: &str, theme: &Theme) -> Style {
    match s.to_uppercase().as_str() {
        "ERROR" | "FATAL" => Style::default()
            .fg(theme.health_red)
            .add_modifier(Modifier::BOLD),
        "WARN" => Style::default()
            .fg(theme.health_yellow)
            .add_modifier(Modifier::BOLD),
        "INFO" => Style::default().fg(theme.text),
        "DEBUG" | "TRACE" => Style::default().fg(theme.muted),
        _ => Style::default().fg(theme.text),
    }
}

fn draw_footer(f: &mut Frame, area: Rect, app: &App) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(1)])
        .split(area);

    // Top row: contextual state (filter input, command input, active filter, status/error message, or blank).
    let mut top: Vec<Span> = Vec::new();
    let theme = &app.theme;
    match app.mode {
        Mode::Filter => {
            top.push(Span::styled(
                " /",
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            top.push(Span::raw(" "));
            top.push(Span::styled(
                app.filter.clone(),
                Style::default().fg(theme.text),
            ));
            top.push(Span::styled(
                caret_glyph(theme),
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
            ));
            top.push(Span::styled(
                "  [enter] apply  [esc] cancel",
                Style::default().fg(theme.muted),
            ));
        }
        Mode::Command => {
            top.push(Span::styled(
                " :",
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            ));
            top.push(Span::styled(
                app.command_input.clone(),
                Style::default().fg(theme.text),
            ));
            top.push(Span::styled(
                caret_glyph(theme),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::SLOW_BLINK),
            ));
            top.push(Span::styled(
                "   [enter] run  [esc] cancel",
                Style::default().fg(theme.muted),
            ));
        }
        Mode::QuickJump => {
            top.push(Span::styled(
                " '",
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::BOLD),
            ));
            top.push(Span::raw(" "));
            top.push(Span::styled(
                app.quickjump_input.clone(),
                Style::default().fg(app.theme.text),
            ));
            top.push(Span::styled(
                caret_glyph(theme),
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::SLOW_BLINK),
            ));
            top.push(Span::styled(
                "   jump to env by name prefix   [enter] keep   [esc] cancel",
                Style::default().fg(app.theme.muted),
            ));
        }
        _ => {
            if let Some(msg) = &app.error_message {
                top.push(Span::styled(
                    format!(" {msg}"),
                    Style::default().fg(theme.health_red),
                ));
            } else if let Some(msg) = &app.status_message {
                top.push(Span::styled(
                    format!(" {msg}"),
                    Style::default().fg(theme.health_yellow),
                ));
            } else if !app.filter.is_empty() {
                top.push(Span::styled(
                    format!(" filter: {}", app.filter),
                    Style::default().fg(theme.health_yellow),
                ));
            } else if let Some(hint) = context_hint(app) {
                // Context-aware nudge — only fires when the status / error
                // / filter slots are empty so it doesn't trample anything
                // the user is actively reading.
                top.push(Span::styled(
                    format!(" 💡 {hint}"),
                    Style::default().fg(theme.muted),
                ));
            }
        }
    }
    f.render_widget(Paragraph::new(Line::from(top)), rows[0]);

    // Bottom row: key strip — always visible, mode-aware.
    let keys: String = match app.mode {
        Mode::Filter => " type to filter   [enter] apply   [esc] cancel".into(),
        Mode::Help => " j/k scroll   ? / esc / q   close help".into(),
        Mode::Picker => " j/k move   type to filter   [enter] select   [esc] cancel".into(),
        Mode::Command => " type a command   [enter] run   [esc] cancel  (try :help)".into(),
        Mode::QuickJump => " type env name prefix   [enter] keep selection   [esc] cancel".into(),
        Mode::Palette => " type to fuzzy-find   ↑/↓ move   [enter] run   [esc] cancel".into(),
        Mode::Normal => {
            // Focus-aware key strip: the events panel has its own navigation.
            match app.focus {
                crate::app::Focus::Events if app.events_visible => {
                    " EVENTS  j/k cursor   y yank line   ^] back to table   ^E hide   esc / q".into()
                }
                _ => " j/k move  enter drill  a actions  / filter  : command  ^K palette  r region  p profile  ^R refresh  ? help  q quit".into(),
            }
        }
        Mode::Detail => match app.detail.as_ref().map(|d| d.tab()) {
            Some(crate::app::DetailTab::Instances) => {
                " INSTANCES  j/k move  enter console  s ssm shell  y yank id  x terminate  a actions  ^R refresh  ? help  esc / q back".into()
            }
            _ => " tab/shift-tab switch  j/k scroll  a actions  ^R refresh  R auto-refresh  ? help  esc / q back".into(),
        },
        Mode::Action => " j/k move  enter confirm  ? help  esc / q cancel".into(),
        Mode::Dlq => match app.dlq.as_ref().map(|d| d.viewing) {
            Some(crate::app::QueueView::Main) => {
                " MAIN  j/k move  enter view body  x delete  m → DLQ  ^R refresh  ? help  esc / q back".into()
            }
            _ => " DLQ  j/k move  enter view body  r resend  x delete  p purge  m → MAIN  ^R refresh  ? help  esc / q back".into(),
        },
        Mode::Shell => {
            // Keystrokes are forwarded to the subprocess; F12 detaches.
            " SHELL  keys → subprocess  ·  F12 detach back to ebman  ·  ^D / exit closes".into()
        }
        Mode::Form => " FORM  tab/↓↑ field  type to edit  ^S submit  esc cancel".into(),
    };
    // No Wrap — the strip is intentionally compact; longer mode-specific
    // strips that exceed one row get a horizontal scroll bar visually
    // (truncation) rather than wrapping into the body region. Mode key
    // strips are kept ≤ ~150 chars to fit standard terminals.
    f.render_widget(
        Paragraph::new(Span::styled(keys, Style::default().fg(theme.muted))),
        rows[1],
    );
}

fn draw_help(f: &mut Frame, area: Rect, app: &App) {
    let theme = &app.theme;
    let popup = centered_rect(70, 70, area);
    f.render_widget(Clear, popup);
    // Per-context help: when the user pressed `?` inside Detail / DLQ /
    // Action / Shell, show only the keys relevant to that screen. The
    // global keymap is still available via `?` from Normal mode.
    match app.help_topic {
        crate::app::HelpTopic::Detail => return draw_help_detail(f, popup, app),
        crate::app::HelpTopic::Dlq => return draw_help_dlq(f, popup, app),
        crate::app::HelpTopic::Action => return draw_help_action(f, popup, app),
        crate::app::HelpTopic::Shell => return draw_help_shell(f, popup, app),
        crate::app::HelpTopic::SavedConfigs => {
            return draw_help_saved_configs(f, popup, app);
        }
        crate::app::HelpTopic::Global => {}
    }

    let interval_secs = app.refresh_interval.as_secs();
    let lines = vec![
        Line::from(Span::styled(
            "ebman — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / ↓ / wheel", "move selection down", theme),
        help_line("k / ↑ / wheel", "move selection up", theme),
        help_line("g / G", "jump to top / bottom", theme),
        help_line("enter", "open drill-down view for the selected env", theme),
        help_line("a", "open actions menu (rebuild / restart / swap / terminate)", theme),
        help_line("b", "open selected env in the AWS console", theme),
        help_line("D", "describe overlay (raw env dump as JSON)", theme),
        help_line("f", "freeze / unfreeze auto-refresh", theme),
        help_line("1 - 9", "jump to env at position 1-9 in the current view", theme),
        help_line("'", "name-jump: type a prefix to move selection", theme),
        help_line("Ctrl-W", "yank equivalent `aws elasticbeanstalk describe-environments` command", theme),
        help_line("tab / shift-tab", "cycle scope (envs ↔ apps)", theme),
        help_line("click", "select row", theme),
        help_line("/", "filter rows (name, app, status, health)", theme),
        help_line("s / S", "cycle sort key / toggle ascending", theme),
        help_line("Ctrl-G", "toggle group-by-application", theme),
        help_line("Ctrl-E", "toggle events panel", theme),
        help_line("y / Y", "yank CNAME / name to clipboard", theme),
        help_line("Ctrl-Y", "export filtered table as TSV to clipboard", theme),
        help_line("r", "switch AWS region", theme),
        help_line("p", "switch AWS profile", theme),
        help_line("Ctrl-K", "command palette: fuzzy search across commands / envs / views / plugins", theme),
        help_line("Ctrl-R / F5", "refresh now", theme),
        help_line("Ctrl-X", "toggle redact mode (account id, ARN, CNAMEs)", theme),
        help_line("?", "toggle this help", theme),
        help_line("q / Ctrl-C", "quit", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Command bar (press :)",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        help_line(":q", "quit", theme),
        help_line(":region X", "switch AWS region", theme),
        help_line(":profile X", "switch AWS profile", theme),
        help_line(":sort KEY [desc]", "set sort (name/app/status/health/version/age)", theme),
        help_line(":group on|off", "toggle grouping", theme),
        help_line(":redact on|off", "toggle redact mode", theme),
        help_line(":save NAME", "save current filter as NAME", theme),
        help_line(":f NAME / :filter NAME", "recall a saved filter", theme),
        help_line(":filters / :drop NAME", "list / remove saved filters", theme),
        help_line(":events on|off", "toggle the events panel", theme),
        help_line(":export / :json / :report", "copy filtered table (TSV / JSON / Markdown)", theme),
        help_line(":refresh", "re-fetch the table immediately", theme),
        help_line(":readonly on|off", "toggle destructive-action lockout", theme),
        help_line(":alias NAME LABEL", "set or update a local env alias", theme),
        help_line(":alias-drop NAME", "remove an alias", theme),
        help_line(":pin", "pin / unpin the selected env (also `*`)", theme),
        help_line(":whatsnew", "embedded changelog popup", theme),
        help_line(":save-view NAME", "snapshot filter/sort/grouping/scope under NAME", theme),
        help_line(":view NAME", "load a previously saved view", theme),
        help_line(":views / :view-drop NAME", "list / remove saved views", theme),
        help_line(":history", "show recent info/error messages", theme),
        help_line(":cols", "list / hide / show / reset columns (e.g. :cols hide PLATFORM)", theme),
        help_line(":diff NAME", "side-by-side comparison with another env", theme),
        help_line(":alarms", "CloudWatch alarms list for selected env", theme),
        help_line(":loglevel LEVEL", "live-reload tracing filter (trace/debug/info/warn/error)", theme),
        help_line(":saved-configs", "list EB saved configuration templates per application", theme),
        help_line(":plugins  /  :NAME", "list / invoke plugin commands defined in commands.toml", theme),
        help_line("[ / ] (Metrics tab)", "decrease / increase metric range (15m → 24h)", theme),
        help_line("(Logs tab) ^R", "request tail logs (takes ~10–20s while EB samples instances)", theme),
        help_line("(Logs tab) s", "open CW Logs streaming overlay (live tail; needs `:logs-stream on`)", theme),
        help_line("(Logs tab) /", "regex-filter the visible log lines", theme),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "Refresh runs automatically every {interval_secs}s. Theme: {}. Configurable in ~/.config/ebman/config.toml.",
                app.theme.name
            ),
            Style::default().fg(app.theme.muted),
        )),
        Line::from(Span::styled(
            "Region/profile come from the standard AWS env (AWS_REGION, AWS_PROFILE).",
            Style::default().fg(app.theme.muted),
        )),
    ];
    let help = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.help_scroll, 0))
        .block(
            titled_block(&app.theme, "help", true, app.theme.title_alt)
                .padding(Padding::uniform(1)),
        );
    f.render_widget(help, popup);
}

fn draw_dlq(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme.clone();
    let Some(dlq) = app.dlq.as_mut() else { return };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(2),
        ])
        .split(area);

    // Header — adapts to which queue is currently loaded.
    let (window_title, view_label, accent) = match dlq.viewing {
        crate::app::QueueView::Main => ("Main Worker Queue", "MAIN", theme.health_yellow),
        crate::app::QueueView::Dlq => ("Dead-Letter Queue", "DLQ", theme.health_red),
    };
    let header = Paragraph::new(Line::from(vec![
        Span::styled(format!("{view_label}: "), Style::default().fg(theme.muted)),
        Span::styled(
            dlq.env_name.clone(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::raw("   "),
        Span::styled(
            format!("{} messages", dlq.messages.len()),
            Style::default().fg(theme.health_yellow),
        ),
        if dlq.confirm_delete_idx.is_some() {
            Span::styled(
                "   ⚠ delete this message? y / n",
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("")
        },
    ]))
    .block(titled_block(&theme, window_title, true, accent));
    f.render_widget(header, chunks[0]);

    // Message list
    let block = rounded_block(&theme, true);
    if dlq.messages.is_empty() {
        let p = Paragraph::new(Span::styled(
            if dlq.loading {
                "loading messages…"
            } else {
                "no messages in DLQ"
            },
            Style::default().fg(theme.muted),
        ))
        .block(block);
        f.render_widget(p, chunks[1]);
    } else {
        let now = chrono::Utc::now();
        let items: Vec<ListItem> = dlq
            .messages
            .iter()
            .map(|m| {
                let age = m
                    .sent_at
                    .map(|t| humanize_age(now.signed_duration_since(t)))
                    .unwrap_or_else(|| "—".into());
                let preview = m.body.lines().next().unwrap_or("").to_string();
                let preview = if preview.len() > 80 {
                    format!("{}…", &preview[..80])
                } else {
                    preview
                };
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!(" {:<20} ", m.id),
                        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("recv:{:<3} ", m.receive_count),
                        Style::default().fg(theme.health_yellow),
                    ),
                    Span::styled(format!("{:>5} ", age), Style::default().fg(theme.muted)),
                    Span::raw(preview),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(theme.row_selected_bg)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(cursor_marker(&theme));
        f.render_stateful_widget(list, chunks[1], &mut dlq.list_state);
    }

    // Footer / confirm
    if dlq.confirm_purge {
        let line = Paragraph::new(Line::from(vec![
            Span::styled(
                " PURGE DLQ — type ",
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                dlq.env_name.clone(),
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " to confirm: ",
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                dlq.purge_typed.clone(),
                Style::default()
                    .fg(if dlq.purge_typed == dlq.env_name {
                        theme.health_green
                    } else {
                        theme.text
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                caret_glyph(&theme),
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
            ),
        ]));
        f.render_widget(line, chunks[2]);
    } else {
        let keys = match dlq.viewing {
            crate::app::QueueView::Main => {
                " MAIN  j/k move  enter view body  x delete  m → DLQ  ^R refresh  esc / q back"
            }
            crate::app::QueueView::Dlq => {
                " DLQ  j/k move  enter view body  r resend  x delete  p purge  m → MAIN  ^R refresh  esc / q back"
            }
        };
        let footer = Paragraph::new(vec![
            Line::from(match &dlq.error {
                Some(err) => Span::styled(format!(" {err}"), Style::default().fg(theme.health_red)),
                None => Span::raw(""),
            }),
            Line::from(Span::styled(keys, Style::default().fg(theme.muted))),
        ]);
        f.render_widget(footer, chunks[2]);
    }
}

fn draw_action(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme.clone();
    let Some(flow) = app.action_flow.as_mut() else {
        return;
    };
    match flow {
        ActionFlow::Menu { list_state } => {
            let popup = centered_rect(50, 40, area);
            f.render_widget(Clear, popup);
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(3), Constraint::Length(1)])
                .split(popup);
            let items: Vec<ListItem> = ACTIONS
                .iter()
                .map(|a| {
                    let style = if a.destructive() {
                        Style::default()
                            .fg(theme.health_red)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.text)
                    };
                    // Per-action glyph in muted (or red for destructive) so the
                    // shape carries the signal without competing with the label.
                    let glyph_style = if a.destructive() {
                        Style::default().fg(theme.health_red)
                    } else {
                        Style::default().fg(theme.title_alt)
                    };
                    ListItem::new(Line::from(vec![
                        Span::styled(format!(" {} ", a.glyph(theme.icons)), glyph_style),
                        Span::styled(format!("{} ", a.label()), style),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .block(titled_block(&theme, "action", true, theme.title_alt))
                .highlight_style(
                    Style::default()
                        .bg(theme.row_selected_bg)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol(cursor_marker(&theme));
            f.render_stateful_widget(list, layout[0], list_state);
            f.render_widget(
                Paragraph::new(Span::styled(
                    " j/k move   [enter] select   [esc] cancel",
                    Style::default().fg(theme.muted),
                )),
                layout[1],
            );
        }
        ActionFlow::SwapTarget { source, picker } => {
            let popup = centered_rect(50, 60, area);
            f.render_widget(Clear, popup);
            let layout = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(3),
                    Constraint::Min(3),
                    Constraint::Length(1),
                ])
                .split(popup);
            let title = format!("swap CNAMEs: {source} ↔ ?");
            let block = titled_block(&theme, &title, true, theme.title_alt);
            let prompt = Paragraph::new(Line::from(vec![
                Span::styled(
                    " /",
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(picker.filter.clone(), Style::default().fg(theme.text)),
                Span::styled(
                    caret_glyph(&theme),
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::SLOW_BLINK),
                ),
            ]))
            .block(block);
            f.render_widget(prompt, layout[0]);
            let filtered = picker.filtered();
            let items: Vec<ListItem> = filtered
                .iter()
                .map(|i| {
                    let name = picker.items[*i].clone();
                    ListItem::new(Line::from(Span::styled(
                        format!(" {name}"),
                        Style::default().fg(theme.text),
                    )))
                })
                .collect();
            let list = List::new(items)
                .block(rounded_block(&theme, true))
                .highlight_style(
                    Style::default()
                        .bg(theme.row_selected_bg)
                        .add_modifier(Modifier::BOLD),
                );
            let mut vs = ratatui::widgets::ListState::default();
            if let Some(real) = picker.list_state.selected() {
                vs.select(filtered.iter().position(|i| *i == real));
            }
            f.render_stateful_widget(list, layout[1], &mut vs);
            f.render_widget(
                Paragraph::new(Span::styled(
                    " j/k move   type to filter   [enter] confirm   [esc] cancel",
                    Style::default().fg(theme.muted),
                )),
                layout[2],
            );
        }
        ActionFlow::Confirm(modal) => {
            let popup = centered_rect(60, 35, area);
            f.render_widget(Clear, popup);
            let accent = if modal.action.destructive() {
                theme.health_red
            } else {
                theme.title_alt
            };
            let block = rounded_block(&theme, true)
                .border_style(Style::default().fg(accent))
                .title(Span::styled(
                    " confirm ",
                    Style::default().fg(accent).add_modifier(Modifier::BOLD),
                ));
            let mut lines: Vec<Line> = Vec::new();
            let summary = match modal.action {
                Action::Rebuild => format!(
                    "Rebuild environment '{}'? (terminates and recreates all instances)",
                    modal.target_env
                ),
                Action::RestartAppServer => {
                    format!("Restart app server on environment '{}'?", modal.target_env)
                }
                Action::SwapCnames => format!(
                    "Swap CNAMEs between '{}' and '{}'?",
                    modal.target_env,
                    modal.swap_with.as_deref().unwrap_or("?")
                ),
                Action::Terminate => format!(
                    "TERMINATE environment '{}'. This cannot be undone.",
                    modal.target_env
                ),
                Action::Deploy => format!(
                    "Deploy version '{}' to environment '{}'? (rolling, reversible)",
                    modal.deploy_version.as_deref().unwrap_or("?"),
                    modal.target_env
                ),
                Action::UpgradePlatform => format!(
                    "Upgrade '{}' to platform: {} (rolling, reversible)",
                    modal.target_env,
                    modal.upgrade_platform_label.as_deref().unwrap_or("?")
                ),
                Action::Clone => format!(
                    "Clone '{}' into a new env named '{}'? (creates a new env)",
                    modal.target_env,
                    modal.clone_target.as_deref().unwrap_or("?")
                ),
                Action::Scale => format!(
                    "Scale '{}' to min={} / max={}? (rolling)",
                    modal.target_env,
                    modal.scale_min.unwrap_or(0),
                    modal.scale_max.unwrap_or(0)
                ),
                Action::AbortUpdate => format!("Abort current update on '{}'?", modal.target_env),
                // These variants never reach the ConfirmModal — they're
                // dispatched directly from command paths. Placeholder copy
                // keeps the match exhaustive without dead UI.
                Action::ConfigSave
                | Action::ConfigDelete
                | Action::ConfigApply
                | Action::TerminateInstance => {
                    format!("{} on '{}'", modal.action.label(), modal.target_env)
                }
            };
            lines.push(Line::from(""));
            // Render the env name in red+bold for destructive actions so
            // the operator can't miss what's about to be nuked even if
            // they scan the modal too fast to read the full sentence.
            let body_style = Style::default()
                .fg(if modal.action.destructive() {
                    theme.health_red
                } else {
                    theme.text
                })
                .add_modifier(Modifier::BOLD);
            let name_style = if modal.action.destructive() {
                Style::default()
                    .fg(theme.health_red)
                    .bg(theme.row_red_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD)
            };
            lines.push(highlight_env_in_summary(
                &summary,
                &modal.target_env,
                body_style,
                name_style,
            ));
            // Pre-flight traffic-level warning if anything noteworthy is in
            // progress (mid-deploy, recent change, currently Red). Rendered
            // before the dry-run info so the operator sees state-level concerns
            // first.
            if let Some(w) = &modal.traffic_warning {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  ⚠ {w}"),
                    Style::default()
                        .fg(theme.health_red)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            // Dry-run preview: instance count + AZ spread, when available.
            if modal.loading_dryrun {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  fetching impact…",
                    Style::default().fg(theme.muted),
                )));
            } else if let Some(dr) = &modal.dryrun {
                lines.push(Line::from(""));
                let inst_word = if dr.instance_count == 1 {
                    "instance"
                } else {
                    "instances"
                };
                let az_word = if dr.az_count == 1 { "AZ" } else { "AZs" };
                lines.push(Line::from(Span::styled(
                    format!(
                        "  impact: {} {inst_word} across {} {az_word}",
                        dr.instance_count, dr.az_count
                    ),
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                )));
            }
            // Pre-flight events: last 3 events on this env.
            if let Some(events) = &modal.recent_events {
                if !events.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "  recent events:",
                        Style::default().fg(theme.muted),
                    )));
                    let now = chrono::Utc::now();
                    for e in events.iter().take(3) {
                        let when = match e.at {
                            Some(t) => humanize_age(now.signed_duration_since(t)),
                            None => "—".into(),
                        };
                        // Full message — the modal wraps now, so we no
                        // longer truncate mid-word.
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("    {when:>4}  "),
                                Style::default().fg(theme.muted),
                            ),
                            Span::styled(
                                format!("{:<5}  ", e.severity),
                                severity_style(&e.severity, &theme),
                            ),
                            Span::styled(e.message.clone(), Style::default().fg(theme.text)),
                        ]));
                    }
                }
            } else if modal.loading_events {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  fetching recent events…",
                    Style::default().fg(theme.muted),
                )));
            }
            lines.push(Line::from(""));
            match modal.kind {
                ConfirmKind::YesNo => {
                    lines.push(Line::from(Span::styled(
                        "  [y] yes / [enter]    [n] no / [esc]",
                        Style::default().fg(theme.muted),
                    )));
                }
                ConfirmKind::TypeName => {
                    lines.push(Line::from(vec![
                        Span::styled("  type ", Style::default().fg(theme.muted)),
                        Span::styled(
                            modal.target_env.clone(),
                            Style::default()
                                .fg(theme.health_yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" to confirm:", Style::default().fg(theme.muted)),
                    ]));
                    lines.push(Line::from(""));
                    let matches = modal.typed == modal.target_env;
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            modal.typed.clone(),
                            Style::default()
                                .fg(if matches {
                                    theme.health_green
                                } else {
                                    theme.text
                                })
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            caret_glyph(&theme),
                            Style::default()
                                .fg(theme.health_yellow)
                                .add_modifier(Modifier::SLOW_BLINK),
                        ),
                    ]));
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        if matches {
                            "  [enter] terminate    [esc] cancel"
                        } else {
                            "  [esc] cancel"
                        },
                        Style::default().fg(theme.muted),
                    )));
                }
            }
            f.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(block),
                popup,
            );
        }
        ActionFlow::Running { action, env, since } => {
            let popup = centered_rect(50, 25, area);
            f.render_widget(Clear, popup);
            let elapsed = since.elapsed().as_secs();
            let lines = vec![
                Line::from(""),
                Line::from(Span::styled(
                    format!("  {} on {env}…", action.label()),
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    format!("  elapsed {elapsed}s"),
                    Style::default().fg(theme.muted),
                )),
            ];
            let block = rounded_block(&theme, true)
                .border_style(Style::default().fg(theme.health_yellow))
                .title(Span::styled(
                    " running ",
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                ));
            f.render_widget(Paragraph::new(lines).block(block), popup);
        }
    }
}

fn draw_detail(f: &mut Frame, area: Rect, app: &mut App) {
    let Some(detail) = app.detail.as_ref() else {
        return;
    };
    let env = detail.env_snapshot.clone();
    let env = &env;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // env header
            Constraint::Length(3), // tab strip
            Constraint::Min(3),    // body
            Constraint::Length(2), // footer (2-row, like main view)
        ])
        .split(area);

    // Env header. Status and health render as coloured pills so they pop
    // out of the run of plain Name / Application text — same convention as
    // the env table's STATUS column. Health gets its dot glyph too so the
    // colour blind don't have to lean on hue alone.
    let theme = &app.theme;
    let mut h1 = kv("Name", &env.name, theme);
    h1.push(sep(theme));
    h1.extend(kv("Application", &env.application, theme));
    h1.push(sep(theme));
    h1.push(Span::styled("Status: ", Style::default().fg(theme.muted)));
    h1.push(status_pill(&env.status, theme));
    h1.push(sep(theme));
    h1.push(Span::styled("Health: ", Style::default().fg(theme.muted)));
    h1.push(health_dot(&env.health, theme));
    h1.push(Span::raw(" "));
    h1.push(Span::styled(
        env.health.clone(),
        health_style(&env.health, &app.theme),
    ));
    if let Some(reco) = health_recommendation(env, app) {
        h1.push(Span::raw("  "));
        h1.push(Span::styled(
            reco,
            Style::default()
                .fg(app.theme.health_yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }

    let cname_text = redact(&env.cname, app.redact);
    let mut h2 = kv("Platform", &env.platform, theme);
    h2.push(sep(theme));
    h2.extend(kv("Version", &env.version_label, theme));
    h2.push(sep(theme));
    h2.extend(kv("CNAME", &cname_text, theme));
    let header_title = format!("environment: {}", env.name);
    let header = Paragraph::new(vec![Line::from(h1), Line::from(h2), Line::raw("")]).block(
        titled_block(&app.theme, &header_title, true, app.theme.title),
    );
    f.render_widget(header, chunks[0]);

    // Tab strip
    let tabs_block = rounded_block(&app.theme, false);
    let tab_line = render_tabs(&detail.tabs, detail.tab_idx, &app.theme);
    f.render_widget(Paragraph::new(tab_line).block(tabs_block), chunks[1]);

    // Body
    let body_area = chunks[2];
    let active_tab = detail.tab();
    match active_tab {
        DetailTab::Events => draw_detail_events(f, body_area, detail, &app.theme),
        DetailTab::Instances => draw_detail_instances(f, body_area, detail, &app.theme),
        DetailTab::Metrics => draw_detail_metrics(f, body_area, detail, &app.theme),
        DetailTab::Queue => draw_detail_queue(f, body_area, detail, app.redact, &app.theme),
        DetailTab::Logs => draw_detail_logs(f, body_area, detail, &app.theme),
        DetailTab::Config => draw_detail_config(
            f,
            body_area,
            env,
            detail,
            app.redact,
            &app.required_tags,
            &app.theme,
        ),
    }
    // Snapshot the fields the footer block needs before we drop the immutable
    // borrow and reach for `app.detail.as_mut()` to write metrics_body_rect.
    let footer_state = DetailFooterState {
        auto_refresh: detail.auto_refresh,
        error: detail.error.clone(),
        loading_events: detail.loading_events,
        loading_instances: detail.loading_instances,
        loading_queues: detail.loading_queues,
        loading_metrics: detail.loading_metrics,
        log_stage: detail.log_tail.stage,
    };

    // Remember the Metrics body rect so handle_mouse can decide whether a
    // Moved event falls inside it. Cleared as soon as the user leaves the
    // tab so stale rects from a previous tab don't pin a hover line.
    if let Some(d) = app.detail.as_mut() {
        d.metrics_body_rect = if active_tab == DetailTab::Metrics {
            Some(body_area)
        } else {
            d.metrics_hover_col = None;
            None
        };
    }

    // Footer
    let auto_badge: Span<'static> = if footer_state.auto_refresh {
        pill("AUTO", Color::Black, app.theme.health_green)
    } else {
        Span::raw("")
    };
    let footer = Paragraph::new(vec![
        Line::from(vec![
            if let Some(err) = &footer_state.error {
                Span::styled(format!(" {err}"), Style::default().fg(app.theme.health_red))
            } else if footer_state.loading_events
                || footer_state.loading_instances
                || footer_state.loading_queues
                || footer_state.loading_metrics
                || matches!(
                    footer_state.log_stage,
                    crate::app::LogTailStage::Requesting
                        | crate::app::LogTailStage::Polling
                        | crate::app::LogTailStage::Fetching
                )
            {
                Span::styled(" loading…", Style::default().fg(app.theme.health_yellow))
            } else {
                Span::raw("")
            },
            Span::raw("   "),
            auto_badge,
        ]),
        Line::from(Span::styled(
            detail_tab_keystrip(active_tab),
            Style::default().fg(app.theme.muted),
        )),
    ]);
    f.render_widget(footer, chunks[3]);
}

/// Per-tab key strip for the Detail footer. Each tab advertises the keys
/// most relevant to that view; common cycling keys (tab / shift-tab / ^R)
/// stay across all of them.
fn detail_tab_keystrip(tab: DetailTab) -> &'static str {
    match tab {
        DetailTab::Instances => {
            " INSTANCES  j/k cursor  s ssm shell  i info  y yank id  x terminate  tab→ Metrics  a actions  ^R refresh  ? help  esc back"
        }
        DetailTab::Events => {
            " EVENTS  j/k scroll  / filter  n/N next/prev  tab→ Instances  a actions  ^R refresh  ? help  esc back"
        }
        DetailTab::Metrics => {
            " METRICS  [ / ]  range  hover values  tab→ Queue  a actions  ^R refresh  R auto-refresh  ? help  esc back"
        }
        DetailTab::Queue => {
            " QUEUE  j/k pick Main/DLQ  enter view  d DLQ  ^R refresh  ? help  esc back"
        }
        DetailTab::Logs => {
            " LOGS  ^R snapshot  s live-stream  / filter  ? help  esc back"
        }
        DetailTab::Config => {
            " CONFIG  tab/shift-tab cycle  a actions  ^R refresh  ? help  esc back"
        }
    }
}

struct DetailFooterState {
    auto_refresh: bool,
    error: Option<String>,
    loading_events: bool,
    loading_instances: bool,
    loading_queues: bool,
    loading_metrics: bool,
    log_stage: crate::app::LogTailStage,
}

fn render_tabs(tabs: &[DetailTab], active: usize, theme: &Theme) -> Line<'static> {
    // In Powerline mode each tab is a coloured segment with a U+E0B0
    // triangle flowing into the next tab's bg, so the strip reads as one
    // continuous ribbon. The active tab uses border_active (bright); the
    // inactive tabs use a low-contrast muted bg so the ribbon is visible
    // but doesn't compete with the active tab.
    if theme.icons == IconStyle::Powerline {
        let active_bg = theme.border_active;
        let inactive_bg = theme.row_alt_bg;
        let mut spans: Vec<Span> = Vec::with_capacity(tabs.len() * 2 + 2);
        // Lead-in arrow flowing from default bg into the first tab. Use
        // U+E0B2 (LEFT-pointing) so the tab colour's base sits adjacent to
        // the tab, not adjacent to the empty space before it — otherwise
        // the leading wedge reads as much smaller than the trailing E0B0s
        // along the ribbon. See pill_chain for the same rationale.
        let first_bg = if active == 0 { active_bg } else { inactive_bg };
        spans.push(Span::styled("\u{e0b2}", Style::default().fg(first_bg)));
        for (i, t) in tabs.iter().enumerate() {
            let is_active = i == active;
            let bg = if is_active { active_bg } else { inactive_bg };
            let fg = if is_active { Color::Black } else { theme.muted };
            let label = format!(" {} {} ", tab_icon(*t, theme.icons), t.title());
            let mut style = Style::default().fg(fg).bg(bg);
            if is_active {
                style = style.add_modifier(Modifier::BOLD);
            }
            spans.push(Span::styled(label, style));
            // Bridge: fg = this tab's bg, bg = next tab's bg (or default
            // for the last tab).
            let bridge_style =
                if let Some(next_is_active) = tabs.get(i + 1).map(|_| i + 1 == active) {
                    let next_bg = if next_is_active {
                        active_bg
                    } else {
                        inactive_bg
                    };
                    Style::default().fg(bg).bg(next_bg)
                } else {
                    Style::default().fg(bg)
                };
            spans.push(Span::styled("\u{e0b0}", bridge_style));
        }
        return Line::from(spans);
    }
    // Non-Powerline: same as before, color-only differentiation.
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, t) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("   ", Style::default()));
        }
        let label = format!(" {} {} ", tab_icon(*t, theme.icons), t.title());
        let style = if i == active {
            // Underline + bold + bg highlight — three signals so the active
            // tab is visible even in low-contrast / colorblind terminals.
            Style::default()
                .fg(Color::Black)
                .bg(theme.border_active)
                .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::default().fg(theme.muted)
        };
        spans.push(Span::styled(label, style));
    }
    Line::from(spans)
}

fn draw_detail_events(f: &mut Frame, area: Rect, detail: &crate::app::DetailState, theme: &Theme) {
    let matches = if let Some(re) = detail.search_pattern.as_ref() {
        detail
            .events
            .iter()
            .filter(|e| re.is_match(&e.message))
            .count()
    } else {
        0
    };
    let title = if detail.search_pattern.is_some() {
        format!(" Events [{}] · matches: {matches} ", detail.events.len())
    } else {
        format!(" Events [{}] ", detail.events.len())
    };
    let outer = rounded_block(theme, true)
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    // Reserve a line at the top for the search prompt when active or applied.
    let show_search_bar =
        detail.search_active || detail.search_pattern.is_some() || detail.search_error.is_some();
    let (search_area, body_area) = if show_search_bar {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(inner);
        (Some(rows[0]), rows[1])
    } else {
        (None, inner)
    };

    if let Some(sa) = search_area {
        let mut spans = vec![
            Span::styled(
                "/",
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(detail.search_input.clone(), Style::default().fg(theme.text)),
        ];
        if detail.search_active {
            spans.push(Span::styled(
                caret_glyph(theme),
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
            ));
            spans.push(Span::styled(
                "  [enter] apply  [esc] cancel",
                Style::default().fg(theme.muted),
            ));
        } else if let Some(err) = &detail.search_error {
            spans.push(Span::styled(
                format!("  {err}"),
                Style::default().fg(theme.health_red),
            ));
        } else if detail.search_pattern.is_some() {
            spans.push(Span::styled(
                "  n / N next/prev   / re-edit   esc clear",
                Style::default().fg(theme.muted),
            ));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), sa);
    }

    if detail.events.is_empty() && !detail.loading_events {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                " ◌  no events for this environment",
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "    ^R to re-fetch, R to toggle auto-refresh",
                Style::default().fg(theme.muted),
            )),
        ];
        let p = Paragraph::new(lines);
        f.render_widget(p, body_area);
        return;
    }

    let now = chrono::Utc::now();
    let re = detail.search_pattern.as_ref();
    let lines: Vec<Line> = detail
        .events
        .iter()
        .map(|e| {
            let when = match e.at {
                Some(t) => humanize_age(now.signed_duration_since(t)),
                None => "—".into(),
            };
            let matches = re.is_some_and(|r| r.is_match(&e.message));
            let msg_style = if matches {
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            Line::from(vec![
                Span::styled(format!("{:>4} ", when), Style::default().fg(theme.muted)),
                Span::styled(
                    format!("{:<5} ", e.severity),
                    severity_style(&e.severity, theme),
                ),
                Span::styled(e.message.clone(), msg_style),
            ])
        })
        .collect();
    let p = Paragraph::new(lines).scroll((detail.events_scroll, 0));
    f.render_widget(p, body_area);
}

fn draw_detail_instances(
    f: &mut Frame,
    area: Rect,
    detail: &crate::app::DetailState,
    theme: &Theme,
) {
    let block = rounded_block(theme, true)
        .title(Span::styled(
            format!(" Instances [{}] ", detail.instances.len()),
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1));
    if detail.instances.is_empty() && !detail.loading_instances {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                "  ◌  no instance data",
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "     env may be terminating, or DescribeInstancesHealth not yet warm",
                Style::default().fg(theme.muted),
            )),
        ];
        let p = Paragraph::new(lines).block(block);
        f.render_widget(p, area);
        return;
    }
    let now = chrono::Utc::now();
    let cursor_idx = detail.instances_cursor;
    let confirming = detail.instance_terminate_confirm.is_some();
    let mut lines: Vec<Line> = Vec::new();
    if confirming {
        if let Some(idx) = detail.instance_terminate_confirm {
            if let Some(inst) = detail.instances.get(idx) {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  ⚠ TERMINATE instance {}? ASG will replace it. y / n",
                        inst.id
                    ),
                    Style::default()
                        .fg(theme.health_red)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(""));
            }
        }
    }
    for (idx, i) in detail.instances.iter().enumerate() {
        let age = i
            .launched_at
            .map(|t| humanize_age(now.signed_duration_since(t)))
            .unwrap_or_else(|| "—".into());
        let is_cursor = idx == cursor_idx;
        // Full-row bg highlight on cursor, mirroring the main env table's
        // pattern so the cursor reads the same way across the app.
        let row_bg = if is_cursor {
            Some(theme.row_selected_bg)
        } else {
            None
        };
        let with_bg = |s: Style| match row_bg {
            Some(bg) => s.bg(bg),
            None => s,
        };
        let marker = if is_cursor { "▶ " } else { "  " };
        let marker_style = with_bg(if is_cursor {
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        });
        let id_style = with_bg(if is_cursor {
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD)
        });
        let head = vec![
            Span::styled(marker.to_string(), marker_style),
            Span::styled(format!("{:<19} ", i.id), id_style),
            Span::styled(
                format!("{:<8} ", i.health),
                with_bg(health_style(&i.color, theme)),
            ),
            Span::styled(
                format!("{:<12} ", i.instance_type),
                with_bg(Style::default().fg(theme.muted)),
            ),
            Span::styled(
                format!("{:<14} ", i.availability_zone),
                with_bg(Style::default().fg(theme.muted)),
            ),
            Span::styled(
                format!("up {age}"),
                with_bg(Style::default().fg(theme.muted)),
            ),
        ];
        lines.push(Line::from(head));
        for cause in &i.causes {
            lines.push(Line::from(Span::styled(
                format!("      ↳ {cause}"),
                with_bg(Style::default().fg(theme.health_yellow)),
            )));
        }
    }
    let p = Paragraph::new(lines)
        .block(block)
        .scroll((detail.instances_scroll, 0));
    f.render_widget(p, area);
}

fn draw_detail_metrics(f: &mut Frame, area: Rect, detail: &crate::app::DetailState, theme: &Theme) {
    let title_text = if detail.metrics_hover_col.is_some() {
        format!(
            "Metrics · last {} · CloudWatch · cursor pinned (mouse to roam)",
            humanize_range(detail.metrics_range_secs)
        )
    } else {
        format!(
            "Metrics · last {} · CloudWatch",
            humanize_range(detail.metrics_range_secs)
        )
    };
    let outer = titled_block(theme, &title_text, true, theme.title).padding(Padding::horizontal(1));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    if detail.metrics.is_empty() {
        let msg = if detail.loading_metrics {
            "loading metrics…"
        } else {
            "no metrics returned — env may be too new, or CloudWatch perms missing"
        };
        f.render_widget(
            Paragraph::new(Span::styled(msg, Style::default().fg(theme.muted))),
            inner,
        );
        return;
    }

    let n = detail.metrics.len() as u16;
    if n == 0 || inner.height < n {
        return;
    }
    let per = (inner.height / n).max(3);
    let constraints: Vec<Constraint> = (0..n).map(|_| Constraint::Length(per)).collect();
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints)
        .split(inner);

    for (i, series) in detail.metrics.iter().enumerate() {
        let series_color = match series.id.as_str() {
            "health" => theme.health_green,
            "req4xx" => theme.health_yellow,
            "req5xx" => theme.health_red,
            "p90" => theme.title,
            _ => theme.text,
        };
        let values: Vec<f64> = series.points.iter().map(|(_, v)| *v).collect();
        let max = values.iter().copied().fold(0.0_f64, f64::max);
        let min = values.iter().copied().fold(f64::INFINITY, f64::min);
        let min = if min.is_infinite() { 0.0 } else { min };
        let last = values.last().copied().unwrap_or(0.0);
        let first = values.first().copied().unwrap_or(last);
        let delta = last - first;

        // Anomaly: a series-specific signal that the most recent sample is
        // dramatically above its short-term baseline. For error-rate series
        // (`req5xx`, `req4xx`) we flag `last > 2 × mean(prior points)`; for
        // latency we flag `last > 1.5 × mean(prior)`. Health / other series
        // don't carry an interpretable baseline so we skip them.
        let anomaly = series_anomaly_label(&series.id, &values);
        // Hover lookup: if the mouse column is over the metrics body, translate
        // it to a point index and surface the value at that index.
        let hover_value = detail
            .metrics_hover_col
            .and_then(|col| hover_index(col, inner, values.len()))
            .and_then(|idx| values.get(idx).copied());
        let mut title_spans: Vec<Span<'static>> = vec![Span::styled(
            format!("{:<26} ", series.label),
            Style::default()
                .fg(series_color)
                .add_modifier(Modifier::BOLD),
        )];
        if values.is_empty() {
            // CW returned no datapoints in the window. "now 0 max 0 min 0
            // Δ flat" reads like "the metric IS 0" which is misleading;
            // surface "(no data)" instead so operators know the metric
            // isn't being populated.
            title_spans.push(Span::styled(
                "(no data in window)",
                Style::default().fg(theme.muted),
            ));
        } else {
            title_spans.push(Span::styled(
                format!("now {}  ", format_metric(&series.id, last)),
                Style::default().fg(theme.text),
            ));
            title_spans.push(Span::styled(
                format!("max {}  ", format_metric(&series.id, max)),
                Style::default().fg(theme.muted),
            ));
            title_spans.push(Span::styled(
                format!("min {}  ", format_metric(&series.id, min)),
                Style::default().fg(theme.muted),
            ));
            title_spans.push(delta_span(delta, &series.id, theme));
        }
        if let Some(label) = anomaly {
            title_spans.push(Span::raw("  "));
            title_spans.push(Span::styled(
                label,
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if let Some(hv) = hover_value {
            title_spans.push(Span::raw("  "));
            title_spans.push(Span::styled(
                format!("@cursor {}", format_metric(&series.id, hv)),
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        let title = Line::from(title_spans);
        let row_layout = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Min(1)])
            .split(rows[i]);
        f.render_widget(Paragraph::new(title), row_layout[0]);

        // Real Chart with braille marker.
        let pts: Vec<(f64, f64)> = values
            .iter()
            .enumerate()
            .map(|(idx, v)| (idx as f64, *v))
            .collect();
        if pts.is_empty() {
            continue;
        }
        let max_x = (pts.len() as f64 - 1.0).max(1.0);
        let max_y = (max * 1.1).max(1.0);
        let dataset = Dataset::default()
            .marker(Marker::Braille)
            .graph_type(GraphType::Line)
            .style(Style::default().fg(series_color))
            .data(&pts);
        let chart = Chart::new(vec![dataset])
            .style(Style::default())
            .x_axis(Axis::default().bounds([0.0, max_x]))
            .y_axis(Axis::default().bounds([0.0, max_y]));
        f.render_widget(chart, row_layout[1]);
    }
}

/// Map a mouse column to the corresponding metric point index. `col` is the
/// raw terminal column from the crossterm event; `area` is the inner Rect of
/// the metrics body; `n` is the number of points in the series. Returns
/// `None` when the column is outside the body. The mapping is linear with
/// integer rounding so the cursor "snaps" to the nearest sample.
pub fn hover_index(col: u16, area: Rect, n: usize) -> Option<usize> {
    if n == 0 || area.width < 2 {
        return None;
    }
    if col < area.x || col >= area.x.saturating_add(area.width) {
        return None;
    }
    let rel = (col - area.x) as f64;
    let width = (area.width - 1) as f64;
    let scaled = (rel / width) * (n as f64 - 1.0);
    Some(scaled.round() as usize)
}

/// Return an anomaly badge for a metric series, or `None` if the latest sample
/// looks consistent with the baseline. The threshold is series-dependent —
/// error rates spike more aggressively than latency does, so we use a higher
/// multiplier for `req4xx` / `req5xx` than for `p90`. Series IDs we don't
/// recognise (e.g. `health`) return `None`.
pub fn series_anomaly_label(id: &str, values: &[f64]) -> Option<String> {
    if values.len() < 4 {
        return None;
    }
    let last = *values.last()?;
    let prior = &values[..values.len() - 1];
    let sum: f64 = prior.iter().copied().filter(|v| v.is_finite()).sum();
    let count = prior.iter().filter(|v| v.is_finite()).count() as f64;
    if count == 0.0 {
        return None;
    }
    let mean = sum / count;
    if mean <= 0.0 || !last.is_finite() {
        return None;
    }
    let (multiplier, glyph) = match id {
        "req5xx" => (2.0_f64, "▲ anomaly: 5xx > 2× baseline"),
        "req4xx" => (2.0_f64, "▲ anomaly: 4xx > 2× baseline"),
        "p90" => (1.5_f64, "▲ anomaly: latency > 1.5× baseline"),
        _ => return None,
    };
    if last > mean * multiplier {
        Some(glyph.to_string())
    } else {
        None
    }
}

fn delta_span(delta: f64, id: &str, theme: &Theme) -> Span<'static> {
    if delta.abs() < f64::EPSILON {
        return Span::styled("Δ flat", Style::default().fg(theme.muted));
    }
    let arrow = if delta >= 0.0 { "▲" } else { "▼" };
    let color = match (id, delta >= 0.0) {
        // For health 0=OK and higher=worse, so up is bad.
        ("health", true) => theme.health_red,
        ("health", false) => theme.health_green,
        // For errors / latency, higher = bad.
        ("req4xx" | "req5xx" | "p90", true) => theme.health_red,
        ("req4xx" | "req5xx" | "p90", false) => theme.health_green,
        _ => theme.text,
    };
    Span::styled(
        format!("Δ {arrow} {}", format_metric(id, delta.abs())),
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn format_metric(id: &str, v: f64) -> String {
    match id {
        "health" => format!("{:.0}", v),
        "p90" => {
            if v >= 1.0 {
                format!("{:.2}s", v)
            } else {
                format!("{:.0}ms", v * 1000.0)
            }
        }
        _ => format!("{:.0}", v),
    }
}

fn humanize_range(secs: i64) -> String {
    if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn draw_detail_queue(
    f: &mut Frame,
    area: Rect,
    detail: &crate::app::DetailState,
    redact_on: bool,
    theme: &Theme,
) {
    let block = rounded_block(theme, true)
        .title(Span::styled(
            " Queue ",
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(2));

    let q = &detail.queues;
    // Auto-scale bars: max across both queues' stats so visual length is comparable.
    let scale = q
        .main_stats
        .as_ref()
        .into_iter()
        .chain(q.dlq_stats.as_ref())
        .flat_map(|s| [s.visible, s.in_flight, s.delayed])
        .max()
        .unwrap_or(1)
        .max(1);

    let row = |label: &'static str, value: String, hi: Option<Color>| -> Line {
        let v_style = match hi {
            Some(c) => Style::default().fg(c).add_modifier(Modifier::BOLD),
            None => Style::default().fg(theme.text),
        };
        Line::from(vec![
            Span::styled(format!("{label:<22}"), Style::default().fg(theme.muted)),
            Span::styled(value, v_style),
        ])
    };

    let stats_row = |label: &'static str, s: Option<&crate::aws::QueueStats>| -> Vec<Line> {
        match s {
            Some(s) => {
                let bar = |n: i64, color: Color| -> Span<'static> {
                    Span::styled(micro_bar(n, scale, 12), Style::default().fg(color))
                };
                vec![
                    Line::from(vec![
                        Span::styled(format!("{label:<22}"), Style::default().fg(theme.muted)),
                        Span::styled(
                            format!("visible:  {:>5}  ", s.visible),
                            Style::default()
                                .fg(if s.visible > 0 {
                                    theme.health_yellow
                                } else {
                                    theme.text
                                })
                                .add_modifier(Modifier::BOLD),
                        ),
                        bar(s.visible, theme.health_yellow),
                    ]),
                    Line::from(vec![
                        Span::styled(format!("{:<22}", ""), Style::default().fg(theme.muted)),
                        Span::styled(
                            format!("in-flight:{:>5}  ", s.in_flight),
                            Style::default().fg(theme.text),
                        ),
                        bar(s.in_flight, theme.app_palette[0]),
                    ]),
                    Line::from(vec![
                        Span::styled(format!("{:<22}", ""), Style::default().fg(theme.muted)),
                        Span::styled(
                            format!("delayed:  {:>5}  ", s.delayed),
                            Style::default().fg(theme.muted),
                        ),
                        bar(s.delayed, theme.app_palette[1]),
                    ]),
                ]
            }
            None => vec![row(label, "—".into(), None)],
        }
    };

    let main_selected = detail.queue_cursor == 0;
    let dlq_selected = detail.queue_cursor == 1;
    let queue_row = |selected: bool, label: &str, value: String| -> Line<'static> {
        let (marker, marker_style) = if selected {
            (
                "▶ ",
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            ("  ", Style::default().fg(theme.muted))
        };
        let label_style = if selected {
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        let value_style = if selected {
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        Line::from(vec![
            Span::styled(marker.to_string(), marker_style),
            Span::styled(format!("{label:<20}"), label_style),
            Span::styled(value, value_style),
        ])
    };

    let mut lines = Vec::new();
    lines.push(queue_row(
        main_selected,
        "Main queue URL",
        redact(q.main_url.as_deref().unwrap_or("—"), redact_on),
    ));
    lines.extend(stats_row("    stats", q.main_stats.as_ref()));
    lines.push(Line::from(""));
    lines.push(queue_row(
        dlq_selected,
        "DLQ URL",
        redact(q.dlq_url.as_deref().unwrap_or("—"), redact_on),
    ));
    lines.extend(stats_row("    stats", q.dlq_stats.as_ref()));
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  j/k pick queue · enter view messages · d quick-open DLQ",
        Style::default().fg(theme.muted),
    )));
    if detail.loading_queues {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  loading queue stats…",
            Style::default().fg(theme.health_yellow),
        )));
    }
    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_detail_logs(f: &mut Frame, area: Rect, detail: &crate::app::DetailState, theme: &Theme) {
    use crate::app::LogTailStage;
    let tail = &detail.log_tail;
    let lines_total: usize = tail
        .by_instance
        .iter()
        .map(|(_, t)| t.lines().count())
        .sum();
    let matches = if let Some(re) = tail.search_pattern.as_ref() {
        tail.by_instance
            .iter()
            .map(|(_, t)| t.lines().filter(|l| re.is_match(l)).count())
            .sum::<usize>()
    } else {
        0
    };
    let title = if tail.search_pattern.is_some() {
        format!(
            " Logs · {} instance(s) · {lines_total} lines · matches: {matches} ",
            tail.by_instance.len()
        )
    } else {
        format!(
            " Logs · {} instance(s) · {lines_total} lines ",
            tail.by_instance.len()
        )
    };
    let outer = rounded_block(theme, true)
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1));
    let inner = outer.inner(area);
    f.render_widget(outer, area);

    // Stage line + search bar at the top.
    let stage_line: Line<'static> = match tail.stage {
        LogTailStage::Idle => {
            // Tailored hint based on whether we've discovered CW Logs groups
            // for this env. The discover call fires on Detail open so by the
            // time the user navigates to the Logs tab the state is usually
            // settled.
            let hint = match detail.cw_log_groups.as_ref() {
                Some(groups) if !groups.is_empty() => {
                    " press ^R for one-shot snapshot · s to live-stream CW Logs"
                }
                Some(_) => {
                    " press ^R for one-shot snapshot · CW Logs not configured (`:logs-stream on` to enable)"
                }
                None => " press ^R for one-shot snapshot · s to live-stream CW Logs (checking…)",
            };
            Line::from(Span::styled(hint, Style::default().fg(theme.muted)))
        }
        LogTailStage::Requesting => Line::from(Span::styled(
            " requesting tail from EB…",
            Style::default().fg(theme.health_yellow),
        )),
        LogTailStage::Polling => Line::from(Span::styled(
            format!(
                " waiting for instance samples (attempt {}/12)…",
                tail.poll_attempt.max(1)
            ),
            Style::default().fg(theme.health_yellow),
        )),
        LogTailStage::Fetching => Line::from(Span::styled(
            " fetching log content…",
            Style::default().fg(theme.health_yellow),
        )),
        LogTailStage::Ready => {
            if let Some(err) = &tail.error {
                Line::from(Span::styled(
                    format!(" {err}"),
                    Style::default().fg(theme.health_red),
                ))
            } else if tail.search_active || tail.search_pattern.is_some() {
                let mut spans = vec![
                    Span::styled(
                        "/",
                        Style::default()
                            .fg(theme.health_yellow)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(tail.search_input.clone(), Style::default().fg(theme.text)),
                ];
                if tail.search_active {
                    spans.push(Span::styled(
                        caret_glyph(theme),
                        Style::default()
                            .fg(theme.health_yellow)
                            .add_modifier(Modifier::SLOW_BLINK),
                    ));
                    spans.push(Span::styled(
                        "  [enter] apply  [esc] cancel",
                        Style::default().fg(theme.muted),
                    ));
                } else if let Some(err) = &tail.search_error {
                    spans.push(Span::styled(
                        format!("  {err}"),
                        Style::default().fg(theme.health_red),
                    ));
                }
                Line::from(spans)
            } else {
                Line::from(Span::styled(
                    " ^R refresh   / search   esc clear",
                    Style::default().fg(theme.muted),
                ))
            }
        }
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Min(1)])
        .split(inner);
    f.render_widget(Paragraph::new(stage_line), rows[0]);

    // Body — concatenate per-instance blocks separated by a banner row.
    let mut body: Vec<Line<'static>> = Vec::new();
    if tail.by_instance.is_empty() && tail.stage != LogTailStage::Ready {
        body.push(Line::from(Span::styled(
            "  (no content yet)",
            Style::default().fg(theme.muted),
        )));
    }
    for (instance_id, text) in &tail.by_instance {
        body.push(Line::from(Span::styled(
            format!("── {instance_id} "),
            Style::default()
                .fg(theme.title)
                .add_modifier(Modifier::BOLD),
        )));
        for raw in text.lines() {
            if let Some(re) = tail.search_pattern.as_ref() {
                if !re.is_match(raw) {
                    continue;
                }
            }
            body.push(Line::from(Span::styled(
                raw.to_string(),
                Style::default().fg(theme.text),
            )));
        }
        body.push(Line::from(""));
    }
    let scroll = (tail.scroll, 0);
    f.render_widget(
        Paragraph::new(body)
            .wrap(Wrap { trim: false })
            .scroll(scroll),
        rows[1],
    );
}

fn draw_detail_config(
    f: &mut Frame,
    area: Rect,
    env: &crate::aws::Environment,
    detail: &crate::app::DetailState,
    redact_on: bool,
    required_tags: &[String],
    theme: &Theme,
) {
    let block = titled_block(theme, "Config", true, theme.title).padding(Padding::horizontal(2));

    let updated = env
        .updated
        .map(|u| {
            u.with_timezone(&chrono::Local)
                .format("%Y-%m-%d %H:%M:%S %Z")
                .to_string()
        })
        .unwrap_or_else(|| "—".into());

    let row = |label: &'static str, value: String| -> Line<'static> {
        Line::from(vec![
            Span::styled(format!("{label:<14}"), Style::default().fg(theme.muted)),
            Span::styled(value, Style::default().fg(theme.text)),
        ])
    };

    let mut lines: Vec<Line<'static>> = vec![
        row("Environment", env.name.clone()),
        row("Application", env.application.clone()),
        row("Tier", env.tier.clone()),
        row("Status", env.status.clone()),
        row("Health", env.health.clone()),
        row("Platform", env.platform.clone()),
        row("Version", env.version_label.clone()),
        row("CNAME", redact(&env.cname, redact_on)),
        row("Updated", updated),
    ];

    // Cost annotation
    lines.push(Line::raw(""));
    if detail.loading_instances && detail.instances.is_empty() {
        lines.push(Line::from(Span::styled(
            "Est. cost     loading…",
            Style::default().fg(theme.muted),
        )));
    } else if detail.instances.is_empty() {
        lines.push(Line::from(Span::styled(
            "Est. cost     no running instances",
            Style::default().fg(theme.muted),
        )));
    } else {
        let (hourly, missing) = crate::app::estimate_cost(&detail.instances);
        let monthly = hourly * 730.0; // avg hrs/month
        let mut summary = format!(
            "{} instance{}  ~ ${:.2}/hr  ~ ${:.0}/mo",
            detail.instances.len(),
            if detail.instances.len() == 1 { "" } else { "s" },
            hourly,
            monthly,
        );
        if missing > 0 {
            summary.push_str(&format!(
                "  ({missing} unknown type{})",
                if missing == 1 { "" } else { "s" }
            ));
        }
        lines.push(Line::from(vec![
            Span::styled(
                format!("{:<14}", "Est. cost"),
                Style::default().fg(theme.muted),
            ),
            Span::styled(summary, Style::default().fg(theme.text)),
        ]));
        lines.push(Line::from(Span::styled(
            "              (approximate, us-east-1 on-demand Linux rates)",
            Style::default().fg(theme.muted),
        )));
    }

    // Tags section
    lines.push(Line::raw(""));
    if detail.loading_tags && detail.tags.is_empty() {
        lines.push(Line::from(Span::styled(
            "Tags          loading…",
            Style::default().fg(theme.muted),
        )));
    } else if detail.tags.is_empty() {
        lines.push(Line::from(Span::styled(
            "Tags          (none)",
            Style::default().fg(theme.muted),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("Tags          ({} total)", detail.tags.len()),
            Style::default().fg(theme.muted),
        )));
        // Mini-table — sorted alphabetically by key (case-insensitive) so
        // related tags (e.g. `aws:cloudformation:*`) sit together. The key
        // column auto-sizes to the longest key for the env, clamped at
        // half the body width so a single huge key doesn't squish values.
        let mut sorted: Vec<(&String, &String)> = detail.tags.iter().map(|(k, v)| (k, v)).collect();
        sorted.sort_by_key(|(k, _)| k.to_lowercase());
        let max_key_width: usize = sorted
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(12, 40);
        for (k, v) in &sorted {
            let key_len = k.chars().count();
            let key_text = if key_len <= max_key_width {
                format!("  {k:<width$}", width = max_key_width)
            } else {
                // Long key overflows the column — emit it on its own line so
                // the value still aligns on the next row.
                format!("  {k}\n  {pad:<width$}", pad = "", width = max_key_width)
            };
            lines.push(Line::from(vec![
                Span::styled(key_text, Style::default().fg(theme.app_palette[0])),
                Span::raw("  "),
                Span::styled(v.to_string(), Style::default().fg(theme.text)),
            ]));
        }
    }

    // Tag policy check
    if !required_tags.is_empty() {
        let present: std::collections::HashSet<&str> =
            detail.tags.iter().map(|(k, _)| k.as_str()).collect();
        let missing: Vec<&str> = required_tags
            .iter()
            .filter(|r| !present.contains(r.as_str()))
            .map(|r| r.as_str())
            .collect();
        if !missing.is_empty() {
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("Tag policy    ", Style::default().fg(theme.muted)),
                Span::styled(
                    format!("⚠ missing required tag(s): {}", missing.join(", ")),
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }

    // Env vars section — same layout pattern as tags. Operators read them
    // often (debugging, change verification); shown read-only here, edited
    // via `:env set` / `:env unset`.
    lines.push(Line::raw(""));
    if detail.loading_env_vars && detail.env_vars.is_empty() {
        lines.push(Line::from(Span::styled(
            "Env vars      loading…",
            Style::default().fg(theme.muted),
        )));
    } else if detail.env_vars.is_empty() {
        lines.push(Line::from(Span::styled(
            "Env vars      (none — set with `:env set KEY VAL`)",
            Style::default().fg(theme.muted),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            format!("Env vars      ({} total)", detail.env_vars.len()),
            Style::default().fg(theme.muted),
        )));
        let max_key_width: usize = detail
            .env_vars
            .iter()
            .map(|(k, _)| k.chars().count())
            .max()
            .unwrap_or(0)
            .clamp(12, 40);
        for (k, v) in &detail.env_vars {
            let key_len = k.chars().count();
            let key_text = if key_len <= max_key_width {
                format!("  {k:<width$}", width = max_key_width)
            } else {
                format!("  {k}\n  {pad:<width$}", pad = "", width = max_key_width)
            };
            // Render empty value as `""` so operators can distinguish
            // "explicitly empty" from "not set" (mirrors `:env list`).
            let value = if v.is_empty() { "\"\"" } else { v.as_str() };
            lines.push(Line::from(vec![
                Span::styled(key_text, Style::default().fg(theme.app_palette[1])),
                Span::raw("  "),
                Span::styled(value.to_string(), Style::default().fg(theme.text)),
            ]));
        }
    }

    f.render_widget(Paragraph::new(lines).block(block), area);
}

fn draw_picker(f: &mut Frame, area: Rect, app: &mut App) {
    let theme = app.theme.clone();
    let Some(picker) = app.picker.as_mut() else {
        return;
    };
    let popup = centered_rect(50, 60, area);
    f.render_widget(Clear, popup);

    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(popup);

    let filter_block = titled_block(&theme, picker.title().trim(), true, theme.title_alt);
    let filter_inner = Paragraph::new(Line::from(vec![
        Span::styled(
            " /",
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" "),
        Span::styled(picker.filter.clone(), Style::default().fg(theme.text)),
        Span::styled(
            caret_glyph(&theme),
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
    ]))
    .block(filter_block);
    f.render_widget(filter_inner, layout[0]);

    let filtered = picker.filtered();
    let items: Vec<ListItem> = filtered
        .iter()
        .map(|i| {
            let name = picker.items[*i].clone();
            ListItem::new(Line::from(Span::styled(
                format!(" {name}"),
                Style::default().fg(theme.text),
            )))
        })
        .collect();

    let list_block = rounded_block(&theme, true);
    let list = List::new(items).block(list_block).highlight_style(
        Style::default()
            .bg(theme.row_selected_bg)
            .add_modifier(Modifier::BOLD),
    );

    // List widget uses absolute indexes into its items vec, which is `filtered`.
    // Map the picker's "real" selection to its filtered position for rendering.
    let mut visible_state = ratatui::widgets::ListState::default();
    if let Some(real) = picker.list_state.selected() {
        visible_state.select(filtered.iter().position(|i| *i == real));
    }
    f.render_stateful_widget(list, layout[1], &mut visible_state);

    let hint = Paragraph::new(Span::styled(
        " j/k move  type to filter  enter select  esc cancel",
        Style::default().fg(theme.muted),
    ));
    f.render_widget(hint, layout[2]);
}

fn draw_help_detail(f: &mut Frame, popup: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "Detail view — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line(
            "tab / l",
            "next tab (Events → Instances → Metrics → Queue → Logs → Config)",
            theme,
        ),
        help_line("shift-tab / h", "previous tab", theme),
        help_line(
            "j / k",
            "scroll within active tab (cursor on Instances / Queue tabs)",
            theme,
        ),
        help_line("^R", "re-fetch active tab's data", theme),
        help_line("R", "toggle per-tab auto-refresh", theme),
        help_line("a", "actions menu (rebuild / restart / deploy / …)", theme),
        help_line("b", "open env in AWS console", theme),
        help_line("D", "describe overlay (raw env dump as JSON)", theme),
        help_line("d", "open DLQ for this env (Worker tier only)", theme),
        help_line("*", "pin / unpin", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Events tab",
            Style::default().fg(app.theme.title),
        )),
        help_line("/", "regex filter event messages", theme),
        help_line("n / N", "jump next / previous match", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Metrics tab",
            Style::default().fg(app.theme.title),
        )),
        help_line(
            "[ / ]",
            "decrease / increase metric range (15m → 24h)",
            theme,
        ),
        help_line(
            "mouse hover",
            "show metric value at cursor x-position",
            theme,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Instances tab",
            Style::default().fg(app.theme.title),
        )),
        help_line(
            "enter / i",
            "open instance info overlay (id, type, AZ, health, causes)",
            theme,
        ),
        help_line("b", "open instance in EC2 console (browser)", theme),
        help_line("s", "embedded SSM shell into selected instance", theme),
        help_line("y", "yank instance ID", theme),
        help_line(
            "x",
            "terminate selected instance (Y/N; ASG replaces)",
            theme,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Queue tab",
            Style::default().fg(app.theme.title),
        )),
        help_line("j / k", "pick Main / DLQ", theme),
        help_line("enter", "open queue viewer", theme),
        help_line("d", "quick-open DLQ", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Logs tab",
            Style::default().fg(app.theme.title),
        )),
        help_line(
            "^R",
            "request tail logs (10-20s wait for instance samples)",
            theme,
        ),
        help_line(
            "s",
            "open CW Logs streaming overlay (requires `:logs-stream on`)",
            theme,
        ),
        help_line("/", "regex filter visible lines", theme),
        Line::from(""),
        Line::from(Span::styled(
            "esc / q  to close help; from Normal mode `?` shows the full keymap",
            Style::default().fg(theme.muted),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "help — Detail", true, app.theme.title_alt)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_help_dlq(f: &mut Frame, popup: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "Queue viewer — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k", "move cursor", theme),
        help_line("enter", "view full message body", theme),
        help_line("r", "resend selected (DLQ → main) — DLQ view only", theme),
        help_line("x", "delete selected message (Y/N confirm)", theme),
        help_line(
            "p",
            "purge queue (strict typed-name confirm) — DLQ view only",
            theme,
        ),
        help_line("m", "toggle Main ↔ DLQ", theme),
        help_line(
            "^R",
            "refetch messages (deeper peek with long-polling)",
            theme,
        ),
        help_line("esc / q", "close viewer", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Resend and purge are disabled in Main view — too dangerous on a live queue.",
            Style::default().fg(theme.muted),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "help — Queue", true, app.theme.title_alt)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_help_action(f: &mut Frame, popup: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "Action menu — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k", "move cursor between actions", theme),
        help_line(
            "enter",
            "select; opens confirm modal (or picker for Swap)",
            theme,
        ),
        help_line("esc", "close menu", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Confirm modal",
            Style::default().fg(app.theme.title),
        )),
        help_line("y / enter", "confirm and dispatch", theme),
        help_line("n / esc", "cancel", theme),
        help_line(
            "(typing)",
            "TypeName confirm (Terminate) — must match env name exactly",
            theme,
        ),
        Line::from(""),
        Line::from(Span::styled(
            "Parameterised actions (Deploy / Upgrade / Clone / Scale) close the menu",
            Style::default().fg(theme.muted),
        )),
        Line::from(Span::styled(
            "and prefill the command bar; type the arg and Enter to run.",
            Style::default().fg(theme.muted),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "help — Action", true, app.theme.title_alt)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_help_saved_configs(f: &mut Frame, popup: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "Saved configurations — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / k / arrows", "move cursor up / down", theme),
        help_line("g / G", "jump to top / bottom", theme),
        help_line(
            "enter / a",
            "apply selected template to the currently-selected env",
            theme,
        ),
        help_line(
            "i",
            "inspect template — open its option settings as a sorted text dump",
            theme,
        ),
        help_line(
            "c",
            "close overlay + prefill `:config-save ` to save current env as a new template",
            theme,
        ),
        help_line(
            "x",
            "delete selected template (Y/N confirm — config templates are recreatable)",
            theme,
        ),
        help_line("?", "this help", theme),
        help_line("esc / q", "close overlay", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Apply target = whichever env the table cursor is on.",
            Style::default().fg(theme.muted),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(
            &app.theme,
            "help — Saved Configs",
            true,
            app.theme.title_alt,
        )
        .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_help_shell(f: &mut Frame, popup: Rect, app: &App) {
    let theme = &app.theme;
    let lines = vec![
        Line::from(Span::styled(
            "Embedded shell — keybindings",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "Almost every key is forwarded to the subprocess. Exceptions:",
            Style::default().fg(theme.muted),
        )),
        Line::from(""),
        help_line(
            "F12",
            "detach back to ebman (subprocess keeps running)",
            theme,
        ),
        help_line("^D / exit", "close the session", theme),
        Line::from(""),
        Line::from(Span::styled(
            "Open from Instances tab → s on a selected instance.",
            Style::default().fg(theme.muted),
        )),
    ];
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "help — Shell", true, app.theme.title_alt)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn help_line<'a>(key: &'a str, desc: &'a str, theme: &Theme) -> Line<'a> {
    // Pad short keys to a 16-char column so descriptions line up, but if the
    // key itself is wider than the column always emit at least 2 spaces of
    // separator so it can't glue against the description.
    let key_col = 16usize;
    let formatted = if key.chars().count() < key_col {
        format!(" {key:<width$}", width = key_col)
    } else {
        format!(" {key}  ")
    };
    Line::from(vec![
        Span::styled(
            formatted,
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(theme.text)),
    ])
}

/// Returns a short human recommendation when an env has been Red/Yellow for a
/// non-trivial number of consecutive samples. Counts trailing samples in the
/// env's history. Cheap; only invoked from the Detail header.
fn health_recommendation(env: &crate::aws::Environment, app: &App) -> Option<String> {
    let history = app.history.get(&env.name)?;
    if history.is_empty() {
        return None;
    }
    let last = history.back()?.to_lowercase();
    let is_bad = matches!(last.as_str(), "red" | "severe" | "yellow" | "warning");
    if !is_bad {
        return None;
    }
    let target = last.clone();
    let consecutive = history
        .iter()
        .rev()
        .take_while(|s| s.to_lowercase() == target)
        .count();
    // Need at least 4 consecutive (≈ 1 min at 15s tick) to be worth a callout.
    if consecutive < 4 {
        return None;
    }
    let secs = consecutive as u64 * app.refresh_interval.as_secs();
    let approx = humanize_duration(secs);
    let label = if target.eq_ignore_ascii_case("red") || target.eq_ignore_ascii_case("severe") {
        "Red"
    } else {
        "Yellow"
    };
    Some(format!("≥ {approx} in {label}"))
}

fn humanize_duration(secs: u64) -> String {
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

fn breadcrumb_line(app: &App) -> Line<'static> {
    let theme = &app.theme;
    // Powerline-style breadcrumb uses U+E0B1 (the same thin-separator glyph
    // sep() emits) so the divider matches the header chain. Falls back to
    // ASCII slash in unicode/ascii modes — the slash reads as a path
    // separator without needing a Nerd Font.
    let crumb_sep_glyph = if theme.icons == IconStyle::Powerline {
        " \u{e0b1} "
    } else {
        " / "
    };
    let region = app.context.region.clone();
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        region,
        Style::default()
            .fg(theme.title)
            .add_modifier(Modifier::BOLD),
    )];
    let env = match (app.mode, app.detail.as_ref()) {
        (Mode::Detail, Some(d)) => Some((d.env_snapshot.application.clone(), d.env_name.clone())),
        _ => app
            .selected_env()
            .map(|e| (e.application.clone(), e.name.clone())),
    };
    if let Some((app_name, env_name)) = env {
        spans.push(Span::styled(
            crumb_sep_glyph,
            Style::default().fg(theme.muted),
        ));
        spans.push(Span::styled(
            app_name,
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(
            crumb_sep_glyph,
            Style::default().fg(theme.muted),
        ));
        spans.push(Span::styled(
            env_name,
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn kv<'a>(key: &'a str, value: &'a str, theme: &Theme) -> Vec<Span<'a>> {
    vec![
        Span::styled(format!("{key}: "), Style::default().fg(theme.muted)),
        Span::styled(
            value.to_string(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
    ]
}

fn sep(theme: &Theme) -> Span<'static> {
    // U+E0B1 — thin powerline separator — reads as a real divider in
    // Powerline-patched fonts and falls back to a tofu box otherwise.
    let glyph = if theme.icons == IconStyle::Powerline {
        "  \u{e0b1}  "
    } else {
        "  •  "
    };
    Span::styled(glyph, Style::default().fg(theme.muted))
}

/// Cursor / row-selection marker prepended to highlighted rows in lists +
/// tables. Powerline-mode users get the filled U+E0B0 right-triangle so
/// the marker matches the rest of the ribbon aesthetic; everyone else gets
/// the half-block ▌ that doesn't need a patched font.
fn cursor_marker(theme: &Theme) -> &'static str {
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
fn caret_glyph(theme: &Theme) -> &'static str {
    if theme.icons == IconStyle::Ascii {
        "_"
    } else {
        "\u{258e}"
    }
}

fn sparkline_for(
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
        let color = match h.to_lowercase().as_str() {
            "green" | "ok" => theme.health_green,
            "yellow" | "warning" => theme.health_yellow,
            "red" | "severe" | "degraded" => theme.health_red,
            "grey" | "gray" | "info" | "no data" | "pending" => theme.health_grey,
            _ => theme.text,
        };
        // Fade older samples: leftmost 1/3 are dim.
        let mut style = Style::default().fg(color);
        if visible_len > 3 && i < visible_len / 3 {
            style = style.add_modifier(Modifier::DIM);
        }
        // Pulse the rightmost cell when the caller flagged a fresh health
        // transition — swap the block to a full-height `█` and bold it so
        // the change visually pops on the refresh that landed it.
        let glyph = if pulse_last && i + 1 == visible_len {
            style = style.add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK);
            "█"
        } else {
            "▇"
        };
        spans.push(Span::styled(glyph, style));
    }
    Line::from(spans)
}

fn health_style(health: &str, theme: &Theme) -> Style {
    let color = match health.to_lowercase().as_str() {
        "green" | "ok" => theme.health_green,
        "yellow" | "warning" => theme.health_yellow,
        "red" | "severe" | "degraded" => theme.health_red,
        "grey" | "gray" | "info" | "no data" | "pending" => theme.health_grey,
        _ => theme.text,
    };
    Style::default().fg(color).add_modifier(Modifier::BOLD)
}

fn redact(value: &str, on: bool) -> String {
    if !on || value.is_empty() || value == "—" {
        return value.to_string();
    }
    // Preserve length using full-block shaded characters.
    "▓".repeat(value.chars().count())
}

fn short_caller(arn: &str) -> String {
    // arn:aws:iam::123456789012:user/alice          → user/alice
    // arn:aws:sts::123456789012:assumed-role/Foo/x  → assumed-role/Foo/x
    arn.splitn(6, ':').nth(5).unwrap_or(arn).to_string()
}

/// Pure: split a confirm-modal summary into spans so the env name (when
/// it appears inside single quotes — the convention all our summaries
/// follow) renders distinctly from the rest of the sentence. Useful for
/// the destructive paths where the env name is the part the operator
/// needs to verify at a glance. Falls back to a single styled span when
/// the env name isn't found in the summary (e.g. a placeholder path).
fn highlight_env_in_summary(
    summary: &str,
    env_name: &str,
    body_style: Style,
    name_style: Style,
) -> Line<'static> {
    let needle = format!("'{env_name}'");
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled("  ".to_string(), body_style));
    if let Some(idx) = summary.find(&needle) {
        let before = &summary[..idx];
        let after = &summary[idx + needle.len()..];
        if !before.is_empty() {
            spans.push(Span::styled(before.to_string(), body_style));
        }
        spans.push(Span::styled(format!(" {env_name} "), name_style));
        if !after.is_empty() {
            spans.push(Span::styled(after.to_string(), body_style));
        }
    } else {
        spans.push(Span::styled(summary.to_string(), body_style));
    }
    Line::from(spans)
}

/// Pick a context-aware hint to surface in the footer when nothing else
/// is competing for the slot. Reads only from `App` fields the hint
/// cares about, returns the first matching nudge (priority order:
/// alerts > pending > sso > filter-heavy > newly_added). Returns
/// `None` when nothing's worth saying — keeps the footer quiet.
fn context_hint(app: &App) -> Option<String> {
    // Multiple Red envs — point at the alarms / org-health overlays.
    if app.alerts >= 2 {
        return Some(format!(
            "{} envs alerting — try `:alarms` or `:org-health`",
            app.alerts
        ));
    }
    // In-flight pending actions — operators sometimes forget what they
    // dispatched seconds ago. Surface that they can review them.
    let in_flight = app
        .pending_actions
        .iter()
        .filter(|p| p.completed.is_none())
        .count();
    if in_flight >= 3 {
        return Some(format!(
            "{in_flight} actions in flight — `:pending` to review"
        ));
    }
    // SSO about to expire — re-login *before* the next refresh fails.
    if let Some(exp) = app.sso_expiry {
        let remaining = exp.signed_duration_since(chrono::Utc::now());
        if remaining > chrono::Duration::zero() && remaining < chrono::Duration::minutes(15) {
            return Some(format!(
                "SSO expires in {}m — `aws sso login --profile {}`",
                remaining.num_minutes().max(0),
                app.context.profile.as_deref().unwrap_or("default")
            ));
        }
    }
    // New envs landed on this refresh — point at them so the operator
    // sees the `+` marker isn't a glitch.
    if !app.newly_added.is_empty() {
        let n = app.newly_added.len();
        let env_word = if n == 1 { "env" } else { "envs" };
        return Some(format!("{n} new {env_word} this refresh (marked +)"));
    }
    None
}

/// Pure: render a compact summary of in-flight pending-action labels for
/// the header `⏳` pill. Shape: `"rebuild ×2, deploy"`. Identical labels
/// collapse into a `×N` suffix; output truncated to ~25 chars with `…`
/// so the pill stays narrow. Empty input returns an empty string (caller
/// should suppress the pill).
fn summarize_in_flight(labels: &[&str]) -> String {
    use std::collections::BTreeMap;
    if labels.is_empty() {
        return String::new();
    }
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for l in labels {
        // Normalise to a short stem so "Rebuild environment" /
        // "Restart app server" / etc. read as one word in the pill.
        let stem = l
            .split_whitespace()
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        let entry = counts.entry(label_stem(&stem)).or_insert(0);
        *entry += 1;
    }
    let mut parts: Vec<String> = counts
        .iter()
        .map(|(name, n)| {
            if *n > 1 {
                format!("{name} ×{n}")
            } else {
                (*name).to_string()
            }
        })
        .collect();
    parts.sort();
    let mut joined = parts.join(", ");
    const MAX: usize = 25;
    if joined.chars().count() > MAX {
        joined = joined.chars().take(MAX - 1).collect::<String>();
        joined.push('…');
    }
    joined
}

/// Maps a normalised action-label first word to a stable static stem.
/// Falls back to the input when the word is one we haven't catalogued —
/// gives operators useful labels for plugin-defined actions without
/// special-casing every variant.
fn label_stem(word: &str) -> &'static str {
    match word {
        "rebuild" => "rebuild",
        "restart" => "restart",
        "swap" => "swap",
        "terminate" => "terminate",
        "deploy" => "deploy",
        "upgrade" => "upgrade",
        "clone" => "clone",
        "scale" => "scale",
        "abort" => "abort",
        "save" => "config-save",
        "delete" => "delete",
        "apply" => "config-apply",
        _ => "action",
    }
}

/// Pure: render a one-line summary of a group of envs for the per-app
/// banner row. Shape: `"3 envs · 2 web · 1 worker · 1 red"`. Health
/// buckets only appear when non-zero so the summary doesn't include
/// noise like `0 red`. Tier counts only appear when both tiers are
/// represented in the group (showing `2 web` when every env is web adds
/// nothing).
fn summarize_group(envs: &[&Environment]) -> String {
    if envs.is_empty() {
        return String::new();
    }
    let total = envs.len();
    let mut web = 0usize;
    let mut worker = 0usize;
    let mut red = 0usize;
    let mut yellow = 0usize;
    for e in envs {
        match e.tier.as_str() {
            "Web" => web += 1,
            "Worker" => worker += 1,
            _ => {}
        }
        match e.health.to_lowercase().as_str() {
            "red" | "severe" | "degraded" => red += 1,
            "yellow" | "warning" => yellow += 1,
            _ => {}
        }
    }
    let env_word = if total == 1 { "env" } else { "envs" };
    let mut parts: Vec<String> = vec![format!("{total} {env_word}")];
    if web > 0 && worker > 0 {
        parts.push(format!("{web} web"));
        parts.push(format!("{worker} worker"));
    }
    if red > 0 {
        parts.push(format!("{red} red"));
    }
    if yellow > 0 {
        parts.push(format!("{yellow} yellow"));
    }
    parts.join(" · ")
}

/// Split a version label into "fixed prefix / moving build number / fixed
/// suffix" and render the moving part in `accent` with everything else
/// dimmed to `muted`. The "moving part" is the longest run of digits in
/// the label — usually the build number, version digit, or commit-prefix
/// number that operators care about scanning. If no digit run is found,
/// the whole label renders in `accent`.
///
/// Examples:
/// - `build-10678` → `build-` (muted) + `10678` (accent)
/// - `2026-20.1-rc` → `2026-` (muted) + `20` (accent) + `.1-rc` (muted)
///   (first longest run; ties broken by leftmost)
/// - `v3` → `v3` all in accent (no clear prefix/suffix worth dimming)
///
/// Pure — no theme access, no I/O. Caller passes resolved colours so the
/// helper stays testable.
fn format_version_label(label: &str, accent: Color, muted: Color) -> Line<'static> {
    let (start, end) = longest_digit_run(label);
    if start == end {
        // No digits found, or the whole string is digits with no clear
        // surrounding context — render in one colour.
        return Line::from(Span::styled(label.to_string(), Style::default().fg(accent)));
    }
    let mut spans: Vec<Span<'static>> = Vec::with_capacity(3);
    if start > 0 {
        spans.push(Span::styled(
            label[..start].to_string(),
            Style::default().fg(muted),
        ));
    }
    spans.push(Span::styled(
        label[start..end].to_string(),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    ));
    if end < label.len() {
        spans.push(Span::styled(
            label[end..].to_string(),
            Style::default().fg(muted),
        ));
    }
    Line::from(spans)
}

/// Pure: byte indices of the longest consecutive digit run in `s`. Returns
/// `(0, 0)` if there are no digits. Ties broken by leftmost match.
fn longest_digit_run(s: &str) -> (usize, usize) {
    let bytes = s.as_bytes();
    let mut best: (usize, usize) = (0, 0);
    let mut cur_start: Option<usize> = None;
    for (i, &b) in bytes.iter().enumerate() {
        if b.is_ascii_digit() {
            if cur_start.is_none() {
                cur_start = Some(i);
            }
        } else if let Some(start) = cur_start.take() {
            if i - start > best.1 - best.0 {
                best = (start, i);
            }
        }
    }
    if let Some(start) = cur_start {
        if bytes.len() - start > best.1 - best.0 {
            best = (start, bytes.len());
        }
    }
    best
}

fn humanize_age(d: chrono::Duration) -> String {
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        format!("{}s", secs)
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Render an embedded shell pane: a 1-row title at the top, a 1-row footer
/// hint at the bottom, and the vt100 screen contents filling the middle.
/// We resize the PTY to match the available space and iterate the parser's
/// screen cell-by-cell so xterm colours / bold / reverse propagate through
/// to the ratatui buffer.
fn draw_shell(f: &mut Frame, area: Rect, app: &mut App) {
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
fn vt100_color_to_ratatui(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
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
    use super::*;

    #[test]
    fn hover_index_maps_column_to_point() {
        use ratatui::layout::Rect;
        let area = Rect::new(10, 0, 11, 5); // x=10..20 (inclusive of x=20-1)
                                            // x=10 → first point; x=20 → last point.
        assert_eq!(super::hover_index(10, area, 11), Some(0));
        assert_eq!(super::hover_index(20, area, 11), Some(10));
        assert_eq!(super::hover_index(15, area, 11), Some(5));
        // Out of range → None.
        assert_eq!(super::hover_index(9, area, 11), None);
        assert_eq!(super::hover_index(21, area, 11), None);
        // Empty series → None even when in range.
        assert_eq!(super::hover_index(10, area, 0), None);
    }

    #[test]
    fn series_anomaly_flags_5xx_spike() {
        let v = vec![1.0, 1.0, 1.0, 1.0, 10.0];
        assert!(super::series_anomaly_label("req5xx", &v).is_some());
    }

    #[test]
    fn series_anomaly_quiet_when_stable() {
        let v = vec![5.0, 5.0, 5.0, 5.0, 5.5];
        assert!(super::series_anomaly_label("req5xx", &v).is_none());
    }

    #[test]
    fn series_anomaly_ignores_unrelated_id() {
        let v = vec![1.0, 1.0, 1.0, 1.0, 99.0];
        assert!(super::series_anomaly_label("health", &v).is_none());
    }

    #[test]
    fn series_anomaly_handles_short_series() {
        let v = vec![1.0, 9.0];
        assert!(super::series_anomaly_label("req5xx", &v).is_none());
    }

    #[test]
    fn humanize_age_buckets() {
        use chrono::Duration;
        assert_eq!(humanize_age(Duration::seconds(45)), "45s");
        assert_eq!(humanize_age(Duration::seconds(120)), "2m");
        assert_eq!(humanize_age(Duration::seconds(3601)), "1h");
        assert_eq!(humanize_age(Duration::seconds(2 * 86_400)), "2d");
        // Negative durations clamp to 0.
        assert_eq!(humanize_age(Duration::seconds(-30)), "0s");
    }

    #[test]
    fn humanize_duration_buckets() {
        assert_eq!(humanize_duration(15), "15s");
        assert_eq!(humanize_duration(90), "1m");
        assert_eq!(humanize_duration(3700), "1h1m");
        assert_eq!(humanize_duration(2 * 86_400 + 3 * 3600), "2d3h");
    }

    #[test]
    fn humanize_range_picks_unit() {
        assert_eq!(humanize_range(900), "15m");
        assert_eq!(humanize_range(3600), "1h");
        assert_eq!(humanize_range(2 * 86_400), "2d");
    }

    #[test]
    fn short_caller_extracts_principal() {
        assert_eq!(
            short_caller("arn:aws:iam::123456789012:user/alice"),
            "user/alice"
        );
        assert_eq!(
            short_caller("arn:aws:sts::123456789012:assumed-role/Foo/session-name"),
            "assumed-role/Foo/session-name"
        );
        assert_eq!(short_caller("not-an-arn"), "not-an-arn");
    }

    #[test]
    fn redact_passthrough_when_off() {
        assert_eq!(redact("hello", false), "hello");
    }

    #[test]
    fn redact_blocks_chars_when_on() {
        let out = redact("hello", true);
        assert_eq!(out.chars().count(), 5);
        assert!(out.chars().all(|c| c == '▓'));
    }

    #[test]
    fn redact_keeps_placeholder() {
        assert_eq!(redact("—", true), "—");
        assert_eq!(redact("", true), "");
    }

    #[test]
    fn format_metric_branches() {
        assert_eq!(format_metric("health", 12.0), "12");
        assert_eq!(format_metric("p90", 0.250), "250ms");
        assert_eq!(format_metric("p90", 1.5), "1.50s");
        assert_eq!(format_metric("req4xx", 42.0), "42");
    }

    #[test]
    fn micro_bar_renders() {
        assert_eq!(micro_bar(0, 100, 10), "");
        let half = micro_bar(50, 100, 10);
        // Should be 5 full blocks plus no remainder.
        assert!(half.chars().count() <= 10);
        assert!(half.chars().any(|c| c == '█'));
        let full = micro_bar(100, 100, 10);
        assert_eq!(full.chars().count(), 10);
    }

    #[test]
    fn micro_bar_guards_invalid_inputs() {
        assert_eq!(micro_bar(10, 0, 10), "");
        assert_eq!(micro_bar(10, 100, 0), "");
        assert_eq!(micro_bar(-5, 100, 10), "");
        // Above max clamps to full bar.
        assert_eq!(micro_bar(999, 100, 5).chars().count(), 5);
    }

    #[test]
    fn spinner_cycles_through_frames() {
        // Same window → same frame.
        let a = spinner(150, IconStyle::Unicode);
        let b = spinner(199, IconStyle::Unicode);
        assert_eq!(a, b);
        // Next window → different frame.
        assert_ne!(a, spinner(250, IconStyle::Unicode));
        // ASCII fallback uses a different palette.
        assert!(SPINNER_FRAMES.contains(&a));
        let ascii = spinner(0, IconStyle::Ascii);
        assert!(ASCII_SPINNER.contains(&ascii));
    }

    #[test]
    fn visible_window_anchors_to_top_when_items_fit() {
        // Items <= budget → window covers everything from 0.
        assert_eq!(visible_window(0, 5, 10), (0, 5));
        assert_eq!(visible_window(4, 5, 10), (0, 5));
    }

    #[test]
    fn visible_window_slides_to_keep_cursor_visible() {
        // 20 items, budget 5: cursor near top anchors to 0.
        assert_eq!(visible_window(0, 20, 5), (0, 5));
        assert_eq!(visible_window(1, 20, 5), (0, 5));
        // Cursor in middle centres.
        let (s, e) = visible_window(10, 20, 5);
        assert!(s <= 10 && 10 < e, "expected cursor 10 inside [{s},{e})");
        assert_eq!(e - s, 5);
        // Cursor at end clamps so the window doesn't run off.
        assert_eq!(visible_window(19, 20, 5), (15, 20));
    }

    #[test]
    fn visible_window_handles_empty_and_zero_budget() {
        assert_eq!(visible_window(0, 0, 10), (0, 0));
        // Zero budget: degenerate but must not crash; treat as 1.
        let (s, e) = visible_window(3, 10, 0);
        assert!(s <= 3 && 3 < e);
    }

    #[test]
    fn cursor_marker_swaps_per_icon_style() {
        let mut t = Theme::dark();
        t.icons = IconStyle::Unicode;
        assert_eq!(cursor_marker(&t), "▌ ");
        t.icons = IconStyle::Ascii;
        assert_eq!(cursor_marker(&t), "▌ ");
        t.icons = IconStyle::Powerline;
        assert!(cursor_marker(&t).contains('\u{e0b0}'));
    }

    #[test]
    fn highlight_env_in_summary_breaks_at_quoted_name() {
        let body = Style::default().fg(Color::White);
        let name = Style::default().fg(Color::Red);
        let line = highlight_env_in_summary(
            "Rebuild environment 'prod-api'? (terminates and recreates)",
            "prod-api",
            body,
            name,
        );
        // Expect at least 3 spans: leading "  " padding + body prefix +
        // env-name + body suffix. The name span should not contain quotes.
        let env_spans: Vec<&Span> = line
            .spans
            .iter()
            .filter(|s| s.content.contains("prod-api"))
            .collect();
        assert_eq!(env_spans.len(), 1);
        assert!(
            !env_spans[0].content.contains('\''),
            "name span should not include the surrounding single quotes: {:?}",
            env_spans[0].content
        );
    }

    #[test]
    fn highlight_env_in_summary_falls_back_when_name_missing() {
        let body = Style::default().fg(Color::White);
        let name = Style::default().fg(Color::Red);
        let line =
            highlight_env_in_summary("Some action with no env reference", "prod-api", body, name);
        // Should still render — just as one body span (plus the leading
        // "  " padding span).
        assert!(line.spans.iter().any(|s| s.content.contains("Some action")));
    }

    #[test]
    fn summarize_in_flight_collapses_duplicates() {
        let s = summarize_in_flight(&["Rebuild env", "Rebuild env", "Deploy version"]);
        assert!(s.contains("rebuild ×2"), "got {s:?}");
        assert!(s.contains("deploy"), "got {s:?}");
    }

    #[test]
    fn summarize_in_flight_truncates() {
        let s = summarize_in_flight(&[
            "Terminate env",
            "Rebuild env",
            "Restart env",
            "Deploy version",
            "Swap CNAMEs",
        ]);
        assert!(
            s.chars().count() <= 25,
            "got {} chars: {s:?}",
            s.chars().count()
        );
    }

    #[test]
    fn summarize_in_flight_empty() {
        assert_eq!(summarize_in_flight(&[]), "");
    }

    #[test]
    fn summarize_group_omits_empty_buckets() {
        // Build envs with the minimal fields we use in summarize_group.
        // The full Environment struct has many fields; spread defaults
        // for the others.
        fn e(tier: &str, health: &str) -> Environment {
            Environment {
                name: "n".into(),
                application: "a".into(),
                tier: tier.into(),
                status: "Ready".into(),
                health: health.into(),
                cname: "".into(),
                platform: "".into(),
                version_label: "".into(),
                updated: None,
                id: None,
                region: None,
                arn: None,
            }
        }
        let envs = vec![e("Web", "Green"), e("Web", "Green"), e("Web", "Red")];
        let refs: Vec<&Environment> = envs.iter().collect();
        let s = summarize_group(&refs);
        // 3 envs, all web (no worker), 1 red — only the non-empty buckets
        // appear. Tier split omitted because everyone is web.
        assert!(s.contains("3 envs"));
        assert!(s.contains("1 red"));
        assert!(!s.contains("worker"));
        assert!(!s.contains("yellow"));
    }

    #[test]
    fn summarize_group_shows_tier_split_when_both_present() {
        fn e(tier: &str, health: &str) -> Environment {
            Environment {
                name: "n".into(),
                application: "a".into(),
                tier: tier.into(),
                status: "Ready".into(),
                health: health.into(),
                cname: "".into(),
                platform: "".into(),
                version_label: "".into(),
                updated: None,
                id: None,
                region: None,
                arn: None,
            }
        }
        let envs = vec![e("Web", "Green"), e("Worker", "Yellow"), e("Worker", "Red")];
        let refs: Vec<&Environment> = envs.iter().collect();
        let s = summarize_group(&refs);
        assert!(s.contains("1 web"));
        assert!(s.contains("2 worker"));
        assert!(s.contains("1 red"));
        assert!(s.contains("1 yellow"));
    }

    #[test]
    fn summarize_group_empty_input() {
        assert_eq!(summarize_group(&[]), "");
    }

    #[test]
    fn version_label_highlights_build_number() {
        // Pure helper returns a Line we can inspect span-by-span.
        let line = format_version_label("build-10678", Color::Cyan, Color::DarkGray);
        let texts: Vec<String> = line.spans.iter().map(|s| s.content.to_string()).collect();
        assert_eq!(texts, vec!["build-", "10678"]);
    }

    #[test]
    fn version_label_dims_prefix_and_suffix() {
        let line = format_version_label("v2026-20-1-rc", Color::Cyan, Color::DarkGray);
        let texts: Vec<String> = line.spans.iter().map(|s| s.content.to_string()).collect();
        // Longest digit run is "2026"; preceding "v" gets dimmed, trailing
        // "-20-1-rc" also dimmed.
        assert_eq!(texts, vec!["v", "2026", "-20-1-rc"]);
    }

    #[test]
    fn version_label_no_digits_one_span() {
        let line = format_version_label("staging", Color::Cyan, Color::DarkGray);
        assert_eq!(line.spans.len(), 1);
        assert_eq!(line.spans[0].content, "staging");
    }

    #[test]
    fn longest_digit_run_picks_first_on_tie() {
        // Two equal-length runs — leftmost wins.
        assert_eq!(longest_digit_run("v12-34"), (1, 3));
    }

    #[test]
    fn longest_digit_run_empty_no_digits() {
        assert_eq!(longest_digit_run(""), (0, 0));
        assert_eq!(longest_digit_run("abc"), (0, 0));
    }

    #[test]
    fn action_glyph_is_distinct_per_action_per_icon_style() {
        use crate::app::ACTIONS;
        use std::collections::HashSet;
        // Within Powerline mode every action glyph should be distinct
        // (so Terminate doesn't share with Restart, etc.) modulo the
        // intentional Terminate / TerminateInstance / ConfigDelete reuse
        // of the trash icon. We assert "not all the same".
        for icons in [IconStyle::Unicode, IconStyle::Ascii, IconStyle::Powerline] {
            let glyphs: HashSet<&str> = ACTIONS.iter().map(|a| a.glyph(icons)).collect();
            assert!(
                glyphs.len() >= ACTIONS.len() / 2,
                "too many action-glyph collisions in {icons:?}: {glyphs:?}"
            );
        }
    }

    #[test]
    fn caret_glyph_falls_back_to_underscore_on_ascii() {
        let mut t = Theme::dark();
        t.icons = IconStyle::Ascii;
        assert_eq!(caret_glyph(&t), "_");
        t.icons = IconStyle::Unicode;
        assert_eq!(caret_glyph(&t), "\u{258e}");
        t.icons = IconStyle::Powerline;
        assert_eq!(caret_glyph(&t), "\u{258e}");
    }

    #[test]
    fn pill_chain_uses_left_wedge_for_lead_in_in_powerline_mode() {
        let mut t = Theme::dark();
        t.icons = IconStyle::Powerline;
        let pills = vec![("ALERT".to_string(), Color::White, Color::Red)];
        let spans = pill_chain(&pills, &t);
        // Expect: lead-in wedge (E0B2) + pill body + trailing wedge (E0B0).
        let first_glyph: String = spans[0].content.to_string();
        assert!(
            first_glyph.contains('\u{e0b2}'),
            "expected U+E0B2 left-pointing wedge as lead-in, got {first_glyph:?}"
        );
        // The trailing arrow at the end of the chain is still E0B0 (right-
        // pointing), so the pill's outline is symmetric: ◀ ALERT ▶.
        let last_glyph: String = spans.last().unwrap().content.to_string();
        assert!(
            last_glyph.contains('\u{e0b0}'),
            "expected U+E0B0 right-pointing wedge as trail-out, got {last_glyph:?}"
        );
    }

    #[test]
    fn pill_chain_no_powerline_glyphs_in_unicode_mode() {
        let mut t = Theme::dark();
        t.icons = IconStyle::Unicode;
        let pills = vec![("ALERT".to_string(), Color::White, Color::Red)];
        let spans = pill_chain(&pills, &t);
        for s in &spans {
            let body = s.content.to_string();
            assert!(
                !body.contains('\u{e0b0}') && !body.contains('\u{e0b2}'),
                "non-Powerline mode emitted a Powerline triangle: {body:?}"
            );
        }
    }

    #[test]
    fn sep_uses_powerline_glyph_when_opted_in() {
        let mut t = Theme::dark();
        t.icons = IconStyle::Unicode;
        let unicode_sep = sep(&t).content.to_string();
        assert!(unicode_sep.contains('•'));
        t.icons = IconStyle::Powerline;
        let pl_sep = sep(&t).content.to_string();
        assert!(
            pl_sep.contains('\u{e0b1}'),
            "expected U+E0B1 thin separator, got {pl_sep:?}"
        );
        // ASCII path stays on the bullet — opting *out* of unicode shouldn't
        // accidentally trigger a powerline glyph.
        t.icons = IconStyle::Ascii;
        assert!(sep(&t).content.to_string().contains('•'));
    }

    #[test]
    fn tab_icon_is_distinct_per_tab() {
        for icons in [IconStyle::Unicode, IconStyle::Ascii, IconStyle::Powerline] {
            use std::collections::HashSet;
            let tabs = [
                DetailTab::Events,
                DetailTab::Instances,
                DetailTab::Metrics,
                DetailTab::Queue,
                DetailTab::Config,
            ];
            let seen: HashSet<&str> = tabs.iter().map(|t| tab_icon(*t, icons)).collect();
            assert_eq!(seen.len(), tabs.len(), "icons collide for {icons:?}");
        }
    }

    #[test]
    fn titled_block_decorates_per_icon_style() {
        // Crude: render to a buffer and confirm the title text appears.
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::widgets::Widget;
        let mut t = Theme::dark();
        let b = titled_block(&t, "ebman", true, t.title);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        b.render(buf.area, &mut buf);
        let rendered = buffer_to_string(&buf);
        assert!(rendered.contains("◆"));
        assert!(rendered.contains("ebman"));

        t.icons = IconStyle::Ascii;
        let b2 = titled_block(&t, "ebman", true, t.title);
        let mut buf = Buffer::empty(Rect::new(0, 0, 30, 3));
        b2.render(buf.area, &mut buf);
        let rendered = buffer_to_string(&buf);
        assert!(rendered.contains("[ ebman ]"));
        assert!(!rendered.contains("◆"));
    }

    #[test]
    fn pill_wraps_text_with_padding() {
        let s = pill("READY", Color::Black, Color::Green);
        assert_eq!(s.content.as_ref(), " READY ");
    }

    #[test]
    fn health_dot_falls_back_to_ascii() {
        let mut t = Theme::dark();
        let dot = health_dot("green", &t);
        assert_eq!(dot.content.as_ref(), "●");
        t.icons = IconStyle::Ascii;
        let dot = health_dot("green", &t);
        assert_eq!(dot.content.as_ref(), "*");
    }

    fn buffer_to_string(buf: &ratatui::buffer::Buffer) -> String {
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                let cell = &buf[(x, y)];
                out.push_str(cell.symbol());
            }
            out.push('\n');
        }
        out
    }
}
