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

/// One drillable item on the Health tab. Render + Enter-dispatch derive
/// the same list from [`health_items`] so the cursor stays in sync with
/// what's on screen — same data → same items → same indices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HealthItem {
    /// Index into `detail.events` (which is what the Health-tab "recent
    /// events" section reads from — filtered + truncated by the renderer
    /// but indexing the source vec keeps drill-in robust against
    /// rendering changes).
    Event { event_idx: usize },
    /// Index into `detail.instances`.
    Instance { instance_idx: usize },
    /// Main queue row (workers only).
    MainQueue,
    /// Dead-letter queue row (workers only).
    Dlq,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailTab {
    /// Rollup of "is this env OK?" — current status / health, recent
    /// significant events, instance-cause summary, DLQ depth for workers.
    /// Default tab when entering Detail so the operator sees triage info
    /// before drilling into the per-source tabs.
    Health,
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
            Self::Health => "Health",
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
    /// Cursor index into the `health_items` list (driven by j/k on the
    /// Health tab). Wraps around when the list is non-empty; Enter
    /// drills into the selected item via [`HealthItem`].
    pub health_cursor: usize,
    /// Mouse column over the Metrics tab body, captured on hover. The metrics
    /// renderer maps this to a point index and shows the value at that index
    /// in the title row of each chart.
    pub metrics_hover_col: Option<u16>,
    /// Inner Rect of the Metrics tab body, captured by the renderer so
    /// handle_mouse can ignore moves outside it.
    pub metrics_body_rect: Option<ratatui::layout::Rect>,
    /// CW alarms attached to this env. None = not yet fetched; Some(Err) =
    /// fetch failed; Some(Ok) = fetched. Surfaced in the Health tab's
    /// alarms section. Mirrors the alarms data in `:why` so the two
    /// triage surfaces no longer disagree.
    pub cw_alarms: Option<Result<Vec<crate::aws::CwAlarm>, String>>,
    pub loading_cw_alarms: bool,
    /// Recently-registered application versions (up to ~5). Surfaced in
    /// the Health tab's "recent deploys" section so an env that flipped
    /// Red right after a deploy makes that obvious without leaving the
    /// Detail view.
    pub recent_versions: Option<Result<Vec<crate::aws::AppVersion>, String>>,
    pub loading_recent_versions: bool,
}

impl DetailState {
    pub fn tab(&self) -> DetailTab {
        self.tabs[self.tab_idx]
    }
}

/// Pure: enumerate the Health-tab items that the operator can navigate
/// to. The renderer marks the row at `health_cursor` and the
/// Enter-dispatch reads the same list — so a refresh that adds/removes
/// items can shift the cursor predictably (clamp at len-1).
///
/// Mirrors the filter logic in `draw_detail_health`:
///   - Events: ERROR / WARN severity, last 30 min, top 10.
///   - Instances: Red/Severe colour or health, top 3.
///   - Queues (workers only): Main + DLQ rows when stats are populated.
///
/// Returned in render order so the cursor maps 1-to-1 with what the
/// operator sees on screen.
pub fn health_items(detail: &DetailState, now: chrono::DateTime<chrono::Utc>) -> Vec<HealthItem> {
    let mut out: Vec<HealthItem> = Vec::new();
    // Events section — mirror the renderer's filter (severity + recency).
    let cutoff = now - chrono::Duration::minutes(30);
    for (idx, e) in detail.events.iter().enumerate() {
        if out
            .iter()
            .filter(|h| matches!(h, HealthItem::Event { .. }))
            .count()
            >= 10
        {
            break;
        }
        let sev = e.severity.to_uppercase();
        if sev != "ERROR" && sev != "WARN" {
            continue;
        }
        if !e.at.map(|t| t >= cutoff).unwrap_or(true) {
            continue;
        }
        out.push(HealthItem::Event { event_idx: idx });
    }
    // Instances section — only Severe / Red, top 3 (matches renderer).
    let mut shown = 0;
    for (idx, i) in detail.instances.iter().enumerate() {
        if shown >= 3 {
            break;
        }
        let red = i.color.eq_ignore_ascii_case("Red") || i.health.eq_ignore_ascii_case("Severe");
        if !red {
            continue;
        }
        out.push(HealthItem::Instance { instance_idx: idx });
        shown += 1;
    }
    // Worker queues — both rows when populated.
    if detail.env_snapshot.tier.eq_ignore_ascii_case("Worker") {
        if detail.queues.main_stats.is_some() {
            out.push(HealthItem::MainQueue);
        }
        if detail.queues.dlq_stats.is_some() {
            out.push(HealthItem::Dlq);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aws::{Environment, Event as EbEvent, Instance, QueueStats, WorkerQueues};

    fn mk_env(tier: &str) -> Environment {
        Environment {
            name: "uflexi-prod".into(),
            application: "uflexi".into(),
            status: "Updating".into(),
            health: "Red".into(),
            platform: "Java 17".into(),
            tier: tier.into(),
            cname: String::new(),
            version_label: String::new(),
            arn: None,
            updated: None,
            id: None,
            region: None,
        }
    }

    fn mk_event(sev: &str, mins_ago: i64) -> EbEvent {
        EbEvent {
            at: Some(chrono::Utc::now() - chrono::Duration::minutes(mins_ago)),
            env: "uflexi-prod".into(),
            application: "uflexi".into(),
            message: "test".into(),
            severity: sev.into(),
        }
    }

    fn mk_instance(id: &str, color: &str) -> Instance {
        // Pair health to colour so the `color == Red || health == Severe`
        // filter is exercised on real combinations rather than always
        // matching via the Severe-health hatch.
        let health = match color {
            "Red" => "Severe",
            "Yellow" => "Warning",
            _ => "Ok",
        };
        Instance {
            id: id.into(),
            health: health.into(),
            color: color.into(),
            causes: vec![],
            instance_type: "t3.medium".into(),
            availability_zone: "us-east-1a".into(),
            launched_at: None,
        }
    }

    fn empty_detail(tier: &str) -> DetailState {
        DetailState {
            env_name: "uflexi-prod".into(),
            env_snapshot: mk_env(tier),
            tabs: vec![DetailTab::Health],
            tab_idx: 0,
            events: vec![],
            instances: vec![],
            queues: WorkerQueues::default(),
            metrics: vec![],
            metrics_range_secs: 3600,
            auto_refresh: false,
            search_input: String::new(),
            search_active: false,
            search_pattern: None,
            search_error: None,
            events_scroll: 0,
            instances_scroll: 0,
            tags: vec![],
            env_vars: vec![],
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
        }
    }

    #[test]
    fn health_items_includes_error_warn_events_only() {
        let mut d = empty_detail("Web");
        d.events = vec![
            mk_event("ERROR", 1),
            mk_event("INFO", 1), // filtered out
            mk_event("WARN", 5),
            mk_event("DEBUG", 1), // filtered out
        ];
        let items = health_items(&d, chrono::Utc::now());
        assert_eq!(items.len(), 2);
        assert!(matches!(items[0], HealthItem::Event { event_idx: 0 }));
        // INFO at idx 1 filtered → next event-item is the WARN at idx 2.
        assert!(matches!(items[1], HealthItem::Event { event_idx: 2 }));
    }

    #[test]
    fn health_items_excludes_events_older_than_30_min() {
        let mut d = empty_detail("Web");
        d.events = vec![
            mk_event("ERROR", 5),  // included
            mk_event("ERROR", 45), // outside window
        ];
        let items = health_items(&d, chrono::Utc::now());
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn health_items_only_severe_instances_up_to_3() {
        let mut d = empty_detail("Web");
        d.instances = vec![
            mk_instance("i-1", "Red"),
            mk_instance("i-2", "Green"), // filtered
            mk_instance("i-3", "Red"),
            mk_instance("i-4", "Red"),
            mk_instance("i-5", "Red"), // 4th red — should not show (cap=3)
        ];
        let items = health_items(&d, chrono::Utc::now());
        assert_eq!(items.len(), 3);
        // Should pick the first 3 reds by index: 0, 2, 3.
        assert!(matches!(items[0], HealthItem::Instance { instance_idx: 0 }));
        assert!(matches!(items[1], HealthItem::Instance { instance_idx: 2 }));
        assert!(matches!(items[2], HealthItem::Instance { instance_idx: 3 }));
    }

    #[test]
    fn health_items_includes_worker_queues_when_populated() {
        let mut d = empty_detail("Worker");
        d.queues = WorkerQueues {
            main_url: Some("https://sqs/main".into()),
            dlq_url: Some("https://sqs/dlq".into()),
            main_stats: Some(QueueStats::default()),
            dlq_stats: Some(QueueStats::default()),
        };
        let items = health_items(&d, chrono::Utc::now());
        assert_eq!(items.len(), 2);
        assert_eq!(items[0], HealthItem::MainQueue);
        assert_eq!(items[1], HealthItem::Dlq);
    }

    #[test]
    fn health_items_skips_queues_for_web_tier() {
        let mut d = empty_detail("Web");
        // Even with stats populated (defensive case — queues fetcher
        // ran for the wrong tier somehow), Web envs don't get queue
        // rows on the Health tab.
        d.queues = WorkerQueues {
            main_url: None,
            dlq_url: None,
            main_stats: Some(QueueStats::default()),
            dlq_stats: Some(QueueStats::default()),
        };
        let items = health_items(&d, chrono::Utc::now());
        assert!(items.is_empty());
    }

    #[test]
    fn health_items_render_order_matches_section_order() {
        // Verify events → instances → queues so the cursor navigates
        // top-to-bottom in the same order the operator sees them.
        let mut d = empty_detail("Worker");
        d.events = vec![mk_event("ERROR", 1)];
        d.instances = vec![mk_instance("i-1", "Red")];
        d.queues = WorkerQueues {
            main_url: Some("https://sqs/main".into()),
            dlq_url: Some("https://sqs/dlq".into()),
            main_stats: Some(QueueStats::default()),
            dlq_stats: Some(QueueStats::default()),
        };
        let items = health_items(&d, chrono::Utc::now());
        assert!(matches!(items[0], HealthItem::Event { .. }));
        assert!(matches!(items[1], HealthItem::Instance { .. }));
        assert_eq!(items[2], HealthItem::MainQueue);
        assert_eq!(items[3], HealthItem::Dlq);
    }
}
