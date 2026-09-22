//! Raw terminal input: the crossterm event fan-out, mouse handling,
//! and the top-level `handle_key` keymap.
//!
//! House rule that bites here: guarded `KeyCode::Char(c) if Ctrl`
//! arms MUST precede the unguarded `KeyCode::Char(c)` arm for the same
//! character — the compiler does not warn about the shadowing.

use super::*;

/// Map a terminal screen line to an index into the display rows.
///
/// Pure so it can be tested without a terminal. `row_height` is the
/// piece that was missing: in `ViewMode::Spacious` each row occupies two
/// screen lines, so a click N lines below the first data line is row
/// N/2, not row N. Without it, clicking the 3rd visible environment in
/// spacious mode selected the 5th.
pub(crate) fn table_row_at(
    offset: usize,
    screen_row: u16,
    data_top: u16,
    row_height: u16,
) -> usize {
    let lines_below_top = screen_row.saturating_sub(data_top);
    offset + (lines_below_top / row_height.max(1)) as usize
}

/// Which handler owns the keys while an overlay is open.
///
/// This was an if-chain of `matches!` tests whose precedence lived in
/// statement order, and it was non-exhaustive by construction: a new
/// `Overlay` variant needing its own keys relied on somebody
/// remembering to insert a branch, at the right point, in a
/// hundred-line block. A missed one is a dead key — or worse, a key
/// leaking through the overlay into Normal mode.
///
/// The same move ARCHITECTURE.md rule 3 makes for `AppMsg::generation`:
/// the compiler FORCES a new variant to be classified, and the answer
/// is a word rather than a position in a list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OverlayRoute {
    SavedConfigsInteractive,
    LogTail,
    EventTail,
    AppsActionMenu,
    ReportBug,
    /// The shared path: `:why` cursor navigation and drill-in, the
    /// universal Esc/q dismiss, and each variant's extra dismiss key.
    /// Most overlays are read-only popups and want exactly this.
    Generic,
}

impl Overlay {
    /// Exhaustive on purpose. Adding a variant without deciding this
    /// is a compile error, which is the entire point.
    fn key_route(&self) -> OverlayRoute {
        match self {
            Overlay::SavedConfigsInteractive { .. } => OverlayRoute::SavedConfigsInteractive,
            Overlay::LogTail { .. } => OverlayRoute::LogTail,
            Overlay::EventTail { .. } => OverlayRoute::EventTail,
            Overlay::AppsActionMenu { .. } => OverlayRoute::AppsActionMenu,
            Overlay::ReportBug { .. } => OverlayRoute::ReportBug,
            // Named individually rather than caught by `_`. A wildcard
            // would silently route a new variant to Generic and
            // reintroduce the forgetting this exists to prevent.
            Overlay::About(_)
            | Overlay::Describe(_)
            | Overlay::Whatsnew(_)
            | Overlay::History(_)
            | Overlay::Alarms { .. }
            | Overlay::Diff(_)
            | Overlay::SavedConfigs(_)
            | Overlay::TextDump { .. }
            | Overlay::WhyRed { .. } => OverlayRoute::Generic,
        }
    }
}

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
        let target = table_row_at(offset, row, data_top, self.view.mode.row_height());
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
        let target = table_row_at(offset, row, data_top, self.view.mode.row_height());
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
            // One exhaustive classification, not five ordered tests.
            // `Generic` falls through to the shared path below, which
            // is where `:why` navigation and the universal dismiss
            // live — same order, same behaviour, but a new overlay
            // variant now has to say which of these it is.
            match self.current_overlay.as_ref().map(Overlay::key_route) {
                Some(OverlayRoute::SavedConfigsInteractive) => {
                    self.handle_saved_configs_interactive_key(key);
                    return;
                }
                Some(OverlayRoute::LogTail) => {
                    self.handle_log_tail_key(key);
                    return;
                }
                Some(OverlayRoute::EventTail) => {
                    self.handle_event_tail_key(key);
                    return;
                }
                Some(OverlayRoute::AppsActionMenu) => {
                    self.handle_apps_action_menu_key(key);
                    return;
                }
                Some(OverlayRoute::ReportBug) => {
                    self.handle_report_bug_key(key);
                    return;
                }
                // No overlay, or one the shared path handles.
                None | Some(OverlayRoute::Generic) => {}
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
            Mode::Detail => self.handle_detail_mode_key(key),
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
            Mode::Normal => self.handle_normal_key(key),
        }
    }

    /// Apply a `ControlOp` received over the control socket. Snapshot ops
    /// read the terminal's current back-buffer; key/command ops dispatch
    /// through the normal handlers so all existing bindings still apply.
    /// The text the control socket's `screen` op returns.
    ///
    /// Split out of `handle_control_op` so it can be tested: that
    /// handler takes a real `Tui`, which a test cannot construct
    /// without touching the developer's terminal. `last_rendered_buffer`
    /// has exactly one reader — this — and the run loop only populates
    /// it when a control socket is attached, so a wrong condition
    /// either side would make `ebman ctl screen` return the placeholder
    /// forever, silently.
    pub(crate) fn screen_text(&self) -> String {
        self.last_rendered_buffer
            .as_ref()
            .map(crate::control::render_buffer_as_text)
            .unwrap_or_else(|| "(no frame rendered yet)".to_string())
    }

    pub(crate) fn handle_control_op(&mut self, op: crate::control::ControlOp, _terminal: &mut Tui) {
        use crate::control::ControlOp;
        match op {
            ControlOp::Screen(reply) => {
                let _ = reply.send(self.screen_text());
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
                    self.view.sort_key().label(),
                    self.view.grouped(),
                    self.view.redact,
                    self.focus,
                );
                let _ = reply.send(json);
            }
        }
    }
}

#[cfg(test)]
mod row_mapping_tests {
    use super::table_row_at;

    /// The bug this helper exists for. In `ViewMode::Spacious` a row is
    /// two screen lines tall, so the 3rd visible environment starts four
    /// lines below the first data line. The old arithmetic returned 4.
    #[test]
    fn spacious_maps_two_screen_lines_to_one_row() {
        let data_top = 5;
        // Third visible row: lines 9 and 10 (data_top + 4, + 5).
        assert_eq!(table_row_at(0, 9, data_top, 2), 2);
        assert_eq!(
            table_row_at(0, 10, data_top, 2),
            2,
            "both lines of a row select it"
        );
        // First and second, for the boundaries.
        assert_eq!(table_row_at(0, 5, data_top, 2), 0);
        assert_eq!(table_row_at(0, 6, data_top, 2), 0);
        assert_eq!(table_row_at(0, 7, data_top, 2), 1);
    }

    /// Height 1 must be exactly the old behaviour — this is the case that
    /// was already correct, and the fix must not move it.
    #[test]
    fn height_one_is_unchanged() {
        let data_top = 5;
        for line in 0..10u16 {
            assert_eq!(table_row_at(0, data_top + line, data_top, 1), line as usize);
        }
    }

    /// Scrolled: the offset is a row index, not a line count, so it is
    /// added after the division rather than before.
    #[test]
    fn offset_is_added_in_rows_not_lines() {
        assert_eq!(table_row_at(7, 9, 5, 2), 9, "7 + (4 / 2)");
        assert_eq!(table_row_at(7, 9, 5, 1), 11, "7 + 4");
    }

    /// A row_height of 0 would panic on divide. It cannot happen through
    /// `ViewMode::row_height`, but the helper is `pub(crate)` and the
    /// caller passes a `u16`, so it is guarded rather than assumed.
    #[test]
    fn zero_row_height_does_not_panic() {
        assert_eq!(table_row_at(3, 9, 5, 0), 7);
    }

    /// `screen_row` above `data_top` is filtered by the callers, but the
    /// helper must not underflow if that guard ever moves.
    #[test]
    fn screen_row_above_data_top_saturates() {
        assert_eq!(table_row_at(2, 1, 5, 2), 2);
    }
}
