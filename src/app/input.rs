//! Raw terminal input: the crossterm event fan-out, mouse handling,
//! and the top-level `handle_key` keymap.
//!
//! House rule that bites here: guarded `KeyCode::Char(c) if Ctrl`
//! arms MUST precede the unguarded `KeyCode::Char(c)` arm for the same
//! character — the compiler does not warn about the shadowing.

use super::*;

impl App {
    pub(crate) fn handle_event(&mut self, event: Event) {
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

    pub(crate) fn handle_mouse(&mut self, m: MouseEvent) {
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

    pub(crate) fn update_hover(&mut self, row: u16) {
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

    pub(crate) fn select_row_at(&mut self, _col: u16, row: u16) {
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

    pub(crate) fn handle_key(&mut self, key: KeyEvent) {
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
                        self.view.redact = !self.view.redact;
                        self.status_message = Some(if self.view.redact {
                            "redact mode ON".into()
                        } else {
                            "redact mode off".into()
                        });
                    }
                    KeyCode::Char('g') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        self.view.set_grouped(!self.view.grouped());
                        self.rebuild_view();
                        self.status_message = Some(if self.view.grouped() {
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
                        self.view.mode = self.view.mode.next();
                        self.status_message = Some(format!("view: {}", self.view.mode.label()));
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
                        self.view.sort_key = self.view.sort_key.next();
                        self.resort_envs();
                        self.status_message = Some(format!(
                            "sort: {} ({})",
                            self.view.sort_key.label(),
                            if self.view.sort_desc { "desc" } else { "asc" }
                        ));
                    }
                    KeyCode::Char('S') => {
                        self.view.sort_desc = !self.view.sort_desc;
                        self.resort_envs();
                        self.status_message = Some(format!(
                            "sort: {} ({})",
                            self.view.sort_key.label(),
                            if self.view.sort_desc { "desc" } else { "asc" }
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
                        self.view.filter_mut().clear();
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
    pub(crate) fn handle_control_op(&mut self, op: crate::control::ControlOp, _terminal: &mut Tui) {
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
                    esc(self.view.filter().text()),
                    load,
                    self.view.sort_key.label(),
                    self.view.grouped(),
                    self.view.redact,
                    self.focus,
                );
                let _ = reply.send(json);
            }
        }
    }
}
