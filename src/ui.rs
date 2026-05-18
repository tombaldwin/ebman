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
    let decorated = if theme.icons == IconStyle::Ascii {
        format!("[ {trimmed} ]")
    } else {
        format!("[ ◆ {trimmed} ◆ ]")
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

fn health_dot(health: &str, theme: &Theme) -> Span<'static> {
    let c = match health.to_lowercase().as_str() {
        "green" | "ok" => theme.health_green,
        "yellow" | "warning" => theme.health_yellow,
        "red" | "severe" | "degraded" => theme.health_red,
        "grey" | "gray" | "info" | "no data" | "pending" => theme.health_grey,
        _ => theme.text,
    };
    let glyph = if theme.icons == IconStyle::Ascii {
        "*"
    } else {
        "●"
    };
    Span::styled(glyph, Style::default().fg(c).add_modifier(Modifier::BOLD))
}

fn spinner(elapsed_ms: u128, icons: IconStyle) -> &'static str {
    match icons {
        IconStyle::Unicode => SPINNER_FRAMES[(elapsed_ms / 100) as usize % SPINNER_FRAMES.len()],
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
    // Background — Dlq / Detail use a full-screen alternative layout; otherwise
    // draw the main header + table + events + footer.
    if app.mode == Mode::Dlq && app.dlq.is_some() {
        draw_dlq(f, f.area(), app);
    } else if app.mode == Mode::Detail && app.detail.is_some() {
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
        }
    }
    if app.mode == Mode::Palette {
        draw_palette(f, f.area(), app);
    }
    // Toasts render last so they overlay everything else.
    if !app.toasts.is_empty() {
        draw_toasts(f, f.area(), app);
    }
}

fn draw_palette(f: &mut Frame, area: Rect, app: &App) {
    let popup = centered_rect(60, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(popup);

    // Input bar
    let input = Paragraph::new(Line::from(vec![
        Span::styled(
            " ❯ ",
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.palette_input.clone(), Style::default().fg(theme.text)),
        Span::styled(
            "_",
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::SLOW_BLINK),
        ),
    ]))
    .block(titled_block(theme, "palette", true, theme.title_alt));
    f.render_widget(input, layout[0]);

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
        .block(rounded_block(theme, true))
        .highlight_style(
            Style::default()
                .bg(theme.row_selected_bg)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▌ ");
    let mut state = app.palette_state.clone();
    f.render_stateful_widget(list, layout[1], &mut state);

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
    f.render_widget(hint, layout[2]);
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
        let (border_color, label) = match t.kind {
            ToastKind::Info => (theme.title, "info"),
            ToastKind::Success => (theme.health_green, "ok"),
            ToastKind::Error => (theme.health_red, "error"),
        };
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(border_color))
            .title(Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(border_color)
                    .add_modifier(Modifier::BOLD),
            ));
        let mut text = t.text.clone();
        // Truncate so it fits one line inside the box.
        let max = (width as usize).saturating_sub(4);
        if text.chars().count() > max {
            text = text.chars().take(max.saturating_sub(1)).collect::<String>();
            text.push('…');
        }
        let para = Paragraph::new(Line::from(Span::styled(
            text,
            Style::default().fg(theme.text),
        )))
        .block(block);
        f.render_widget(Clear, rect);
        f.render_widget(para, rect);
        y += toast_h;
    }
}

fn draw_saved_configs_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_rect(60, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let lines: Vec<Line> = text
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
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(
            &app.theme,
            "saved configurations — esc / q to close",
            true,
            app.theme.title,
        )
        .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_diff_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_rect(80, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let lines: Vec<Line> = text
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
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(&app.theme, "diff — esc / q to close", true, app.theme.title)
            .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_alarms_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
    let popup = centered_rect(70, 70, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let lines: Vec<Line> = text
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
    let p = Paragraph::new(lines).wrap(Wrap { trim: false }).block(
        titled_block(
            &app.theme,
            "alarms — esc / q to close",
            true,
            app.theme.title,
        )
        .padding(Padding::uniform(1)),
    );
    f.render_widget(p, popup);
}

fn draw_history_overlay(f: &mut Frame, area: Rect, app: &App, text: &str) {
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
            "history — esc / q to close",
            true,
            app.theme.title,
        )
        .padding(Padding::uniform(1)),
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
    let show_loading = app
        .loading_since
        .map(|t| t.elapsed() >= std::time::Duration::from_millis(300))
        .unwrap_or(false);
    let elapsed_ms = app
        .loading_since
        .map(|t| t.elapsed().as_millis())
        .unwrap_or(0);
    let status: Span<'static> = match app.load_state {
        LoadState::Error => Span::styled("error", Style::default().fg(theme.health_red)),
        LoadState::Loading if show_loading => Span::styled(
            format!("{} loading…", spinner(elapsed_ms, theme.icons)),
            Style::default().fg(theme.health_yellow),
        ),
        _ => Span::styled("idle", Style::default().fg(theme.health_green)),
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

    let mut line1 = kv("Account", &account);
    line1.push(sep());
    line1.extend(kv("Region", &app.context.region));
    line1.push(sep());
    line1.extend(kv("Profile", &profile));
    let mut line2 = kv("Caller", &caller);
    line2.push(sep());
    line2.extend(kv("Envs", &env_count));
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
    line2.push(sep());
    line2.extend(kv("Last", &last));
    line2.push(sep());
    line2.push(Span::raw("Status: "));
    line2.push(status);
    let sort_dir = if app.sort_desc { "↓" } else { "↑" };
    let sort_label = format!("{}{}", app.sort_key.label(), sort_dir);
    line2.push(sep());
    line2.extend(kv("Sort", &sort_label));
    if !app.filter.is_empty() {
        line2.push(sep());
        let filter_text = app.filter.clone();
        line2.push(Span::styled("Filter: ", Style::default().fg(theme.muted)));
        line2.push(Span::styled(
            filter_text,
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if app.grouped {
        line2.push(sep());
        line2.push(pill("GROUPED", Color::Black, theme.title_alt));
    }
    match app.view_mode {
        ViewMode::Compact => {
            line2.push(sep());
            line2.push(pill("COMPACT", Color::Black, theme.accent));
        }
        ViewMode::Spacious => {
            line2.push(sep());
            line2.push(pill("SPACIOUS", Color::Black, theme.accent));
        }
        ViewMode::Default => {}
    }
    if app.redact {
        line2.push(sep());
        line2.push(pill("REDACT", Color::Black, theme.health_yellow));
    }
    if app.alerts > 0 {
        line2.push(sep());
        line2.push(pill(
            &format!(
                "! {} alert{}",
                app.alerts,
                if app.alerts == 1 { "" } else { "s" }
            ),
            Color::White,
            theme.health_red,
        ));
    }
    if app.frozen {
        line2.push(sep());
        line2.push(pill("FROZEN", Color::Black, theme.health_grey));
    }
    if app.read_only {
        line2.push(sep());
        line2.push(pill("READ-ONLY", Color::Black, theme.health_green));
    }
    if let Some(release) = app.update_available.as_ref() {
        line2.push(sep());
        line2.push(pill(
            &format!("UPDATE {}", release.version),
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
            line2.push(sep());
            line2.push(pill(&label, Color::Black, bg));
        }
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
        .highlight_symbol("▌ ")
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
    let header_cells: Vec<Cell> = columns
        .iter()
        .map(|(label, key)| {
            // The HEALTH column is rendered as the dot glyph but labelled "●"
            // in the header for the canonical column; sort marker only on it
            // (and the canonical NAME/APPLICATION/STATUS/VERSION/AGE columns).
            let display = if *label == "HEALTH" { "●" } else { *label };
            let mut text = display.to_string();
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
                        "TREND" => Cell::from(sparkline_for(app.history.get(&e.name), &theme)),
                        "PLATFORM" => {
                            Cell::from(e.platform.clone()).style(Style::default().fg(theme.muted))
                        }
                        "VERSION" => Cell::from(e.version_label.clone())
                            .style(Style::default().fg(theme.app_palette[0])),
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
                let next_color = display
                    .iter()
                    .skip(row_idx + 1)
                    .find_map(|r| match r {
                        DisplayRow::Env(i) => {
                            app_colors.get(&app.environments[*i].application).copied()
                        }
                        _ => None,
                    })
                    .unwrap_or(theme.muted);
                let dashes = "─".repeat(DIVIDER_FILL_WIDTH);
                let count = columns.len();
                let cells = (0..count).map(|_| {
                    Cell::from(Span::styled(
                        dashes.clone(),
                        Style::default().fg(next_color),
                    ))
                });
                Row::new(cells)
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
        .highlight_symbol("▌ ")
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
    // or when a filter has hidden everything.
    if env_count_visible == 0 {
        let (heading, hint) = if env_count_total == 0 {
            (
                "no environments in this account / region",
                "try a different region (r) or profile (p), or check the AWS console (b)",
            )
        } else {
            (
                "no environments match the active filter",
                "press / to edit, or Esc in filter mode to clear",
            )
        };
        let inner = Rect {
            x: area.x + 2,
            y: area.y + area.height / 2,
            width: area.width.saturating_sub(4),
            height: 2,
        };
        let lines = vec![
            Line::from(Span::styled(
                format!("  ◌  {heading}"),
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("     {hint}"),
                Style::default().fg(theme.muted),
            )),
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
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
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
    let lower = status.to_lowercase();
    if lower == "ready" {
        Cell::from(pill("Ready", Color::Black, theme.status_ready))
    } else if matches!(lower.as_str(), "updating" | "launching") {
        Cell::from(pill(status, Color::Black, theme.status_updating))
    } else if matches!(lower.as_str(), "terminating" | "terminated") {
        Cell::from(pill(status, Color::White, theme.status_terminating))
    } else {
        Cell::from(Span::styled(
            status.to_string(),
            Style::default().fg(theme.text),
        ))
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
    match app.mode {
        Mode::Filter => {
            top.push(Span::styled(
                " /",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ));
            top.push(Span::raw(" "));
            top.push(Span::styled(
                app.filter.clone(),
                Style::default().fg(Color::White),
            ));
            top.push(Span::styled(
                "_",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
            ));
            top.push(Span::styled(
                "  [enter] apply  [esc] cancel",
                Style::default().fg(Color::Gray),
            ));
        }
        Mode::Command => {
            top.push(Span::styled(
                " :",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ));
            top.push(Span::styled(
                app.command_input.clone(),
                Style::default().fg(Color::White),
            ));
            top.push(Span::styled(
                "_",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::SLOW_BLINK),
            ));
            top.push(Span::styled(
                "   [enter] run  [esc] cancel",
                Style::default().fg(Color::Gray),
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
                "_",
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
                    Style::default().fg(Color::Red),
                ));
            } else if let Some(msg) = &app.status_message {
                top.push(Span::styled(
                    format!(" {msg}"),
                    Style::default().fg(Color::Yellow),
                ));
            } else if !app.filter.is_empty() {
                top.push(Span::styled(
                    format!(" filter: {}", app.filter),
                    Style::default().fg(Color::Yellow),
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
                _ => " j/k move  1-9 jump  ' name-jump  g/G top/bottom  tab scope  enter drill  b console  D describe  space multi  * pin  / filter  : command  ^K palette  s/S sort  ^G group  ^E events  ^] focus  f freeze  y/Y yank  ^Y export  ^W cli  r region  p profile  ^R refresh  ^X redact  ? help  q quit".into(),
            }
        }
        Mode::Detail => {
            " tab/shift-tab switch  j/k scroll  a actions  ^R refresh  R auto-refresh  esc / q back"
                .into()
        }
        Mode::Action => " j/k move  type to filter  enter confirm  esc cancel".into(),
        Mode::Dlq => match app.dlq.as_ref().map(|d| d.viewing) {
            Some(crate::app::QueueView::Main) => {
                " MAIN  j/k move  enter view body  x delete  m → DLQ  ^R refresh  esc / q back".into()
            }
            _ => " DLQ  j/k move  enter view body  r resend  x delete  p purge  m → MAIN  ^R refresh  esc / q back".into(),
        },
    };
    f.render_widget(
        Paragraph::new(Span::styled(keys, Style::default().fg(Color::Gray))),
        rows[1],
    );
}

fn draw_help(f: &mut Frame, area: Rect, app: &App) {
    let popup = centered_rect(70, 70, area);
    f.render_widget(Clear, popup);

    let interval_secs = app.refresh_interval.as_secs();
    let lines = vec![
        Line::from(Span::styled(
            "ebman — keybindings",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        help_line("j / ↓ / wheel", "move selection down"),
        help_line("k / ↑ / wheel", "move selection up"),
        help_line("g / G", "jump to top / bottom"),
        help_line("enter", "open drill-down view for the selected env"),
        help_line("a", "open actions menu (rebuild / restart / swap / terminate)"),
        help_line("b", "open selected env in the AWS console"),
        help_line("D", "describe overlay (raw env dump as JSON)"),
        help_line("f", "freeze / unfreeze auto-refresh"),
        help_line("1 - 9", "jump to env at position 1-9 in the current view"),
        help_line("'", "name-jump: type a prefix to move selection"),
        help_line("Ctrl-W", "yank equivalent `aws elasticbeanstalk describe-environments` command"),
        help_line("tab / shift-tab", "cycle scope (envs ↔ apps)"),
        help_line("click", "select row"),
        help_line("/", "filter rows (name, app, status, health)"),
        help_line("s / S", "cycle sort key / toggle ascending"),
        help_line("Ctrl-G", "toggle group-by-application"),
        help_line("Ctrl-E", "toggle events panel"),
        help_line("y / Y", "yank CNAME / name to clipboard"),
        help_line("Ctrl-Y", "export filtered table as TSV to clipboard"),
        help_line("r", "switch AWS region"),
        help_line("p", "switch AWS profile"),
        help_line("Ctrl-K", "command palette: fuzzy search across commands / envs / views / plugins"),
        help_line("Ctrl-R / F5", "refresh now"),
        help_line("Ctrl-X", "toggle redact mode (account id, ARN, CNAMEs)"),
        help_line("?", "toggle this help"),
        help_line("q / Ctrl-C", "quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Command bar (press :)",
            Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
        )),
        help_line(":q", "quit"),
        help_line(":region X", "switch AWS region"),
        help_line(":profile X", "switch AWS profile"),
        help_line(":sort KEY [desc]", "set sort (name/app/status/health/version/age)"),
        help_line(":group on|off", "toggle grouping"),
        help_line(":redact on|off", "toggle redact mode"),
        help_line(":save NAME", "save current filter as NAME"),
        help_line(":f NAME / :filter NAME", "recall a saved filter"),
        help_line(":filters / :drop NAME", "list / remove saved filters"),
        help_line(":events on|off", "toggle the events panel"),
        help_line(":export / :json / :report", "copy filtered table (TSV / JSON / Markdown)"),
        help_line(":refresh", "re-fetch the table immediately"),
        help_line(":readonly on|off", "toggle destructive-action lockout"),
        help_line(":alias NAME LABEL", "set or update a local env alias"),
        help_line(":alias-drop NAME", "remove an alias"),
        help_line(":pin", "pin / unpin the selected env (also `*`)"),
        help_line(":whatsnew", "embedded changelog popup"),
        help_line(":save-view NAME", "snapshot filter/sort/grouping/scope under NAME"),
        help_line(":view NAME", "load a previously saved view"),
        help_line(":views / :view-drop NAME", "list / remove saved views"),
        help_line(":history", "show recent info/error messages"),
        help_line(":cols", "list / hide / show / reset columns (e.g. :cols hide PLATFORM)"),
        help_line(":diff NAME", "side-by-side comparison with another env"),
        help_line(":alarms", "CloudWatch alarms list for selected env"),
        help_line(":loglevel LEVEL", "live-reload tracing filter (trace/debug/info/warn/error)"),
        help_line(":saved-configs", "list EB saved configuration templates per application"),
        help_line(":plugins  /  :NAME", "list / invoke plugin commands defined in commands.toml"),
        help_line("[ / ] (Metrics tab)", "decrease / increase metric range (15m → 24h)"),
        help_line("(Logs tab) ^R", "request tail logs (takes ~10–20s while EB samples instances)"),
        help_line("(Logs tab) /", "regex-filter the visible log lines"),
        Line::from(""),
        Line::from(Span::styled(
            format!(
                "Refresh runs automatically every {interval_secs}s. Theme: {}. Configurable in ~/.config/ebman/config.toml.",
                app.theme.name
            ),
            Style::default().fg(Color::Gray),
        )),
        Line::from(Span::styled(
            "Region/profile come from the standard AWS env (AWS_REGION, AWS_PROFILE).",
            Style::default().fg(Color::Gray),
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
                        Style::default()
                            .fg(Color::White)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("recv:{:<3} ", m.receive_count),
                        Style::default().fg(Color::Yellow),
                    ),
                    Span::styled(format!("{:>5} ", age), Style::default().fg(Color::Gray)),
                    Span::raw(preview),
                ]))
            })
            .collect();
        let list = List::new(items)
            .block(block)
            .highlight_style(
                Style::default()
                    .bg(Color::Rgb(40, 60, 90))
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol("▌ ");
        f.render_stateful_widget(list, chunks[1], &mut dlq.list_state);
    }

    // Footer / confirm
    if dlq.confirm_purge {
        let line = Paragraph::new(Line::from(vec![
            Span::styled(
                " PURGE DLQ — type ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                dlq.env_name.clone(),
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " to confirm: ",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                dlq.purge_typed.clone(),
                Style::default()
                    .fg(if dlq.purge_typed == dlq.env_name {
                        Color::Green
                    } else {
                        Color::White
                    })
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "_",
                Style::default()
                    .fg(Color::Yellow)
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
                Some(err) => Span::styled(format!(" {err}"), Style::default().fg(Color::Red)),
                None => Span::raw(""),
            }),
            Line::from(Span::styled(keys, Style::default().fg(Color::Gray))),
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
                    ListItem::new(Line::from(Span::styled(format!(" {} ", a.label()), style)))
                })
                .collect();
            let list = List::new(items)
                .block(titled_block(&theme, "action", true, theme.title_alt))
                .highlight_style(
                    Style::default()
                        .bg(theme.row_selected_bg)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("▌ ");
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
                    "_",
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
                    Style::default().fg(Color::Gray),
                )),
                layout[2],
            );
        }
        ActionFlow::Confirm(modal) => {
            let popup = centered_rect(60, 35, area);
            f.render_widget(Clear, popup);
            let block = Block::default().borders(Borders::ALL).title(Span::styled(
                " confirm ",
                Style::default()
                    .fg(if modal.action.destructive() {
                        Color::Red
                    } else {
                        Color::Magenta
                    })
                    .add_modifier(Modifier::BOLD),
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
            };
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                format!("  {summary}"),
                Style::default()
                    .fg(if modal.action.destructive() {
                        Color::Red
                    } else {
                        Color::White
                    })
                    .add_modifier(Modifier::BOLD),
            )));
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
                        let msg = e.message.chars().take(70).collect::<String>();
                        lines.push(Line::from(vec![
                            Span::styled(
                                format!("    {when:>4}  "),
                                Style::default().fg(theme.muted),
                            ),
                            Span::styled(
                                format!("{:<5}  ", e.severity),
                                severity_style(&e.severity, &theme),
                            ),
                            Span::styled(msg, Style::default().fg(theme.text)),
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
                        Style::default().fg(Color::Gray),
                    )));
                }
                ConfirmKind::TypeName => {
                    lines.push(Line::from(vec![
                        Span::styled("  type ", Style::default().fg(Color::Gray)),
                        Span::styled(
                            modal.target_env.clone(),
                            Style::default()
                                .fg(Color::Yellow)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" to confirm:", Style::default().fg(Color::Gray)),
                    ]));
                    lines.push(Line::from(""));
                    let matches = modal.typed == modal.target_env;
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            modal.typed.clone(),
                            Style::default()
                                .fg(if matches { Color::Green } else { Color::White })
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            "_",
                            Style::default()
                                .fg(Color::Yellow)
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
                        Style::default().fg(Color::Gray),
                    )));
                }
            }
            f.render_widget(Paragraph::new(lines).block(block), popup);
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
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    format!("  elapsed {elapsed}s"),
                    Style::default().fg(Color::Gray),
                )),
            ];
            let block = Block::default().borders(Borders::ALL).title(Span::styled(
                " running ",
                Style::default()
                    .fg(Color::Yellow)
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

    // Env header
    let mut h1 = kv("Name", &env.name);
    h1.push(sep());
    h1.extend(kv("Application", &env.application));
    h1.push(sep());
    h1.extend(kv("Status", &env.status));
    h1.push(sep());
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
    let mut h2 = kv("Platform", &env.platform);
    h2.push(sep());
    h2.extend(kv("Version", &env.version_label));
    h2.push(sep());
    h2.extend(kv("CNAME", &cname_text));
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
            " tab/shift-tab switch  j/k scroll  a actions  ^R refresh  R auto-refresh  esc / q back",
            Style::default().fg(app.theme.muted),
        )),
    ]);
    f.render_widget(footer, chunks[3]);
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
    let mut spans: Vec<Span> = vec![Span::raw(" ")];
    for (i, t) in tabs.iter().enumerate() {
        if i > 0 {
            spans.push(Span::styled("   ", Style::default()));
        }
        let label = format!(" {} {} ", tab_icon(*t, theme.icons), t.title());
        let style = if i == active {
            Style::default()
                .fg(Color::Black)
                .bg(theme.border_active)
                .add_modifier(Modifier::BOLD)
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
    let outer = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            title,
            Style::default()
                .fg(Color::Cyan)
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
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                detail.search_input.clone(),
                Style::default().fg(Color::White),
            ),
        ];
        if detail.search_active {
            spans.push(Span::styled(
                "_",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
            ));
            spans.push(Span::styled(
                "  [enter] apply  [esc] cancel",
                Style::default().fg(Color::Gray),
            ));
        } else if let Some(err) = &detail.search_error {
            spans.push(Span::styled(
                format!("  {err}"),
                Style::default().fg(Color::Red),
            ));
        } else if detail.search_pattern.is_some() {
            spans.push(Span::styled(
                "  n / N next/prev   / re-edit   esc clear",
                Style::default().fg(Color::Gray),
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
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" Instances [{}] ", detail.instances.len()),
            Style::default()
                .fg(Color::Cyan)
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
    let lines: Vec<Line> = detail
        .instances
        .iter()
        .flat_map(|i| {
            let age = i
                .launched_at
                .map(|t| humanize_age(now.signed_duration_since(t)))
                .unwrap_or_else(|| "—".into());
            let mut head = vec![
                Span::styled(
                    format!("{:<19} ", i.id),
                    Style::default()
                        .fg(Color::White)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("{:<8} ", i.health), health_style(&i.color, theme)),
                Span::styled(
                    format!("{:<12} ", i.instance_type),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(
                    format!("{:<14} ", i.availability_zone),
                    Style::default().fg(Color::Gray),
                ),
                Span::styled(format!("up {age}"), Style::default().fg(Color::Gray)),
            ];
            let mut lines = vec![Line::from(std::mem::take(&mut head))];
            for cause in &i.causes {
                lines.push(Line::from(Span::styled(
                    format!("    ↳ {cause}"),
                    Style::default().fg(Color::Yellow),
                )));
            }
            lines
        })
        .collect();
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
        let mut title_spans: Vec<Span<'static>> = vec![
            Span::styled(
                format!("{:<26} ", series.label),
                Style::default()
                    .fg(series_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("now {}  ", format_metric(&series.id, last)),
                Style::default().fg(theme.text),
            ),
            Span::styled(
                format!("max {}  ", format_metric(&series.id, max)),
                Style::default().fg(theme.muted),
            ),
            Span::styled(
                format!("min {}  ", format_metric(&series.id, min)),
                Style::default().fg(theme.muted),
            ),
            delta_span(delta, &series.id, theme),
        ];
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
    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            " Queue ",
            Style::default()
                .fg(Color::Cyan)
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
        Style::default().fg(Color::Gray),
    )));
    if detail.loading_queues {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  loading queue stats…",
            Style::default().fg(Color::Yellow),
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
    let outer = Block::default()
        .borders(Borders::ALL)
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
        LogTailStage::Idle => Line::from(Span::styled(
            " press ^R to start log tail",
            Style::default().fg(theme.muted),
        )),
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
                        "_",
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
        // Tag-key column: 20 chars normally; long keys (e.g.
        // `elasticbeanstalk:environment-name`) blow past the column, so we
        // always emit at least 2 spaces of separator before the value.
        let tag_key_col = 20usize;
        for (k, v) in &detail.tags {
            let key_text = if k.chars().count() < tag_key_col {
                format!("  {k:<width$}", width = tag_key_col)
            } else {
                format!("  {k}  ")
            };
            lines.push(Line::from(vec![
                Span::styled(key_text, Style::default().fg(theme.app_palette[0])),
                Span::styled(v.clone(), Style::default().fg(theme.text)),
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
            "_",
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
        Style::default().fg(Color::Gray),
    ));
    f.render_widget(hint, layout[2]);
}

fn help_line<'a>(key: &'a str, desc: &'a str) -> Line<'a> {
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
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(desc, Style::default().fg(Color::White)),
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
        spans.push(Span::styled(" / ", Style::default().fg(theme.muted)));
        spans.push(Span::styled(
            app_name,
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(" / ", Style::default().fg(theme.muted)));
        spans.push(Span::styled(
            env_name,
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

fn kv<'a>(key: &'a str, value: &'a str) -> Vec<Span<'a>> {
    vec![
        Span::styled(format!("{key}: "), Style::default().fg(Color::Gray)),
        Span::styled(
            value.to_string(),
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
    ]
}

fn sep() -> Span<'static> {
    Span::styled("  •  ", Style::default().fg(Color::Gray))
}

fn sparkline_for(
    samples: Option<&std::collections::VecDeque<String>>,
    theme: &Theme,
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
        spans.push(Span::styled("▇", style));
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
    fn tab_icon_is_distinct_per_tab() {
        for icons in [IconStyle::Unicode, IconStyle::Ascii] {
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
