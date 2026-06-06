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

use super::{is_text_input, App, Mode};

impl App {
    /// `Mode::Filter` — typing builds up `self.filter` and re-runs
    /// `rebuild_view` so the table reflects the search live; Esc
    /// clears + exits; Enter commits and exits.
    pub(super) fn handle_filter_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                self.filter.clear();
                self.mode = Mode::Normal;
                self.rebuild_view();
            }
            KeyCode::Enter => self.mode = Mode::Normal,
            // TextInput consumes editing keys (cursor move / Ctrl-W);
            // rebuild the view on any accepted edit so the table tracks
            // the filter live.
            _ if self.filter.handle_key(key) => self.rebuild_view(),
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
                let cmd = self.command_input.clone();
                self.command_input.clear();
                self.completion.origin = None;
                self.mode = Mode::Normal;
                self.execute_command(&cmd);
            }
            KeyCode::Backspace => {
                // Reset the completion cycle — the operator
                // is editing, not cycling. Same intent as
                // typing a new character.
                self.completion.origin = None;
                self.command_input.pop();
            }
            KeyCode::Tab => self.command_completion_step(1),
            KeyCode::BackTab => self.command_completion_step(-1),
            KeyCode::Char(c) if is_text_input(&key) => {
                // Any printable key resets the completion
                // cycle so the operator's next Tab starts a
                // fresh search.
                self.completion.origin = None;
                self.command_input.push(c);
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
}
