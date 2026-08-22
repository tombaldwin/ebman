//! The events panel and the severity / timestamp formatting it needs.
//!
//! Moved verbatim out of `src/ui.rs` (5,046 lines) in the 0.31 split;
//! visibility widened to `pub(crate)` where a sibling needs it, and
//! nothing else changed. `use super::*` picks up the shared vocabulary
//! the way `detail.rs` and `overlays.rs` already do.

use super::*;

/// Colour for an event's severity, shared by every surface that renders
/// an EB event list.
///
/// One function because the map was inlined in three places and they
/// had already drifted: adding the `:event-tail` gap sentinel to one of
/// them left the others rendering it in `muted`, the dimmest colour in
/// the palette and indistinguishable from routine INFO chatter.
pub fn event_severity_style(severity: &str, theme: &Theme) -> Style {
    match severity.to_uppercase().as_str() {
        "ERROR" | "FATAL" => Style::default().fg(theme.health_red),
        "WARN" => Style::default().fg(theme.health_yellow),
        s if s == crate::app::EVENT_TAIL_GAP_SEVERITY => Style::default()
            .fg(theme.health_yellow)
            .add_modifier(Modifier::BOLD),
        _ => Style::default().fg(theme.muted),
    }
}

pub(crate) fn draw_events(f: &mut Frame, area: Rect, app: &mut App) {
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

pub(crate) fn env_label(e: &crate::aws::Event) -> String {
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

pub(crate) fn severity_style(s: &str, theme: &Theme) -> Style {
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

pub(crate) fn humanize_age(d: chrono::Duration) -> String {
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
pub(crate) fn format_event_time(
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
pub(crate) fn event_time_width(mode: crate::app::EventTimeFormat) -> usize {
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
pub(crate) fn age_color(
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
