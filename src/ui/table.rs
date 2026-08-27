//! The environments table and the applications table, plus the cell
//! renderers (tier, status pill, platform style) they share.
//!
//! Moved verbatim out of `src/ui.rs` (5,046 lines) in the 0.31 split;
//! visibility widened to `pub(crate)` where a sibling needs it, and
//! nothing else changed. `use super::*` picks up the shared vocabulary
//! the way `detail.rs` and `overlays.rs` already do.

use super::*;

pub(crate) fn draw_apps_table(f: &mut Frame, area: Rect, app: &mut App) {
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

/// Which columns the table shows, in order.
///
/// Pure: the whole rule set — view-mode presets, the fan-out-only
/// REGION column, the `:cost on` opt-in, and the per-column hide list —
/// decided from four inputs and nothing else. It was 43 lines inside a
/// 695-line `draw_table`, which meant the only way to check "does
/// `:cols hide NAME` actually do nothing" was to render a frame.
pub(crate) fn visible_columns(
    multi_regions: &[String],
    cost_enabled: bool,
    hidden_cols: &std::collections::BTreeSet<String>,
    compact: bool,
) -> Vec<(&'static str, SortKey)> {
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
    if !multi_regions.is_empty() {
        full.insert(1, ("REGION", SortKey::App));
    }
    // COST column opt-in via `:cost on`. Inserted before AGE so the
    // expensive envs catch the eye on the same horizontal band as the
    // stale-env tint.
    if cost_enabled {
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
            !hidden_cols.contains(*label)
        })
        .collect();
    columns
}

/// The narrowest a column can be and still say anything useful.
///
/// Used to decide which columns survive on a narrow terminal — see
/// [`drop_columns_to_fit`]. These are *floors*, not the widths actually
/// rendered: the `Constraint`s below still let the flexible columns
/// grow on a wide terminal.
pub(crate) fn column_min_width(label: &str) -> u16 {
    match label {
        // The row identifier. Long enough for a realistic EB env name
        // (`myapp-production`) rather than a stub.
        "NAME" => 18,
        "REGION" => 12,
        // 11, not 10: the header is the word "APPLICATION" itself, and
        // a floor that truncates its own column heading reads as a
        // rendering bug rather than a deliberate narrowing.
        "APPLICATION" => 11,
        "TIER" => 11,
        "STATUS" => 10,
        "HEALTH" => 3,
        "INST" => 7,
        "TREND" => 12,
        "PLATFORM" => 11,
        "VERSION" => 9,
        "CNAME" => 12,
        "AGE" => 6,
        "COST" => 8,
        _ => 6,
    }
}

/// Columns to shed when the terminal cannot fit them all, least
/// operationally useful first.
///
/// The order is a judgement about what an operator needs at a glance
/// when they have only 80 columns: which env (NAME), is it healthy
/// (HEALTH/STATUS), what is deployed (VERSION), and how stale (AGE).
/// TREND is a sparkline, CNAME is rarely read off the fleet view, and
/// PLATFORM changes about once a year.
///
/// NAME, HEALTH, STATUS, VERSION and AGE are deliberately absent: they
/// are never dropped.
const DROP_ORDER: &[&str] = &["TREND", "CNAME", "PLATFORM", "INST", "TIER", "APPLICATION"];

/// Drop optional columns until the remaining minimums fit `available`.
///
/// Without this the layout silently defeats an invariant the column
/// list states outright — "NAME can never be hidden, it's the row
/// identifier". At 80 columns the fixed-width columns alone claimed ~49
/// cells and ratatui satisfies `Length` before `Percentage`, so NAME and
/// APPLICATION were squeezed to nothing while `TREND (5m)` kept its
/// full twelve. Every row rendered without saying which env it was.
///
/// Returns the labels dropped, newest first, so a caller can tell the
/// operator what is missing rather than leaving them to notice.
pub(crate) fn drop_columns_to_fit(
    columns: &mut Vec<(&'static str, SortKey)>,
    available: u16,
) -> Vec<&'static str> {
    // ratatui puts `column_spacing` (default 1) BETWEEN columns, so N
    // columns cost N-1 cells beyond their own widths. Leaving that out
    // is how a NAME floor of 18 rendered at 13: the widths summed to
    // the budget, ratatui added seven more cells of spacing, overflowed,
    // and squeezed everything back down — silently undoing the floor
    // this function exists to protect.
    let needed = |cols: &[(&'static str, SortKey)]| -> u16 {
        cols.iter()
            .map(|(l, _)| column_min_width(l))
            .fold(0u16, |a, b| a.saturating_add(b))
            .saturating_add(cols.len().saturating_sub(1) as u16)
    };
    let mut dropped = Vec::new();
    for candidate in DROP_ORDER {
        if needed(columns) <= available {
            break;
        }
        if let Some(i) = columns.iter().position(|(l, _)| l == candidate) {
            columns.remove(i);
            dropped.push(*candidate);
        }
    }
    dropped
}

/// How eagerly a column takes leftover space. `0` means fixed.
///
/// NAME and CNAME grow most because their content is genuinely
/// variable-length and gets truncated first; TIER, STATUS, HEALTH,
/// INST, TREND and AGE render fixed-width content and gain nothing
/// from extra cells.
pub(crate) fn column_grow_weight(label: &str) -> u16 {
    match label {
        "NAME" => 3,
        "CNAME" => 3,
        "APPLICATION" => 2,
        "PLATFORM" => 2,
        "VERSION" => 2,
        "REGION" => 1,
        _ => 0,
    }
}

/// Exact width for every column, given the space they have to share.
///
/// Computed rather than delegated to `Constraint` tie-breaking. The
/// previous mix of `Length` and `Percentage` meant the choice of which
/// columns to show (made from [`column_min_width`]) and the widths
/// actually rendered disagreed: at 80 columns VERSION was picked on a
/// floor of 9 and then laid out at 3, so it read `bui` for `build-1`.
/// One source of truth for both decisions is the point.
///
/// Every column gets its minimum; whatever is left over is shared by
/// weight, with the remainder going to the widest-growing column so no
/// cells are lost to rounding.
pub(crate) fn column_widths(columns: &[(&'static str, SortKey)], available: u16) -> Vec<u16> {
    // The inter-column spacing is not ours to hand out — see
    // `drop_columns_to_fit`.
    let available = available.saturating_sub(columns.len().saturating_sub(1) as u16);
    let mins: Vec<u16> = columns.iter().map(|(l, _)| column_min_width(l)).collect();
    let total_min: u16 = mins.iter().fold(0u16, |a, b| a.saturating_add(*b));
    // Already at or over budget — `drop_columns_to_fit` has shed what it
    // can and the rest is genuinely too narrow. Hand back the minimums
    // and let the renderer truncate rather than inventing space.
    if total_min >= available {
        return mins;
    }
    let weights: Vec<u16> = columns.iter().map(|(l, _)| column_grow_weight(l)).collect();
    let total_weight: u16 = weights.iter().fold(0u16, |a, b| a.saturating_add(*b));
    if total_weight == 0 {
        return mins;
    }
    let slack = available - total_min;
    let mut out = mins.clone();
    let mut handed_out = 0u16;
    for (i, w) in weights.iter().enumerate() {
        let share = (u32::from(slack) * u32::from(*w) / u32::from(total_weight)) as u16;
        out[i] = out[i].saturating_add(share);
        handed_out = handed_out.saturating_add(share);
    }
    // Integer division loses up to `total_weight - 1` cells. Give them
    // to the greediest column rather than leaving a ragged gap at the
    // right edge.
    if let Some(best) = weights
        .iter()
        .enumerate()
        .filter(|(_, w)| **w > 0)
        .max_by_key(|(_, w)| **w)
        .map(|(i, _)| i)
    {
        out[best] = out[best].saturating_add(slack - handed_out);
    }
    out
}

/// Everything a cell renderer reads for one row.
///
/// A struct rather than eight parameters: the match below was 160
/// lines inside a closure inside a 695-line `draw_table`, and these
/// eight values are exactly what it closed over. Destructured on entry
/// so the arms move VERBATIM — several of them shadow a context field
/// with a local `let` of the same name (`alert`, `color`), which a
/// rewrite to `ctx.alert` silently broke on the first attempt.
/// Per-row VALUES, not `&App`.
///
/// Six of the arms did a map lookup keyed by this row's env
/// (`worker_dlq_depths`, `env_instance_counts`, `history`,
/// `newly_red`, `stale_platforms`, `costs`). Holding `&App` to reach
/// them made the rows borrow the whole struct, which defeats the
/// field-level split the borrow checker was using to let
/// `render_stateful_widget` take `&mut app.table_state` afterwards.
/// Resolving them once per row is both cheaper and the reason this
/// compiles.
///
/// The lifetime is the ENVIRONMENT's: several arms hand out `&str`
/// into it rather than allocating per row.
pub(crate) struct CellCtx<'a> {
    pub theme: &'a Theme,
    pub e: &'a Environment,
    pub redact: bool,
    pub dlq_depth: i64,
    pub instance_counts: Option<crate::aws::EnvInstanceCounts>,
    pub history: Option<&'a std::collections::VecDeque<String>>,
    pub newly_red: bool,
    pub stale_platform: Option<&'a String>,
    pub cost: Option<f64>,
    /// Pre-built: NAME carries the pin star, multi-select tick,
    /// newly-added marker and drift glyph — assembled once per row,
    /// not once per column.
    pub name_cell: Cell<'a>,
    pub age: String,
    pub color: Color,
    pub now: chrono::DateTime<chrono::Utc>,
}

/// One table cell for `label` in the row `ctx` describes.
pub(crate) fn env_cell<'a>(label: &str, ctx: &CellCtx<'a>) -> Cell<'a> {
    let CellCtx {
        theme,
        e,
        name_cell,
        age,
        color,
        now,
        ..
    } = ctx;
    let (color, now) = (*color, *now);
    match label {
        "NAME" => name_cell.clone(),
        // Application / platform / region values live on
        // `app.environments[i]` which outlives the draw
        // call — borrow rather than clone so the per-row
        // hot path doesn't allocate 3+ Strings per frame.
        "APPLICATION" => Cell::from(Span::raw(e.application.as_str()))
            .style(Style::default().fg(color).add_modifier(Modifier::BOLD)),
        "TIER" => tier_cell(&e.tier, theme),
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
                ctx.dlq_depth
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
                    status_pill_for(&e.status, theme, alert),
                    Span::styled(
                        format!(" {}{dlq}", warn_glyph(theme.icons).trim_end()),
                        Style::default()
                            .fg(theme.health_red)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]))
            } else {
                Cell::from(status_pill_for(&e.status, theme, alert))
            }
        }
        "HEALTH" => Cell::from(health_dot(&e.health, theme)),
        "INST" => {
            // `healthy/total` if the per-env counts have
            // landed for this refresh; em-dash placeholder
            // otherwise (and on the very first frame
            // before the fan-out completes). Cell colour
            // tiers by ratio: all healthy = green, any
            // unhealthy but some healthy = yellow,
            // zero healthy with instances present = red,
            // empty env = muted.
            let counts = ctx.instance_counts;
            let (text, color) = format_instance_counts(counts, theme);
            Cell::from(Span::styled(
                text,
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ))
        }
        "TREND" => Cell::from(sparkline_for(ctx.history, theme, ctx.newly_red)),
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
            let stale = ctx.stale_platform;
            let name_colour = if stale.is_some() {
                theme.health_yellow
            } else {
                colour
            };
            let mut spans = Vec::new();
            if let Some(g) = icon {
                spans.push(Span::styled(format!("{g} "), Style::default().fg(colour)));
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
        "CNAME" => Cell::from(redact(&e.cname, ctx.redact)).style(Style::default().fg(theme.muted)),
        // `age` is built freshly per row inside this scope
        // and so can't be borrowed into the returned Cell.
        // Caching it on rebuild_view would let this be a
        // borrow too, but the age string changes per
        // minute boundary — a stale value would be
        // visible until the next refresh, which is fine
        // operationally but adds bookkeeping. Leave the
        // single per-row clone here as the cheapest
        // honest option for now.
        "AGE" => {
            Cell::from(age.clone()).style(Style::default().fg(age_color(e.updated, now, theme)))
        }
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
            match ctx.cost {
                Some(cost) => {
                    let text = format!("${cost:.0}");
                    let fg = if cost >= 500.0 {
                        theme.health_red
                    } else if cost >= 50.0 {
                        theme.text
                    } else {
                        theme.health_green
                    };
                    Cell::from(text).style(Style::default().fg(fg).add_modifier(Modifier::BOLD))
                }
                None => Cell::from(Span::styled("—", Style::default().fg(theme.muted))),
            }
        }
        _ => Cell::from(""),
    }
}

pub(crate) fn draw_table(f: &mut Frame, area: Rect, app: &mut App) {
    app.table_area = area;
    let theme = app.theme.clone();
    let compact = app.view.mode == ViewMode::Compact;
    let spacious = app.view.mode == ViewMode::Spacious;
    let row_height: u16 = app.view.mode.row_height();
    let block_padding: u16 = if spacious { 2 } else { 1 };
    let indexes = app.filtered_indexes();

    let mut columns = visible_columns(
        &app.multi_regions,
        app.costs.enabled(),
        &app.view.hidden_cols,
        compact,
    );
    // Shed optional columns the terminal cannot fit. Off the top come the
    // block's two borders and the highlight symbol ratatui prepends to
    // every row — measured, not guessed, because it is two cells in
    // unicode/ascii and two in powerline but that is a coincidence
    // rather than a guarantee. The inter-column spacing is handled
    // inside `drop_columns_to_fit` / `column_widths`, since it depends
    // on how many columns survive.
    let gutter = cursor_marker(&theme).chars().count() as u16;
    let usable = area.width.saturating_sub(2).saturating_sub(gutter);
    let dropped = drop_columns_to_fit(&mut columns, usable);
    let sort_marker = if app.view.sort_desc() {
        glyph(app.theme.icons, " ▼", " v")
    } else {
        glyph(app.theme.icons, " ▲", " ^")
    };
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
                glyph(app.theme.icons, "●", "*").into()
            } else if *label == "TREND" {
                format!("TREND ({trend_window})").into()
            } else {
                (*label).into()
            };
            let mut text = display.into_owned();
            let primary_match = matches!(
                (key, app.view.sort_key()),
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
    let app_colors = app.view.app_colors();

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
                // Checked: the index comes from the cached view, and a
                // stale cache would otherwise panic here — in the alt
                // screen, which is exactly what `assert_fresh`'s
                // release-mode softening exists to avoid. An absent row
                // for one frame is what that softening was asking for.
                let Some(e) = app.environments.get(*i) else {
                    return Row::new(Vec::<Cell>::new());
                };
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
                let cell_ctx = CellCtx {
                    theme: &theme,
                    e,
                    name_cell,
                    age,
                    color,
                    now,
                    redact: app.view.redact,
                    dlq_depth: app.worker_dlq_depths.get(&e.name).copied().unwrap_or(0),
                    instance_counts: app.env_instance_counts.get(&e.name).copied(),
                    history: app.history.get(&e.name),
                    newly_red: app.newly_red.contains(&e.name),
                    stale_platform: app.view.stale_platforms().get(&e.name),
                    cost: app.costs.get(&e.name),
                };
                let cells: Vec<Cell> = columns
                    .iter()
                    .map(|(label, _)| env_cell(label, &cell_ctx))
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
            DisplayRow::Separator => separator_row(
                display,
                row_idx,
                &app.environments,
                app_colors,
                &theme,
                &columns,
            ),
        })
        .collect();

    // Say when columns were dropped for width. Silently hiding them
    // leaves the operator wondering where VERSION went, or worse, not
    // noticing it is missing and reading the fleet as fully described.
    let title = if dropped.is_empty() {
        format!("Environments  {}/{}", indexes.len(), app.environments.len())
    } else {
        format!(
            "Environments  {}/{}  \u{b7} {} cols hidden (widen)",
            indexes.len(),
            app.environments.len(),
            dropped.len()
        )
    };
    let widths: Vec<Constraint> = column_widths(&columns, usable)
        .into_iter()
        .map(Constraint::Length)
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
            let e = app.environments.get(*i)?;
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

/// The group-separator row drawn between applications under `:group on`.
///
/// Lifted verbatim out of `draw_table`'s match arm — 137 lines of its
/// 511. Unlike the `DisplayRow::Env` arm beside it, this one needs
/// almost nothing: the env list to look ahead for the next group, the
/// per-app colour map, and three theme fields. Taking `envs` rather than
/// `&App` keeps that narrowness visible in the signature, and avoids
/// borrowing all of `App` while `draw_table` still holds
/// `&mut app.table_state`.
fn separator_row<'a>(
    display: &[DisplayRow],
    row_idx: usize,
    envs: &[crate::aws::Environment],
    app_colors: &std::collections::HashMap<String, Color>,
    theme: &Theme,
    // The separator spans the table, so it needs one cell per column.
    columns: &[(&'static str, SortKey)],
) -> Row<'a> {
    // Resolve the next app's name + color via the same
    // look-ahead pattern; we use the name for the Powerline
    // ribbon and the color for the dashed fill in other styles.
    let (next_app_name, next_color) = display
        .iter()
        .skip(row_idx + 1)
        .find_map(|r| match r {
            DisplayRow::Env(i) => {
                let env = envs.get(*i)?;
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
            DisplayRow::Env(i) => envs.get(*i),
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
                        Span::styled("── ".to_string(), Style::default().fg(theme.muted)),
                        Span::styled(
                            format!("{glyph} "),
                            Style::default().fg(next_color).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(
                            next_app_name.clone(),
                            Style::default().fg(next_color).add_modifier(Modifier::BOLD),
                        ),
                        Span::styled(" ──".to_string(), Style::default().fg(theme.muted)),
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

pub(crate) fn tier_cell(tier: &str, theme: &Theme) -> Cell<'static> {
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
pub(crate) struct PlatformStyle {
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
pub(crate) fn platform_style(family: &str) -> Option<PlatformStyle> {
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
pub(crate) fn tier_icons(icons: IconStyle) -> (&'static str, &'static str) {
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
pub(crate) enum StatusAlert {
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
pub(crate) fn format_instance_counts(
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
pub(crate) fn status_alert(health: &str, dlq: i64) -> StatusAlert {
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
pub(crate) fn status_pill(status: &str, theme: &Theme) -> Span<'static> {
    status_pill_for(status, theme, StatusAlert::None)
}

/// Variant of [`status_pill`] that knows whether the env is otherwise
/// alerting. When `alert` is `Yellow` / `Red`, the `Ready` pill renders
/// in the health colour (bold) instead of bright green — `Ready` means
/// "no lifecycle op in flight" per EB, NOT "everything is fine". A
/// green pill on a Red-tinted row gives the wrong at-a-glance read.
/// Updating / Terminating are unaffected — they already carry a strong
/// "something happening" signal that the operator wants to see in full.
pub(crate) fn status_pill_for(status: &str, theme: &Theme, alert: StatusAlert) -> Span<'static> {
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

/// Pure: render a one-line summary of a group of envs for the per-app
/// banner row. Shape: `"3 envs · 2 web · 1 worker · 1 red"`. Health
/// buckets only appear when non-zero so the summary doesn't include
/// noise like `0 red`. Tier counts only appear when both tiers are
/// represented in the group (showing `2 web` when every env is web adds
/// nothing).
pub(crate) fn summarize_group(envs: &[&Environment]) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    fn cols(regions: &[&str], cost: bool, hidden: &[&str], compact: bool) -> Vec<&'static str> {
        let regions: Vec<String> = regions.iter().map(|s| s.to_string()).collect();
        let hidden: BTreeSet<String> = hidden.iter().map(|s| s.to_string()).collect();
        visible_columns(&regions, cost, &hidden, compact)
            .into_iter()
            .map(|(l, _)| l)
            .collect()
    }

    #[test]
    fn the_column_set_follows_its_four_inputs() {
        // 43 lines of rules that lived inside a 695-line `draw_table`,
        // so the only way to ask "does `:cols hide NAME` do nothing?"
        // was to render a frame and read it back.
        let base = cols(&[], false, &[], false);
        assert_eq!(base.first(), Some(&"NAME"));
        assert!(!base.contains(&"REGION"), "REGION is fan-out only");
        assert!(!base.contains(&"COST"), "COST is opt-in");

        // REGION appears second, next to the name it qualifies.
        let fanned = cols(&["us-east-1", "eu-west-2"], false, &[], false);
        assert_eq!(fanned[1], "REGION");

        // COST lands immediately before AGE, so spend and staleness
        // read on the same horizontal band.
        let costed = cols(&[], true, &[], false);
        let ci = costed
            .iter()
            .position(|c| *c == "COST")
            .expect("COST shown");
        assert_eq!(costed[ci + 1], "AGE");

        // Compact drops TREND and PLATFORM whatever the user hid.
        let compact = cols(&[], false, &[], true);
        assert!(!compact.contains(&"TREND"));
        assert!(!compact.contains(&"PLATFORM"));
        assert!(compact.contains(&"STATUS"), "only those two");

        // The hide list is honoured…
        let hidden = cols(&[], false, &["CNAME", "VERSION"], false);
        assert!(!hidden.contains(&"CNAME") && !hidden.contains(&"VERSION"));
        // …except for NAME, which is the row identifier. Hiding it
        // would leave rows that can't be told apart.
        let hidden = cols(&[], false, &["NAME"], false);
        assert_eq!(hidden.first(), Some(&"NAME"));

        // HEALTH and TREND share a sort key but hide independently.
        let no_trend = cols(&[], false, &["TREND"], false);
        assert!(no_trend.contains(&"HEALTH") && !no_trend.contains(&"TREND"));
        let no_health = cols(&[], false, &["HEALTH"], false);
        assert!(no_health.contains(&"TREND") && !no_health.contains(&"HEALTH"));

        // Everything hideable, hidden: NAME survives alone.
        let all: Vec<&str> = base.to_vec();
        let everything = cols(&[], false, &all, false);
        assert_eq!(everything, vec!["NAME"]);
    }
}
