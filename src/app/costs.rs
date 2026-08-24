//! Per-env cost data and whether it can be trusted, in one type.
//!
//! Four `App` fields used to carry this: `costs`, `costs_complete`,
//! `costs_fetched_at`, and `cost_enabled`. Three of them have to move
//! together and the compiler had no way to say so, which had already
//! produced a bug — recorded in the old `costs_complete` doc comment:
//!
//! > Without this, "do we already have costs?" was the only test
//! > available, and it made a partial map permanent: the first truncated
//! > walk populated `costs`, and every later truncated walk then saw a
//! > non-empty map and kept it — so the partial data from the first
//! > failure survived the whole session while each retry paid for twenty
//! > metered Cost Explorer pages and discarded them.
//!
//! The fix at the time was a fourth field. This is the same fix the
//! `ViewState` refactor applied to the view cache: make the fields
//! private, and expose the transitions rather than the state. There are
//! exactly three ways cost data changes, and each is a method here —
//! so "populate without saying whether the walk finished" is no longer
//! something you can write.
//!
//! The one that matters is [`Costs::set_partial`]. It cannot stamp
//! `fetched_at`, because a timestamp is what suppresses the retry, and
//! partial data must never suppress a retry. That was a rule in a
//! comment; it is now a rule in the type.

use std::collections::HashMap;

use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Default)]
pub(crate) struct Costs {
    by_env: HashMap<String, f64>,
    /// Did the walk that produced `by_env` finish? An empty map is
    /// trivially complete; a truncated walk is not.
    complete: bool,
    /// When a COMPLETE walk last landed. `None` means "retry is due" —
    /// which is why a partial walk must not set it.
    fetched_at: Option<DateTime<Utc>>,
    /// Cost Explorer is opt-in (`:cost on`) because it is metered.
    /// Persisted to state.toml.
    enabled: bool,
}

impl Costs {
    /// No data, and that is a settled answer rather than a pending one.
    /// Used on `:cost off` and on context switch.
    pub(crate) fn clear(&mut self) {
        self.by_env.clear();
        self.complete = true;
        self.fetched_at = None;
    }

    /// A walk that finished. Stamps `fetched_at`, so the result is
    /// cacheable and suppresses a retry until it ages out.
    pub(crate) fn set_complete<I>(&mut self, rows: I, now: DateTime<Utc>)
    where
        I: IntoIterator<Item = (String, f64)>,
    {
        self.by_env = rows.into_iter().collect();
        self.complete = true;
        self.fetched_at = Some(now);
    }

    /// A walk that hit the page cap. Deliberately takes no timestamp:
    /// partial data must not suppress the retry that would replace it,
    /// and must not be cached. Enforced by the signature rather than by
    /// remembering.
    pub(crate) fn set_partial<I>(&mut self, rows: I)
    where
        I: IntoIterator<Item = (String, f64)>,
    {
        self.by_env = rows.into_iter().collect();
        self.complete = false;
        self.fetched_at = None;
    }

    /// Restore a complete result from the on-disk cache.
    pub(crate) fn restore_cached(
        &mut self,
        by_env: HashMap<String, f64>,
        fetched_at: Option<DateTime<Utc>>,
    ) {
        self.by_env = by_env;
        self.complete = true;
        self.fetched_at = fetched_at;
    }

    /// Should an incoming PARTIAL result be kept, or is what we hold
    /// worth more?
    ///
    /// A complete map beats fresher partial data — the whole reason
    /// `complete` exists. Asking it as one question keeps the rule in
    /// one place instead of at each call site.
    pub(crate) fn partial_would_lose_better_data(&self) -> bool {
        !self.by_env.is_empty() && self.complete
    }

    pub(crate) fn get(&self, env: &str) -> Option<f64> {
        self.by_env.get(env).copied()
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.by_env.is_empty()
    }

    pub(crate) fn len(&self) -> usize {
        self.by_env.len()
    }

    pub(crate) fn is_complete(&self) -> bool {
        self.complete
    }

    pub(crate) fn fetched_at(&self) -> Option<DateTime<Utc>> {
        self.fetched_at
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn set_enabled(&mut self, on: bool) {
        self.enabled = on;
    }

    /// The map itself, for the cache writer and the render path.
    pub(crate) fn by_env(&self) -> &HashMap<String, f64> {
        &self.by_env
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<(String, f64)> {
        vec![("api".to_string(), 12.5), ("worker".to_string(), 3.0)]
    }

    /// The bug the `complete` flag was added for, now expressed as a
    /// property of the type: a partial result must not look settled.
    #[test]
    fn a_partial_walk_never_stamps_a_fetch_time() {
        let mut c = Costs::default();
        c.set_partial(rows());
        assert!(!c.is_complete());
        assert_eq!(
            c.fetched_at(),
            None,
            "a timestamp suppresses the retry, and partial data must not \
             suppress the retry that would replace it"
        );
        assert_eq!(c.get("api"), Some(12.5), "but it is still shown");
    }

    #[test]
    fn a_complete_walk_stamps_and_is_trusted() {
        let mut c = Costs::default();
        let now = Utc::now();
        c.set_complete(rows(), now);
        assert!(c.is_complete());
        assert_eq!(c.fetched_at(), Some(now));
        assert_eq!(c.len(), 2);
    }

    /// The decision the message handler used to spell out inline.
    #[test]
    fn a_complete_map_outranks_incoming_partial_data() {
        let mut c = Costs::default();
        c.set_complete(rows(), Utc::now());
        assert!(
            c.partial_would_lose_better_data(),
            "holding complete data, so an incoming partial result must not \
             replace it"
        );

        c.set_partial(rows());
        assert!(
            !c.partial_would_lose_better_data(),
            "holding partial data, so partial-but-fresher is an improvement"
        );

        c.clear();
        assert!(
            !c.partial_would_lose_better_data(),
            "holding nothing, so anything beats blank"
        );
    }

    /// `clear` is a settled empty, not a pending one — otherwise `:cost
    /// off` would read as "a walk that hasn't finished".
    #[test]
    fn clearing_is_a_settled_empty() {
        let mut c = Costs::default();
        c.set_partial(rows());
        c.clear();
        assert!(c.is_empty());
        assert!(c.is_complete(), "an empty map is trivially complete");
        assert_eq!(c.fetched_at(), None);
    }

    /// `enabled` rides along because it is the same concern, but it is
    /// independent of the data — toggling it must not discard costs.
    #[test]
    fn toggling_enabled_does_not_touch_the_data() {
        let mut c = Costs::default();
        c.set_complete(rows(), Utc::now());
        c.set_enabled(true);
        assert_eq!(c.len(), 2);
        c.set_enabled(false);
        assert_eq!(
            c.len(),
            2,
            "`:cost off` hides the column; the ticker clears"
        );
    }

    /// The original bug, expressed as something that must not compile.
    ///
    /// `costs` and `costs_complete` were separate `pub(crate)` fields, so
    /// "populate the map and forget to say the walk was truncated" was a
    /// thing you could write — and it shipped. The fields are private
    /// now and `set_partial` takes no timestamp, so the bug is not a
    /// discipline problem any more.
    #[test]
    fn the_original_bug_is_unrepresentable() {
        let mut c = Costs::default();
        c.set_partial([("api".to_string(), 1.0)]);
        // There is no API that leaves data in place while claiming
        // completeness, and none that stamps a time on a partial walk.
        assert!(!c.is_complete() && c.fetched_at().is_none());
        // The only route to `complete` also supplies the stamp.
        let now = chrono::Utc::now();
        c.set_complete([("api".to_string(), 1.0)], now);
        assert!(c.is_complete() && c.fetched_at() == Some(now));
    }
}
