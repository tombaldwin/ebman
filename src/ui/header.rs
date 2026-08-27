//! The top chrome — the pill chain, the context breadcrumb, and the
//! width arithmetic that decides how much of it fits.
//!
//! Moved verbatim out of `src/ui.rs` (5,046 lines) in the 0.31 split;
//! visibility widened to `pub(crate)` where a sibling needs it, and
//! nothing else changed. `use super::*` picks up the shared vocabulary
//! the way `detail.rs` and `overlays.rs` already do.

use super::*;

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
pub(crate) fn pill_chain(items: &[(String, Color, Color)], theme: &Theme) -> Vec<Span<'static>> {
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
pub(crate) fn build_chain_pills(app: &App) -> Vec<(String, Color, Color)> {
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
            format!(
                "{}rollback {env} in {remaining}",
                rollback_timer_glyph(theme)
            )
        } else {
            format!(
                "{}{} rollbacks armed (next: {env} in {remaining})",
                rollback_timer_glyph(theme),
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
            format!("{}watching {env} {remaining}", watching_glyph(theme))
        } else {
            format!(
                "{}{} watching (next: {env} {remaining})",
                watching_glyph(theme),
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
    } else if let Some(days) = app
        .release_date
        .and_then(|d| build_age_days(d, chrono::Utc::now()))
        .filter(|d| *d >= STALE_BUILD_DAYS)
    {
        // Only when `update_available` is None. If crates.io answered
        // and named a newer version, that is strictly better
        // information than "your build is old" and this would just be
        // a second pill saying the same thing worse.
        //
        // The value here is the case the crates.io check cannot cover:
        // offline, or behind a proxy that eats the request. There a
        // failed check is indistinguishable from "you are up to date",
        // and this is the only thing that will ever say otherwise.
        chain.push((
            format!("build is {days}d old (:update)"),
            fg(theme.health_grey),
            theme.health_grey,
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

/// Decides the header's vertical footprint and whether the contextual pill
/// chain fits on the info row (`line2`) at this terminal width. When the
/// chain fits, the dedicated 4th row is dropped to save vertical space.
///
/// Returns `(header_rows, merge_pills)`.
pub(crate) fn header_layout(app: &App, area_width: u16) -> (u16, bool) {
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
pub(crate) fn prune_pills_to_width(
    pills: &mut Vec<(String, Color, Color)>,
    theme: &Theme,
    max_w: usize,
) {
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
pub(crate) fn header_dimensions(
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
pub(crate) fn estimated_info_row_width(app: &App) -> usize {
    const STATUS_SLOT: usize = 10;
    let sep_w = 5; // both "  •  " and "  ❘  " render at 5 cols
    let sort_dir = if app.view.sort_desc() {
        glyph(app.theme.icons, "↓", "v")
    } else {
        glyph(app.theme.icons, "↑", "^")
    };
    let sort_label = format!("{}{}", app.view.sort_key().label(), sort_dir);
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

pub(crate) fn draw_header(f: &mut Frame, area: Rect, app: &App, merge_pills: bool) {
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

    // The info block's inner width: its own two borders come off the
    // column it was given.
    let usable_width = cols[0].width.saturating_sub(2);
    // Drop whole fields rather than let ratatui clip the right edge.
    // Clipping leaves `Profile: ` with the value gone, which an
    // operator reads as an empty profile rather than as a narrow
    // terminal.
    let line1 = join_fields_to_fit(
        vec![
            kv("Account", &account, theme),
            kv("Region", &app.context.region, theme),
            kv("Profile", &profile, theme),
        ],
        theme,
        usable_width,
    );
    // Ordering on this row matters under width pressure: ratatui clips
    // the right edge when content exceeds the column, so anything the
    // operator needs ALWAYS visible (Sort, Status) goes first. Caller +
    // Last get pushed right so they're the first to clip on narrow
    // terminals — we'd rather lose "20s ago" than lose "↑app".
    let sort_dir = if app.view.sort_desc() {
        glyph(app.theme.icons, "↓", "v")
    } else {
        glyph(app.theme.icons, "↑", "^")
    };
    let sort_label = format!("{}{}", app.view.sort_key().label(), sort_dir);
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
        let arrow = if *delta > 0 {
            glyph(theme.icons, "▲", "^")
        } else {
            glyph(theme.icons, "▼", "v")
        };
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
    let info = Paragraph::new(paragraph_lines).block(titled_block(
        theme,
        &version_title(theme, cols[0].width),
        false,
        theme.title,
    ));
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
            // Whole hints only. Clipped, this read `<:> com` — a chord
            // with its action cut in half, which is worse than one
            // hint fewer. Same treatment as the footer key strip.
            hints_to_fit(
                "<tab> scope  <?> help  <:> command  </> filter  <q> quit",
                cols[1].width.saturating_sub(2),
            ),
            Style::default().fg(theme.muted),
        )),
    ])
    .alignment(Alignment::Right)
    .block(rounded_block(theme, false));
    f.render_widget(context_panel, cols[1]);
}

pub(crate) fn breadcrumb_line(app: &App) -> Line<'static> {
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
    // The crumb names an env, so it has to name THAT env's region.
    // It used to show `context.region` unconditionally, which was
    // accidentally truthful while Detail rendered home-region data —
    // and became a lie the moment Detail started fetching from the
    // row's region. `us-east-1 / uflexi / api-prod` for an env that
    // lives in eu-west-2 is exactly the confusion this release exists
    // to remove. With no env named, the session's region is the right
    // answer.
    let env = match (app.mode, app.detail.as_ref()) {
        (Mode::Detail, Some(d)) => Some((
            d.env_snapshot.application.clone(),
            d.env_name.clone(),
            app.region_for(&d.env_snapshot),
        )),
        _ => app
            .selected_env()
            .map(|e| (e.application.clone(), e.name.clone(), app.region_for(e))),
    };
    let region = env
        .as_ref()
        .map(|(_, _, r)| r.clone())
        .unwrap_or_else(|| app.context.region.clone());
    let mut spans: Vec<Span<'static>> = vec![Span::styled(
        region,
        Style::default()
            .fg(theme.title)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some((app_name, env_name, _)) = env {
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

pub(crate) fn short_caller(arn: &str) -> String {
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
pub(crate) fn highlight_env_in_summary(
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
pub(crate) fn context_hint(app: &App) -> Option<String> {
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
pub(crate) fn summarize_in_flight(labels: &[&str]) -> String {
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
pub(crate) fn label_stem(word: &str) -> &'static str {
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

/// Pure: render the header "last refresh" label as Grafana-style
/// relative time — `12s ago · next 3s`. Cheaper visual scan than the
/// absolute `HH:MM:SS (every Ns)` it replaces. Returns the format
/// untouched when `last_refresh` is `None` (haven't refreshed yet).
///
/// The `next` countdown can go negative when a refresh is overdue
/// (throttled, network slow, frozen with `f`); we clamp it to `0s` and
/// the operator sees the indicator continue to tick up the `… ago`.
pub(crate) fn format_refresh_label(
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
