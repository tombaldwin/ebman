//! The Detail view: tab strip + per-tab renderers (health, events,
//! instances, metrics, queue, logs, config) — carved out of the 9,400-line `ui.rs` root (0.27
//! architecture pass, the same `app/` submodule pattern). Items are
//! `pub(super)`; the root glob-imports them so call sites and tests
//! are untouched. Shared chrome helpers (blocks, pills, glyphs,
//! `centered_overlay`) stay in the root and reach here via
//! `use super::*`.

use super::*;

pub(super) fn draw_detail(f: &mut Frame, area: Rect, app: &mut App) {
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

    let cname_text = redact(&env.cname, app.view.redact);
    let mut h2 = kv("Platform", &env.platform, theme);
    if let Some(newer) = app.view.stale_platforms().get(&env.name) {
        h2.push(Span::styled(
            format!("  {}v{newer} available", stale_glyph(theme.icons)),
            Style::default()
                .fg(theme.health_yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
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
                app.event_panel.time_format,
            ));
        }
        DetailTab::Instances => draw_detail_instances(f, body_area, detail, &app.theme),
        DetailTab::Metrics => draw_detail_metrics(f, body_area, detail, &app.theme),
        DetailTab::Queue => draw_detail_queue(f, body_area, detail, app.view.redact, &app.theme),
        DetailTab::Logs => draw_detail_logs(f, body_area, detail, &app.theme),
        DetailTab::Config => {
            config_scroll = Some(draw_detail_config(
                f,
                body_area,
                env,
                detail,
                app.view.redact,
                &app.cfg.required_tags,
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
        pill(
            "AUTO",
            app.theme.contrast_text(app.theme.health_green),
            app.theme.health_green,
        )
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
        render_detail_keystrip(active_tab, &app.theme),
    ]);
    f.render_widget(footer, chunks[3]);
}

/// Tab-specific `(key, label)` pairs for the Detail footer key strip.
/// The global keys (`tab` / `?` / `esc`) are appended uniformly by
/// `render_detail_keystrip`, so each tab only declares what's unique to it.
pub(super) fn detail_tab_keys(tab: DetailTab) -> &'static [(&'static str, &'static str)] {
    match tab {
        DetailTab::Health => &[
            ("j/k", "move"),
            ("enter", "drill"),
            ("a", "actions"),
            ("^R", "refresh"),
        ],
        DetailTab::Instances => &[
            ("j/k", "cursor"),
            ("s", "ssm shell"),
            ("i", "info"),
            ("y", "yank id"),
            ("x", "terminate"),
            ("a", "actions"),
            ("^R", "refresh"),
        ],
        DetailTab::Events => &[
            ("j/k", "scroll"),
            ("/", "filter"),
            ("n/N", "next"),
            ("L", "level"),
            ("w", "window"),
            ("T", "time"),
            ("^R", "refresh"),
        ],
        DetailTab::Metrics => &[
            ("[ ]", "range"),
            ("hover", "values"),
            ("R", "auto-refresh"),
            ("a", "actions"),
            ("^R", "refresh"),
        ],
        DetailTab::Queue => &[
            ("j/k", "Main/DLQ"),
            ("enter", "view"),
            ("d", "DLQ"),
            ("^R", "refresh"),
        ],
        DetailTab::Logs => &[("^R", "snapshot"), ("s", "live-stream"), ("/", "filter")],
        DetailTab::Config => &[
            ("j/k", "move"),
            ("enter", "edit"),
            ("r", "rename"),
            ("n", "new"),
            ("x", "delete"),
            ("a", "actions"),
            ("^R", "refresh"),
        ],
    }
}

/// Keys that behave identically on every Detail tab. Appended after the
/// tab-specific set so the strip always ends `tab · ? · esc`.
pub(super) const DETAIL_GLOBAL_KEYS: &[(&str, &str)] =
    &[("tab", "tabs"), ("?", "help"), ("esc", "back")];

/// Build the Detail footer key strip, lazygit-style: a bold tab name,
/// then `key`/`label` pairs where the key is bold + bright and the label
/// muted, separated by a thin dim `·`. The visual key/label contrast lets
/// the operator scan keys without the strip needing extra width.
pub(super) fn render_detail_keystrip(tab: DetailTab, theme: &Theme) -> Line<'static> {
    let key_style = Style::default().fg(theme.text).add_modifier(Modifier::BOLD);
    let label_style = Style::default().fg(theme.muted);
    let sep_style = Style::default().fg(theme.border_idle);
    let mut spans: Vec<Span> = vec![Span::styled(
        format!(" {} ", tab.title().to_uppercase()),
        Style::default()
            .fg(theme.title)
            .add_modifier(Modifier::BOLD),
    )];
    for (i, (key, label)) in detail_tab_keys(tab)
        .iter()
        .chain(DETAIL_GLOBAL_KEYS.iter())
        .enumerate()
    {
        spans.push(Span::styled(if i == 0 { " " } else { " · " }, sep_style));
        spans.push(Span::styled(*key, key_style));
        spans.push(Span::raw(" "));
        spans.push(Span::styled(*label, label_style));
    }
    Line::from(spans)
}

pub(super) struct DetailFooterState {
    auto_refresh: bool,
    error: Option<String>,
    loading_events: bool,
    loading_instances: bool,
    loading_queues: bool,
    loading_metrics: bool,
    log_stage: crate::app::LogTailStage,
}

pub(super) fn render_tabs(tabs: &[DetailTab], active: usize, theme: &Theme) -> Line<'static> {
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
            let fg = if is_active {
                theme.contrast_text(active_bg)
            } else {
                theme.muted
            };
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
                .fg(theme.contrast_text(theme.border_active))
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
pub(super) fn draw_detail_health(
    f: &mut Frame,
    area: Rect,
    detail: &crate::app::DetailState,
    app: &App,
) {
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
        // Last check failed → the depth is the last-known value, not
        // a live reading. Say so instead of presenting it as current.
        let stale_suffix = if app.worker_dlq_stale.contains(&env.name) {
            " (stale)"
        } else {
            ""
        };
        status_line.push(Span::raw("   "));
        status_line.push(Span::styled(
            format!("{}DLQ:{dlq_depth}{stale_suffix}", warn_glyph(theme.icons)),
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
    // Cost suffix — when `:cost on` has populated app.costs, append
    // a `cost: $NN/mo` chip to the status line so spend lives alongside
    // health in the same scanline. Same bucket palette as the COST
    // column + `:why` overlay (green / muted / red) for cross-view
    // consistency. Hidden when cost tracking isn't enabled — keeps
    // the line layout stable for operators who don't care about cost.
    if let Some(cost) = app.costs.get(&env.name).copied() {
        let bucket_fg = if cost >= 500.0 {
            theme.health_red
        } else if cost >= 50.0 {
            theme.text
        } else {
            theme.health_green
        };
        status_line.push(Span::raw("   "));
        status_line.push(Span::styled("cost: ", Style::default().fg(theme.muted)));
        status_line.push(Span::styled(
            format!("${cost:.0}/mo"),
            Style::default().fg(bucket_fg).add_modifier(Modifier::BOLD),
        ));
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
pub(super) fn draw_detail_events(
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
            Span::styled(
                detail.search_input.text().to_string(),
                Style::default().fg(theme.text),
            ),
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
                format!(" {}  no events for this env", glyph(theme.icons, "◌", "o")),
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
                format!(
                    " {}  no events match filter ({} hidden)",
                    glyph(theme.icons, "◌", "o"),
                    total
                ),
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
                // Theme-derived highlight — the pill fix's pattern
                // (hardcoded Black-on-Yellow breaks the moment a
                // theme's palette shifts, e.g. Solarized-light).
                Style::default()
                    .fg(theme.contrast_text(theme.health_yellow))
                    .bg(theme.health_yellow)
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

pub(super) fn draw_detail_instances(
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
                format!("  {}  no instance data", glyph(theme.icons, "◌", "o")),
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
        let marker = if is_cursor {
            glyph(theme.icons, "▶ ", "> ")
        } else {
            "  "
        };
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

pub(super) fn draw_detail_metrics(
    f: &mut Frame,
    area: Rect,
    detail: &crate::app::DetailState,
    theme: &Theme,
) {
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

pub(super) fn delta_span(delta: f64, id: &str, theme: &Theme) -> Span<'static> {
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

pub(super) fn format_metric(id: &str, v: f64) -> String {
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

pub(super) fn humanize_range(secs: i64) -> String {
    if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

pub(super) fn draw_detail_queue(
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
                glyph(theme.icons, "▶ ", "> "),
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

pub(super) fn draw_detail_logs(
    f: &mut Frame,
    area: Rect,
    detail: &crate::app::DetailState,
    theme: &Theme,
) {
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
                    Span::styled(
                        tail.search_input.text().to_string(),
                        Style::default().fg(theme.text),
                    ),
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
pub(super) fn config_new_row_line(edit: &crate::app::ConfigEdit, theme: &Theme) -> Line<'static> {
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
pub(super) fn config_editable_row(
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
    // An existing-row edit (Value or RenameKey) draws inside this
    // row; the add-new-row editor renders separately as its own line.
    let editing = detail.config_edit.as_ref().filter(|e| {
        e.mode != crate::app::ConfigEditMode::NewRow && e.kind == item.kind && e.key == key
    });
    let is_cursor = detail.config_cursor == this && editing.is_none();
    let marker = if is_cursor {
        glyph(theme.icons, "▶ ", "> ")
    } else {
        "  "
    };
    let editor_style = Style::default()
        .fg(theme.title_alt)
        .add_modifier(Modifier::BOLD);

    // Key cell — the in-place editor when this row's *key* is being
    // renamed, otherwise the fixed key text.
    let renaming = matches!(
        editing.map(|e| e.mode),
        Some(crate::app::ConfigEditMode::RenameKey)
    );
    let key_span = if let (true, Some(e)) = (renaming, editing) {
        let (before, after) = e.split_at_caret();
        Span::styled(
            format!("{marker}{before}{}{after}", caret_glyph(theme)),
            editor_style,
        )
    } else {
        let key_len = key.chars().count();
        let key_text = if key_len <= key_width {
            format!("{marker}{key:<key_width$}")
        } else {
            // Long key overflows its column — wrap it onto its own line
            // so the value still aligns. Marker stays on the first row.
            format!("{marker}{key}\n  {pad:<key_width$}", pad = "")
        };
        let key_style = if is_cursor {
            Style::default().fg(key_color).add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(key_color)
        };
        Span::styled(key_text, key_style)
    };

    // Value cell — the in-place editor when this row's *value* is
    // being edited, otherwise the plain value.
    let value_span = match editing.map(|e| e.mode) {
        Some(crate::app::ConfigEditMode::Value) => {
            let e = editing.expect("editing is Some in this arm");
            let (before, after) = e.split_at_caret();
            Span::styled(
                format!("{before}{}{after}", caret_glyph(theme)),
                editor_style,
            )
        }
        _ => {
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
    let mut spans = vec![key_span, Span::raw("  "), value_span];
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
pub(super) fn draw_detail_config(
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
    if let Some(e) = detail.config_edit.as_ref().filter(|e| {
        e.mode == crate::app::ConfigEditMode::NewRow && e.kind == crate::app::ConfigItemKind::Tag
    }) {
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
    if let Some(e) = detail.config_edit.as_ref().filter(|e| {
        e.mode == crate::app::ConfigEditMode::NewRow && e.kind == crate::app::ConfigItemKind::EnvVar
    }) {
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
pub(super) fn config_scroll_follow(
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
