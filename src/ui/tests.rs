//! Render tests for the `ui` module tree.
//!
//! Lifted out of `src/ui.rs` in the 0.31 split, unchanged. They reach
//! their subjects through `super::*`, which the root's glob
//! re-exports resolve to whichever sibling now owns each item — so a
//! later move between view modules doesn't touch this file.

#![cfg(test)]

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
    assert!(super::series_anomaly_label("req5xx", &v, IconStyle::Unicode).is_some());
}

#[test]
fn series_anomaly_quiet_when_stable() {
    let v = vec![5.0, 5.0, 5.0, 5.0, 5.5];
    assert!(super::series_anomaly_label("req5xx", &v, IconStyle::Unicode).is_none());
}

#[test]
fn series_anomaly_ignores_unrelated_id() {
    let v = vec![1.0, 1.0, 1.0, 1.0, 99.0];
    assert!(super::series_anomaly_label("health", &v, IconStyle::Unicode).is_none());
}

#[test]
fn series_anomaly_handles_short_series() {
    let v = vec![1.0, 9.0];
    assert!(super::series_anomaly_label("req5xx", &v, IconStyle::Unicode).is_none());
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
fn every_status_glyph_has_an_ascii_form() {
    // `icons = "ascii"` exists for terminals without a usable
    // unicode font — a mode where an unconditional ▲ renders as a
    // replacement box, which for a SORT MARKER or a health delta
    // means the operator can't read what the glyph encodes.
    let ascii = IconStyle::Ascii;
    assert_eq!(super::glyph(ascii, "▲", "^"), "^");
    assert_eq!(super::glyph(IconStyle::Unicode, "▲", "^"), "▲");

    // The anomaly badge builds its text from the glyph, so the
    // whole label has to come through ascii-clean — a `▲` baked
    // into the message string is how this one hid.
    let spike = vec![1.0, 1.0, 1.0, 99.0];
    let label =
        super::series_anomaly_label("req5xx", &spike, ascii).expect("a 99x spike is an anomaly");
    assert!(!label.contains('▲'), "ascii mode still emitted ▲: {label}");
    assert!(label.starts_with('^'), "{label}");
    let label = super::series_anomaly_label("req5xx", &spike, IconStyle::Unicode)
        .expect("still fires in unicode mode");
    assert!(label.starts_with('▲'), "{label}");
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
    let label = format_refresh_label(None, chrono::Utc::now(), std::time::Duration::from_secs(15));
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

/// OSC 8 hyperlinks still don't work through ratatui, and the reason
/// changed in 0.30 — which is why this test exists.
///
/// Under 0.29 every byte of an escape sequence, `\x1b` included, took a
/// full cell: the 24-byte opener ate 24 cells and pushed the visible
/// text off the buffer. Ugly, but the bytes survived, so a custom
/// widget bypassing the diff renderer could in principle reassemble
/// them.
///
/// 0.30 strips the ESC bytes and renders the REST as literal text, so
/// the buffer holds `]8;;https://example.com\Click]8;;\`. For our
/// purposes that is worse: the escape is now unrecoverable from the
/// buffer, so even the custom-widget escape hatch is gone. Emitting
/// OSC 8 anywhere would put visible junk on the operator's screen.
///
/// Nothing in production emits OSC 8 — this pins the constraint, not a
/// behaviour we depend on. The previous version of this test said it
/// would "fail loudly and prompt us to revisit the feature" if ratatui
/// ever changed here. It did exactly that on the 0.30 bump, which is
/// the whole reason it was written.
#[test]
fn osc8_still_cannot_round_trip_through_ratatui() {
    use ratatui::buffer::Buffer;
    use ratatui::layout::Rect;
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Paragraph, Widget};
    let osc8 = "\x1b]8;;https://example.com\x1b\\Click\x1b]8;;\x1b\\";
    let para = Paragraph::new(Line::from(Span::raw(osc8)));
    let mut buf = Buffer::empty(Rect::new(0, 0, 40, 1));
    para.render(buf.area, &mut buf);
    let rendered = buffer_to_string(&buf);

    // The ESC bytes are gone — dropped, not preserved.
    assert!(
        !rendered.contains('\x1b'),
        "0.30 strips ESC; if this fails the behaviour changed again: {rendered:?}"
    );
    // ...and the sequence's payload is now visible junk, which is what
    // makes emitting OSC 8 unsafe rather than merely ineffective.
    assert!(
        rendered.contains("]8;;"),
        "the escape payload renders as literal text: {rendered:?}"
    );
    // The link text is present, but wrapped in that junk, so it is not a
    // hyperlink — just a mess.
    //
    // Not evidence about 0.29 either way: this buffer is 40 cells and the
    // 0.29 rewrite widened it from 20 to fit the whole sequence. Under
    // 0.29 the opener consumed 26 cells, so `Click` would have landed at
    // 26..31 and survived at this width too. What changed in 0.30 is the
    // ESC handling asserted above, not whether the text fits.
    assert!(rendered.contains("Click"), "{rendered:?}");
}

// ── mutation-sweep triage, 2026-08-26 ────────────────────────────────
//
// The `ui/` pure helpers. These are the render-layer functions where a
// mutation changes what an operator *concludes*, not just where a pixel
// lands — a severity that renders in the ordinary colour is one an
// operator scanning for red does not see.

#[test]
fn severity_style_distinguishes_every_level() {
    use crate::ui::events::severity_style;
    let theme = crate::theme::Theme::default();

    let red = severity_style("ERROR", &theme);
    let yellow = severity_style("WARN", &theme);
    let info = severity_style("INFO", &theme);
    let debug = severity_style("DEBUG", &theme);

    assert_eq!(red.fg, Some(theme.health_red), "ERROR must read as red");
    assert_eq!(
        severity_style("FATAL", &theme).fg,
        Some(theme.health_red),
        "FATAL shares the ERROR arm"
    );
    assert_eq!(
        yellow.fg,
        Some(theme.health_yellow),
        "WARN must read as yellow"
    );
    assert_eq!(info.fg, Some(theme.text));
    assert_eq!(debug.fg, Some(theme.muted), "DEBUG is de-emphasised");
    assert_eq!(severity_style("TRACE", &theme).fg, Some(theme.muted));

    // The distinctions that matter: an ERROR must not look like an INFO,
    // and a WARN must not look like either. Deleting an arm sends it to
    // `_ => theme.text`, which is exactly INFO.
    assert_ne!(
        red.fg, info.fg,
        "an ERROR rendering as ordinary text is one nobody scanning for \
         red would see"
    );
    assert_ne!(yellow.fg, info.fg);
    assert_ne!(red.fg, yellow.fg);

    // Case-insensitive, and an unknown level falls back rather than
    // panicking.
    assert_eq!(severity_style("error", &theme).fg, red.fg);
    assert_eq!(severity_style("NOTICE", &theme).fg, Some(theme.text));
}

#[test]
fn event_severity_style_marks_the_tail_gap() {
    use crate::ui::events::event_severity_style;
    let theme = crate::theme::Theme::default();

    assert_eq!(
        event_severity_style("ERROR", &theme).fg,
        Some(theme.health_red)
    );
    assert_eq!(
        event_severity_style("WARN", &theme).fg,
        Some(theme.health_yellow)
    );
    // The synthetic gap marker is its own case — it borrows WARN's
    // colour but is bold, so a dropped-events notice can't be mistaken
    // for an ordinary line.
    let gap = event_severity_style(crate::app::EVENT_TAIL_GAP_SEVERITY, &theme);
    assert_eq!(gap.fg, Some(theme.health_yellow));
    assert!(
        gap.add_modifier.contains(ratatui::style::Modifier::BOLD),
        "the tail-gap marker must stand out from an ordinary WARN"
    );
    assert_eq!(
        event_severity_style("INFO", &theme).fg,
        Some(theme.muted),
        "everything else is de-emphasised in the event pane"
    );
}

#[test]
fn humanize_age_buckets_at_the_boundaries() {
    use crate::ui::events::humanize_age;
    use chrono::Duration as D;
    for (d, want) in [
        (D::seconds(0), "0s"),
        (D::seconds(59), "59s"),
        (D::seconds(60), "1m"),
        (D::seconds(3599), "59m"),
        (D::seconds(3600), "1h"),
        (D::seconds(86_399), "23h"),
        (D::seconds(86_400), "1d"),
        (D::days(9), "9d"),
    ] {
        assert_eq!(humanize_age(d), want, "at {d}");
    }
    // Negative (clock skew) clamps rather than rendering a negative age.
    assert_eq!(humanize_age(D::seconds(-5)), "0s");
}

#[test]
fn age_color_bands_by_recency() {
    use crate::ui::events::age_color;
    let theme = crate::theme::Theme::default();
    let now = chrono::Utc::now();
    let at = |d: chrono::Duration| age_color(Some(now - d), now, &theme);

    assert_eq!(age_color(None, now, &theme), theme.muted, "never updated");
    assert_eq!(at(chrono::Duration::hours(1)), theme.title_alt, "fresh");
    assert_eq!(
        at(chrono::Duration::hours(23)),
        theme.title_alt,
        "still inside the 24h band"
    );
    assert_eq!(
        at(chrono::Duration::hours(25)),
        theme.text,
        "past a day is ordinary, not fresh"
    );
    assert_eq!(at(chrono::Duration::days(29)), theme.text);
    assert_eq!(
        at(chrono::Duration::days(31)),
        theme.muted,
        "over a month is stale"
    );
    // A future timestamp (clock skew) reads as fresh rather than stale.
    assert_eq!(
        age_color(Some(now + chrono::Duration::hours(1)), now, &theme),
        theme.title_alt
    );
}

#[test]
fn hsl_to_rgb_covers_every_hue_sextant() {
    use crate::splash::hsl_to_rgb;
    // One per branch of the sextant chain, plus the wrap.
    for (h, want) in [
        (0.0, (255, 0, 0)),
        (60.0, (255, 255, 0)),
        (120.0, (0, 255, 0)),
        (180.0, (0, 255, 255)),
        (240.0, (0, 0, 255)),
        (300.0, (255, 0, 255)),
        (360.0, (255, 0, 0)),
        (-60.0, (255, 0, 255)),
    ] {
        assert_eq!(hsl_to_rgb(h, 1.0, 0.5), want, "hue {h}");
    }
    // Saturation and lightness extremes are greys, not colours.
    assert_eq!(hsl_to_rgb(210.0, 0.0, 0.5), (128, 128, 128));
    assert_eq!(hsl_to_rgb(210.0, 1.0, 0.0), (0, 0, 0));
    assert_eq!(hsl_to_rgb(210.0, 1.0, 1.0), (255, 255, 255));
}
