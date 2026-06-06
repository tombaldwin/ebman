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

use tui_common::TextInput;

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

/// Minimum-severity filter for the Events tab. Acts as a floor:
/// `Info` shows INFO and above, hiding DEBUG / TRACE. `All` shows
/// everything. Cycles `All → Info → Warn → Error → All`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EventLevel {
    #[default]
    All,
    Info,
    Warn,
    Error,
}

impl EventLevel {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Info,
            Self::Info => Self::Warn,
            Self::Warn => Self::Error,
            Self::Error => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Info => "info+",
            Self::Warn => "warn+",
            Self::Error => "error",
        }
    }

    /// Numeric floor an event's severity must reach to pass.
    fn rank(self) -> u8 {
        match self {
            Self::All => 0,
            Self::Info => 2,
            Self::Warn => 3,
            Self::Error => 4,
        }
    }
}

/// Map an EB event severity string to a comparable rank. EB emits
/// `TRACE` / `DEBUG` / `INFO` / `WARN` / `ERROR`; an unrecognised or
/// empty value is treated as INFO so it isn't silently hidden by a
/// `warn+` filter.
pub fn severity_rank(severity: &str) -> u8 {
    match severity.to_ascii_uppercase().as_str() {
        "TRACE" => 0,
        "DEBUG" => 1,
        "INFO" => 2,
        "WARN" | "WARNING" => 3,
        "ERROR" | "FATAL" => 4,
        _ => 2,
    }
}

/// Time-window filter for the Events tab. `All` disables the window;
/// the rest keep only events newer than the cutoff. Cycles
/// `All → 1h → 6h → 24h → 7d → All`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EventWindow {
    #[default]
    All,
    H1,
    H6,
    D1,
    D7,
}

impl EventWindow {
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::H1,
            Self::H1 => Self::H6,
            Self::H6 => Self::D1,
            Self::D1 => Self::D7,
            Self::D7 => Self::All,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::H1 => "1h",
            Self::H6 => "6h",
            Self::D1 => "24h",
            Self::D7 => "7d",
        }
    }

    /// Oldest timestamp that still passes, given `now`. `None` means
    /// "no cutoff" (the `All` variant).
    pub fn cutoff(
        self,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Option<chrono::DateTime<chrono::Utc>> {
        let dur = match self {
            Self::All => return None,
            Self::H1 => chrono::Duration::hours(1),
            Self::H6 => chrono::Duration::hours(6),
            Self::D1 => chrono::Duration::days(1),
            Self::D7 => chrono::Duration::days(7),
        };
        Some(now - dur)
    }
}

/// Pure: return the indices of `events` that pass both the severity
/// floor and the time window. Indices (not clones) so the renderer
/// can still map back to the source vec for n/N search-jump and
/// Health-tab drill-in. An event with no timestamp passes the window
/// filter (can't be excluded by a cutoff it has no value for).
pub fn filter_event_indices(
    events: &[EbEvent],
    level: EventLevel,
    window: EventWindow,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<usize> {
    let floor = level.rank();
    let cutoff = window.cutoff(now);
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| severity_rank(&e.severity) >= floor)
        .filter(|(_, e)| match (cutoff, e.at) {
            (Some(c), Some(at)) => at >= c,
            _ => true,
        })
        .map(|(i, _)| i)
        .collect()
}

/// Which editable section of the Config tab a row belongs to.
/// Determines the dispatch path on commit (`UpdateOptionSettings`
/// for env vars, `UpdateTags` for tags).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigItemKind {
    EnvVar,
    Tag,
}

/// One cursor-addressable, editable row on the Config tab. The
/// read-only metadata rows (name / status / platform / …) are not
/// represented here — the cursor only stops on rows the operator
/// can actually change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigItem {
    pub kind: ConfigItemKind,
    pub key: String,
    pub value: String,
}

/// What an open Config-tab editor is editing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigEditMode {
    /// Editing an existing row's value — `key` fixed, `input` is the value.
    Value,
    /// Adding a new row — `key` empty, `input` is a `KEY=VALUE` string.
    NewRow,
    /// Renaming an existing row's key — `key` is the *old* key,
    /// `input` is the new key being typed. Commit dispatches a
    /// remove-old + set-new under the same value.
    RenameKey,
}

/// Active in-place editor on the Config tab. `input` is the text
/// buffer; what it means depends on `mode` (a value, a `KEY=VALUE`
/// pair, or a replacement key). `caret` is a *char* index into
/// `input` (not a byte offset) — the text-cursor position that
/// Left/Right/Home/End move and where insert/delete act.
#[derive(Debug, Clone)]
pub struct ConfigEdit {
    pub kind: ConfigItemKind,
    pub key: String,
    pub original: String,
    pub input: String,
    pub caret: usize,
    pub mode: ConfigEditMode,
}

impl ConfigEdit {
    /// Char count of the value buffer.
    fn char_len(&self) -> usize {
        self.input.chars().count()
    }

    /// Byte offset of the caret within `input`. Always a valid char
    /// boundary — `char_indices` yields boundaries, and a caret at
    /// the very end maps to `input.len()`.
    fn caret_byte(&self) -> usize {
        self.input
            .char_indices()
            .nth(self.caret)
            .map(|(b, _)| b)
            .unwrap_or(self.input.len())
    }

    /// Insert a char at the caret and step the caret past it.
    pub fn insert(&mut self, c: char) {
        let b = self.caret_byte();
        self.input.insert(b, c);
        self.caret += 1;
    }

    /// Delete the char *before* the caret (Backspace).
    pub fn backspace(&mut self) {
        if self.caret == 0 {
            return;
        }
        self.caret -= 1;
        let b = self.caret_byte();
        self.input.remove(b);
    }

    /// Delete the char *at* the caret (Delete / Del).
    pub fn delete(&mut self) {
        if self.caret >= self.char_len() {
            return;
        }
        let b = self.caret_byte();
        self.input.remove(b);
    }

    pub fn move_left(&mut self) {
        self.caret = self.caret.saturating_sub(1);
    }

    pub fn move_right(&mut self) {
        self.caret = (self.caret + 1).min(self.char_len());
    }

    pub fn move_home(&mut self) {
        self.caret = 0;
    }

    pub fn move_end(&mut self) {
        self.caret = self.char_len();
    }

    /// Split `input` at the caret for rendering — `(before, after)`.
    pub fn split_at_caret(&self) -> (&str, &str) {
        self.input.split_at(self.caret_byte())
    }
}

/// Parse the `KEY=VALUE` buffer of an add-a-new-row Config edit.
/// Splits on the *first* `=` so a value may itself contain `=`; the
/// key is trimmed and must be non-empty. Returns `None` for
/// malformed input (no `=`, or an empty/whitespace key). The value
/// is taken verbatim — not trimmed — since trailing spaces can be
/// significant in an env var.
pub fn parse_new_config_row(input: &str) -> Option<(String, String)> {
    let (k, v) = input.split_once('=')?;
    let k = k.trim();
    if k.is_empty() {
        return None;
    }
    Some((k.to_string(), v.to_string()))
}

/// Pure: build the flat list of editable Config-tab rows in the
/// exact order [`crate::ui::draw_detail_config`] renders them —
/// tags first (sorted case-insensitively by key, matching the
/// render), then env vars (natural order). The Config-tab cursor
/// indexes into this list, so render + navigation agree by
/// construction.
pub fn config_editable_items(detail: &DetailState) -> Vec<ConfigItem> {
    let mut out = Vec::with_capacity(detail.tags.len() + detail.env_vars.len());
    let mut tags: Vec<&(String, String)> = detail.tags.iter().collect();
    tags.sort_by_key(|(k, _)| k.to_lowercase());
    for (k, v) in tags {
        out.push(ConfigItem {
            kind: ConfigItemKind::Tag,
            key: k.clone(),
            value: v.clone(),
        });
    }
    for (k, v) in &detail.env_vars {
        out.push(ConfigItem {
            kind: ConfigItemKind::EnvVar,
            key: k.clone(),
            value: v.clone(),
        });
    }
    out
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
    pub search_input: TextInput,
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
    pub search_input: TextInput,
    pub search_active: bool, // true while user is typing a pattern
    pub search_pattern: Option<regex::Regex>,
    pub search_error: Option<String>,
    pub events_scroll: u16,
    /// Max legal `events_scroll`, written by the Events-tab renderer
    /// each frame (filtered line count minus the visible body height).
    /// The key handler clamps against it so j/k can't scroll the list
    /// off the bottom into blank space. Same pattern as `help_max_scroll`.
    pub events_max_scroll: u16,
    /// Minimum-severity filter for the Events tab. `L` cycles it.
    pub events_level: EventLevel,
    /// Time-window filter for the Events tab. `w` cycles it.
    pub events_window: EventWindow,
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
    /// Cursor index into [`config_editable_items`] — the editable
    /// rows of the Config tab (tags + env vars). `j`/`k` move it;
    /// `enter` opens the in-place editor for the row it points at.
    pub config_cursor: usize,
    /// Active in-place value editor on the Config tab. `Some` while
    /// the operator is typing a new value; commit dispatches the
    /// change and clears it.
    pub config_edit: Option<ConfigEdit>,
    /// Vertical scroll offset of the Config-tab body. Recomputed by
    /// the renderer each frame to keep `config_cursor` in the
    /// viewport (the body is one tall `Paragraph`, so a long
    /// tag/env-var list would otherwise run off the bottom).
    pub config_scroll: u16,
    /// When `Some(i)`, the Config-tab row at editable-index `i` has a
    /// delete pending operator confirmation (`x` armed it; `y`
    /// confirms, anything else cancels). Mirrors
    /// `instance_terminate_confirm`.
    pub config_delete_confirm: Option<usize>,
}

impl DetailState {
    pub fn tab(&self) -> DetailTab {
        self.tabs[self.tab_idx]
    }

    /// Clamp `config_cursor` into the current editable-row range.
    /// Called after a tags / env-vars refetch so a delete that
    /// shrank the list doesn't leave the cursor pointing past the
    /// end (which would hide the `▶` marker until the next `j`/`k`).
    pub fn clamp_config_cursor(&mut self) {
        let n = config_editable_items(self).len();
        self.config_cursor = if n == 0 {
            0
        } else {
            self.config_cursor.min(n - 1)
        };
    }

    /// Drop a now-stale in-place edit after a refetch. If the row
    /// being edited no longer exists (deleted by this operator or
    /// another), the editor would render invisible — its `key` no
    /// longer matches any row — yet still swallow every keypress and
    /// commit a write that *re-creates* the deleted key. Clearing it
    /// here is the safe outcome. Add-row edits reference no existing
    /// row, so they're left alone.
    pub fn revalidate_config_edit(&mut self) {
        let Some((kind, key)) = self.config_edit.as_ref().and_then(|e| {
            if e.mode == ConfigEditMode::NewRow {
                None
            } else {
                Some((e.kind, e.key.clone()))
            }
        }) else {
            return;
        };
        let still_present = config_editable_items(self)
            .iter()
            .any(|it| it.kind == kind && it.key == key);
        if !still_present {
            self.config_edit = None;
        }
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
            solution_stack: String::new(),
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
            version_label: None,
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
            search_input: TextInput::new(),
            search_active: false,
            search_pattern: None,
            search_error: None,
            events_scroll: 0,
            events_max_scroll: 0,
            events_level: EventLevel::default(),
            events_window: EventWindow::default(),
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
            config_cursor: 0,
            config_edit: None,
            config_scroll: 0,
            config_delete_confirm: None,
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

    #[test]
    fn event_level_cycles_and_parses_severities() {
        assert_eq!(EventLevel::default(), EventLevel::All);
        assert_eq!(EventLevel::All.next(), EventLevel::Info);
        assert_eq!(EventLevel::Info.next(), EventLevel::Warn);
        assert_eq!(EventLevel::Warn.next(), EventLevel::Error);
        assert_eq!(EventLevel::Error.next(), EventLevel::All);
        // Severity-string ranking, incl. the synonyms + unknown→INFO.
        assert!(severity_rank("ERROR") > severity_rank("WARN"));
        assert!(severity_rank("WARN") > severity_rank("INFO"));
        assert!(severity_rank("INFO") > severity_rank("DEBUG"));
        assert_eq!(severity_rank("WARNING"), severity_rank("WARN"));
        assert_eq!(severity_rank("fatal"), severity_rank("ERROR"));
        assert_eq!(severity_rank("garbage"), severity_rank("INFO"));
    }

    #[test]
    fn event_window_cycles_and_computes_cutoff() {
        assert_eq!(EventWindow::default(), EventWindow::All);
        assert_eq!(EventWindow::All.next(), EventWindow::H1);
        assert_eq!(EventWindow::D7.next(), EventWindow::All);
        let now = chrono::Utc::now();
        assert_eq!(EventWindow::All.cutoff(now), None);
        assert_eq!(
            EventWindow::H1.cutoff(now),
            Some(now - chrono::Duration::hours(1))
        );
        assert_eq!(
            EventWindow::D7.cutoff(now),
            Some(now - chrono::Duration::days(7))
        );
    }

    #[test]
    fn filter_event_indices_applies_severity_floor() {
        // mins_ago small so the time window never excludes these.
        let events = vec![
            mk_event("INFO", 1),
            mk_event("WARN", 2),
            mk_event("ERROR", 3),
            mk_event("DEBUG", 4),
        ];
        let now = chrono::Utc::now();
        // All → every event.
        assert_eq!(
            filter_event_indices(&events, EventLevel::All, EventWindow::All, now).len(),
            4
        );
        // Warn+ → WARN + ERROR only.
        assert_eq!(
            filter_event_indices(&events, EventLevel::Warn, EventWindow::All, now),
            vec![1, 2]
        );
        // Error → ERROR only.
        assert_eq!(
            filter_event_indices(&events, EventLevel::Error, EventWindow::All, now),
            vec![2]
        );
    }

    #[test]
    fn filter_event_indices_applies_time_window() {
        let events = vec![
            mk_event("INFO", 10),          // 10 min ago
            mk_event("INFO", 120),         // 2 h ago
            mk_event("INFO", 60 * 24 * 3), // 3 days ago
        ];
        let now = chrono::Utc::now();
        // 1h window → only the 10-min-old event.
        assert_eq!(
            filter_event_indices(&events, EventLevel::All, EventWindow::H1, now),
            vec![0]
        );
        // 24h window → the 10-min + 2h events.
        assert_eq!(
            filter_event_indices(&events, EventLevel::All, EventWindow::D1, now),
            vec![0, 1]
        );
        // 7d window → all three.
        assert_eq!(
            filter_event_indices(&events, EventLevel::All, EventWindow::D7, now).len(),
            3
        );
    }

    fn mk_edit(value: &str) -> ConfigEdit {
        ConfigEdit {
            kind: ConfigItemKind::EnvVar,
            key: "K".into(),
            original: value.into(),
            input: value.into(),
            caret: value.chars().count(),
            mode: ConfigEditMode::Value,
        }
    }

    #[test]
    fn config_edit_caret_moves_and_clamps() {
        let mut e = mk_edit("abc");
        assert_eq!(e.caret, 3);
        e.move_right(); // already at end — clamps
        assert_eq!(e.caret, 3);
        e.move_left();
        e.move_left();
        assert_eq!(e.caret, 1);
        e.move_home();
        assert_eq!(e.caret, 0);
        e.move_left(); // at start — clamps
        assert_eq!(e.caret, 0);
        e.move_end();
        assert_eq!(e.caret, 3);
    }

    #[test]
    fn config_edit_insert_at_caret() {
        let mut e = mk_edit("ac");
        e.move_left(); // caret between a and c
        assert_eq!(e.caret, 1);
        e.insert('b');
        assert_eq!(e.input, "abc");
        assert_eq!(e.caret, 2);
        e.move_home();
        e.insert('X');
        assert_eq!(e.input, "Xabc");
        assert_eq!(e.caret, 1);
    }

    #[test]
    fn config_edit_backspace_and_delete() {
        let mut e = mk_edit("abc");
        e.backspace(); // removes 'c'
        assert_eq!(e.input, "ab");
        assert_eq!(e.caret, 2);
        e.move_home();
        e.backspace(); // at start — no-op
        assert_eq!(e.input, "ab");
        e.delete(); // removes 'a' at caret
        assert_eq!(e.input, "b");
        assert_eq!(e.caret, 0);
        e.move_end();
        e.delete(); // at end — no-op
        assert_eq!(e.input, "b");
    }

    #[test]
    fn config_edit_split_at_caret() {
        let mut e = mk_edit("hello");
        e.move_left();
        e.move_left();
        assert_eq!(e.split_at_caret(), ("hel", "lo"));
        e.move_home();
        assert_eq!(e.split_at_caret(), ("", "hello"));
        e.move_end();
        assert_eq!(e.split_at_caret(), ("hello", ""));
    }

    #[test]
    fn config_edit_handles_multibyte_chars() {
        // Caret arithmetic is char-based — a multi-byte char must
        // not panic insert/remove (which take byte offsets).
        let mut e = mk_edit("café");
        assert_eq!(e.caret, 4);
        e.move_left(); // before 'é'
        e.insert('X');
        assert_eq!(e.input, "cafXé");
        e.move_end();
        e.backspace(); // removes 'é'
        assert_eq!(e.input, "cafX");
    }

    #[test]
    fn parse_new_config_row_splits_on_first_equals() {
        assert_eq!(
            parse_new_config_row("PORT=8080"),
            Some(("PORT".into(), "8080".into()))
        );
        // Value may itself contain `=`.
        assert_eq!(
            parse_new_config_row("URL=a=b=c"),
            Some(("URL".into(), "a=b=c".into()))
        );
        // Key is trimmed; value is not.
        assert_eq!(
            parse_new_config_row("  KEY  = val "),
            Some(("KEY".into(), " val ".into()))
        );
        // Empty value is allowed (explicit empty env var).
        assert_eq!(
            parse_new_config_row("EMPTY="),
            Some(("EMPTY".into(), "".into()))
        );
    }

    #[test]
    fn parse_new_config_row_rejects_malformed() {
        // No `=` at all.
        assert_eq!(parse_new_config_row("JUSTAKEY"), None);
        // Empty / whitespace key.
        assert_eq!(parse_new_config_row("=value"), None);
        assert_eq!(parse_new_config_row("   =value"), None);
    }

    #[test]
    fn config_editable_items_empty_when_no_tags_or_env_vars() {
        let d = empty_detail("Web");
        assert!(config_editable_items(&d).is_empty());
    }

    #[test]
    fn config_editable_items_lists_tags_sorted_then_env_vars_natural() {
        let mut d = empty_detail("Web");
        // Tags inserted out of order — expect case-insensitive sort.
        d.tags = vec![
            ("Zone".into(), "eu".into()),
            ("app".into(), "uflexi".into()),
        ];
        // Env vars keep natural (insertion) order, NOT sorted.
        d.env_vars = vec![("PORT".into(), "8080".into()), ("DEBUG".into(), "0".into())];
        let items = config_editable_items(&d);
        assert_eq!(items.len(), 4);
        // Tags first, sorted case-insensitively: app, Zone.
        assert_eq!(items[0].kind, ConfigItemKind::Tag);
        assert_eq!(items[0].key, "app");
        assert_eq!(items[1].key, "Zone");
        // Env vars next, in insertion order: PORT, DEBUG.
        assert_eq!(items[2].kind, ConfigItemKind::EnvVar);
        assert_eq!(items[2].key, "PORT");
        assert_eq!(items[2].value, "8080");
        assert_eq!(items[3].key, "DEBUG");
    }

    #[test]
    fn clamp_config_cursor_pulls_back_past_end() {
        let mut d = empty_detail("Web");
        d.env_vars = vec![
            ("A".into(), "1".into()),
            ("B".into(), "2".into()),
            ("C".into(), "3".into()),
        ];
        // Cursor was on row 2; a delete shrank the list to 2 rows.
        d.config_cursor = 2;
        d.env_vars.truncate(2);
        d.clamp_config_cursor();
        assert_eq!(d.config_cursor, 1);
        // Cursor already in range — left alone.
        d.config_cursor = 0;
        d.clamp_config_cursor();
        assert_eq!(d.config_cursor, 0);
        // Empty list — cursor pinned to 0, not len-1 underflow.
        d.env_vars.clear();
        d.config_cursor = 5;
        d.clamp_config_cursor();
        assert_eq!(d.config_cursor, 0);
    }

    #[test]
    fn config_editable_items_handles_only_env_vars() {
        let mut d = empty_detail("Web");
        d.env_vars = vec![("A".into(), "1".into())];
        let items = config_editable_items(&d);
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].kind, ConfigItemKind::EnvVar);
    }

    #[test]
    fn filter_event_indices_keeps_events_with_no_timestamp() {
        // An event with `at: None` can't be excluded by a time window.
        let mut e = mk_event("INFO", 0);
        e.at = None;
        let events = vec![e];
        let now = chrono::Utc::now();
        assert_eq!(
            filter_event_indices(&events, EventLevel::All, EventWindow::H1, now),
            vec![0]
        );
    }
}
