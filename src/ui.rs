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

/// Builds the contextual pill chain — group/view/redact/alerts/in-flight/
/// frozen/read-only/update/sso — that sits in the header. Pure: same
/// inputs → same chain, no time/IO except the SSO countdown which reads
/// `Utc::now()`.
fn build_chain_pills(app: &App) -> Vec<(String, Color, Color)> {
    let theme = &app.theme;
    // Single source of truth for pill text colour: WCAG-derived contrast
    // against each pill's bg, so light + high-contrast themes don't render
    // black-on-dark or white-on-bright tofu. Previously every pill
    // hardcoded `Color::Black` (with one `Color::White` outlier for
    // alerts) which broke the moment a theme changed.
    let fg = |bg: Color| theme.contrast_text(bg);

    // Pill ordering follows the priority used by `prune_pills_to_width` —
    // most operationally critical signals (alerts, pending, multi-select,
    // read-only, update) land first so they survive the elision pass when
    // the header gets narrower. UX signals (grouped / compact / redact /
    // SSO / frozen) drop first.
    let mut chain: Vec<(String, Color, Color)> = Vec::new();
    if app.alerts > 0 {
        chain.push((
            format!(
                "! {} alert{}",
                app.alerts,
                if app.alerts == 1 { "" } else { "s" }
            ),
            fg(theme.health_red),
            theme.health_red,
        ));
    }
    // Pending-dispatch countdown — operator just authorised an action
    // and is in the 5s cancel window. Red bg so the operator catches
    // it peripherally; the 100ms anim ticker re-renders the second
    // digit each frame so the countdown is smooth.
    if let Some(pd) = app.pending_dispatch.as_ref() {
        let now = std::time::Instant::now();
        let remaining = pd.deadline.saturating_duration_since(now).as_secs() + 1;
        chain.push((
            format!("{} {}s — U undo", pd.label, remaining),
            fg(theme.health_red),
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
        chain.push((
            format!(
                "{}{}",
                pending_glyph(theme),
                summarize_in_flight(&in_flight)
            ),
            fg(theme.health_yellow),
            theme.health_yellow,
        ));
    }
    // Multi-select active — surface persistently so the operator can't
    // accidentally fan a destructive action across N envs after wandering
    // off (the status-message hint disappears after one refresh tick).
    let n_selected = app.multi_selected.len();
    if n_selected > 0 {
        chain.push((
            format!("{}{n_selected} selected", multi_select_glyph(theme)),
            fg(theme.title),
            theme.title,
        ));
    }
    if app.read_only {
        chain.push((
            "READ-ONLY".into(),
            fg(theme.health_green),
            theme.health_green,
        ));
    }
    if let Some(release) = app.update_available.as_ref() {
        chain.push((
            format!("UPDATE {} (:update)", release.version),
            fg(theme.title_alt),
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
            chain.push((label, fg(bg), bg));
        }
    }
    if app.frozen {
        // Frozen auto-refresh during an incident is operationally
        // important to not forget about. After 5 minutes of staleness
        // the FROZEN pill turns yellow so the operator sees they're
        // looking at old data while they were heads-down on something
        // else. Grey-on-grey while it's fresh, warning colour after.
        let stale = app
            .last_refresh
            .map(|t| chrono::Utc::now().signed_duration_since(t) >= chrono::Duration::minutes(5))
            .unwrap_or(false);
        let bg = if stale {
            theme.health_yellow
        } else {
            theme.health_grey
        };
        let label = if stale {
            "FROZEN (stale)".to_string()
        } else {
            "FROZEN".to_string()
        };
        chain.push((label, fg(bg), bg));
    }
    if app.redact {
        chain.push((
            "REDACT".into(),
            fg(theme.health_yellow),
            theme.health_yellow,
        ));
    }
    if app.grouped {
        chain.push(("GROUPED".into(), fg(theme.title_alt), theme.title_alt));
    }
    match app.view_mode {
        ViewMode::Compact => {
            chain.push(("COMPACT".into(), fg(theme.accent), theme.accent));
        }
        ViewMode::Spacious => {
            chain.push(("SPACIOUS".into(), fg(theme.accent), theme.accent));
        }
        ViewMode::Default => {}
    }
    chain
}

/// Glyph for the pending-actions pill, gated on the active icon style.
/// `⏳` (U+23F3) is unicode-only — operators on `icons = "ascii"`
/// terminals saw box-tofu before this; falls back to a `*` tag now.
fn pending_glyph(theme: &Theme) -> &'static str {
    match theme.icons {
        IconStyle::Ascii => "* ",
        _ => "⏳ ",
    }
}

/// Glyph for the multi-select-active pill.
fn multi_select_glyph(theme: &Theme) -> &'static str {
    match theme.icons {
        IconStyle::Ascii => "+ ",
        _ => "▶ ",
    }
}

/// Decides the header's vertical footprint and whether the contextual pill
/// chain fits on the info row (`line2`) at this terminal width. When the
/// chain fits, the dedicated 4th row is dropped to save vertical space.
///
/// Returns `(header_rows, merge_pills)`.
fn header_layout(app: &App, area_width: u16) -> (u16, bool) {
    // Header's left column is Constraint::Percentage(60) of `area`; the
    // titled_block adds one column of padding on each side.
    let col0 = (area_width as u32 * 60 / 100) as u16;
    let inner = col0.saturating_sub(2) as usize;

    let mut pills = build_chain_pills(app);
    prune_pills_to_width(&mut pills, &app.theme, inner);
    let chain_spans = pill_chain(&pills, &app.theme);
    let chain_w: usize = chain_spans.iter().map(|s| s.width()).sum();
    let info_w = estimated_info_row_width(app);

    header_dimensions(info_w, chain_w, inner, !app.named_filters.is_empty())
}

/// Drops trailing (low-priority) pills from `pills` until the rendered
/// width fits in `max_w`. `build_chain_pills` orders pills by priority
/// (most operationally critical first — alerts, pending, multi-select,
/// read-only, update; least — view-mode, grouped, redact), so trimming
/// from the end strips the cosmetic chips first while preserving the
/// "you have something serious going on" pills. Mutates in place.
///
/// When pills do get elided, the last surviving pill is appended with a
/// `+N` suffix so the operator knows pills are hidden — silent elision
/// would be worse than overflow.
fn prune_pills_to_width(pills: &mut Vec<(String, Color, Color)>, theme: &Theme, max_w: usize) {
    if pills.is_empty() {
        return;
    }
    let measure = |slice: &[(String, Color, Color)]| -> usize {
        pill_chain(slice, theme).iter().map(|s| s.width()).sum()
    };
    let original_len = pills.len();
    while pills.len() > 1 && measure(pills) > max_w {
        pills.pop();
    }
    if pills.len() < original_len {
        // Mark the last visible pill so the operator knows there's more.
        let hidden = original_len - pills.len();
        if let Some(last) = pills.last_mut() {
            last.0 = format!("{} +{hidden}", last.0);
        }
    }
}

/// Pure width math behind `header_layout`. Given the rendered width of the
/// info row, the rendered width of the pill chain (0 when no pills are
/// active), the inner column width, and whether the saved-filter chip bar
/// is shown, returns `(header_rows, merge_pills)`.
fn header_dimensions(
    info_row_w: usize,
    chain_w: usize,
    inner_w: usize,
    has_filters: bool,
) -> (u16, bool) {
    // Two-space gap between info row and pill chain on the merged line.
    let gap = 2usize;
    let pills_present = chain_w > 0;
    let merge_pills = pills_present && info_row_w + gap + chain_w <= inner_w;
    let pill_row = pills_present && !merge_pills;
    // 2 block borders + crumb + line1 + line2 + optional pill + optional filter
    let rows = 2 + 3 + (if pill_row { 1 } else { 0 }) + (if has_filters { 1 } else { 0 });
    (rows as u16, merge_pills)
}

/// Estimates the rendered width of the info row (`line2`) — Sort · Status ·
/// Envs · Last · Caller · (Filter). Mirrors the construction in
/// `draw_header`; the status spinner is fixed at `STATUS_SLOT` columns so
/// width is stable across spinner phases.
fn estimated_info_row_width(app: &App) -> usize {
    const STATUS_SLOT: usize = 10;
    let sep_w = 5; // both "  •  " and "  ❘  " render at 5 cols
    let sort_dir = if app.sort_desc { "↓" } else { "↑" };
    let sort_label = format!("{}{}", app.sort_key.label(), sort_dir);
    let env_count = app.environments.len().to_string();
    let caller = redact(
        &app.context
            .caller_arn
            .as_deref()
            .map(short_caller)
            .unwrap_or_else(|| "—".into()),
        app.redact,
    );
    let last = format_refresh_label(app.last_refresh, chrono::Utc::now(), app.refresh_interval);

    let mut w = "Sort: ".chars().count() + sort_label.chars().count();
    w += sep_w + "Status: ".chars().count() + STATUS_SLOT;
    w += sep_w + "Envs: ".chars().count() + env_count.chars().count();
    for (bucket, delta) in app.health_delta.iter().chain(app.status_delta.iter()) {
        if *delta == 0 {
            continue;
        }
        // " ▲N Bucket"
        w += 1 + 1 + delta.abs().to_string().chars().count() + 1 + bucket.chars().count();
    }
    w += sep_w + "Last: ".chars().count() + last.chars().count();
    w += sep_w + "Caller: ".chars().count() + caller.chars().count();
    if !app.filter.is_empty() {
        w += sep_w + "Filter: ".chars().count() + app.filter.chars().count();
    }
    w
}

fn health_dot(health: &str, theme: &Theme) -> Span<'static> {
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

const SPARKLINE_WIDTH: usize = 10;
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
        // Header rows: crumb + line1 (Account/Region/Profile) + line2
        // (Sort/Status/Envs/Last/Caller/Filter) + chain (alerts/redact/sso/
        // etc.) + optional filter-chip row. At wide-enough terminals the
        // chain merges onto line2 — `header_layout` decides per-frame.
        let (header_height, merge_pills) = header_layout(app, f.area().width);
        let mut constraints: Vec<Constraint> =
            vec![Constraint::Length(header_height), Constraint::Min(3)];
        if events_height > 0 {
            constraints.push(Constraint::Length(events_height));
        }
        // Footer is 2 rows normally (status row + key strip); the
        // first-run nudge inserts a third row above so adopters
        // see the discovery hints without the existing layout
        // shifting around once they dismiss.
        let footer_height: u16 = if app.first_run_hint { 3 } else { 2 };
        constraints.push(Constraint::Length(footer_height));

        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints(constraints)
            .split(f.area());

        draw_header(f, chunks[0], app, merge_pills);
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
    } else {
        // Reset the cached max so a stale value doesn't survive across
        // hides; the next help open will recompute it on the first frame.
        app.help_max_scroll = 0;
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
            Overlay::WhyRed { .. } => draw_why_red_overlay(f, f.area(), app),
            Overlay::AppsActionMenu {
                app_name,
                env_names,
                cursor,
            } => draw_apps_action_menu(f, f.area(), app, &app_name, &env_names, cursor),
            Overlay::ReportBug { body } => draw_report_bug_overlay(f, f.area(), app, &body),
            Overlay::About(opened) => draw_about(f, f.area(), app, opened),
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
                FieldKind::MultiSelect { options } => {
                    // Value row shows a one-line summary; the full option
                    // list is rendered below on its own lines.
                    let n_selected = crate::form::parse_multi_value(&fld.value).len();
                    format!("({n_selected} / {} selected)", options.len())
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
            // MultiSelect: render the full option list below the value
            // summary. Each row shows `[x] {opt}` or `[ ] {opt}`; if the
            // field carries `option_annotations`, the matching entry is
            // appended in muted text on the same line. The row at
            // `option_cursor` gets the same row_selected_bg treatment
            // the table uses for the focused row.
            if let FieldKind::MultiSelect { options } = &fld.kind {
                let annotations = fld.option_annotations.as_deref();
                for (idx, opt) in options.iter().enumerate() {
                    let selected = crate::form::is_multi_selected(&fld.value, opt);
                    let marker = if selected { "[x]" } else { "[ ]" };
                    let row_is_cursor = is_cursor && idx == fld.option_cursor;
                    let row_style = if row_is_cursor {
                        Style::default().fg(theme.text).bg(theme.row_selected_bg)
                    } else if selected {
                        Style::default()
                            .fg(theme.title_alt)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.text)
                    };
                    let annot = annotations
                        .and_then(|a| a.get(idx))
                        .map(|s| s.as_str())
                        .unwrap_or("");
                    let row_spans = if annot.is_empty() {
                        vec![Span::styled(format!("     {marker} {opt}"), row_style)]
                    } else {
                        vec![
                            Span::styled(format!("     {marker} {opt}  "), row_style),
                            Span::styled(annot.to_string(), Style::default().fg(theme.muted)),
                        ]
                    };
                    lines.push(Line::from(row_spans));
                }
            }
            if let Some(help) = &fld.help {
                lines.push(Line::from(Span::styled(
                    format!("     {help}"),
                    Style::default().fg(theme.muted),
                )));
            }
            if let Some(err) = &fld.error {
                lines.push(Line::from(Span::styled(
                    format!("     {}{err}", warn_glyph(theme.icons)),
                    Style::default().fg(theme.health_red),
                )));
            }
        }
        f.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), chunks[2]);
    }
    let footer = match form.state {
        FormState::Loading => " esc to cancel",
        FormState::Submitting => " submitting…",
        FormState::Ready => " tab field · ↓↑ field-or-option · type to edit · space toggle · ←→ cycle select · ^S submit · esc cancel",
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
                stripe_glyph(theme.icons),
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
        format!(" {}{err}", warn_glyph(theme.icons))
    } else {
        " j/k scroll · g/G top/follow · / filter · n clear-filter · Tab change group · esc / q close".to_string()
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

/// Rendered width of the giant scene (30 art pixels × 2 cells).
const ABOUT_SCENE_W: u16 = 60;
/// Width budget for the `:about` project-text block.
const ABOUT_TEXT_W: u16 = 58;

/// Which of the three `:about` layouts to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AboutLayout {
    /// Scene above the text — roomy terminal.
    Stacked,
    /// Scene left, text right — wide but short.
    SideBySide,
    /// No scene — small terminal.
    TextOnly,
}

/// Pure: pick the `:about` layout for a `w`×`h` terminal given the
/// project-text block height `text_h`. The `+6` / `+8` budgets
/// cover the bordered block, padding, and ~2 rows/cols of slack so
/// content never butts against the card edge.
fn about_layout(w: u16, h: u16, text_h: u16) -> AboutLayout {
    let scene_h = crate::GIANT_SCENE_ROWS as u16;
    if w >= ABOUT_SCENE_W + 6 && h >= scene_h + text_h + 6 {
        AboutLayout::Stacked
    } else if w >= ABOUT_SCENE_W + ABOUT_TEXT_W + 8 && h >= scene_h + 4 {
        AboutLayout::SideBySide
    } else {
        AboutLayout::TextOnly
    }
}

/// `:about` overlay — the project card with the animated 8-bit
/// angry-giant-eats-the-beanstalk scene. The animation frame is
/// derived from `opened.elapsed()` (the `anim` ticker wakes the
/// draw loop while this overlay is up).
///
/// Three responsive layouts pick themselves from the terminal size:
/// **stacked** (scene above text, roomy terminal), **side-by-side**
/// (scene left, text right — wide but short), or **text-only** (no
/// scene, small terminal). The popup is sized to the layout chosen.
fn draw_about(f: &mut Frame, area: Rect, app: &App, opened: std::time::Instant) {
    let theme = &app.theme;
    let frame = (opened.elapsed().as_millis() / 30) as u64;

    // Project text block.
    let title_style = Style::default()
        .fg(theme.title)
        .add_modifier(Modifier::BOLD);
    let muted = Style::default().fg(theme.muted);
    let text = Style::default().fg(theme.text);
    let accent = Style::default().fg(theme.title_alt);
    let centered = |span: Span<'static>| Line::from(span).alignment(Alignment::Center);
    let mut text_lines: Vec<Line> = Vec::new();
    text_lines.push(centered(Span::styled(
        format!("ebman {}", env!("CARGO_PKG_VERSION")),
        title_style,
    )));
    text_lines.push(centered(Span::styled(
        "k9s-style TUI for AWS Elastic Beanstalk".to_string(),
        muted,
    )));
    text_lines.push(Line::from(""));
    text_lines.push(centered(Span::styled(
        "Built by Tom Baldwin · Polymorphism Ltd".to_string(),
        accent,
    )));
    text_lines.push(centered(Span::styled(
        "https://polymorphism.co.uk".to_string(),
        muted,
    )));
    text_lines.push(Line::from(""));
    for row in [
        "Source:   https://github.com/tombaldwin/ebman",
        "License:  MIT OR Apache-2.0",
        "Crates:   https://crates.io/crates/ebman",
    ] {
        text_lines.push(centered(Span::styled(row.to_string(), text)));
    }
    text_lines.push(Line::from(""));
    for row in [
        "Polymorphism Ltd builds operations tools for teams",
        "running EB / ECS / Lambda at scale. Hire us, fork",
        "the code, or tell us what's missing — happy either way.",
    ] {
        text_lines.push(centered(Span::styled(row.to_string(), muted)));
    }
    text_lines.push(Line::from(""));
    text_lines.push(centered(Span::styled(
        "esc / q to close".to_string(),
        muted,
    )));

    // Pick a layout for the terminal, then size the popup to match.
    let scene_h = crate::GIANT_SCENE_ROWS as u16;
    let text_h = text_lines.len() as u16;
    let layout = about_layout(area.width, area.height, text_h);
    let (pw, ph) = match layout {
        AboutLayout::Stacked => (ABOUT_SCENE_W + 6, scene_h + text_h + 6),
        AboutLayout::SideBySide => (ABOUT_SCENE_W + ABOUT_TEXT_W + 8, scene_h + 4),
        AboutLayout::TextOnly => (ABOUT_TEXT_W + 6, text_h + 4),
    };
    let pw = pw.min(area.width);
    let ph = ph.min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(pw)) / 2,
        y: area.y + (area.height.saturating_sub(ph)) / 2,
        width: pw,
        height: ph,
    };
    f.render_widget(Clear, popup);
    let outer = titled_block(&app.theme, "about ebman", true, app.theme.title)
        .padding(Padding::horizontal(1));
    let inner = outer.inner(popup);
    f.render_widget(outer, popup);

    match layout {
        AboutLayout::Stacked => {
            let mut all: Vec<Line> = vec![Line::from("")];
            all.extend(crate::splash_giant_lines(frame));
            all.push(Line::from(""));
            all.extend(text_lines);
            f.render_widget(Paragraph::new(all), inner);
        }
        AboutLayout::SideBySide => {
            let cols = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([
                    Constraint::Length(ABOUT_SCENE_W),
                    Constraint::Length(2),
                    Constraint::Min(0),
                ])
                .split(inner);
            // Scene with a one-row margin above; the column is sized
            // one row taller than the scene, so a row also falls below.
            let mut scene = vec![Line::from("")];
            scene.extend(crate::splash_giant_lines(frame));
            f.render_widget(Paragraph::new(scene), cols[0]);
            // Text vertically centred in the same column height.
            let mut col_text: Vec<Line> = Vec::new();
            let pad = (cols[2].height.saturating_sub(text_h) / 2) as usize;
            col_text.extend(std::iter::repeat_with(|| Line::from("")).take(pad));
            col_text.extend(text_lines);
            f.render_widget(Paragraph::new(col_text), cols[2]);
        }
        AboutLayout::TextOnly => {
            let mut all: Vec<Line> = vec![Line::from("")];
            all.extend(text_lines);
            f.render_widget(Paragraph::new(all), inner);
        }
    }
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

fn draw_why_red_overlay(f: &mut Frame, area: Rect, app: &App) {
    let Some(crate::app::Overlay::WhyRed {
        env_name,
        tier,
        events,
        alarms,
        instances,
        deploys,
        queues,
        dlq_messages,
        ..
    }) = app.current_overlay.as_ref()
    else {
        return;
    };
    let is_worker = tier.eq_ignore_ascii_case("Worker");
    let popup = centered_rect(78, 80, area);
    f.render_widget(Clear, popup);
    let theme = &app.theme;
    let now = chrono::Utc::now();
    let mut lines: Vec<Line> = Vec::new();
    let section = |title: &str| -> Line<'static> {
        Line::from(Span::styled(
            format!("─── {title} "),
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let blank = || Line::raw("");
    let muted = |s: String| -> Line<'static> {
        Line::from(Span::styled(s, Style::default().fg(theme.muted)))
    };

    // 1. RECENT EVENTS (last 30 minutes — the window where "what went
    // wrong" usually shows up; older events are noise during triage).
    lines.push(section("recent events (last 30 min)"));
    match events {
        None => lines.push(muted(" fetching events…".into())),
        Some(Err(e)) => lines.push(Line::from(Span::styled(
            format!(" error: {e}"),
            Style::default().fg(theme.health_red),
        ))),
        Some(Ok(evs)) => {
            let cutoff = now - chrono::Duration::minutes(30);
            let recent: Vec<&crate::aws::Event> = evs
                .iter()
                .filter(|e| e.at.map(|t| t >= cutoff).unwrap_or(true))
                .take(15)
                .collect();
            if recent.is_empty() {
                lines.push(muted(" (no events in the last 30 min)".into()));
            } else {
                for e in recent {
                    let when =
                        e.at.map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
                            .unwrap_or_else(|| "??:??".into());
                    let sev_style = match e.severity.to_uppercase().as_str() {
                        "ERROR" => Style::default().fg(theme.health_red),
                        "WARN" => Style::default().fg(theme.health_yellow),
                        _ => Style::default().fg(theme.muted),
                    };
                    lines.push(Line::from(vec![
                        Span::styled(format!(" {when}  "), Style::default().fg(theme.muted)),
                        Span::styled(format!("{:<5}", e.severity), sev_style),
                        Span::raw("  "),
                        Span::styled(e.message.clone(), Style::default().fg(theme.text)),
                    ]));
                }
            }
        }
    }
    lines.push(blank());

    // 2. ALARMS — ALARM-state ones first (red), then INSUFFICIENT_DATA
    // (yellow), then OK (green/muted). Operator wants to scan for active
    // alarms; OK alarms confirm what *isn't* the problem.
    lines.push(section("alarms"));
    match alarms {
        None => lines.push(muted(" fetching alarms…".into())),
        Some(Err(e)) => lines.push(Line::from(Span::styled(
            format!(" error: {e}"),
            Style::default().fg(theme.health_red),
        ))),
        Some(Ok(als)) => {
            if als.is_empty() {
                lines.push(muted(" (no CloudWatch alarms attached to this env)".into()));
            } else {
                // Active first
                let mut sorted: Vec<&crate::aws::CwAlarm> = als.iter().collect();
                sorted.sort_by_key(|a| match a.state.as_str() {
                    "ALARM" => 0,
                    "INSUFFICIENT_DATA" => 1,
                    _ => 2,
                });
                for a in sorted.iter().take(10) {
                    let (tag, style) = match a.state.as_str() {
                        "ALARM" => (
                            "ALARM",
                            Style::default()
                                .fg(theme.health_red)
                                .add_modifier(Modifier::BOLD),
                        ),
                        "OK" => ("OK   ", Style::default().fg(theme.health_green)),
                        _ => ("INS  ", Style::default().fg(theme.muted)),
                    };
                    lines.push(Line::from(vec![
                        Span::raw(" "),
                        Span::styled(tag.to_string(), style),
                        Span::raw("  "),
                        Span::styled(
                            a.name.clone(),
                            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            format!("  ({}/{})", a.namespace, a.metric_name),
                            Style::default().fg(theme.muted),
                        ),
                    ]));
                    if !a.state_reason.is_empty() && a.state == "ALARM" {
                        lines.push(Line::from(Span::styled(
                            format!("   ↳ {}", a.state_reason),
                            Style::default().fg(theme.muted),
                        )));
                    }
                }
            }
        }
    }
    lines.push(blank());

    // 2.5 WORKER QUEUES — only rendered for Worker-tier envs. Surfaces
    // main + DLQ depths and a peek of DLQ message bodies so the operator
    // sees why the row went Red without leaving the overlay. Hidden
    // entirely for Web envs.
    if is_worker {
        lines.push(section("worker queues"));
        match queues {
            None => lines.push(muted(" fetching queue depths…".into())),
            Some(Err(e)) => lines.push(Line::from(Span::styled(
                format!(" error: {e}"),
                Style::default().fg(theme.health_red),
            ))),
            Some(Ok(q)) => {
                let main_line = match q.main_stats.as_ref() {
                    Some(s) => format!(
                        " main:  visible={}  in-flight={}  delayed={}",
                        s.visible, s.in_flight, s.delayed
                    ),
                    None => " main:  (queue URL not resolved)".to_string(),
                };
                let main_style = match q.main_stats.as_ref().map(|s| s.visible).unwrap_or(0) {
                    n if n > 100 => Style::default().fg(theme.health_yellow),
                    _ => Style::default().fg(theme.text),
                };
                lines.push(Line::from(Span::styled(main_line, main_style)));
                let dlq_visible = q.dlq_stats.as_ref().map(|s| s.visible).unwrap_or(0);
                let dlq_line = match q.dlq_stats.as_ref() {
                    Some(s) => format!(
                        " dlq:   visible={}  in-flight={}  delayed={}",
                        s.visible, s.in_flight, s.delayed
                    ),
                    None => " dlq:   (queue URL not resolved)".to_string(),
                };
                let dlq_style = if dlq_visible > 0 {
                    Style::default()
                        .fg(theme.health_red)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.text)
                };
                lines.push(Line::from(Span::styled(dlq_line, dlq_style)));
                // DLQ peek — only renders when there's something to peek
                // at. Empty result = "DLQ is clean" (no header line);
                // non-empty = bodies truncated to one screen-line each.
                if dlq_visible > 0 {
                    match dlq_messages {
                        None => lines.push(muted(" peeking dlq messages…".into())),
                        Some(Err(e)) => lines.push(Line::from(Span::styled(
                            format!(" dlq peek error: {e}"),
                            Style::default().fg(theme.health_red),
                        ))),
                        Some(Ok(msgs)) if msgs.is_empty() => {
                            // DLQ has visible messages but the peek
                            // returned empty — likely the messages are
                            // mid-visibility-timeout from another peek.
                            lines.push(muted(
                                " dlq peek returned no bodies (try again in a few seconds)".into(),
                            ));
                        }
                        Some(Ok(msgs)) => {
                            lines.push(Line::from(Span::styled(
                                format!(" dlq message peek ({} of {dlq_visible}):", msgs.len()),
                                Style::default().fg(theme.muted),
                            )));
                            for (i, m) in msgs.iter().enumerate() {
                                let when = m
                                    .sent_at
                                    .map(|t| humanize_age(now.signed_duration_since(t)))
                                    .unwrap_or_else(|| "—".into());
                                lines.push(Line::from(vec![
                                    Span::styled(
                                        format!("   {}.", i + 1),
                                        Style::default().fg(theme.muted),
                                    ),
                                    Span::styled(
                                        format!(" sent {when} ago"),
                                        Style::default().fg(theme.muted),
                                    ),
                                    Span::styled(
                                        format!("  · received {}×", m.receive_count),
                                        Style::default().fg(theme.muted),
                                    ),
                                ]));
                                lines.push(Line::from(Span::styled(
                                    format!("      {}", truncate_for_display(&m.body, 100)),
                                    Style::default().fg(theme.text),
                                )));
                            }
                        }
                    }
                }
            }
        }
        lines.push(blank());
    }

    // 3. INSTANCE HEALTH — list each instance with its health colour +
    // causes. Severe / Warning rows pull the operator's eye first.
    lines.push(section("instance health"));
    match instances {
        None => lines.push(muted(" fetching instance health…".into())),
        Some(Err(e)) => lines.push(Line::from(Span::styled(
            format!(" error: {e}"),
            Style::default().fg(theme.health_red),
        ))),
        Some(Ok(insts)) => {
            if insts.is_empty() {
                lines.push(muted(" (no instances reported)".into()));
            } else {
                for i in insts {
                    let style = match i.color.as_str() {
                        "Red" => Style::default().fg(theme.health_red),
                        "Yellow" => Style::default().fg(theme.health_yellow),
                        "Green" => Style::default().fg(theme.health_green),
                        _ => Style::default().fg(theme.muted),
                    };
                    lines.push(Line::from(vec![
                        Span::raw(" "),
                        Span::styled(i.id.clone(), Style::default().fg(theme.text)),
                        Span::raw("  "),
                        Span::styled(format!("{:<8}", i.health), style),
                        Span::styled(
                            format!("  {}  {}", i.instance_type, i.availability_zone),
                            Style::default().fg(theme.muted),
                        ),
                    ]));
                    for cause in i.causes.iter().take(3) {
                        lines.push(Line::from(Span::styled(
                            format!("   ↳ {cause}"),
                            Style::default().fg(theme.muted),
                        )));
                    }
                }
            }
        }
    }
    lines.push(blank());

    // 4. RECENT DEPLOYS — top 3 versions, newest first. The most-recent
    // deploy is the prime suspect when health flips Red right after.
    lines.push(section("recent deploys"));
    match deploys {
        None => lines.push(muted(" fetching deploys…".into())),
        Some(Err(e)) => lines.push(Line::from(Span::styled(
            format!(" error: {e}"),
            Style::default().fg(theme.health_red),
        ))),
        Some(Ok(vers)) => {
            if vers.is_empty() {
                lines.push(muted(" (no versions registered yet)".into()));
            } else {
                for v in vers.iter().take(5) {
                    let when = v
                        .created
                        .map(|t| humanize_age(now.signed_duration_since(t)))
                        .unwrap_or_else(|| "—".into());
                    let when_style = Style::default().fg(age_color(v.created, now, theme));
                    let mut spans = vec![
                        Span::raw(" "),
                        Span::styled(
                            v.label.clone(),
                            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(format!("  {when} ago"), when_style),
                    ];
                    if !v.description.is_empty() {
                        spans.push(Span::styled(
                            format!("  — {}", truncate_for_display(&v.description, 60)),
                            Style::default().fg(theme.muted),
                        ));
                    }
                    lines.push(Line::from(spans));
                }
            }
        }
    }
    push_close_hint(&mut lines, theme);

    let title = format!("why is {env_name} red?");
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(titled_block(theme, &title, true, theme.title).padding(Padding::uniform(1)));
    f.render_widget(p, popup);
}

fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let prefix: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{prefix}…")
}

/// Apps-scope action overlay. Small centred popup with one row per
/// `AppsActionItem`. Cursor row gets the title-alt accent (matches the
/// SavedConfigsInteractive cursor styling). Footer hint enumerates the
/// keys so the operator doesn't have to read help.
fn draw_apps_action_menu(
    f: &mut Frame,
    area: Rect,
    app: &App,
    app_name: &str,
    env_names: &[String],
    cursor: usize,
) {
    let theme = &app.theme;
    let popup = centered_rect(40, 30, area);
    f.render_widget(Clear, popup);
    let n_envs = env_names.len();
    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(Span::styled(
        format!("application: {app_name}  ·  {n_envs} env(s)"),
        Style::default().fg(theme.muted),
    )));
    lines.push(Line::from(""));
    for (i, item) in crate::app::APPS_ACTION_ITEMS.iter().enumerate() {
        let active = i == cursor;
        let cursor_glyph = if active { cursor_marker(theme) } else { "  " };
        // Inline the env count so the operator sees the blast radius
        // for the destructive batch entries without flipping screens.
        let label = match item {
            crate::app::AppsActionItem::BatchRebuild => {
                format!("Rebuild all {n_envs} env(s)")
            }
            crate::app::AppsActionItem::BatchRestart => {
                format!("Restart all {n_envs} env(s)")
            }
            crate::app::AppsActionItem::BatchDeploy => {
                format!("Deploy version label to all {n_envs} env(s)")
            }
            _ => item.label().to_string(),
        };
        let style = if active {
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.text)
        };
        lines.push(Line::from(vec![
            Span::styled(cursor_glyph.to_string(), style),
            Span::styled(label, style),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        " j/k move · enter dispatch · esc / q cancel",
        Style::default().fg(theme.muted),
    )));
    let title = format!("apps action — {app_name}");
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(titled_block(theme, &title, true, theme.title).padding(Padding::uniform(1)));
    f.render_widget(p, popup);
}

/// Bug-report overlay. Renders the scrubbed payload as a scrollable
/// text dump + a footer key strip advertising the y / b / esc
/// keybinds the operator picks among. Wide popup so long log lines
/// don't reflow into unreadable wrap.
fn draw_report_bug_overlay(f: &mut Frame, area: Rect, app: &App, body: &str) {
    let theme = &app.theme;
    let popup = centered_rect(80, 80, area);
    f.render_widget(Clear, popup);
    let mut lines: Vec<Line<'static>> = body
        .lines()
        .map(|l| {
            // Distinguish section headers (### …) and code-fence rows
            // for at-a-glance scanning. Pure text overlay otherwise.
            if l.starts_with("### ") || l.starts_with("## ") {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default()
                        .fg(theme.title)
                        .add_modifier(Modifier::BOLD),
                ))
            } else if l.starts_with("```") {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default().fg(theme.muted),
                ))
            } else if l.starts_with("<!--") {
                Line::from(Span::styled(
                    l.to_string(),
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::ITALIC),
                ))
            } else {
                Line::from(Span::styled(l.to_string(), Style::default().fg(theme.text)))
            }
        })
        .collect();
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled(
            "  y",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" copy to clipboard   ", Style::default().fg(theme.muted)),
        Span::styled(
            "b",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            " open GitHub issue in browser   ",
            Style::default().fg(theme.muted),
        ),
        Span::styled(
            "esc / q",
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" cancel", Style::default().fg(theme.muted)),
    ]));
    let title = "bug report (scrubbed — review before sending)";
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(titled_block(theme, title, true, theme.title).padding(Padding::uniform(1)));
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

fn draw_header(f: &mut Frame, area: Rect, app: &App, merge_pills: bool) {
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
    let last = format_refresh_label(app.last_refresh, chrono::Utc::now(), app.refresh_interval);
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
    // Ordering on this row matters under width pressure: ratatui clips
    // the right edge when content exceeds the column, so anything the
    // operator needs ALWAYS visible (Sort, Status) goes first. Caller +
    // Last get pushed right so they're the first to clip on narrow
    // terminals — we'd rather lose "20s ago" than lose "↑app".
    let sort_dir = if app.sort_desc { "↓" } else { "↑" };
    let sort_label = format!("{}{}", app.sort_key.label(), sort_dir);
    let mut line2 = kv("Sort", &sort_label, theme);
    line2.push(sep(theme));
    line2.push(Span::raw("Status: "));
    line2.push(status);
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
    line2.extend(kv("Caller", &caller, theme));
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
    // Contextual pill chain — built via `build_chain_pills` so the layout
    // pass (which sizes the header height) can predict whether the chain
    // fits on the info row at this width. Pruned via the same
    // `prune_pills_to_width` pass that `header_layout` ran so the
    // measurements stay consistent.
    let inner_w = (area.width as u32 * 60 / 100) as usize;
    let inner_w = inner_w.saturating_sub(2);
    let mut chain_pills = build_chain_pills(app);
    prune_pills_to_width(&mut chain_pills, theme, inner_w);
    if merge_pills && !chain_pills.is_empty() {
        // Wide window: pills tail the info row. Two-space gap so they
        // don't butt up against the last field (or the Powerline lead-in
        // wedge — see `pill_chain`).
        line2.push(Span::raw("  "));
        line2.extend(pill_chain(&chain_pills, theme));
    }
    let pill_line: Option<Line<'static>> = if merge_pills || chain_pills.is_empty() {
        None
    } else {
        // Narrow window: dedicated row so the chain doesn't get clipped
        // off the right edge — the alert pill would squash mid-stream
        // because ratatui truncates the Paragraph at the column boundary.
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::raw("  "));
        spans.extend(pill_chain(&chain_pills, theme));
        Some(Line::from(spans))
    };

    // Breadcrumb: region / application / env — gives context at a glance.
    let crumb = breadcrumb_line(app);
    // Saved-filter tab bar — only rendered when the user has saved any.
    // Each chip is the filter name; the chip matching the currently-applied
    // filter is highlighted. The user activates with `:f NAME` or the palette.
    let mut paragraph_lines: Vec<Line> = vec![crumb, Line::from(line1), Line::from(line2)];
    if let Some(pl) = pill_line {
        paragraph_lines.push(pl);
    }
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
        [
            "NAME",
            "ENVS",
            "RED",
            "UPDATING",
            "VERSIONS",
            "UPDATED",
            "LATEST",
            "DESCRIPTION",
        ]
        .map(|h| {
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
            // LATEST = "label · 2h ago" once `latest_version_label` lands
            // from the post-Applications fan-out. Until then, show "—" so
            // the column is obviously still loading rather than blank.
            // Age suffix gets the same three-bucket tint as the envs-table
            // AGE column so fresh/stale signals read consistently.
            let latest_cell = match (a.latest_version_label.as_deref(), a.latest_version_created) {
                (Some(label), Some(created)) => Cell::from(Line::from(vec![
                    Span::styled(
                        label.to_string(),
                        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  {}", humanize_age(now.signed_duration_since(created))),
                        Style::default().fg(age_color(Some(created), now, &theme)),
                    ),
                ])),
                (Some(label), None) => Cell::from(Span::styled(
                    label.to_string(),
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                )),
                _ => Cell::from(Span::styled("—", Style::default().fg(theme.muted))),
            };
            // Operational rollup — env count + Red / Updating buckets.
            // Pulls from the global env list via `app_rollup` so the
            // numbers move with the same 15s ticker as the envs table.
            let rollup = crate::app::app_rollup(&app.environments, &a.name, &app.worker_dlq_depths);
            let red_style = if rollup.red_count > 0 {
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            let updating_style = if rollup.updating_count > 0 {
                Style::default()
                    .fg(theme.status_updating)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            // Red column merges "EB-side Red" with the worker-DLQ alert
            // so an env where EB reports Ready but the DLQ is filling
            // up still counts — same rule as the env-table status pill.
            let total_alerting = rollup.red_count + rollup.worker_dlq_alerts;
            // Per-row affordances: pin glyph (★), multi-select marker
            // (▶), or two-space gutter. Cursor row picks up the table's
            // row_highlight_style — both can coexist.
            let pinned = app.pinned_apps.contains(&a.name);
            let selected = app.apps_selected.contains(&a.name);
            let prefix = if pinned {
                Span::styled(
                    "★ ",
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )
            } else if selected {
                Span::styled(
                    "▶ ",
                    Style::default()
                        .fg(theme.title_alt)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::raw("  ")
            };
            let name_cell = Cell::from(Line::from(vec![
                prefix,
                Span::styled(
                    a.name.clone(),
                    Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                ),
            ]));
            let r = Row::new(vec![
                name_cell,
                Cell::from(rollup.env_count.to_string())
                    .style(Style::default().fg(theme.text).add_modifier(Modifier::BOLD)),
                Cell::from(total_alerting.to_string()).style(red_style),
                Cell::from(rollup.updating_count.to_string()).style(updating_style),
                Cell::from(a.version_count.to_string())
                    .style(Style::default().fg(theme.app_palette[0])),
                Cell::from(age(a.date_updated)).style(Style::default().fg(age_color(
                    a.date_updated,
                    now,
                    &theme,
                ))),
                latest_cell,
                Cell::from(a.description.clone()).style(Style::default().fg(theme.text)),
            ]);
            // Selection bg is layered on by Table::row_highlight_style;
            // apply zebra striping here. Multi-selected apps get the
            // accent bg so the operator catches them peripherally
            // without losing the cursor highlight on the active row.
            // Even-row zebra striping otherwise; odd-rows pass through.
            if selected {
                r.style(Style::default().bg(theme.row_selected_bg))
            } else if i % 2 == 0 {
                r.style(Style::default().bg(theme.row_alt_bg))
            } else {
                r
            }
        })
        .collect();
    let title = format!("Applications  {}", app.applications.len());
    let widths = [
        Constraint::Percentage(20),
        Constraint::Length(5),      // ENVS
        Constraint::Length(4),      // RED
        Constraint::Length(9),      // UPDATING
        Constraint::Length(8),      // VERSIONS
        Constraint::Length(8),      // UPDATED
        Constraint::Percentage(22), // LATEST
        Constraint::Percentage(28), // DESCRIPTION
    ];
    let popup_open = matches!(
        app.mode,
        Mode::Help | Mode::Picker | Mode::Command | Mode::Action | Mode::Filter
    );
    let table = Table::new(rows, widths)
        .header(header)
        .row_highlight_style(
            // See `draw_table` row_highlight_style — REVERSED preserves
            // pill contrast better than a flat bg override.
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
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
    // COST column opt-in via `:cost on`. Inserted before AGE so the
    // expensive envs catch the eye on the same horizontal band as the
    // stale-env tint.
    if app.cost_enabled {
        let age_idx = full
            .iter()
            .position(|(l, _)| *l == "AGE")
            .unwrap_or(full.len());
        full.insert(age_idx, ("COST", SortKey::Name));
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
                        // Application / platform / region values live on
                        // `app.environments[i]` which outlives the draw
                        // call — borrow rather than clone so the per-row
                        // hot path doesn't allocate 3+ Strings per frame.
                        "APPLICATION" => Cell::from(Span::raw(e.application.as_str()))
                            .style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
                        "TIER" => tier_cell(&e.tier, &theme),
                        "STATUS" => {
                            // For Worker envs with DLQ messages, append a
                            // small `⚠N` suffix to the status pill so the
                            // operator can spot the reason a Green-EB row
                            // is tinted red. STATUS column is 10 cells;
                            // " Ready " pill takes 7, leaving room for
                            // " ⚠N" (3 cells). Larger DLQ counts clip
                            // gracefully — the row tint is the primary
                            // signal anyway.
                            let dlq = if e.tier.eq_ignore_ascii_case("Worker") {
                                app.worker_dlq_depths.get(&e.name).copied().unwrap_or(0)
                            } else {
                                0
                            };
                            // When the env is otherwise alerting (Red /
                            // Severe health, or worker with DLQ > 0), mute
                            // the `Ready` pill — `Ready` means "no
                            // lifecycle op in flight", NOT "everything's
                            // fine". A bright green pill on a Red-tinted
                            // row competes with the actual alert signals.
                            let alerting = dlq > 0
                                || e.health.eq_ignore_ascii_case("Red")
                                || e.health.eq_ignore_ascii_case("Severe");
                            if dlq > 0 {
                                Cell::from(Line::from(vec![
                                    status_pill_for(&e.status, &theme, alerting),
                                    Span::styled(
                                        format!(" {}{dlq}", warn_glyph(theme.icons).trim_end()),
                                        Style::default()
                                            .fg(theme.health_red)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]))
                            } else {
                                Cell::from(status_pill_for(&e.status, &theme, alerting))
                            }
                        }
                        "HEALTH" => Cell::from(health_dot(&e.health, &theme)),
                        "TREND" => Cell::from(sparkline_for(
                            app.history.get(&e.name),
                            &theme,
                            app.newly_red.contains(&e.name),
                        )),
                        "PLATFORM" => {
                            // Devicons icon is Powerline-only (PUA
                            // codepoints tofu without a Nerd Font);
                            // colour-coding applies in every icon mode
                            // so unicode / ASCII users still get the
                            // visual differentiation between platforms.
                            let style = platform_style(&e.platform);
                            let colour = style
                                .as_ref()
                                .and_then(|s| theme.app_palette.get(s.palette_idx).copied())
                                .unwrap_or(theme.muted);
                            let icon = if theme.icons == IconStyle::Powerline {
                                style.as_ref().map(|s| s.icon)
                            } else {
                                None
                            };
                            match icon {
                                Some(g) => Cell::from(Line::from(vec![
                                    Span::styled(format!("{g} "), Style::default().fg(colour)),
                                    Span::styled(e.platform.as_str(), Style::default().fg(colour)),
                                ])),
                                None => Cell::from(Span::raw(e.platform.as_str()))
                                    .style(Style::default().fg(colour)),
                            }
                        }
                        "VERSION" => Cell::from(Span::raw(e.version_label.as_str()))
                            .style(Style::default().fg(theme.app_palette[0])),
                        "CNAME" => Cell::from(redact(&e.cname, app.redact))
                            .style(Style::default().fg(theme.muted)),
                        // `age` is built freshly per row inside this scope
                        // and so can't be borrowed into the returned Cell.
                        // Caching it on rebuild_view would let this be a
                        // borrow too, but the age string changes per
                        // minute boundary — a stale value would be
                        // visible until the next refresh, which is fine
                        // operationally but adds bookkeeping. Leave the
                        // single per-row clone here as the cheapest
                        // honest option for now.
                        "AGE" => Cell::from(age.clone())
                            .style(Style::default().fg(age_color(e.updated, now, &theme))),
                        "REGION" => Cell::from(Span::raw(e.region.as_deref().unwrap_or_default()))
                            .style(Style::default().fg(theme.accent)),
                        "COST" => {
                            // `:cost on` populates `app.costs` from
                            // Cost Explorer (Tag: elasticbeanstalk:env-name).
                            // Display as `$NNN` (no fractional cents —
                            // the precision is misleading; Cost Explorer
                            // reports `1240.503125...` and that's noise).
                            // Tint cells by bucket so the eye lands on
                            // the expensive ones: green < $50, muted
                            // $50–$500, red ≥ $500.
                            match app.costs.get(&e.name).copied() {
                                Some(cost) => {
                                    let text = format!("${cost:.0}");
                                    let fg = if cost >= 500.0 {
                                        theme.health_red
                                    } else if cost >= 50.0 {
                                        theme.text
                                    } else {
                                        theme.health_green
                                    };
                                    Cell::from(text)
                                        .style(Style::default().fg(fg).add_modifier(Modifier::BOLD))
                                }
                                None => {
                                    Cell::from(Span::styled("—", Style::default().fg(theme.muted)))
                                }
                            }
                        }
                        _ => Cell::from(""),
                    })
                    .collect();

                // Row tint priority: severity > hover > zebra. Selection is
                // handled by Table::row_highlight_style so it overlays cleanly.
                let is_hover = hover == Some(row_idx);
                // Worker envs with DLQ messages tint the row Red even
                // when EB reports Green/Yellow — failed jobs sitting in
                // the dead-letter queue are an operational red flag the
                // EB health check doesn't model.
                let dlq_red = e.tier.eq_ignore_ascii_case("Worker")
                    && app.worker_dlq_depths.get(&e.name).copied().unwrap_or(0) > 0;
                let bg = if dlq_red
                    || e.health.eq_ignore_ascii_case("Red")
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
                } else if !next_app_name.is_empty() {
                    // Non-Powerline path: previously rendered every cell as
                    // dashes (200×─), so the banner read as a homogeneous
                    // line with no app name and no break. Now: NAME cell
                    // gets `── ▶ app ──`, second cell carries the summary,
                    // remaining cells stay as the dash fill so the row
                    // still scans as a visible group divider.
                    let glyph = separator_glyph(theme.icons);
                    let summary_text = summary.clone();
                    let cells: Vec<Cell> = columns
                        .iter()
                        .enumerate()
                        .map(|(i, (label, _))| {
                            if i == 0 && *label == "NAME" {
                                Cell::from(Line::from(vec![
                                    Span::styled(
                                        "── ".to_string(),
                                        Style::default().fg(theme.muted),
                                    ),
                                    Span::styled(
                                        format!("{glyph} "),
                                        Style::default()
                                            .fg(next_color)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(
                                        next_app_name.clone(),
                                        Style::default()
                                            .fg(next_color)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                    Span::styled(
                                        " ──".to_string(),
                                        Style::default().fg(theme.muted),
                                    ),
                                ]))
                            } else if i == 1 {
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
            // 11 fits `" {icon} Worker " + trailing breathing space`
            // exactly (1 pill-pad + 1 icon + 1 sep + 6 label + 1 pill-
            // pad + 1 breathing = 11). Web fills the same width with
            // trailing pad inside the pill so the bg stops at the same
            // column boundary either way.
            "TIER" => Constraint::Length(11),
            "STATUS" => Constraint::Length(10),
            "HEALTH" => Constraint::Length(3),
            "TREND" => Constraint::Length(12),
            "PLATFORM" => Constraint::Percentage(15),
            "VERSION" => Constraint::Percentage(10),
            "CNAME" => Constraint::Percentage(14),
            "AGE" => Constraint::Length(6),
            "COST" => Constraint::Length(8),
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
            // REVERSED swaps fg/bg per terminal cell at render time. This
            // preserves pill contrast on the selected row — pill cells
            // (black fg on yellow/green bg) flip to (yellow/green fg on
            // black bg), which is still readable, whereas overriding bg
            // would mask the pill colour and leave the black fg sitting
            // on the dark row_selected_bg (low contrast).
            Style::default().add_modifier(Modifier::REVERSED | Modifier::BOLD),
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
            heading = "no envs in this account / region".to_string();
            hint = "try a different region (r) or profile (p), or check the AWS console (b)"
                .to_string();
        } else if app.filter.is_empty() {
            heading = "no envs match the active view".to_string();
            hint = "type `:views` to switch back to default, or `:filters` to drop a saved one"
                .to_string();
        } else {
            heading = format!("no envs match  `{}`", app.filter);
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
        let color = health_color(&e.health, theme);
        f.render_widget(
            Paragraph::new(Span::styled("█", Style::default().fg(color))),
            cell,
        );
    }
}

fn tier_cell(tier: &str, theme: &Theme) -> Cell<'static> {
    // Both tiers render as same-shape pills with coloured backgrounds
    // and a trailing un-coloured space so the bg ends *before* the
    // STATUS column starts (otherwise the pill backgrounds bleed into
    // the adjacent column boundary and look cramped). Web uses
    // `theme.title` (the default-primary signal); Worker keeps the
    // accent (yellow) bg since it's the less-common tier and the
    // contrast still calls it out.
    // Left-justify both labels (so "Web" sits at the same position as
    // "Worker") and prefix each with an icon-style-aware glyph. Same
    // pill background dimensions for both — 6-char label padding +
    // 1-cell icon + 1 separator space = 8 inner chars, plus pill's
    // surrounding ` … ` = 10 cells of coloured bg.
    let label_width = "Worker".chars().count();
    let (web_icon, worker_icon) = tier_icons(theme.icons);
    match tier {
        "Worker" => Cell::from(Line::from(vec![
            pill(
                &format!("{worker_icon} {:<label_width$}", "Worker"),
                Color::Black,
                theme.accent,
            ),
            Span::raw(" "),
        ])),
        "Web" => Cell::from(Line::from(vec![
            pill(
                &format!("{web_icon} {:<label_width$}", "Web"),
                Color::Black,
                theme.title,
            ),
            Span::raw(" "),
        ])),
        other => Cell::from(Span::styled(
            other.to_string(),
            Style::default().fg(theme.muted),
        )),
    }
}

/// Per-platform render style: icon + colour palette slot. `None` ⇒
/// "unrecognised, render plain". The palette index is an offset into
/// `theme.app_palette` so the colour automatically adapts to the
/// active theme without a per-theme mapping.
struct PlatformStyle {
    icon: &'static str,
    palette_idx: usize,
}

/// Pure: pick a Devicons glyph + theme palette colour for the env's
/// platform family. The icon is rendered Powerline-only (Devicons
/// codepoints live in the PUA range and tofu without a Nerd Font);
/// the colour applies in every icon mode so unicode / ASCII users
/// still get the visual differentiation.
///
/// Palette indices are stable, low slots so each language sticks to
/// the same hue across refreshes (rather than drifting with the app-
/// colour cache).
///
/// **Caveat:** Devicons codepoints have been stable since Nerd Fonts
/// 1.x, but if any render wrong in the wild (the MDI block burned us
/// before), the fix is to either update the codepoint or return
/// `None` for that family.
fn platform_style(family: &str) -> Option<PlatformStyle> {
    let lc = family.to_ascii_lowercase();
    // Match longest / most-specific tokens first so e.g. "Corretto" is
    // recognised as Java even though it doesn't mention Java.
    let (icon, palette_idx) = if lc.contains("node") {
        ("\u{e718}", 2) // green-teal slot for Node's brand green
    } else if lc.contains("java") || lc.contains("tomcat") || lc.contains("corretto") {
        ("\u{e738}", 3) // tan/orange for Java's coffee
    } else if lc.contains("python") {
        ("\u{e73c}", 0) // blue for Python
    } else if lc.contains("ruby") {
        ("\u{e791}", 5) // pink-red for Ruby
    } else if lc.contains("php") {
        ("\u{e73d}", 6) // purple for PHP
    } else if lc.contains(".net") || lc.contains("iis") {
        ("\u{e77f}", 1) // mauve for .NET
    } else if lc.contains("docker") {
        ("\u{e7b0}", 7) // pale blue for Docker
    } else if lc.contains("go ") || lc.ends_with(" go") || lc == "go" {
        ("\u{e626}", 9) // mint for Go
    } else {
        return None;
    };
    Some(PlatformStyle { icon, palette_idx })
}

/// Returns `(web_icon, worker_icon)` for the given icon style. Picks
/// single-cell glyphs that render predictably without depending on
/// Nerd Font MDI codepoint stability across font versions (an earlier
/// version tried `\u{f0319}` / `\u{f0294}` and got an inbox-tray + an
/// arrow-expand instead of web / wrench).
///
/// Web → `⊕` (circle-plus, reads as a globe/world stand-in); Worker
/// → `⚒` (hammer-and-pick, the universal blue-collar glyph). Both
/// are BMP unicode, single cell in standard monospaced + Powerline
/// fonts. ASCII falls back to letter tags so the pill column still
/// aligns when no decoration is available.
fn tier_icons(icons: IconStyle) -> (&'static str, &'static str) {
    match icons {
        IconStyle::Ascii => ("W", "K"),
        _ => ("⊕", "⚒"),
    }
}

/// Render a status string as a coloured pill. Wrapper around
/// [`status_pill_for`] for callers that don't care about the alerting
/// distinction (Detail header, etc.).
fn status_pill(status: &str, theme: &Theme) -> Span<'static> {
    status_pill_for(status, theme, false)
}

/// Variant of [`status_pill`] that knows whether the env is otherwise
/// alerting (Red health or worker with DLQ > 0). When `muted` is true,
/// the `Ready` pill renders in a dim muted style instead of bright green
/// — `Ready` means "no lifecycle op in flight" per EB, NOT "everything
/// is fine". Muting it stops the green pill from competing with the
/// health-dot / row-tint / `⚠N` chip when the env is actually alerting.
/// Other statuses (Updating / Terminating) are unaffected — they
/// already signal "something happening" and the operator wants the full
/// colour cue.
fn status_pill_for(status: &str, theme: &Theme, muted: bool) -> Span<'static> {
    // Case-insensitive match without allocating a lowercase copy per
    // call — the table renderer hits this once per env-row per frame.
    if status.eq_ignore_ascii_case("ready") {
        if muted {
            // Dimmed text rather than a coloured pill so the eye lands
            // on the alert signals instead of the green pill.
            Span::styled(" Ready ", Style::default().fg(theme.muted))
        } else {
            pill("Ready", Color::Black, theme.status_ready)
        }
    } else if ieq_any(status, &["updating", "launching"]) {
        // Slow blink draws the eye to in-flight lifecycle ops without
        // changing the pill width or colour. Modern terminals (iTerm2,
        // Alacritty, Ghostty, etc.) support it; legacy ones silently
        // ignore the modifier and fall back to a static pill.
        Span::styled(
            format!(" {status} "),
            Style::default()
                .fg(Color::Black)
                .bg(theme.status_updating)
                .add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK),
        )
    } else if ieq_any(status, &["terminating", "terminated"]) {
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
    let tw = event_time_width(app.event_time_format);
    let lines: Vec<Line> = app
        .events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let when = format_event_time(e.at, app.event_time_format, now);
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
                    format!("{when:>tw$} "),
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
    // First-run hint sits ABOVE the regular footer rows when this
    // is the operator's first launch (no `state.toml` on disk).
    // Clears on first input event — the renderer just reads the
    // flag every frame. Adds one row to the footer when present;
    // the layout below stays the same shape otherwise so existing
    // mode-aware logic is untouched.
    let constraints: &[Constraint] = if app.first_run_hint {
        &[
            Constraint::Length(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ]
    } else {
        &[Constraint::Length(1), Constraint::Length(1)]
    };
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints(constraints.to_vec())
        .split(area);
    let (hint_row, top_row, bottom_row) = if app.first_run_hint {
        (Some(rows[0]), rows[1], rows[2])
    } else {
        (None, rows[0], rows[1])
    };

    // First-run hint row — bright accent, single line, dismisses
    // on any input. Wording emphasises the three discovery
    // surfaces an adopter most needs to know about.
    if let Some(area) = hint_row {
        let theme = &app.theme;
        let line = Line::from(vec![
            Span::styled(
                "  ★ ",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "First launch — press ",
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "?",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for help, ", Style::default().fg(theme.title)),
            Span::styled(
                ":",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for commands, ", Style::default().fg(theme.title)),
            Span::styled(
                "Ctrl-K",
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " for fuzzy search.  (any key dismisses)",
                Style::default().fg(theme.title),
            ),
        ]);
        f.render_widget(Paragraph::new(line), area);
    }

    let rows = [top_row, bottom_row];

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
                    format!(" {}{hint}", hint_glyph(theme.icons)),
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

fn draw_help(f: &mut Frame, area: Rect, app: &mut App) {
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
    let mut lines: Vec<Line> = vec![
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
        help_line(
            "a",
            "open actions menu (rebuild / restart / swap / terminate)",
            theme,
        ),
        help_line("b", "open selected env in the AWS console", theme),
        help_line("D", "describe overlay (raw env dump as JSON)", theme),
        help_line(
            "!",
            "diagnose selected env (events + alarms + instances + recent deploys)",
            theme,
        ),
        help_line(
            "U",
            "undo a pending action dispatch during its 5s cancel window",
            theme,
        ),
        help_line("f", "freeze / unfreeze auto-refresh", theme),
        help_line(
            "1 - 9",
            "jump to env at position 1-9 in the current view",
            theme,
        ),
        help_line("'", "name-jump: type a prefix to move selection", theme),
        help_line(
            "Ctrl-W",
            "yank equivalent `aws elasticbeanstalk describe-environments` command",
            theme,
        ),
        help_line("tab / shift-tab", "cycle scope (envs ↔ apps); Apps scope shows per-app rollup + has its own `a` / `b` / Enter", theme),
        help_line("click", "select row", theme),
        help_line("/", "filter rows (name, app, status, health)", theme),
        help_line("s / S", "cycle sort key / toggle ascending", theme),
        help_line("Ctrl-G", "toggle group-by-application", theme),
        help_line("Ctrl-E", "toggle events panel", theme),
        help_line(
            "T",
            "cycle event timestamp format (UTC → local → age)",
            theme,
        ),
        help_line("y / Y", "yank CNAME / name to clipboard", theme),
        help_line("Ctrl-Y", "export filtered table as TSV to clipboard", theme),
        help_line("r", "switch AWS region", theme),
        help_line("p", "switch AWS profile", theme),
        help_line(
            "Ctrl-K",
            "command palette: fuzzy search across commands / envs / views / plugins",
            theme,
        ),
        help_line("Ctrl-R / F5", "refresh now", theme),
        help_line(
            "Ctrl-X",
            "toggle redact mode (account id, ARN, CNAMEs)",
            theme,
        ),
        help_line("?", "toggle this help", theme),
        help_line("q / Ctrl-C", "quit", theme),
    ];
    // Apps-scope keys — pressed when Tab has swapped the main table to
    // the Applications view. Distinct from Envs-scope behaviour so the
    // operator knows what `a` / `b` / Enter do over there.
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Apps-scope keys (tab to enter)",
        Style::default()
            .fg(app.theme.title)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(help_line(
        "enter",
        "drill into envs (filters the envs table to this application)",
        theme,
    ));
    lines.push(help_line(
        "a",
        "open per-app action menu (Rebuild / Restart / Deploy / Open in console)",
        theme,
    ));
    lines.push(help_line(
        "b",
        "open application's AWS console page in the browser",
        theme,
    ));
    lines.push(help_line("j / k / g / G", "navigate the apps table", theme));
    lines.push(help_line(
        "space",
        "multi-select an app (persistent until esc clears)",
        theme,
    ));
    lines.push(help_line(
        "*",
        "pin / unpin selected app (sticks to top of apps table; persists in state.toml)",
        theme,
    ));
    // Command-bar reference — driven by `crate::commands::COMMANDS` so
    // adding a built-in only touches one file. Sections render in
    // `Category::ORDER`. Plugins land in their own footer block below.
    for category in crate::commands::Category::ORDER {
        let entries: Vec<&crate::commands::CommandSpec> = crate::commands::COMMANDS
            .iter()
            .filter(|c| c.category == *category)
            .collect();
        if entries.is_empty() {
            continue;
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            category.label(),
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )));
        for c in entries {
            // Label is `:name` plus a `/ :alias` chain when aliases
            // exist — matches the existing help convention where
            // `:q / :quit` was on one row.
            let mut label = format!(":{}", c.name);
            for alias in c.aliases {
                label.push_str(&format!(" / :{alias}"));
            }
            lines.push(help_line(&label, c.help, theme));
        }
    }
    // Plugin commands (user-defined in commands.toml). Listed last so
    // they don't interleave with built-ins in the built-in sections.
    if !app.plugins.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "User plugin commands (commands.toml)",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )));
        for (name, plugin) in &app.plugins {
            let desc = plugin
                .description
                .clone()
                .unwrap_or_else(|| "plugin command".to_string());
            lines.push(help_line(&format!(":{name}"), &desc, theme));
        }
    }
    // Detail-view per-tab keys — these aren't `:commands` so they
    // don't fit the registry; render manually under their own header.
    let mut detail_lines: Vec<Line> = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Detail-view per-tab keys",
            Style::default()
                .fg(app.theme.title)
                .add_modifier(Modifier::BOLD),
        )),
        help_line(
            "[ / ] (Metrics tab)",
            "decrease / increase metric range (15m → 24h)",
            theme,
        ),
        help_line(
            "(Logs tab) ^R",
            "request tail logs (takes ~10–20s while EB samples instances)",
            theme,
        ),
        help_line(
            "(Logs tab) s",
            "open CW Logs streaming overlay (live tail; needs `:logs-stream on`)",
            theme,
        ),
        help_line("(Logs tab) /", "regex-filter the visible log lines", theme),
        help_line(
            "(Logs overlay) Tab",
            "switch tailed log group via picker (over the env's discovered groups)",
            theme,
        ),
        Line::from(""),
    ];
    lines.append(&mut detail_lines);
    lines.push(Line::from(Span::styled(
        format!(
            "Refresh runs automatically every {interval_secs}s. Theme: {}. Configurable in ~/.config/ebman/config.toml.",
            app.theme.name
        ),
        Style::default().fg(app.theme.muted),
    )));
    lines.push(Line::from(Span::styled(
        "Region/profile come from the standard AWS env (AWS_REGION, AWS_PROFILE).",
        Style::default().fg(app.theme.muted),
    )));
    // Split the popup into a scrollable body + a sticky 1-row byline at
    // the bottom inside the border. The body is the popup minus the
    // border (top/bottom) and minus the padding (uniform(1) — top/bottom).
    // That gives the visible row budget for line-count clamping.
    let total_lines = lines.len() as u16;
    let inner_height = popup.height.saturating_sub(4); // top border + top pad + bottom pad + bottom border
                                                       // Reserve the bottommost inner row for the sticky byline; the body
                                                       // proper gets one less than that.
    let body_height = inner_height.saturating_sub(1);
    // Maximum scroll = where the last line is pinned to the body's
    // bottom. Below that, scrolling further would reveal blank space.
    let max_scroll = total_lines.saturating_sub(body_height);
    app.help_max_scroll = max_scroll;
    let effective_scroll = app.help_scroll.min(max_scroll);

    // Scroll indicators: emit "↑ N more above" on the top inner row and
    // "↓ N more below" on the row just above the byline when there's
    // content past the viewport. Rendered AFTER the body so they overlay
    // its first / last visible row.
    let footer_row = Rect {
        x: popup.x + 2,
        y: popup.y + popup.height.saturating_sub(2),
        width: popup.width.saturating_sub(4),
        height: 1,
    };
    let above_row = Rect {
        x: popup.x + 2,
        y: popup.y + 2, // skip border + top pad
        width: popup.width.saturating_sub(4),
        height: 1,
    };
    let below_row = Rect {
        x: popup.x + 2,
        y: popup.y + popup.height.saturating_sub(3),
        width: popup.width.saturating_sub(4),
        height: 1,
    };
    let help = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((effective_scroll, 0))
        .block(
            titled_block(&app.theme, "help", true, app.theme.title_alt)
                .padding(Padding::uniform(1)),
        );
    f.render_widget(help, popup);
    let muted_hint = Style::default()
        .fg(app.theme.accent)
        .add_modifier(Modifier::BOLD);
    if effective_scroll > 0 {
        let n = effective_scroll;
        // Clear blanks the row in the back-buffer; without it the
        // indicator overlays the body's visible line and leaves ghost
        // characters past the indicator text.
        f.render_widget(Clear, above_row);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("↑ {n} more above"),
                muted_hint,
            ))),
            above_row,
        );
    }
    if effective_scroll < max_scroll {
        let n = max_scroll - effective_scroll;
        f.render_widget(Clear, below_row);
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!("↓ {n} more below"),
                muted_hint,
            ))),
            below_row,
        );
    }
    // Sticky byline row at the bottom of the popup. Clear first for the
    // same reason as the indicators above — without it, longer help
    // body lines bleed past the byline's text.
    f.render_widget(Clear, footer_row);
    let credit = Paragraph::new(Line::from(Span::styled(
        format!(
            "ebman {} · built by Tom Baldwin / Polymorphism Ltd · :about",
            env!("CARGO_PKG_VERSION")
        ),
        Style::default()
            .fg(app.theme.muted)
            .add_modifier(Modifier::ITALIC),
    )));
    f.render_widget(credit, footer_row);
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
                format!("   {}delete this message? y / n", warn_glyph(theme.icons)),
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
                    "Rebuild env '{}'? (terminates and recreates all instances)",
                    modal.target_env
                ),
                Action::RestartAppServer => {
                    format!("Restart app server on env '{}'?", modal.target_env)
                }
                Action::SwapCnames => format!(
                    "Swap CNAMEs between '{}' and '{}'?",
                    modal.target_env,
                    modal.swap_with.as_deref().unwrap_or("?")
                ),
                Action::Terminate => format!(
                    "TERMINATE env '{}'. This cannot be undone.",
                    modal.target_env
                ),
                Action::Deploy => format!(
                    "Deploy version '{}' to env '{}'? (rolling, reversible)",
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
                // dispatched directly from command paths (Capacity opens a
                // modal form; Config* and TerminateInstance have their own
                // spawn paths). Placeholder copy keeps the match
                // exhaustive without dead UI.
                Action::Capacity
                | Action::ConfigSave
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
                    format!("  {}{w}", warn_glyph(theme.icons)),
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
    let header_title = format!("env: {}", env.name);
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
    let mut events_max_scroll: Option<u16> = None;
    let mut config_scroll: Option<u16> = None;
    match active_tab {
        DetailTab::Health => draw_detail_health(f, body_area, detail, app),
        DetailTab::Events => {
            events_max_scroll = Some(draw_detail_events(
                f,
                body_area,
                detail,
                &app.theme,
                app.event_time_format,
            ));
        }
        DetailTab::Instances => draw_detail_instances(f, body_area, detail, &app.theme),
        DetailTab::Metrics => draw_detail_metrics(f, body_area, detail, &app.theme),
        DetailTab::Queue => draw_detail_queue(f, body_area, detail, app.redact, &app.theme),
        DetailTab::Logs => draw_detail_logs(f, body_area, detail, &app.theme),
        DetailTab::Config => {
            config_scroll = Some(draw_detail_config(
                f,
                body_area,
                env,
                detail,
                app.redact,
                &app.required_tags,
                &app.theme,
            ));
        }
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
        // Persist the Events-tab scroll ceiling computed by the
        // renderer so the j/k key handler can clamp against it.
        if let Some(max) = events_max_scroll {
            d.events_max_scroll = max;
        }
        // Persist the Config-tab scroll offset the renderer adjusted
        // to keep the cursor in view.
        if let Some(s) = config_scroll {
            d.config_scroll = s;
        }
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
        DetailTab::Health => {
            " HEALTH  j/k move  enter drill  tab→ Events  a actions  ^R refresh  ? help  esc back"
        }
        DetailTab::Instances => {
            " INSTANCES  j/k cursor  s ssm shell  i info  y yank id  x terminate  tab→ Metrics  a actions  ^R refresh  ? help  esc back"
        }
        DetailTab::Events => {
            " EVENTS  j/k scroll  / filter  n/N next  L lvl  w window  T time  tab→ Instances  a actions  ^R refresh  ? help  esc back"
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
            " CONFIG  j/k move  enter edit  n new  x delete  a actions  ^R refresh  ? help  esc back"
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

/// Health rollup tab — the operator-first landing page when they Enter
/// on an env. Synthesises the same triage info as `:why` (recent events,
/// instance summary, worker DLQ depth) but inline as a tab, so the
/// operator can dwell on it without an overlay obscuring the rest of
/// the Detail chrome.
fn draw_detail_health(f: &mut Frame, area: Rect, detail: &crate::app::DetailState, app: &App) {
    let theme = &app.theme;
    let env = &detail.env_snapshot;
    let now = chrono::Utc::now();
    let mut lines: Vec<Line> = Vec::new();
    let section = |title: &str| -> Line<'static> {
        Line::from(Span::styled(
            format!("─── {title} "),
            Style::default()
                .fg(theme.title_alt)
                .add_modifier(Modifier::BOLD),
        ))
    };
    let muted = |s: String| -> Line<'static> {
        Line::from(Span::styled(s, Style::default().fg(theme.muted)))
    };
    // Build the navigable items + resolve the active one so the
    // renderer can prefix interactive rows with the cursor marker.
    let items = crate::app::health_items(detail, now);
    let active_item: Option<crate::app::HealthItem> = items.get(detail.health_cursor).copied();
    let cursor_glyph = cursor_marker(theme);
    // Two-cell-wide prefix so cursor/non-cursor rows align.
    let item_prefix = |is_active: bool| -> Span<'static> {
        if is_active {
            Span::styled(
                cursor_glyph.to_string(),
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            )
        } else {
            Span::raw("  ")
        }
    };

    // STATUS line — pill + health dot + worker DLQ chip when relevant.
    let mut status_line: Vec<Span<'static>> = vec![
        Span::styled(" status: ", Style::default().fg(theme.muted)),
        status_pill(&env.status, theme),
        Span::raw("  "),
        Span::styled("health: ", Style::default().fg(theme.muted)),
        health_dot(&env.health, theme),
        Span::raw(" "),
        Span::styled(
            env.health.clone(),
            Style::default()
                .fg(health_color(&env.health, theme))
                .add_modifier(Modifier::BOLD),
        ),
    ];
    let is_worker = env.tier.eq_ignore_ascii_case("Worker");
    let dlq_depth = if is_worker {
        app.worker_dlq_depths.get(&env.name).copied().unwrap_or(0)
    } else {
        0
    };
    if dlq_depth > 0 {
        status_line.push(Span::raw("   "));
        status_line.push(Span::styled(
            format!("{}DLQ:{dlq_depth}", warn_glyph(theme.icons)),
            Style::default()
                .fg(theme.health_red)
                .add_modifier(Modifier::BOLD),
        ));
    }
    // Updating-kind annotation: when EB reports `Updating` we can usually
    // infer what's actually happening from the most recent event. Render
    // a "→ deploying build-142" / "→ config change" / etc. suffix when
    // detail.events have populated and the env is mid-update.
    if env.status.eq_ignore_ascii_case("Updating") {
        use crate::app::UpdateKind;
        let kind_label: Option<String> = match crate::app::classify_update_kind(&detail.events) {
            UpdateKind::Deploy {
                version_label: Some(label),
            } => Some(format!("deploying {label}")),
            UpdateKind::Deploy {
                version_label: None,
            } => Some("deploying a new version".into()),
            UpdateKind::Config => Some("config change".into()),
            UpdateKind::Scale => Some("scaling instances".into()),
            UpdateKind::Platform => Some("platform update".into()),
            // Generic = either no events loaded yet or no recognised
            // pattern. Skip the suffix in that case rather than guessing.
            UpdateKind::Generic => None,
        };
        if let Some(label) = kind_label {
            status_line.push(Span::raw("   "));
            status_line.push(Span::styled(
                format!("→ {label}"),
                Style::default()
                    .fg(theme.status_updating)
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }
    lines.push(Line::from(status_line));
    lines.push(Line::raw(""));

    // 1. Recent significant events (ERROR / WARN in last 30m). Falls
    // back to "no recent events" rather than dumping noise.
    lines.push(section("recent events (last 30 min · errors + warnings)"));
    if detail.loading_events && detail.events.is_empty() {
        lines.push(muted(" fetching events…".into()));
    } else {
        let cutoff = now - chrono::Duration::minutes(30);
        // Filter with the source index so the cursor prefix can match
        // against `HealthItem::Event { event_idx }` later.
        let recent: Vec<(usize, &crate::aws::Event)> = detail
            .events
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                let sev = e.severity.to_uppercase();
                sev == "ERROR" || sev == "WARN"
            })
            .filter(|(_, e)| e.at.map(|t| t >= cutoff).unwrap_or(true))
            .take(10)
            .collect();
        if recent.is_empty() {
            lines.push(muted(
                " (no error / warning events in the last 30 min)".into(),
            ));
        } else {
            for (idx, e) in recent {
                let when =
                    e.at.map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
                        .unwrap_or_else(|| "??:??".into());
                let sev_style = match e.severity.to_uppercase().as_str() {
                    "ERROR" => Style::default().fg(theme.health_red),
                    "WARN" => Style::default().fg(theme.health_yellow),
                    _ => Style::default().fg(theme.muted),
                };
                let is_active =
                    active_item == Some(crate::app::HealthItem::Event { event_idx: idx });
                lines.push(Line::from(vec![
                    item_prefix(is_active),
                    Span::styled(format!("{when}  "), Style::default().fg(theme.muted)),
                    Span::styled(format!("{:<5}", e.severity), sev_style),
                    Span::raw("  "),
                    Span::styled(e.message.clone(), Style::default().fg(theme.text)),
                ]));
            }
        }
    }
    lines.push(Line::raw(""));

    // 2. Instance health summary — counts by colour. Severe instances
    // get a "(see Instances tab)" pointer so the operator knows where
    // to drill in.
    lines.push(section("instances"));
    if detail.loading_instances && detail.instances.is_empty() {
        lines.push(muted(" fetching instances…".into()));
    } else if detail.instances.is_empty() {
        lines.push(muted(" (no instances reported)".into()));
    } else {
        let mut buckets: std::collections::BTreeMap<String, u32> =
            std::collections::BTreeMap::new();
        for i in &detail.instances {
            *buckets.entry(i.color.clone()).or_default() += 1;
        }
        let total = detail.instances.len();
        let mut summary_spans = vec![Span::styled(
            format!(" {total} instance(s) · "),
            Style::default().fg(theme.muted),
        )];
        for (color, count) in &buckets {
            let style = match color.as_str() {
                "Red" => Style::default().fg(theme.health_red),
                "Yellow" => Style::default().fg(theme.health_yellow),
                "Green" => Style::default().fg(theme.health_green),
                _ => Style::default().fg(theme.muted),
            };
            summary_spans.push(Span::styled(format!("{count} {color}  "), style));
        }
        lines.push(Line::from(summary_spans));
        // Surface Severe instances inline so the operator doesn't need
        // to switch tabs to see WHICH instance is unhealthy. Iterate
        // with source index so the cursor can match by `instance_idx`.
        let mut shown = 0;
        for (idx, i) in detail.instances.iter().enumerate() {
            if shown >= 3 {
                break;
            }
            let red =
                i.color.eq_ignore_ascii_case("Red") || i.health.eq_ignore_ascii_case("Severe");
            if !red {
                continue;
            }
            shown += 1;
            let is_active =
                active_item == Some(crate::app::HealthItem::Instance { instance_idx: idx });
            lines.push(Line::from(vec![
                item_prefix(is_active),
                Span::styled(i.id.clone(), Style::default().fg(theme.text)),
                Span::raw("  "),
                Span::styled(
                    i.health.clone(),
                    Style::default()
                        .fg(theme.health_red)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            for cause in i.causes.iter().take(2) {
                lines.push(Line::from(Span::styled(
                    format!("    ↳ {cause}"),
                    Style::default().fg(theme.muted),
                )));
            }
        }
    }
    lines.push(Line::raw(""));

    // 3. CW alarms attached to this env. Mirrors the alarms section in
    // `:why` so the two triage surfaces tell the same story. Active
    // (ALARM-state) alarms first; the section is hidden when no alarms
    // exist to keep the panel quiet for healthy envs.
    let alarms_present = matches!(&detail.cw_alarms, Some(Ok(a)) if !a.is_empty());
    let alarms_loading = detail.loading_cw_alarms && detail.cw_alarms.is_none();
    if alarms_present || alarms_loading {
        lines.push(section("alarms"));
        if alarms_loading {
            lines.push(muted(" fetching alarms…".into()));
        } else if let Some(Ok(als)) = &detail.cw_alarms {
            let mut sorted: Vec<&crate::aws::CwAlarm> = als.iter().collect();
            sorted.sort_by_key(|a| match a.state.as_str() {
                "ALARM" => 0,
                "INSUFFICIENT_DATA" => 1,
                _ => 2,
            });
            for a in sorted.iter().take(8) {
                let (tag, style) = match a.state.as_str() {
                    "ALARM" => (
                        "ALARM",
                        Style::default()
                            .fg(theme.health_red)
                            .add_modifier(Modifier::BOLD),
                    ),
                    "OK" => ("OK   ", Style::default().fg(theme.health_green)),
                    _ => ("INS  ", Style::default().fg(theme.muted)),
                };
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(tag.to_string(), style),
                    Span::raw("  "),
                    Span::styled(
                        a.name.clone(),
                        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("  ({}/{})", a.namespace, a.metric_name),
                        Style::default().fg(theme.muted),
                    ),
                ]));
            }
        }
        lines.push(Line::raw(""));
    } else if let Some(Err(e)) = &detail.cw_alarms {
        lines.push(section("alarms"));
        lines.push(Line::from(Span::styled(
            format!(" error: {e}"),
            Style::default().fg(theme.health_red),
        )));
        lines.push(Line::raw(""));
    }

    // 4. Recent deploys — top 3 versions, newest first. The most-recent
    // deploy is the prime suspect when an env flips Red right after.
    // Section is skipped entirely on a brand-new app with no versions.
    let versions_present = matches!(&detail.recent_versions, Some(Ok(v)) if !v.is_empty());
    let versions_loading = detail.loading_recent_versions && detail.recent_versions.is_none();
    if versions_present || versions_loading {
        lines.push(section("recent deploys"));
        if versions_loading {
            lines.push(muted(" fetching deploys…".into()));
        } else if let Some(Ok(vers)) = &detail.recent_versions {
            for v in vers.iter().take(3) {
                let when = v
                    .created
                    .map(|t| humanize_age(now.signed_duration_since(t)))
                    .unwrap_or_else(|| "—".into());
                let when_style = Style::default().fg(age_color(v.created, now, theme));
                let mut spans = vec![
                    Span::raw("  "),
                    Span::styled(
                        v.label.clone(),
                        Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("  {when} ago"), when_style),
                ];
                if !v.description.is_empty() {
                    spans.push(Span::styled(
                        format!("  — {}", truncate_for_display(&v.description, 60)),
                        Style::default().fg(theme.muted),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }
        lines.push(Line::raw(""));
    }

    // 5. Worker queues — only for Worker envs. Reuses the queues data
    // populated by `detail_refresh_active_tab`'s `spawn_detail_queues`.
    if is_worker {
        lines.push(section("worker queues"));
        if detail.loading_queues {
            lines.push(muted(" fetching queue depths…".into()));
        } else {
            let q = &detail.queues;
            // Main queue row.
            let main_text = match q.main_stats.as_ref() {
                Some(s) => format!(
                    "main:  visible={}  in-flight={}  delayed={}",
                    s.visible, s.in_flight, s.delayed
                ),
                None => "main:  (queue URL not resolved)".to_string(),
            };
            let main_active = active_item == Some(crate::app::HealthItem::MainQueue);
            lines.push(Line::from(vec![
                item_prefix(main_active),
                Span::styled(main_text, Style::default().fg(theme.text)),
            ]));
            // DLQ row.
            let dlq_visible = q.dlq_stats.as_ref().map(|s| s.visible).unwrap_or(0);
            let dlq_text = match q.dlq_stats.as_ref() {
                Some(s) => format!(
                    "dlq:   visible={}  in-flight={}  delayed={}",
                    s.visible, s.in_flight, s.delayed
                ),
                None => "dlq:   (queue URL not resolved)".to_string(),
            };
            let dlq_style = if dlq_visible > 0 {
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.text)
            };
            let dlq_active = active_item == Some(crate::app::HealthItem::Dlq);
            lines.push(Line::from(vec![
                item_prefix(dlq_active),
                Span::styled(dlq_text, dlq_style),
            ]));
        }
        lines.push(Line::raw(""));
    }

    // 4. Drill-in hint — explicit pointer to the other tabs.
    lines.push(muted(
        " ── tab → drill into Events / Instances / Metrics / Queue / Logs / Config ──".into(),
    ));

    let block = rounded_block(theme, false).padding(Padding::horizontal(1));
    let p = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(block);
    f.render_widget(p, area);
}

/// Renders the Detail/Events tab. Returns the maximum legal
/// `events_scroll` for the current filtered line count + body height,
/// so the caller can persist it onto `DetailState` for the key
/// handler to clamp against (same contract as `help_max_scroll`).
fn draw_detail_events(
    f: &mut Frame,
    area: Rect,
    detail: &crate::app::DetailState,
    theme: &Theme,
    time_format: crate::app::EventTimeFormat,
) -> u16 {
    let now = chrono::Utc::now();
    // Severity + time-window filter. Indices map back to the source
    // `detail.events` vec so search-jump / Health drill-in stay valid.
    let visible: Vec<usize> = crate::mode_detail::filter_event_indices(
        &detail.events,
        detail.events_level,
        detail.events_window,
        now,
    );
    let total = detail.events.len();
    let shown = visible.len();
    let filters_on = detail.events_level != crate::app::EventLevel::default()
        || detail.events_window != crate::app::EventWindow::default();

    let matches = if let Some(re) = detail.search_pattern.as_ref() {
        visible
            .iter()
            .filter(|&&i| re.is_match(&detail.events[i].message))
            .count()
    } else {
        0
    };
    let mut title = if filters_on {
        format!(" Events [{shown}/{total}] ")
    } else {
        format!(" Events [{total}] ")
    };
    if filters_on {
        title.push_str(&format!(
            "· {} {} ",
            detail.events_level.label(),
            detail.events_window.label()
        ));
    }
    if detail.search_pattern.is_some() {
        title.push_str(&format!("· matches: {matches} "));
    }
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
                " ◌  no events for this env",
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "    ^R to re-fetch, R to toggle auto-refresh",
                Style::default().fg(theme.muted),
            )),
        ];
        f.render_widget(Paragraph::new(lines), body_area);
        return 0;
    }

    // Events exist but the active filter hides every one of them.
    if shown == 0 && !detail.events.is_empty() {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!(" ◌  no events match filter ({} hidden)", total),
                Style::default()
                    .fg(theme.muted)
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "    L widens severity · w widens time window",
                Style::default().fg(theme.muted),
            )),
        ];
        f.render_widget(Paragraph::new(lines), body_area);
        return 0;
    }

    let tw = event_time_width(time_format);
    let re = detail.search_pattern.as_ref();
    let lines: Vec<Line> = visible
        .iter()
        .map(|&i| {
            let e = &detail.events[i];
            let when = format_event_time(e.at, time_format, now);
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
                Span::styled(format!("{when:>tw$} "), Style::default().fg(theme.muted)),
                Span::styled(
                    format!("{:<5} ", e.severity),
                    severity_style(&e.severity, theme),
                ),
                Span::styled(e.message.clone(), msg_style),
            ])
        })
        .collect();
    // Clamp scroll so j/k can't push the list off the bottom into
    // blank space — `max_scroll` is the offset that pins the final
    // line to the body's bottom edge.
    let max_scroll = (lines.len() as u16).saturating_sub(body_area.height);
    let effective_scroll = detail.events_scroll.min(max_scroll);
    f.render_widget(
        Paragraph::new(lines).scroll((effective_scroll, 0)),
        body_area,
    );
    max_scroll
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
                        "  {}TERMINATE instance {}? ASG will replace it. y / n",
                        warn_glyph(theme.icons),
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

/// Render the in-progress "add a new row" editor line — a `+`
/// marker followed by the `KEY=VALUE` buffer with the caret drawn
/// at its position. Shown below whichever section (tags / env vars)
/// the new row will join.
fn config_new_row_line(edit: &crate::app::ConfigEdit, theme: &Theme) -> Line<'static> {
    let (before, after) = edit.split_at_caret();
    let editor_style = Style::default()
        .fg(theme.title_alt)
        .add_modifier(Modifier::BOLD);
    Line::from(vec![
        Span::styled("  + ", editor_style),
        Span::styled(
            format!("{before}{}{after}", caret_glyph(theme)),
            editor_style,
        ),
        Span::styled("   (KEY=VALUE)", Style::default().fg(theme.muted)),
    ])
}

/// Render one editable Config-tab row (a tag or env-var k/v pair).
/// Bumps `*idx` — the running editable-row counter shared across the
/// tags + env-vars sections — and decides from `detail.config_cursor`
/// / `detail.config_edit` whether to draw the `▶` cursor marker or
/// the in-place value editor (input buffer + blinking caret).
fn config_editable_row(
    detail: &crate::app::DetailState,
    idx: &mut usize,
    item: &crate::app::ConfigItem,
    key_width: usize,
    key_color: Color,
    theme: &Theme,
) -> Line<'static> {
    let this = *idx;
    *idx += 1;
    let key = item.key.as_str();
    let value = item.value.as_str();
    // Only an *existing-row* edit (`!is_new`) draws inside a row; the
    // add-new-row editor renders as its own line via `config_new_row_line`.
    let editing = detail
        .config_edit
        .as_ref()
        .filter(|e| !e.is_new && e.kind == item.kind && e.key == key);
    let is_cursor = detail.config_cursor == this && editing.is_none();
    let marker = if is_cursor { "▶ " } else { "  " };
    let key_len = key.chars().count();
    let key_text = if key_len <= key_width {
        format!("{marker}{key:<key_width$}")
    } else {
        // Long key overflows its column — wrap it onto its own line so
        // the value still aligns. Marker stays on the first row.
        format!("{marker}{key}\n  {pad:<key_width$}", pad = "")
    };
    let key_style = if is_cursor {
        Style::default().fg(key_color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(key_color)
    };
    let value_span = match editing {
        Some(e) => {
            // Caret renders at its position, not pinned to the end —
            // Left/Right move it within the value being edited.
            let (before, after) = e.split_at_caret();
            Span::styled(
                format!("{before}{}{after}", caret_glyph(theme)),
                Style::default()
                    .fg(theme.title_alt)
                    .add_modifier(Modifier::BOLD),
            )
        }
        None => {
            // Empty value shows as `""` so "explicitly empty" is
            // visually distinct from "absent".
            let shown = if value.is_empty() {
                "\"\"".to_string()
            } else {
                value.to_string()
            };
            Span::styled(shown, Style::default().fg(theme.text))
        }
    };
    let mut spans = vec![
        Span::styled(key_text, key_style),
        Span::raw("  "),
        value_span,
    ];
    // Delete-pending row gets a red confirm suffix.
    if detail.config_delete_confirm == Some(this) {
        spans.push(Span::styled(
            "   delete? y / N",
            Style::default()
                .fg(theme.health_red)
                .add_modifier(Modifier::BOLD),
        ));
    }
    Line::from(spans)
}

/// Renders the Detail/Config tab. Returns the (possibly adjusted)
/// vertical scroll offset so the caller can persist it onto
/// `DetailState.config_scroll` — the body is one tall `Paragraph`,
/// so without scroll-follow the cursor would run off the bottom on
/// an env with many tags + env vars.
fn draw_detail_config(
    f: &mut Frame,
    area: Rect,
    env: &crate::aws::Environment,
    detail: &crate::app::DetailState,
    redact_on: bool,
    required_tags: &[String],
    theme: &Theme,
) -> u16 {
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

    // Running counter across the editable sections (tags then env
    // vars) — must match the order `config_editable_items` produces
    // so the cursor index lines up with what's on screen.
    let mut editable_idx: usize = 0;
    // Line index (into `lines`) of each editable row, in cursor
    // order — drives scroll-follow so the cursor stays on screen.
    let mut row_line_idx: Vec<usize> = Vec::new();
    // Line index of the in-progress add-a-row editor, if any —
    // scroll-follow targets it so the operator never types blind
    // below the fold.
    let mut new_row_line: Option<usize> = None;

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
            let item = crate::app::ConfigItem {
                kind: crate::app::ConfigItemKind::Tag,
                key: (*k).clone(),
                value: (*v).clone(),
            };
            row_line_idx.push(lines.len());
            lines.push(config_editable_row(
                detail,
                &mut editable_idx,
                &item,
                max_key_width,
                theme.app_palette[0],
                theme,
            ));
        }
    }
    // In-progress add-a-tag editor renders below the tag rows.
    if let Some(e) = detail
        .config_edit
        .as_ref()
        .filter(|e| e.is_new && e.kind == crate::app::ConfigItemKind::Tag)
    {
        new_row_line = Some(lines.len());
        lines.push(config_new_row_line(e, theme));
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
                    format!(
                        "{}missing required tag(s): {}",
                        warn_glyph(theme.icons),
                        missing.join(", ")
                    ),
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
            let item = crate::app::ConfigItem {
                kind: crate::app::ConfigItemKind::EnvVar,
                key: k.clone(),
                value: v.clone(),
            };
            row_line_idx.push(lines.len());
            lines.push(config_editable_row(
                detail,
                &mut editable_idx,
                &item,
                max_key_width,
                theme.app_palette[1],
                theme,
            ));
        }
    }
    // In-progress add-an-env-var editor renders below the env-var rows.
    if let Some(e) = detail
        .config_edit
        .as_ref()
        .filter(|e| e.is_new && e.kind == crate::app::ConfigItemKind::EnvVar)
    {
        new_row_line = Some(lines.len());
        lines.push(config_new_row_line(e, theme));
    }

    // Scroll-follow: keep the active row inside the viewport. While
    // adding, follow the new-row editor (so the operator doesn't
    // type blind below the fold); otherwise follow the cursor row.
    let inner_h = area.height.saturating_sub(2) as usize;
    let follow_line = new_row_line.or_else(|| row_line_idx.get(detail.config_cursor).copied());
    let scroll = config_scroll_follow(detail.config_scroll, follow_line, inner_h, lines.len());
    f.render_widget(Paragraph::new(lines).block(block).scroll((scroll, 0)), area);
    scroll
}

/// Pure: adjust a Config-tab scroll offset to keep `cursor_line`
/// inside a `viewport_h`-tall window over `total_lines`. The offset
/// only moves when the cursor would fall off an edge (so unrelated
/// scrolling doesn't jump), then clamps so the view never runs past
/// the last line. `cursor_line` is `None` when there's no editable
/// row — the offset is left as-is (just clamped).
fn config_scroll_follow(
    current: u16,
    cursor_line: Option<usize>,
    viewport_h: usize,
    total_lines: usize,
) -> u16 {
    let mut scroll = current as usize;
    if let Some(cl) = cursor_line {
        if cl < scroll {
            scroll = cl;
        } else if viewport_h > 0 && cl >= scroll + viewport_h {
            scroll = cl + 1 - viewport_h;
        }
    }
    let max_scroll = total_lines.saturating_sub(viewport_h);
    scroll.min(max_scroll) as u16
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
        help_line("L", "cycle min severity (all → info → warn → error)", theme),
        help_line("w", "cycle time window (all → 1h → 6h → 24h → 7d)", theme),
        help_line("T", "cycle timestamp format (UTC → local → age)", theme),
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
            "Config tab",
            Style::default().fg(app.theme.title),
        )),
        help_line(
            "j / k",
            "move cursor over editable rows (tags + env vars)",
            theme,
        ),
        help_line(
            "enter",
            "edit selected value in place (enter saves, esc cancels)",
            theme,
        ),
        help_line(
            "n",
            "add a new row (KEY=VALUE; kind from cursor section)",
            theme,
        ),
        help_line("x", "delete the selected row (y confirms)", theme),
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

fn help_line(key: &str, desc: &str, theme: &Theme) -> Line<'static> {
    // Pad short keys to a 16-char column so descriptions line up, but if the
    // key itself is wider than the column always emit at least 2 spaces of
    // separator so it can't glue against the description.
    //
    // Returns Line<'static> by cloning into owned Spans so callers can
    // pass non-'static labels (e.g. the registry-driven loop builds
    // `format!(":{name}")` per row). Cheap — the help screen renders
    // once per `?` press, not per frame.
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
        Span::styled(desc.to_string(), Style::default().fg(theme.text)),
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

/// Pure: ASCII-case-insensitive "is `s` any of these?" predicate. Cheap
/// alternative to `s.to_lowercase().as_str()` matching against a fixed
/// option list — saves a per-call `String` allocation in the table-row
/// render hot path, where `health` / `status` strings come from AWS in
/// known-case form anyway.
fn ieq_any(s: &str, options: &[&str]) -> bool {
    options.iter().any(|o| s.eq_ignore_ascii_case(o))
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

/// Pure: chevron used in the non-Powerline group-banner row to mark the
/// start of an app section (`── ▶ app-name ──`). Powerline mode renders
/// its own ribbon and never calls this — but we return a sensible glyph
/// anyway so the helper is total.
fn separator_glyph(icons: IconStyle) -> &'static str {
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
fn warn_glyph(icons: IconStyle) -> &'static str {
    match icons {
        IconStyle::Ascii => "! ",
        _ => "⚠ ",
    }
}

/// Hint / suggestion glyph — `💡 ` (lightbulb) in unicode/powerline,
/// `? ` in ascii. Used by context-aware footer hints (`:why` / `:alarms`
/// suggestions when the status slot is empty).
fn hint_glyph(icons: IconStyle) -> &'static str {
    match icons {
        IconStyle::Ascii => "? ",
        _ => "💡 ",
    }
}

/// Severity-stripe glyph for toast notification bodies. Half-block
/// `▎` in unicode/powerline, `|` in ascii.
fn stripe_glyph(icons: IconStyle) -> &'static str {
    match icons {
        IconStyle::Ascii => "|",
        _ => "▎",
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
fn scale_rgb(color: Color, factor: f32) -> Color {
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

fn health_style(health: &str, theme: &Theme) -> Style {
    Style::default()
        .fg(health_color(health, theme))
        .add_modifier(Modifier::BOLD)
}

/// Pure: map an EB health bucket name (any case) to the theme's
/// corresponding palette colour. Allocation-free — extracted so the
/// per-row hot path doesn't pay a `to_lowercase` per cell.
fn health_color(health: &str, theme: &Theme) -> Color {
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
    // Red envs — point at the v0.3.0 triage tool. The alerts pill in
    // the header already shows the count, so this hint doesn't repeat
    // it; it sends the operator at the action.
    if app.alerts > 0 {
        return Some(
            "`!` on a Red env opens :why (events + alarms + instances + recent deploys)".into(),
        );
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
        // Normalise to a short stem so "Rebuild env" /
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
        parts.push(format!("{web} Web"));
        parts.push(format!("{worker} Worker"));
    }
    if red > 0 {
        parts.push(format!("{red} red"));
    }
    if yellow > 0 {
        parts.push(format!("{yellow} yellow"));
    }
    parts.join(" · ")
}

/// Pure: render the header "last refresh" label as Grafana-style
/// relative time — `12s ago · next 3s`. Cheaper visual scan than the
/// absolute `HH:MM:SS (every Ns)` it replaces. Returns the format
/// untouched when `last_refresh` is `None` (haven't refreshed yet).
///
/// The `next` countdown can go negative when a refresh is overdue
/// (throttled, network slow, frozen with `f`); we clamp it to `0s` and
/// the operator sees the indicator continue to tick up the `… ago`.
fn format_refresh_label(
    last_refresh: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    refresh_interval: std::time::Duration,
) -> String {
    let interval_s = refresh_interval.as_secs() as i64;
    match last_refresh {
        Some(t) => {
            let ago = now.signed_duration_since(t).num_seconds().max(0);
            let until = (interval_s - ago).max(0);
            format!("{}s ago · next {}s", ago, until)
        }
        None => format!("— · next {interval_s}s"),
    }
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

/// Render an event timestamp according to the operator's chosen
/// [`EventTimeFormat`]. `Utc` / `Local` produce a full
/// `YYYY-MM-DD HH:MM:SS` stamp (UTC suffixed with `Z`); `Age` keeps
/// the compact relative form. `None` timestamps render as `—`.
/// Pure — `now` is passed in so the Age branch is testable.
fn format_event_time(
    at: Option<chrono::DateTime<chrono::Utc>>,
    mode: crate::app::EventTimeFormat,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    use crate::app::EventTimeFormat;
    let Some(t) = at else {
        return "—".into();
    };
    match mode {
        EventTimeFormat::Utc => t.format("%Y-%m-%d %H:%M:%SZ").to_string(),
        EventTimeFormat::Local => t
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        EventTimeFormat::Age => humanize_age(now.signed_duration_since(t)),
    }
}

/// Column width to reserve for the event-time cell, given the mode.
/// UTC carries the `Z` suffix so it's one wider than Local; Age is
/// the compact 4-cell form. Keeps the two event renderers aligned.
fn event_time_width(mode: crate::app::EventTimeFormat) -> usize {
    use crate::app::EventTimeFormat;
    match mode {
        EventTimeFormat::Utc => 20,   // "YYYY-MM-DD HH:MM:SSZ"
        EventTimeFormat::Local => 19, // "YYYY-MM-DD HH:MM:SS"
        EventTimeFormat::Age => 4,    // ">999d" worst case is 5; 4 matches old layout
    }
}

/// Pure: pick a theme colour for the AGE column based on how recently the
/// env was updated. Three buckets:
///
/// - `< 24h` → `title_alt` (just-deployed; pairs with the `◆` drift glyph)
/// - `24h – 30d` → `text` (actively maintained)
/// - `> 30d` or missing → `muted` (sleeping / no signal)
///
/// Negative durations (clock skew) are treated as 0 so the call doesn't
/// flip into the >30d bucket on a tiny future timestamp.
fn age_color(
    updated: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
    theme: &Theme,
) -> Color {
    let Some(u) = updated else {
        return theme.muted;
    };
    let dur = now.signed_duration_since(u);
    if dur < chrono::Duration::zero() || dur < chrono::Duration::hours(24) {
        theme.title_alt
    } else if dur > chrono::Duration::days(30) {
        theme.muted
    } else {
        theme.text
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
    fn warn_glyph_falls_back_in_ascii() {
        assert_eq!(super::warn_glyph(IconStyle::Ascii), "! ");
        assert_eq!(super::warn_glyph(IconStyle::Unicode), "⚠ ");
        assert_eq!(super::warn_glyph(IconStyle::Powerline), "⚠ ");
    }

    #[test]
    fn hint_glyph_falls_back_in_ascii() {
        assert_eq!(super::hint_glyph(IconStyle::Ascii), "? ");
        assert_eq!(super::hint_glyph(IconStyle::Unicode), "💡 ");
    }

    #[test]
    fn stripe_glyph_falls_back_in_ascii() {
        assert_eq!(super::stripe_glyph(IconStyle::Ascii), "|");
        assert_eq!(super::stripe_glyph(IconStyle::Unicode), "▎");
    }

    #[test]
    fn prune_pills_keeps_first_under_width() {
        let theme = crate::theme::Theme::dark();
        let mut pills: Vec<(String, ratatui::style::Color, ratatui::style::Color)> = vec![
            ("ALERTS".into(), Color::Black, theme.health_red),
            ("PENDING".into(), Color::Black, theme.health_yellow),
            ("READ-ONLY".into(), Color::Black, theme.health_green),
        ];
        super::prune_pills_to_width(&mut pills, &theme, 10);
        // First pill always kept even when nothing fits.
        assert!(!pills.is_empty());
        assert_eq!(pills[0].0.split(' ').next().unwrap(), "ALERTS");
    }

    #[test]
    fn prune_pills_marks_overflow_count_on_last_pill() {
        let theme = crate::theme::Theme::dark();
        let mut pills: Vec<(String, ratatui::style::Color, ratatui::style::Color)> = vec![
            ("ALERTS".into(), Color::Black, theme.health_red),
            ("PENDING".into(), Color::Black, theme.health_yellow),
            ("READ-ONLY".into(), Color::Black, theme.health_green),
            ("UPDATE".into(), Color::Black, theme.title_alt),
        ];
        // Tight budget that fits one pill: marker appears on the survivor.
        super::prune_pills_to_width(&mut pills, &theme, 10);
        assert_eq!(pills.len(), 1);
        assert!(
            pills[0].0.contains("+3"),
            "expected last-pill marker '+3' on survivor, got {:?}",
            pills[0].0
        );
    }

    #[test]
    fn prune_pills_noop_when_chain_fits() {
        let theme = crate::theme::Theme::dark();
        let mut pills: Vec<(String, ratatui::style::Color, ratatui::style::Color)> = vec![
            ("A".into(), Color::Black, theme.health_red),
            ("B".into(), Color::Black, theme.health_yellow),
        ];
        let before = pills.clone();
        super::prune_pills_to_width(&mut pills, &theme, 1_000);
        assert_eq!(pills, before, "wide budget should not trim or mark");
    }

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
    fn age_color_fresh_uses_title_alt() {
        let t = Theme::dark();
        let now = chrono::Utc::now();
        let updated = now - chrono::Duration::hours(2);
        assert_eq!(super::age_color(Some(updated), now, &t), t.title_alt);
    }

    #[test]
    fn age_color_normal_uses_text() {
        let t = Theme::dark();
        let now = chrono::Utc::now();
        let updated = now - chrono::Duration::days(5);
        assert_eq!(super::age_color(Some(updated), now, &t), t.text);
    }

    #[test]
    fn age_color_stale_uses_muted() {
        let t = Theme::dark();
        let now = chrono::Utc::now();
        let updated = now - chrono::Duration::days(45);
        assert_eq!(super::age_color(Some(updated), now, &t), t.muted);
    }

    #[test]
    fn age_color_missing_uses_muted() {
        let t = Theme::dark();
        let now = chrono::Utc::now();
        assert_eq!(super::age_color(None, now, &t), t.muted);
    }

    #[test]
    fn age_color_future_clock_skew_is_fresh_not_stale() {
        // If `updated` is slightly in the future (clock drift between EB
        // and the local box), don't classify it as >30d — that would flip
        // the colour straight to muted on a brand-new env.
        let t = Theme::dark();
        let now = chrono::Utc::now();
        let updated = now + chrono::Duration::seconds(30);
        assert_eq!(super::age_color(Some(updated), now, &t), t.title_alt);
    }

    #[test]
    fn age_color_boundary_at_24h_is_normal() {
        // Exactly 24h: dur < 24h is false, dur > 30d is false → normal (text).
        let t = Theme::dark();
        let now = chrono::Utc::now();
        let updated = now - chrono::Duration::hours(24);
        assert_eq!(super::age_color(Some(updated), now, &t), t.text);
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
    fn format_event_time_renders_each_mode() {
        use crate::app::EventTimeFormat;
        use chrono::{TimeZone, Utc};
        let t = Utc.with_ymd_and_hms(2026, 5, 21, 22, 34, 56).unwrap();
        let now = Utc.with_ymd_and_hms(2026, 5, 21, 22, 39, 56).unwrap();
        // UTC: full stamp with trailing Z.
        assert_eq!(
            format_event_time(Some(t), EventTimeFormat::Utc, now),
            "2026-05-21 22:34:56Z"
        );
        // Age: 5 minutes elapsed.
        assert_eq!(format_event_time(Some(t), EventTimeFormat::Age, now), "5m");
        // Local: shape only (TZ-dependent) — assert length + no Z suffix.
        let local = format_event_time(Some(t), EventTimeFormat::Local, now);
        assert_eq!(local.len(), 19);
        assert!(!local.ends_with('Z'));
    }

    #[test]
    fn format_event_time_handles_missing_timestamp() {
        use crate::app::EventTimeFormat;
        let now = chrono::Utc::now();
        for mode in [
            EventTimeFormat::Utc,
            EventTimeFormat::Local,
            EventTimeFormat::Age,
        ] {
            assert_eq!(format_event_time(None, mode, now), "—");
        }
    }

    #[test]
    fn config_scroll_follow_keeps_cursor_in_viewport() {
        // Cursor in view → offset unchanged.
        assert_eq!(config_scroll_follow(0, Some(5), 20, 100), 0);
        // Cursor below the fold → scroll so cursor is the last visible row.
        assert_eq!(config_scroll_follow(0, Some(25), 20, 100), 6);
        // Cursor above the current offset → scroll up to it.
        assert_eq!(config_scroll_follow(30, Some(10), 20, 100), 10);
        // Never scroll past the end: max = total - viewport.
        assert_eq!(config_scroll_follow(0, Some(99), 20, 100), 80);
        // No editable row → offset just clamped, not moved by a cursor.
        assert_eq!(config_scroll_follow(50, None, 20, 100), 50);
        assert_eq!(config_scroll_follow(95, None, 20, 100), 80);
        // Content shorter than the viewport → no scroll at all.
        assert_eq!(config_scroll_follow(0, Some(3), 20, 8), 0);
    }

    #[test]
    fn about_layout_picks_by_terminal_size() {
        // text_h ~15 — a representative project-text height.
        let th = 15;
        // Roomy → scene stacked above text.
        assert_eq!(about_layout(120, 60, th), AboutLayout::Stacked);
        // Wide but short → scene beside text.
        assert_eq!(about_layout(140, 30, th), AboutLayout::SideBySide);
        // Small both ways → text only.
        assert_eq!(about_layout(50, 20, th), AboutLayout::TextOnly);
        // Wide enough to stack but the scene won't fit width-wise
        // for side-by-side either → text only.
        assert_eq!(about_layout(64, 60, th), AboutLayout::TextOnly);
        // Tall enough but too narrow for the scene → text only.
        assert_eq!(about_layout(62, 60, th), AboutLayout::TextOnly);
    }

    #[test]
    fn event_time_width_matches_rendered_stamp() {
        use crate::app::EventTimeFormat;
        // UTC width must fit "YYYY-MM-DD HH:MM:SSZ" (20 chars).
        assert_eq!(event_time_width(EventTimeFormat::Utc), 20);
        assert_eq!(event_time_width(EventTimeFormat::Local), 19);
        assert_eq!(event_time_width(EventTimeFormat::Age), 4);
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
            "Rebuild env 'prod-api'? (terminates and recreates)",
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
        let envs = [e("Web", "Green"), e("Web", "Green"), e("Web", "Red")];
        let refs: Vec<&Environment> = envs.iter().collect();
        let s = summarize_group(&refs);
        // 3 envs, all web (no worker), 1 red — only the non-empty buckets
        // appear. Tier split omitted because everyone is web.
        assert!(s.contains("3 envs"));
        assert!(s.contains("1 red"));
        assert!(!s.contains("Worker"));
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
        let envs = [e("Web", "Green"), e("Worker", "Yellow"), e("Worker", "Red")];
        let refs: Vec<&Environment> = envs.iter().collect();
        let s = summarize_group(&refs);
        assert!(s.contains("1 Web"));
        assert!(s.contains("2 Worker"));
        assert!(s.contains("1 red"));
        assert!(s.contains("1 yellow"));
    }

    #[test]
    fn summarize_group_empty_input() {
        assert_eq!(summarize_group(&[]), "");
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
    fn format_refresh_label_relative_with_recent_refresh() {
        let interval = std::time::Duration::from_secs(15);
        let last = chrono::Utc::now() - chrono::Duration::seconds(3);
        let now = chrono::Utc::now();
        let label = format_refresh_label(Some(last), now, interval);
        // Tolerate ±1s clock jitter in the test.
        assert!(label.starts_with("3s ago") || label.starts_with("2s ago"));
        assert!(label.contains("next 1") || label.contains("next 12") || label.contains("next 13"));
    }

    #[test]
    fn format_refresh_label_clamps_overdue_to_zero() {
        // Refresh was 30s ago with a 15s interval — countdown should
        // clamp to 0 not show a negative number.
        let interval = std::time::Duration::from_secs(15);
        let now = chrono::Utc::now();
        let last = now - chrono::Duration::seconds(30);
        let label = format_refresh_label(Some(last), now, interval);
        assert!(label.contains("next 0s"), "got {label:?}");
    }

    #[test]
    fn format_refresh_label_handles_no_prior_refresh() {
        let label =
            format_refresh_label(None, chrono::Utc::now(), std::time::Duration::from_secs(15));
        assert_eq!(label, "— · next 15s");
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
    fn scale_rgb_darkens_proportionally_and_passes_through_named() {
        // 0.5 of (200, 100, 50) → (100, 50, 25). Truncating float → u8 cast.
        assert_eq!(
            super::scale_rgb(Color::Rgb(200, 100, 50), 0.5),
            Color::Rgb(100, 50, 25)
        );
        // Factor 1.0 is identity.
        assert_eq!(
            super::scale_rgb(Color::Rgb(200, 100, 50), 1.0),
            Color::Rgb(200, 100, 50)
        );
        // Factor 0.0 → black.
        assert_eq!(
            super::scale_rgb(Color::Rgb(255, 255, 255), 0.0),
            Color::Rgb(0, 0, 0)
        );
        // Factor clamps to [0, 1] — overflowing values don't yield > 255.
        assert_eq!(
            super::scale_rgb(Color::Rgb(200, 100, 50), 2.0),
            Color::Rgb(200, 100, 50)
        );
        // Non-RGB colours pass through unchanged (no portable darken).
        assert_eq!(super::scale_rgb(Color::Red, 0.5), Color::Red);
    }

    #[test]
    fn truncate_for_display_handles_short_long_and_multibyte() {
        // No truncation when under the cap.
        assert_eq!(super::truncate_for_display("hello", 10), "hello");
        // Exactly at the cap — also untouched.
        assert_eq!(super::truncate_for_display("0123456789", 10), "0123456789");
        // Over the cap — drops chars to fit `…`. max=5 means 4 chars + `…`.
        assert_eq!(super::truncate_for_display("0123456789", 5), "0123…");
        // Multi-byte (each char width 1 in unicode-width terms here) —
        // count by chars, not bytes.
        assert_eq!(super::truncate_for_display("éééééééé", 4), "ééé…");
    }

    #[test]
    fn separator_glyph_falls_back_to_ascii_chevron() {
        assert_eq!(super::separator_glyph(IconStyle::Ascii), ">");
        assert_eq!(super::separator_glyph(IconStyle::Unicode), "▶");
        // Powerline mode never reaches the non-Powerline banner path in
        // practice (it has its own ribbon renderer) — but the glyph
        // should still be a sensible BMP chevron rather than panicking
        // or returning empty.
        assert_eq!(super::separator_glyph(IconStyle::Powerline), "▶");
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

    #[test]
    fn header_dimensions_merges_pills_when_room_to_spare() {
        // Info row 60w + 2 gap + 20w chain = 82, well inside 120w column.
        let (rows, merge) = header_dimensions(60, 20, 120, false);
        assert!(merge, "wide window should merge pills onto info row");
        assert_eq!(rows, 5, "merged layout uses 5 rows (2 borders + 3 content)");
    }

    #[test]
    fn header_dimensions_keeps_pill_row_when_too_narrow() {
        // Info row 60w + 2 gap + 30w chain = 92 > 80w column — has to wrap.
        let (rows, merge) = header_dimensions(60, 30, 80, false);
        assert!(!merge, "narrow window should keep pills on their own row");
        assert_eq!(rows, 6, "split layout adds one row for the pill chain");
    }

    #[test]
    fn header_dimensions_with_no_pills_uses_compact_layout() {
        // No pills present (chain_w == 0): never merges, never reserves a pill row.
        let (rows, merge) = header_dimensions(60, 0, 80, false);
        assert!(!merge);
        assert_eq!(rows, 5);
    }

    #[test]
    fn header_dimensions_adds_row_for_saved_filters() {
        let (rows, _) = header_dimensions(60, 0, 200, true);
        assert_eq!(rows, 6, "saved-filter chip bar adds one row");

        let (rows_with_pills, merged) = header_dimensions(60, 20, 200, true);
        assert!(merged);
        assert_eq!(rows_with_pills, 6, "merged + filters = 5 + 1");
    }

    #[test]
    fn header_dimensions_boundary_is_inclusive() {
        // info(50) + gap(2) + chain(48) == inner(100) → should merge (≤).
        let (_, merge) = header_dimensions(50, 48, 100, false);
        assert!(merge);
        // One column over → no longer merges.
        let (_, merge) = header_dimensions(50, 49, 100, false);
        assert!(!merge);
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
