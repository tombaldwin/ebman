//! CloudWatch Logs: discovering an environment's log groups, the
//! recent-events fetch behind `:logs-tail`, and Logs Insights queries.

use super::*;

/// One event from a CloudWatch Logs stream — server-side timestamp + the
/// stream it came from + the raw message. `:logs-tail` builds these from
/// FilterLogEvents and renders them in chronological order.
#[derive(Clone, Debug)]
pub struct LogEvent {
    pub timestamp_ms: i64,
    pub stream: String,
    pub message: String,
}

/// One result row from a CloudWatch Logs Insights query. Each entry is
/// a (field-name, value) pair — Insights returns fields in query-order,
/// so the Vec preserves that order rather than a HashMap.
#[derive(Clone, Debug)]
pub struct InsightsRow {
    pub fields: Vec<(String, String)>,
}

/// The completed payload of an Insights query — result rows plus the
/// scan statistics (records_matched / records_scanned). Scanned is what
/// AWS bills against; surfacing it in the overlay footer makes the cost
/// of broad queries visible.
#[derive(Clone, Debug)]
pub struct InsightsResults {
    pub rows: Vec<InsightsRow>,
    pub records_scanned: i64,
    pub records_matched: i64,
}

/// Pure: parse a time-window spec for `:logs-insights --window WINDOW`.
/// Accepts `<n><unit>` with unit `m` / `h` / `d` — e.g. `30m`, `6h`, `7d`.
/// Returns the window length in *milliseconds* so callers can subtract
/// from `Utc::now().timestamp_millis()` directly. Returns `None` for
/// malformed input or non-positive values so the caller surfaces a
/// usage error instead of silently substituting a wrong window.
///
/// Deliberately a superset of `parse_replay_spec` in `mode_dlq.rs`:
/// same `m` / `h` / `d` units and the same overflow guards, plus `s`
/// (seconds), which makes sense for a log window and not for a DLQ
/// replay.
pub fn parse_window_ms(input: &str) -> Option<i64> {
    let s = input.trim().to_lowercase();
    if s.is_empty() {
        return None;
    }
    let unit = s.chars().last()?;
    let num: i64 = s[..s.len() - unit.len_utf8()].parse().ok()?;
    if num <= 0 {
        return None;
    }
    // checked_mul: an absurd window ("999999999999d") must reject,
    // not overflow — the wrapped value silently filters everything
    // out in release and panics in debug (and via the MCP audit_log
    // tool, that panic left the request unanswered forever).
    let ms = match unit {
        's' => num.checked_mul(1_000),
        'm' => num.checked_mul(60_000),
        'h' => num.checked_mul(60 * 60_000),
        'd' => num.checked_mul(24 * 60 * 60_000),
        _ => return None,
    }?;
    // Bound to chrono-safe range: callers subtract this from now()
    // as a chrono::Duration, which panics past ±262,000 years.
    // 100 years of milliseconds is far beyond any real window.
    const MAX_WINDOW_MS: i64 = 100 * 365 * 24 * 60 * 60 * 1_000;
    if ms > MAX_WINDOW_MS {
        return None;
    }
    Some(ms)
}

/// Pure: render an `InsightsResults` payload to a multi-line string
/// suitable for a TextOverlay body.
///
/// Columns are the union of every row's field names, in first-seen
/// order — *not* row 0's. Insights omits an absent field from a record
/// rather than returning it empty, so taking the header from the first
/// row drops any field that record happens to lack, for every row. An
/// earlier version of this doc asserted the opposite guarantee; see
/// `insights_columns_are_the_union_across_rows_not_just_row_zero`.
///
/// Each cell is width-padded against the column max (measured in
/// chars, since that is what the padding counts) so the result reads
/// like a table. Long values are truncated to keep the overlay
/// readable. Empty input renders as a "no rows matched" stub plus the
/// scan stats — same shape so the overlay never collapses.
pub fn format_insights_results(
    results: &InsightsResults,
    query: &str,
    log_groups: &[String],
) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "query: {query}\nlog groups: {}\nmatched: {} / scanned: {}\n",
        if log_groups.is_empty() {
            "(none)".to_string()
        } else {
            log_groups.join(", ")
        },
        results.records_matched,
        results.records_scanned,
    ));
    out.push_str(&"─".repeat(60));
    out.push('\n');
    if results.rows.is_empty() {
        out.push_str("(no rows matched the query)\n");
        return out;
    }
    // Column set is the UNION of every row's fields, in first-seen
    // order. Insights omits an absent field from a record rather than
    // returning it empty, so taking the columns from row 0 alone drops
    // any field the first matching record happens not to carry — for
    // every row, including the ones that do carry it. A row missing a
    // field renders blank below, which is the honest presentation.
    //
    // Skip the synthetic `@ptr` field — a record locator for the API to
    // drill back to individual events, not useful in the overlay.
    let mut headers: Vec<String> = Vec::new();
    for row in &results.rows {
        for (k, _) in &row.fields {
            if k != "@ptr" && !headers.iter().any(|h| h == k) {
                headers.push(k.clone());
            }
        }
    }
    // Per-column max-width pass — bounded at 60 cells so a single huge
    // message field doesn't push every other column off-screen.
    //
    // `chars().count()`, not `len()`: the values below are measured in
    // chars and `{:<w$}` pads in chars, so a byte length here would
    // over-reserve for a non-ASCII header and desync the separator.
    const COL_MAX: usize = 60;
    let mut widths: Vec<usize> = headers.iter().map(|h| h.chars().count()).collect();
    for row in &results.rows {
        for (i, h) in headers.iter().enumerate() {
            if let Some((_, v)) = row.fields.iter().find(|(k, _)| k == h) {
                let cells = v.chars().count().min(COL_MAX);
                if cells > widths[i] {
                    widths[i] = cells;
                }
            }
        }
    }
    // Header row.
    let mut header_line = String::new();
    for (i, h) in headers.iter().enumerate() {
        if i > 0 {
            header_line.push_str("  ");
        }
        header_line.push_str(&format!("{:<w$}", h, w = widths[i]));
    }
    out.push_str(&header_line);
    out.push('\n');
    // Separator.
    let mut sep_line = String::new();
    for (i, w) in widths.iter().enumerate() {
        if i > 0 {
            sep_line.push_str("  ");
        }
        sep_line.push_str(&"─".repeat(*w));
    }
    out.push_str(&sep_line);
    out.push('\n');
    // Data rows.
    for row in &results.rows {
        let mut line = String::new();
        for (i, h) in headers.iter().enumerate() {
            if i > 0 {
                line.push_str("  ");
            }
            let raw = row
                .fields
                .iter()
                .find(|(k, _)| k == h)
                .map(|(_, v)| v.as_str())
                .unwrap_or("");
            let trimmed: String = if raw.chars().count() > COL_MAX {
                let mut s: String = raw.chars().take(COL_MAX.saturating_sub(1)).collect();
                s.push('…');
                s
            } else {
                raw.to_string()
            };
            line.push_str(&format!("{:<w$}", trimmed, w = widths[i]));
        }
        out.push_str(&line);
        out.push('\n');
    }
    out
}

impl AwsClient {
    /// Discover the CloudWatch Logs groups an EB env streams to. EB names
    /// them under the prefix `/aws/elasticbeanstalk/{env}/...` so we
    /// `DescribeLogGroups` with that prefix. Returns sorted group names;
    /// empty if `:logs-stream on` hasn't been issued for the env.
    pub async fn discover_env_log_groups(&self, env_name: &str) -> Result<Vec<String>> {
        let prefix = format!("/aws/elasticbeanstalk/{env_name}/");
        let (this, pfx) = (self, prefix.as_str());
        let raw = super::paginate("DescribeLogGroups", move |token| async move {
            let mut req = this
                .cw_logs
                .describe_log_groups()
                .log_group_name_prefix(pfx);
            if let Some(t) = token {
                req = req.next_token(t);
            }
            let resp = req.send().await.wrap_err("DescribeLogGroups failed")?;
            Ok((resp.log_groups.unwrap_or_default(), resp.next_token))
        })
        .await?
        .items();
        let mut out: Vec<String> = raw.into_iter().filter_map(|g| g.log_group_name).collect();
        out.sort();
        Ok(out)
    }

    /// Fetch events from one CW Logs group since `since_ms` (Unix
    /// milliseconds). Uses `FilterLogEvents` so the result spans all log
    /// streams in the group in chronological order — that's how an EB-tier
    /// log group works (one stream per instance). The returned tuple is
    /// `(events, next_since_ms)` where `next_since_ms` is the highest
    /// timestamp + 1 we saw, suitable to pass back on the next call.
    pub async fn fetch_recent_log_events(
        &self,
        log_group: &str,
        since_ms: i64,
        limit: i32,
        // Event ids already delivered at exactly `since_ms` — the
        // truncated-poll watermark doesn't advance past the boundary
        // millisecond (see below), so its events are re-fetched; this
        // set filters them out instead of showing duplicate lines.
        skip_at_since: &std::collections::HashSet<String>,
    ) -> Result<(Vec<LogEvent>, i64, std::collections::HashSet<String>)> {
        // Follow `next_token` up to a page cap: FilterLogEvents
        // truncates at `limit` OR its 1MB response cap and hands back
        // a token. The pre-0.27 single-page shape advanced the
        // watermark past events it never received — during a traffic
        // spike (the exact moment an operator is watching the tail),
        // lines silently vanished with no gap indicator. The cap
        // bounds one poll's work; anything beyond it is picked up by
        // the next poll because the watermark only advances past
        // RETURNED events.
        const MAX_PAGES_PER_POLL: usize = 5;
        let mut out: Vec<LogEvent> = Vec::new();
        let mut max_ts = since_ms;
        let mut next_token: Option<String> = None;
        let mut truncated = false;
        let mut boundary_ids: std::collections::HashSet<String> = std::collections::HashSet::new();
        for _page in 0..MAX_PAGES_PER_POLL {
            let mut req = self
                .cw_logs
                .filter_log_events()
                .log_group_name(log_group)
                .start_time(since_ms)
                .limit(limit);
            if let Some(t) = next_token.take() {
                req = req.next_token(t);
            }
            let resp = req.send().await.wrap_err("FilterLogEvents failed")?;
            for e in resp.events.unwrap_or_default() {
                let ts = e.timestamp.unwrap_or(since_ms);
                let id = e.event_id.unwrap_or_default();
                if ts == since_ms && !id.is_empty() && skip_at_since.contains(&id) {
                    continue;
                }
                if ts > max_ts {
                    max_ts = ts;
                    boundary_ids.clear();
                }
                if ts == max_ts && !id.is_empty() {
                    boundary_ids.insert(id);
                }
                out.push(LogEvent {
                    timestamp_ms: ts,
                    stream: e.log_stream_name.unwrap_or_default(),
                    message: e.message.unwrap_or_default(),
                });
            }
            match resp.next_token {
                Some(t) if !t.is_empty() => {
                    next_token = Some(t);
                    truncated = true;
                }
                _ => {
                    truncated = false;
                    break;
                }
            }
        }
        // Move the cursor past the newest event we RECEIVED so the
        // next poll doesn't return it again. If the page cap left a
        // token unfollowed, do NOT skip past `max_ts` — same-ms events
        // in unfetched pages would be lost; the next poll re-fetches
        // from `max_ts` and the (rare) duplicates are bounded to one
        // millisecond of events.
        let next_since = if max_ts > since_ms {
            if truncated {
                max_ts
            } else {
                max_ts + 1
            }
        } else {
            since_ms
        };
        // The carry set must hold every id already delivered at exactly
        // `next_since`, because that is the millisecond the next poll
        // re-fetches (FilterLogEvents' `start_time` is inclusive).
        //
        // Keying this off `truncated` alone was wrong in the case where
        // the watermark doesn't move. A truncated poll stalls it at
        // `max_ts` and carries that ms's ids; if the group then goes
        // quiet, the next poll skips those ids correctly, delivers
        // nothing, and is NOT truncated — so it dropped the carry while
        // leaving the watermark where it was. The poll after that
        // re-fetched the same events with an empty skip set and showed
        // them again, and so did every poll after it: `:logs-tail`
        // re-printed the same lines every 2 s until a newer event
        // arrived. Keying off "did the watermark move" instead ties the
        // suppression to the thing that actually causes the re-fetch.
        let carry = if next_since == since_ms {
            // Stalled. Everything ever delivered at this millisecond
            // has to stay suppressed: the ids we were given plus any
            // we added this time round.
            let mut c = skip_at_since.clone();
            c.extend(boundary_ids);
            c
        } else if truncated {
            // Advanced to `max_ts` but not past it. That is a
            // millisecond we only reached this poll, so nothing was
            // delivered at it before now.
            boundary_ids
        } else {
            // Advanced past everything returned; nothing gets
            // re-fetched, so nothing needs carrying.
            std::collections::HashSet::new()
        };
        Ok((out, next_since, carry))
    }

    /// Run a CloudWatch Logs Insights query against `log_groups` over the
    /// `[start_ms, end_ms]` window. Starts the query via `StartQuery`, polls
    /// `GetQueryResults` every 2 seconds until the status leaves
    /// Scheduled/Running, and returns the final result rows + scan stats.
    /// The terminal Failed / Cancelled / Timeout states surface as a clean
    /// error rather than empty rows so the caller can show the right toast.
    pub async fn run_insights_query(
        &self,
        log_groups: &[String],
        start_ms: i64,
        end_ms: i64,
        query: &str,
    ) -> Result<InsightsResults> {
        use aws_sdk_cloudwatchlogs::types::QueryStatus;
        // StartQuery's start_time / end_time are epoch *seconds*, not ms.
        let start_s = start_ms / 1000;
        let end_s = end_ms / 1000;
        let mut req = self
            .cw_logs
            .start_query()
            .start_time(start_s)
            .end_time(end_s)
            .query_string(query);
        for g in log_groups {
            req = req.log_group_names(g);
        }
        let start_resp = req.send().await.wrap_err("StartQuery failed")?;
        let query_id = start_resp
            .query_id
            .ok_or_else(|| eyre!("StartQuery returned no query_id"))?;

        // Poll every 2s. Insights queries are server-side timed (max 15
        // min by default) so we don't need our own watchdog; the API
        // surfaces Timeout when the server gives up.
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            let resp = self
                .cw_logs
                .get_query_results()
                .query_id(&query_id)
                .send()
                .await
                .wrap_err("GetQueryResults failed")?;
            let status = resp.status.clone();
            let scanned = resp
                .statistics
                .as_ref()
                .map(|s| s.records_scanned as i64)
                .unwrap_or(0);
            let matched = resp
                .statistics
                .as_ref()
                .map(|s| s.records_matched as i64)
                .unwrap_or(0);
            match status {
                Some(QueryStatus::Scheduled) | Some(QueryStatus::Running) => continue,
                Some(QueryStatus::Complete) => {
                    let rows: Vec<InsightsRow> = resp
                        .results
                        .unwrap_or_default()
                        .into_iter()
                        .map(|fields| InsightsRow {
                            fields: fields
                                .into_iter()
                                .map(|f| (f.field.unwrap_or_default(), f.value.unwrap_or_default()))
                                .collect(),
                        })
                        .collect();
                    return Ok(InsightsResults {
                        rows,
                        records_scanned: scanned,
                        records_matched: matched,
                    });
                }
                Some(QueryStatus::Failed) => {
                    return Err(eyre!("Insights query failed"));
                }
                Some(QueryStatus::Cancelled) => {
                    return Err(eyre!("Insights query was cancelled"));
                }
                Some(QueryStatus::Timeout) => {
                    return Err(eyre!("Insights query timed out (server-side 15min cap)"));
                }
                Some(other) => {
                    return Err(eyre!(
                        "unexpected Insights query status: {}",
                        other.as_str()
                    ));
                }
                None => {
                    return Err(eyre!("Insights query returned no status"));
                }
            }
        }
    }
}
