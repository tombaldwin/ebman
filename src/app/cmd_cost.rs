//! Cost and promotion-report `:commands`.

use super::*;

impl App {
    /// Toggle the COST column. `state` = None flips the current
    /// value; Some(true)/Some(false) sets explicitly. Persists to
    /// state.toml so the toggle survives restarts. Opting in triggers
    /// a fetch immediately (with stale-cache rendered while it runs);
    /// opting out clears the costs map so the column stops showing
    /// numbers that no longer represent reality.
    pub(crate) fn cmd_cost(&mut self, rest: &[&str]) {
        let next = match rest.first().copied() {
            Some("on") | Some("true") | Some("enable") => true,
            Some("off") | Some("false") | Some("disable") => false,
            Some("status") | None => {
                let pretty = match (self.cost_enabled, self.costs_fetched_at) {
                    (false, _) => "off".to_string(),
                    (true, None) => "on (no data yet)".into(),
                    (true, Some(t)) => {
                        let age = chrono::Utc::now()
                            .signed_duration_since(t)
                            .to_std()
                            .unwrap_or_default();
                        format!(
                            "on (refreshed {} ago, {} env(s) cached)",
                            humanize_short_age(age),
                            self.costs.len()
                        )
                    }
                };
                self.status_message = Some(format!("cost: {pretty}"));
                return;
            }
            Some(other) => {
                self.error_message =
                    Some(format!("usage: :cost on | off | status  (got '{other}')"));
                return;
            }
        };
        if next == self.cost_enabled {
            self.status_message =
                Some(format!("cost: already {}", if next { "on" } else { "off" }));
            return;
        }
        self.cost_enabled = next;
        if next {
            // Load whatever the cache has so the column renders
            // immediately with stale data; spawn a fresh fetch in
            // the background. The CostsFetched handler will refresh
            // and persist when the result lands.
            let account = self
                .context
                .account_id
                .clone()
                .unwrap_or_else(|| "unknown".into());
            let cache = crate::cost_cache::load(&account, &self.context.region);
            let now = chrono::Utc::now();
            let stale = cache.is_stale(now);
            self.costs = cache.costs;
            self.costs_fetched_at = cache.fetched_at;
            // Only complete walks are ever persisted, so anything the
            // cache hands back is complete by construction.
            self.costs_complete = true;
            if stale {
                // Cache stale (>24h) or absent. Fetch in background;
                // operator sees stale numbers (or "—") immediately
                // and the column refreshes when CostsFetched lands.
                self.spawn_cost_fetch();
                self.status_message =
                    Some("cost: on — fetching latest from Cost Explorer (1-3s; cached 24h)".into());
            } else {
                // Fresh cache hit — Cost Explorer data only refreshes
                // ~24h on AWS's side anyway, so an extra fetch buys
                // nothing but rate-limit pressure. Tell the operator
                // what they're seeing.
                let age = now
                    .signed_duration_since(cache.fetched_at.unwrap_or(now))
                    .to_std()
                    .unwrap_or_default();
                self.status_message = Some(format!(
                    "cost: on — cached ({} ago; AWS refreshes ~24h)",
                    humanize_short_age(age)
                ));
            }
        } else {
            self.costs.clear();
            self.costs_fetched_at = None;
            self.status_message = Some("cost: off — column hidden, cache preserved".into());
        }
        self.persist_state();
    }

    /// `:promotions` — overlay showing the in-memory promotion
    /// history captured by `:promote-env` in this session. Lineage
    /// trace for "this version was promoted from staging → prod (at
    /// T)" post-mortems. Empty state is a status toast, not an
    /// overlay (low-noise UX for the common case).
    pub(crate) fn cmd_promotions(&mut self) {
        if self.promotion_history.is_empty() {
            self.status_message = Some(
                "promotions: no promotion history in this session — run `:promote-env SOURCE TARGET` first".into(),
            );
            return;
        }
        let body = render_promotions(&self.promotion_history, chrono::Utc::now());
        self.current_overlay = Some(Overlay::TextDump {
            title: format!("promotions ({})", self.promotion_history.len()),
            body,
        });
    }

    /// `:fleet-cost` — one-screen overlay summarising the current
    /// context's Cost Explorer cache: total $/mo, broken down by
    /// application, tier, and health. Read-only over the existing
    /// `App.costs` cache (populated by `:cost on`). No AWS calls.
    ///
    /// Empty state when `:cost on` hasn't been run yet (or the
    /// cache is empty): toast pointing the operator at the enable
    /// command, no overlay opened.
    pub(crate) fn cmd_fleet_cost(&mut self) {
        if !self.cost_enabled {
            self.error_message =
                Some("cost tracking is off — run `:cost on` to populate the cache first".into());
            return;
        }
        if self.costs.is_empty() {
            self.status_message = Some(
                "fleet-cost: no cost data yet (Cost Explorer fetch may still be in flight; try again in 10s)".into(),
            );
            return;
        }
        let body = render_fleet_cost(
            &self.environments,
            &self.costs,
            self.costs_fetched_at,
            chrono::Utc::now(),
        );
        self.current_overlay = Some(Overlay::TextDump {
            title: format!("fleet cost ({})", self.context.region),
            body,
        });
    }
}
