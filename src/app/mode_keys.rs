//! Key handlers for the small text-input / list-navigation modes.
//!
//! Mirrors the `cmd_*.rs` split: each of these used to be a multi-arm
//! `match key.code` block inline in `handle_key`'s top-level mode
//! dispatch, which made the dispatch site itself hundreds of lines
//! long. Lifting them out leaves the dispatcher as one-liner method
//! calls and lets each mode's body sit next to nothing else.
//!
//! Bigger modes (`Detail`, `Action`, `Dlq`, `Form`, `Shell`) already
//! had their own `handle_*_key` helpers — those stay where they were.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

// The Detail and Normal keymaps moved here from `input.rs`, which
// imports `super::*`. Naming what they need keeps this module's
// surface visible rather than pulling the whole parent in.
use super::{App, DetailTab, Focus, HelpTopic, Mode, Scope, YankKind};

impl App {
    /// `Mode::Filter` — typing builds up `self.view.filter()` and re-runs
    /// `rebuild_view` so the table reflects the search live; Esc
    /// clears + exits; Enter commits and exits.
    pub(super) fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.view.filter_mut().clear();
                self.mode = Mode::Normal;
                self.rebuild_view();
            }
            KeyCode::Enter => self.mode = Mode::Normal,
            // TextInput consumes editing keys (cursor move / Ctrl-W);
            // rebuild the view on any accepted edit so the table tracks
            // the filter live. `filter_handle_key` only marks the cache
            // stale when the key was actually consumed — asking with
            // `filter_mut()` would dirty it even for keys that fall
            // through to the no-op arm below, which never rebuilds.
            _ if self.view.filter_handle_key(key) => self.rebuild_view(),
            _ => {}
        }
    }

    /// `Mode::Help` — Esc / `?` / q dismisses (restoring the
    /// pre-help mode + overlay so `?` from a Detail / overlay
    /// context returns the operator to where they were). j/k scroll.
    pub(super) fn handle_help_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {
                // Restore the screen the user was on before opening
                // help. `pre_help_mode` is set at every `?` keypress; if
                // somehow missing, fall back to Normal so we don't get
                // stuck in Help.
                self.mode = self.help.pre_mode.take().unwrap_or(Mode::Normal);
                if let Some(overlay) = self.help.pre_overlay.take() {
                    self.current_overlay = Some(overlay);
                }
                self.help.scroll = 0;
            }
            KeyCode::Char('j') | KeyCode::Down => {
                // Clamp to the last-known content bound so scrolling
                // past the end doesn't accumulate phantom offsets.
                self.help.scroll = self.help.scroll.saturating_add(1).min(self.help.max_scroll);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.help.scroll = self.help.scroll.saturating_sub(1);
            }
            _ => {}
        }
    }

    /// `Mode::Command` — the `:` prompt. Enter dispatches via
    /// `execute_command`; Tab / Shift-Tab cycles through completion
    /// matches; any printable key (besides Tab) resets the cycle.
    pub(super) fn handle_command_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.command_input.clear();
                self.completion.origin = None;
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let cmd = self.command_input.text().to_string();
                self.command_input.clear();
                self.completion.origin = None;
                self.mode = Mode::Normal;
                self.execute_command(&cmd);
            }
            KeyCode::Tab => self.command_completion_step(1),
            KeyCode::BackTab => self.command_completion_step(-1),
            // Any edit — typing, backspace, cursor move, Ctrl-W — is
            // delegated to TextInput and resets the completion cycle so
            // the operator's next Tab starts a fresh search.
            _ if self.command_input.handle_key(key) => {
                self.completion.origin = None;
            }
            _ => {}
        }
    }

    /// `Mode::Palette` — the `Ctrl-K` fuzzy command palette. ↑/↓
    /// moves the cursor, Enter dispatches the selection, any printable
    /// re-filters.
    pub(super) fn handle_palette_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.mode = Mode::Normal;
                self.palette_input.clear();
            }
            KeyCode::Down => self.palette_move(1),
            KeyCode::Up => self.palette_move(-1),
            KeyCode::Enter => self.palette_execute(),
            // TextInput consumes editing keys (insert / backspace /
            // delete / cursor move / Ctrl-W word-delete); re-filter on
            // any accepted edit. Non-editing keys fall through.
            _ if self.palette_input.handle_key(key) => self.palette_refilter(),
            _ => {}
        }
    }

    /// `Mode::QuickJump` — the `'`-prefixed name-prefix jump. Typing
    /// moves the table cursor to the first env whose name starts with
    /// the buffer; Enter / Esc commit / cancel respectively (both
    /// return to Normal — the cursor stays where it landed on the
    /// last typed character).
    pub(super) fn handle_quickjump_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.quickjump_input.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                self.quickjump_input.clear();
                self.mode = Mode::Normal;
            }
            // TextInput consumes editing keys; re-run the prefix jump on
            // any accepted edit. Non-editing keys fall through.
            _ if self.quickjump_input.handle_key(key) => self.quickjump_apply(),
            _ => {}
        }
    }

    /// `Mode::Picker` — generic single-select list picker used by
    /// region / profile / log-group / swap-target. j/k or ↑/↓ moves;
    /// typing filters; Enter applies; Esc cancels.
    pub(super) fn handle_picker_key(&mut self, key: KeyEvent) {
        match key.code {
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
            KeyCode::Up | KeyCode::Char('k') if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                if let Some(p) = self.picker.as_mut() {
                    p.move_selection(-1);
                }
            }
            // TextInput consumes editing keys (insert / backspace /
            // cursor move / Ctrl-W); after any accepted edit, keep the
            // selection on a still-matching row.
            _ => {
                if let Some(p) = self.picker.as_mut() {
                    if p.filter.handle_key(key) {
                        let filt = p.filtered();
                        if !filt.iter().any(|i| Some(*i) == p.list_state.selected()) {
                            p.list_state.select(filt.first().copied());
                        }
                    }
                }
            }
        }
    }

    /// `Mode::Detail` keys.
    ///
    /// Moved from `handle_key`'s mode dispatch, text-identical, for
    /// the reason the module doc already gives: the dispatcher should
    /// be one-liner calls. Detail and Normal were the last two modes
    /// still inline, and between them they were most of an 842-line
    /// function.
    ///
    /// Readability, not defect risk — the 228 mutation survivors in
    /// `input.rs` are the arms with no test, and moving them does not
    /// change that. Kept honest rather than folded into the exhaustive
    /// routing commit, which does reduce defect risk.
    pub(super) fn handle_detail_mode_key(&mut self, key: KeyEvent) {
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
            KeyCode::Char('a') => {
                // Refusal already surfaced its own message.
                let _ = self.open_action_menu();
            }
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
                if matches!(self.detail.as_ref().map(|d| d.tab()), Some(DetailTab::Logs)) =>
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
                if matches!(self.detail.as_ref().map(|d| d.tab()), Some(DetailTab::Logs)) =>
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

    /// `Mode::Normal` keys — the main table.
    pub(super) fn handle_normal_key(&mut self, key: KeyEvent) {
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
                self.status_message = Some(format!("apps multi-select cleared ({n} app(s))"));
            }
            KeyCode::Tab => self.set_scope(self.scope.next()),
            KeyCode::BackTab => self.set_scope(self.scope.prev()),
            KeyCode::Enter if self.scope == Scope::Apps => self.drill_into_app(),
            KeyCode::Enter => self.open_detail(),
            KeyCode::Char('a') if self.scope == Scope::Apps => {
                self.open_apps_action_menu();
            }
            KeyCode::Char('a') if self.scope == Scope::Envs => {
                // Refusal already surfaced its own message.
                let _ = self.open_action_menu();
            }
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
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.event_panel.visible =>
            {
                self.event_panel.height = (self.event_panel.height + 1).min(30);
            }
            KeyCode::Down
                if key.modifiers.contains(KeyModifiers::CONTROL) && self.event_panel.visible =>
            {
                self.event_panel.height = self.event_panel.height.saturating_sub(1).max(4);
            }
            KeyCode::Char('s') => {
                self.set_sort(self.view.sort_key().next(), self.view.sort_desc());
                self.status_message = Some(format!(
                    "sort: {} ({})",
                    self.view.sort_key().label(),
                    if self.view.sort_desc() { "desc" } else { "asc" }
                ));
            }
            KeyCode::Char('S') => {
                self.set_sort(self.view.sort_key(), !self.view.sort_desc());
                self.status_message = Some(format!(
                    "sort: {} ({})",
                    self.view.sort_key().label(),
                    if self.view.sort_desc() { "desc" } else { "asc" }
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
                if matches!(self.focus, Focus::Events) && self.event_panel.cursor.is_none() {
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
                self.event_panel.cursor = self.event_panel.cursor.and_then(|c| c.checked_sub(1));
            }
            KeyCode::Char('b') if self.scope == Scope::Envs => self.open_in_console(),
            KeyCode::Char('D') if self.scope == Scope::Envs => self.open_describe_overlay(),
            KeyCode::Char('*') if self.scope == Scope::Envs => self.toggle_pin_selected(),
            KeyCode::Char('*') if self.scope == Scope::Apps => self.toggle_pin_selected_app(),
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
                    self.error_message = Some(
                        "no env selected — press 1-9, click a row, or type ' to jump by name"
                            .into(),
                    );
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
                        .map(|c| (c + 1).min(self.event_panel.events.len().saturating_sub(1)))
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
