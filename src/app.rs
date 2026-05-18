use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use color_eyre::eyre::Result;
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
        Application, AwsClient, AwsContext, CwAlarm, Environment, Event as EbEvent, Identity,
        Instance, MetricSeries, QueueMessage, WorkerQueues,
    },
    config::Config,
    profiles,
    state::{self, PersistedState},
    theme::{IconStyle, Theme},
    ui, Tui,
};

/// Names of all built-in `:commands`. Used to detect collisions when loading
/// user plugins from `commands.toml` — plugins that shadow a built-in are
/// dropped with a warning rather than silently masking it.
pub const BUILTIN_COMMANDS: &[&str] = &[
    "q",
    "quit",
    "refresh",
    "help",
    "?",
    "region",
    "r",
    "profile",
    "p",
    "sort",
    "group",
    "redact",
    "events",
    "export",
    "json",
    "report",
    "markdown",
    "readonly",
    "pin",
    "alias",
    "alias-drop",
    "alias-rm",
    "whatsnew",
    "history",
    "saved-configs",
    "configs",
    "plugins",
    "diff",
    "alarms",
    "loglevel",
    "cols",
    "save-view",
    "view",
    "views",
    "view-drop",
    "filter",
    "f",
    "save",
    "drop",
    "filters",
];

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

const HISTORY_CAP: usize = 20;
const MESSAGE_LOG_CAP: usize = 50;
const TOAST_CAP: usize = 4;

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
    /// EB saved-configuration templates per app shown via `:saved-configs`.
    SavedConfigs(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // Success reserved for future success-specific toasts.
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

pub struct DlqState {
    pub env_name: String,
    pub main_queue_url: String,
    pub dlq_url: String,
    pub messages: Vec<QueueMessage>,
    pub list_state: ListState,
    pub loading: bool,
    pub error: Option<String>,
    pub confirm_purge: bool,
    pub purge_typed: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Rebuild,
    RestartAppServer,
    SwapCnames,
    Terminate,
}

impl Action {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rebuild => "Rebuild environment",
            Self::RestartAppServer => "Restart app server",
            Self::SwapCnames => "Swap CNAMEs with another env",
            Self::Terminate => "Terminate environment",
        }
    }
    pub fn destructive(self) -> bool {
        matches!(self, Self::Terminate)
    }
}

pub enum ActionFlow {
    Menu {
        list_state: ListState,
    },
    SwapTarget {
        source: String,
        picker: Picker,
    },
    Confirm(ConfirmModal),
    Running {
        action: Action,
        env: String,
        since: Instant,
    },
}

#[derive(Clone)]
pub struct ConfirmModal {
    pub action: Action,
    pub target_env: String,
    pub swap_with: Option<String>,
    pub typed: String,
    pub kind: ConfirmKind,
    pub dryrun: Option<DryRunInfo>,
    pub loading_dryrun: bool,
    pub recent_events: Option<Vec<EbEvent>>,
    pub loading_events: bool,
    /// One-line pre-flight warning derived from the env's current state when
    /// the modal opened — e.g. "ACTIVE DEPLOY: status=Updating" or "RECENT
    /// CHANGE: updated 4m ago". Surfaced above the Y/N row so the operator
    /// sees mid-flight changes before authorising another action.
    pub traffic_warning: Option<String>,
}

#[derive(Clone, Debug)]
pub struct DryRunInfo {
    pub instance_count: usize,
    pub az_count: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    YesNo,
    TypeName, // user must type the env name exactly
}

pub const ACTIONS: &[Action] = &[
    Action::Rebuild,
    Action::RestartAppServer,
    Action::SwapCnames,
    Action::Terminate,
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    Events,
    Instances,
    Metrics,
    Queue,
    Logs,
    Config,
}

impl DetailTab {
    pub fn title(self) -> &'static str {
        match self {
            Self::Events => "Events",
            Self::Instances => "Instances",
            Self::Metrics => "Metrics",
            Self::Queue => "Queue",
            Self::Logs => "Logs",
            Self::Config => "Config",
        }
    }
}

/// Per-instance tail-log capture state.
#[derive(Debug, Clone, Default)]
pub struct LogTail {
    /// `(ec2_instance_id, last_known_content)` — content is empty until the
    /// first fetch lands. Order is preserved across refreshes so the user
    /// doesn't see instance entries jump around.
    pub by_instance: Vec<(String, String)>,
    /// Sub-state of the request/poll/fetch pipeline.
    pub stage: LogTailStage,
    /// Last `RetrieveEnvironmentInfo` poll attempt (1-based; 0 = haven't polled).
    pub poll_attempt: u32,
    /// Sticky error so the user can see why the tail failed.
    pub error: Option<String>,
    /// When set, the tab is filtered to lines matching this regex.
    pub search_input: String,
    pub search_active: bool,
    pub search_pattern: Option<regex::Regex>,
    pub search_error: Option<String>,
    pub scroll: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogTailStage {
    /// Tab opened but no request issued (or context cleared).
    #[default]
    Idle,
    /// `RequestEnvironmentInfo` in flight.
    Requesting,
    /// `RetrieveEnvironmentInfo` poll loop in flight.
    Polling,
    /// At least one URL fetch in flight.
    Fetching,
    /// All content fetched — UI shows the tabbed content.
    Ready,
}

pub struct DetailState {
    pub env_name: String,
    pub env_snapshot: Environment, // taken at open-time; not refreshed
    pub tabs: Vec<DetailTab>,
    pub tab_idx: usize,
    pub events: Vec<EbEvent>,
    pub instances: Vec<Instance>,
    pub queues: WorkerQueues,
    pub metrics: Vec<MetricSeries>,
    pub metrics_range_secs: i64,
    pub auto_refresh: bool,
    pub search_input: String,
    pub search_active: bool, // true while user is typing a pattern
    pub search_pattern: Option<regex::Regex>,
    pub search_error: Option<String>,
    pub events_scroll: u16,
    pub instances_scroll: u16,
    pub tags: Vec<(String, String)>,
    pub loading_events: bool,
    pub loading_instances: bool,
    pub loading_queues: bool,
    pub loading_metrics: bool,
    pub loading_tags: bool,
    pub error: Option<String>,
    /// Tail-log state, populated when the user visits the Logs tab.
    pub log_tail: LogTail,
}

impl DetailState {
    pub fn tab(&self) -> DetailTab {
        self.tabs[self.tab_idx]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Profile,
    Region,
}

pub struct Picker {
    pub kind: PickerKind,
    pub items: Vec<String>,
    pub filter: String,
    pub list_state: ListState,
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
    pub quickjump_input: String,
    pub named_filters: BTreeMap<String, String>,
    pub extra_regions: Vec<String>,
    pub events: Vec<EbEvent>,
    pub events_visible: bool,
    pub events_scroll: u16,
    pub detail: Option<DetailState>,
    pub action_flow: Option<ActionFlow>,
    pub dlq: Option<DlqState>,
    pub theme: Arc<Theme>,
    pub view_mode: ViewMode,
    pub events_panel_height: u16,
    pub help_scroll: u16,
    pub hover_row: Option<usize>,
    pub alerts: usize, // count of envs currently in Red, recomputed each refresh
    pub frozen: bool,  // when true, auto-refresh ticker is no-op
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
    pub aliases: BTreeMap<String, String>,
    pub saved_views: BTreeMap<String, String>,
    pub hidden_cols: BTreeSet<String>,
    pub log_reload: Option<crate::LogReloadHandle>,
    pub log_directive: String,
    pub plugins: BTreeMap<String, crate::plugins::Plugin>,
    /// Snapshot of `(status_message, error_message)` captured when the current
    /// refresh was spawned. apply_refresh clears messages only if they still
    /// match this snapshot, so user-initiated status set between kickoff and
    /// apply (e.g. pressing `s` to sort during the round-trip) is preserved.
    pub status_snapshot_at_refresh: Option<(Option<String>, Option<String>)>,
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
    /// Newer ebman release advertised by crates.io, if any. Populated by the
    /// fire-and-forget update-check task that runs once at startup.
    pub update_available: Option<crate::update_check::LatestRelease>,
    pub notify_bell: bool,
    pub required_tags: Vec<String>,
    /// Webhook URL invoked once per env that transitions into Red on refresh.
    /// `None` disables the feature.
    pub webhook_url: Option<String>,
    pub newly_red: HashSet<String>,
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
    DryRunResult {
        gen: u64,
        env_name: String,
        result: Result<Vec<Instance>, String>,
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
    /// Result of the startup update-check. `None` means "no newer release"
    /// or the check couldn't reach crates.io; either way, the UI doesn't
    /// nag the user. We don't carry a generation — the message is anchored
    /// to the process, not a particular AWS context.
    UpdateCheck(Option<crate::update_check::LatestRelease>),
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
        let (aws, override_profile, override_region, identity_warning) =
            init_client(persisted.profile.clone(), persisted.region.clone()).await?;
        let aws = Arc::new(aws);
        let context = aws.context.clone();
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
        let refresh_interval = config.refresh_interval;

        let mut app_table_state = TableState::default();
        app_table_state.select(Some(0));

        let plugins_loaded = crate::plugins::load(BUILTIN_COMMANDS);
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
            quickjump_input: String::new(),
            named_filters: persisted.named_filters,
            extra_regions: config.extra_regions,
            events: Vec::new(),
            events_visible,
            events_scroll: 0,
            detail: None,
            action_flow: None,
            dlq: None,
            theme: {
                let (mut t, warning) = Theme::resolve(&config.theme);
                if let Some(w) = warning {
                    tracing::warn!("{w}");
                }
                if config.icons.trim().eq_ignore_ascii_case("ascii") {
                    t.icons = IconStyle::Ascii;
                }
                Arc::new(t)
            },
            view_mode: ViewMode::Default,
            events_panel_height: 10,
            help_scroll: 0,
            hover_row: None,
            alerts: 0,
            frozen: false,
            current_overlay: None,
            message_log: VecDeque::with_capacity(MESSAGE_LOG_CAP),
            toasts: VecDeque::with_capacity(TOAST_CAP),
            palette_input: String::new(),
            palette_items: Vec::new(),
            palette_filtered: Vec::new(),
            palette_state: ListState::default(),
            read_only: false,
            pinned: persisted.pinned,
            aliases: persisted.aliases,
            saved_views: persisted.saved_views,
            hidden_cols: persisted.hidden_cols,
            log_reload: None,
            log_directive: std::env::var("RUST_LOG")
                .unwrap_or_else(|_| "info,aws=warn,hyper=warn".to_string()),
            plugins: plugins_loaded.plugins,
            status_snapshot_at_refresh: None,
            throttle_until: None,
            consecutive_throttles: 0,
            sso_expiry: crate::sso::latest_session_expiry(),
            update_available: None,
            notify_bell: config.notify_bell,
            required_tags: config.required_tags,
            webhook_url: config.webhook_url,
            newly_red: HashSet::new(),
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
        Ok(app)
    }

    pub async fn run(&mut self, terminal: &mut Tui) -> Result<()> {
        let mut events = EventStream::new();
        let mut ticker = tokio::time::interval(self.refresh_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut anim = tokio::time::interval(Duration::from_millis(100));
        anim.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.spawn_refresh();
        self.spawn_update_check();

        loop {
            terminal.draw(|f| ui::draw(f, self))?;
            if self.quit {
                break;
            }

            let prev_status = self.status_message.clone();
            let prev_error = self.error_message.clone();

            tokio::select! {
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
                _ = anim.tick(), if self.loading_since.is_some() || !self.toasts.is_empty() => {
                    // Wake the draw loop so the spinner can advance and toasts
                    // expire promptly. Gated to keep idle CPU at zero otherwise.
                }
                Some(msg) = self.msg_rx.recv() => {
                    self.handle_msg(msg);
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
        }
        self.persist_state();
        Ok(())
    }

    fn push_toast(&mut self, kind: ToastKind, text: String) {
        // Dedupe: if an identical toast (same kind + text) is already on
        // screen, refresh its timestamp instead of stacking a duplicate.
        // Without this, a flurry of identical status updates (e.g. repeated
        // "no environment selected" key presses, or a rebuilt-context message
        // arriving twice) would push the same card N times.
        if let Some(existing) = self
            .toasts
            .iter_mut()
            .find(|t| t.text == text && t.kind == kind)
        {
            existing.shown_at = Instant::now();
            return;
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
        if self.message_log.is_empty() {
            return "no messages yet".into();
        }
        let mut out = String::new();
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
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Press => self.handle_key(key),
            Event::Mouse(m) => self.handle_mouse(m),
            _ => {}
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
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
        if let Some(overlay) = self.current_overlay.as_ref() {
            let universal = matches!(key.code, KeyCode::Esc | KeyCode::Char('q'));
            let variant_extra = match overlay {
                Overlay::Describe(_) => matches!(key.code, KeyCode::Char('d') | KeyCode::Char('D')),
                Overlay::Whatsnew(_) => matches!(key.code, KeyCode::Char('w')),
                _ => false,
            };
            if universal || variant_extra {
                self.current_overlay = None;
            }
            return;
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
                    self.mode = Mode::Normal;
                    self.help_scroll = 0;
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    self.help_scroll = self.help_scroll.saturating_add(1);
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.help_scroll = self.help_scroll.saturating_sub(1);
                }
                _ => {}
            },
            Mode::Command => match key.code {
                KeyCode::Esc => {
                    self.command_input.clear();
                    self.mode = Mode::Normal;
                }
                KeyCode::Enter => {
                    let cmd = self.command_input.clone();
                    self.command_input.clear();
                    self.mode = Mode::Normal;
                    self.execute_command(&cmd);
                }
                KeyCode::Backspace => {
                    self.command_input.pop();
                }
                KeyCode::Char(c) if is_text_input(&key) => self.command_input.push(c),
                _ => {}
            },
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
                    KeyCode::Char('a') => self.open_action_menu(),
                    KeyCode::Char('b') => self.open_in_console(),
                    KeyCode::Char('*') => self.toggle_pin_selected(),
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
            Mode::Action => self.handle_action_key(key),
            Mode::Dlq => self.handle_dlq_key(key),
            Mode::Normal => match key.code {
                KeyCode::Char('q') => self.quit = true,
                KeyCode::Tab => self.scope = self.scope.next(),
                KeyCode::BackTab => self.scope = self.scope.prev(),
                KeyCode::Enter if self.scope == Scope::Apps => self.drill_into_app(),
                KeyCode::Enter => self.open_detail(),
                KeyCode::Char('a') if self.scope == Scope::Envs => self.open_action_menu(),
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
                    self.events_panel_height = self.events_panel_height.saturating_sub(1).max(4);
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
                KeyCode::Char('y') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.export_tsv();
                }
                KeyCode::Char('w') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.yank_cli();
                }
                KeyCode::Char('y') => self.yank_selected(YankKind::Cname),
                KeyCode::Char('Y') => self.yank_selected(YankKind::Name),
                KeyCode::Char('b') if self.scope == Scope::Envs => self.open_in_console(),
                KeyCode::Char('D') if self.scope == Scope::Envs => self.open_describe_overlay(),
                KeyCode::Char('*') if self.scope == Scope::Envs => self.toggle_pin_selected(),
                KeyCode::Char('f') if self.scope == Scope::Envs => {
                    self.frozen = !self.frozen;
                    self.status_message = Some(if self.frozen {
                        "frozen — auto-refresh paused".into()
                    } else {
                        "unfrozen".into()
                    });
                }
                KeyCode::Char(c @ '1'..='9') => self.quick_jump((c as u8 - b'0') as usize),
                KeyCode::Char('?') => self.mode = Mode::Help,
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
                KeyCode::Char('j') | KeyCode::Down => self.move_scope_selection(1),
                KeyCode::Char('k') | KeyCode::Up => self.move_scope_selection(-1),
                KeyCode::Char('g') | KeyCode::Home => self.scope_select_first(),
                KeyCode::Char('G') | KeyCode::End => self.scope_select_last(),
                _ => {}
            },
        }
    }

    fn manual_refresh(&mut self) {
        self.spawn_refresh();
        self.status_message = Some("refresh requested".into());
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

    fn toggle_pin_selected(&mut self) {
        let name_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_name.clone())
        } else {
            self.selected_env().map(|e| e.name.clone())
        };
        let Some(name) = name_opt else {
            self.status_message = Some("no environment selected".into());
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

    fn yank_cli(&mut self) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(env) = env_opt else {
            self.status_message = Some("no environment selected".into());
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
            self.status_message = Some("no environment selected".into());
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
            self.status_message = Some("no environment selected".into());
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
            self.status_message = Some("no environment selected".into());
            return;
        };
        let mut tabs = vec![DetailTab::Events, DetailTab::Instances, DetailTab::Metrics];
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
            instances_scroll: 0,
            tags: Vec::new(),
            loading_events: false,
            loading_instances: false,
            loading_queues: false,
            loading_metrics: false,
            loading_tags: false,
            error: None,
            log_tail: LogTail::default(),
        };
        self.detail = Some(detail);
        self.mode = Mode::Detail;
        self.detail_refresh_active_tab();
        // Tags & instances load eagerly so the Config tab (tags + cost
        // annotation) is populated without the user having to switch tabs.
        self.spawn_detail_tags();
        if let Some(d) = self.detail.as_ref() {
            let env_name = d.env_name.clone();
            self.spawn_detail_instances(env_name);
        }
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

    fn detail_cycle_tab(&mut self, delta: i32) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let n = detail.tabs.len() as i32;
        let next = (detail.tab_idx as i32 + delta).rem_euclid(n) as usize;
        detail.tab_idx = next;
        self.detail_refresh_active_tab();
    }

    fn detail_scroll(&mut self, delta: i32) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        match detail.tab() {
            DetailTab::Events => {
                detail.events_scroll = scroll_apply(detail.events_scroll, delta);
            }
            DetailTab::Instances => {
                detail.instances_scroll = scroll_apply(detail.instances_scroll, delta);
            }
            DetailTab::Logs => {
                detail.log_tail.scroll = scroll_apply(detail.log_tail.scroll, delta);
            }
            DetailTab::Metrics | DetailTab::Queue | DetailTab::Config => {}
        }
    }

    fn detail_refresh_active_tab(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let env_name = detail.env_name.clone();
        let app_name = detail.env_snapshot.application.clone();
        let tab = detail.tab();
        match tab {
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

    fn detail_search_jump(&mut self, delta: i32) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let Some(re) = detail.search_pattern.as_ref() else {
            return;
        };
        let n = detail.events.len();
        if n == 0 {
            return;
        }
        let cur = detail.events_scroll as usize;
        if delta >= 0 {
            for off in 1..=n {
                let i = (cur + off) % n;
                if re.is_match(&detail.events[i].message) {
                    detail.events_scroll = i as u16;
                    return;
                }
            }
        } else {
            for off in 1..=n {
                let i = (cur + n - off) % n;
                if re.is_match(&detail.events[i].message) {
                    detail.events_scroll = i as u16;
                    return;
                }
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
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let name = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .fetch_env_metrics(&name, range)
                .await
                .map_err(|e| flatten_err("fetch_env_metrics", e));
            let _ = tx.send(AppMsg::DetailMetrics {
                gen,
                env_name,
                result,
            });
        });
    }

    fn open_dlq(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        if detail.tab() != DetailTab::Queue {
            return;
        }
        let Some(dlq_url) = detail.queues.dlq_url.clone() else {
            self.status_message = Some("no DLQ for this environment".into());
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
        let dlq_url = dlq.dlq_url.clone();
        tokio::spawn(async move {
            let result = aws
                .peek_messages(&dlq_url, 10)
                .await
                .map_err(|e| flatten_err("peek_messages", e));
            let _ = tx.send(AppMsg::DlqMessages {
                gen,
                env_name,
                result,
            });
        });
    }

    fn handle_dlq_key(&mut self, key: KeyEvent) {
        let Some(dlq) = self.dlq.as_mut() else { return };
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
            KeyCode::Char('r') => self.spawn_dlq_resend_selected(),
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
            self.status_message = Some("no environment selected".into());
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

    fn handle_action_key(&mut self, key: KeyEvent) {
        let Some(flow) = self.action_flow.as_mut() else {
            self.mode = Mode::Normal;
            return;
        };
        match flow {
            ActionFlow::Menu { list_state } => match key.code {
                KeyCode::Esc => self.close_action_flow(),
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
                (KeyCode::Char('y'), ConfirmKind::YesNo) | (KeyCode::Enter, ConfirmKind::YesNo) => {
                    let m = modal.clone();
                    self.action_flow = Some(ActionFlow::Running {
                        action: m.action,
                        env: m.target_env.clone(),
                        since: Instant::now(),
                    });
                    self.spawn_action(m);
                }
                (KeyCode::Char('n'), ConfirmKind::YesNo) => self.close_action_flow(),
                (KeyCode::Enter, ConfirmKind::TypeName) if modal.typed == modal.target_env => {
                    let m = modal.clone();
                    self.action_flow = Some(ActionFlow::Running {
                        action: m.action,
                        env: m.target_env.clone(),
                        since: Instant::now(),
                    });
                    self.spawn_action(m);
                }
                (KeyCode::Backspace, ConfirmKind::TypeName) => {
                    modal.typed.pop();
                }
                (KeyCode::Char(c), ConfirmKind::TypeName) if is_text_input(&key) => {
                    modal.typed.push(c);
                }
                _ => {}
            },
            ActionFlow::Running { .. } => {
                if key.code == KeyCode::Esc {
                    self.close_action_flow();
                }
            }
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
                        "no swap candidates: app '{}' has only one environment",
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
                self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                    action,
                    target_env: env.name.clone(),
                    swap_with: None,
                    typed: String::new(),
                    kind: ConfirmKind::TypeName,
                    dryrun: None,
                    loading_dryrun: true,
                    recent_events: None,
                    loading_events: true,
                    traffic_warning: compute_traffic_warning(&env),
                }));
                self.spawn_dry_run(env.name.clone());
                self.spawn_preflight_events(env.name.clone());
            }
            Action::Rebuild => {
                self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                    action,
                    target_env: env.name.clone(),
                    swap_with: None,
                    typed: String::new(),
                    kind: ConfirmKind::YesNo,
                    dryrun: None,
                    loading_dryrun: true,
                    recent_events: None,
                    loading_events: true,
                    traffic_warning: compute_traffic_warning(&env),
                }));
                self.spawn_dry_run(env.name.clone());
                self.spawn_preflight_events(env.name.clone());
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
                }));
            }
        }
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

    fn spawn_action(&mut self, modal: ConfirmModal) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let action = modal.action;
        let env = modal.target_env.clone();
        let swap_with = modal.swap_with.clone();
        write_audit_entry(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            action,
            &env,
            swap_with.as_deref(),
        );
        tokio::spawn(async move {
            let result = match action {
                Action::Rebuild => aws.rebuild_env(&env).await,
                Action::RestartAppServer => aws.restart_app_server(&env).await,
                Action::Terminate => aws.terminate_env(&env).await,
                Action::SwapCnames => match swap_with {
                    Some(dest) => aws.swap_cnames(&env, &dest).await,
                    None => Err(color_eyre::eyre::eyre!("swap target missing")),
                },
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
            "help" | "?" => self.mode = Mode::Help,
            "region" | "r" => match rest.first() {
                Some(r) => self.apply_picker_choice(PickerKind::Region, (*r).to_string()),
                None => self.error_message = Some("usage: :region <name>".into()),
            },
            "profile" | "p" => match rest.first() {
                Some(p) => self.apply_picker_choice(PickerKind::Profile, (*p).to_string()),
                None => self.error_message = Some("usage: :profile <name>".into()),
            },
            "sort" => {
                let Some(key) = rest.first() else {
                    self.error_message = Some(
                        "usage: :sort <key> [asc|desc]  — keys: name app status health version age"
                            .into(),
                    );
                    return;
                };
                match SortKey::parse(key) {
                    Some(k) => {
                        self.sort_key = k;
                        self.sort_desc = matches!(rest.get(1), Some(&"desc"));
                        self.resort_envs();
                        self.status_message = Some(format!(
                            "sort: {} ({})",
                            self.sort_key.label(),
                            if self.sort_desc { "desc" } else { "asc" }
                        ));
                    }
                    None => self.error_message = Some(format!("unknown sort key: {key}")),
                }
            }
            "group" => {
                self.grouped = parse_toggle(rest.first().copied(), self.grouped);
                self.rebuild_view();
                self.status_message = Some(if self.grouped {
                    "grouped by application".into()
                } else {
                    "ungrouped".into()
                });
            }
            "redact" => {
                self.redact = parse_toggle(rest.first().copied(), self.redact);
                self.status_message = Some(if self.redact {
                    "redact mode ON".into()
                } else {
                    "redact mode off".into()
                });
            }
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
            "history" => {
                self.current_overlay = Some(Overlay::History(self.format_message_log()));
            }
            "saved-configs" | "configs" => {
                self.current_overlay = Some(Overlay::SavedConfigs(format_saved_configs(
                    &self.applications,
                )));
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
                        self.error_message = Some("no environment selected".into());
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
                                Some(format!("no environment named '{target}' in current view"));
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
                    None => self.error_message = Some("no environment selected".into()),
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
            "cols" => {
                let known: &[&str] = &[
                    "NAME",
                    "APPLICATION",
                    "TIER",
                    "STATUS",
                    "HEALTH",
                    "TREND",
                    "PLATFORM",
                    "VERSION",
                    "CNAME",
                    "AGE",
                ];
                match rest.first().copied() {
                    None | Some("list") => {
                        let listing: Vec<String> = known
                            .iter()
                            .map(|c| {
                                if self.hidden_cols.contains(*c) {
                                    format!("{c} (hidden)")
                                } else {
                                    c.to_string()
                                }
                            })
                            .collect();
                        self.status_message = Some(format!("cols: {}", listing.join(", ")));
                    }
                    Some("hide") => match rest.get(1) {
                        Some(name) => {
                            let upper = name.to_uppercase();
                            if upper == "NAME" {
                                self.error_message = Some("NAME cannot be hidden".into());
                            } else if !known.contains(&upper.as_str()) {
                                self.error_message = Some(format!("unknown column '{name}'"));
                            } else {
                                self.hidden_cols.insert(upper.clone());
                                self.persist_state();
                                self.status_message = Some(format!("hid column {upper}"));
                            }
                        }
                        None => self.error_message = Some("usage: :cols hide <name>".into()),
                    },
                    Some("show") => match rest.get(1) {
                        Some(name) => {
                            let upper = name.to_uppercase();
                            if self.hidden_cols.remove(&upper) {
                                self.persist_state();
                                self.status_message = Some(format!("showed column {upper}"));
                            } else {
                                self.status_message = Some(format!("column {upper} already visible"));
                            }
                        }
                        None => self.error_message = Some("usage: :cols show <name>".into()),
                    },
                    Some("reset") => {
                        self.hidden_cols.clear();
                        self.persist_state();
                        self.status_message = Some("all columns visible".into());
                    }
                    Some(other) => self.error_message = Some(format!(
                        "unknown :cols subcommand '{other}'  (try: list / hide NAME / show NAME / reset)"
                    )),
                }
            }
            "save-view" => match rest.first() {
                Some(name) => {
                    let snap = encode_view(self);
                    self.saved_views.insert((*name).to_string(), snap.clone());
                    self.persist_state();
                    self.status_message = Some(format!("saved view '{name}'  ({snap})"));
                }
                None => self.error_message = Some("usage: :save-view <name>".into()),
            },
            "view" => match rest.first() {
                None => {
                    self.error_message = Some("usage: :view <name>  — see :views".into());
                }
                Some(name) => {
                    if let Some(snap) = self.saved_views.get(*name).cloned() {
                        apply_view(self, &snap);
                        self.status_message = Some(format!("loaded view '{name}'"));
                    } else {
                        self.error_message = Some(format!("no view '{name}'"));
                    }
                }
            },
            "views" => {
                if self.saved_views.is_empty() {
                    self.status_message =
                        Some("no saved views — :save-view <name> to create one".into());
                } else {
                    let listing: Vec<String> = self.saved_views.keys().cloned().collect();
                    self.status_message = Some(format!("views: {}", listing.join(", ")));
                }
            }
            "view-drop" => match rest.first() {
                Some(name) => {
                    if self.saved_views.remove(*name).is_some() {
                        self.persist_state();
                        self.status_message = Some(format!("dropped view '{name}'"));
                    } else {
                        self.error_message = Some(format!("no view '{name}'"));
                    }
                }
                None => self.error_message = Some("usage: :view-drop <name>".into()),
            },
            "filter" | "f" => match rest.first() {
                None => {
                    self.filter.clear();
                    self.rebuild_view();
                    self.status_message = Some("filter cleared".into());
                }
                Some(name) if self.named_filters.contains_key(*name) => {
                    self.filter = self.named_filters[*name].clone();
                    self.rebuild_view();
                    self.status_message = Some(format!("filter: {name} → \"{}\"", self.filter));
                }
                Some(name) => {
                    self.error_message =
                        Some(format!("no saved filter named '{name}' — try :filters"));
                }
            },
            "save" => match rest.first() {
                Some(name) => {
                    if self.filter.is_empty() {
                        self.error_message =
                            Some("nothing to save — set a filter with / first".into());
                    } else {
                        self.named_filters
                            .insert((*name).to_string(), self.filter.clone());
                        self.status_message =
                            Some(format!("saved filter '{name}' = \"{}\"", self.filter));
                        self.persist_state();
                    }
                }
                None => self.error_message = Some("usage: :save <name>".into()),
            },
            "drop" => match rest.first() {
                Some(name) => {
                    if self.named_filters.remove(*name).is_some() {
                        self.status_message = Some(format!("dropped saved filter '{name}'"));
                        self.persist_state();
                    } else {
                        self.error_message = Some(format!("no saved filter named '{name}'"));
                    }
                }
                None => self.error_message = Some("usage: :drop <name>".into()),
            },
            "filters" => {
                if self.named_filters.is_empty() {
                    self.status_message =
                        Some("no saved filters — :save <name> to create one".into());
                } else {
                    let listing: Vec<String> = self
                        .named_filters
                        .iter()
                        .map(|(k, v)| format!("{k}=\"{v}\""))
                        .collect();
                    self.status_message = Some(format!("filters: {}", listing.join("  ")));
                }
            }
            other => {
                if let Some(plugin) = self.plugins.get(other).cloned() {
                    self.run_plugin_command(other, &plugin);
                    return;
                }
                self.error_message = Some(format!("unknown command: :{other}  (try :help)"));
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
            self.error_message = Some(format!(":{name} — no environment selected"));
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

    fn persist_state(&self) {
        let selected = self.selected_env().map(|e| e.name.clone());
        state::save(&PersistedState {
            profile: self.override_profile.clone(),
            region: self.override_region.clone(),
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
            selected_env: selected,
            named_filters: self.named_filters.clone(),
            pinned: self.pinned.clone(),
            aliases: self.aliases.clone(),
            saved_views: self.saved_views.clone(),
            hidden_cols: self.hidden_cols.clone(),
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
                self.override_profile = Some(value.clone());
                self.override_region = None;
                self.status_message = Some(format!("switching to profile {value}…"));
            }
            PickerKind::Region => {
                self.override_region = Some(value.clone());
                self.status_message = Some(format!("switching to region {value}…"));
            }
        }
        self.spawn_rebuild();
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

    fn spawn_refresh(&mut self) {
        if matches!(self.load_state, LoadState::Loading) {
            return;
        }
        self.load_state = LoadState::Loading;
        self.loading_since = Some(Instant::now());
        self.status_snapshot_at_refresh =
            Some((self.status_message.clone(), self.error_message.clone()));
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .list_environments()
                .await
                .map_err(|e| flatten_err("list_environments", e));
            let _ = tx.send(AppMsg::Refresh { gen, result });
        });
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

    fn spawn_events(&mut self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .list_events(50)
                .await
                .map_err(|e| flatten_err("list_events", e));
            let _ = tx.send(AppMsg::Events { gen, result });
        });
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
                        self.applications = apps;
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
                    }
                    Err(msg) => tracing::warn!(error = %msg, "applications fetch failed"),
                }
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
                if gen != self.generation {
                    return;
                }
                let Some(detail) = self.detail.as_mut() else {
                    return;
                };
                if detail.env_name != env_name {
                    return; // user switched to a different env meanwhile
                }
                detail.loading_events = false;
                match result {
                    Ok(events) => {
                        detail.events = events;
                        detail.error = None;
                    }
                    Err(msg) => detail.error = Some(msg),
                }
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
                if gen != self.generation {
                    return;
                }
                let Some(detail) = self.detail.as_mut() else {
                    return;
                };
                if detail.env_name != env_name {
                    return;
                }
                detail.loading_instances = false;
                match result {
                    Ok(instances) => {
                        detail.instances = instances;
                        detail.error = None;
                    }
                    Err(msg) => detail.error = Some(msg),
                }
            }
            AppMsg::DetailMetrics {
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
                detail.loading_metrics = false;
                match result {
                    Ok(metrics) => {
                        detail.metrics = metrics;
                        detail.error = None;
                    }
                    Err(msg) => detail.error = Some(msg),
                }
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
                if gen != self.generation {
                    return;
                }
                let Some(detail) = self.detail.as_mut() else {
                    return;
                };
                if detail.env_name != env_name {
                    return;
                }
                detail.loading_tags = false;
                match result {
                    Ok(tags) => detail.tags = tags,
                    Err(msg) => tracing::warn!(error = %msg, "tags fetch failed"),
                }
            }
            AppMsg::DetailQueues {
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
                detail.loading_queues = false;
                match result {
                    Ok(queues) => {
                        detail.queues = queues;
                        detail.error = None;
                    }
                    Err(msg) => detail.error = Some(msg),
                }
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
                self.environments.clear();
                self.events.clear();
                self.events_scroll = 0;
                self.history.clear();
                // Overlays show data from the previous context (describe dump,
                // alarms list, …); close them so the user doesn't act on stale info.
                self.current_overlay = None;
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
                self.load_state = LoadState::Idle;
                self.persist_state();
                self.spawn_identity();
                self.spawn_refresh();
            }
            Err(msg) => {
                tracing::error!(error = %msg, "rebuild failed");
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

    fn drill_into_app(&mut self) {
        let Some(idx) = self.app_table_state.selected() else {
            return;
        };
        let Some(name) = self.applications.get(idx).map(|a| a.name.clone()) else {
            return;
        };
        self.filter = name.clone();
        self.scope = Scope::Envs;
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

                let new_alerts = envs.iter().filter(|e| is_red(&e.health)).count();
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
                    if self.status_message == prev_status {
                        self.status_message = None;
                    }
                    if self.error_message == prev_error {
                        self.error_message = None;
                    }
                } else {
                    self.status_message = None;
                    self.error_message = None;
                }
                self.restore_or_clamp_selection();
            }
            Err(msg) => {
                tracing::error!(error = %msg, "refresh failed");
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
fn is_throttling_error(msg: &str) -> bool {
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

fn yank(text: &str) -> std::result::Result<(), String> {
    let mut cb = arboard::Clipboard::new().map_err(|e| e.to_string())?;
    cb.set_text(text.to_string()).map_err(|e| e.to_string())
}

/// Pair every async AWS error with a full-chain log entry. The returned string
/// is the SDK's top-level `Display` (concise, suitable for the toast/footer);
/// the chain — including the underlying `dyn Error` causes that color-eyre
/// records on `Report` — goes to `ebman.log` via `tracing::error!`. Without
/// this the chain was lost both from the UI and the log.
fn flatten_err(op: &str, e: color_eyre::eyre::Report) -> String {
    tracing::error!(target: "ebman::aws", op = op, error = ?e, "aws call failed");
    format!("{e}")
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

    // Commands without args — `RunCommand` so Enter executes immediately.
    let zero_arg_cmds: &[(&str, &str)] = &[
        ("refresh", "force a refresh now"),
        ("help", "open the help popup"),
        ("export", "yank filtered view as TSV"),
        ("json", "yank filtered view as JSON"),
        ("report", "yank filtered view as Markdown"),
        ("history", "recent status / error messages"),
        ("whatsnew", "embedded changelog"),
        ("alarms", "CloudWatch alarms for selected env"),
        ("saved-configs", "EB saved configuration templates"),
        ("plugins", "list user plugin commands"),
        ("views", "list saved views"),
        ("filters", "list saved filters"),
        ("pin", "pin / unpin selected env"),
        ("quit", "exit ebman"),
    ];
    for (name, desc) in zero_arg_cmds {
        out.push(PaletteItem {
            label: format!(":{name}"),
            detail: (*desc).to_string(),
            kind_tag: "cmd",
            action: PaletteAction::RunCommand((*name).to_string()),
        });
    }

    // Commands that take an argument — prefill the command bar so the user
    // can type the rest.
    let prefill_cmds: &[(&str, &str)] = &[
        ("region ", "switch AWS region"),
        ("profile ", "switch AWS profile"),
        ("sort ", "set sort key (name/app/status/health/version/age)"),
        ("group ", "toggle grouping (on/off)"),
        ("redact ", "toggle redact mode (on/off)"),
        ("events ", "toggle events panel (on/off)"),
        ("save ", "save the current filter as NAME"),
        ("f ", "load named filter"),
        ("filter ", "load named filter"),
        ("save-view ", "save current view as NAME"),
        ("view ", "load saved view"),
        ("alias ", "set alias: <env-name> <label>"),
        ("alias-drop ", "remove alias for <env-name>"),
        ("diff ", "diff with another env: <env-name>"),
        ("cols ", "manage columns (list / hide / show / reset)"),
        (
            "loglevel ",
            "set tracing filter (trace/debug/info/warn/error)",
        ),
        ("readonly ", "toggle read-only (on/off)"),
    ];
    for (prefix, desc) in prefill_cmds {
        out.push(PaletteItem {
            label: format!(":{}", prefix.trim_end()),
            detail: (*desc).to_string(),
            kind_tag: "cmd",
            action: PaletteAction::PrefillCommand((*prefix).to_string()),
        });
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
        Ok(alarms) if alarms.is_empty() => "no CloudWatch alarms reference this environment".into(),
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
                    out.push_str(&format!("           ↳ {}\n", a.state_reason));
                }
                out.push('\n');
            }
            out
        }
    }
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
        }
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
            DetailTab::Events,
            DetailTab::Instances,
            DetailTab::Metrics,
            DetailTab::Queue,
            DetailTab::Config,
        ]
        .iter()
        .map(|t| t.title())
        .collect();
        assert_eq!(titles.len(), 5);
    }
}
