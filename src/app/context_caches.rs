//! The fleet state a context switch forgets.
//!
//! A profile, account or region switch (`apply_rebuild`) used to clear
//! each of these by hand, one `.clear()` per field with a comment saying
//! why. The list was complete, but nothing made it so: every new cache
//! was one more line to remember, and the comments recorded the
//! cross-account bugs a missed one had caused. Here a reset is
//! `self.caches = ContextCaches::default()`.
//!
//! What stays out, deliberately: the open Detail view, forms, modals,
//! pickers and the DLQ viewer (their reset also restores the mode), the
//! armed watchdogs, pending actions and undo history (action state with
//! its own semantics), and `costs` (a reset keeps the `:cost on`
//! opt-in).

use super::*;

#[derive(Debug, Default)]
pub(crate) struct ContextCaches {
    /// DLQ depth (`Visible` messages) per Worker-tier env, from a
    /// per-refresh fan-out of `describe_worker_queues`. Drives the
    /// Red-alert count, the row tint and the `⚠N` chip. `absent` = the
    /// worker has no DLQ; `stale` = the last check failed and the depth
    /// is the last known one, so an AccessDenied or a throttle cannot
    /// silently clear an alert.
    pub(crate) worker_dlq: PolledByEnv<i64>,
    /// The CloudWatch alarms in ALARM state per env, from a per-region
    /// `DescribeAlarms` each refresh. Tints the row and fills ALARM:
    /// EB health can be Green while an alarm fires. `stale` = the
    /// region's last check failed; the table marks the count unknown
    /// rather than letting a failure read as "no alarms".
    pub(crate) firing_alarms: PolledByEnv<Vec<String>>,
    /// Bumped once per fleet refresh and stamped on that refresh's
    /// polls, so a slow answer from an older round cannot overwrite a
    /// newer one (`PolledByEnv::accept_round`).
    pub(crate) poll_round: u64,
    /// Per-env `(healthy, total)` instance counts, populated by
    /// `spawn_env_instance_counts` after each refresh tick. Drives the
    /// `INST` column on the main env table. No value = not checked, or
    /// an env with nothing to count (terminal lifecycle states); both
    /// render as `—`. `EnvInstanceCounts { 0, 0 }` is a real value
    /// ("env reports no instances") and renders as `0/0`. A failed
    /// check keeps the last counts, marked stale.
    pub(crate) env_instance_counts: PolledByEnv<crate::aws::EnvInstanceCounts>,
    /// The last `HISTORY_CAP` health readings per env, for TREND.
    pub(crate) history: HashMap<EnvKey, VecDeque<String>>,
    pub(crate) newly_red: HashSet<EnvKey>,
    /// Env names that appeared for the first time on the most recent
    /// refresh (weren't in `prev_health` last cycle). Used by the env
    /// table to render a transient `+` marker on the NAME cell so a new
    /// env doesn't scroll past unnoticed. Cleared on context switch +
    /// rotated each refresh.
    pub(crate) newly_added: HashSet<EnvKey>,
    pub(crate) prev_health: HashMap<EnvKey, String>,
    pub(crate) prev_status: HashMap<EnvKey, String>,
    /// Delta in counts vs. the previous refresh, e.g. {"Red" → +1, "Yellow" → -1}.
    pub(crate) health_delta: Vec<(String, i32)>,
    pub(crate) status_delta: Vec<(String, i32)>,
    /// Lazy cache for `spawn_confirm_lint`'s parallel tag fetch.
    /// Populated opportunistically by every lint call site that fires
    /// the inline `list_tags(env.arn)` fetch. TTL is `LINT_INPUT_CACHE_TTL`
    /// (60s). Modal-open latency drops to `max(t_opts)` when cache
    /// is fresh — saves ~one round-trip per repeated modal-open
    /// against the same env. Cleared on context switch alongside
    /// the other env-keyed state. 0.21 addition.
    pub(crate) env_tag_cache: std::collections::HashMap<EnvKey, (Vec<String>, std::time::Instant)>,
    /// Same shape as `env_tag_cache` but for the
    /// `fetch_env_instance_counts` healthy-count input that
    /// EBL012 reads. Independent TTL — same constant.
    pub(crate) env_health_cache: std::collections::HashMap<EnvKey, (i64, std::time::Instant)>,
    /// Last-known region per environment name.
    ///
    /// `region_for_name` searches the live table first, but a write can
    /// outlive its row: the confirm modal carries only a target NAME,
    /// and there is an undo window between the operator confirming and
    /// `tick_pending_dispatch` firing. A 15-second refresh landing in
    /// that window — a terminated env, or a region whose fetch failed
    /// under a fan-out — used to drop the answer, and the dispatch fell
    /// back to the home region. Silently, which is the whole class this
    /// release is named after.
    ///
    /// An environment cannot move between regions, so a remembered
    /// answer can only go stale by the name being reused in a different
    /// one. The live table always wins, and a context switch clears it.
    /// Every region a name was seen in: under `:region all` a name can
    /// be in two, and remembering only the last one let a refresh that
    /// lost one region's listing make the name look unambiguous.
    pub(crate) env_regions: std::collections::HashMap<String, std::collections::BTreeSet<String>>,
    pub(crate) applications: Vec<Application>,
    /// `family_key → newest available version` from `ListAvailableSolutionStacks`,
    /// built by `spawn_solution_stacks`. Drives the envs-table stale-platform
    /// tint. Empty until the first fetch lands; cleared on context switch so a
    /// new account/region rebuilds it.
    pub(crate) latest_stacks: std::collections::HashMap<String, String>,
    /// Why the last `latest_stacks` fetch failed, if it did. An empty
    /// `latest_stacks` alone cannot say whether the list is still
    /// loading or was denied, and lint's EBL008 gap should name which.
    pub(crate) latest_stacks_error: Option<String>,
    /// The App tab's discovery, kept for the session.
    pub(crate) app_tab_discovery: app_tab_form::AppTabDiscovery,
    pub(crate) prev_alerts: usize,
}
