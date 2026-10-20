//! The Detail view's App tab: the operator's own metrics for one env,
//! one row per series. Design: `docs/design/app-view.md`.

use super::*;
use crate::app_tab::{AppSeries, Level};

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
    match tabs.envs.get(env) {
        None => {
            lines.push(Line::from(Span::styled(
                format!(
                    "No App tab for {env}. Add app.{env}.namespace and \
                     app.{env}.metric.LABEL lines to config.toml (docs/configuration.md)."
                ),
                muted,
            )));
        }
        Some(spec) if spec.metrics.is_empty() => {
            lines.push(Line::from(Span::styled(
                format!("No metrics configured: add app.{env}.metric.LABEL lines to config.toml."),
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
                    .max()
                    .unwrap_or(0);
                for s in &m.series {
                    lines.push(series_row(s, label_w, area.width, theme));
                }
            }
        }
    }
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
    fn range_labels_name_the_window() {
        assert_eq!(range_label(900), "15m");
        assert_eq!(range_label(3600), "1h");
        assert_eq!(range_label(86_400), "24h");
    }
}
