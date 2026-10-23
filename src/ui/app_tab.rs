//! The Detail view's App tab: the operator's own metrics for one env,
//! one row per series. Design: `docs/design/app-view.md`.

use super::*;
use crate::app_tab::{AppSeries, Level, ResourceRow, ValueUnit};

/// Cells given to each row's sparkline.
const SPARK_WIDTH: usize = 24;

pub(super) fn draw_detail_app(
    f: &mut Frame,
    area: Rect,
    detail: &crate::app::DetailState,
    app: &App,
) {
    let theme = &app.theme;
    let env = detail.env_name.as_str();
    let tabs = &app.cfg.app_tabs;
    let red = Style::default().fg(theme.health_red);
    let muted = Style::default().fg(theme.muted);
    let mut lines: Vec<Line> = Vec::new();

    for e in tabs.errors_for(env) {
        lines.push(Line::from(Span::styled(format!("config: {e}"), red)));
    }
    alarm_line(&mut lines, &detail.cw_alarms, theme);
    match app.app_tab_source(&detail.key) {
        crate::app::AppTabSource::NotSetUp => {
            not_set_up(&mut lines, env, detail, app, area.width, None)
        }
        crate::app::AppTabSource::Preview(found) => {
            not_set_up(&mut lines, env, detail, app, area.width, Some(found))
        }
        crate::app::AppTabSource::Configured(spec) if spec.is_empty() => {
            lines.push(Line::from(Span::styled(
                format!(
                    "Nothing to show: add app.{env}.metric.LABEL, app.{env}.rds or \
                     app.{env}.elasticache lines to config.toml, or press e."
                ),
                muted,
            )));
        }
        crate::app::AppTabSource::Configured(_) => {
            metric_rows(&mut lines, detail, area.width, theme)
        }
    }
    instance_rows(&mut lines, &detail.instances, area.width, theme);
    let queryable = app.top_errors_group(&detail.key).is_some();
    top_error_rows(&mut lines, &detail.app_tab.top_errors, queryable, theme);
    let mut title = format!("App · {}", humanize_range(detail.metrics_range_secs));
    if theme.icons == IconStyle::Ascii {
        title = asciify(&title);
        for line in &mut lines {
            for span in &mut line.spans {
                if !span.content.is_ascii() {
                    span.content = asciify(&span.content).into();
                }
            }
        }
    }
    let block =
        titled_block(&theme.chrome(), &title, true, theme.title).padding(Padding::horizontal(1));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The tab's own non-ASCII punctuation, for `icons = "ascii"`: one
/// choke point over every line, so a string added later cannot slip
/// past it. Sparklines are already drawn in ASCII in that mode.
///
/// One cell for one cell: this runs after rows were fitted to the
/// width, so `…` becomes `.`, not `...`, or a fitted row would grow
/// two cells and wrap. Anything else outside ASCII (in log text, a log
/// group's name, a CloudWatch note) becomes `?`: ascii mode is for
/// terminals that cannot draw it.
fn asciify(s: &str) -> String {
    use unicode_segmentation::UnicodeSegmentation;
    use unicode_width::UnicodeWidthStr;
    let mut out = String::with_capacity(s.len());
    // By grapheme, not char: an emoji joined by ZWJ, one with a variation
    // selector, a flag pair, each draws as one cluster whose width is not
    // the sum of its chars'.
    for g in s.graphemes(true) {
        match g {
            "—" => out.push('-'),
            "…" => out.push('.'),
            "·" => out.push('|'),
            g if g.is_ascii() => out.push_str(g),
            // As many cells as it takes: an e with a combining accent one,
            // a CJK character two.
            g => out.push_str(&"?".repeat(g.width())),
        }
    }
    out
}

/// What discovery found, as the lines an unconfigured env's App tab
/// shows, each saying what found it.
fn summary_lines(found: &crate::app_discover::Discovered) -> Vec<String> {
    let d = &found.discovery;
    let mut out = Vec::new();
    if let Some((prefix, n)) = &d.alarm_prefix {
        out.push(format!("{n} alarms named {prefix}…"));
    }
    for (id, var) in &d.cache_found {
        out.push(format!("cache node {id} (env var {var})"));
    }
    for (id, var) in &d.rds_found {
        out.push(format!("RDS instance {id} (env var {var})"));
    }
    if !d.rds_options.is_empty() && d.rds_found.is_empty() {
        out.push(format!(
            "{} RDS instances to choose from",
            d.rds_options.len()
        ));
    }
    if let Some(log) = &d.app_log {
        out.push(format!("app log {log} (choose it for top errors)"));
    }
    if let Some(app) = &d.app_metrics {
        out.push(format!(
            "{} metrics in {} dimensioned {}={}",
            app.names.len(),
            app.namespace,
            app.dimension.0,
            app.dimension.1
        ));
    }
    for (what, why) in &found.failed {
        out.push(format!("could not check {what}: {why}"));
    }
    out
}

/// `label  value unit  sparkline  instance value  instance value …`.
fn series_row(s: &AppSeries, label_w: usize, width: u16, theme: &Theme) -> Line<'static> {
    let latest = s.points.last().map(|(_, v)| *v);
    let value_style = match (latest, s.threshold) {
        (Some(v), Some(t)) => match t.classify(v) {
            Level::Ok => Style::default().fg(theme.text),
            Level::Warn => Style::default().fg(theme.health_yellow),
            Level::Critical => Style::default().fg(theme.health_red),
        },
        _ => Style::default().fg(theme.text),
    }
    .add_modifier(Modifier::BOLD);
    let value = latest.map_or_else(|| "—".to_string(), format_value);
    let values: Vec<f64> = s.points.iter().map(|(_, v)| *v).collect();
    let mut spans = vec![
        Span::styled(
            format!("{:<label_w$}  ", s.label),
            Style::default().fg(theme.muted),
        ),
        Span::styled(format!("{value:>8}"), value_style),
        Span::styled(
            format!(" {:<6} ", s.unit.as_deref().unwrap_or("")),
            Style::default().fg(theme.muted),
        ),
        Span::styled(
            format!(
                "{:<SPARK_WIDTH$}  ",
                sparkline(&values, SPARK_WIDTH, theme.icons)
            ),
            Style::default().fg(theme.title),
        ),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    let stale = stale_group(&s.points, chrono::Utc::now(), theme);
    let groups: Vec<Vec<Span<'static>>> = stale
        .into_iter()
        .chain(s.instances.iter().map(|(id, points)| {
            let v = points
                .last()
                .map_or_else(|| "—".to_string(), |(_, v)| format_value(*v));
            vec![
                Span::styled(format!("{id} "), Style::default().fg(theme.muted)),
                Span::styled(v, Style::default().fg(theme.text)),
            ]
        }))
        .collect();
    // Border and padding, two cells each side.
    let room = (width as usize).saturating_sub(used + 4);
    spans.extend(join_fields_to_fit(groups, &theme.chrome(), room as u16));
    Line::from(spans)
}

/// Past this, a series' latest value is marked with its age: a metric
/// the app stopped publishing would otherwise read as current, since
/// the sparkline spaces points by count, not by time.
const STALE_AFTER: chrono::TimeDelta = chrono::TimeDelta::minutes(5);

/// `stale 12m`, in the warning colour, when the latest point is older
/// than [`STALE_AFTER`]; drawn first in the row's trailing fields so it
/// is the last thing a narrow terminal drops.
fn stale_group(
    points: &[crate::app_tab::Point],
    now: chrono::DateTime<chrono::Utc>,
    theme: &Theme,
) -> Option<Vec<Span<'static>>> {
    let (t, _) = points.last()?;
    let age = now - *t;
    (age > STALE_AFTER).then(|| {
        vec![Span::styled(
            format!("stale {}", humanize_age(age)),
            Style::default().fg(theme.health_yellow),
        )]
    })
}

/// An env with no App tab config: what discovery found, as an
/// invitation to `e`, with the resources it has evidence for drawn as
/// suggestions (`preview`, what [`App::app_tab_source`] found them in).
/// Without discovery (demo mode), the config keys to add.
fn not_set_up(
    lines: &mut Vec<Line<'static>>,
    env: &str,
    detail: &crate::app::DetailState,
    app: &App,
    width: u16,
    preview: Option<&crate::app_discover::Discovered>,
) {
    let theme = &app.theme;
    let muted = Style::default().fg(theme.muted);
    let strong = Style::default()
        .fg(theme.title)
        .add_modifier(Modifier::BOLD);
    if app.discovery_running(&detail.key) {
        lines.push(Line::from(Span::styled(
            format!("Not set up yet. Looking for what {env} uses…"),
            muted,
        )));
        return;
    }
    let Some(found) = preview.or_else(|| app.discovered_for(&detail.key)) else {
        lines.push(Line::from(Span::styled(
            format!(
                "No App tab config for {env}: press e to set it up, or add \
                 app.{env}.* lines to config.toml (docs/configuration.md)."
            ),
            muted,
        )));
        return;
    };
    let summary = summary_lines(found);
    if summary.is_empty() {
        lines.push(Line::from(Span::styled(
            format!("Not set up yet, and nothing found for {env}: press e to set it up."),
            muted,
        )));
        return;
    }
    lines.push(Line::from(Span::styled(
        format!("Not set up yet. Found for {env}:"),
        strong,
    )));
    for line in summary {
        lines.push(Line::from(Span::styled(
            format!("  {line}"),
            Style::default().fg(theme.text),
        )));
    }
    lines.push(Line::from(vec![
        Span::styled("Press ", muted),
        Span::styled(
            "e",
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" to review and save.", muted),
    ]));
    // The resources an env variable names, drawn now; nothing is saved.
    if preview.is_some() {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled("Suggested, not saved:", muted)));
        metric_rows(lines, detail, width, theme);
    }
}

/// The configured (or previewed) series and resource rows.
fn metric_rows(
    lines: &mut Vec<Line<'static>>,
    detail: &crate::app::DetailState,
    width: u16,
    theme: &Theme,
) {
    let red = Style::default().fg(theme.health_red);
    let muted = Style::default().fg(theme.muted);
    if detail.app_tab.metrics.is_first_load() {
        lines.push(Line::from(Span::styled("loading metrics…", muted)));
    }
    if let Some(e) = detail.app_tab.metrics.error() {
        lines.push(Line::from(Span::styled(format!("metrics: {e}"), red)));
    }
    let Some(m) = detail.app_tab.metrics.ready() else {
        return;
    };
    for note in &m.notes {
        lines.push(Line::from(Span::styled(
            format!("CloudWatch: {note}"),
            Style::default().fg(theme.health_yellow),
        )));
    }
    let label_w = m
        .series
        .iter()
        .map(|s| s.label.chars().count())
        .chain(
            m.resources
                .iter()
                .map(|r| resource_label(r).chars().count()),
        )
        .max()
        .unwrap_or(0);
    for s in &m.series {
        lines.push(series_row(s, label_w, width, theme));
    }
    for r in &m.resources {
        lines.push(resource_row(r, label_w, width, theme));
    }
}

/// The env's most frequent ERROR fingerprints, one per line, with how
/// old the answer is: the query runs on open and `^R` only.
fn top_error_rows(
    lines: &mut Vec<Line<'static>>,
    top: &crate::app::Fetch<crate::app_tab::TopErrors>,
    queryable: bool,
    theme: &Theme,
) {
    let muted = Style::default().fg(theme.muted);
    if top.is_blank() {
        // Configured but not run: not yet, or reset by a config edit.
        // It is billed per scan, so it waits to be asked, and says so
        // rather than looking like no log group at all.
        if queryable {
            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "top errors  not queried yet: ^R runs it (billed per GB scanned)",
                muted,
            )));
        }
        return;
    }
    let window = crate::app_tab::TOP_ERRORS_WINDOW_MINS;
    lines.push(Line::default());
    if let Some(e) = top.error() {
        lines.push(Line::from(vec![
            Span::styled("top errors  ", muted),
            Span::styled(e.to_string(), Style::default().fg(theme.health_red)),
        ]));
        return;
    }
    let Some(t) = top.ready() else {
        if top.is_first_load() {
            lines.push(Line::from(Span::styled(
                "top errors  querying Logs Insights…",
                muted,
            )));
        }
        return;
    };
    let age = humanize_age(chrono::Utc::now() - t.fetched_at);
    lines.push(Line::from(Span::styled(
        format!(
            "top errors (last {window}m of {}, {age} ago, ^R to rerun)",
            t.log_group
        ),
        muted,
    )));
    if t.rows.is_empty() && t.unmatched == 0 {
        lines.push(Line::from(Span::styled("  no ERROR lines", muted)));
    }
    for (fp, n) in &t.rows {
        lines.push(Line::from(vec![
            Span::styled(
                format!("{n:>6}  "),
                Style::default()
                    .fg(theme.health_red)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(fp.clone(), Style::default().fg(theme.text)),
        ]));
    }
    if t.more.0 > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "{:>6}  in {} more fingerprints{}",
                t.more.1,
                t.more.0,
                if t.truncated {
                    " (and possibly more: the query hit its row limit)"
                } else {
                    ""
                }
            ),
            muted,
        )));
    }
    if t.unmatched > 0 {
        lines.push(Line::from(Span::styled(
            format!(
                "{:>6}  ERROR lines the fingerprint did not match",
                t.unmatched
            ),
            muted,
        )));
    }
}

/// `alarms  2 in ALARM: a, b · 1 insufficient data · 6 OK`, firing
/// names in red. The alarms are the env's, matched by dimension or by
/// `app.ENV.alarm_prefix`, and arrive ALARM first.
fn alarm_line(
    lines: &mut Vec<Line<'static>>,
    alarms: &crate::app::Fetch<Vec<crate::aws::CwAlarm>>,
    theme: &Theme,
) {
    let muted = Style::default().fg(theme.muted);
    let label = Span::styled("alarms  ", muted);
    if let Some(e) = alarms.error() {
        lines.push(Line::from(vec![
            label,
            Span::styled(e.to_string(), Style::default().fg(theme.health_red)),
        ]));
        return;
    }
    let Some(list) = alarms.ready() else {
        if alarms.is_first_load() {
            lines.push(Line::from(vec![label, Span::styled("loading…", muted)]));
        }
        return;
    };
    let s = AlarmSummary::of(list);
    let mut spans = vec![label];
    if list.is_empty() {
        spans.push(Span::styled("none for this env", muted));
    }
    if !s.firing.is_empty() {
        spans.push(Span::styled(
            format!("{} in ALARM: {}", s.firing.len(), s.firing.join(", ")),
            Style::default()
                .fg(theme.health_red)
                .add_modifier(Modifier::BOLD),
        ));
    }
    for (n, what) in [(s.insufficient, "insufficient data"), (s.ok, "OK")] {
        if n > 0 {
            if spans.len() > 1 {
                spans.push(Span::styled(" · ", muted));
            }
            spans.push(Span::styled(
                format!("{n} {what}"),
                Style::default().fg(theme.text),
            ));
        }
    }
    lines.push(Line::from(spans));
}

/// Alarms by state: the firing ones by name, the others counted.
#[derive(Debug, PartialEq, Eq)]
struct AlarmSummary {
    pub firing: Vec<String>,
    pub insufficient: usize,
    pub ok: usize,
}

impl AlarmSummary {
    fn of(alarms: &[crate::aws::CwAlarm]) -> Self {
        let mut s = Self {
            firing: Vec::new(),
            insufficient: 0,
            ok: 0,
        };
        for a in alarms {
            match a.state.as_str() {
                "ALARM" => s.firing.push(a.name.clone()),
                "INSUFFICIENT_DATA" => s.insufficient += 1,
                _ => s.ok += 1,
            }
        }
        s
    }
}

/// Enhanced health's last window per instance: requests, p99, error
/// shares, CPU. Needs no config, so every env gets it; an env without
/// enhanced health says so once instead of drawing empty rows.
fn instance_rows(
    lines: &mut Vec<Line<'static>>,
    instances: &[crate::aws::Instance],
    width: u16,
    theme: &Theme,
) {
    if instances.is_empty() {
        return;
    }
    let muted = Style::default().fg(theme.muted);
    lines.push(Line::default());
    if instances.iter().all(|i| i.app_stats.is_none()) {
        lines.push(Line::from(Span::styled(
            "instances: no enhanced health data (requests, latency and CPU \
             per instance need enhanced health on)",
            muted,
        )));
        return;
    }
    lines.push(Line::from(Span::styled(
        "instances (enhanced health, last window)",
        muted,
    )));
    let id_w = instances
        .iter()
        .map(|i| i.id.chars().count())
        .max()
        .unwrap_or(0);
    for i in instances {
        let mut spans = vec![Span::styled(
            format!("{:<id_w$}  ", i.id),
            Style::default().fg(health_color(&i.color, theme)),
        )];
        let used: usize = spans.iter().map(Span::width).sum();
        let stat = |label: &str, value: String, style: Style| {
            vec![
                Span::styled(format!("{label} "), muted),
                Span::styled(value, style),
            ]
        };
        let plain = Style::default().fg(theme.text);
        let errors = |pct: i32| {
            if pct > 0 {
                Style::default().fg(theme.health_red)
            } else {
                plain
            }
        };
        let groups: Vec<Vec<Span<'static>>> = match &i.app_stats {
            None => vec![vec![Span::styled("no data", muted)]],
            Some(s) => [
                s.requests_per_sec
                    .map(|r| stat("req/s", format_value(r), plain)),
                s.p99_secs.map(|p| stat("p99", format_latency(p), plain)),
                s.pct_5xx.map(|p| stat("5xx", format!("{p}%"), errors(p))),
                s.pct_4xx.map(|p| stat("4xx", format!("{p}%"), plain)),
                s.cpu_pct
                    .map(|c| stat("cpu", format!("{}%", format_value(c)), plain)),
            ]
            .into_iter()
            .flatten()
            .collect(),
        };
        let room = (width as usize).saturating_sub(used + 4);
        spans.extend(join_fields_to_fit(groups, &theme.chrome(), room as u16));
        lines.push(Line::from(spans));
    }
}

/// `840ms` under a second, `2.1s` from one.
fn format_latency(secs: f64) -> String {
    if !secs.is_finite() {
        "—".to_string()
    } else if secs < 1.0 {
        format!("{:.0}ms", secs * 1000.0)
    } else {
        format!("{}s", format_value(secs))
    }
}

fn resource_label(r: &ResourceRow) -> String {
    format!("{} {}", r.kind.label(), r.id)
}

/// `rds shop-db  31%  sparkline  conns 48 · free 1.2G · lag 0s`: the
/// kind's first metric is the headline, the rest follow when they have
/// data (a primary has no replica lag).
fn resource_row(r: &ResourceRow, label_w: usize, width: u16, theme: &Theme) -> Line<'static> {
    let muted = Style::default().fg(theme.muted);
    let latest = |points: &[crate::app_tab::Point]| points.last().map(|(_, v)| *v);
    let (headline, rest) = match r.metrics.split_first() {
        Some((h, rest)) => (Some(h), rest),
        None => (None, &[][..]),
    };
    let value = headline
        .and_then(|(m, points)| latest(points).map(|v| format_linked(v, m.unit)))
        .unwrap_or_else(|| "—".to_string());
    let values: Vec<f64> = headline
        .map(|(_, points)| points.iter().map(|(_, v)| *v).collect())
        .unwrap_or_default();
    let mut spans = vec![
        Span::styled(format!("{:<label_w$}  ", resource_label(r)), muted),
        Span::styled(
            format!("{value:>8}"),
            Style::default().fg(theme.text).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" {:<6} ", headline.map_or("", |(m, _)| m.label)),
            muted,
        ),
        Span::styled(
            format!(
                "{:<SPARK_WIDTH$}  ",
                sparkline(&values, SPARK_WIDTH, theme.icons)
            ),
            Style::default().fg(theme.title),
        ),
    ];
    let used: usize = spans.iter().map(Span::width).sum();
    let stale = headline.and_then(|(_, points)| stale_group(points, chrono::Utc::now(), theme));
    let groups: Vec<Vec<Span<'static>>> = stale
        .into_iter()
        .chain(rest.iter().filter_map(|(m, points)| {
            let v = latest(points)?;
            Some(vec![
                Span::styled(format!("{} ", m.label), muted),
                Span::styled(format_linked(v, m.unit), Style::default().fg(theme.text)),
            ])
        }))
        .collect();
    let room = (width as usize).saturating_sub(used + 4);
    spans.extend(join_fields_to_fit(groups, &theme.chrome(), room as u16));
    Line::from(spans)
}

/// A linked metric's value with its unit: `31%`, `48`, `1.2G`, `0.4s`.
fn format_linked(v: f64, unit: ValueUnit) -> String {
    match unit {
        ValueUnit::Percent => format!("{}%", format_value(v)),
        ValueUnit::Count => format_value(v),
        ValueUnit::Seconds => format!("{}s", format_value(v)),
        ValueUnit::Bytes => format_bytes(v),
    }
}

/// Bytes in powers of 1024: `900B`, `3.4K`, `512M`, `1.2G`.
fn format_bytes(v: f64) -> String {
    if !v.is_finite() {
        return "—".to_string();
    }
    const STEPS: [(f64, &str); 4] = [
        (1024.0 * 1024.0 * 1024.0 * 1024.0, "T"),
        (1024.0 * 1024.0 * 1024.0, "G"),
        (1024.0 * 1024.0, "M"),
        (1024.0, "K"),
    ];
    for (size, suffix) in STEPS {
        if v.abs() >= size {
            let n = v / size;
            return if n.abs() >= 100.0 {
                format!("{n:.0}{suffix}")
            } else {
                format!("{n:.1}{suffix}")
            };
        }
    }
    format!("{v:.0}B")
}

/// A metric value in at most about six cells: `1620`, `41.5`, `0.42`,
/// `12.3k`, `4.1M`.
fn format_value(v: f64) -> String {
    if !v.is_finite() {
        return "—".to_string();
    }
    let a = v.abs();
    // Each suffix step starts where the one below would round to 1000:
    // 999,960 is `1.0M`, not `1000.0k`.
    let s = if a >= 999.95e9 {
        format!("{:.1}T", v / 1e12)
    } else if a >= 999.95e6 {
        format!("{:.1}G", v / 1e9)
    } else if a >= 999.95e3 {
        format!("{:.1}M", v / 1e6)
    } else if a >= 1e4 {
        format!("{:.1}k", v / 1e3)
    } else if a >= 100.0 {
        format!("{v:.0}")
    } else if a >= 10.0 {
        format!("{v:.1}")
    } else {
        format!("{v:.2}")
    };
    // `3.00` reads as `3`, `41.50` as `41.5`; the suffixed forms keep
    // their one decimal so `1.0k` does not jump to `1k` and back.
    if s.contains('.') && !s.ends_with(['k', 'M', 'G', 'T']) {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// One cell per bucket, scaled between the series' own min and max.
/// More points than cells are averaged into buckets; fewer draw one
/// cell each.
fn sparkline(values: &[f64], width: usize, icons: IconStyle) -> String {
    const UNICODE: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    const ASCII: [char; 8] = ['_', '.', '-', ':', '=', '+', '*', '#'];
    let glyphs = if icons == IconStyle::Ascii {
        &ASCII
    } else {
        &UNICODE
    };
    let values: Vec<f64> = values.iter().copied().filter(|v| v.is_finite()).collect();
    if values.is_empty() || width == 0 {
        return String::new();
    }
    let buckets: Vec<f64> = if values.len() <= width {
        values
    } else {
        (0..width)
            .map(|b| {
                let lo = b * values.len() / width;
                let hi = ((b + 1) * values.len() / width).max(lo + 1);
                values[lo..hi].iter().sum::<f64>() / (hi - lo) as f64
            })
            .collect()
    };
    let min = buckets.iter().copied().fold(f64::INFINITY, f64::min);
    let max = buckets.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let top = (glyphs.len() - 1) as f64;
    buckets
        .iter()
        .map(|v| {
            // A flat series sits mid-height: at the bottom it would
            // read as zero.
            let level = if max > min {
                ((v - min) / (max - min) * top).round() as usize
            } else {
                glyphs.len() / 2 - 1
            };
            glyphs[level.min(glyphs.len() - 1)]
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_format_into_a_few_cells() {
        for (v, want) in [
            (0.0, "0"),
            (0.4213, "0.42"),
            (3.0, "3"),
            (41.5, "41.5"),
            (41.0, "41"),
            (1620.4, "1620"),
            (12_345.0, "12.3k"),
            (10_000.0, "10.0k"),
            (4_100_000.0, "4.1M"),
            (999_960.0, "1.0M"),
            (2.5e9, "2.5G"),
            (999_990_000.0, "1.0G"),
            (3.0e13, "30.0T"),
            (-2.5, "-2.5"),
            (f64::NAN, "—"),
            (f64::INFINITY, "—"),
        ] {
            assert_eq!(format_value(v), want, "{v}");
        }
    }

    #[test]
    fn a_sparkline_spans_its_own_range() {
        assert_eq!(
            sparkline(
                &[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0],
                8,
                IconStyle::Unicode
            ),
            "▁▂▃▄▅▆▇█"
        );
        assert_eq!(
            sparkline(&[10.0, 20.0], 8, IconStyle::Unicode),
            "▁█",
            "one cell per point"
        );
        assert_eq!(
            sparkline(&[5.0, 5.0, 5.0], 8, IconStyle::Unicode),
            "▄▄▄",
            "flat is mid-height"
        );
        assert_eq!(
            sparkline(&[0.0], 8, IconStyle::Ascii),
            ":",
            "so is a single point"
        );
        assert_eq!(sparkline(&[], 8, IconStyle::Unicode), "");
        assert_eq!(sparkline(&[1.0], 0, IconStyle::Unicode), "");
    }

    #[test]
    fn a_long_series_is_averaged_into_the_width() {
        // Four points into two cells: (0,2) averages 1, (8,10) averages 9.
        assert_eq!(
            sparkline(&[0.0, 2.0, 8.0, 10.0], 2, IconStyle::Unicode),
            "▁█"
        );
        // (0,10) and (5,5) both average 5: flat. Taking each bucket's
        // first point instead would draw a rise.
        assert_eq!(
            sparkline(&[0.0, 10.0, 5.0, 5.0], 2, IconStyle::Unicode),
            "▄▄"
        );
        let long: Vec<f64> = (0..1440).map(f64::from).collect();
        assert_eq!(sparkline(&long, 24, IconStyle::Unicode).chars().count(), 24);
    }

    #[test]
    fn an_ascii_sparkline_is_ascii() {
        let s = sparkline(&[0.0, 7.0, 3.0, f64::NAN], 8, IconStyle::Ascii);
        assert_eq!(s, "_#:", "non-finite points are skipped");
        assert!(s.is_ascii());
    }

    #[test]
    fn bytes_format_in_powers_of_1024() {
        for (v, want) in [
            (900.0, "900B"),
            (3.4 * 1024.0, "3.4K"),
            (512.0 * 1024.0 * 1024.0, "512M"),
            (1.25 * 1024.0 * 1024.0 * 1024.0, "1.2G"),
            (2.0 * 1024f64.powi(4), "2.0T"),
            (f64::NAN, "—"),
        ] {
            assert_eq!(format_bytes(v), want, "{v}");
        }
    }

    #[test]
    fn linked_values_carry_their_unit() {
        assert_eq!(format_linked(31.0, ValueUnit::Percent), "31%");
        assert_eq!(format_linked(48.0, ValueUnit::Count), "48");
        assert_eq!(format_linked(0.4, ValueUnit::Seconds), "0.4s");
        assert_eq!(format_linked(2048.0, ValueUnit::Bytes), "2.0K");
    }

    #[test]
    fn latency_reads_in_ms_under_a_second() {
        assert_eq!(format_latency(0.84), "840ms");
        assert_eq!(format_latency(0.0), "0ms");
        assert_eq!(format_latency(1.0), "1s");
        assert_eq!(format_latency(2.14), "2.14s");
        assert_eq!(format_latency(f64::NAN), "—");
    }

    #[test]
    fn asciify_keeps_every_cell_count() {
        // One sample at a time, so a cell gained in one cannot cancel a
        // cell lost in another: a precomposed é, an arrow, an e with a
        // combining acute (zero cells for the mark), a CJK character (two).
        // And clusters: a ZWJ family, a heart with VS16, a flag pair.
        for s in [
            "loading… — a · b",
            "é→",
            "e\u{301}",
            "日",
            "👨\u{200d}👩\u{200d}👧",
            "❤\u{fe0f}",
            "🇬🇧",
        ] {
            let a = asciify(s);
            assert!(a.is_ascii(), "{a}");
            assert_eq!(
                Span::raw(a.clone()).width(),
                Span::raw(s).width(),
                "{s:?}: a fitted row must not grow or shrink"
            );
        }
    }

    #[test]
    fn a_value_older_than_five_minutes_is_marked_with_its_age() {
        let theme = Theme::dark();
        let now = chrono::DateTime::from_timestamp(10_000, 0).unwrap();
        let at = |secs_ago: i64| vec![(now - chrono::Duration::seconds(secs_ago), 1.0)];
        assert_eq!(
            stale_group(&at(60), now, &theme),
            None,
            "a minute old is current"
        );
        assert_eq!(
            stale_group(&at(300), now, &theme),
            None,
            "five minutes is the edge"
        );
        let marked = stale_group(&at(12 * 60), now, &theme).expect("marked");
        assert_eq!(marked[0].content, "stale 12m");
        assert_eq!(marked[0].style.fg, Some(theme.health_yellow));
        assert_eq!(
            stale_group(&[], now, &theme),
            None,
            "no points, nothing to age"
        );
    }

    #[test]
    fn the_summary_says_what_found_each_line_and_what_could_not_be_checked() {
        let mut d = discovered();
        d.failed = vec![("custom metrics".into(), "AccessDenied".into())];
        let lines = summary_lines(&d);
        assert!(
            lines.contains(&"3 alarms named Shop-prod-…".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"cache node sessions-001 (env var REDIS)".to_string()),
            "{lines:?}"
        );
        assert!(
            lines.contains(&"2 RDS instances to choose from".to_string()),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("app log var/log/app.log")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("3 metrics in Shop")),
            "{lines:?}"
        );
        assert_eq!(
            lines.last().unwrap(),
            "could not check custom metrics: AccessDenied"
        );
        assert!(summary_lines(&crate::app_discover::Discovered::default()).is_empty());
    }

    fn discovered() -> crate::app_discover::Discovered {
        crate::app_discover::sample()
    }

    #[test]
    fn alarms_summarise_by_state_with_firing_names() {
        let alarm = |name: &str, state: &str| crate::aws::CwAlarm {
            name: name.into(),
            state: state.into(),
            state_reason: String::new(),
            metric_name: String::new(),
            namespace: String::new(),
        };
        let s = AlarmSummary::of(&[
            alarm("a", "ALARM"),
            alarm("b", "OK"),
            alarm("c", "INSUFFICIENT_DATA"),
            alarm("d", "ALARM"),
            alarm("e", "OK"),
        ]);
        assert_eq!(
            s,
            AlarmSummary {
                firing: vec!["a".into(), "d".into()],
                insufficient: 1,
                ok: 2,
            }
        );
    }
}
