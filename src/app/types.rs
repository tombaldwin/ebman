//! Value types for the `app` module: the small enums, records and
//! constants that describe App's state without owning any behaviour.
//!
//! Split out of `src/app.rs` verbatim. Everything here is re-exported
//! from the parent (`pub use types::*`), so `crate::app::Overlay`,
//! `crate::app::SortKey` etc. keep resolving exactly as before.

use std::time::{Duration, Instant};

use ratatui::widgets::ListState;
use tui_common::TextInput;

use crate::aws::Event as EbEvent;

use super::tail;

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
    /// Screen lines per table row. **The single source of truth** — the
    /// renderer sizes rows with it and the mouse handlers divide by it
    /// to map a screen line back to a row index.
    ///
    /// It exists because those two disagreed. `draw_table` computed
    /// `if spacious { 2 } else { 1 }` locally while `select_row_at` and
    /// `update_hover` assumed one line per row, so in spacious mode a
    /// click landed on the wrong environment — and the hover tint moved
    /// with it, confirming the wrong row rather than exposing the bug.
    pub fn row_height(self) -> u16 {
        match self {
            Self::Default | Self::Compact => 1,
            Self::Spacious => 2,
        }
    }

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
pub(crate) const MESSAGE_LOG_CAP: usize = 50;
pub(crate) const TOAST_CAP: usize = 4;

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
        /// Cursor over the drillable items rendered in the overlay. The
        /// renderer maintains the parallel `App.why_items` list in lockstep,
        /// so the key handler can look up `why_items[cursor]` on `Enter`.
        cursor: usize,
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
        since_ms: i64,
        /// Shared scroll/follow/filter surface (see [`tail::TailView`]).
        view: tail::TailView,
        last_err: Option<String>,
        /// Unique-per-session id; the polling task carries the same id and
        /// late events for stale sessions are dropped on arrival.
        session_id: u64,
    },
    /// Cross-fleet EB event tail opened by `:event-tail` — every env
    /// in the current context, merged into one stream (the console's
    /// flat event firehose, in-app). Same polling/session shape as
    /// [`Overlay::LogTail`]; the buffer is capped at
    /// [`EVENT_TAIL_MAX_EVENTS`] (oldest dropped when growing).
    EventTail {
        events: std::collections::VecDeque<crate::aws::Event>,
        /// Shared scroll/follow/filter surface (see [`tail::TailView`]).
        view: tail::TailView,
        last_err: Option<String>,
        /// Unique-per-session id; the polling task carries the same id and
        /// late events for stale sessions are dropped on arrival.
        session_id: u64,
        /// How many polls couldn't fetch their whole window.
        ///
        /// Sticky, and rendered in the title, because the in-stream gap
        /// marker cannot be relied on: a truncated poll can carry more
        /// events than this ring holds, so the marker — inserted as the
        /// oldest row — is evicted by its own batch or by the next
        /// poll, and the overlay opens in follow mode at the newest end
        /// where the marker isn't. A counter in the chrome survives
        /// eviction by construction.
        truncated_polls: usize,
    },
    /// `:about` / `:credits` — the project card with the animated
    /// 8-bit giant-grabs-the-beanstalk scene. The `Instant` is the
    /// open time; the renderer derives the animation frame from its
    /// elapsed time, and the `anim` ticker is woken while it's open.
    About(std::time::Instant),
}

pub const LOG_TAIL_MAX_LINES: usize = 2000;

/// Ring-buffer cap for the `:event-tail` overlay. EB events are far
/// sparser than log lines, so a smaller cap than
/// [`LOG_TAIL_MAX_LINES`] still holds hours of fleet history.
pub const EVENT_TAIL_MAX_EVENTS: usize = 1000;

/// First `:event-tail` batch — the fleet's most recent events,
/// unwatermarked, so the overlay opens with context.
pub(crate) const EVENT_TAIL_FIRST_BATCH: i32 = 100;

/// Per-poll `:event-tail` batch cap. Applied server-side via
/// `max_records`; with the `start_time` watermark a normal 5s window
/// never comes close, so this only bites on a very noisy fleet — and
/// there it's the rate limiter that keeps the overlay responsive.
pub(crate) const EVENT_TAIL_POLL_BATCH: i32 = 300;

/// Advance the `:event-tail` poll watermark past the newest event in
/// `events`. +1ms because DescribeEvents' `start_time` is inclusive —
/// without it every poll re-ships the previous newest event. Falls
/// back to (and never regresses below) the previous watermark when
/// the batch is empty or carries older/undated events.
pub(crate) fn next_event_watermark_ms(events: &[crate::aws::Event], prev_ms: i64) -> i64 {
    events
        .iter()
        .filter_map(|e| e.at.map(|at| at.timestamp_millis() + 1))
        .max()
        .unwrap_or(prev_ms)
        .max(prev_ms)
}

/// Severity stamped on the synthetic row `:event-tail` inserts when a
/// poll couldn't fetch the whole window. Not an EB severity — it exists
/// so the filter and the renderer can recognise the row.
pub(crate) const EVENT_TAIL_GAP_SEVERITY: &str = "GAP";

/// Is this the synthetic "events were dropped" row rather than a real
/// EB event?
pub(crate) fn is_event_tail_gap(ev: &crate::aws::Event) -> bool {
    ev.severity == EVENT_TAIL_GAP_SEVERITY && ev.at.is_none()
}

/// Filter predicate for the `:event-tail` overlay — the regex runs
/// over env name, application, severity and message so `/prod`,
/// `/error` and free text all narrow the stream.
///
/// The gap marker is exempt. It carries no env or application, so any
/// filter narrowing to a specific environment would drop it — and a
/// filtered tail that silently omits "some events are missing" is
/// exactly the unbroken-looking chronology the marker exists to
/// prevent.
pub(crate) fn event_tail_matches(pattern: &regex::Regex, ev: &crate::aws::Event) -> bool {
    is_event_tail_gap(ev)
        || pattern.is_match(&ev.env)
        || pattern.is_match(&ev.application)
        || pattern.is_match(&ev.severity)
        || pattern.is_match(&ev.message)
}

/// One drillable row in the `:why` triage overlay. The renderer pushes
/// these in lockstep with the lines it emits (events / alarms /
/// instances / deploys / queues / dlq), and writes the list to
/// `App.why_items` so the key handler can act on `items[cursor]` when
/// the operator presses `Enter`.
#[derive(Debug, Clone)]
pub enum WhyItem {
    /// Pop up `Overlay::Describe` with the formatted detail text. Used
    /// for events / alarms / instances / deploys — read-only examination.
    Describe(String),
    /// Jump to the DLQ viewer (where the operator can examine / purge /
    /// replay). Used for the worker-queues summary row + DLQ message
    /// peek rows. The env name + queue URLs are read from the active
    /// `Overlay::WhyRed` at drill time.
    OpenDlq,
}

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

pub(crate) const WHATSNEW: &str = "\
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

pub(crate) const WELCOME_OVERLAY: &str = "\
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

/// One promotion event — captured when `:promote-env SOURCE TARGET`
/// opens the deploy confirm modal. The operator intent is recorded
/// regardless of whether the subsequent deploy succeeds — the
/// `:promotions` overlay surfaces it as "this version was promoted
/// from staging → prod (at T)" for post-mortem / lineage tracing.
///
/// In-memory only (cleared on context switch alongside the other
/// env-keyed state). Cross-session persistence in state.toml is a
/// 0.21+ follow-up — schema migration of the hand-rolled state
/// parser is out of scope for the initial cut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionRecord {
    pub source: String,
    pub target: String,
    pub version_label: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

/// Snapshot of an env's pre-deploy state, captured by `spawn_action`
/// just before a Deploy fires. `previous_version_label` is what the
/// env was running at capture time — the rollback target. `taken_at`
/// is wall-clock; the watchdog uses it for status reporting ("armed
/// 3m ago, 2m to deadline"). Persisted to state.toml so a cross-
/// session `:rollback` still has a target.
#[derive(Debug, Clone)]
#[allow(dead_code)] // env_name + taken_at are diagnostic / future-render fields
pub(crate) struct DeploySnapshot {
    /// The env this snapshot was captured for. Redundant with the
    /// `App.deploy_snapshots` map key, kept for log/debug output.
    pub env_name: String,
    pub previous_version_label: String,
    /// Capture timestamp. Available for "snapshot taken Xs ago"
    /// status messages on `:rollback` (already used by cmd_rollback)
    /// + future UI surfacing of how stale a snapshot is.
    pub taken_at: chrono::DateTime<chrono::Utc>,
}

/// In-flight auto-rollback watchdog state. Inserted at deploy
/// dispatch when `--auto-rollback Nm` is set, drained on early
/// disarm (env reached Green by the next refresh) or on the
/// deadline firing. The `target_label` is the version we'd
/// redeploy if the env still isn't healthy at the deadline —
/// the same as the captured `DeploySnapshot.previous_version_label`
/// at arm time, snapshotted here so we don't have to re-look-up.
#[derive(Debug, Clone)]
#[allow(dead_code)] // most fields are arm-time diagnostic / future-render
pub(crate) struct ArmedWatchdog {
    pub env_name: String,
    /// Snapshot of the rollback target at arm time so the watchdog
    /// doesn't have to re-look-up via deploy_snapshots when it fires.
    /// Currently unused — `handle_auto_rollback_check` re-reads from
    /// `deploy_snapshots` for consistency — but the duplicate is
    /// load-bearing for a future "show armed countdown with target"
    /// surface.
    pub target_label: String,
    pub armed_at: chrono::DateTime<chrono::Utc>,
    pub deadline_at: chrono::DateTime<chrono::Utc>,
}

/// In-flight `--wait-for-green` tracker. Populated when the
/// operator dispatches `:deploy LABEL --wait-for-green Nm` and
/// drained by `apply_refresh` once the env either reaches Green
/// (success) or the deadline elapses (timeout). Parallel to
/// `ArmedWatchdog` but doesn't dispatch a follow-on action — its
/// only outcome is a pinned status / error so the operator knows
/// the deploy result without staring at the table.
#[derive(Debug, Clone)]
pub(crate) struct WatchingDeploy {
    pub env_name: String,
    pub target_label: String,
    pub armed_at: chrono::DateTime<chrono::Utc>,
    pub deadline_at: chrono::DateTime<chrono::Utc>,
}

/// A captured undo entry — the reverse-action of a single
/// option-settings write, ready to be re-dispatched by `:undo`.
/// Captured by `spawn_option_settings_update` right before the
/// write (via an extra DescribeConfigurationSettings call) so
/// the operator can reverse the most recent edit even after EB
/// has committed it.
///
/// `to_set` reverses the original write's NEW values back to
/// their PRIOR values; `to_remove` reverses what was previously
/// unset (so the reverse drops the key rather than leaving it
/// as an empty string).
#[derive(Debug, Clone)]
pub(crate) struct UndoEntry {
    pub env_name: String,
    pub to_set: Vec<(String, String, String)>,
    pub to_remove: Vec<(String, String)>,
    /// One-line summary of what the ORIGINAL action was, so the
    /// undo toast can read "undoing: keypair foo" rather than the
    /// generic "option-settings update".
    pub original_summary: String,
    pub captured_at: chrono::DateTime<chrono::Utc>,
}

/// Cap on the undo-history deque. Bounds memory while still
/// covering most operator workflows — a long incident might
/// run 4-5 config edits in a row; 10 is a generous ceiling.
pub(crate) const UNDO_HISTORY_CAP: usize = 10;

/// Session-scoped temporary write-lock set by `:freeze-deploys`.
/// Layered above the per-env / per-account safety pins in
/// `is_read_only_for` so destructive ops refuse fleet-wide
/// during triage. Cleared by `:thaw-deploys` or by exiting
/// ebman — not persisted to state.toml (intentional: the freeze
/// is an in-session safety gesture, not a durable policy).
#[derive(Debug, Clone)]
pub(crate) struct DeployFreeze {
    /// Operator-supplied reason (e.g. "incident #1234"). Empty
    /// string when no reason was given. Surfaced in the refusal
    /// toast so the operator (or a teammate sharing the terminal)
    /// knows why the lock is on.
    pub reason: String,
    pub frozen_at: chrono::DateTime<chrono::Utc>,
}

/// Session-scoped incident mode set by `:incident START "headline"`.
/// A composite gesture over existing machinery: starting an incident
/// also sets a [`DeployFreeze`] (same fleet-wide write-lock) and
/// writes an `IncidentStart` audit line; the header renders a
/// high-priority banner pill while it's active. `:incident END`
/// clears both and writes an `IncidentEnd` summary line. Like the
/// freeze, not persisted to state.toml — an in-session gesture.
#[derive(Debug, Clone)]
pub(crate) struct Incident {
    /// Operator-supplied headline (e.g. "checkout 5xx spike").
    /// Empty string when none was given.
    pub headline: String,
    pub started_at: chrono::DateTime<chrono::Utc>,
}

impl DeploySnapshot {
    /// On-disk shape — `"label|RFC3339-ts"`. Pipe separator keeps the
    /// existing line-oriented state.toml parser happy. The pipe is
    /// illegal inside an EB version label (EB rejects `|` per its
    /// version-label validator), so there's no escaping needed.
    pub fn to_persisted(&self) -> String {
        format!(
            "{}|{}",
            self.previous_version_label,
            self.taken_at.to_rfc3339()
        )
    }

    /// Inverse of `to_persisted`. Returns `None` for malformed lines
    /// so the loader can silently drop them — better to lose one
    /// stale entry than to abort the App-init path.
    pub fn parse_persisted(env_name: &str, raw: &str) -> Option<Self> {
        let (label, ts_str) = raw.split_once('|')?;
        let label = label.trim();
        if label.is_empty() {
            return None;
        }
        let taken_at = chrono::DateTime::parse_from_rfc3339(ts_str.trim())
            .ok()?
            .with_timezone(&chrono::Utc);
        Some(Self {
            env_name: env_name.to_string(),
            previous_version_label: label.to_string(),
            taken_at,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    Profile,
    Region,
    /// Picker over the env's discovered CW log groups, opened from the
    /// LogTail streaming overlay so the operator can switch the tailed
    /// group without typing the full ARN.
    LogGroup,
    /// Picker over the env's instances when `:ssh` is invoked without
    /// a target. Source is `Detail.instances` — the operator must
    /// have the Detail view open + the Instances tab loaded, or pass
    /// an explicit `:ssh i-abc` instance ID. Avoids adding a spawn
    /// path just for `:ssh`-from-cold; the operator's already on
    /// Detail/Instances when they reach for an SSM session.
    SshInstance,
}

pub struct Picker {
    pub kind: PickerKind,
    pub items: Vec<String>,
    pub filter: TextInput,
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
            filter: TextInput::new(),
            list_state,
        }
    }

    pub fn title(&self) -> &'static str {
        match self.kind {
            PickerKind::Profile => " select profile ",
            PickerKind::Region => " select region ",
            PickerKind::LogGroup => " select log group ",
            PickerKind::SshInstance => " select instance for SSM session ",
        }
    }

    pub fn filtered(&self) -> Vec<usize> {
        if self.filter.is_empty() {
            return (0..self.items.len()).collect();
        }
        let needle = self.filter.text().to_lowercase();
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

/// In-progress command-bar Tab-completion cycle.
#[derive(Default)]
pub struct CompletionState {
    /// The text the operator had typed before they first pressed Tab to
    /// start a completion cycle. Cycling forward / backward matches against
    /// this prefix; typing a new character resets it (and the cycle).
    /// `None` when no cycle is active.
    pub origin: Option<String>,
    /// Position within the candidate list for the active completion cycle.
    /// Only meaningful when `origin` is `Some`. Zero before the first Tab
    /// (so the first Tab lands on the first match).
    pub index: usize,
}

/// State for the global help overlay.
pub struct HelpState {
    pub scroll: u16,
    /// Last computed max scroll, written by `draw_help` each frame and read
    /// by the j/k handler so an incremental scroll past the bottom doesn't
    /// accumulate (which would otherwise require N matching scroll-ups to
    /// bring content back into view).
    pub max_scroll: u16,
    /// Which keymap subset `draw_help` renders. Set whenever `?` opens Help.
    pub topic: HelpTopic,
    /// The mode the user was in before they opened help. Restored when help
    /// closes so pressing `?` from Detail / Action / Dlq doesn't drop the
    /// user back to Normal and lose the active screen.
    pub pre_mode: Option<Mode>,
    /// Overlay (if any) the user had open before pressing `?`. Help renders
    /// before overlays in the z-order so it's stashed here and restored
    /// around the help round-trip.
    pub pre_overlay: Option<Overlay>,
}

/// State for the bottom Events panel (and the event-timestamp format it
/// shares with the Detail/Events tab).
pub struct EventPanel {
    pub events: Vec<EbEvent>,
    pub visible: bool,
    /// How event timestamps render in the Events panel + Detail/Events tab.
    /// Defaults to UTC so the column matches CloudWatch / EB API output.
    /// Operator cycles `Utc → Local → Age` via `:event-time` or the `T` key
    /// in scopes where events are visible. Persists.
    pub time_format: EventTimeFormat,
    /// Env the current `events` list was fetched for. `None` = global. Used
    /// by `refresh_events_if_selection_changed` to detect when the user has
    /// moved the table cursor to a different env and refetch.
    pub for_env: Option<String>,
    pub scroll: u16,
    /// Inner Rect of the events panel — captured by the renderer so the
    /// mouse handler can detect drags on the top edge (divider row) for
    /// resize.
    pub area: Option<ratatui::layout::Rect>,
    /// Set when a divider drag is in progress; stores the panel height at
    /// the moment the user pressed down so we can compute the delta against
    /// the current mouse row.
    pub drag_origin: Option<u16>,
    /// When set, the user has "entered" the events panel for navigation:
    /// J/K move the cursor within the events list, Y yanks the highlighted
    /// line. `None` means events keys are inert and the main table responds
    /// to J/K.
    pub cursor: Option<usize>,
    /// Rendered height of the events panel, in rows.
    pub height: u16,
}

/// Config-derived values resolved once at startup (from `config.toml` +
/// the active profile), grouped off `App` so the dozen mirror fields
/// don't clutter the top-level state. Round-tripped by `:settings` via
/// `current_config_snapshot`; reassigned wholesale on profile/config
/// reload. Pure config — runtime UI state stays on `App`.
#[derive(Debug, Clone, Default)]
pub struct ResolvedConfig {
    /// Mirror of `Config::notify_webhook` (fan-out reads a process-wide
    /// `OnceLock` in [`crate::audit`]; held here for `:settings` save).
    pub notify_webhook: Option<String>,
    /// `config.toml` `alias.NAME = "expansion"` command aliases.
    pub command_aliases: std::collections::HashMap<String, String>,
    /// Operator-disabled lint rule IDs (`lint.disable = "EBL001,…"`).
    pub lint_disable: Vec<String>,
    /// Resolved `[explain]` LLM settings (provider + model + auth).
    pub explain_settings: crate::llm::Settings,
    /// Tag keys every env must carry (`required_tags`).
    pub required_tags: Vec<String>,
    /// CloudWatch dimension names that identify an env, for matching
    /// alarms to it (`alarm_dimensions`). Normally just
    /// `EnvironmentName`.
    pub alarm_dimensions: Vec<String>,
    /// Config lines the parser didn't recognise, kept so a `:settings`
    /// save doesn't destroy them.
    pub passthrough: Vec<String>,
    /// Raw `icons = …` string before resolution to `IconStyle` (so
    /// `:settings` round-trips `"auto"` without flattening it).
    pub cfg_icons_raw: String,
    /// Per-profile theme overrides (`profile_themes` key).
    pub profile_themes: std::collections::HashMap<String, String>,
    /// Per-env runbook URLs (`runbooks.ENV`).
    pub runbooks: std::collections::HashMap<String, String>,
    /// Per-env read-only locks (`safety.envs.NAME.read_only`).
    pub safety_envs: std::collections::HashMap<String, bool>,
    /// Per-account read-only locks (`safety.accounts.NAME.read_only`).
    pub safety_accounts: std::collections::HashMap<String, bool>,
    /// Named AssumeRole accounts (`accounts.NAME.*`).
    pub accounts: std::collections::HashMap<String, crate::config::AccountSpec>,
    /// Base theme name (`theme = …`), kept separate from the running
    /// `theme` so a profile-themed session reverts cleanly.
    pub base_theme_name: String,
}
