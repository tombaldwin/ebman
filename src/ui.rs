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
use crate::overlay::{centered_overlay, OverlaySize};
use crate::theme::{IconStyle, Theme};

use crate::app::{
    Action, ActionFlow, App, ConfirmKind, DetailTab, DisplayRow, LoadState, Mode, Overlay, Scope,
    SortKey, ToastKind, ViewMode, ACTIONS,
};

mod detail;
mod help;
mod overlays;
use detail::*;
pub use detail::{hover_index, series_anomaly_label};
use help::*;
use overlays::*;

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
    // Incident banner — `:incident START` is active. First in the
    // chain after alerts: during an incident this is the one signal
    // everyone sharing the terminal must see, so it survives the
    // width-pruning pass ahead of the UX pills.
    if let Some(incident) = app.incident.as_ref() {
        let age = (chrono::Utc::now() - incident.started_at)
            .to_std()
            .unwrap_or_default();
        let glyph = incident_glyph(theme);
        let label = if incident.headline.is_empty() {
            format!("{glyph}INCIDENT ({})", crate::app::humanize_short_age(age))
        } else {
            // Cap the headline: `prune_pills_to_width` only pops whole
            // trailing pills, so an uncapped headline would evict every
            // lower-priority pill and still clip.
            format!(
                "{glyph}INCIDENT ({}): {}",
                crate::app::humanize_short_age(age),
                truncate_for_display(&incident.headline, 60)
            )
        };
        chain.push((label, fg(theme.health_red), theme.health_red));
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
    // Armed auto-rollback watchdog — operator dispatched `:deploy
    // LABEL --auto-rollback Nm` and the deadline hasn't fired yet.
    // Without this pill the operator has no visible signal between
    // arm-time toast and deadline fire; with it, the countdown is
    // always one glance away. Re-renders on the 100ms anim ticker
    // (loading_visible_until / pending_dispatch already keep that
    // alive when relevant; rebuild gates it on `!armed.is_empty()`).
    if let Some((env, remaining)) =
        crate::app::soonest_armed_rollback(&app.armed_watchdogs, chrono::Utc::now())
    {
        let label = if app.armed_watchdogs.len() == 1 {
            format!("⏱ rollback {env} in {remaining}")
        } else {
            format!(
                "⏱ {} rollbacks armed (next: {env} in {remaining})",
                app.armed_watchdogs.len()
            )
        };
        chain.push((label, fg(theme.health_yellow), theme.health_yellow));
    }
    // Watching-deploy countdown — `:deploy LABEL --wait-for-green Nm`
    // armed a watcher. Different glyph + colour from the armed-rollback
    // pill so the operator can tell at a glance which kind of in-flight
    // observer is on the env: "👁 watching" (just reports outcome) vs
    // "⏱ rollback" (will redeploy on timeout). Blue/title colour reads
    // as informational rather than alarming.
    if let Some((env, remaining)) =
        crate::app::soonest_watching_deploy(&app.watching_deploys, chrono::Utc::now())
    {
        let label = if app.watching_deploys.len() == 1 {
            format!("👁 watching {env} {remaining}")
        } else {
            format!(
                "👁 {} watching (next: {env} {remaining})",
                app.watching_deploys.len()
            )
        };
        chain.push((label, fg(theme.title), theme.title));
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
    if app.view.redact {
        chain.push((
            "REDACT".into(),
            fg(theme.health_yellow),
            theme.health_yellow,
        ));
    }
    if app.view.grouped() {
        chain.push(("GROUPED".into(), fg(theme.title_alt), theme.title_alt));
    }
    match app.view.mode {
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

/// Pick the ascii fallback for a decorative glyph when the operator's
/// font can't render unicode (`icons = "ascii"` — the mode's contract
/// is "stays readable when the font lacks the glyphs", so raw literals
/// in draw paths are stragglers).
fn glyph<'a>(icons: IconStyle, unicode: &'a str, ascii: &'a str) -> &'a str {
    match icons {
        IconStyle::Ascii => ascii,
        _ => unicode,
    }
}

/// Glyph for the multi-select-active pill.
fn multi_select_glyph(theme: &Theme) -> &'static str {
    match theme.icons {
        IconStyle::Ascii => "+ ",
        _ => "▶ ",
    }
}

/// Glyph for the incident banner pill. `🚨` is unicode-only; ascii
/// terminals get a loud `!!` tag instead of tofu.
fn incident_glyph(theme: &Theme) -> &'static str {
    match theme.icons {
        IconStyle::Ascii => "!! ",
        _ => "🚨 ",
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

    header_dimensions(info_w, chain_w, inner, !app.saved_views.is_empty())
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
    let sort_dir = if app.view.sort_desc { "↓" } else { "↑" };
    let sort_label = format!("{}{}", app.view.sort_key.label(), sort_dir);
    let env_count = app.environments.len().to_string();
    let caller = redact(
        &app.context
            .caller_arn
            .as_deref()
            .map(short_caller)
            .unwrap_or_else(|| "—".into()),
        app.view.redact,
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
    if !app.view.filter().is_empty() {
        w += sep_w + "Filter: ".chars().count() + app.view.filter().text().chars().count();
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
        let events_height: u16 = if app.event_panel.visible {
            app.event_panel.height
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
        if app.event_panel.visible {
            app.event_panel.area = Some(chunks[2]);
            draw_events(f, chunks[2], app);
            draw_footer(f, chunks[3], app);
        } else {
            app.event_panel.area = None;
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
        app.help.max_scroll = 0;
    }
    if app.mode == Mode::Picker {
        draw_picker(f, f.area(), app);
    }
    if app.mode == Mode::Action {
        draw_action(f, f.area(), app);
    }
    // WhyRed drawn separately (its renderer needs `&mut App`); every
    // other overlay matches by REFERENCE — the previous per-frame
    // `.clone()` deep-copied the whole overlay each draw, including up
    // to 2000 tail events, in exactly the busy-fleet hot path the tail
    // overlays exist for.
    if matches!(app.current_overlay, Some(Overlay::WhyRed { .. })) {
        draw_why_red_overlay(f, f.area(), app);
    } else if let Some(overlay) = app.current_overlay.as_ref() {
        match overlay {
            Overlay::Describe(text) => draw_describe(f, f.area(), app, text),
            Overlay::Whatsnew(text) => draw_whatsnew(f, f.area(), app, text),
            Overlay::History(text) => draw_history_overlay(f, f.area(), app, text),
            Overlay::Alarms { body, .. } => draw_alarms_overlay(f, f.area(), app, body),
            Overlay::Diff(text) => draw_diff_overlay(f, f.area(), app, text),
            Overlay::SavedConfigs(text) => draw_saved_configs_overlay(f, f.area(), app, text),
            Overlay::SavedConfigsInteractive {
                items,
                cursor,
                confirm_delete,
            } => draw_saved_configs_interactive(f, f.area(), app, items, *cursor, *confirm_delete),
            Overlay::TextDump { title, body } => {
                draw_text_dump_overlay(f, f.area(), app, title, body)
            }
            Overlay::LogTail { .. } => draw_log_tail_overlay(f, f.area(), app),
            Overlay::EventTail { .. } => draw_event_tail_overlay(f, f.area(), app),
            // Handled above — the renderer needs &mut App.
            Overlay::WhyRed { .. } => {}
            Overlay::AppsActionMenu {
                app_name,
                env_names,
                cursor,
            } => draw_apps_action_menu(f, f.area(), app, app_name, env_names, *cursor),
            Overlay::ReportBug { body } => draw_report_bug_overlay(f, f.area(), app, body),
            Overlay::About(opened) => draw_about(f, f.area(), app, *opened),
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

fn truncate_for_display(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let prefix: String = s.chars().take(max.saturating_sub(1)).collect();
    format!("{prefix}…")
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
        app.view.redact,
    );
    let caller = redact(
        &app.context
            .caller_arn
            .as_deref()
            .map(short_caller)
            .unwrap_or_else(|| "—".into()),
        app.view.redact,
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
    let sort_dir = if app.view.sort_desc { "↓" } else { "↑" };
    let sort_label = format!("{}{}", app.view.sort_key.label(), sort_dir);
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
    if !app.view.filter().is_empty() {
        line2.push(sep(theme));
        let filter_text = app.view.filter().text().to_string();
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
    if !app.saved_views.is_empty() {
        let mut chips: Vec<Span> = vec![Span::styled("Views: ", Style::default().fg(theme.muted))];
        // "Active" derived by comparing the current filter against
        // each view's encoded `filter=` portion — matches the cycle
        // keybind's `cycle_saved_view` active-check. Operators with
        // legacy filter-only views (auto-migrated from 0.11
        // `named_filters`) still see them as chips, since those are
        // now stored as `view.NAME = "filter=..."`.
        if theme.icons == IconStyle::Powerline {
            let pills: Vec<(String, Color, Color)> = app
                .saved_views
                .iter()
                .map(|(name, encoded)| {
                    let active = !app.view.filter().is_empty()
                        && crate::app::view_filter_value(encoded) == app.view.filter().text();
                    let (fg, bg) = if active {
                        (theme.contrast_text(theme.title_alt), theme.title_alt)
                    } else {
                        (theme.muted, theme.row_alt_bg)
                    };
                    (name.to_string(), fg, bg)
                })
                .collect();
            chips.extend(pill_chain(&pills, theme));
        } else {
            for (name, encoded) in app.saved_views.iter() {
                let active = !app.view.filter().is_empty()
                    && crate::app::view_filter_value(encoded) == app.view.filter().text();
                chips.push(Span::styled(
                    format!(" {name} "),
                    if active {
                        Style::default()
                            .fg(theme.contrast_text(theme.title_alt))
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
            pill(scope_label, theme.contrast_text(theme.title), theme.title),
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
                    glyph(theme.icons, "★ ", "* "),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                )
            } else if selected {
                Span::styled(
                    glyph(theme.icons, "▶ ", "> "),
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
    let compact = app.view.mode == ViewMode::Compact;
    let spacious = app.view.mode == ViewMode::Spacious;
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
        ("INST", SortKey::Health),
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
            !app.view.hidden_cols.contains(*label)
        })
        .collect();
    let sort_marker = if app.view.sort_desc { " ▼" } else { " ▲" };
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
                (key, app.view.sort_key),
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
    let app_colors = &app.view.app_colors();

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
                    glyph(app.theme.icons, "★ ", "* ")
                } else {
                    ""
                };
                let checked = if app.multi_selected.contains(&e.name) {
                    glyph(app.theme.icons, "✓ ", "x ")
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
                    glyph(app.theme.icons, "▲ ", "! ")
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
                            (glyph(app.theme.icons, "◆ ", "# "), theme.title_alt)
                        } else if dur > chrono::Duration::days(30) {
                            (glyph(app.theme.icons, "◇ ", "o "), theme.muted)
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
                    // tf-managed badge: `ⓣ` (U+24E3) when the env
                    // appears in the discovered tfstate. Lookup is
                    // O(1) against the cached HashSet refreshed at
                    // startup + on context switch. Operators see at
                    // a glance which envs will drift after ebman-
                    // side mutations.
                    Span::styled(
                        if app.tf_managed_envs.contains(&e.name) {
                            glyph(app.theme.icons, "ⓣ ", "t ")
                        } else {
                            ""
                        },
                        Style::default()
                            .fg(theme.accent)
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
                            // Tier the `Ready` pill by the env's actual
                            // alert level — `Ready` is EB's operational
                            // state, not a health verdict. A green pill
                            // on a Red row reads as "fine"; render it in
                            // the health colour so the column matches
                            // reality. Updating / Terminating are kept as
                            // their own distinctive pills.
                            let alert = status_alert(&e.health, dlq);
                            if dlq > 0 {
                                Cell::from(Line::from(vec![
                                    status_pill_for(&e.status, &theme, alert),
                                    Span::styled(
                                        format!(" {}{dlq}", warn_glyph(theme.icons).trim_end()),
                                        Style::default()
                                            .fg(theme.health_red)
                                            .add_modifier(Modifier::BOLD),
                                    ),
                                ]))
                            } else {
                                Cell::from(status_pill_for(&e.status, &theme, alert))
                            }
                        }
                        "HEALTH" => Cell::from(health_dot(&e.health, &theme)),
                        "INST" => {
                            // `healthy/total` if the per-env counts have
                            // landed for this refresh; em-dash placeholder
                            // otherwise (and on the very first frame
                            // before the fan-out completes). Cell colour
                            // tiers by ratio: all healthy = green, any
                            // unhealthy but some healthy = yellow,
                            // zero healthy with instances present = red,
                            // empty env = muted.
                            let counts = app.env_instance_counts.get(&e.name).copied();
                            let (text, color) = format_instance_counts(counts, &theme);
                            Cell::from(Span::styled(
                                text,
                                Style::default().fg(color).add_modifier(Modifier::BOLD),
                            ))
                        }
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
                            // A newer platform version in the same family
                            // recolours the name amber + appends an ↑ glyph
                            // so the operator sees the console's "update
                            // available" nag without leaving the table.
                            // Staleness is precomputed in `rebuild_view` —
                            // this is an O(1) lookup, not a per-frame parse.
                            let stale = app.view.stale_platforms().get(&e.name);
                            let name_colour = if stale.is_some() {
                                theme.health_yellow
                            } else {
                                colour
                            };
                            let mut spans = Vec::new();
                            if let Some(g) = icon {
                                spans.push(Span::styled(
                                    format!("{g} "),
                                    Style::default().fg(colour),
                                ));
                            }
                            spans.push(Span::styled(
                                e.platform.as_str(),
                                Style::default().fg(name_colour),
                            ));
                            if stale.is_some() {
                                spans.push(Span::styled(
                                    format!(" {}", stale_glyph(theme.icons)),
                                    Style::default().fg(theme.health_yellow),
                                ));
                            }
                            Cell::from(Line::from(spans))
                        }
                        "VERSION" => Cell::from(Span::raw(e.version_label.as_str()))
                            .style(Style::default().fg(theme.app_palette[0])),
                        "CNAME" => Cell::from(redact(&e.cname, app.view.redact))
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
                                            .fg(theme.contrast_text(next_color))
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
            // " 99/99 " worst case = 7 cells incl. trailing pad. Most envs
            // are single-digit on each side so we sit at 4-5 typical width.
            "INST" => Constraint::Length(7),
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
                redact(&e.cname, app.view.redact),
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
        } else if app.view.filter().is_empty() {
            heading = "no envs match the active view".to_string();
            hint = "type `:views` to switch back to default, or `:filters` to drop a saved one"
                .to_string();
        } else {
            heading = format!("no envs match  `{}`", app.view.filter().text());
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
                theme.contrast_text(theme.accent),
                theme.accent,
            ),
            Span::raw(" "),
        ])),
        "Web" => Cell::from(Line::from(vec![
            pill(
                &format!("{web_icon} {:<label_width$}", "Web"),
                theme.contrast_text(theme.title),
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

/// Severity of the "this env is alerting" signal, used to colour-tier
/// the `Ready` status pill. `Ready` is EB's *operational* state
/// ("no lifecycle op in flight"), not a health verdict — so a bright
/// green pill on an env whose health is Red reads as "everything fine"
/// when it isn't. Pure classification: `Red` for Red/Severe health,
/// `Yellow` for the warning band or any non-empty DLQ on a worker,
/// `None` otherwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusAlert {
    None,
    Yellow,
    Red,
}

/// Pure: render the `INST` column for an env. `counts == None` means
/// the per-env fan-out hasn't landed yet — show an em-dash placeholder
/// in `theme.muted` so the column doesn't read as "0/0 (broken)".
/// `(0, 0)` is "env has no instances" (mid-launch or fully scaled
/// down) — `0/0` in `theme.muted`. Otherwise the cell colour-tiers:
/// `healthy == total` → `theme.health_green` (all good); `healthy >
/// 0 && healthy < total` → `theme.health_yellow` (partial); `healthy
/// == 0 && total > 0` → `theme.health_red` (everything's down).
pub fn format_instance_counts(
    counts: Option<crate::aws::EnvInstanceCounts>,
    theme: &Theme,
) -> (String, Color) {
    let Some(c) = counts else {
        return ("—".into(), theme.muted);
    };
    let text = format!("{}/{}", c.healthy, c.total);
    let color = if c.total == 0 {
        theme.muted
    } else if c.healthy == c.total {
        theme.health_green
    } else if c.healthy == 0 {
        theme.health_red
    } else {
        theme.health_yellow
    };
    (text, color)
}

/// Pure classifier: what alert tier the `Ready` pill should render in
/// for an env with the given health string + worker-DLQ depth.
pub fn status_alert(health: &str, dlq: i64) -> StatusAlert {
    if health.eq_ignore_ascii_case("Red") || health.eq_ignore_ascii_case("Severe") {
        StatusAlert::Red
    } else if health.eq_ignore_ascii_case("Yellow")
        || health.eq_ignore_ascii_case("Warning")
        || health.eq_ignore_ascii_case("Degraded")
        || dlq > 0
    {
        StatusAlert::Yellow
    } else {
        StatusAlert::None
    }
}

/// Render a status string as a coloured pill. Wrapper around
/// [`status_pill_for`] for callers that don't care about the alerting
/// distinction (Detail header, etc.).
fn status_pill(status: &str, theme: &Theme) -> Span<'static> {
    status_pill_for(status, theme, StatusAlert::None)
}

/// Variant of [`status_pill`] that knows whether the env is otherwise
/// alerting. When `alert` is `Yellow` / `Red`, the `Ready` pill renders
/// in the health colour (bold) instead of bright green — `Ready` means
/// "no lifecycle op in flight" per EB, NOT "everything is fine". A
/// green pill on a Red-tinted row gives the wrong at-a-glance read.
/// Updating / Terminating are unaffected — they already carry a strong
/// "something happening" signal that the operator wants to see in full.
fn status_pill_for(status: &str, theme: &Theme, alert: StatusAlert) -> Span<'static> {
    // Case-insensitive match without allocating a lowercase copy per
    // call — the table renderer hits this once per env-row per frame.
    if status.eq_ignore_ascii_case("ready") {
        match alert {
            StatusAlert::Red => Span::styled(
                " Ready ",
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD),
            ),
            StatusAlert::Yellow => Span::styled(
                " Ready ",
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            StatusAlert::None => pill(
                "Ready",
                theme.contrast_text(theme.status_ready),
                theme.status_ready,
            ),
        }
    } else if ieq_any(status, &["updating", "launching"]) {
        // Slow blink draws the eye to in-flight lifecycle ops without
        // changing the pill width or colour. Modern terminals (iTerm2,
        // Alacritty, Ghostty, etc.) support it; legacy ones silently
        // ignore the modifier and fall back to a static pill.
        Span::styled(
            format!(" {status} "),
            Style::default()
                .fg(theme.contrast_text(theme.status_updating))
                .bg(theme.status_updating)
                .add_modifier(Modifier::BOLD | Modifier::SLOW_BLINK),
        )
    } else if ieq_any(status, &["terminating", "terminated"]) {
        pill(
            status,
            theme.contrast_text(theme.status_terminating),
            theme.status_terminating,
        )
    } else {
        Span::styled(status.to_string(), Style::default().fg(theme.text))
    }
}

fn draw_events(f: &mut Frame, area: Rect, app: &mut App) {
    // Cursor-follow (same contract as every other list: the ▶ row
    // stays inside the viewport). `event_panel.scroll` was previously
    // dead state pinned at 0 — holding J walked the cursor below the
    // fold and every subsequent key (incl. `y` yank) operated on an
    // invisible row. Events render one line each (no wrap), so the
    // cursor index IS the line index.
    let body_rows = area.height.saturating_sub(2) as usize;
    app.event_panel.scroll = config_scroll_follow(
        app.event_panel.scroll,
        app.event_panel.cursor,
        body_rows,
        app.event_panel.events.len(),
    );
    let scope_suffix = match app.event_panel.for_env.as_deref() {
        Some(env) => format!(" · {env}"),
        None => " · all envs".to_string(),
    };
    let title = format!("Events  {}{}", app.event_panel.events.len(), scope_suffix);
    let block =
        titled_block(&app.theme, &title, true, app.theme.title).padding(Padding::horizontal(1));

    if app.event_panel.events.is_empty() {
        let lines = vec![
            Line::from(""),
            Line::from(Span::styled(
                format!("  {}  no events yet", glyph(app.theme.icons, "◌", "o")),
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
    let tw = event_time_width(app.event_panel.time_format);
    let lines: Vec<Line> = app
        .event_panel
        .events
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let when = format_event_time(e.at, app.event_panel.time_format, now);
            let sev_style = severity_style(&e.severity, &app.theme);
            let is_cursor = app.event_panel.cursor == Some(i);
            let marker = if is_cursor {
                glyph(app.theme.icons, "▶ ", "> ")
            } else {
                "  "
            };
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
        .scroll((app.event_panel.scroll, 0));
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
                glyph(theme.icons, "  ★ ", "  * "),
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
            top.extend(input_caret_spans(
                app.view.filter().text(),
                app.view.filter().cursor_col(),
                Style::default().fg(theme.text),
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
                theme,
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
            top.extend(input_caret_spans(
                app.command_input.text(),
                app.command_input.cursor_col(),
                Style::default().fg(theme.text),
                Style::default()
                    .fg(theme.title)
                    .add_modifier(Modifier::SLOW_BLINK),
                theme,
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
            top.extend(input_caret_spans(
                app.quickjump_input.text(),
                app.quickjump_input.cursor_col(),
                Style::default().fg(app.theme.text),
                Style::default()
                    .fg(app.theme.accent)
                    .add_modifier(Modifier::SLOW_BLINK),
                theme,
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
            } else if !app.view.filter().is_empty() {
                top.push(Span::styled(
                    format!(" filter: {}", app.view.filter().text()),
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
                crate::app::Focus::Events if app.event_panel.visible => {
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
            _ => " DLQ  j/k move  enter view body  r resend  R replay  x delete  p purge  m → MAIN  ^R refresh  ? help  esc / q back".into(),
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
        if dlq.confirm_delete_id.is_some() {
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
                // Char-safe truncation: the body is arbitrary producer
                // data, and a byte slice at 80 panics mid-draw when a
                // multi-byte char straddles the boundary.
                let preview = truncate_for_display(m.body.lines().next().unwrap_or(""), 80);
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
        // Typed text turns green once it exactly matches the env name.
        let typed_style = Style::default()
            .fg(if dlq.purge_typed.text() == dlq.env_name.as_str() {
                theme.health_green
            } else {
                theme.text
            })
            .add_modifier(Modifier::BOLD);
        let mut spans = vec![
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
        ];
        spans.extend(input_caret_spans(
            dlq.purge_typed.text(),
            dlq.purge_typed.cursor_col(),
            typed_style,
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::SLOW_BLINK),
            &theme,
        ));
        let line = Paragraph::new(Line::from(spans));
        f.render_widget(line, chunks[2]);
    } else if let Some(input) = &dlq.replay_input {
        let mut spans = vec![
            Span::styled(
                " REPLAY → main queue — ",
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "all / count (20) / window (1h 24h 7d): ",
                Style::default().fg(theme.muted),
            ),
        ];
        spans.extend(input_caret_spans(
            input.text(),
            input.cursor_col(),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::SLOW_BLINK),
            &theme,
        ));
        let line = Paragraph::new(Line::from(spans));
        f.render_widget(line, chunks[2]);
    } else {
        let keys = match dlq.viewing {
            crate::app::QueueView::Main => {
                " MAIN  j/k move  enter view body  x delete  m → DLQ  ^R refresh  esc / q back"
            }
            crate::app::QueueView::Dlq => {
                " DLQ  j/k move  enter view body  r resend  R replay  x delete  p purge  m → MAIN  ^R refresh  esc / q back"
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
            let popup = centered_overlay(OverlaySize::Small, area);
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
            let popup = centered_overlay(OverlaySize::Picker, area);
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
            let mut prompt_spans = vec![
                Span::styled(
                    " /",
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
            ];
            prompt_spans.extend(input_caret_spans(
                picker.filter.text(),
                picker.filter.cursor_col(),
                Style::default().fg(theme.text),
                Style::default()
                    .fg(theme.health_yellow)
                    .add_modifier(Modifier::SLOW_BLINK),
                &theme,
            ));
            let prompt = Paragraph::new(Line::from(prompt_spans)).block(block);
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
            let popup = centered_overlay(OverlaySize::Small, area);
            f.render_widget(Clear, popup);
            // Treat scale-to-zero as destructive at the modal level even
            // though `Action::Scale.destructive() == false` — dropping
            // an env to 0 instances serves zero requests, which is
            // operator-visible as severe as Terminate. `Action::Scale`
            // stays non-destructive in the type system so non-zero
            // scales (which are routine) don't get the alarming red
            // accent. (0.17.4 fix; the 0.17.2 patch added the SCALE-TO-
            // ZERO body copy without the matching modal styling.)
            let scale_to_zero = modal.action == Action::Scale
                && modal.scale_min == Some(0)
                && modal.scale_max == Some(0);
            let render_destructive = modal.action.destructive() || scale_to_zero;
            let accent = if render_destructive {
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
                Action::Scale => {
                    let min = modal.scale_min.unwrap_or(0);
                    let max = modal.scale_max.unwrap_or(0);
                    if min == 0 && max == 0 {
                        format!(
                            "SCALE TO ZERO: '{}' will serve 0 requests (`:start` to resume)",
                            modal.target_env
                        )
                    } else {
                        format!(
                            "Scale '{}' to min={min} / max={max}? (rolling)",
                            modal.target_env
                        )
                    }
                }
                Action::AbortUpdate => format!("Abort current update on '{}'?", modal.target_env),
                Action::SsmRun => {
                    let cmd = modal.ssm_run_command.as_deref().unwrap_or("?");
                    let n = modal
                        .ssm_run_instances
                        .as_ref()
                        .map(|v| v.len())
                        .unwrap_or(0);
                    format!(
                        "SSM-RUN: `{cmd}` on {n} instance{} of '{}' (treat as write)",
                        if n == 1 { "" } else { "s" },
                        modal.target_env
                    )
                }
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
                .fg(if render_destructive {
                    theme.health_red
                } else {
                    theme.text
                })
                .add_modifier(Modifier::BOLD);
            let name_style = if render_destructive {
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
            // tf-managed env warning: if the target appears in the
            // cached tfstate, the operator's about to drift the tf
            // state by mutating EB directly. Yellow not red — this
            // is a heads-up, not a block. Rendered just below
            // traffic_warning so state-level concerns stay grouped.
            if app.tf_managed_envs.contains(&modal.target_env) {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!(
                        "  {}env is terraform-managed — changes will drift on next plan/apply",
                        warn_glyph(theme.icons)
                    ),
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(Span::styled(
                    "    :drift to see what will diverge",
                    Style::default().fg(theme.muted),
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
            // Deploy-plan unavailability — capacity-impact derived
            // from deployment-policy + batch + ASG max. Yellow when
            // any instance unavailability, green/muted when zero.
            // Loading state stays silent rather than showing a
            // placeholder — keeps the modal compact when the fetch
            // hasn't landed yet.
            if let Some((body, caution)) = &modal.unavailability_line {
                let style = if *caution {
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.health_green)
                };
                lines.push(Line::from(Span::styled(format!("  {body}"), style)));
            }
            // Lint warnings — rule-keyed risks against the env's
            // pre-write state. Renders only Warn+ (Info issues
            // are noise in a confirm modal; the operator can
            // see them via `:lint` separately). Each issue gets
            // a header line `⚠ [EBL001] <title>` plus an
            // indented `→ <suggestion>` when one's available.
            // Skips the loading state silently — modal stays
            // compact while the spawn runs.
            if let Some(issues) = &modal.lint_issues {
                use crate::lint::Severity;
                let mut to_show: Vec<&crate::lint::Issue> = issues
                    .iter()
                    .filter(|i| i.severity >= Severity::Warn)
                    .collect();
                // Sort severity DESC then rule_id ASC so the operator
                // sees `Error` issues at the top of the modal — when
                // there are 3+ warnings, the worst one shouldn't be
                // buried at the bottom (0.19 review item).
                to_show.sort_by(|a, b| {
                    b.severity
                        .cmp(&a.severity)
                        .then_with(|| a.rule_id.cmp(&b.rule_id))
                });
                if !to_show.is_empty() {
                    lines.push(Line::from(""));
                    lines.push(Line::from(Span::styled(
                        "  lint warnings:",
                        Style::default().fg(theme.muted),
                    )));
                    for issue in to_show {
                        let sev_glyph = match issue.severity {
                            Severity::Error => glyph(theme.icons, "✗", "x"),
                            Severity::Warn => glyph(theme.icons, "⚠", "!"),
                            Severity::Info => glyph(theme.icons, "·", "-"),
                        };
                        let color = match issue.severity {
                            Severity::Error => theme.health_red,
                            _ => theme.health_yellow,
                        };
                        lines.push(Line::from(Span::styled(
                            format!("    {sev_glyph} [{}] {}", issue.rule_id, issue.title),
                            Style::default().fg(color).add_modifier(Modifier::BOLD),
                        )));
                        if let Some(suggestion) = &issue.suggestion {
                            lines.push(Line::from(Span::styled(
                                format!("      → {suggestion}"),
                                Style::default().fg(theme.muted),
                            )));
                        }
                    }
                }
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
            // Inline version preview for deploys — saves the operator
            // a separate `:deploy LABEL --preview` round-trip. Only
            // populated for Action::Deploy; renders below the recent-
            // events block. Indented to match the other modal blocks.
            if modal.loading_version_preview {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  fetching version metadata…",
                    Style::default().fg(theme.muted),
                )));
            } else if let Some(preview) = &modal.version_preview {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  pre-deploy preview:",
                    Style::default().fg(theme.muted),
                )));
                for raw in preview.lines() {
                    // The formatter prefixes warning lines with `⚠` (older-
                    // candidate rollback hint, unknown-label refusal).
                    // Match on the glyph to colour just those lines.
                    let style = if raw.contains('⚠') {
                        Style::default()
                            .fg(theme.health_yellow)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(theme.text)
                    };
                    lines.push(Line::from(Span::styled(format!("    {raw}"), style)));
                }
            }
            // Pre-deploy health-check probe outcome. Silence is
            // golden — only render when the probe found a problem.
            // Failed probe doesn't block the deploy; auto-rollback
            // is the catch-net, the warning is the heads-up.
            if modal.loading_health_check {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  probing health-check URL…",
                    Style::default().fg(theme.muted),
                )));
            } else if let Some(Err(reason)) = &modal.health_check_probe {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    format!("  ⚠ health-check probe: {reason}"),
                    Style::default()
                        .fg(theme.health_yellow)
                        .add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(Span::styled(
                    "    (deploy will proceed; consider --auto-rollback Nm if this matters)",
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
                    let matches = modal.typed.text() == modal.target_env.as_str();
                    let typed_style = Style::default()
                        .fg(if matches {
                            theme.health_green
                        } else {
                            theme.text
                        })
                        .add_modifier(Modifier::BOLD);
                    let mut typed_spans = vec![Span::raw("  ")];
                    typed_spans.extend(input_caret_spans(
                        modal.typed.text(),
                        modal.typed.cursor_col(),
                        typed_style,
                        Style::default()
                            .fg(theme.health_yellow)
                            .add_modifier(Modifier::SLOW_BLINK),
                        &theme,
                    ));
                    lines.push(Line::from(typed_spans));
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
        ActionFlow::Rollout(flow) => {
            let popup = centered_overlay(OverlaySize::Wide, area);
            f.render_widget(Clear, popup);
            let title = format!(
                " rollout {} — {} → {} ",
                flow.rollout_id, flow.env_name, flow.version_label
            );
            let block = ratatui::widgets::Block::default()
                .borders(ratatui::widgets::Borders::ALL)
                .title(title)
                .border_style(Style::default().fg(theme.title));
            let mut lines: Vec<Line> = Vec::new();
            // Header note keyed to current state.
            let state_line = match &flow.state {
                crate::mode_action::RolloutState::Planning => {
                    "  pre-flighting regions…".to_string()
                }
                crate::mode_action::RolloutState::AwaitingConfirm => {
                    let n_ok = flow
                        .regions
                        .iter()
                        .filter(|r| r.env_found == Some(true))
                        .count();
                    let n_total = flow.regions.len();
                    format!(
                        "  pre-flight complete ({n_ok}/{n_total} ok) — press y to dispatch, n / esc to abort"
                    )
                }
                crate::mode_action::RolloutState::Dispatching { next_index } => {
                    format!(
                        "  dispatching region {}/{}…",
                        next_index + 1,
                        flow.regions.len()
                    )
                }
                crate::mode_action::RolloutState::Done => {
                    let n_ok = flow
                        .regions
                        .iter()
                        .filter(|r| matches!(r.outcome, Some(Ok(()))))
                        .count();
                    let n_err = flow
                        .regions
                        .iter()
                        .filter(|r| matches!(r.outcome, Some(Err(_))))
                        .count();
                    let n_skipped = flow
                        .regions
                        .iter()
                        .filter(|r| r.outcome.is_none() && r.env_found != Some(false))
                        .count();
                    format!(
                        "  done — {n_ok} ok, {n_err} failed, {n_skipped} skipped (esc / q to close)"
                    )
                }
            };
            lines.push(Line::from(Span::styled(
                state_line,
                Style::default().fg(theme.muted),
            )));
            lines.push(Line::from(""));
            // Column header.
            lines.push(Line::from(Span::styled(
                "  REGION                ENV               CURRENT           TARGET            STATUS",
                Style::default().fg(theme.muted),
            )));
            for row in &flow.regions {
                // status_text is borrowed `&str` for the static
                // cases and `String` for the error case. To unify,
                // build it as `String` always and slice as `&str`
                // in the Span.
                let (status_text, status_color): (String, _) =
                    match (&row.outcome, row.env_found, &row.preflight_error) {
                        (Some(Ok(())), _, _) => (
                            format!("{} deployed", glyph(theme.icons, "✓", "+")),
                            theme.health_green,
                        ),
                        (Some(Err(_)), _, _) => {
                            // Short label only; the full error
                            // surfaces as an indented "↳" line below
                            // the row.
                            (
                                format!("{} failed", glyph(theme.icons, "✗", "x")),
                                theme.health_red,
                            )
                        }
                        (None, Some(false), Some(_)) => (
                            format!("{} pre-flight fail", glyph(theme.icons, "✗", "x")),
                            theme.health_red,
                        ),
                        (None, Some(false), None) => (
                            format!("{} env not found", glyph(theme.icons, "✗", "x")),
                            theme.health_red,
                        ),
                        (None, Some(true), _) => {
                            if matches!(
                                flow.state,
                                crate::mode_action::RolloutState::Dispatching { .. }
                            ) {
                                ("pending".to_string(), theme.muted)
                            } else {
                                (
                                    format!("{} pre-flight ok", glyph(theme.icons, "✓", "+")),
                                    theme.health_green,
                                )
                            }
                        }
                        (None, None, _) => ("planning…".to_string(), theme.muted),
                    };
                let current = row.current_version.as_deref().unwrap_or("…");
                let region_col = pad_right(&row.region, 22);
                let env_col = pad_right(&flow.env_name, 18);
                let current_col = pad_right(current, 18);
                let target_col = pad_right(&flow.version_label, 18);
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(region_col),
                    Span::raw(env_col),
                    Span::raw(current_col),
                    Span::raw(target_col),
                    Span::styled(
                        status_text.to_string(),
                        Style::default()
                            .fg(status_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                if let Some(err) = &row.preflight_error {
                    lines.push(Line::from(Span::styled(
                        format!("        ↳ {err}"),
                        Style::default().fg(theme.muted),
                    )));
                }
                if let Some(Err(e)) = &row.outcome {
                    lines.push(Line::from(Span::styled(
                        format!("        ↳ {e}"),
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

/// Pad a string to at least `width` chars with spaces. Uses
/// char-count rather than byte-count because Region / env names
/// can contain non-ASCII (rare but legal).
fn pad_right(s: &str, width: usize) -> String {
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

/// Render a single-line text input as `before-caret` + caret glyph +
/// `after-caret`, so the blinking caret sits at `cursor_col` (a char
/// offset) instead of always at the end. Shared by the `TextInput`-backed
/// input renderers (quickjump / palette / …) now that those inputs
/// support mid-string cursor movement. `cursor_col` past the end clamps
/// to the end (caret after all text).
fn input_caret_spans(
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

/// "Newer platform version available" glyph — `↑` in unicode/powerline,
/// `^` in ascii. Flags stale platforms in the envs-table PLATFORM column.
fn stale_glyph(icons: IconStyle) -> &'static str {
    match icons {
        IconStyle::Ascii => "^",
        _ => "↑",
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

// `OverlaySize`, `centered_overlay`, `centered_rect`, and `overlay_dims`
// live in the shared `tui-common` crate. Re-exported from
// `crate::overlay` in `lib.rs`; call sites continue to use
// `OverlaySize::Picker` etc. through the `use` statement at the top
// of this file.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warn_glyph_falls_back_in_ascii() {
        assert_eq!(super::warn_glyph(IconStyle::Ascii), "! ");
        assert_eq!(super::warn_glyph(IconStyle::Unicode), "⚠ ");
        assert_eq!(super::warn_glyph(IconStyle::Powerline), "⚠ ");
    }

    // Overlay-sizing invariants moved with the code to `tui-common::overlay`'s
    // own test module; no need to keep parallel copies here.

    #[test]
    fn stale_glyph_falls_back_in_ascii() {
        assert_eq!(super::stale_glyph(IconStyle::Ascii), "^");
        assert_eq!(super::stale_glyph(IconStyle::Unicode), "↑");
    }

    #[test]
    fn detail_keystrip_has_no_duplicate_keys() {
        for tab in [
            DetailTab::Health,
            DetailTab::Events,
            DetailTab::Instances,
            DetailTab::Metrics,
            DetailTab::Queue,
            DetailTab::Logs,
            DetailTab::Config,
        ] {
            assert!(
                !detail_tab_keys(tab).is_empty(),
                "no tab-specific keys for {tab:?}"
            );
            let mut seen = std::collections::HashSet::new();
            for (key, _) in detail_tab_keys(tab).iter().chain(DETAIL_GLOBAL_KEYS.iter()) {
                assert!(
                    seen.insert(*key),
                    "key {key:?} listed twice on the {tab:?} strip"
                );
            }
        }
    }

    #[test]
    fn render_detail_keystrip_carries_tab_name_and_global_keys() {
        let theme = Theme::dark();
        let line = render_detail_keystrip(DetailTab::Config, &theme);
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(text.contains("CONFIG"), "missing tab name: {text:?}");
        assert!(
            text.contains("rename"),
            "missing tab-specific key: {text:?}"
        );
        assert!(text.contains("esc"), "missing global key: {text:?}");
    }

    #[test]
    fn status_alert_tiers_by_health_and_dlq() {
        // Red / Severe → red, regardless of DLQ.
        assert_eq!(status_alert("Red", 0), StatusAlert::Red);
        assert_eq!(status_alert("Severe", 0), StatusAlert::Red);
        assert_eq!(status_alert("severe", 99), StatusAlert::Red);
        // Yellow band → yellow.
        assert_eq!(status_alert("Yellow", 0), StatusAlert::Yellow);
        assert_eq!(status_alert("Warning", 0), StatusAlert::Yellow);
        assert_eq!(status_alert("Degraded", 0), StatusAlert::Yellow);
        // Green env with worker DLQ items → yellow (warning, not alarm).
        assert_eq!(status_alert("Green", 1), StatusAlert::Yellow);
        assert_eq!(status_alert("Ok", 50), StatusAlert::Yellow);
        // Healthy env, no DLQ → no alert tier.
        assert_eq!(status_alert("Green", 0), StatusAlert::None);
        assert_eq!(status_alert("Ok", 0), StatusAlert::None);
        assert_eq!(status_alert("", 0), StatusAlert::None);
        // Red health *with* DLQ stays Red (Red wins over Yellow).
        assert_eq!(status_alert("Red", 200), StatusAlert::Red);
    }

    #[test]
    fn format_instance_counts_tiers_by_ratio() {
        use crate::aws::EnvInstanceCounts;
        let theme = crate::theme::Theme::dark();

        // Missing data → em-dash + muted (avoids "0/0 = broken" misread).
        assert_eq!(format_instance_counts(None, &theme).0, "—");
        assert_eq!(format_instance_counts(None, &theme).1, theme.muted);

        // Empty env (no instances right now) → muted, not red.
        let (text, color) = format_instance_counts(
            Some(EnvInstanceCounts {
                healthy: 0,
                total: 0,
            }),
            &theme,
        );
        assert_eq!(text, "0/0");
        assert_eq!(color, theme.muted);

        // All healthy → green.
        let (text, color) = format_instance_counts(
            Some(EnvInstanceCounts {
                healthy: 3,
                total: 3,
            }),
            &theme,
        );
        assert_eq!(text, "3/3");
        assert_eq!(color, theme.health_green);

        // Partial (some healthy, some not) → yellow.
        let (text, color) = format_instance_counts(
            Some(EnvInstanceCounts {
                healthy: 2,
                total: 3,
            }),
            &theme,
        );
        assert_eq!(text, "2/3");
        assert_eq!(color, theme.health_yellow);

        // None healthy with instances present → red.
        let (text, color) = format_instance_counts(
            Some(EnvInstanceCounts {
                healthy: 0,
                total: 1,
            }),
            &theme,
        );
        assert_eq!(text, "0/1");
        assert_eq!(color, theme.health_red);
    }

    #[test]
    fn status_pill_for_alerting_returns_text_without_pill_bg() {
        // Pins the 0.6 visual fix (commit bf64ce0): when an env is in an
        // alert tier (Red / Yellow), the `Ready` pill must render as
        // styled text — fg = health colour, no bg — so the row's red /
        // yellow tint shows through. A solid-green pill on an alerting
        // row reads as "fine" at a glance, which the fix exists to stop.
        let theme = crate::theme::Theme::dark();

        let red = super::status_pill_for("Ready", &theme, super::StatusAlert::Red);
        assert_eq!(
            red.style.fg,
            Some(theme.health_red),
            "Red alert: fg = theme.health_red"
        );
        assert!(
            red.style.bg.is_none(),
            "Red alert: no bg (so row tint shows through), got {:?}",
            red.style.bg,
        );

        let yellow = super::status_pill_for("Ready", &theme, super::StatusAlert::Yellow);
        assert_eq!(yellow.style.fg, Some(theme.health_yellow));
        assert!(
            yellow.style.bg.is_none(),
            "Yellow alert: no bg, got {:?}",
            yellow.style.bg,
        );

        // No alert: the bright-green Ready pill (with explicit bg) is
        // still what we want.
        let none = super::status_pill_for("Ready", &theme, super::StatusAlert::None);
        assert_eq!(
            none.style.bg,
            Some(theme.status_ready),
            "No alert: solid green pill"
        );
    }

    #[test]
    fn why_overlay_title_is_framed_by_health() {
        // Red / Severe → diagnostic framing.
        assert_eq!(why_overlay_title("prod", "Red"), "why is prod red?");
        assert_eq!(why_overlay_title("prod", "Severe"), "why is prod red?");
        // Yellow family → amber framing (matches operator vocabulary).
        assert_eq!(why_overlay_title("prod", "Yellow"), "why is prod amber?");
        assert_eq!(why_overlay_title("prod", "Warning"), "why is prod amber?");
        assert_eq!(why_overlay_title("prod", "Degraded"), "why is prod amber?");
        // Green / Ok / Info / NoData / Pending / Grey / unknown / blank
        // → neutral "recent activity" framing (operator's just looking).
        assert_eq!(why_overlay_title("prod", "Green"), "prod — recent activity");
        assert_eq!(why_overlay_title("prod", "Ok"), "prod — recent activity");
        assert_eq!(why_overlay_title("prod", ""), "prod — recent activity");
        // Health string is matched case-insensitively (EB's casing varies).
        assert_eq!(why_overlay_title("prod", "red"), "why is prod red?");
        assert_eq!(why_overlay_title("prod", "yellow"), "why is prod amber?");
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
        // ABOUT_SCENE_W is 40, ABOUT_TEXT_W is 58; thresholds:
        // Stacked needs w >= max(40, 58) + 6 = 64,
        // SideBySide needs w >= 40 + 58 + 8 = 106.
        let th = 15;
        // Roomy → scene stacked above text.
        assert_eq!(about_layout(120, 60, th), AboutLayout::Stacked);
        // Wide but short → scene beside text.
        assert_eq!(about_layout(140, 30, th), AboutLayout::SideBySide);
        // Small both ways → text only.
        assert_eq!(about_layout(40, 20, th), AboutLayout::TextOnly);
        // Wide enough to stack but too narrow for side-by-side → text only.
        assert_eq!(about_layout(44, 60, th), AboutLayout::TextOnly);
        // Tall enough but too narrow for the scene → text only.
        assert_eq!(about_layout(42, 60, th), AboutLayout::TextOnly);
        // 50 cols would have picked Stacked under the old 46-col
        // threshold, but the text lines need 58 cols to render
        // without mid-word wrap. Must fall through to TextOnly.
        assert_eq!(about_layout(50, 60, th), AboutLayout::TextOnly);
        // Right at the new Stacked boundary.
        assert_eq!(about_layout(64, 60, th), AboutLayout::Stacked);
        assert_eq!(about_layout(63, 60, th), AboutLayout::TextOnly);
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
                solution_stack: "".into(),
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
                solution_stack: "".into(),
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

    /// Regression test pinning ratatui 0.29's broken OSC 8 behavior.
    /// Verified by experiment: each byte of an escape sequence
    /// (including the leading `\x1b`) is treated as a 1-cell-wide
    /// printing character — there's no special handling for control
    /// sequences in ratatui's `Buffer::set_stringn` path. The
    /// consequences for OSC 8 hyperlinks:
    ///
    /// - The 24-byte opener `\x1b]8;;https://example.com\x1b\\`
    ///   consumes 24 cells of layout space.
    /// - The visible text `Click` gets pushed past the buffer width
    ///   (or past the column the caller intended).
    /// - The escape bytes get rendered as visible control characters
    ///   in terminals that don't recognise them mid-cell.
    ///
    /// This test pins the broken behavior so that if a future
    /// ratatui upgrade adds OSC 8 (or zero-width control) support,
    /// it will fail loudly and prompt us to revisit the feature.
    /// Currently shipping OSC 8 would require a custom widget that
    /// bypasses ratatui's diff renderer, which is too invasive
    /// for the value — see BACKLOG.
    #[test]
    fn osc8_in_span_is_split_into_per_byte_cells_ratatui_0_29_limitation() {
        use ratatui::buffer::Buffer;
        use ratatui::layout::Rect;
        use ratatui::text::{Line, Span};
        use ratatui::widgets::{Paragraph, Widget};
        let osc8 = "\x1b]8;;https://example.com\x1b\\Click\x1b]8;;\x1b\\";
        let para = Paragraph::new(Line::from(Span::raw(osc8)));
        let mut buf = Buffer::empty(Rect::new(0, 0, 20, 1));
        para.render(buf.area, &mut buf);
        // Cell 0 must hold the ESC byte (proves each escape byte
        // is taking a full cell, not being zero-width or merged).
        assert_eq!(buf[(0, 0)].symbol(), "\x1b");
        // The URL chars get spread across cells 5..19. "Click" never
        // makes it into the visible buffer — proof that ratatui treats
        // every escape byte as 1 cell of layout width.
        let rendered = buffer_to_string(&buf);
        assert!(
            !rendered.contains("Click"),
            "If this fails, ratatui learned about OSC 8 — revisit the BACKLOG entry. Got: {rendered:?}"
        );
        // The escape framing reaches the buffer (bytes are preserved
        // per cell) but spread across cells in a way that won't
        // assemble into a hyperlink at terminal render time.
        assert!(rendered.contains('\x1b'));
        assert!(rendered.contains("]8;;"));
    }
}
