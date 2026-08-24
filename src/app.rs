//! The `App` type: everything the TUI knows and everything it can do.
//!
//! `App` owns the AWS context, the fetched world (`environments`,
//! `applications`, events, metrics), the view cache derived from it, and the
//! transient UI state — mode, overlay, form, action flow. `run()` is the event
//! loop: it selects over terminal input, async AWS results (`AppMsg`) and
//! timers, mutates `App`, and redraws.
//!
//! Three invariants hold across every module listed below, and none of them
//! are enforced by the compiler:
//!
//! 1. **Mutating view state means rebuilding the view.** `ViewState` now
//!    enforces most of this: its derived slices are private, changing
//!    `filter` or `grouped` marks them stale, and reading a stale one is a
//!    debug assertion. The inputs it does not own — `environments`,
//!    `aliases`, `latest_stacks`, the theme palette — still need an explicit
//!    `view.invalidate()` followed by `App::rebuild_view`.
//! 2. **Async results check `generation`.** A spawned task captures the
//!    generation it launched at; if `App` has since switched region, profile
//!    or account, the handler drops the result instead of applying it to the
//!    wrong context.
//! 3. **Guarded key arms come first.** A `KeyCode::Char(c) if ctrl` arm must
//!    precede the unguarded `KeyCode::Char(c)` arm for the same character.
//!    The compiler does not warn when the unguarded one shadows it.
//!
//! Writes have a single choke point: `App::deny_write` in `safety`. Every
//! mutating path — TUI, CLI and MCP — passes through it.

// ARCHITECTURE.md rule 5, enforced by the compiler rather than by
// memory: the alternate screen swallows stdout/stderr, so a stray
// `println!` here does not appear — it corrupts the frame. Use
// `tracing::*`; output goes to ~/.cache/ebman/ebman.log.
//
// Module-scoped, so the CLI's legitimate printing is unaffected. There
// were zero violations when this went in, so it costs nothing today and
// catches the next one at compile time instead of in a review.
#![cfg_attr(
    not(test),
    deny(clippy::print_stdout, clippy::print_stderr, clippy::dbg_macro)
)]

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
// the sub-modules below) keep their `crate::app::Action` etc. paths
// working after the move into `crate::mode_action`.
pub(crate) use crate::mode_action::{
    Action, ActionFlow, ConfirmKind, ConfirmModal, DryRunInfo, ParameterisedAction, ACTIONS,
};
pub(crate) use crate::mode_detail::{
    config_editable_items, health_items, ConfigEdit, ConfigEditMode, ConfigItem, ConfigItemKind,
    DetailState, DetailTab, EventLevel, EventWindow, HealthItem, LogTail, LogTailStage,
};

// ---------------------------------------------------------------------------
// Sub-modules. `App`'s inherent impl is split across these files; each one
// contributes `impl App { ... }` for one cohesive slice of behaviour. They are
// fragments of *this* module rather than independent units, so they open with
// `use super::*` and are glob-re-exported below — every `crate::app::foo` path
// resolves exactly as it did when this was one file.
// ---------------------------------------------------------------------------

// Input and routing.
mod dispatch; // `:command` router — one-liner arms only
mod input; // crossterm events, mouse, the top-level keymap
mod mode_keys; // per-mode key handling
mod msg; // the `AppMsg` enum + async-result handlers
mod palette; // Ctrl-P palette, quick-jump, tab completion

// `:command` bodies, split by category. `dispatch::execute_command` is pure
// routing; every arm body lives in one of these.
mod cmd_action; // lifecycle: deploy / upgrade / clone / scale / ...
mod cmd_alarms; // CloudWatch alarm CRUD
mod cmd_config_template; // saved-configuration template CRUD
mod cmd_cost; // cost + promotion reports
mod cmd_inspect; // read-only overlays: secrets, diff, listeners, explain
mod cmd_misc; // custom platforms, versions, metrics
mod cmd_nav; // region / profile / sort / group / redact
mod cmd_ops; // rollback, SSM run, SSH, `$EDITOR` env edit
mod cmd_option; // option-setting setters
mod cmd_overlay; // multi-account overlays: accounts, org-health, find-env
mod cmd_settings; // per-env settings: tag, env, capacity
mod cmd_view; // saved views and filters
mod cmd_write; // bulk writes: batch-action, batch-deploy

// Interactive surfaces.
mod action_flow; // action menu -> confirm -> undo window -> dispatch
mod apps_menu; // the Applications scope's menu + info overlay
mod config_edit; // Detail's Config tab editor + template CRUD
mod detail_nav; // the per-environment Detail view
mod export; // yank to clipboard, JSON/TSV/Markdown, open in console
mod forms; // modal forms: open / edit / submit
mod mode_dlq_handlers; // dead-letter-queue browser
mod open_overlay; // read-only informational overlays
mod shell_session; // suspending the TUI for `:shell` / `$EDITOR`
mod view; // filter / sort / group / pin / cursor movement
mod view_state; // the view cache, and the invariant that keeps it fresh

// Async work. Every `spawn_*` carries the `generation` it launched at; its
// handler drops the result if `App` has since switched context.
mod spawn_batch; // fan-out writes across many environments
mod spawn_deploy; // bundle upload, deploy, and its pre-flight checks
mod spawn_detail; // per-tab Detail fetches
mod spawn_dlq; // queue peek / redrive / purge
mod spawn_refresh; // the main fetch-the-world loop and its `apply_*` half
mod spawn_rollout; // multi-region staged rollouts
mod spawn_tail; // log and event tails
mod spawn_why_red; // the why-is-this-red diagnostic fan-out

// Pure logic — no `App` receiver, no I/O, directly unit-testable.
mod config_diff; // `:diff` option-setting comparison
mod cost; // instance pricing, fleet rollups
mod deploy_math; // rolling-batch and unavailability arithmetic
mod env_edit; // the `:env` editor round-trip
mod render; // overlay body renderers (`-> String`)
mod safety; // the `deny_write` gate every mutation passes through
mod saved_views; // saved-view encode / apply
mod tail; // the shared scroll/follow/filter surface
mod text; // string, parse and format helpers
mod types; // Focus / Overlay / Mode / SortKey / Picker / ...

pub(crate) use config_diff::*;
pub(crate) use cost::*;
pub(crate) use deploy_math::*;
pub(crate) use env_edit::*;
pub(crate) use render::*;
pub(crate) use saved_views::*;
pub(crate) use text::*;
pub(crate) use types::*;
pub(crate) use view_state::ViewState;

pub(crate) use crate::mode_dlq::{DlqState, QueueView};
pub(crate) use tail::tail_window_start;
pub(crate) use tail::TailView;

/// Names of all built-in `:commands`. Used to detect collisions when loading
/// user plugins from `commands.toml` — plugins that shadow a built-in are
/// dropped with a warning rather than silently masking it.
///
/// Derived from `commands::COMMANDS` (crate-private) so adding a command only
/// requires one edit (`commands.rs`). The list is built lazily on first
/// access; the registry is a `const` slice so the work is O(N) with N≈90.
pub(crate) fn builtin_commands() -> Vec<&'static str> {
    crate::commands::all_names()
}

pub struct App {
    pub(crate) context: AwsContext,
    pub(crate) scope: Scope,
    pub(crate) applications: Vec<Application>,
    pub(crate) app_table_state: TableState,
    pub(crate) environments: Vec<Environment>,
    pub(crate) table_state: TableState,
    pub(crate) table_area: Rect,
    pub(crate) mode: Mode,
    /// Filter / sort / grouping and the cached projection of
    /// `environments` that `ui` actually draws. See `ViewState` — its
    /// derived slices are private precisely so they cannot go stale
    /// unnoticed.
    pub(crate) view: ViewState,
    pub(crate) load_state: LoadState,
    pub(crate) loading_since: Option<Instant>,
    pub(crate) refresh_interval: Duration,
    /// Once the loading indicator has been visible (i.e. `loading_since`
    /// exceeded its display-threshold), keep showing it until this instant
    /// even after the load actually finishes. Smooths over the case where
    /// an AWS round-trip is *just* slow enough to trigger the indicator
    /// and then completes ~100 ms later — without this, the status flashes
    /// yellow → green for a single frame which reads as a flicker. Cleared
    /// by the render path once `Instant::now() > t`.
    pub(crate) loading_visible_until: Option<Instant>,
    pub(crate) last_refresh: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) status_message: Option<String>,
    pub(crate) error_message: Option<String>,
    pub(crate) picker: Option<Picker>,
    pub(crate) override_profile: Option<String>,
    pub(crate) override_region: Option<String>,
    pub(crate) history: HashMap<String, VecDeque<String>>,
    pub(crate) command_input: TextInput,
    pub(crate) completion: CompletionState,
    pub(crate) quickjump_input: TextInput,
    pub(crate) extra_regions: Vec<String>,
    pub(crate) event_panel: EventPanel,
    /// Env names the user has marked for batch action via `space`. Cleared on
    /// Esc, on context switch, and after a successful batch dispatch.
    pub(crate) multi_selected: BTreeSet<String>,
    /// Apps-scope multi-selection (parallel to `multi_selected`).
    /// `space` in Apps scope toggles an app in/out. Doesn't persist
    /// across sessions — selection is operator-intent for a single
    /// task. Apps-scope batch ops (future expansion) will fan across
    /// every env in every selected app.
    pub(crate) apps_selected: BTreeSet<String>,
    /// Currently-focused panel. Drives j/k routing and footer hints.
    pub(crate) focus: Focus,
    /// Regions to fan refreshes across. Empty = single-region mode (only the
    /// AwsClient's region). Populated by `:region all`.
    pub(crate) multi_regions: Vec<String>,
    pub(crate) detail: Option<DetailState>,
    pub(crate) action_flow: Option<ActionFlow>,
    pub(crate) dlq: Option<DlqState>,
    pub(crate) theme: Arc<Theme>,
    pub(crate) help: HelpState,
    pub(crate) hover_row: Option<usize>,
    pub(crate) alerts: usize, // count of envs currently in Red, recomputed each refresh
    /// Cached DLQ depth (`Visible` messages) for each Worker-tier env,
    /// keyed by env name. Populated by a per-refresh fan-out of
    /// `describe_worker_queues`. Used by the Red-alert calc + the table
    /// render's `⚠ DLQ:N` chip on Worker rows. Missing entry = "not
    /// checked yet" (don't fire an alert on cold state).
    pub(crate) worker_dlq_depths: std::collections::HashMap<String, i64>,
    /// Envs whose last worker-queue check FAILED — their entry in
    /// `worker_dlq_depths` is the last-known depth, kept so an
    /// AccessDenied/throttle can't silently clear an alert. The UI
    /// appends a staleness marker so the operator knows the number
    /// may be old. Cleared per-env on the next successful check and
    /// wholesale on context switch.
    pub(crate) worker_dlq_stale: std::collections::HashSet<String>,
    /// Monotonic counter of context-switch spawns (`:region`,
    /// `:profile`, `:account`). Stamped into `AppMsg::Rebuild` so a
    /// slow older switch losing the race to a newer one is dropped in
    /// `apply_rebuild` instead of overwriting the operator's last
    /// choice. Distinct from `generation`, which bumps on APPLY.
    pub(crate) rebuild_epoch: u64,
    /// Bumped whenever `:region all` / `:region off` changes the set of
    /// regions the fleet listing covers. `spawn_refresh` stamps it onto
    /// the `Refresh` message; `apply_refresh` drops a listing whose
    /// stamp is stale and re-spawns, because `spawn_refresh` skips
    /// while one is already in flight and the mode change would
    /// otherwise not be picked up until the next 15s tick.
    pub(crate) fanout_epoch: u64,
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
    pub(crate) env_regions: std::collections::HashMap<String, String>,
    /// When `aws` was built. The client cache's TTL only ever reached
    /// `list_environments_in_region`; everything else in the app goes
    /// through `self.aws`, which was replaced only by an explicit
    /// context switch. So a single-region operator who pasted fresh
    /// static credentials — the case the TTL was added for — still had
    /// to restart, because static profile credentials carry no expiry
    /// and the SDK's providers never re-resolve them.
    pub(crate) aws_built_at: Instant,
    /// When the active Detail tab's last refresh was fired. Paired
    /// with `DetailState::tab_loading` to keep the auto-refresh tick
    /// from stacking fetches on a scan slower than the tick.
    pub(crate) detail_fetch_started: Option<Instant>,
    /// Set while a background home-client refresh is in flight, so the
    /// 15-second tick can't stack them.
    pub(crate) aws_refresh_in_flight: bool,
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
    pub(crate) demo_mode: bool,
    /// Per-env `(healthy, total)` instance counts, populated by
    /// `spawn_env_instance_counts` after each refresh tick. Drives the
    /// `INST` column on the main env table. Missing entry = "not
    /// checked yet"; rendered as `—`. `EnvInstanceCounts { 0, 0 }` is
    /// a real value ("env reports no instances") and renders as `0/0`.
    pub(crate) env_instance_counts:
        std::collections::HashMap<String, crate::aws::EnvInstanceCounts>,
    /// Cost Explorer integration is opt-in via `:cost on`. Toggling
    /// flips this + triggers a fetch (or a stale-cache load); the
    /// envs-table COST column renders only while this is true.
    /// Persisted to state.toml under `cost_enabled`.
    pub(crate) cost_enabled: bool,
    /// Per-env monthly USD spend, populated by `spawn_cost_fetch`
    /// after a `:cost on` opt-in. Empty when costs haven't been
    /// fetched yet or the cache file is missing. Cleared when the
    /// operator toggles `:cost off` so the column stops rendering
    /// stale numbers.
    pub(crate) costs: std::collections::HashMap<String, f64>,
    pub(crate) costs_fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    /// The last `:yank-cli` snippet, so a test can assert on what was
    /// copied without reaching into the system clipboard.
    pub(crate) last_yanked_cli: Option<String>,
    /// Whether what's in `costs` came from a walk that finished.
    ///
    /// Without this, "do we already have costs?" was the only test
    /// available, and it made a partial map permanent: the first
    /// truncated walk populated `costs`, and every later truncated walk
    /// then saw a non-empty map and kept it — so the partial data from
    /// the first failure survived the whole session while each retry
    /// paid for twenty metered Cost Explorer pages and discarded them.
    pub(crate) costs_complete: bool,
    /// `family_key → newest available version` from `ListAvailableSolutionStacks`,
    /// built by `spawn_solution_stacks`. Drives the envs-table stale-platform
    /// tint. Empty until the first fetch lands; cleared on context switch so a
    /// new account/region rebuilds it.
    pub(crate) latest_stacks: std::collections::HashMap<String, String>,
    pub(crate) frozen: bool, // when true, auto-refresh ticker is no-op
    /// `true` when ebman launched without a `state.toml` on disk —
    /// i.e. first-ever run on this machine. Renderer surfaces a
    /// one-line "press ? for help, : for commands, Ctrl-K for
    /// fuzzy search" hint at the very bottom of the screen.
    /// Cleared on the operator's first input event so it never
    /// blocks; the persisted state.toml that every refresh writes
    /// also means subsequent launches won't re-trigger it.
    pub(crate) first_run_hint: bool,
    /// The currently visible overlay popup, if any. See [`Overlay`].
    pub(crate) current_overlay: Option<Overlay>,
    pub(crate) message_log: VecDeque<(chrono::DateTime<chrono::Utc>, MsgKind, String)>,
    pub(crate) toasts: VecDeque<Toast>,
    pub(crate) palette_input: TextInput,
    pub(crate) palette_items: Vec<PaletteItem>,
    pub(crate) palette_filtered: Vec<usize>,
    pub(crate) palette_state: ListState,
    pub(crate) read_only: bool,
    pub(crate) pinned: BTreeSet<String>,
    /// Apps-scope pinned set — apps stay at the top of the Apps table
    /// regardless of sort. Persisted to state.toml's `pinned_apps`
    /// field. Parallel to `pinned` (which covers envs); the two
    /// surfaces have different cursor / sort behaviour so keeping
    /// them as separate sets is cleaner than a tagged union.
    pub(crate) pinned_apps: BTreeSet<String>,
    pub(crate) aliases: BTreeMap<String, String>,
    pub(crate) saved_views: BTreeMap<String, String>,
    /// User-defined extra metric charts for the Metrics tab. Keyed by the
    /// operator-chosen display label so re-adding the same label updates
    /// in place. Persisted in `state.toml` under `metric.LABEL`.
    pub(crate) custom_metrics: BTreeMap<String, crate::state::CustomMetricSpec>,
    pub(crate) log_reload: Option<crate::LogReloadHandle>,
    pub(crate) log_directive: String,
    pub(crate) plugins: BTreeMap<String, crate::plugins::Plugin>,
    /// Snapshot of `(status_message, error_message)` captured when the current
    /// refresh was spawned. apply_refresh clears messages only if they still
    /// match this snapshot, so user-initiated status set between kickoff and
    /// apply (e.g. pressing `s` to sort during the round-trip) is preserved.
    pub(crate) status_snapshot_at_refresh: Option<(Option<String>, Option<String>)>,
    /// `true` when `status_message` was set by a user-facing command (e.g.
    /// `:pending`, `:metric add`) rather than a background spawn helper.
    /// Refresh-time auto-clear only touches non-pinned messages — without
    /// this, every 15s tick wipes out informational results the user just
    /// invoked.
    pub(crate) status_message_pinned: bool,
    /// When set, the next ticker firing skips `spawn_refresh` until this
    /// instant has passed. Driven by exponential backoff in response to
    /// AWS throttling responses; the user can still force a refresh with
    /// `Ctrl-R` / `:refresh`.
    pub(crate) throttle_until: Option<Instant>,
    /// How many consecutive refreshes have come back throttled. Each one
    /// roughly doubles the back-off; resets to zero on the next success.
    pub(crate) consecutive_throttles: u32,
    /// Latest still-valid `expiresAt` discovered in `~/.aws/sso/cache`.
    /// Recomputed on every ticker tick — the file is cheap to read and the
    /// user may `aws sso login` from another shell while ebman is open.
    pub(crate) sso_expiry: Option<chrono::DateTime<chrono::Utc>>,
    /// Rolling list of in-flight + recently-completed action dispatches.
    /// See `PendingAction`. Surfaced as a header chip + `:pending` overlay.
    pub(crate) pending_actions: std::collections::VecDeque<PendingAction>,
    /// Action queued for dispatch but inside the [`UNDO_WINDOW`] —
    /// see [`PendingDispatch`]. `tick_pending_dispatch` (called from
    /// the main loop) fires the AWS call when the deadline passes;
    /// `U` in Normal mode cancels.
    pub(crate) pending_dispatch: Option<PendingDispatch>,
    /// Active modal-form session (`:capacity`, future `:network`, etc.).
    /// Populated by `open_form`; cleared on cancel / submit completion.
    pub(crate) form: Option<crate::form::Form>,
    /// Handle to the `:logs-tail` polling task. Stored so we can `abort()`
    /// it when the overlay closes or the user switches context. None when
    /// no tail session is active.
    pub(crate) log_tail_task: Option<tokio::task::JoinHandle<()>>,
    /// Monotonically increasing id for `:logs-tail` sessions. Lets late
    /// `AppMsg::LogTailEvents` from a previous session be dropped on arrival.
    pub(crate) log_tail_session: u64,
    /// Handle to the `:event-tail` polling task — same lifecycle as
    /// `log_tail_task` (aborted on overlay close / context switch).
    pub(crate) event_tail_task: Option<tokio::task::JoinHandle<()>>,
    /// Monotonically increasing id for `:event-tail` sessions; late
    /// `AppMsg::EventTail*` from a previous session are dropped.
    pub(crate) event_tail_session: u64,
    /// Same pattern for `:why` diagnostic overlays. Late
    /// `AppMsg::WhyRed{Events,Alarms,Instances,Deploys}` for a prior
    /// invocation get dropped when this counter has moved on.
    pub(crate) why_red_session: u64,
    /// Drillable items rendered in the active `:why` overlay, written by
    /// `draw_why_red_overlay` and read by the overlay's key handler on
    /// `Enter`. Empty whenever the overlay isn't a `WhyRed`.
    pub(crate) why_items: Vec<WhyItem>,
    /// Newer ebman release advertised by crates.io, if any. Populated by the
    /// fire-and-forget update-check task that runs once at startup.
    pub(crate) update_available: Option<crate::update_check::LatestRelease>,
    /// When `true`, `run()` exits and `main()` re-execs the binary so the
    /// user keeps their terminal session across a code change. Driven by
    /// `ControlOp::Reload` over the control socket.
    pub(crate) reload_requested: bool,
    /// When `Some`, the run loop spawns an embedded SSM shell session
    /// targeting this instance ID into `current_shell`. Keystrokes in
    /// `Mode::Shell` are forwarded to the PTY rather than dispatched as
    /// ebman key bindings.
    pub(crate) pending_shell_target: Option<String>,
    /// Set when `:env-edit` is mid-flight: the `fetch_env_vars`
    /// result arrived but the main loop hasn't yet shelled out to
    /// `$EDITOR` (which needs the `Tui` handle to leave + re-enter
    /// the alternate screen, only available in the main loop).
    /// Carries `(env_name, current_env_vars)` — the editor opens
    /// against these, diffs on save, dispatches the deltas.
    pub(crate) pending_env_edit: Option<(String, Vec<(String, String)>)>,
    /// The live embedded shell pane, if any. `None` outside Mode::Shell.
    pub(crate) current_shell: Option<Box<crate::shell::ShellSession>>,
    /// Mode to return to when the user detaches from a shell pane (F12).
    pub(crate) shell_return_mode: Mode,
    /// Snapshot of the last buffer we rendered, captured from inside the
    /// `terminal.draw` closure. ratatui swaps the front/back buffer after
    /// `draw()` returns, so a snapshot taken at SCREEN-request time via
    /// `current_buffer_mut()` would read the empty back-buffer; cloning
    /// during the render is the only reliable way to expose what's actually
    /// on screen to the control plane.
    pub(crate) last_rendered_buffer: Option<ratatui::buffer::Buffer>,
    pub(crate) notify_bell: bool,
    /// Config-derived values resolved at startup — see ResolvedConfig.
    pub(crate) cfg: ResolvedConfig,
    pub(crate) newly_red: HashSet<String>,
    /// Env names that appeared for the first time on the most recent
    /// refresh (weren't in `prev_health` last cycle). Used by the env
    /// table to render a transient `+` marker on the NAME cell so a new
    /// env doesn't scroll past unnoticed. Cleared on context switch +
    /// rotated each refresh.
    pub(crate) newly_added: HashSet<String>,
    /// Delta in counts vs. the previous refresh, e.g. {"Red" → +1, "Yellow" → -1}.
    pub(crate) health_delta: Vec<(String, i32)>,
    pub(crate) status_delta: Vec<(String, i32)>,
    prev_alerts: usize,
    prev_health: HashMap<String, String>,
    prev_status: HashMap<String, String>,
    pending_select: Option<String>,
    aws: Arc<AwsClient>,
    generation: u64,
    msg_tx: mpsc::UnboundedSender<AppMsg>,
    msg_rx: mpsc::UnboundedReceiver<AppMsg>,
    quit: bool,
}

pub(crate) enum AppMsg {
    Refresh {
        gen: u64,
        /// The `fanout_epoch` this listing was launched at.
        ///
        /// `generation` can't carry this. A `:region all` / `:region
        /// off` changes which regions the fleet listing covers, but not
        /// the account or the credentials, so bumping `generation`
        /// would also drop every in-flight per-env result that is still
        /// perfectly valid — including `ActionResult` for a dispatched
        /// write, whose `complete_pending` would then never run and
        /// leave the header's `⏳ N` chip stuck forever. This is the
        /// narrower axis: only the fleet listing is stale.
        fanout: u64,
        result: Result<Vec<Environment>, String>,
        /// Per-region failures from a multi-region fan-out that still
        /// returned rows from other regions.
        ///
        /// Without this the fan-out dropped them: it only reported an
        /// error when EVERY region failed, so one region timing out,
        /// throttling or exceeding its page budget removed all of its
        /// environments from the table with nothing on screen. That was
        /// survivable while a truncated walk still returned a short
        /// list; once `list_environments` started refusing partial
        /// results it meant a whole region could vanish silently.
        partial_errors: Vec<String>,
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
    /// A same-context rebuild of the home client, so freshly-pasted
    /// static profile credentials take effect without a restart.
    ///
    /// Deliberately NOT `Rebuild`: that variant tears down the fleet,
    /// the overlays and both tails, which is right for a context
    /// switch and absurd for a credential refresh that changes
    /// nothing the operator can see. Carries `rebuild_epoch` so a real
    /// switch spawned in the meantime always wins.
    ClientRefreshed {
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
        result: Result<crate::aws::EnvCosts, String>,
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
pub(crate) enum DlqOp {
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
            view: ViewState::new(
                persisted.filter.unwrap_or_default().into(),
                grouped,
                sort_key,
                sort_desc,
                redact,
                persisted.hidden_cols,
            ),
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
            fanout_epoch: 0,
            aws_built_at: Instant::now(),
            env_regions: std::collections::HashMap::new(),
            detail_fetch_started: None,
            aws_refresh_in_flight: false,
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
            costs_complete: true,
            last_yanked_cli: None,
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
                alarm_dimensions: config.alarm_dimensions,
                passthrough: config.passthrough,
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
                app.view.set_filter(filter);
            } else if let Some(app_name) = proj.application {
                // Treat `application` as a filter prefill when no
                // explicit `filter` was set — pre-scopes the table to
                // a single-app repo's envs without a hard pin.
                app.view.set_filter(app_name);
            }
            app.cfg.runbooks.extend(proj.runbooks);
        }
        // EB CLI application name fills in as a filter prefill when
        // `.ebman/` hasn't already set one. Same "soft scope" intent
        // as the project-config path. `.ebman/` always wins because
        // it's the more explicit, ebman-native source.
        if app.view.filter().is_empty() {
            if let Some(eb) = eb_cli {
                if let Some(app_name) = eb.application {
                    app.view.set_filter(app_name);
                }
            }
        }
        // The project / EB-CLI blocks above mutate `app.view.filter()` after the
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
    /// Mark the session read-only. Set by `main` from `--deny-write`
    /// once argv is parsed, which is after `App` is built.
    ///
    /// A setter rather than a public field because `App`'s fields are
    /// `pub(crate)`: `main.rs` is the only consumer of this crate as a
    /// library, and it needs exactly three things from `App`'s state.
    /// Narrowing that from 91 public fields to three named methods is
    /// what stops an internal rename from being a semver event.
    pub fn set_read_only(&mut self, read_only: bool) {
        self.read_only = read_only;
    }

    /// Hand over the `tracing` reload handle so `:log-level` can
    /// re-filter a running session. Owned by `main` because it comes
    /// from subscriber setup, which happens before the `App` exists.
    pub fn set_log_reload(&mut self, handle: crate::LogReloadHandle) {
        self.log_reload = Some(handle);
    }

    /// Did the session ask to be restarted? `main` re-execs on this
    /// after `run` returns, so it must outlive the `App`.
    pub fn reload_requested(&self) -> bool {
        self.reload_requested
    }

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
            view: ViewState::new(
                TextInput::new(),
                config.grouped_default.unwrap_or(false),
                SortKey::App,
                false,
                config.redact_default.unwrap_or(false),
                BTreeSet::new(),
            ),
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
            fanout_epoch: 0,
            aws_built_at: Instant::now(),
            env_regions: std::collections::HashMap::new(),
            detail_fetch_started: None,
            aws_refresh_in_flight: false,
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
            costs_complete: true,
            last_yanked_cli: None,
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
                alarm_dimensions: config.alarm_dimensions.clone(),
                passthrough: config.passthrough.clone(),
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

            // The clone exists solely so the control socket's `screen`
            // op can return the last frame — `last_rendered_buffer` has
            // exactly one reader. Without a socket attached (the common
            // case: it needs `--control-socket`) it was a full-screen
            // allocation every frame that nothing ever read.
            //
            // Read `control_rx` directly rather than capturing a flag:
            // a derived copy is one more thing that can disagree with
            // the condition it stands for, and nothing tests this path.
            let want_snapshot = control_rx.is_some();
            let mut snapshot: Option<ratatui::buffer::Buffer> = None;
            terminal.draw(|f| {
                ui::draw(f, self);
                if want_snapshot {
                    snapshot = Some(f.buffer_mut().clone());
                }
            })?;
            if want_snapshot {
                self.last_rendered_buffer = snapshot;
            }
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
                    // Age out the home client so credentials edited on
                    // disk take effect. Runs on the tick rather than
                    // gated behind the back-off below: it is one call,
                    // it is what UNBLOCKS an operator whose creds
                    // expired, and being throttled is no reason to keep
                    // using a client that can't authenticate.
                    self.spawn_home_client_refresh();
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

    /// Set a status message that survives the next refresh tick. Use this
    /// for one-shot informational results the operator just asked for
    /// (e.g. `:pending` outcome, `:metric add` ack); plain
    /// `self.status_message = Some(...)` writes are still ephemeral and
    /// get auto-cleared by `apply_refresh`.
    pub(crate) fn pin_status(&mut self, msg: impl Into<String>) {
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
    pub(crate) fn pin_error(&mut self, msg: impl Into<String>) {
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
            .map(|a| redact_for_log(a, self.view.redact))
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
        // they store final `Color` values, not palette indices. Both
        // callers rebuild immediately afterwards.
        self.view.invalidate();
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
        self.cfg.alarm_dimensions = cfg.alarm_dimensions.clone();
        // Theme swap invalidates the cached per-app colour assignments —
        // those store final `Color` values, not palette indices, so they'd
        // otherwise carry the old palette into the new theme's rendering.
        self.rebuild_view();
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
            filter: if self.view.filter().is_empty() {
                None
            } else {
                Some(self.view.filter().text().to_string())
            },
            sort: Some(format!(
                "{}:{}",
                self.view.sort_key().label(),
                if self.view.sort_desc() { "desc" } else { "asc" }
            )),
            grouped: Some(self.view.grouped()),
            redact: Some(self.view.redact),
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
            hidden_cols: self.view.hidden_cols.clone(),
            custom_metrics: self.custom_metrics.clone(),
        });
    }

    pub(crate) fn selected_env(&self) -> Option<&Environment> {
        let sel = self.table_state.selected()?;
        match self.display_rows().get(sel)? {
            DisplayRow::Env(i) => self.environments.get(*i),
            DisplayRow::Separator => None,
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
pub(crate) enum YankKind {
    Cname,
    Name,
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum DisplayRow {
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
pub(crate) fn compute_traffic_warning(env: &Environment) -> Option<String> {
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
    // Reads the classification `flatten_err_to_string` already made,
    // rather than making it again.
    //
    // This used to substring-match the flattened message for
    // "throttling" — a SECOND sniff, after the first one had already
    // decided. That meant any error whose text merely contained the word
    // armed the refresh back-off: an environment named `throttling-test`
    // failing with AccessDenied slowed the whole fleet listing over a
    // permissions problem that backing off cannot fix.
    //
    // Both callers (`spawn_refresh`'s partial-error filter and its
    // Err arm) receive strings built by `flatten_err`, so the prefix is
    // always present when it applies.
    // `contains`, not `starts_with`: the multi-region fan-out reports a
    // partial failure as "region eu-west-2: <flattened>", so the marker
    // is not always at position 0. The colon is what makes this specific
    // — it is the prefix `flatten_err_to_string` emits, not the bare
    // word that appeared in an environment's name.
    msg.contains("ThrottlingException:")
}

/// Exponential back-off horizon: 2× base on the first throttle, doubling each
/// consecutive failure, capped at 5 minutes. The 5 min cap keeps the app
/// responsive when the throttle clears — the user shouldn't have to wait
/// arbitrarily long after rate limits ease.
/// Pure: given the moment a load started and the display constants, return
/// the instant the loading indicator should remain visible until (if it
/// was visible at all). Returns `None` when the load completed before the
/// indicator's display threshold, signalling "no linger needed".
pub(crate) fn compute_loading_linger_target(
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
    // Stubbed under `cfg(test)`. Nine call sites reach here, and the
    // tests that drive them assert on what *would* be copied (via
    // `last_yanked_cli` and friends) — none of them want the real
    // clipboard. Writing to it from `cargo test` destroys whatever the
    // developer running the suite had copied, which is a side effect
    // on their machine, not on the program under test. On a headless
    // CI box it's worse: the call fails for want of a display and the
    // assertion reports a clipboard error instead of its subject.
    #[cfg(test)]
    {
        let _ = text;
        Ok(())
    }
    #[cfg(not(test))]
    {
        let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
        cb.set_text(text.to_string()).map_err(|e| e.to_string())
    }
}

/// Which EC2 surface a MultiSelect form is pulling its option list from.
/// Drives both the EC2 API call and the option-setting target so the
/// pickers share `open_multi_select_form` without conditional branches.
#[derive(Copy, Clone, Debug)]
pub(crate) enum MultiSelectFlavour {
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
        .map(|(_, _, v)| crate::util::split_csv(v))
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

/// Pair every async AWS error with a full-chain log entry. The returned string
/// is the SDK's top-level `Display` (concise, suitable for the toast/footer);
/// the chain — including the underlying `dyn Error` causes that color-eyre
/// records on `Report` — goes to `ebman.log` via `tracing::error!`. Without
/// this the chain was lost both from the UI and the log.
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
    // Prefer the code the SDK itself reported. `AwsErrorMeta` is our own
    // type, inserted at the boundary by `aws::wrap_aws`, so it can be
    // downcast back out of the chain — which a generic
    // `ProvideErrorMetadata` cannot be, hence capturing it there.
    //
    // The Debug sniff below is the fallback for the call sites not yet
    // converted. It is a fallback, not the mechanism: it lowercases the
    // `Debug` rendering of an SDK type and substring-matches it, so an
    // environment named `throttling-test` reclassifies as throttling and
    // arms the refresh back-off.
    // `downcast_ref` on the Report, not a walk over `chain()`: eyre
    // stores a context layer inside its own wrapper type, so the chain
    // yields that wrapper rather than our struct and a per-link
    // downcast finds nothing.
    if let Some(meta) = e.downcast_ref::<crate::aws::AwsErrorMeta>() {
        if let Some(code) = meta.code.as_deref() {
            let lower = code.to_ascii_lowercase();
            if lower.contains("throttl") || lower.contains("requestlimitexceeded") {
                return format!("ThrottlingException: {display}");
            }
            if lower.contains("accessdenied") || lower.contains("unauthorized") {
                return format!("AccessDenied: {display}");
            }
            if lower.contains("notfound") || lower.contains("nosuch") {
                return format!("NotFound: {display}");
            }
        }
    }
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
pub(crate) struct PendingDispatch {
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
/// `Single` re-uses `App::spawn_action`; the batch variants
/// re-use the per-env `spawn_batch_*` helpers in a loop.
// See the matching allow on `ActionFlow` — same trade-off.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
pub(crate) enum PendingDispatchKind {
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
pub(crate) const UNDO_WINDOW: Duration = Duration::from_secs(5);

/// TTL for `App.env_tag_cache` + `App.env_health_cache` — the lazy
/// caches that back `spawn_confirm_lint`'s parallel tag + health
/// fetches. 60s matches the operator's typical "open a confirm
/// modal, look at it, press Y" cadence — fresh enough that data
/// drift across rapid modal cycles is negligible; long enough that
/// repeated modal-opens against the same env benefit. 0.21
/// addition (lint input caching).
pub(crate) const LINT_INPUT_CACHE_TTL: Duration = Duration::from_secs(60);

/// Items the Apps-scope action overlay (`Overlay::AppsActionMenu`)
/// offers when the operator presses `a` from the Apps table. Each
/// dispatches via `cmd_batch_*` after seeding `multi_selected` with the
/// envs captured at menu-open time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AppsActionItem {
    Drill,
    BatchRebuild,
    BatchRestart,
    BatchDeploy,
    OpenInConsole,
}

impl AppsActionItem {
    pub(crate) fn label(self) -> &'static str {
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
pub(crate) const APPS_ACTION_ITEMS: &[AppsActionItem] = &[
    AppsActionItem::Drill,
    AppsActionItem::BatchRebuild,
    AppsActionItem::BatchRestart,
    AppsActionItem::BatchDeploy,
    AppsActionItem::OpenInConsole,
];

/// `arn:PARTITION:sts::ACCOUNT:assumed-role/ROLE/SESSION` →
/// `arn:PARTITION:iam::ACCOUNT:role/ROLE`, or `None` if it isn't that
/// shape.
///
/// Partition-generic: this matched the literal `arn:aws:sts::` and
/// rebuilt `arn:aws:iam::`, so in GovCloud, China or an ISO partition
/// the rewrite never happened and IAM received a session ARN it
/// refuses.
fn rewrite_assumed_role_arn(arn: &str) -> Option<String> {
    let partition = crate::util::arn_partition(arn)?;
    let rest = arn.strip_prefix(&format!("arn:{partition}:sts::"))?;
    let (account, role_part) = rest.split_once(':')?;
    let role_name = role_part.strip_prefix("assumed-role/")?.split('/').next()?;
    Some(format!("arn:{partition}:iam::{account}:role/{role_name}"))
}

/// A client for the region a particular row lives in, resolved lazily
/// inside the spawned task.
///
/// Detail's ten background fetches all used `self.aws`, whose region is
/// `context.region`. Under a multi-region fan-out the selected row is
/// routinely in some *other* region, so opening Detail on it showed
/// that environment's name beside the home region's instances, metrics,
/// events and alarms — wrong data wearing the right label, which is
/// worse than an error. `region_for` already tells us where the row
/// came from; this carries the answer into the task.
#[derive(Clone)]
pub(crate) struct RegionClient {
    home: Arc<AwsClient>,
    /// `Some` only when the row is somewhere other than where `home` is
    /// pointed. Keeping the home client for the common case matters:
    /// it is the only one carrying a live AssumeRole session for the
    /// home region.
    remote: Option<Remote>,
}

/// How to reach a region other than the home one — mirroring the two
/// multi-region fan-outs exactly, so Detail can't resolve a row
/// differently from the listing that produced it.
#[derive(Clone)]
pub(crate) enum Remote {
    /// `cached_client`, like `list_environments_in_region`.
    Profile(Option<String>, String),
    /// A fresh AssumeRole into the same account, pointed at the other
    /// region — like `list_environments_for_account`.
    ///
    /// Not cached, deliberately: those sessions carry a hard one-hour
    /// cap and the client cache has no notion of expiry. `assume_role`
    /// under `:account NAME` also puts the friendly ACCOUNT name in
    /// `context.profile`, so without this branch a cross-region row
    /// resolved `cached_client(Some("prod"), …)` and failed looking
    /// for an AWS profile called `prod` that was never a profile.
    Account(String, Box<crate::config::AccountSpec>),
}

impl RegionClient {
    /// The region this client will actually talk to.
    #[cfg(test)]
    pub(crate) fn region_for_tests(&self) -> String {
        match &self.remote {
            Some(Remote::Profile(_, region)) => region.clone(),
            Some(Remote::Account(_, spec)) => spec.region.clone().unwrap_or_default(),
            None => self.home.context.region.clone(),
        }
    }

    /// The configured account this will assume into, if any.
    #[cfg(test)]
    pub(crate) fn account_for_tests(&self) -> Option<String> {
        match &self.remote {
            Some(Remote::Account(name, _)) => Some(name.clone()),
            _ => None,
        }
    }

    /// Whether this stayed on the app's existing client rather than
    /// resolving a new one.
    #[cfg(test)]
    pub(crate) fn is_home_for_tests(&self) -> bool {
        self.remote.is_none()
    }

    pub(crate) async fn resolve(self) -> Result<Arc<AwsClient>, color_eyre::eyre::Report> {
        match self.remote {
            None => Ok(self.home),
            Some(Remote::Profile(profile, region)) => {
                crate::aws::cached_client(profile, region).await
            }
            Some(Remote::Account(name, spec)) => crate::aws::cached_role_client(&name, &spec).await,
        }
    }
}

impl App {
    /// The client to use for work about `region`.
    ///
    /// Resolved the same way the fan-out that produced the row resolved
    /// it — `override_profile` then `context.profile` — so Detail can't
    /// disagree with the table it was opened from.
    pub(crate) fn client_for_region(&self, region: &str) -> RegionClient {
        let home = self.aws.clone();
        // Demo mode's fan-out is fictional and its client is a stub;
        // resolving a "remote" region there would reach real AWS for a
        // region the fixture invented.
        if self.demo_mode || region == self.context.region || region.is_empty() {
            return RegionClient { home, remote: None };
        }
        // An assumed-role context re-assumes into the same account
        // pointed at the other region, exactly as `:org-health`'s
        // fan-out does. Falling through to the profile branch would
        // look for an AWS profile named after the account.
        if let Some((name, mut spec)) = self.assumed_account() {
            spec.region = Some(region.to_string());
            return RegionClient {
                home,
                remote: Some(Remote::Account(name, Box::new(spec))),
            };
        }
        let profile = self
            .override_profile
            .clone()
            .or_else(|| self.context.profile.clone());
        RegionClient {
            home,
            remote: Some(Remote::Profile(profile, region.to_string())),
        }
    }

    /// Whether the home client is stale enough to be worth rebuilding.
    ///
    /// Pure so the policy is testable without a runtime. Excluded:
    /// demo mode (the stub isn't rebuildable), a refresh already in
    /// flight, and an AssumeRole context — those sessions have a hard
    /// one-hour cap and re-assuming is a different operation with its
    /// own failure modes, not a silent swap.
    pub(crate) fn should_refresh_home_client(&self) -> bool {
        if self.demo_mode || self.aws_refresh_in_flight {
            return false;
        }
        if self.assumed_account().is_some() {
            return false;
        }
        self.aws_built_at.elapsed() >= crate::aws::CLIENT_CACHE_TTL
    }

    /// The configured account name we're assumed into, if any.
    ///
    /// `AwsClient::assume_role` puts the friendly account name in
    /// `context.profile` as the header breadcrumb, so a name that
    /// matches a configured account IS the assumed-role context. An
    /// operator with a real AWS profile of the same name gets the
    /// account spec, which is the more specific intent.
    pub(crate) fn assumed_account(&self) -> Option<(String, crate::config::AccountSpec)> {
        let name = self
            .override_profile
            .clone()
            .or_else(|| self.context.profile.clone())?;
        let spec = self.cfg.accounts.get(&name)?.clone();
        Some((name, spec))
    }

    /// The region an environment lives in, by name.
    ///
    /// Falls back to the home region for a name we don't hold a row
    /// for — a modal opened before the refresh landed, say. That is
    /// the pre-fan-out behaviour, so the fallback can't be worse than
    /// what shipped.
    /// The environment a **cached view index** points at, checked.
    ///
    /// `ViewState`'s derived rows hold indices into `environments`, and
    /// `environments` is one of the four inputs `ViewState` does not own
    /// — so a mutation that forgets `view.invalidate()` leaves indices
    /// pointing into a shorter list. `assert_fresh` is deliberately
    /// softened in release on the reasoning that "one wrong frame is
    /// better than a panic in the alt screen"; unchecked indexing here
    /// means the wrong frame IS the panic, which defeats the softening.
    ///
    /// Returns `None` instead. A missing row renders as absent for one
    /// frame and the next refresh corrects it — which is what the
    /// release-mode softening was actually asking for.
    pub(crate) fn env_at(&self, i: usize) -> Option<&crate::aws::Environment> {
        self.environments.get(i)
    }

    pub(crate) fn region_for_name(&self, env_name: &str) -> String {
        self.environments
            .iter()
            .find(|e| e.name == env_name)
            // Detail's snapshot is taken at open time and is NOT torn
            // down when a refresh drops the row — a terminated env, or
            // a region whose fetch failed under a fan-out. Without
            // this the action menu, which targets Detail's env, fell
            // back to the home region: the original wrong-region bug
            // in a narrow window, and silently.
            .or_else(|| {
                self.detail
                    .as_ref()
                    .map(|d| &d.env_snapshot)
                    .filter(|e| e.name == env_name)
            })
            .map(|e| self.region_for(e))
            // Then what we last saw. Covers the write whose row left
            // the table during its own undo window.
            .or_else(|| self.env_regions.get(env_name).cloned())
            .unwrap_or_else(|| self.context.region.clone())
    }

    /// The client for work about one named environment.
    ///
    /// The accessor per-env spawns should reach for. `self.aws` is
    /// correct only for work that is genuinely account- or
    /// region-wide — the fleet listing, identity, the applications
    /// catalogue, Cost Explorer — and a guard test in `app/tests/refresh.rs`
    /// requires every spawn site that keeps it to say why.
    pub(crate) fn client_for_env(&self, env_name: &str) -> RegionClient {
        self.client_for_region(&self.region_for_name(env_name))
    }

    /// The region an EB APPLICATION's resources live in.
    ///
    /// Applications and their versions are region-scoped in EB, so an
    /// app under a fan-out has one copy per region. Resolved through
    /// the first row of that application, which is how it got on
    /// screen; falls back to the home region when we hold none.
    pub(crate) fn region_for_app(&self, app_name: &str) -> String {
        self.environments
            .iter()
            .find(|e| e.application == app_name)
            .map(|e| self.region_for(e))
            .unwrap_or_else(|| self.context.region.clone())
    }

    /// The client for work about one EB application.
    pub(crate) fn client_for_app(&self, app_name: &str) -> RegionClient {
        self.client_for_region(&self.region_for_app(app_name))
    }

    /// The client for whatever environment the operator is pointed at
    /// — Detail's, if it's open, otherwise the selected row. For
    /// commands that name a resource belonging to an env without
    /// naming the env (`:alarm-history NAME`, say).
    pub(crate) fn current_env_client(&self) -> RegionClient {
        match self
            .detail
            .as_ref()
            .map(|d| d.env_name.clone())
            .or_else(|| self.selected_env().map(|e| e.name.clone()))
        {
            Some(name) => self.client_for_env(&name),
            None => self.client_for_region(&self.context.region),
        }
    }

    /// The client for the environment the `:why` overlay is diagnosing.
    ///
    /// Same reason as `detail_client`: `:why` is per-row triage, and
    /// answering "why is this red" with another region's events,
    /// alarms and instances is the most misleading thing it could do.
    pub(crate) fn why_red_client(&self) -> RegionClient {
        let region = match self.current_overlay.as_ref() {
            Some(Overlay::WhyRed { env_name, .. }) => self.region_for_name(env_name),
            _ => self.context.region.clone(),
        };
        self.client_for_region(&region)
    }

    /// The client for the environment whose queue the DLQ viewer has
    /// open. SQS queue URLs are region-scoped, so a peek or a purge
    /// against the home region's SQS is not merely stale — the URL
    /// doesn't exist there.
    pub(crate) fn dlq_client(&self) -> RegionClient {
        let region = match self.dlq.as_ref() {
            Some(d) => self.region_for_name(&d.env_name),
            None => self.context.region.clone(),
        };
        self.client_for_region(&region)
    }

    /// The client for whichever environment Detail currently has open.
    pub(crate) fn detail_client(&self) -> RegionClient {
        let region = self
            .detail
            .as_ref()
            .map(|d| self.region_for(&d.env_snapshot))
            .unwrap_or_else(|| self.context.region.clone());
        self.client_for_region(&region)
    }
}

/// Pure: why `arn` can't be handed to `iam:SimulatePrincipalPolicy`
/// as a policy source, or `None` when it can.
///
/// The API accepts an IAM user, group or role ARN — those are the
/// things policies attach to. An STS ARN that survived
/// [`rewrite_assumed_role_arn`] (a federated user, a service session)
/// is not one, and neither is `:root`. Sending one anyway gets an
/// `InvalidInput` back, which `:explain` then rendered under its
/// "you probably lack iam:SimulatePrincipalPolicy" hint — pointing
/// the operator at a permissions problem they don't have.
pub(crate) fn principal_not_simulatable(arn: &str) -> Option<String> {
    let partition = crate::util::arn_partition(arn)?;
    if let Some(rest) = arn.strip_prefix(&format!("arn:{partition}:iam::")) {
        let resource = rest.split_once(':').map(|(_, r)| r).unwrap_or(rest);
        if resource.starts_with("user/")
            || resource.starts_with("group/")
            || resource.starts_with("role/")
        {
            return None;
        }
        if resource == "root" {
            return Some(format!(
                "{arn} is the account root — it has no attached policies to \
                 simulate. Pass the IAM role or user that made the call."
            ));
        }
    }
    Some(format!(
        "{arn} isn't an IAM user, group or role, so SimulatePrincipalPolicy \
         can't evaluate it. Federated and service sessions aren't policy \
         attachment points — pass the underlying role ARN \
         (arn:{partition}:iam::ACCOUNT:role/NAME)."
    ))
}

/// Pure: parse an AWS `AccessDenied` error message into
/// `(principal_arn, action)`. Returns `None` when the message
/// doesn't match a recognised shape.
///
/// Recognised shapes:
///   - `User: arn:PARTITION:sts::ACCOUNT:assumed-role/ROLE/SESSION is
///     not authorized to perform: SERVICE:ACTION ...`
///   - `User: arn:PARTITION:iam::ACCOUNT:{user,role}/NAME is not
///     authorized to perform: SERVICE:ACTION ...`
///
/// Assumed-role ARNs are rewritten to the underlying role ARN by
/// [`rewrite_assumed_role_arn`], because that's what
/// `iam:SimulatePrincipalPolicy` wants as the policy source — the
/// session credentials themselves aren't a policy attachment point.
/// Any other principal is returned unchanged: a rewrite that doesn't
/// apply must not fail the parse.
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
    // Rewrite an assumed-role session ARN into the role ARN it came
    // from: session credentials aren't a policy attachment point, so
    // `iam:SimulatePrincipalPolicy` rejects them.
    //
    // A failed rewrite must leave the principal alone, not fail the
    // whole parse — an STS ARN that isn't an assumed-role (a federated
    // user, say) is still worth reporting to the operator. The `?`s
    // live in the helper so they can't propagate out of here.
    let principal =
        rewrite_assumed_role_arn(principal_raw).unwrap_or_else(|| principal_raw.to_string());
    Some((principal, action))
}

/// Console deep link for an environment, or `None` when the region's
/// partition has no console host we can name (the ISO partitions). The
/// host used to be hardcoded to the commercial one, which produced a
/// link that couldn't resolve for a GovCloud or China operator.
fn console_url(region: &str, app_name: &str, env_name: &str) -> Option<String> {
    let base = crate::util::console_base_url(region)?;
    let app = urlencode(app_name);
    let env = urlencode(env_name);
    Some(format!(
        "{base}/elasticbeanstalk/home?region={region}#/environment/dashboard?applicationName={app}&environmentName={env}"
    ))
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
pub(crate) mod tests;
