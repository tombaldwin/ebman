use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use color_eyre::eyre::{Result, WrapErr};
use crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use futures::StreamExt;
use ratatui::{
    layout::Rect,
    widgets::{ListState, TableState},
};
use tokio::sync::mpsc;

use tui_common::TextInput;

use crate::{
    aws::{
        AppVersion, Application, AwsClient, AwsContext, CwAlarm, Environment, Event as EbEvent,
        Identity, Instance, MetricSeries, QueueMessage, WorkerQueues,
    },
    config::Config,
    profiles,
    state::{self, PersistedState},
    theme::{IconStyle, Theme},
    ui, Tui,
};

// Re-export action-cluster types so existing consumers (ui.rs, tests,
// the `App` impl below) keep their `crate::app::Action` etc. paths
// working after the move into `crate::mode_action`.
pub use crate::mode_action::{
    Action, ActionFlow, ConfirmKind, ConfirmModal, DryRunInfo, ParameterisedAction, ACTIONS,
};
pub use crate::mode_detail::{
    config_editable_items, health_items, ConfigEdit, ConfigEditMode, ConfigItem, ConfigItemKind,
    DetailState, DetailTab, EventLevel, EventWindow, HealthItem, LogTail, LogTailStage,
};

// Sub-modules: `execute_command` arms split by category. The
// dispatch site below is now pure one-liner routing — every arm
// body lives in one of these modules. Categories: lifecycle
// actions (deploy/upgrade/clone/scale/...), alarm CRUD,
// config-template CRUD, navigation (region/profile/sort/group/...),
// option-settings setters, multi-account overlays
// (accounts/org-health/find-env), per-env settings
// (tag/env/capacity/...), view persistence (views/filters),
// bulk-write commands (batch-action/batch-deploy/...), and the
// remaining misc cluster (custom-platforms/versions/metric/...).
mod cmd_action;
mod cmd_alarms;
mod cmd_config_template;
mod cmd_misc;
mod cmd_nav;
mod cmd_option;
mod cmd_overlay;
mod cmd_settings;
mod cmd_view;
mod cmd_write;
mod mode_dlq_handlers;
mod mode_keys;
mod msg;
mod tail;
pub(crate) use tail::tail_window_start;
pub use tail::TailView;
mod spawn_batch;
mod spawn_detail;
mod spawn_dlq;
mod spawn_rollout;
mod spawn_why_red;
pub use crate::mode_dlq::{DlqState, QueueView};

/// Names of all built-in `:commands`. Used to detect collisions when loading
/// user plugins from `commands.toml` — plugins that shadow a built-in are
/// dropped with a warning rather than silently masking it.
///
/// Derived from [`crate::commands::COMMANDS`] so adding a command only
/// requires one edit (`commands.rs`). The list is built lazily on first
/// access; the registry is a `const` slice so the work is O(N) with N≈90.
pub fn builtin_commands() -> Vec<&'static str> {
    crate::commands::all_names()
}

mod types;
pub use types::*;

// Topical slices of what used to be one file. Each holds pure logic —
// no `App` receiver, no I/O — and is glob-re-exported so every existing
// `crate::app::foo` path keeps resolving.
mod config_diff;
mod cost;
mod deploy_math;
mod env_edit;
mod render;
mod saved_views;
mod text;
pub use config_diff::*;
pub use cost::*;
pub use deploy_math::*;
pub use env_edit::*;
pub use render::*;
pub use saved_views::*;
pub use text::*;

pub struct App {
    pub context: AwsContext,
    pub scope: Scope,
    pub applications: Vec<Application>,
    pub app_table_state: TableState,
    pub environments: Vec<Environment>,
    pub table_state: TableState,
    pub table_area: Rect,
    pub mode: Mode,
    pub filter: TextInput,
    pub load_state: LoadState,
    pub loading_since: Option<Instant>,
    pub refresh_interval: Duration,
    /// Once the loading indicator has been visible (i.e. `loading_since`
    /// exceeded its display-threshold), keep showing it until this instant
    /// even after the load actually finishes. Smooths over the case where
    /// an AWS round-trip is *just* slow enough to trigger the indicator
    /// and then completes ~100 ms later — without this, the status flashes
    /// yellow → green for a single frame which reads as a flicker. Cleared
    /// by the render path once `Instant::now() > t`.
    pub loading_visible_until: Option<Instant>,
    pub last_refresh: Option<chrono::DateTime<chrono::Utc>>,
    pub status_message: Option<String>,
    pub error_message: Option<String>,
    pub picker: Option<Picker>,
    pub override_profile: Option<String>,
    pub override_region: Option<String>,
    pub history: HashMap<String, VecDeque<String>>,
    pub redact: bool,
    pub grouped: bool,
    pub sort_key: SortKey,
    pub sort_desc: bool,
    pub command_input: TextInput,
    pub completion: CompletionState,
    pub quickjump_input: TextInput,
    pub extra_regions: Vec<String>,
    pub event_panel: EventPanel,
    /// Env names the user has marked for batch action via `space`. Cleared on
    /// Esc, on context switch, and after a successful batch dispatch.
    pub multi_selected: BTreeSet<String>,
    /// Apps-scope multi-selection (parallel to `multi_selected`).
    /// `space` in Apps scope toggles an app in/out. Doesn't persist
    /// across sessions — selection is operator-intent for a single
    /// task. Apps-scope batch ops (future expansion) will fan across
    /// every env in every selected app.
    pub apps_selected: BTreeSet<String>,
    /// Currently-focused panel. Drives j/k routing and footer hints.
    pub focus: Focus,
    /// Regions to fan refreshes across. Empty = single-region mode (only the
    /// AwsClient's region). Populated by `:region all`.
    pub multi_regions: Vec<String>,
    pub detail: Option<DetailState>,
    pub action_flow: Option<ActionFlow>,
    pub dlq: Option<DlqState>,
    pub theme: Arc<Theme>,
    pub view_mode: ViewMode,
    pub help: HelpState,
    pub hover_row: Option<usize>,
    pub alerts: usize, // count of envs currently in Red, recomputed each refresh
    /// Cached DLQ depth (`Visible` messages) for each Worker-tier env,
    /// keyed by env name. Populated by a per-refresh fan-out of
    /// `describe_worker_queues`. Used by the Red-alert calc + the table
    /// render's `⚠ DLQ:N` chip on Worker rows. Missing entry = "not
    /// checked yet" (don't fire an alert on cold state).
    pub worker_dlq_depths: std::collections::HashMap<String, i64>,
    /// Envs whose last worker-queue check FAILED — their entry in
    /// `worker_dlq_depths` is the last-known depth, kept so an
    /// AccessDenied/throttle can't silently clear an alert. The UI
    /// appends a staleness marker so the operator knows the number
    /// may be old. Cleared per-env on the next successful check and
    /// wholesale on context switch.
    pub worker_dlq_stale: std::collections::HashSet<String>,
    /// Monotonic counter of context-switch spawns (`:region`,
    /// `:profile`, `:account`). Stamped into `AppMsg::Rebuild` so a
    /// slow older switch losing the race to a newer one is dropped in
    /// `apply_rebuild` instead of overwriting the operator's last
    /// choice. Distinct from `generation`, which bumps on APPLY.
    pub(crate) rebuild_epoch: u64,
    /// Lazy cache for `spawn_confirm_lint`'s parallel tag fetch.
    /// Populated opportunistically by every lint call site that fires
    /// the inline `list_tags(env.arn)` fetch. TTL is `LINT_INPUT_CACHE_TTL`
    /// (60s). Modal-open latency drops to `max(t_opts)` when cache
    /// is fresh — saves ~one round-trip per repeated modal-open
    /// against the same env. Cleared on context switch alongside
    /// the other env-keyed state. 0.21 addition.
    pub(crate) env_tag_cache: std::collections::HashMap<String, (Vec<String>, std::time::Instant)>,
    /// Same shape as `env_tag_cache` but for the
    /// `fetch_env_instance_counts` healthy-count input that
    /// EBL012 reads. Independent TTL — same constant.
    pub(crate) env_health_cache: std::collections::HashMap<String, (i64, std::time::Instant)>,
    /// Pre-deploy snapshots keyed by env name. Captured at deploy
    /// dispatch time so `:rollback-deploy ENV` (and the watchdog
    /// armed by `:deploy --auto-rollback Nm`) can redeploy whatever
    /// version was running just before. In-memory only — lost on
    /// app restart; the existing `:rollback` falls back to scanning
    /// the env's event history. See `DeploySnapshot`.
    pub(crate) deploy_snapshots: std::collections::HashMap<String, DeploySnapshot>,
    /// Currently-armed auto-rollback watchdogs keyed by env name.
    /// Populated by `:deploy --auto-rollback Nm`, drained on either
    /// (a) the env reaching Green on a refresh tick (early disarm)
    /// or (b) the deadline firing `AutoRollbackCheck`. Used both
    /// for `apply_refresh`'s early-disarm check and for surfacing
    /// "auto-rollback armed for X — Ys remaining" in the UI. The
    /// tokio task that drives the deadline is fire-and-forget;
    /// the in-flight visibility lives here.
    pub(crate) armed_watchdogs: std::collections::HashMap<String, ArmedWatchdog>,
    /// In-flight `--wait-for-green` trackers keyed by env name. Populated
    /// by `:deploy --wait-for-green Nm`; drained on either (a) the env
    /// reaching Green on a refresh tick (success outcome) or (b) the
    /// deadline elapsing without Green (timeout outcome). Either way the
    /// outcome is a pinned status — no follow-on action like
    /// `armed_watchdogs`. Both maps can be populated for the same env
    /// when the operator passes both flags.
    pub(crate) watching_deploys: std::collections::HashMap<String, WatchingDeploy>,
    /// Session-scoped freeze set by `:freeze-deploys`. `None` is
    /// the common case (no freeze active); `Some(...)` makes
    /// every destructive op refuse with the freeze's reason.
    pub(crate) deploy_freeze: Option<DeployFreeze>,
    /// Session-scoped incident mode set by `:incident START`. Rides
    /// on top of `deploy_freeze` (START sets one, END clears it);
    /// carried separately so the header banner + END summary know
    /// the headline and start time.
    pub(crate) incident: Option<Incident>,
    /// Parsed terraform.tfstate from a walk-up of cwd at App
    /// construction time, refreshed on `apply_rebuild` (context
    /// switch) and on `:drift refresh`. `None` when no tfstate
    /// was discovered — the badge / drift overlay surfaces are
    /// no-ops in that case. The full `TfState` is held (rather
    /// than just a derived set) so `:drift ENV` can pull the
    /// declared option_settings + version_label for the report.
    pub(crate) tf_state: Option<crate::terraform::TfState>,
    /// Cached `HashSet` of tf-managed env names — derived from
    /// `tf_state` and kept in sync with it. Used by the env-table
    /// render path for the `ⓣ` badge: O(1) lookup per row,
    /// which matters when an operator has 50+ envs and the
    /// renderer fires every frame.
    pub(crate) tf_managed_envs: std::collections::HashSet<String>,
    /// Ring buffer of reversible option-settings writes captured
    /// just before each `spawn_option_settings_update` dispatch.
    /// `:undo` pops the most recent (back of the deque) and
    /// dispatches its reverse-action. Capped at `UNDO_HISTORY_CAP`;
    /// older entries fall off the front when the cap is hit.
    /// Session-scoped — not persisted. Cross-context state is
    /// cleared on `apply_rebuild` alongside the other env-keyed
    /// state.
    pub(crate) undo_history: std::collections::VecDeque<UndoEntry>,
    /// Promotion-event log: SOURCE → TARGET pairs captured when
    /// `:promote-env` opens a deploy confirm modal. The `:promotions`
    /// overlay (0.20+) surfaces these as a lineage trace. In-memory
    /// only — cleared on context switch. State.toml persistence is a
    /// 0.21+ follow-up.
    pub(crate) promotion_history: Vec<PromotionRecord>,
    /// `--demo` mode flag. Suppresses the periodic refresh (`spawn_refresh`
    /// becomes a no-op) and the update-check (`spawn_update_check` likewise)
    /// so hand-crafted fixture data from `demo_fixture::install` stays put.
    /// All other paths run as normal — keybinds work, overlays render — so
    /// VHS / asciinema captures show the genuine UI surface. Drill-into-
    /// other-tabs (`:why`, Detail/Events, …) still fire against the stub
    /// AwsClient and may return empty or errored data; closing that gap
    /// is a separate piece of work (spawn-site gating).
    pub demo_mode: bool,
    /// Per-env `(healthy, total)` instance counts, populated by
    /// `spawn_env_instance_counts` after each refresh tick. Drives the
    /// `INST` column on the main env table. Missing entry = "not
    /// checked yet"; rendered as `—`. `EnvInstanceCounts { 0, 0 }` is
    /// a real value ("env reports no instances") and renders as `0/0`.
    pub env_instance_counts: std::collections::HashMap<String, crate::aws::EnvInstanceCounts>,
    /// Cost Explorer integration is opt-in via `:cost on`. Toggling
    /// flips this + triggers a fetch (or a stale-cache load); the
    /// envs-table COST column renders only while this is true.
    /// Persisted to state.toml under `cost_enabled`.
    pub cost_enabled: bool,
    /// Per-env monthly USD spend, populated by `spawn_cost_fetch`
    /// after a `:cost on` opt-in. Empty when costs haven't been
    /// fetched yet or the cache file is missing. Cleared when the
    /// operator toggles `:cost off` so the column stops rendering
    /// stale numbers.
    pub costs: std::collections::HashMap<String, f64>,
    pub costs_fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `family_key → newest available version` from `ListAvailableSolutionStacks`,
    /// built by `spawn_solution_stacks`. Drives the envs-table stale-platform
    /// tint. Empty until the first fetch lands; cleared on context switch so a
    /// new account/region rebuilds it.
    pub latest_stacks: std::collections::HashMap<String, String>,
    pub frozen: bool, // when true, auto-refresh ticker is no-op
    /// `true` when ebman launched without a `state.toml` on disk —
    /// i.e. first-ever run on this machine. Renderer surfaces a
    /// one-line "press ? for help, : for commands, Ctrl-K for
    /// fuzzy search" hint at the very bottom of the screen.
    /// Cleared on the operator's first input event so it never
    /// blocks; the persisted state.toml that every refresh writes
    /// also means subsequent launches won't re-trigger it.
    pub first_run_hint: bool,
    /// The currently visible overlay popup, if any. See [`Overlay`].
    pub current_overlay: Option<Overlay>,
    pub message_log: VecDeque<(chrono::DateTime<chrono::Utc>, MsgKind, String)>,
    pub toasts: VecDeque<Toast>,
    pub palette_input: TextInput,
    pub palette_items: Vec<PaletteItem>,
    pub palette_filtered: Vec<usize>,
    pub palette_state: ListState,
    pub read_only: bool,
    pub pinned: BTreeSet<String>,
    /// Apps-scope pinned set — apps stay at the top of the Apps table
    /// regardless of sort. Persisted to state.toml's `pinned_apps`
    /// field. Parallel to `pinned` (which covers envs); the two
    /// surfaces have different cursor / sort behaviour so keeping
    /// them as separate sets is cleaner than a tagged union.
    pub pinned_apps: BTreeSet<String>,
    pub aliases: BTreeMap<String, String>,
    pub saved_views: BTreeMap<String, String>,
    pub hidden_cols: BTreeSet<String>,
    /// User-defined extra metric charts for the Metrics tab. Keyed by the
    /// operator-chosen display label so re-adding the same label updates
    /// in place. Persisted in `state.toml` under `metric.LABEL`.
    pub custom_metrics: BTreeMap<String, crate::state::CustomMetricSpec>,
    pub log_reload: Option<crate::LogReloadHandle>,
    pub log_directive: String,
    pub plugins: BTreeMap<String, crate::plugins::Plugin>,
    /// Snapshot of `(status_message, error_message)` captured when the current
    /// refresh was spawned. apply_refresh clears messages only if they still
    /// match this snapshot, so user-initiated status set between kickoff and
    /// apply (e.g. pressing `s` to sort during the round-trip) is preserved.
    pub status_snapshot_at_refresh: Option<(Option<String>, Option<String>)>,
    /// `true` when `status_message` was set by a user-facing command (e.g.
    /// `:pending`, `:metric add`) rather than a background spawn helper.
    /// Refresh-time auto-clear only touches non-pinned messages — without
    /// this, every 15s tick wipes out informational results the user just
    /// invoked.
    pub status_message_pinned: bool,
    /// When set, the next ticker firing skips `spawn_refresh` until this
    /// instant has passed. Driven by exponential backoff in response to
    /// AWS throttling responses; the user can still force a refresh with
    /// `Ctrl-R` / `:refresh`.
    pub throttle_until: Option<Instant>,
    /// How many consecutive refreshes have come back throttled. Each one
    /// roughly doubles the back-off; resets to zero on the next success.
    pub consecutive_throttles: u32,
    /// Latest still-valid `expiresAt` discovered in `~/.aws/sso/cache`.
    /// Recomputed on every ticker tick — the file is cheap to read and the
    /// user may `aws sso login` from another shell while ebman is open.
    pub sso_expiry: Option<chrono::DateTime<chrono::Utc>>,
    /// Rolling list of in-flight + recently-completed action dispatches.
    /// See `PendingAction`. Surfaced as a header chip + `:pending` overlay.
    pub pending_actions: std::collections::VecDeque<PendingAction>,
    /// Action queued for dispatch but inside the [`UNDO_WINDOW`] —
    /// see [`PendingDispatch`]. `tick_pending_dispatch` (called from
    /// the main loop) fires the AWS call when the deadline passes;
    /// `U` in Normal mode cancels.
    pub pending_dispatch: Option<PendingDispatch>,
    /// Active modal-form session (`:capacity`, future `:network`, etc.).
    /// Populated by `open_form`; cleared on cancel / submit completion.
    pub form: Option<crate::form::Form>,
    /// Handle to the `:logs-tail` polling task. Stored so we can `abort()`
    /// it when the overlay closes or the user switches context. None when
    /// no tail session is active.
    pub log_tail_task: Option<tokio::task::JoinHandle<()>>,
    /// Monotonically increasing id for `:logs-tail` sessions. Lets late
    /// `AppMsg::LogTailEvents` from a previous session be dropped on arrival.
    pub log_tail_session: u64,
    /// Handle to the `:event-tail` polling task — same lifecycle as
    /// `log_tail_task` (aborted on overlay close / context switch).
    pub event_tail_task: Option<tokio::task::JoinHandle<()>>,
    /// Monotonically increasing id for `:event-tail` sessions; late
    /// `AppMsg::EventTail*` from a previous session are dropped.
    pub event_tail_session: u64,
    /// Same pattern for `:why` diagnostic overlays. Late
    /// `AppMsg::WhyRed{Events,Alarms,Instances,Deploys}` for a prior
    /// invocation get dropped when this counter has moved on.
    pub why_red_session: u64,
    /// Drillable items rendered in the active `:why` overlay, written by
    /// `draw_why_red_overlay` and read by the overlay's key handler on
    /// `Enter`. Empty whenever the overlay isn't a `WhyRed`.
    pub why_items: Vec<WhyItem>,
    /// Newer ebman release advertised by crates.io, if any. Populated by the
    /// fire-and-forget update-check task that runs once at startup.
    pub update_available: Option<crate::update_check::LatestRelease>,
    /// When `true`, `run()` exits and `main()` re-execs the binary so the
    /// user keeps their terminal session across a code change. Driven by
    /// `ControlOp::Reload` over the control socket.
    pub reload_requested: bool,
    /// When `Some`, the run loop spawns an embedded SSM shell session
    /// targeting this instance ID into `current_shell`. Keystrokes in
    /// `Mode::Shell` are forwarded to the PTY rather than dispatched as
    /// ebman key bindings.
    pub pending_shell_target: Option<String>,
    /// Set when `:env-edit` is mid-flight: the `fetch_env_vars`
    /// result arrived but the main loop hasn't yet shelled out to
    /// `$EDITOR` (which needs the `Tui` handle to leave + re-enter
    /// the alternate screen, only available in the main loop).
    /// Carries `(env_name, current_env_vars)` — the editor opens
    /// against these, diffs on save, dispatches the deltas.
    pub pending_env_edit: Option<(String, Vec<(String, String)>)>,
    /// The live embedded shell pane, if any. `None` outside Mode::Shell.
    pub current_shell: Option<Box<crate::shell::ShellSession>>,
    /// Mode to return to when the user detaches from a shell pane (F12).
    pub shell_return_mode: Mode,
    /// Snapshot of the last buffer we rendered, captured from inside the
    /// `terminal.draw` closure. ratatui swaps the front/back buffer after
    /// `draw()` returns, so a snapshot taken at SCREEN-request time via
    /// `current_buffer_mut()` would read the empty back-buffer; cloning
    /// during the render is the only reliable way to expose what's actually
    /// on screen to the control plane.
    pub last_rendered_buffer: Option<ratatui::buffer::Buffer>,
    pub notify_bell: bool,
    /// Config-derived values resolved at startup — see ResolvedConfig.
    pub cfg: ResolvedConfig,
    pub newly_red: HashSet<String>,
    /// Env names that appeared for the first time on the most recent
    /// refresh (weren't in `prev_health` last cycle). Used by the env
    /// table to render a transient `+` marker on the NAME cell so a new
    /// env doesn't scroll past unnoticed. Cleared on context switch +
    /// rotated each refresh.
    pub newly_added: HashSet<String>,
    /// Delta in counts vs. the previous refresh, e.g. {"Red" → +1, "Yellow" → -1}.
    pub health_delta: Vec<(String, i32)>,
    pub status_delta: Vec<(String, i32)>,
    prev_alerts: usize,
    prev_health: HashMap<String, String>,
    prev_status: HashMap<String, String>,
    cached_filtered: Vec<usize>,
    cached_display: Vec<DisplayRow>,
    /// Per-application palette colour, assigned by order of first appearance
    /// in the *filtered* view. Rebuilt in [`App::rebuild_view`] so that the
    /// render hot path can look up `app → Color` without allocating a fresh
    /// HashMap per frame (previously `draw_table` did this on every draw).
    pub cached_app_colors: HashMap<String, ratatui::style::Color>,
    /// `env_name → newest available platform version` for envs running a
    /// superseded solution stack. Rebuilt in [`App::rebuild_view`] so the
    /// render hot path does an O(1) lookup instead of re-parsing every
    /// env's stack string per row per frame. Empty until `latest_stacks`
    /// has been fetched.
    pub cached_stale_platforms: HashMap<String, String>,
    pending_select: Option<String>,
    aws: Arc<AwsClient>,
    generation: u64,
    msg_tx: mpsc::UnboundedSender<AppMsg>,
    msg_rx: mpsc::UnboundedReceiver<AppMsg>,
    quit: bool,
}

enum AppMsg {
    Refresh {
        gen: u64,
        result: Result<Vec<Environment>, String>,
    },
    Applications {
        gen: u64,
        result: Result<Vec<Application>, String>,
    },
    /// Per-app newest version, fanned out after `Applications` lands. Each
    /// tuple is `(app_name, latest_version_label, latest_version_created)`;
    /// apps that failed to fetch are simply absent from the results vec so
    /// a transient error on one app doesn't blank the column for all.
    AppLatestVersions {
        gen: u64,
        results: Vec<(
            String,
            Option<String>,
            Option<chrono::DateTime<chrono::Utc>>,
        )>,
    },
    /// Per-Worker-env DLQ depth, fanned out after `Refresh` lands. Each
    /// tuple is `(env_name, dlq_visible_count)`; envs whose fetch failed
    /// are absent so a transient SQS error doesn't blank the column for
    /// all of them. Feeds into the Red-alert calc + the table render.
    WorkerQueueCheck {
        gen: u64,
        /// Per-env outcome: `Ok(Some(depth))` = DLQ depth fetched,
        /// `Ok(None)` = env genuinely has no DLQ, `Err(msg)` = fetch
        /// failed — the handler keeps the previous depth so an
        /// AccessDenied/throttle can't silently clear an alert.
        results: Vec<(String, Result<Option<i64>, String>)>,
    },
    /// Per-env `(healthy, total)` instance counts, fanned out after
    /// `Refresh` lands via `spawn_env_instance_counts`. Failed envs are
    /// absent. Feeds the `INST` column on the main table.
    EnvInstanceCountsCheck {
        gen: u64,
        results: Vec<(String, crate::aws::EnvInstanceCounts)>,
    },
    Rebuild {
        /// Monotonic rebuild epoch captured at spawn. `apply_rebuild`
        /// drops arrivals whose epoch is stale — without it, a slow
        /// switch (SSO refresh) losing the race to a fast one left the
        /// app settled on the FIRST choice, not the last.
        epoch: u64,
        result: Result<Box<AwsClient>, String>,
    },
    Identity {
        gen: u64,
        result: Result<Identity, String>,
    },
    Events {
        gen: u64,
        result: Result<Vec<EbEvent>, String>,
    },
    DetailEvents {
        gen: u64,
        env_name: String,
        result: Result<Vec<EbEvent>, String>,
    },
    DetailInstances {
        gen: u64,
        env_name: String,
        result: Result<Vec<Instance>, String>,
    },
    DetailQueues {
        gen: u64,
        env_name: String,
        result: Result<WorkerQueues, String>,
    },
    DetailMetrics {
        gen: u64,
        env_name: String,
        result: Result<Vec<MetricSeries>, String>,
    },
    DetailTags {
        gen: u64,
        env_name: String,
        result: Result<Vec<(String, String)>, String>,
    },
    /// Env vars for the Config tab — same shape as DetailTags but pulled
    /// from `DescribeConfigurationSettings` filtered to the app:environment
    /// namespace.
    DetailEnvVars {
        gen: u64,
        env_name: String,
        result: Result<Vec<(String, String)>, String>,
    },
    /// CloudWatch Logs groups discovered for an env. Sent once on Detail
    /// open; the Logs tab uses this to render an accurate "streaming
    /// available" hint.
    DetailLogGroups {
        gen: u64,
        env_name: String,
        groups: Vec<String>,
    },
    /// CW alarms attached to an env. Populates the Detail-Health-tab
    /// alarms section. Mirrors `AppMsg::WhyRedAlarms` but lands on the
    /// Detail view's `cw_alarms` field — single fetch path, two
    /// destinations.
    DetailAlarms {
        gen: u64,
        env_name: String,
        result: Result<Vec<crate::aws::CwAlarm>, String>,
    },
    /// Cost Explorer fetch result. Populates `App.costs` so the env
    /// table's COST column renders without waiting for the next
    /// refresh tick. Also written through to the on-disk cache so
    /// subsequent sessions render immediately.
    CostsFetched {
        gen: u64,
        account: Option<String>,
        region: String,
        result: Result<Vec<crate::aws::EnvCost>, String>,
    },
    /// Flat `ListAvailableSolutionStacks` result. The handler folds it into
    /// `App.latest_stacks` (family → newest version) so the envs table can
    /// flag platforms with a newer version available.
    SolutionStacks {
        gen: u64,
        result: Result<Vec<String>, String>,
    },
    /// Recently-registered application versions for an env's app.
    /// Populates the Detail-Health-tab "recent deploys" section.
    DetailRecentVersions {
        gen: u64,
        env_name: String,
        result: Result<Vec<crate::aws::AppVersion>, String>,
    },
    /// Pre-fill values for an open modal form. The handler walks the form's
    /// `(field_key, namespace, option_name)` mappings and populates each
    /// field's `value` from `settings`. Late messages (stale form / context
    /// switch) are dropped.
    FormPrefilled {
        gen: u64,
        env_name: String,
        settings: Result<Vec<(String, String, String)>, String>,
    },
    /// Load `MultiSelect` options for the named field of an open form.
    /// Used by the `:subnets` / `:security-groups` pickers — the option
    /// list comes from EC2 (DescribeSubnets / DescribeSecurityGroups),
    /// not from the env's option settings, so this lives on a separate
    /// AppMsg from FormPrefilled. Annotations are the per-row display
    /// suffixes (AZ + CIDR for subnets; group name + description for SGs).
    FormMultiSelectLoaded {
        gen: u64,
        env_name: String,
        field_key: String,
        result: Result<MultiSelectOptions, String>,
    },
    /// Result of a `:deploy --from PATH` chain (upload → create version →
    /// optional deploy). `summary` is the same label used in the pending
    /// row so `complete_pending` can match. On success we also surface the
    /// new version label in the toast.
    DeployFromLocal {
        gen: u64,
        env_name: String,
        label: String,
        summary: String,
        result: Result<(), String>,
    },
    /// Sent once at the start of a `:logs-tail` session after the log
    /// group is resolved (via discovery or user-supplied). Tells the App
    /// handler to install the `Overlay::LogTail` with the resolved group.
    LogTailOpened {
        gen: u64,
        session_id: u64,
        env_name: String,
        log_group: String,
        since_ms: i64,
    },
    /// New events pushed by the `:logs-tail` polling task. `session_id`
    /// must match the active `Overlay::LogTail` session or the message is
    /// dropped (stale session after the user closed and reopened).
    LogTailEvents {
        gen: u64,
        session_id: u64,
        next_since_ms: i64,
        result: Result<Vec<crate::aws::LogEvent>, String>,
    },
    /// Sent once at the start of an `:event-tail` session — installs the
    /// `Overlay::EventTail` (empty; the first poll fills it).
    EventTailOpened { gen: u64, session_id: u64 },
    /// New fleet events pushed by the `:event-tail` polling task, oldest
    /// first. Same session-id drop rule as `LogTailEvents`.
    EventTailEvents {
        gen: u64,
        session_id: u64,
        result: Result<Vec<crate::aws::Event>, String>,
    },
    /// One section's result for the `:why` diagnostic overlay. The session
    /// id matches the `Overlay::WhyRed { session_id, .. }` active when the
    /// fetcher was spawned; late results for stale sessions are dropped on
    /// arrival.
    WhyRedEvents {
        gen: u64,
        session_id: u64,
        result: Result<Vec<crate::aws::Event>, String>,
    },
    WhyRedAlarms {
        gen: u64,
        session_id: u64,
        result: Result<Vec<crate::aws::CwAlarm>, String>,
    },
    WhyRedInstances {
        gen: u64,
        session_id: u64,
        result: Result<Vec<crate::aws::Instance>, String>,
    },
    WhyRedDeploys {
        gen: u64,
        session_id: u64,
        result: Result<Vec<crate::aws::AppVersion>, String>,
    },
    /// Worker-only: main + DLQ queue stats for the `:why` overlay.
    WhyRedQueues {
        gen: u64,
        session_id: u64,
        result: Result<crate::aws::WorkerQueues, String>,
    },
    /// Worker-only: DLQ message peek (3 bodies). Fired by the queues
    /// handler once the DLQ stats indicate non-zero depth.
    WhyRedDlqMessages {
        gen: u64,
        session_id: u64,
        result: Result<Vec<crate::aws::QueueMessage>, String>,
    },
    DryRunResult {
        gen: u64,
        env_name: String,
        result: Result<Vec<Instance>, String>,
    },
    /// `fetch_env_vars` result for `:env-edit`. The handler stashes
    /// the env-name + KV pairs in `App.pending_env_edit`; the main
    /// loop tick takes them and shells out to `$EDITOR`. Two-step
    /// because the editor needs the `Tui` handle (alt-screen
    /// leave/enter), which is only available in the main loop.
    EnvVarsForEdit {
        gen: u64,
        env_name: String,
        result: Result<Vec<(String, String)>, String>,
    },
    PreflightEvents {
        gen: u64,
        env_name: String,
        result: Result<Vec<EbEvent>, String>,
    },
    /// Pre-deploy version preview for the confirm modal. Carries
    /// the pre-rendered `format_deploy_preview` body so the
    /// handler stays trivial — just stuff it into the modal slot.
    VersionPreview {
        gen: u64,
        env_name: String,
        result: Result<String, String>,
    },
    /// Pre-deploy health-check probe outcome. `Ok(())` means the
    /// probe was successful (2xx); `Err(reason)` means non-2xx /
    /// timeout / connect error and the modal should render a
    /// yellow warning so the operator can decide whether to
    /// continue. Doesn't block the deploy either way.
    HealthCheckProbe {
        gen: u64,
        env_name: String,
        result: Result<(), String>,
    },
    /// Pre-deploy unavailability estimate. `line` is the rendered
    /// modal text plus a caution flag for colouring. `None` if the
    /// option-settings fetch failed — the modal stays silent rather
    /// than rendering an error line (the impact is observability,
    /// not safety).
    UnavailabilityEstimate {
        gen: u64,
        env_name: String,
        line: Option<(String, bool)>,
    },
    /// Lint findings against the confirm-modal's target env,
    /// emitted by `spawn_confirm_lint`. Same `Issue` shape as
    /// the `:lint` TUI overlay + `ebman lint` CLI — designed
    /// for one engine, three surfaces.
    ConfirmModalLint {
        gen: u64,
        env_name: String,
        issues: Vec<crate::lint::Issue>,
    },
    /// 0.21: side-channel cache update for the lint-input caches.
    /// Emitted by `spawn_confirm_lint` (and any future lint call site)
    /// after a fresh tag / health fetch lands. `tags`/`healthy` are
    /// `None` when the value came from cache (no need to re-store).
    /// Handler writes to `env_tag_cache` / `env_health_cache` so
    /// the NEXT modal-open against the same env hits cache.
    LintInputsCached {
        gen: u64,
        env_name: String,
        tags: Option<Vec<String>>,
        healthy: Option<i64>,
    },
    /// Pre-flight result for one region of a `:rollout` flow.
    /// Carries the region's current version_label on success
    /// (so the plan overlay can show "currently build-820 →
    /// target build-900") or an error string on failure (STS,
    /// list_environments, or env-not-found). The handler
    /// populates the matching `RolloutRegion` row + advances
    /// the flow to AwaitingConfirm once all regions report.
    RolloutPreflight {
        gen: u64,
        region: String,
        result: Result<String, String>,
    },
    /// Dispatch outcome for one region of a `:rollout` flow.
    /// `Ok(())` after a successful `deploy_version` (and Green
    /// observation if --wait-for-green was set);
    /// `Err(reason)` on dispatch failure or wait timeout. The
    /// handler records the outcome, advances `next_index`, and
    /// either dispatches the next region OR halts (on first
    /// failure).
    RolloutDispatched {
        gen: u64,
        region: String,
        result: Result<(), String>,
    },
    /// `:undo` capture — emitted from the option-settings update
    /// spawn after a successful write, carrying the reverse-action
    /// so `App.undo_history` can push it for later `:undo`.
    UndoCaptured { gen: u64, entry: UndoEntry },
    /// `:rollback` — the env's recent events came back; the handler
    /// scans them for the previously-deployed version label and opens
    /// the deploy-confirm modal for it.
    RollbackTarget {
        gen: u64,
        env_name: String,
        current_version: String,
        result: Result<Vec<EbEvent>, String>,
    },
    Alarms {
        gen: u64,
        env_name: String,
        result: Result<Vec<CwAlarm>, String>,
    },
    DlqMessages {
        gen: u64,
        env_name: String,
        /// The queue the peek ran against — the handler drops results
        /// for a queue that's no longer `dlq.viewing` (an `m`-toggle
        /// mid-fetch used to display the WRONG queue's messages, with
        /// receipt handles AWS would reject).
        queue_url: String,
        result: Result<Vec<QueueMessage>, String>,
    },
    DlqActionResult {
        gen: u64,
        env_name: String,
        result: Result<DlqOp, String>,
    },
    ActionResult {
        gen: u64,
        action: Action,
        env_name: String,
        result: Result<(), String>,
    },
    /// Intermediate progress for the tail-logs pipeline (`Requesting` →
    /// `Polling` → `Fetching` → `Ready`). The UI consumes these so the user
    /// sees forward motion during the multi-second wait for EB to upload tail
    /// samples to S3.
    DetailLogsProgress {
        gen: u64,
        env_name: String,
        stage: LogTailStage,
        attempt: u32,
    },
    /// Final tail-logs payload — `Vec<(ec2_instance_id, log_text)>` on success.
    DetailLogs {
        gen: u64,
        env_name: String,
        result: Result<Vec<(String, String)>, String>,
    },
    /// Generic text overlay payload. Used by several commands that all
    /// finish on a background task and want to render the result as a
    /// scrollable text dump (`:find-env`, `:resources`, `:org-health`,
    /// `:upgrade`, `:custom-platforms`). `title` shows in the overlay block
    /// header; previous variants reused the SavedConfigs styling and
    /// inherited its title which lied about the content.
    TextOverlay {
        gen: u64,
        title: String,
        body: String,
    },
    /// Application versions listing for the env's app, fetched via `:versions`.
    /// `deployed_label` is the env's current version_label so the overlay
    /// can mark which row is "the live one" — common operator pain when
    /// rolling back.
    AppVersions {
        gen: u64,
        application: String,
        deployed_label: Option<String>,
        result: Result<Vec<AppVersion>, String>,
    },
    /// Result of the startup update-check. `None` means "no newer release"
    /// or the check couldn't reach crates.io; either way, the UI doesn't
    /// nag the user. We don't carry a generation — the message is anchored
    /// to the process, not a particular AWS context.
    UpdateCheck(Option<crate::update_check::LatestRelease>),
    /// Watchdog deadline for `:deploy --auto-rollback Nm`. Fires once
    /// `secs` after the deploy dispatched. Handler reads the env's
    /// current cached health: if Green, the watchdog disarms with a
    /// status toast; otherwise it dispatches a rollback deploy to
    /// the captured `DeploySnapshot.previous_version_label`.
    AutoRollbackCheck { gen: u64, env_name: String },
    /// Result of an `UpdateTagsForResource` call from `:tag` / `:untag`.
    /// On success we re-issue the Config-tab tag fetch so the UI reflects
    /// the new state immediately.
    TagUpdate {
        gen: u64,
        env_name: String,
        summary: String,
        result: Result<(), String>,
    },
    /// Result of an `UpdateEnvironment(option_settings)` call from any of
    /// the small option-settings commands (`:logs-stream`, `:notify`,
    /// `:managed-window`). `summary` is the same human-readable label that
    /// went into the pending panel so `complete_pending` can match.
    OptionSettingsUpdate {
        gen: u64,
        env_name: String,
        summary: String,
        result: Result<(), String>,
    },
    /// Result of a CloudWatch alarm create / delete via `:alarm-create` /
    /// `:alarm-delete`. `verb` is "create" or "delete" so the toast can use
    /// the correct tense.
    AlarmOp {
        gen: u64,
        verb: &'static str,
        alarm_name: String,
        env_name: String,
        result: Result<(), String>,
    },
    /// Result of a `DeleteApplicationVersion` call from `:delete-version`.
    DeleteAppVersion {
        gen: u64,
        application: String,
        label: String,
        force: bool,
        result: Result<(), String>,
    },
}

#[derive(Debug, Clone)]
pub enum DlqOp {
    Resent {
        message_id: String,
    },
    /// Single-message delete (`x`) — drops the row by id like Resent,
    /// but the toast must not claim a resend that never happened.
    Deleted {
        message_id: String,
    },
    Purged,
    /// Outcome of a batch replay: `count` messages moved to the main queue
    /// (sent + deleted from the DLQ), `failures` that errored mid-way.
    Replayed {
        count: usize,
        failures: usize,
    },
}

/// True when this looks like the user's very first run: no persisted ebman
/// state on disk *and* no AWS credentials or config to talk to. We use that as
/// the trigger for the welcome overlay rather than nagging on every cold
/// start.
fn is_first_run() -> bool {
    let no_state = !crate::util::config_file("state.toml").exists();
    let home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_default();
    let no_creds = !home.join(".aws").join("credentials").exists()
        && !home.join(".aws").join("config").exists();
    no_state && no_creds
}

async fn init_client(
    profile: Option<String>,
    region: Option<String>,
) -> Result<(AwsClient, Option<String>, Option<String>, Option<String>)> {
    // Two-stage init:
    //   1. AwsClient::with must succeed (SDK config / region parsing). On
    //      failure we fall back from persisted profile/region to env defaults.
    //   2. verify_identity is *best-effort* — STS perms aren't required to use
    //      EB describe APIs. On failure we log + surface a startup warning but
    //      keep going with the client, leaving account/caller fields unset.
    let (mut client, used_profile, used_region) =
        match AwsClient::with(profile.clone(), region.clone()).await {
            Ok(c) => (c, profile, region),
            Err(e) if profile.is_some() || region.is_some() => {
                tracing::warn!(
                    error = %e,
                    profile = ?profile,
                    region = ?region,
                    "persisted profile/region failed to resolve — falling back to env defaults"
                );
                let c = AwsClient::with(None, None).await?;
                (c, None, None)
            }
            Err(e) => return Err(e),
        };

    let warning = match client.verify_identity().await {
        Ok(id) => {
            client.context.account_id = id.account_id;
            client.context.caller_arn = id.caller_arn;
            None
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                "sts:GetCallerIdentity failed — proceeding without identity. EB describe perms may still be available."
            );
            Some(format!("identity unknown ({e}); EB calls may still work"))
        }
    };
    Ok((client, used_profile, used_region, warning))
}

impl App {
    pub async fn new(config: Config) -> Result<Self> {
        // Stash the notify-webhook URL globally before any audit
        // line could be written. OnceLock::set is no-op on second
        // call so calling App::new twice in the same process (e.g.
        // a test harness) doesn't crash, but does mean the FIRST
        // App's webhook wins — fine for production where there's
        // only ever one App.
        crate::audit::set_notify_webhook(config.notify_webhook.clone());
        // Resolve LLM settings here so the struct literal below can
        // own `config.extra_regions` without a partial-move conflict
        // — `Settings::from_config` borrows; the field assignment
        // moves out of the same struct.
        let explain_settings = crate::llm::Settings::from_config(&config);
        let persisted = state::load();
        // Project config: optional `.ebman/ebman.toml` walked up from
        // cwd. Profile / region from the project win over persisted
        // state so a repo can pin its working context; everything
        // else (filter, application, runbooks) merges in further down
        // once `app` is constructed.
        let project = crate::project::load_from_cwd();
        let project_profile = project.as_ref().and_then(|p| p.profile.clone());
        let project_region = project.as_ref().and_then(|p| p.region.clone());
        // EB CLI config (`.elasticbeanstalk/config.yml`) is a
        // secondary source — fills in profile / region / application
        // only when the higher-precedence `.ebman/` file doesn't.
        // Most EB CLI users already maintain this file, so reading
        // it avoids forcing a duplicate `.ebman/` entry.
        let eb_cli = crate::eb_cli::load_from_cwd();
        let eb_cli_profile = eb_cli.as_ref().and_then(|c| c.profile.clone());
        let eb_cli_region = eb_cli.as_ref().and_then(|c| c.region.clone());
        tracing::info!(
            target: "ebman::state",
            persisted_profile = ?persisted.profile,
            persisted_region = ?persisted.region,
            project_profile = ?project_profile,
            project_region = ?project_region,
            eb_cli_profile = ?eb_cli_profile,
            eb_cli_region = ?eb_cli_region,
            "state::load"
        );
        let effective_profile = project_profile
            .or(eb_cli_profile)
            .or_else(|| persisted.profile.clone());
        let effective_region = project_region
            .or(eb_cli_region)
            .or_else(|| persisted.region.clone());
        let (aws, override_profile, override_region, identity_warning) =
            init_client(effective_profile, effective_region).await?;
        let aws = Arc::new(aws);
        let context = aws.context.clone();
        tracing::info!(
            target: "ebman::state",
            override_profile = ?override_profile,
            override_region = ?override_region,
            context_region = %context.region,
            context_profile = ?context.profile,
            "init_client returned"
        );
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let mut table_state = TableState::default();
        table_state.select(Some(0));

        let (sort_key, sort_desc) = parse_sort(persisted.sort.as_deref());
        let redact = persisted.redact.or(config.redact_default).unwrap_or(false);
        let grouped = persisted
            .grouped
            .or(config.grouped_default)
            .unwrap_or(false);
        let events_visible = persisted.events_visible.unwrap_or(false);
        let event_time_format = persisted.event_time_format.unwrap_or_default();
        let refresh_interval = config.refresh_interval;

        let mut app_table_state = TableState::default();
        app_table_state.select(Some(0));

        let names = builtin_commands();
        let plugins_loaded = crate::plugins::load(&names);
        for w in &plugins_loaded.warnings {
            tracing::warn!(target: "ebman::plugins", "{}", w);
        }
        let plugin_startup_warning = if plugins_loaded.warnings.is_empty() {
            None
        } else {
            Some(format!("plugins: {}", plugins_loaded.warnings.join("; ")))
        };

        let mut app = Self {
            context,
            scope: Scope::Envs,
            applications: Vec::new(),
            app_table_state,
            environments: Vec::new(),
            table_state,
            table_area: Rect::default(),
            mode: Mode::Normal,
            filter: persisted.filter.unwrap_or_default().into(),
            load_state: LoadState::Idle,
            loading_since: None,
            refresh_interval,
            loading_visible_until: None,
            last_refresh: None,
            status_message: None,
            error_message: None,
            picker: None,
            override_profile,
            override_region,
            history: HashMap::new(),
            redact,
            grouped,
            sort_key,
            sort_desc,
            command_input: TextInput::new(),
            completion: CompletionState::default(),
            quickjump_input: TextInput::new(),
            extra_regions: config.extra_regions,
            event_panel: EventPanel {
                events: Vec::new(),
                visible: events_visible,
                time_format: event_time_format,
                for_env: None,
                scroll: 0,
                area: None,
                drag_origin: None,
                cursor: None,
                height: 10,
            },
            multi_selected: BTreeSet::new(),
            apps_selected: BTreeSet::new(),
            focus: Focus::Table,
            multi_regions: Vec::new(),
            detail: None,
            action_flow: None,
            dlq: None,
            theme: {
                let (mut t, warning) = Theme::resolve(&config.theme);
                if let Some(w) = warning {
                    tracing::warn!("{w}");
                }
                match config.icons.trim().to_ascii_lowercase().as_str() {
                    "ascii" => t.icons = IconStyle::Ascii,
                    "powerline" | "nerd" | "nerdfont" => t.icons = IconStyle::Powerline,
                    _ => {}
                }
                Arc::new(t)
            },
            view_mode: ViewMode::Default,
            help: HelpState {
                scroll: 0,
                max_scroll: 0,
                topic: HelpTopic::Global,
                pre_mode: None,
                pre_overlay: None,
            },
            hover_row: None,
            alerts: 0,
            worker_dlq_depths: std::collections::HashMap::new(),
            worker_dlq_stale: std::collections::HashSet::new(),
            rebuild_epoch: 0,
            env_tag_cache: std::collections::HashMap::new(),
            env_health_cache: std::collections::HashMap::new(),
            // Restore persisted snapshots so a cross-session `:rollback`
            // / auto-rollback still has a target. Malformed lines are
            // silently skipped — better to drop one stale entry than
            // abort the App-init path.
            deploy_snapshots: persisted
                .deploy_snapshots
                .iter()
                .filter_map(
                    |(env, raw)| match DeploySnapshot::parse_persisted(env, raw) {
                        Some(snap) => Some((env.clone(), snap)),
                        None => {
                            // Log the malformed line so the operator can spot
                            // a corrupted state.toml entry. We still skip the
                            // entry — better to lose one stale snapshot than
                            // to abort App init.
                            tracing::warn!(
                                target: "ebman::state",
                                env = %env,
                                raw = %raw,
                                "malformed deploy_snapshot entry in state.toml — skipping"
                            );
                            None
                        }
                    },
                )
                .collect(),
            armed_watchdogs: std::collections::HashMap::new(),
            watching_deploys: std::collections::HashMap::new(),
            deploy_freeze: None,
            incident: None,
            // Load tfstate from cwd at construction time. Failure
            // is silent (`None`) — operators not using terraform
            // shouldn't see any UI surface; operators with a
            // discoverable tfstate get the badge + drift overlay
            // immediately. Re-loaded on context switch (account /
            // region change) and on `:drift refresh`.
            tf_state: crate::terraform::load_from_cwd(),
            tf_managed_envs: std::collections::HashSet::new(),
            undo_history: std::collections::VecDeque::new(),
            promotion_history: Vec::new(),
            demo_mode: false,
            env_instance_counts: std::collections::HashMap::new(),
            cost_enabled: persisted.cost_enabled.unwrap_or(false),
            costs: std::collections::HashMap::new(),
            costs_fetched_at: None,
            latest_stacks: std::collections::HashMap::new(),
            frozen: false,
            first_run_hint: !crate::state::file_exists(),
            current_overlay: None,
            message_log: VecDeque::with_capacity(MESSAGE_LOG_CAP),
            toasts: VecDeque::with_capacity(TOAST_CAP),
            palette_input: TextInput::new(),
            palette_items: Vec::new(),
            palette_filtered: Vec::new(),
            palette_state: ListState::default(),
            read_only: false,
            pinned: persisted.pinned,
            pinned_apps: persisted.pinned_apps,
            aliases: persisted.aliases,
            saved_views: persisted.saved_views,
            hidden_cols: persisted.hidden_cols,
            custom_metrics: persisted.custom_metrics,
            log_reload: None,
            log_directive: std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "info,aws=warn,hyper=warn".to_string()),
            plugins: plugins_loaded.plugins,
            status_snapshot_at_refresh: None,
            status_message_pinned: false,
            throttle_until: None,
            consecutive_throttles: 0,
            sso_expiry: crate::sso::latest_session_expiry(),
            pending_actions: std::collections::VecDeque::with_capacity(PENDING_CAP),
            pending_dispatch: None,
            form: None,
            log_tail_task: None,
            log_tail_session: 0,
            event_tail_task: None,
            event_tail_session: 0,
            why_red_session: 0,
            why_items: Vec::new(),
            update_available: None,
            reload_requested: false,
            pending_shell_target: None,
            pending_env_edit: None,
            current_shell: None,
            shell_return_mode: Mode::Normal,
            last_rendered_buffer: None,
            notify_bell: config.notify_bell,
            cfg: ResolvedConfig {
                notify_webhook: config.notify_webhook.clone(),
                command_aliases: config.command_aliases.clone(),
                lint_disable: config.lint_disable.clone(),
                explain_settings,
                required_tags: config.required_tags,
                cfg_icons_raw: config.icons.clone(),
                profile_themes: config.profile_themes.clone(),
                runbooks: config.runbooks.clone(),
                safety_envs: config.safety_envs.clone(),
                safety_accounts: config.safety_accounts.clone(),
                accounts: config.accounts.clone(),
                base_theme_name: config.theme.clone(),
            },
            newly_red: HashSet::new(),
            newly_added: HashSet::new(),
            health_delta: Vec::new(),
            status_delta: Vec::new(),
            prev_alerts: 0,
            prev_health: HashMap::new(),
            prev_status: HashMap::new(),
            cached_filtered: Vec::new(),
            cached_display: Vec::new(),
            cached_app_colors: HashMap::new(),
            cached_stale_platforms: HashMap::new(),
            pending_select: persisted.selected_env,
            aws,
            generation: 0,
            msg_tx,
            msg_rx,
            quit: false,
        };
        app.rebuild_view();
        // Plugin warnings take priority over identity warnings — they're a user
        // misconfiguration the user can act on now (red error banner).
        // identity_warning is informational (yellow status line) — for
        // fresh-creds users (no SSO login, expired creds), the warning is
        // the EXPECTED state, not an error. Route to status_message with
        // the actionable hints so first-run UX isn't an alarm.
        if let Some(w) = plugin_startup_warning {
            app.error_message = Some(w);
        } else if let Some(w) = identity_warning {
            app.status_message = Some(format!(
                "{w} — try `aws sso login` or `:profile NAME` to switch creds"
            ));
            app.status_message_pinned = true;
        }
        if is_first_run() {
            app.current_overlay = Some(Overlay::Whatsnew(WELCOME_OVERLAY.into()));
        }
        // Swap to the per-profile theme override if one is configured for
        // the resolved profile. Done here (after `context` is populated)
        // so the initial frame already shows the right palette.
        app.maybe_apply_profile_theme();
        // Apply the rest of the project config (filter / application
        // prefill, runbook merge) after the App is fully constructed.
        // Project entries win over user-level runbooks because the
        // repo is the more-specific source.
        if let Some(proj) = project {
            if let Some(filter) = proj.filter {
                app.filter = filter.into();
            } else if let Some(app_name) = proj.application {
                // Treat `application` as a filter prefill when no
                // explicit `filter` was set — pre-scopes the table to
                // a single-app repo's envs without a hard pin.
                app.filter = app_name.into();
            }
            app.cfg.runbooks.extend(proj.runbooks);
        }
        // EB CLI application name fills in as a filter prefill when
        // `.ebman/` hasn't already set one. Same "soft scope" intent
        // as the project-config path. `.ebman/` always wins because
        // it's the more explicit, ebman-native source.
        if app.filter.is_empty() {
            if let Some(eb) = eb_cli {
                if let Some(app_name) = eb.application {
                    app.filter = app_name.into();
                }
            }
        }
        // The project / EB-CLI blocks above mutate `app.filter` after the
        // initial `rebuild_view()`, so the cached view is stale w.r.t. the
        // configured filter — rebuild once more so the first frame honours
        // it (house rule: filter mutations call rebuild_view).
        app.rebuild_view();
        // Derive the tf-managed name set from the loaded tfstate
        // so the env-table badge can do O(1) lookups per row.
        app.refresh_tf_managed_envs();
        Ok(app)
    }

    /// Runtime constructor for `--demo` mode. Wraps `for_tests` with a
    /// stub `AwsClient` and an explicit `demo_mode = true` so the
    /// refresh / update-check spawns become no-ops. Then asks the
    /// hand-crafted fixture to populate `environments` / events /
    /// instance counts / cost data so the main table renders with
    /// believable content. Synchronous — no AWS calls, no disk I/O
    /// (state.load is skipped via the for_tests path).
    pub fn new_demo(config: Config) -> Self {
        // Demo's stub client fails every call by design — downgrade
        // those log lines from ERROR to debug so a demo session doesn't
        // fill the log with expected failures.
        DEMO_QUIET_AWS_ERRORS.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut app = Self::for_tests(crate::aws::AwsClient::stub(), config);
        app.demo_mode = true;
        crate::demo_fixture::install(&mut app);
        app
    }

    /// Synchronous AWS-free constructor. Skips `init_client` (no AWS
    /// round-trip), `state::load` (no disk read — caller passes a fresh
    /// empty state), and the spawn_identity / spawn_refresh kickoffs.
    /// The caller is responsible for providing a pre-built `AwsClient`
    /// (typically via `AwsClient::for_tests` or `AwsClient::stub()`).
    /// `msg_tx` / `msg_rx` are created here so `handle_event` can fire
    /// spawn helpers that send AppMsg variants without panicking;
    /// callers can drain `msg_rx` to inspect dispatched messages.
    ///
    /// Two consumers today: the unit-test harness (`#[cfg(test)]`
    /// builds) and the runtime `--demo` mode constructor (`new_demo`,
    /// which builds on top of this + a hand-crafted fixture). Kept
    /// `pub(crate)` — both callers are in this crate.
    pub(crate) fn for_tests(aws: crate::aws::AwsClient, config: Config) -> Self {
        let aws = Arc::new(aws);
        let context = aws.context.clone();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
        let explain_settings = crate::llm::Settings::from_config(&config);
        let mut table_state = TableState::default();
        table_state.select(Some(0));
        let mut app_table_state = TableState::default();
        app_table_state.select(Some(0));
        let mut app = Self {
            context,
            scope: Scope::Envs,
            applications: Vec::new(),
            app_table_state,
            environments: Vec::new(),
            table_state,
            table_area: Rect::default(),
            mode: Mode::Normal,
            filter: TextInput::new(),
            load_state: LoadState::Idle,
            loading_since: None,
            refresh_interval: config.refresh_interval,
            loading_visible_until: None,
            last_refresh: None,
            status_message: None,
            error_message: None,
            picker: None,
            override_profile: None,
            override_region: None,
            history: HashMap::new(),
            redact: config.redact_default.unwrap_or(false),
            grouped: config.grouped_default.unwrap_or(false),
            sort_key: SortKey::App,
            sort_desc: false,
            command_input: TextInput::new(),
            completion: CompletionState::default(),
            quickjump_input: TextInput::new(),
            extra_regions: config.extra_regions.clone(),
            event_panel: EventPanel {
                events: Vec::new(),
                visible: false,
                time_format: EventTimeFormat::default(),
                for_env: None,
                scroll: 0,
                area: None,
                drag_origin: None,
                cursor: None,
                height: 10,
            },
            multi_selected: BTreeSet::new(),
            apps_selected: BTreeSet::new(),
            focus: Focus::Table,
            multi_regions: Vec::new(),
            detail: None,
            action_flow: None,
            dlq: None,
            theme: {
                let (mut t, _w) = Theme::resolve(&config.theme);
                match config.icons.trim().to_ascii_lowercase().as_str() {
                    "ascii" => t.icons = IconStyle::Ascii,
                    "powerline" | "nerd" | "nerdfont" => t.icons = IconStyle::Powerline,
                    _ => {}
                }
                Arc::new(t)
            },
            view_mode: ViewMode::Default,
            help: HelpState {
                scroll: 0,
                max_scroll: 0,
                topic: HelpTopic::Global,
                pre_mode: None,
                pre_overlay: None,
            },
            hover_row: None,
            alerts: 0,
            worker_dlq_depths: std::collections::HashMap::new(),
            worker_dlq_stale: std::collections::HashSet::new(),
            rebuild_epoch: 0,
            env_tag_cache: std::collections::HashMap::new(),
            env_health_cache: std::collections::HashMap::new(),
            deploy_snapshots: std::collections::HashMap::new(),
            armed_watchdogs: std::collections::HashMap::new(),
            watching_deploys: std::collections::HashMap::new(),
            deploy_freeze: None,
            incident: None,
            // Tests / demo mode don't probe the operator's cwd
            // for tfstate — keeps test runs deterministic and
            // prevents demo screencasts from leaking real fleet
            // detail. Tests that exercise drift behavior set
            // `app.tf_state` explicitly.
            tf_state: None,
            tf_managed_envs: std::collections::HashSet::new(),
            undo_history: std::collections::VecDeque::new(),
            promotion_history: Vec::new(),
            demo_mode: false,
            env_instance_counts: std::collections::HashMap::new(),
            cost_enabled: false,
            costs: std::collections::HashMap::new(),
            costs_fetched_at: None,
            latest_stacks: std::collections::HashMap::new(),
            frozen: false,
            first_run_hint: false,
            current_overlay: None,
            message_log: VecDeque::with_capacity(MESSAGE_LOG_CAP),
            toasts: VecDeque::with_capacity(TOAST_CAP),
            palette_input: TextInput::new(),
            palette_items: Vec::new(),
            palette_filtered: Vec::new(),
            palette_state: ListState::default(),
            read_only: false,
            pinned: BTreeSet::new(),
            pinned_apps: BTreeSet::new(),
            aliases: std::collections::BTreeMap::new(),
            saved_views: std::collections::BTreeMap::new(),
            hidden_cols: BTreeSet::new(),
            custom_metrics: std::collections::BTreeMap::new(),
            log_reload: None,
            log_directive: "info".to_string(),
            plugins: std::collections::BTreeMap::new(),
            status_snapshot_at_refresh: None,
            status_message_pinned: false,
            throttle_until: None,
            consecutive_throttles: 0,
            sso_expiry: None,
            pending_actions: std::collections::VecDeque::with_capacity(PENDING_CAP),
            pending_dispatch: None,
            form: None,
            log_tail_task: None,
            log_tail_session: 0,
            event_tail_task: None,
            event_tail_session: 0,
            why_red_session: 0,
            why_items: Vec::new(),
            update_available: None,
            reload_requested: false,
            pending_shell_target: None,
            pending_env_edit: None,
            current_shell: None,
            shell_return_mode: Mode::Normal,
            last_rendered_buffer: None,
            notify_bell: config.notify_bell,
            cfg: ResolvedConfig {
                notify_webhook: config.notify_webhook.clone(),
                command_aliases: config.command_aliases.clone(),
                lint_disable: config.lint_disable.clone(),
                explain_settings,
                required_tags: config.required_tags.clone(),
                cfg_icons_raw: config.icons.clone(),
                profile_themes: config.profile_themes.clone(),
                runbooks: config.runbooks.clone(),
                safety_envs: config.safety_envs.clone(),
                safety_accounts: config.safety_accounts.clone(),
                accounts: config.accounts.clone(),
                base_theme_name: config.theme.clone(),
            },
            newly_red: HashSet::new(),
            newly_added: HashSet::new(),
            health_delta: Vec::new(),
            status_delta: Vec::new(),
            prev_alerts: 0,
            prev_health: HashMap::new(),
            prev_status: HashMap::new(),
            cached_filtered: Vec::new(),
            cached_display: Vec::new(),
            cached_app_colors: HashMap::new(),
            cached_stale_platforms: HashMap::new(),
            pending_select: None,
            aws,
            generation: 0,
            msg_tx,
            msg_rx,
            quit: false,
        };
        app.rebuild_view();
        app
    }

    pub async fn run(
        &mut self,
        terminal: &mut Tui,
        mut control_rx: Option<mpsc::UnboundedReceiver<crate::control::ControlOp>>,
    ) -> Result<()> {
        let mut events = EventStream::new();
        let mut ticker = tokio::time::interval(self.refresh_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut anim = tokio::time::interval(Duration::from_millis(100));
        anim.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Higher-frequency ticker for the embedded shell pane (~30 fps) so
        // PTY output renders promptly. Idle-gated below.
        let mut shell_tick = tokio::time::interval(Duration::from_millis(30));
        shell_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // Listen for OS termination signals (SIGINT from terminal Ctrl-C,
        // SIGTERM from cargo-watch / process supervisors). Default handlers
        // would kill us abruptly without running `leave_tui` — leaving the
        // terminal in raw mode and breaking the user's shell. Catching them
        // lets us set `quit = true` and break the loop, which the main
        // entrypoint follows with a proper terminal restore.
        let mut sigint = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())
            .map_err(|e| color_eyre::eyre::eyre!("install SIGINT handler: {e}"))?;
        let mut sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .map_err(|e| color_eyre::eyre::eyre!("install SIGTERM handler: {e}"))?;
        let mut sighup = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::hangup())
            .map_err(|e| color_eyre::eyre::eyre!("install SIGHUP handler: {e}"))?;
        // Track mode across iterations so we can clear the terminal when
        // entering or leaving Shell mode (avoids the prior view bleeding
        // around the new pane / shell content lingering after exit).
        let mut prev_mode = self.mode;
        self.spawn_refresh();
        self.spawn_update_check();

        loop {
            // The closure both renders and clones the resulting buffer so the
            // control plane has a faithful snapshot — ratatui's terminal swaps
            // front/back after draw() so we can't grab it post-hoc.
            // Refetch the events panel when the cursor has moved to a
            // different env since the last fetch. Fires before draw so the
            // user sees "loading…" rather than the previous env's events.
            self.refresh_events_if_selection_changed();

            // Clear the terminal on Shell-mode boundary crossings so cells
            // from the prior view don't bleed through (entering Shell) and
            // shell content doesn't linger when we exit (leaving Shell).
            if (self.mode == Mode::Shell) != (prev_mode == Mode::Shell) {
                let _ = terminal.clear();
            }
            prev_mode = self.mode;

            let mut snapshot: Option<ratatui::buffer::Buffer> = None;
            terminal.draw(|f| {
                ui::draw(f, self);
                snapshot = Some(f.buffer_mut().clone());
            })?;
            self.last_rendered_buffer = snapshot;
            if self.quit {
                break;
            }

            let prev_status = self.status_message.clone();
            let prev_error = self.error_message.clone();

            tokio::select! {
                // Termination signals — set the quit flag and break so the
                // main entrypoint's `leave_tui` runs and the terminal is
                // restored. Without these the default OS handler kills the
                // process abruptly, leaving the terminal in raw mode + alt-
                // screen for the parent shell to deal with.
                _ = sigint.recv() => {
                    tracing::info!(target: "ebman", "received SIGINT, shutting down gracefully");
                    self.quit = true;
                }
                _ = sigterm.recv() => {
                    tracing::info!(target: "ebman", "received SIGTERM, shutting down gracefully");
                    self.quit = true;
                }
                _ = sighup.recv() => {
                    tracing::info!(target: "ebman", "received SIGHUP, shutting down gracefully");
                    self.quit = true;
                }
                maybe_event = events.next() => {
                    match maybe_event {
                        Some(Ok(event)) => self.handle_event(event),
                        Some(Err(e)) => {
                            self.error_message = Some(format!("input error: {e}"));
                        }
                        None => break,
                    }
                }
                _ = ticker.tick() => {
                    // Cheap and self-contained — re-read the SSO cache on every
                    // tick so the header countdown stays accurate even if the
                    // user `aws sso login`s in another shell mid-session.
                    self.sso_expiry = crate::sso::latest_session_expiry();
                    let now = Instant::now();
                    let backed_off = self
                        .throttle_until
                        .map(|t| now < t)
                        .unwrap_or(false);
                    if !self.frozen && !backed_off {
                        self.spawn_refresh();
                        if matches!(self.mode, Mode::Detail) {
                            if let Some(d) = self.detail.as_ref() {
                                if d.auto_refresh {
                                    self.detail_refresh_active_tab();
                                }
                            }
                        }
                    } else if backed_off && self.throttle_until.is_some_and(|t| now >= t) {
                        // Just crossed the back-off horizon — clear so the next
                        // tick proceeds normally even if no refresh fired here.
                        self.throttle_until = None;
                    }
                }
                _ = shell_tick.tick(), if self.current_shell.is_some() => {
                    // ~30 fps redraw while a shell pane is live so typed
                    // echo / backspace erase / vim frames render promptly.
                    // Demo sessions also use this beat to drain their
                    // canned bytes into the parser (typewriter animation).
                    if let Some(shell) = self.current_shell.as_ref() {
                        shell.tick_demo_typer();
                    }
                }
                _ = anim.tick(), if self.loading_since.is_some()
                    || !self.toasts.is_empty()
                    || self.pending_dispatch.is_some()
                    || !self.armed_watchdogs.is_empty()
                    || !self.watching_deploys.is_empty()
                    || matches!(self.current_overlay, Some(Overlay::About(_)))
                    || self.loading_visible_until.map(|t| Instant::now() < t).unwrap_or(false) => {
                    // Wake the draw loop so the spinner can advance, toasts
                    // expire promptly, the cancel-window countdown stays
                    // accurate, and the loading-indicator linger window can
                    // finish counting down. Gated to keep idle CPU at zero
                    // otherwise.
                }
                Some(msg) = self.msg_rx.recv() => {
                    self.handle_msg(msg);
                }
                Some(op) = async {
                    match control_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    self.handle_control_op(op, terminal);
                }
            }

            if self.status_message != prev_status {
                if let Some(s) = self.status_message.clone() {
                    self.log_message(MsgKind::Info, s.clone());
                    self.push_toast(ToastKind::Info, s);
                }
            }
            if self.error_message != prev_error {
                if let Some(s) = self.error_message.clone() {
                    self.log_message(MsgKind::Error, s.clone());
                    self.push_toast(ToastKind::Error, s);
                }
            }
            // Drop expired toasts so the screen clears even on idle ticks.
            let now = Instant::now();
            while self
                .toasts
                .front()
                .map(|t| now.duration_since(t.shown_at) > t.ttl())
                .unwrap_or(false)
            {
                self.toasts.pop_front();
            }
            // Drop pending-actions entries that completed > PENDING_COMPLETED_TTL ago.
            self.expire_pending();
            // Fire any pending dispatch whose cancel window has elapsed.
            // Cheap (a single Instant comparison when None); placed here
            // so the deadline is checked on every loop iteration, not
            // gated on user input.
            self.tick_pending_dispatch();
            // Pending embedded shell — allocate a PTY and switch mode.
            if let Some(target) = self.pending_shell_target.take() {
                self.open_embedded_shell(terminal, &target)?;
            }
            // Pending env-edit — shell out to `$EDITOR` against a
            // temp file holding the current env vars. Same
            // leave-altscreen / spawn / re-enter pattern as the
            // legacy inline-SSM path.
            if let Some((env_name, vars)) = self.pending_env_edit.take() {
                if let Err(e) = self.run_env_editor(terminal, &env_name, &vars) {
                    self.error_message = Some(format!("env-edit: {e}"));
                }
            }

            // Auto-close the shell pane when the subprocess has exited.
            if matches!(self.mode, Mode::Shell)
                && self.current_shell.as_ref().is_some_and(|s| s.is_dead())
            {
                self.close_shell_session();
            }
        }
        // persist_state ALSO runs in main.rs after `run()` returns
        // (Ok or Err) so a draw / select error mid-shutdown can't drop
        // the operator's state. This call here is kept so the Ok path
        // still persists *before* `leave_tui()` (cheap, idempotent).
        self.persist_state();
        Ok(())
    }

    /// Open an embedded SSM session into `instance_id`. Allocates a PTY,
    /// spawns `aws ssm start-session` inside it, and switches to
    /// `Mode::Shell` where keystrokes are forwarded to the subprocess
    /// instead of running ebman bindings. **F12** detaches back to the
    /// previous mode; the session keeps running and the user can re-open
    /// the pane (state preserved). The session ends when the subprocess
    /// exits — typically via the user typing `exit` or `^D`.
    fn open_embedded_shell(&mut self, terminal: &mut Tui, instance_id: &str) -> Result<()> {
        // Demo-mode short-circuit. The fixture's instance IDs are
        // synthetic, the AwsClient is a stub, and `aws ssm start-
        // session` would fail with "InstanceNotFound" (or hang
        // waiting for the session-manager-plugin handshake). Instead
        // spin up a fake `ShellSession` with a vt100::Parser
        // pre-loaded with canned content (session banner + a few
        // operator-realistic commands), and route into `Mode::Shell`
        // exactly like a real session. VHS captures show a real-
        // looking SSM pane; F12 detaches per the usual contract.
        if self.demo_mode {
            let size = terminal.size()?;
            let rows = size.height.saturating_sub(2).max(4);
            let cols = size.width.max(20);
            let content = crate::demo_fixture::canned_ssm_session(instance_id);
            let session =
                crate::shell::ShellSession::demo(instance_id.to_string(), &content, rows, cols);
            self.shell_return_mode = self.mode;
            self.current_shell = Some(Box::new(session));
            self.mode = Mode::Shell;
            return Ok(());
        }
        let region = self.context.region.clone();
        let profile = self
            .override_profile
            .clone()
            .or_else(|| self.context.profile.clone());
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            profile.as_deref(),
            &region,
            "SsmSession",
            instance_id,
            &[],
        );

        let size = terminal.size()?;
        // Reserve 2 rows for a thin status bar so the pane title + detach
        // hint are always visible.
        let rows = size.height.saturating_sub(2).max(4);
        let cols = size.width.max(20);

        let mut args = vec![
            "ssm",
            "start-session",
            "--target",
            instance_id,
            "--region",
            &region,
        ];
        let prof = profile.clone();
        if let Some(p) = prof.as_deref() {
            args.push("--profile");
            args.push(p);
        }
        match crate::shell::ShellSession::spawn(
            "aws",
            &args,
            rows,
            cols,
            format!("ssm: {instance_id}"),
        ) {
            Ok(session) => {
                self.current_shell = Some(Box::new(session));
                self.shell_return_mode = self.mode;
                self.mode = Mode::Shell;
                self.status_message = Some(format!(
                    "ssm session into {instance_id} — F12 detaches, ^D / exit closes"
                ));
            }
            Err(e) => {
                self.error_message = Some(format!(
                    "could not start SSM session ({e}). Install the AWS CLI + session-manager-plugin and check ssm:StartSession IAM"
                ));
            }
        }
        Ok(())
    }

    /// Forward a key event to the running shell's PTY. Called only when
    /// `Mode::Shell` is active. F12 is consumed locally as the detach key.
    pub fn handle_shell_key(&mut self, key: KeyEvent) {
        // F12 detaches without killing the subprocess. Demo sessions
        // (no real PTY behind them) also accept Esc as a detach — VHS
        // can't emit F12 reliably, and there's no subprocess to
        // forward bytes to anyway. Real sessions keep Esc forwarded
        // to the PTY because vim / less / many TUIs need it.
        let is_demo_session = self
            .current_shell
            .as_ref()
            .is_some_and(|s| s.writer.is_none());
        let detach = matches!(key.code, KeyCode::F(12))
            || (is_demo_session && matches!(key.code, KeyCode::Esc));
        if detach {
            self.mode = self.shell_return_mode;
            self.status_message = Some(
                "detached from shell — F12 reattaches, or open shell again from Instances tab"
                    .into(),
            );
            return;
        }
        if let Some(shell) = self.current_shell.as_mut() {
            if let Some(bytes) = crate::shell::key_event_to_bytes(&key) {
                let _ = shell.send(&bytes);
            }
        }
    }

    /// Tear down a finished shell session: the subprocess has exited, the
    /// reader thread returned. Surfaces a status message and routes the
    /// user back to where they came from.
    pub fn close_shell_session(&mut self) {
        if let Some(mut s) = self.current_shell.take() {
            s.kill();
            self.status_message = Some(format!("{} ended", s.label));
        }
        self.mode = self.shell_return_mode;
    }

    /// Open the operator's `$EDITOR` against a temp file holding
    /// the current env vars in `KEY=VALUE` form. On save, parses
    /// the file, diffs against `original`, and dispatches the
    /// deltas via `spawn_option_settings_update`. Cancel paths
    /// (unchanged file / missing file / editor non-zero exit)
    /// are no-ops with a clear status message.
    ///
    /// Drops out of the alt-screen for the editor (vim / nano /
    /// VS Code's `code --wait` etc. all need the terminal directly)
    /// and re-enters when the editor exits.
    fn run_env_editor(
        &mut self,
        terminal: &mut Tui,
        env_name: &str,
        original: &[(String, String)],
    ) -> Result<()> {
        use crossterm::{
            event::{DisableMouseCapture, EnableMouseCapture},
            execute,
            terminal::{
                disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
            },
        };

        let editor = std::env::var("VISUAL")
            .or_else(|_| std::env::var("EDITOR"))
            .unwrap_or_else(|_| "vi".to_string());

        // Temp file path. Use the OS temp dir + a fingerprint
        // built from the env name + epoch nanos so concurrent
        // sessions can't collide. Format suffix `.env` so editor
        // syntax-highlighters give the operator a useful default.
        let now_ns = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let safe = env_name
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("ebman-env-{safe}-{now_ns}.env"));

        let body = build_env_edit_body(env_name, original);
        // 0600: the body is the env's variables — secrets — sitting in
        // the shared temp dir for the whole $EDITOR session.
        crate::util::write_secure(&path, body.as_bytes()).wrap_err("writing env-edit temp file")?;

        // Leave the TUI for the editor.
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        let status = std::process::Command::new(&editor).arg(&path).status();

        // Always re-enter, regardless of editor outcome.
        enable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture
        )?;
        terminal.hide_cursor()?;
        terminal.clear()?;

        match status {
            Ok(s) if !s.success() => {
                self.error_message = Some(format!(
                    "$EDITOR ({editor}) exited {} — no changes dispatched",
                    s.code().unwrap_or(-1)
                ));
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
            Err(e) => {
                self.error_message = Some(format!(
                    "couldn't launch editor ({editor}): {e} — set $EDITOR / $VISUAL"
                ));
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
            _ => {}
        }

        let edited = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                self.error_message = Some(format!(
                    "couldn't re-read temp file at {} — no changes dispatched ({e})",
                    path.display()
                ));
                // Every other branch removes the (secrets-bearing)
                // temp file — this one must too.
                let _ = std::fs::remove_file(&path);
                return Ok(());
            }
        };
        let _ = std::fs::remove_file(&path);

        let edited_map = parse_env_edit_body(&edited);
        let original_map: std::collections::BTreeMap<String, String> = original
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let (to_set, to_remove) = diff_env_vars(
            "aws:elasticbeanstalk:application:environment",
            &original_map,
            &edited_map,
        );

        if to_set.is_empty() && to_remove.is_empty() {
            self.status_message = Some("env-edit: no changes — nothing dispatched".into());
            return Ok(());
        }

        let label = format!(
            "env-edit ({} set, {} removed)",
            to_set.len(),
            to_remove.len()
        );
        self.spawn_option_settings_update(label, to_set, to_remove);
        Ok(())
    }

    /// Set a status message that survives the next refresh tick. Use this
    /// for one-shot informational results the operator just asked for
    /// (e.g. `:pending` outcome, `:metric add` ack); plain
    /// `self.status_message = Some(...)` writes are still ephemeral and
    /// get auto-cleared by `apply_refresh`.
    pub fn pin_status(&mut self, msg: impl Into<String>) {
        self.status_message = Some(msg.into());
        self.status_message_pinned = true;
    }

    /// Error-message counterpart to `pin_status`. Sets
    /// `error_message` AND raises `status_message_pinned` so the
    /// next `apply_refresh` doesn't wipe it (the "no-snapshot"
    /// branch of the refresh clear path gates BOTH status and error
    /// behind the pinned flag). Used by paths that surface
    /// permanent-until-acknowledged conditions — e.g. dispatch_auto_rollback's
    /// "no pre-deploy snapshot" branch, which can fire from inside
    /// apply_refresh and would otherwise be cleared in the same tick.
    pub fn pin_error(&mut self, msg: impl Into<String>) {
        self.error_message = Some(msg.into());
        self.status_message_pinned = true;
    }

    fn push_toast(&mut self, kind: ToastKind, text: String) {
        // Dedupe: if an identical toast (same kind + text) is already on
        // screen, refresh its timestamp instead of stacking a duplicate.
        // Without this, a flurry of identical status updates (e.g. repeated
        // "no env selected" key presses, or a rebuilt-context message
        // arriving twice) would push the same card N times.
        if let Some(existing) = self
            .toasts
            .iter_mut()
            .find(|t| t.text == text && t.kind == kind)
        {
            existing.shown_at = Instant::now();
            return;
        }
        // Bucket-aware dedupe: status-diff toasts like "▲2 Red", "▲3 Red"
        // would otherwise stack as the deltas churn. Collapse to the latest
        // value when the new text shares the same delta-bucket key as an
        // existing toast.
        if let Some(new_key) = delta_toast_key(&text) {
            if let Some(existing) = self.toasts.iter_mut().find(|t| {
                t.kind == kind
                    && delta_toast_key(&t.text)
                        .map(|k| k == new_key)
                        .unwrap_or(false)
            }) {
                existing.text = text;
                existing.shown_at = Instant::now();
                return;
            }
        }
        while self.toasts.len() >= TOAST_CAP {
            self.toasts.pop_front();
        }
        self.toasts.push_back(Toast {
            text,
            kind,
            shown_at: Instant::now(),
        });
    }

    fn log_message(&mut self, kind: MsgKind, text: String) {
        if self.message_log.len() >= MESSAGE_LOG_CAP {
            self.message_log.pop_front();
        }
        self.message_log.push_back((chrono::Utc::now(), kind, text));
    }

    fn format_message_log(&self) -> String {
        let mut out = String::new();
        // Active-context header — useful when scanning recent messages
        // across an `:account` / `:profile` / `:region` switch so the
        // operator can see which account a given action targeted.
        // Audit log on disk (`~/.cache/ebman/audit.log`) carries the
        // full per-action `account=…` field; this header is the in-app
        // shorthand reminder.
        let account = self
            .context
            .account_id
            .as_deref()
            .map(|a| redact_for_log(a, self.redact))
            .unwrap_or_else(|| "—".into());
        let profile = self.context.profile.as_deref().unwrap_or("default");
        out.push_str(&format!(
            "context: account={account} · profile={profile} · region={}\n",
            self.context.region
        ));
        if self.message_log.is_empty() {
            out.push_str("─────────────────────────────────\n\n");
            out.push_str("no messages yet\n");
            return out;
        }
        out.push_str("recent messages (most recent last)\n");
        out.push_str("─────────────────────────────────\n\n");
        for (when, kind, text) in &self.message_log {
            let when = when.with_timezone(&chrono::Local).format("%H:%M:%S");
            let tag = match kind {
                MsgKind::Info => "INFO",
                MsgKind::Error => "ERR ",
            };
            out.push_str(&format!("{when}  {tag}  {text}\n"));
        }
        out
    }

    fn handle_event(&mut self, event: Event) {
        // First-run hint dismisses on any input. The renderer
        // checks the flag every frame, so this is enough to make
        // the footer line vanish on the operator's first real
        // interaction — typed key, mouse click, anything.
        if self.first_run_hint && matches!(event, Event::Key(_) | Event::Mouse(_) | Event::Paste(_))
        {
            self.first_run_hint = false;
        }
        match event {
            // Press AND Repeat — the latter fires when the user holds a
            // key (Backspace to delete a line, arrow to scroll). Repeat
            // events were previously dropped, which felt like "the key
            // isn't working" inside the embedded shell pane.
            Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                self.handle_key(key)
            }
            Event::Mouse(m) => self.handle_mouse(m),
            _ => {}
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        // Drag-to-resize on the events-panel divider. The divider is the top
        // row of the events area (one row above the panel body, conceptually).
        // We bracket the row with a 1-cell tolerance so clicks land easily.
        if self.event_panel.visible {
            if let Some(area) = self.event_panel.area {
                let divider_row = area.y;
                let in_drag = self.event_panel.drag_origin.is_some();
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left)
                        if (m.row as i32 - divider_row as i32).abs() <= 0 =>
                    {
                        self.event_panel.drag_origin = Some(self.event_panel.height);
                        return;
                    }
                    MouseEventKind::Drag(MouseButton::Left) if in_drag => {
                        // The mouse row is now where the divider should sit;
                        // events panel height = footer_bottom - mouse_row.
                        let footer_bottom = area.y.saturating_add(area.height).saturating_add(2);
                        let new_height = footer_bottom.saturating_sub(m.row);
                        self.event_panel.height = new_height.clamp(4, 30);
                        return;
                    }
                    MouseEventKind::Up(MouseButton::Left) if in_drag => {
                        self.event_panel.drag_origin = None;
                        return;
                    }
                    _ => {}
                }
            }
        }

        // Metrics-tab hover capture: in Detail mode, track the mouse column
        // when it's over the metrics body so the renderer can surface the
        // value at that point.
        if matches!(self.mode, Mode::Detail) {
            if let Some(d) = self.detail.as_mut() {
                if d.tab() == DetailTab::Metrics {
                    if let MouseEventKind::Moved = m.kind {
                        let in_body = d
                            .metrics_body_rect
                            .map(|r| {
                                m.column >= r.x
                                    && m.column < r.x.saturating_add(r.width)
                                    && m.row >= r.y
                                    && m.row < r.y.saturating_add(r.height)
                            })
                            .unwrap_or(false);
                        d.metrics_hover_col = if in_body { Some(m.column) } else { None };
                    }
                }
            }
            return;
        }

        // Mouse events steer the main table — wheel scroll moves selection,
        // left click selects a row, hover tints. None of those make sense
        // outside Normal mode: in Detail / Dlq / Action / Palette / QuickJump
        // the table is hidden, and a wheel scroll would silently change which
        // env you'd land on when you popped back out. Pickers / overlays /
        // command-mode are also handled by the keyboard.
        //
        // Apps scope shares the table area but uses a different selection
        // state; mouse routing for that is out of scope for now (movement
        // would land on env rows even when Apps is the active scope).
        let mouse_active = matches!(self.mode, Mode::Normal)
            && self.scope == Scope::Envs
            && self.current_overlay.is_none();
        if !mouse_active {
            self.hover_row = None;
            return;
        }
        match m.kind {
            MouseEventKind::ScrollDown => self.move_selection(1),
            MouseEventKind::ScrollUp => self.move_selection(-1),
            MouseEventKind::Down(MouseButton::Left) => self.select_row_at(m.column, m.row),
            MouseEventKind::Moved => self.update_hover(m.row),
            _ => {}
        }
    }

    fn update_hover(&mut self, row: u16) {
        let area = self.table_area;
        if area.width == 0 || area.height == 0 {
            self.hover_row = None;
            return;
        }
        let data_top = area.y.saturating_add(2);
        let data_bottom = area.y.saturating_add(area.height).saturating_sub(1);
        if row < data_top || row >= data_bottom {
            self.hover_row = None;
            return;
        }
        let offset = self.table_state.offset();
        let target = offset + (row - data_top) as usize;
        self.hover_row = Some(target);
    }

    fn select_row_at(&mut self, _col: u16, row: u16) {
        let area = self.table_area;
        if area.width == 0 || area.height == 0 {
            return;
        }
        // Table block: 1-row border on top, then 1-row header, then data rows.
        let data_top = area.y.saturating_add(2);
        let data_bottom = area.y.saturating_add(area.height).saturating_sub(1);
        if row < data_top || row >= data_bottom {
            return;
        }
        let rows = self.display_rows();
        if rows.is_empty() {
            return;
        }
        let offset = self.table_state.offset();
        let target = offset + (row - data_top) as usize;
        if target < rows.len() && matches!(rows[target], DisplayRow::Env(_)) {
            self.table_state.select(Some(target));
        }
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if matches!(key.code, KeyCode::Char('c')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }

        // Read-only popups overlay any mode and absorb all keys until dismissed.
        // Variant-specific extra dismiss keys (e.g. `D` re-toggles describe, `w`
        // re-toggles whatsnew) are honoured in addition to the universal Esc/q.
        // The SavedConfigsInteractive variant is its own mini-mode — j/k cursor
        // plus a/c/x dispatch — handled before the universal dismiss.
        // Mode::Picker short-circuits the overlay key handlers: when a
        // picker is open on top of an overlay (e.g. LogTail's group switcher
        // opened via Tab), the picker needs the keys, not the overlay.
        // Falls through to the `match self.mode` block below where
        // Mode::Picker has its own arm.
        if !matches!(self.mode, Mode::Picker) {
            if matches!(
                self.current_overlay.as_ref(),
                Some(Overlay::SavedConfigsInteractive { .. })
            ) {
                self.handle_saved_configs_interactive_key(key);
                return;
            }
            if matches!(self.current_overlay.as_ref(), Some(Overlay::LogTail { .. })) {
                self.handle_log_tail_key(key);
                return;
            }
            if matches!(
                self.current_overlay.as_ref(),
                Some(Overlay::EventTail { .. })
            ) {
                self.handle_event_tail_key(key);
                return;
            }
            if matches!(
                self.current_overlay.as_ref(),
                Some(Overlay::AppsActionMenu { .. })
            ) {
                self.handle_apps_action_menu_key(key);
                return;
            }
            if matches!(
                self.current_overlay.as_ref(),
                Some(Overlay::ReportBug { .. })
            ) {
                self.handle_report_bug_key(key);
                return;
            }
            // `:why` cursor navigation — handled before the generic overlay
            // close logic so j/k/↑/↓ in the overlay scroll its items
            // instead of being ignored. The cursor lives on the overlay;
            // `App.why_items` (written by the renderer) sets the bound.
            if let Some(Overlay::WhyRed { cursor, .. }) = self.current_overlay.as_mut() {
                let item_count = self.why_items.len();
                let moved = match key.code {
                    KeyCode::Char('j') | KeyCode::Down if item_count > 0 => {
                        *cursor = cursor.saturating_add(1).min(item_count - 1);
                        true
                    }
                    KeyCode::Char('k') | KeyCode::Up if *cursor > 0 => {
                        *cursor -= 1;
                        true
                    }
                    _ => false,
                };
                if moved {
                    return;
                }
            }
            // `:why` Enter drill — extract the action under an immutable
            // borrow, then release it before mutating the overlay/mode.
            if matches!(key.code, KeyCode::Enter) {
                let drill: Option<(WhyItem, String, Option<String>, Option<String>)> =
                    if let Some(Overlay::WhyRed {
                        cursor,
                        queues,
                        env_name,
                        ..
                    }) = self.current_overlay.as_ref()
                    {
                        self.why_items.get(*cursor).cloned().map(|item| {
                            let qs = queues.as_ref().and_then(|r| r.as_ref().ok());
                            (
                                item,
                                env_name.clone(),
                                qs.and_then(|q| q.main_url.clone()),
                                qs.and_then(|q| q.dlq_url.clone()),
                            )
                        })
                    } else {
                        None
                    };
                if let Some((item, env_name, main_url_opt, dlq_url_opt)) = drill {
                    match item {
                        WhyItem::Describe(text) => {
                            self.current_overlay = Some(Overlay::Describe(text));
                        }
                        WhyItem::OpenDlq => {
                            if let Some(dlq_url) = dlq_url_opt {
                                self.current_overlay = None;
                                self.open_dlq_from_why(
                                    env_name,
                                    main_url_opt.unwrap_or_default(),
                                    dlq_url,
                                );
                            }
                        }
                    }
                    return;
                }
            }
            if let Some(overlay) = self.current_overlay.as_ref() {
                // Drill-in actions transition out of the overlay into
                // another mode. Evaluated first so the overlay's q/esc
                // close semantics still apply on the fallback path.
                let drill_dlq: Option<(String, String, String)> = match overlay {
                    Overlay::WhyRed {
                        env_name,
                        tier,
                        queues,
                        ..
                    } if matches!(key.code, KeyCode::Char('d'))
                        && tier.eq_ignore_ascii_case("Worker") =>
                    {
                        queues
                            .as_ref()
                            .and_then(|r| r.as_ref().ok())
                            .and_then(|qs| {
                                qs.dlq_url.clone().map(|du| {
                                    (
                                        env_name.clone(),
                                        qs.main_url.clone().unwrap_or_default(),
                                        du,
                                    )
                                })
                            })
                    }
                    _ => None,
                };
                if let Some((env_name, main_url, dlq_url)) = drill_dlq {
                    self.current_overlay = None;
                    self.open_dlq_from_why(env_name, main_url, dlq_url);
                    return;
                }
                let universal = matches!(key.code, KeyCode::Esc | KeyCode::Char('q'));
                let variant_extra = match overlay {
                    Overlay::Describe(_) => {
                        matches!(key.code, KeyCode::Char('d') | KeyCode::Char('D'))
                    }
                    Overlay::Whatsnew(_) => matches!(key.code, KeyCode::Char('w')),
                    _ => false,
                };
                if universal || variant_extra {
                    self.current_overlay = None;
                }
                return;
            }
        }

        match self.mode {
            Mode::Filter => self.handle_filter_key(key),
            Mode::Help => self.handle_help_key(key),
            Mode::Command => self.handle_command_key(key),
            Mode::Shell => self.handle_shell_key(key),
            Mode::Palette => self.handle_palette_key(key),
            Mode::QuickJump => self.handle_quickjump_key(key),
            Mode::Picker => self.handle_picker_key(key),
            Mode::Detail => {
                // If a search is being typed (events or logs tab), capture keys there first.
                if self
                    .detail
                    .as_ref()
                    .is_some_and(|d| d.search_active || d.log_tail.search_active)
                {
                    self.handle_detail_search_key(key);
                    return;
                }
                // In-place Config-tab value editor intercepts ALL keys
                // while open — same pattern as the search input.
                if self
                    .detail
                    .as_ref()
                    .is_some_and(|d| d.config_edit.is_some())
                {
                    self.handle_config_edit_key(key);
                    return;
                }
                // Instance-terminate confirm intercepts ALL keys until resolved.
                if let Some(idx) = self
                    .detail
                    .as_ref()
                    .and_then(|d| d.instance_terminate_confirm)
                {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                            if let Some(d) = self.detail.as_mut() {
                                d.instance_terminate_confirm = None;
                            }
                            self.spawn_terminate_instance(idx);
                        }
                        _ => {
                            if let Some(d) = self.detail.as_mut() {
                                d.instance_terminate_confirm = None;
                            }
                            self.status_message = Some("terminate cancelled".into());
                        }
                    }
                    return;
                }
                // Config-row delete confirm intercepts ALL keys until resolved.
                if self
                    .detail
                    .as_ref()
                    .and_then(|d| d.config_delete_confirm)
                    .is_some()
                {
                    match key.code {
                        KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                            self.commit_config_delete();
                        }
                        _ => {
                            if let Some(d) = self.detail.as_mut() {
                                d.config_delete_confirm = None;
                            }
                            self.status_message = Some("delete cancelled".into());
                        }
                    }
                    return;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => {
                        self.detail = None;
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Tab | KeyCode::Char('l') => self.detail_cycle_tab(1),
                    KeyCode::BackTab | KeyCode::Char('h') => self.detail_cycle_tab(-1),
                    KeyCode::Char('j') | KeyCode::Down => self.detail_scroll(1),
                    KeyCode::Char('k') | KeyCode::Up => self.detail_scroll(-1),
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.detail_refresh_active_tab();
                    }
                    KeyCode::Char('R') => {
                        if let Some(d) = self.detail.as_mut() {
                            d.auto_refresh = !d.auto_refresh;
                            let msg = if d.auto_refresh {
                                "detail auto-refresh ON"
                            } else {
                                "detail auto-refresh off"
                            };
                            self.status_message = Some(msg.into());
                        }
                    }
                    KeyCode::Char('T') => {
                        self.cmd_event_time(&[]);
                    }
                    // Events-tab severity / time-window filters. Guarded
                    // to the Events tab so `L` / `w` stay free elsewhere.
                    KeyCode::Char('L')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Events)
                        ) =>
                    {
                        if let Some(d) = self.detail.as_mut() {
                            d.events_level = d.events_level.next();
                            d.events_scroll = 0;
                            let label = d.events_level.label();
                            self.status_message = Some(format!("events: severity ≥ {label}"));
                        }
                    }
                    KeyCode::Char('w')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Events)
                        ) =>
                    {
                        if let Some(d) = self.detail.as_mut() {
                            d.events_window = d.events_window.next();
                            d.events_scroll = 0;
                            let label = d.events_window.label();
                            self.status_message = Some(format!("events: window {label}"));
                        }
                    }
                    KeyCode::Char('?') => {
                        self.help.topic = HelpTopic::Detail;
                        self.help.pre_mode = Some(Mode::Detail);
                        self.mode = Mode::Help;
                    }
                    KeyCode::Char('a') => self.open_action_menu(),
                    // Guarded `b` on Instances tab opens the EC2 console for
                    // the selected instance; must come before the unguarded
                    // `b` (which opens the env console) per the match-arm
                    // order rule documented in CLAUDE.md.
                    KeyCode::Char('b')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Instances)
                        ) =>
                    {
                        self.open_instance_in_console();
                    }
                    KeyCode::Char('b') => self.open_in_console(),
                    KeyCode::Char('*') => self.toggle_pin_selected(),
                    KeyCode::Enter
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Health)
                        ) =>
                    {
                        self.drill_health_item();
                    }
                    KeyCode::Enter
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Queue)
                        ) =>
                    {
                        // On the Queue tab, Enter opens whichever queue the
                        // cursor is on. 0 = Main, 1 = DLQ.
                        let want_main = self
                            .detail
                            .as_ref()
                            .map(|d| d.queue_cursor == 0)
                            .unwrap_or(false);
                        if want_main {
                            self.open_queue_viewer(crate::app::QueueView::Main);
                        } else {
                            self.open_queue_viewer(crate::app::QueueView::Dlq);
                        }
                    }
                    KeyCode::Enter
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Instances)
                        ) =>
                    {
                        // Enter now opens an info overlay (non-intrusive).
                        // For the AWS EC2 console deeplink — which used to
                        // be Enter — use `b` from the Instances tab.
                        self.open_instance_info_overlay();
                    }
                    KeyCode::Char('i')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Instances)
                        ) =>
                    {
                        // `i` is an alias for Enter on the Instances tab —
                        // open the info overlay.
                        self.open_instance_info_overlay();
                    }
                    KeyCode::Enter
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Config)
                        ) =>
                    {
                        // On the Config tab, Enter opens the in-place
                        // value editor for the row under the cursor.
                        self.start_config_edit();
                    }
                    KeyCode::Char('n')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Config)
                        ) =>
                    {
                        // `n` on the Config tab — add a new row (tag or
                        // env var, kind taken from the cursor's section).
                        self.start_config_add();
                    }
                    KeyCode::Char('x')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Config)
                        ) =>
                    {
                        // `x` on the Config tab — arm delete of the row
                        // under the cursor (y confirms).
                        self.arm_config_delete();
                    }
                    KeyCode::Char('r')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Config)
                        ) =>
                    {
                        // `r` on the Config tab — rename the key of the
                        // row under the cursor.
                        self.start_config_rename();
                    }
                    KeyCode::Char('y')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Instances)
                        ) =>
                    {
                        self.yank_instance_id();
                    }
                    KeyCode::Char('s')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Instances)
                        ) =>
                    {
                        // Queue an SSM session into the selected instance.
                        // The run loop handles the TUI suspend/resume.
                        // An interactive shell is a write surface
                        // (docs/commands.md documents SSM as
                        // treat-as-write) — read-only / freeze / pins
                        // must block it like `:ssm-run`.
                        let target = self.detail.as_ref().and_then(|d| {
                            Some((
                                d.env_name.clone(),
                                d.instances.get(d.instances_cursor)?.id.clone(),
                            ))
                        });
                        if let Some((env_name, instance_id)) = target {
                            if !self.deny_write(&env_name, "ssm-session") {
                                self.pending_shell_target = Some(instance_id);
                            }
                        }
                    }
                    KeyCode::Char('s')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Logs)
                        ) =>
                    {
                        // Open the CW Logs streaming overlay over the
                        // existing snapshot view. spawn_logs_tail handles
                        // group discovery + auto-pick. The snapshot path
                        // stays untouched so esc returns to it.
                        if let Some(d) = self.detail.as_ref() {
                            let env_name = d.env_name.clone();
                            self.spawn_logs_tail(env_name, None);
                        }
                    }
                    KeyCode::Char('x')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Instances)
                        ) =>
                    {
                        // Start delete-confirm flow. Y/N resolved in the
                        // same handler the next time a key arrives.
                        if let Some(d) = self.detail.as_mut() {
                            if d.instances.get(d.instances_cursor).is_some() {
                                d.instance_terminate_confirm = Some(d.instances_cursor);
                            }
                        }
                    }
                    KeyCode::Char('d') => self.open_dlq(),
                    KeyCode::Char('D') => self.open_describe_overlay(),
                    KeyCode::Char(']')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Metrics)
                        ) =>
                    {
                        self.cycle_metrics_range(1);
                    }
                    KeyCode::Char('[')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Metrics)
                        ) =>
                    {
                        self.cycle_metrics_range(-1);
                    }
                    KeyCode::Char('/')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Events)
                        ) =>
                    {
                        if let Some(d) = self.detail.as_mut() {
                            d.search_active = true;
                            d.search_input.clear();
                            d.search_error = None;
                        }
                    }
                    KeyCode::Char('/')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Logs)
                        ) =>
                    {
                        if let Some(d) = self.detail.as_mut() {
                            d.log_tail.search_active = true;
                            d.log_tail.search_input.clear();
                            d.log_tail.search_error = None;
                        }
                    }
                    KeyCode::Char('n')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Events)
                        ) =>
                    {
                        self.detail_search_jump(1);
                    }
                    KeyCode::Char('N')
                        if matches!(
                            self.detail.as_ref().map(|d| d.tab()),
                            Some(DetailTab::Events)
                        ) =>
                    {
                        self.detail_search_jump(-1);
                    }
                    _ => {}
                }
            }
            Mode::Action => {
                if key.code == KeyCode::Char('?') {
                    self.help.topic = HelpTopic::Action;
                    self.help.pre_mode = Some(Mode::Action);
                    self.mode = Mode::Help;
                } else {
                    self.handle_action_key(key);
                }
            }
            Mode::Dlq => {
                if key.code == KeyCode::Char('?') {
                    self.help.topic = HelpTopic::Dlq;
                    self.help.pre_mode = Some(Mode::Dlq);
                    self.mode = Mode::Help;
                } else {
                    self.handle_dlq_key(key);
                }
            }
            Mode::Form => self.handle_form_key(key),
            Mode::Normal => {
                match key.code {
                    KeyCode::Char('q') => self.quit = true,
                    // `U` undoes a pending action dispatch during the
                    // 5s cancel window — last-ditch "oh god no" rescue
                    // after a Y / typed-name confirm. Uppercase so it
                    // can't be mistaken for a regular keystroke.
                    KeyCode::Char('U') if self.pending_dispatch.is_some() => {
                        self.cancel_pending_dispatch();
                    }
                    // Esc clears multi-select when active. Honours the
                    // "esc = clear" hint the multi-select status message
                    // advertises; previously a no-op (silent footgun).
                    KeyCode::Esc if !self.multi_selected.is_empty() => {
                        let n = self.multi_selected.len();
                        self.multi_selected.clear();
                        self.status_message = Some(format!("multi-select cleared ({n} env(s))"));
                    }
                    KeyCode::Esc if !self.apps_selected.is_empty() => {
                        let n = self.apps_selected.len();
                        self.apps_selected.clear();
                        self.status_message =
                            Some(format!("apps multi-select cleared ({n} app(s))"));
                    }
                    KeyCode::Tab => self.set_scope(self.scope.next()),
                    KeyCode::BackTab => self.set_scope(self.scope.prev()),
                    KeyCode::Enter if self.scope == Scope::Apps => self.drill_into_app(),
                    KeyCode::Enter => self.open_detail(),
                    KeyCode::Char('a') if self.scope == Scope::Apps => {
                        self.open_apps_action_menu();
                    }
                    KeyCode::Char('a') if self.scope == Scope::Envs => self.open_action_menu(),
                    KeyCode::Char('b') if self.scope == Scope::Apps => {
                        self.open_app_in_console();
                    }
                    KeyCode::F(5) => self.manual_refresh(),
                    KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.manual_refresh();
                    }
                    KeyCode::Char('x') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.redact = !self.redact;
                        self.status_message = Some(if self.redact {
                            "redact mode ON".into()
                        } else {
                            "redact mode off".into()
                        });
                    }
                    KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.grouped = !self.grouped;
                        self.rebuild_view();
                        self.status_message = Some(if self.grouped {
                            "grouped by application".into()
                        } else {
                            "ungrouped".into()
                        });
                    }
                    KeyCode::Char('e') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.event_panel.visible = !self.event_panel.visible;
                        if self.event_panel.visible {
                            self.event_panel.scroll = 0;
                            // events were fetched on each refresh; if we have none yet, prompt one.
                            if self.event_panel.events.is_empty() {
                                self.spawn_events();
                            }
                        }
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.view_mode = self.view_mode.next();
                        self.status_message = Some(format!("view: {}", self.view_mode.label()));
                    }
                    KeyCode::Up
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && self.event_panel.visible =>
                    {
                        self.event_panel.height = (self.event_panel.height + 1).min(30);
                    }
                    KeyCode::Down
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            && self.event_panel.visible =>
                    {
                        self.event_panel.height = self.event_panel.height.saturating_sub(1).max(4);
                    }
                    KeyCode::Char('s') => {
                        self.sort_key = self.sort_key.next();
                        self.resort_envs();
                        self.status_message = Some(format!(
                            "sort: {} ({})",
                            self.sort_key.label(),
                            if self.sort_desc { "desc" } else { "asc" }
                        ));
                    }
                    KeyCode::Char('S') => {
                        self.sort_desc = !self.sort_desc;
                        self.resort_envs();
                        self.status_message = Some(format!(
                            "sort: {} ({})",
                            self.sort_key.label(),
                            if self.sort_desc { "desc" } else { "asc" }
                        ));
                    }
                    KeyCode::Char('T') => {
                        self.cmd_event_time(&[]);
                    }
                    KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.export_tsv();
                    }
                    KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.yank_cli();
                    }
                    KeyCode::Char(']') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.focus = match self.focus {
                            Focus::Table => {
                                if self.event_panel.visible {
                                    Focus::Events
                                } else {
                                    Focus::Table
                                }
                            }
                            Focus::Events => Focus::Table,
                        };
                        if matches!(self.focus, Focus::Events) && self.event_panel.cursor.is_none()
                        {
                            self.event_panel.cursor = Some(0);
                        }
                        if matches!(self.focus, Focus::Table) {
                            self.event_panel.cursor = None;
                        }
                        self.status_message = Some(format!(
                            "focus: {}",
                            if matches!(self.focus, Focus::Table) {
                                "table"
                            } else {
                                "events"
                            }
                        ));
                    }
                    KeyCode::Char('[') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.focus = match self.focus {
                            Focus::Events => Focus::Table,
                            Focus::Table => {
                                if self.event_panel.visible {
                                    Focus::Events
                                } else {
                                    Focus::Table
                                }
                            }
                        };
                    }
                    // ] / [ on the main env table cycle through the
                    // saved-view chips above the table — a one-key flip
                    // instead of typing `:view NAME` each time. Placed
                    // AFTER the guarded Ctrl-]/Ctrl-[ arms (match-arm
                    // order — the compiler won't warn on shadowing).
                    // These lived unreachably inside the Detail-mode
                    // match until the 0.26 max-review; docs/keys.md
                    // documented them as a main-table binding all along.
                    KeyCode::Char(']') if !self.saved_views.is_empty() => {
                        self.cycle_saved_view(1);
                    }
                    KeyCode::Char('[') if !self.saved_views.is_empty() => {
                        self.cycle_saved_view(-1);
                    }
                    KeyCode::Char(' ') if self.scope == Scope::Envs => {
                        if let Some(env) = self.selected_env().cloned() {
                            if !self.multi_selected.remove(&env.name) {
                                self.multi_selected.insert(env.name);
                            }
                            let n = self.multi_selected.len();
                            self.status_message = if n == 0 {
                                Some("multi-select cleared".into())
                            } else {
                                Some(format!(
                                    "{n} env(s) selected (a = batch action, esc = clear)"
                                ))
                            };
                        }
                    }
                    KeyCode::Char(' ') if self.scope == Scope::Apps => {
                        // Apps-scope multi-select — toggles the
                        // selected app in/out of `apps_selected`.
                        // Selection is render-only today; future
                        // Apps-scope batch ops will fan across every
                        // env in every selected app.
                        if let Some(idx) = self.app_table_state.selected() {
                            if let Some(name) = self.applications.get(idx).map(|a| a.name.clone()) {
                                if !self.apps_selected.remove(&name) {
                                    self.apps_selected.insert(name);
                                }
                                let n = self.apps_selected.len();
                                self.status_message = if n == 0 {
                                    Some("apps multi-select cleared".into())
                                } else {
                                    Some(format!("{n} app(s) selected (esc = clear)"))
                                };
                            }
                        }
                    }
                    KeyCode::Char('y') => {
                        if let Some(i) = self.event_panel.cursor {
                            self.yank_event_at(i);
                        } else {
                            self.yank_selected(YankKind::Cname);
                        }
                    }
                    KeyCode::Char('Y') => self.yank_selected(YankKind::Name),
                    KeyCode::Char('J')
                        if self.event_panel.visible && !self.event_panel.events.is_empty() =>
                    {
                        let next = self
                            .event_panel
                            .cursor
                            .map(|c| (c + 1).min(self.event_panel.events.len().saturating_sub(1)))
                            .unwrap_or(0);
                        self.event_panel.cursor = Some(next);
                    }
                    KeyCode::Char('K')
                        if self.event_panel.visible && !self.event_panel.events.is_empty() =>
                    {
                        self.event_panel.cursor =
                            self.event_panel.cursor.and_then(|c| c.checked_sub(1));
                    }
                    KeyCode::Char('b') if self.scope == Scope::Envs => self.open_in_console(),
                    KeyCode::Char('D') if self.scope == Scope::Envs => self.open_describe_overlay(),
                    KeyCode::Char('*') if self.scope == Scope::Envs => self.toggle_pin_selected(),
                    KeyCode::Char('*') if self.scope == Scope::Apps => {
                        self.toggle_pin_selected_app()
                    }
                    KeyCode::Char('!') if self.scope == Scope::Envs => {
                        // Diagnostic shortcut — opens `:why` for the
                        // selected env. Works on any health (not just
                        // Red) so the operator can pull up the same
                        // four-section context any time, but the
                        // mnemonic targets the Red-row triage case.
                        if let Some(env) = self.selected_env() {
                            let env_name = env.name.clone();
                            let app_name = env.application.clone();
                            self.open_why_red(env_name, app_name);
                        } else {
                            self.error_message = Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
                        }
                    }
                    KeyCode::Char('f') if self.scope == Scope::Envs => {
                        self.frozen = !self.frozen;
                        self.status_message = Some(if self.frozen {
                            "frozen — auto-refresh paused".into()
                        } else {
                            "unfrozen".into()
                        });
                    }
                    KeyCode::Char(c @ '1'..='9') => self.quick_jump((c as u8 - b'0') as usize),
                    KeyCode::Char('?') => {
                        self.help.topic = HelpTopic::Global;
                        self.help.pre_mode = Some(Mode::Normal);
                        self.mode = Mode::Help;
                    }
                    KeyCode::Char(':') => {
                        self.command_input.clear();
                        self.mode = Mode::Command;
                    }
                    KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.open_palette();
                    }
                    KeyCode::Char('\'') => {
                        self.quickjump_input.clear();
                        self.mode = Mode::QuickJump;
                    }
                    KeyCode::Char('/') => {
                        // Clearing `filter` mutates view state, so the
                        // cached slices must be rebuilt — otherwise
                        // opening filter mode while a filter is already
                        // active leaves the old filtered subset on
                        // screen (stale) until the first keystroke.
                        self.filter.clear();
                        self.mode = Mode::Filter;
                        self.rebuild_view();
                    }
                    KeyCode::Char('p') => self.open_profile_picker(),
                    KeyCode::Char('r') => self.open_region_picker(),
                    KeyCode::Char('j') | KeyCode::Down => match self.focus {
                        Focus::Events if self.event_panel.visible => {
                            let next = self
                                .event_panel
                                .cursor
                                .map(|c| {
                                    (c + 1).min(self.event_panel.events.len().saturating_sub(1))
                                })
                                .unwrap_or(0);
                            self.event_panel.cursor = Some(next);
                        }
                        _ => self.move_scope_selection(1),
                    },
                    KeyCode::Char('k') | KeyCode::Up => match self.focus {
                        Focus::Events if self.event_panel.visible => {
                            self.event_panel.cursor =
                                self.event_panel.cursor.and_then(|c| c.checked_sub(1));
                        }
                        _ => self.move_scope_selection(-1),
                    },
                    KeyCode::Char('g') | KeyCode::Home => self.scope_select_first(),
                    KeyCode::Char('G') | KeyCode::End => self.scope_select_last(),
                    _ => {}
                }
            }
        }
    }

    /// Apply a `ControlOp` received over the control socket. Snapshot ops
    /// read the terminal's current back-buffer; key/command ops dispatch
    /// through the normal handlers so all existing bindings still apply.
    fn handle_control_op(&mut self, op: crate::control::ControlOp, _terminal: &mut Tui) {
        use crate::control::ControlOp;
        match op {
            ControlOp::Screen(reply) => {
                let text = self
                    .last_rendered_buffer
                    .as_ref()
                    .map(crate::control::render_buffer_as_text)
                    .unwrap_or_else(|| "(no frame rendered yet)".to_string());
                let _ = reply.send(text);
            }
            ControlOp::Key(ke) => {
                self.handle_event(Event::Key(ke));
            }
            ControlOp::Command(text) => {
                self.execute_command(&text);
            }
            ControlOp::Reload => {
                self.reload_requested = true;
                self.quit = true;
                self.status_message = Some("reloading (exec self)…".into());
            }
            ControlOp::State(reply) => {
                let selected = self
                    .selected_env()
                    .map(|e| e.name.clone())
                    .unwrap_or_default();
                let env_count = self.environments.len();
                let load = match self.load_state {
                    LoadState::Idle => "idle",
                    LoadState::Loading => "loading",
                    LoadState::Error => "error",
                };
                let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
                let json = format!(
                    "{{\"mode\":\"{:?}\",\"profile\":\"{}\",\"region\":\"{}\",\"account\":\"{}\",\"envs\":{},\"selected\":\"{}\",\"filter\":\"{}\",\"load\":\"{}\",\"sort\":\"{}\",\"grouped\":{},\"redact\":{},\"focus\":\"{:?}\"}}",
                    self.mode,
                    esc(self.context.profile.as_deref().unwrap_or("")),
                    esc(&self.context.region),
                    esc(self.context.account_id.as_deref().unwrap_or("")),
                    env_count,
                    esc(&selected),
                    esc(self.filter.text()),
                    load,
                    self.sort_key.label(),
                    self.grouped,
                    self.redact,
                    self.focus,
                );
                let _ = reply.send(json);
            }
        }
    }

    fn manual_refresh(&mut self) {
        self.spawn_refresh();
        self.status_message = Some("refresh requested".into());
    }

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

    /// Spawn a Cost Explorer fetch in the background. Result lands
    /// via `AppMsg::CostsFetched`; on success the costs map updates
    /// AND the cache file is rewritten. Idempotent — multiple
    /// fetches in flight overwrite each other harmlessly (last
    /// write wins; the tag-grouped result is stable across calls).
    /// Spawn a background AWS call off the UI thread.
    ///
    /// `op` runs against a cloned `AwsClient`; on failure its `eyre::Report`
    /// is flattened to a user-facing string tagged with `op_name`. The
    /// `Result<T, String>` plus the generation captured at spawn time are
    /// handed to `into_msg`, whose `AppMsg` is sent back to the event loop.
    /// This is the boilerplate every simple single-call `spawn_*` helper
    /// shares; multi-call fan-outs (`spawn_worker_queue_check`,
    /// `spawn_app_latest_versions`) still build their tasks directly.
    fn spawn_aws<T, Fut, Op, Build>(&self, op_name: &'static str, op: Op, into_msg: Build)
    where
        T: Send + 'static,
        Fut: std::future::Future<Output = Result<T, color_eyre::eyre::Report>> + Send + 'static,
        Op: FnOnce(Arc<AwsClient>) -> Fut + Send + 'static,
        Build: FnOnce(u64, Result<T, String>) -> AppMsg + Send + 'static,
    {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = op(aws).await.map_err(|e| flatten_err(op_name, e));
            let _ = tx.send(into_msg(gen, result));
        });
    }

    fn spawn_cost_fetch(&mut self) {
        let account = self.context.account_id.clone();
        let region = self.context.region.clone();
        self.spawn_aws(
            "fetch_env_costs",
            move |aws| async move { aws.fetch_env_costs().await },
            move |gen, result| AppMsg::CostsFetched {
                gen,
                account,
                region,
                result,
            },
        );
    }

    fn spawn_alarms_fetch(&mut self, env_name: String) {
        // The fetch's env name lives on the Overlay::Alarms variant so a late
        // result for a different env can be dropped at the handler. The body
        // is initially a placeholder until the result arrives.
        self.current_overlay = Some(Overlay::Alarms {
            env_name: env_name.clone(),
            body: format!("fetching alarms for {env_name}…"),
        });
        let name_for_msg = env_name.clone();
        self.spawn_aws(
            "list_alarms_for_env",
            move |aws| async move { aws.list_alarms_for_env(&env_name).await },
            move |gen, result| AppMsg::Alarms {
                gen,
                env_name: name_for_msg,
                result,
            },
        );
    }

    /// `:why` / `:diagnose` — open the unified diagnostic overlay for the
    /// given env. Installs an empty `Overlay::WhyRed` immediately so the
    /// user sees "fetching…" placeholders, then fans out four parallel
    /// fetchers (events, alarms, instances, deploys). Each lands as its
    /// own `AppMsg::WhyRed*` variant gated on `session_id`.
    fn open_why_red(&mut self, env_name: String, app_name: String) {
        self.why_red_session = self.why_red_session.wrapping_add(1);
        let session_id = self.why_red_session;
        // Tier captured up front so the renderer can hide the queue
        // section for Web envs without consulting `self.environments`
        // (which may have refreshed under us by the time the overlay
        // renders).
        let tier = self
            .environments
            .iter()
            .find(|e| e.name == env_name)
            .map(|e| e.tier.clone())
            .unwrap_or_default();
        let is_worker = tier.eq_ignore_ascii_case("Worker");
        self.current_overlay = Some(Overlay::WhyRed {
            env_name: env_name.clone(),
            tier,
            events: None,
            alarms: None,
            instances: None,
            deploys: None,
            // Web envs never get a queues entry — keep it None so the
            // renderer omits the section entirely. Worker envs start at
            // None and fill in via WhyRedQueues.
            queues: None,
            dlq_messages: None,
            session_id,
            cursor: 0,
        });
        self.spawn_why_red_events(env_name.clone(), session_id);
        self.spawn_why_red_alarms(env_name.clone(), session_id);
        self.spawn_why_red_instances(env_name.clone(), session_id);
        self.spawn_why_red_deploys(app_name.clone(), session_id);
        if is_worker {
            self.spawn_why_red_queues(app_name, env_name, session_id);
        }
    }

    // spawn_why_red_* cluster moved to src/app/spawn_why_red.rs in 0.21
    // (6 methods: queues, dlq_peek, events, alarms, instances, deploys).

    fn set_log_level(&mut self, level: &str) {
        // Treat a bare level as a directive applied to the root, but keep the
        // AWS/hyper crates capped at warn unless the user explicitly opts in.
        let directive = match level.to_lowercase().as_str() {
            "trace" | "debug" | "info" | "warn" | "error" => {
                format!("{level},aws=warn,hyper=warn")
            }
            other => other.to_string(),
        };
        let new_filter = match tracing_subscriber::EnvFilter::try_new(&directive) {
            Ok(f) => f,
            Err(e) => {
                self.error_message = Some(format!("invalid log directive '{level}': {e}"));
                return;
            }
        };
        let Some(handle) = self.log_reload.as_ref() else {
            self.error_message = Some("log reload handle missing".into());
            return;
        };
        match handle.modify(|f| *f = new_filter) {
            Ok(()) => {
                self.log_directive = directive.clone();
                self.status_message = Some(format!("log level → {directive}"));
            }
            Err(e) => self.error_message = Some(format!("log reload failed: {e}")),
        }
    }

    fn open_whatsnew(&mut self) {
        // Embedded changelog text. Keep this short — full release notes live in
        // git history / GitHub releases. Update on every release.
        self.current_overlay = Some(Overlay::Whatsnew(WHATSNEW.into()));
    }

    /// `:about` / `:credits` — author + license + repo info. Discoverable
    /// via the command palette but never pushed at the operator;
    /// existence justifies removing the splash byline if anyone ever
    /// objects to the 3-second introduction.
    /// `:report-bug` — build a scrubbed bug-report payload from
    /// current app state + ~/.cache/ebman/ebman.log tail + latest
    /// crash log (if any), and show it in the `Overlay::ReportBug`.
    /// Operator chooses `y` (copy to clipboard) or `b` (open
    /// GitHub issue in browser). See `report_bug` module for the
    /// scrubbing rules.
    pub(crate) fn open_report_bug_overlay(&mut self) {
        let cnames: std::collections::BTreeSet<String> = self
            .environments
            .iter()
            .filter(|e| !e.cname.is_empty())
            .map(|e| e.cname.clone())
            .collect();
        let env_names: std::collections::BTreeSet<String> =
            self.environments.iter().map(|e| e.name.clone()).collect();
        let app_names: std::collections::BTreeSet<String> =
            self.applications.iter().map(|a| a.name.clone()).collect();
        // message_log entries are (timestamp, kind, text) tuples;
        // pull the text + a single-char severity prefix so the
        // operator can see whether each line was a status or an
        // error without the structured tracing noise.
        let recent_messages: Vec<String> = self
            .message_log
            .iter()
            .rev()
            .take(10)
            .map(|(ts, kind, text)| {
                let sev = match kind {
                    MsgKind::Info => "[i]",
                    MsgKind::Error => "[!]",
                };
                let when = ts.format("%H:%M:%S");
                format!("{when}  {sev}  {text}")
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let icons = format!("{:?}", self.theme.icons).to_lowercase();
        let input = crate::report_bug::ReportInput {
            ebman_version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            os_release: std::env::consts::ARCH,
            icons: &icons,
            theme: self.theme.name,
            refresh_interval_secs: self.refresh_interval.as_secs(),
            recent_log_lines: crate::report_bug::tail_ebman_log(30),
            recent_messages,
            recent_crash: crate::report_bug::latest_crash_log(),
            env_count: self.environments.len(),
            app_count: self.applications.len(),
            multi_regions_count: self.multi_regions.len(),
            multi_account_enabled: !self.cfg.accounts.is_empty(),
        };
        let ctx = crate::report_bug::ScrubContext {
            account_id: self.context.account_id.clone(),
            profile: self.context.profile.clone(),
            region: Some(self.context.region.clone()),
            env_names,
            app_names,
            cnames,
        };
        let body = crate::report_bug::build_report(&input, &ctx);
        self.current_overlay = Some(Overlay::ReportBug { body });
    }

    /// Key handler for the `:report-bug` overlay. `y` copies the
    /// scrubbed payload to clipboard; `b` opens a pre-filled
    /// GitHub issue in the browser; `esc` / `q` closes. Same shape
    /// as the other interactive overlays.
    fn handle_report_bug_key(&mut self, key: KeyEvent) {
        let body = match self.current_overlay.as_ref() {
            Some(Overlay::ReportBug { body }) => body.clone(),
            _ => return,
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.current_overlay = None;
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                match yank(&body) {
                    Ok(()) => {
                        self.status_message = Some(format!(
                            "bug report copied to clipboard ({} chars) — paste at https://github.com/tombaldwin/ebman/issues/new",
                            body.chars().count()
                        ));
                    }
                    Err(e) => {
                        self.error_message = Some(format!("clipboard error: {e}"));
                    }
                }
                self.current_overlay = None;
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                let url = crate::report_bug::github_issue_url(
                    "tombaldwin/ebman",
                    "Bug report from ebman",
                    &body,
                );
                match open_url(&url) {
                    Ok(()) => {
                        self.status_message = Some("opened GitHub issue draft in browser".into());
                    }
                    Err(e) => {
                        self.error_message = Some(format!("couldn't open browser: {e}"));
                    }
                }
                self.current_overlay = None;
            }
            _ => {}
        }
    }

    /// `:rds` — fetch the env's RDS dbinstance option settings and
    /// Advance / rewind the command-mode completion cycle by
    /// `delta` (+1 = Tab, -1 = Shift-Tab). Captures the operator's
    /// typed prefix on the first Tab; subsequent Tabs cycle
    /// through matches without losing the original prefix (so
    /// they can pop out by typing).
    ///
    /// What gets completed depends on where the cursor is:
    /// - **No whitespace yet** → the command name (the whole input
    ///   is the name fragment).
    /// - **Whitespace, and the first token is an env-arg command**
    ///   (`:diff` / `:config-diff` / `:rds-detach`, see
    ///   [`command_takes_env_arg`]) → the *trailing* token as an
    ///   environment name, drawn from the loaded fleet. `:diff
    ///   ENV-A ENV-B` completes whichever env name is last.
    /// - **Whitespace, any other command** → the command-name
    ///   fragment is re-completed and args after the first space
    ///   pass through untouched. Means `:set-option aws` still
    ///   completes `set-option` if the operator Tabs at the start.
    fn command_completion_step(&mut self, delta: i32) {
        // First Tab of a cycle: snapshot what the operator had typed
        // so a subsequent reverse-Tab (or text input) can restore.
        // `first_step` also anchors the landing spot below so the very
        // first Tab lands on the *first* candidate (forward) / last
        // (backward), rather than immediately stepping past it.
        let first_step = self.completion.origin.is_none();
        if first_step {
            self.completion.origin = Some(self.command_input.text().to_string());
            self.completion.index = 0;
        }
        let origin = self.completion.origin.clone().unwrap_or_default();
        let ws = origin.find(char::is_whitespace);
        // Env-arg mode: a whitespace-bearing input whose first token
        // is one of the env-name-taking commands. Then we complete
        // the trailing token against the loaded env names instead of
        // the command list.
        let env_mode = ws
            .map(|i| command_takes_env_arg(&origin[..i]))
            .unwrap_or(false);
        // `head` is preserved verbatim before the candidate; `tail`
        // is appended after it. Command-name completion keeps the
        // arg tail (`rest`); env completion folds the whole prefix
        // (command + earlier args + the separating space) into
        // `head` and has no tail.
        let (head, candidates, tail): (String, Vec<String>, String) = match ws {
            None => (String::new(), completion_candidates(&origin), String::new()),
            Some(_) if env_mode => {
                let last_ws = origin
                    .rfind(char::is_whitespace)
                    .expect("origin has whitespace in this arm");
                // `rfind` gives the *first byte* of the last whitespace
                // char; step over the whole char so the split lands on a
                // char boundary (a multi-byte space like U+00A0 NBSP
                // otherwise slices mid-char and panics).
                let frag_start =
                    last_ws + origin[last_ws..].chars().next().map_or(1, char::len_utf8);
                let head = origin[..frag_start].to_string();
                let frag = origin[frag_start..].to_string();
                (head, self.env_name_candidates(&frag), String::new())
            }
            Some(i) => (
                String::new(),
                completion_candidates(&origin[..i]),
                origin[i..].to_string(),
            ),
        };
        if candidates.is_empty() {
            // Restore the operator's typed prefix and surface a
            // hint so the silent-no-op doesn't feel broken.
            self.command_input = origin.clone().into();
            self.status_message = Some(if env_mode {
                "no environment matches (Tab cycles env names)".to_string()
            } else {
                let prefix = ws.map(|i| &origin[..i]).unwrap_or(&origin[..]);
                format!("no command matches '{prefix}' (Tab cycles command names)")
            });
            return;
        }
        let n = candidates.len() as i32;
        let next = if first_step {
            // Land on the first (forward) / last (backward) match.
            if delta >= 0 {
                0
            } else {
                (n - 1) as usize
            }
        } else {
            let cur = self.completion.index as i32;
            (cur + delta).rem_euclid(n) as usize
        };
        self.completion.index = next;
        self.command_input = format!("{head}{}{tail}", candidates[next]).into();
        self.status_message = Some(format!(
            "completion {}/{} — Tab cycles, Esc cancels",
            next + 1,
            n
        ));
    }

    /// Environment names from the loaded fleet that start with
    /// `prefix`, sorted + deduped — the candidate list for
    /// command-bar argument completion (see
    /// [`Self::command_completion_step`]).
    fn env_name_candidates(&self, prefix: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .environments
            .iter()
            .map(|e| e.name.clone())
            .filter(|n| n.starts_with(prefix))
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// `:secrets [FILTER]` — list Secrets Manager secrets in the
    /// active region. Optional substring filter matches against
    /// secret name. Output: one section per secret with name +
    /// ARN + description + last-changed / last-rotated dates.
    /// Operator yanks the ARN to paste into `:env-edit` /
    /// `:env set ENV_VAR ARN` for downstream consumption.
    ///
    /// No secret *values* shown here — that's a separate explicit
    /// `:secret NAME` call so an accidentally-typed `:secrets`
    /// doesn't dump credentials to the screen.
    pub(crate) fn cmd_secrets(&mut self, rest: &[&str]) {
        let filter = rest.first().map(|s| s.to_string());
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let title_filter = filter.clone();
        self.status_message = Some(match filter.as_deref() {
            Some(f) => format!("listing secrets matching '{f}'…"),
            None => "listing secrets…".into(),
        });
        tokio::spawn(async move {
            let result = aws
                .list_secrets(filter.as_deref())
                .await
                .map_err(|e| flatten_err("list_secrets", e));
            let body = match result {
                Ok(rows) => render_secrets_overlay(&rows, title_filter.as_deref()),
                Err(e) => format!("secrets: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: "secrets".into(),
                body,
            });
        });
    }

    /// `:secret NAME` — fetch and reveal a single Secrets Manager
    /// secret's value. Requires an explicit name to make this an
    /// opt-in action (accidental `:secret` with no arg is an
    /// error, not a "dump every secret"). Audit-logs the read so
    /// the operator's CloudTrail-equivalent has a record.
    ///
    /// Output respects `app.redact` — when redact mode is on, the
    /// value is hashed instead of shown. The operator can flip
    /// `:redact off` first if they need to see it.
    pub(crate) fn cmd_secret_view(&mut self, rest: &[&str]) {
        let Some(name) = rest.first().map(|s| s.to_string()) else {
            self.error_message =
                Some("usage: :secret NAME  (NAME or full ARN; see :secrets to list)".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let redact = self.redact;
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "GetSecretValue",
            &name,
            &[],
        );
        // Captured for the completion audit line written from the task.
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        self.status_message = Some(format!("fetching secret '{name}'…"));
        tokio::spawn(async move {
            let result = aws
                .fetch_secret_value(&name)
                .await
                .map_err(|e| flatten_err("fetch_secret_value", e));
            // Audit the completion — `stage=completed`, matching the
            // dispatched/completed pairing of the write paths. (The
            // AWS-side CloudTrail event is the canonical record; this
            // is ebman's own breadcrumb.)
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                "GetSecretValue",
                &name,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[],
            );
            let body = match result {
                Ok(value) => render_secret_value_overlay(&name, &value, redact),
                Err(e) => format!("secret: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("secret — {name}"),
                body,
            });
        });
    }

    /// `:rollback` — redeploy the env's previously-deployed version.
    /// Fetches the env's recent events, scans them for the version
    /// label that was current before this one (see
    /// [`previous_version_label`]), and opens the standard deploy
    /// confirm modal for it — so the operator sees + confirms the
    /// target, and the 5s undo window still applies.
    pub(crate) fn cmd_rollback(&mut self, rest: &[&str]) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, "rollback") {
            return;
        }
        // `--auto-rollback Nm` arms the same watchdog as
        // `:deploy LABEL --auto-rollback`. Composes with `--to LABEL`
        // so the operator can dispatch "roll back to build-820,
        // auto-roll-forward to build-823 if Green doesn't land
        // within Nm". Same duration grammar (`parse_window_ms`).
        let auto_rollback_secs = parse_named_arg::<String>(rest, "--auto-rollback").and_then(|s| {
            let ms = crate::aws::parse_window_ms(&s)?;
            Some((ms / 1000) as u64)
        });
        if rest.contains(&"--auto-rollback") && auto_rollback_secs.is_none() {
            self.error_message =
                Some("--auto-rollback expects a duration like `5m` / `30m` / `1h`".into());
            return;
        }

        // `:rollback --to LABEL` — operator picked the target
        // themselves. Skip snapshot detection + event-scan and
        // route straight to the deploy confirm with the named
        // label. EB will reject an unknown label downstream
        // with a clear error, so no pre-validation is needed.
        if let Some(target) = parse_named_arg::<String>(rest, "--to") {
            if target.is_empty() {
                self.error_message = Some("--to expects a version label".into());
                return;
            }
            if target == env.version_label {
                self.error_message = Some(format!("{target} is already the deployed version"));
                return;
            }
            self.open_parameterised_action(
                Action::Deploy,
                ParameterisedAction {
                    deploy_version: Some(target.clone()),
                    auto_rollback_secs,
                    ..Default::default()
                },
            );
            self.status_message = Some(format!("rollback target: {target} (operator-specified)"));
            return;
        }

        let env_name = env.name.clone();
        let current_version = env.version_label.clone();
        // Prefer the captured pre-deploy snapshot if one exists —
        // more reliable than scanning events (which can hit the
        // 100-event window cap on chatty envs and miss the actual
        // previous version). The snapshot was taken right before
        // the deploy we'd be rolling back from, so it's exactly
        // what the operator means.
        if let Some(snapshot) = self.deploy_snapshots.get(&env_name).cloned() {
            if snapshot.previous_version_label != current_version {
                self.open_parameterised_action(
                    Action::Deploy,
                    ParameterisedAction {
                        deploy_version: Some(snapshot.previous_version_label.clone()),
                        auto_rollback_secs,
                        ..Default::default()
                    },
                );
                let age = (chrono::Utc::now() - snapshot.taken_at).num_seconds();
                self.status_message = Some(format!(
                    "rollback target: {} (from snapshot taken {}s ago)",
                    snapshot.previous_version_label, age
                ));
                return;
            }
        }
        // Fallback: scan the env's recent event history for the
        // most-recent version_label that differs from current. The
        // RollbackTarget message handler opens the confirm modal.
        // The event-scan path doesn't currently thread `auto_rollback_secs`
        // through `AppMsg::RollbackTarget` — surface a friendly
        // refusal when the operator asked for it but we had to fall
        // back to the scan, so they don't think their flag was
        // honoured silently.
        if auto_rollback_secs.is_some() {
            self.error_message = Some(format!(
                "--auto-rollback needs an in-memory snapshot for {env_name} — none captured. \
                 Try `:rollback --to LABEL --auto-rollback Nm` to name the target explicitly."
            ));
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!("rollback: finding {env_name}'s previous version…"));
        tokio::spawn(async move {
            let result = aws
                .list_events_for_env(&env_name, 100)
                .await
                .map_err(|e| flatten_err("list_events_for_env", e));
            let _ = tx.send(AppMsg::RollbackTarget {
                gen,
                env_name,
                current_version,
                result,
            });
        });
    }

    /// `:changes` — config-change timeline for the selected env: the
    /// deploy + configuration-update events from `DescribeEvents`,
    /// newest-first, with routine health/scaling noise filtered out.
    /// `:ssm-run "<shell-command>"` — fan a shell command out across
    /// the selected env's instances via SSM Run Command, poll the
    /// per-instance results, and land them in a TextOverlay. Sources
    /// the target list from cached `Detail.instances` (same as `:ssh`'s
    /// no-arg form) — if Detail isn't open with the Instances tab
    /// loaded, surfaces a clear error. The command runs as the SSM
    /// agent's default user (root on most EB AMIs); operators should
    /// treat this as a write operation and prefer read-only probes
    /// (e.g. `:ssm-run "uptime"`, `:ssm-run "ls /var/log"`) over
    /// state-mutating shells. Hard-capped at 60s wall-clock per
    /// command to keep the overlay from hanging on a stuck instance.
    pub(crate) fn cmd_ssm_run(&mut self, rest: &[&str]) {
        // The shell command is everything after `:ssm-run`. Rejoin
        // tokens with single spaces — the operator can quote-wrap to
        // preserve internal whitespace if needed. EB CLI's
        // `eb ssh -c '...'` uses the same shape.
        //
        // Dispatch flow: parse + resolve target instances + read-only
        // gate fire fast (before the modal opens), then route through
        // `open_parameterised_action` so the operator gets the
        // standard Y/N confirm with the command + fan-out count
        // before anything reaches the SDK. spawn_action short-
        // circuits to `spawn_ssm_run_impl` for the dispatch tail.
        if rest.is_empty() {
            self.error_message = Some(
                "usage: :ssm-run \"<shell-command>\"  (fans the command out across the env's instances; quotes preserve whitespace)".into(),
            );
            return;
        }
        let command_str = rest.join(" ");
        let trimmed = command_str
            .trim_matches(|c: char| c == '"' || c == '\'')
            .to_string();
        if trimmed.is_empty() {
            self.error_message = Some("empty command — nothing to run".into());
            return;
        }
        let instances: Vec<String> = self
            .detail
            .as_ref()
            .map(|d| d.instances.iter().map(|i| i.id.clone()).collect())
            .unwrap_or_default();
        if instances.is_empty() {
            self.error_message = Some(
                "no cached instances — open the env's Detail/Instances tab first so :ssm-run knows what to target".into(),
            );
            return;
        }
        // The env name comes from the Detail tab (which is what
        // sourced the instance list). Falling back to selected_env()
        // would mismatch if the operator changed selection between
        // opening Detail and running :ssm-run.
        let env_name = self
            .detail
            .as_ref()
            .map(|d| d.env_name.clone())
            .unwrap_or_default();
        // Resolve the env from the cached fleet so
        // `open_parameterised_action_on` has an `Environment` to work
        // with. `selected_env()` could be the wrong env if the cursor
        // moved after Detail was opened, so look up by name.
        let Some(env) = self
            .environments
            .iter()
            .find(|e| e.name == env_name)
            .cloned()
        else {
            self.error_message = Some(format!(
                "ssm-run: env '{env_name}' not in cached fleet — refresh (Ctrl-R) and retry"
            ));
            return;
        };
        // `open_parameterised_action_on` calls `deny_write` for us —
        // it gates the read-only / safety-pin / demo-mode locks.
        self.open_parameterised_action_on(
            env,
            Action::SsmRun,
            ParameterisedAction {
                ssm_run_command: Some(trimmed),
                ssm_run_instances: Some(instances),
                ..Default::default()
            },
        );
    }

    /// `:ssh [INSTANCE-ID]` — open an SSM Session Manager session into
    /// one of the selected env's instances. With an arg (`:ssh i-abc`)
    /// the target is taken verbatim; with no arg, a picker opens over
    /// `Detail.instances` if the operator has the env's Detail view
    /// open with the Instances tab loaded (otherwise a clear error
    /// points them at the missing precondition). Either path routes
    /// to the existing `pending_shell_target → open_embedded_shell`
    /// machinery — same TUI-suspend/resume + alt-screen dance as
    /// pressing `s` on Detail/Instances. Requires the AWS CLI +
    /// `session-manager-plugin` on PATH (the SDK can't substitute —
    /// SSM start-session uses a binary side-channel).
    pub(crate) fn cmd_ssh(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            Some(id) => {
                if !id.starts_with("i-") {
                    self.error_message =
                        Some(format!("expected an EC2 instance ID (`i-…`), got '{id}'"));
                    return;
                }
                // Interactive shell = write surface (same gate as
                // `:ssm-run`). The env for pin purposes is the open
                // Detail env when there is one; global read-only /
                // freeze / incident apply regardless.
                let gate_env = self
                    .detail
                    .as_ref()
                    .map(|d| d.env_name.clone())
                    .unwrap_or_default();
                if self.deny_write(&gate_env, "ssm-session") {
                    return;
                }
                // Log the dispatch. Both this (typed-command) path
                // and the Detail/Instances `s` keybind end up
                // shell-ing out to `aws ssm start-session`; the audit
                // line is the operator-facing breadcrumb.
                crate::audit::append_action_dispatched(
                    self.context.account_id.as_deref(),
                    self.context.profile.as_deref(),
                    &self.context.region,
                    "SsmSession",
                    id,
                    &[("via", "cmd_ssh")],
                );
                self.pending_shell_target = Some(id.to_string());
                self.status_message = Some(format!("opening SSM session to {id}…"));
            }
            None => {
                // Picker path. Source from Detail.instances rather than
                // spawning a fresh DescribeInstancesHealth — keeps the
                // command boundary-free of new async machinery, and the
                // operator's typical journey already passes through
                // Detail/Instances on the way to a session.
                let instances: Vec<String> = self
                    .detail
                    .as_ref()
                    .map(|d| d.instances.iter().map(|i| i.id.clone()).collect())
                    .unwrap_or_default();
                if instances.is_empty() {
                    self.error_message = Some(
                        "no cached instances — open the env's Detail/Instances tab first, or pass an ID (`:ssh i-abc`)".into(),
                    );
                    return;
                }
                self.picker = Some(Picker::new(PickerKind::SshInstance, instances, None));
                self.mode = Mode::Picker;
            }
        }
    }

    /// `:lineage` — chronological deploy timeline for the selected env.
    /// Where `:changes` mixes deploy events with config-change events,
    /// `:lineage` filters to deploys only (events that carry a
    /// `version_label`), collapses consecutive same-label events into
    /// one row, and shows the inter-deploy gap (`Δ`) plus deploy span
    /// (`took`). Answers "what was deployed at HH:MM" during incident
    /// review — the cut that's currently a manual scan through
    /// `:changes` mixed output.
    pub(crate) fn cmd_lineage(&mut self) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_name.clone())
        } else {
            self.selected_env().map(|e| e.name.clone())
        };
        let Some(env_name) = env_opt else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!("fetching deploy lineage for {env_name}…"));
        tokio::spawn(async move {
            let result = aws
                .list_events_for_env(&env_name, 100)
                .await
                .map_err(|e| flatten_err("list_events_for_env", e));
            let body = match result {
                Ok(events) => format_lineage(&env_name, &events),
                Err(e) => format!("lineage: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("lineage — {env_name}"),
                body,
            });
        });
    }

    pub(crate) fn cmd_changes(&mut self) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_name.clone())
        } else {
            self.selected_env().map(|e| e.name.clone())
        };
        let Some(env_name) = env_opt else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!("fetching change history for {env_name}…"));
        tokio::spawn(async move {
            let result = aws
                .list_events_for_env(&env_name, 100)
                .await
                .map_err(|e| flatten_err("list_events_for_env", e));
            let body = match result {
                Ok(events) => render_changes_overlay(&env_name, &events),
                Err(e) => format!("changes: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("changes — {env_name}"),
                body,
            });
        });
    }

    /// `:event-time [utc|local|age]` — set how event timestamps render
    /// in the Events panel + Detail/Events tab. No argument cycles
    /// `Utc → Local → Age`. Persists to state.toml. UTC is the
    /// default because it matches the EB / CloudWatch API output the
    /// operator cross-references against.
    pub(crate) fn cmd_event_time(&mut self, rest: &[&str]) {
        let next = match rest.first().copied() {
            None => self.event_panel.time_format.next(),
            Some(arg) => match EventTimeFormat::parse(arg) {
                Some(f) => f,
                None => {
                    self.error_message = Some(format!(
                        "unknown event-time format '{arg}'  (use: utc | local | age)"
                    ));
                    return;
                }
            },
        };
        self.event_panel.time_format = next;
        self.persist_state();
        self.status_message = Some(match next {
            EventTimeFormat::Utc => "event timestamps: UTC (YYYY-MM-DD HH:MM:SSZ)".into(),
            EventTimeFormat::Local => "event timestamps: local time".into(),
            EventTimeFormat::Age => "event timestamps: relative age".into(),
        });
    }

    /// `:env-edit` — bulk env-var editor via `$EDITOR`. Two-stage:
    ///
    ///   1. Async fetch of the env's current env vars
    ///      (`spawn_env_vars_for_edit`).
    ///   2. Main-loop tick takes the result + shells out to
    ///      `$EDITOR` against a temp file, parses the result on
    ///      save, dispatches the diff via `spawn_option_settings_update`.
    ///
    /// Closes the bulk-edit gap that single-key `:env set` /
    /// `:env unset` doesn't. Operator can add / remove / rename
    /// multiple env vars in one update — and saving an unchanged
    /// file is a clean no-op.
    pub(crate) fn cmd_env_edit(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, ":env-edit") {
            return;
        }
        if self.pending_env_edit.is_some() {
            self.error_message =
                Some("another :env-edit is mid-flight — wait for the editor to close".into());
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        let env_name_for_msg = env_name.clone();
        self.status_message = Some(format!("fetching env vars for {env_name}…"));
        tokio::spawn(async move {
            let result = aws
                .fetch_env_vars(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("fetch_env_vars", e));
            let _ = tx.send(AppMsg::EnvVarsForEdit {
                gen,
                env_name: env_name_for_msg,
                result,
            });
        });
    }

    /// `:explain` — diagnose an IAM `AccessDenied` by calling
    /// `iam:SimulatePrincipalPolicy` against the principal + action
    /// the failed request named. Surfaces the policy decision
    /// (allowed / explicitDeny / implicitDeny), the matched
    /// statements, SCP / permission-boundary blockers, and a
    /// concrete JSON snippet the operator can paste into a policy.
    ///
    /// Two shapes:
    ///   - `:explain` (no args) walks the most recent error message
    ///     looking for the standard AWS AccessDenied shape; uses
    ///     [`parse_access_denied`] to extract principal + action.
    ///   - `:explain ARN ACTION [ACTION ...]` evaluates explicit
    ///     pairs. Useful for pre-flight ("can this role rebuild
    ///     this env?") even when no error has happened yet.
    ///
    /// Caller needs `iam:SimulatePrincipalPolicy` on the target
    /// principal — common gap on assumed-role sessions. We surface
    /// that as a clear error rather than a silent no-op.
    pub(crate) fn cmd_explain(&mut self, rest: &[&str]) {
        // New in 0.14: `:explain EBL###` routes to the LLM-backed
        // explainer for lint issues. Backward-compatible with the
        // existing IAM AccessDenied flow because rule IDs don't
        // start with `arn:aws:` and don't take a second positional.
        if let Some(first) = rest.first().copied() {
            if first.starts_with("EBL") {
                self.cmd_explain_issue(first);
                return;
            }
        }
        let (principal, actions): (String, Vec<String>) = match rest.first().copied() {
            // Args form: ARN + 1..N action names.
            Some(arn) if arn.starts_with("arn:aws:") && rest.len() >= 2 => {
                let actions: Vec<String> = rest[1..].iter().map(|s| s.to_string()).collect();
                (arn.to_string(), actions)
            }
            Some(_) => {
                self.error_message = Some(
                    "usage: :explain (IAM AccessDenied) | :explain ARN ACTION [...] | :explain EBL###"
                        .into(),
                );
                return;
            }
            None => {
                // Walk message_log for the latest error containing
                // "is not authorized to perform" — that's the
                // AWS AccessDenied shape `parse_access_denied`
                // understands.
                let latest = self.message_log.iter().rev().find(|(_, kind, text)| {
                    matches!(kind, MsgKind::Error) && text.contains("is not authorized to perform")
                });
                let Some((_, _, text)) = latest else {
                    self.error_message = Some(
                        "no recent AccessDenied to explain — :explain ARN ACTION to evaluate explicitly".into(),
                    );
                    return;
                };
                match parse_access_denied(text) {
                    Some((arn, action)) => (arn, vec![action]),
                    None => {
                        self.error_message = Some(format!(
                            "couldn't parse principal + action from last error: {text}"
                        ));
                        return;
                    }
                }
            }
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let principal_for_title = principal.clone();
        self.status_message = Some(format!(
            "diagnosing IAM perms for {} action(s) on {principal}…",
            actions.len()
        ));
        tokio::spawn(async move {
            let result = aws
                .simulate_principal_policy(&principal, &actions, &[])
                .await
                .map_err(|e| flatten_err("simulate_principal_policy", e));
            let body = match result {
                Ok(rows) => render_explain_overlay(&principal, &rows),
                Err(e) => format!(
                    "explain: {e}\n\n\
                     This usually means the caller lacks `iam:SimulatePrincipalPolicy`\n\
                     on the target role — common with assumed-role sessions that don't\n\
                     have IAM perms. Try from a profile with IAM access.\n\n\
                     esc / q to close"
                ),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("explain — {principal_for_title}"),
                body,
            });
        });
    }

    /// `:explain EBL###` — LLM-backed explanation of a lint issue.
    /// Runs the lint engine against the currently-selected env,
    /// finds the matching issue, builds the standard explain prompt
    /// via [`crate::llm::build_prompt`], and dispatches to the
    /// configured Provider. Result lands in a TextOverlay (same
    /// surface as the IAM AccessDenied explainer).
    ///
    /// Opt-in via `[explain] enabled = true` in `config.toml` plus
    /// the env-var holding the provider API key. Without consent
    /// the overlay just says so with a config-file pointer.
    fn cmd_explain_issue(&mut self, issue_id: &str) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let mut disabled = self.cfg.lint_disable.clone();
        disabled.extend(crate::project::load_lint_disables_from_cwd());
        let app_name = env.application.clone();
        let env_name_for_fetch = env.name.clone();
        let settings = self.cfg.explain_settings.clone();
        // Snapshot the lint-context inputs that aren't already
        // implied by `&env` + `&opts`. All four 0.18 wire-ups land
        // here too so `:explain` sees the same rule firing pattern
        // as `:lint` (EBL008 newer-stack, EBL010 required-tags,
        // EBL011 worker DLQ, EBL012 healthy-count).
        let newer_stack_owned =
            crate::aws::newer_stack_version(&env.solution_stack, &self.latest_stacks);
        let required_tags_owned = self.cfg.required_tags.clone();
        let dlq_depth_owned = if env.tier.eq_ignore_ascii_case("Worker") {
            self.worker_dlq_depths.get(&env.name).copied()
        } else {
            None
        };
        let env_arn_owned = env.arn.clone();
        let issue_id_owned = issue_id.to_string();
        let issue_id_title = issue_id.to_string();
        self.status_message = Some(format!("explain: building prompt for {issue_id}…"));
        tokio::spawn(async move {
            // Parallel fetch — see spawn_confirm_lint for the rationale.
            let opts_fut = aws.fetch_env_option_settings(&app_name, &env_name_for_fetch);
            let tags_fut = async {
                match env_arn_owned.as_deref() {
                    Some(arn) => aws.list_tags(arn).await.ok(),
                    None => None,
                }
            };
            let health_fut = aws.fetch_env_instance_counts(&env_name_for_fetch);
            let (opts_res, tags_opt, health_res) = tokio::join!(opts_fut, tags_fut, health_fut);
            let env_tag_keys_owned: Vec<String> = tags_opt
                .unwrap_or_default()
                .into_iter()
                .map(|(k, _)| k)
                .collect();
            let healthy_count_owned = health_res.ok().map(|c| c.healthy as i64);
            let body = match opts_res {
                Ok(opts) => {
                    let mut ctx = crate::lint::LintContext::for_env(&env, &opts)
                        .with_required_tags(&required_tags_owned)
                        .with_env_tag_keys(&env_tag_keys_owned);
                    if let Some(newer) = newer_stack_owned.as_deref() {
                        ctx = ctx.with_newer_stack_available(newer);
                    }
                    if let Some(depth) = dlq_depth_owned {
                        ctx = ctx.with_dlq_depth(depth);
                    }
                    if let Some(count) = healthy_count_owned {
                        ctx = ctx.with_healthy_count(count);
                    }
                    let rules = crate::lint::default_rules(&disabled);
                    let issues = crate::lint::run_rules(&rules, &ctx);
                    match issues.iter().find(|i| i.rule_id == issue_id_owned) {
                        None => format!(
                            "explain: rule {issue_id_owned} doesn't fire on env {} — nothing to explain.\n\
                             Run :lint to see which issues do fire here.\n\nesc / q to close",
                            env.name
                        ),
                        Some(issue) => {
                            let prompt = crate::llm::build_prompt(issue);
                            // Cache first — operators running the
                            // same explain multiple times in a
                            // session don't burn API calls.
                            match crate::llm::read_cache(issue) {
                                Some(cached) => cached,
                                None => match crate::llm::dispatch(&settings, &prompt).await {
                                    Ok(r) => {
                                        crate::llm::write_cache(issue, &r);
                                        r
                                    }
                                    Err(e) => format!(
                                        "explain: {e}\n\n\
                                         Configure [explain] in {} or run from CLI with `ebman explain {issue_id_owned} --env {}`.\n\n\
                                         esc / q to close",
                                        crate::util::config_file("config.toml").display(),
                                        env.name,
                                    ),
                                },
                            }
                        }
                    }
                }
                Err(e) => format!("explain: fetch_env_option_settings: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("explain — {issue_id_title}"),
                body,
            });
        });
    }

    /// `:options [NAMESPACE]` — full settable-option vocabulary for
    /// the selected env's platform. Closes the biggest console-parity
    /// gap (config discoverability): the console has the canonical
    /// list of every settable EB option with metadata; ebman's
    /// `:set-option NAMESPACE NAME VALUE` requires the operator to
    /// already know the vocabulary.
    ///
    /// `:options` lists everything. `:options NAMESPACE` filters
    /// to one family (e.g. `:options aws:elbv2:listener`,
    /// `:options aws:autoscaling:asg`).
    pub(crate) fn cmd_options(&mut self, rest: &[&str]) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let filter_ns = rest.first().map(|s| s.to_string());
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        self.status_message = Some(format!(
            "fetching config vocabulary for {env_name}… (this can take a few seconds)"
        ));
        tokio::spawn(async move {
            let result = aws
                .fetch_env_configuration_options(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("fetch_env_configuration_options", e));
            let body = match result {
                Ok(rows) => render_options_overlay(&rows, filter_ns.as_deref(), &env_name),
                Err(e) => format!("options: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("options — {env_name}"),
                body,
            });
        });
    }

    /// `:config-diff-local [NAME]` — diff the deployed env's current
    /// option settings against a local EB CLI saved config (the YAML
    /// under `.elasticbeanstalk/saved_configs/<NAME>.cfg.yml`). With
    /// no arg, auto-picks the lone config if there's exactly one;
    /// with multiple, errors and lists names so the operator can
    /// pick. Bridges EB CLI users into ebman: answers "is what I
    /// committed still what's deployed" without rerunning
    /// `eb config get` and eyeballing the diff.
    pub(crate) fn cmd_config_diff_local(&mut self, rest: &[&str]) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let cwd = match std::env::current_dir() {
            Ok(p) => p,
            Err(e) => {
                self.error_message = Some(format!("can't read cwd: {e}"));
                return;
            }
        };
        let path = match rest.first().copied() {
            Some(name) => match crate::saved_config::resolve_saved_config(&cwd, name) {
                Ok(p) => p,
                Err(e) => {
                    self.error_message = Some(format!("config-diff-local: {e}"));
                    return;
                }
            },
            None => {
                let configs = match crate::saved_config::discover_saved_configs(&cwd) {
                    Ok(c) => c,
                    Err(e) => {
                        self.error_message = Some(format!("config-diff-local: {e}"));
                        return;
                    }
                };
                match configs.len() {
                    0 => {
                        self.error_message = Some(format!(
                            "no .elasticbeanstalk/saved_configs/*.cfg.yml under {}",
                            cwd.display()
                        ));
                        return;
                    }
                    1 => configs.into_iter().next().unwrap(),
                    _ => {
                        let names: Vec<String> = configs
                            .iter()
                            .map(|p| crate::saved_config::saved_config_name(p))
                            .collect();
                        self.error_message = Some(format!(
                            "multiple saved configs — pick one: :config-diff-local <{}>",
                            names.join(" | ")
                        ));
                        return;
                    }
                }
            }
        };
        let yaml = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                self.error_message = Some(format!("reading {}: {e}", path.display()));
                return;
            }
        };
        let local_opts = match crate::saved_config::parse_saved_config(&yaml) {
            Ok(o) => o,
            Err(e) => {
                self.error_message = Some(format!("parsing {}: {e}", path.display()));
                return;
            }
        };
        let local_name = crate::saved_config::saved_config_name(&path);
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let (app_name, env_name) = (env.application.clone(), env.name.clone());
        let env_name_for_title = env_name.clone();
        let local_name_for_title = local_name.clone();
        self.status_message = Some(format!(
            "comparing {env_name} ↔ saved config '{local_name}'…"
        ));
        tokio::spawn(async move {
            let result = aws
                .fetch_env_configuration_options(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("fetch_env_configuration_options", e));
            let body = match result {
                Ok(deployed) => {
                    let diffs = diff_config_options(&local_opts, &deployed);
                    let left_label = format!("local:{local_name}");
                    render_config_diff_overlay(&left_label, &env_name, &diffs)
                }
                Err(e) => format!("config-diff-local: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("config-diff-local — {env_name_for_title} ↔ {local_name_for_title}"),
                body,
            });
        });
    }

    /// `:diff ENV` / `:diff ENV-A ENV-B` — side-by-side env-metadata
    /// comparison (name/tier/status/health/platform/version/cname/
    /// updated). `--ignore-keys "k1,k2"` suppresses matching rows, the
    /// same flag shape `:config-diff` uses. Lives in its own helper (not
    /// inline in `execute_command`) so the `--ignore-keys` literal stays
    /// out of the dispatch-arm scanner's view — see
    /// `commands::tests::registry_covers_every_dispatch_arm`.
    pub(crate) fn cmd_diff(&mut self, rest: &[&str]) {
        // Parse out `--ignore-keys "k1,k2"`, leaving the positional env
        // name(s). Matches the metadata diff's field labels — `version`,
        // `updated`, `cname`, etc. — case-insensitively.
        let mut positionals: Vec<&str> = Vec::new();
        let mut ignore_csv: Option<String> = None;
        let mut iter = rest.iter().copied();
        let mut bad_arg: Option<String> = None;
        while let Some(arg) = iter.next() {
            match arg {
                "--ignore-keys" => {
                    let Some(value) = iter.next() else {
                        self.error_message = Some(
                            "--ignore-keys expects a comma-separated list (e.g. \"version,updated\")"
                                .into(),
                        );
                        return;
                    };
                    if value.starts_with("--") {
                        self.error_message = Some(format!(
                            "--ignore-keys expects a comma-separated list, got flag '{value}'"
                        ));
                        return;
                    }
                    ignore_csv = Some(value.to_string());
                }
                other if other.starts_with("--") => {
                    bad_arg = Some(other.to_string());
                    break;
                }
                other => positionals.push(other),
            }
        }
        if let Some(other) = bad_arg {
            self.error_message = Some(format!("unknown arg '{other}'"));
            return;
        }
        // Reject excess positionals rather than silently dropping them
        // (`:config-diff` does the same for its second positional).
        if positionals.len() > 2 {
            self.error_message = Some(
                "usage: :diff takes at most two env names (selected ↔ ENV, or ENV-A ENV-B)".into(),
            );
            return;
        }
        let ignore_keys: Vec<String> = parse_ignore_keys(ignore_csv.as_deref());
        match (positionals.first(), positionals.get(1)) {
            (None, _) => {
                self.error_message = Some(
                    "usage: :diff ENV  (selected ↔ ENV)  |  :diff ENV-A ENV-B  [--ignore-keys \"k1,k2\"]"
                        .into(),
                );
            }
            // Two-arg form: both envs named explicitly, so no implicit
            // selected-env side. Useful for picking env-A ↔ env-B from a
            // different scope than what's currently selected.
            (Some(a), Some(b)) => {
                if a == b {
                    self.error_message = Some("pick two different envs to compare".into());
                    return;
                }
                let Some(left) = self.environments.iter().find(|e| e.name == **a).cloned() else {
                    self.error_message = Some(format!("no env named '{a}' in current view"));
                    return;
                };
                let Some(right) = self.environments.iter().find(|e| e.name == **b).cloned() else {
                    self.error_message = Some(format!("no env named '{b}' in current view"));
                    return;
                };
                self.current_overlay = Some(Overlay::Diff(diff_envs(
                    &left,
                    &right,
                    self.redact,
                    &ignore_keys,
                )));
            }
            // Legacy single-arg form: selected (or detail-pane) env
            // compared against the named arg. Preserves the existing
            // behaviour every operator already knows.
            (Some(target), None) => {
                let left_opt = if let Some(d) = self.detail.as_ref() {
                    Some(d.env_snapshot.clone())
                } else {
                    self.selected_env().cloned()
                };
                let Some(left) = left_opt else {
                    self.error_message = Some(
                        "no env selected — press 1-9, click a row, or type ' to jump by name"
                            .into(),
                    );
                    return;
                };
                if left.name == **target {
                    self.error_message = Some("pick a different env to compare against".into());
                    return;
                }
                let right = self
                    .environments
                    .iter()
                    .find(|e| e.name == **target)
                    .cloned();
                match right {
                    None => {
                        self.error_message =
                            Some(format!("no env named '{target}' in current view"));
                    }
                    Some(right) => {
                        self.current_overlay = Some(Overlay::Diff(diff_envs(
                            &left,
                            &right,
                            self.redact,
                            &ignore_keys,
                        )));
                    }
                }
            }
        }
    }

    /// `:config-diff ENV` — compare the selected env's option-settings
    /// against `ENV`'s, showing every setting that differs. Answers
    /// "why does staging differ from prod?". Fetches both envs'
    /// configuration options in parallel and renders the diff.
    pub(crate) fn cmd_config_diff(&mut self, rest: &[&str]) {
        // Parse `:config-diff ENV [--ignore-keys "k1,k2,..."]`. Ignore-
        // keys is case-insensitive and matches the option `name` field
        // (e.g. "version_label", "EC2KeyName"). Operators can also use
        // `namespace:name` form for precise matches against a specific
        // namespace (e.g. "aws:autoscaling:asg:MinSize"). 0.19 addition.
        let mut env_name: Option<String> = None;
        let mut ignore_csv: Option<String> = None;
        let mut iter = rest.iter().copied();
        while let Some(arg) = iter.next() {
            match arg {
                "--ignore-keys" => {
                    let Some(value) = iter.next() else {
                        self.error_message = Some(
                            "--ignore-keys expects a comma-separated list (e.g. \"version_label,EC2KeyName\")"
                                .into(),
                        );
                        return;
                    };
                    // Guard against `:config-diff PROD --ignore-keys --json`
                    // consuming the next flag as the value (same pattern
                    // 0.17.0's --baseline parsing uses).
                    if value.starts_with("--") {
                        self.error_message = Some(format!(
                            "--ignore-keys expects a comma-separated list, got flag '{value}'"
                        ));
                        return;
                    }
                    ignore_csv = Some(value.to_string());
                }
                other if !other.starts_with("--") && env_name.is_none() => {
                    env_name = Some(other.to_string());
                }
                other => {
                    self.error_message = Some(format!("unknown arg '{other}'"));
                    return;
                }
            }
        }
        let Some(target) = env_name else {
            self.error_message = Some(
                "usage: :config-diff ENV [--ignore-keys \"k1,k2\"]  (compare the selected env's option-settings against ENV)"
                    .into(),
            );
            return;
        };
        let ignore_keys: Vec<String> = parse_ignore_keys(ignore_csv.as_deref());
        let left = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(left) = left else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let Some(right) = self.environments.iter().find(|e| e.name == target).cloned() else {
            self.error_message = Some(format!("no env named '{target}' in the current view"));
            return;
        };
        if left.name == right.name {
            self.error_message = Some("pick a different env to compare against".into());
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let (la, ln) = (left.application.clone(), left.name.clone());
        let (ra, rn) = (right.application.clone(), right.name.clone());
        self.status_message = Some(format!("comparing config: {ln} ↔ {rn}…"));
        tokio::spawn(async move {
            let body = match tokio::try_join!(
                aws.fetch_env_configuration_options(&la, &ln),
                aws.fetch_env_configuration_options(&ra, &rn),
            ) {
                Ok((lopts, ropts)) => {
                    let diffs = diff_config_options(&lopts, &ropts);
                    let filtered = filter_config_diffs(diffs, &ignore_keys);
                    render_config_diff_overlay(&ln, &rn, &filtered)
                }
                Err(e) => format!(
                    "config-diff: {}\n\nesc / q to close",
                    flatten_err("fetch_env_configuration_options", e)
                ),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("config diff — {ln} ↔ {rn}"),
                body,
            });
        });
    }

    /// `:rds` — fetch the env's RDS dbinstance option settings and
    /// render them. Visibility-only first cut: attach (via
    /// `UpdateEnvironment(aws:rds:dbinstance.*)`) and detach (the
    /// decouple-via-snapshot workflow) are follow-ups — both need
    /// careful operator confirmation flows and the detach path is
    /// genuinely destructive.
    ///
    /// Empty result = no RDS attached. We surface that as an
    /// explicit message rather than "no config" so the operator
    /// isn't left wondering whether the fetch failed silently.
    pub(crate) fn cmd_rds(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        self.status_message = Some(format!("fetching RDS config for {env_name}…"));
        tokio::spawn(async move {
            let result = aws
                .fetch_env_rds_config(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("fetch_env_rds_config", e));
            let body = match result {
                Ok(rows) if rows.is_empty() => "No RDS instance attached to this env.\n\n\
                     EB-managed RDS is configured via `aws:rds:dbinstance.*`\n\
                     option settings. To attach a new one:\n\n  \
                     :set-option aws:rds:dbinstance DBEngine postgres\n  \
                     :set-option aws:rds:dbinstance DBInstanceClass db.t3.micro\n  \
                     :set-option aws:rds:dbinstance DBPassword <secret>\n\n\
                     (See the EB docs — there are 10+ required fields. A\n\
                     dedicated `:rds-attach` form is a planned follow-up.)\n\n\
                     esc / q to close"
                    .to_string(),
                Ok(rows) => {
                    let mut body = String::from("RDS dbinstance configuration:\n\n");
                    for (opt, value) in &rows {
                        // Redact the password field even when the
                        // operator hasn't toggled global redact mode —
                        // surfacing a DB password into an overlay is a
                        // worse default than hiding it.
                        let safe_value = if opt.eq_ignore_ascii_case("DBPassword") {
                            "(redacted)".to_string()
                        } else {
                            value.clone()
                        };
                        body.push_str(&format!("  {opt:<28}  {safe_value}\n"));
                    }
                    body.push_str(
                        "\nUse `:set-option aws:rds:dbinstance <KEY> <VALUE>` to change a setting.\n\
                         Note: most RDS option changes trigger instance modification (downtime risk).\n\
                         esc / q to close",
                    );
                    body
                }
                Err(e) => format!("rds: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("rds — {env_name}"),
                body,
            });
        });
    }

    /// `:listeners` — fetch the env's ALB listener config (per-port:
    /// protocol, attached cert ARN, SSL policy, default rule) and
    /// render it as a text overlay. Web-tier only — Worker envs
    /// don't have an ALB. Edit support (cert rotation, listener
    /// add/remove) is a follow-up; the generic
    /// `:set-option aws:elbv2:listener:<PORT> KEY VAL` already
    /// works for one-off updates.
    pub(crate) fn cmd_listeners(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if env.tier.eq_ignore_ascii_case("Worker") {
            self.error_message = Some(format!(
                "env '{}' is Worker tier — no ALB to configure",
                env.name
            ));
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        self.status_message = Some(format!("fetching listeners for {env_name}…"));
        tokio::spawn(async move {
            let result = aws
                .fetch_env_listeners(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("fetch_env_listeners", e));
            let body = match result {
                Ok(rows) if rows.is_empty() => "No listener config found.\n\n\
                     The env may use a Classic ELB instead of an ALB, or no\n\
                     listener overrides have been set (EB uses account defaults).\n\
                     `:set-option aws:elbv2:listener:443 SSLCertificateArns ARN`\n\
                     to configure a listener from scratch.\n\nesc / q to close"
                    .to_string(),
                Ok(rows) => {
                    let mut body = String::from("Listener configuration:\n");
                    body.push_str("(one block per port; `default` = HTTP/80)\n\n");
                    let mut current_port: Option<String> = None;
                    for (port, opt, value) in &rows {
                        if current_port.as_deref() != Some(port.as_str()) {
                            if current_port.is_some() {
                                body.push('\n');
                            }
                            body.push_str(&format!("── aws:elbv2:listener:{port} ──\n"));
                            current_port = Some(port.clone());
                        }
                        body.push_str(&format!("  {opt:<32}  {value}\n"));
                    }
                    body.push_str(
                        "\n`:set-option aws:elbv2:listener:<PORT> <KEY> <VALUE>` to change a setting.\n\
                         esc / q to close",
                    );
                    body
                }
                Err(e) => format!("listeners: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("listeners — {env_name}"),
                body,
            });
        });
    }

    /// `:listener-edit PORT` — modal cert-rotation form for an ALB
    /// listener. Opens a single MultiSelect field whose options are the
    /// region's ISSUED ACM certificates (loaded async), pre-selected with
    /// the listener's current `SSLCertificateArns`. Submit writes the new
    /// cert set to `aws:elbv2:listener:<PORT>` via the option-settings
    /// path. `PORT` is `443` / a numeric port / `default` (HTTP/80).
    pub(crate) fn cmd_listener_edit(&mut self, rest: &[&str]) {
        use crate::form::{Form, FormField, FormSubmit};
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if env.tier.eq_ignore_ascii_case("Worker") {
            self.error_message = Some(format!(
                "env '{}' is Worker tier — no ALB to configure",
                env.name
            ));
            return;
        }
        let Some(port) = rest.first().copied() else {
            self.error_message = Some(
                "usage: :listener-edit PORT  (e.g. :listener-edit 443; `default` = HTTP/80)".into(),
            );
            return;
        };
        let port = port.to_string();
        let ns = format!("aws:elbv2:listener:{port}");
        let placeholder = FormField::multi_select(
            "cert",
            "SSL certificate(s)",
            Vec::new(),
            Vec::new(),
            Some::<String>("space toggle · ↑↓ option cursor · loaded from ACM".into()),
        );
        let form = Form::loading(
            format!("listener {port} — {}", env.name),
            env.name.clone(),
            format!("listener {port} cert update"),
            vec![placeholder],
            FormSubmit::OptionSettings {
                mappings: vec![("cert".into(), ns, "SSLCertificateArns".into())],
            },
        );
        // Bypass open_form's DescribeConfigurationSettings pre-fill (it
        // wouldn't load ACM inventory) — stash the form and spawn the
        // cert-specific loader, mirroring the subnet / SG pickers.
        self.form = Some(form);
        self.mode = Mode::Form;
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env.name.clone();
        let app_name = env.application.clone();
        tokio::spawn(async move {
            let result = load_listener_certs(aws, &app_name, &env_for_msg, &port).await;
            let _ = tx.send(AppMsg::FormMultiSelectLoaded {
                gen,
                env_name: env_for_msg,
                field_key: "cert".to_string(),
                result,
            });
        });
    }

    /// `:apps-info` — surface application metadata that doesn't fit
    /// in the apps-table columns: full description, creation date,
    /// last-updated date, saved-config templates, env count.
    /// Resolves the target via cursor position in either scope:
    /// Apps scope uses `app_table_state`; Envs scope walks
    /// `selected_env().application`.
    pub(crate) fn open_apps_info_overlay(&mut self) {
        let app_name_opt = match self.scope {
            Scope::Apps => self
                .app_table_state
                .selected()
                .and_then(|i| self.applications.get(i).map(|a| a.name.clone())),
            Scope::Envs => self.selected_env().map(|e| e.application.clone()),
        };
        let Some(app_name) = app_name_opt else {
            self.error_message = Some("no application selected".into());
            return;
        };
        let Some(app) = self.applications.iter().find(|a| a.name == app_name) else {
            self.error_message = Some(format!(
                "application '{app_name}' not in cache yet — refresh and retry"
            ));
            return;
        };
        // Walk env list for the rollup figures; mirrors the apps-table
        // columns so the operator can compare without bouncing.
        let rollup = app_rollup(&self.environments, &app.name, &self.worker_dlq_depths);
        let env_names: Vec<&str> = self
            .environments
            .iter()
            .filter(|e| e.application == app.name)
            .map(|e| e.name.as_str())
            .collect();
        let date_fmt = |dt: Option<chrono::DateTime<chrono::Utc>>| -> String {
            dt.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "—".into())
        };
        let templates_block = if app.templates.is_empty() {
            "  (none)".to_string()
        } else {
            app.templates
                .iter()
                .map(|t| format!("  ▸ {t}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let envs_block = if env_names.is_empty() {
            "  (none)".to_string()
        } else {
            env_names
                .iter()
                .map(|n| format!("  ▸ {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let description = if app.description.is_empty() {
            "(no description)".to_string()
        } else {
            app.description.clone()
        };
        let latest_line = match (
            app.latest_version_label.as_deref(),
            app.latest_version_created,
        ) {
            (Some(label), Some(created)) => format!("{label}  ({})", date_fmt(Some(created))),
            (Some(label), None) => label.to_string(),
            _ => "—".into(),
        };
        let body = format!(
            "Application: {}\n\
             Description: {description}\n\n\
             Created:     {created}\n\
             Updated:     {updated}\n\n\
             Versions:    {version_count} registered · latest: {latest_line}\n\
             Envs:        {env_count} total · {red_count} alerting · {updating_count} updating\n\n\
             Environments:\n{envs_block}\n\n\
             Saved configuration templates:\n{templates_block}\n\n\
             esc / q to close",
            app.name,
            created = date_fmt(app.date_created),
            updated = date_fmt(app.date_updated),
            version_count = app.version_count,
            env_count = rollup.env_count,
            red_count = rollup.red_count + rollup.worker_dlq_alerts,
            updating_count = rollup.updating_count,
        );
        self.current_overlay = Some(Overlay::TextDump {
            title: format!("info — {}", app.name),
            body,
        });
    }

    fn open_about_overlay(&mut self) {
        // The card content is built by `draw_about`; the overlay just
        // carries the open time so the giant scene can animate.
        self.current_overlay = Some(Overlay::About(Instant::now()));
    }

    fn toggle_pin_selected(&mut self) {
        let name_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_name.clone())
        } else {
            self.selected_env().map(|e| e.name.clone())
        };
        let Some(name) = name_opt else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.pinned.remove(&name) {
            self.status_message = Some(format!("unpinned {name}"));
        } else {
            self.pinned.insert(name.clone());
            self.status_message = Some(format!("pinned {name}"));
        }
        self.resort_envs();
        self.persist_state();
    }

    /// Apps-scope counterpart to `toggle_pin_selected`. Pins / unpins
    /// the application under the apps-table cursor. Pinned apps sort
    /// to the top of the Apps table regardless of the sort key (the
    /// `applications` Vec gets re-sorted on every refresh; see
    /// `resort_applications`).
    fn toggle_pin_selected_app(&mut self) {
        let Some(idx) = self.app_table_state.selected() else {
            self.status_message = Some("no app selected".into());
            return;
        };
        let Some(name) = self.applications.get(idx).map(|a| a.name.clone()) else {
            return;
        };
        if self.pinned_apps.remove(&name) {
            self.status_message = Some(format!("unpinned app {name}"));
        } else {
            self.pinned_apps.insert(name.clone());
            self.status_message = Some(format!("pinned app {name}"));
        }
        self.resort_applications();
        self.persist_state();
    }

    /// Sort `self.applications` so pinned apps float to the top.
    /// Within each pinned / unpinned bucket, alphabetical by name to
    /// keep ordering stable.
    fn resort_applications(&mut self) {
        let pinned = self.pinned_apps.clone();
        self.applications.sort_by(|a, b| {
            let a_pin = pinned.contains(&a.name);
            let b_pin = pinned.contains(&b.name);
            if a_pin != b_pin {
                return if a_pin {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            a.name.cmp(&b.name)
        });
    }

    fn yank_cli(&mut self) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(env) = env_opt else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let cmd = build_describe_cli(
            &env.name,
            &self.context.region,
            self.override_profile
                .as_deref()
                .or(self.context.profile.as_deref()),
        );
        match yank(&cmd) {
            Ok(()) => {
                self.status_message = Some("equivalent AWS CLI command copied".into());
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    fn export_json(&mut self) {
        let count = self.cached_filtered.len();
        let mut out = String::from("[\n");
        for (idx, &i) in self.cached_filtered.iter().enumerate() {
            let e = &self.environments[i];
            let cname = if self.redact {
                redact_block(&e.cname)
            } else {
                e.cname.clone()
            };
            let updated = e
                .updated
                .map(|u| format!("\"{}\"", u.to_rfc3339()))
                .unwrap_or_else(|| "null".into());
            out.push_str(&format!(
                "  {{\"name\":\"{}\",\"application\":\"{}\",\"tier\":\"{}\",\"status\":\"{}\",\"health\":\"{}\",\"platform\":\"{}\",\"version\":\"{}\",\"cname\":\"{}\",\"updated\":{}}}",
                json_escape(&e.name),
                json_escape(&e.application),
                json_escape(&e.tier),
                json_escape(&e.status),
                json_escape(&e.health),
                json_escape(&e.platform),
                json_escape(&e.version_label),
                json_escape(&cname),
                updated,
            ));
            if idx + 1 < count {
                out.push(',');
            }
            out.push('\n');
        }
        out.push(']');
        match yank(&out) {
            Ok(()) => {
                self.status_message = Some(format!("exported {count} rows (JSON) to clipboard"));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    fn export_markdown(&mut self) {
        let count = self.cached_filtered.len();
        let mut out = String::new();
        out.push_str("| NAME | APPLICATION | TIER | STATUS | HEALTH | PLATFORM | VERSION | CNAME | UPDATED |\n");
        out.push_str("| ---- | ----------- | ---- | ------ | ------ | -------- | ------- | ----- | ------- |\n");
        for &i in &self.cached_filtered {
            let e = &self.environments[i];
            let cname = if self.redact {
                redact_block(&e.cname)
            } else {
                e.cname.clone()
            };
            let updated = e.updated.map(|u| u.to_rfc3339()).unwrap_or_default();
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                md_escape(&e.name),
                md_escape(&e.application),
                e.tier,
                e.status,
                e.health,
                md_escape(&e.platform),
                md_escape(&e.version_label),
                md_escape(&cname),
                updated,
            ));
        }
        match yank(&out) {
            Ok(()) => {
                self.status_message =
                    Some(format!("exported {count} rows (Markdown) to clipboard"));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    fn open_describe_overlay(&mut self) {
        let env = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(env) = env else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        self.current_overlay = Some(Overlay::Describe(describe_env(&env)));
    }

    fn open_in_console(&mut self) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(env) = env_opt else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let url = console_url(&self.context.region, &env.application, &env.name);
        match open_url(&url) {
            Ok(()) => {
                self.status_message = Some(format!("opened {} in browser", env.name));
            }
            Err(e) => {
                self.error_message = Some(format!("couldn't open browser: {e}"));
            }
        }
    }

    fn open_palette(&mut self) {
        self.palette_input.clear();
        self.palette_items = build_palette_items(self);
        self.palette_refilter();
        self.mode = Mode::Palette;
    }

    fn palette_refilter(&mut self) {
        let needle = self.palette_input.text().to_lowercase();
        let mut scored: Vec<(usize, isize)> = self
            .palette_items
            .iter()
            .enumerate()
            .filter_map(|(i, it)| {
                let s = palette_score(&needle, &it.label, &it.detail)?;
                Some((i, s))
            })
            .collect();
        scored.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
        self.palette_filtered = scored.into_iter().map(|(i, _)| i).collect();
        self.palette_state
            .select(if self.palette_filtered.is_empty() {
                None
            } else {
                Some(0)
            });
    }

    fn palette_move(&mut self, delta: i32) {
        let n = self.palette_filtered.len();
        if n == 0 {
            self.palette_state.select(None);
            return;
        }
        let cur = self.palette_state.selected().unwrap_or(0) as i32;
        let next = (cur + delta).rem_euclid(n as i32) as usize;
        self.palette_state.select(Some(next));
    }

    fn palette_execute(&mut self) {
        let Some(pos) = self.palette_state.selected() else {
            return;
        };
        let Some(&idx) = self.palette_filtered.get(pos) else {
            return;
        };
        let Some(item) = self.palette_items.get(idx).cloned() else {
            return;
        };
        self.mode = Mode::Normal;
        self.palette_input.clear();
        match item.action {
            PaletteAction::RunCommand(cmd) => self.execute_command(&cmd),
            PaletteAction::PrefillCommand(prefix) => {
                self.command_input = prefix.into();
                self.mode = Mode::Command;
            }
            PaletteAction::JumpEnv(name) => {
                if let Some(pos) = self.cached_display.iter().position(|r| match r {
                    DisplayRow::Env(i) => self.environments[*i].name == name,
                    DisplayRow::Separator => false,
                }) {
                    self.table_state.select(Some(pos));
                    self.status_message = Some(format!("jumped to {name}"));
                }
            }
            PaletteAction::LoadView(name) => {
                self.execute_command(&format!("view {name}"));
            }
        }
    }

    fn quickjump_apply(&mut self) {
        if self.quickjump_input.is_empty() {
            return;
        }
        let needle = self.quickjump_input.text().to_lowercase();
        for (pos, row) in self.cached_display.iter().enumerate() {
            if let DisplayRow::Env(i) = row {
                let e = &self.environments[*i];
                let alias = self
                    .aliases
                    .get(&e.name)
                    .map(|a| a.to_lowercase())
                    .unwrap_or_default();
                if e.name.to_lowercase().starts_with(&needle) || alias.starts_with(&needle) {
                    self.table_state.select(Some(pos));
                    return;
                }
            }
        }
    }

    fn quick_jump(&mut self, n: usize) {
        // 1..=9 maps to position n-1 in the visible env rows.
        let Some(target_env) = self
            .cached_display
            .iter()
            .filter(|r| matches!(r, DisplayRow::Env(_)))
            .nth(n.saturating_sub(1))
        else {
            return;
        };
        if let Some(pos) = self
            .cached_display
            .iter()
            .position(|r| std::ptr::eq(r, target_env))
        {
            self.table_state.select(Some(pos));
        }
    }

    fn open_detail(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let mut tabs = vec![
            DetailTab::Health,
            DetailTab::Events,
            DetailTab::Instances,
            DetailTab::Metrics,
        ];
        if env.tier == "Worker" {
            tabs.push(DetailTab::Queue);
        }
        tabs.push(DetailTab::Logs);
        tabs.push(DetailTab::Config);
        let detail = DetailState {
            env_name: env.name.clone(),
            env_snapshot: env,
            tabs,
            tab_idx: 0,
            events: Vec::new(),
            instances: Vec::new(),
            queues: WorkerQueues::default(),
            metrics: Vec::new(),
            metrics_range_secs: 3600, // 1h default
            auto_refresh: false,
            search_input: TextInput::new(),
            search_active: false,
            search_pattern: None,
            search_error: None,
            events_scroll: 0,
            events_max_scroll: 0,
            events_level: EventLevel::default(),
            events_window: EventWindow::default(),
            instances_scroll: 0,
            tags: Vec::new(),
            env_vars: Vec::new(),
            cw_log_groups: None,
            loading_events: false,
            loading_instances: false,
            loading_queues: false,
            loading_metrics: false,
            loading_tags: false,
            loading_env_vars: false,
            error: None,
            log_tail: LogTail::default(),
            queue_cursor: 0,
            instances_cursor: 0,
            instance_terminate_confirm: None,
            health_cursor: 0,
            metrics_hover_col: None,
            metrics_body_rect: None,
            cw_alarms: None,
            loading_cw_alarms: false,
            recent_versions: None,
            loading_recent_versions: false,
            config_cursor: 0,
            config_edit: None,
            config_scroll: 0,
            config_delete_confirm: None,
        };
        self.detail = Some(detail);
        self.mode = Mode::Detail;
        self.detail_refresh_active_tab();
        // Tags & instances load eagerly so the Config tab (tags + cost
        // annotation) is populated without the user having to switch tabs.
        self.spawn_detail_tags();
        self.spawn_detail_env_vars();
        self.spawn_detail_log_groups();
        if let Some(d) = self.detail.as_ref() {
            let env_name = d.env_name.clone();
            self.spawn_detail_instances(env_name);
        }
    }

    /// Enter handler for the Health tab — drills into whichever
    /// `HealthItem` the `health_cursor` is currently on. Event → opens
    /// the full message in a TextDump overlay (some EB events are
    /// multi-line); Instance → switches to the Instances tab and
    /// positions the cursor on that instance; Main/DLQ queue → switches
    /// to the Queue tab and positions the queue cursor on the
    /// corresponding row (operator then presses Enter again to open the
    /// queue viewer).
    fn drill_health_item(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let now = chrono::Utc::now();
        let items = crate::app::health_items(detail, now);
        let Some(item) = items.get(detail.health_cursor).copied() else {
            return;
        };
        match item {
            HealthItem::Event { event_idx } => {
                let Some(ev) = detail.events.get(event_idx) else {
                    return;
                };
                let when = ev
                    .at
                    .map(|t| t.with_timezone(&chrono::Local).to_string())
                    .unwrap_or_else(|| "?".into());
                let body = format!(
                    "{when}\n[{}]  {}\n\n{}\n\nesc / q to close",
                    ev.severity, ev.env, ev.message
                );
                self.current_overlay = Some(Overlay::TextDump {
                    title: "event detail".into(),
                    body,
                });
            }
            HealthItem::Instance { instance_idx } => {
                // Switch to the Instances tab and seat the cursor on
                // the chosen instance. Then the operator can Enter
                // again for the info overlay, `s` for SSM, etc.
                let Some(d) = self.detail.as_mut() else {
                    return;
                };
                if let Some(pos) = d.tabs.iter().position(|t| *t == DetailTab::Instances) {
                    d.tab_idx = pos;
                }
                d.instances_cursor = instance_idx.min(d.instances.len().saturating_sub(1));
                d.instances_scroll = (d.instances_cursor as u16).saturating_sub(3);
                self.detail_refresh_active_tab();
            }
            HealthItem::MainQueue | HealthItem::Dlq => {
                let Some(d) = self.detail.as_mut() else {
                    return;
                };
                if let Some(pos) = d.tabs.iter().position(|t| *t == DetailTab::Queue) {
                    d.tab_idx = pos;
                }
                d.queue_cursor = match item {
                    HealthItem::MainQueue => 0,
                    HealthItem::Dlq => 1,
                    _ => 0,
                };
                self.detail_refresh_active_tab();
            }
        }
    }

    fn detail_cycle_tab(&mut self, delta: i32) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let n = detail.tabs.len() as i32;
        let next = (detail.tab_idx as i32 + delta).rem_euclid(n) as usize;
        detail.tab_idx = next;
        self.detail_refresh_active_tab();
        // NB: an earlier iteration auto-spawned the CW Logs streaming
        // overlay here when groups were discovered. Reverted because
        // jumping into a popup obscures the Logs tab's own snapshot path
        // (`^R`) and removes the explicit opt-in that `s` represents.
        // Pressing `s` on the Logs tab is the way to open the stream;
        // the in-overlay `g` keybind switches between discovered groups.
    }

    fn detail_scroll(&mut self, delta: i32) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        match detail.tab() {
            DetailTab::Events => {
                // Clamp to the ceiling the renderer published last frame
                // so j/k can't scroll the list off into blank space.
                detail.events_scroll =
                    scroll_apply(detail.events_scroll, delta).min(detail.events_max_scroll);
            }
            DetailTab::Instances => {
                let n = detail.instances.len();
                if n == 0 {
                    return;
                }
                let cur = detail.instances_cursor as i32;
                let next = (cur + delta).rem_euclid(n as i32) as usize;
                detail.instances_cursor = next;
                // Keep the scroll offset roughly aligned with the cursor so
                // the active row stays visible when navigating with j/k.
                detail.instances_scroll = (next as u16).saturating_sub(3);
            }
            DetailTab::Logs => {
                // Upper-bound by the unwrapped line count — the
                // paragraph wraps, so the exact ceiling depends on
                // width, but this stops j from scrolling far into
                // blank space (recovery was symmetric k presses).
                let total: usize = detail
                    .log_tail
                    .by_instance
                    .iter()
                    .map(|(_, t)| t.lines().count())
                    .sum();
                detail.log_tail.scroll = scroll_apply(detail.log_tail.scroll, delta)
                    .min(total.min(u16::MAX as usize) as u16);
            }
            DetailTab::Queue => {
                // Cursor wraps between the two queue rows (Main / DLQ).
                let n: i32 = 2;
                let cur = detail.queue_cursor as i32;
                detail.queue_cursor = (cur + delta).rem_euclid(n) as usize;
            }
            DetailTab::Health => {
                // Cursor wraps over the interactive items list; see
                // `health_items` for the enumeration order.
                let now = chrono::Utc::now();
                let n = crate::app::health_items(detail, now).len() as i32;
                if n == 0 {
                    return;
                }
                let cur = detail.health_cursor as i32;
                detail.health_cursor = (cur + delta).rem_euclid(n) as usize;
            }
            DetailTab::Config => {
                // Cursor moves over the editable rows (tags + env vars).
                // Clamped at the ends — no wrap — since the list can be
                // long and wrapping past the bottom is disorienting.
                let n = crate::app::config_editable_items(detail).len();
                if n == 0 {
                    return;
                }
                let cur = detail.config_cursor as i32;
                detail.config_cursor = (cur + delta).clamp(0, n as i32 - 1) as usize;
            }
            // Metrics tab has no scrollable cursor — the chart body
            // handles its own keyboard interactions.
            DetailTab::Metrics => {}
        }
    }

    fn detail_refresh_active_tab(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let env_name = detail.env_name.clone();
        let app_name = detail.env_snapshot.application.clone();
        let is_worker = detail.env_snapshot.tier.eq_ignore_ascii_case("Worker");
        let tab = detail.tab();
        // Release the immutable borrow of `detail` before calling
        // spawn_* methods which take `&mut self`.
        let _ = detail;
        match tab {
            // Health tab is a rollup — refresh events (for the recent-
            // events list) and queues (for worker DLQ depth shown
            // inline). Instances were eagerly fetched in `open_detail`
            // and don't change often, so we don't refetch them here on
            // every Health-tab visit; the eager fetch + periodic
            // background refresh keeps the count fresh enough.
            DetailTab::Health => {
                self.spawn_detail_events(env_name.clone());
                self.spawn_detail_alarms(env_name.clone());
                self.spawn_detail_recent_versions(app_name.clone(), env_name.clone());
                if is_worker {
                    self.spawn_detail_queues(app_name, env_name);
                }
            }
            DetailTab::Events => self.spawn_detail_events(env_name),
            DetailTab::Instances => self.spawn_detail_instances(env_name),
            DetailTab::Queue => self.spawn_detail_queues(app_name, env_name),
            DetailTab::Metrics => self.spawn_detail_metrics(env_name),
            DetailTab::Logs => self.spawn_detail_logs(env_name),
            DetailTab::Config => {}
        }
    }

    fn handle_detail_search_key(&mut self, key: KeyEvent) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        // Pick the search target based on which tab's search is currently active.
        // The Logs tab carries its own search state on `log_tail` so its filter
        // is independent of the Events tab's filter.
        let on_logs = detail.log_tail.search_active;
        match key.code {
            KeyCode::Esc => {
                if on_logs {
                    detail.log_tail.search_active = false;
                    detail.log_tail.search_input.clear();
                    detail.log_tail.search_error = None;
                } else {
                    detail.search_active = false;
                    detail.search_input.clear();
                    detail.search_error = None;
                }
            }
            KeyCode::Enter => {
                if on_logs {
                    detail.log_tail.search_active = false;
                    if detail.log_tail.search_input.is_empty() {
                        detail.log_tail.search_pattern = None;
                        detail.log_tail.search_error = None;
                        return;
                    }
                    match regex::RegexBuilder::new(detail.log_tail.search_input.text())
                        .case_insensitive(true)
                        .build()
                    {
                        Ok(r) => {
                            detail.log_tail.search_pattern = Some(r);
                            detail.log_tail.search_error = None;
                        }
                        Err(e) => {
                            detail.log_tail.search_pattern = None;
                            detail.log_tail.search_error = Some(format!("invalid regex: {e}"));
                        }
                    }
                    return;
                }
                detail.search_active = false;
                if detail.search_input.is_empty() {
                    detail.search_pattern = None;
                    detail.search_error = None;
                    return;
                }
                match regex::RegexBuilder::new(detail.search_input.text())
                    .case_insensitive(true)
                    .build()
                {
                    Ok(r) => {
                        detail.search_pattern = Some(r);
                        detail.search_error = None;
                    }
                    Err(e) => {
                        detail.search_pattern = None;
                        detail.search_error = Some(format!("invalid regex: {e}"));
                    }
                }
            }
            // TextInput consumes editing keys (cursor move / Ctrl-W
            // included) for whichever search field is active; the regex
            // is compiled on Enter, so no live side-effect on edit.
            _ => {
                if on_logs {
                    detail.log_tail.search_input.handle_key(key);
                } else {
                    detail.search_input.handle_key(key);
                }
            }
        }
    }

    /// Open the in-place value editor for the Config-tab row under the
    /// cursor. No-op if the cursor isn't on an editable row (empty
    /// list). Refuses in read-only mode so the operator isn't left
    /// typing a value that can't be dispatched.
    fn start_config_edit(&mut self) {
        let env_name = match self.detail.as_ref() {
            Some(d) => d.env_name.clone(),
            None => return,
        };
        if self.deny_write(&env_name, "config editing") {
            return;
        }
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let items = crate::app::config_editable_items(detail);
        let Some(item) = items.get(detail.config_cursor) else {
            self.error_message = Some("no editable config rows".into());
            return;
        };
        let key = item.key.clone();
        // TextInput seeds the caret at the end of the value so the
        // operator can append immediately, or arrow left to edit.
        detail.config_edit = Some(ConfigEdit {
            kind: item.kind,
            key: item.key.clone(),
            original: item.value.clone(),
            input: item.value.clone().into(),
            mode: ConfigEditMode::Value,
        });
        self.status_message = Some(format!("editing {key} — enter saves, esc cancels"));
    }

    /// Key handling while the Config-tab in-place editor is open.
    /// Esc cancels, Enter commits, Backspace / printable chars edit
    /// the value buffer. Mirrors `handle_detail_search_key`.
    fn handle_config_edit_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if let Some(d) = self.detail.as_mut() {
                    d.config_edit = None;
                }
                self.status_message = Some("config edit cancelled".into());
            }
            KeyCode::Enter => self.commit_config_edit(),
            KeyCode::Backspace => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.backspace();
                }
            }
            KeyCode::Delete => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.delete();
                }
            }
            KeyCode::Left => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.move_left();
                }
            }
            KeyCode::Right => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.move_right();
                }
            }
            KeyCode::Home => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.move_home();
                }
            }
            KeyCode::End => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.move_end();
                }
            }
            KeyCode::Char(c) if is_text_input(&key) => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.insert(c);
                }
            }
            _ => {}
        }
    }

    /// Commit the open Config-tab edit. All three modes dispatch via
    /// the same `UpdateOptionSettings` (env var) / `UpdateTags` (tag)
    /// paths `:env set` / `:tag` use. `Value` sets the row's new
    /// value (unchanged → no-op); `NewRow` parses the `KEY=VALUE`
    /// buffer and sets the new row; `RenameKey` sets the new key +
    /// removes the old in one call, carrying the row's value across.
    /// Clears the editor either way.
    fn commit_config_edit(&mut self) {
        let Some(edit) = self.detail.as_mut().and_then(|d| d.config_edit.take()) else {
            return;
        };
        let ns = "aws:elasticbeanstalk:application:environment";
        match edit.mode {
            ConfigEditMode::Value => {
                if edit.input.text() == edit.original.as_str() {
                    self.status_message = Some(format!("{} unchanged", edit.key));
                    return;
                }
                match edit.kind {
                    ConfigItemKind::EnvVar => self.spawn_option_settings_update(
                        format!("env set {}", edit.key),
                        vec![(ns.into(), edit.key.clone(), edit.input.text().to_string())],
                        vec![],
                    ),
                    ConfigItemKind::Tag => self.spawn_tag_update(
                        vec![(edit.key.clone(), edit.input.text().to_string())],
                        vec![],
                    ),
                }
            }
            ConfigEditMode::NewRow => {
                let Some((k, v)) = crate::mode_detail::parse_new_config_row(edit.input.text())
                else {
                    self.error_message = Some("new row needs KEY=VALUE (non-empty key)".into());
                    return;
                };
                match edit.kind {
                    ConfigItemKind::EnvVar => self.spawn_option_settings_update(
                        format!("env set {k}"),
                        vec![(ns.into(), k, v)],
                        vec![],
                    ),
                    ConfigItemKind::Tag => self.spawn_tag_update(vec![(k, v)], vec![]),
                }
            }
            ConfigEditMode::RenameKey => {
                let new_key = edit.input.trimmed().to_string();
                if new_key.is_empty() {
                    self.error_message = Some("rename: the new key can't be empty".into());
                    return;
                }
                if new_key == edit.original {
                    self.status_message = Some(format!("{} unchanged", edit.key));
                    return;
                }
                // Carry the row's current value across to the new key.
                let value = self.detail.as_ref().and_then(|d| {
                    config_editable_items(d)
                        .into_iter()
                        .find(|it| it.kind == edit.kind && it.key == edit.key)
                        .map(|it| it.value)
                });
                let Some(value) = value else {
                    self.error_message = Some("rename: the row no longer exists".into());
                    return;
                };
                let old = edit.key.clone();
                match edit.kind {
                    ConfigItemKind::EnvVar => self.spawn_option_settings_update(
                        format!("env rename {old} -> {new_key}"),
                        vec![(ns.into(), new_key, value)],
                        vec![(ns.into(), old)],
                    ),
                    ConfigItemKind::Tag => self.spawn_tag_update(vec![(new_key, value)], vec![old]),
                }
            }
        }
    }

    /// `n` on the Config tab — open the add-a-new-row editor. The new
    /// row's kind (tag vs env var) is taken from the section the
    /// cursor currently sits in; an empty editable list defaults to
    /// an env var (the more common edit target). The buffer is typed
    /// as `KEY=VALUE`.
    fn start_config_add(&mut self) {
        let env_name = match self.detail.as_ref() {
            Some(d) => d.env_name.clone(),
            None => return,
        };
        if self.deny_write(&env_name, "config editing") {
            return;
        }
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let items = crate::app::config_editable_items(detail);
        let kind = items
            .get(detail.config_cursor)
            .map(|i| i.kind)
            .unwrap_or(ConfigItemKind::EnvVar);
        detail.config_edit = Some(ConfigEdit {
            kind,
            key: String::new(),
            original: String::new(),
            input: TextInput::new(),
            mode: ConfigEditMode::NewRow,
        });
        let what = match kind {
            ConfigItemKind::EnvVar => "env var",
            ConfigItemKind::Tag => "tag",
        };
        self.status_message = Some(format!(
            "new {what} — type KEY=VALUE, enter saves, esc cancels"
        ));
    }

    /// `r` on the Config tab — open the key-rename editor for the row
    /// under the cursor. `input` is seeded with the current key;
    /// commit dispatches a remove-old + set-new (keeping the value)
    /// as one `UpdateOptionSettings` / `UpdateTags` call.
    fn start_config_rename(&mut self) {
        let env_name = match self.detail.as_ref() {
            Some(d) => d.env_name.clone(),
            None => return,
        };
        if self.deny_write(&env_name, "config editing") {
            return;
        }
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let items = crate::app::config_editable_items(detail);
        let Some(item) = items.get(detail.config_cursor) else {
            self.error_message = Some("no editable config rows".into());
            return;
        };
        let key = item.key.clone();
        detail.config_edit = Some(ConfigEdit {
            kind: item.kind,
            key: item.key.clone(),
            original: item.key.clone(),
            input: item.key.clone().into(),
            mode: ConfigEditMode::RenameKey,
        });
        self.status_message = Some(format!(
            "renaming {key} — type the new key, enter saves, esc cancels"
        ));
    }

    /// `x` on the Config tab — arm a delete of the row under the
    /// cursor. The actual `UpdateTags` / `UpdateOptionSettings`
    /// removal waits for the `y` confirmation (see the
    /// `config_delete_confirm` interception in the key handler).
    fn arm_config_delete(&mut self) {
        let env_name = match self.detail.as_ref() {
            Some(d) => d.env_name.clone(),
            None => return,
        };
        if self.deny_write(&env_name, "config editing") {
            return;
        }
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let items = crate::app::config_editable_items(detail);
        let Some(item) = items.get(detail.config_cursor) else {
            self.error_message = Some("no editable config rows".into());
            return;
        };
        let key = item.key.clone();
        detail.config_delete_confirm = Some(detail.config_cursor);
        self.status_message = Some(format!("delete {key}? — y confirms, any other key cancels"));
    }

    /// Confirmed delete of the armed Config-tab row — dispatches the
    /// removal (`UpdateTags` remove / `UpdateOptionSettings` remove).
    fn commit_config_delete(&mut self) {
        let Some(idx) = self
            .detail
            .as_mut()
            .and_then(|d| d.config_delete_confirm.take())
        else {
            return;
        };
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let items = crate::app::config_editable_items(detail);
        let Some(item) = items.get(idx) else {
            self.error_message = Some("config row no longer exists".into());
            return;
        };
        let kind = item.kind;
        let key = item.key.clone();
        match kind {
            ConfigItemKind::EnvVar => {
                let ns = "aws:elasticbeanstalk:application:environment";
                self.spawn_option_settings_update(
                    format!("env unset {key}"),
                    vec![],
                    vec![(ns.into(), key)],
                );
            }
            ConfigItemKind::Tag => {
                self.spawn_tag_update(vec![], vec![key]);
            }
        }
    }

    fn detail_search_jump(&mut self, delta: i32) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let Some(re) = detail.search_pattern.as_ref() else {
            return;
        };
        // Search only within the *filtered* event set — `events_scroll`
        // is a line offset into the rendered (filtered) list, so the
        // jump target must be a position in that same list, not a raw
        // index into `detail.events`.
        let visible = crate::mode_detail::filter_event_indices(
            &detail.events,
            detail.events_level,
            detail.events_window,
            chrono::Utc::now(),
        );
        let n = visible.len();
        if n == 0 {
            return;
        }
        let cur = (detail.events_scroll as usize).min(n - 1);
        let order: Vec<usize> = if delta >= 0 {
            (1..=n).map(|off| (cur + off) % n).collect()
        } else {
            (1..=n).map(|off| (cur + n - off) % n).collect()
        };
        for pos in order {
            if re.is_match(&detail.events[visible[pos]].message) {
                detail.events_scroll = pos as u16;
                return;
            }
        }
    }

    /// Cycle through saved-view chips above the env table.
    /// `delta = +1` → next chip; `-1` → previous; both wrap.
    /// "Active" is derived from comparing the current filter to
    /// each view's encoded `filter=` portion — matches the chip
    /// bar's own active-test, so cycling lands on the chip
    /// immediately to the right/left of whichever is currently
    /// applied. If no chip is active (operator typed a freeform
    /// filter or none at all), starts at index 0 / -1 depending
    /// on direction.
    ///
    /// Replaced the earlier `cycle_named_filter` (0.11 and prior)
    /// when saved-views unified into a single store in 0.12.
    /// Loading a view applies the full encoded snapshot via
    /// `apply_view`, so cycling can change sort / group / scope
    /// alongside the filter — the BACKLOG-promised "tab"
    /// behavior. Filter-only views (the legacy migration case)
    /// only change the filter, leaving sort/group/scope alone.
    fn cycle_saved_view(&mut self, delta: i32) {
        if self.saved_views.is_empty() {
            return;
        }
        // BTreeMap iteration is sorted by key, so the cycle order
        // matches the chip-bar render order. Keep them in sync.
        let names: Vec<String> = self.saved_views.keys().cloned().collect();
        let cur_idx = if self.filter.is_empty() {
            None
        } else {
            names.iter().position(|n| {
                self.saved_views
                    .get(n)
                    .map(|encoded| view_filter_value(encoded) == self.filter.text())
                    .unwrap_or(false)
            })
        };
        let next = match cur_idx {
            Some(i) => (i as i32 + delta).rem_euclid(names.len() as i32) as usize,
            None if delta >= 0 => 0,
            None => names.len() - 1,
        };
        let chosen = names[next].clone();
        if let Some(snap) = self.saved_views.get(&chosen).cloned() {
            apply_view(self, &snap);
            self.status_message = Some(format!("view: {chosen}"));
        }
    }

    /// Dispatch an auto-rollback redeploy for `env_name`. Single
    /// source of truth for the rollback dispatch — `apply_refresh`
    /// calls this when an armed watchdog's deadline has passed and
    /// the freshly-applied env is still non-Green. Earlier shape
    /// had this inline in `handle_auto_rollback_check`, which read
    /// possibly-stale cached health; making `apply_refresh` the
    /// decision point eliminates that race.
    ///
    /// Caller contracts: env is in the cached fleet, env is non-
    /// Green, watchdog slot exists. The "no snapshot" + read-only
    /// gating paths are handled here (drain the watchdog + surface
    /// an error / status) so caller logic stays simple.
    pub(crate) fn dispatch_auto_rollback(&mut self, env_name: String, health: String) {
        // Always drain a parallel wait-for-green watcher when the
        // rollback fires — otherwise the subsequent Green from the
        // rolled-back version would pin "✓ deploy reached Green:
        // ENV (build-900)" even though build-900 is the version we
        // just rolled away from. The auto-rollback's own pin is
        // the signal the operator should see.
        self.watching_deploys.remove(&env_name);
        let Some(snapshot) = self.deploy_snapshots.get(&env_name).cloned() else {
            // pin_error so the warning survives apply_refresh's auto-
            // clear — when this fires *from* apply_refresh (the
            // common case), the unpinned error_message would
            // otherwise be wiped on the same tick.
            self.pin_error(format!(
                "auto-rollback for {env_name}: no pre-deploy snapshot; manual rollback required"
            ));
            self.armed_watchdogs.remove(&env_name);
            return;
        };
        if self.deny_write(&env_name, "auto-rollback") {
            self.armed_watchdogs.remove(&env_name);
            return;
        }
        self.armed_watchdogs.remove(&env_name);
        let label = snapshot.previous_version_label.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        crate::audit::append_action_dispatched(
            account.as_deref(),
            profile.as_deref(),
            &region,
            "AutoRollback",
            env_name.as_str(),
            &[("version", label.as_str()), ("health", health.as_str())],
        );
        self.push_pending("Auto-rollback", env_name.clone());
        self.pin_status(format!(
            "auto-rollback for {env_name}: redeploying {label} (env was {health})"
        ));
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .deploy_version(&env_name, &label)
                .await
                .map_err(|e| flatten_err("deploy_version", e));
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action: Action::Deploy,
                env_name: env_for_msg,
                result,
            });
        });
    }

    fn cycle_metrics_range(&mut self, delta: i32) {
        const RANGES: &[i64] = &[900, 3600, 21_600, 86_400]; // 15m / 1h / 6h / 24h
        let Some(d) = self.detail.as_mut() else {
            return;
        };
        let cur = RANGES
            .iter()
            .position(|r| *r == d.metrics_range_secs)
            .unwrap_or(1) as i32;
        let next = (cur + delta).rem_euclid(RANGES.len() as i32) as usize;
        d.metrics_range_secs = RANGES[next];
        let env_name = d.env_name.clone();
        self.spawn_detail_metrics(env_name);
    }

    /// Open the worker-queue viewer for the env in Detail mode, defaulting
    /// to whichever queue the caller asked for. `open_dlq` is the legacy
    /// shortcut that always opens the DLQ.
    fn open_queue_viewer(&mut self, viewing: QueueView) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        if detail.tab() != DetailTab::Queue {
            return;
        }
        let main_url = detail.queues.main_url.clone().unwrap_or_default();
        let dlq_url = detail.queues.dlq_url.clone().unwrap_or_default();
        let target_url = match viewing {
            QueueView::Main => main_url.clone(),
            QueueView::Dlq => dlq_url.clone(),
        };
        if target_url.is_empty() {
            self.status_message = Some(match viewing {
                QueueView::Main => "no main queue URL known".into(),
                QueueView::Dlq => "no DLQ for this env".into(),
            });
            return;
        }
        let dlq = DlqState {
            env_name: detail.env_name.clone(),
            main_queue_url: main_url,
            dlq_url,
            messages: Vec::new(),
            list_state: ListState::default(),
            loading: false,
            error: None,
            confirm_purge: false,
            purge_typed: TextInput::new(),
            viewing,
            confirm_delete_id: None,
            replay_input: None,
        };
        self.dlq = Some(dlq);
        self.mode = Mode::Dlq;
        self.spawn_dlq_fetch();
    }

    fn target_env_for_action(&self) -> Option<Environment> {
        // Detail view targets the env it was opened on; Normal view targets selection.
        if let Some(d) = self.detail.as_ref() {
            return Some(d.env_snapshot.clone());
        }
        self.selected_env().cloned()
    }

    fn open_action_menu(&mut self) {
        let Some(target) = self.target_env_for_action() else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&target.name, "action menu") {
            return;
        }
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        self.action_flow = Some(ActionFlow::Menu { list_state });
        self.mode = Mode::Action;
    }

    fn close_action_flow(&mut self) {
        self.action_flow = None;
        if self.detail.is_some() {
            self.mode = Mode::Detail;
        } else {
            self.mode = Mode::Normal;
        }
    }

    /// Open a modal form. Captures the env at open-time (so later main-table
    /// cursor moves don't redirect the submit), spawns a
    /// `DescribeConfigurationSettings` fetch to pre-fill values, and flips
    /// to `Mode::Form`. The form stays in `FormState::Loading` until the
    /// `FormPrefilled` AppMsg lands.
    fn open_form(&mut self, mut form: crate::form::Form) {
        // LocalConfig forms don't need an AWS pre-fill — the caller has
        // already populated the field values from the live `App` state.
        // Skip the DescribeConfigurationSettings round-trip and go straight
        // to Ready so the user can type immediately.
        if matches!(form.submit, crate::form::FormSubmit::LocalConfig) {
            form.state = crate::form::FormState::Ready;
            self.form = Some(form);
            self.mode = Mode::Form;
            return;
        }
        let env_name = form.env_name.clone();
        // Look up the env's application from the live env list. We need it
        // for DescribeConfigurationSettings; the form itself only knows the
        // env name.
        let app_name = match self.environments.iter().find(|e| e.name == env_name) {
            Some(e) => e.application.clone(),
            None => {
                self.error_message = Some(format!("env '{env_name}' not in current list"));
                return;
            }
        };
        self.form = Some(form);
        self.mode = Mode::Form;
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let settings = aws
                .fetch_env_option_settings(&app_name, &env_for_msg)
                .await
                .map_err(|e| flatten_err("fetch_env_option_settings", e));
            let _ = tx.send(AppMsg::FormPrefilled {
                gen,
                env_name: env_for_msg,
                settings,
            });
        });
    }

    /// Key handler for `Mode::Form`. Loading-state forms ignore input
    /// (operator waits for the pre-fill); Ready forms route through Tab /
    /// arrow nav + per-field input; Submitting forms ignore input (waiting
    /// for the AppMsg::OptionSettingsUpdate that lands the result).
    fn handle_form_key(&mut self, key: KeyEvent) {
        use crate::form::{FieldKind, FormState};
        // Resolve current state before borrowing the form mutably so the
        // submit branch can dispatch through self.
        let state = self.form.as_ref().map(|f| f.state.clone());
        let cursor_kind = self
            .form
            .as_ref()
            .and_then(|f| f.current_field().map(|fld| fld.kind.clone()));
        match state {
            None => return,
            Some(FormState::Loading) | Some(FormState::Submitting) => {
                if matches!(key.code, KeyCode::Esc) {
                    self.form = None;
                    self.mode = Mode::Normal;
                }
                return;
            }
            Some(FormState::Ready) => {}
        }
        // Submit shortcut works regardless of focused-field kind.
        if matches!(key.code, KeyCode::Char('s')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.submit_form();
            return;
        }
        if matches!(key.code, KeyCode::Esc) {
            self.form = None;
            self.mode = Mode::Normal;
            return;
        }
        // Field navigation that's always available: Tab, Shift-Tab, Up, Down.
        // Up/Down would conflict with vim-style j/k inside text input — we
        // don't bind j/k for nav inside the form. Exception: when the
        // focused field is a MultiSelect, Up/Down (and j/k) move the
        // *option cursor* within the field rather than between fields;
        // Tab/Shift-Tab still leave the field.
        let is_multi = matches!(cursor_kind.as_ref(), Some(FieldKind::MultiSelect { .. }));
        let between_fields = match key.code {
            KeyCode::Tab => Some(1),
            KeyCode::BackTab => Some(-1),
            KeyCode::Up | KeyCode::Down if !is_multi => {
                if matches!(key.code, KeyCode::Up) {
                    Some(-1)
                } else {
                    Some(1)
                }
            }
            _ => None,
        };
        if let Some(delta) = between_fields {
            if let Some(form) = self.form.as_mut() {
                form.move_cursor(delta);
            }
            return;
        }
        // In-field option-cursor movement for MultiSelect fields. Wraps
        // around the option list both ways.
        if is_multi
            && matches!(
                key.code,
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k')
            )
        {
            if let Some(form) = self.form.as_mut() {
                if let Some(field) = form.current_field_mut() {
                    if let FieldKind::MultiSelect { options } = &field.kind {
                        let n = options.len();
                        if n > 0 {
                            let delta: isize =
                                matches!(key.code, KeyCode::Down | KeyCode::Char('j')) as isize * 2
                                    - 1;
                            let cur = field.option_cursor as isize;
                            let next = ((cur + delta) % n as isize + n as isize) % n as isize;
                            field.option_cursor = next as usize;
                        }
                    }
                }
            }
            return;
        }
        // Per-kind editing on the focused field.
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let Some(field) = form.current_field_mut() else {
            return;
        };
        // Live-revalidate after every edit so the inline error clears as the
        // operator fixes it.
        match (cursor_kind.unwrap_or(FieldKind::Text), key.code) {
            (FieldKind::Text, KeyCode::Backspace) => {
                field.value.pop();
            }
            (FieldKind::Text, KeyCode::Char(c)) if is_text_input(&key) => {
                field.value.push(c);
            }
            (FieldKind::Integer { .. }, KeyCode::Backspace) => {
                field.value.pop();
            }
            (FieldKind::Integer { .. }, KeyCode::Char(c))
                if c.is_ascii_digit() || (c == '-' && field.value.is_empty()) =>
            {
                field.value.push(c);
            }
            (FieldKind::Boolean, KeyCode::Char(' ')) => {
                field.value = if field.value == "true" {
                    "false".into()
                } else {
                    "true".into()
                };
            }
            (FieldKind::Boolean, KeyCode::Char('t')) => {
                field.value = "true".into();
            }
            (FieldKind::Boolean, KeyCode::Char('f')) => {
                field.value = "false".into();
            }
            (FieldKind::Select { options }, KeyCode::Left)
            | (FieldKind::Select { options }, KeyCode::Char('h')) => {
                let i = options.iter().position(|o| o == &field.value).unwrap_or(0);
                let next = (i + options.len() - 1) % options.len();
                field.value = options[next].clone();
            }
            (FieldKind::Select { options }, KeyCode::Right)
            | (FieldKind::Select { options }, KeyCode::Char('l')) => {
                let i = options.iter().position(|o| o == &field.value).unwrap_or(0);
                let next = (i + 1) % options.len();
                field.value = options[next].clone();
            }
            (FieldKind::MultiSelect { options }, KeyCode::Char(' ')) => {
                if let Some(opt) = options.get(field.option_cursor) {
                    field.value = crate::form::toggle_multi(&field.value, opt);
                }
            }
            _ => {}
        }
        // Clear stale error on this field after any edit.
        let _ = crate::form::validate_field(&field.value, &field.kind).map(|_| field.error = None);
    }

    /// Validate the form; if good, dispatch via the existing option-settings
    /// helper and switch to Submitting. Failures keep the form open with
    /// per-field error messages.
    fn submit_form(&mut self) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        if let Err(failing) = form.validate() {
            form.cursor = failing[0];
            return;
        }
        // LocalConfig submits write `config.toml` and apply changes live to
        // the running App. No AWS round-trip, so close out immediately.
        if matches!(form.submit, crate::form::FormSubmit::LocalConfig) {
            self.submit_local_config();
            return;
        }
        let env_name = form.env_name.clone();
        let summary = form.summary.clone();
        let (to_set, to_remove) = form.to_option_settings();
        form.state = crate::form::FormState::Submitting;
        // We can't reuse spawn_option_settings_update directly because it
        // reads self.selected_env() for the env_name; the form captured its
        // env at open time so we dispatch by-value here. Inlining keeps the
        // form's env binding authoritative.
        if self.deny_write(&env_name, "form submit") {
            self.form = None;
            self.mode = Mode::Normal;
            return;
        }
        if to_set.is_empty() && to_remove.is_empty() {
            self.status_message = Some("no changes to apply".into());
            self.form = None;
            self.mode = Mode::Normal;
            return;
        }
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "UpdateOptionSettings",
            env_name.as_str(),
            &[("summary", summary.as_str())],
        );
        self.push_pending(summary.clone(), env_name.clone());
        // No status_message ack here — the pending-actions pill in the
        // header (`⏳ N`) is the truth-source for in-flight work, and a
        // status_message ack would just race with whatever the operator
        // last set there. Completion fires a Success / Error toast.
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        let summary_for_msg = summary.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        // Undo capture — same shape as `spawn_option_settings_update`.
        // The form path lost the env's application name when it
        // stashed only `env_name`; recover it by looking up the
        // env in the cached fleet. Race with context switch leaves
        // `app_for_undo` as None and we silently skip capture.
        let app_for_undo = self
            .environments
            .iter()
            .find(|e| e.name == env_name)
            .map(|e| e.application.clone());
        let env_for_undo = env_name.clone();
        let summary_for_undo = summary.clone();
        let to_set_for_undo = to_set.clone();
        let to_remove_for_undo = to_remove.clone();
        tokio::spawn(async move {
            let undo_entry = if let Some(app_name) = app_for_undo {
                match aws
                    .fetch_env_option_settings(&app_name, &env_for_undo)
                    .await
                {
                    Ok(opts) => Some(build_undo_entry(
                        &env_for_undo,
                        &summary_for_undo,
                        &to_set_for_undo,
                        &to_remove_for_undo,
                        &opts,
                    )),
                    Err(_) => None,
                }
            } else {
                None
            };
            let result = aws
                .update_env_option_settings(&env_for_msg, &to_set, &to_remove)
                .await
                .map_err(|e| flatten_err("update_env_option_settings", e));
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                "UpdateOptionSettings",
                &env_for_msg,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[("summary", &summary_for_msg)],
            );
            if result.is_ok() {
                if let Some(entry) = undo_entry {
                    let _ = tx.send(AppMsg::UndoCaptured { gen, entry });
                }
            }
            let _ = tx.send(AppMsg::OptionSettingsUpdate {
                gen,
                env_name: env_for_msg,
                summary: summary_for_msg,
                result,
            });
        });
        // Close the form so the user returns to wherever they were.
        // OptionSettingsUpdate handler will fire a toast on completion.
        self.form = None;
        self.mode = Mode::Normal;
    }

    /// Apply a [`crate::form::FormSubmit::LocalConfig`] submit: render the
    /// form values back into a [`Config`], write it to disk, and update the
    /// live `App` state so theme / icons / refresh interval changes take
    /// effect immediately. Other fields (notify_bell, required_tags,
    /// redact, grouped, extra_regions) are updated in place but
    /// only take effect on the next refresh / restart depending on what
    /// reads them — see the field docs.
    fn submit_local_config(&mut self) {
        let Some(form) = self.form.as_ref() else {
            return;
        };
        let snapshot = self.current_config_snapshot();
        let updated = form.apply_to_config(&snapshot);
        match crate::config::save(&updated) {
            Ok(()) => {
                let path = crate::config::config_path();
                self.apply_config_live(&updated);
                self.pin_status(format!("settings saved → {}", path.display()));
            }
            Err(e) => {
                self.error_message = Some(format!("settings save failed: {e}"));
            }
        }
        self.form = None;
        self.mode = Mode::Normal;
    }

    /// Build the `:settings` form pre-filled from the live App state and
    /// Open the `:subnets` MultiSelect form: lists subnets in the env's
    /// VPC via `DescribeSubnets`, pre-fills with the env's current
    /// `aws:ec2:vpc.Subnets` selection, submits via the shared
    /// option-settings update path. Bound to the env table cursor —
    /// reports an error and bails if no env is selected.
    fn open_subnets_form(&mut self) {
        self.open_multi_select_form(MultiSelectFlavour::Subnets);
    }

    /// Open the `:elb-subnets` MultiSelect form. Same EC2 list call as
    /// `:subnets` but targets `aws:ec2:vpc.ELBSubnets` — the option
    /// setting that controls which subnets the env's ELB attaches to.
    /// Web-tier only; worker-tier envs leave this empty.
    fn open_elb_subnets_form(&mut self) {
        self.open_multi_select_form(MultiSelectFlavour::ElbSubnets);
    }

    /// Open the `:security-groups` MultiSelect form. Same shape as
    /// `:subnets` but lists security groups in the env's VPC and
    /// targets `aws:autoscaling:launchconfiguration.SecurityGroups`.
    fn open_security_groups_form(&mut self) {
        self.open_multi_select_form(MultiSelectFlavour::SecurityGroups);
    }

    /// Shared open + async-load path for the two MultiSelect pickers.
    /// Opens the form in `Loading` state with an empty option list,
    /// then spawns a tokio task that fans out to fetch the VPC context
    /// (via DescribeConfigurationSettings) and the EC2 listing
    /// (DescribeSubnets / DescribeSecurityGroups). The result lands as
    /// `AppMsg::FormMultiSelectLoaded` which the handler matches by
    /// `field_key` to populate the form.
    fn open_multi_select_form(&mut self, flavour: MultiSelectFlavour) {
        use crate::form::{Form, FormField, FormSubmit};
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let (title_prefix, summary, field_key, label, ns, opt_name) = match flavour {
            MultiSelectFlavour::Subnets => (
                "subnets",
                "subnets update",
                "subnets",
                "Subnets",
                "aws:ec2:vpc",
                "Subnets",
            ),
            MultiSelectFlavour::ElbSubnets => (
                "elb-subnets",
                "elb-subnets update",
                "elb_subnets",
                "ELB subnets",
                "aws:ec2:vpc",
                "ELBSubnets",
            ),
            MultiSelectFlavour::SecurityGroups => (
                "security-groups",
                "security-groups update",
                "security_groups",
                "Security groups",
                "aws:autoscaling:launchconfiguration",
                "SecurityGroups",
            ),
        };
        let placeholder = FormField::multi_select(
            field_key,
            label,
            Vec::new(),
            Vec::new(),
            Some::<String>("space toggle · ↑↓ option cursor · tab field".into()),
        );
        let form = Form::loading(
            format!("{title_prefix} — {}", env.name),
            env.name.clone(),
            summary.to_string(),
            vec![placeholder],
            FormSubmit::OptionSettings {
                mappings: vec![(field_key.into(), ns.into(), opt_name.into())],
            },
        );
        // open_form would dispatch the default DescribeConfigurationSettings
        // pre-fill, which doesn't load EC2 inventory. Bypass it: stash the
        // form ourselves and spawn the multi-select-specific loader.
        self.form = Some(form);
        self.mode = Mode::Form;
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env.name.clone();
        let app_name = env.application.clone();
        let field_key_for_msg = field_key.to_string();
        tokio::spawn(async move {
            let result = load_multi_select(aws, &app_name, &env_for_msg, flavour).await;
            let _ = tx.send(AppMsg::FormMultiSelectLoaded {
                gen,
                env_name: env_for_msg,
                field_key: field_key_for_msg,
                result,
            });
        });
    }

    /// Open the `:settings` form pre-filled from the live App state and
    /// open it. Submit writes `config.toml` and live-applies any field
    /// that can change at runtime (see [`App::apply_config_live`]).
    fn open_settings_form(&mut self) {
        use crate::form::{Form, FormField, FormSubmit};
        let snapshot = self.current_config_snapshot();
        let bool_select = vec!["true".to_string(), "false".to_string()];
        let triple_select = vec!["auto".to_string(), "true".to_string(), "false".to_string()];
        let mut fields: Vec<FormField> = Vec::new();
        // Theme — present the known names as a select; user can still
        // type-edit via the value field if they prefer a wider list later.
        let theme_options = vec![
            "dark".to_string(),
            "light".to_string(),
            "high-contrast".to_string(),
        ];
        let mut theme_field = FormField::select(
            "theme",
            "Theme",
            theme_options.clone(),
            Some::<String>("dark / light / high-contrast".into()),
        );
        // Pre-fill from current Config. Theme name is always one of the
        // known options at this point — App::new normalises unknown names
        // back to `dark`. Fall back to the first option defensively in
        // case a future theme is added without updating this list.
        theme_field.value = if theme_options.iter().any(|o| o == &snapshot.theme) {
            snapshot.theme.clone()
        } else {
            theme_options[0].clone()
        };
        fields.push(theme_field);

        let icons_options = vec![
            "unicode".to_string(),
            "ascii".to_string(),
            "powerline".to_string(),
            "auto".to_string(),
        ];
        let mut icons_field = FormField::select(
            "icons",
            "Icons",
            icons_options.clone(),
            Some::<String>("auto = probe the terminal at startup".into()),
        );
        icons_field.value = if icons_options
            .iter()
            .any(|o| o.eq_ignore_ascii_case(&snapshot.icons))
        {
            snapshot.icons.to_ascii_lowercase()
        } else {
            "unicode".to_string()
        };
        fields.push(icons_field);

        let mut refresh_field = FormField::integer(
            "refresh_interval_secs",
            "Refresh interval (s)",
            Some("How often the env list reloads from AWS"),
            Some(5),
            Some(600),
            false,
        );
        refresh_field.value = snapshot.refresh_interval.as_secs().to_string();
        fields.push(refresh_field);

        // redact_default and grouped_default are Option<bool> → use a
        // three-way select.
        let mut redact_field = FormField::select(
            "redact_default",
            "Redact by default",
            triple_select.clone(),
            Some::<String>("auto leaves the toggle to per-session state".into()),
        );
        redact_field.value = match snapshot.redact_default {
            None => "auto".into(),
            Some(true) => "true".into(),
            Some(false) => "false".into(),
        };
        fields.push(redact_field);

        let mut grouped_field = FormField::select(
            "grouped_default",
            "Group by app by default",
            triple_select,
            Some::<String>("auto leaves the toggle to per-session state".into()),
        );
        grouped_field.value = match snapshot.grouped_default {
            None => "auto".into(),
            Some(true) => "true".into(),
            Some(false) => "false".into(),
        };
        fields.push(grouped_field);

        let mut notify_field = FormField::select(
            "notify_bell",
            "Bell on new Red",
            bool_select,
            Some::<String>("ring BEL when an env transitions into Red".into()),
        );
        notify_field.value = if snapshot.notify_bell {
            "true".into()
        } else {
            "false".into()
        };
        fields.push(notify_field);

        let mut tags_field = FormField::text(
            "required_tags",
            "Required tags",
            Some::<String>("comma-separated; surfaced in :report".into()),
        );
        tags_field.value = snapshot.required_tags.join(",");
        fields.push(tags_field);

        let mut regions_field = FormField::text(
            "extra_regions",
            "Extra regions",
            Some::<String>("comma-separated; appended to :region picker".into()),
        );
        regions_field.value = snapshot.extra_regions.join(",");
        fields.push(regions_field);

        let form = Form::loading(
            "settings",
            String::new(),
            "settings".to_string(),
            fields,
            FormSubmit::LocalConfig,
        );
        self.open_form(form);
    }

    /// Build a [`Config`] from the App's current state. Used by the
    /// `:settings` form for pre-fill and as the base the form's edited
    /// fields are merged onto before writing back to disk.
    fn current_config_snapshot(&self) -> Config {
        let mut snapshot = Config {
            refresh_interval: self.refresh_interval,
            extra_regions: self.extra_regions.clone(),
            redact_default: Some(self.redact),
            grouped_default: Some(self.grouped),
            // Snapshot the BASE theme name, not the currently-applied one;
            // otherwise a profile-overridden theme would persist as the
            // new default and erase the operator's per-profile mapping.
            theme: self.cfg.base_theme_name.clone(),
            icons: self.cfg.cfg_icons_raw.clone(),
            notify_bell: self.notify_bell,
            required_tags: self.cfg.required_tags.clone(),
            profile_themes: self.cfg.profile_themes.clone(),
            // Accounts live in config.toml only — :settings doesn't
            // surface them in the form (the assume-role schema would
            // need its own editor), so the snapshot just preserves
            // whatever was loaded.
            accounts: self.cfg.accounts.clone(),
            runbooks: self.cfg.runbooks.clone(),
            safety_envs: self.cfg.safety_envs.clone(),
            safety_accounts: self.cfg.safety_accounts.clone(),
            notify_webhook: self.cfg.notify_webhook.clone(),
            command_aliases: self.cfg.command_aliases.clone(),
            lint_disable: self.cfg.lint_disable.clone(),
            // `lint.fix_disable` is a CLI-only knob (no TUI surface
            // consumes it; `ebman lint --fix` reads via
            // `config::load_lint_fix_disables` directly). We re-read
            // from disk on snapshot so `:settings save` doesn't
            // silently drop the existing line.
            lint_fix_disable: crate::config::load_lint_fix_disables(),
            explain_enabled: false,
            explain_provider: String::new(),
            explain_model: String::new(),
            explain_api_key_env: String::new(),
            explain_ollama_url: String::new(),
            explain_max_tokens: 0,
        };
        // Single source of truth for the `[explain]` block: the
        // resolved `Settings` on App. `write_to_config` fills the
        // Config struct's discrete fields and uses empty-string
        // sentinels for defaults so the serialiser only emits the
        // lines the operator has actually configured.
        self.cfg.explain_settings.write_to_config(&mut snapshot);
        snapshot
    }

    /// Resolve the effective read-only lock for a destructive action
    /// against `env_name`. Layered:
    ///
    /// 1. Global `--read-only` flag / `:readonly on` (master switch).
    /// 2. Per-env safety pin (`safety.envs.NAME.read_only = true` in
    ///    config.toml).
    /// 3. Per-account safety pin (`safety.accounts.NAME.read_only = true`)
    ///    matched against the active profile name.
    ///
    /// Any of these returning `true` blocks the action; the operator-
    /// facing error message can differentiate via `read_only_reason`.
    pub fn is_read_only_for(&self, env_name: &str) -> bool {
        if self.read_only {
            return true;
        }
        // Session-scoped freeze (`:freeze-deploys`) is fleet-wide —
        // doesn't care about env_name. Layered above the per-env /
        // per-account pins because it's the most-recent operator
        // gesture: if they froze deploys, they meant for nothing to
        // dispatch regardless of what the persisted pins say.
        if self.deploy_freeze.is_some() {
            return true;
        }
        if self.cfg.safety_envs.get(env_name).copied().unwrap_or(false) {
            return true;
        }
        if let Some(profile) = self.context.profile.as_deref() {
            if self
                .cfg
                .safety_accounts
                .get(profile)
                .copied()
                .unwrap_or(false)
            {
                return true;
            }
        }
        false
    }

    /// Enforce the read-only gate for a destructive action against
    /// `env_name`. Returns `true` (and sets `self.error_message` to a
    /// "<reason> — <verb> disabled" toast) when the env is locked;
    /// `false` (no side effects) otherwise. Designed to be the single
    /// guard at the top of every `spawn_*`-style destructive helper:
    ///
    /// ```ignore
    /// if self.deny_write(&env.name, "rollback") { return; }
    /// ```
    ///
    /// Saves duplicating the `is_read_only_for` + `read_only_reason`
    /// + `error_message` triplet at every call site (~25 of them).
    pub fn deny_write(&mut self, env_name: &str, verb: &str) -> bool {
        // `--demo` mode refuses writes outright (see spawn_action's
        // matching guard for the rationale — synthetic fleet, fake
        // AwsClient, real audit log).
        //
        // When BOTH demo_mode and a safety-pin / read-only lock apply,
        // mention both in the toast — operators using `--demo` to
        // validate their `safety.envs.*` / `safety.accounts.*` config
        // before going live shouldn't have to exit demo to confirm
        // the pin is wired correctly. (0.17.4 review)
        if self.demo_mode {
            let pin_reason = self.read_only_reason(env_name);
            let suffix = match pin_reason {
                Some(reason) => format!(" — would also refuse: {reason}"),
                None => String::new(),
            };
            self.error_message = Some(format!(
                "demo mode — {verb} not dispatched (writes are inert; press q to exit){suffix}"
            ));
            return true;
        }
        if !self.is_read_only_for(env_name) {
            return false;
        }
        let reason = self
            .read_only_reason(env_name)
            .unwrap_or_else(|| "read-only mode".into());
        self.error_message = Some(format!("{reason} — {verb} disabled"));
        true
    }

    /// Read-only gate for a *batch* destructive op over `env_names`.
    /// Returns `true` (and sets `self.error_message`) when the op must
    /// be refused. Unlike single-env [`deny_write`], a batch is gated
    /// per-env: if ANY selected env is locked the whole batch is
    /// refused (refuse-all, not skip-some — a safety pin shouldn't be
    /// silently routed around for the unpinned remainder), with the
    /// locked env names named so the operator can deselect them.
    ///
    /// Catches the env-independent gates (`--demo`, global read-only,
    /// `:freeze-deploys`) first via a representative `is_read_only_for`
    /// probe so those produce their normal whole-fleet message, then
    /// scans for per-env / per-account pins. Mirrors the precedence in
    /// [`is_read_only_for`]. `verb` names the op for the toast.
    pub fn deny_write_batch(&mut self, env_names: &[String], verb: &str) -> bool {
        // Demo mode + global/freeze gates are env-independent: probe
        // with the first env (or "") so the existing single-env path
        // produces the familiar "demo mode …" / "read-only mode …" /
        // "deploys frozen …" toast rather than a per-env list.
        let probe = env_names.first().map(|s| s.as_str()).unwrap_or("");
        if self.demo_mode || self.read_only || self.deploy_freeze.is_some() {
            return self.deny_write(probe, verb);
        }
        let locked: Vec<String> = env_names
            .iter()
            .filter(|n| self.is_read_only_for(n))
            .cloned()
            .collect();
        if locked.is_empty() {
            return false;
        }
        // Use the first locked env's reason as the headline (per-env
        // and per-account pins read the same regardless of which env);
        // list the locked names so the operator knows what to deselect.
        let reason = self
            .read_only_reason(&locked[0])
            .unwrap_or_else(|| "read-only mode".into());
        self.error_message = Some(format!(
            "{reason} — {verb} refused: {} of {} selected env(s) locked ({})",
            locked.len(),
            env_names.len(),
            locked.join(", ")
        ));
        true
    }

    /// Human-readable explanation of *why* an env is read-only, used
    /// in the toast / footer when a destructive action is blocked.
    /// Returns `None` when the env isn't locked (caller shouldn't have
    /// called this; defensive return). The three reasons are ordered
    /// to match `is_read_only_for`'s precedence.
    pub fn read_only_reason(&self, env_name: &str) -> Option<String> {
        if self.read_only {
            return Some("read-only mode (global toggle)".into());
        }
        if let Some(freeze) = self.deploy_freeze.as_ref() {
            let age = (chrono::Utc::now() - freeze.frozen_at).num_seconds().max(0);
            let age = crate::app::humanize_short_age(std::time::Duration::from_secs(age as u64));
            // When the freeze came from `:incident START`, point the
            // operator at the gesture that actually closes it — a bare
            // :thaw-deploys would lift the lock but leave the incident
            // banner up, which is rarely what they meant.
            let unlock_hint = if self.incident.is_some() {
                ":incident END to close"
            } else {
                ":thaw-deploys to unfreeze"
            };
            return Some(if freeze.reason.is_empty() {
                format!("deploys frozen ({age} ago) — {unlock_hint}")
            } else {
                format!(
                    "deploys frozen ({age} ago): {} — {unlock_hint}",
                    freeze.reason
                )
            });
        }
        if self.cfg.safety_envs.get(env_name).copied().unwrap_or(false) {
            return Some(format!(
                "read-only mode (env pinned via safety.envs.{env_name})"
            ));
        }
        if let Some(profile) = self.context.profile.as_deref() {
            if self
                .cfg
                .safety_accounts
                .get(profile)
                .copied()
                .unwrap_or(false)
            {
                return Some(format!(
                    "read-only mode (account pinned via safety.accounts.{profile})"
                ));
            }
        }
        None
    }

    /// Per-profile theme override. Looks at the active profile (from
    /// `self.context.profile`) and the configured `profile_themes` map;
    /// swaps `self.theme` to the override if one exists, or back to the
    /// base theme otherwise. Idempotent — calling repeatedly with the
    /// same profile is a no-op.
    fn maybe_apply_profile_theme(&mut self) {
        let profile = self.context.profile.as_deref().unwrap_or("default");
        let target_name = self
            .cfg
            .profile_themes
            .get(profile)
            .cloned()
            .unwrap_or_else(|| self.cfg.base_theme_name.clone());
        // Avoid rebuilding the Arc<Theme> when nothing changed.
        if self.theme.name == target_name {
            return;
        }
        let (mut t, warning) = Theme::resolve(&target_name);
        if let Some(w) = warning {
            tracing::warn!("{w}");
        }
        // Preserve the live-resolved icon style across the swap — icons
        // are a font-capability fact, not a theme preference, and the
        // `auto` probe only runs once at startup.
        t.icons = self.theme.icons;
        self.theme = Arc::new(t);
        // Theme swap invalidates the cached per-app colour assignments —
        // same reason as `apply_config_live`.
        self.cached_app_colors.clear();
    }

    /// Apply a saved [`Config`] to the running App. Mirrors the assignments
    /// in [`App::new`] for the slots that can change at runtime; fields not
    /// listed here only take effect on restart.
    fn apply_config_live(&mut self, cfg: &Config) {
        // Theme + icons are stored on an `Arc<Theme>`; rebuild it from the
        // resolved values so renderers pick up the new palette/icon style
        // on the next draw.
        let (mut t, warning) = Theme::resolve(&cfg.theme);
        if let Some(w) = warning {
            tracing::warn!("{w}");
        }
        // Resolve `icons = "auto"` again — the form may have set it. We
        // can't run the probe from inside the TUI (alt-screen swallows the
        // cursor query), so "auto" falls back to whatever the previous
        // resolution chose. Operators who want a fresh probe should restart.
        let icons_raw = cfg.icons.clone();
        let resolved_icons = if icons_raw.eq_ignore_ascii_case("auto") {
            // Keep the previous resolved style on the running theme;
            // restart picks up a fresh probe.
            self.theme.icons
        } else {
            match icons_raw.trim().to_ascii_lowercase().as_str() {
                "ascii" => IconStyle::Ascii,
                "powerline" | "nerd" | "nerdfont" => IconStyle::Powerline,
                _ => IconStyle::Unicode,
            }
        };
        t.icons = resolved_icons;
        self.theme = Arc::new(t);
        self.cfg.cfg_icons_raw = icons_raw;
        // Refresh interval — the ticker reads `self.refresh_interval` on
        // each tick boundary, so the new value applies on the next cycle.
        self.refresh_interval = cfg.refresh_interval;
        // Defaults that flow through the persisted-state overlay: don't
        // overwrite the live toggles (the user may have flipped them with
        // `:redact` / `:group`), only the *_default fields in cfg get
        // written back. Reflecting those onto the running view would
        // surprise the operator.
        self.extra_regions = cfg.extra_regions.clone();
        self.notify_bell = cfg.notify_bell;
        self.cfg.required_tags = cfg.required_tags.clone();
        // Theme swap invalidates the cached per-app colour assignments —
        // those store final `Color` values, not palette indices, so they'd
        // otherwise carry the old palette into the new theme's rendering.
        self.rebuild_view();
    }

    fn handle_action_key(&mut self, key: KeyEvent) {
        let Some(flow) = self.action_flow.as_mut() else {
            self.mode = Mode::Normal;
            return;
        };
        match flow {
            ActionFlow::Menu { list_state } => match key.code {
                // Menu has j/k cursor + Enter to pick — no text input, so
                // `q` as close is unambiguous and matches every other
                // overlay's pattern.
                KeyCode::Esc | KeyCode::Char('q') => self.close_action_flow(),
                KeyCode::Char('j') | KeyCode::Down => {
                    let cur = list_state.selected().unwrap_or(0);
                    let next = (cur + 1) % ACTIONS.len();
                    list_state.select(Some(next));
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    let cur = list_state.selected().unwrap_or(0);
                    let next = (cur + ACTIONS.len() - 1) % ACTIONS.len();
                    list_state.select(Some(next));
                }
                KeyCode::Enter => {
                    let Some(idx) = list_state.selected() else {
                        return;
                    };
                    let action = ACTIONS[idx];
                    self.advance_action_flow(action);
                }
                _ => {}
            },
            ActionFlow::SwapTarget { picker, .. } => match key.code {
                KeyCode::Esc => self.close_action_flow(),
                KeyCode::Down | KeyCode::Char('j')
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    picker.move_selection(1);
                }
                KeyCode::Up | KeyCode::Char('k')
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    picker.move_selection(-1);
                }
                KeyCode::Enter => {
                    let Some(target) = picker.selected_value() else {
                        return;
                    };
                    let source = match flow {
                        ActionFlow::SwapTarget { source, .. } => source.clone(),
                        _ => return,
                    };
                    let warning = self
                        .environments
                        .iter()
                        .find(|e| e.name == source)
                        .map(compute_traffic_warning)
                        .unwrap_or(None);
                    self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                        action: Action::SwapCnames,
                        target_env: source,
                        swap_with: Some(target),
                        typed: TextInput::new(),
                        kind: ConfirmKind::YesNo,
                        dryrun: None,
                        loading_dryrun: false,
                        recent_events: None,
                        loading_events: false,
                        traffic_warning: warning,
                        deploy_version: None,
                        upgrade_platform_arn: None,
                        upgrade_platform_label: None,
                        clone_target: None,
                        scale_min: None,
                        scale_max: None,
                        auto_rollback_secs: None,
                        wait_for_green_secs: None,
                        version_preview: None,
                        loading_version_preview: false,
                        health_check_probe: None,
                        loading_health_check: false,
                        unavailability_line: None,
                        loading_unavailability: false,
                        lint_issues: None,
                        loading_lint: false,
                        ssm_run_command: None,
                        ssm_run_instances: None,
                    }));
                }
                // TextInput consumes editing keys (incl. Backspace);
                // reselect a still-matching row after any accepted edit.
                _ => {
                    if picker.filter.handle_key(key) {
                        let filt = picker.filtered();
                        if !filt
                            .iter()
                            .any(|i| Some(*i) == picker.list_state.selected())
                        {
                            picker.list_state.select(filt.first().copied());
                        }
                    }
                }
            },
            ActionFlow::Confirm(modal) => match (key.code, modal.kind) {
                (KeyCode::Esc, _) => self.close_action_flow(),
                // `q` cancels Y/N confirms (n / esc are the others). TypeName
                // confirms intentionally don't bind q since the user is
                // typing the env name and `q` might be part of it.
                (KeyCode::Char('q'), ConfirmKind::YesNo) => self.close_action_flow(),
                (KeyCode::Char('y'), ConfirmKind::YesNo) | (KeyCode::Enter, ConfirmKind::YesNo) => {
                    // Queue with a 5s cancel window instead of dispatching
                    // immediately. The action flow closes (modal gone)
                    // and a countdown lands in `status_message`; `U` in
                    // Normal mode undoes it before the deadline.
                    let m = modal.clone();
                    self.close_action_flow();
                    self.queue_action_dispatch(m);
                }
                (KeyCode::Char('n'), ConfirmKind::YesNo) => self.close_action_flow(),
                (KeyCode::Enter, ConfirmKind::TypeName)
                    if modal.typed.text() == modal.target_env.as_str() =>
                {
                    // Same cancel-window treatment as Y-confirms. Terminate
                    // is the loudest example — the typed-name guard already
                    // prevents accidental dispatch, but the 5s window is a
                    // last-ditch "oh god no" rescue.
                    let m = modal.clone();
                    self.close_action_flow();
                    self.queue_action_dispatch(m);
                }
                // TextInput consumes editing keys for the type-to-confirm
                // field (cursor move / Ctrl-W included).
                (_, ConfirmKind::TypeName) => {
                    modal.typed.handle_key(key);
                }
                _ => {}
            },
            ActionFlow::Rollout(flow) => match (key.code, &flow.state) {
                // Esc / q close the rollout at any state — even
                // during Dispatching the operator can abort
                // further regions. The dispatched ones have
                // already fired (each region's UpdateEnvironment
                // is a non-reversible AWS write), but the loop
                // halts so regions queued behind the current
                // one don't fire.
                (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => self.close_action_flow(),
                // `y` confirms a fully-pre-flighted plan. Only
                // valid in AwaitingConfirm. Switches state to
                // Dispatching and fires the first region.
                (KeyCode::Char('y'), crate::mode_action::RolloutState::AwaitingConfirm)
                | (KeyCode::Enter, crate::mode_action::RolloutState::AwaitingConfirm) => {
                    // Refuse to dispatch if no regions passed
                    // pre-flight — there's nothing safe to send.
                    let any_ok = flow.regions.iter().any(|r| r.env_found == Some(true));
                    if !any_ok {
                        self.error_message = Some(
                            "rollout: no regions passed pre-flight — fix or `esc` to abort".into(),
                        );
                        return;
                    }
                    // Find the first region that passed pre-
                    // flight. Failed regions are skipped (their
                    // outcome stays None — surfaced as
                    // "skipped" in the final report).
                    let Some((first_idx, _)) = flow
                        .regions
                        .iter()
                        .enumerate()
                        .find(|(_, r)| r.env_found == Some(true))
                    else {
                        return;
                    };
                    flow.state = crate::mode_action::RolloutState::Dispatching {
                        next_index: first_idx,
                    };
                    let region = flow.regions[first_idx].region.clone();
                    let env_name = flow.env_name.clone();
                    let version_label = flow.version_label.clone();
                    let wait_for_green_secs = flow.wait_for_green_secs;
                    let rollout_id = flow.rollout_id.clone();
                    let profile = self.context.profile.clone();
                    self.spawn_rollout_dispatch(
                        rollout_id,
                        profile,
                        region,
                        env_name,
                        version_label,
                        wait_for_green_secs,
                    );
                }
                // `n` aborts before any dispatch fires. Same as
                // esc but matches the y/n posture of the
                // standard ConfirmModal.
                (KeyCode::Char('n'), crate::mode_action::RolloutState::AwaitingConfirm) => {
                    self.close_action_flow();
                }
                _ => {}
            },
        }
    }

    fn advance_action_flow(&mut self, action: Action) {
        let Some(env) = self.target_env_for_action() else {
            self.close_action_flow();
            return;
        };
        match action {
            Action::SwapCnames => {
                // Build a list of envs in the same application (excluding the source).
                let candidates: Vec<String> = self
                    .environments
                    .iter()
                    .filter(|e| e.application == env.application && e.name != env.name)
                    .map(|e| e.name.clone())
                    .collect();
                if candidates.is_empty() {
                    self.action_flow = None;
                    self.mode = if self.detail.is_some() {
                        Mode::Detail
                    } else {
                        Mode::Normal
                    };
                    self.error_message = Some(format!(
                        "no swap candidates: app '{}' has only one env",
                        env.application
                    ));
                    return;
                }
                let picker = Picker::new(PickerKind::Region, candidates, None); // kind unused here
                self.action_flow = Some(ActionFlow::SwapTarget {
                    source: env.name.clone(),
                    picker,
                });
            }
            Action::Terminate => {
                // Terminate is the only Action that uses TypeName confirm;
                // every other entry routes through `open_parameterised_action`.
                // Preflight gating still flows from `Action::wants_preflight()`
                // so the rule lives in exactly one place.
                let wants_preflight = action.wants_preflight();
                self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                    action,
                    target_env: env.name.clone(),
                    swap_with: None,
                    typed: TextInput::new(),
                    kind: ConfirmKind::TypeName,
                    dryrun: None,
                    loading_dryrun: wants_preflight,
                    recent_events: None,
                    loading_events: wants_preflight,
                    traffic_warning: compute_traffic_warning(&env),
                    deploy_version: None,
                    upgrade_platform_arn: None,
                    upgrade_platform_label: None,
                    clone_target: None,
                    scale_min: None,
                    scale_max: None,
                    auto_rollback_secs: None,
                    wait_for_green_secs: None,
                    version_preview: None,
                    loading_version_preview: false,
                    health_check_probe: None,
                    loading_health_check: false,
                    unavailability_line: None,
                    loading_unavailability: false,
                    lint_issues: None,
                    loading_lint: false,
                    ssm_run_command: None,
                    ssm_run_instances: None,
                }));
                if wants_preflight {
                    self.spawn_dry_run(env.name.clone());
                    self.spawn_preflight_events(env.name.clone());
                }
            }
            Action::Rebuild => {
                self.open_parameterised_action(action, ParameterisedAction::default());
            }
            // Parameterised actions need user input before the confirm can
            // be built. The menu closes itself and pre-fills the command
            // bar so the user types `<arg>` and Enter, which routes through
            // the existing `:deploy` / `:upgrade` / `:clone` / `:scale`
            // handlers (all of which open a confirm modal).
            Action::Deploy => {
                self.close_action_flow();
                self.mode = Mode::Command;
                self.command_input = "deploy ".into();
                self.status_message = Some("type a version label and press enter".into());
            }
            Action::UpgradePlatform => {
                self.close_action_flow();
                self.spawn_list_compatible_platforms(env.name.clone());
                self.mode = Mode::Command;
                self.command_input = "upgrade ".into();
                self.status_message =
                    Some("listing platforms in overlay; paste an ARN and press enter".into());
            }
            Action::Clone => {
                self.close_action_flow();
                self.mode = Mode::Command;
                self.command_input = "clone ".into();
                self.status_message = Some("type a new env name and press enter".into());
            }
            Action::Scale => {
                self.close_action_flow();
                self.mode = Mode::Command;
                self.command_input = "scale ".into();
                self.status_message = Some(
                    "scale N (instances), or `scale min N` / `scale max N`; enter to apply".into(),
                );
            }
            Action::Capacity => {
                // `:capacity` opens a modal form pre-filled from
                // DescribeConfigurationSettings — no command-bar args
                // needed, so we close the menu and dispatch straight
                // to the form opener.
                self.close_action_flow();
                self.cmd_capacity();
            }
            Action::AbortUpdate => {
                self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                    action,
                    target_env: env.name.clone(),
                    swap_with: None,
                    typed: TextInput::new(),
                    kind: ConfirmKind::YesNo,
                    dryrun: None,
                    loading_dryrun: false,
                    recent_events: None,
                    loading_events: false,
                    traffic_warning: compute_traffic_warning(&env),
                    deploy_version: None,
                    upgrade_platform_arn: None,
                    upgrade_platform_label: None,
                    clone_target: None,
                    scale_min: None,
                    scale_max: None,
                    auto_rollback_secs: None,
                    wait_for_green_secs: None,
                    version_preview: None,
                    loading_version_preview: false,
                    health_check_probe: None,
                    loading_health_check: false,
                    unavailability_line: None,
                    loading_unavailability: false,
                    lint_issues: None,
                    loading_lint: false,
                    ssm_run_command: None,
                    ssm_run_instances: None,
                }));
            }
            _ => {
                self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                    action,
                    target_env: env.name.clone(),
                    swap_with: None,
                    typed: TextInput::new(),
                    kind: ConfirmKind::YesNo,
                    dryrun: None,
                    loading_dryrun: false,
                    recent_events: None,
                    loading_events: false,
                    traffic_warning: compute_traffic_warning(&env),
                    deploy_version: None,
                    upgrade_platform_arn: None,
                    upgrade_platform_label: None,
                    clone_target: None,
                    scale_min: None,
                    scale_max: None,
                    auto_rollback_secs: None,
                    wait_for_green_secs: None,
                    version_preview: None,
                    loading_version_preview: false,
                    health_check_probe: None,
                    loading_health_check: false,
                    unavailability_line: None,
                    loading_unavailability: false,
                    lint_issues: None,
                    loading_lint: false,
                    ssm_run_command: None,
                    ssm_run_instances: None,
                }));
            }
        }
    }

    /// Key handler for the `:logs-tail` streaming overlay. j/k scroll, G
    /// snaps back to follow-mode (auto-tail), g jumps to top (and pauses
    /// follow), / opens a regex filter, n clears it, esc/q closes the
    /// overlay and tears down the polling task.
    fn handle_log_tail_key(&mut self, key: KeyEvent) {
        // Group-switcher: Tab opens a Picker over the env's discovered CW
        // log groups. Handled up-front before the borrow of
        // `current_overlay` below so the picker open can re-borrow `self`.
        // (In filter-entry mode Tab is input, not the switcher.)
        if matches!(key.code, KeyCode::Tab) {
            let in_filter = matches!(
                self.current_overlay.as_ref(),
                Some(Overlay::LogTail { view, .. }) if view.filter_active
            );
            if !in_filter {
                self.open_log_group_picker();
                return;
            }
        }
        let Some(Overlay::LogTail { view, events, .. }) = self.current_overlay.as_mut() else {
            return;
        };
        let outcome = tail::handle_tail_key(view, key);
        // Clamp the scroll to the buffer: `g` sets the u16::MAX
        // sentinel, and without a ceiling every subsequent `j` costs
        // one dead press (~63k of them) before the view moves again.
        view.scroll = view.scroll.min(events.len() as u16);
        if outcome == tail::TailKeyOutcome::Close {
            // Reap so a late `LogTailOpened` from the aborted task can't
            // re-open the overlay the user just dismissed.
            tail::reap_tail_task(&mut self.log_tail_task, &mut self.log_tail_session);
            self.current_overlay = None;
        }
    }

    /// Open a Picker over the env's discovered CW log groups so the operator
    /// can switch the tailed group from inside the streaming overlay.
    /// Pre-selects the currently-tailed group; no-op (with a status hint) if
    /// no groups have been discovered for this env.
    fn open_log_group_picker(&mut self) {
        let Some(Overlay::LogTail { log_group, .. }) = self.current_overlay.as_ref() else {
            return;
        };
        let current_group = log_group.clone();
        let groups: Vec<String> = self
            .detail
            .as_ref()
            .and_then(|d| d.cw_log_groups.clone())
            .unwrap_or_default();
        if groups.is_empty() {
            self.status_message = Some(
                "no CW log groups discovered for this env — try `:logs-tail <full-group-name>`"
                    .into(),
            );
            return;
        }
        self.picker = Some(Picker::new(
            PickerKind::LogGroup,
            groups,
            Some(current_group.as_str()),
        ));
        self.mode = Mode::Picker;
    }

    /// Dispatch `UpdateEnvironment(template_name)`. Used by both the typed
    /// `:config-apply TEMPLATE` command and the `a`/enter key in the
    /// interactive saved-configs overlay. Reads template + env directly
    /// so callers can pass strings with embedded spaces (the typed-command
    /// parser joins rest with single spaces; the overlay passes the raw
    /// template name).
    fn spawn_config_apply_template(&mut self, env_name: String, template: String) {
        if self.deny_write(&env_name, "config-apply") {
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        // In-flight ack lives on the pending pill; completion toasts.
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "ConfigApply",
            env_name.as_str(),
            &[("template", template.as_str())],
        );
        self.push_pending(Action::ConfigApply.label(), env_name.clone());
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .apply_config_template(&env_for_msg, &template)
                .await
                .map_err(|e| flatten_err("apply_config_template", e));
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action: Action::ConfigApply,
                env_name: env_for_msg,
                result,
            });
        });
    }

    /// Dispatch `DeleteConfigurationTemplate`. Same shape as
    /// `spawn_config_apply_template`; bypasses the typed-command parser so
    /// the overlay can pass template names with embedded spaces.
    fn spawn_config_delete_template(&mut self, app_name: String, template: String) {
        // config-delete is app-scoped, not env-scoped — the template
        // lives at the application level. Per-account safety still
        // applies; per-env doesn't. The global / account-pin gate fires
        // via `deny_write` with an empty env name (which never matches
        // any `safety_envs` key).
        if self.deny_write("", "config-delete") {
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let target = format!("{app_name}/{template}");
        self.status_message = Some(format!(
            "deleting template '{template}' from app '{app_name}'…"
        ));
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "ConfigDelete",
            &target,
            &[],
        );
        self.push_pending(Action::ConfigDelete.label(), target.clone());
        let template_for_msg = template.clone();
        tokio::spawn(async move {
            let result = aws
                .delete_config_template(&app_name, &template)
                .await
                .map_err(|e| flatten_err("delete_config_template", e))
                .map_err(|e| format!("config-delete '{template_for_msg}': {e}"));
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action: Action::ConfigDelete,
                env_name: target,
                result,
            });
        });
    }

    /// Fetch a template's option settings and surface them as a TextOverlay.
    /// Read-only — no read-only-mode gate. Called by `:config-inspect` and
    /// by the `i` keybind in the interactive saved-configs overlay.
    fn spawn_config_inspect_template(&mut self, app_name: String, template: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let title = format!("template — {app_name}/{template}");
        // In-flight ack: pending pill. Inspect result lands as a TextOverlay.
        tokio::spawn(async move {
            let body = match aws.describe_template_settings(&app_name, &template).await {
                Ok(settings) if settings.is_empty() => {
                    "(template has no option settings)".to_string()
                }
                Ok(settings) => format_template_settings(&settings),
                Err(e) => format!("error: {}", flatten_err("describe_template_settings", e)),
            };
            let _ = tx.send(AppMsg::TextOverlay { gen, title, body });
        });
    }

    /// Open a streaming CW Logs view for `env_name`. If `explicit_group` is
    /// `None`, discovers the env's log groups and picks the most useful one
    /// via `pick_default_log_group`. Aborts any active log-tail task before
    /// starting the new one, then spawns a polling loop that sends
    /// `AppMsg::LogTailEvents` every ~2s. The overlay opens immediately in
    /// a "discovering" state and gets replaced with the LogTail variant
    /// once the group is known.
    fn spawn_logs_tail(&mut self, env_name: String, explicit_group: Option<String>) {
        // Tear down any prior session so we don't have two pollers racing.
        tail::reap_tail_task(&mut self.log_tail_task, &mut self.log_tail_session);
        let session_id = self.log_tail_session;
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        // In-flight ack: the LogTail overlay opens itself when data lands.
        let handle = tokio::spawn(async move {
            // Resolve the log group up front. If the user supplied one,
            // trust it (no DescribeLogGroups round-trip); otherwise discover.
            let group = match explicit_group {
                Some(g) => g,
                None => match aws.discover_env_log_groups(&env_for_msg).await {
                    Ok(groups) => match pick_default_log_group(&groups) {
                        Some(g) => g,
                        None => {
                            let _ = tx.send(AppMsg::LogTailEvents {
                                gen,
                                session_id,
                                next_since_ms: 0,
                                result: Err(format!(
                                    "no CW log groups under /aws/elasticbeanstalk/{env_for_msg}/ — enable streaming with `:logs-stream on`"
                                )),
                            });
                            return;
                        }
                    },
                    Err(e) => {
                        let _ = tx.send(AppMsg::LogTailEvents {
                            gen,
                            session_id,
                            next_since_ms: 0,
                            result: Err(format!("discover log groups: {e}")),
                        });
                        return;
                    }
                },
            };
            // First batch: fetch the last 5 minutes so the overlay isn't
            // empty on open.
            let mut since_ms = chrono::Utc::now().timestamp_millis() - 5 * 60 * 1000;
            // Send an "opening" message that tells the App handler what log
            // group resolved + replaces the overlay with a real LogTail.
            let _ = tx.send(AppMsg::LogTailOpened {
                gen,
                session_id,
                env_name: env_for_msg.clone(),
                log_group: group.clone(),
                since_ms,
            });
            let mut boundary_ids: std::collections::HashSet<String> = Default::default();
            loop {
                match aws
                    .fetch_recent_log_events(&group, since_ms, 1000, &boundary_ids)
                    .await
                {
                    Ok((events, next_since, carry)) => {
                        let next_since_ms = next_since;
                        let _ = tx.send(AppMsg::LogTailEvents {
                            gen,
                            session_id,
                            next_since_ms,
                            result: Ok(events),
                        });
                        since_ms = next_since;
                        boundary_ids = carry;
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::LogTailEvents {
                            gen,
                            session_id,
                            next_since_ms: since_ms,
                            result: Err(format!("{e}")),
                        });
                        // Keep going on errors — transient throttling
                        // shouldn't kill the session.
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
        self.log_tail_task = Some(handle);
    }

    /// `:event-tail` — open the cross-fleet event tail overlay and
    /// start its polling task. First batch is the fleet's most recent
    /// events regardless of age (so the overlay isn't empty on open);
    /// subsequent polls pass a `start_time` watermark so a busy fleet
    /// re-ships only what's new. DescribeEvents is more
    /// throttle-sensitive than FilterLogEvents, hence the 5s cadence
    /// (vs logs-tail's 2s). Errors keep the loop alive.
    fn spawn_event_tail(&mut self) {
        // Tear down any prior session so we don't have two pollers racing.
        tail::reap_tail_task(&mut self.event_tail_task, &mut self.event_tail_session);
        let session_id = self.event_tail_session;
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let handle = tokio::spawn(async move {
            // Install the (empty) overlay first so the operator sees
            // the tail open immediately; the first batch fills it.
            let _ = tx.send(AppMsg::EventTailOpened { gen, session_id });
            let mut since_ms = match aws.list_events(EVENT_TAIL_FIRST_BATCH).await {
                Ok(mut events) => {
                    // DescribeEvents returns newest-first; the ring
                    // buffer appends oldest-first.
                    events.reverse();
                    // Watermark = newest event + 1ms, NOT local now():
                    // clamping to now() would skip anything that lands
                    // between the server-side snapshot and our clock
                    // (eventual consistency / clock skew). Events in
                    // (newest, now] weren't in this batch, so there's
                    // no duplicate risk. Empty fleet history falls
                    // back to now.
                    let watermark = if events.is_empty() {
                        chrono::Utc::now().timestamp_millis()
                    } else {
                        next_event_watermark_ms(&events, 0)
                    };
                    let _ = tx.send(AppMsg::EventTailEvents {
                        gen,
                        session_id,
                        result: Ok(events),
                    });
                    watermark
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::EventTailEvents {
                        gen,
                        session_id,
                        result: Err(format!("{e}")),
                    });
                    chrono::Utc::now().timestamp_millis()
                }
            };
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                match aws.list_events_since(since_ms, EVENT_TAIL_POLL_BATCH).await {
                    Ok(mut events) => {
                        since_ms = next_event_watermark_ms(&events, since_ms);
                        events.reverse();
                        let _ = tx.send(AppMsg::EventTailEvents {
                            gen,
                            session_id,
                            result: Ok(events),
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::EventTailEvents {
                            gen,
                            session_id,
                            result: Err(format!("{e}")),
                        });
                        // Keep going on errors — transient throttling
                        // shouldn't kill the session.
                    }
                }
            }
        });
        self.event_tail_task = Some(handle);
    }

    /// Key handling while the `:event-tail` overlay is open — the
    /// same surface as [`handle_log_tail_key`] minus the log-group
    /// picker (there's no group to switch; the tail is fleet-wide).
    fn handle_event_tail_key(&mut self, key: KeyEvent) {
        let Some(Overlay::EventTail { view, events, .. }) = self.current_overlay.as_mut() else {
            return;
        };
        let outcome = tail::handle_tail_key(view, key);
        // Same scroll ceiling as the log tail — `g`'s u16::MAX
        // sentinel must not leave `j` dead for thousands of presses.
        view.scroll = view.scroll.min(events.len() as u16);
        if outcome == tail::TailKeyOutcome::Close {
            // Reap so a late `EventTailOpened` from the aborted task
            // can't re-open the dismissed overlay.
            tail::reap_tail_task(&mut self.event_tail_task, &mut self.event_tail_session);
            self.current_overlay = None;
        }
    }

    /// Dispatch an `UpdateEnvironment(option_settings)` call. Used by the
    /// three "tweak one or two settings" commands (`:logs-stream`, `:notify`,
    /// `:managed-window`); each pushes its own pending row + audit entry
    /// then funnels through here. `summary` is the human-readable label
    /// that ends up in the toast and the pending panel.
    pub(crate) fn spawn_option_settings_update(
        &mut self,
        summary: String,
        to_set: Vec<(String, String, String)>,
        to_remove: Vec<(String, String)>,
    ) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, &summary) {
            return;
        }
        if to_set.is_empty() && to_remove.is_empty() {
            self.error_message = Some(format!(
                "{summary}: nothing to do (no options to set or remove)"
            ));
            return;
        }
        let env_name = env.name.clone();
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "UpdateOptionSettings",
            env_name.as_str(),
            &[("summary", summary.as_str())],
        );
        self.push_pending(summary.clone(), env_name.clone());
        // No status_message ack here — the pending-actions pill in the
        // header (`⏳ N`) is the truth-source for in-flight work, and a
        // status_message ack would just race with whatever the operator
        // last set there. Completion fires a Success / Error toast.
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        let summary_for_msg = summary.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        // Capture inputs for the optional :undo round-trip — the
        // option-settings fetch happens BEFORE the write so we can
        // record the prior state and offer a clean reverse-action.
        let app_for_undo = env.application.clone();
        let env_for_undo = env_name.clone();
        let summary_for_undo = summary.clone();
        let to_set_for_undo = to_set.clone();
        let to_remove_for_undo = to_remove.clone();
        tokio::spawn(async move {
            // Fetch current option-settings for the affected keys
            // BEFORE the write so we can build the reverse-action.
            // Read failure doesn't block the write — undo is a
            // safety net, not a correctness invariant.
            let undo_entry = match aws
                .fetch_env_option_settings(&app_for_undo, &env_for_undo)
                .await
            {
                Ok(opts) => Some(build_undo_entry(
                    &env_for_undo,
                    &summary_for_undo,
                    &to_set_for_undo,
                    &to_remove_for_undo,
                    &opts,
                )),
                Err(_) => None,
            };
            let result = aws
                .update_env_option_settings(&env_for_msg, &to_set, &to_remove)
                .await
                .map_err(|e| flatten_err("update_env_option_settings", e));
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                "UpdateOptionSettings",
                &env_for_msg,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[("summary", &summary_for_msg)],
            );
            // Only record undo on a successful write — otherwise
            // `:undo` would "revert" a write that never landed.
            if result.is_ok() {
                if let Some(entry) = undo_entry {
                    let _ = tx.send(AppMsg::UndoCaptured { gen, entry });
                }
            }
            let _ = tx.send(AppMsg::OptionSettingsUpdate {
                gen,
                env_name: env_for_msg,
                summary: summary_for_msg,
                result,
            });
        });
    }

    /// Register a new application version pointing at an existing S3
    /// object, and optionally deploy it. Skips the local-read +
    /// storage-location + put_object steps that `spawn_deploy_from_local`
    /// does. Useful when the bundle is already in S3 — most CI pipelines
    /// upload artifacts to S3 themselves.
    fn spawn_deploy_from_s3(
        &mut self,
        bucket: String,
        key: String,
        explicit_label: Option<String>,
        description: Option<String>,
        and_deploy: bool,
    ) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, "deploy-from-s3") {
            return;
        }
        // Derive label from the S3 key's basename if not pinned. Same
        // convention as the local-path flow so the audit log + version list
        // are consistent across the two sources.
        let label = explicit_label
            .unwrap_or_else(|| derive_version_label(&key, chrono::Utc::now().timestamp()));
        let env_name = env.name.clone();
        let app_name = env.application.clone();
        let summary = if and_deploy {
            format!("deploy-from-s3 {label}")
        } else {
            format!("create-version-from-s3 {label}")
        };
        let source_s3 = format!("s3://{bucket}/{key}");
        let and_deploy_str = and_deploy.to_string();
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "DeployFromS3",
            env_name.as_str(),
            &[
                ("label", label.as_str()),
                ("source", source_s3.as_str()),
                ("and_deploy", and_deploy_str.as_str()),
            ],
        );
        self.push_pending(summary.clone(), env_name.clone());
        // In-flight ack lives on the pending pill; completion toasts.
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        let label_for_msg = label.clone();
        let summary_for_msg = summary.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        let description_owned = description;
        tokio::spawn(async move {
            if let Err(e) = aws
                .create_app_version(
                    &app_name,
                    &label_for_msg,
                    description_owned.as_deref(),
                    &bucket,
                    &key,
                )
                .await
            {
                let err = format!("create-version: {}", flatten_err("create_app_version", e));
                finish_deploy_from_local(
                    &tx,
                    gen,
                    env_for_msg,
                    label_for_msg,
                    summary_for_msg,
                    account.as_deref(),
                    profile.as_deref(),
                    &region,
                    Err(err),
                );
                return;
            }
            if and_deploy {
                if let Err(e) = aws.deploy_version(&env_for_msg, &label_for_msg).await {
                    let err = format!("deploy: {}", flatten_err("deploy_version", e));
                    finish_deploy_from_local(
                        &tx,
                        gen,
                        env_for_msg,
                        label_for_msg,
                        summary_for_msg,
                        account.as_deref(),
                        profile.as_deref(),
                        &region,
                        Err(err),
                    );
                    return;
                }
            }
            finish_deploy_from_local(
                &tx,
                gen,
                env_for_msg,
                label_for_msg,
                summary_for_msg,
                account.as_deref(),
                profile.as_deref(),
                &region,
                Ok(()),
            );
        });
    }

    /// Upload a local bundle to EB's managed S3 storage, register a new
    /// application version pointing at it, and optionally deploy it to the
    /// selected env. The chain runs serially in one spawned task; failures
    /// at any stage surface as a single error toast with the stage name.
    /// Fetch the candidate version's metadata + the currently-deployed
    /// version's metadata for the env's app, render a preview text, and
    /// land it as a TextOverlay. EB application versions carry only a
    /// label + description + source-bundle S3 pointer + created date;
    /// there's no option-settings diff to surface (settings live on the
    /// env, not the version). So the preview is "informed deploy" —
    /// label, age, description, plus a warning when the candidate is
    /// older than what's currently deployed.
    fn spawn_deploy_preview(&self, env: crate::aws::Environment, label: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        let current_label = env.version_label.clone();
        tokio::spawn(async move {
            let body = match aws.list_application_versions(&app_name).await {
                Ok(versions) => format_deploy_preview(&env_name, &current_label, &label, &versions),
                Err(e) => format!(
                    "deploy preview — failed to fetch application versions:\n  {}\n",
                    flatten_err_to_string(&e)
                ),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("deploy preview — {env_name} ← {label}"),
                body,
            });
        });
    }

    fn spawn_deploy_from_local(
        &mut self,
        path: String,
        explicit_label: Option<String>,
        description: Option<String>,
        and_deploy: bool,
    ) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        // Route through `deny_write` (not bare `is_read_only_for`) so the
        // gate picks up the `--demo` refusal alongside read-only / safety
        // pins. Pre-0.17.4 this dispatched real audit lines in --demo.
        if self.deny_write(&env.name, "deploy-from-local") {
            return;
        }
        // Path resolution: ~ expansion + check file exists + size.
        // The bundle is streamed from disk by the AWS layer, not slurped
        // here — keeps RAM flat regardless of bundle size and lets the
        // multipart path handle anything above MULTIPART_THRESHOLD.
        let resolved = expand_tilde(&path);
        let resolved_path = std::path::PathBuf::from(&resolved);
        let size = match std::fs::metadata(&resolved_path) {
            Ok(m) => m.len(),
            Err(e) => {
                self.error_message = Some(format!("can't read {resolved}: {e}"));
                return;
            }
        };
        if size == 0 {
            self.error_message = Some(format!("{resolved} is empty"));
            return;
        }
        // Derive label if the operator didn't pin one. We use the filename
        // basename + a unix timestamp so re-deploys don't collide.
        let label = explicit_label
            .unwrap_or_else(|| derive_version_label(&resolved, chrono::Utc::now().timestamp()));
        let env_name = env.name.clone();
        let app_name = env.application.clone();
        let summary = if and_deploy {
            format!("deploy-from-local {label}")
        } else {
            format!("upload-version {label}")
        };
        let size_str = size.to_string();
        let and_deploy_str = and_deploy.to_string();
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "DeployFromLocal",
            env_name.as_str(),
            &[
                ("label", &label),
                ("bytes", &size_str),
                ("and_deploy", &and_deploy_str),
            ],
        );
        self.push_pending(summary.clone(), env_name.clone());
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        let label_for_msg = label.clone();
        let summary_for_msg = summary.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        let description_owned = description;
        tokio::spawn(async move {
            // Three (or four) stages: bucket → put → create version → (deploy).
            // We surface the stage name in any error so the operator knows
            // where it failed.
            let bucket = match aws.create_storage_location().await {
                Ok(b) => b,
                Err(e) => {
                    let err = format!(
                        "storage-location: {}",
                        flatten_err("create_storage_location", e)
                    );
                    finish_deploy_from_local(
                        &tx,
                        gen,
                        env_for_msg,
                        label_for_msg,
                        summary_for_msg,
                        account.as_deref(),
                        profile.as_deref(),
                        &region,
                        Err(err),
                    );
                    return;
                }
            };
            // Key: `applications/<app>/<label>` mirrors EB's own layout.
            let key = format!("applications/{app_name}/{label_for_msg}");
            if let Err(e) = aws.upload_bundle(&bucket, &key, &resolved_path).await {
                let err = format!("s3-put: {}", flatten_err("upload_bundle", e));
                finish_deploy_from_local(
                    &tx,
                    gen,
                    env_for_msg,
                    label_for_msg,
                    summary_for_msg,
                    account.as_deref(),
                    profile.as_deref(),
                    &region,
                    Err(err),
                );
                return;
            }
            if let Err(e) = aws
                .create_app_version(
                    &app_name,
                    &label_for_msg,
                    description_owned.as_deref(),
                    &bucket,
                    &key,
                )
                .await
            {
                let err = format!("create-version: {}", flatten_err("create_app_version", e));
                finish_deploy_from_local(
                    &tx,
                    gen,
                    env_for_msg,
                    label_for_msg,
                    summary_for_msg,
                    account.as_deref(),
                    profile.as_deref(),
                    &region,
                    Err(err),
                );
                return;
            }
            if and_deploy {
                if let Err(e) = aws.deploy_version(&env_for_msg, &label_for_msg).await {
                    let err = format!("deploy: {}", flatten_err("deploy_version", e));
                    finish_deploy_from_local(
                        &tx,
                        gen,
                        env_for_msg,
                        label_for_msg,
                        summary_for_msg,
                        account.as_deref(),
                        profile.as_deref(),
                        &region,
                        Err(err),
                    );
                    return;
                }
            }
            finish_deploy_from_local(
                &tx,
                gen,
                env_for_msg,
                label_for_msg,
                summary_for_msg,
                account.as_deref(),
                profile.as_deref(),
                &region,
                Ok(()),
            );
        });
    }

    /// Dispatch a `DeleteApplicationVersion` for the selected env's app.
    /// `force` also requests `DeleteSourceBundle=true` so the underlying
    /// `.zip` is removed from the env's storage bucket.
    fn spawn_delete_app_version(&mut self, label: String, force: bool) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, "delete-version") {
            return;
        }
        let application = env.application.clone();
        // Target carries the optional "(+source bundle)" suffix so a
        // tail of the audit log makes the force flag visible without
        // a separate extras field.
        let target_label = if force {
            format!("{application}/{label} (+source bundle)")
        } else {
            format!("{application}/{label}")
        };
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "DeleteAppVersion",
            &target_label,
            &[],
        );
        // In-flight ack lives on the pending pill; completion toasts.
        let pending_label = if force {
            "Delete app version (+source)"
        } else {
            "Delete app version"
        };
        let pending_target = format!("{application}/{label}");
        self.push_pending(pending_label, pending_target);
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        let app_for_msg = application.clone();
        let label_for_msg = label.clone();
        let target_label_for_outcome = target_label.clone();
        tokio::spawn(async move {
            let result = aws
                .delete_application_version(&application, &label, force)
                .await
                .map_err(|e| flatten_err("delete_application_version", e));
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                "DeleteAppVersion",
                &target_label_for_outcome,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[],
            );
            let _ = tx.send(AppMsg::DeleteAppVersion {
                gen,
                application: app_for_msg,
                label: label_for_msg,
                force,
                result,
            });
        });
    }

    /// Key handler for the interactive saved-configs overlay. Cursor moves
    /// with j/k/arrows/g/G; `a` applies the selected template to the current
    /// env (via `apply_config_template`); `x` deletes it; `c` closes the
    /// overlay and prefills `:config-save ` so the user can type a name; `?`
    /// stashes the overlay and surfaces the SavedConfigs help topic — closing
    /// help restores the overlay.
    fn handle_saved_configs_interactive_key(&mut self, key: KeyEvent) {
        // Mutate cursor in-place for navigation keys, then return early; for
        // dispatch keys (a/x/c) extract the selected pair, clear the overlay,
        // and re-enter the existing command path so we inherit read-only
        // gating + audit trail + ActionResult plumbing.
        {
            let Some(Overlay::SavedConfigsInteractive {
                items,
                cursor,
                confirm_delete,
            }) = self.current_overlay.as_mut()
            else {
                return;
            };
            if items.is_empty() {
                self.current_overlay = None;
                return;
            }
            let len = items.len();
            // When the delete confirm is armed, only y/Y/enter and n/N/esc do
            // anything — navigation keys are inert so a stray j/k doesn't
            // discard the confirm state and reset the cursor.
            if *confirm_delete {
                match key.code {
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        *confirm_delete = false;
                        return;
                    }
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        // Fall through to the dispatch block below.
                    }
                    _ => return,
                }
            } else {
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        *cursor = (*cursor + 1).min(len.saturating_sub(1));
                        return;
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        *cursor = cursor.saturating_sub(1);
                        return;
                    }
                    KeyCode::Char('g') | KeyCode::Home => {
                        *cursor = 0;
                        return;
                    }
                    KeyCode::Char('G') | KeyCode::End => {
                        *cursor = len.saturating_sub(1);
                        return;
                    }
                    KeyCode::Char('x') => {
                        *confirm_delete = true;
                        return;
                    }
                    _ => {}
                }
            }
        }
        let Some(Overlay::SavedConfigsInteractive {
            items,
            cursor,
            confirm_delete,
        }) = self.current_overlay.as_ref()
        else {
            return;
        };
        let cursor = *cursor;
        let confirm_delete = *confirm_delete;
        let selected = items.get(cursor).cloned();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.current_overlay = None;
            }
            KeyCode::Char('a') | KeyCode::Enter if !confirm_delete => {
                if let Some((_app, template)) = selected {
                    self.current_overlay = None;
                    let Some(env) = self.selected_env().cloned() else {
                        self.error_message = Some(
                            "no env selected — press 1-9, click a row, or type ' to jump by name"
                                .into(),
                        );
                        return;
                    };
                    // Direct call bypasses execute_command's whitespace
                    // split so template names with spaces work.
                    self.spawn_config_apply_template(env.name, template);
                }
            }
            // y/Y/enter under armed-confirm dispatches the delete.
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter if confirm_delete => {
                if let Some((app_name, template)) = selected {
                    self.current_overlay = None;
                    self.spawn_config_delete_template(app_name, template);
                }
            }
            KeyCode::Char('c') => {
                self.current_overlay = None;
                self.command_input = "config-save ".into();
                self.mode = Mode::Command;
            }
            KeyCode::Char('i') => {
                // Inspect: close the interactive overlay and dispatch
                // config-inspect directly. Template name may contain spaces
                // (e.g. "Dev config pre-redis") — direct method call avoids
                // execute_command's whitespace-split parser.
                if let Some((app_name, template)) = selected {
                    self.current_overlay = None;
                    self.spawn_config_inspect_template(app_name, template);
                }
            }
            KeyCode::Char('?') => {
                self.help.pre_overlay = self.current_overlay.take();
                self.help.pre_mode = Some(self.mode);
                self.help.topic = HelpTopic::SavedConfigs;
                self.mode = Mode::Help;
            }
            _ => {}
        }
    }

    /// Dispatch an `UpdateTagsForResource` for the selected env. `to_add`
    /// and `to_remove` follow EB semantics: the API allows both in a single
    /// call; we surface a summary toast either way.
    fn spawn_tag_update(&mut self, to_add: Vec<(String, String)>, to_remove: Vec<String>) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, "tag edits") {
            return;
        }
        let Some(arn) = env.arn.clone() else {
            self.error_message = Some(format!("env {} has no ARN — re-fetch and retry", env.name));
            return;
        };
        if to_add.is_empty() && to_remove.is_empty() {
            self.error_message =
                Some("nothing to do — provide tags to add or keys to remove".into());
            return;
        }
        let summary = if !to_add.is_empty() {
            let keys: Vec<String> = to_add.iter().map(|(k, _)| k.clone()).collect();
            format!("tag {}", keys.join(","))
        } else {
            format!("untag {}", to_remove.join(","))
        };
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "UpdateTags",
            &env.name,
            &[("summary", &summary)],
        );
        // Label intentionally carries the operation (`tag …` / `untag …`) so
        // the pending panel distinguishes simultaneous edits. The pending
        // pill in the header is the in-flight truth-source; no
        // status_message ack here (would race with the next operation).
        self.push_pending(summary.clone(), env.name.clone());
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_name = env.name.clone();
        let summary_for_msg = summary.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        tokio::spawn(async move {
            let result = aws
                .update_tags(&arn, &to_add, &to_remove)
                .await
                .map_err(|e| flatten_err("update_tags", e));
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                "UpdateTags",
                &env_name,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[("summary", &summary)],
            );
            let _ = tx.send(AppMsg::TagUpdate {
                gen,
                env_name,
                summary: summary_for_msg,
                result,
            });
        });
    }

    fn spawn_preflight_events(&mut self, env_name: String) {
        let env_for_msg = env_name.clone();
        self.spawn_aws(
            "preflight_events",
            move |aws| async move { aws.list_events_for_env(&env_name, 3).await },
            move |gen, result| AppMsg::PreflightEvents {
                gen,
                env_name: env_for_msg,
                result,
            },
        );
    }

    fn spawn_dry_run(&mut self, env_name: String) {
        let env_for_msg = env_name.clone();
        self.spawn_aws(
            "dry_run_list_instances",
            move |aws| async move { aws.list_instances(&env_name).await },
            move |gen, result| AppMsg::DryRunResult {
                gen,
                env_name: env_for_msg,
                result,
            },
        );
    }

    /// Kick off the version-list fetch for the Deploy confirm
    /// modal's inline preview. Builds the `format_deploy_preview`
    /// body off-thread so the spawn handler doesn't allocate; the
    /// handler just stuffs the rendered string into the modal.
    /// `current_label` may be empty for a brand-new env (first
    /// deploy) — the formatter handles that gracefully.
    fn spawn_version_preview(
        &mut self,
        app_name: String,
        env_name: String,
        current_label: String,
        candidate_label: String,
    ) {
        let env_for_msg = env_name.clone();
        let env_for_render = env_name.clone();
        let candidate_for_render = candidate_label.clone();
        self.spawn_aws(
            "version_preview",
            move |aws| async move { aws.list_application_versions(&app_name).await },
            move |gen, result| {
                let body = match result {
                    Ok(versions) => Ok(format_deploy_preview(
                        &env_for_render,
                        &current_label,
                        &candidate_for_render,
                        &versions,
                    )),
                    Err(e) => Err(e),
                };
                AppMsg::VersionPreview {
                    gen,
                    env_name: env_for_msg,
                    result: body,
                }
            },
        );
    }

    /// Pre-deploy health-check probe for the confirm modal. Reads
    /// the env's current `Application Healthcheck URL` option
    /// (defaults to `/` if unset), composes a probe URL against
    /// the env's CNAME, and HEADs it via curl with a 2s cap. The
    /// outcome (success / non-2xx / timeout / refusal) is just a
    /// warning surface; it doesn't block the deploy.
    ///
    /// Shells out to `curl` for the same reason `fetch_url_text`
    /// and `fire_audit_webhook` do — keeps ebman HTTP-client-dep
    /// free. Output is parsed to an HTTP status code (or classified
    /// as a transport error) so the warning text can surface what
    /// specifically failed.
    fn spawn_health_check_probe(&mut self, app_name: String, env_name: String, cname: String) {
        let env_for_msg = env_name.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            // Look up the configured health-check path. Missing or
            // empty setting means EB defaults to `/`, so we probe
            // the env root.
            let path = match aws.fetch_env_option_settings(&app_name, &env_name).await {
                Ok(opts) => opts
                    .into_iter()
                    .find(|(ns, name, _)| {
                        ns == "aws:elasticbeanstalk:application"
                            && name == "Application Healthcheck URL"
                    })
                    .map(|(_, _, v)| v)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "/".into()),
                Err(_) => "/".into(),
            };
            let url = crate::probe::build_health_check_probe_url(&cname, &path);
            let result = crate::probe::run_health_check_probe(&url).await;
            let _ = tx.send(AppMsg::HealthCheckProbe {
                gen,
                env_name: env_for_msg,
                result,
            });
        });
    }

    /// Spawn the pre-deploy unavailability estimator. Fetches the
    /// env's option-settings, extracts deployment policy + batch +
    /// ASG max, formats a one-line summary, and emits
    /// `AppMsg::UnavailabilityEstimate`. Uses its own fetch rather
    /// than piggy-backing on the health-check probe — two parallel
    /// DescribeConfigurationSettings calls is fine for a one-shot
    /// modal open, and keeps the two features isolated so failure
    /// of one doesn't taint the other.
    fn spawn_unavailability_estimate(&mut self, app_name: String, env_name: String) {
        let env_for_msg = env_name.clone();
        self.spawn_aws(
            "unavailability_estimate",
            move |aws| async move { aws.fetch_env_option_settings(&app_name, &env_name).await },
            move |gen, result| {
                let line = result.ok().map(|opts| {
                    let (policy, batch, btype, asg_max) = extract_unavailability_inputs(&opts);
                    let count = compute_unavailability_count(&policy, batch, &btype, asg_max);
                    format_unavailability_line(&policy, count, asg_max)
                });
                AppMsg::UnavailabilityEstimate {
                    gen,
                    env_name: env_for_msg,
                    line,
                }
            },
        );
    }

    /// Run the lint engine against the env at confirm-modal-open
    /// time. Same rules `:lint` and `ebman lint` use; same
    /// operator-tunable disables. Issues at `>= Warn` render as
    /// modal warning lines so the operator sees rule-keyed risk
    /// before authorising the action.
    ///
    /// Uses the same option-settings fetch path as the
    /// unavailability estimate + health-check probe, but issues
    /// its own DescribeConfigurationSettings call to keep the
    /// three features isolated. Failure of the read is non-
    /// blocking — modal renders without lint when the fetch
    /// fails (the operator can still see the rule-output via
    /// `:lint` once the modal closes).
    fn spawn_confirm_lint(&mut self, env: crate::aws::Environment) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        // Snapshot operator-tunable disables — user-level (already
        // mirrored on App) + project-local (read fresh from cwd).
        let mut disabled = self.cfg.lint_disable.clone();
        disabled.extend(crate::project::load_lint_disables_from_cwd());
        let env_for_msg = env.name.clone();
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        // Plumb the live lint-context inputs. latest_stack enables
        // EBL008; required_tags + env_tag_keys (fetched parallel below)
        // enable EBL010; dlq_depth enables EBL011; healthy instance
        // count enables EBL012. All four wire-up tracks landed in 0.18.
        let newer_stack_owned =
            crate::aws::newer_stack_version(&env.solution_stack, &self.latest_stacks);
        let required_tags_owned = self.cfg.required_tags.clone();
        // EBL011 only fires for Worker envs and only when we have a
        // cached DLQ depth (populated by the Queue tab / worker poll).
        let dlq_depth_owned = if env.tier.eq_ignore_ascii_case("Worker") {
            self.worker_dlq_depths.get(&env.name).copied()
        } else {
            None
        };
        let env_arn_owned = env.arn.clone();
        // 0.21: opportunistic lint-input cache lookup. If tags or
        // health are fresh (< LINT_INPUT_CACHE_TTL), skip the
        // corresponding fetch — modal-open latency drops from
        // `max(t_opts, t_tags, t_health)` to `t_opts` when both
        // inputs are cached (typical for repeated modal-opens
        // against the same env).
        let now = std::time::Instant::now();
        let cached_tags: Option<Vec<String>> = self
            .env_tag_cache
            .get(&env.name)
            .and_then(|(v, t)| (now.duration_since(*t) < LINT_INPUT_CACHE_TTL).then(|| v.clone()));
        let cached_health: Option<i64> = self
            .env_health_cache
            .get(&env.name)
            .and_then(|(v, t)| (now.duration_since(*t) < LINT_INPUT_CACHE_TTL).then_some(*v));
        let cache_env_name = env.name.clone();
        tokio::spawn(async move {
            // Parallel fetch: option settings (always), tags + health
            // (only if not cached). The `tags_fut` / `health_fut`
            // async blocks resolve to the cached value when fresh —
            // tokio::join! still polls all three but the cached
            // branches return immediately.
            let opts_fut = aws.fetch_env_option_settings(&app_name, &env_name);
            let tags_was_cached = cached_tags.is_some();
            let tags_fut = async {
                if let Some(cached) = cached_tags {
                    Some(cached)
                } else {
                    match env_arn_owned.as_deref() {
                        Some(arn) => aws
                            .list_tags(arn)
                            .await
                            .ok()
                            .map(|kvs| kvs.into_iter().map(|(k, _)| k).collect()),
                        None => None,
                    }
                }
            };
            let health_was_cached = cached_health.is_some();
            let health_fut = async {
                if let Some(cached) = cached_health {
                    Some(cached)
                } else {
                    aws.fetch_env_instance_counts(&env_name)
                        .await
                        .ok()
                        .map(|c| c.healthy as i64)
                }
            };
            let (opts_res, tags_opt, health_opt) = tokio::join!(opts_fut, tags_fut, health_fut);
            // Send cache-update before lint computation so the cache
            // is fresh for the NEXT modal-open even if lint logic
            // changes shape.
            if !tags_was_cached || !health_was_cached {
                let _ = tx.send(AppMsg::LintInputsCached {
                    gen,
                    env_name: cache_env_name,
                    tags: if tags_was_cached {
                        None
                    } else {
                        tags_opt.clone()
                    },
                    healthy: if health_was_cached { None } else { health_opt },
                });
            }
            let env_tag_keys_owned: Vec<String> = tags_opt.unwrap_or_default();
            let healthy_count_owned = health_opt;
            let issues = match opts_res {
                Ok(opts) => {
                    let mut ctx = crate::lint::LintContext::for_env(&env, &opts)
                        .with_required_tags(&required_tags_owned)
                        .with_env_tag_keys(&env_tag_keys_owned);
                    if let Some(newer) = newer_stack_owned.as_deref() {
                        ctx = ctx.with_newer_stack_available(newer);
                    }
                    if let Some(depth) = dlq_depth_owned {
                        ctx = ctx.with_dlq_depth(depth);
                    }
                    if let Some(count) = healthy_count_owned {
                        ctx = ctx.with_healthy_count(count);
                    }
                    let rules = crate::lint::default_rules(&disabled);
                    crate::lint::run_rules(&rules, &ctx)
                }
                Err(_) => Vec::new(),
            };
            let _ = tx.send(AppMsg::ConfirmModalLint {
                gen,
                env_name: env_for_msg,
                issues,
            });
        });
    }

    /// Open a confirm modal for an action that carries parameters (deploy
    /// version, clone target, scale min/max, …). Uses the same Y/N path as
    /// the existing Rebuild / Restart / Swap confirms so the operator sees
    /// the impact summary before authorising.
    /// Surface the selected instance's details as an `Overlay::TextDump`.
    /// Non-intrusive alternative to opening the EC2 console — operators
    /// can scan id / type / AZ / health / causes / launch age without
    /// leaving the TUI. `b` still opens the browser when needed.
    fn open_instance_info_overlay(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(inst) = d.instances.get(d.instances_cursor) else {
            self.status_message = Some("no instance selected".into());
            return;
        };
        let mut body = String::new();
        body.push_str(&format!("Instance ID       {}\n", inst.id));
        body.push_str(&format!("Type              {}\n", inst.instance_type));
        body.push_str(&format!("Availability zone {}\n", inst.availability_zone));
        body.push_str(&format!(
            "Health            {} ({})\n",
            inst.health, inst.color
        ));
        if let Some(t) = inst.launched_at {
            let age = chrono::Utc::now().signed_duration_since(t);
            body.push_str(&format!(
                "Launched          {}  (up {})\n",
                t.format("%Y-%m-%d %H:%M UTC"),
                humanize_short_age(age.to_std().unwrap_or_default())
            ));
        }
        if !inst.causes.is_empty() {
            body.push_str("\nCauses:\n");
            for c in &inst.causes {
                body.push_str(&format!("  • {c}\n"));
            }
        }
        body.push_str(
            "\nKeys: b → open in EC2 console · s → SSM shell · y → yank id · x → terminate",
        );
        self.current_overlay = Some(Overlay::TextDump {
            title: format!("instance — {}", inst.id),
            body,
        });
    }

    /// Open the currently-selected instance (in the Instances tab) in the
    /// EC2 console. No-op when no instance is selected.
    fn open_instance_in_console(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(inst) = d.instances.get(d.instances_cursor) else {
            return;
        };
        let region = self.context.region.clone();
        let id = inst.id.clone();
        let url = format!(
            "https://{region}.console.aws.amazon.com/ec2/home?region={region}#InstanceDetails:instanceId={id}"
        );
        let display = id.clone();
        let result = std::process::Command::new(if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        })
        .arg(&url)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn();
        match result {
            Ok(_) => {
                self.status_message = Some(format!("opened {display} in EC2 console"));
            }
            Err(e) => {
                self.error_message = Some(format!("could not open browser: {e}"));
            }
        }
    }

    /// Copy the currently-selected instance ID to the clipboard.
    fn yank_instance_id(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(inst) = d.instances.get(d.instances_cursor) else {
            return;
        };
        let id = inst.id.clone();
        match yank(&id) {
            Ok(()) => self.status_message = Some(format!("yanked instance id: {id}")),
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    /// Fire `ec2:TerminateInstances` for the selected instance. ASG will
    /// re-launch a replacement automatically. Goes through the same
    /// `AppMsg::ActionResult` path so the status surface stays consistent.
    fn spawn_terminate_instance(&mut self, idx: usize) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(inst) = d.instances.get(idx).cloned() else {
            return;
        };
        let env_name = d.env_name.clone();
        // Route through `deny_write` (not bare `is_read_only_for`) so the
        // gate picks up `--demo` mode alongside read-only / safety pins.
        // Pre-0.17.4 this dispatched real audit lines in --demo.
        if self.deny_write(&env_name, "terminate-instance") {
            return;
        }
        let id = inst.id.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "TerminateInstance",
            env_name.as_str(),
            &[("instance", id.as_str())],
        );
        // Pending target carries env + instance id so the operator can tell
        // simultaneous terminations apart. Label must match
        // `Action::TerminateInstance.label()` exactly so the AppMsg handler's
        // `complete_pending` finds the row.
        let target = format!("{env_name}/{id}");
        self.push_pending(Action::TerminateInstance.label(), target.clone());
        // In-flight ack lives on the pending pill; completion toasts.
        let _ = id;
        tokio::spawn(async move {
            let result = aws
                .terminate_instance(&id)
                .await
                .map_err(|e| flatten_err("terminate_instance", e));
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action: Action::TerminateInstance,
                env_name: target,
                result,
            });
        });
    }

    /// Add a row to the pending-actions panel before dispatching. Callers
    /// follow with a `tokio::spawn` that sends an `AppMsg::ActionResult`;
    /// the result handler finds the first matching unfinished row and
    /// stamps it with the outcome. Caps the list at `PENDING_CAP`.
    pub fn push_pending(&mut self, label: impl Into<String>, target: impl Into<String>) {
        if self.pending_actions.len() >= PENDING_CAP {
            self.pending_actions.pop_front();
        }
        self.pending_actions.push_back(PendingAction {
            label: label.into(),
            target: target.into(),
            started: Instant::now(),
            completed: None,
        });
    }

    /// Resolve a pending entry against an arriving `ActionResult`. Picks
    /// the oldest unfinished entry whose `(label, target)` match — the
    /// dispatch order is preserved so this is correct without IDs as long
    /// as we don't have two concurrent dispatches of the same action to the
    /// same target (a deliberate operator wouldn't do that).
    pub fn complete_pending(&mut self, label: &str, target: &str, result: Result<(), String>) {
        if let Some(entry) = self
            .pending_actions
            .iter_mut()
            .find(|e| e.completed.is_none() && e.label == label && e.target == target)
        {
            entry.completed = Some((Instant::now(), result));
        }
    }

    /// Drop completed entries older than `PENDING_COMPLETED_TTL`. Called
    /// from the run loop's per-frame housekeeping so the panel quietens
    /// after a minute of inactivity.
    pub fn expire_pending(&mut self) {
        let now = Instant::now();
        self.pending_actions.retain(|e| match e.completed {
            Some((c, _)) => now.duration_since(c) < PENDING_COMPLETED_TTL,
            None => true,
        });
    }

    fn open_parameterised_action(&mut self, action: Action, params: ParameterisedAction) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        self.open_parameterised_action_on(env, action, params);
    }

    /// Variant of `open_parameterised_action` that targets an
    /// explicit env rather than the currently-selected one. Used by
    /// commands like `:promote-env SOURCE TARGET` where the
    /// destination is named in the command itself, not implied by
    /// the table cursor. The `selected_env()`-based wrapper is the
    /// common path; this is the cursor-independent escape hatch.
    pub(crate) fn open_parameterised_action_on(
        &mut self,
        env: crate::aws::Environment,
        action: Action,
        params: ParameterisedAction,
    ) {
        if self.deny_write(&env.name, action.label()) {
            return;
        }
        // The preflight (impact preview + last-3 events) is gated by
        // `Action::wants_preflight()` — single source of truth, see
        // `mode_action.rs`. Every ConfirmModal construction site must
        // route through here so the rule can't drift.
        let wants_preflight = action.wants_preflight();
        // For Deploy with a candidate label, pull the version
        // metadata (label / age / description) and inline the
        // existing `:deploy --preview` body in the modal. Saves
        // the operator the separate `:deploy LABEL --preview`
        // round-trip.
        let wants_version_preview = action == Action::Deploy && params.deploy_version.is_some();
        let modal = ConfirmModal {
            action,
            target_env: env.name.clone(),
            swap_with: params.swap_with,
            typed: TextInput::new(),
            kind: ConfirmKind::YesNo,
            dryrun: None,
            loading_dryrun: wants_preflight,
            recent_events: None,
            loading_events: wants_preflight,
            // Skip traffic_warning for SsmRun — operators frequently
            // run diagnostic shells on Red envs (it's how they diagnose
            // Red), so showing "env is currently Red" in the modal
            // would suggest they shouldn't dispatch the very command
            // they're using to investigate. Other write actions get
            // the warning because Red+write is genuinely risky.
            // (0.17.4 review)
            traffic_warning: if action == Action::SsmRun {
                None
            } else {
                compute_traffic_warning(&env)
            },
            deploy_version: params.deploy_version.clone(),
            upgrade_platform_arn: params.upgrade_platform_arn,
            upgrade_platform_label: params.upgrade_platform_label,
            clone_target: params.clone_target,
            scale_min: params.scale_min,
            scale_max: params.scale_max,
            auto_rollback_secs: params.auto_rollback_secs,
            wait_for_green_secs: params.wait_for_green_secs,
            version_preview: None,
            loading_version_preview: wants_version_preview,
            health_check_probe: None,
            // Pre-deploy health-check probe only runs for Deploy
            // confirms — we want to warn the operator if the env's
            // current health-check-url is dead BEFORE they ship a
            // new build over it. For non-Deploy actions, the probe
            // is meaningless. Skipped in `--demo` mode because the
            // synthetic CNAMEs would always fail DNS and pollute
            // screencasts with a fake-warning red herring.
            loading_health_check: wants_version_preview && !env.cname.is_empty() && !self.demo_mode,
            unavailability_line: None,
            // Same gate as the health-check probe — only useful for
            // Deploy confirms; --demo mode skips the AWS call.
            loading_unavailability: wants_version_preview && !self.demo_mode,
            lint_issues: None,
            // Lint at confirm time runs against every confirm modal,
            // not just Deploy. The health-check probe + unavailability
            // pill specialise on deploys; lint is universal — operator
            // sees AllAtOnce / health-check-empty / cooldown-low etc.
            // before confirming any destructive action. Skipped in
            // demo mode (same gate as the other probes — no AWS
            // round-trip in demo).
            //
            // SsmRun also skips lint — running an ad-hoc shell command
            // isn't gated by EB-config-health rules; firing them here
            // would be noise.
            loading_lint: !self.demo_mode && action != Action::SsmRun,
            ssm_run_command: params.ssm_run_command,
            ssm_run_instances: params.ssm_run_instances,
        };
        let needs_health_check_probe = modal.loading_health_check;
        let needs_unavailability = modal.loading_unavailability;
        let needs_lint = modal.loading_lint;
        self.action_flow = Some(ActionFlow::Confirm(modal));
        self.mode = Mode::Action;
        if wants_preflight {
            self.spawn_dry_run(env.name.clone());
            self.spawn_preflight_events(env.name.clone());
        }
        if wants_version_preview {
            if let Some(label) = params.deploy_version {
                self.spawn_version_preview(
                    env.application.clone(),
                    env.name.clone(),
                    env.version_label.clone(),
                    label,
                );
            }
        }
        if needs_health_check_probe {
            self.spawn_health_check_probe(
                env.application.clone(),
                env.name.clone(),
                env.cname.clone(),
            );
        }
        if needs_unavailability {
            self.spawn_unavailability_estimate(env.application.clone(), env.name.clone());
        }
        if needs_lint {
            self.spawn_confirm_lint(env.clone());
        }
    }

    /// Fetch `list_compatible_platforms` for `env` and surface them in an
    /// overlay so the user can copy the desired ARN into `:upgrade <arn>`.
    fn spawn_list_compatible_platforms(&mut self, env_name: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!(
            "fetching compatible platform versions for {env_name}…"
        ));
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .list_compatible_platforms(&env_name)
                .await
                .map_err(|e| flatten_err("list_compatible_platforms", e));
            let body = match result {
                Ok(p) if p.is_empty() => {
                    format!("No compatible platform versions found for {env_for_msg}.\n\nesc / q to close")
                }
                Ok(platforms) => {
                    let mut lines: Vec<String> = vec![
                        format!("Compatible platform versions for {env_for_msg}"),
                        "─────────────────────────────────────────────".into(),
                        String::new(),
                    ];
                    for p in platforms.iter().take(20) {
                        lines.push(format!(
                            "  v{}  {}  ({}, {})",
                            p.version, p.branch, p.status, p.lifecycle
                        ));
                        lines.push(format!("      {}", p.arn));
                    }
                    lines.push(String::new());
                    lines.push(
                        "Copy an ARN and run `:upgrade <ARN>` to migrate. esc / q to close".into(),
                    );
                    lines.join("\n")
                }
                Err(e) => format!("upgrade list failed: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("compatible platforms — {env_for_msg}"),
                body,
            });
        });
    }

    /// Queue a single-env action with the cancel window. Called from
    /// the Y / TypeName-confirm paths in `handle_action_key`.
    fn queue_action_dispatch(&mut self, modal: ConfirmModal) {
        // SsmRun bypasses the 5s cancel-window — operators using it for
        // diagnostic probes (`:ssm-run "uptime"`) expect immediate
        // dispatch; the 5s wait for the undo window was a 0.17.3
        // regression. The Y press itself is the gate; the modal already
        // surfaces command + fan-out count + env before dispatch.
        // Other destructive actions keep the cancel window for the
        // explicit "I just pressed Y, oh no" rescue path.
        if modal.action == Action::SsmRun {
            self.spawn_action(modal);
            return;
        }
        if self.pending_dispatch.is_some() {
            self.error_message = Some(
                "another action is mid-dispatch — wait for it to land or press U to undo".into(),
            );
            return;
        }
        let label = modal.action.label().to_string();
        let target = modal.target_env.clone();
        let deadline = Instant::now() + UNDO_WINDOW;
        self.pending_dispatch = Some(PendingDispatch {
            deadline,
            label: label.clone(),
            target: target.clone(),
            kind: PendingDispatchKind::Single { modal },
        });
        self.status_message = Some(format!(
            "{} → {} dispatches in {}s — press U to undo",
            label,
            target,
            UNDO_WINDOW.as_secs()
        ));
    }

    /// Queue a batch dispatch with the same cancel window. Caller
    /// resolves the kind + display labels (e.g. `"Batch rebuild"` /
    /// `"5 envs"`) before invoking. One-at-a-time rule shared with
    /// `queue_action_dispatch`.
    pub(crate) fn queue_batch_dispatch(
        &mut self,
        label: String,
        target: String,
        kind: PendingDispatchKind,
    ) {
        if self.pending_dispatch.is_some() {
            self.error_message = Some(
                "another dispatch is mid-window — wait for it to land or press U to undo".into(),
            );
            return;
        }
        let deadline = Instant::now() + UNDO_WINDOW;
        let status = format!(
            "{} → {} dispatches in {}s — press U to undo",
            label,
            target,
            UNDO_WINDOW.as_secs()
        );
        self.pending_dispatch = Some(PendingDispatch {
            deadline,
            label,
            target,
            kind,
        });
        self.status_message = Some(status);
    }

    /// Per-tick check called from the main loop. Fires whatever
    /// dispatch is queued when its cancel window expires. The
    /// per-variant dispatch re-uses the same helpers the immediate
    /// path used to call (`spawn_action`, `spawn_batch_*`), so audit
    /// log + pending pill + toast plumbing carry over unchanged.
    fn tick_pending_dispatch(&mut self) {
        let now = Instant::now();
        let Some(pd) = self.pending_dispatch.as_ref() else {
            return;
        };
        if now < pd.deadline {
            return;
        }
        let kind = pd.kind.clone();
        self.pending_dispatch = None;
        // `--demo` mode refuses ANY dispatch from the pending queue —
        // covers the batch variants that bypass `spawn_action`'s own
        // demo gate (Single dispatches still hit that guard too, so
        // both paths land on the same refusal toast). See spawn_action
        // for the rationale.
        if self.demo_mode {
            self.error_message = Some("demo mode — writes are inert; press q to exit".into());
            return;
        }
        match kind {
            PendingDispatchKind::Single { modal } => self.spawn_action(modal),
            PendingDispatchKind::BatchAction { action, env_names } => {
                for env in env_names {
                    self.spawn_batch_action(action, env);
                }
            }
            PendingDispatchKind::BatchDeploy {
                env_names,
                version_label,
            } => {
                for env in env_names {
                    self.spawn_batch_deploy(env, version_label.clone());
                }
            }
            PendingDispatchKind::BatchTag {
                envs_with_arns,
                key,
                value,
            } => {
                for (env, arn) in envs_with_arns {
                    self.spawn_batch_tag(env, arn, key.clone(), value.clone());
                }
            }
            PendingDispatchKind::BatchSetOption {
                env_names,
                namespace,
                option_name,
                value,
            } => {
                for env in env_names {
                    self.spawn_batch_set_option(
                        env,
                        namespace.clone(),
                        option_name.clone(),
                        value.clone(),
                    );
                }
            }
        }
    }

    /// Cancel the pending dispatch (bound to `U` in Normal mode).
    /// Audit-logs the cancel + emits a status toast. Silent abort
    /// would feel like a missed keypress.
    fn cancel_pending_dispatch(&mut self) {
        let Some(pd) = self.pending_dispatch.take() else {
            return;
        };
        let msg = format!("undone — {} → {} not dispatched", pd.label, pd.target);
        let action_for_audit = match &pd.kind {
            PendingDispatchKind::Single { modal } => format!("{:?}", modal.action),
            PendingDispatchKind::BatchAction { action, .. } => format!("Batch{action:?}"),
            PendingDispatchKind::BatchDeploy { .. } => "BatchDeploy".into(),
            PendingDispatchKind::BatchTag { value, .. } => {
                if value.is_some() {
                    "BatchTag".into()
                } else {
                    "BatchUntag".into()
                }
            }
            PendingDispatchKind::BatchSetOption { .. } => "BatchSetOption".into(),
        };
        crate::audit::append_action_undone(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &action_for_audit,
            &pd.target,
        );
        self.status_message = Some(msg);
    }

    fn spawn_action(&mut self, modal: ConfirmModal) {
        // `--demo` mode runs against a synthetic fleet on a fake
        // AwsClient. Destructive dispatch against that client would
        // fail at the SDK layer but still write `stage=dispatched`
        // audit lines to the real audit log. Refuse outright so a
        // fat-fingered keypress during a demo / screencast doesn't
        // touch operator state.
        if self.demo_mode {
            self.error_message = Some(format!(
                "demo mode — {} not dispatched (writes are inert; press q to exit)",
                modal.action.label()
            ));
            return;
        }
        // Per-env / per-account read-only locks short-circuit the
        // dispatch before any AWS call. `read_only_reason` returns
        // the specific cause (global toggle vs. config-pinned env vs.
        // pinned account) so the toast tells the operator exactly
        // which knob is keeping them safe.
        if self.is_read_only_for(&modal.target_env) {
            let reason = self
                .read_only_reason(&modal.target_env)
                .unwrap_or_else(|| "read-only mode".into());
            self.error_message = Some(format!("{reason} — {} disabled", modal.action.label()));
            return;
        }
        // For Deploy actions: snapshot the env's pre-deploy version
        // label before dispatching, so :rollback-deploy and the
        // optional `--auto-rollback Nm` watchdog know what to roll
        // back TO. Skip if we don't have the env in our cached
        // fleet (e.g. assume-role race where the modal opened
        // before the refresh landed) — the existing :rollback can
        // scan events as a fallback.
        if modal.action == Action::Deploy {
            if let Some(env) = self
                .environments
                .iter()
                .find(|e| e.name == modal.target_env)
            {
                if !env.version_label.is_empty() {
                    self.deploy_snapshots.insert(
                        env.name.clone(),
                        DeploySnapshot {
                            env_name: env.name.clone(),
                            previous_version_label: env.version_label.clone(),
                            taken_at: chrono::Utc::now(),
                        },
                    );
                }
            }
            // Arm the auto-rollback watchdog if requested. Two signals
            // can fire: the env reaching Green on the next refresh
            // tick (early disarm via apply_refresh — most common
            // outcome) or the deadline timer firing AutoRollbackCheck.
            // `armed_watchdogs` carries the in-flight state for both
            // surfaces.
            if let Some(secs) = modal.auto_rollback_secs {
                let tx = self.msg_tx.clone();
                let env_name = modal.target_env.clone();
                let gen = self.generation;
                // Snapshot the rollback target now so the watchdog
                // doesn't have to re-look it up later. The pre-deploy
                // snapshot we just inserted is the source of truth.
                let target_label = self
                    .deploy_snapshots
                    .get(&modal.target_env)
                    .map(|s| s.previous_version_label.clone())
                    .unwrap_or_default();
                let armed_at = chrono::Utc::now();
                let deadline_at = armed_at + chrono::Duration::seconds(secs as i64);
                self.armed_watchdogs.insert(
                    modal.target_env.clone(),
                    ArmedWatchdog {
                        env_name: modal.target_env.clone(),
                        target_label,
                        armed_at,
                        deadline_at,
                    },
                );
                self.status_message = Some(format!(
                    "auto-rollback armed: {secs}s to reach Green or revert"
                ));
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    let _ = tx.send(AppMsg::AutoRollbackCheck { gen, env_name });
                });
            }
            // Arm the wait-for-green tracker if requested. Pure
            // observability — `apply_refresh` watches `watching_deploys`
            // and pins the outcome (success on Green, error on timeout).
            // No tokio task needed: apply_refresh runs on every refresh
            // tick anyway and checks deadlines there. Orthogonal to
            // auto-rollback so both flags can coexist.
            if let Some(secs) = modal.wait_for_green_secs {
                let target_label = modal.deploy_version.clone().unwrap_or_default();
                let armed_at = chrono::Utc::now();
                let deadline_at = armed_at + chrono::Duration::seconds(secs as i64);
                self.watching_deploys.insert(
                    modal.target_env.clone(),
                    WatchingDeploy {
                        env_name: modal.target_env.clone(),
                        target_label,
                        armed_at,
                        deadline_at,
                    },
                );
                // Don't clobber a "auto-rollback armed" message if
                // both flags were set — append instead.
                if let Some(existing) = self.status_message.as_mut() {
                    existing.push_str(&format!("; watching for Green ({secs}s)"));
                } else {
                    self.status_message = Some(format!(
                        "watching deploy: {secs}s to reach Green or report timeout"
                    ));
                }
            }
        }
        // SsmRun's dispatch shape doesn't fit the standard
        // `Result<(), _>` + `ActionResult` pipeline below — the SDK
        // call returns per-instance rows that surface in a
        // TextOverlay. Short-circuit to a dedicated helper that
        // carries the audit / pending / spawn dance with the right
        // payload. Everything ABOVE this point (read-only check, demo
        // guard, deploy-only auto-rollback / wait-for-green arming —
        // none of which fires for SsmRun) still runs uniformly.
        if modal.action == Action::SsmRun {
            let command = modal.ssm_run_command.clone().unwrap_or_default();
            let instances = modal.ssm_run_instances.clone().unwrap_or_default();
            self.spawn_ssm_run_impl(modal.target_env.clone(), command, instances);
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let action = modal.action;
        let env = modal.target_env.clone();
        let swap_with = modal.swap_with.clone();
        let deploy_version = modal.deploy_version.clone();
        let upgrade_arn = modal.upgrade_platform_arn.clone();
        let clone_target = modal.clone_target.clone();
        let scale_min = modal.scale_min;
        let scale_max = modal.scale_max;
        write_audit_entry(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            action,
            &env,
            swap_with.as_deref(),
        );
        self.push_pending(action.label(), env.clone());
        tokio::spawn(async move {
            let result = match action {
                Action::Rebuild => aws.rebuild_env(&env).await,
                Action::RestartAppServer => aws.restart_app_server(&env).await,
                Action::Terminate => aws.terminate_env(&env).await,
                Action::SwapCnames => match swap_with {
                    Some(dest) => aws.swap_cnames(&env, &dest).await,
                    None => Err(color_eyre::eyre::eyre!("swap target missing")),
                },
                Action::Deploy => match deploy_version {
                    Some(ver) => aws.deploy_version(&env, &ver).await,
                    None => Err(color_eyre::eyre::eyre!("deploy version missing")),
                },
                Action::UpgradePlatform => match upgrade_arn {
                    Some(arn) => aws.upgrade_platform(&env, &arn).await,
                    None => Err(color_eyre::eyre::eyre!("upgrade platform ARN missing")),
                },
                Action::Clone => match clone_target {
                    Some(target) => aws.clone_env(&env, &target).await,
                    None => Err(color_eyre::eyre::eyre!("clone target name missing")),
                },
                Action::Scale => match (scale_min, scale_max) {
                    (Some(mn), Some(mx)) => aws.scale_env(&env, mn, mx).await,
                    _ => Err(color_eyre::eyre::eyre!("scale min/max missing")),
                },
                Action::AbortUpdate => aws.abort_environment_update(&env).await,
                // Capacity opens a modal form (cmd_capacity) and dispatches
                // via spawn_option_settings_update — it never reaches
                // spawn_action's ConfirmModal path. Same for Config* and
                // TerminateInstance which have dedicated spawn paths.
                // SsmRun is short-circuited above via spawn_ssm_run_impl
                // and never reaches this match — included here for
                // exhaustiveness with a defensive error in case the
                // short-circuit is ever removed.
                Action::Capacity
                | Action::ConfigSave
                | Action::ConfigDelete
                | Action::ConfigApply
                | Action::TerminateInstance
                | Action::SsmRun => Err(color_eyre::eyre::eyre!(
                    "internal: {} dispatched through spawn_action path",
                    action.label()
                )),
            }
            .map_err(|e| flatten_err("action", e));
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action,
                env_name: env,
                result,
            });
        });
    }

    /// Dispatch helper for `Action::SsmRun` — split from `spawn_action`
    /// because the SDK call returns per-instance rows that surface in a
    /// `TextOverlay`, not the `Result<(), String>` shape every other
    /// action variant uses. Carries the audit "dispatched" line + a
    /// status toast + the spawn + the audit "completed" line on
    /// arrival.
    ///
    /// **Deliberately bypasses `push_pending`** (the header `⏳ N` chip
    /// and `:pending` overlay). The 0.17.4 code-review flagged this as
    /// an invariant break — every other write-class dispatch shows in
    /// pending. SsmRun is the exception: it's a one-shot diagnostic
    /// probe with a 60s hard cap, the result lands in a TextOverlay
    /// (full-screen — operator can't miss it), and pending entries
    /// would mostly survive the SSM run as `…dispatching` because the
    /// overlay lands before the operator looks back at the bar.
    /// `:ssm-run` doesn't touch EB state so it doesn't need to
    /// participate in the "what writes are in-flight against the
    /// fleet" surface the pending pill exists to provide.
    fn spawn_ssm_run_impl(&mut self, env_name: String, command: String, instances: Vec<String>) {
        if command.is_empty() || instances.is_empty() {
            // Defensive — `cmd_ssm_run` should have refused before
            // opening the modal. If we ever land here with empty
            // payload, abort cleanly rather than dispatching a
            // pointless command.
            self.error_message = Some("ssm-run: empty command or no resolved instances".into());
            return;
        }
        // Audit-log the dispatch + the completion outcome. SSM
        // commands can mutate state; treating them as write-class
        // operations means an after-the-fact incident review can pin
        // down "who ran what, when, on which env" by tailing
        // ~/.cache/ebman/audit.log. The command string is escaped so
        // quotes don't break the line shape.
        let audit_cmd = command.replace('"', "'");
        let instances_str = instances.len().to_string();
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            // Use the Debug-format name (`SsmRun`) for audit consistency
            // with cancel_pending_dispatch's UNDONE line and with every
            // other Action variant (`Rebuild`, `Terminate`, …). Pre-
            // 0.17.4 this was the literal "SsmRunCommand" which broke
            // grep-by-action correlation across dispatched/cancelled
            // stages. 0.17.3 audit consumers that grepped for
            // "SsmRunCommand" need to switch to "SsmRun".
            "SsmRun",
            env_name.as_str(),
            &[("instances", &instances_str), ("cmd", &audit_cmd)],
        );
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let command_for_render = command.clone();
        let n = instances.len();
        // Snapshot context for the completion-stage audit line.
        let audit_account = self.context.account_id.clone();
        let audit_profile = self.context.profile.clone();
        let audit_region = self.context.region.clone();
        let audit_env = env_name.clone();
        let audit_cmd_for_outcome = audit_cmd.clone();
        self.status_message = Some(format!("running `{command}` on {n} instance(s)…"));
        tokio::spawn(async move {
            let result = aws
                .run_shell_command(&instances, &command, 60)
                .await
                .map_err(|e| flatten_err("run_shell_command", e));
            let ok_count_str = match &result {
                Ok(rows) => {
                    let oks = rows.iter().filter(|r| r.status == "Success").count();
                    format!("{oks}/{n}")
                }
                Err(_) => format!("0/{n}"),
            };
            crate::audit::append_action_completed(
                audit_account.as_deref(),
                audit_profile.as_deref(),
                &audit_region,
                "SsmRun", // canonical name — see dispatched-stage comment above
                &audit_env,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[("ok_count", &ok_count_str), ("cmd", &audit_cmd_for_outcome)],
            );
            let body = match result {
                Ok(rows) => format_ssm_results(&command_for_render, &rows),
                Err(e) => format!("ssm-run: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: "ssm-run".into(),
                body,
            });
        });
    }

    fn execute_command(&mut self, raw: &str) {
        let line = raw.trim();
        if line.is_empty() {
            return;
        }
        // Expand user-defined aliases first — `alias.dp = "deploy
        // --auto-rollback 5m"` + `:dp build-900` becomes the line
        // `deploy --auto-rollback 5m build-900`. Single-level
        // expansion only so `alias.x = "x"` can't loop. The
        // expansion is owned (String) because the borrowed `raw`
        // doesn't outlive this scope.
        let expanded = expand_command_alias(line, &self.cfg.command_aliases);
        let line = expanded.as_str();
        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else { return };
        let rest: Vec<&str> = parts.collect();
        match cmd {
            "q" | "quit" => self.quit = true,
            "refresh" => self.manual_refresh(),
            "help" | "?" => {
                // Mirror the `?` keybind: scope help to the screen the user
                // was on before opening the command bar. The Command-mode
                // transition doesn't leave a breadcrumb, so we infer from
                // what's currently set (Detail view live, action flow open,
                // DLQ open, interactive overlay open).
                self.help.topic = if self.detail.is_some() {
                    HelpTopic::Detail
                } else if self.action_flow.is_some() {
                    HelpTopic::Action
                } else if self.dlq.is_some() {
                    HelpTopic::Dlq
                } else if matches!(
                    self.current_overlay,
                    Some(Overlay::SavedConfigsInteractive { .. })
                ) {
                    HelpTopic::SavedConfigs
                } else {
                    HelpTopic::Global
                };
                self.help.pre_mode = Some(self.mode);
                self.mode = Mode::Help;
            }
            "region" | "r" => self.cmd_region(&rest),
            "custom-platforms" | "platforms" => self.cmd_custom_platforms(),
            "accounts" => self.cmd_accounts(),
            "org-health" => self.cmd_org_health(),
            "find-env" => match rest.first().copied() {
                None => {
                    self.error_message = Some(
                        "usage: :find-env <name-substring>  (scans every AWS profile + AssumeRole account)"
                            .into(),
                    );
                }
                Some(needle) => self.cmd_find_env(needle),
            },
            "envs-by-version" => match rest.first().copied() {
                None => {
                    self.error_message = Some(
                        "usage: :envs-by-version <label>  (scans every AWS profile + AssumeRole account for envs running that exact version label)"
                            .into(),
                    );
                }
                Some(label) => self.cmd_envs_by_version(label),
            },
            "logs-insights" => {
                // Pass the whole remainder verbatim. `cmd_logs_insights`
                // parses the optional `--window WINDOW` prefix; everything
                // after is the Insights query (spacing + punctuation kept
                // exactly as the operator typed it).
                let args = rest.join(" ");
                self.cmd_logs_insights(&args);
            }
            "account" => self.cmd_account(&rest),
            "profile" | "p" => self.cmd_profile(&rest),
            "sort" => self.cmd_sort(&rest),
            "group" => self.cmd_group(&rest),
            "redact" => self.cmd_redact(&rest),
            "events" => {
                self.event_panel.visible =
                    parse_toggle(rest.first().copied(), self.event_panel.visible);
                if self.event_panel.visible && self.event_panel.events.is_empty() {
                    self.spawn_events();
                }
                self.status_message = Some(if self.event_panel.visible {
                    "events panel ON".into()
                } else {
                    "events panel off".into()
                });
            }
            "event-time" => self.cmd_event_time(&rest),
            "export" => self.export_tsv(),
            "json" => self.export_json(),
            "report" | "markdown" => self.export_markdown(),
            "readonly" => {
                self.read_only = parse_toggle(rest.first().copied(), self.read_only);
                self.status_message = Some(if self.read_only {
                    "read-only ON — destructive actions disabled".into()
                } else {
                    "read-only off".into()
                });
            }
            "pin" => self.toggle_pin_selected(),
            "alias" => match rest.first().copied() {
                Some(name) => {
                    let label = rest[1..].join(" ");
                    if label.is_empty() {
                        self.error_message = Some(
                            "usage: :alias <env-name> <label>  (label cannot be empty)".to_string(),
                        );
                    } else {
                        self.aliases.insert(name.to_string(), label.clone());
                        self.status_message = Some(format!("alias '{name}' → \"{label}\""));
                        self.persist_state();
                    }
                }
                None => {
                    if self.aliases.is_empty() {
                        self.status_message = Some("no aliases set".into());
                    } else {
                        let list: Vec<String> = self
                            .aliases
                            .iter()
                            .map(|(k, v)| format!("{k} → \"{v}\""))
                            .collect();
                        self.status_message = Some(format!("aliases: {}", list.join("  ")));
                    }
                }
            },
            "alias-drop" | "alias-rm" => match rest.first() {
                Some(name) => {
                    if self.aliases.remove(*name).is_some() {
                        self.status_message = Some(format!("alias '{name}' removed"));
                        self.persist_state();
                    } else {
                        self.error_message = Some(format!("no alias for '{name}'"));
                    }
                }
                None => self.error_message = Some("usage: :alias-drop <env-name>".into()),
            },
            "whatsnew" => self.open_whatsnew(),
            "about" | "credits" => self.open_about_overlay(),
            "apps-info" => self.open_apps_info_overlay(),
            "cost" => self.cmd_cost(&rest),
            "fleet-cost" => self.cmd_fleet_cost(),
            "promotions" => self.cmd_promotions(),
            "listeners" => self.cmd_listeners(),
            "listener-edit" => self.cmd_listener_edit(&rest),
            "rds" => self.cmd_rds(),
            "rds-attach" => self.cmd_rds_attach(),
            "rds-detach" => self.cmd_rds_detach(&rest),
            "options" => self.cmd_options(&rest),
            "config-diff" => self.cmd_config_diff(&rest),
            "config-diff-local" => self.cmd_config_diff_local(&rest),
            "explain" => self.cmd_explain(&rest),
            "env-edit" => self.cmd_env_edit(),
            "secrets" => self.cmd_secrets(&rest),
            "secret" => self.cmd_secret_view(&rest),
            "report-bug" => self.open_report_bug_overlay(),
            "settings" => {
                self.open_settings_form();
            }
            "capacity" => self.cmd_capacity(),
            "scaling-triggers" => self.cmd_scaling_triggers(),
            "subnets" => self.open_subnets_form(),
            "elb-subnets" => self.open_elb_subnets_form(),
            "security-groups" => self.open_security_groups_form(),
            "update" => {
                // Surface the upgrade command for whichever install channel
                // looks live. Doesn't actually upgrade — operators on
                // AWS-touching tools prefer conscious upgrades, and
                // self-replacing the binary across Cellar / cargo-bin /
                // tarball layouts has too many platform footguns.
                let channel = crate::update_check::detect_install_channel();
                let cmd = channel.upgrade_command();
                let current = env!("CARGO_PKG_VERSION");
                let msg = match self.update_available.as_ref() {
                    Some(release) => format!(
                        "update available: {current} → {}.  run: {cmd}",
                        release.version
                    ),
                    None => {
                        format!("already on the latest ({current}).  to force-reinstall: {cmd}")
                    }
                };
                // Best-effort yank to the clipboard so the operator can
                // paste the upgrade command directly. Silent if the
                // clipboard isn't reachable.
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(cmd.to_string());
                }
                self.pin_status(msg);
            }
            "history" => {
                self.current_overlay = Some(Overlay::History(self.format_message_log()));
            }
            "saved-configs" | "configs" => {
                let items = collect_saved_configs(&self.applications);
                if items.is_empty() {
                    self.current_overlay = Some(Overlay::SavedConfigs(format_saved_configs(
                        &self.applications,
                    )));
                } else {
                    self.current_overlay = Some(Overlay::SavedConfigsInteractive {
                        items,
                        cursor: 0,
                        confirm_delete: false,
                    });
                }
            }
            "plugins" => {
                if self.plugins.is_empty() {
                    self.status_message =
                        Some("no plugins — add ~/.config/ebman/commands.toml".into());
                } else {
                    let names: Vec<&str> = self.plugins.keys().map(String::as_str).collect();
                    self.status_message = Some(format!(":<plugin>  {}", names.join(", ")));
                }
            }
            "diff" => self.cmd_diff(&rest),
            "alarms" => {
                let env_opt = if let Some(d) = self.detail.as_ref() {
                    Some(d.env_name.clone())
                } else {
                    self.selected_env().map(|e| e.name.clone())
                };
                match env_opt {
                    Some(env_name) => self.spawn_alarms_fetch(env_name),
                    None => {
                        self.error_message = Some(
                            "no env selected — press 1-9, click a row, or type ' to jump by name"
                                .into(),
                        )
                    }
                }
            }
            "why" | "diagnose" => {
                let env_opt = if let Some(d) = self.detail.as_ref() {
                    Some((d.env_name.clone(), d.env_snapshot.application.clone()))
                } else {
                    self.selected_env()
                        .map(|e| (e.name.clone(), e.application.clone()))
                };
                match env_opt {
                    Some((env_name, app_name)) => self.open_why_red(env_name, app_name),
                    None => {
                        self.error_message = Some(
                            "no env selected — press 1-9, click a row, or type ' to jump by name"
                                .into(),
                        )
                    }
                }
            }
            "loglevel" => match rest.first() {
                None => {
                    self.status_message =
                        Some(format!("current log directive: {}", self.log_directive));
                }
                Some(level) => {
                    self.set_log_level(level);
                }
            },
            "cols" => self.cmd_cols(&rest),
            "save-view" => self.cmd_save_view(&rest),
            "view" => self.cmd_view(&rest),
            "views" => self.cmd_views(),
            "view-drop" => self.cmd_view_drop(&rest),
            "filter" | "f" => self.cmd_filter_load(&rest),
            "save" => self.cmd_save_filter(&rest),
            "drop" => self.cmd_drop_filter(&rest),
            "filters" => self.cmd_filters(),
            "batch-rebuild" => self.cmd_batch_action(Action::Rebuild),
            "batch-restart" => self.cmd_batch_action(Action::RestartAppServer),
            "batch-deploy" => self.cmd_batch_deploy(&rest),
            "batch-tag" => self.cmd_batch_tag_or_untag(true, &rest),
            "batch-untag" => self.cmd_batch_tag_or_untag(false, &rest),
            "batch-set-option" => self.cmd_batch_set_option(&rest),
            "versions" => self.cmd_versions(),
            "deploy" => self.cmd_deploy(&rest),
            "rollback" => self.cmd_rollback(&rest),
            "changes" => self.cmd_changes(),
            "lineage" => self.cmd_lineage(),
            "ssh" => self.cmd_ssh(&rest),
            "ssm-run" => self.cmd_ssm_run(&rest),
            "delete-version" => self.cmd_delete_version(&rest),
            "upgrade" => self.cmd_upgrade(&rest),
            "clone" => self.cmd_clone(&rest),
            "promote-env" => self.cmd_promote_env(&rest),
            "rollout" => self.cmd_rollout(&rest),
            "scale" => self.cmd_scale(&rest),
            "stop" => self.cmd_stop(),
            "start" => self.cmd_start(),
            "abort" => self.cmd_abort(),
            "pending" | "in-flight" | "inflight" => self.cmd_pending(),
            "rollbacks-armed" | "rb-armed" => self.cmd_rollbacks_armed(),
            "abort-rollback" => self.cmd_abort_rollback(&rest),
            "freeze-deploys" => self.cmd_freeze_deploys(&rest),
            "incident" => self.cmd_incident(&rest),
            "thaw-deploys" => self.cmd_thaw_deploys(),
            "undo" => self.cmd_undo(),
            "lint" => self.cmd_lint(&rest),
            "drift" => self.cmd_drift(&rest),
            "tag" => self.cmd_tag(&rest),
            "untag" => self.cmd_untag(&rest),
            "resources" | "res" => self.cmd_resources(),
            "rebuild" => self.cmd_rebuild(),
            "restart" => self.cmd_restart(),
            "terminate" => self.cmd_terminate(),
            "swap" => self.cmd_swap(&rest),
            "config-save" => self.cmd_config_save(&rest),
            "config-delete" => self.cmd_config_delete(&rest),
            "config-apply" => self.cmd_config_apply(&rest),
            "deployment-policy" => self.cmd_deployment_policy(&rest),
            "rolling-update" => self.cmd_rolling_update(&rest),
            "health-check-url" => self.cmd_health_check_url(&rest),
            "keypair" => self.cmd_keypair(&rest),
            "service-role" => self.cmd_service_role(&rest),
            "instance-profile" => self.cmd_instance_profile(&rest),
            "public-ip" => self.cmd_public_ip(&rest),
            "elb-scheme" => self.cmd_elb_scheme(&rest),
            "set-option" => self.cmd_set_option(&rest),
            "unset-option" => self.cmd_unset_option(&rest),
            "instance-type" => self.cmd_instance_type(&rest),
            "custom-platform-delete" => self.cmd_custom_platform_delete(&rest),
            "env" => self.cmd_env(&rest),
            "metric" => self.cmd_metric(&rest),
            "logs-tail" => {
                // `:logs-tail [LOG_GROUP]` — stream a CW Logs group for the
                // selected env. If no group given, discover groups for the
                // env and pick the most useful one (web.stdout.log if
                // present, else the first by name). The polling task is
                // tracked on App.log_tail_task so subsequent calls / close
                // can abort cleanly.
                let Some(env) = self.selected_env().cloned() else {
                    self.error_message = Some(
                        "no env selected — press 1-9, click a row, or type ' to jump by name"
                            .into(),
                    );
                    return;
                };
                let explicit_group = rest.first().map(|s| s.to_string());
                self.spawn_logs_tail(env.name.clone(), explicit_group);
            }
            "event-tail" | "tail-events" => {
                // `:event-tail` — cross-fleet EB event stream. No env
                // selection needed; the tail covers every env in the
                // current context. The polling task is tracked on
                // App.event_tail_task so re-issue / close abort cleanly.
                self.status_message = Some("event tail: fetching fleet events…".into());
                self.spawn_event_tail();
            }
            "logs-stream" => self.cmd_logs_stream(&rest),
            "notify" => self.cmd_notify(&rest),
            "managed-window" => self.cmd_managed_window(&rest),
            "alarm-create" => self.cmd_alarm_create(&rest),
            "alarm-delete" => self.cmd_alarm_delete(&rest),
            "alarm-history" => self.cmd_alarm_history(&rest),
            "config-inspect" => self.cmd_config_inspect(&rest),
            "deselect" | "select-clear" => {
                let n = self.multi_selected.len();
                self.multi_selected.clear();
                self.status_message = Some(format!("cleared {n} env selection(s)"));
            }
            other => {
                if let Some(plugin) = self.plugins.get(other).cloned() {
                    self.run_plugin_command(other, &plugin);
                    return;
                }
                // Did-you-mean: surface the closest registry name
                // within edit-distance 2. Catches everyday typos
                // like `:restrt` → `:restart`. Skips the suggestion
                // entirely when nothing's close enough — a wild
                // guess would mislead rather than help.
                let suggestion = suggest_command(other);
                let msg = match suggestion {
                    Some(name) => {
                        format!("unknown command: :{other} — did you mean :{name}? (try :help)")
                    }
                    None => format!("unknown command: :{other}  (try :help)"),
                };
                self.error_message = Some(msg);
            }
        }
    }

    fn run_plugin_command(&mut self, name: &str, plugin: &crate::plugins::Plugin) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(env) = env_opt else {
            self.error_message = Some(format!(":{name} — no env selected"));
            return;
        };
        let rendered = crate::plugins::render(
            &plugin.template,
            &env.name,
            &env.cname,
            &env.application,
            &env.tier,
            &self.context.region,
            self.override_profile
                .as_deref()
                .or(self.context.profile.as_deref()),
        );
        match yank(&rendered) {
            Ok(()) => {
                self.status_message = Some(format!(
                    "plugin :{name} → clipboard ({} chars)",
                    rendered.chars().count()
                ));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    fn open_profile_picker(&mut self) {
        let items = profiles::load_profiles();
        let current = self.context.profile.as_deref();
        self.picker = Some(Picker::new(PickerKind::Profile, items, current));
        self.mode = Mode::Picker;
    }

    fn open_region_picker(&mut self) {
        let mut items: Vec<String> = profiles::REGIONS.iter().map(|s| (*s).to_string()).collect();
        for r in &self.extra_regions {
            if !items.iter().any(|i| i == r) {
                items.push(r.clone());
            }
        }
        let current = Some(self.context.region.as_str());
        self.picker = Some(Picker::new(PickerKind::Region, items, current));
        self.mode = Mode::Picker;
    }

    pub fn persist_state(&self) {
        // `--demo` mode runs against a synthetic fleet on a fake
        // profile/region with cost tracking flipped on by the fixture.
        // Writing that to ~/.config/ebman/state.toml would clobber the
        // operator's real saved state (selected env, sort, named
        // filters, cost-enabled, …) on every demo session exit. Bail
        // before touching disk.
        if self.demo_mode {
            return;
        }
        let selected = self.selected_env().map(|e| e.name.clone());
        // Persist the operator's *intent* first, then fall back to the
        // effective state. Override-wins matters when the user has
        // dispatched `:region X` (so `override_region` is `Some(X)`) but
        // the rebuild hasn't landed yet (so `context.region` is still the
        // *previous* region). Quitting in that gap would otherwise
        // persist the stale context and restore the user to the old
        // region on next launch. Falling back to `context` when override
        // is `None` covers the env-default case so we still remember
        // where the user was even if they never explicitly switched.
        let region = self.override_region.clone().or_else(|| {
            if !self.context.region.is_empty() && self.context.region != "unknown" {
                Some(self.context.region.clone())
            } else {
                None
            }
        });
        let profile = self
            .override_profile
            .clone()
            .or_else(|| self.context.profile.clone());
        tracing::debug!(
            target: "ebman::state",
            override_region = ?self.override_region,
            context_region = %self.context.region,
            persisted_region = ?region,
            override_profile = ?self.override_profile,
            context_profile = ?self.context.profile,
            persisted_profile = ?profile,
            "persist_state"
        );
        state::save(&PersistedState {
            profile,
            region,
            filter: if self.filter.is_empty() {
                None
            } else {
                Some(self.filter.text().to_string())
            },
            sort: Some(format!(
                "{}:{}",
                self.sort_key.label(),
                if self.sort_desc { "desc" } else { "asc" }
            )),
            grouped: Some(self.grouped),
            redact: Some(self.redact),
            events_visible: Some(self.event_panel.visible),
            event_time_format: Some(self.event_panel.time_format),
            selected_env: selected,
            pinned: self.pinned.clone(),
            pinned_apps: self.pinned_apps.clone(),
            cost_enabled: Some(self.cost_enabled),
            aliases: self.aliases.clone(),
            saved_views: self.saved_views.clone(),
            deploy_snapshots: self
                .deploy_snapshots
                .iter()
                .map(|(env, snap)| (env.clone(), snap.to_persisted()))
                .collect(),
            hidden_cols: self.hidden_cols.clone(),
            custom_metrics: self.custom_metrics.clone(),
        });
    }

    fn resort_envs(&mut self) {
        let key = self.sort_key;
        let desc = self.sort_desc;
        let pinned = self.pinned.clone();
        self.environments.sort_by(|a, b| {
            // Pinned envs always sort to the top regardless of key/direction.
            let a_pin = pinned.contains(&a.name);
            let b_pin = pinned.contains(&b.name);
            if a_pin != b_pin {
                return if a_pin {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            let ord = match key {
                SortKey::App => a
                    .application
                    .to_lowercase()
                    .cmp(&b.application.to_lowercase())
                    .then(a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Status => a
                    .status
                    .to_lowercase()
                    .cmp(&b.status.to_lowercase())
                    .then(a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortKey::Health => health_rank(&a.health)
                    .cmp(&health_rank(&b.health))
                    .then(a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortKey::Age => a.updated.cmp(&b.updated),
                SortKey::Version => a
                    .version_label
                    .to_lowercase()
                    .cmp(&b.version_label.to_lowercase()),
            };
            if desc {
                ord.reverse()
            } else {
                ord
            }
        });
        self.rebuild_view();
    }

    fn yank_selected(&mut self, kind: YankKind) {
        let Some(env) = self.selected_env() else {
            self.status_message = Some("nothing to yank".into());
            return;
        };
        let value = match kind {
            YankKind::Cname => env.cname.clone(),
            YankKind::Name => env.name.clone(),
        };
        if value.is_empty() {
            self.status_message = Some("selected env has no value to yank".into());
            return;
        }
        match yank(&value) {
            Ok(()) => {
                self.status_message = Some(format!(
                    "copied {} to clipboard",
                    match kind {
                        YankKind::Cname => "CNAME",
                        YankKind::Name => "name",
                    }
                ));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    fn export_tsv(&mut self) {
        let count = self.cached_filtered.len();
        let mut out = String::new();
        out.push_str(
            "NAME\tAPPLICATION\tTIER\tSTATUS\tHEALTH\tPLATFORM\tVERSION\tCNAME\tUPDATED\n",
        );
        for &i in &self.cached_filtered {
            let e = &self.environments[i];
            let cname = if self.redact {
                redact_block(&e.cname)
            } else {
                e.cname.clone()
            };
            let updated = e.updated.map(|u| u.to_rfc3339()).unwrap_or_default();
            out.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
                e.name,
                e.application,
                e.tier,
                e.status,
                e.health,
                e.platform,
                e.version_label,
                cname,
                updated
            ));
        }
        match yank(&out) {
            Ok(()) => {
                self.status_message = Some(format!("exported {count} rows (TSV) to clipboard"));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }

    /// Recompute `tf_managed_envs` from the current `tf_state`.
    /// Called at startup (after `App::new`'s tfstate load), on
    /// `apply_rebuild` (context switch), and on the `:drift
    /// refresh` operator gesture. Cheap — set construction is
    /// O(n) over tf-managed env names; typically < 50.
    pub(crate) fn refresh_tf_managed_envs(&mut self) {
        self.tf_managed_envs = self
            .tf_state
            .as_ref()
            .map(|s| s.managed_names())
            .unwrap_or_default();
    }

    pub fn selected_env(&self) -> Option<&Environment> {
        let sel = self.table_state.selected()?;
        match self.display_rows().get(sel)? {
            DisplayRow::Env(i) => self.environments.get(*i),
            DisplayRow::Separator => None,
        }
    }

    fn apply_picker_choice(&mut self, kind: PickerKind, value: String) {
        match kind {
            PickerKind::Profile => {
                tracing::info!(
                    target: "ebman::state",
                    new_profile = %value,
                    cleared_override_region = ?self.override_region,
                    "apply_picker_choice(Profile) clears override_region so SDK re-resolves from new profile config"
                );
                self.override_profile = Some(value.clone());
                self.override_region = None;
                self.status_message = Some(format!("switching to profile {value}…"));
                self.spawn_rebuild();
            }
            PickerKind::Region => {
                tracing::info!(
                    target: "ebman::state",
                    new_region = %value,
                    prior_override = ?self.override_region,
                    "apply_picker_choice(Region) sets override_region"
                );
                self.override_region = Some(value.clone());
                self.status_message = Some(format!("switching to region {value}…"));
                self.spawn_rebuild();
            }
            PickerKind::LogGroup => {
                // Swap the streaming overlay's tailed group. Read the env
                // from the currently-open LogTail overlay; `spawn_logs_tail`
                // aborts the existing poller and opens a fresh one against
                // the chosen group, replacing `current_overlay` via the
                // resulting `AppMsg::LogTailOpened`.
                let env = match self.current_overlay.as_ref() {
                    Some(Overlay::LogTail { env_name, .. }) => env_name.clone(),
                    _ => return,
                };
                self.spawn_logs_tail(env, Some(value));
            }
            PickerKind::SshInstance => {
                // Same flow as pressing `s` on Detail/Instances — the
                // main loop tick consumes `pending_shell_target` and
                // handles the TUI suspend/resume + alt-screen dance.
                crate::audit::append_action_dispatched(
                    self.context.account_id.as_deref(),
                    self.context.profile.as_deref(),
                    &self.context.region,
                    "SsmSession",
                    value.as_str(),
                    &[("via", "cmd_ssh_picker")],
                );
                self.pending_shell_target = Some(value.clone());
                self.status_message = Some(format!("opening SSM session to {value}…"));
            }
        }
    }

    fn spawn_rebuild(&mut self) {
        self.load_state = LoadState::Loading;
        self.loading_since = Some(Instant::now());
        self.rebuild_epoch = self.rebuild_epoch.wrapping_add(1);
        let epoch = self.rebuild_epoch;
        let profile = self.override_profile.clone();
        let region = self.override_region.clone();
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = match AwsClient::with(profile, region).await {
                Ok(c) => Ok(Box::new(c)),
                Err(e) => Err(flatten_err("aws_client_with", e)),
            };
            let _ = tx.send(AppMsg::Rebuild { epoch, result });
        });
    }

    /// Background task variant of `spawn_rebuild` for the AssumeRole
    /// path. Calls `AwsClient::assume_role` with the operator's named
    /// account spec; same `AppMsg::Rebuild` arrival point so the rest
    /// of the swap (overlay tear-down, throttle reset, identity refresh)
    /// flows through the existing `apply_rebuild` handler.
    fn spawn_assume_role_switch(&mut self, account_name: String) {
        let Some(spec) = self.cfg.accounts.get(&account_name).cloned() else {
            self.error_message = Some(format!(
                "no `accounts.{account_name}` in config.toml — add `accounts.{account_name}.role_arn = …`"
            ));
            return;
        };
        self.load_state = LoadState::Loading;
        self.loading_since = Some(Instant::now());
        self.status_message = Some(format!("assuming role for account '{account_name}'…"));
        self.rebuild_epoch = self.rebuild_epoch.wrapping_add(1);
        let epoch = self.rebuild_epoch;
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = match AwsClient::assume_role(&account_name, &spec).await {
                Ok(c) => Ok(Box::new(c)),
                Err(e) => Err(flatten_err("aws_client_assume_role", e)),
            };
            let _ = tx.send(AppMsg::Rebuild { epoch, result });
        });
    }

    fn spawn_identity(&mut self) {
        self.spawn_aws(
            "verify_identity",
            move |aws| async move { aws.verify_identity().await },
            |gen, result| AppMsg::Identity { gen, result },
        );
    }

    fn spawn_update_check(&mut self) {
        // No outbound network in `--demo` mode — VHS captures shouldn't
        // pulse a "latest version available" toast partway through.
        if self.demo_mode {
            return;
        }
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = crate::update_check::check_async().await;
            let _ = tx.send(AppMsg::UpdateCheck(result));
        });
    }

    /// If the `loading…` indicator was visible during the current load (i.e.
    /// `loading_since` was set and crossed the display threshold), arm a
    /// linger window so the indicator stays on for at least
    /// [`LOADING_INDICATOR_LINGER`] after the load completes. Call this
    /// *before* clearing `loading_since` and flipping `load_state` back to
    /// Idle/Error in the AppMsg handler.
    fn arm_loading_linger(&mut self) {
        let now = Instant::now();
        if let Some(until) = compute_loading_linger_target(
            self.loading_since,
            LOADING_INDICATOR_THRESHOLD,
            LOADING_INDICATOR_LINGER,
            now,
        ) {
            self.loading_visible_until = Some(until);
        }
    }

    fn spawn_refresh(&mut self) {
        // `--demo` mode pins the fixture data in place — refresh would
        // call into the stub AwsClient, get empty results, and blank
        // the table. Skip entirely.
        if self.demo_mode {
            return;
        }
        if matches!(self.load_state, LoadState::Loading) {
            return;
        }
        self.load_state = LoadState::Loading;
        self.loading_since = Some(Instant::now());
        self.status_snapshot_at_refresh =
            Some((self.status_message.clone(), self.error_message.clone()));
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        if self.multi_regions.is_empty() {
            let aws = self.aws.clone();
            tokio::spawn(async move {
                let result = aws
                    .list_environments()
                    .await
                    .map_err(|e| flatten_err("list_environments", e));
                let _ = tx.send(AppMsg::Refresh { gen, result });
            });
        } else {
            let regions = self.multi_regions.clone();
            let profile = self
                .override_profile
                .clone()
                .or_else(|| self.context.profile.clone());
            tokio::spawn(async move {
                use futures::future::join_all;
                let tasks = regions.into_iter().map(|r| {
                    let p = profile.clone();
                    async move { crate::aws::list_environments_in_region(p, r).await }
                });
                let results = join_all(tasks).await;
                let mut envs = Vec::new();
                let mut errs = Vec::new();
                for r in results {
                    match r {
                        Ok(v) => envs.extend(v),
                        Err(e) => errs.push(format!("{e}")),
                    }
                }
                let result = if envs.is_empty() && !errs.is_empty() {
                    Err(errs.join("; "))
                } else {
                    Ok(envs)
                };
                let _ = tx.send(AppMsg::Refresh { gen, result });
            });
        }
        if self.event_panel.visible {
            self.spawn_events();
        }
        self.spawn_applications();
        // Solution stacks change rarely (AWS releases platform versions
        // roughly monthly); fetch once per context and reuse. Cleared on a
        // context switch so a new account/region rebuilds it.
        if self.latest_stacks.is_empty() {
            self.spawn_solution_stacks();
        }
    }

    /// Fetch the region's solution-stack catalogue so the envs table can
    /// flag platforms with a newer version available. Best-effort: a failed
    /// fetch just leaves `latest_stacks` empty and no env is flagged.
    fn spawn_solution_stacks(&self) {
        self.spawn_aws(
            "list_solution_stacks",
            move |aws| async move { aws.list_solution_stacks().await },
            |gen, result| AppMsg::SolutionStacks { gen, result },
        );
    }

    fn spawn_applications(&self) {
        self.spawn_aws(
            "list_applications",
            move |aws| async move { aws.list_applications().await },
            |gen, result| AppMsg::Applications { gen, result },
        );
    }

    /// Set the active scope. Triggers the lazy `spawn_app_latest_versions`
    /// fetch when transitioning to `Apps`, so the LATEST column populates
    /// on entry rather than waiting for the next periodic refresh tick.
    /// Idempotent — re-entering the same scope is a no-op.
    fn set_scope(&mut self, new: Scope) {
        let changed = self.scope != new;
        self.scope = new;
        if changed && new == Scope::Apps {
            self.spawn_app_latest_versions();
        }
    }

    /// Fan out `DescribeApplicationVersions` per app to compute the LATEST
    /// column in the apps view. The AWS application-level `date_updated`
    /// only changes on metadata edits (description / templates / lifecycle),
    /// not on new version pushes — so operators expect this column to track
    /// version `date_created` instead. Errors on individual apps drop that
    /// row from the result rather than failing the batch.
    fn spawn_app_latest_versions(&self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let names: Vec<String> = self.applications.iter().map(|a| a.name.clone()).collect();
        if names.is_empty() {
            return;
        }
        tokio::spawn(async move {
            use futures::future::join_all;
            let futs = names.into_iter().map(|name| {
                let aws = aws.clone();
                async move {
                    let res = aws.list_application_versions(&name).await;
                    let head = res.ok().and_then(|mut v| v.drain(..).next());
                    (
                        name,
                        head.as_ref().map(|h| h.label.clone()),
                        head.and_then(|h| h.created),
                    )
                }
            });
            let results: Vec<(
                String,
                Option<String>,
                Option<chrono::DateTime<chrono::Utc>>,
            )> = join_all(futs).await;
            let _ = tx.send(AppMsg::AppLatestVersions { gen, results });
        });
    }

    /// Per-Worker-env DLQ depth fan-out. Fires once per refresh after
    /// `list_environments` lands. Skips Web envs (no DLQ). Each env's
    /// fetch is independent — a failure on one drops that entry from
    /// the result rather than failing the batch.
    fn spawn_worker_queue_check(&self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let workers: Vec<(String, String)> = self
            .environments
            .iter()
            .filter(|e| e.tier.eq_ignore_ascii_case("Worker"))
            .map(|e| (e.name.clone(), e.application.clone()))
            .collect();
        if workers.is_empty() {
            return;
        }
        tokio::spawn(async move {
            use futures::future::join_all;
            let futs = workers.into_iter().map(|(env, app)| {
                let aws = aws.clone();
                async move {
                    // Errors stay errors — a failed fetch must not be
                    // indistinguishable from "no DLQ" (the pre-0.27
                    // shape silently blinded red-alerting on
                    // AccessDenied/throttle).
                    let outcome = match aws.describe_worker_queues(&app, &env).await {
                        Ok(q) => Ok(q.dlq_stats.map(|s| s.visible)),
                        Err(e) => Err(flatten_err("describe_worker_queues", e)),
                    };
                    (env, outcome)
                }
            });
            let results: Vec<(String, Result<Option<i64>, String>)> =
                join_all(futs).await.into_iter().collect();
            let _ = tx.send(AppMsg::WorkerQueueCheck { gen, results });
        });
    }

    /// Fan `DescribeEnvironmentHealth` across every env on each refresh
    /// tick to populate the `INST` column. Skips Terminated / Terminating
    /// envs (EB returns AccessDenied-ish errors for them) and silently
    /// drops failures so a single env's API blip doesn't poison the
    /// whole batch. Same shape as `spawn_worker_queue_check`.
    fn spawn_env_instance_counts(&self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let targets: Vec<String> = self
            .environments
            .iter()
            .filter(|e| {
                // EB rejects DescribeEnvironmentHealth for envs in
                // terminal lifecycle states — no instances to count.
                !matches!(
                    e.status.as_str(),
                    "Terminated" | "Terminating" | "Launching"
                )
            })
            .map(|e| e.name.clone())
            .collect();
        if targets.is_empty() {
            return;
        }
        tokio::spawn(async move {
            use futures::future::join_all;
            let futs = targets.into_iter().map(|env| {
                let aws = aws.clone();
                async move {
                    aws.fetch_env_instance_counts(&env)
                        .await
                        .ok()
                        .map(|counts| (env, counts))
                }
            });
            let results: Vec<(String, crate::aws::EnvInstanceCounts)> =
                join_all(futs).await.into_iter().flatten().collect();
            let _ = tx.send(AppMsg::EnvInstanceCountsCheck { gen, results });
        });
    }

    fn spawn_events(&mut self) {
        // Scope the events panel to the currently-selected env so it tells
        // the user about *this* env, not the entire account. Falls back to
        // the global event stream when no env is selected. The previously-
        // fetched env name is recorded so we can detect selection changes
        // and refetch without firing a request on every j/k.
        let selected = self.selected_env().map(|e| e.name.clone());
        self.event_panel.for_env = selected.clone();
        self.spawn_aws(
            "list_events",
            move |aws| async move {
                match selected {
                    Some(name) => aws.list_events_for_env(&name, 50).await,
                    None => aws.list_events(50).await,
                }
            },
            |gen, result| AppMsg::Events { gen, result },
        );
    }

    /// Refetch the events panel if the cursor has moved to a different env
    /// since the last fetch. Called from the main loop just before draw, so
    /// any keystroke / mouse click that changed selection picks up the new
    /// env's events on the next frame.
    fn refresh_events_if_selection_changed(&mut self) {
        if !self.event_panel.visible {
            return;
        }
        let selected = self.selected_env().map(|e| e.name.clone());
        if selected != self.event_panel.for_env {
            self.spawn_events();
        }
    }

    /// Apply a Detail-tab AppMsg payload. Handles the boilerplate every
    /// `Detail*` variant shares: drop when no Detail view is open, drop
    /// when the user switched to a different env mid-fetch. The stale-
    /// generation drop is handled upstream by `handle_msg`'s central guard.
    ///
    /// The closure runs against `&mut DetailState` + the raw
    /// `Result<T, String>` so the caller picks its own success / error
    /// behaviour — most clear `detail.error` on the Ok branch, but tags /
    /// env-vars use `tracing::warn!` instead since their failures
    /// shouldn't tint the whole tab red.
    fn apply_detail_msg<T, F>(&mut self, env_name: &str, result: Result<T, String>, apply: F)
    where
        F: FnOnce(&mut DetailState, Result<T, String>),
    {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.env_name != env_name {
            return;
        }
        apply(detail, result);
    }

    fn apply_rebuild(&mut self, epoch: u64, result: Result<Box<AwsClient>, String>) {
        // Stale arrival: a NEWER switch was spawned after this one —
        // applying it would settle the app on an older choice.
        if epoch != self.rebuild_epoch {
            return;
        }
        match result {
            Ok(client) => {
                self.generation = self.generation.wrapping_add(1);
                self.context = client.context.clone();
                self.aws = Arc::new(*client);
                self.maybe_apply_profile_theme();
                self.environments.clear();
                self.event_panel.events.clear();
                self.event_panel.scroll = 0;
                self.history.clear();
                // Solution-stack catalogue is region-specific; drop it so the
                // new context's `spawn_refresh` rebuilds it.
                self.latest_stacks.clear();
                // Overlays show data from the previous context (describe dump,
                // alarms list, …); close them so the user doesn't act on stale info.
                self.current_overlay = None;
                // Tear down any long-running CW Logs poll that's mid-flight;
                // it would otherwise keep hitting the previous account's CW.
                // Also bump session id so any in-flight LogTailOpened from
                // the aborted task is dropped on arrival.
                tail::reap_tail_task(&mut self.log_tail_task, &mut self.log_tail_session);
                // Same teardown for the fleet event tail — it polls
                // DescribeEvents against the previous context.
                tail::reap_tail_task(&mut self.event_tail_task, &mut self.event_tail_session);
                // Reset throttle back-off across context switches — the new
                // account/region has its own rate limits.
                self.throttle_until = None;
                self.consecutive_throttles = 0;
                // Diff state is keyed by env name. Switching accounts/regions may
                // surface envs with overlapping names but unrelated history;
                // clearing here prevents spurious "newly red" / status-delta noise
                // on the first refresh in the new context.
                self.prev_health.clear();
                self.prev_status.clear();
                self.prev_alerts = 0;
                self.newly_red.clear();
                self.newly_added.clear();
                self.health_delta.clear();
                self.status_delta.clear();
                // Drop any auto-rollback watchdogs armed against the
                // previous context. The deadline tokio tasks survive
                // (no JoinHandle for cancellation), but their late
                // `AutoRollbackCheck` messages get dropped by the
                // generation-guard at msg.rs's entry. Clearing here
                // also prevents a same-name env in the new context
                // from being seen as "still armed" by apply_refresh.
                self.armed_watchdogs.clear();
                // Same reasoning applies to wait-for-green trackers:
                // env-name keyed, context-scoped — drop on rebuild.
                self.watching_deploys.clear();
                // Pre-deploy snapshots are env-name keyed; clearing on
                // context switch avoids :rollback in the new context
                // picking up a label that doesn't exist there.
                self.deploy_snapshots.clear();
                // Undo entries reference env names from the previous
                // context — meaningless after a switch. Drop them
                // alongside the other env-keyed state.
                self.undo_history.clear();
                // Promotion lineage is env-name keyed (and would point
                // at envs that don't exist in the new context). Clear
                // alongside undo_history.
                self.promotion_history.clear();
                // 0.21: lint-input caches are env-name keyed and
                // context-scoped (instance IDs from health, tag keys
                // are account/region-scoped). Same context-switch
                // semantics as worker_dlq_depths above.
                self.env_tag_cache.clear();
                self.env_health_cache.clear();
                // Pending actions reference env names from the
                // previous context. Their spawned tasks (if any) are
                // dropped at the generation guard in msg.rs, so the
                // matching `complete_pending` never runs and the
                // header `⏳ N` chip / `:pending` overlay would show
                // the previous-context op forever.
                self.pending_actions.clear();
                // Any pending arm-then-dispatch (`:rebuild` modal etc.)
                // is also context-scoped — drop it so a stray Enter
                // doesn't fire against the new context. Also clear the
                // associated status_message — `queue_action_dispatch`
                // set "X dispatches in 5s — press U to undo", and
                // without this the bar would lie about a dispatch that
                // we just cancelled.
                self.pending_dispatch = None;
                self.status_message = None;
                // Detail tab snapshot (env name + instances + tab
                // state) is context-scoped: instance IDs are EC2-IDs
                // in the OLD account. Without this, `:ssm-run` would
                // resolve env+instances from the stale snapshot and
                // dispatch against the NEW AwsClient — cross-account
                // silent dispatch if the new account has a same-named
                // env. Same reasoning for the cached log-tail and
                // pending shell target. Drop the mode back to Normal
                // so the UI doesn't render a Detail view with no
                // data underneath it.
                self.detail = None;
                self.pending_shell_target = None;
                // An open `:a` action modal / confirm modal references
                // an env name from the previous context. Drop it
                // alongside detail so the operator doesn't confirm
                // against stale data after a context switch.
                self.action_flow = None;
                // Picker overlays (profile / region / log-group / ssh
                // instance) all carry context-scoped state too.
                self.picker = None;
                // An open form (`:capacity`, `:subnets`, env-var
                // editor…) was built against the OLD context's env; a
                // `^S` after the switch would deny_write-check the new
                // context's config with the old env name and dispatch
                // against the new client — cross-account silent write
                // if the new account has a same-named env. Same class
                // as the detail/:ssm-run clear above.
                self.form = None;
                // Batch selections are env-name keyed; a stale set
                // would let `:batch-rebuild` fan out old names against
                // the new client.
                self.multi_selected.clear();
                self.apps_selected.clear();
                // DLQ state carries queue URLs from the old account
                // (and `:help`'s topic inference reads it).
                self.dlq = None;
                // Per-env caches from the old context — stale numbers
                // would render as current until the first refresh.
                self.worker_dlq_depths.clear();
                self.worker_dlq_stale.clear();
                self.env_instance_counts.clear();
                self.applications.clear();
                self.costs.clear();
                self.costs_fetched_at = None;
                // Help's stash (pre_mode / pre_overlay) points at
                // modes and overlays this switch just tore down —
                // closing help would restore e.g. Mode::Detail with
                // detail == None (a ghost state).
                self.help.pre_mode = None;
                self.help.pre_overlay = None;
                if self.mode == Mode::Detail
                    || self.mode == Mode::Dlq
                    || self.mode == Mode::Action
                    || self.mode == Mode::Picker
                    || self.mode == Mode::Form
                    || self.mode == Mode::Help
                {
                    self.mode = Mode::Normal;
                }
                // Re-read tfstate from cwd. The new context might
                // be a different repo (operator cd'd between
                // sessions of `ebman` left running, or switched
                // account / region within the same shell);
                // re-discovery ensures the tf-managed badge reflects
                // the current project.
                self.tf_state = crate::terraform::load_from_cwd();
                self.refresh_tf_managed_envs();
                self.rebuild_view();
                self.table_state.select(None);
                self.status_message = Some(format!(
                    "context: {} / {}",
                    self.context.profile.as_deref().unwrap_or("default"),
                    self.context.region
                ));
                self.error_message = None;
                self.arm_loading_linger();
                self.load_state = LoadState::Idle;
                self.persist_state();
                self.spawn_identity();
                self.spawn_refresh();
            }
            Err(msg) => {
                tracing::error!(error = %msg, "rebuild failed");
                self.arm_loading_linger();
                self.load_state = LoadState::Error;
                self.loading_since = None;
                self.error_message = Some(self.format_aws_error("context switch", &msg));
            }
        }
    }

    fn move_scope_selection(&mut self, delta: i32) {
        match self.scope {
            Scope::Envs => self.move_selection(delta),
            Scope::Apps => {
                let n = self.applications.len();
                if n == 0 {
                    self.app_table_state.select(None);
                    return;
                }
                let cur = self.app_table_state.selected().unwrap_or(0) as i32;
                let next = (cur + delta).rem_euclid(n as i32) as usize;
                self.app_table_state.select(Some(next));
            }
        }
    }

    fn scope_select_first(&mut self) {
        match self.scope {
            Scope::Envs => self.select_first(),
            Scope::Apps => {
                if !self.applications.is_empty() {
                    self.app_table_state.select(Some(0));
                }
            }
        }
    }

    fn scope_select_last(&mut self) {
        match self.scope {
            Scope::Envs => self.select_last(),
            Scope::Apps => {
                if !self.applications.is_empty() {
                    self.app_table_state
                        .select(Some(self.applications.len() - 1));
                }
            }
        }
    }

    /// Open the apps-scope action overlay for the selected application.
    /// Captures the env list at open time so later refreshes (e.g. an
    /// env terminating mid-action) can't shift which envs the operator
    /// thought they were targeting. Closes silently when no app is
    /// selected or the application has no envs.
    pub(crate) fn open_apps_action_menu(&mut self) {
        let Some(idx) = self.app_table_state.selected() else {
            return;
        };
        let Some(app_name) = self.applications.get(idx).map(|a| a.name.clone()) else {
            return;
        };
        let env_names: Vec<String> = self
            .environments
            .iter()
            .filter(|e| e.application == app_name)
            .map(|e| e.name.clone())
            .collect();
        if env_names.is_empty() {
            self.status_message = Some(format!(
                "application '{app_name}' has no envs — nothing to act on"
            ));
            return;
        }
        self.current_overlay = Some(Overlay::AppsActionMenu {
            app_name,
            env_names,
            cursor: 0,
        });
    }

    /// Key handler for the apps-scope action overlay. j/k cycles the
    /// cursor; Enter dispatches the selected item; esc / q closes.
    /// Five items, dispatched via the matching `cmd_batch_*` helpers
    /// after seeding `multi_selected` with the captured env list.
    fn handle_apps_action_menu_key(&mut self, key: KeyEvent) {
        let n_items = APPS_ACTION_ITEMS.len() as i32;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.current_overlay = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(Overlay::AppsActionMenu { cursor, .. }) = self.current_overlay.as_mut()
                {
                    let cur = *cursor as i32;
                    *cursor = (cur + 1).rem_euclid(n_items) as usize;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(Overlay::AppsActionMenu { cursor, .. }) = self.current_overlay.as_mut()
                {
                    let cur = *cursor as i32;
                    *cursor = (cur - 1).rem_euclid(n_items) as usize;
                }
            }
            KeyCode::Enter => self.dispatch_apps_action_menu(),
            _ => {}
        }
    }

    fn dispatch_apps_action_menu(&mut self) {
        let Some(Overlay::AppsActionMenu {
            app_name,
            env_names,
            cursor,
        }) = self.current_overlay.as_ref().cloned()
        else {
            return;
        };
        // Close the overlay before dispatching so the resulting toast /
        // confirm modal renders on the bare apps table, not on top of
        // the menu.
        self.current_overlay = None;
        let item = match APPS_ACTION_ITEMS.get(cursor) {
            Some(it) => *it,
            None => return,
        };
        match item {
            AppsActionItem::Drill => {
                self.filter = app_name.clone().into();
                self.set_scope(Scope::Envs);
                self.rebuild_view();
                self.status_message = Some(format!("filtered envs to application '{app_name}'"));
            }
            AppsActionItem::BatchRebuild => {
                self.multi_selected = env_names.into_iter().collect();
                self.cmd_batch_action(Action::Rebuild);
            }
            AppsActionItem::BatchRestart => {
                self.multi_selected = env_names.into_iter().collect();
                self.cmd_batch_action(Action::RestartAppServer);
            }
            AppsActionItem::BatchDeploy => {
                // Seed the multi-select then drop into command mode
                // with `:batch-deploy ` so the operator types the
                // version label and Enter dispatches.
                self.multi_selected = env_names.into_iter().collect();
                self.mode = Mode::Command;
                self.command_input = "batch-deploy ".into();
                self.status_message = Some("type a version label and press enter".into());
            }
            AppsActionItem::OpenInConsole => {
                self.open_app_in_console();
            }
        }
    }

    /// Open the EB applications-page console URL for the selected
    /// application in the browser. Mirrors `open_in_console`'s
    /// `arboard`-clipboard-on-failure shape so the operator still has
    /// the URL available when the browser launch fails (SSH session,
    /// no DISPLAY, etc.).
    pub(crate) fn open_app_in_console(&mut self) {
        let Some(idx) = self.app_table_state.selected() else {
            self.status_message = Some("no application selected".into());
            return;
        };
        let Some(name) = self.applications.get(idx).map(|a| a.name.clone()) else {
            return;
        };
        let region = &self.context.region;
        let app_enc = urlencode(&name);
        let url = format!(
            "https://{region}.console.aws.amazon.com/elasticbeanstalk/home?region={region}#/application/overview?applicationName={app_enc}"
        );
        match open_url(&url) {
            Ok(()) => {
                self.status_message = Some(format!("opened {name} in browser"));
            }
            Err(e) => {
                self.error_message = Some(format!("couldn't open browser: {e}"));
            }
        }
    }

    fn drill_into_app(&mut self) {
        let Some(idx) = self.app_table_state.selected() else {
            return;
        };
        let Some(name) = self.applications.get(idx).map(|a| a.name.clone()) else {
            return;
        };
        self.filter = name.clone().into();
        self.set_scope(Scope::Envs);
        self.rebuild_view();
        self.status_message = Some(format!("filtered envs to application '{name}'"));
    }

    fn select_first(&mut self) {
        let rows = self.display_rows();
        if let Some(pos) = rows.iter().position(|r| matches!(r, DisplayRow::Env(_))) {
            self.table_state.select(Some(pos));
        }
    }

    fn select_last(&mut self) {
        let rows = self.display_rows();
        if let Some(pos) = rows.iter().rposition(|r| matches!(r, DisplayRow::Env(_))) {
            self.table_state.select(Some(pos));
        }
    }

    fn move_selection(&mut self, delta: i32) {
        let rows = self.display_rows();
        if rows.is_empty() {
            self.table_state.select(None);
            return;
        }
        // Build a list of indexes that are selectable (Env rows only).
        let selectable: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| matches!(r, DisplayRow::Env(_)).then_some(i))
            .collect();
        if selectable.is_empty() {
            self.table_state.select(None);
            return;
        }
        let current = self.table_state.selected().unwrap_or(selectable[0]);
        let pos_in_selectable = selectable.iter().position(|i| *i == current).unwrap_or(0) as i32;
        let next = (pos_in_selectable + delta).rem_euclid(selectable.len() as i32) as usize;
        self.table_state.select(Some(selectable[next]));
    }

    pub fn display_rows(&self) -> &[DisplayRow] {
        &self.cached_display
    }

    pub fn filtered_indexes(&self) -> &[usize] {
        &self.cached_filtered
    }

    /// Recompute the cached filtered/display slices. Call after any change to
    /// filter, sort, grouping, or the env list.
    pub fn rebuild_view(&mut self) {
        // Filtered indexes.
        self.cached_filtered.clear();
        if self.filter.is_empty() {
            self.cached_filtered.extend(0..self.environments.len());
        } else {
            let needle = self.filter.text().to_lowercase();
            for (i, e) in self.environments.iter().enumerate() {
                let alias_hit = self
                    .aliases
                    .get(&e.name)
                    .map(|a| a.to_lowercase().contains(&needle))
                    .unwrap_or(false);
                if e.name.to_lowercase().contains(&needle)
                    || alias_hit
                    || e.application.to_lowercase().contains(&needle)
                    || e.health.to_lowercase().contains(&needle)
                    || e.status.to_lowercase().contains(&needle)
                {
                    self.cached_filtered.push(i);
                }
            }
        }

        // Display rows (with optional group separators).
        self.cached_display.clear();
        let mut prev_app: Option<&str> = None;
        for i in &self.cached_filtered {
            let e = &self.environments[*i];
            if self.grouped && prev_app.is_some() && prev_app != Some(e.application.as_str()) {
                self.cached_display.push(DisplayRow::Separator);
            }
            self.cached_display.push(DisplayRow::Env(*i));
            prev_app = Some(e.application.as_str());
        }

        // Per-application palette colour cache. Assigned by order of first
        // appearance in the filtered view; rebuilt here so the render path
        // can do an O(1) lookup instead of building this map per frame.
        self.cached_app_colors = assign_app_colors(
            self.cached_filtered
                .iter()
                .map(|i| self.environments[*i].application.as_str()),
            &self.theme.app_palette,
        );

        // Stale-platform cache: parse each env's solution stack against the
        // available-versions catalogue once here, so the render path looks
        // up `env_name → newer version` instead of re-parsing per row per
        // frame. Empty while `latest_stacks` hasn't loaded yet.
        self.cached_stale_platforms.clear();
        if !self.latest_stacks.is_empty() {
            for e in &self.environments {
                if let Some(newer) =
                    crate::aws::newer_stack_version(&e.solution_stack, &self.latest_stacks)
                {
                    self.cached_stale_platforms.insert(e.name.clone(), newer);
                }
            }
        }
    }

    fn apply_refresh(&mut self, result: Result<Vec<Environment>, String>) {
        match result {
            Ok(envs) => {
                // Track newly-Red transitions for the anomaly highlight.
                let is_red =
                    |h: &str| h.eq_ignore_ascii_case("Red") || h.eq_ignore_ascii_case("Severe");
                self.newly_red.clear();
                // Compute newly-added envs *before* swapping prev_health
                // below — once we overwrite it, "previously unseen" is no
                // longer derivable. Skip the first refresh (prev_health is
                // empty then) so every env doesn't get flagged on startup.
                self.newly_added.clear();
                if !self.prev_health.is_empty() {
                    for e in &envs {
                        if !self.prev_health.contains_key(&e.name) {
                            self.newly_added.insert(e.name.clone());
                        }
                    }
                }
                for e in &envs {
                    let prev_red = self
                        .prev_health
                        .get(&e.name)
                        .map(|h| is_red(h))
                        .unwrap_or(false);
                    if is_red(&e.health) && !prev_red {
                        self.newly_red.insert(e.name.clone());
                        // Surface the transition via tracing + the audit log
                        // so operators can wire their own notifier (Slack,
                        // pager, etc.) off the audit stream. The previous
                        // built-in `webhook_url` POST was trimmed — too rigid
                        // for real ops workflows.
                        tracing::warn!(
                            env = %e.name,
                            application = %e.application,
                            health = %e.health,
                            region = %self.context.region,
                            "env transitioned into Red",
                        );
                        crate::audit::append_raw(
                            self.context.account_id.as_deref(),
                            self.context.profile.as_deref(),
                            &self.context.region,
                            &format!(
                                "stage=event kind=red_transition env={} application={} health={}",
                                e.name, e.application, e.health
                            ),
                        );
                    }
                }
                // Compute health + status deltas before swapping prev maps.
                self.health_delta = bucket_delta(&self.prev_health, &envs, |e| e.health.clone());
                self.status_delta = bucket_delta(&self.prev_status, &envs, |e| e.status.clone());

                self.prev_health = envs
                    .iter()
                    .map(|e| (e.name.clone(), e.health.clone()))
                    .collect();
                self.prev_status = envs
                    .iter()
                    .map(|e| (e.name.clone(), e.status.clone()))
                    .collect();

                let new_alerts = compute_red_alerts(&envs, &self.worker_dlq_depths);
                if self.notify_bell && new_alerts > self.prev_alerts {
                    // BEL — write to stderr and flush so the terminal rings
                    // immediately even though we're in the alt screen.
                    use std::io::Write;
                    let mut err = std::io::stderr().lock();
                    let _ = err.write_all(b"\x07");
                    let _ = err.flush();
                }
                self.prev_alerts = new_alerts;
                self.alerts = new_alerts;

                self.environments = envs;
                self.resort_envs();

                // Watchdog decision pass — single source of truth for
                // auto-rollback outcomes. Every armed watchdog gets
                // evaluated against the *freshly-applied* env list
                // (line above), eliminating the stale-cache race the
                // earlier "deadline handler dispatches inline" design
                // had: the deadline `tokio::spawn` now just sends an
                // `AutoRollbackCheck` message whose handler kicks a
                // manual refresh, so by the time we reach here the
                // health field is current.
                //
                // Three outcomes per armed env:
                //   1. Env is Green/Ok → drain, pin status.
                //   2. Env still non-Green AND deadline passed →
                //      dispatch the rollback redeploy.
                //   3. Else → keep armed; check again next refresh.
                let armed: Vec<(String, chrono::DateTime<chrono::Utc>)> = self
                    .armed_watchdogs
                    .iter()
                    .map(|(env, w)| (env.clone(), w.deadline_at))
                    .collect();
                let now = chrono::Utc::now();
                for (env_name, deadline_at) in armed {
                    let Some((status, health)) = self
                        .environments
                        .iter()
                        .find(|e| e.name == env_name)
                        .map(|e| (e.status.clone(), e.health.clone()))
                    else {
                        // Env left the fleet (terminated mid-watch) —
                        // disarm instead of dispatching a doomed
                        // redeploy at the deadline.
                        self.armed_watchdogs.remove(&env_name);
                        self.pin_status(format!(
                            "auto-rollback for {env_name}: env no longer in the fleet — watchdog disarmed"
                        ));
                        continue;
                    };
                    let healthy = deploy_settled_green(&status, &health);
                    if healthy {
                        self.armed_watchdogs.remove(&env_name);
                        // pin_status survives the same-tick auto-clear
                        // at the bottom of apply_refresh — without it
                        // the disarm message gets wiped before the
                        // operator ever sees it.
                        self.pin_status(format!(
                            "auto-rollback for {env_name}: env reached Green, watchdog disarmed"
                        ));
                    } else if now >= deadline_at {
                        // Deadline reached, env still bad. Dispatch
                        // the redeploy using the just-refreshed health
                        // so the audit line is accurate.
                        self.dispatch_auto_rollback(env_name, health);
                    }
                    // else: still armed, next refresh re-evaluates.
                }

                // Wait-for-green watcher decision pass. Same shape as
                // armed_watchdogs, but the resolution is purely
                // observational — no follow-on dispatch. Three outcomes:
                //   1. Env is Green/Ok → drain, pin success.
                //   2. Deadline passed and env still non-Green → drain,
                //      pin timeout error (operator decides next move).
                //   3. Else → keep watching, re-check next refresh.
                let watching: Vec<(String, chrono::DateTime<chrono::Utc>, String, u64)> = self
                    .watching_deploys
                    .iter()
                    .map(|(env, w)| {
                        let secs = (w.deadline_at - w.armed_at).num_seconds().max(0) as u64;
                        (env.clone(), w.deadline_at, w.target_label.clone(), secs)
                    })
                    .collect();
                for (env_name, deadline_at, target_label, total_secs) in watching {
                    let Some((status, health)) = self
                        .environments
                        .iter()
                        .find(|e| e.name == env_name)
                        .map(|e| (e.status.clone(), e.health.clone()))
                    else {
                        // Env left the fleet — stop watching with an
                        // honest note rather than timing out later.
                        self.watching_deploys.remove(&env_name);
                        self.pin_error(format!(
                            "deploy watch for {env_name}: env no longer in the fleet"
                        ));
                        continue;
                    };
                    let healthy = deploy_settled_green(&status, &health);
                    if healthy {
                        self.watching_deploys.remove(&env_name);
                        let label_hint = if target_label.is_empty() {
                            String::new()
                        } else {
                            format!(" ({target_label})")
                        };
                        self.pin_status(format!("✓ deploy reached Green: {env_name}{label_hint}"));
                    } else if now >= deadline_at {
                        self.watching_deploys.remove(&env_name);
                        let label_hint = if target_label.is_empty() {
                            String::new()
                        } else {
                            format!(" ({target_label})")
                        };
                        self.pin_error(format!(
                            "deploy did not reach Green within {total_secs}s: {env_name}{label_hint} — status={status} health={health}"
                        ));
                    }
                }

                let live: HashSet<String> =
                    self.environments.iter().map(|e| e.name.clone()).collect();
                for e in &self.environments {
                    let buf = self.history.entry(e.name.clone()).or_default();
                    buf.push_back(e.health.clone());
                    while buf.len() > HISTORY_CAP {
                        buf.pop_front();
                    }
                }
                self.history.retain(|k, _| live.contains(k));

                self.arm_loading_linger();
                self.load_state = LoadState::Idle;
                self.loading_since = None;
                self.last_refresh = Some(chrono::Utc::now());
                // A successful refresh resets the throttle back-off so the
                // next throttle (if any) starts again from the base interval.
                self.consecutive_throttles = 0;
                self.throttle_until = None;
                // Clear status/error only if the user hasn't replaced them
                // during the refresh round-trip. Otherwise their action message
                // (sort change, alias set, …) would get clobbered here.
                if let Some((prev_status, prev_error)) = self.status_snapshot_at_refresh.take() {
                    // Don't auto-clear user-pinned messages — those are
                    // results the operator just asked for and would lose
                    // every 15s otherwise.
                    if !self.status_message_pinned && self.status_message == prev_status {
                        self.status_message = None;
                    }
                    if self.error_message == prev_error {
                        self.error_message = None;
                    }
                } else if !self.status_message_pinned {
                    self.status_message = None;
                    self.error_message = None;
                }
                // Pin lasts one refresh cycle. After that the message
                // survives in the slot but the next ephemeral write (e.g.
                // a spawn helper's "fetching…") gets normal auto-clear
                // semantics again.
                self.status_message_pinned = false;
                self.restore_or_clamp_selection();
                // Fan out DLQ depth checks for Worker-tier envs. Result
                // lands as `AppMsg::WorkerQueueCheck` and updates the
                // alert count + the in-row `⚠ DLQ:N` chip on the next
                // draw.
                self.spawn_worker_queue_check();
                // Same fan-out shape for the INST column: per-env
                // `DescribeEnvironmentHealth` summarised down to
                // `(healthy, total)`. Cache rebuilt from results in
                // `handle_env_instance_counts`.
                self.spawn_env_instance_counts();
            }
            Err(msg) => {
                tracing::error!(error = %msg, "refresh failed");
                self.arm_loading_linger();
                self.load_state = LoadState::Error;
                self.loading_since = None;
                self.status_snapshot_at_refresh = None;
                if is_throttling_error(&msg) {
                    let backoff =
                        throttle_backoff(self.refresh_interval, self.consecutive_throttles);
                    self.consecutive_throttles = self.consecutive_throttles.saturating_add(1);
                    self.throttle_until = Some(Instant::now() + backoff);
                    self.error_message = Some(format!(
                        "rate-limited by AWS — backing off {}s (^R to force)",
                        backoff.as_secs().max(1)
                    ));
                } else {
                    self.error_message = Some(self.format_aws_error("refresh", &msg));
                }
            }
        }
    }

    fn restore_or_clamp_selection(&mut self) {
        if self.cached_display.is_empty() {
            self.table_state.select(None);
            return;
        }
        let first_env_idx = self
            .cached_display
            .iter()
            .position(|r| matches!(r, DisplayRow::Env(_)))
            .unwrap_or(0);
        let pending = self.pending_select.take();
        if let Some(name) = pending {
            let pos = self.cached_display.iter().position(|r| match r {
                DisplayRow::Env(i) => self.environments[*i].name == name,
                DisplayRow::Separator => false,
            });
            if let Some(p) = pos {
                self.table_state.select(Some(p));
                return;
            }
        }
        let valid = self
            .table_state
            .selected()
            .is_some_and(|s| matches!(self.cached_display.get(s), Some(DisplayRow::Env(_))));
        if !valid {
            self.table_state.select(Some(first_env_idx));
        }
    }

    fn format_aws_error(&self, op: &str, msg: &str) -> String {
        let profile = self
            .override_profile
            .clone()
            .or_else(|| self.context.profile.clone())
            .unwrap_or_else(|| "default".into());
        if let Some(rewritten) = crate::aws::rewrite_credential_error(&profile, msg) {
            // The TUI appends its own refresh hint; the shared
            // rewrite carries the actionable command only.
            return match rewritten {
                crate::aws::CredentialHint::Expired(text) => {
                    format!("{text}  (or refresh your creds, then press Ctrl-R)")
                }
                crate::aws::CredentialHint::Invalid(text) => {
                    format!("{text}  (or press `p` to pick a different profile)")
                }
            };
        }
        format!("{op} failed: {msg}")
    }
}

fn is_text_input(key: &KeyEvent) -> bool {
    // Allow plain text and shifted text (capital letters); block Ctrl/Alt/Super.
    let m = key.modifiers;
    !m.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER)
}

#[derive(Debug, Clone, Copy)]
pub enum YankKind {
    Cname,
    Name,
}

#[derive(Debug, Clone, Copy)]
pub enum DisplayRow {
    Env(usize),
    Separator,
}

/// Drive the tail-log capture pipeline end-to-end:
/// 1. `RequestEnvironmentInfo` to kick EB into producing samples.
/// 2. Poll `RetrieveEnvironmentInfo` until pre-signed S3 URLs appear or we
///    hit the attempt cap.
/// 3. Fetch each URL (sequentially — typically only 1-3 instances; serial
///    keeps error handling simple and avoids hammering S3).
///
/// Progress messages are emitted via `tx` so the UI advances through the
/// Requesting → Polling → Fetching → Ready states while this future runs.
async fn collect_tail_logs(
    aws: Arc<AwsClient>,
    env_name: String,
    tx: mpsc::UnboundedSender<AppMsg>,
    gen: u64,
) -> std::result::Result<Vec<(String, String)>, String> {
    const POLL_ATTEMPTS: u32 = 12;
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    aws.request_env_info_tail(&env_name)
        .await
        .map_err(|e| flatten_err("request_env_info_tail", e))?;
    let _ = tx.send(AppMsg::DetailLogsProgress {
        gen,
        env_name: env_name.clone(),
        stage: LogTailStage::Polling,
        attempt: 0,
    });

    let mut urls: Vec<(String, String)> = Vec::new();
    for attempt in 1..=POLL_ATTEMPTS {
        tokio::time::sleep(POLL_INTERVAL).await;
        urls = aws
            .retrieve_env_info_tail(&env_name)
            .await
            .map_err(|e| flatten_err("retrieve_env_info_tail", e))?;
        if !urls.is_empty() {
            break;
        }
        let _ = tx.send(AppMsg::DetailLogsProgress {
            gen,
            env_name: env_name.clone(),
            stage: LogTailStage::Polling,
            attempt,
        });
    }
    if urls.is_empty() {
        return Err(format!(
            "no tail samples uploaded after {}s — instance role may lack s3:PutObject on the EB info bucket",
            POLL_ATTEMPTS as u64 * POLL_INTERVAL.as_secs()
        ));
    }
    let _ = tx.send(AppMsg::DetailLogsProgress {
        gen,
        env_name: env_name.clone(),
        stage: LogTailStage::Fetching,
        attempt: 0,
    });

    let mut out = Vec::with_capacity(urls.len());
    for (instance_id, url) in urls {
        match AwsClient::fetch_url_text(&url).await {
            Ok(text) => out.push((instance_id, text)),
            Err(e) => out.push((instance_id, format!("(fetch failed: {e})"))),
        }
    }
    Ok(out)
}

/// Pre-flight signal for the confirm modal: looks at the env's current state
/// at action-open time and returns a one-line warning when something
/// noteworthy is in progress (mid-deploy, recently updated, currently in
/// Updating / Terminating). `None` for envs that look quiet. Pure function so
/// the rule set can be pinned down with unit tests.
pub fn compute_traffic_warning(env: &Environment) -> Option<String> {
    let status_lower = env.status.to_lowercase();
    if status_lower.contains("updating") || status_lower.contains("launching") {
        return Some(format!("ACTIVE DEPLOY: status={}", env.status));
    }
    if status_lower.contains("terminating") {
        return Some(format!("env is {} already", env.status));
    }
    if let Some(updated) = env.updated {
        let dur = chrono::Utc::now().signed_duration_since(updated);
        if dur >= chrono::Duration::zero() && dur < chrono::Duration::minutes(5) {
            return Some(format!(
                "RECENT CHANGE: updated {}s ago",
                dur.num_seconds().max(0)
            ));
        }
    }
    if env.health.eq_ignore_ascii_case("Red") || env.health.eq_ignore_ascii_case("Severe") {
        return Some(format!("env is currently {}", env.health));
    }
    None
}

/// Recognise AWS throttling error messages. The SDK surfaces these via the
/// `ThrottlingException` code (EB, STS) or `RequestLimitExceeded` (older
/// services). Match case-insensitively against the flattened error string so
/// that exact framing of the message doesn't matter.
/// Pure: count "Red-equivalent" alerts across the env list. An env counts
/// as alert-worthy when either (a) EB reports its health as Red / Severe,
/// or (b) it's a Worker-tier env with `worker_dlq_depths.get(name) > 0`.
/// The two predicates are disjoint per env, so a worker that's both
/// EB-Red and DLQ-loaded is counted once.
pub(crate) fn compute_red_alerts(
    envs: &[crate::aws::Environment],
    worker_dlq_depths: &std::collections::HashMap<String, i64>,
) -> usize {
    envs.iter()
        .filter(|e| {
            let eb_red =
                e.health.eq_ignore_ascii_case("Red") || e.health.eq_ignore_ascii_case("Severe");
            let dlq_red = e.tier.eq_ignore_ascii_case("Worker")
                && worker_dlq_depths.get(&e.name).copied().unwrap_or(0) > 0;
            eb_red || dlq_red
        })
        .count()
}

pub(crate) fn is_throttling_error(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    [
        "throttling",
        "throttlingexception",
        "requestlimitexceeded",
        "too many requests",
        "rate exceeded",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// Exponential back-off horizon: 2× base on the first throttle, doubling each
/// consecutive failure, capped at 5 minutes. The 5 min cap keeps the app
/// responsive when the throttle clears — the user shouldn't have to wait
/// arbitrarily long after rate limits ease.
/// Pure: given the moment a load started and the display constants, return
/// the instant the loading indicator should remain visible until (if it
/// was visible at all). Returns `None` when the load completed before the
/// indicator's display threshold, signalling "no linger needed".
pub fn compute_loading_linger_target(
    loading_since: Option<Instant>,
    threshold: Duration,
    linger: Duration,
    now: Instant,
) -> Option<Instant> {
    let elapsed = loading_since.map(|t| now.duration_since(t))?;
    if elapsed >= threshold {
        Some(now + linger)
    } else {
        None
    }
}

fn throttle_backoff(base: Duration, consecutive: u32) -> Duration {
    const MAX_BACKOFF: Duration = Duration::from_secs(300);
    let factor: u32 = 2u32.saturating_pow(consecutive.min(6).saturating_add(1));
    let scaled = base.saturating_mul(factor);
    scaled.min(MAX_BACKOFF)
}

/// Assign palette colours to application names in order of first appearance.
/// Once the palette is exhausted, colours wrap around (so the 17th distinct app
/// reuses the first colour, etc.). With an empty palette the result is empty —
/// callers should fall back to a default text colour.
fn assign_app_colors<'a>(
    names: impl IntoIterator<Item = &'a str>,
    palette: &[ratatui::style::Color],
) -> HashMap<String, ratatui::style::Color> {
    let mut out: HashMap<String, ratatui::style::Color> = HashMap::new();
    if palette.is_empty() {
        return out;
    }
    for name in names {
        if !out.contains_key(name) {
            let idx = out.len() % palette.len();
            out.insert(name.to_string(), palette[idx]);
        }
    }
    out
}

impl App {
    fn yank_event_at(&mut self, idx: usize) {
        let Some(ev) = self.event_panel.events.get(idx) else {
            self.event_panel.cursor = None;
            return;
        };
        let when = ev
            .at
            .map(|t| {
                t.with_timezone(&chrono::Local)
                    .format("%Y-%m-%d %H:%M:%S")
                    .to_string()
            })
            .unwrap_or_else(|| "—".into());
        let line = format!("{when}  [{}]  {}  {}", ev.severity, ev.env, ev.message);
        match yank(&line) {
            Ok(()) => {
                self.status_message = Some(format!(
                    "yanked event line ({} chars)",
                    line.chars().count()
                ));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }
}

/// Pure: build the reverse-action of an option-settings write by
/// looking up the affected (namespace, name) pairs in the
/// pre-write snapshot. Keys that previously had a value get
/// reversed via `to_set` (restore old value); keys that were
/// previously unset get reversed via `to_remove` (drop the key).
///
/// EB's option-settings API doesn't distinguish "unset" from
/// "set to empty string" — we treat empty-string-prior as unset
/// (the common case) so the reverse cleanly removes the key
/// rather than leaving it as a literal empty string.
pub(crate) fn build_undo_entry(
    env_name: &str,
    original_summary: &str,
    to_set: &[(String, String, String)],
    to_remove: &[(String, String)],
    pre_write: &[(String, String, String)],
) -> UndoEntry {
    let lookup = |ns: &str, name: &str| -> Option<&String> {
        pre_write
            .iter()
            .find(|(n, k, _)| n == ns && k == name)
            .map(|(_, _, v)| v)
    };
    let mut reverse_set: Vec<(String, String, String)> = Vec::new();
    let mut reverse_remove: Vec<(String, String)> = Vec::new();
    // For each key the original write SET, the reverse is either
    // (a) restore the prior value, or (b) remove the key if it
    // was previously unset / empty.
    for (ns, name, _) in to_set {
        match lookup(ns, name) {
            Some(prev) if !prev.is_empty() => {
                reverse_set.push((ns.clone(), name.clone(), prev.clone()));
            }
            _ => {
                reverse_remove.push((ns.clone(), name.clone()));
            }
        }
    }
    // For each key the original write REMOVED, the reverse is to
    // restore the prior value — but only if there was one. If the
    // key was already absent, the remove was a no-op and the
    // reverse is nothing.
    for (ns, name) in to_remove {
        if let Some(prev) = lookup(ns, name) {
            if !prev.is_empty() {
                reverse_set.push((ns.clone(), name.clone(), prev.clone()));
            }
        }
    }
    UndoEntry {
        env_name: env_name.to_string(),
        to_set: reverse_set,
        to_remove: reverse_remove,
        original_summary: original_summary.to_string(),
        captured_at: chrono::Utc::now(),
    }
}

fn yank(text: &str) -> std::result::Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text.to_string()).map_err(|e| e.to_string())
}

/// Pair every async AWS error with a full-chain log entry. The returned string
/// is the SDK's top-level `Display` (concise, suitable for the toast/footer);
/// the chain — including the underlying `dyn Error` causes that color-eyre
/// records on `Report` — goes to `ebman.log` via `tracing::error!`. Without
/// this the chain was lost both from the UI and the log.
/// Which EC2 surface a MultiSelect form is pulling its option list from.
/// Drives both the EC2 API call and the option-setting target so the
/// pickers share `open_multi_select_form` without conditional branches.
#[derive(Copy, Clone, Debug)]
enum MultiSelectFlavour {
    Subnets,
    /// Subnets attached to the env's ELB (web tier). Same EC2 list call
    /// as `Subnets` but writes to a different option setting and
    /// pre-fills from a different field on the env's VPC context.
    ElbSubnets,
    SecurityGroups,
}

/// Fetch VPC context + EC2 inventory + current selection for a MultiSelect
/// picker, in parallel. Returns the data the form's field needs to flip
/// from Loading → Ready.
async fn load_multi_select(
    aws: Arc<crate::aws::AwsClient>,
    app_name: &str,
    env_name: &str,
    flavour: MultiSelectFlavour,
) -> Result<MultiSelectOptions, String> {
    let ctx = aws
        .fetch_env_vpc_context(app_name, env_name)
        .await
        .map_err(|e| flatten_err("fetch_env_vpc_context", e))?;
    let Some(vpc_id) = ctx.vpc_id.as_deref() else {
        return Err("env has no VPC id in its option settings — using account-default VPC?".into());
    };
    match flavour {
        MultiSelectFlavour::Subnets | MultiSelectFlavour::ElbSubnets => {
            let subnets = aws
                .list_subnets_in_vpc(vpc_id)
                .await
                .map_err(|e| flatten_err("list_subnets_in_vpc", e))?;
            let mut options = Vec::with_capacity(subnets.len());
            let mut annotations = Vec::with_capacity(subnets.len());
            for s in subnets {
                options.push(s.id.clone());
                let mut annot = format!("({} · {}", s.availability_zone, s.cidr_block);
                if let Some(name) = s.name_tag.as_ref().filter(|n| !n.is_empty()) {
                    annot.push_str(" · ");
                    annot.push_str(name);
                }
                annot.push(')');
                annotations.push(annot);
            }
            let initial = match flavour {
                MultiSelectFlavour::ElbSubnets => ctx.elb_subnets,
                _ => ctx.subnets,
            };
            Ok(MultiSelectOptions {
                options,
                annotations,
                initial,
            })
        }
        MultiSelectFlavour::SecurityGroups => {
            let groups = aws
                .list_security_groups_in_vpc(vpc_id)
                .await
                .map_err(|e| flatten_err("list_security_groups_in_vpc", e))?;
            let mut options = Vec::with_capacity(groups.len());
            let mut annotations = Vec::with_capacity(groups.len());
            for g in groups {
                options.push(g.id.clone());
                let desc_suffix = if g.description.is_empty() {
                    String::new()
                } else {
                    format!(" — {}", g.description)
                };
                annotations.push(format!("({}{desc_suffix})", g.group_name));
            }
            Ok(MultiSelectOptions {
                options,
                annotations,
                initial: ctx.security_groups,
            })
        }
    }
}

/// Load the cert picker for `:listener-edit`: the region's ACM
/// certificates as options, plus the listener's current
/// `SSLCertificateArns` as the pre-selected `initial` set.
async fn load_listener_certs(
    aws: Arc<crate::aws::AwsClient>,
    app_name: &str,
    env_name: &str,
    port: &str,
) -> Result<MultiSelectOptions, String> {
    let certs = aws
        .list_certificates()
        .await
        .map_err(|e| flatten_err("list_certificates", e))?;
    let listeners = aws
        .fetch_env_listeners(app_name, env_name)
        .await
        .map_err(|e| flatten_err("fetch_env_listeners", e))?;
    let initial: Vec<String> = listeners
        .iter()
        .find(|(p, opt, _)| p == port && opt == "SSLCertificateArns")
        .map(|(_, _, v)| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default();
    let mut options = Vec::with_capacity(certs.len());
    let mut annotations = Vec::with_capacity(certs.len());
    for c in certs {
        options.push(c.arn);
        annotations.push(if c.domain.is_empty() {
            String::new()
        } else {
            format!("({})", c.domain)
        });
    }
    Ok(MultiSelectOptions {
        options,
        annotations,
        initial,
    })
}

/// Set once at `--demo` app construction: the fail-loudly stub client
/// errors on every call BY DESIGN, so logging each at ERROR just
/// spams the log during demos/screenshots. Real mode keeps the loud
/// contract (a genuine AWS failure IS an error worth seeing).
pub(crate) static DEMO_QUIET_AWS_ERRORS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

fn flatten_err(op: &str, e: color_eyre::eyre::Report) -> String {
    if DEMO_QUIET_AWS_ERRORS.load(std::sync::atomic::Ordering::Relaxed) {
        tracing::debug!(target: "ebman::aws", op = op, error = ?e, "aws call failed (demo stub)");
    } else {
        tracing::error!(target: "ebman::aws", op = op, error = ?e, "aws call failed");
    }
    flatten_err_to_string(&e)
}

/// Pure: convert an `eyre::Report` into the user-facing string we route into
/// toasts and the refresh-error path. The SDK's `Display` impl returns
/// generic strings like `"service error"` for throttling — the structured
/// AWS error codes (`ThrottlingException`, `AccessDenied`, etc.) live in
/// the `Debug` form. To keep toasts clean *and* let downstream predicates
/// like [`is_throttling_error`] do their job, we peek at the Debug dump
/// for known error codes and surface a clean `"<CodeName>: ..."` prefix.
/// All other errors pass through with Display unchanged.
pub(crate) fn flatten_err_to_string(e: &color_eyre::eyre::Report) -> String {
    let display = e.to_string();
    let dbg_lower = format!("{e:?}").to_lowercase();
    // Throttling tokens — kept in sync with `is_throttling_error` so the
    // predicate and the surfaced prefix can't drift.
    const THROTTLING_TOKENS: &[&str] = &[
        "throttling",
        "throttlingexception",
        "requestlimitexceeded",
        "too many requests",
        "rate exceeded",
    ];
    if THROTTLING_TOKENS.iter().any(|t| dbg_lower.contains(t)) {
        return format!("ThrottlingException: {display}");
    }
    // IAM / authorisation failures — operators hit these constantly when
    // bouncing between profiles. A clean prefix points them at the policy
    // gap rather than burying it in the SDK chain dump.
    const ACCESS_TOKENS: &[&str] = &[
        "accessdenied",
        "accessdeniedexception",
        "unauthorizedoperation",
        "not authorized to perform",
    ];
    if ACCESS_TOKENS.iter().any(|t| dbg_lower.contains(t)) {
        return format!("AccessDenied: {display}");
    }
    // Missing-resource errors. EB / S3 / SQS each have their own variant
    // names — surface a uniform NotFound prefix so operators don't have
    // to learn the per-service vocabulary.
    const NOTFOUND_TOKENS: &[&str] = &[
        "resourcenotfoundexception",
        "nosuchentity",
        "nosuchbucket",
        "nosuchkey",
        "queuedoesnotexist",
        "environmentnotfound",
        "applicationversionnotfound",
    ];
    if NOTFOUND_TOKENS.iter().any(|t| dbg_lower.contains(t)) {
        return format!("NotFound: {display}");
    }
    // Dependency conflicts — usually "can't delete X, Y still references it".
    const DEPENDENCY_TOKENS: &[&str] = &[
        "dependencyviolation",
        "resourceinuse",
        "operationinprogressexception",
        "invalidrequestexception",
    ];
    if DEPENDENCY_TOKENS.iter().any(|t| dbg_lower.contains(t)) {
        return format!("Conflict: {display}");
    }
    // Expired SSO / STS credentials — surface the rewrite the
    // ExpiredToken handler already does, in case the error reaches
    // this path via a different route.
    if dbg_lower.contains("expiredtoken") || dbg_lower.contains("tokenexpired") {
        return format!("ExpiredToken: {display}");
    }
    display
}

/// Bucketed delta between two snapshots. `prev` is a per-env-name → bucket
/// snapshot from the previous refresh; `next` is the new env list. The accessor
/// extracts the bucket label (e.g. health or status). The result is sorted with
/// non-zero changes only, bucket-alphabetical.
/// Build the palette item list from current app state. Items are returned in a
/// stable order (commands first, then envs, then views, then plugins); ranking
/// happens at filter time.
fn build_palette_items(app: &App) -> Vec<PaletteItem> {
    let mut out: Vec<PaletteItem> = Vec::new();

    // Built-in commands — generated from `crate::commands::COMMANDS` so
    // the registry, the palette, and the help screen can't drift apart.
    // ZeroArg → Enter executes; Prefill → Enter switches to command-bar
    // mode with the prefix typed in; Hidden → skipped here.
    for c in crate::commands::COMMANDS {
        match c.kind {
            crate::commands::CommandKind::ZeroArg => {
                out.push(PaletteItem {
                    label: format!(":{}", c.name),
                    detail: c.help.to_string(),
                    kind_tag: "cmd",
                    action: PaletteAction::RunCommand(c.name.to_string()),
                });
            }
            crate::commands::CommandKind::Prefill(prefix) => {
                out.push(PaletteItem {
                    label: format!(":{}", prefix.trim_end()),
                    detail: c.help.to_string(),
                    kind_tag: "cmd",
                    action: PaletteAction::PrefillCommand(prefix.to_string()),
                });
            }
        }
    }

    // Envs — jump cursor.
    for e in &app.environments {
        let alias = app
            .aliases
            .get(&e.name)
            .map(|a| format!("  ({a})"))
            .unwrap_or_default();
        out.push(PaletteItem {
            label: e.name.clone(),
            detail: format!("env in {}{alias}  ·  {}", e.application, e.health),
            kind_tag: "env",
            action: PaletteAction::JumpEnv(e.name.clone()),
        });
    }

    // Saved views.
    for name in app.saved_views.keys() {
        out.push(PaletteItem {
            label: format!("view: {name}"),
            detail: "load saved view".into(),
            kind_tag: "view",
            action: PaletteAction::LoadView(name.clone()),
        });
    }

    // Plugins.
    for (name, plugin) in &app.plugins {
        out.push(PaletteItem {
            label: format!(":{name}"),
            detail: plugin
                .description
                .clone()
                .unwrap_or_else(|| format!("plugin: {}", plugin.template)),
            kind_tag: "plugin",
            action: PaletteAction::RunCommand(name.clone()),
        });
    }

    out
}

/// Score a palette item against the needle. Lower is better; `None` means no
/// match. Score is: prefix match → 0; substring → byte index of first match.
/// Detail string is also searched, with a penalty so label matches rank higher.
fn palette_score(needle: &str, label: &str, detail: &str) -> Option<isize> {
    if needle.is_empty() {
        return Some(0);
    }
    let l = label.to_lowercase();
    let d = detail.to_lowercase();
    if let Some(i) = l.find(needle) {
        return Some(i as isize);
    }
    if let Some(i) = d.find(needle) {
        return Some(1_000 + i as isize);
    }
    None
}

fn bucket_delta<F>(
    prev: &HashMap<String, String>,
    next: &[Environment],
    accessor: F,
) -> Vec<(String, i32)>
where
    F: Fn(&Environment) -> String,
{
    // Only count envs present in *both* sides. Disappearing envs aren't a
    // transition (they just left), and new envs aren't a transition either
    // (no previous state to compare). This also makes a cleared `prev`
    // (e.g. after a context switch) produce zero deltas, instead of spamming
    // +N for every bucket the first time the new context loads.
    let mut prev_counts: BTreeMap<String, i32> = BTreeMap::new();
    let mut next_counts: BTreeMap<String, i32> = BTreeMap::new();
    for e in next {
        if let Some(prev_bucket) = prev.get(&e.name) {
            *prev_counts.entry(prev_bucket.clone()).or_insert(0) += 1;
            *next_counts.entry(accessor(e)).or_insert(0) += 1;
        }
    }
    let mut keys: BTreeMap<String, ()> = BTreeMap::new();
    for k in prev_counts.keys().chain(next_counts.keys()) {
        keys.insert(k.clone(), ());
    }
    keys.into_keys()
        .filter_map(|k| {
            let p = *prev_counts.get(&k).unwrap_or(&0);
            let n = *next_counts.get(&k).unwrap_or(&0);
            let d = n - p;
            if d != 0 {
                Some((k, d))
            } else {
                None
            }
        })
        .collect()
}

/// Helper: write the outcome audit line and send the AppMsg in one place
/// so each of the four early-return paths in `spawn_deploy_from_local`
/// stays one line. Free function (not a method) so it can be called from
/// the async closure without borrowing `self`.
#[allow(clippy::too_many_arguments)]
fn finish_deploy_from_local(
    tx: &tokio::sync::mpsc::UnboundedSender<AppMsg>,
    gen: u64,
    env_name: String,
    label: String,
    summary: String,
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    result: Result<(), String>,
) {
    crate::audit::append_action_completed(
        account,
        profile,
        region,
        "DeployFromLocal",
        &env_name,
        result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
        &[("label", &label)],
    );
    let _ = tx.send(AppMsg::DeployFromLocal {
        gen,
        env_name,
        label,
        summary,
        result,
    });
}

fn build_describe_cli(env_name: &str, region: &str, profile: Option<&str>) -> String {
    let env_q = shell_quote(env_name);
    let mut out = format!(
        "aws elasticbeanstalk describe-environments --environment-names {env_q} --region {region}"
    );
    if let Some(p) = profile {
        out.push_str(&format!(" --profile {}", shell_quote(p)));
    }
    out
}

/// Thin typed wrappers around the [`crate::audit`] module's writer
/// APIs. App-side callers pass [`Action`] (the typed enum) — these
/// adapt to the `action_label: &str` shape `audit::append_action_*`
/// expects. Same Debug-derived names (`Rebuild`, `Restart`, ...) the
/// audit log used pre-consolidation, so the wire format is unchanged.
fn write_audit_entry(
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    action: Action,
    env: &str,
    swap_with: Option<&str>,
) {
    let target = match swap_with {
        Some(other) => format!("{env} ↔ {other}"),
        None => env.to_string(),
    };
    crate::audit::append_action_dispatched(
        account,
        profile,
        region,
        &format!("{action:?}"),
        &target,
        &[],
    );
}

/// Log the outcome of a dispatched action. Thin wrapper around
/// [`crate::audit::append_action_completed`].
fn write_audit_outcome(
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    action: Action,
    env: &str,
    result: Result<(), &str>,
) {
    crate::audit::append_action_completed(
        account,
        profile,
        region,
        &format!("{action:?}"),
        env,
        result,
        &[],
    );
}

/// One action (or batch of actions) queued for dispatch with a brief
/// cancel window. After the operator authorises a confirm (Y on a
/// YesNo modal, typed name on a TypeName modal) or runs a
/// `:batch-*` command, ebman doesn't fire the AWS call immediately —
/// it holds the dispatch here, shows a countdown in the header, and
/// fires only when [`UNDO_WINDOW`] elapses. `U` in Normal mode
/// aborts before the deadline.
///
/// One pending dispatch at a time. The `kind` carries the work
/// shape; the deadline + display labels are shared.
#[derive(Clone)]
pub struct PendingDispatch {
    pub deadline: Instant,
    /// Label rendered in the header pill — `"Rebuild env"` or
    /// `"Batch rebuild × 5"`. Captured at queue time so the
    /// rendering doesn't have to walk the kind on every frame.
    pub label: String,
    /// Display target. For singles it's the env name; for batches
    /// it's the count summary (`"5 envs"`) so the pill stays compact.
    pub target: String,
    pub kind: PendingDispatchKind,
}

/// The actual work `tick_pending_dispatch` dispatches when the
/// cancel window elapses. Mirrors the existing dispatch paths:
/// `Single` re-uses [`App::spawn_action`]; the batch variants
/// re-use the per-env `spawn_batch_*` helpers in a loop.
// See the matching allow on `ActionFlow` — same trade-off.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub enum PendingDispatchKind {
    /// A single Y/TypeName-confirm dispatch — preserves the full
    /// `ConfirmModal` because `spawn_action` reads params off it
    /// (deploy version, swap target, scale min/max, etc.).
    Single { modal: ConfirmModal },
    /// `:batch-rebuild` / `:batch-restart` — one [`Action`] applied
    /// to every env in the captured set.
    BatchAction {
        action: Action,
        env_names: Vec<String>,
    },
    /// `:batch-deploy LABEL` — same version label fanned out.
    BatchDeploy {
        env_names: Vec<String>,
        version_label: String,
    },
    /// `:batch-tag KEY VALUE` (`value = Some`) / `:batch-untag KEY`
    /// (`value = None`). ARN per env captured at queue time so a
    /// mid-window refresh that drops an env's ARN can't break the
    /// fan-out.
    BatchTag {
        envs_with_arns: Vec<(String, String)>,
        key: String,
        value: Option<String>,
    },
    /// `:batch-set-option NAMESPACE NAME VALUE`.
    BatchSetOption {
        env_names: Vec<String>,
        namespace: String,
        option_name: String,
        value: String,
    },
}

/// Cancel window after a confirm — long enough that an "oops" reflex
/// can recover but short enough that operators don't notice it on a
/// deliberate action. The UX review flagged the absence of any
/// abort affordance after dispatch as a real safety gap.
pub const UNDO_WINDOW: Duration = Duration::from_secs(5);

/// TTL for `App.env_tag_cache` + `App.env_health_cache` — the lazy
/// caches that back `spawn_confirm_lint`'s parallel tag + health
/// fetches. 60s matches the operator's typical "open a confirm
/// modal, look at it, press Y" cadence — fresh enough that data
/// drift across rapid modal cycles is negligible; long enough that
/// repeated modal-opens against the same env benefit. 0.21
/// addition (lint input caching).
pub const LINT_INPUT_CACHE_TTL: Duration = Duration::from_secs(60);

/// Items the Apps-scope action overlay (`Overlay::AppsActionMenu`)
/// offers when the operator presses `a` from the Apps table. Each
/// dispatches via `cmd_batch_*` after seeding `multi_selected` with the
/// envs captured at menu-open time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppsActionItem {
    Drill,
    BatchRebuild,
    BatchRestart,
    BatchDeploy,
    OpenInConsole,
}

impl AppsActionItem {
    pub fn label(self) -> &'static str {
        match self {
            Self::Drill => "Drill into envs",
            Self::BatchRebuild => "Rebuild all envs in app",
            Self::BatchRestart => "Restart all envs in app",
            Self::BatchDeploy => "Deploy version label to all envs",
            Self::OpenInConsole => "Open application in AWS console",
        }
    }
}

/// Menu order — Drill at the top because it's the default action
/// operators reach for; OpenInConsole at the bottom so it's not the
/// thumb-stroke option.
pub const APPS_ACTION_ITEMS: &[AppsActionItem] = &[
    AppsActionItem::Drill,
    AppsActionItem::BatchRebuild,
    AppsActionItem::BatchRestart,
    AppsActionItem::BatchDeploy,
    AppsActionItem::OpenInConsole,
];

/// Pure: parse an AWS `AccessDenied` error message into
/// `(principal_arn, action)`. Returns `None` when the message
/// doesn't match a recognised shape.
///
/// Recognised shapes:
///   - `User: arn:aws:sts::ACCOUNT:assumed-role/ROLE/SESSION is
///     not authorized to perform: SERVICE:ACTION ...`
///   - `User: arn:aws:iam::ACCOUNT:{user,role}/NAME is not
///     authorized to perform: SERVICE:ACTION ...`
///
/// Assumed-role ARNs are rewritten to the underlying role ARN
/// (`arn:aws:iam::ACCOUNT:role/ROLE`) because that's what
/// `iam:SimulatePrincipalPolicy` wants as the policy source —
/// the session credentials themselves aren't a policy attachment
/// point.
pub(crate) fn parse_access_denied(msg: &str) -> Option<(String, String)> {
    let user_prefix = "User: ";
    let action_prefix = "is not authorized to perform:";
    let user_start = msg.find(user_prefix)? + user_prefix.len();
    let user_end = msg[user_start..]
        .find(|c: char| c.is_whitespace())
        .map(|i| user_start + i)?;
    let principal_raw = &msg[user_start..user_end];
    let action_start = msg.find(action_prefix)? + action_prefix.len();
    let action_rest = msg[action_start..].trim_start();
    let action_end = action_rest
        .find(|c: char| c.is_whitespace() || c == ',')
        .unwrap_or(action_rest.len());
    let action = action_rest[..action_end].to_string();
    let principal = if let Some(rest) = principal_raw.strip_prefix("arn:aws:sts::") {
        // `arn:aws:sts::ACCOUNT:assumed-role/ROLE/SESSION`
        let parts: Vec<&str> = rest.splitn(2, ':').collect();
        let account = parts.first()?;
        let role_part = parts.get(1)?;
        let role_name = role_part.strip_prefix("assumed-role/")?.split('/').next()?;
        format!("arn:aws:iam::{account}:role/{role_name}")
    } else {
        principal_raw.to_string()
    };
    Some((principal, action))
}

fn console_url(region: &str, app_name: &str, env_name: &str) -> String {
    let app = urlencode(app_name);
    let env = urlencode(env_name);
    format!(
        "https://{region}.console.aws.amazon.com/elasticbeanstalk/home?region={region}#/environment/dashboard?applicationName={app}&environmentName={env}"
    )
}

fn open_url(url: &str) -> std::result::Result<(), String> {
    #[cfg(target_os = "macos")]
    let cmd = "open";
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = "xdg-open";
    #[cfg(target_os = "windows")]
    let cmd = "explorer";

    #[cfg(not(any(unix, target_os = "windows")))]
    {
        let _ = url;
        return Err("don't know how to open a URL on this platform".into());
    }
    #[cfg(any(unix, target_os = "windows"))]
    {
        std::process::Command::new(cmd)
            .arg(url)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

// JSON-escape canonical helper lives in `crate::util`; brought into
// scope locally for the `format!` sites in this module.
use crate::util::json_escape;

#[cfg(test)]
mod tests;
