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
    match tabs.envs.get(env) {
        None => {
            lines.push(Line::from(Span::styled(
                format!(
                    "No App tab config for {env}: add app.{env}.namespace and \
                     app.{env}.metric.LABEL lines to config.toml for your own \
                     metrics (docs/configuration.md)."
                ),
                muted,
            )));
        }
        Some(spec) if spec.is_empty() => {
            lines.push(Line::from(Span::styled(
                format!(
                    "Nothing to show: add app.{env}.metric.LABEL, app.{env}.rds or \
                     app.{env}.elasticache lines to config.toml."
                ),
                muted,
            )));
        }
        Some(_) => {
            if detail.app_metrics.is_first_load() {
                lines.push(Line::from(Span::styled("loading metrics…", muted)));
            }
            if let Some(e) = detail.app_metrics.error() {
                lines.push(Line::from(Span::styled(format!("metrics: {e}"), red)));
            }
            if let Some(m) = detail.app_metrics.ready() {
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
                    lines.push(series_row(s, label_w, area.width, theme));
                }
                for r in &m.resources {
                    lines.push(resource_row(r, label_w, area.width, theme));
                }
            }
        }
    }
    instance_rows(&mut lines, &detail.instances, area.width, theme);
    let block = titled_block(
        &theme.chrome(),
        &format!("App · {}", range_label(detail.metrics_range_secs)),
        true,
        theme.title,
    )
    .padding(Padding::horizontal(1));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
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
    let groups: Vec<Vec<Span<'static>>> = s
        .instances
        .iter()
        .map(|(id, points)| {
            let v = points
                .last()
                .map_or_else(|| "—".to_string(), |(_, v)| format_value(*v));
            vec![
                Span::styled(format!("{id} "), Style::default().fg(theme.muted)),
                Span::styled(v, Style::default().fg(theme.text)),
            ]
        })
        .collect();
    // Border and padding, two cells each side.
    let room = (width as usize).saturating_sub(used + 4);
    spans.extend(join_fields_to_fit(groups, &theme.chrome(), room as u16));
    Line::from(spans)
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
pub(crate) struct AlarmSummary {
    pub firing: Vec<String>,
    pub insufficient: usize,
    pub ok: usize,
}

impl AlarmSummary {
    pub(crate) fn of(alarms: &[crate::aws::CwAlarm]) -> Self {
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
pub(crate) fn format_latency(secs: f64) -> String {
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
    let groups: Vec<Vec<Span<'static>>> = rest
        .iter()
        .filter_map(|(m, points)| {
            let v = latest(points)?;
            Some(vec![
                Span::styled(format!("{} ", m.label), muted),
                Span::styled(format_linked(v, m.unit), Style::default().fg(theme.text)),
            ])
        })
        .collect();
    let room = (width as usize).saturating_sub(used + 4);
    spans.extend(join_fields_to_fit(groups, &theme.chrome(), room as u16));
    Line::from(spans)
}

/// A linked metric's value with its unit: `31%`, `48`, `1.2G`, `0.4s`.
pub(crate) fn format_linked(v: f64, unit: ValueUnit) -> String {
    match unit {
        ValueUnit::Percent => format!("{}%", format_value(v)),
        ValueUnit::Count => format_value(v),
        ValueUnit::Seconds => format!("{}s", format_value(v)),
        ValueUnit::Bytes => format_bytes(v),
    }
}

/// Bytes in powers of 1024: `900B`, `3.4K`, `512M`, `1.2G`.
pub(crate) fn format_bytes(v: f64) -> String {
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

fn range_label(secs: i64) -> String {
    match secs {
        s if s % 3600 == 0 => format!("{}h", s / 3600),
        s => format!("{}m", s / 60),
    }
}

/// A metric value in at most about six cells: `1620`, `41.5`, `0.42`,
/// `12.3k`, `4.1M`.
pub(crate) fn format_value(v: f64) -> String {
    if !v.is_finite() {
        return "—".to_string();
    }
    let a = v.abs();
    let s = if a >= 1e6 {
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
    if s.contains('.') && !s.ends_with(['k', 'M']) {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s
    }
}

/// One cell per bucket, scaled between the series' own min and max.
/// More points than cells are averaged into buckets; fewer draw one
/// cell each.
pub(crate) fn sparkline(values: &[f64], width: usize, icons: IconStyle) -> String {
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

    #[test]
    fn range_labels_name_the_window() {
        assert_eq!(range_label(900), "15m");
        assert_eq!(range_label(3600), "1h");
        assert_eq!(range_label(86_400), "24h");
    }
}
