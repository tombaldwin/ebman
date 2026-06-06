//! DLQ viewer control flow — open / close + the `Mode::Dlq` key handler.
//! The App-coupled counterpart to `spawn_dlq.rs` (the background SQS
//! spawners); the `spawn_*` clusters refactor noted `handle_dlq_key`
//! would follow the spawners out of `app.rs`. Pure relocation, no
//! behaviour change. Methods are `pub(super)` so the dispatch sites in
//! `app.rs` can still reach them.

use super::*;

impl App {
    pub(super) fn open_dlq(&mut self) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        if detail.tab() != DetailTab::Queue {
            return;
        }
        let Some(dlq_url) = detail.queues.dlq_url.clone() else {
            self.status_message = Some("no DLQ for this env".into());
            return;
        };
        let main_url = detail.queues.main_url.clone().unwrap_or_default();
        let dlq = DlqState {
            env_name: detail.env_name.clone(),
            main_queue_url: main_url,
            dlq_url,
            messages: Vec::new(),
            list_state: ListState::default(),
            loading: false,
            error: None,
            confirm_purge: false,
            purge_typed: TextInput::new(),
            viewing: QueueView::Dlq,
            confirm_delete_idx: None,
            replay_input: None,
        };
        self.dlq = Some(dlq);
        self.mode = Mode::Dlq;
        self.spawn_dlq_fetch();
    }

    /// Open the DLQ viewer for an env outside the Detail flow — used
    /// when drilling in from the `:why` overlay, which already has the
    /// env's queue URLs from its `WhyRedQueues` fetch and shouldn't make
    /// the operator detour through Detail's Queue tab first.
    pub(super) fn open_dlq_from_why(
        &mut self,
        env_name: String,
        main_queue_url: String,
        dlq_url: String,
    ) {
        let dlq = DlqState {
            env_name,
            main_queue_url,
            dlq_url,
            messages: Vec::new(),
            list_state: ListState::default(),
            loading: false,
            error: None,
            confirm_purge: false,
            purge_typed: TextInput::new(),
            viewing: QueueView::Dlq,
            confirm_delete_idx: None,
            replay_input: None,
        };
        self.dlq = Some(dlq);
        self.mode = Mode::Dlq;
        self.spawn_dlq_fetch();
    }

    pub(super) fn close_dlq(&mut self) {
        self.dlq = None;
        self.mode = if self.detail.is_some() {
            Mode::Detail
        } else {
            Mode::Normal
        };
    }

    pub(super) fn handle_dlq_key(&mut self, key: KeyEvent) {
        let Some(dlq) = self.dlq.as_mut() else { return };
        // Single-message delete confirmation: Y/N inline. Anything else cancels.
        if let Some(idx) = dlq.confirm_delete_idx {
            match key.code {
                KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                    dlq.confirm_delete_idx = None;
                    self.spawn_dlq_delete_one(idx);
                }
                _ => {
                    dlq.confirm_delete_idx = None;
                }
            }
            return;
        }
        // Strict-confirm mode for purge: capture text input until match.
        if dlq.confirm_purge {
            match key.code {
                KeyCode::Esc => {
                    dlq.confirm_purge = false;
                    dlq.purge_typed.clear();
                }
                KeyCode::Enter if dlq.purge_typed.text() == dlq.env_name.as_str() => {
                    let dlq_url = dlq.dlq_url.clone();
                    let env_name = dlq.env_name.clone();
                    dlq.confirm_purge = false;
                    dlq.purge_typed.clear();
                    self.spawn_dlq_purge(env_name, dlq_url);
                }
                // TextInput consumes editing keys (Backspace / cursor /
                // Ctrl-W); a non-matching Enter falls through as a no-op.
                _ => {
                    dlq.purge_typed.handle_key(key);
                }
            }
            return;
        }
        // Time-windowed replay prompt: type a spec, Enter resolves + dispatches.
        if let Some(input) = dlq.replay_input.as_mut() {
            match key.code {
                KeyCode::Esc => dlq.replay_input = None,
                KeyCode::Enter => match crate::mode_dlq::parse_replay_spec(input.text()) {
                    None => {
                        dlq.error = Some(
                            "replay: type `all`, a count (e.g. 20), or a window (1h / 24h / 7d)"
                                .into(),
                        );
                    }
                    Some(spec) => {
                        let idxs = crate::mode_dlq::select_replay_indices(
                            &dlq.messages,
                            &spec,
                            chrono::Utc::now(),
                        );
                        let msgs: Vec<_> = idxs
                            .iter()
                            .filter_map(|&i| dlq.messages.get(i).cloned())
                            .collect();
                        dlq.replay_input = None;
                        if msgs.is_empty() {
                            self.error_message = Some("replay: no messages match".into());
                        } else {
                            self.spawn_dlq_replay_batch(msgs);
                        }
                    }
                },
                // TextInput consumes editing keys; the spec is parsed on
                // Enter, so no live side-effect on edit.
                _ => {
                    input.handle_key(key);
                }
            }
            return;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.close_dlq(),
            KeyCode::Enter => {
                let Some(idx) = dlq.list_state.selected() else {
                    return;
                };
                let Some(msg) = dlq.messages.get(idx).cloned() else {
                    return;
                };
                let when = msg
                    .sent_at
                    .map(|t| {
                        t.with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M:%S %Z")
                            .to_string()
                    })
                    .unwrap_or_else(|| "—".into());
                let view_label = match dlq.viewing {
                    QueueView::Main => "Main queue",
                    QueueView::Dlq => "DLQ",
                };
                let body = format!(
                    "{view_label} message\n\
                     ─────────────────────────────\n\
                     id:           {}\n\
                     receive-count:{}\n\
                     sent:         {when}\n\
                     bytes:        {}\n\n\
                     ─ body ─\n{}\n\nesc / q to close",
                    msg.id,
                    msg.receive_count,
                    msg.body.len(),
                    msg.body
                );
                self.current_overlay = Some(Overlay::Describe(body));
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let n = dlq.messages.len();
                if n == 0 {
                    return;
                }
                let cur = dlq.list_state.selected().unwrap_or(0);
                dlq.list_state.select(Some((cur + 1) % n));
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let n = dlq.messages.len();
                if n == 0 {
                    return;
                }
                let cur = dlq.list_state.selected().unwrap_or(0);
                dlq.list_state.select(Some((cur + n - 1) % n));
            }
            KeyCode::Char('r') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.spawn_dlq_fetch();
            }
            KeyCode::Char('r') => {
                if matches!(dlq.viewing, QueueView::Main) {
                    self.error_message = Some("resend is only available in DLQ view".into());
                } else {
                    self.spawn_dlq_resend_selected();
                }
            }
            KeyCode::Char('R') => {
                if matches!(dlq.viewing, QueueView::Main) {
                    self.error_message = Some("replay is only available in DLQ view".into());
                } else if dlq.messages.is_empty() {
                    self.error_message = Some("replay: DLQ is empty".into());
                } else {
                    dlq.replay_input = Some(TextInput::new());
                    dlq.error = None;
                }
            }
            KeyCode::Char('m') => {
                // Toggle which queue is loaded. Main-queue view disables
                // resend/purge (too dangerous on a live queue). Refetch on switch.
                if dlq.main_queue_url.is_empty() {
                    self.error_message = Some("no main queue URL known".into());
                } else {
                    dlq.viewing = match dlq.viewing {
                        QueueView::Dlq => QueueView::Main,
                        QueueView::Main => QueueView::Dlq,
                    };
                    dlq.messages.clear();
                    dlq.list_state.select(None);
                    self.spawn_dlq_fetch();
                }
            }
            KeyCode::Char('x') => {
                // Single-message delete. The dispatch loop catches y/n in the
                // next iteration via `confirm_delete_idx`.
                if let Some(idx) = dlq.list_state.selected() {
                    if dlq.messages.get(idx).is_some() {
                        dlq.confirm_delete_idx = Some(idx);
                    }
                }
            }
            KeyCode::Char('p') => {
                if let Some(dlq) = self.dlq.as_mut() {
                    dlq.confirm_purge = true;
                    dlq.purge_typed.clear();
                }
            }
            _ => {}
        }
    }
}
