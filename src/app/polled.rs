//! Per-env values the fleet view polls on every refresh: DLQ depth,
//! instance counts, firing alarms.
//!
//! Each poll used to keep its own maps and its own reconcile, and they
//! drifted: the instance-count handler cleared and rebuilt, so one
//! throttled env blanked its INST cell (the shape the DLQ handler had
//! already been fixed away from); DLQ depths were never pruned when an
//! env left the fleet; and every map was keyed by the env's NAME, so
//! under `:region all` two same-named envs in two regions shared one
//! entry and whichever region answered last decided both rows. One
//! type, one reconcile, one prune, keyed by [`EnvKey`].

use std::collections::{BTreeMap, BTreeSet};

/// An environment as the fleet view knows it: its region and name. EB
/// names are unique only within a region, and the multi-region fan-out
/// puts two regions' envs in one table.
///
/// No account: every per-env cache is emptied on a profile or account
/// switch (`apply_rebuild`), so one account's envs never share a map
/// with another's.
// No `Hash`: its maps are B-trees, keyed by the derived `Ord`. Hashing
// it, derived or by hand, crashes the nightly the CI candor lint is
// pinned to ("internal compiler error: missing value for assoc item in
// impl", x86_64 Linux only, 2026-10-21); the same nightly on macOS and
// stable do not.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct EnvKey {
    pub region: String,
    pub name: String,
}

impl EnvKey {
    /// `REGION/NAME`, as `state.toml` keeps a per-env entry. EB env names
    /// cannot contain `/`, so the split is unambiguous.
    pub(crate) fn to_persisted(&self) -> String {
        format!("{}/{}", self.region, self.name)
    }

    /// The inverse of [`to_persisted`](Self::to_persisted); `None` for a
    /// key saved without a region.
    pub(crate) fn parse_persisted(raw: &str) -> Option<Self> {
        let (region, name) = raw.split_once('/')?;
        (!region.is_empty() && !name.is_empty()).then(|| Self::new(region, name))
    }

    pub(crate) fn new(region: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            region: region.into(),
            name: name.into(),
        }
    }

    /// `env`'s key: its own region, or `home_region` for a row that
    /// carries none (a single-region listing). `App::env_key` is this
    /// with the App's region; the table's row builder calls it directly
    /// because it may borrow only fields of the App, not all of it.
    pub(crate) fn of(env: &crate::aws::Environment, home_region: &str) -> Self {
        Self::new(
            env.region
                .as_deref()
                .filter(|r| !r.is_empty())
                .unwrap_or(home_region),
            env.name.clone(),
        )
    }
}

/// One polled value per env, with what the last check said.
///
/// - a value: the last check found one (or a failed check kept it);
/// - `absent`: the last check found there is nothing to have (a worker
///   with no DLQ configured), which is not "not checked yet";
/// - `stale`: the last check FAILED. Any value is the last known one,
///   kept so an AccessDenied or a throttle cannot silently clear an
///   alert, and the UI says so.
#[derive(Debug)]
pub(crate) struct PolledByEnv<T> {
    values: BTreeMap<EnvKey, T>,
    stale: BTreeSet<EnvKey>,
    absent: BTreeSet<EnvKey>,
    /// The newest poll round applied. An older round's answer that
    /// arrives late is dropped, rather than overwriting a newer one.
    applied_round: u64,
}

impl<T> Default for PolledByEnv<T> {
    fn default() -> Self {
        Self {
            values: BTreeMap::new(),
            stale: BTreeSet::new(),
            absent: BTreeSet::new(),
            applied_round: 0,
        }
    }
}

impl<T> PolledByEnv<T> {
    pub(crate) fn get(&self, key: &EnvKey) -> Option<&T> {
        self.values.get(key)
    }

    pub(crate) fn is_stale(&self, key: &EnvKey) -> bool {
        self.stale.contains(key)
    }

    pub(crate) fn is_absent(&self, key: &EnvKey) -> bool {
        self.absent.contains(key)
    }

    /// Whether an answer from poll `round` may be applied: false when a
    /// newer round's answer already was. Applying marks `round` seen.
    pub(crate) fn accept_round(&mut self, round: u64) -> bool {
        if round < self.applied_round {
            return false;
        }
        self.applied_round = round;
        true
    }

    /// Apply one env's outcome: `Ok(Some)` a value, `Ok(None)` nothing
    /// to have, `Err` a failed check that keeps the last known value.
    /// A failure after `Ok(None)` leaves the key both `absent` and
    /// `stale`, on purpose: the last good check found nothing, and that
    /// is now unverified. Lint reads both (`tui_lint::worker_dlq`).
    pub(crate) fn reconcile<E>(&mut self, key: EnvKey, outcome: Result<Option<T>, E>) {
        match outcome {
            Ok(Some(value)) => {
                self.stale.remove(&key);
                self.absent.remove(&key);
                self.values.insert(key, value);
            }
            Ok(None) => {
                self.values.remove(&key);
                self.stale.remove(&key);
                self.absent.insert(key);
            }
            Err(_) => {
                self.stale.insert(key);
            }
        }
    }

    /// Record a value as a successful check would: demo fixtures and
    /// tests.
    pub(crate) fn set(&mut self, key: EnvKey, value: T) {
        self.reconcile::<()>(key, Ok(Some(value)));
    }

    /// Forget every env not in `fleet`. An env that left the fleet
    /// takes its values with it; one recreated later under the same
    /// name starts clean.
    pub(crate) fn prune_to(&mut self, fleet: &BTreeSet<EnvKey>) {
        self.values.retain(|k, _| fleet.contains(k));
        self.stale.retain(|k| fleet.contains(k));
        self.absent.retain(|k| fleet.contains(k));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(region: &str, name: &str) -> EnvKey {
        EnvKey::new(region, name)
    }

    #[test]
    fn a_failed_check_keeps_the_last_value_and_says_so() {
        let mut p = PolledByEnv::default();
        let k = key("us-east-1", "wk");
        p.reconcile::<()>(k.clone(), Ok(Some(7)));
        p.reconcile(k.clone(), Err("AccessDenied"));
        assert_eq!(p.get(&k), Some(&7), "a failure must not read as none");
        assert!(p.is_stale(&k));
        p.reconcile::<()>(k.clone(), Ok(Some(3)));
        assert_eq!(p.get(&k), Some(&3));
        assert!(!p.is_stale(&k), "a good check clears the mark");
    }

    #[test]
    fn nothing_to_have_is_absent_not_unchecked() {
        let mut p = PolledByEnv::<i64>::default();
        let k = key("us-east-1", "wk");
        assert!(!p.is_absent(&k), "never checked");
        p.set(k.clone(), 4);
        p.reconcile::<()>(k.clone(), Ok(None));
        assert_eq!(p.get(&k), None);
        assert!(p.is_absent(&k));
        p.set(k.clone(), 1);
        assert!(!p.is_absent(&k));
    }

    #[test]
    fn same_named_envs_in_two_regions_are_two_entries() {
        let mut p = PolledByEnv::default();
        p.set(key("us-east-1", "api"), 1);
        p.set(key("eu-west-2", "api"), 2);
        assert_eq!(p.get(&key("us-east-1", "api")), Some(&1));
        assert_eq!(p.get(&key("eu-west-2", "api")), Some(&2));
    }

    #[test]
    fn pruning_forgets_envs_that_left_the_fleet() {
        let mut p = PolledByEnv::default();
        let (kept, gone) = (key("r", "kept"), key("r", "gone"));
        p.set(kept.clone(), 1);
        p.set(gone.clone(), 2);
        p.reconcile(gone.clone(), Err(()));
        p.reconcile::<()>(key("r", "gone-absent"), Ok(None));
        p.prune_to(&BTreeSet::from([kept.clone()]));
        assert_eq!(p.get(&kept), Some(&1));
        assert_eq!(p.get(&gone), None);
        assert!(!p.is_stale(&gone));
        assert!(!p.is_absent(&key("r", "gone-absent")));
    }

    #[test]
    fn a_late_answer_from_an_older_round_is_dropped() {
        let mut p = PolledByEnv::<i64>::default();
        assert!(p.accept_round(2));
        assert!(!p.accept_round(1), "round 1 arrived after round 2");
        assert!(
            p.accept_round(2),
            "the same round's other answers still apply"
        );
        assert!(p.accept_round(3));
    }
}
