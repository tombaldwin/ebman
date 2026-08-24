//! Cost Explorer: per-environment spend.
//!
//! Global service — it has one endpoint per partition, not per region,
//! and [`cost_explorer_client`] resolves the operator's own via
//! [`super::global_service_region`]. The client is built on first use,
//! because only `:cost on` reaches it.

use super::*;

/// Build a Cost Explorer client endpointed in the operator's partition.
///
/// Cost Explorer is global: one endpoint per partition. Called from the
/// wrong region it returns an empty result with no error — exactly the
/// silent failure an operator never debugs — so the region is set here
/// rather than inherited. It was hardcoded to `us-east-1`, which is the
/// right answer for the commercial partition and unusable in any other.
pub(super) fn cost_explorer_client(base: &SdkConfig) -> CostExplorerClient {
    let region = base.region().map(|r| r.to_string()).unwrap_or_default();
    let cfg = base
        .to_builder()
        .region(Region::new(super::global_service_region(&region)))
        .build();
    CostExplorerClient::new(&cfg)
}

/// The result of a cost fetch: the per-env rows, plus whether the page
/// cap cut the walk short.
///
/// `truncated` exists so a partial map can't be mistaken for a complete
/// one. It used to be invisible: the loop fell out of `MAX_COST_PAGES`
/// with a token still in hand, returned what it had, and the caller
/// persisted it to the 24-hour cache — so every env past the cap
/// rendered as unknown cost, indistinguishable from an untagged one,
/// for a day.
#[derive(Clone, Debug, Default)]
pub(crate) struct EnvCosts {
    pub rows: Vec<EnvCost>,
    pub truncated: bool,
}

/// One row of cost data — an EB env name and its monthly cost in USD
/// across the trailing window. Whole + fractional dollars; the SDK
/// returns strings and we parse at the boundary.
#[derive(Clone, Debug)]
pub(crate) struct EnvCost {
    pub env_name: String,
    /// Monthly USD spend (summed across the trailing-30d window).
    pub cost_usd: f64,
}

impl AwsClient {
    /// Per-env monthly cost from AWS Cost Explorer. One round trip;
    /// returns a row per env tag value the Cost Explorer API saw in
    /// the trailing-30-day window.
    ///
    /// Cost Explorer is rate-limited (~1 req/s per account) and slow
    /// (1-3s per query) — the caller is expected to cache the result
    /// for ~24h via `crate::cost_cache`. The 24h granularity matches
    /// AWS's own data freshness (most cost data lags ~24h).
    ///
    /// Returned costs span the full window — divide by ~30 days for
    /// a daily rate, or treat as a monthly figure (which is what
    /// every operator actually wants).
    ///
    /// Tag key: `elasticbeanstalk:environment-name` (the EB-set tag
    /// AWS adds to every env-owned resource by default). Envs whose
    /// resources have been re-tagged or never carried the tag won't
    /// show up — surface as zero / unknown rather than guessing.
    pub(crate) async fn fetch_env_costs(&self) -> Result<EnvCosts> {
        use aws_sdk_costexplorer::types::{DateInterval, GroupDefinition, GroupDefinitionType};

        // Trailing window — end is "today" (exclusive in Cost Explorer)
        // so the inclusive Start is 30 days ago. Cost Explorer dates
        // are ISO-8601 (YYYY-MM-DD) in UTC.
        let now = chrono::Utc::now().date_naive();
        let start = (now - chrono::Duration::days(30))
            .format("%Y-%m-%d")
            .to_string();
        let end = now.format("%Y-%m-%d").to_string();
        let time_period = DateInterval::builder()
            .start(start)
            .end(end)
            .build()
            .wrap_err("Cost Explorer DateInterval missing field")?;
        let group_by = GroupDefinition::builder()
            .r#type(GroupDefinitionType::Tag)
            .key("elasticbeanstalk:environment-name")
            .build();
        // Result format: results_by_time[N].groups[].keys[0] is the
        // tag value (prefixed with `elasticbeanstalk:environment-name$`
        // — the Cost Explorer SDK encodes the tag key in the group
        // key, separated by `$`). Strip the prefix to recover the env
        // name. Sum across the time buckets in case the window spans
        // multiple months. Pages through NextPageToken — the pre-0.27
        // single-call shape silently truncated large grouped results,
        // and the partial map was then cached for 24h (missing envs
        // indistinguishable from untagged ones).
        let mut totals: std::collections::HashMap<String, f64> = std::collections::HashMap::new();
        let mut next_page: Option<String> = None;
        // Page cap: Cost Explorer paginates, but 20 pages of grouped
        // monthly cost is already absurd for one account's EB fleet —
        // bound the loop so a pathological response can't spin.
        const MAX_COST_PAGES: usize = 20;
        for _page in 0..MAX_COST_PAGES {
            let mut req = self
                .cost()
                .get_cost_and_usage()
                .time_period(time_period.clone())
                .granularity(aws_sdk_costexplorer::types::Granularity::Monthly)
                .metrics("UnblendedCost")
                .group_by(group_by.clone());
            if let Some(t) = next_page.take() {
                req = req.next_page_token(t);
            }
            let resp = req.send().await.wrap_err("GetCostAndUsage failed")?;
            for period in resp.results_by_time.unwrap_or_default() {
                for group in period.groups.unwrap_or_default() {
                    let raw_key = match group.keys.as_ref().and_then(|k| k.first()) {
                        Some(k) => k.clone(),
                        None => continue,
                    };
                    // Cost Explorer encodes a tag group key as
                    // `elasticbeanstalk:environment-name$<value>`. The
                    // empty-tag bucket (resources untagged) shows up as
                    // the bare prefix — skip it.
                    let env_name = match raw_key.split_once('$') {
                        Some((_, v)) if !v.is_empty() => v.to_string(),
                        _ => continue,
                    };
                    let amount: f64 = group
                        .metrics
                        .as_ref()
                        .and_then(|m| m.get("UnblendedCost"))
                        .and_then(|m| m.amount.as_deref())
                        .and_then(|s| s.parse().ok())
                        // Non-finite amounts (a corrupted response
                        // would otherwise poison the sum, the sort
                        // comparator, and the serialized cache).
                        .filter(|a: &f64| a.is_finite())
                        .unwrap_or(0.0);
                    *totals.entry(env_name).or_insert(0.0) += amount;
                }
            }
            match resp.next_page_token {
                Some(t) if !t.is_empty() => next_page = Some(t),
                _ => {
                    // Walked the whole result. Clear the token so the
                    // check below can't see a stale one from the
                    // previous iteration.
                    next_page = None;
                    break;
                }
            }
        }
        // A token still in hand means the cap cut us short.
        let truncated = next_page.is_some();
        if truncated {
            tracing::warn!(
                target: "ebman::aws",
                max_pages = MAX_COST_PAGES,
                envs = totals.len(),
                "Cost Explorer page cap reached with more pages available — \
                 costs are incomplete and will not be cached"
            );
        }
        let mut out: Vec<EnvCost> = totals
            .into_iter()
            .map(|(env_name, cost_usd)| EnvCost { env_name, cost_usd })
            .collect();
        // Stable ordering — highest-cost first so the operator's eye
        // catches the expensive envs without scrolling.
        out.sort_by(|a, b| {
            b.cost_usd
                .partial_cmp(&a.cost_usd)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        Ok(EnvCosts {
            rows: out,
            truncated,
        })
    }
}
