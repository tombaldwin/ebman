//! Navigating the per-environment Detail view: opening it, cycling
//! and scrolling tabs, drilling into a health item, and the in-detail
//! search. The Detail *state* itself lives in `crate::mode_detail`.

use super::*;

/// Why the active Detail tab is being refreshed. Only top errors, which
/// is billed per scan, tells these apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefreshTrigger {
    /// `^R`.
    Asked,
    /// The tab came on screen.
    Opened,
    /// The auto-refresh tick.
    Tick,
}

/// How long a top-errors answer stands when the App tab is merely
/// opened again.
const TOP_ERRORS_FRESH: chrono::TimeDelta = chrono::TimeDelta::minutes(5);

/// Whether a refresh runs the top-errors query, which is billed per GB
/// scanned: always when asked, on opening only past [`TOP_ERRORS_FRESH`]
/// (or with no answer yet), and never on the auto-refresh tick.
fn top_errors_due(
    trigger: RefreshTrigger,
    fetched_at: Option<chrono::DateTime<chrono::Utc>>,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    match trigger {
        RefreshTrigger::Asked => true,
        RefreshTrigger::Opened => fetched_at.is_none_or(|t| now - t >= TOP_ERRORS_FRESH),
        RefreshTrigger::Tick => false,
    }
}

impl App {
    pub(crate) fn open_detail(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let mut tabs = vec![
            DetailTab::Health,
            DetailTab::App,
            DetailTab::Events,
            DetailTab::Instances,
            DetailTab::Metrics,
        ];
        if env.tier == "Worker" {
            tabs.push(DetailTab::Queue);
        }
        tabs.push(DetailTab::Logs);
        tabs.push(DetailTab::Config);
        // A previous env's outstanding fetch must not gate this one.
        self.detail_fetch_started = None;
        let key = self.env_key(&env);
        let detail = DetailState {
            env_name: env.name.clone(),
            key,
            env_snapshot: env,
            tabs,
            tab_idx: 0,
            events: Vec::new(),
            instances: Vec::new(),
            queues: WorkerQueues::default(),
            metrics: Vec::new(),
            metrics_range_secs: 3600, // 1h default
            app_tab: Default::default(),
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
            events_request: Default::default(),
            instances_request: Default::default(),
            queues_request: Default::default(),
            metrics_request: Default::default(),
            tags_request: Default::default(),
            env_vars_request: Default::default(),
            error: None,
            log_tail: LogTail::default(),
            queue_cursor: 0,
            instances_cursor: 0,
            instance_terminate_confirm: None,
            health_cursor: 0,
            metrics_hover_col: None,
            metrics_body_rect: None,
            cw_alarms: Default::default(),
            recent_versions: Default::default(),
            config_cursor: 0,
            config_edit: None,
            config_scroll: 0,
            config_delete_confirm: None,
        };
        self.detail = Some(detail);
        self.mode = Mode::Detail;
        self.detail_refresh_on_open();
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
    pub(crate) fn drill_health_item(&mut self) {
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
                self.detail_refresh_on_open();
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
                self.detail_refresh_on_open();
            }
        }
    }

    pub(crate) fn detail_cycle_tab(&mut self, delta: i32) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let n = detail.tabs.len() as i32;
        let next = crate::util::wrap_index(detail.tab_idx, delta as isize, n as usize);
        detail.tab_idx = next;
        self.detail_refresh_on_open();
        // NB: an earlier iteration auto-spawned the CW Logs streaming
        // overlay here when groups were discovered. Reverted because
        // jumping into a popup obscures the Logs tab's own snapshot path
        // (`^R`) and removes the explicit opt-in that `s` represents.
        // Pressing `s` on the Logs tab is the way to open the stream;
        // the in-overlay `g` keybind switches between discovered groups.
    }

    pub(crate) fn detail_scroll(&mut self, delta: i32) {
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
                let next = crate::util::wrap_index(cur as usize, delta as isize, n);
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
                //
                // With exactly two rows, `+ delta` and `- delta` are the
                // same operation mod 2, so the sweep reports flipping
                // that sign as survivable — correctly. Equivalent, not a
                // coverage gap. It stops being equivalent the moment a
                // third row appears here, and the wrap test would then
                // need a direction assertion.
                let n: i32 = 2;
                let cur = detail.queue_cursor as i32;
                detail.queue_cursor =
                    crate::util::wrap_index(cur as usize, delta as isize, n as usize);
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
                detail.health_cursor =
                    crate::util::wrap_index(cur as usize, delta as isize, n as usize);
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
                detail.config_cursor = crate::util::clamp_index(cur as usize, delta as isize, n);
            }
            // Metrics tab has no scrollable cursor — the chart body
            // handles its own keyboard interactions. Nor does App: one
            // row per series fits the screen.
            DetailTab::Metrics | DetailTab::App => {}
        }
    }

    /// After this long an outstanding fetch is treated as lost rather
    /// than slow, so a result that never arrives can't wedge the tab.
    /// Well past any real `SCAN_PAGES` walk; short enough that an
    /// operator retrying by hand isn't left tapping.
    const DETAIL_FETCH_STUCK_AFTER: Duration = Duration::from_secs(120);

    /// `^R`: refresh the active tab, everything on it.
    pub(crate) fn detail_refresh_active_tab(&mut self) {
        self.detail_refresh(RefreshTrigger::Asked);
    }

    /// A tab came on screen (Detail opened, a tab switch, a drill-in).
    /// The same as `^R`, except that a top-errors answer younger than
    /// [`TOP_ERRORS_FRESH`] is kept: cycling past the App tab must not
    /// buy a Logs Insights scan each time.
    pub(crate) fn detail_refresh_on_open(&mut self) {
        self.detail_refresh(RefreshTrigger::Opened);
    }

    /// The auto-refresh tick's refresh: the same as `^R`, except that
    /// what is billed per scan (top errors' Logs Insights query) waits
    /// for a person to ask.
    pub(crate) fn detail_refresh_on_tick(&mut self) {
        self.detail_refresh(RefreshTrigger::Tick);
    }

    /// The refresh tick's part for an open Detail view: refresh the
    /// active tab when `R` turned auto-refresh on. Out of the run loop's
    /// `select!` so a test can drive it.
    pub(crate) fn refresh_open_detail_on_tick(&mut self) {
        if matches!(self.mode, Mode::Detail) && self.detail.as_ref().is_some_and(|d| d.auto_refresh)
        {
            self.detail_refresh_on_tick();
        }
    }

    /// Re-read the live config (`app.` lines and `alarm_dimensions`,
    /// [`crate::config::LiveConfig`]) when the config file changed since
    /// it was last read, so a hand edit applies without a restart.
    /// Called on every fleet refresh, which covers the table's alarm
    /// marks, and wherever the config is about to be used between
    /// refreshes: any Detail refresh, the App tab's editor, `:alarms`,
    /// `:why`. Returns whether anything was re-read.
    pub(crate) fn reload_config_if_changed(&mut self) -> bool {
        let Some(path) = self.config_file.clone() else {
            return false;
        };
        let stamp = crate::config::stamp_of(&path);
        if stamp == self.config_seen {
            return false;
        }
        // Unreadable: keep what we have, and try again next time.
        let Some(live) = crate::config::live_config_from_read(std::fs::read_to_string(&path))
        else {
            return false;
        };
        self.config_seen = stamp;
        self.set_alarm_dimensions(live.alarm_dimensions);
        self.set_app_tabs(live.app_tabs);
        true
    }

    /// Replace `alarm_dimensions`. They decide which alarms are an env's,
    /// as the prefix does, so a change empties the alarms the Detail
    /// view fetched under the old ones. The one writer: a reload and a
    /// `:settings` save both come through here.
    pub(crate) fn set_alarm_dimensions(&mut self, dims: Vec<String>) {
        if dims == self.cfg.alarm_dimensions {
            return;
        }
        self.cfg.alarm_dimensions = dims;
        if let Some(d) = self.detail.as_mut() {
            d.cw_alarms.reset();
        }
    }

    /// Replace the app. config, and forget what the Detail view fetched
    /// under the old one wherever it no longer applies: the rows would
    /// otherwise stay on screen beside config they were not fetched for,
    /// and an answer still in flight for the old config would land as
    /// the new one's. Top errors only when what it queries changed: it
    /// is billed per scan, and a threshold edit is no reason to rerun it.
    pub(crate) fn set_app_tabs(&mut self, tabs: crate::app_tab::AppTabs) {
        let old = std::mem::replace(&mut self.cfg.app_tabs, tabs);
        let Some(d) = self.detail.as_mut() else {
            return;
        };
        let (was, now) = (
            old.envs.get(&d.env_name),
            self.cfg.app_tabs.envs.get(&d.env_name),
        );
        // Each fetch is reset only by a change to what it reads: the
        // series by everything but the alarm prefix and the query, the
        // alarms by the prefix, top errors by its group and fingerprint.
        let series = |s: Option<&crate::app_tab::AppTabSpec>| {
            s.map(|s| crate::app_tab::AppTabSpec {
                alarm_prefix: None,
                log_group: None,
                error_fingerprint: None,
                ..s.clone()
            })
        };
        let prefix =
            |s: Option<&crate::app_tab::AppTabSpec>| s.and_then(|s| s.alarm_prefix.clone());
        let queried = |s: Option<&crate::app_tab::AppTabSpec>| {
            s.map(|s| (s.log_group.clone(), s.error_fingerprint.clone()))
        };
        // A spec gained or lost differs too: the series then come from
        // a preview, or from none.
        if series(was) != series(now) {
            d.app_tab.metrics.reset();
        }
        if prefix(was) != prefix(now) {
            d.cw_alarms.reset();
        }
        if queried(was) != queried(now) {
            d.app_tab.top_errors.reset();
        }
    }

    /// [`reload_config_if_changed`](Self::reload_config_if_changed)
    /// outside a Detail refresh (the editor, `:alarms`, `:why`): what the
    /// reload emptied on the tab on screen is fetched again. A refresh
    /// fetches it anyway.
    pub(super) fn reload_config_and_refetch(&mut self) {
        self.reload_config_if_changed();
        self.refetch_what_an_edit_emptied();
    }

    /// Fetch again what [`set_app_tabs`](Self::set_app_tabs) left blank
    /// on the tab on screen: the alarms on the Health and App tabs (they
    /// share them), the series on the App tab. Top errors is billed per
    /// scan, so it waits for `^R`, and the tab says so. Without this a
    /// reset fetch stayed blank until the next refresh, which reads as no
    /// alarms at all.
    pub(super) fn refetch_what_an_edit_emptied(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let tab = d.tab();
        let series = tab == DetailTab::App && d.app_tab.metrics.is_blank();
        let alarms = matches!(tab, DetailTab::App | DetailTab::Health) && d.cw_alarms.is_blank();
        let env = d.env_name.clone();
        if series {
            self.spawn_detail_app(env.clone());
        }
        if alarms {
            self.spawn_detail_alarms(env);
        }
        if series || alarms {
            // As a refresh would, so the tick's stuck check measures from
            // this fetch rather than an older one.
            self.detail_fetch_started = Some(std::time::Instant::now());
        }
    }

    /// The App tab's part of a refresh: its instance rows and alarm line
    /// (each unless already coming), top errors when due, discovery for
    /// an env with no config, and its series.
    fn refresh_app_tab(&mut self, env_name: String, trigger: RefreshTrigger) {
        // Its instance rows come from the instance list; one
        // already in flight (the eager fetch on open) will do.
        if !self
            .detail
            .as_ref()
            .is_some_and(|d| d.instances_request.in_flight())
        {
            self.spawn_detail_instances(env_name.clone());
        }
        // And its alarm line from the Health tab's alarm list.
        if !self
            .detail
            .as_ref()
            .is_some_and(|d| d.cw_alarms.in_flight())
        {
            self.spawn_detail_alarms(env_name.clone());
        }
        let fetched_at = self
            .detail
            .as_ref()
            .and_then(|d| d.app_tab.top_errors.ready())
            .map(|t| t.fetched_at);
        if top_errors_due(trigger, fetched_at, chrono::Utc::now()) {
            self.spawn_detail_top_errors(env_name.clone());
        }
        // No config yet: find what the env uses, kept for the
        // session, so the tab can say so rather than point at a
        // file.
        let key = self.detail_key();
        if let Some(key) = key.filter(|k| !self.app_tab_source(k).is_configured()) {
            // `^R` looks again: resources come and go, and a
            // result cached for the session would never see them.
            if trigger == RefreshTrigger::Asked {
                self.caches.app_tab_discovery.discovered.remove(&key);
            }
            if self.discovered_for(&key).is_none() {
                self.spawn_app_discovery(key);
            }
        }
        self.spawn_detail_app(env_name);
    }

    fn detail_refresh(&mut self, trigger: RefreshTrigger) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        // Don't stack a refresh on one that's still running. The
        // auto-refresh tick fires every 15 seconds and the interactive
        // scans behind these tabs can take longer than that, so
        // without this a slow scan collected a new companion every
        // tick — each one a fresh fan of sequential AWS calls against
        // an account that is, by the time anyone is watching this
        // screen, usually having a bad day already.
        if detail.tab_loading()
            && self
                .detail_fetch_started
                .is_some_and(|t| t.elapsed() < Self::DETAIL_FETCH_STUCK_AFTER)
        {
            return;
        }
        let env_name = detail.env_name.clone();
        let app_name = detail.env_snapshot.application.clone();
        let is_worker = detail.env_snapshot.tier.eq_ignore_ascii_case("Worker");
        let tab = detail.tab();
        // Release the immutable borrow of `detail` before calling
        // spawn_* methods which take `&mut self`.
        let _ = detail;
        // Before anything reads app. config (the App tab's fetches, every
        // tab's alarm match), and past the in-flight guard: a reload
        // resets the fetches the edit changed (`set_app_tabs`), and a
        // refresh the guard then skipped would start none to replace
        // them, leaving the tab blank until the next one.
        self.reload_config_if_changed();
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
                self.spawn_detail_recent_versions(app_name.clone());
                if is_worker {
                    self.spawn_detail_queues(app_name, env_name);
                }
            }
            DetailTab::Events => self.spawn_detail_events(env_name),
            DetailTab::Instances => self.spawn_detail_instances(env_name),
            DetailTab::Queue => self.spawn_detail_queues(app_name, env_name),
            DetailTab::Metrics => self.spawn_detail_metrics(env_name),
            DetailTab::App => self.refresh_app_tab(env_name, trigger),
            DetailTab::Logs => self.spawn_detail_logs(env_name),
            DetailTab::Config => {}
        }
        self.detail_fetch_started = Some(Instant::now());
    }

    pub(crate) fn handle_detail_search_key(&mut self, key: KeyEvent) {
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

    pub(crate) fn detail_search_jump(&mut self, delta: i32) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_errors_runs_when_asked_on_opening_once_stale_and_never_on_the_tick() {
        let now = chrono::DateTime::from_timestamp(10_000, 0).unwrap();
        let just_now = Some(now - chrono::TimeDelta::seconds(10));
        let at_fresh = Some(now - TOP_ERRORS_FRESH);
        let due = |t, at| top_errors_due(t, at, now);
        assert!(due(RefreshTrigger::Asked, just_now), "^R always");
        assert!(due(RefreshTrigger::Opened, None), "opened with nothing yet");
        assert!(
            !due(RefreshTrigger::Opened, just_now),
            "opened, still fresh"
        );
        assert!(due(RefreshTrigger::Opened, at_fresh), "opened, at the edge");
        assert!(!due(RefreshTrigger::Tick, None), "the tick never");
    }
}
