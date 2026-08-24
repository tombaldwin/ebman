//! Instance pricing and fleet cost rollups.
//!
//! The hourly-rate table is a coarse on-disk-free approximation — it
//! exists so `:cost` can render an order-of-magnitude number without a
//! Pricing API round-trip. See `cost_cache.rs` for the cached path.

use super::*;

/// Best-effort hourly USD price for an EC2 instance type, on-demand Linux,
/// us-east-1 as the baseline. Returned in USD/hour. Returns None for unknown
/// types — caller should label the estimate as "approximate (us-east-1)".
pub(crate) fn instance_hourly_usd(instance_type: &str) -> Option<f64> {
    // Hand-curated subset covering the families EB typically runs.
    // Prices are public list (on-demand Linux, us-east-1) as a baseline.
    match instance_type {
        // T-family burstable
        "t2.nano" => Some(0.0058),
        "t2.micro" => Some(0.0116),
        "t2.small" => Some(0.023),
        "t2.medium" => Some(0.0464),
        "t2.large" => Some(0.0928),
        "t3.nano" => Some(0.0052),
        "t3.micro" => Some(0.0104),
        "t3.small" => Some(0.0208),
        "t3.medium" => Some(0.0416),
        "t3.large" => Some(0.0832),
        "t3.xlarge" => Some(0.1664),
        "t3.2xlarge" => Some(0.3328),
        "t3a.nano" => Some(0.0047),
        "t3a.micro" => Some(0.0094),
        "t3a.small" => Some(0.0188),
        "t3a.medium" => Some(0.0376),
        "t3a.large" => Some(0.0752),
        "t4g.nano" => Some(0.0042),
        "t4g.micro" => Some(0.0084),
        "t4g.small" => Some(0.0168),
        "t4g.medium" => Some(0.0336),
        "t4g.large" => Some(0.0672),
        // General purpose
        "m5.large" => Some(0.096),
        "m5.xlarge" => Some(0.192),
        "m5.2xlarge" => Some(0.384),
        "m5.4xlarge" => Some(0.768),
        "m6i.large" => Some(0.096),
        "m6i.xlarge" => Some(0.192),
        "m6i.2xlarge" => Some(0.384),
        "m6g.large" => Some(0.077),
        "m6g.xlarge" => Some(0.154),
        // Compute optimized
        "c5.large" => Some(0.085),
        "c5.xlarge" => Some(0.17),
        "c5.2xlarge" => Some(0.34),
        "c6i.large" => Some(0.085),
        "c6i.xlarge" => Some(0.17),
        // Memory optimized
        "r5.large" => Some(0.126),
        "r5.xlarge" => Some(0.252),
        "r6i.large" => Some(0.126),
        _ => None,
    }
}

/// Sum of hourly prices for a list of instance types, with a "missing" count
/// of instances whose type wasn't in the table.
pub(crate) fn estimate_cost(instances: &[Instance]) -> (f64, usize) {
    let mut total = 0.0;
    let mut missing = 0;
    for i in instances {
        match instance_hourly_usd(&i.instance_type) {
            Some(p) => total += p,
            None => missing += 1,
        }
    }
    (total, missing)
}

/// Rollup of operational signals across every env in an application.
/// Pure — driven entirely by the in-memory env list, so the Apps
/// table can refresh as part of the same view-rebuild that touches
/// the Envs table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct AppRollup {
    pub env_count: usize,
    pub red_count: usize,
    pub updating_count: usize,
    pub worker_dlq_alerts: usize,
}

/// Pure: render the `:fleet-cost` overlay body. Aggregates
/// `App.costs` (monthly $/env, populated by `:cost on`) across the
/// fleet, broken down by application, tier, and health. Pure so the
/// formatting can be pinned in unit tests.
///
/// `fetched_at` and `now` drive the "X ago" freshness chip; pass
/// `chrono::Utc::now()` for `now`. Renders a freshness warning when
/// the cache is stale (> 24h) — Cost Explorer data has its own ~24h
/// refresh lag anyway.
pub(crate) fn render_fleet_cost(
    envs: &[crate::aws::Environment],
    costs: &HashMap<String, f64>,
    fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    use std::collections::BTreeMap;
    // Total + by-application + by-tier + by-health aggregates. All
    // BTreeMap so the rendered order is alphabetical (deterministic
    // for tests; readable for operators scanning the overlay).
    let mut total: f64 = 0.0;
    let mut by_app: BTreeMap<String, f64> = BTreeMap::new();
    let mut by_tier: BTreeMap<String, f64> = BTreeMap::new();
    let mut by_health: BTreeMap<String, f64> = BTreeMap::new();
    let mut covered = 0usize;
    let mut missing = 0usize;
    for e in envs {
        match costs.get(&e.name).copied() {
            Some(c) => {
                total += c;
                covered += 1;
                *by_app.entry(e.application.clone()).or_default() += c;
                let tier = if e.tier.is_empty() {
                    "?".to_string()
                } else {
                    e.tier.clone()
                };
                *by_tier.entry(tier).or_default() += c;
                let health = if e.health.is_empty() {
                    "?".to_string()
                } else {
                    e.health.clone()
                };
                *by_health.entry(health).or_default() += c;
            }
            None => {
                missing += 1;
            }
        }
    }
    let mut out = String::new();
    out.push_str(&format!("Total: ${total:.2}/mo  ({covered} env(s) covered"));
    if missing > 0 {
        out.push_str(&format!(", {missing} without cost data"));
    }
    out.push_str(")\n");
    if let Some(t) = fetched_at {
        let age = now
            .signed_duration_since(t)
            .to_std()
            .unwrap_or(std::time::Duration::ZERO);
        out.push_str(&format!("Cached: {} ago", humanize_short_age(age)));
        if age >= std::time::Duration::from_secs(24 * 60 * 60) {
            out.push_str(" (stale — Cost Explorer refreshes ~24h)");
        }
        out.push('\n');
    }
    out.push('\n');
    out.push_str("By application\n");
    out.push_str("──────────────\n");
    let mut by_app_sorted: Vec<(&String, &f64)> = by_app.iter().collect();
    by_app_sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (name, cost) in by_app_sorted {
        out.push_str(&format!("  ${cost:>10.2}/mo  {name}\n"));
    }
    out.push('\n');
    out.push_str("By tier\n");
    out.push_str("───────\n");
    let mut by_tier_sorted: Vec<(&String, &f64)> = by_tier.iter().collect();
    by_tier_sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (name, cost) in by_tier_sorted {
        out.push_str(&format!("  ${cost:>10.2}/mo  {name}\n"));
    }
    out.push('\n');
    out.push_str("By health\n");
    out.push_str("─────────\n");
    let mut by_health_sorted: Vec<(&String, &f64)> = by_health.iter().collect();
    by_health_sorted.sort_by(|a, b| b.1.partial_cmp(a.1).unwrap_or(std::cmp::Ordering::Equal));
    for (name, cost) in by_health_sorted {
        out.push_str(&format!("  ${cost:>10.2}/mo  {name}\n"));
    }
    out.push_str("\nesc / q to close");
    out
}

pub(crate) fn app_rollup(
    envs: &[crate::aws::Environment],
    app_name: &str,
    dlq_depths: &HashMap<String, i64>,
) -> AppRollup {
    let mut out = AppRollup::default();
    for e in envs.iter().filter(|e| e.application == app_name) {
        out.env_count += 1;
        // Red / Severe — operator-visible distress signals.
        if matches!(
            e.health.to_lowercase().as_str(),
            "red" | "severe" | "degraded"
        ) {
            out.red_count += 1;
        }
        if e.status.eq_ignore_ascii_case("Updating")
            || e.status.eq_ignore_ascii_case("Launching")
            || e.status.eq_ignore_ascii_case("Terminating")
        {
            out.updating_count += 1;
        }
        if e.tier.eq_ignore_ascii_case("Worker")
            && dlq_depths.get(&e.name).copied().unwrap_or(0) > 0
        {
            out.worker_dlq_alerts += 1;
        }
    }
    out
}
