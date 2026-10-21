//! Tests for `aws`: CloudWatch Logs.

use super::*;

#[test]
fn parse_window_ms_accepts_minutes_hours_days() {
    // Seconds — the unit every doc example (`--interval 60s`)
    // used but the parser rejected until the 0.26 max-review.
    assert_eq!(super::parse_window_ms("60s"), Some(60_000));
    assert_eq!(super::parse_window_ms("30m"), Some(30 * 60_000));
    assert_eq!(super::parse_window_ms("1h"), Some(60 * 60_000));
    assert_eq!(super::parse_window_ms("6h"), Some(6 * 60 * 60_000));
    assert_eq!(super::parse_window_ms("24h"), Some(24 * 60 * 60_000));
    assert_eq!(super::parse_window_ms("7d"), Some(7 * 24 * 60 * 60_000));
    // Whitespace-trimmed.
    assert_eq!(super::parse_window_ms("  2h  "), Some(2 * 60 * 60_000));
    // Case-insensitive on unit.
    assert_eq!(super::parse_window_ms("3H"), Some(3 * 60 * 60_000));
}

#[test]
fn parse_window_ms_rejects_malformed_input() {
    // Empty.
    assert_eq!(super::parse_window_ms(""), None);
    // Missing unit.
    assert_eq!(super::parse_window_ms("30"), None);
    // Missing number.
    assert_eq!(super::parse_window_ms("h"), None);
    // Unknown unit (y / w).
    assert_eq!(super::parse_window_ms("1y"), None);
    assert_eq!(super::parse_window_ms("2w"), None);
    // Non-positive — silently substituting 0 would surprise the operator.
    assert_eq!(super::parse_window_ms("0h"), None);
    assert_eq!(super::parse_window_ms("-1h"), None);
    // Garbage.
    assert_eq!(super::parse_window_ms("hour"), None);
    // Overflow / absurd windows reject rather than wrap (the
    // wrapped value panicked in debug and silently filtered
    // everything in release; chrono panics past ±262k years).
    assert_eq!(super::parse_window_ms("999999999999d"), None);
    assert_eq!(super::parse_window_ms("9999999999d"), None);
    assert_eq!(
        super::parse_window_ms("36500d"),
        Some(36_500 * 24 * 60 * 60_000)
    );
}

#[test]
fn format_insights_results_renders_table() {
    let results = InsightsResults {
        rows: vec![
            InsightsRow {
                fields: vec![
                    ("@timestamp".into(), "2026-05-23T10:00:00Z".into()),
                    ("@message".into(), "POST /checkout 200 42ms".into()),
                    ("@ptr".into(), "CWL_PTR_X".into()),
                ],
            },
            InsightsRow {
                fields: vec![
                    ("@timestamp".into(), "2026-05-23T10:00:01Z".into()),
                    ("@message".into(), "GET /healthcheck 200 1ms".into()),
                    ("@ptr".into(), "CWL_PTR_Y".into()),
                ],
            },
        ],
        records_scanned: 1234,
        records_matched: 2,
    };
    let body = super::format_insights_results(
        &results,
        "fields @timestamp, @message",
        &["/aws/elasticbeanstalk/prod/var/log/web.stdout.log".to_string()],
    );
    assert!(
        body.contains("matched: 2 / scanned: 1234"),
        "stats line present"
    );
    assert!(body.contains("@timestamp"), "@timestamp header present");
    assert!(body.contains("@message"), "@message header present");
    // @ptr is a record-locator field — always dropped from operator-facing output.
    assert!(
        !body.contains("@ptr"),
        "@ptr field should be filtered out of the rendered table"
    );
    assert!(body.contains("POST /checkout"), "first row body present");
    assert!(body.contains("GET /healthcheck"), "second row body present");
}

#[test]
fn format_insights_results_empty_input_shows_no_rows_stub() {
    let results = InsightsResults {
        rows: vec![],
        records_scanned: 1000,
        records_matched: 0,
    };
    let body = super::format_insights_results(
        &results,
        "fields @message | filter @message like /never/",
        &["/aws/elasticbeanstalk/prod/var/log/web.stdout.log".to_string()],
    );
    assert!(body.contains("no rows matched"), "empty-input stub fires");
    assert!(
        body.contains("matched: 0 / scanned: 1000"),
        "stats line still present"
    );
}

#[test]
fn format_insights_results_truncates_long_values() {
    // A 200-character message should get truncated to ≤ COL_MAX (60)
    // so the table doesn't dominate the overlay.
    let huge = "x".repeat(200);
    let results = InsightsResults {
        rows: vec![InsightsRow {
            fields: vec![("@message".into(), huge.clone())],
        }],
        records_scanned: 1,
        records_matched: 1,
    };
    let body = super::format_insights_results(&results, "fields @message", &[]);
    assert!(
        !body.contains(&huge),
        "raw 200-char value should not appear untouched"
    );
    assert!(
        body.contains("…"),
        "truncation marker should signal the cut to the operator"
    );
}

// ── Regression #1 ────────────────────────────────────────────────────
// `DescribeConfigurationSettings` returns `WorkerQueueURL = ""` when
// EB autocreates the queue (the operator didn't override it). The
// original code looked only at option settings and would show "no
// queue" for the most common worker-tier shape. The fix queries
// `DescribeEnvironmentResources` first and only falls back to option
// settings when explicit overrides exist.

#[tokio::test]
async fn log_tail_skips_already_delivered_boundary_ids() {
    // After a page-capped poll the watermark stays AT the boundary
    // millisecond and its events are re-fetched — the carried id
    // set must filter them so the overlay shows no duplicates.
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;

    let page = aws_smithy_mocks::mock!(CwLogsClient::filter_log_events).then_output(|| {
        FilterLogEventsOutput::builder()
            .events(
                FilteredLogEvent::builder()
                    .timestamp(1_000)
                    .event_id("e1")
                    .log_stream_name("i-abc")
                    .message("already delivered")
                    .build(),
            )
            .events(
                FilteredLogEvent::builder()
                    .timestamp(1_000)
                    .event_id("e2")
                    .log_stream_name("i-abc")
                    .message("new at boundary")
                    .build(),
            )
            .build()
    });
    let cw_logs = aws_smithy_mocks::mock_client!(aws_sdk_cloudwatchlogs, [&page]);
    let client = client_with_cw_logs(cw_logs);
    let skip: std::collections::HashSet<String> = ["e1".to_string()].into_iter().collect();
    let (events, next_since, _carry) = client
        .fetch_recent_log_events("/aws/eb/env", 1_000, 1000, &skip)
        .await
        .expect("ok");
    let msgs: Vec<&str> = events.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(msgs, vec!["new at boundary"], "e1 filtered, e2 delivered");
    assert_eq!(next_since, 1_000, "no newer event — watermark holds");
}

/// A complete scan of a window that held more than `want` says both.
///
/// Field-reported on a live fleet: two hours of nginx access log,
/// `limit: 5`, five newest rows back and `complete: true` beside them.
/// True about the scan — it walked the whole window — and read as
/// "nothing else is in here", which is the opposite of what the field
/// exists to warn about.
///
/// `complete` and `truncated_by_limit` are different facts and can
/// both be true. Conflating them meant the common case (a small limit
/// on a busy log) reported completeness it did not have.
#[tokio::test]
async fn a_complete_scan_still_reports_truncation_by_limit() {
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;

    let ev = |ms: i64| {
        FilteredLogEvent::builder()
            .timestamp(ms)
            .log_stream_name("i-1")
            .message(format!("line at {ms}"))
            .build()
    };
    // One page, six events, no next_token: the scan IS complete.
    let rule = mock!(aws_sdk_cloudwatchlogs::Client::filter_log_events)
        .sequence()
        .output(move || {
            FilterLogEventsOutput::builder()
                .events(ev(1))
                .events(ev(2))
                .events(ev(3))
                .events(ev(4))
                .events(ev(5))
                .events(ev(6))
                .build()
        })
        .build();
    let client = client_with_cw_logs(mock_client!(aws_sdk_cloudwatchlogs, [&rule]));

    let (events, complete, truncated) = client
        .fetch_latest_log_events("/aws/eb/env", 0, 2, None)
        .await
        .expect("fetch");

    assert!(complete, "one page and no next token — the scan finished");
    assert!(
        truncated,
        "but six events were seen and two were asked for, so what comes back \
         is the newest slice of a larger set and must say so"
    );
    assert_eq!(events.len(), 2, "the limit is honoured");
    assert_eq!(
        events.iter().map(|e| e.timestamp_ms).collect::<Vec<_>>(),
        vec![5, 6],
        "and it keeps the NEWEST two, which is the whole point of the fetch"
    );
}

#[tokio::test]
async fn log_tail_fetch_follows_next_token_without_skipping_events() {
    // 0.27 fix: a truncated FilterLogEvents page used to advance
    // the watermark past events it never received — silent line
    // drops during traffic spikes.
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;

    let mk = |ts: i64, msg: &str| {
        FilteredLogEvent::builder()
            .timestamp(ts)
            .log_stream_name("i-abc")
            .message(msg)
            .build()
    };
    let page1 = aws_smithy_mocks::mock!(CwLogsClient::filter_log_events)
        .match_requests(|req| req.next_token().is_none())
        .then_output(move || {
            FilterLogEventsOutput::builder()
                .events(mk(1_000, "a"))
                .events(mk(1_005, "b"))
                .next_token("PAGE_2")
                .build()
        });
    let page2 = aws_smithy_mocks::mock!(CwLogsClient::filter_log_events)
        .match_requests(|req| req.next_token() == Some("PAGE_2"))
        .then_output(move || {
            FilterLogEventsOutput::builder()
                .events(mk(1_005, "c"))
                .events(mk(1_010, "d"))
                .build()
        });
    let cw_logs = aws_smithy_mocks::mock_client!(aws_sdk_cloudwatchlogs, [&page1, &page2]);
    let client = client_with_cw_logs(cw_logs);

    let (events, next_since, carry) = client
        .fetch_recent_log_events("/aws/eb/env", 500, 1000, &Default::default())
        .await
        .expect("ok");
    assert!(
        carry.is_empty(),
        "clean (non-truncated) poll carries no boundary ids"
    );
    let msgs: Vec<&str> = events.iter().map(|e| e.message.as_str()).collect();
    assert_eq!(
        msgs,
        vec!["a", "b", "c", "d"],
        "both pages' events delivered — none skipped"
    );
    assert_eq!(
        next_since, 1_011,
        "watermark advances past the newest RECEIVED event"
    );
    assert_eq!(page1.num_calls(), 1);
    assert_eq!(page2.num_calls(), 1);
}

#[test]
fn insights_columns_are_the_union_across_rows_not_just_row_zero() {
    // Insights omits an absent field from a record rather than
    // returning it empty. Taking headers from row 0 alone dropped the
    // `level` column for EVERY row whenever the first matching record
    // happened to be an unstructured line — silently discarding data
    // the operator asked for by name in `fields`.
    let results = InsightsResults {
        rows: vec![
            insights_row(&[("@timestamp", "T1"), ("@message", "plain line")]),
            insights_row(&[
                ("@timestamp", "T2"),
                ("@message", "structured"),
                ("level", "ERROR"),
            ]),
        ],
        records_scanned: 2,
        records_matched: 2,
    };
    let out = format_insights_results(&results, "fields @timestamp, @message, level", &[]);
    assert!(out.contains("level"), "level column must appear:\n{out}");
    assert!(out.contains("ERROR"), "its value must render:\n{out}");
    // The row that lacks the field renders blank, not omitted.
    let body: Vec<&str> = out.lines().filter(|l| l.starts_with('T')).collect();
    assert_eq!(body.len(), 2, "both rows render:\n{out}");
    assert!(body[0].contains("plain line"));
}

#[test]
fn insights_drops_the_synthetic_ptr_field_from_every_row() {
    let results = InsightsResults {
        rows: vec![
            insights_row(&[("@ptr", "abc"), ("@message", "one")]),
            insights_row(&[("@ptr", "def"), ("@message", "two")]),
        ],
        records_scanned: 2,
        records_matched: 2,
    };
    let out = format_insights_results(&results, "q", &[]);
    assert!(
        !out.contains("@ptr"),
        "@ptr must not reach the overlay:\n{out}"
    );
    assert!(!out.contains("abc"));
}

#[test]
fn insights_column_widths_are_measured_in_chars_not_bytes() {
    // A non-ASCII header measured with `len()` over-reserves (three
    // bytes per `é`) while the padding and the separator count chars,
    // so the rule under the header ran short and the columns stepped.
    let results = InsightsResults {
        rows: vec![insights_row(&[("réqüest", "x")])],
        records_scanned: 1,
        records_matched: 1,
    };
    let out = format_insights_results(&results, "q", &[]);
    let lines: Vec<&str> = out.lines().collect();
    let hdr = lines
        .iter()
        .position(|l| l.contains("réqüest"))
        .expect("header row");
    let header_cells = lines[hdr].trim_end().chars().count();
    let sep_cells = lines[hdr + 1].chars().count();
    assert_eq!(
        header_cells,
        sep_cells,
        "separator must be exactly as wide as the header it underlines\nheader: {:?}\nsep:    {:?}",
        lines[hdr],
        lines[hdr + 1]
    );
}

// ── log-tail boundary dedupe survives a stalled watermark ──────────

#[tokio::test]
async fn log_tail_does_not_re_emit_after_a_truncated_poll_goes_quiet() {
    // The loop: a truncated poll stalls the watermark at `max_ts` and
    // carries that millisecond's ids. The group then goes quiet, so the
    // next poll skips them correctly, delivers nothing — and, not being
    // truncated itself, used to drop the carry while leaving the
    // watermark where it was. Every poll after that re-fetched the same
    // events with an empty skip set and re-printed them.
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;
    use std::collections::HashSet;

    fn event() -> FilteredLogEvent {
        FilteredLogEvent::builder()
            .event_id("EV-1")
            .timestamp(1_000)
            .log_stream_name("i-abc")
            .message("boundary line")
            .build()
    }
    // First poll: page 1 returns the event and a token; every following
    // page keeps handing back a token, so the page cap is reached and
    // the poll ends truncated with the watermark stalled at 1000.
    let first = mock!(aws_sdk_cloudwatchlogs::Client::filter_log_events)
        .match_requests(|req| req.start_time() == Some(500) && req.next_token().is_none())
        .then_output(|| {
            FilterLogEventsOutput::builder()
                .events(event())
                .next_token("MORE")
                .build()
        });
    let more = mock!(aws_sdk_cloudwatchlogs::Client::filter_log_events)
        .match_requests(|req| req.next_token() == Some("MORE"))
        .then_output(|| FilterLogEventsOutput::builder().next_token("MORE").build());
    // Later polls start at the stalled watermark and see only the same
    // event again — the group has gone quiet.
    let quiet = mock!(aws_sdk_cloudwatchlogs::Client::filter_log_events)
        .match_requests(|req| req.start_time() == Some(1_000) && req.next_token().is_none())
        .then_output(|| FilterLogEventsOutput::builder().events(event()).build());

    // MatchAny, not the default Sequential: these rules describe
    // request *shapes* that recur across polls, not a fixed call order.
    let cw_logs = mock_client!(
        aws_sdk_cloudwatchlogs,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&first, &more, &quiet]
    );
    let client = client_with_cw_logs(cw_logs);

    // Poll 1 — truncated, stalls at 1000, carries EV-1.
    let (events, next_since, carry) = client
        .fetch_recent_log_events("/aws/eb/api-prod", 500, 1000, &HashSet::new())
        .await
        .unwrap();
    assert!(!events.is_empty(), "poll 1 delivers the line");
    assert_eq!(next_since, 1_000, "truncated poll must not skip the ms");
    assert!(carry.contains("EV-1"));

    // Poll 2 — quiet. Skips the known id, delivers nothing, watermark
    // still stalled, so the carry must survive.
    let (events, next_since, carry) = client
        .fetch_recent_log_events("/aws/eb/api-prod", next_since, 1000, &carry)
        .await
        .unwrap();
    assert!(events.is_empty(), "already-delivered line must be skipped");
    assert_eq!(next_since, 1_000);
    assert!(
        carry.contains("EV-1"),
        "the watermark did not move, so the skip set must be kept"
    );

    // Poll 3 — this is where the loop used to start.
    let (events, _, _) = client
        .fetch_recent_log_events("/aws/eb/api-prod", next_since, 1000, &carry)
        .await
        .unwrap();
    assert!(
        events.is_empty(),
        "the same line must not be re-emitted on every subsequent poll"
    );
}

#[tokio::test]
async fn log_tail_clean_poll_advances_past_the_boundary_and_carries_nothing() {
    // The complement: when the watermark does advance past everything
    // returned, nothing gets re-fetched, so nothing needs carrying.
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;
    use std::collections::HashSet;

    let page = mock!(aws_sdk_cloudwatchlogs::Client::filter_log_events).then_output(|| {
        FilterLogEventsOutput::builder()
            .events(
                FilteredLogEvent::builder()
                    .event_id("EV-9")
                    .timestamp(2_000)
                    .log_stream_name("i-abc")
                    .message("line")
                    .build(),
            )
            .build()
    });
    let cw_logs = mock_client!(aws_sdk_cloudwatchlogs, [&page]);
    let client = client_with_cw_logs(cw_logs);

    let (events, next_since, carry) = client
        .fetch_recent_log_events("/aws/eb/api-prod", 500, 1000, &HashSet::new())
        .await
        .unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(next_since, 2_001, "clean poll advances past the newest ms");
    assert!(
        carry.is_empty(),
        "nothing is re-fetched, so nothing carries"
    );
}

/// The newest-first point query, through the real SDK path.
///
/// `fetch_latest_log_events` had no test: it needs CloudWatch, so only
/// its rolling-tail helper was covered. The paging, the `complete`
/// flag and the filter pattern were all unpinned — and this is the
/// function whose entire reason to exist is not returning the oldest
/// lines in the window.
#[tokio::test]
async fn latest_log_events_returns_the_newest_across_pages() {
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;

    let ev = |ts: i64, msg: &'static str| {
        FilteredLogEvent::builder()
            .timestamp(ts)
            .event_id(format!("e{ts}"))
            .log_stream_name("i-abc")
            .message(msg)
            .build()
    };

    // Two pages, oldest-first as CloudWatch actually returns them.
    let page1 = aws_smithy_mocks::mock!(CwLogsClient::filter_log_events)
        .match_requests(|req| req.next_token().is_none())
        .then_output(move || {
            FilterLogEventsOutput::builder()
                .events(ev(1_000, "oldest"))
                .events(ev(2_000, "older"))
                .next_token("PAGE_2")
                .build()
        });
    let page2 = aws_smithy_mocks::mock!(CwLogsClient::filter_log_events)
        .match_requests(|req| req.next_token() == Some("PAGE_2"))
        .then_output(move || {
            FilterLogEventsOutput::builder()
                .events(ev(3_000, "newer"))
                .events(ev(4_000, "newest"))
                .build()
        });
    let cw_logs = aws_smithy_mocks::mock_client!(aws_sdk_cloudwatchlogs, [&page1, &page2]);
    let client = client_with_cw_logs(cw_logs);

    let (events, complete, _truncated) = client
        .fetch_latest_log_events("/aws/eb/env", 0, 2, None)
        .await
        .expect("ok");

    assert_eq!(
        events
            .iter()
            .map(|e| e.message.as_str())
            .collect::<Vec<_>>(),
        vec!["newer", "newest"],
        "asking for 2 must give the NEWEST 2 — CloudWatch returns \
         oldest-first, so the naive implementation gives \"oldest\" and \
         \"older\" and reads as a task that stopped hours ago"
    );
    assert!(
        complete,
        "the last page carried no token, so the window was fully read"
    );
}

/// When the page cap is hit, the answer must say so.
///
/// This is the load-bearing half: a truncated window leaves us holding
/// the OLDEST part of it, which is the opposite of what was asked for.
/// Silently returning those is the failure the `complete` flag exists
/// to prevent.
#[tokio::test]
async fn a_truncated_window_reports_incomplete() {
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;
    use aws_sdk_cloudwatchlogs::types::FilteredLogEvent;

    // Every page hands back another token: the cap is reached and never
    // exhausted.
    let endless = aws_smithy_mocks::mock!(CwLogsClient::filter_log_events).then_output(|| {
        FilterLogEventsOutput::builder()
            .events(
                FilteredLogEvent::builder()
                    .timestamp(1_000)
                    .event_id("e1")
                    .log_stream_name("i-abc")
                    .message("line")
                    .build(),
            )
            .next_token("MORE")
            .build()
    });
    // `MatchAny` so the one rule serves every page: the point of this
    // fixture is a window that NEVER exhausts, and a sequential rule is
    // consumed after its first use.
    let cw_logs = aws_smithy_mocks::mock_client!(
        aws_sdk_cloudwatchlogs,
        aws_smithy_mocks::RuleMode::MatchAny,
        [&endless]
    );
    let client = client_with_cw_logs(cw_logs);

    let (events, complete, _truncated) = client
        .fetch_latest_log_events("/aws/eb/env", 0, 10, None)
        .await
        .expect("ok");
    assert!(
        !complete,
        "the page cap was hit with a token outstanding — the caller must \
         be told to narrow the window rather than shown the oldest lines \
         as though they were the newest"
    );
    assert!(!events.is_empty(), "and what was read is still returned");
}

/// A filter pattern must reach the request.
///
/// Dropping it silently returns every line in the window, which looks
/// like a working query with a lot of noise rather than a broken one.
#[tokio::test]
async fn the_log_filter_pattern_reaches_the_request() {
    use aws_sdk_cloudwatchlogs::operation::filter_log_events::FilterLogEventsOutput;

    let filtered = aws_smithy_mocks::mock!(CwLogsClient::filter_log_events)
        .match_requests(|req| req.filter_pattern() == Some("ERROR"))
        .then_output(|| FilterLogEventsOutput::builder().build());
    let cw_logs = aws_smithy_mocks::mock_client!(aws_sdk_cloudwatchlogs, [&filtered]);
    let client = client_with_cw_logs(cw_logs);

    // The mock only matches when the pattern is present, so this
    // resolving at all is the assertion.
    let (events, _, _) = client
        .fetch_latest_log_events("/aws/eb/env", 0, 10, Some("ERROR"))
        .await
        .expect("the request must carry filter_pattern=ERROR");
    assert!(events.is_empty());
}
