//! Navigating the per-environment Detail view: opening it, cycling
//! and scrolling tabs, drilling into a health item, and the in-detail
//! search. The Detail *state* itself lives in `crate::mode_detail`.

use super::*;

impl App {
    pub(crate) fn open_detail(&mut self) {
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
        // A previous env's outstanding fetch must not gate this one.
        self.detail_fetch_started = None;
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
            cw_alarms: Default::default(),
            recent_versions: Default::default(),
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

    pub(crate) fn detail_cycle_tab(&mut self, delta: i32) {
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

    /// After this long an outstanding fetch is treated as lost rather
    /// than slow, so a result that never arrives can't wedge the tab.
    /// Well past any real `SCAN_PAGES` walk; short enough that an
    /// operator retrying by hand isn't left tapping.
    const DETAIL_FETCH_STUCK_AFTER: Duration = Duration::from_secs(120);

    pub(crate) fn detail_refresh_active_tab(&mut self) {
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
