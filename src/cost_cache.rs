//! On-disk cache for Cost Explorer results.
//!
//! Cost Explorer is slow (1-3s per query) and rate-limited (~1 req/s).
//! Worse, the data only refreshes once per ~24h on AWS's side — there's
//! no value in fetching more often even if the budget allowed it. This
//! module persists the per-env cost map between sessions so an operator
//! who opens ebman with `:cost on` already enabled gets the column
//! rendered on the first refresh tick instead of staring at "—" for a
//! few seconds.
//!
//! Cache lives at `~/.cache/ebman/cost-{account}-{region}.toml`. Format:
//!
//! ```toml
//! fetched_at = "2026-05-21T12:34:56Z"
//! env."prod-api" = 1240.50
//! env."staging-api" = 180.00
//! ```
//!
//! Keyed by (account, region) because the same env name in two AWS
//! accounts is two different surfaces, and Cost Explorer's account
//! scoping is implicit in the caller's credentials. The Cost Explorer
//! API itself is global / `us-east-1`-pinned (see `aws.rs`); the cache
//! still keys by the operator's *active* region because the
//! env-vs-region mapping is what the operator cares about.

use std::collections::HashMap;
use std::path::PathBuf;

use chrono::{DateTime, Utc};

use crate::util::{cache_dir, write_atomic};

/// Cached Cost Explorer result. `fetched_at` lets the caller decide
/// whether to refresh; the map itself is `env_name -> monthly USD`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostCache {
    pub fetched_at: Option<DateTime<Utc>>,
    pub costs: HashMap<String, f64>,
}

/// How long the cache is considered fresh before we trigger a
/// re-fetch. Matches AWS's own ~24h data refresh — pulling sooner
/// gives identical numbers + burns rate-limit budget.
pub const CACHE_TTL_HOURS: i64 = 24;

impl CostCache {
    /// True when the cache is stale (older than [`CACHE_TTL_HOURS`])
    /// or has never been fetched. Callers use this to decide
    /// whether to spawn a fresh fetch.
    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        let Some(t) = self.fetched_at else {
            return true;
        };
        now.signed_duration_since(t).num_hours() >= CACHE_TTL_HOURS
    }
}

/// File path for the cache keyed by (account, region). `account` is
/// the 12-digit AWS account id (falls back to `"unknown"` when not
/// yet resolved via STS). Same key shape as the audit log.
pub fn cache_path(account: &str, region: &str) -> PathBuf {
    let mut dir = cache_dir();
    dir.push(format!("cost-{account}-{region}.toml"));
    dir
}

/// Parse the TOML format described in the module docs. Pure; tested
/// below. Errors silently to a default cache so a malformed file
/// can't block startup.
pub fn parse(text: &str) -> CostCache {
    let mut out = CostCache::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim().trim_matches('"');
        if key == "fetched_at" {
            out.fetched_at = DateTime::parse_from_rfc3339(value)
                .ok()
                .map(|t| t.with_timezone(&Utc));
        } else if let Some(env_key) = key.strip_prefix("env.") {
            let env_name = env_key.trim_matches('"');
            if env_name.is_empty() {
                continue;
            }
            if let Ok(amount) = value.parse::<f64>() {
                out.costs.insert(env_name.to_string(), amount);
            }
        }
    }
    out
}

/// Serialise a cache to TOML. Float precision intentionally limited
/// to 2 decimal places — Cost Explorer reports fractional cents
/// (`1240.503125...`) and the apparent precision is misleading.
pub fn serialize(cache: &CostCache) -> String {
    let mut out = String::new();
    if let Some(t) = cache.fetched_at {
        out.push_str(&format!("fetched_at = \"{}\"\n", t.to_rfc3339()));
    }
    let mut envs: Vec<(&String, &f64)> = cache.costs.iter().collect();
    envs.sort_by(|a, b| a.0.cmp(b.0));
    for (env, amount) in envs {
        out.push_str(&format!("env.\"{env}\" = {amount:.2}\n"));
    }
    out
}

/// Read + parse the cache for the given (account, region). Returns
/// a default `CostCache` if the file doesn't exist or is malformed
/// — both shape up as "stale" via `is_stale`, so the caller will
/// trigger a fresh fetch.
pub fn load(account: &str, region: &str) -> CostCache {
    let path = cache_path(account, region);
    match std::fs::read_to_string(&path) {
        Ok(text) => parse(&text),
        Err(_) => CostCache::default(),
    }
}

/// Atomically persist the cache. Same write-temp-then-rename pattern
/// as `state.rs` to avoid leaving a half-written file behind on a
/// crash mid-write.
pub fn save(account: &str, region: &str, cache: &CostCache) -> std::io::Result<()> {
    let path = cache_path(account, region);
    let text = serialize(cache);
    write_atomic(&path, &text)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(year: i32, month: u32, day: u32) -> DateTime<Utc> {
        chrono::NaiveDate::from_ymd_opt(year, month, day)
            .unwrap()
            .and_hms_opt(12, 0, 0)
            .unwrap()
            .and_utc()
    }

    #[test]
    fn is_stale_with_no_fetched_at() {
        let c = CostCache::default();
        assert!(c.is_stale(Utc::now()), "never-fetched cache must be stale");
    }

    #[test]
    fn is_stale_after_ttl_window() {
        let c = CostCache {
            fetched_at: Some(at(2026, 5, 1)),
            costs: HashMap::new(),
        };
        // 24h later: still inside the TTL window (compared with >=).
        assert!(!c.is_stale(at(2026, 5, 1) + chrono::Duration::hours(23)));
        // 24h boundary triggers a re-fetch.
        assert!(c.is_stale(at(2026, 5, 1) + chrono::Duration::hours(24)));
        // Well past TTL.
        assert!(c.is_stale(at(2026, 5, 5)));
    }

    #[test]
    fn parse_round_trip() {
        let mut costs = HashMap::new();
        costs.insert("prod-api".into(), 1240.50);
        costs.insert("staging-api".into(), 180.0);
        let c = CostCache {
            fetched_at: Some(at(2026, 5, 21)),
            costs,
        };
        let text = serialize(&c);
        let parsed = parse(&text);
        assert_eq!(parsed.fetched_at, c.fetched_at);
        assert!((parsed.costs.get("prod-api").copied().unwrap() - 1240.50).abs() < 0.01);
        assert!((parsed.costs.get("staging-api").copied().unwrap() - 180.0).abs() < 0.01);
    }

    #[test]
    fn parse_ignores_malformed_lines() {
        let text = r#"
fetched_at = "2026-05-21T12:00:00Z"
env."prod-api" = 1240.50
# this is a comment
not a key=value pair just garbage
env."" = 50.00
env."staging" = not-a-number
"#;
        let c = parse(text);
        assert!(c.fetched_at.is_some());
        assert_eq!(c.costs.len(), 1, "only prod-api should parse cleanly");
        assert!(c.costs.contains_key("prod-api"));
    }
}
