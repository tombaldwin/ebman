//! Per-env drill-down mode — the tabbed view (Events / Instances /
//! Metrics / Queue / Logs / Config) the operator gets on `Enter`.
//!
//! Type cluster lives here; the App-coupled methods that populate it
//! (`open_detail`, `close_detail`, `spawn_detail_events`,
//! `spawn_detail_instances`, …, `detail_refresh_active_tab`,
//! `handle_detail_search_key`, `handle_detail_key`) still live on
//! [`crate::app::App`]. Same rationale as `mode_action.rs` and
//! `mode_dlq.rs` — every handler reaches into AwsClient + audit + the
//! status / pending toasts. The types are testable in isolation; the
//! App-coupled control flow is the next split.

use crate::aws::{Environment, Event as EbEvent, Instance, MetricSeries, WorkerQueues};

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
    /// Env vars pulled from `DescribeConfigurationSettings` filtered to
    /// `aws:elasticbeanstalk:application:environment`. Surfaced in the
    /// Config tab so operators don't need `:env list` for the common case.
    pub env_vars: Vec<(String, String)>,
    /// CW Logs groups discovered for this env. `None` = "not checked yet";
    /// `Some(empty)` = "checked, none found"; `Some(non_empty)` = "live
    /// streaming available via `s`". The Logs tab uses this to render a
    /// hint that's accurate rather than generic.
    pub cw_log_groups: Option<Vec<String>>,
    pub loading_events: bool,
    pub loading_instances: bool,
    pub loading_queues: bool,
    pub loading_metrics: bool,
    pub loading_tags: bool,
    pub loading_env_vars: bool,
    pub error: Option<String>,
    /// Tail-log state, populated when the user visits the Logs tab.
    pub log_tail: LogTail,
    /// Cursor position within the Queue tab (0 = Main queue, 1 = DLQ). The
    /// tab body lists both rows; j/k moves the cursor, Enter opens the
    /// selected queue's viewer.
    pub queue_cursor: usize,
    /// Cursor position within the Instances tab — selects one of
    /// `detail.instances` for actions (Enter = console, y = yank id,
    /// x = terminate). Independent of `instances_scroll`.
    pub instances_cursor: usize,
    /// Pending single-instance terminate confirmation. Holds the
    /// instances-tab index when the user pressed `x`; Y/N resolves it.
    pub instance_terminate_confirm: Option<usize>,
    /// Mouse column over the Metrics tab body, captured on hover. The metrics
    /// renderer maps this to a point index and shows the value at that index
    /// in the title row of each chart.
    pub metrics_hover_col: Option<u16>,
    /// Inner Rect of the Metrics tab body, captured by the renderer so
    /// handle_mouse can ignore moves outside it.
    pub metrics_body_rect: Option<ratatui::layout::Rect>,
}

impl DetailState {
    pub fn tab(&self) -> DetailTab {
        self.tabs[self.tab_idx]
    }
}
