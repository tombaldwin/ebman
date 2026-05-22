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
    config_editable_items, health_items, ConfigEdit, ConfigItem, ConfigItemKind, DetailState,
    DetailTab, EventLevel, EventWindow, HealthItem, LogTail, LogTailStage,
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

/// Which on-screen panel is "focused" — i.e. which one j/k/Enter target. The
/// main table is the default; the user can `Ctrl-]` over to the events panel
/// (when visible) for cursor navigation + line yank.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Table,
    Events,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Default,
    Compact,
    Spacious,
}

impl ViewMode {
    pub fn next(self) -> Self {
        match self {
            Self::Default => Self::Compact,
            Self::Compact => Self::Spacious,
            Self::Spacious => Self::Default,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Compact => "compact",
            Self::Spacious => "spacious",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Envs,
    Apps,
}

impl Scope {
    pub fn next(self) -> Self {
        match self {
            Self::Envs => Self::Apps,
            Self::Apps => Self::Envs,
        }
    }
    pub fn prev(self) -> Self {
        // With two scopes, prev() and next() are equivalent, but expose both so
        // a third scope can be added without changing call sites.
        self.next()
    }
}

// Note on match-arm ordering: guarded arms like `KeyCode::Char('r') if Ctrl`
// must come BEFORE their unguarded counterparts (`KeyCode::Char('r') => …`),
// otherwise the unguarded arm shadows them.

pub const HISTORY_CAP: usize = 20;
const MESSAGE_LOG_CAP: usize = 50;
const TOAST_CAP: usize = 4;

/// How long a refresh has to be in flight before the `loading…` indicator
/// in the header appears. Faster round-trips complete invisibly so the user
/// doesn't see a quick blip on every cycle.
pub const LOADING_INDICATOR_THRESHOLD: Duration = Duration::from_millis(300);

/// Once the loading indicator becomes visible, keep it visible for at
/// least this long even if the load completes earlier. Smooths over the
/// case where a round-trip is *just* slow enough to cross the threshold
/// and then finishes ~100 ms later — without the linger, the indicator
/// flashes on and off in a single visible frame which reads as flicker.
pub const LOADING_INDICATOR_LINGER: Duration = Duration::from_millis(500);

/// A single read-only popup that overlays the main UI. Only one can be open
/// at once: opening another replaces it; `Esc` / `q` dismisses it. Replacing
/// the previous six `Option<String>` fields with this enum eliminates the
/// "did I forget one?" footgun every time a new overlay is added (separate
/// dismiss path, separate draw conditional, separate dismiss-on-context-switch
/// branch, …).
#[derive(Debug, Clone)]
pub enum Overlay {
    /// Raw `DescribeEnvironment` dump shown as pretty JSON via `D`.
    Describe(String),
    /// Embedded changelog shown via `:whatsnew`.
    Whatsnew(String),
    /// Recent status/error message log shown via `:history`.
    History(String),
    /// CloudWatch alarms list shown via `:alarms`. `env_name` carries the env
    /// the fetch was issued for, so a late `AppMsg::Alarms` for a different
    /// env can be dropped instead of replacing the overlay's contents.
    Alarms { env_name: String, body: String },
    /// Side-by-side env comparison shown via `:diff NAME`.
    Diff(String),
    /// Fallback for the `:saved-configs` command when no templates exist.
    /// Renders the styled `Application: foo / ▸ template` text; for the
    /// generic-text-dump cases use `TextDump` instead.
    SavedConfigs(String),
    /// Generic scrollable text overlay with a custom title. Used by
    /// `:pending`, `:resources`, `:find-env`, `:org-health`, `:versions`,
    /// etc. — anywhere we want to show a multi-line result without
    /// inventing a structured overlay.
    TextDump { title: String, body: String },
    /// Interactive variant of `:saved-configs` — cursor over (app, template)
    /// pairs, with `a` (apply to selected env), `x` (delete), `c` (prefill
    /// :config-save in the command bar). Distinct from `SavedConfigs(String)`
    /// because the latter is used as a generic text-dump escape hatch.
    /// `confirm_delete` armed when the user presses `x` — next y/Y/enter
    /// dispatches; n/N/esc cancels back to navigation.
    SavedConfigsInteractive {
        items: Vec<(String, String)>,
        cursor: usize,
        confirm_delete: bool,
    },
    /// Unified diagnostic overlay opened by `:why` — aggregates the four
    /// pieces of context an operator needs when an env goes Red: recent
    /// events, current alarm states, per-instance health, and the most-
    /// recent deploy. Each section is fetched in parallel; rendered with
    /// a "loading…" placeholder until the result lands. `session_id`
    /// drops late results for a prior `:why` invocation (e.g. when the
    /// operator opens it on env A, closes it, opens on env B before A's
    /// fetchers finished).
    WhyRed {
        env_name: String,
        /// Captured at open time so the renderer knows whether to show
        /// the worker-only sections (queues, DLQ peek).
        tier: String,
        events: Option<Result<Vec<crate::aws::Event>, String>>,
        alarms: Option<Result<Vec<crate::aws::CwAlarm>, String>>,
        instances: Option<Result<Vec<crate::aws::Instance>, String>>,
        deploys: Option<Result<Vec<crate::aws::AppVersion>, String>>,
        /// Worker-only: main + DLQ stats. `None` while loading; `Some(Err)`
        /// surfaced as a red error line. Non-Worker envs leave this as
        /// `None` forever and the renderer hides the section.
        queues: Option<Result<crate::aws::WorkerQueues, String>>,
        /// Worker-only: peek of the first few DLQ messages, fetched as a
        /// second-stage spawn once the queue stats land + DLQ is non-empty.
        /// `None` until either (a) the queue stats came back empty, or
        /// (b) the peek result lands. `Some(Ok(empty))` means "DLQ has
        /// messages but the peek returned no bodies in the visibility
        /// window we asked for".
        dlq_messages: Option<Result<Vec<crate::aws::QueueMessage>, String>>,
        session_id: u64,
    },
    /// Scrubbed bug-report payload from `:report-bug`. The operator
    /// chooses how to deliver: `y` copies to clipboard (paste into a
    /// GitHub issue manually); `b` opens a pre-filled GitHub issue
    /// in the browser; `esc` cancels. Ebman never sends the payload
    /// itself — the operator is always the agent that emits data,
    /// on their machine, after seeing the exact bytes that would
    /// leave.
    ReportBug { body: String },
    /// Per-app action menu opened by Apps-scope `a`. Lists batch
    /// operations that target every env in the application — the
    /// operator picks one via j/k + Enter and the dispatcher fans
    /// out through the existing `cmd_batch_*` helpers. Closing with
    /// esc / q returns to the Apps table without doing anything.
    AppsActionMenu {
        app_name: String,
        /// Cached at open time so the action labels can show "N envs"
        /// without re-walking `app.environments` per frame.
        env_names: Vec<String>,
        cursor: usize,
    },
    /// Streaming CloudWatch Logs view opened by `:logs-tail`. Polling task
    /// pushes new events via `AppMsg::LogTailEvents` every ~2s; the buffer
    /// is capped at `LOG_TAIL_MAX_LINES` (oldest dropped when growing).
    /// `following` snaps to the tail on new events; the user can pause it
    /// by scrolling up.
    LogTail {
        log_group: String,
        env_name: String,
        events: std::collections::VecDeque<crate::aws::LogEvent>,
        scroll: u16,
        following: bool,
        since_ms: i64,
        filter_input: String,
        filter_active: bool,
        filter_pattern: Option<regex::Regex>,
        last_err: Option<String>,
        /// Unique-per-session id; the polling task carries the same id and
        /// late events for stale sessions are dropped on arrival.
        session_id: u64,
    },
}

pub const LOG_TAIL_MAX_LINES: usize = 2000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub text: String,
    pub kind: ToastKind,
    pub shown_at: Instant,
}

impl Toast {
    pub fn ttl(&self) -> Duration {
        match self.kind {
            ToastKind::Error => Duration::from_secs(8),
            _ => Duration::from_secs(4),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgKind {
    Info,
    Error,
}

const WHATSNEW: &str = "\
ebman — what's new
==================

Recent additions:
  • --version / --help / --read-only CLI flags
  • README and GitHub Actions CI
  • Themes: dark, light, high-contrast (set in config.toml)
  • Detail auto-refresh (R in Detail mode)
  • Open env in console (b)
  • Describe overlay (D — raw env JSON)
  • Breadcrumb top-line, FROZEN pill, quick-jump 1-9
  • Pin / star envs (*), persisted across runs
  • Local env aliases (:alias NAME LABEL)
  • Exports: TSV (^Y), JSON (:json), Markdown (:report)
  • Read-only mode (--read-only or :readonly on)
  • Local audit log (~/.cache/ebman/audit.log)
  • Notification bell (notify_bell = true in config.toml)
  • Crash report writer

Press esc / q / w to close.";

const WELCOME_OVERLAY: &str = "\
Welcome to ebman
================

Looks like this is your first run — no AWS credentials or persisted ebman
state were found on this machine. Here's what you'll need:

1. AWS credentials. Either:
     aws sso login --profile my-sso-profile     (recommended)
   or set up ~/.aws/credentials with an access key, then
     export AWS_PROFILE=my-profile

2. The IAM identity needs at least these EB read permissions:
     elasticbeanstalk:DescribeEnvironments
     elasticbeanstalk:DescribeApplications
     elasticbeanstalk:DescribeEvents
   Destructive actions (rebuild / restart / swap / terminate) require their
   matching write permission; you can stay safe with `--read-only` until then.

3. Optional: drop a config at ~/.config/ebman/config.toml. See README.md for
   the full schema (theme, refresh_interval_secs, extra_regions, …).

Key bindings:
  ?         this help screen
  p / r     switch profile / region
  :         command bar
  Ctrl-K    fuzzy command palette
  Ctrl-X    redact mode (good for screenshots / streaming)

Press esc / q / w to close.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    App,
    Name,
    Status,
    Health,
    Age,
    Version,
}

impl SortKey {
    /// Cycle in the same order the columns appear in the UI:
    /// NAME → APPLICATION → STATUS → HEALTH → VERSION → AGE → NAME.
    pub fn next(self) -> Self {
        match self {
            Self::Name => Self::App,
            Self::App => Self::Status,
            Self::Status => Self::Health,
            Self::Health => Self::Version,
            Self::Version => Self::Age,
            Self::Age => Self::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::App => "app",
            Self::Name => "name",
            Self::Status => "status",
            Self::Health => "health",
            Self::Age => "age",
            Self::Version => "version",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "app" => Some(Self::App),
            "name" => Some(Self::Name),
            "status" => Some(Self::Status),
            "health" => Some(Self::Health),
            "age" => Some(Self::Age),
            "version" => Some(Self::Version),
            _ => None,
        }
    }
}

/// How event timestamps render. Three-state cycle:
/// `Utc` (default — matches EB / CloudWatch API output) →
/// `Local` (operator's wall-clock for cross-referencing with
/// other terminals / Slack threads) → `Age` (compact `5m` /
/// `2h` / `3d` relative form). Persists in state.toml as
/// `event_time_format = "utc"|"local"|"age"`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EventTimeFormat {
    #[default]
    Utc,
    Local,
    Age,
}

impl EventTimeFormat {
    /// Cycle in the order documented above. Keeping UTC first means
    /// the no-arg `:event-time` press most often lands the operator
    /// back at the canonical form (the EB API uses UTC).
    pub fn next(self) -> Self {
        match self {
            Self::Utc => Self::Local,
            Self::Local => Self::Age,
            Self::Age => Self::Utc,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Utc => "utc",
            Self::Local => "local",
            Self::Age => "age",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "utc" => Some(Self::Utc),
            "local" => Some(Self::Local),
            "age" | "relative" => Some(Self::Age),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Help,
    Picker,
    Command,
    Detail,
    Action,
    Dlq,
    QuickJump,
    Palette,
    /// Embedded shell pane is foreground; keystrokes are forwarded to the
    /// subprocess's PTY rather than dispatched as ebman key bindings.
    /// F12 detaches back to `shell_return_mode`.
    Shell,
    /// Modal multi-field form (e.g. `:capacity`). Tab navigates fields,
    /// per-field input handlers below; `Esc` cancels, `^S` submits.
    Form,
}

#[derive(Debug, Clone)]
pub enum PaletteAction {
    /// Run a `:` command immediately with no further input.
    RunCommand(String),
    /// Switch to command mode with this prefix typed.
    PrefillCommand(String),
    /// Jump table cursor to this env.
    JumpEnv(String),
    /// Run `:view NAME`.
    LoadView(String),
}

#[derive(Debug, Clone)]
pub struct PaletteItem {
    pub label: String,
    pub detail: String,
    pub kind_tag: &'static str, // "cmd" / "env" / "view" / "plugin"
    pub action: PaletteAction,
}

// `DlqState` / `QueueView` moved to `crate::mode_dlq` — re-exported
// from app.rs above.

// `ActionFlow` / `ConfirmModal` / `ParameterisedAction` / `DryRunInfo`
// / `ConfirmKind` / `Action` / `ACTIONS` moved to `crate::mode_action`
// — re-exported from app.rs below so existing imports keep working.

/// One in-flight or recently-completed action. `label` is the human-readable
/// verb (e.g. "Rebuild env"), `target` the env or instance the
/// action was dispatched against. `completed` lands when `AppMsg::ActionResult`
/// arrives; until then the entry counts as in-flight and the user can see it
/// in the `:pending` overlay + header chip.
#[derive(Debug, Clone)]
pub struct PendingAction {
    pub label: String,
    pub target: String,
    pub started: Instant,
    pub completed: Option<(Instant, Result<(), String>)>,
}

/// Help overlay scope. `Global` shows the full keymap; the per-mode topics
/// surface only the keys relevant to where the user just pressed `?`,
/// avoiding the "wall of help" problem when the user just needs a reminder
/// about the screen they're on. Set when entering `Mode::Help`.
///
/// `Shell` is currently unreachable — `?` in the embedded shell is a
/// legitimate character to forward to the subprocess (e.g. globbing) — but
/// kept here for symmetry in case we later bind a separate detach-and-help
/// combo (e.g. F11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum HelpTopic {
    Global,
    Detail,
    Dlq,
    Action,
    Shell,
    /// Help for the interactive `:saved-configs` overlay (j/k cursor +
    /// a/c/x dispatch keys).
    SavedConfigs,
}

/// Cap on the in-flight + recently-completed list. Older entries fall off
/// the front when this is reached.
pub const PENDING_CAP: usize = 20;
/// Completed entries linger for this long so the user has time to see the
/// outcome before the panel clears.
pub const PENDING_COMPLETED_TTL: Duration = Duration::from_secs(60);

// `DetailTab` / `LogTail` / `LogTailStage` / `DetailState` (+ impl)
// moved to `crate::mode_detail` — re-exported from app.rs above.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Profile,
    Region,
    /// Picker over the env's discovered CW log groups, opened from the
    /// LogTail streaming overlay so the operator can switch the tailed
    /// group without typing the full ARN.
    LogGroup,
}

pub struct Picker {
    pub kind: PickerKind,
    pub items: Vec<String>,
    pub filter: String,
    pub list_state: ListState,
}

/// Payload for `AppMsg::FormMultiSelectLoaded`. Carries the full option
/// list, parallel display annotations, and the current EB selection so
/// the form's `MultiSelect` field can be populated in one update.
#[derive(Clone, Debug)]
pub struct MultiSelectOptions {
    pub options: Vec<String>,
    pub annotations: Vec<String>,
    pub initial: Vec<String>,
}

impl Picker {
    pub fn new(kind: PickerKind, items: Vec<String>, current: Option<&str>) -> Self {
        let mut list_state = ListState::default();
        let initial = current
            .and_then(|c| items.iter().position(|i| i == c))
            .unwrap_or(0);
        if !items.is_empty() {
            list_state.select(Some(initial));
        }
        Self {
            kind,
            items,
            filter: String::new(),
            list_state,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            PickerKind::Profile => " select profile ",
            PickerKind::Region => " select region ",
            PickerKind::LogGroup => " select log group ",
        }
    }

    pub fn filtered(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.items.len()).collect();
        }
        let needle = self.filter.to_lowercase();
        self.items
            .iter()
            .enumerate()
            .filter(|(_, v)| v.to_lowercase().contains(&needle))
            .map(|(i, _)| i)
            .collect()
    }

    pub fn move_selection(&mut self, delta: i32) {
        let filt = self.filtered();
        if filt.is_empty() {
            self.list_state.select(None);
            return;
        }
        let cur_visible = self
            .list_state
            .selected()
            .and_then(|s| filt.iter().position(|i| *i == s))
            .unwrap_or(0) as i32;
        let next = (cur_visible + delta).rem_euclid(filt.len() as i32) as usize;
        self.list_state.select(Some(filt[next]));
    }

    pub fn selected_value(&self) -> Option<String> {
        self.list_state
            .selected()
            .and_then(|i| self.items.get(i).cloned())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadState {
    Idle,
    Loading,
    Error,
}

pub struct App {
    pub context: AwsContext,
    pub scope: Scope,
    pub applications: Vec<Application>,
    pub app_table_state: TableState,
    pub environments: Vec<Environment>,
    pub table_state: TableState,
    pub table_area: Rect,
    pub mode: Mode,
    pub filter: String,
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
    pub command_input: String,
    /// The text the operator had typed before they first pressed
    /// Tab to start a completion cycle. Cycling forward / backward
    /// matches against this prefix; typing a new character resets
    /// it (and the cycle). `None` when no cycle is active.
    pub command_completion_origin: Option<String>,
    /// Position within the candidate list for the active completion
    /// cycle. Only meaningful when `command_completion_origin` is
    /// `Some`. Zero before the first Tab (so the first Tab lands
    /// on the first match).
    pub command_completion_index: usize,
    pub quickjump_input: String,
    pub named_filters: BTreeMap<String, String>,
    pub extra_regions: Vec<String>,
    pub events: Vec<EbEvent>,
    pub events_visible: bool,
    /// How event timestamps render in the Events panel + Detail/Events tab.
    /// Defaults to UTC so the column matches CloudWatch / EB API output.
    /// Operator can cycle through `Utc → Local → Age` via `:event-time`
    /// or the `T` key in scopes where events are visible. Persists.
    pub event_time_format: EventTimeFormat,
    /// Env the current `events` list was fetched for. `None` = global. Used
    /// by `refresh_events_if_selection_changed` to detect when the user has
    /// moved the table cursor to a different env and refetch.
    pub events_for_env: Option<String>,
    pub events_scroll: u16,
    /// Inner Rect of the events panel — captured by the renderer so the mouse
    /// handler can detect drags on the top edge (divider row) for resize.
    pub events_area: Option<ratatui::layout::Rect>,
    /// Set when a divider drag is in progress; stores the events panel
    /// height at the moment the user pressed down so we can compute the
    /// delta against the current mouse row.
    pub events_drag_origin: Option<u16>,
    /// When set, the user has "entered" the events panel for navigation: J/K
    /// move the cursor within the events list, Y yanks the highlighted line.
    /// `None` means events keys are inert and the main table responds to J/K.
    pub events_cursor: Option<usize>,
    /// Env names the user has marked for batch action via `space`. Cleared on
    /// Esc, on context switch, and after a successful batch dispatch.
    pub multi_selected: BTreeSet<String>,
    /// Apps-scope multi-selection (parallel to `multi_selected`).
    /// `space` in Apps scope toggles an app in/out. Doesn't persist
    /// across sessions — selection is operator-intent for a single
    /// task. Apps-scope batch ops (future expansion) will fan across
    /// every env in every selected app.
    pub apps_selected: BTreeSet<String>,
    /// Render a small corner mini-map showing one coloured cell per env.
    /// Off by default — toggled via `:minimap on|off`.
    pub show_minimap: bool,
    /// Currently-focused panel. Drives j/k routing and footer hints.
    pub focus: Focus,
    /// Regions to fan refreshes across. Empty = single-region mode (only the
    /// AwsClient's region). Populated by `:region all`.
    pub multi_regions: Vec<String>,
    /// User-defined key bindings parsed from `~/.config/ebman/keys.toml`.
    pub custom_keys: crate::keys::CustomKeys,
    pub detail: Option<DetailState>,
    pub action_flow: Option<ActionFlow>,
    pub dlq: Option<DlqState>,
    pub theme: Arc<Theme>,
    pub view_mode: ViewMode,
    pub events_panel_height: u16,
    pub help_scroll: u16,
    /// Last computed max scroll for the global help overlay. Written by
    /// `draw_help` each frame, read by the j/k handler so an incremental
    /// scroll past the bottom doesn't accumulate (which would otherwise
    /// require N matching scroll-ups to bring content back into view).
    pub help_max_scroll: u16,
    pub hover_row: Option<usize>,
    pub alerts: usize, // count of envs currently in Red, recomputed each refresh
    /// Cached DLQ depth (`Visible` messages) for each Worker-tier env,
    /// keyed by env name. Populated by a per-refresh fan-out of
    /// `describe_worker_queues`. Used by the Red-alert calc + the table
    /// render's `⚠ DLQ:N` chip on Worker rows. Missing entry = "not
    /// checked yet" (don't fire an alert on cold state).
    pub worker_dlq_depths: std::collections::HashMap<String, i64>,
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
    pub palette_input: String,
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
    /// Current scope of the help overlay. Determines which keymap subset
    /// `draw_help` renders. Set whenever `?` opens Help.
    pub help_topic: HelpTopic,
    /// The mode the user was in before they opened the help overlay. Restored
    /// when help closes so pressing `?` from Detail / Action / Dlq doesn't
    /// drop the user back to Normal and lose the active screen.
    pub pre_help_mode: Option<Mode>,
    /// Overlay (if any) the user had open before pressing `?`. Help renders
    /// before overlays in the z-order so we have to stash and restore the
    /// overlay around the help round-trip.
    pub pre_help_overlay: Option<Overlay>,
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
    /// Same pattern for `:why` diagnostic overlays. Late
    /// `AppMsg::WhyRed{Events,Alarms,Instances,Deploys}` for a prior
    /// invocation get dropped when this counter has moved on.
    pub why_red_session: u64,
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
    pub required_tags: Vec<String>,
    /// Webhook URL invoked once per env that transitions into Red on refresh.
    /// `None` disables the feature.
    pub webhook_url: Option<String>,
    /// The raw `icons = …` string from `config.toml` (before resolution to
    /// [`crate::theme::IconStyle`]). Kept verbatim so `:settings` can round-trip
    /// values like `"auto"` without flattening them to the resolved style.
    pub cfg_icons_raw: String,
    /// Per-profile theme overrides loaded from `config.toml`'s
    /// `profile_themes` key. Empty when nothing is configured. Consulted
    /// by `maybe_apply_profile_theme` on initial setup + every profile
    /// switch through `apply_rebuild` so the visual cue follows the
    /// active profile without restart.
    pub profile_themes: std::collections::HashMap<String, String>,
    /// Named AssumeRole accounts loaded from `config.toml`'s
    /// `accounts.NAME.*` keys. `:account NAME` consults this map first;
    /// if the name matches, builds an `AwsClient` via STS AssumeRole.
    /// Otherwise falls back to the legacy `:profile NAME` aliasing.
    pub accounts: std::collections::HashMap<String, crate::config::AccountSpec>,
    /// Base theme name from `theme = …` — kept separate from the
    /// running `theme` so a profile-themed session reverts cleanly when
    /// the operator switches back to a profile with no override.
    pub base_theme_name: String,
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
        results: Vec<(String, i64)>,
    },
    Rebuild(Result<Box<AwsClient>, String>),
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
    Alarms {
        gen: u64,
        env_name: String,
        result: Result<Vec<CwAlarm>, String>,
    },
    DlqMessages {
        gen: u64,
        env_name: String,
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
    Resent { message_id: String },
    Purged,
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
        let persisted = state::load();
        tracing::info!(
            target: "ebman::state",
            persisted_profile = ?persisted.profile,
            persisted_region = ?persisted.region,
            "state::load"
        );
        let (aws, override_profile, override_region, identity_warning) =
            init_client(persisted.profile.clone(), persisted.region.clone()).await?;
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
            filter: persisted.filter.unwrap_or_default(),
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
            command_input: String::new(),
            command_completion_origin: None,
            command_completion_index: 0,
            quickjump_input: String::new(),
            named_filters: persisted.named_filters,
            extra_regions: config.extra_regions,
            events: Vec::new(),
            events_visible,
            event_time_format,
            events_for_env: None,
            events_scroll: 0,
            events_area: None,
            events_drag_origin: None,
            events_cursor: None,
            multi_selected: BTreeSet::new(),
            apps_selected: BTreeSet::new(),
            show_minimap: false,
            focus: Focus::Table,
            multi_regions: Vec::new(),
            custom_keys: crate::keys::load(),
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
            events_panel_height: 10,
            help_scroll: 0,
            help_max_scroll: 0,
            hover_row: None,
            alerts: 0,
            worker_dlq_depths: std::collections::HashMap::new(),
            cost_enabled: persisted.cost_enabled.unwrap_or(false),
            costs: std::collections::HashMap::new(),
            costs_fetched_at: None,
            frozen: false,
            first_run_hint: !crate::state::file_exists(),
            current_overlay: None,
            message_log: VecDeque::with_capacity(MESSAGE_LOG_CAP),
            toasts: VecDeque::with_capacity(TOAST_CAP),
            palette_input: String::new(),
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
            help_topic: HelpTopic::Global,
            pre_help_mode: None,
            pre_help_overlay: None,
            form: None,
            log_tail_task: None,
            log_tail_session: 0,
            why_red_session: 0,
            update_available: None,
            reload_requested: false,
            pending_shell_target: None,
            pending_env_edit: None,
            current_shell: None,
            shell_return_mode: Mode::Normal,
            last_rendered_buffer: None,
            notify_bell: config.notify_bell,
            required_tags: config.required_tags,
            webhook_url: config.webhook_url,
            cfg_icons_raw: config.icons.clone(),
            profile_themes: config.profile_themes.clone(),
            accounts: config.accounts.clone(),
            base_theme_name: config.theme.clone(),
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
            pending_select: persisted.selected_env,
            aws,
            generation: 0,
            msg_tx,
            msg_rx,
            quit: false,
        };
        app.rebuild_view();
        // Plugin warnings take priority over identity warnings — they're a user
        // misconfiguration the user can act on now; identity_warning is informational.
        if let Some(w) = plugin_startup_warning {
            app.error_message = Some(w);
        } else if let Some(w) = identity_warning {
            app.error_message = Some(w);
        }
        if is_first_run() {
            app.current_overlay = Some(Overlay::Whatsnew(WELCOME_OVERLAY.into()));
        }
        // Swap to the per-profile theme override if one is configured for
        // the resolved profile. Done here (after `context` is populated)
        // so the initial frame already shows the right palette.
        app.maybe_apply_profile_theme();
        Ok(app)
    }

    /// Synchronous test-only constructor. Skips `init_client` (no AWS
    /// round-trip), `state::load` (no disk read — caller passes a fresh
    /// empty state), and the spawn_identity / spawn_refresh kickoffs.
    /// The caller is responsible for providing a pre-built `AwsClient`
    /// (typically via `AwsClient::for_tests` with mocked sub-clients).
    /// `msg_tx` / `msg_rx` are created here so `handle_event` can fire
    /// spawn helpers that send AppMsg variants without panicking;
    /// callers can drain `msg_rx` to inspect dispatched messages.
    #[cfg(test)]
    pub fn for_tests(aws: crate::aws::AwsClient, config: Config) -> Self {
        let aws = Arc::new(aws);
        let context = aws.context.clone();
        let (msg_tx, msg_rx) = mpsc::unbounded_channel();
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
            filter: String::new(),
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
            command_input: String::new(),
            command_completion_origin: None,
            command_completion_index: 0,
            quickjump_input: String::new(),
            named_filters: std::collections::BTreeMap::new(),
            extra_regions: config.extra_regions.clone(),
            events: Vec::new(),
            events_visible: false,
            event_time_format: EventTimeFormat::default(),
            events_for_env: None,
            events_scroll: 0,
            events_area: None,
            events_drag_origin: None,
            events_cursor: None,
            multi_selected: BTreeSet::new(),
            apps_selected: BTreeSet::new(),
            show_minimap: false,
            focus: Focus::Table,
            multi_regions: Vec::new(),
            custom_keys: Default::default(),
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
            events_panel_height: 10,
            help_scroll: 0,
            help_max_scroll: 0,
            hover_row: None,
            alerts: 0,
            worker_dlq_depths: std::collections::HashMap::new(),
            cost_enabled: false,
            costs: std::collections::HashMap::new(),
            costs_fetched_at: None,
            frozen: false,
            first_run_hint: false,
            current_overlay: None,
            message_log: VecDeque::with_capacity(MESSAGE_LOG_CAP),
            toasts: VecDeque::with_capacity(TOAST_CAP),
            palette_input: String::new(),
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
            help_topic: HelpTopic::Global,
            pre_help_mode: None,
            pre_help_overlay: None,
            form: None,
            log_tail_task: None,
            log_tail_session: 0,
            why_red_session: 0,
            update_available: None,
            reload_requested: false,
            pending_shell_target: None,
            pending_env_edit: None,
            current_shell: None,
            shell_return_mode: Mode::Normal,
            last_rendered_buffer: None,
            notify_bell: config.notify_bell,
            required_tags: config.required_tags.clone(),
            webhook_url: config.webhook_url.clone(),
            cfg_icons_raw: config.icons.clone(),
            profile_themes: config.profile_themes.clone(),
            accounts: config.accounts.clone(),
            base_theme_name: config.theme.clone(),
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
                }
                _ = anim.tick(), if self.loading_since.is_some()
                    || !self.toasts.is_empty()
                    || self.pending_dispatch.is_some()
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
        let region = self.context.region.clone();
        let profile = self
            .override_profile
            .clone()
            .or_else(|| self.context.profile.clone());
        write_audit_line(
            self.context.account_id.as_deref(),
            profile.as_deref(),
            &region,
            &format!("stage=dispatched action=SsmSession target={instance_id}"),
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
        // F12 detaches without killing the subprocess.
        if matches!(key.code, KeyCode::F(12)) {
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
    /// and re-enters when the editor exits — same pattern as
    /// `run_inline_ssm`.
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
        std::fs::write(&path, body.as_bytes()).wrap_err("writing env-edit temp file")?;

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

    /// Legacy inline-subprocess path: drops out of the TUI, runs
    /// `aws ssm start-session` against the terminal directly, and
    /// returns when the subprocess exits. **Not the active code path** —
    /// `open_embedded_shell` is the live SSM entry point and embeds the
    /// session inside a Mode::Shell pane (preserving the table behind
    /// it). Kept as a reference for any future "drop out fully" toggle;
    /// do not call from new code without confirming the embedded path
    /// genuinely can't serve the operator's use case.
    #[allow(dead_code)]
    fn run_inline_ssm(&mut self, terminal: &mut Tui, instance_id: &str) -> Result<()> {
        use crossterm::{
            event::{DisableMouseCapture, EnableMouseCapture},
            execute,
            terminal::{
                disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
            },
        };
        // 1. Leave the TUI cleanly.
        disable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        )?;
        terminal.show_cursor()?;

        let region = self.context.region.clone();
        let profile = self
            .override_profile
            .clone()
            .or_else(|| self.context.profile.clone());
        write_audit_line(
            self.context.account_id.as_deref(),
            profile.as_deref(),
            &region,
            &format!("stage=dispatched action=SsmSession target={instance_id}"),
        );

        println!("→ aws ssm start-session --target {instance_id}");
        println!(
            "  region={region}{}",
            match &profile {
                Some(p) => format!("  profile={p}"),
                None => String::new(),
            }
        );
        println!("  ^D or `exit` to return to ebman");
        println!();

        let mut cmd = std::process::Command::new("aws");
        cmd.arg("ssm")
            .arg("start-session")
            .arg("--target")
            .arg(instance_id)
            .arg("--region")
            .arg(&region);
        if let Some(p) = &profile {
            cmd.arg("--profile").arg(p);
        }
        let status = cmd.status();

        // 3. Re-enter the TUI regardless of the subprocess outcome.
        enable_raw_mode()?;
        execute!(
            terminal.backend_mut(),
            EnterAlternateScreen,
            EnableMouseCapture
        )?;
        terminal.hide_cursor()?;
        terminal.clear()?;

        match status {
            Ok(s) if s.success() => {
                self.status_message = Some(format!("ssm session to {instance_id} ended"));
            }
            Ok(s) => {
                self.error_message = Some(format!(
                    "aws ssm start-session exited {} — check that the AWS CLI + session-manager-plugin are installed and you have ssm:StartSession",
                    s.code().unwrap_or(-1)
                ));
            }
            Err(e) => {
                self.error_message = Some(format!(
                    "could not invoke `aws`: {e} — install the AWS CLI + session-manager-plugin"
                ));
            }
        }
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
        if self.events_visible {
            if let Some(area) = self.events_area {
                let divider_row = area.y;
                let in_drag = self.events_drag_origin.is_some();
                match m.kind {
                    MouseEventKind::Down(MouseButton::Left)
                        if (m.row as i32 - divider_row as i32).abs() <= 0 =>
                    {
                        self.events_drag_origin = Some(self.events_panel_height);
                        return;
                    }
                    MouseEventKind::Drag(MouseButton::Left) if in_drag => {
                        // The mouse row is now where the divider should sit;
                        // events panel height = footer_bottom - mouse_row.
                        let footer_bottom = area.y.saturating_add(area.height).saturating_add(2);
                        let new_height = footer_bottom.saturating_sub(m.row);
                        self.events_panel_height = new_height.clamp(4, 30);
                        return;
                    }
                    MouseEventKind::Up(MouseButton::Left) if in_drag => {
                        self.events_drag_origin = None;
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
            if let Some(overlay) = self.current_overlay.as_ref() {
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
            Mode::Filter => match key.code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.mode = Mode::Normal;
                    self.rebuild_view();
                }
                KeyCode::Enter => self.mode = Mode::Normal,
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.rebuild_view();
                }
                KeyCode::Char(c) if is_text_input(&key) => {
                    self.filter.push(c);
                    self.rebuild_view();
                }
                _ => {}
            },
            Mode::Help => match key.code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                    // Restore the screen the user was on before opening
                    // help. `pre_help_mode` is set at every `?` keypress; if
                    // somehow missing, fall back to Normal so we don't get
                    // stuck in Help.
                    self.mode = self.pre_help_mode.take().unwrap_or(Mode::Normal);
                    if let Some(overlay) = self.pre_help_overlay.take() {
                        self.current_overlay = Some(overlay);
                    }
                    self.help_scroll = 0;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    // Clamp to the last-known content bound so scrolling
                    // past the end doesn't accumulate phantom offsets.
                    self.help_scroll = self.help_scroll.saturating_add(1).min(self.help_max_scroll);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                }
                _ => {}
            },
            Mode::Command => match key.code {
                KeyCode::Esc => {
                    self.command_input.clear();
                    self.command_completion_origin = None;
                    self.mode = Mode::Normal;
                }
                KeyCode::Enter => {
                    let cmd = self.command_input.clone();
                    self.command_input.clear();
                    self.command_completion_origin = None;
                    self.mode = Mode::Normal;
                    self.execute_command(&cmd);
                }
                KeyCode::Backspace => {
                    // Reset the completion cycle — the operator
                    // is editing, not cycling. Same intent as
                    // typing a new character.
                    self.command_completion_origin = None;
                    self.command_input.pop();
                }
                KeyCode::Tab => self.command_completion_step(1),
                KeyCode::BackTab => self.command_completion_step(-1),
                KeyCode::Char(c) if is_text_input(&key) => {
                    // Any printable key resets the completion
                    // cycle so the operator's next Tab starts a
                    // fresh search.
                    self.command_completion_origin = None;
                    self.command_input.push(c);
                }
                _ => {}
            },
            Mode::Shell => {
                self.handle_shell_key(key);
            }
            Mode::Palette => match key.code {
                KeyCode::Esc => {
                    self.mode = Mode::Normal;
                    self.palette_input.clear();
                }
                KeyCode::Down => self.palette_move(1),
                KeyCode::Up => self.palette_move(-1),
                KeyCode::Enter => self.palette_execute(),
                KeyCode::Backspace => {
                    self.palette_input.pop();
                    self.palette_refilter();
                }
                KeyCode::Char(c) if is_text_input(&key) => {
                    self.palette_input.push(c);
                    self.palette_refilter();
                }
                _ => {}
            },
            Mode::QuickJump => match key.code {
                KeyCode::Esc => {
                    self.quickjump_input.clear();
                    self.mode = Mode::Normal;
                }
                KeyCode::Enter => {
                    self.quickjump_input.clear();
                    self.mode = Mode::Normal;
                }
                KeyCode::Backspace => {
                    self.quickjump_input.pop();
                    self.quickjump_apply();
                }
                KeyCode::Char(c) if is_text_input(&key) => {
                    self.quickjump_input.push(c);
                    self.quickjump_apply();
                }
                _ => {}
            },
            Mode::Picker => match key.code {
                KeyCode::Esc => {
                    self.picker = None;
                    self.mode = Mode::Normal;
                }
                KeyCode::Enter => {
                    if let Some(picker) = self.picker.take() {
                        let kind = picker.kind;
                        if let Some(value) = picker.selected_value() {
                            self.apply_picker_choice(kind, value);
                        }
                    }
                    self.mode = Mode::Normal;
                }
                KeyCode::Down | KeyCode::Char('j')
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_selection(1);
                    }
                }
                KeyCode::Up | KeyCode::Char('k')
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    if let Some(p) = self.picker.as_mut() {
                        p.move_selection(-1);
                    }
                }
                KeyCode::Backspace => {
                    if let Some(p) = self.picker.as_mut() {
                        p.filter.pop();
                    }
                }
                KeyCode::Char(c) if is_text_input(&key) => {
                    if let Some(p) = self.picker.as_mut() {
                        p.filter.push(c);
                        let filt = p.filtered();
                        if !filt.iter().any(|i| Some(*i) == p.list_state.selected()) {
                            p.list_state.select(filt.first().copied());
                        }
                    }
                }
                _ => {}
            },
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
                        self.help_topic = HelpTopic::Detail;
                        self.pre_help_mode = Some(Mode::Detail);
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
                        if let Some(d) = self.detail.as_ref() {
                            if let Some(inst) = d.instances.get(d.instances_cursor) {
                                self.pending_shell_target = Some(inst.id.clone());
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
                    self.help_topic = HelpTopic::Action;
                    self.pre_help_mode = Some(Mode::Action);
                    self.mode = Mode::Help;
                } else {
                    self.handle_action_key(key);
                }
            }
            Mode::Dlq => {
                if key.code == KeyCode::Char('?') {
                    self.help_topic = HelpTopic::Dlq;
                    self.pre_help_mode = Some(Mode::Dlq);
                    self.mode = Mode::Help;
                } else {
                    self.handle_dlq_key(key);
                }
            }
            Mode::Form => self.handle_form_key(key),
            Mode::Normal => {
                // Custom keybindings — checked first so a user-bound key
                // overrides any built-in fallthrough. Only F1-F12 and
                // uppercase single-letters are accepted by the parser, which
                // limits collision risk with existing bindings.
                if let Some(command) = self.lookup_custom_key(&key) {
                    self.execute_command(&command);
                    return;
                }
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
                        self.events_visible = !self.events_visible;
                        if self.events_visible {
                            self.events_scroll = 0;
                            // events were fetched on each refresh; if we have none yet, prompt one.
                            if self.events.is_empty() {
                                self.spawn_events();
                            }
                        }
                    }
                    KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.view_mode = self.view_mode.next();
                        self.status_message = Some(format!("view: {}", self.view_mode.label()));
                    }
                    KeyCode::Up
                        if key.modifiers.contains(KeyModifiers::CONTROL) && self.events_visible =>
                    {
                        self.events_panel_height = (self.events_panel_height + 1).min(30);
                    }
                    KeyCode::Down
                        if key.modifiers.contains(KeyModifiers::CONTROL) && self.events_visible =>
                    {
                        self.events_panel_height =
                            self.events_panel_height.saturating_sub(1).max(4);
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
                                if self.events_visible {
                                    Focus::Events
                                } else {
                                    Focus::Table
                                }
                            }
                            Focus::Events => Focus::Table,
                        };
                        if matches!(self.focus, Focus::Events) && self.events_cursor.is_none() {
                            self.events_cursor = Some(0);
                        }
                        if matches!(self.focus, Focus::Table) {
                            self.events_cursor = None;
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
                                if self.events_visible {
                                    Focus::Events
                                } else {
                                    Focus::Table
                                }
                            }
                        };
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
                        if let Some(i) = self.events_cursor {
                            self.yank_event_at(i);
                        } else {
                            self.yank_selected(YankKind::Cname);
                        }
                    }
                    KeyCode::Char('Y') => self.yank_selected(YankKind::Name),
                    KeyCode::Char('J') if self.events_visible && !self.events.is_empty() => {
                        let next = self
                            .events_cursor
                            .map(|c| (c + 1).min(self.events.len().saturating_sub(1)))
                            .unwrap_or(0);
                        self.events_cursor = Some(next);
                    }
                    KeyCode::Char('K') if self.events_visible && !self.events.is_empty() => {
                        self.events_cursor = self.events_cursor.and_then(|c| c.checked_sub(1));
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
                            self.error_message = Some("no env selected".into());
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
                        self.help_topic = HelpTopic::Global;
                        self.pre_help_mode = Some(Mode::Normal);
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
                        self.filter.clear();
                        self.mode = Mode::Filter;
                    }
                    KeyCode::Char('p') => self.open_profile_picker(),
                    KeyCode::Char('r') => self.open_region_picker(),
                    KeyCode::Char('j') | KeyCode::Down => match self.focus {
                        Focus::Events if self.events_visible => {
                            let next = self
                                .events_cursor
                                .map(|c| (c + 1).min(self.events.len().saturating_sub(1)))
                                .unwrap_or(0);
                            self.events_cursor = Some(next);
                        }
                        _ => self.move_scope_selection(1),
                    },
                    KeyCode::Char('k') | KeyCode::Up => match self.focus {
                        Focus::Events if self.events_visible => {
                            self.events_cursor = self.events_cursor.and_then(|c| c.checked_sub(1));
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

    /// Resolve a Normal-mode key event against the user's `keys.toml`.
    /// Currently supports F1–F12 and single uppercase letters; returns the
    /// command body (without `:`) when bound.
    fn lookup_custom_key(&self, key: &KeyEvent) -> Option<String> {
        if !key.modifiers.is_empty() && !key.modifiers.contains(KeyModifiers::SHIFT) {
            return None;
        }
        let spec = match key.code {
            KeyCode::F(n) if (1..=12).contains(&n) => format!("F{n}"),
            KeyCode::Char(c) if c.is_ascii_uppercase() => c.to_string(),
            _ => return None,
        };
        self.custom_keys.bindings.get(&spec).cloned()
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
                    esc(&self.filter),
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

    /// Spawn a Cost Explorer fetch in the background. Result lands
    /// via `AppMsg::CostsFetched`; on success the costs map updates
    /// AND the cache file is rewritten. Idempotent — multiple
    /// fetches in flight overwrite each other harmlessly (last
    /// write wins; the tag-grouped result is stable across calls).
    fn spawn_cost_fetch(&mut self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let account = self.context.account_id.clone();
        let region = self.context.region.clone();
        tokio::spawn(async move {
            let result = aws
                .fetch_env_costs()
                .await
                .map_err(|e| flatten_err("fetch_env_costs", e));
            let _ = tx.send(AppMsg::CostsFetched {
                gen,
                account,
                region,
                result,
            });
        });
    }

    fn spawn_alarms_fetch(&mut self, env_name: String) {
        // The fetch's env name lives on the Overlay::Alarms variant so a late
        // result for a different env can be dropped at the handler. The body
        // is initially a placeholder until the result arrives.
        self.current_overlay = Some(Overlay::Alarms {
            env_name: env_name.clone(),
            body: format!("fetching alarms for {env_name}…"),
        });
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let name_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .list_alarms_for_env(&env_name)
                .await
                .map_err(|e| flatten_err("list_alarms_for_env", e));
            let _ = tx.send(AppMsg::Alarms {
                gen,
                env_name: name_for_msg,
                result,
            });
        });
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
        });
        self.spawn_why_red_events(env_name.clone(), session_id);
        self.spawn_why_red_alarms(env_name.clone(), session_id);
        self.spawn_why_red_instances(env_name.clone(), session_id);
        self.spawn_why_red_deploys(app_name.clone(), session_id);
        if is_worker {
            self.spawn_why_red_queues(app_name, env_name, session_id);
        }
    }

    fn spawn_why_red_queues(&self, app_name: String, env_name: String, session_id: u64) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .describe_worker_queues(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("describe_worker_queues", e));
            let _ = tx.send(AppMsg::WhyRedQueues {
                gen,
                session_id,
                result,
            });
        });
    }

    /// Second-stage worker-queues fetch: once the queue stats land and
    /// the DLQ has visible messages, peek a few bodies so the operator
    /// sees what's failing without leaving the overlay. Uses the same
    /// `peek_messages` (visibility_timeout=5s) as the DLQ overlay — the
    /// brief invisibility is acceptable since the DLQ isn't being
    /// consumed by anyone in normal operation.
    fn spawn_why_red_dlq_peek(&self, dlq_url: String, session_id: u64) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .peek_messages(&dlq_url, 3)
                .await
                .map_err(|e| flatten_err("peek_messages", e));
            let _ = tx.send(AppMsg::WhyRedDlqMessages {
                gen,
                session_id,
                result,
            });
        });
    }

    fn spawn_why_red_events(&self, env_name: String, session_id: u64) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .list_events_for_env(&env_name, 50)
                .await
                .map_err(|e| flatten_err("list_events_for_env", e));
            let _ = tx.send(AppMsg::WhyRedEvents {
                gen,
                session_id,
                result,
            });
        });
    }

    fn spawn_why_red_alarms(&self, env_name: String, session_id: u64) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .list_alarms_for_env(&env_name)
                .await
                .map_err(|e| flatten_err("list_alarms_for_env", e));
            let _ = tx.send(AppMsg::WhyRedAlarms {
                gen,
                session_id,
                result,
            });
        });
    }

    fn spawn_why_red_instances(&self, env_name: String, session_id: u64) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .list_instances(&env_name)
                .await
                .map_err(|e| flatten_err("list_instances", e));
            let _ = tx.send(AppMsg::WhyRedInstances {
                gen,
                session_id,
                result,
            });
        });
    }

    fn spawn_why_red_deploys(&self, app_name: String, session_id: u64) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .list_application_versions(&app_name)
                .await
                .map_err(|e| flatten_err("list_application_versions", e));
            let _ = tx.send(AppMsg::WhyRedDeploys {
                gen,
                session_id,
                result,
            });
        });
    }

    /// Detail-Health-tab alarms fetch. Mirrors `spawn_why_red_alarms`
    /// but lands on `AppMsg::DetailAlarms` so the result populates the
    /// Detail view's `cw_alarms` field instead of the `:why` overlay
    /// state. The Health tab + `:why` now share the *same* underlying
    /// AWS call shape but each lands on its own typed result so a stale
    /// fetch from a closed overlay can't clobber the Detail view.
    fn spawn_detail_alarms(&mut self, env_name: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        if let Some(d) = self.detail.as_mut() {
            d.loading_cw_alarms = true;
        }
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .list_alarms_for_env(&env_name)
                .await
                .map_err(|e| flatten_err("list_alarms_for_env", e));
            let _ = tx.send(AppMsg::DetailAlarms {
                gen,
                env_name: env_for_msg,
                result,
            });
        });
    }

    /// Detail-Health-tab recent-versions fetch. Same shape as
    /// `spawn_why_red_deploys` but lands on `AppMsg::DetailRecentVersions`.
    fn spawn_detail_recent_versions(&mut self, app_name: String, env_name: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        if let Some(d) = self.detail.as_mut() {
            d.loading_recent_versions = true;
        }
        tokio::spawn(async move {
            let result = aws
                .list_application_versions(&app_name)
                .await
                .map_err(|e| flatten_err("list_application_versions", e));
            let _ = tx.send(AppMsg::DetailRecentVersions {
                gen,
                env_name,
                result,
            });
        });
    }

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
            multi_account_enabled: !self.accounts.is_empty(),
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
    /// Args after the first whitespace pass through untouched —
    /// only the command-name fragment gets matched. Means `:set-
    /// option aws` still completes `set-option` if the operator
    /// goes back and Tabs at the start.
    fn command_completion_step(&mut self, delta: i32) {
        // First Tab: snapshot what the operator had typed so a
        // subsequent reverse-Tab (or text input) can restore.
        if self.command_completion_origin.is_none() {
            self.command_completion_origin = Some(self.command_input.clone());
            self.command_completion_index = 0;
        }
        let origin = self.command_completion_origin.clone().unwrap_or_default();
        // Split origin into (name_fragment, rest). Only the
        // pre-whitespace fragment is completed; anything after
        // (args) is preserved as-is. Take ownership of the
        // fragments so we can move `origin` if we hit the
        // empty-candidates restore path below.
        let (prefix, rest): (String, String) = match origin.find(char::is_whitespace) {
            Some(i) => (origin[..i].to_string(), origin[i..].to_string()),
            None => (origin.clone(), String::new()),
        };
        let candidates = completion_candidates(&prefix);
        if candidates.is_empty() {
            // Restore the operator's typed prefix and surface a
            // hint so the silent-no-op doesn't feel broken.
            self.command_input = origin;
            self.status_message = Some(format!(
                "no command matches '{prefix}' (Tab cycles command names)"
            ));
            return;
        }
        let n = candidates.len() as i32;
        let cur = self.command_completion_index as i32;
        let next = (cur + delta).rem_euclid(n) as usize;
        self.command_completion_index = next;
        self.command_input = format!("{}{rest}", candidates[next]);
        self.status_message = Some(format!(
            "completion {}/{} — Tab cycles, Esc cancels",
            next + 1,
            n
        ));
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
        let name_for_audit = name.clone();
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!("stage=dispatched action=GetSecretValue target={name_for_audit}"),
        );
        self.status_message = Some(format!("fetching secret '{name}'…"));
        tokio::spawn(async move {
            let result = aws
                .fetch_secret_value(&name)
                .await
                .map_err(|e| flatten_err("fetch_secret_value", e));
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

    /// `:event-time [utc|local|age]` — set how event timestamps render
    /// in the Events panel + Detail/Events tab. No argument cycles
    /// `Utc → Local → Age`. Persists to state.toml. UTC is the
    /// default because it matches the EB / CloudWatch API output the
    /// operator cross-references against.
    pub(crate) fn cmd_event_time(&mut self, rest: &[&str]) {
        let next = match rest.first().copied() {
            None => self.event_time_format.next(),
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
        self.event_time_format = next;
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
            self.error_message = Some("no env selected".into());
            return;
        };
        if self.read_only {
            self.error_message = Some("read-only mode — :env-edit disabled".into());
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
        let (principal, actions): (String, Vec<String>) = match rest.first().copied() {
            // Args form: ARN + 1..N action names.
            Some(arn) if arn.starts_with("arn:aws:") && rest.len() >= 2 => {
                let actions: Vec<String> = rest[1..].iter().map(|s| s.to_string()).collect();
                (arn.to_string(), actions)
            }
            Some(_) => {
                self.error_message = Some(
                    "usage: :explain (no args, walks last error) | :explain ARN ACTION [ACTION ...]"
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
            self.error_message = Some("no env selected".into());
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
            self.error_message = Some("no env selected".into());
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
            self.error_message = Some("no env selected".into());
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
        let body = format!(
            "ebman {version}\n\
             k9s-style TUI for AWS Elastic Beanstalk.\n\n\
             Built by Tom Baldwin · Polymorphism Ltd\n\
             https://polymorphism.co.uk\n\n\
             Source:    https://github.com/tombaldwin/ebman\n\
             License:   MIT OR Apache-2.0\n\
             Crates:    https://crates.io/crates/ebman\n\n\
             Polymorphism Ltd builds operations tools for teams running\n\
             EB / ECS / Lambda at scale. Hire us, fork the code, or just\n\
             tell us what's missing — happy either way.\n\n\
             esc / q to close.",
            version = env!("CARGO_PKG_VERSION"),
        );
        self.current_overlay = Some(Overlay::TextDump {
            title: "about ebman".to_string(),
            body,
        });
    }

    fn toggle_pin_selected(&mut self) {
        let name_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_name.clone())
        } else {
            self.selected_env().map(|e| e.name.clone())
        };
        let Some(name) = name_opt else {
            self.status_message = Some("no env selected".into());
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
            self.status_message = Some("no env selected".into());
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
            self.status_message = Some("no env selected".into());
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
            self.status_message = Some("no env selected".into());
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
        let needle = self.palette_input.to_lowercase();
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
                self.command_input = prefix;
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
        let needle = self.quickjump_input.to_lowercase();
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
            self.status_message = Some("no env selected".into());
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
            search_input: String::new(),
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

    fn spawn_detail_log_groups(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let env_name = d.env_name.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            // We don't surface fetch errors here — failure just means we
            // can't tell whether CW Logs are configured, in which case the
            // Logs tab falls back to the generic "press ^R or s" hint.
            let groups = aws
                .discover_env_log_groups(&env_name)
                .await
                .unwrap_or_default();
            let _ = tx.send(AppMsg::DetailLogGroups {
                gen,
                env_name,
                groups,
            });
        });
    }

    fn spawn_detail_env_vars(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let app_name = d.env_snapshot.application.clone();
        let env_name = d.env_name.clone();
        if let Some(d) = self.detail.as_mut() {
            d.loading_env_vars = true;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .fetch_env_vars(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("fetch_env_vars", e));
            let _ = tx.send(AppMsg::DetailEnvVars {
                gen,
                env_name,
                result,
            });
        });
    }

    fn spawn_detail_tags(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(arn) = d.env_snapshot.arn.clone() else {
            return;
        };
        let env_name = d.env_name.clone();
        if let Some(d) = self.detail.as_mut() {
            d.loading_tags = true;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .list_tags(&arn)
                .await
                .map_err(|e| flatten_err("list_tags", e));
            let _ = tx.send(AppMsg::DetailTags {
                gen,
                env_name,
                result,
            });
        });
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
                detail.log_tail.scroll = scroll_apply(detail.log_tail.scroll, delta);
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
                    match regex::RegexBuilder::new(&detail.log_tail.search_input)
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
                match regex::RegexBuilder::new(&detail.search_input)
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
            KeyCode::Backspace => {
                if on_logs {
                    detail.log_tail.search_input.pop();
                } else {
                    detail.search_input.pop();
                }
            }
            KeyCode::Char(c) if is_text_input(&key) => {
                if on_logs {
                    detail.log_tail.search_input.push(c);
                } else {
                    detail.search_input.push(c);
                }
            }
            _ => {}
        }
    }

    /// Open the in-place value editor for the Config-tab row under the
    /// cursor. No-op if the cursor isn't on an editable row (empty
    /// list). Refuses in read-only mode so the operator isn't left
    /// typing a value that can't be dispatched.
    fn start_config_edit(&mut self) {
        if self.read_only {
            self.error_message = Some("read-only mode — config editing disabled".into());
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
        // Caret starts at the end of the value so the operator can
        // append immediately, or arrow left to edit mid-string.
        let caret = item.value.chars().count();
        detail.config_edit = Some(ConfigEdit {
            kind: item.kind,
            key: item.key.clone(),
            original: item.value.clone(),
            input: item.value.clone(),
            caret,
            is_new: false,
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

    /// Commit the open Config-tab edit. For an existing-row edit the
    /// value change dispatches via the same `UpdateOptionSettings`
    /// (env var) / `UpdateTags` (tag) paths `:env set` / `:tag` use;
    /// an unchanged value is dropped without a dispatch. For an
    /// add-new-row edit the `KEY=VALUE` buffer is parsed and the new
    /// row dispatched as a set. Clears the editor either way.
    fn commit_config_edit(&mut self) {
        let Some(edit) = self.detail.as_mut().and_then(|d| d.config_edit.take()) else {
            return;
        };
        // Resolve to a concrete (kind, key, value) to dispatch.
        let (kind, key, value) = if edit.is_new {
            // New row — the buffer is `KEY=VALUE`.
            match crate::mode_detail::parse_new_config_row(&edit.input) {
                Some((k, v)) => (edit.kind, k, v),
                None => {
                    self.error_message = Some("new row needs KEY=VALUE (non-empty key)".into());
                    return;
                }
            }
        } else {
            if edit.input == edit.original {
                self.status_message = Some(format!("{} unchanged", edit.key));
                return;
            }
            (edit.kind, edit.key.clone(), edit.input.clone())
        };
        match kind {
            ConfigItemKind::EnvVar => {
                let ns = "aws:elasticbeanstalk:application:environment";
                self.spawn_option_settings_update(
                    format!("env set {key}"),
                    vec![(ns.into(), key, value)],
                    vec![],
                );
            }
            ConfigItemKind::Tag => {
                self.spawn_tag_update(vec![(key, value)], vec![]);
            }
        }
    }

    /// `n` on the Config tab — open the add-a-new-row editor. The new
    /// row's kind (tag vs env var) is taken from the section the
    /// cursor currently sits in; an empty editable list defaults to
    /// an env var (the more common edit target). The buffer is typed
    /// as `KEY=VALUE`.
    fn start_config_add(&mut self) {
        if self.read_only {
            self.error_message = Some("read-only mode — config editing disabled".into());
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
            input: String::new(),
            caret: 0,
            is_new: true,
        });
        let what = match kind {
            ConfigItemKind::EnvVar => "env var",
            ConfigItemKind::Tag => "tag",
        };
        self.status_message = Some(format!(
            "new {what} — type KEY=VALUE, enter saves, esc cancels"
        ));
    }

    /// `x` on the Config tab — arm a delete of the row under the
    /// cursor. The actual `UpdateTags` / `UpdateOptionSettings`
    /// removal waits for the `y` confirmation (see the
    /// `config_delete_confirm` interception in the key handler).
    fn arm_config_delete(&mut self) {
        if self.read_only {
            self.error_message = Some("read-only mode — config editing disabled".into());
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

    fn spawn_detail_logs(&mut self, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            // Re-entering an in-flight tail is a refresh; reset state. Existing
            // content is retained until the new fetch lands so the user keeps
            // seeing the previous tail rather than a blank screen.
            d.log_tail.stage = LogTailStage::Requesting;
            d.log_tail.poll_attempt = 0;
            d.log_tail.error = None;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = collect_tail_logs(aws, env_name.clone(), tx.clone(), gen).await;
            let _ = tx.send(AppMsg::DetailLogs {
                gen,
                env_name: env_for_msg,
                result,
            });
        });
    }

    fn spawn_detail_metrics(&mut self, env_name: String) {
        let range = self
            .detail
            .as_ref()
            .map(|d| d.metrics_range_secs)
            .unwrap_or(3600);
        if let Some(d) = self.detail.as_mut() {
            d.loading_metrics = true;
            d.error = None;
        }
        // Snapshot the custom-metrics spec list at spawn time so concurrent
        // `:metric add`s don't race with the in-flight fetch.
        let custom: Vec<crate::aws::CustomMetricQuery> = self
            .custom_metrics
            .iter()
            .map(|(label, spec)| {
                (
                    label.clone(),
                    spec.namespace.clone(),
                    spec.name.clone(),
                    spec.stat.clone(),
                    spec.dimensions.clone(),
                )
            })
            .collect();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let name = env_name.clone();
        tokio::spawn(async move {
            // Fire both queries concurrently; combine into one ordered series
            // list. Built-ins come first, then user metrics in add-order so
            // the operator sees their additions appended to the familiar
            // four.
            let (builtin, user) = tokio::join!(
                aws.fetch_env_metrics(&name, range),
                aws.fetch_custom_env_metrics(&name, range, &custom),
            );
            let result = match builtin {
                Ok(mut series) => {
                    if let Ok(extra) = user {
                        series.extend(extra);
                    }
                    Ok(series)
                }
                Err(e) => Err(flatten_err("fetch_env_metrics", e)),
            };
            let _ = tx.send(AppMsg::DetailMetrics {
                gen,
                env_name,
                result,
            });
        });
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
            purge_typed: String::new(),
            viewing,
            confirm_delete_idx: None,
        };
        self.dlq = Some(dlq);
        self.mode = Mode::Dlq;
        self.spawn_dlq_fetch();
    }

    fn open_dlq(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        if detail.tab() != DetailTab::Queue {
            return;
        }
        let Some(dlq_url) = detail.queues.dlq_url.clone() else {
            self.status_message = Some("no DLQ for this env".into());
            return;
        };
        let main_url = detail.queues.main_url.clone().unwrap_or_default();
        let dlq = DlqState {
            env_name: detail.env_name.clone(),
            main_queue_url: main_url,
            dlq_url,
            messages: Vec::new(),
            list_state: ListState::default(),
            loading: false,
            error: None,
            confirm_purge: false,
            purge_typed: String::new(),
            viewing: QueueView::Dlq,
            confirm_delete_idx: None,
        };
        self.dlq = Some(dlq);
        self.mode = Mode::Dlq;
        self.spawn_dlq_fetch();
    }

    fn close_dlq(&mut self) {
        self.dlq = None;
        self.mode = if self.detail.is_some() {
            Mode::Detail
        } else {
            Mode::Normal
        };
    }

    fn spawn_dlq_fetch(&mut self) {
        let Some(dlq) = self.dlq.as_mut() else { return };
        dlq.loading = true;
        dlq.error = None;
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_name = dlq.env_name.clone();
        let queue_url = match dlq.viewing {
            QueueView::Dlq => dlq.dlq_url.clone(),
            QueueView::Main => dlq.main_queue_url.clone(),
        };
        tokio::spawn(async move {
            let result = aws
                .peek_messages(&queue_url, 50)
                .await
                .map_err(|e| flatten_err("peek_messages", e));
            let _ = tx.send(AppMsg::DlqMessages {
                gen,
                env_name,
                result,
            });
        });
    }

    /// Delete a single message from whichever queue is currently loaded
    /// (`dlq.viewing`). The message's `receipt_handle` keeps it deletable
    /// even though our visibility timeout window is short — SQS treats the
    /// receipt handle as the canonical authorisation token for delete.
    fn spawn_dlq_delete_one(&mut self, idx: usize) {
        let Some(dlq) = self.dlq.as_mut() else { return };
        let Some(msg) = dlq.messages.get(idx).cloned() else {
            return;
        };
        let queue_url = match dlq.viewing {
            QueueView::Dlq => dlq.dlq_url.clone(),
            QueueView::Main => dlq.main_queue_url.clone(),
        };
        if queue_url.is_empty() {
            self.error_message = Some("queue URL missing — cannot delete".into());
            return;
        }
        let env_name = dlq.env_name.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!(
                "sqs-delete env={env_name} queue={} msg_id={}",
                if matches!(dlq.viewing, QueueView::Main) {
                    "MAIN"
                } else {
                    "DLQ"
                },
                msg.id
            ),
        );
        tokio::spawn(async move {
            let result = aws
                .delete_message(&queue_url, &msg.receipt_handle)
                .await
                .map(|_| DlqOp::Resent {
                    // Reuse the existing "Resent" variant — the handler
                    // already drops the message by id, which is exactly what
                    // delete should do.
                    message_id: msg.id.clone(),
                })
                .map_err(|e| flatten_err("delete_message", e));
            let _ = tx.send(AppMsg::DlqActionResult {
                gen,
                env_name,
                result,
            });
        });
    }

    fn handle_dlq_key(&mut self, key: KeyEvent) {
        let Some(dlq) = self.dlq.as_mut() else { return };
        // Single-message delete confirmation: Y/N inline. Anything else cancels.
        if let Some(idx) = dlq.confirm_delete_idx {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    dlq.confirm_delete_idx = None;
                    self.spawn_dlq_delete_one(idx);
                }
                _ => {
                    dlq.confirm_delete_idx = None;
                }
            }
            return;
        }
        // Strict-confirm mode for purge: capture text input until match.
        if dlq.confirm_purge {
            match key.code {
                KeyCode::Esc => {
                    dlq.confirm_purge = false;
                    dlq.purge_typed.clear();
                }
                KeyCode::Enter if dlq.purge_typed == dlq.env_name => {
                    let dlq_url = dlq.dlq_url.clone();
                    let env_name = dlq.env_name.clone();
                    dlq.confirm_purge = false;
                    dlq.purge_typed.clear();
                    self.spawn_dlq_purge(env_name, dlq_url);
                }
                KeyCode::Backspace => {
                    dlq.purge_typed.pop();
                }
                KeyCode::Char(c) if is_text_input(&key) => dlq.purge_typed.push(c),
                _ => {}
            }
            return;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_dlq(),
            KeyCode::Enter => {
                let Some(idx) = dlq.list_state.selected() else {
                    return;
                };
                let Some(msg) = dlq.messages.get(idx).cloned() else {
                    return;
                };
                let when = msg
                    .sent_at
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M:%S %Z")
                            .to_string()
                    })
                    .unwrap_or_else(|| "—".into());
                let view_label = match dlq.viewing {
                    QueueView::Main => "Main queue",
                    QueueView::Dlq => "DLQ",
                };
                let body = format!(
                    "{view_label} message\n\
                     ─────────────────────────────\n\
                     id:           {}\n\
                     receive-count:{}\n\
                     sent:         {when}\n\
                     bytes:        {}\n\n\
                     ─ body ─\n{}\n\nesc / q to close",
                    msg.id,
                    msg.receive_count,
                    msg.body.len(),
                    msg.body
                );
                self.current_overlay = Some(Overlay::Describe(body));
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let n = dlq.messages.len();
                if n == 0 {
                    return;
                }
                let cur = dlq.list_state.selected().unwrap_or(0);
                dlq.list_state.select(Some((cur + 1) % n));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let n = dlq.messages.len();
                if n == 0 {
                    return;
                }
                let cur = dlq.list_state.selected().unwrap_or(0);
                dlq.list_state.select(Some((cur + n - 1) % n));
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.spawn_dlq_fetch();
            }
            KeyCode::Char('r') => {
                if matches!(dlq.viewing, QueueView::Main) {
                    self.error_message = Some("resend is only available in DLQ view".into());
                } else {
                    self.spawn_dlq_resend_selected();
                }
            }
            KeyCode::Char('m') => {
                // Toggle which queue is loaded. Main-queue view disables
                // resend/purge (too dangerous on a live queue). Refetch on switch.
                if dlq.main_queue_url.is_empty() {
                    self.error_message = Some("no main queue URL known".into());
                } else {
                    dlq.viewing = match dlq.viewing {
                        QueueView::Dlq => QueueView::Main,
                        QueueView::Main => QueueView::Dlq,
                    };
                    dlq.messages.clear();
                    dlq.list_state.select(None);
                    self.spawn_dlq_fetch();
                }
            }
            KeyCode::Char('x') => {
                // Single-message delete. The dispatch loop catches y/n in the
                // next iteration via `confirm_delete_idx`.
                if let Some(idx) = dlq.list_state.selected() {
                    if dlq.messages.get(idx).is_some() {
                        dlq.confirm_delete_idx = Some(idx);
                    }
                }
            }
            KeyCode::Char('p') => {
                if let Some(dlq) = self.dlq.as_mut() {
                    dlq.confirm_purge = true;
                    dlq.purge_typed.clear();
                }
            }
            _ => {}
        }
    }

    fn spawn_dlq_resend_selected(&mut self) {
        if self.read_only {
            self.error_message = Some("read-only mode — resend disabled".into());
            return;
        }
        let Some(dlq) = self.dlq.as_mut() else { return };
        let Some(idx) = dlq.list_state.selected() else {
            return;
        };
        let Some(msg) = dlq.messages.get(idx).cloned() else {
            return;
        };
        if dlq.main_queue_url.is_empty() {
            dlq.error = Some("main queue URL unknown — cannot resend".into());
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_name = dlq.env_name.clone();
        let main_url = dlq.main_queue_url.clone();
        let dlq_url = dlq.dlq_url.clone();
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!("dlq-resend env={env_name} msg_id={}", msg.id),
        );
        tokio::spawn(async move {
            let result = match aws.send_message(&main_url, &msg.body).await {
                Ok(()) => match aws.delete_message(&dlq_url, &msg.receipt_handle).await {
                    Ok(()) => Ok(DlqOp::Resent {
                        message_id: msg.id.clone(),
                    }),
                    Err(e) => {
                        tracing::error!(target: "ebman::aws", op = "dlq_delete_after_send", error = ?e, "aws call failed");
                        Err(format!("sent to main queue, but DLQ delete failed: {e}"))
                    }
                },
                Err(e) => {
                    tracing::error!(target: "ebman::aws", op = "dlq_send", error = ?e, "aws call failed");
                    Err(format!("send to main queue failed: {e}"))
                }
            };
            let _ = tx.send(AppMsg::DlqActionResult {
                gen,
                env_name,
                result,
            });
        });
    }

    fn spawn_dlq_purge(&mut self, env_name: String, dlq_url: String) {
        if self.read_only {
            self.error_message = Some("read-only mode — purge disabled".into());
            return;
        }
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!("dlq-purge env={env_name}"),
        );
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .purge_queue(&dlq_url)
                .await
                .map(|_| DlqOp::Purged)
                .map_err(|e| flatten_err("purge_queue", e));
            let _ = tx.send(AppMsg::DlqActionResult {
                gen,
                env_name,
                result,
            });
        });
    }

    fn spawn_detail_queues(&mut self, application_name: String, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            d.loading_queues = true;
            d.error = None;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let name = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .describe_worker_queues(&application_name, &name)
                .await
                .map_err(|e| flatten_err("describe_worker_queues", e));
            let _ = tx.send(AppMsg::DetailQueues {
                gen,
                env_name,
                result,
            });
        });
    }

    fn spawn_detail_events(&mut self, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            d.loading_events = true;
            d.error = None;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let name = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .list_events_for_env(&name, 50)
                .await
                .map_err(|e| flatten_err("list_events_for_env", e));
            let _ = tx.send(AppMsg::DetailEvents {
                gen,
                env_name,
                result,
            });
        });
    }

    fn target_env_for_action(&self) -> Option<Environment> {
        // Detail view targets the env it was opened on; Normal view targets selection.
        if let Some(d) = self.detail.as_ref() {
            return Some(d.env_snapshot.clone());
        }
        self.selected_env().cloned()
    }

    fn open_action_menu(&mut self) {
        if self.read_only {
            self.error_message =
                Some("read-only mode — actions are disabled (:readonly off to enable)".into());
            return;
        }
        if self.target_env_for_action().is_none() {
            self.status_message = Some("no env selected".into());
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
        if self.read_only {
            self.error_message = Some("read-only mode — form submit disabled".into());
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
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!(
                "stage=dispatched action=UpdateOptionSettings target={env_name} summary=\"{summary}\""
            ),
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
        tokio::spawn(async move {
            let result = aws
                .update_env_option_settings(&env_for_msg, &to_set, &to_remove)
                .await
                .map_err(|e| flatten_err("update_env_option_settings", e));
            let outcome = match &result {
                Ok(()) => format!(
                    "stage=completed action=UpdateOptionSettings target={env_for_msg} summary=\"{summary_for_msg}\" ok"
                ),
                Err(e) => format!(
                    "stage=completed action=UpdateOptionSettings target={env_for_msg} summary=\"{summary_for_msg}\" err=\"{}\"",
                    e.replace('"', "'")
                ),
            };
            write_audit_line(account.as_deref(), profile.as_deref(), &region, &outcome);
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
    /// webhook_url, redact, grouped, extra_regions) are updated in place but
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
            self.error_message = Some("no env selected".into());
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

        let mut webhook_field = FormField::text(
            "webhook_url",
            "Webhook URL",
            Some::<String>("POSTed on Red transitions; blank = disabled".into()),
        );
        webhook_field.value = snapshot.webhook_url.clone().unwrap_or_default();
        fields.push(webhook_field);

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
        Config {
            refresh_interval: self.refresh_interval,
            extra_regions: self.extra_regions.clone(),
            redact_default: Some(self.redact),
            grouped_default: Some(self.grouped),
            // Snapshot the BASE theme name, not the currently-applied one;
            // otherwise a profile-overridden theme would persist as the
            // new default and erase the operator's per-profile mapping.
            theme: self.base_theme_name.clone(),
            icons: self.cfg_icons_raw.clone(),
            notify_bell: self.notify_bell,
            required_tags: self.required_tags.clone(),
            webhook_url: self.webhook_url.clone(),
            profile_themes: self.profile_themes.clone(),
            // Accounts live in config.toml only — :settings doesn't
            // surface them in the form (the assume-role schema would
            // need its own editor), so the snapshot just preserves
            // whatever was loaded.
            accounts: self.accounts.clone(),
        }
    }

    /// Per-profile theme override. Looks at the active profile (from
    /// `self.context.profile`) and the configured `profile_themes` map;
    /// swaps `self.theme` to the override if one exists, or back to the
    /// base theme otherwise. Idempotent — calling repeatedly with the
    /// same profile is a no-op.
    fn maybe_apply_profile_theme(&mut self) {
        let profile = self.context.profile.as_deref().unwrap_or("default");
        let target_name = self
            .profile_themes
            .get(profile)
            .cloned()
            .unwrap_or_else(|| self.base_theme_name.clone());
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
        self.cfg_icons_raw = icons_raw;
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
        self.required_tags = cfg.required_tags.clone();
        self.webhook_url = cfg.webhook_url.clone();
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
                KeyCode::Backspace => {
                    picker.filter.pop();
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
                        typed: String::new(),
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
                    }));
                }
                KeyCode::Char(c) if is_text_input(&key) => {
                    picker.filter.push(c);
                    let filt = picker.filtered();
                    if !filt
                        .iter()
                        .any(|i| Some(*i) == picker.list_state.selected())
                    {
                        picker.list_state.select(filt.first().copied());
                    }
                }
                _ => {}
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
                (KeyCode::Enter, ConfirmKind::TypeName) if modal.typed == modal.target_env => {
                    // Same cancel-window treatment as Y-confirms. Terminate
                    // is the loudest example — the typed-name guard already
                    // prevents accidental dispatch, but the 5s window is a
                    // last-ditch "oh god no" rescue.
                    let m = modal.clone();
                    self.close_action_flow();
                    self.queue_action_dispatch(m);
                }
                (KeyCode::Backspace, ConfirmKind::TypeName) => {
                    modal.typed.pop();
                }
                (KeyCode::Char(c), ConfirmKind::TypeName) if is_text_input(&key) => {
                    modal.typed.push(c);
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
                    typed: String::new(),
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
                    typed: String::new(),
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
                }));
            }
            _ => {
                self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                    action,
                    target_env: env.name.clone(),
                    swap_with: None,
                    typed: String::new(),
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
        // log groups. Handled up-front before the destructured borrow of
        // `current_overlay` below so the picker open can re-borrow `self`.
        if matches!(key.code, KeyCode::Tab)
            && !matches!(
                self.current_overlay.as_ref(),
                Some(Overlay::LogTail {
                    filter_active: true,
                    ..
                })
            )
        {
            self.open_log_group_picker();
            return;
        }
        // Filter input mode swallows printable keys.
        {
            let Some(Overlay::LogTail {
                filter_active,
                filter_input,
                filter_pattern,
                ..
            }) = self.current_overlay.as_mut()
            else {
                return;
            };
            if *filter_active {
                match key.code {
                    KeyCode::Esc => {
                        *filter_active = false;
                        filter_input.clear();
                        *filter_pattern = None;
                        return;
                    }
                    KeyCode::Enter => {
                        *filter_active = false;
                        if filter_input.is_empty() {
                            *filter_pattern = None;
                        } else {
                            match regex::RegexBuilder::new(filter_input)
                                .case_insensitive(true)
                                .build()
                            {
                                Ok(re) => *filter_pattern = Some(re),
                                Err(_) => *filter_pattern = None,
                            }
                        }
                        return;
                    }
                    KeyCode::Backspace => {
                        filter_input.pop();
                        return;
                    }
                    KeyCode::Char(c) if is_text_input(&key) => {
                        filter_input.push(c);
                        return;
                    }
                    _ => return,
                }
            }
        }
        let Some(Overlay::LogTail {
            scroll,
            following,
            filter_active,
            filter_input,
            filter_pattern,
            ..
        }) = self.current_overlay.as_mut()
        else {
            return;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                if let Some(handle) = self.log_tail_task.take() {
                    handle.abort();
                }
                // Bump session id so a late `LogTailOpened` from the
                // aborted task can't re-open the overlay after the user
                // dismissed it (abort + channel-send race).
                self.log_tail_session = self.log_tail_session.wrapping_add(1);
                self.current_overlay = None;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                if *scroll > 0 {
                    *scroll -= 1;
                }
                if *scroll == 0 {
                    *following = true;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                *scroll = scroll.saturating_add(1);
                *following = false;
            }
            KeyCode::Char('G') | KeyCode::End => {
                *scroll = 0;
                *following = true;
            }
            KeyCode::Char('g') | KeyCode::Home => {
                *scroll = u16::MAX;
                *following = false;
            }
            KeyCode::Char('/') => {
                *filter_active = true;
                filter_input.clear();
                *filter_pattern = None;
            }
            KeyCode::Char('n') => {
                filter_input.clear();
                *filter_pattern = None;
            }
            _ => {}
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
        if self.read_only {
            self.error_message = Some("read-only mode — config-apply disabled".into());
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        // In-flight ack lives on the pending pill; completion toasts.
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!("stage=dispatched action=ConfigApply target={env_name} template={template}"),
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
        if self.read_only {
            self.error_message = Some("read-only mode — config-delete disabled".into());
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let target = format!("{app_name}/{template}");
        self.status_message = Some(format!(
            "deleting template '{template}' from app '{app_name}'…"
        ));
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!("stage=dispatched action=ConfigDelete target={target}"),
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
        if let Some(handle) = self.log_tail_task.take() {
            handle.abort();
        }
        self.log_tail_session = self.log_tail_session.wrapping_add(1);
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
            loop {
                match aws.fetch_recent_log_events(&group, since_ms, 1000).await {
                    Ok((events, next_since)) => {
                        let next_since_ms = next_since;
                        let _ = tx.send(AppMsg::LogTailEvents {
                            gen,
                            session_id,
                            next_since_ms,
                            result: Ok(events),
                        });
                        since_ms = next_since;
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

    /// Dispatch an `UpdateEnvironment(option_settings)` call. Used by the
    /// three "tweak one or two settings" commands (`:logs-stream`, `:notify`,
    /// `:managed-window`); each pushes its own pending row + audit entry
    /// then funnels through here. `summary` is the human-readable label
    /// that ends up in the toast and the pending panel.
    fn spawn_option_settings_update(
        &mut self,
        summary: String,
        to_set: Vec<(String, String, String)>,
        to_remove: Vec<(String, String)>,
    ) {
        if self.read_only {
            self.error_message = Some(format!("read-only mode — {summary} disabled"));
            return;
        }
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        if to_set.is_empty() && to_remove.is_empty() {
            self.error_message = Some(format!(
                "{summary}: nothing to do (no options to set or remove)"
            ));
            return;
        }
        let env_name = env.name.clone();
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!(
                "stage=dispatched action=UpdateOptionSettings target={env_name} summary=\"{summary}\""
            ),
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
        tokio::spawn(async move {
            let result = aws
                .update_env_option_settings(&env_for_msg, &to_set, &to_remove)
                .await
                .map_err(|e| flatten_err("update_env_option_settings", e));
            let outcome = match &result {
                Ok(()) => format!(
                    "stage=completed action=UpdateOptionSettings target={env_for_msg} summary=\"{summary_for_msg}\" ok"
                ),
                Err(e) => format!(
                    "stage=completed action=UpdateOptionSettings target={env_for_msg} summary=\"{summary_for_msg}\" err=\"{}\"",
                    e.replace('"', "'")
                ),
            };
            write_audit_line(account.as_deref(), profile.as_deref(), &region, &outcome);
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
        if self.read_only {
            self.error_message = Some("read-only mode — deploy-from-s3 disabled".into());
            return;
        }
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
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
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!(
                "stage=dispatched action=DeployFromS3 target={env_name} label={label} source=s3://{bucket}/{key} and_deploy={and_deploy}"
            ),
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
        if self.read_only {
            self.error_message = Some("read-only mode — deploy-from-local disabled".into());
            return;
        }
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        // Path resolution: ~ expansion + check file exists + read bytes.
        let resolved = expand_tilde(&path);
        let bytes = match std::fs::read(&resolved) {
            Ok(b) => b,
            Err(e) => {
                self.error_message = Some(format!("can't read {resolved}: {e}"));
                return;
            }
        };
        if bytes.is_empty() {
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
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!(
                "stage=dispatched action=DeployFromLocal target={env_name} label={label} bytes={} and_deploy={and_deploy}",
                bytes.len()
            ),
        );
        self.push_pending(summary.clone(), env_name.clone());
        // In-flight ack lives on the pending pill; completion toasts.
        let _ = bytes.len(); // size already in pending row's summary
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
            if let Err(e) = aws.put_application_bundle(&bucket, &key, bytes).await {
                let err = format!("s3-put: {}", flatten_err("put_application_bundle", e));
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
        if self.read_only {
            self.error_message = Some("read-only mode — delete-version disabled".into());
            return;
        }
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        let application = env.application.clone();
        let force_str = if force { " (+source bundle)" } else { "" };
        let detail = format!(
            "stage=dispatched action=DeleteAppVersion target={application}/{label}{force_str}"
        );
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &detail,
        );
        // In-flight ack lives on the pending pill; completion toasts.
        let _ = force_str;
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
        tokio::spawn(async move {
            let result = aws
                .delete_application_version(&application, &label, force)
                .await
                .map_err(|e| flatten_err("delete_application_version", e));
            let outcome = match &result {
                Ok(()) => format!(
                    "stage=completed action=DeleteAppVersion target={application}/{label}{force_str} ok"
                ),
                Err(e) => format!(
                    "stage=completed action=DeleteAppVersion target={application}/{label}{force_str} err=\"{}\"",
                    e.replace('"', "'")
                ),
            };
            write_audit_line(account.as_deref(), profile.as_deref(), &region, &outcome);
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
                        self.error_message = Some("no env selected".into());
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
                self.pre_help_overlay = self.current_overlay.take();
                self.pre_help_mode = Some(self.mode);
                self.help_topic = HelpTopic::SavedConfigs;
                self.mode = Mode::Help;
            }
            _ => {}
        }
    }

    /// Dispatch an `UpdateTagsForResource` for the selected env. `to_add`
    /// and `to_remove` follow EB semantics: the API allows both in a single
    /// call; we surface a summary toast either way.
    fn spawn_tag_update(&mut self, to_add: Vec<(String, String)>, to_remove: Vec<String>) {
        if self.read_only {
            self.error_message = Some("read-only mode — tag edits disabled".into());
            return;
        }
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
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
        let detail = format!(
            "stage=dispatched action=UpdateTags target={} {}",
            env.name, summary
        );
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &detail,
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
            let outcome_detail = match &result {
                Ok(()) => {
                    format!("stage=completed action=UpdateTags target={env_name} {summary} ok")
                }
                Err(e) => format!(
                    "stage=completed action=UpdateTags target={env_name} {summary} err=\"{}\"",
                    e.replace('"', "'"),
                ),
            };
            write_audit_line(
                account.as_deref(),
                profile.as_deref(),
                &region,
                &outcome_detail,
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
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .list_events_for_env(&env_name, 3)
                .await
                .map_err(|e| flatten_err("preflight_events", e));
            let _ = tx.send(AppMsg::PreflightEvents {
                gen,
                env_name,
                result,
            });
        });
    }

    fn spawn_dry_run(&mut self, env_name: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .list_instances(&env_name)
                .await
                .map_err(|e| flatten_err("dry_run_list_instances", e));
            let _ = tx.send(AppMsg::DryRunResult {
                gen,
                env_name,
                result,
            });
        });
    }

    /// Fire a single non-destructive action for batch mode. Unlike
    /// `spawn_action` this doesn't need a `ConfirmModal` — the user already
    /// opted in by typing `:batch-…`. Only Rebuild and RestartAppServer are
    /// allowed; destructive actions still require per-env strict confirm.
    fn spawn_batch_action(&mut self, action: Action, env: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        write_audit_entry(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            action,
            &env,
            None,
        );
        self.push_pending(action.label(), env.clone());
        let env_for_msg = env.clone();
        tokio::spawn(async move {
            let result = match action {
                Action::Rebuild => aws.rebuild_env(&env).await,
                Action::RestartAppServer => aws.restart_app_server(&env).await,
                _ => Err(color_eyre::eyre::eyre!(
                    "batch-mode only supports Rebuild / Restart"
                )),
            }
            .map_err(|e| flatten_err("batch_action", e));
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action,
                env_name: env_for_msg,
                result,
            });
        });
    }

    /// Per-env deploy dispatch for `:batch-deploy`. Shares the pending
    /// pill + audit + `ActionResult` plumbing with the existing single-env
    /// `:deploy` path via `Action::Deploy`.
    fn spawn_batch_deploy(&mut self, env: String, label: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!("stage=dispatched action=Deploy target={env} version={label}"),
        );
        self.push_pending(Action::Deploy.label(), env.clone());
        let env_for_msg = env.clone();
        tokio::spawn(async move {
            let result = aws
                .deploy_version(&env, &label)
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

    /// Per-env tag / untag dispatch for `:batch-tag` / `:batch-untag`.
    /// `value = Some(v)` adds the tag; `value = None` removes the key.
    /// Audit + pending entries label the op so a query against the audit
    /// log can distinguish tag from untag.
    fn spawn_batch_tag(&mut self, env: String, arn: String, key: String, value: Option<String>) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let is_add = value.is_some();
        let op_label = if is_add { "tag" } else { "untag" };
        let detail = match &value {
            Some(v) => {
                format!("stage=dispatched action=Tag target={env} key={key} value={v}")
            }
            None => format!("stage=dispatched action=Untag target={env} key={key}"),
        };
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &detail,
        );
        let pending_label = format!("{op_label} {key}");
        self.push_pending(pending_label.clone(), env.clone());
        let env_for_msg = env.clone();
        tokio::spawn(async move {
            let to_add: Vec<(String, String)> = match &value {
                Some(v) => vec![(key.clone(), v.clone())],
                None => Vec::new(),
            };
            let to_remove: Vec<String> = if value.is_none() {
                vec![key.clone()]
            } else {
                Vec::new()
            };
            let result = aws
                .update_tags(&arn, &to_add, &to_remove)
                .await
                .map_err(|e| flatten_err("update_tags", e));
            let _ = tx.send(AppMsg::TagUpdate {
                gen,
                env_name: env_for_msg,
                summary: pending_label,
                result,
            });
        });
    }

    /// Per-env option-settings dispatch for `:batch-set-option`. Each env
    /// is its own `UpdateEnvironment(option_settings)` call; the existing
    /// `spawn_option_settings_update` is selected-env-only so this is a
    /// parameterised parallel.
    fn spawn_batch_set_option(
        &mut self,
        env: String,
        namespace: String,
        name: String,
        value: String,
    ) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let detail = format!(
            "stage=dispatched action=SetOption target={env} ns={namespace} name={name} value={value}"
        );
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &detail,
        );
        let pending_label = format!("set-option {namespace}.{name}");
        self.push_pending(pending_label.clone(), env.clone());
        let env_for_msg = env.clone();
        tokio::spawn(async move {
            let settings = vec![(namespace, name, value)];
            let result = aws
                .update_env_option_settings(&env, &settings, &[])
                .await
                .map_err(|e| flatten_err("update_env_option_settings", e));
            let _ = tx.send(AppMsg::OptionSettingsUpdate {
                gen,
                env_name: env_for_msg,
                summary: pending_label,
                result,
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
        if self.read_only {
            self.error_message = Some("read-only mode — terminate disabled".into());
            return;
        }
        let env_name = d.env_name.clone();
        let id = inst.id.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!("stage=dispatched action=TerminateInstance target={env_name} instance={id}"),
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
        if self.read_only {
            self.error_message =
                Some("read-only mode — actions disabled (:readonly off to enable)".into());
            return;
        }
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        // The preflight (impact preview + last-3 events) is gated by
        // `Action::wants_preflight()` — single source of truth, see
        // `mode_action.rs`. Every ConfirmModal construction site must
        // route through here so the rule can't drift.
        let wants_preflight = action.wants_preflight();
        let modal = ConfirmModal {
            action,
            target_env: env.name.clone(),
            swap_with: params.swap_with,
            typed: String::new(),
            kind: ConfirmKind::YesNo,
            dryrun: None,
            loading_dryrun: wants_preflight,
            recent_events: None,
            loading_events: wants_preflight,
            traffic_warning: compute_traffic_warning(&env),
            deploy_version: params.deploy_version,
            upgrade_platform_arn: params.upgrade_platform_arn,
            upgrade_platform_label: params.upgrade_platform_label,
            clone_target: params.clone_target,
            scale_min: params.scale_min,
            scale_max: params.scale_max,
        };
        self.action_flow = Some(ActionFlow::Confirm(modal));
        self.mode = Mode::Action;
        if wants_preflight {
            self.spawn_dry_run(env.name.clone());
            self.spawn_preflight_events(env.name.clone());
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
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!(
                "stage=undone action={action_for_audit} target={}",
                pd.target
            ),
        );
        self.status_message = Some(msg);
    }

    fn spawn_action(&mut self, modal: ConfirmModal) {
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
                Action::Capacity
                | Action::ConfigSave
                | Action::ConfigDelete
                | Action::ConfigApply
                | Action::TerminateInstance => Err(color_eyre::eyre::eyre!(
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

    fn spawn_detail_instances(&mut self, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            d.loading_instances = true;
            d.error = None;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let name = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .list_instances(&name)
                .await
                .map_err(|e| flatten_err("list_instances", e));
            let _ = tx.send(AppMsg::DetailInstances {
                gen,
                env_name,
                result,
            });
        });
    }

    fn execute_command(&mut self, raw: &str) {
        let line = raw.trim();
        if line.is_empty() {
            return;
        }
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
                self.help_topic = if self.detail.is_some() {
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
                self.pre_help_mode = Some(self.mode);
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
            "account" => self.cmd_account(&rest),
            "profile" | "p" => self.cmd_profile(&rest),
            "sort" => self.cmd_sort(&rest),
            "group" => self.cmd_group(&rest),
            "redact" => self.cmd_redact(&rest),
            "events" => {
                self.events_visible = parse_toggle(rest.first().copied(), self.events_visible);
                if self.events_visible && self.events.is_empty() {
                    self.spawn_events();
                }
                self.status_message = Some(if self.events_visible {
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
            "listeners" => self.cmd_listeners(),
            "rds" => self.cmd_rds(),
            "options" => self.cmd_options(&rest),
            "explain" => self.cmd_explain(&rest),
            "env-edit" => self.cmd_env_edit(),
            "secrets" => self.cmd_secrets(&rest),
            "secret" => self.cmd_secret_view(&rest),
            "report-bug" => self.open_report_bug_overlay(),
            "settings" => {
                self.open_settings_form();
            }
            "capacity" => self.cmd_capacity(),
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
            "diff" => match rest.first() {
                None => self.error_message = Some("usage: :diff <env-name>".into()),
                Some(target) => {
                    let left_opt = if let Some(d) = self.detail.as_ref() {
                        Some(d.env_snapshot.clone())
                    } else {
                        self.selected_env().cloned()
                    };
                    let Some(left) = left_opt else {
                        self.error_message = Some("no env selected".into());
                        return;
                    };
                    let right = self
                        .environments
                        .iter()
                        .find(|e| e.name == *target)
                        .cloned();
                    match right {
                        None => {
                            self.error_message =
                                Some(format!("no env named '{target}' in current view"));
                        }
                        Some(right) => {
                            self.current_overlay =
                                Some(Overlay::Diff(diff_envs(&left, &right, self.redact)));
                        }
                    }
                }
            },
            "alarms" => {
                let env_opt = if let Some(d) = self.detail.as_ref() {
                    Some(d.env_name.clone())
                } else {
                    self.selected_env().map(|e| e.name.clone())
                };
                match env_opt {
                    Some(env_name) => self.spawn_alarms_fetch(env_name),
                    None => self.error_message = Some("no env selected".into()),
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
                    None => self.error_message = Some("no env selected".into()),
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
            "delete-version" => self.cmd_delete_version(&rest),
            "upgrade" => self.cmd_upgrade(&rest),
            "clone" => self.cmd_clone(&rest),
            "scale" => self.cmd_scale(&rest),
            "stop" => self.cmd_stop(),
            "start" => self.cmd_start(),
            "abort" => self.cmd_abort(),
            "pending" | "in-flight" | "inflight" => self.cmd_pending(),
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
                    self.error_message = Some("no env selected".into());
                    return;
                };
                let explicit_group = rest.first().map(|s| s.to_string());
                self.spawn_logs_tail(env.name.clone(), explicit_group);
            }
            "logs-stream" => self.cmd_logs_stream(&rest),
            "notify" => self.cmd_notify(&rest),
            "managed-window" => self.cmd_managed_window(&rest),
            "alarm-create" => self.cmd_alarm_create(&rest),
            "alarm-delete" => self.cmd_alarm_delete(&rest),
            "config-inspect" => self.cmd_config_inspect(&rest),
            "minimap" => {
                self.show_minimap = parse_toggle(rest.first().copied(), self.show_minimap);
                self.status_message = Some(if self.show_minimap {
                    "minimap ON".into()
                } else {
                    "minimap off".into()
                });
            }
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
                Some(self.filter.clone())
            },
            sort: Some(format!(
                "{}:{}",
                self.sort_key.label(),
                if self.sort_desc { "desc" } else { "asc" }
            )),
            grouped: Some(self.grouped),
            redact: Some(self.redact),
            events_visible: Some(self.events_visible),
            event_time_format: Some(self.event_time_format),
            selected_env: selected,
            named_filters: self.named_filters.clone(),
            pinned: self.pinned.clone(),
            pinned_apps: self.pinned_apps.clone(),
            cost_enabled: Some(self.cost_enabled),
            aliases: self.aliases.clone(),
            saved_views: self.saved_views.clone(),
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
        }
    }

    fn spawn_rebuild(&mut self) {
        self.load_state = LoadState::Loading;
        self.loading_since = Some(Instant::now());
        let profile = self.override_profile.clone();
        let region = self.override_region.clone();
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = match AwsClient::with(profile, region).await {
                Ok(c) => Ok(Box::new(c)),
                Err(e) => Err(flatten_err("aws_client_with", e)),
            };
            let _ = tx.send(AppMsg::Rebuild(result));
        });
    }

    /// Background task variant of `spawn_rebuild` for the AssumeRole
    /// path. Calls `AwsClient::assume_role` with the operator's named
    /// account spec; same `AppMsg::Rebuild` arrival point so the rest
    /// of the swap (overlay tear-down, throttle reset, identity refresh)
    /// flows through the existing `apply_rebuild` handler.
    fn spawn_assume_role_switch(&mut self, account_name: String) {
        let Some(spec) = self.accounts.get(&account_name).cloned() else {
            self.error_message = Some(format!(
                "no `accounts.{account_name}` in config.toml — add `accounts.{account_name}.role_arn = …`"
            ));
            return;
        };
        self.load_state = LoadState::Loading;
        self.loading_since = Some(Instant::now());
        self.status_message = Some(format!("assuming role for account '{account_name}'…"));
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = match AwsClient::assume_role(&account_name, &spec).await {
                Ok(c) => Ok(Box::new(c)),
                Err(e) => Err(flatten_err("aws_client_assume_role", e)),
            };
            let _ = tx.send(AppMsg::Rebuild(result));
        });
    }

    fn spawn_identity(&mut self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .verify_identity()
                .await
                .map_err(|e| flatten_err("verify_identity", e));
            let _ = tx.send(AppMsg::Identity { gen, result });
        });
    }

    fn spawn_update_check(&mut self) {
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
        if self.events_visible {
            self.spawn_events();
        }
        self.spawn_applications();
    }

    fn spawn_applications(&self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .list_applications()
                .await
                .map_err(|e| flatten_err("list_applications", e));
            let _ = tx.send(AppMsg::Applications { gen, result });
        });
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
                    aws.describe_worker_queues(&app, &env)
                        .await
                        .ok()
                        .and_then(|q| q.dlq_stats.map(|s| (env, s.visible)))
                }
            });
            let results: Vec<(String, i64)> = join_all(futs).await.into_iter().flatten().collect();
            let _ = tx.send(AppMsg::WorkerQueueCheck { gen, results });
        });
    }

    fn spawn_events(&mut self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        // Scope the events panel to the currently-selected env so it tells
        // the user about *this* env, not the entire account. Falls back to
        // the global event stream when no env is selected. The previously-
        // fetched env name is recorded so we can detect selection changes
        // and refetch without firing a request on every j/k.
        let selected = self.selected_env().map(|e| e.name.clone());
        self.events_for_env = selected.clone();
        tokio::spawn(async move {
            let result = match selected {
                Some(name) => aws.list_events_for_env(&name, 50).await,
                None => aws.list_events(50).await,
            }
            .map_err(|e| flatten_err("list_events", e));
            let _ = tx.send(AppMsg::Events { gen, result });
        });
    }

    /// Refetch the events panel if the cursor has moved to a different env
    /// since the last fetch. Called from the main loop just before draw, so
    /// any keystroke / mouse click that changed selection picks up the new
    /// env's events on the next frame.
    fn refresh_events_if_selection_changed(&mut self) {
        if !self.events_visible {
            return;
        }
        let selected = self.selected_env().map(|e| e.name.clone());
        if selected != self.events_for_env {
            self.spawn_events();
        }
    }

    /// Apply a Detail-tab AppMsg payload. Handles the boilerplate every
    /// `Detail*` variant shares: drop on stale generation, drop when no
    /// Detail view is open, drop when the user switched to a different
    /// env mid-fetch.
    ///
    /// The closure runs against `&mut DetailState` + the raw
    /// `Result<T, String>` so the caller picks its own success / error
    /// behaviour — most clear `detail.error` on the Ok branch, but tags /
    /// env-vars use `tracing::warn!` instead since their failures
    /// shouldn't tint the whole tab red.
    fn apply_detail_msg<T, F>(
        &mut self,
        gen: u64,
        env_name: &str,
        result: Result<T, String>,
        apply: F,
    ) where
        F: FnOnce(&mut DetailState, Result<T, String>),
    {
        if gen != self.generation {
            return;
        }
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.env_name != env_name {
            return;
        }
        apply(detail, result);
    }

    fn handle_msg(&mut self, msg: AppMsg) {
        match msg {
            AppMsg::Refresh { gen, result } => {
                if gen != self.generation {
                    return; // stale result from a previous context — drop.
                }
                self.apply_refresh(result);
            }
            AppMsg::Rebuild(result) => self.apply_rebuild(result),
            AppMsg::Identity { gen, result } => {
                if gen != self.generation {
                    return;
                }
                match result {
                    Ok(id) => {
                        self.context.account_id = id.account_id;
                        self.context.caller_arn = id.caller_arn;
                    }
                    Err(msg) => {
                        tracing::warn!(error = %msg, "identity refresh failed");
                    }
                }
            }
            AppMsg::Applications { gen, result } => {
                if gen != self.generation {
                    return;
                }
                match result {
                    Ok(mut apps) => {
                        apps.sort_by_key(|a| a.name.to_lowercase());
                        // Preserve the previously-fetched LATEST values across
                        // refreshes so the column doesn't flicker to "—" every
                        // tick while the follow-up fan-out is in flight.
                        merge_app_latest_versions(&self.applications, &mut apps);
                        self.applications = apps;
                        // Pinned-first sort runs every refresh so newly-arrived
                        // apps don't shuffle the pinned ones off the top row.
                        self.resort_applications();
                        if self.applications.is_empty() {
                            self.app_table_state.select(None);
                        } else if self
                            .app_table_state
                            .selected()
                            .map(|s| s >= self.applications.len())
                            .unwrap_or(true)
                        {
                            self.app_table_state.select(Some(0));
                        }
                        // Fan out latest-version fetches only when the
                        // operator is actually looking at the apps view.
                        // Otherwise we'd burn N DescribeApplicationVersions
                        // calls on every refresh tick for users who live in
                        // the envs view all day. Switching scope to Apps
                        // (Tab / BackTab) kicks off the fetch on demand;
                        // the periodic refresh then keeps it fresh.
                        if self.scope == Scope::Apps {
                            self.spawn_app_latest_versions();
                        }
                    }
                    Err(msg) => tracing::warn!(error = %msg, "applications fetch failed"),
                }
            }
            AppMsg::AppLatestVersions { gen, results } => {
                if gen != self.generation {
                    return;
                }
                let by_name: std::collections::HashMap<_, _> = results
                    .into_iter()
                    .map(|(name, label, created)| (name, (label, created)))
                    .collect();
                for app in self.applications.iter_mut() {
                    if let Some((label, created)) = by_name.get(&app.name) {
                        app.latest_version_label = label.clone();
                        app.latest_version_created = *created;
                    }
                }
            }
            AppMsg::WorkerQueueCheck { gen, results } => {
                if gen != self.generation {
                    return;
                }
                // Rebuild the cache from scratch so workers whose DLQ
                // drained back to zero are reflected. Missing entries =
                // "fetch failed this tick"; we drop them so a transient
                // SQS error doesn't blank the chip for everyone.
                self.worker_dlq_depths.clear();
                for (env_name, depth) in results {
                    self.worker_dlq_depths.insert(env_name, depth);
                }
                // Recompute alerts now that the cache is fresh — the
                // count set during apply_refresh used the *previous*
                // tick's cache. Workers newly above DLQ=0 join the
                // alert pill on the next draw.
                self.alerts = compute_red_alerts(&self.environments, &self.worker_dlq_depths);
            }
            AppMsg::Events { gen, result } => {
                if gen != self.generation {
                    return;
                }
                match result {
                    // The API returns events in time-descending order already.
                    Ok(events) => self.events = events,
                    Err(msg) => tracing::warn!(error = %msg, "event fetch failed"),
                }
            }
            AppMsg::DetailEvents {
                gen,
                env_name,
                result,
            } => {
                self.apply_detail_msg(gen, &env_name, result, |d, r| {
                    d.loading_events = false;
                    match r {
                        Ok(events) => {
                            d.events = events;
                            d.error = None;
                        }
                        Err(msg) => d.error = Some(msg),
                    }
                });
            }
            AppMsg::ActionResult {
                gen,
                action,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                write_audit_outcome(
                    self.context.account_id.as_deref(),
                    self.context.profile.as_deref(),
                    &self.context.region,
                    action,
                    &env_name,
                    result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                );
                // Stamp the matching pending-actions entry with the outcome
                // so the panel shows ✓ / ✗ instead of "in flight".
                self.complete_pending(
                    action.label(),
                    &env_name,
                    result.as_ref().map(|_| ()).map_err(|e| e.clone()),
                );
                match result {
                    Ok(()) => {
                        self.close_action_flow();
                        self.status_message =
                            Some(format!("{} on {env_name} dispatched", action.label()));
                        self.spawn_refresh();
                    }
                    Err(msg) => {
                        // Keep the confirm modal open via a Running→error transition;
                        // simpler: close flow, surface the error.
                        self.close_action_flow();
                        self.error_message =
                            Some(format!("{} on {env_name} failed: {msg}", action.label()));
                    }
                }
            }
            AppMsg::DetailInstances {
                gen,
                env_name,
                result,
            } => {
                self.apply_detail_msg(gen, &env_name, result, |d, r| {
                    d.loading_instances = false;
                    match r {
                        Ok(instances) => {
                            d.instances = instances;
                            d.error = None;
                        }
                        Err(msg) => d.error = Some(msg),
                    }
                });
            }
            AppMsg::DetailMetrics {
                gen,
                env_name,
                result,
            } => {
                self.apply_detail_msg(gen, &env_name, result, |d, r| {
                    d.loading_metrics = false;
                    match r {
                        Ok(metrics) => {
                            d.metrics = metrics;
                            d.error = None;
                        }
                        Err(msg) => d.error = Some(msg),
                    }
                });
            }
            AppMsg::DetailLogsProgress {
                gen,
                env_name,
                stage,
                attempt,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(detail) = self.detail.as_mut() else {
                    return;
                };
                if detail.env_name != env_name {
                    return;
                }
                detail.log_tail.stage = stage;
                if matches!(stage, LogTailStage::Polling) {
                    detail.log_tail.poll_attempt = attempt;
                }
            }
            AppMsg::DetailLogs {
                gen,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(detail) = self.detail.as_mut() else {
                    return;
                };
                if detail.env_name != env_name {
                    return;
                }
                match result {
                    Ok(by_instance) => {
                        detail.log_tail.by_instance = by_instance;
                        detail.log_tail.stage = LogTailStage::Ready;
                        detail.log_tail.error = None;
                    }
                    Err(msg) => {
                        detail.log_tail.stage = LogTailStage::Ready;
                        detail.log_tail.error = Some(msg);
                    }
                }
            }
            AppMsg::TextOverlay { gen, title, body } => {
                if gen != self.generation {
                    return;
                }
                self.current_overlay = Some(Overlay::TextDump { title, body });
            }
            AppMsg::AppVersions {
                gen,
                application,
                deployed_label,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                match result {
                    Ok(versions) if versions.is_empty() => {
                        self.status_message =
                            Some(format!("no application versions for {application}"));
                    }
                    Ok(versions) => {
                        self.current_overlay = Some(Overlay::TextDump {
                            title: format!("application versions — {application}"),
                            body: format_app_versions(&versions, deployed_label.as_deref(), 20),
                        });
                    }
                    Err(msg) => self.error_message = Some(msg),
                }
            }
            AppMsg::UpdateCheck(latest) => {
                if let Some(release) = latest {
                    tracing::info!(target: "ebman::update", current = env!("CARGO_PKG_VERSION"), latest = %release.version, "newer ebman released on crates.io");
                    self.update_available = Some(release);
                }
            }
            AppMsg::DryRunResult {
                gen,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(ActionFlow::Confirm(modal)) = self.action_flow.as_mut() else {
                    return;
                };
                if modal.target_env != env_name {
                    return;
                }
                modal.loading_dryrun = false;
                if let Ok(instances) = result {
                    let azs: std::collections::HashSet<&str> = instances
                        .iter()
                        .map(|i| i.availability_zone.as_str())
                        .filter(|az| !az.is_empty())
                        .collect();
                    modal.dryrun = Some(DryRunInfo {
                        instance_count: instances.len(),
                        az_count: azs.len(),
                    });
                }
            }
            AppMsg::Alarms {
                gen,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                // Drop stale results: the user may have closed the overlay or
                // requested alarms for a different env during the round-trip.
                // The overlay carries the env it was opened for; only replace
                // its body if that still matches the result we just received.
                match self.current_overlay.as_mut() {
                    Some(Overlay::Alarms {
                        env_name: requested,
                        body,
                    }) if requested == &env_name => {
                        *body = format_alarms(result);
                    }
                    _ => (),
                }
            }
            AppMsg::WhyRedEvents {
                gen,
                session_id,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                if let Some(Overlay::WhyRed {
                    session_id: s,
                    events,
                    ..
                }) = self.current_overlay.as_mut()
                {
                    if *s == session_id {
                        *events = Some(result);
                    }
                }
            }
            AppMsg::WhyRedAlarms {
                gen,
                session_id,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                if let Some(Overlay::WhyRed {
                    session_id: s,
                    alarms,
                    ..
                }) = self.current_overlay.as_mut()
                {
                    if *s == session_id {
                        *alarms = Some(result);
                    }
                }
            }
            AppMsg::WhyRedInstances {
                gen,
                session_id,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                if let Some(Overlay::WhyRed {
                    session_id: s,
                    instances,
                    ..
                }) = self.current_overlay.as_mut()
                {
                    if *s == session_id {
                        *instances = Some(result);
                    }
                }
            }
            AppMsg::WhyRedDeploys {
                gen,
                session_id,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                if let Some(Overlay::WhyRed {
                    session_id: s,
                    deploys,
                    ..
                }) = self.current_overlay.as_mut()
                {
                    if *s == session_id {
                        *deploys = Some(result);
                    }
                }
            }
            AppMsg::WhyRedQueues {
                gen,
                session_id,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                // Land the queues result, then kick the DLQ peek if the
                // DLQ has visible messages. The peek is a second-stage
                // fetch — it only fires when the first stage shows
                // something worth peeking at, avoiding pointless SQS
                // calls on healthy workers.
                let mut dlq_url_to_peek: Option<String> = None;
                if let Some(Overlay::WhyRed {
                    session_id: s,
                    queues,
                    dlq_messages,
                    ..
                }) = self.current_overlay.as_mut()
                {
                    if *s == session_id {
                        if let Ok(ref qs) = result {
                            let dlq_visible = qs.dlq_stats.as_ref().map(|s| s.visible).unwrap_or(0);
                            if dlq_visible > 0 {
                                if let Some(url) = qs.dlq_url.clone() {
                                    dlq_url_to_peek = Some(url);
                                }
                            } else {
                                // Mark dlq_messages as resolved-empty so
                                // the renderer doesn't show "loading…"
                                // forever for a clean DLQ.
                                *dlq_messages = Some(Ok(Vec::new()));
                            }
                        }
                        *queues = Some(result);
                    }
                }
                if let Some(url) = dlq_url_to_peek {
                    self.spawn_why_red_dlq_peek(url, session_id);
                }
            }
            AppMsg::WhyRedDlqMessages {
                gen,
                session_id,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                if let Some(Overlay::WhyRed {
                    session_id: s,
                    dlq_messages,
                    ..
                }) = self.current_overlay.as_mut()
                {
                    if *s == session_id {
                        *dlq_messages = Some(result);
                    }
                }
            }
            AppMsg::PreflightEvents {
                gen,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(ActionFlow::Confirm(modal)) = self.action_flow.as_mut() else {
                    return;
                };
                if modal.target_env != env_name {
                    return;
                }
                modal.loading_events = false;
                if let Ok(events) = result {
                    modal.recent_events = Some(events);
                }
            }
            AppMsg::DetailTags {
                gen,
                env_name,
                result,
            } => {
                self.apply_detail_msg(gen, &env_name, result, |d, r| {
                    d.loading_tags = false;
                    match r {
                        Ok(tags) => d.tags = tags,
                        Err(msg) => tracing::warn!(error = %msg, "tags fetch failed"),
                    }
                });
            }
            AppMsg::DeployFromLocal {
                gen,
                env_name,
                label,
                summary,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                self.complete_pending(
                    &summary,
                    &env_name,
                    result.as_ref().map(|_| ()).map_err(|e| e.clone()),
                );
                match result {
                    Ok(()) => {
                        self.push_toast(
                            ToastKind::Info,
                            format!("{summary} → {env_name} (version {label})"),
                        );
                    }
                    Err(msg) => {
                        self.push_toast(
                            ToastKind::Error,
                            format!("{summary} on {env_name} failed: {msg}"),
                        );
                    }
                }
            }
            AppMsg::LogTailOpened {
                gen,
                session_id,
                env_name,
                log_group,
                since_ms,
            } => {
                if gen != self.generation || session_id != self.log_tail_session {
                    return;
                }
                self.current_overlay = Some(Overlay::LogTail {
                    log_group,
                    env_name,
                    events: std::collections::VecDeque::with_capacity(LOG_TAIL_MAX_LINES),
                    scroll: 0,
                    following: true,
                    since_ms,
                    filter_input: String::new(),
                    filter_active: false,
                    filter_pattern: None,
                    last_err: None,
                    session_id,
                });
                self.status_message = None;
            }
            AppMsg::LogTailEvents {
                gen,
                session_id,
                next_since_ms,
                result,
            } => {
                if gen != self.generation || session_id != self.log_tail_session {
                    return;
                }
                // Route to whichever overlay slot currently holds the LogTail
                // — `current_overlay` normally, or `pre_help_overlay` if the
                // user pressed `?` mid-tail. Without the second slot, events
                // arriving during the help round-trip would be lost.
                let target = if matches!(
                    self.current_overlay.as_ref(),
                    Some(Overlay::LogTail { session_id: s, .. }) if *s == session_id
                ) {
                    self.current_overlay.as_mut()
                } else if matches!(
                    self.pre_help_overlay.as_ref(),
                    Some(Overlay::LogTail { session_id: s, .. }) if *s == session_id
                ) {
                    self.pre_help_overlay.as_mut()
                } else {
                    return;
                };
                let Some(Overlay::LogTail {
                    events,
                    since_ms,
                    last_err,
                    ..
                }) = target
                else {
                    return;
                };
                *since_ms = next_since_ms;
                match result {
                    Ok(new_events) => {
                        *last_err = None;
                        for ev in new_events {
                            if events.len() >= LOG_TAIL_MAX_LINES {
                                events.pop_front();
                            }
                            events.push_back(ev);
                        }
                    }
                    Err(msg) => {
                        *last_err = Some(msg);
                    }
                }
            }
            AppMsg::DetailLogGroups {
                gen,
                env_name,
                groups,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(detail) = self.detail.as_mut() else {
                    return;
                };
                if detail.env_name != env_name {
                    return;
                }
                detail.cw_log_groups = Some(groups);
            }
            AppMsg::DetailAlarms {
                gen,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(detail) = self.detail.as_mut() else {
                    return;
                };
                if detail.env_name != env_name {
                    return;
                }
                detail.loading_cw_alarms = false;
                detail.cw_alarms = Some(result);
            }
            AppMsg::EnvVarsForEdit {
                gen,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                match result {
                    Ok(vars) => {
                        // Stash for the main loop to consume. The
                        // editor shell-out has to happen there
                        // because that's where the `Tui` handle is.
                        self.pending_env_edit = Some((env_name, vars));
                    }
                    Err(msg) => {
                        self.error_message = Some(format!("env-edit fetch: {msg}"));
                    }
                }
            }
            AppMsg::CostsFetched {
                gen,
                account,
                region,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                match result {
                    Ok(rows) => {
                        let now = chrono::Utc::now();
                        self.costs.clear();
                        for row in &rows {
                            self.costs.insert(row.env_name.clone(), row.cost_usd);
                        }
                        self.costs_fetched_at = Some(now);
                        // Persist to ~/.cache/ebman/cost-{account}-{region}.toml
                        // so subsequent sessions render immediately.
                        let account_key = account.unwrap_or_else(|| "unknown".into());
                        let cache = crate::cost_cache::CostCache {
                            fetched_at: Some(now),
                            costs: self.costs.clone(),
                        };
                        if let Err(e) = crate::cost_cache::save(&account_key, &region, &cache) {
                            tracing::warn!(
                                target: "ebman::cost",
                                error = %e,
                                "cost cache write failed (non-fatal)"
                            );
                        }
                        let n = rows.len();
                        self.status_message = Some(format!("cost: refreshed {n} env(s)"));
                    }
                    Err(msg) => {
                        self.error_message = Some(format!("cost fetch: {msg}"));
                    }
                }
            }
            AppMsg::DetailRecentVersions {
                gen,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(detail) = self.detail.as_mut() else {
                    return;
                };
                if detail.env_name != env_name {
                    return;
                }
                detail.loading_recent_versions = false;
                detail.recent_versions = Some(result);
            }
            AppMsg::FormPrefilled {
                gen,
                env_name,
                settings,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(form) = self.form.as_mut() else {
                    return;
                };
                if form.env_name != env_name {
                    return;
                }
                match settings {
                    Err(msg) => {
                        // Surface the fetch failure on the form's first
                        // field as a global error; operator can dismiss or
                        // fill values manually.
                        if let Some(first) = form.fields.first_mut() {
                            first.error = Some(format!("pre-fill failed: {msg}"));
                        }
                        form.state = crate::form::FormState::Ready;
                    }
                    Ok(rows) => {
                        // Build a (ns, name) -> value lookup; populate the
                        // form's fields using the mappings stored on submit.
                        use std::collections::HashMap;
                        let lookup: HashMap<(String, String), String> = rows
                            .into_iter()
                            .map(|(ns, name, value)| ((ns, name), value))
                            .collect();
                        let mappings = match &form.submit {
                            crate::form::FormSubmit::OptionSettings { mappings } => {
                                mappings.clone()
                            }
                            // LocalConfig forms skip the AWS pre-fill in
                            // `open_form` so the FormPrefilled msg never
                            // fires for them — drop the result if one
                            // arrives anyway (stale message after the user
                            // switched form types).
                            crate::form::FormSubmit::LocalConfig => return,
                        };
                        for (key, ns, opt) in mappings {
                            if let Some(value) = lookup.get(&(ns, opt)) {
                                if let Some(field) = form.fields.iter_mut().find(|f| f.key == key) {
                                    field.value = value.clone();
                                }
                            }
                        }
                        form.state = crate::form::FormState::Ready;
                    }
                }
            }
            AppMsg::FormMultiSelectLoaded {
                gen,
                env_name,
                field_key,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(form) = self.form.as_mut() else {
                    return;
                };
                if form.env_name != env_name {
                    return;
                }
                let Some(field) = form.fields.iter_mut().find(|f| f.key == field_key) else {
                    return;
                };
                match result {
                    Err(msg) => {
                        field.error = Some(format!("load failed: {msg}"));
                        form.state = crate::form::FormState::Ready;
                    }
                    Ok(opts) => {
                        let initial_filtered: Vec<String> = opts
                            .initial
                            .iter()
                            .filter(|v| opts.options.iter().any(|o| o == *v))
                            .cloned()
                            .collect();
                        field.value = initial_filtered.join(",");
                        field.kind = crate::form::FieldKind::MultiSelect {
                            options: opts.options.clone(),
                        };
                        if opts.annotations.len() == opts.options.len()
                            && !opts.annotations.is_empty()
                        {
                            field.option_annotations = Some(opts.annotations);
                        }
                        field.option_cursor = 0;
                        form.state = crate::form::FormState::Ready;
                    }
                }
            }
            AppMsg::DetailEnvVars {
                gen,
                env_name,
                result,
            } => {
                self.apply_detail_msg(gen, &env_name, result, |d, r| {
                    d.loading_env_vars = false;
                    match r {
                        Ok(vars) => d.env_vars = vars,
                        Err(msg) => tracing::warn!(error = %msg, "env vars fetch failed"),
                    }
                });
            }
            AppMsg::OptionSettingsUpdate {
                gen,
                env_name,
                summary,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                self.complete_pending(
                    &summary,
                    &env_name,
                    result.as_ref().map(|_| ()).map_err(|e| e.clone()),
                );
                match result {
                    Ok(()) => {
                        self.push_toast(ToastKind::Success, format!("{summary} → {env_name}"));
                        // If it was an env-var set/unset and the Detail view
                        // is open on the same env, refresh the Config tab's
                        // env vars so the change reflects without waiting
                        // for the next 15s tick.
                        if summary.starts_with("env set ") || summary.starts_with("env unset ") {
                            if let Some(d) = self.detail.as_ref() {
                                if d.env_name == env_name {
                                    self.spawn_detail_env_vars();
                                }
                            }
                        }
                    }
                    Err(msg) => {
                        self.push_toast(
                            ToastKind::Error,
                            format!("{summary} on {env_name} failed: {msg}"),
                        );
                    }
                }
            }
            AppMsg::AlarmOp {
                gen,
                verb,
                alarm_name,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                let label = match verb {
                    "create" => "Create alarm",
                    "delete" => "Delete alarm",
                    _ => "Alarm op",
                };
                let target = format!("{env_name}/{alarm_name}");
                self.complete_pending(
                    label,
                    &target,
                    result.as_ref().map(|_| ()).map_err(|e| e.clone()),
                );
                match result {
                    Ok(()) => {
                        let past = if verb == "create" {
                            "created"
                        } else {
                            "deleted"
                        };
                        self.push_toast(ToastKind::Success, format!("alarm '{alarm_name}' {past}"));
                    }
                    Err(msg) => {
                        self.push_toast(
                            ToastKind::Error,
                            format!("alarm '{alarm_name}' {verb} failed: {msg}"),
                        );
                    }
                }
            }
            AppMsg::DeleteAppVersion {
                gen,
                application,
                label,
                force,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                let force_str = if force { " (+source bundle)" } else { "" };
                let pending_label = if force {
                    "Delete app version (+source)"
                } else {
                    "Delete app version"
                };
                let pending_target = format!("{application}/{label}");
                self.complete_pending(
                    pending_label,
                    &pending_target,
                    result.as_ref().map(|_| ()).map_err(|e| e.clone()),
                );
                match result {
                    Ok(()) => {
                        self.push_toast(
                            ToastKind::Info,
                            format!("deleted {application}/{label}{force_str}"),
                        );
                        // If the user has the matching `:versions` overlay
                        // open, re-fetch so the deleted entry disappears
                        // instead of lingering as stale text.
                        let want_title = format!("application versions — {application}");
                        if matches!(
                            self.current_overlay.as_ref(),
                            Some(Overlay::TextDump { title, .. }) if title == &want_title
                        ) {
                            let aws = self.aws.clone();
                            let tx = self.msg_tx.clone();
                            let gen = self.generation;
                            let app_name = application.clone();
                            // Look up the env's currently-deployed version
                            // to re-mark it after the refresh. Picks the
                            // first env in this application — single-env
                            // case is the norm; multi-env case is rare and
                            // the marker is best-effort anyway.
                            let deployed_label = self
                                .environments
                                .iter()
                                .find(|e| e.application == application)
                                .filter(|e| !e.version_label.is_empty())
                                .map(|e| e.version_label.clone());
                            tokio::spawn(async move {
                                let result = aws
                                    .list_application_versions(&app_name)
                                    .await
                                    .map_err(|e| flatten_err("list_application_versions", e));
                                let _ = tx.send(AppMsg::AppVersions {
                                    gen,
                                    application: app_name,
                                    deployed_label,
                                    result,
                                });
                            });
                        }
                    }
                    Err(msg) => {
                        self.push_toast(
                            ToastKind::Error,
                            format!("delete {application}/{label}{force_str} failed: {msg}"),
                        );
                    }
                }
            }
            AppMsg::TagUpdate {
                gen,
                env_name,
                summary,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                self.complete_pending(
                    &summary,
                    &env_name,
                    result.as_ref().map(|_| ()).map_err(|e| e.clone()),
                );
                match result {
                    Ok(()) => {
                        self.push_toast(ToastKind::Info, format!("{summary} on {env_name}"));
                        if let Some(d) = self.detail.as_ref() {
                            if d.env_name == env_name {
                                self.spawn_detail_tags();
                            }
                        }
                    }
                    Err(msg) => {
                        self.push_toast(
                            ToastKind::Error,
                            format!("{summary} on {env_name} failed: {msg}"),
                        );
                    }
                }
            }
            AppMsg::DetailQueues {
                gen,
                env_name,
                result,
            } => {
                self.apply_detail_msg(gen, &env_name, result, |d, r| {
                    d.loading_queues = false;
                    match r {
                        Ok(queues) => {
                            d.queues = queues;
                            d.error = None;
                        }
                        Err(msg) => d.error = Some(msg),
                    }
                });
            }
            AppMsg::DlqMessages {
                gen,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(dlq) = self.dlq.as_mut() else { return };
                if dlq.env_name != env_name {
                    return;
                }
                dlq.loading = false;
                match result {
                    Ok(messages) => {
                        dlq.messages = messages;
                        let cur = dlq.list_state.selected().unwrap_or(0);
                        if dlq.messages.is_empty() {
                            dlq.list_state.select(None);
                        } else if cur >= dlq.messages.len() {
                            dlq.list_state.select(Some(0));
                        }
                        dlq.error = None;
                    }
                    Err(msg) => dlq.error = Some(msg),
                }
            }
            AppMsg::DlqActionResult {
                gen,
                env_name,
                result,
            } => {
                if gen != self.generation {
                    return;
                }
                let Some(dlq) = self.dlq.as_mut() else { return };
                if dlq.env_name != env_name {
                    return;
                }
                match result {
                    Ok(DlqOp::Resent { message_id }) => {
                        dlq.messages.retain(|m| m.id != message_id);
                        self.status_message = Some(format!("message {message_id} resent"));
                    }
                    Ok(DlqOp::Purged) => {
                        dlq.messages.clear();
                        self.status_message = Some("DLQ purged".into());
                    }
                    Err(msg) => dlq.error = Some(msg),
                }
            }
        }
    }

    fn apply_rebuild(&mut self, result: Result<Box<AwsClient>, String>) {
        match result {
            Ok(client) => {
                self.generation = self.generation.wrapping_add(1);
                self.context = client.context.clone();
                self.aws = Arc::new(*client);
                self.maybe_apply_profile_theme();
                self.environments.clear();
                self.events.clear();
                self.events_scroll = 0;
                self.history.clear();
                // Overlays show data from the previous context (describe dump,
                // alarms list, …); close them so the user doesn't act on stale info.
                self.current_overlay = None;
                // Tear down any long-running CW Logs poll that's mid-flight;
                // it would otherwise keep hitting the previous account's CW.
                // Also bump session id so any in-flight LogTailOpened from
                // the aborted task is dropped on arrival.
                if let Some(handle) = self.log_tail_task.take() {
                    handle.abort();
                }
                self.log_tail_session = self.log_tail_session.wrapping_add(1);
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
                self.filter = app_name.clone();
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
        self.filter = name.clone();
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
    fn rebuild_view(&mut self) {
        // Filtered indexes.
        self.cached_filtered.clear();
        if self.filter.is_empty() {
            self.cached_filtered.extend(0..self.environments.len());
        } else {
            let needle = self.filter.to_lowercase();
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
                        if let Some(url) = self.webhook_url.clone() {
                            fire_webhook(
                                url,
                                e.name.clone(),
                                e.application.clone(),
                                e.health.clone(),
                                self.context.region.clone(),
                                self.context.account_id.clone(),
                            );
                        }
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
        let lower = msg.to_lowercase();
        let sso_signals = [
            "expiredtoken",
            "expired token",
            "token has expired",
            "the security token included in the request is expired",
            "unable to load credentials",
            "no credentials in the property bag",
            "sso session has expired",
        ];
        if sso_signals.iter().any(|s| lower.contains(s)) {
            let profile = self
                .override_profile
                .clone()
                .or_else(|| self.context.profile.clone())
                .unwrap_or_else(|| "default".into());
            return format!(
                "credentials expired — run: aws sso login --profile {profile}  (or refresh your creds, then press Ctrl-R)"
            );
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

/// Render a small JSON payload describing a Red transition and fire a POST
/// via `curl` (already in the toolchain budget for log-tail). The fire is
/// detached — we don't await it, don't care about the response, just want to
/// nudge the configured webhook so a Slack / collector can react. The text
/// is escaped just enough to survive single-line JSON; env / app names from
/// EB are restricted to alphanumeric + `-_.` so the escape is conservative.
fn fire_webhook(
    url: String,
    env: String,
    application: String,
    health: String,
    region: String,
    account: Option<String>,
) {
    let payload = build_webhook_payload(&env, &application, &health, &region, account.as_deref());
    tokio::spawn(async move {
        use tokio::process::Command;
        let _ = Command::new("curl")
            .args([
                "-s",
                "-S",
                "--max-time",
                "10",
                "-X",
                "POST",
                "-H",
                "Content-Type: application/json",
                "-d",
            ])
            .arg(&payload)
            .arg(&url)
            .output()
            .await;
    });
}

/// Format the webhook payload as a flat JSON object. Public for tests so we
/// can pin down the shape independently of the network code.
pub fn build_webhook_payload(
    env: &str,
    application: &str,
    health: &str,
    region: &str,
    account: Option<&str>,
) -> String {
    let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
    format!(
        "{{\"event\":\"env_red\",\"env\":\"{}\",\"application\":\"{}\",\"health\":\"{}\",\"region\":\"{}\",\"account\":\"{}\"}}",
        esc(env),
        esc(application),
        esc(health),
        esc(region),
        esc(account.unwrap_or("")),
    )
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
        let Some(ev) = self.events.get(idx) else {
            self.events_cursor = None;
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

/// Compact age formatter — "3s", "12s", "2m", "1h", "4d". Used for the
/// pending-actions overlay so ages stay short and uniform.
pub fn humanize_short_age(d: Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

/// Parse a `:tag KEY [value tokens…]` argument list. Returns `Some((key,
/// value))` when there's at least a key and one value token. Value tokens
/// are joined with a single space — there's no shell-style quoting, since
/// we trust the operator and want the command bar to stay typeable.
pub fn parse_tag_args(rest: &[&str]) -> Option<(String, String)> {
    let key = (*rest.first()?).to_string();
    if rest.len() < 2 {
        return None;
    }
    let value = rest[1..].join(" ");
    if key.is_empty() || value.is_empty() {
        return None;
    }
    Some((key, value))
}

/// Extract a "delta toast key" from text shaped like `▲2 Red` / `▼1 Yellow`.
/// Returns `Some(bucket_name)` when the text is a status-delta toast and we
/// want subsequent updates for the same bucket to replace rather than stack.
/// Pure function so it's easy to pin down in tests.
pub fn delta_toast_key(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    let mut chars = trimmed.chars();
    let first = chars.next()?;
    if first != '▲' && first != '▼' {
        return None;
    }
    let rest: String = chars.collect();
    // Require at least one digit immediately after the arrow.
    let first_rest = rest.chars().next()?;
    if !first_rest.is_ascii_digit() {
        return None;
    }
    let bucket_start = rest.find(|c: char| !c.is_ascii_digit())?;
    let after_digits = &rest[bucket_start..];
    let bucket = after_digits.trim_start();
    if bucket.is_empty() || !bucket.starts_with(|c: char| c.is_ascii_alphabetic()) {
        return None;
    }
    let word: String = bucket
        .chars()
        .take_while(|c| c.is_ascii_alphabetic())
        .collect();
    Some(word)
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

/// Pure: copy `latest_version_label` / `latest_version_created` from a
/// previous `applications` snapshot onto the new one (matched by name) so
/// the apps-view LATEST column doesn't flicker to "—" while the follow-up
/// `DescribeApplicationVersions` fan-out is in flight after each refresh.
///
/// Only fills slots that are currently `None`. Today `list_applications`
/// never populates those fields itself so the conditional is a no-op
/// safety net — but it means a future caller that *does* pre-populate
/// won't get silently overwritten with stale data.
fn merge_app_latest_versions(prev: &[Application], next: &mut [Application]) {
    let by_name: std::collections::HashMap<
        &str,
        (&Option<String>, &Option<chrono::DateTime<chrono::Utc>>),
    > = prev
        .iter()
        .map(|a| {
            (
                a.name.as_str(),
                (&a.latest_version_label, &a.latest_version_created),
            )
        })
        .collect();
    for app in next.iter_mut() {
        let Some((label, created)) = by_name.get(app.name.as_str()) else {
            continue;
        };
        if app.latest_version_label.is_none() {
            app.latest_version_label = (*label).clone();
        }
        if app.latest_version_created.is_none() {
            app.latest_version_created = **created;
        }
    }
}

/// Pure: redact a free-form string for display in the `:history` overlay
/// context header. Matches the `redact` helper in `ui.rs` (full-block
/// shaded chars preserving length) so the look is consistent — duplicated
/// rather than imported because the ui module's `redact` is private.
pub(crate) fn redact_for_log(value: &str, on: bool) -> String {
    if !on || value.is_empty() || value == "—" {
        return value.to_string();
    }
    "▓".repeat(value.chars().count())
}

/// Inferred kind of an `Updating` env's in-flight operation. EB's
/// `status` field is generic ("Updating") regardless of cause, but the
/// recent events expose what's actually happening. The Health tab uses
/// this to render `Updating: deploying build-142` (or similar) instead
/// of just the generic pill.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateKind {
    /// `UpdateEnvironment(version_label)` — a version deploy in flight.
    /// `version_label` is extracted from the event message when present.
    Deploy { version_label: Option<String> },
    /// `UpdateEnvironment(option_settings)` — configuration change in flight.
    Config,
    /// Auto-scaling activity — instances being added or removed.
    Scale,
    /// `UpdateEnvironment(platform_arn)` / managed platform update.
    Platform,
    /// Status is Updating but no recent event matches a known pattern.
    /// Falls back to a generic "updating" label.
    Generic,
}

/// Pure: classify an `Updating` env's in-flight op by looking at the
/// most recent event whose message matches a known pattern. Events are
/// expected newest-first (as the EB API returns them); returns the kind
/// from the first matching event. Returns `Generic` when nothing
/// matches.
pub fn classify_update_kind(events: &[crate::aws::Event]) -> UpdateKind {
    for e in events {
        let lower = e.message.to_lowercase();
        // Deploy comes first — "version label" is the unambiguous signal.
        // The "version label" check catches both the dispatch event
        // (`Updating environment to use version label 'X'`) and the
        // completion event (`Environment update completed successfully
        // … version 'X'`).
        if lower.contains("version label") {
            return UpdateKind::Deploy {
                version_label: extract_quoted_after(&e.message, "version label"),
            };
        }
        if lower.contains("deploying") && lower.contains("version") {
            return UpdateKind::Deploy {
                version_label: extract_quoted_after(&e.message, "version"),
            };
        }
        // Platform updates have a distinctive "platform" + "updat" stem.
        if lower.contains("platform") && (lower.contains("updat") || lower.contains("upgrad")) {
            return UpdateKind::Platform;
        }
        // Config changes — option settings.
        if lower.contains("configuration") && lower.contains("updat") {
            return UpdateKind::Config;
        }
        // Auto-scaling — instances coming or going.
        if (lower.contains("adding") || lower.contains("removing")) && lower.contains("instance") {
            return UpdateKind::Scale;
        }
    }
    UpdateKind::Generic
}

/// Pure: extract the first single-quoted string that appears after
/// `needle` in `msg` (case-insensitive needle match). Returns None if
/// the needle isn't found or there's no quoted substring after it. Used
/// to pull `'build-142'` out of "Updating environment to use version
/// label 'build-142'.".
fn extract_quoted_after(msg: &str, needle: &str) -> Option<String> {
    let lower = msg.to_lowercase();
    let needle_lower = needle.to_lowercase();
    let after = lower.find(&needle_lower)? + needle_lower.len();
    let tail = msg.get(after..)?;
    let start = tail.find('\'')?;
    let body = &tail[start + 1..];
    let end = body.find('\'')?;
    Some(body[..end].to_string())
}

fn flatten_err(op: &str, e: color_eyre::eyre::Report) -> String {
    tracing::error!(target: "ebman::aws", op = op, error = ?e, "aws call failed");
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

fn parse_sort(raw: Option<&str>) -> (SortKey, bool) {
    let Some(s) = raw else {
        return (SortKey::App, false);
    };
    let (k, dir) = s.split_once(':').unwrap_or((s, "asc"));
    let key = SortKey::parse(k.trim()).unwrap_or(SortKey::App);
    let desc = dir.trim().eq_ignore_ascii_case("desc");
    (key, desc)
}

fn health_rank(h: &str) -> u8 {
    match h.to_lowercase().as_str() {
        "green" | "ok" => 0,
        "grey" | "gray" | "info" | "no data" | "pending" => 1,
        "yellow" | "warning" => 2,
        "red" | "severe" | "degraded" => 3,
        _ => 4,
    }
}

fn parse_toggle(arg: Option<&str>, current: bool) -> bool {
    match arg.map(str::to_ascii_lowercase).as_deref() {
        Some("on") | Some("true") | Some("yes") | Some("1") => true,
        Some("off") | Some("false") | Some("no") | Some("0") => false,
        _ => !current,
    }
}

fn scroll_apply(current: u16, delta: i32) -> u16 {
    let next = current as i32 + delta;
    next.max(0) as u16
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

/// Render env vars as `KEY=VALUE` lines, aligned on the `=` for easy scan.
/// Empty values render as `""` so operators can distinguish "explicitly
/// empty" from "not set". Pure.
pub fn format_env_vars(vars: &[(String, String)]) -> String {
    if vars.is_empty() {
        return "(no env vars set)".into();
    }
    let key_width = vars
        .iter()
        .map(|(k, _)| k.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(8, 40);
    let mut out = String::new();
    for (k, v) in vars {
        let rendered = if v.is_empty() {
            "\"\"".to_string()
        } else {
            v.clone()
        };
        out.push_str(&format!("{k:<key_width$} = {rendered}\n"));
    }
    out
}

/// Parse the optional trailing args of `:metric add LABEL NS NAME ...`.
/// Args after `NAME` are either a stat name (`Average`, `Sum`, ...) or a
/// dimension list (`InstanceId=i-abc,Foo=bar`). Any token containing `=`
/// is treated as dims; the other is stat. Returns `(stat, dims)` with
/// `stat` defaulting to `Average` and `dims` to empty when absent. Pure.
pub fn parse_metric_extra_args(args: &[&str]) -> (String, Vec<(String, String)>) {
    let mut stat: Option<String> = None;
    let mut dims: Vec<(String, String)> = Vec::new();
    for tok in args {
        if tok.contains('=') {
            for kv in tok.split(',') {
                if let Some((k, v)) = kv.split_once('=') {
                    let k = k.trim();
                    let v = v.trim();
                    if !k.is_empty() && !v.is_empty() {
                        dims.push((k.to_string(), v.to_string()));
                    }
                }
            }
        } else if stat.is_none() {
            stat = Some(tok.to_string());
        }
    }
    (stat.unwrap_or_else(|| "Average".into()), dims)
}

/// Parse an `s3://bucket/key/with/slashes` URL into a `(bucket, key)`
/// tuple. Returns `None` if the input isn't an `s3://` URL or the bucket
/// or key is empty. Pure.
pub fn parse_s3_url(raw: &str) -> Option<(String, String)> {
    let rest = raw.strip_prefix("s3://")?;
    let (bucket, key) = rest.split_once('/')?;
    if bucket.is_empty() || key.is_empty() {
        return None;
    }
    Some((bucket.to_string(), key.to_string()))
}

/// Expand a leading `~/` to `$HOME/`. Other tilde forms (e.g. `~user`) are
/// left as-is; the operator gets a clear "can't read" error if they pass
/// something obscure. Pure for ease of testing.
pub fn expand_tilde(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = std::path::PathBuf::from(home);
            p.push(rest);
            return p.display().to_string();
        }
    }
    path.to_string()
}

/// Derive a version label from a file path + a timestamp. Uses the
/// filename stem (everything before the last `.`) so `./build.zip` becomes
/// `build_1684512345`. Sanitises any chars EB rejects in version labels
/// (anything outside `[A-Za-z0-9_.-]`). Pure for testability.
pub fn derive_version_label(path: &str, unix_ts: i64) -> String {
    let stem = std::path::Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("bundle");
    let sanitised: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    format!("{sanitised}_{unix_ts}")
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
    let outcome = match &result {
        Ok(()) => {
            format!("stage=completed action=DeployFromLocal target={env_name} label={label} ok")
        }
        Err(e) => format!(
            "stage=completed action=DeployFromLocal target={env_name} label={label} err=\"{}\"",
            e.replace('"', "'")
        ),
    };
    write_audit_line(account, profile, region, &outcome);
    let _ = tx.send(AppMsg::DeployFromLocal {
        gen,
        env_name,
        label,
        summary,
        result,
    });
}

/// Pick the most useful CloudWatch Logs group for an env's `:logs-tail`
/// default. EB streams to a handful of groups per env (web.stdout.log,
/// nginx access, eb-engine.log, …); we prefer the app stdout because that's
/// where deploy / runtime output lives. Falls back to the first by name.
/// Pure for testability.
pub fn pick_default_log_group(groups: &[String]) -> Option<String> {
    const PRIORITIES: &[&str] = &[
        "/var/log/web.stdout.log",
        "/var/log/eb-engine.log",
        "/var/log/eb-hooks.log",
        "/var/log/nginx/access.log",
    ];
    for needle in PRIORITIES {
        if let Some(g) = groups.iter().find(|g| g.ends_with(needle)) {
            return Some(g.clone());
        }
    }
    groups.first().cloned()
}

/// Pull a `--flag VALUE` style named argument out of a `:command` `rest`
/// slice and parse it. Returns `None` if the flag is absent, the value is
/// missing, or parsing fails. Used by commands like `:logs-stream` that
/// take optional flags alongside their positional args. Pure.
pub fn parse_named_arg<T: std::str::FromStr>(rest: &[&str], flag: &str) -> Option<T> {
    let pos = rest.iter().position(|s| *s == flag)?;
    rest.get(pos + 1).and_then(|v| v.parse().ok())
}

/// Render the `:versions` overlay body. Marks the currently-deployed
/// version with `◀ deployed`; trims the redundant
/// "Application version created from " prefix that every CI-pipeline
/// description tends to carry; shows "showing N of M (newest first)"
/// when the list was truncated. `limit` caps the visible rows.
/// Pure: render the `:deploy LABEL --preview` body. Highlights the
/// candidate version (label / age / description), the currently-deployed
/// version's age for context, and warns if the candidate predates the
/// current one (rolling back is intentional but worth flagging).
///
/// `versions` is the result of `list_application_versions` (already
/// sorted newest-first by the aws layer). Missing labels surface as
/// human-readable "not found" hints rather than blanks.
/// Pure: render the `:accounts` overlay body. Rows are sorted ACTIVE-first
/// then by name (the `list_org_accounts` helper does the sort on the AWS
/// side); this just formats one row per account with a `(:account NAME)`
/// hint when a matching `accounts.NAME` entry is configured in
/// config.toml. Without that entry, the row is informational only — the
/// operator must still configure a role_arn before AssumeRole works.
///
/// `configured` is the set of friendly names from `config.toml`'s
/// `accounts.*` section; matching is name-or-id-suffix so an operator
/// who names their entries by account-id still gets the hint.
pub fn format_org_accounts(
    accounts: &[crate::aws::OrgAccount],
    configured: &std::collections::HashMap<String, String>,
) -> String {
    if accounts.is_empty() {
        return "no accounts returned by organizations:ListAccounts\n\nesc / q to close".into();
    }
    let mut out = String::new();
    out.push_str(&format!(
        "Org accounts ({})\n────────────────────\n\n",
        accounts.len()
    ));
    let max_name = accounts
        .iter()
        .map(|a| a.name.len())
        .max()
        .unwrap_or(0)
        .min(28);
    for a in accounts {
        let switchable = configured
            .keys()
            .find(|n| {
                n.eq_ignore_ascii_case(&a.name)
                    || n.eq_ignore_ascii_case(&a.id)
                    || n.eq_ignore_ascii_case(&format!("acct-{}", a.id))
            })
            .cloned();
        let switch_hint = match switchable {
            Some(n) => format!(" :account {n}"),
            None => String::new(),
        };
        let status_marker = match a.status.as_str() {
            "ACTIVE" => "●",
            "SUSPENDED" => "⊘",
            _ => "○",
        };
        out.push_str(&format!(
            "  {status_marker} {name:<width$}  {id}  [{status}]{switch_hint}\n",
            name = a.name,
            width = max_name,
            id = a.id,
            status = a.status,
        ));
        if let Some(email) = a.email.as_ref() {
            out.push_str(&format!(
                "    {pad:<width$}  ↳ {email}\n",
                pad = "",
                width = max_name,
            ));
        }
    }
    out.push('\n');
    out.push_str(
        "To switch into an account, add `accounts.NAME.role_arn = …` to config.toml\n\
         then use `:account NAME`. esc / q to close.",
    );
    out
}

pub fn format_deploy_preview(
    env_name: &str,
    current_label: &str,
    candidate_label: &str,
    versions: &[crate::aws::AppVersion],
) -> String {
    let now = chrono::Utc::now();
    let humanize = |d: Option<chrono::DateTime<chrono::Utc>>| -> String {
        d.map(|t| {
            let dur = now.signed_duration_since(t);
            let secs = dur.num_seconds().max(0);
            if secs < 3600 {
                format!("{}m ago", secs / 60)
            } else if secs < 86_400 {
                format!("{}h ago", secs / 3600)
            } else {
                format!("{}d ago", secs / 86_400)
            }
        })
        .unwrap_or_else(|| "—".into())
    };
    let candidate = versions.iter().find(|v| v.label == candidate_label);
    let current = if current_label.is_empty() {
        None
    } else {
        versions.iter().find(|v| v.label == current_label)
    };
    let mut out = String::new();
    out.push_str(&format!("env:        {env_name}\n"));
    out.push_str(&format!(
        "current:    {}{}\n",
        if current_label.is_empty() {
            "(none deployed)".to_string()
        } else {
            current_label.to_string()
        },
        match current.and_then(|v| v.created) {
            Some(t) => format!("  ({})", humanize(Some(t))),
            None => String::new(),
        }
    ));
    out.push_str(&format!("candidate:  {candidate_label}"));
    match candidate {
        Some(v) => {
            out.push_str(&format!("  ({})\n", humanize(v.created)));
            if !v.description.is_empty() {
                out.push_str(&format!("description: {}\n", v.description));
            }
        }
        None => {
            out.push_str("\n\n");
            out.push_str(&format!(
                "⚠ candidate label '{candidate_label}' not found in this app's version list — \
                 deploy will fail. Run :versions to see available labels.\n"
            ));
            return out;
        }
    }
    // Rollback warning — only fires when both timestamps are known and
    // the candidate is older than current. Rolling back IS legitimate;
    // the warning just gives the operator a beat to confirm intent.
    if let (Some(cand), Some(curr)) = (
        candidate.and_then(|v| v.created),
        current.and_then(|v| v.created),
    ) {
        if cand < curr {
            let secs = curr.signed_duration_since(cand).num_seconds().max(0) as u32;
            let diff = if secs < 3600 {
                format!("{}m", secs / 60)
            } else if secs < 86_400 {
                format!("{}h", secs / 3600)
            } else {
                format!("{}d", secs / 86_400)
            };
            out.push('\n');
            out.push_str(&format!(
                "⚠ candidate is {diff} older than the currently-deployed version — \
                 looks like a rollback. Confirm intent.\n"
            ));
        }
    }
    out.push_str("\nrun :deploy without --preview to dispatch, or :versions for the full list.\n");
    out
}

pub fn format_app_versions(
    versions: &[crate::aws::AppVersion],
    deployed_label: Option<&str>,
    limit: usize,
) -> String {
    let mut out = String::new();
    let total = versions.len();
    let shown = total.min(limit);
    if total > limit {
        out.push_str(&format!(
            "showing {shown} of {total} (newest first; deploy older with `:deploy LABEL`)\n\n",
        ));
    }
    for v in versions.iter().take(limit) {
        // Drop the standard EB CI-pipeline prefix. The rest (usually a
        // pipeline URL) still distinguishes versions but consumes much less
        // horizontal width.
        let desc = v
            .description
            .strip_prefix("Application version created from ")
            .unwrap_or(&v.description);
        let marker = if deployed_label == Some(v.label.as_str()) {
            "▶ "
        } else {
            "  "
        };
        let suffix = if deployed_label == Some(v.label.as_str()) {
            "  ◀ deployed"
        } else {
            ""
        };
        if desc.is_empty() {
            out.push_str(&format!("{marker}{}{}\n", v.label, suffix));
        } else {
            out.push_str(&format!("{marker}{}  {desc}{}\n", v.label, suffix));
        }
    }
    out.push('\n');
    out.push_str("Use `:deploy <label>` to ship one to the selected env.");
    out
}

/// Map a friendly env-metric "kind" to a `(metric_name, default_op, default_stat)`
/// triple. The user can override the operator on the CLI but the defaults
/// reflect "what you'd reasonably alarm on for this metric" — e.g. drop in
/// health (LE) vs spike in 5xx (GT). Pure so the unit tests don't need
/// AWS.
pub fn alarm_kind_to_metric(kind: &str) -> Option<(&'static str, &'static str, &'static str)> {
    match kind {
        "health" => Some(("EnvironmentHealth", "LessThanOrEqualToThreshold", "Maximum")),
        "4xx" | "req4xx" => Some(("ApplicationRequests4xx", "GreaterThanThreshold", "Sum")),
        "5xx" | "req5xx" => Some(("ApplicationRequests5xx", "GreaterThanThreshold", "Sum")),
        "latency" | "p90" => Some(("ApplicationLatencyP90", "GreaterThanThreshold", "Average")),
        _ => None,
    }
}

/// Render a sorted `(namespace, option_name, value)` list as an aligned
/// text block grouped by namespace. Empty values render as `""` so the
/// reader can distinguish "explicitly empty" from "not present".
pub fn format_template_settings(settings: &[(String, String, String)]) -> String {
    if settings.is_empty() {
        return "(no option settings)".into();
    }
    let key_width = settings
        .iter()
        .map(|(_, name, _)| name.chars().count())
        .max()
        .unwrap_or(0)
        .clamp(16, 40);
    let mut out = String::new();
    let mut prev_ns: Option<&str> = None;
    for (ns, name, value) in settings {
        if Some(ns.as_str()) != prev_ns {
            if prev_ns.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("[{ns}]\n"));
            prev_ns = Some(ns.as_str());
        }
        let rendered = if value.is_empty() {
            "\"\"".to_string()
        } else {
            value.clone()
        };
        out.push_str(&format!("  {name:<key_width$} = {rendered}\n"));
    }
    out
}

/// Flatten the per-application configuration_templates lists into a single
/// `(application, template)` vector, sorted by app then by template name so
/// the overlay's cursor order is stable across refreshes. Pure so the unit
/// tests don't need an AWS client.
pub fn collect_saved_configs(apps: &[Application]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = apps
        .iter()
        .flat_map(|a| a.templates.iter().map(|t| (a.name.clone(), t.clone())))
        .collect();
    out.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    out
}

fn format_saved_configs(apps: &[Application]) -> String {
    if apps.is_empty() {
        return "no applications loaded — wait for first refresh or :region NAME".into();
    }
    let mut out = String::new();
    out.push_str("EB saved configurations (templates per application)\n");
    out.push_str("──────────────────────────────────────────────────\n\n");
    let mut any = false;
    for a in apps {
        if a.templates.is_empty() {
            continue;
        }
        any = true;
        out.push_str(&format!("Application: {}\n", a.name));
        for t in &a.templates {
            out.push_str(&format!("  ▸ {t}\n"));
        }
        out.push('\n');
    }
    if !any {
        out.push_str("no saved configuration templates in any application\n");
    }
    out
}

fn diff_envs(left: &Environment, right: &Environment, redact_on: bool) -> String {
    let cn = |s: &str| {
        if redact_on {
            redact_block(s)
        } else {
            s.to_string()
        }
    };
    let updated = |e: &Environment| {
        e.updated
            .map(|u| u.to_rfc3339())
            .unwrap_or_else(|| "—".into())
    };
    let rows: Vec<(&str, String, String)> = vec![
        ("Name", left.name.clone(), right.name.clone()),
        (
            "Application",
            left.application.clone(),
            right.application.clone(),
        ),
        ("Tier", left.tier.clone(), right.tier.clone()),
        ("Status", left.status.clone(), right.status.clone()),
        ("Health", left.health.clone(), right.health.clone()),
        ("Platform", left.platform.clone(), right.platform.clone()),
        (
            "Version",
            left.version_label.clone(),
            right.version_label.clone(),
        ),
        ("CNAME", cn(&left.cname), cn(&right.cname)),
        ("Updated", updated(left), updated(right)),
    ];

    // Width-aware truncation so long values don't blow out the popup.
    let width: usize = 28;
    let truncate = |s: &str| -> String {
        if s.chars().count() > width {
            let mut t: String = s.chars().take(width.saturating_sub(1)).collect();
            t.push('…');
            t
        } else {
            s.to_string()
        }
    };

    let left_label = truncate(&format!("◄ {}", left.name));
    let right_label = truncate(&format!("{} ►", right.name));
    let mut out = String::new();
    out.push_str(&format!(
        "{:<14}    {:<width$}    {}\n",
        "", left_label, right_label,
    ));
    out.push_str(&"─".repeat(14 + 4 + width + 4 + width));
    out.push('\n');
    for (field, l, r) in rows {
        let differs = l != r;
        let marker = if differs { "≠" } else { " " };
        out.push_str(&format!(
            "{marker} {:<12}  {:<width$}    {}\n",
            field,
            truncate(&l),
            truncate(&r),
        ));
    }
    out
}

fn format_alarms(result: Result<Vec<CwAlarm>, String>) -> String {
    match result {
        Err(e) => format!("error fetching alarms: {e}"),
        Ok(alarms) if alarms.is_empty() => "no CloudWatch alarms reference this env".into(),
        Ok(alarms) => {
            let mut out = String::new();
            out.push_str(&format!("CloudWatch alarms ({})\n", alarms.len()));
            out.push_str("──────────────────────────────────────────\n\n");
            for a in alarms {
                out.push_str(&format!(
                    "{:<10} {} ({}/{})\n",
                    a.state, a.name, a.namespace, a.metric_name,
                ));
                if !a.state_reason.is_empty() {
                    // Pre-wrap the reason at a conservative column width
                    // with a hanging indent so continuation lines stay
                    // aligned. Avoids ratatui's auto-wrap dropping to
                    // column 0 which looks broken.
                    let lead = "           ↳ ";
                    let cont = "             ";
                    out.push_str(&wrap_with_hanging_indent(&a.state_reason, 100, lead, cont));
                    out.push('\n');
                }
                out.push('\n');
            }
            out
        }
    }
}

/// Wrap `text` at `width` columns, prefixing the first line with `lead` and
/// subsequent lines with `cont` so continuation visually flows under the
/// leader (e.g. `"↳ "` followed by aligned continuation). Greedy
/// word-wrap; falls back to hard-break inside a word that won't fit on its
/// own line. Pure for testability.
pub fn wrap_with_hanging_indent(text: &str, width: usize, lead: &str, cont: &str) -> String {
    if text.is_empty() {
        return lead.to_string();
    }
    let body_width = width.saturating_sub(lead.chars().count()).max(1);
    let mut out = String::new();
    let mut first = true;
    let mut current = String::new();
    let prefix = |first: bool| if first { lead } else { cont };
    for word in text.split_whitespace() {
        // If a single word is longer than the body width, hard-break it.
        if word.chars().count() > body_width {
            if !current.is_empty() {
                out.push_str(prefix(first));
                out.push_str(&current);
                out.push('\n');
                first = false;
                current.clear();
            }
            let mut chars = word.chars();
            loop {
                let chunk: String = (&mut chars).take(body_width).collect();
                if chunk.is_empty() {
                    break;
                }
                out.push_str(prefix(first));
                out.push_str(&chunk);
                out.push('\n');
                first = false;
            }
            continue;
        }
        let candidate_len = if current.is_empty() {
            word.chars().count()
        } else {
            current.chars().count() + 1 + word.chars().count()
        };
        if candidate_len > body_width {
            out.push_str(prefix(first));
            out.push_str(&current);
            out.push('\n');
            first = false;
            current.clear();
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        out.push_str(prefix(first));
        out.push_str(&current);
        out.push('\n');
    }
    out.pop(); // remove trailing newline (caller adds its own)
    out
}

fn encode_view(app: &App) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !app.filter.is_empty() {
        parts.push(format!("filter={}", app.filter));
    }
    parts.push(format!(
        "sort={}:{}",
        app.sort_key.label(),
        if app.sort_desc { "desc" } else { "asc" }
    ));
    parts.push(format!("grouped={}", app.grouped));
    let scope = match app.scope {
        Scope::Envs => "envs",
        Scope::Apps => "apps",
    };
    parts.push(format!("scope={scope}"));
    parts.join(";")
}

fn apply_view(app: &mut App, snap: &str) {
    let mut new_filter = String::new();
    for part in snap.split(';') {
        let Some((k, v)) = part.split_once('=') else {
            continue;
        };
        match k.trim() {
            "filter" => new_filter = v.trim().to_string(),
            "sort" => {
                let (key, desc) = parse_sort(Some(v.trim()));
                app.sort_key = key;
                app.sort_desc = desc;
            }
            "grouped" => app.grouped = v.trim().eq_ignore_ascii_case("true"),
            "scope" => {
                app.scope = match v.trim() {
                    "apps" => Scope::Apps,
                    _ => Scope::Envs,
                };
            }
            _ => {}
        }
    }
    app.filter = new_filter;
    app.resort_envs(); // also rebuilds the view.
}

/// Best-effort hourly USD price for an EC2 instance type, on-demand Linux,
/// us-east-1 as the baseline. Returned in USD/hour. Returns None for unknown
/// types — caller should label the estimate as "approximate (us-east-1)".
pub fn instance_hourly_usd(instance_type: &str) -> Option<f64> {
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
pub fn estimate_cost(instances: &[Instance]) -> (f64, usize) {
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

fn shell_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
    {
        s.to_string()
    } else {
        // POSIX-safe single-quote: replace ' with '\'' and wrap.
        let escaped = s.replace('\'', "'\\''");
        format!("'{escaped}'")
    }
}

fn md_escape(s: &str) -> String {
    // Escape '|' (table separator) and backslash. Other Markdown specials are
    // safe inside a table cell.
    s.replace('\\', "\\\\").replace('|', "\\|")
}

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
    let detail = format!("stage=dispatched action={action:?} target={target}");
    write_audit_line(account, profile, region, &detail);
}

/// Log the outcome of a dispatched action. Called once the SDK response lands
/// so that the audit trail reflects what AWS actually did, not just what we
/// asked it to do.
fn write_audit_outcome(
    account: Option<&str>,
    profile: Option<&str>,
    region: &str,
    action: Action,
    env: &str,
    result: Result<(), &str>,
) {
    let outcome = match result {
        Ok(()) => "ok".to_string(),
        Err(e) => format!("err=\"{}\"", e.replace('"', "'")),
    };
    let detail = format!("stage=completed action={action:?} target={env} {outcome}");
    write_audit_line(account, profile, region, &detail);
}

/// Soft cap on `audit.log` size before we rotate to `audit.log.1` (single
/// historical backup, older history is discarded). 1 MiB ≈ ~5k action entries,
/// plenty for an interactive operator tool.
const AUDIT_LOG_MAX_BYTES: u64 = 1 << 20;

fn write_audit_line(account: Option<&str>, profile: Option<&str>, region: &str, detail: &str) {
    let dir = crate::util::cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("audit.log");
    rotate_if_oversize(&path, AUDIT_LOG_MAX_BYTES);
    let when = chrono::Utc::now().to_rfc3339();
    let line = format!(
        "{when}\taccount={}\tprofile={}\tregion={}\t{detail}\n",
        account.unwrap_or("-"),
        profile.unwrap_or("-"),
        region,
    );
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// If `path` exists and is larger than `max_bytes`, move it to `path.1`
/// (overwriting any previous backup) so the next write starts a fresh file.
/// Best-effort: any I/O error is swallowed — we don't want to lose the audit
/// entry just because rotation failed.
fn rotate_if_oversize(path: &std::path::Path, max_bytes: u64) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.len() <= max_bytes {
        return;
    }
    let backup = {
        let mut name = path
            .file_name()
            .map(|s| s.to_os_string())
            .unwrap_or_default();
        name.push(".1");
        path.with_file_name(name)
    };
    let _ = std::fs::rename(path, backup);
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

/// Rollup of operational signals across every env in an application.
/// Pure — driven entirely by the in-memory env list, so the Apps
/// table can refresh as part of the same view-rebuild that touches
/// the Envs table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AppRollup {
    pub env_count: usize,
    pub red_count: usize,
    pub updating_count: usize,
    pub worker_dlq_alerts: usize,
}

/// Compute the rollup for one application. Iterates `envs` once and
/// counts Red / Updating envs (case-insensitive on the health + status
/// columns). Worker-DLQ alerts come from `dlq_depths` which the App
/// owns globally — passed in so this stays a free fn that test code
/// can call without a full `App`.
pub fn app_rollup(
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

/// Pure: format the temp-file body the `:env-edit` flow opens
/// in `$EDITOR`. Header comment explains the contract (lines look
/// like `KEY=VALUE`; `#` comments and blank lines are ignored;
/// save+quit applies; quit-without-save / unchanged-body cancels).
/// Existing env vars are sorted alphabetically so the operator
/// gets a stable target for diffs across runs.
pub(crate) fn build_env_edit_body(env_name: &str, vars: &[(String, String)]) -> String {
    let mut out = String::new();
    out.push_str(&format!("# ebman env-var editor — {env_name}\n"));
    out.push_str("#\n");
    out.push_str("# Lines that look like KEY=VALUE are interpreted as env vars.\n");
    out.push_str("# Lines starting with # are comments.\n");
    out.push_str("# Blank lines are ignored.\n");
    out.push_str("#\n");
    out.push_str("# Save and quit to apply changes. Saving an unchanged file is a clean\n");
    out.push_str("# no-op. To reference a Secrets Manager value, store the ARN here\n");
    out.push_str("# (e.g. `DB_PASSWORD_SECRET_ARN=arn:aws:secretsmanager:...`) and have\n");
    out.push_str("# your app's bootstrap call GetSecretValue at runtime — EB does not\n");
    out.push_str("# resolve secretsmanager:// references natively.\n\n");
    let mut sorted: Vec<&(String, String)> = vars.iter().collect();
    sorted.sort_by(|a, b| a.0.cmp(&b.0));
    for (k, v) in sorted {
        out.push_str(&format!("{k}={v}\n"));
    }
    out
}

/// Pure: parse the operator's edited `:env-edit` body back into a
/// `KEY -> VALUE` map. Splits each non-comment line on the *first*
/// `=` so values containing `=` (common for query-string-style
/// settings or base64-encoded secrets) pass through intact.
/// Keys that fail to validate (empty after trim, contain whitespace)
/// are dropped — EB's option-settings API would reject them anyway,
/// and the operator gets the diff feedback after save.
pub(crate) fn parse_env_edit_body(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for raw in text.lines() {
        let trimmed = raw.trim_start();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() || key.chars().any(char::is_whitespace) {
            continue;
        }
        // Trailing whitespace + a single optional carriage return
        // (Windows line endings) get stripped from the value, but
        // intentional internal whitespace is preserved.
        let value = value.trim_end_matches('\r').trim_end_matches('\n');
        out.insert(key.to_string(), value.to_string());
    }
    out
}

/// Pure: produce `(to_set, to_remove)` deltas from two env-var
/// snapshots. `to_set` carries the EB option-settings triple
/// `(namespace, name, value)`; `to_remove` carries `(namespace,
/// name)`. Caller-supplied namespace because the same shape is
/// reusable beyond `aws:elasticbeanstalk:application:environment`
/// (e.g. future `:options-edit` could feed any namespace).
/// `(namespace, key, value)` triples — the shape EB's option-settings
/// update API expects for "set these". Aliased so [`diff_env_vars`]'s
/// signature isn't tripping the complex-type clippy lint.
pub(crate) type OptionSet = Vec<(String, String, String)>;
/// `(namespace, key)` pairs — "remove these" shape.
pub(crate) type OptionRemove = Vec<(String, String)>;

pub(crate) fn diff_env_vars(
    namespace: &str,
    original: &std::collections::BTreeMap<String, String>,
    edited: &std::collections::BTreeMap<String, String>,
) -> (OptionSet, OptionRemove) {
    let mut to_set: OptionSet = Vec::new();
    let mut to_remove: OptionRemove = Vec::new();
    // Set or update: any key present in `edited` whose value
    // differs from the original (or was missing entirely).
    for (k, v) in edited {
        match original.get(k) {
            Some(prev) if prev == v => continue,
            _ => to_set.push((namespace.to_string(), k.clone(), v.clone())),
        }
    }
    // Remove: keys present in `original` but absent from `edited`.
    for k in original.keys() {
        if !edited.contains_key(k) {
            to_remove.push((namespace.to_string(), k.clone()));
        }
    }
    (to_set, to_remove)
}

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

/// Pure: render the result of an IAM `SimulatePrincipalPolicy`
/// call. One section per evaluated action, with the decision +
/// matched statements + SCP / boundary blockers + a concrete
/// suggestion of what policy statement to add when the decision
/// was implicitDeny.
pub(crate) fn render_explain_overlay(principal: &str, rows: &[crate::aws::IamSimResult]) -> String {
    let mut out = String::new();
    out.push_str(&format!("IAM diagnosis for {principal}\n"));
    out.push_str("═══════════════════════════════════════════════════\n\n");
    if rows.is_empty() {
        out.push_str("(no evaluation results returned)\n\nesc / q to close");
        return out;
    }
    for (idx, r) in rows.iter().enumerate() {
        if idx > 0 {
            out.push('\n');
        }
        out.push_str(&format!("Action:   {}\n", r.action));
        if !r.resource.is_empty() {
            out.push_str(&format!("Resource: {}\n", r.resource));
        }
        let (mark, label) = match r.decision.as_str() {
            "allowed" => ("✓", "allowed"),
            "explicitDeny" => ("✗", "explicitDeny — a policy *denies* this action"),
            "implicitDeny" => ("✗", "implicitDeny — no policy allows this action"),
            other => ("?", other),
        };
        out.push_str(&format!("Decision: {mark} {label}\n"));
        if r.blocked_by_scp {
            out.push_str("          ⚠ also blocked by an Organizations SCP at the org level\n");
        }
        if r.blocked_by_boundary {
            out.push_str("          ⚠ also blocked by the role's permission boundary\n");
        }
        if !r.matched_statements.is_empty() {
            out.push_str("Matched statements:\n");
            for s in &r.matched_statements {
                out.push_str(&format!("  ▸ {s}\n"));
            }
        }
        if !r.missing_context.is_empty() {
            out.push_str("Missing context keys (conditions unsatisfied):\n");
            for c in &r.missing_context {
                out.push_str(&format!("  ▸ {c}\n"));
            }
        }
        if r.decision == "implicitDeny" {
            out.push_str(&format!(
                "\nTo allow, add this statement to one of the role's policies:\n\
                 \n\
                 {{\n\
                 \x20\x20\"Effect\": \"Allow\",\n\
                 \x20\x20\"Action\": \"{}\",\n\
                 \x20\x20\"Resource\": \"*\"\n\
                 }}\n",
                r.action
            ));
        } else if r.decision == "explicitDeny" {
            out.push_str(
                "\nAn explicit Deny in the matched statement(s) above is\n\
                 overriding any Allow. Remove or scope down the Deny to\n\
                 unblock — explicit Deny always wins.\n",
            );
        }
    }
    out.push_str("\nesc / q to close");
    out
}

/// Pure: render the env's underlying AWS resources as a tree.
/// Replaces the previous flat-section dump. The hierarchy mirrors
/// the conceptual graph an operator builds in their head:
///
///   env  (Tier)
///   ├─ ASGs
///   │  └─ <asg-name>
///   │     ├─ <instance-id>
///   │     └─ <instance-id>
///   ├─ Launch template / config
///   ├─ Load balancers
///   ├─ Triggers
///   └─ Queues  (Worker only)
///      ├─ WorkerQueue
///      │     https://sqs.../...
///      └─ WorkerDeadLetterQueue
///            https://sqs.../...
///
/// Instances are nested under ASGs because EB envs typically have
/// one ASG that owns every instance. The first ASG in the list
/// carries the instance children; if the env has zero ASGs but
/// non-zero instances (rare; mid-launch maybe), those instances
/// surface as a separate "orphan" section.
pub(crate) fn render_env_resources_tree(
    res: &crate::aws::EnvResources,
    env_name: &str,
    tier: &str,
) -> String {
    let mut out = String::new();
    out.push_str(&format!("Resources for {env_name}  ({tier})\n"));
    out.push_str("═══════════════════════════════════════\n\n");

    // Collect non-empty sections first. The last kept section
    // uses `└─`; the rest `├─`. Easier to track once we know how
    // many sections survive than to count inline.
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();

    if !res.asgs.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n_asgs = res.asgs.len();
        for (asg_idx, asg) in res.asgs.iter().enumerate() {
            let last_asg = asg_idx + 1 == n_asgs;
            let asg_prefix = if last_asg { "└─" } else { "├─" };
            lines.push(format!("  {asg_prefix} {asg}"));
            // Only the first ASG carries the instance children
            // (typical case: one ASG per env).
            if asg_idx == 0 && !res.instances.is_empty() {
                let n_inst = res.instances.len();
                let cont = if last_asg { "  " } else { "│ " };
                for (i, id) in res.instances.iter().enumerate() {
                    let last_inst = i + 1 == n_inst;
                    let glyph = if last_inst { "└─" } else { "├─" };
                    lines.push(format!("  {cont}   {glyph} {id}"));
                }
            }
        }
        sections.push((format!("Auto-scaling groups ({})", res.asgs.len()), lines));
    } else if !res.instances.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.instances.len();
        for (i, id) in res.instances.iter().enumerate() {
            let last = i + 1 == n;
            let glyph = if last { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {id}"));
        }
        sections.push((format!("Instances ({n}) — orphan (no ASG attached)"), lines));
    }

    if !res.launch_templates.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.launch_templates.len();
        for (i, t) in res.launch_templates.iter().enumerate() {
            let glyph = if i + 1 == n { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {t}"));
        }
        sections.push((format!("Launch templates ({n})"), lines));
    }
    if !res.launch_configs.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.launch_configs.len();
        for (i, lc) in res.launch_configs.iter().enumerate() {
            let glyph = if i + 1 == n { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {lc}"));
        }
        sections.push((format!("Launch configurations ({n})"), lines));
    }
    if !res.load_balancers.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.load_balancers.len();
        for (i, lb) in res.load_balancers.iter().enumerate() {
            let glyph = if i + 1 == n { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {lb}"));
        }
        sections.push((format!("Load balancers ({n})"), lines));
    }
    if !res.triggers.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.triggers.len();
        for (i, t) in res.triggers.iter().enumerate() {
            let glyph = if i + 1 == n { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {t}"));
        }
        sections.push((format!("Triggers ({n})"), lines));
    }
    if !res.queues.is_empty() {
        let mut lines: Vec<String> = Vec::new();
        let n = res.queues.len();
        for (i, q) in res.queues.iter().enumerate() {
            let last = i + 1 == n;
            let glyph = if last { "└─" } else { "├─" };
            lines.push(format!("  {glyph} {}", q.name));
            if !q.url.is_empty() {
                let url_prefix = if last { "       " } else { "  │    " };
                lines.push(format!("{url_prefix}{}", q.url));
            }
        }
        sections.push((format!("Queues ({n})"), lines));
    }

    if sections.is_empty() {
        out.push_str("  (no resources reported — env may still be launching)\n");
    } else {
        let n_sections = sections.len();
        for (idx, (label, lines)) in sections.iter().enumerate() {
            let last_section = idx + 1 == n_sections;
            let section_glyph = if last_section { "└─" } else { "├─" };
            out.push_str(&format!("{section_glyph} {label}\n"));
            let prefix = if last_section { "  " } else { "│ " };
            for line in lines {
                out.push_str(&format!("{prefix}{line}\n"));
            }
            if !last_section {
                out.push_str("│\n");
            }
        }
    }

    out.push_str("\nesc / q to close");
    out
}

/// Pure: edit (Levenshtein) distance between two strings, counting
/// single-character insertions / deletions / substitutions. Used by
/// the unknown-command `did-you-mean` path; small enough that
/// pulling in the `strsim` crate would be over-spec.
///
/// Implemented as the standard O(m·n) DP table with byte-level
/// iteration. ASCII-only paths get exact answers; multi-byte
/// UTF-8 still terminates but the distance is counted in bytes,
/// not graphemes. Acceptable for the command-name use case
/// (every built-in is ASCII).
pub(crate) fn edit_distance(a: &str, b: &str) -> usize {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.is_empty() {
        return b_bytes.len();
    }
    if b_bytes.is_empty() {
        return a_bytes.len();
    }
    // Two-row rolling DP: only the previous row's distances are
    // needed to compute the current row. Saves O(m·n) memory →
    // O(min(m,n)) without changing the answer.
    let (short, long) = if a_bytes.len() < b_bytes.len() {
        (a_bytes, b_bytes)
    } else {
        (b_bytes, a_bytes)
    };
    let mut prev: Vec<usize> = (0..=short.len()).collect();
    let mut curr: Vec<usize> = vec![0; short.len() + 1];
    for (i, lc) in long.iter().enumerate() {
        curr[0] = i + 1;
        for (j, sc) in short.iter().enumerate() {
            let cost = if lc == sc { 0 } else { 1 };
            curr[j + 1] = (prev[j + 1] + 1).min(curr[j] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[short.len()]
}

/// Suggest the closest registry name to `input` within an
/// edit-distance threshold. Returns `None` when no candidate is
/// close enough — a wild guess would mislead rather than help.
///
/// Threshold is length-dependent: short inputs (`:q`, `:r`) get
/// distance ≤ 1; longer ones tolerate up to 2 typos. The
/// length-aware threshold prevents a 2-char miss like `:xy`
/// from "matching" every 3-char name in the registry.
pub(crate) fn suggest_command(input: &str) -> Option<String> {
    let threshold = if input.len() <= 3 { 1 } else { 2 };
    let mut best: Option<(usize, String)> = None;
    for name in crate::commands::all_names() {
        let d = edit_distance(input, name);
        if d <= threshold && best.as_ref().is_none_or(|(bd, _)| d < *bd) {
            best = Some((d, name.to_string()));
        }
    }
    best.map(|(_, name)| name)
}

/// Pure: return the built-in command names + aliases that begin
/// with `prefix`. Sorted alphabetically. Empty prefix returns every
/// name (still alpha-sorted). De-duplicated so a command's
/// canonical name and any aliases don't both surface for the same
/// dispatch arm — first occurrence wins.
///
/// Used by command-mode Tab cycling. Plugins (`commands.toml`) are
/// not included here because plugins are operator-specific and
/// can change without a registry update; future enhancement could
/// merge them in but the registry-driven first cut keeps the
/// behaviour predictable.
pub(crate) fn completion_candidates(prefix: &str) -> Vec<String> {
    let mut names: Vec<String> = crate::commands::all_names()
        .into_iter()
        .filter(|n| n.starts_with(prefix))
        .map(String::from)
        .collect();
    names.sort();
    names.dedup();
    names
}

/// Pure render of the `:options` overlay body. Groups `rows` by
/// namespace; within each group, operator-set rows come first
/// (marked `▸`), defaults follow (marked `•`). Optional
/// `filter_ns` restricts to one namespace.
///
/// Format per row:
///   `<marker> NAME[<padding>]  = VALUE       (default: X, type: T, ...)`
///
/// The metadata trailer (`default:`, `type:`, `severity:`, ranges,
/// value_options) only renders when the field is set — keeps the
/// line lean. Long value-option lists get truncated to "first 5 +
/// …" to avoid one option blowing past the popup width.
///
/// Top of the body carries a one-line legend so the operator
/// doesn't have to learn the marker convention from `?`.
pub(crate) fn render_options_overlay(
    rows: &[crate::aws::ConfigOption],
    filter_ns: Option<&str>,
    env_name: &str,
) -> String {
    let filtered: Vec<&crate::aws::ConfigOption> = rows
        .iter()
        .filter(|r| filter_ns.is_none_or(|ns| r.namespace == ns))
        .collect();
    if filtered.is_empty() {
        return match filter_ns {
            Some(ns) => format!(
                "No options found for namespace '{ns}' on env '{env_name}'.\n\n\
                 Spelling? Try `:options` (no arg) to see the full list of\n\
                 namespaces available for this env's platform.\n\n\
                 esc / q to close"
            ),
            None => format!(
                "No configuration options returned for env '{env_name}'.\n\n\
                 This usually means the env's platform doesn't expose an option\n\
                 vocabulary (custom platform or stale solution-stack). Try\n\
                 `:set-option` directly if you know what you want to change.\n\n\
                 esc / q to close"
            ),
        };
    }
    // Compute the longest name within each namespace so the `= value`
    // columns line up per group. Walking once first; second pass renders.
    let mut max_name_per_ns: std::collections::HashMap<&str, usize> =
        std::collections::HashMap::new();
    for r in &filtered {
        let e = max_name_per_ns.entry(r.namespace.as_str()).or_insert(0);
        *e = (*e).max(r.name.chars().count()).min(38);
    }

    let user_set = filtered.iter().filter(|r| r.value.is_some()).count();
    let mut body = String::new();
    body.push_str(&format!(
        "Configuration vocabulary for {env_name}\n\
         {user_set}/{total} options are operator-set; the rest are at default.\n\n\
         ▸ = operator-set    • = default    severity warns when changing rolls instances\n\n",
        total = filtered.len()
    ));

    let mut current_ns: Option<&str> = None;
    for r in &filtered {
        if Some(r.namespace.as_str()) != current_ns {
            if current_ns.is_some() {
                body.push('\n');
            }
            body.push_str(&format!("── {} ──\n", r.namespace));
            current_ns = Some(r.namespace.as_str());
        }
        let marker = if r.value.is_some() { "▸" } else { "•" };
        let name_width = max_name_per_ns
            .get(r.namespace.as_str())
            .copied()
            .unwrap_or(20);
        let name_padded = if r.name.chars().count() < name_width {
            format!("{name:<width$}", name = r.name, width = name_width)
        } else {
            r.name.clone()
        };
        let value_str = match &r.value {
            Some(v) => format!(" = {v}"),
            None => String::new(),
        };
        // Trailing metadata — only emit what's set so short-form
        // rows stay short.
        let mut meta: Vec<String> = Vec::new();
        if let Some(d) = &r.default_value {
            if !d.is_empty() {
                meta.push(format!("default: {d}"));
            }
        }
        if !r.value_type.is_empty() && r.value_type != "Scalar" {
            // Scalar is the default; only call out non-scalars
            // (`List`) which surprise the operator.
            meta.push(format!("type: {}", r.value_type));
        }
        if let Some(s) = &r.change_severity {
            if s != "NoInterruption" && s != "Unknown" {
                meta.push(format!("severity: {s}"));
            }
        }
        match (r.min_value, r.max_value) {
            (Some(min), Some(max)) => meta.push(format!("range: {min}-{max}")),
            (Some(min), None) => meta.push(format!("min: {min}")),
            (None, Some(max)) => meta.push(format!("max: {max}")),
            (None, None) => {}
        }
        if let Some(maxlen) = r.max_length {
            meta.push(format!("max_len: {maxlen}"));
        }
        if !r.value_options.is_empty() {
            let preview: Vec<&str> = r.value_options.iter().take(5).map(String::as_str).collect();
            let more = r.value_options.len().saturating_sub(5);
            let suffix = if more > 0 {
                format!(", … +{more}")
            } else {
                String::new()
            };
            meta.push(format!("oneof: {}{suffix}", preview.join(", ")));
        }
        let meta_str = if meta.is_empty() {
            String::new()
        } else {
            format!("  ({})", meta.join(", "))
        };
        body.push_str(&format!("  {marker} {name_padded}{value_str}{meta_str}\n"));
    }
    body.push_str(
        "\n`:set-option NAMESPACE NAME VALUE` to change a setting.\n\
         `:options NAMESPACE` to filter to one family.\n\
         esc / q to close",
    );
    body
}

/// Render the `:secrets` overlay — metadata only, never values.
/// Pure (takes the SDK rows + filter, returns the body string) so
/// the table layout / empty-state copy can be unit-tested without
/// hitting Secrets Manager.
pub(crate) fn render_secrets_overlay(
    rows: &[crate::aws::SecretSummary],
    filter: Option<&str>,
) -> String {
    if rows.is_empty() {
        return match filter {
            Some(f) => format!(
                "No secrets matching '{f}'.\n\n\
                 `:secrets` (no arg) to see everything in this region.\n\
                 Secrets Manager is region-scoped — switch with `:region` first if needed.\n\n\
                 esc / q to close"
            ),
            None => "No Secrets Manager secrets in this region.\n\n\
                 Either none have been created, or the caller is missing\n\
                 `secretsmanager:ListSecrets`. Try `:explain :secrets` to check.\n\n\
                 esc / q to close"
                .to_string(),
        };
    }
    let now = chrono::Utc::now();
    let mut body = String::new();
    body.push_str(&match filter {
        Some(f) => format!(
            "Secrets Manager — {n} matching '{f}'\n\
             Sorted by last-changed (newest first). Values not shown — use `:secret NAME`.\n\n",
            n = rows.len()
        ),
        None => format!(
            "Secrets Manager — {n} secrets\n\
             Sorted by last-changed (newest first). Values not shown — use `:secret NAME`.\n\n",
            n = rows.len()
        ),
    });
    for r in rows {
        body.push_str(&format!("▸ {}\n", r.name));
        if !r.arn.is_empty() {
            body.push_str(&format!("    arn: {}\n", r.arn));
        }
        if let Some(d) = &r.description {
            body.push_str(&format!("    desc: {d}\n"));
        }
        let changed = r.last_changed.map(|t| format_age(now, t));
        let rotated = r.last_rotated.map(|t| format_age(now, t));
        match (changed, rotated) {
            (Some(c), Some(r)) => {
                body.push_str(&format!("    changed: {c}    rotated: {r}\n"));
            }
            (Some(c), None) => {
                body.push_str(&format!("    changed: {c}    rotated: never\n"));
            }
            (None, Some(r)) => {
                body.push_str(&format!("    rotated: {r}\n"));
            }
            (None, None) => {}
        }
        if let Some(k) = &r.kms_key_id {
            body.push_str(&format!("    kms: {k}\n"));
        }
        body.push('\n');
    }
    body.push_str(
        "y to yank an ARN (select first) · `:secret NAME` to read the value\n\
         esc / q to close",
    );
    body
}

/// Render the `:secret NAME` overlay — the single-secret detail view.
/// Honours `redact` mode by replacing the value with a length + sha
/// hint, so an operator on a screen-share can confirm "yes I have
/// the right secret" without exposing it. JSON-shaped values are
/// pretty-printed for readability (Secrets Manager's common k/v
/// idiom is `{"USERNAME":"…","PASSWORD":"…"}`).
pub(crate) fn render_secret_value_overlay(name: &str, value: &str, redact: bool) -> String {
    let mut body = String::new();
    body.push_str(&format!("Secret — {name}\n\n"));
    if redact {
        body.push_str(&format!(
            "value: <redacted; {} chars, fingerprint {}>\n\
             Run `:redact off` then re-fetch if you need the cleartext.\n\n\
             esc / q to close",
            value.chars().count(),
            short_fingerprint(value),
        ));
        return body;
    }
    // Try to pretty-print JSON so k/v secrets are scannable.
    let pretty = try_pretty_json(value);
    body.push_str("value:\n");
    body.push_str(&pretty);
    if !pretty.ends_with('\n') {
        body.push('\n');
    }
    body.push_str("\ny to yank the value · esc / q to close");
    body
}

/// FNV-1a 32-bit fingerprint of the value, hex-encoded — short,
/// dependency-free, good enough to confirm "same secret as before"
/// without leaking the value itself. NOT a cryptographic hash and
/// not used for security decisions; only for the redact-mode
/// "is this the right one" eyeball check.
fn short_fingerprint(s: &str) -> String {
    let mut h: u32 = 0x811C_9DC5;
    for b in s.as_bytes() {
        h ^= *b as u32;
        h = h.wrapping_mul(0x0100_0193);
    }
    format!("{h:08x}")
}

/// If the value parses as JSON, return a pretty-printed form;
/// otherwise return the raw string. Uses a very minimal recursive
/// parser instead of pulling in `serde_json` for one render path.
fn try_pretty_json(s: &str) -> String {
    let trimmed = s.trim();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return s.to_string();
    }
    // Minimal pass: walk chars, indenting on { [ and dedenting on } ].
    // Quoted strings are preserved verbatim. This handles the
    // Secrets-Manager k/v idiom without taking a hard JSON dep.
    let mut out = String::with_capacity(s.len() + 32);
    let mut depth: usize = 0;
    let mut in_str = false;
    let mut escape = false;
    let mut chars = trimmed.chars().peekable();
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if escape {
                escape = false;
            } else if c == '\\' {
                escape = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '{' | '[' => {
                out.push(c);
                // Empty container? Don't add a newline.
                if matches!(chars.peek(), Some('}') | Some(']')) {
                    continue;
                }
                depth += 1;
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
                out.push(c);
            }
            ',' => {
                out.push(c);
                out.push('\n');
                out.push_str(&"  ".repeat(depth));
            }
            ':' => {
                out.push(c);
                out.push(' ');
            }
            ' ' | '\n' | '\t' | '\r' => {} // collapse whitespace outside strings
            _ => out.push(c),
        }
    }
    out
}

/// Format an "age" against now. Pure; keeps the secrets renderer
/// from depending on ui.rs's private `humanize_age`.
fn format_age(now: chrono::DateTime<chrono::Utc>, t: chrono::DateTime<chrono::Utc>) -> String {
    let d = now.signed_duration_since(t);
    let secs = d.num_seconds().max(0);
    if secs < 60 {
        return format!("{secs}s ago");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m ago");
    }
    let hrs = mins / 60;
    if hrs < 48 {
        return format!("{hrs}h ago");
    }
    let days = hrs / 24;
    if days < 60 {
        return format!("{days}d ago");
    }
    let months = days / 30;
    if months < 24 {
        return format!("~{months}mo ago");
    }
    format!("~{}y ago", days / 365)
}

fn console_url(region: &str, app_name: &str, env_name: &str) -> String {
    let app = urlencode(app_name);
    let env = urlencode(env_name);
    format!(
        "https://{region}.console.aws.amazon.com/elasticbeanstalk/home?region={region}#/environment/dashboard?applicationName={app}&environmentName={env}"
    )
}

fn urlencode(s: &str) -> String {
    // Minimal URL-encode of the characters that appear in EB app / env names.
    // EB names are restricted to a–z A–Z 0–9 - _ so most input passes through;
    // we still encode space and any non-ASCII for safety.
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
            out.push(c);
        } else {
            for b in c.to_string().bytes() {
                out.push_str(&format!("%{b:02X}"));
            }
        }
    }
    out
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

fn describe_env(e: &Environment) -> String {
    let updated = e
        .updated
        .map(|u| u.to_rfc3339())
        .unwrap_or_else(|| "null".into());
    format!(
        "{{\n  \"name\":            \"{}\",\n  \"application\":     \"{}\",\n  \"tier\":            \"{}\",\n  \"status\":          \"{}\",\n  \"health\":          \"{}\",\n  \"platform\":        \"{}\",\n  \"version_label\":   \"{}\",\n  \"cname\":           \"{}\",\n  \"updated\":         {}\n}}",
        json_escape(&e.name),
        json_escape(&e.application),
        json_escape(&e.tier),
        json_escape(&e.status),
        json_escape(&e.health),
        json_escape(&e.platform),
        json_escape(&e.version_label),
        json_escape(&e.cname),
        if updated == "null" { updated } else { format!("\"{updated}\"") },
    )
}

fn json_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn redact_block(value: &str) -> String {
    if value.is_empty() {
        return value.to_string();
    }
    "▓".repeat(value.chars().count())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loading_linger_target_none_when_no_load() {
        let now = Instant::now();
        assert!(compute_loading_linger_target(
            None,
            Duration::from_millis(300),
            Duration::from_millis(500),
            now,
        )
        .is_none());
    }

    #[test]
    fn loading_linger_target_none_when_under_threshold() {
        let now = Instant::now();
        // Load started 100 ms ago — threshold (300 ms) not crossed.
        let started = now - Duration::from_millis(100);
        assert!(compute_loading_linger_target(
            Some(started),
            Duration::from_millis(300),
            Duration::from_millis(500),
            now,
        )
        .is_none());
    }

    #[test]
    fn loading_linger_target_arms_past_threshold() {
        let now = Instant::now();
        let started = now - Duration::from_millis(400);
        let until = compute_loading_linger_target(
            Some(started),
            Duration::from_millis(300),
            Duration::from_millis(500),
            now,
        )
        .expect("should arm linger past threshold");
        // Linger should extend ~500 ms past `now`. Allow a tiny slop so the
        // assertion isn't sensitive to test runner clock granularity.
        let target_delta = until.duration_since(now);
        assert!(
            target_delta >= Duration::from_millis(495)
                && target_delta <= Duration::from_millis(505),
            "linger target should be ~500ms in the future, got {target_delta:?}"
        );
    }

    #[test]
    fn sort_key_cycle_matches_ui_column_order() {
        let order = [
            SortKey::Name,
            SortKey::App,
            SortKey::Status,
            SortKey::Health,
            SortKey::Version,
            SortKey::Age,
        ];
        let mut cur = order[0];
        for expected in order.iter().skip(1).chain(std::iter::once(&order[0])) {
            cur = cur.next();
            assert_eq!(cur, *expected);
        }
    }

    #[test]
    fn sort_key_parse_roundtrip() {
        for k in [
            SortKey::Name,
            SortKey::App,
            SortKey::Status,
            SortKey::Health,
            SortKey::Version,
            SortKey::Age,
        ] {
            assert_eq!(SortKey::parse(k.label()), Some(k));
        }
        assert_eq!(SortKey::parse("bogus"), None);
    }

    #[test]
    fn parse_sort_handles_directions() {
        assert_eq!(parse_sort(Some("app:desc")), (SortKey::App, true));
        assert_eq!(parse_sort(Some("name:asc")), (SortKey::Name, false));
        assert_eq!(parse_sort(Some("name")), (SortKey::Name, false));
        assert_eq!(parse_sort(Some("bogus:desc")), (SortKey::App, true)); // unknown key → default key, dir kept
        assert_eq!(parse_sort(None), (SortKey::App, false));
    }

    #[test]
    fn parse_toggle_explicit_and_default() {
        assert!(parse_toggle(Some("on"), false));
        assert!(parse_toggle(Some("yes"), false));
        assert!(parse_toggle(Some("1"), false));
        assert!(!parse_toggle(Some("off"), true));
        assert!(!parse_toggle(Some("no"), true));
        // No arg → toggle current.
        assert!(parse_toggle(None, false));
        assert!(!parse_toggle(None, true));
        // Garbage → toggle current.
        assert!(parse_toggle(Some("maybe"), false));
    }

    #[test]
    fn health_rank_orders_severities() {
        assert!(health_rank("green") < health_rank("grey"));
        assert!(health_rank("grey") < health_rank("yellow"));
        assert!(health_rank("yellow") < health_rank("red"));
        assert_eq!(health_rank("ok"), health_rank("Green"));
    }

    #[test]
    fn scroll_apply_clamps_at_zero() {
        assert_eq!(scroll_apply(0, -1), 0);
        assert_eq!(scroll_apply(0, 0), 0);
        assert_eq!(scroll_apply(0, 1), 1);
        assert_eq!(scroll_apply(5, -10), 0);
        assert_eq!(scroll_apply(5, 3), 8);
    }

    #[test]
    fn redact_block_preserves_length() {
        assert_eq!(redact_block(""), "");
        assert_eq!(redact_block("hello").chars().count(), 5);
        assert_eq!(redact_block("über-café").chars().count(), 9);
    }

    #[test]
    fn scope_next_alternates() {
        assert_eq!(Scope::Envs.next(), Scope::Apps);
        assert_eq!(Scope::Apps.next(), Scope::Envs);
    }

    #[test]
    fn action_destructive_only_for_terminate() {
        assert!(Action::Terminate.destructive());
        assert!(!Action::Rebuild.destructive());
        assert!(!Action::RestartAppServer.destructive());
        assert!(!Action::SwapCnames.destructive());
    }

    #[test]
    fn scope_prev_is_inverse_of_next() {
        assert_eq!(Scope::Envs.next(), Scope::Apps);
        assert_eq!(Scope::Envs.prev(), Scope::Apps);
        assert_eq!(Scope::Apps.next().next(), Scope::Apps);
        assert_eq!(Scope::Envs.prev().prev(), Scope::Envs);
    }

    #[test]
    fn view_mode_labels() {
        assert_eq!(ViewMode::Default.label(), "default");
        assert_eq!(ViewMode::Compact.label(), "compact");
        assert_eq!(ViewMode::Spacious.label(), "spacious");
    }

    #[test]
    fn console_url_includes_region_app_env() {
        let url = console_url("us-east-1", "myapp", "myenv");
        assert!(url.contains("us-east-1.console.aws.amazon.com"));
        assert!(url.contains("region=us-east-1"));
        assert!(url.contains("applicationName=myapp"));
        assert!(url.contains("environmentName=myenv"));
    }

    #[test]
    fn console_url_encodes_special_chars() {
        // Reserved or non-alnum chars get %XX'd so the URL stays valid.
        let url = console_url("us-east-1", "my app", "env/with?slash");
        assert!(url.contains("applicationName=my%20app"));
        assert!(url.contains("environmentName=env%2Fwith%3Fslash"));
    }

    #[test]
    fn urlencode_keeps_safe_chars() {
        assert_eq!(urlencode("hello-world_1.0"), "hello-world_1.0");
        assert_eq!(urlencode("a b"), "a%20b");
        assert_eq!(urlencode("a/b?c=d"), "a%2Fb%3Fc%3Dd");
        // Unicode is byte-wise percent-encoded.
        assert!(urlencode("café").starts_with("caf"));
    }

    #[test]
    fn json_escape_handles_quotes_and_controls() {
        assert_eq!(json_escape("hello"), "hello");
        assert_eq!(json_escape(r#"he said "hi""#), r#"he said \"hi\""#);
        assert_eq!(json_escape("line\nbreak"), "line\\nbreak");
        assert_eq!(json_escape("\\path"), "\\\\path");
        // Control character → \uXXXX.
        let out = json_escape("\u{0001}");
        assert_eq!(out, "\\u0001");
    }

    #[test]
    fn build_describe_cli_no_profile() {
        let cmd = build_describe_cli("my-env", "eu-west-2", None);
        assert_eq!(
            cmd,
            "aws elasticbeanstalk describe-environments --environment-names my-env --region eu-west-2"
        );
    }

    #[test]
    fn build_describe_cli_with_profile_and_special_chars() {
        let cmd = build_describe_cli("my env!", "eu-west-2", Some("prod"));
        assert!(cmd.contains("--environment-names 'my env!'"));
        assert!(cmd.contains("--profile prod"));
    }

    fn fake_env_with(
        name: &str,
        status: &str,
        health: &str,
        updated_minutes_ago: Option<i64>,
    ) -> Environment {
        let updated =
            updated_minutes_ago.map(|m| chrono::Utc::now() - chrono::Duration::minutes(m));
        Environment {
            name: name.into(),
            application: "app".into(),
            status: status.into(),
            health: health.into(),
            platform: "Java 17".into(),
            tier: "Web".into(),
            cname: "x.elb".into(),
            version_label: "v1".into(),
            arn: None,
            updated,
            id: None,
            region: None,
        }
    }

    #[test]
    fn app_rollup_counts_envs_red_and_updating() {
        let envs = vec![
            crate::aws::Environment {
                name: "prod".into(),
                application: "foo".into(),
                status: "Ready".into(),
                health: "Green".into(),
                platform: "Java 17".into(),
                tier: "WebServer".into(),
                cname: String::new(),
                version_label: String::new(),
                arn: None,
                updated: None,
                id: None,
                region: None,
            },
            crate::aws::Environment {
                name: "staging".into(),
                application: "foo".into(),
                status: "Updating".into(),
                health: "Red".into(),
                platform: "Java 17".into(),
                tier: "WebServer".into(),
                cname: String::new(),
                version_label: String::new(),
                arn: None,
                updated: None,
                id: None,
                region: None,
            },
            crate::aws::Environment {
                name: "other-app".into(),
                application: "bar".into(),
                status: "Ready".into(),
                health: "Green".into(),
                platform: "Java 17".into(),
                tier: "WebServer".into(),
                cname: String::new(),
                version_label: String::new(),
                arn: None,
                updated: None,
                id: None,
                region: None,
            },
        ];
        let dlq: HashMap<String, i64> = HashMap::new();
        let r = super::app_rollup(&envs, "foo", &dlq);
        assert_eq!(r.env_count, 2, "foo has 2 envs (prod + staging)");
        assert_eq!(r.red_count, 1, "staging is Red");
        assert_eq!(r.updating_count, 1, "staging is Updating");
        assert_eq!(r.worker_dlq_alerts, 0, "no worker envs in foo");
    }

    #[test]
    fn app_rollup_worker_dlq_alert_counts() {
        let envs = vec![crate::aws::Environment {
            name: "worker-prod".into(),
            application: "wapp".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: "Java 17".into(),
            tier: "Worker".into(),
            cname: String::new(),
            version_label: String::new(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        }];
        let mut dlq: HashMap<String, i64> = HashMap::new();
        dlq.insert("worker-prod".into(), 7);
        let r = super::app_rollup(&envs, "wapp", &dlq);
        // EB calls it Green; ebman flags it because the DLQ is non-empty.
        assert_eq!(r.env_count, 1);
        assert_eq!(r.red_count, 0, "EB health stays Green");
        assert_eq!(
            r.worker_dlq_alerts, 1,
            "worker env with DLQ depth > 0 counts as alerting"
        );
    }

    #[test]
    fn app_rollup_empty_for_unknown_app() {
        let envs: Vec<crate::aws::Environment> = vec![];
        let dlq: HashMap<String, i64> = HashMap::new();
        let r = super::app_rollup(&envs, "nope", &dlq);
        assert_eq!(r, super::AppRollup::default());
    }

    fn opt(
        ns: &str,
        name: &str,
        value: Option<&str>,
        default: Option<&str>,
    ) -> crate::aws::ConfigOption {
        crate::aws::ConfigOption {
            namespace: ns.into(),
            name: name.into(),
            value: value.map(String::from),
            default_value: default.map(String::from),
            value_type: "Scalar".into(),
            value_options: vec![],
            change_severity: None,
            user_defined: Some(true),
            min_value: None,
            max_value: None,
            max_length: None,
        }
    }

    #[test]
    fn build_env_edit_body_sorts_keys_and_emits_header() {
        let vars = vec![
            ("LOG_LEVEL".into(), "info".into()),
            ("DB_HOST".into(), "db.example".into()),
            ("DB_PORT".into(), "5432".into()),
        ];
        let body = super::build_env_edit_body("prod", &vars);
        // Header comment present.
        assert!(body.starts_with("# ebman env-var editor — prod\n"));
        assert!(body.contains("Secrets Manager"));
        // Keys sorted alphabetically.
        let db_host_pos = body.find("DB_HOST=").expect("DB_HOST line");
        let db_port_pos = body.find("DB_PORT=").expect("DB_PORT line");
        let log_pos = body.find("LOG_LEVEL=").expect("LOG_LEVEL line");
        assert!(db_host_pos < db_port_pos && db_port_pos < log_pos);
    }

    #[test]
    fn parse_env_edit_body_round_trip() {
        let vars = vec![
            ("LOG_LEVEL".into(), "info".into()),
            (
                "DB_URL".into(),
                "postgres://user:pass@host:5432/db?sslmode=require".into(),
            ),
        ];
        let body = super::build_env_edit_body("env", &vars);
        let parsed = super::parse_env_edit_body(&body);
        assert_eq!(parsed.get("LOG_LEVEL").map(String::as_str), Some("info"));
        // Value containing `=` (postgres URL) passes through intact
        // because we split on the *first* `=` only.
        assert_eq!(
            parsed.get("DB_URL").map(String::as_str),
            Some("postgres://user:pass@host:5432/db?sslmode=require")
        );
    }

    #[test]
    fn parse_env_edit_body_skips_comments_and_blanks() {
        let body = "# comment\n\nDB_HOST=localhost\n   # indented comment\n\nLOG=debug\n";
        let parsed = super::parse_env_edit_body(body);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed.get("DB_HOST").map(String::as_str), Some("localhost"));
        assert_eq!(parsed.get("LOG").map(String::as_str), Some("debug"));
    }

    #[test]
    fn parse_env_edit_body_drops_invalid_keys() {
        let body = "= no-key\n KEY WITH SPACES=foo\nGOOD=val\n";
        let parsed = super::parse_env_edit_body(body);
        assert_eq!(parsed.len(), 1);
        assert!(parsed.contains_key("GOOD"));
    }

    #[test]
    fn diff_env_vars_produces_set_and_remove_lists() {
        let mut original = std::collections::BTreeMap::new();
        original.insert("KEEP".into(), "same".into());
        original.insert("CHANGE".into(), "old".into());
        original.insert("DROP".into(), "going".into());
        let mut edited = std::collections::BTreeMap::new();
        edited.insert("KEEP".into(), "same".into()); // unchanged
        edited.insert("CHANGE".into(), "new".into()); // updated
        edited.insert("NEW".into(), "added".into()); // added

        let (to_set, to_remove) = super::diff_env_vars("ns", &original, &edited);
        // CHANGE + NEW should be in to_set; KEEP excluded (unchanged).
        let set_keys: std::collections::BTreeSet<&str> =
            to_set.iter().map(|(_, k, _)| k.as_str()).collect();
        assert_eq!(
            set_keys,
            ["CHANGE", "NEW"]
                .into_iter()
                .collect::<std::collections::BTreeSet<_>>(),
            "to_set should include changed + added keys"
        );
        assert!(
            !set_keys.contains("KEEP"),
            "unchanged key must not re-dispatch"
        );
        // DROP should be in to_remove.
        assert_eq!(to_remove.len(), 1);
        assert_eq!(to_remove[0].1, "DROP");
    }

    #[test]
    fn diff_env_vars_empty_when_unchanged() {
        let mut original = std::collections::BTreeMap::new();
        original.insert("A".into(), "1".into());
        original.insert("B".into(), "2".into());
        let edited = original.clone();
        let (to_set, to_remove) = super::diff_env_vars("ns", &original, &edited);
        assert!(to_set.is_empty());
        assert!(to_remove.is_empty());
    }

    #[test]
    fn parse_access_denied_handles_assumed_role() {
        let msg = "User: arn:aws:sts::123456789012:assumed-role/EbmanReadOnly/session-abc \
                   is not authorized to perform: elasticbeanstalk:RebuildEnvironment \
                   on resource: arn:aws:elasticbeanstalk:eu-west-2:123:environment/foo/bar";
        let parsed = super::parse_access_denied(msg);
        assert_eq!(
            parsed,
            Some((
                "arn:aws:iam::123456789012:role/EbmanReadOnly".into(),
                "elasticbeanstalk:RebuildEnvironment".into()
            )),
            "assumed-role should be rewritten to the role ARN"
        );
    }

    #[test]
    fn parse_access_denied_handles_iam_user() {
        let msg = "User: arn:aws:iam::123456789012:user/alice is not authorized to \
                   perform: s3:GetObject on resource: arn:aws:s3:::bucket/key";
        let parsed = super::parse_access_denied(msg);
        assert_eq!(
            parsed,
            Some((
                "arn:aws:iam::123456789012:user/alice".into(),
                "s3:GetObject".into()
            )),
            "IAM-user ARN should pass through unchanged"
        );
    }

    #[test]
    fn parse_access_denied_returns_none_on_unrelated_error() {
        assert_eq!(
            super::parse_access_denied("ThrottlingException: rate exceeded"),
            None
        );
        assert_eq!(super::parse_access_denied("random garbage text"), None);
    }

    #[test]
    fn render_explain_overlay_marks_decisions_and_suggests_fix() {
        let rows = vec![
            crate::aws::IamSimResult {
                action: "elasticbeanstalk:RebuildEnvironment".into(),
                resource: "*".into(),
                decision: "implicitDeny".into(),
                matched_statements: vec![],
                missing_context: vec![],
                blocked_by_scp: false,
                blocked_by_boundary: false,
            },
            crate::aws::IamSimResult {
                action: "ec2:DescribeInstances".into(),
                resource: "*".into(),
                decision: "allowed".into(),
                matched_statements: vec![
                    "arn:aws:iam::aws:policy/AmazonEC2ReadOnlyAccess @ 0:0".into()
                ],
                missing_context: vec![],
                blocked_by_scp: false,
                blocked_by_boundary: false,
            },
        ];
        let body = super::render_explain_overlay("arn:aws:iam::123:role/EbmanReadOnly", &rows);
        // Both action sections present, marked with correct decision glyphs.
        assert!(body.contains("Action:   elasticbeanstalk:RebuildEnvironment"));
        assert!(body.contains("✗ implicitDeny"));
        assert!(body.contains("Action:   ec2:DescribeInstances"));
        assert!(body.contains("✓ allowed"));
        // implicitDeny suggests the JSON-policy fix.
        assert!(body.contains("\"Effect\": \"Allow\""));
        assert!(body.contains("\"Action\": \"elasticbeanstalk:RebuildEnvironment\""));
        // The allowed action does NOT get the fix suggestion.
        assert!(body.matches("To allow, add this statement").count() == 1);
        // Matched statement surfaces for the allowed action.
        assert!(body.contains("AmazonEC2ReadOnlyAccess"));
    }

    #[test]
    fn render_explain_overlay_flags_scp_and_boundary_blockers() {
        let rows = vec![crate::aws::IamSimResult {
            action: "ec2:TerminateInstances".into(),
            resource: "*".into(),
            decision: "explicitDeny".into(),
            matched_statements: vec!["org-scp/SCPDenyTerminate @ 0:0".into()],
            missing_context: vec![],
            blocked_by_scp: true,
            blocked_by_boundary: true,
        }];
        let body = super::render_explain_overlay("arn:aws:iam::123:role/X", &rows);
        assert!(body.contains("Organizations SCP"));
        assert!(body.contains("permission boundary"));
        // explicitDeny gives the "Remove the Deny" hint instead of
        // the implicitDeny JSON snippet.
        assert!(body.contains("explicit Deny always wins"));
        assert!(!body.contains("\"Effect\": \"Allow\""));
    }

    fn empty_resources() -> crate::aws::EnvResources {
        crate::aws::EnvResources::default()
    }

    #[test]
    fn render_env_resources_tree_shows_asg_with_nested_instances() {
        let mut res = empty_resources();
        res.asgs = vec!["awseb-AWSEBAutoScalingGroup-XYZ".into()];
        res.instances = vec!["i-0abc".into(), "i-0def".into(), "i-0ghi".into()];
        let body = super::render_env_resources_tree(&res, "prod-api", "Web");
        // Section header for ASG group.
        assert!(body.contains("Auto-scaling groups (1)"));
        // ASG node under it (└─ since only one ASG).
        assert!(body.contains("└─ awseb-AWSEBAutoScalingGroup-XYZ"));
        // Instances nested below the ASG with proper tree glyphs.
        assert!(body.contains("├─ i-0abc"));
        assert!(body.contains("├─ i-0def"));
        assert!(body.contains("└─ i-0ghi"));
    }

    #[test]
    fn render_env_resources_tree_skips_empty_sections() {
        let mut res = empty_resources();
        res.asgs = vec!["asg-1".into()];
        // Everything else empty.
        let body = super::render_env_resources_tree(&res, "small-env", "Web");
        assert!(body.contains("Auto-scaling groups (1)"));
        // No load-balancer / launch-config / queue headers when
        // the lists are empty.
        assert!(!body.contains("Load balancers"));
        assert!(!body.contains("Launch configurations"));
        assert!(!body.contains("Queues"));
    }

    #[test]
    fn render_env_resources_tree_marks_orphan_instances_when_no_asg() {
        let mut res = empty_resources();
        res.instances = vec!["i-stranded".into()];
        let body = super::render_env_resources_tree(&res, "env", "Web");
        assert!(body.contains("orphan (no ASG attached)"));
        assert!(body.contains("i-stranded"));
    }

    #[test]
    fn render_env_resources_tree_renders_queue_urls_inline() {
        let mut res = empty_resources();
        res.queues = vec![
            crate::aws::EnvResourceQueue {
                name: "WorkerQueue".into(),
                url: "https://sqs.eu-west-2.amazonaws.com/123/main".into(),
            },
            crate::aws::EnvResourceQueue {
                name: "WorkerDeadLetterQueue".into(),
                url: "https://sqs.eu-west-2.amazonaws.com/123/dlq".into(),
            },
        ];
        let body = super::render_env_resources_tree(&res, "worker-prod", "Worker");
        assert!(body.contains("├─ WorkerQueue"));
        assert!(body.contains("https://sqs.eu-west-2.amazonaws.com/123/main"));
        assert!(body.contains("└─ WorkerDeadLetterQueue"));
        assert!(body.contains("https://sqs.eu-west-2.amazonaws.com/123/dlq"));
    }

    #[test]
    fn render_env_resources_tree_handles_zero_resources() {
        let res = empty_resources();
        let body = super::render_env_resources_tree(&res, "fresh-env", "Web");
        assert!(body.contains("(no resources reported"));
    }

    #[tokio::test]
    async fn first_run_hint_dismisses_on_first_key() {
        let mut app = test_app();
        app.first_run_hint = true;
        press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
        assert!(
            !app.first_run_hint,
            "first key event should clear first_run_hint"
        );
    }

    #[tokio::test]
    async fn first_run_hint_stays_false_for_subsequent_launches() {
        // Simulates "ebman has run before; state.toml exists."
        // The test harness defaults first_run_hint to false anyway,
        // but this nails down the contract: a state.toml on disk
        // means no hint, full stop.
        let app = test_app();
        assert!(
            !app.first_run_hint,
            "test harness must default first_run_hint=false (state.toml presumed present)"
        );
    }

    #[test]
    fn edit_distance_basic_cases() {
        assert_eq!(super::edit_distance("", ""), 0);
        assert_eq!(super::edit_distance("abc", ""), 3);
        assert_eq!(super::edit_distance("", "abc"), 3);
        assert_eq!(super::edit_distance("kitten", "sitting"), 3);
        assert_eq!(super::edit_distance("restart", "restart"), 0);
        assert_eq!(super::edit_distance("restrt", "restart"), 1);
        assert_eq!(super::edit_distance("rebild", "rebuild"), 1);
        assert_eq!(super::edit_distance("scal", "scale"), 1);
    }

    #[test]
    fn suggest_command_catches_one_char_typos() {
        // Operator typo: forgot the 'a' in restart.
        assert_eq!(super::suggest_command("restrt").as_deref(), Some("restart"));
        // Operator typo: dropped a 'u' in rebuild.
        assert_eq!(super::suggest_command("rebild").as_deref(), Some("rebuild"));
        // Operator typo: dropped the 'e' in scale.
        assert_eq!(super::suggest_command("scal").as_deref(), Some("scale"));
    }

    #[test]
    fn suggest_command_returns_none_when_too_far() {
        // Nonsense input — no command is within edit-distance 2.
        assert_eq!(super::suggest_command("zzzzzz"), None);
    }

    #[test]
    fn suggest_command_threshold_is_strict_for_short_input() {
        // 2-char input shouldn't "match" every 3-char alias —
        // the operator's intent is too ambiguous to guess.
        // `:zz` is distance 2 from many names; we cap at 1.
        let suggestion = super::suggest_command("zz");
        assert!(
            suggestion.is_none(),
            "2-char typo should require distance ≤ 1; got {suggestion:?}"
        );
    }

    #[test]
    fn completion_candidates_filters_by_prefix() {
        let c = super::completion_candidates("ba");
        assert!(
            c.iter().any(|s| s == "batch-rebuild"),
            "expected batch-rebuild among ba-prefixed candidates; got {c:?}"
        );
        assert!(
            c.iter().all(|s| s.starts_with("ba")),
            "every candidate must start with the prefix; got {c:?}"
        );
        assert_eq!(
            c.clone(),
            {
                let mut sorted = c.clone();
                sorted.sort();
                sorted
            },
            "candidates must be alphabetically sorted"
        );
    }

    #[test]
    fn completion_candidates_with_empty_prefix_returns_full_list() {
        let c = super::completion_candidates("");
        // The registry has 80+ names + aliases — exact count drifts
        // with each release, just sanity-check the shape.
        assert!(
            c.len() > 50,
            "expected the full command list; got {} entries",
            c.len()
        );
        assert!(c.iter().any(|s| s == "why"));
        assert!(c.iter().any(|s| s == "rebuild"));
    }

    #[tokio::test]
    async fn tab_in_command_mode_cycles_through_matches() {
        let mut app = test_app();
        app.mode = Mode::Command;
        app.command_input = "bat".into();
        // First Tab → first match (batch-deploy alphabetically).
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        let first = app.command_input.clone();
        assert!(
            first.starts_with("bat"),
            "Tab should keep the bat-prefix; got {first:?}"
        );
        assert!(
            crate::commands::all_names().contains(&first.as_str()),
            "Tab should expand to a real command name; got {first:?}"
        );
        // Second Tab cycles forward; should differ from first.
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        let second = app.command_input.clone();
        assert_ne!(first, second, "second Tab should advance the cycle");
    }

    #[tokio::test]
    async fn typing_in_command_mode_breaks_the_completion_cycle() {
        let mut app = test_app();
        app.mode = Mode::Command;
        app.command_input = "re".into();
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert!(app.command_completion_origin.is_some());
        // Operator types — cycle should reset.
        press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(
            app.command_completion_origin.is_none(),
            "typing must reset the completion origin"
        );
    }

    #[tokio::test]
    async fn shift_tab_cycles_backward() {
        let mut app = test_app();
        app.mode = Mode::Command;
        app.command_input = "ba".into();
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        let forward = app.command_input.clone();
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        press(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
        // Two forward + one back = same as one forward.
        assert_eq!(
            app.command_input, forward,
            "Tab Tab BackTab should land on the first match"
        );
    }

    #[test]
    fn render_options_overlay_groups_by_namespace_and_marks_set_vs_default() {
        let rows = vec![
            opt("aws:autoscaling:asg", "MinSize", Some("2"), Some("1")),
            opt("aws:autoscaling:asg", "MaxSize", None, Some("4")),
            opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                Some("Rolling"),
                Some("AllAtOnce"),
            ),
        ];
        let body = super::render_options_overlay(&rows, None, "uflexi-prod");
        // Section headers per namespace.
        assert!(body.contains("── aws:autoscaling:asg ──"));
        assert!(body.contains("── aws:elasticbeanstalk:command ──"));
        // Operator-set rows marked with ▸; default rows with •.
        assert!(body.contains("▸ MinSize"));
        assert!(body.contains("• MaxSize"));
        assert!(body.contains("▸ DeploymentPolicy"));
        // Default value is surfaced.
        assert!(body.contains("default: 1"));
        assert!(body.contains("default: 4"));
        // Top header counts set vs default.
        assert!(body.contains("2/3 options are operator-set"));
    }

    #[test]
    fn render_options_overlay_filters_to_namespace_when_given() {
        let rows = vec![
            opt("aws:autoscaling:asg", "MinSize", Some("2"), None),
            opt(
                "aws:elasticbeanstalk:command",
                "DeploymentPolicy",
                Some("Rolling"),
                None,
            ),
        ];
        let body = super::render_options_overlay(&rows, Some("aws:autoscaling:asg"), "uflexi-prod");
        assert!(body.contains("MinSize"));
        assert!(!body.contains("DeploymentPolicy"));
    }

    #[test]
    fn render_options_overlay_handles_unknown_namespace() {
        let rows = vec![opt("aws:autoscaling:asg", "MinSize", Some("2"), None)];
        let body = super::render_options_overlay(&rows, Some("aws:bogus:ns"), "uflexi-prod");
        assert!(body.contains("No options found"));
        assert!(body.contains("aws:bogus:ns"));
    }

    #[test]
    fn render_secrets_overlay_empty_with_filter_explains_region_scope() {
        let body = super::render_secrets_overlay(&[], Some("prod-db"));
        assert!(body.contains("No secrets matching 'prod-db'"));
        assert!(body.contains("region-scoped"));
    }

    #[test]
    fn render_secrets_overlay_empty_no_filter_hints_at_iam() {
        let body = super::render_secrets_overlay(&[], None);
        assert!(body.contains("No Secrets Manager secrets"));
        assert!(body.contains("ListSecrets"));
    }

    #[test]
    fn render_secrets_overlay_lists_metadata_only() {
        let now = chrono::Utc::now();
        let rows = vec![crate::aws::SecretSummary {
            name: "prod/db/password".into(),
            arn: "arn:aws:secretsmanager:us-east-1:123456789012:secret:prod/db/password-AbCdEf"
                .into(),
            description: Some("RDS master".into()),
            last_changed: Some(now - chrono::Duration::days(3)),
            last_rotated: Some(now - chrono::Duration::days(30)),
            kms_key_id: Some("alias/aws/secretsmanager".into()),
        }];
        let body = super::render_secrets_overlay(&rows, None);
        assert!(body.contains("prod/db/password"));
        assert!(body.contains("RDS master"));
        assert!(body.contains("arn:aws:secretsmanager"));
        assert!(body.contains("changed:"));
        assert!(body.contains("rotated:"));
        assert!(body.contains("alias/aws/secretsmanager"));
        // The values themselves must never appear in :secrets output.
        assert!(!body.to_lowercase().contains("password:"));
    }

    #[test]
    fn render_secrets_overlay_marks_never_rotated() {
        let now = chrono::Utc::now();
        let rows = vec![crate::aws::SecretSummary {
            name: "api-key".into(),
            arn: "arn:aws:secretsmanager:us-east-1:1:secret:api-key-x".into(),
            description: None,
            last_changed: Some(now - chrono::Duration::hours(2)),
            last_rotated: None,
            kms_key_id: None,
        }];
        let body = super::render_secrets_overlay(&rows, None);
        assert!(body.contains("rotated: never"));
    }

    #[test]
    fn render_secret_value_overlay_redacts_when_redact_on() {
        let body = super::render_secret_value_overlay("api-key", "hunter2", true);
        assert!(body.contains("<redacted; 7 chars"));
        assert!(body.contains("fingerprint"));
        assert!(!body.contains("hunter2"));
        assert!(body.contains(":redact off"));
    }

    #[test]
    fn render_secret_value_overlay_shows_value_when_redact_off() {
        let body = super::render_secret_value_overlay("api-key", "hunter2", false);
        assert!(body.contains("hunter2"));
        assert!(body.contains("yank"));
    }

    #[test]
    fn render_secret_value_overlay_pretty_prints_json() {
        let body = super::render_secret_value_overlay(
            "prod/db",
            r#"{"USERNAME":"app","PASSWORD":"x"}"#,
            false,
        );
        // Expect a multi-line shape, not the input one-liner.
        assert!(body.contains("USERNAME"));
        assert!(body.contains("PASSWORD"));
        assert!(
            body.matches('\n').count() >= 4,
            "should pretty-print: {body}"
        );
    }

    #[test]
    fn render_secret_value_overlay_leaves_non_json_alone() {
        let body = super::render_secret_value_overlay("flat", "ABC-DEF-GHI", false);
        assert!(body.contains("ABC-DEF-GHI"));
    }

    #[test]
    fn short_fingerprint_is_stable_and_diffs() {
        let a = super::short_fingerprint("hunter2");
        let b = super::short_fingerprint("hunter2");
        let c = super::short_fingerprint("hunter3");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(a.len(), 8);
    }

    #[test]
    fn try_pretty_json_passes_through_non_json() {
        assert_eq!(super::try_pretty_json("just a string"), "just a string");
        assert_eq!(super::try_pretty_json(""), "");
    }

    #[test]
    fn try_pretty_json_indents_objects() {
        let pretty = super::try_pretty_json(r#"{"a":1,"b":2}"#);
        let lines: Vec<&str> = pretty.lines().collect();
        assert!(lines.len() >= 4, "lines={lines:?}");
        assert!(lines.iter().any(|l| l.contains("\"a\": 1")));
        assert!(lines.iter().any(|l| l.contains("\"b\": 2")));
    }

    #[test]
    fn try_pretty_json_preserves_strings_with_braces() {
        // A `{` inside a string must not trigger indent.
        let pretty = super::try_pretty_json(r#"{"msg":"hello {world}"}"#);
        assert!(pretty.contains("hello {world}"));
    }

    #[test]
    fn format_age_buckets() {
        let now = chrono::Utc::now();
        assert!(super::format_age(now, now).ends_with("s ago"));
        assert!(super::format_age(now, now - chrono::Duration::seconds(120)).ends_with("m ago"));
        assert!(super::format_age(now, now - chrono::Duration::hours(5)).ends_with("h ago"));
        assert!(super::format_age(now, now - chrono::Duration::days(10)).ends_with("d ago"));
        let body = super::format_age(now, now - chrono::Duration::days(120));
        assert!(body.starts_with('~') && body.contains("mo"));
    }

    #[test]
    fn render_options_overlay_truncates_long_value_options_list() {
        let mut row = opt("aws:foo", "Enum", Some("a"), None);
        row.value_options = (0..20).map(|i| format!("v{i}")).collect();
        let rows = vec![row];
        let body = super::render_options_overlay(&rows, None, "env");
        assert!(body.contains("oneof: v0, v1, v2, v3, v4, … +15"));
    }

    #[test]
    fn flatten_err_marks_access_denied() {
        let e = color_eyre::eyre::eyre!("operation failed")
            .wrap_err("AccessDeniedException: User: arn:aws:sts::1234 is not authorized");
        let out = super::flatten_err_to_string(&e);
        assert!(out.starts_with("AccessDenied:"), "got: {out}");
    }

    #[test]
    fn flatten_err_marks_not_found() {
        let e = color_eyre::eyre::eyre!("operation failed")
            .wrap_err("ResourceNotFoundException: alarm 'foo' does not exist");
        let out = super::flatten_err_to_string(&e);
        assert!(out.starts_with("NotFound:"), "got: {out}");
    }

    #[test]
    fn flatten_err_marks_dependency_violation() {
        let e = color_eyre::eyre::eyre!("operation failed")
            .wrap_err("DependencyViolation: resource still has dependencies");
        let out = super::flatten_err_to_string(&e);
        assert!(out.starts_with("Conflict:"), "got: {out}");
    }

    #[test]
    fn flatten_err_marks_expired_token() {
        let e = color_eyre::eyre::eyre!("operation failed")
            .wrap_err("ExpiredToken: session credentials expired");
        let out = super::flatten_err_to_string(&e);
        assert!(out.starts_with("ExpiredToken:"), "got: {out}");
    }

    #[test]
    fn flatten_err_passes_unknown_through_unchanged() {
        let e = color_eyre::eyre::eyre!("some other failure");
        let out = super::flatten_err_to_string(&e);
        assert!(
            !out.contains(":"),
            "expected no classification prefix; got: {out}"
        );
    }

    #[test]
    fn traffic_warning_flags_updating() {
        let e = fake_env_with("prod", "Updating", "Yellow", Some(20));
        assert!(super::compute_traffic_warning(&e)
            .unwrap()
            .contains("ACTIVE DEPLOY"));
    }

    #[test]
    fn traffic_warning_flags_recent_change() {
        let e = fake_env_with("prod", "Ready", "Green", Some(2));
        assert!(super::compute_traffic_warning(&e)
            .unwrap()
            .contains("RECENT CHANGE"));
    }

    #[test]
    fn traffic_warning_silent_on_quiet_env() {
        let e = fake_env_with("prod", "Ready", "Green", Some(60));
        assert!(super::compute_traffic_warning(&e).is_none());
    }

    #[test]
    fn traffic_warning_flags_red_health() {
        let e = fake_env_with("prod", "Ready", "Red", Some(120));
        assert!(super::compute_traffic_warning(&e).unwrap().contains("Red"));
    }

    #[test]
    fn webhook_payload_escapes_quotes_and_backslashes() {
        let p = super::build_webhook_payload(
            "my\"env",
            "my\\app",
            "Red",
            "eu-west-2",
            Some("123456789012"),
        );
        assert!(p.contains("\"event\":\"env_red\""));
        assert!(p.contains("my\\\"env"));
        assert!(p.contains("my\\\\app"));
        assert!(p.contains("\"account\":\"123456789012\""));
    }

    #[test]
    fn webhook_payload_handles_missing_account() {
        let p = super::build_webhook_payload("env", "app", "Red", "us-east-1", None);
        assert!(p.contains("\"account\":\"\""));
    }

    #[test]
    fn is_throttling_error_matches_common_aws_strings() {
        assert!(is_throttling_error("ThrottlingException: Rate exceeded"));
        assert!(is_throttling_error(
            "service error: ThrottlingException — please slow down"
        ));
        assert!(is_throttling_error("RequestLimitExceeded"));
        assert!(is_throttling_error("HTTP 429 Too Many Requests"));
        assert!(is_throttling_error("rate exceeded for this account"));
        // Negative cases.
        assert!(!is_throttling_error("EnvironmentNotFound"));
        assert!(!is_throttling_error("AccessDenied"));
        assert!(!is_throttling_error(""));
    }

    #[test]
    fn throttle_backoff_grows_then_caps() {
        let base = Duration::from_secs(15);
        let b0 = throttle_backoff(base, 0);
        let b1 = throttle_backoff(base, 1);
        let b2 = throttle_backoff(base, 2);
        // First throttle: 2x base (30 s); second: 4x; third: 8x.
        assert_eq!(b0, Duration::from_secs(30));
        assert_eq!(b1, Duration::from_secs(60));
        assert_eq!(b2, Duration::from_secs(120));
        // Way past the cap stays at the cap.
        let bn = throttle_backoff(base, 30);
        assert_eq!(bn, Duration::from_secs(300));
    }

    #[test]
    fn throttle_backoff_handles_overflow_safely() {
        // Pathologically large base must not panic — saturating_mul keeps us safe.
        let base = Duration::MAX;
        let b = throttle_backoff(base, 5);
        assert_eq!(b, Duration::from_secs(300));
    }

    #[test]
    fn delta_toast_key_extracts_bucket_for_delta_shapes() {
        assert_eq!(super::delta_toast_key("▲2 Red").as_deref(), Some("Red"));
        assert_eq!(
            super::delta_toast_key("▼1 Yellow").as_deref(),
            Some("Yellow")
        );
        // Leading whitespace is allowed.
        assert_eq!(
            super::delta_toast_key("  ▲10 Green").as_deref(),
            Some("Green")
        );
    }

    #[test]
    fn format_app_versions_marks_deployed_and_shows_total_when_truncated() {
        use crate::aws::AppVersion;
        let mk = |label: &str, desc: &str| AppVersion {
            label: label.into(),
            description: desc.into(),
            created: None,
        };
        let versions: Vec<AppVersion> = (1..=30)
            .map(|i| {
                mk(
                    &format!("build-{i}"),
                    &format!("Application version created from https://example.com/build/{i}"),
                )
            })
            .rev()
            .collect();
        // build-5 is outside the top 20 (which is build-30 down to build-11
        // after the rev). Lets us check the truncation banner without the
        // deployed marker showing up.
        let out = super::format_app_versions(&versions, Some("build-5"), 20);
        assert!(out.contains("showing 20 of 30"));
        assert!(!out.contains("◀ deployed"));
        // Description prefix stripped.
        assert!(out.contains("https://example.com/build/"));
        assert!(!out.contains("Application version created from "));
    }

    #[test]
    fn format_app_versions_marks_deployed_when_present() {
        use crate::aws::AppVersion;
        let versions = vec![
            AppVersion {
                label: "build-3".into(),
                description: String::new(),
                created: None,
            },
            AppVersion {
                label: "build-2".into(),
                description: String::new(),
                created: None,
            },
        ];
        let out = super::format_app_versions(&versions, Some("build-2"), 20);
        assert!(out.contains("◀ deployed"));
        // No truncation banner when total <= limit.
        assert!(!out.contains("showing "));
    }

    #[test]
    fn wrap_with_hanging_indent_first_line_keeps_lead_marker() {
        let out = super::wrap_with_hanging_indent(
            "Threshold Crossed: alarm details continue",
            30,
            "  ↳ ",
            "    ",
        );
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("  ↳ "));
        // Continuation line uses the cont prefix.
        if lines.len() > 1 {
            assert!(lines[1].starts_with("    "));
        }
    }

    #[test]
    fn wrap_with_hanging_indent_hard_breaks_oversize_words() {
        // A single 50-char word at width 20 + 4-char lead → body width 16.
        let big_word = "x".repeat(50);
        let out = super::wrap_with_hanging_indent(&big_word, 20, "    ", "    ");
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines.len() >= 3);
    }

    #[test]
    fn parse_s3_url_extracts_bucket_and_key() {
        let (b, k) = super::parse_s3_url("s3://my-bucket/path/to/bundle.zip").unwrap();
        assert_eq!(b, "my-bucket");
        assert_eq!(k, "path/to/bundle.zip");
    }

    #[test]
    fn parse_s3_url_rejects_malformed() {
        assert!(super::parse_s3_url("/local/path.zip").is_none());
        assert!(super::parse_s3_url("s3://").is_none());
        assert!(super::parse_s3_url("s3://bucket").is_none());
        assert!(super::parse_s3_url("s3://bucket/").is_none());
        assert!(super::parse_s3_url("s3:///key").is_none());
    }

    #[test]
    fn parse_metric_extra_args_defaults_to_average() {
        let (stat, dims) = super::parse_metric_extra_args(&[]);
        assert_eq!(stat, "Average");
        assert!(dims.is_empty());
    }

    #[test]
    fn parse_metric_extra_args_picks_stat_first() {
        let (stat, dims) = super::parse_metric_extra_args(&["Sum"]);
        assert_eq!(stat, "Sum");
        assert!(dims.is_empty());
    }

    #[test]
    fn parse_metric_extra_args_picks_dims_when_present() {
        let (stat, dims) = super::parse_metric_extra_args(&["InstanceId=i-abc"]);
        assert_eq!(stat, "Average");
        assert_eq!(dims, vec![("InstanceId".into(), "i-abc".into())]);
    }

    #[test]
    fn parse_metric_extra_args_supports_both_in_any_order() {
        let (stat, dims) = super::parse_metric_extra_args(&["Sum", "InstanceId=i-abc,Tier=web"]);
        assert_eq!(stat, "Sum");
        assert_eq!(
            dims,
            vec![
                ("InstanceId".into(), "i-abc".into()),
                ("Tier".into(), "web".into()),
            ]
        );
        // Reversed order: dims first.
        let (stat, dims) = super::parse_metric_extra_args(&["InstanceId=i-abc", "Sum"]);
        assert_eq!(stat, "Sum");
        assert_eq!(dims, vec![("InstanceId".into(), "i-abc".into())]);
    }

    #[test]
    fn derive_version_label_uses_filename_stem_and_timestamp() {
        let l = super::derive_version_label("./build.zip", 1684512345);
        assert_eq!(l, "build_1684512345");
        let l = super::derive_version_label("/tmp/myapp-2.1.0.zip", 42);
        assert_eq!(l, "myapp-2.1.0_42");
    }

    #[test]
    fn derive_version_label_sanitises_disallowed_chars() {
        // EB version labels don't allow spaces or weird punctuation; we
        // replace them with `_` so the operator gets a valid label even from
        // a goofy filename.
        let l = super::derive_version_label("/tmp/build with spaces & specials!.zip", 1);
        assert_eq!(l, "build_with_spaces___specials__1");
    }

    #[test]
    fn derive_version_label_falls_back_to_bundle_on_pathological_input() {
        // Bare `/` has no filename stem.
        let l = super::derive_version_label("/", 9);
        assert_eq!(l, "bundle_9");
    }

    #[test]
    fn expand_tilde_only_replaces_leading() {
        // Set HOME for the test.
        let prev = std::env::var_os("HOME");
        // SAFETY: tests run single-threaded by default; restore at the end.
        unsafe {
            std::env::set_var("HOME", "/Users/tester");
        }
        assert_eq!(super::expand_tilde("~/foo/bar"), "/Users/tester/foo/bar");
        // No leading tilde → unchanged.
        assert_eq!(super::expand_tilde("/abs/path"), "/abs/path");
        // `~name` left alone (not supported).
        assert_eq!(super::expand_tilde("~tom/foo"), "~tom/foo");
        // Mid-path tilde left alone.
        assert_eq!(super::expand_tilde("/foo/~/bar"), "/foo/~/bar");
        if let Some(v) = prev {
            unsafe {
                std::env::set_var("HOME", v);
            }
        } else {
            unsafe {
                std::env::remove_var("HOME");
            }
        }
    }

    #[test]
    fn pick_default_log_group_prefers_web_stdout() {
        let groups: Vec<String> = vec![
            "/aws/elasticbeanstalk/myenv/var/log/eb-engine.log".into(),
            "/aws/elasticbeanstalk/myenv/var/log/web.stdout.log".into(),
            "/aws/elasticbeanstalk/myenv/var/log/nginx/access.log".into(),
        ];
        assert_eq!(
            super::pick_default_log_group(&groups).as_deref(),
            Some("/aws/elasticbeanstalk/myenv/var/log/web.stdout.log")
        );
    }

    #[test]
    fn pick_default_log_group_falls_back_to_first() {
        let groups: Vec<String> = vec!["/aws/elasticbeanstalk/myenv/var/log/custom.log".into()];
        assert_eq!(
            super::pick_default_log_group(&groups).as_deref(),
            Some("/aws/elasticbeanstalk/myenv/var/log/custom.log")
        );
        // No groups at all → None.
        assert_eq!(super::pick_default_log_group(&[]), None);
    }

    #[test]
    fn pick_default_log_group_prefers_engine_log_when_stdout_absent() {
        let groups: Vec<String> = vec![
            "/aws/elasticbeanstalk/myenv/var/log/nginx/access.log".into(),
            "/aws/elasticbeanstalk/myenv/var/log/eb-engine.log".into(),
        ];
        assert_eq!(
            super::pick_default_log_group(&groups).as_deref(),
            Some("/aws/elasticbeanstalk/myenv/var/log/eb-engine.log")
        );
    }

    #[test]
    fn format_env_vars_aligns_on_equals() {
        let vars = vec![
            ("DEBUG".into(), "1".into()),
            ("DATABASE_URL".into(), "postgres://x".into()),
        ];
        let out = super::format_env_vars(&vars);
        assert!(out.contains("DEBUG"));
        assert!(out.contains("= 1"));
        assert!(out.contains("DATABASE_URL"));
        let vars = vec![("EMPTY".into(), "".into())];
        assert!(super::format_env_vars(&vars).contains("\"\""));
    }

    #[test]
    fn format_env_vars_handles_empty_input() {
        assert_eq!(super::format_env_vars(&[]), "(no env vars set)");
    }

    #[test]
    fn parse_named_arg_picks_up_value_after_flag() {
        let rest: Vec<&str> = vec!["on", "--retention", "14"];
        assert_eq!(
            super::parse_named_arg::<i32>(&rest, "--retention"),
            Some(14)
        );
        // Flag absent.
        assert_eq!(super::parse_named_arg::<i32>(&["on"], "--retention"), None);
        // Flag present but no following value.
        assert_eq!(
            super::parse_named_arg::<i32>(&["on", "--retention"], "--retention"),
            None
        );
        // Following value doesn't parse.
        assert_eq!(
            super::parse_named_arg::<i32>(&["on", "--retention", "abc"], "--retention"),
            None
        );
    }

    #[test]
    fn alarm_kind_to_metric_covers_known_kinds() {
        use crate::app::alarm_kind_to_metric;
        let (m, op, _) = alarm_kind_to_metric("health").unwrap();
        assert_eq!(m, "EnvironmentHealth");
        // Health is "drop below" → LessThanOrEqualToThreshold.
        assert_eq!(op, "LessThanOrEqualToThreshold");
        let (m, op, _) = alarm_kind_to_metric("5xx").unwrap();
        assert_eq!(m, "ApplicationRequests5xx");
        assert_eq!(op, "GreaterThanThreshold");
        // Aliases.
        assert_eq!(alarm_kind_to_metric("req5xx"), alarm_kind_to_metric("5xx"));
        assert_eq!(alarm_kind_to_metric("p90"), alarm_kind_to_metric("latency"));
        // Unknown.
        assert!(alarm_kind_to_metric("cpu").is_none());
        assert!(alarm_kind_to_metric("").is_none());
    }

    #[test]
    fn format_template_settings_groups_by_namespace() {
        let s = vec![
            (
                "aws:elasticbeanstalk:environment".into(),
                "EnvironmentType".into(),
                "LoadBalanced".into(),
            ),
            ("aws:autoscaling:asg".into(), "MinSize".into(), "2".into()),
            ("aws:autoscaling:asg".into(), "MaxSize".into(), "8".into()),
        ];
        let out = super::format_template_settings(&s);
        assert!(out.contains("[aws:autoscaling:asg]"));
        assert!(out.contains("[aws:elasticbeanstalk:environment]"));
        assert!(out.contains("MinSize"));
        assert!(out.contains("= 2"));
        // Empty value renders as the literal "" so operators can tell empty
        // from unset.
        let s = vec![(
            "aws:elasticbeanstalk:application:environment".into(),
            "DEBUG".into(),
            String::new(),
        )];
        assert!(super::format_template_settings(&s).contains("DEBUG"));
        assert!(super::format_template_settings(&s).contains("\"\""));
    }

    #[test]
    fn format_template_settings_handles_empty_input() {
        assert_eq!(super::format_template_settings(&[]), "(no option settings)");
    }

    #[test]
    fn action_labels_are_distinct_and_non_empty() {
        // Catches accidental "placeholder Action::Rebuild" reuses — every
        // variant must carry its own label so audit logs + toasts reflect
        // what was actually dispatched.
        use crate::app::Action;
        use std::collections::HashSet;
        let all = [
            Action::Rebuild,
            Action::RestartAppServer,
            Action::SwapCnames,
            Action::Terminate,
            Action::Deploy,
            Action::UpgradePlatform,
            Action::Clone,
            Action::Scale,
            Action::AbortUpdate,
            Action::ConfigSave,
            Action::ConfigDelete,
            Action::ConfigApply,
            Action::TerminateInstance,
        ];
        let mut labels = HashSet::new();
        for a in all {
            let l = a.label();
            assert!(!l.is_empty(), "{a:?} has empty label");
            assert!(labels.insert(l), "{a:?} reuses label {l:?}");
        }
    }

    #[test]
    fn collect_saved_configs_flattens_and_sorts_stably() {
        use crate::aws::Application;
        let app = |name: &str, templates: Vec<String>| Application {
            name: name.into(),
            description: String::new(),
            date_created: None,
            date_updated: None,
            version_count: 0,
            templates,
            latest_version_label: None,
            latest_version_created: None,
        };
        let apps = vec![
            app("beta", vec!["prod".into(), "canary".into()]),
            app("alpha", vec![]),
            app("alpha", vec!["staging".into()]),
        ];
        let out = super::collect_saved_configs(&apps);
        assert_eq!(
            out,
            vec![
                ("alpha".into(), "staging".into()),
                ("beta".into(), "canary".into()),
                ("beta".into(), "prod".into()),
            ]
        );
    }

    #[test]
    fn collect_saved_configs_empty_when_no_templates() {
        use crate::aws::Application;
        let apps = vec![Application {
            name: "alpha".into(),
            description: String::new(),
            date_created: None,
            date_updated: None,
            version_count: 0,
            templates: vec![],
            latest_version_label: None,
            latest_version_created: None,
        }];
        assert!(super::collect_saved_configs(&apps).is_empty());
    }

    #[test]
    fn merge_app_latest_versions_carries_previous_values_by_name() {
        use crate::aws::Application;
        let mk = |name: &str,
                  label: Option<&str>,
                  created: Option<chrono::DateTime<chrono::Utc>>|
         -> Application {
            Application {
                name: name.into(),
                description: String::new(),
                date_created: None,
                date_updated: None,
                version_count: 0,
                templates: vec![],
                latest_version_label: label.map(|s| s.into()),
                latest_version_created: created,
            }
        };
        let t0 = chrono::Utc::now();
        let prev = vec![
            mk("alpha", Some("build-1"), Some(t0)),
            mk("beta", Some("build-9"), Some(t0)),
        ];
        // Fresh refresh: same apps, plus a new one, all with empty LATEST.
        let mut next = vec![
            mk("alpha", None, None),
            mk("beta", None, None),
            mk("gamma", None, None),
        ];
        super::merge_app_latest_versions(&prev, &mut next);
        assert_eq!(next[0].latest_version_label.as_deref(), Some("build-1"));
        assert_eq!(next[0].latest_version_created, Some(t0));
        assert_eq!(next[1].latest_version_label.as_deref(), Some("build-9"));
        // New app has no prior value; stays None.
        assert_eq!(next[2].latest_version_label, None);
        assert_eq!(next[2].latest_version_created, None);
    }

    #[test]
    fn merge_app_latest_versions_does_not_overwrite_already_populated_slots() {
        // Safety net: if a future caller pre-populates the LATEST fields on
        // `next` (e.g. a faster fan-out lands before the apps-list does),
        // the carry-forward must not stomp on fresher data.
        use crate::aws::Application;
        let mk = |name: &str, label: Option<&str>| -> Application {
            Application {
                name: name.into(),
                description: String::new(),
                date_created: None,
                date_updated: None,
                version_count: 0,
                templates: vec![],
                latest_version_label: label.map(|s| s.into()),
                latest_version_created: None,
            }
        };
        let prev = vec![mk("alpha", Some("OLD"))];
        let mut next = vec![mk("alpha", Some("NEW"))];
        super::merge_app_latest_versions(&prev, &mut next);
        assert_eq!(next[0].latest_version_label.as_deref(), Some("NEW"));
    }

    #[test]
    fn merge_app_latest_versions_handles_app_disappearance() {
        // If an app is renamed / deleted between refreshes, its prev entry
        // simply has no matching `next` and the carry-forward is a no-op.
        use crate::aws::Application;
        let mk = |name: &str, label: Option<&str>| -> Application {
            Application {
                name: name.into(),
                description: String::new(),
                date_created: None,
                date_updated: None,
                version_count: 0,
                templates: vec![],
                latest_version_label: label.map(|s| s.into()),
                latest_version_created: None,
            }
        };
        let prev = vec![mk("alpha", Some("build-old")), mk("beta", Some("build-2"))];
        let mut next = vec![mk("beta", None)];
        super::merge_app_latest_versions(&prev, &mut next);
        assert_eq!(next.len(), 1);
        assert_eq!(next[0].latest_version_label.as_deref(), Some("build-2"));
    }

    #[test]
    fn format_org_accounts_includes_switch_hint_when_configured() {
        use crate::aws::OrgAccount;
        let accounts = vec![
            OrgAccount {
                id: "111122223333".into(),
                name: "prod".into(),
                email: Some("prod@example.com".into()),
                status: "ACTIVE".into(),
            },
            OrgAccount {
                id: "444455556666".into(),
                name: "sandbox".into(),
                email: None,
                status: "SUSPENDED".into(),
            },
        ];
        let mut configured = std::collections::HashMap::new();
        configured.insert("prod".to_string(), "prod".to_string());
        let body = super::format_org_accounts(&accounts, &configured);
        assert!(body.contains("● prod"));
        assert!(body.contains("⊘ sandbox"));
        assert!(body.contains("prod@example.com"));
        // Switch hint only for the configured account.
        assert!(body.contains(":account prod"));
        assert!(!body.contains(":account sandbox"));
    }

    #[test]
    fn format_org_accounts_empty_returns_hint() {
        let body = super::format_org_accounts(&[], &std::collections::HashMap::new());
        assert!(body.contains("no accounts returned"));
    }

    #[test]
    fn format_org_accounts_matches_id_when_named_by_id() {
        use crate::aws::OrgAccount;
        let accounts = vec![OrgAccount {
            id: "111122223333".into(),
            name: "prod".into(),
            email: None,
            status: "ACTIVE".into(),
        }];
        // Operator named the AssumeRole entry by account-id rather
        // than friendly name — still matches.
        let mut configured = std::collections::HashMap::new();
        configured.insert("111122223333".to_string(), "111122223333".to_string());
        let body = super::format_org_accounts(&accounts, &configured);
        assert!(body.contains(":account 111122223333"));
    }

    #[test]
    fn format_deploy_preview_happy_path() {
        use crate::aws::AppVersion;
        let now = chrono::Utc::now();
        let versions = vec![
            AppVersion {
                label: "build-142".into(),
                description: "fix: idempotent retries".into(),
                created: Some(now - chrono::Duration::hours(2)),
            },
            AppVersion {
                label: "build-141".into(),
                description: "feat: /metrics endpoint".into(),
                created: Some(now - chrono::Duration::days(1)),
            },
        ];
        let body = super::format_deploy_preview("uflexi-prod", "build-141", "build-142", &versions);
        assert!(body.contains("env:        uflexi-prod"));
        assert!(body.contains("current:    build-141"));
        assert!(body.contains("candidate:  build-142"));
        assert!(body.contains("fix: idempotent retries"));
        // Newer candidate → no rollback warning.
        assert!(!body.contains("rollback"));
    }

    #[test]
    fn format_deploy_preview_rollback_warning_fires_when_older() {
        use crate::aws::AppVersion;
        let now = chrono::Utc::now();
        let versions = vec![
            AppVersion {
                label: "build-old".into(),
                description: String::new(),
                created: Some(now - chrono::Duration::days(7)),
            },
            AppVersion {
                label: "build-new".into(),
                description: String::new(),
                created: Some(now - chrono::Duration::hours(1)),
            },
        ];
        // Deploying the OLDER version on top of the NEWER one → rollback.
        let body = super::format_deploy_preview("uflexi-prod", "build-new", "build-old", &versions);
        assert!(
            body.contains("rollback"),
            "expected rollback warning, got: {body}"
        );
    }

    #[test]
    fn format_deploy_preview_unknown_label_calls_out_the_gap() {
        use crate::aws::AppVersion;
        let versions = vec![AppVersion {
            label: "build-141".into(),
            description: String::new(),
            created: Some(chrono::Utc::now()),
        }];
        let body = super::format_deploy_preview(
            "uflexi-prod",
            "build-141",
            "build-DOES-NOT-EXIST",
            &versions,
        );
        assert!(body.contains("not found"));
        assert!(body.contains("build-DOES-NOT-EXIST"));
    }

    fn make_event(msg: &str) -> crate::aws::Event {
        crate::aws::Event {
            at: Some(chrono::Utc::now()),
            env: "uflexi-prod".into(),
            application: "uflexi".into(),
            message: msg.into(),
            severity: "INFO".into(),
        }
    }

    #[test]
    fn classify_update_kind_deploy_extracts_label() {
        let evs = vec![make_event(
            "Updating environment uflexi-prod to use version label 'build-142'.",
        )];
        match super::classify_update_kind(&evs) {
            super::UpdateKind::Deploy { version_label } => {
                assert_eq!(version_label.as_deref(), Some("build-142"));
            }
            other => panic!("expected Deploy, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_kind_deploy_without_label_still_classifies() {
        let evs = vec![make_event("Deploying new version to instance i-abc123.")];
        match super::classify_update_kind(&evs) {
            super::UpdateKind::Deploy { version_label } => {
                // Label can't be extracted from this message shape — that's
                // fine, it's still a Deploy.
                assert!(version_label.is_none());
            }
            other => panic!("expected Deploy, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_kind_platform_update() {
        let evs = vec![make_event(
            "Updating environment to use platform 'arn:aws:elasticbeanstalk:…:platform/Corretto 17'.",
        )];
        // Even though the message also contains 'platform', deploy
        // pattern (`version label`) isn't matched, so we fall through
        // to the platform branch.
        assert_eq!(
            super::classify_update_kind(&evs),
            super::UpdateKind::Platform
        );
    }

    #[test]
    fn classify_update_kind_config_change() {
        let evs = vec![make_event("Updating environment configuration completed.")];
        assert_eq!(super::classify_update_kind(&evs), super::UpdateKind::Config);
    }

    #[test]
    fn classify_update_kind_scale_event() {
        let evs = vec![make_event("Adding instance 'i-abc123' to environment.")];
        assert_eq!(super::classify_update_kind(&evs), super::UpdateKind::Scale);
    }

    #[test]
    fn classify_update_kind_unknown_message_falls_through_to_generic() {
        let evs = vec![make_event("Something cryptic happened.")];
        assert_eq!(
            super::classify_update_kind(&evs),
            super::UpdateKind::Generic
        );
    }

    #[test]
    fn classify_update_kind_picks_most_recent_match() {
        // Events are newest-first; the deploy event sits ahead of the
        // older scale event, so Deploy wins.
        let evs = vec![
            make_event("Updating environment to use version label 'build-99'."),
            make_event("Adding instance 'i-old' to environment."),
        ];
        match super::classify_update_kind(&evs) {
            super::UpdateKind::Deploy { version_label } => {
                assert_eq!(version_label.as_deref(), Some("build-99"));
            }
            other => panic!("expected Deploy from newest match, got {other:?}"),
        }
    }

    #[test]
    fn classify_update_kind_empty_events_is_generic() {
        assert_eq!(super::classify_update_kind(&[]), super::UpdateKind::Generic);
    }

    #[test]
    fn compute_red_alerts_counts_eb_red_and_worker_dlq() {
        use crate::aws::Environment;
        let mk = |name: &str, tier: &str, health: &str| Environment {
            name: name.into(),
            application: "uflexi".into(),
            status: "Ready".into(),
            health: health.into(),
            platform: "Java 17".into(),
            tier: tier.into(),
            cname: String::new(),
            version_label: String::new(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        };
        let envs = vec![
            mk("web-prod", "Web", "Green"),
            mk("web-red", "Web", "Red"),
            mk("worker-green-dlq", "Worker", "Green"),
            mk("worker-clean", "Worker", "Green"),
            mk("worker-red", "Worker", "Severe"),
        ];
        let mut dlq = std::collections::HashMap::new();
        dlq.insert("worker-green-dlq".to_string(), 3);
        dlq.insert("worker-clean".to_string(), 0);
        // EB-Red + DLQ-Red + EB-Red-on-worker = 3 alerts (worker-red counted once).
        assert_eq!(super::compute_red_alerts(&envs, &dlq), 3);
    }

    #[test]
    fn compute_red_alerts_ignores_dlq_for_web_tier() {
        use crate::aws::Environment;
        let env = Environment {
            name: "web-prod".into(),
            application: "uflexi".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: "Java 17".into(),
            tier: "Web".into(),
            cname: String::new(),
            version_label: String::new(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        };
        // Even with a spurious "web-prod" entry in dlq_depths, a Web env
        // never counts as DLQ-red. Belt-and-braces against a stale cache
        // entry surviving a tier change.
        let mut dlq = std::collections::HashMap::new();
        dlq.insert("web-prod".to_string(), 99);
        assert_eq!(super::compute_red_alerts(&[env], &dlq), 0);
    }

    #[test]
    fn compute_red_alerts_zero_dlq_is_not_alert_worthy() {
        use crate::aws::Environment;
        let env = Environment {
            name: "worker-clean".into(),
            application: "uflexi".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: "Java 17".into(),
            tier: "Worker".into(),
            cname: String::new(),
            version_label: String::new(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        };
        let mut dlq = std::collections::HashMap::new();
        dlq.insert("worker-clean".to_string(), 0);
        assert_eq!(super::compute_red_alerts(&[env], &dlq), 0);
    }

    #[test]
    fn redact_for_log_preserves_length_with_block_chars() {
        assert_eq!(super::redact_for_log("540847557034", true), "▓".repeat(12));
        assert_eq!(super::redact_for_log("540847557034", false), "540847557034");
        // Em-dash placeholder + empty stay readable so the context line
        // doesn't render `▓` for "no account known yet".
        assert_eq!(super::redact_for_log("—", true), "—");
        assert_eq!(super::redact_for_log("", true), "");
    }

    #[test]
    fn parse_tag_args_happy_path() {
        let v: Vec<&str> = vec!["Owner", "platform-team"];
        let (k, v) = super::parse_tag_args(&v).unwrap();
        assert_eq!(k, "Owner");
        assert_eq!(v, "platform-team");
    }

    #[test]
    fn parse_tag_args_joins_value_tokens_with_spaces() {
        let v: Vec<&str> = vec!["Description", "owned", "by", "platform"];
        let (k, v) = super::parse_tag_args(&v).unwrap();
        assert_eq!(k, "Description");
        assert_eq!(v, "owned by platform");
    }

    #[test]
    fn parse_tag_args_rejects_missing_value() {
        // Bare key with no value tokens.
        let v: Vec<&str> = vec!["Owner"];
        assert!(super::parse_tag_args(&v).is_none());
        // Empty input.
        let v: Vec<&str> = vec![];
        assert!(super::parse_tag_args(&v).is_none());
    }

    #[test]
    fn delta_toast_key_returns_none_for_non_delta_text() {
        assert_eq!(super::delta_toast_key("refreshing…"), None);
        assert_eq!(super::delta_toast_key(""), None);
        assert_eq!(super::delta_toast_key("▲"), None);
        // Arrow with no count.
        assert_eq!(super::delta_toast_key("▲ Red"), None);
        // Arrow + count but no bucket word.
        assert_eq!(super::delta_toast_key("▲5 "), None);
    }

    #[test]
    fn assign_app_colors_stable_first_appearance() {
        use ratatui::style::Color;
        let palette = vec![Color::Red, Color::Green, Color::Blue];
        let names = ["app-a", "app-b", "app-a", "app-c", "app-b"];
        let m = assign_app_colors(names.iter().copied(), &palette);
        assert_eq!(m.get("app-a").copied(), Some(Color::Red));
        assert_eq!(m.get("app-b").copied(), Some(Color::Green));
        assert_eq!(m.get("app-c").copied(), Some(Color::Blue));
        assert_eq!(m.len(), 3);
    }

    #[test]
    fn assign_app_colors_wraps_when_palette_exhausted() {
        use ratatui::style::Color;
        let palette = vec![Color::Red, Color::Green];
        let names = ["a", "b", "c", "d"];
        let m = assign_app_colors(names.iter().copied(), &palette);
        assert_eq!(m.get("a").copied(), Some(Color::Red));
        assert_eq!(m.get("b").copied(), Some(Color::Green));
        // c wraps back to palette[0]; d to palette[1].
        assert_eq!(m.get("c").copied(), Some(Color::Red));
        assert_eq!(m.get("d").copied(), Some(Color::Green));
    }

    #[test]
    fn assign_app_colors_empty_palette_yields_empty_map() {
        let m = assign_app_colors(["a", "b"].iter().copied(), &[]);
        assert!(m.is_empty());
    }

    #[test]
    fn rotate_if_oversize_renames_when_too_big() {
        let dir = std::env::temp_dir().join(format!("ebman-rotate-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("audit.log");
        let backup = dir.join("audit.log.1");
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&backup);
        // Write 100 bytes; rotation threshold = 50.
        std::fs::write(&path, vec![b'x'; 100]).unwrap();
        rotate_if_oversize(&path, 50);
        assert!(!path.exists(), "current file should have been renamed");
        assert!(backup.exists(), "rotated backup should now exist");
        let _ = std::fs::remove_file(&backup);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn rotate_if_oversize_leaves_small_files_alone() {
        let dir = std::env::temp_dir().join(format!("ebman-rotate-small-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("audit.log");
        let _ = std::fs::remove_file(&path);
        std::fs::write(&path, b"tiny").unwrap();
        rotate_if_oversize(&path, 1_000);
        assert!(path.exists());
        assert!(!dir.join("audit.log.1").exists());
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_dir(&dir);
    }

    #[test]
    fn event_time_format_cycles_utc_local_age() {
        let f = EventTimeFormat::default();
        assert_eq!(f, EventTimeFormat::Utc);
        assert_eq!(f.next(), EventTimeFormat::Local);
        assert_eq!(f.next().next(), EventTimeFormat::Age);
        assert_eq!(f.next().next().next(), EventTimeFormat::Utc);
    }

    #[test]
    fn event_time_format_parse_round_trips() {
        for f in [
            EventTimeFormat::Utc,
            EventTimeFormat::Local,
            EventTimeFormat::Age,
        ] {
            assert_eq!(EventTimeFormat::parse(f.label()), Some(f));
        }
        // Case-insensitive + the "relative" alias for age.
        assert_eq!(EventTimeFormat::parse("UTC"), Some(EventTimeFormat::Utc));
        assert_eq!(
            EventTimeFormat::parse("relative"),
            Some(EventTimeFormat::Age)
        );
        assert_eq!(EventTimeFormat::parse("nonsense"), None);
    }

    #[test]
    fn shell_quote_passes_safe_chars_unchanged() {
        assert_eq!(shell_quote("safe-Name_1.0"), "safe-Name_1.0");
        assert_eq!(shell_quote("with space"), "'with space'");
        // Single quote escape uses POSIX trick: '\''
        assert_eq!(shell_quote("o'clock"), "'o'\\''clock'");
    }

    #[test]
    fn instance_hourly_usd_known_types() {
        assert!(instance_hourly_usd("t3.micro").unwrap() > 0.0);
        assert!(instance_hourly_usd("m5.large").unwrap() > 0.0);
        assert_eq!(instance_hourly_usd("not-a-real-type"), None);
    }

    #[test]
    fn estimate_cost_handles_mixed() {
        let mk = |t: &str, az: &str| Instance {
            id: "i-1".into(),
            health: "Ok".into(),
            color: "Green".into(),
            causes: vec![],
            instance_type: t.into(),
            availability_zone: az.into(),
            launched_at: None,
        };
        let instances = vec![
            mk("t3.micro", "us-east-1a"),
            mk("t3.micro", "us-east-1b"),
            mk("unknown-type-xyz", "us-east-1c"),
        ];
        let (hourly, missing) = estimate_cost(&instances);
        assert_eq!(missing, 1);
        // Two t3.micro at $0.0104/hr each.
        assert!((hourly - 0.0208).abs() < 1e-9);
    }

    fn fake_env(name: &str, status: &str, health: &str, version: &str) -> Environment {
        Environment {
            name: name.into(),
            application: "my-app".into(),
            status: status.into(),
            health: health.into(),
            platform: "Java 17".into(),
            tier: "Web".into(),
            cname: format!("{name}.elb.amazonaws.com"),
            version_label: version.into(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        }
    }

    #[test]
    fn palette_score_prefers_label_prefix_then_substring_then_detail() {
        // Empty needle returns score 0 for everything.
        assert_eq!(palette_score("", "anything", "anything"), Some(0));
        // Label prefix → 0.
        assert_eq!(palette_score("reg", "region", "switch AWS region"), Some(0));
        // Label substring later in string → higher score.
        let s_label = palette_score("ion", "region", "switch AWS region").unwrap();
        assert!(s_label > 0 && s_label < 1_000);
        // Detail-only match is penalised by +1000 vs label.
        let s_detail = palette_score("aws", ":region", "switch AWS profile").unwrap();
        let s_label_match = palette_score("aws", "aws-thing", "irrelevant").unwrap();
        assert!(s_detail >= 1_000);
        assert!(s_label_match < s_detail);
        // No match → None.
        assert_eq!(palette_score("xyzzy", "region", "switch AWS region"), None);
    }

    #[test]
    fn bucket_delta_only_envs_in_both() {
        let mut prev = HashMap::new();
        prev.insert("a".into(), "Green".into());
        prev.insert("b".into(), "Red".into());
        prev.insert("c".into(), "Green".into()); // c disappears in next, so dropped from delta
        let next = vec![
            fake_env("a", "Ready", "Yellow", "v1"), // Green → Yellow: −1 Green, +1 Yellow
            fake_env("b", "Ready", "Red", "v1"),    // Red → Red: no change
            fake_env("d", "Ready", "Green", "v1"),  // new env: ignored (no prev state)
        ];
        let delta = bucket_delta(&prev, &next, |e| e.health.clone());
        let map: BTreeMap<String, i32> = delta.into_iter().collect();
        // Only env `a` transitions: −1 Green, +1 Yellow. b unchanged; c disappeared (ignored); d is new (ignored).
        assert_eq!(map.get("Green").copied(), Some(-1));
        assert_eq!(map.get("Yellow").copied(), Some(1));
        assert_eq!(map.get("Red").copied(), None);
    }

    #[test]
    fn bucket_delta_empty_prev_yields_no_deltas() {
        // Regression: when prev_health is cleared (e.g. on context switch),
        // the delta against the new env list should produce nothing. Otherwise
        // every env shows up as a transition.
        let prev = HashMap::new();
        let next = vec![
            fake_env("a", "Ready", "Green", "v1"),
            fake_env("b", "Ready", "Red", "v1"),
        ];
        let delta = bucket_delta(&prev, &next, |e| e.health.clone());
        assert!(
            delta.is_empty(),
            "expected no deltas with empty prev, got {delta:?}"
        );
    }

    #[test]
    fn diff_envs_marks_differing_fields() {
        let a = fake_env("prod", "Ready", "Green", "v1");
        let b = fake_env("staging", "Updating", "Yellow", "v2");
        let out = diff_envs(&a, &b, false);
        // Differing fields prefixed by ≠
        assert!(out.contains("≠ Status"));
        assert!(out.contains("≠ Health"));
        assert!(out.contains("≠ Version"));
        assert!(out.contains("≠ Name"));
        assert!(out.contains("≠ CNAME"));
        // Identical fields prefixed by space
        assert!(out.contains("  Application"));
        assert!(out.contains("  Tier"));
        assert!(out.contains("  Platform"));
    }

    #[test]
    fn diff_envs_redacts_cname() {
        let a = fake_env("prod", "Ready", "Green", "v1");
        let b = fake_env("staging", "Updating", "Yellow", "v2");
        let out = diff_envs(&a, &b, true);
        // CNAMEs become blocks; the canonical envname-portion shouldn't survive.
        assert!(!out.contains("prod.elb.amazonaws.com"));
        assert!(out.contains("▓"));
    }

    #[test]
    fn format_alarms_handles_empty_and_error() {
        let none = format_alarms(Ok(vec![]));
        assert!(none.contains("no CloudWatch alarms"));
        let err = format_alarms(Err("boom".into()));
        assert!(err.contains("error"));
        let alarms = format_alarms(Ok(vec![CwAlarm {
            name: "high-cpu".into(),
            state: "ALARM".into(),
            state_reason: "CPU > 80%".into(),
            metric_name: "CPUUtilization".into(),
            namespace: "AWS/EC2".into(),
        }]));
        assert!(alarms.contains("ALARM"));
        assert!(alarms.contains("high-cpu"));
        assert!(alarms.contains("CPU > 80%"));
    }

    #[test]
    fn view_round_trips() {
        // We can't easily construct an App in tests, but encode_view's format
        // is straightforward — check a hand-built snap round-trips through
        // parse_sort and the trivial fields.
        let snap = "filter=prod;sort=health:desc;grouped=true;scope=apps";
        let mut got_filter = String::new();
        let mut got_sort = (SortKey::App, false);
        let mut got_grouped = false;
        let mut got_scope = Scope::Envs;
        for part in snap.split(';') {
            let (k, v) = part.split_once('=').unwrap();
            match k {
                "filter" => got_filter = v.into(),
                "sort" => got_sort = parse_sort(Some(v)),
                "grouped" => got_grouped = v == "true",
                "scope" => {
                    got_scope = if v == "apps" {
                        Scope::Apps
                    } else {
                        Scope::Envs
                    }
                }
                _ => {}
            }
        }
        assert_eq!(got_filter, "prod");
        assert_eq!(got_sort, (SortKey::Health, true));
        assert!(got_grouped);
        assert_eq!(got_scope, Scope::Apps);
    }

    #[test]
    fn view_mode_cycle_includes_spacious() {
        assert_eq!(ViewMode::Default.next(), ViewMode::Compact);
        assert_eq!(ViewMode::Compact.next(), ViewMode::Spacious);
        assert_eq!(ViewMode::Spacious.next(), ViewMode::Default);
        assert_eq!(ViewMode::Spacious.label(), "spacious");
    }

    #[test]
    fn md_escape_protects_pipes_and_backslashes() {
        assert_eq!(md_escape("simple"), "simple");
        assert_eq!(md_escape("a|b|c"), "a\\|b\\|c");
        assert_eq!(md_escape("back\\slash"), "back\\\\slash");
        assert_eq!(md_escape("a\\|b"), "a\\\\\\|b");
    }

    #[test]
    fn describe_env_dumps_known_fields() {
        let env = Environment {
            name: "my-env".into(),
            application: "my-app".into(),
            status: "Ready".into(),
            health: "Green".into(),
            platform: "Java 17".into(),
            tier: "Web".into(),
            cname: "my-env.elb.amazonaws.com".into(),
            version_label: "v42".into(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        };
        let text = describe_env(&env);
        assert!(text.contains("\"name\""));
        assert!(text.contains("my-env"));
        assert!(text.contains("\"updated\":         null"));
    }

    #[test]
    fn detail_tab_titles_are_distinct() {
        use std::collections::HashSet;
        let titles: HashSet<&str> = [
            DetailTab::Health,
            DetailTab::Events,
            DetailTab::Instances,
            DetailTab::Metrics,
            DetailTab::Queue,
            DetailTab::Config,
        ]
        .iter()
        .map(|t| t.title())
        .collect();
        assert_eq!(titles.len(), 6);
    }

    // ── UI integration harness ──────────────────────────────────────
    //
    // These tests drive `crossterm::Event`s through `handle_event` and
    // (optionally) render to a `ratatui::TestBackend`-backed Terminal
    // to inspect the resulting buffer. The harness uses `App::for_tests`
    // — synchronous, no AWS network, no disk reads — so each test starts
    // from a known clean state.
    //
    // What this catches that the pure-helper tests don't:
    //   - Mode-transition glitches (overlay closes correctly, Filter
    //     mode swallows printable keys, etc.)
    //   - Key-precedence regressions (Mode::Picker over LogTail
    //     overlay, ESC routing, Tab cycling)
    //   - Render-side state-dependent bugs (a field is None, the
    //     renderer panics; an overlay shape changes, the dispatch
    //     desyncs).
    //
    // Pattern:
    //   1. `let mut app = test_app();` — clean App.
    //   2. Mutate state as needed (push fake envs onto `app.environments`,
    //      flip toggles, etc.). The struct is fully `pub` so tests can
    //      seed any shape without going through async fetchers.
    //   3. `press(&mut app, KeyCode::*, KeyModifiers::*)` — feed a key.
    //   4. Assert on `app.<field>` — or render to a buffer string via
    //      `render(&mut app, w, h)` and grep.

    use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

    /// Build a minimal App in a deterministic state. Useful for tests
    /// that don't care about real AWS data — just keyboard flow + mode
    /// transitions. Seed envs / overlays / detail state by mutating
    /// the returned App directly.
    fn test_app() -> App {
        // Match the unicode/dark defaults so the renderer's per-theme
        // branches are exercised on the common path.
        let cfg = crate::config::Config {
            theme: "dark".into(),
            icons: "unicode".into(),
            ..crate::config::Config::default()
        };
        App::for_tests(crate::aws::AwsClient::stub(), cfg)
    }

    /// Synthesize a `KeyEvent::Press` and dispatch it through
    /// `handle_event`. Mirrors how `run()` feeds real terminal events.
    fn press(app: &mut App, code: KeyCode, mods: KeyModifiers) {
        app.handle_event(Event::Key(KeyEvent {
            code,
            modifiers: mods,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        }));
    }

    /// Render the App into a fixed-size `TestBackend` buffer and return
    /// the flattened string (one row per line, joined with `\n`).
    /// Useful for grep-style assertions on rendered output.
    fn render(app: &mut App, w: u16, h: u16) -> String {
        use ratatui::backend::TestBackend;
        use ratatui::Terminal;
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| crate::ui::draw(f, app)).expect("draw");
        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..buf.area.height {
            for x in 0..buf.area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn mk_env(name: &str, app: &str, tier: &str, health: &str) -> crate::aws::Environment {
        crate::aws::Environment {
            name: name.into(),
            application: app.into(),
            status: "Ready".into(),
            health: health.into(),
            platform: "Java 17".into(),
            tier: tier.into(),
            cname: format!("{name}.example.com"),
            version_label: "build-1".into(),
            arn: Some(format!("arn:aws:eb:us-east-1:0:env/{name}")),
            updated: None,
            id: None,
            region: None,
        }
    }

    #[tokio::test]
    async fn tab_cycles_scope_envs_to_apps_and_back() {
        let mut app = test_app();
        assert_eq!(app.scope, Scope::Envs);
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.scope, Scope::Apps);
        press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.scope, Scope::Envs);
    }

    #[tokio::test]
    async fn question_mark_opens_help_and_escape_dismisses_it() {
        let mut app = test_app();
        assert_eq!(app.mode, Mode::Normal);
        press(&mut app, KeyCode::Char('?'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Help);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
    }

    #[tokio::test]
    async fn colon_enters_command_mode_and_esc_cancels() {
        let mut app = test_app();
        press(&mut app, KeyCode::Char(':'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Command);
        // Typed chars land in the command input buffer.
        press(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
        assert_eq!(app.command_input, "q");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
        // Input cleared on cancel.
        assert!(app.command_input.is_empty());
    }

    #[tokio::test]
    async fn slash_enters_filter_mode_and_text_lands() {
        let mut app = test_app();
        // Seed an env so filter has something to operate on.
        app.environments = vec![
            mk_env("prod-web", "uflexi", "Web", "Green"),
            mk_env("staging-web", "uflexi", "Web", "Green"),
        ];
        app.rebuild_view();
        press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Filter);
        for c in "prod".chars() {
            press(&mut app, KeyCode::Char(c), KeyModifiers::NONE);
        }
        assert_eq!(app.filter, "prod");
        // Esc clears the filter and returns to Normal.
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.mode, Mode::Normal);
        assert!(app.filter.is_empty());
    }

    #[tokio::test]
    async fn enter_on_red_env_opens_why_via_bang_keybind() {
        let mut app = test_app();
        // Seed a Red env + select it.
        app.environments = vec![mk_env("prod-web", "uflexi", "Web", "Red")];
        app.rebuild_view();
        app.table_state.select(Some(0));
        // `!` shortcut in Envs scope opens the :why overlay.
        press(&mut app, KeyCode::Char('!'), KeyModifiers::NONE);
        assert!(
            matches!(app.current_overlay, Some(Overlay::WhyRed { .. })),
            "expected WhyRed overlay, got {:?}",
            app.current_overlay
        );
    }

    #[tokio::test]
    async fn render_main_table_includes_seeded_env_name() {
        let mut app = test_app();
        app.environments = vec![mk_env("api-prod-canary", "uflexi", "Web", "Green")];
        app.rebuild_view();
        let frame = render(&mut app, 160, 24);
        assert!(
            frame.contains("api-prod-canary"),
            "rendered frame should show seeded env name; got:\n{frame}"
        );
    }

    #[tokio::test]
    async fn ctrl_x_toggles_redact() {
        let mut app = test_app();
        assert!(!app.redact);
        press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert!(app.redact);
        press(&mut app, KeyCode::Char('x'), KeyModifiers::CONTROL);
        assert!(!app.redact);
    }

    /// Helper for the cancel-window tests — build a ConfirmModal for
    /// the given Action / env. Mirrors the shape `advance_action_flow`
    /// produces; pre-flight fields stay None (the cancel-window code
    /// path doesn't read them).
    fn mk_modal(action: Action, env: &str) -> ConfirmModal {
        ConfirmModal {
            action,
            target_env: env.into(),
            swap_with: None,
            typed: String::new(),
            kind: ConfirmKind::YesNo,
            dryrun: None,
            loading_dryrun: false,
            recent_events: None,
            loading_events: false,
            traffic_warning: None,
            deploy_version: None,
            upgrade_platform_arn: None,
            upgrade_platform_label: None,
            clone_target: None,
            scale_min: None,
            scale_max: None,
        }
    }

    #[tokio::test]
    async fn queue_action_dispatch_holds_action_for_cancel_window() {
        let mut app = test_app();
        let modal = mk_modal(Action::Rebuild, "uflexi-prod");
        app.queue_action_dispatch(modal);
        let pd = app
            .pending_dispatch
            .as_ref()
            .expect("queue should set pending_dispatch");
        assert_eq!(pd.target, "uflexi-prod");
        assert!(
            matches!(pd.kind, PendingDispatchKind::Single { .. }),
            "queue_action_dispatch should produce a Single variant"
        );
        assert!(
            pd.deadline > std::time::Instant::now(),
            "deadline must be in the future"
        );
        let remaining = pd
            .deadline
            .saturating_duration_since(std::time::Instant::now());
        assert!(
            remaining <= UNDO_WINDOW && remaining >= UNDO_WINDOW - Duration::from_millis(500),
            "deadline should be roughly UNDO_WINDOW from now; got {remaining:?}"
        );
    }

    #[tokio::test]
    async fn cancel_pending_dispatch_clears_field_and_emits_status() {
        let mut app = test_app();
        app.queue_action_dispatch(mk_modal(Action::Terminate, "uflexi-prod"));
        assert!(app.pending_dispatch.is_some());
        app.cancel_pending_dispatch();
        assert!(app.pending_dispatch.is_none());
        let msg = app.status_message.as_deref().unwrap_or("");
        assert!(
            msg.contains("undone") && msg.contains("uflexi-prod"),
            "status should mention the undo + env; got: {msg:?}"
        );
    }

    #[tokio::test]
    async fn second_queue_attempt_errors_while_first_pending() {
        let mut app = test_app();
        app.queue_action_dispatch(mk_modal(Action::Rebuild, "first"));
        assert!(app.pending_dispatch.is_some());
        let first_deadline = app.pending_dispatch.as_ref().unwrap().deadline;
        // Second queue attempt is rejected; first dispatch is untouched.
        app.queue_action_dispatch(mk_modal(Action::Rebuild, "second"));
        assert_eq!(
            app.pending_dispatch.as_ref().unwrap().target,
            "first",
            "second queue must not replace the first"
        );
        assert_eq!(
            app.pending_dispatch.as_ref().unwrap().deadline,
            first_deadline,
            "second queue must not bump the deadline"
        );
        assert!(
            app.error_message
                .as_deref()
                .unwrap_or("")
                .contains("press U to undo"),
            "second queue should surface a useful error"
        );
    }

    #[tokio::test]
    async fn tick_pending_dispatch_fires_after_deadline() {
        let mut app = test_app();
        // Forge a pending dispatch whose deadline has already elapsed
        // so tick_pending_dispatch fires synchronously without us
        // having to wait 5 seconds.
        let modal = mk_modal(Action::Rebuild, "expired");
        app.pending_dispatch = Some(PendingDispatch {
            deadline: std::time::Instant::now() - Duration::from_millis(1),
            label: "Rebuild env".into(),
            target: "expired".into(),
            kind: PendingDispatchKind::Single { modal },
        });
        app.tick_pending_dispatch();
        assert!(
            app.pending_dispatch.is_none(),
            "expired tick should clear the field (dispatch handed to spawn_action)"
        );
    }

    #[tokio::test]
    async fn batch_action_routes_through_cancel_window() {
        let mut app = test_app();
        app.environments = vec![
            mk_env("prod-web", "uflexi", "Web", "Green"),
            mk_env("staging-web", "uflexi", "Web", "Green"),
        ];
        app.multi_selected.insert("prod-web".into());
        app.multi_selected.insert("staging-web".into());
        app.cmd_batch_action(Action::Rebuild);
        // Multi-select cleared; dispatch queued with a 5s deadline.
        assert!(
            app.multi_selected.is_empty(),
            "multi-select should clear once the batch is queued"
        );
        let pd = app
            .pending_dispatch
            .as_ref()
            .expect("batch action should queue a pending dispatch");
        match &pd.kind {
            PendingDispatchKind::BatchAction { action, env_names } => {
                assert_eq!(*action, Action::Rebuild);
                assert_eq!(env_names.len(), 2);
            }
            other => panic!(
                "expected BatchAction variant; got {other:?}",
                other = match other {
                    PendingDispatchKind::Single { .. } => "Single",
                    PendingDispatchKind::BatchAction { .. } => "BatchAction",
                    PendingDispatchKind::BatchDeploy { .. } => "BatchDeploy",
                    PendingDispatchKind::BatchTag { .. } => "BatchTag",
                    PendingDispatchKind::BatchSetOption { .. } => "BatchSetOption",
                }
            ),
        }
    }

    #[tokio::test]
    async fn batch_action_undo_cancels_whole_fanout() {
        let mut app = test_app();
        app.environments = vec![
            mk_env("e1", "uflexi", "Web", "Green"),
            mk_env("e2", "uflexi", "Web", "Green"),
            mk_env("e3", "uflexi", "Web", "Green"),
        ];
        for name in ["e1", "e2", "e3"] {
            app.multi_selected.insert(name.into());
        }
        app.cmd_batch_action(Action::RestartAppServer);
        assert!(app.pending_dispatch.is_some());
        app.cancel_pending_dispatch();
        assert!(
            app.pending_dispatch.is_none(),
            "cancel should drop the whole batch, not just one env"
        );
        let msg = app.status_message.as_deref().unwrap_or("");
        assert!(
            msg.contains("undone") && msg.contains("3 env(s)"),
            "status should call out the 3-env batch; got: {msg:?}"
        );
    }

    #[tokio::test]
    async fn apps_scope_space_toggles_apps_selected() {
        let mut app = test_app();
        // Seed two apps + select Apps scope.
        app.applications = vec![
            crate::aws::Application {
                name: "billing".into(),
                description: String::new(),
                date_created: None,
                date_updated: None,
                version_count: 0,
                templates: vec![],
                latest_version_label: None,
                latest_version_created: None,
            },
            crate::aws::Application {
                name: "checkout".into(),
                description: String::new(),
                date_created: None,
                date_updated: None,
                version_count: 0,
                templates: vec![],
                latest_version_label: None,
                latest_version_created: None,
            },
        ];
        app.set_scope(Scope::Apps);
        app.app_table_state.select(Some(0));
        // First space adds; second space removes.
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.apps_selected.contains("billing"));
        press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(!app.apps_selected.contains("billing"));
    }

    #[tokio::test]
    async fn apps_scope_star_pins_and_unpins_app() {
        let mut app = test_app();
        app.applications = vec![crate::aws::Application {
            name: "billing".into(),
            description: String::new(),
            date_created: None,
            date_updated: None,
            version_count: 0,
            templates: vec![],
            latest_version_label: None,
            latest_version_created: None,
        }];
        app.set_scope(Scope::Apps);
        app.app_table_state.select(Some(0));
        assert!(!app.pinned_apps.contains("billing"));
        press(&mut app, KeyCode::Char('*'), KeyModifiers::SHIFT);
        assert!(app.pinned_apps.contains("billing"));
        press(&mut app, KeyCode::Char('*'), KeyModifiers::SHIFT);
        assert!(!app.pinned_apps.contains("billing"));
    }

    #[tokio::test]
    async fn esc_clears_apps_selected_when_no_envs_selected() {
        let mut app = test_app();
        app.apps_selected.insert("billing".into());
        app.apps_selected.insert("checkout".into());
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.apps_selected.is_empty());
    }

    #[tokio::test]
    async fn capital_u_cancels_pending_dispatch_in_normal_mode() {
        let mut app = test_app();
        app.queue_action_dispatch(mk_modal(Action::Rebuild, "uflexi-prod"));
        assert!(app.pending_dispatch.is_some());
        press(&mut app, KeyCode::Char('U'), KeyModifiers::SHIFT);
        assert!(
            app.pending_dispatch.is_none(),
            "capital U in Normal mode should cancel the pending dispatch"
        );
    }
}
