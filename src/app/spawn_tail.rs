//! Log- and event-tail spawns and their key handlers. The scroll /
//! follow / filter surface both tails share is `super::tail::TailView`.

use super::*;

impl App {
    /// Key handler for the `:logs-tail` streaming overlay. j/k scroll, G
    /// snaps back to follow-mode (auto-tail), g jumps to top (and pauses
    /// follow), / opens a regex filter, n clears it, esc/q closes the
    /// overlay and tears down the polling task.
    pub(crate) fn handle_log_tail_key(&mut self, key: KeyEvent) {
        // Group-switcher: Tab opens a Picker over the env's discovered CW
        // log groups. Handled up-front before the borrow of
        // `current_overlay` below so the picker open can re-borrow `self`.
        // (In filter-entry mode Tab is input, not the switcher.)
        if matches!(key.code, KeyCode::Tab) {
            let in_filter = matches!(
                self.current_overlay.as_ref(),
                Some(Overlay::LogTail { view, .. }) if view.filter_active
            );
            if !in_filter {
                self.open_log_group_picker();
                return;
            }
        }
        let Some(Overlay::LogTail { view, events, .. }) = self.current_overlay.as_mut() else {
            return;
        };
        let outcome = tail::handle_tail_key(view, key);
        // Clamp the scroll to the buffer: `g` sets the u16::MAX
        // sentinel, and without a ceiling every subsequent `j` costs
        // one dead press (~63k of them) before the view moves again.
        view.scroll = view.scroll.min(events.len() as u16);
        if outcome == tail::TailKeyOutcome::Close {
            // Reap so a late `LogTailOpened` from the aborted task can't
            // re-open the overlay the user just dismissed.
            tail::reap_tail_task(&mut self.log_tail_task, &mut self.log_tail_session);
            self.current_overlay = None;
        }
    }

    /// Open a Picker over the env's discovered CW log groups so the operator
    /// can switch the tailed group from inside the streaming overlay.
    /// Pre-selects the currently-tailed group; no-op (with a status hint) if
    /// no groups have been discovered for this env.
    pub(crate) fn open_log_group_picker(&mut self) {
        let Some(Overlay::LogTail { log_group, .. }) = self.current_overlay.as_ref() else {
            return;
        };
        let current_group = log_group.clone();
        let groups: Vec<String> = self
            .detail
            .as_ref()
            .and_then(|d| d.cw_log_groups.clone())
            .unwrap_or_default();
        if groups.is_empty() {
            self.status_message = Some(
                "no CW log groups discovered for this env — try `:logs-tail <full-group-name>`"
                    .into(),
            );
            return;
        }
        self.picker = Some(Picker::new(
            PickerKind::LogGroup,
            groups,
            Some(current_group.as_str()),
        ));
        self.mode = Mode::Picker;
    }

    /// Open a streaming CW Logs view for `env_name`. If `explicit_group` is
    /// `None`, discovers the env's log groups and picks the most useful one
    /// via `pick_default_log_group`. Aborts any active log-tail task before
    /// starting the new one, then spawns a polling loop that sends
    /// `AppMsg::LogTailEvents` every ~2s. The overlay opens immediately in
    /// a "discovering" state and gets replaced with the LogTail variant
    /// once the group is known.
    pub(crate) fn spawn_logs_tail(&mut self, env_name: String, explicit_group: Option<String>) {
        // Tear down any prior session so we don't have two pollers racing.
        tail::reap_tail_task(&mut self.log_tail_task, &mut self.log_tail_session);
        let session_id = self.log_tail_session;
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        // In-flight ack: the LogTail overlay opens itself when data lands.
        let handle = tokio::spawn(async move {
            // Resolve the log group up front. If the user supplied one,
            // trust it (no DescribeLogGroups round-trip); otherwise discover.
            let group = match explicit_group {
                Some(g) => g,
                None => match aws.discover_env_log_groups(&env_for_msg).await {
                    Ok(groups) => match pick_default_log_group(&groups) {
                        Some(g) => g,
                        None => {
                            let _ = tx.send(AppMsg::LogTailEvents {
                                gen,
                                session_id,
                                next_since_ms: 0,
                                result: Err(format!(
                                    "no CW log groups under /aws/elasticbeanstalk/{env_for_msg}/ — enable streaming with `:logs-stream on`"
                                )),
                            });
                            return;
                        }
                    },
                    Err(e) => {
                        let _ = tx.send(AppMsg::LogTailEvents {
                            gen,
                            session_id,
                            next_since_ms: 0,
                            result: Err(format!("discover log groups: {e}")),
                        });
                        return;
                    }
                },
            };
            // First batch: fetch the last 5 minutes so the overlay isn't
            // empty on open.
            let mut since_ms = chrono::Utc::now().timestamp_millis() - 5 * 60 * 1000;
            // Send an "opening" message that tells the App handler what log
            // group resolved + replaces the overlay with a real LogTail.
            let _ = tx.send(AppMsg::LogTailOpened {
                gen,
                session_id,
                env_name: env_for_msg.clone(),
                log_group: group.clone(),
                since_ms,
            });
            let mut boundary_ids: std::collections::HashSet<String> = Default::default();
            loop {
                match aws
                    .fetch_recent_log_events(&group, since_ms, 1000, &boundary_ids)
                    .await
                {
                    Ok((events, next_since, carry)) => {
                        let next_since_ms = next_since;
                        let _ = tx.send(AppMsg::LogTailEvents {
                            gen,
                            session_id,
                            next_since_ms,
                            result: Ok(events),
                        });
                        since_ms = next_since;
                        boundary_ids = carry;
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::LogTailEvents {
                            gen,
                            session_id,
                            next_since_ms: since_ms,
                            result: Err(format!("{e}")),
                        });
                        // Keep going on errors — transient throttling
                        // shouldn't kill the session.
                    }
                }
                tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            }
        });
        self.log_tail_task = Some(handle);
    }

    /// `:event-tail` — open the cross-fleet event tail overlay and
    /// start its polling task. First batch is the fleet's most recent
    /// events regardless of age (so the overlay isn't empty on open);
    /// subsequent polls pass a `start_time` watermark so a busy fleet
    /// re-ships only what's new. DescribeEvents is more
    /// throttle-sensitive than FilterLogEvents, hence the 5s cadence
    /// (vs logs-tail's 2s). Errors keep the loop alive.
    pub(crate) fn spawn_event_tail(&mut self) {
        // Tear down any prior session so we don't have two pollers racing.
        tail::reap_tail_task(&mut self.event_tail_task, &mut self.event_tail_session);
        let session_id = self.event_tail_session;
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let handle = tokio::spawn(async move {
            // Install the (empty) overlay first so the operator sees
            // the tail open immediately; the first batch fills it.
            let _ = tx.send(AppMsg::EventTailOpened { gen, session_id });
            let mut since_ms = match aws.list_events(EVENT_TAIL_FIRST_BATCH).await {
                Ok(mut events) => {
                    // DescribeEvents returns newest-first; the ring
                    // buffer appends oldest-first.
                    events.reverse();
                    // Watermark = newest event + 1ms, NOT local now():
                    // clamping to now() would skip anything that lands
                    // between the server-side snapshot and our clock
                    // (eventual consistency / clock skew). Events in
                    // (newest, now] weren't in this batch, so there's
                    // no duplicate risk. Empty fleet history falls
                    // back to now.
                    let watermark = if events.is_empty() {
                        chrono::Utc::now().timestamp_millis()
                    } else {
                        next_event_watermark_ms(&events, 0)
                    };
                    let _ = tx.send(AppMsg::EventTailEvents {
                        gen,
                        session_id,
                        result: Ok(events),
                    });
                    watermark
                }
                Err(e) => {
                    let _ = tx.send(AppMsg::EventTailEvents {
                        gen,
                        session_id,
                        result: Err(format!("{e}")),
                    });
                    chrono::Utc::now().timestamp_millis()
                }
            };
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                match aws.list_events_since(since_ms, EVENT_TAIL_POLL_BATCH).await {
                    Ok(mut events) => {
                        since_ms = next_event_watermark_ms(&events, since_ms);
                        events.reverse();
                        let _ = tx.send(AppMsg::EventTailEvents {
                            gen,
                            session_id,
                            result: Ok(events),
                        });
                    }
                    Err(e) => {
                        let _ = tx.send(AppMsg::EventTailEvents {
                            gen,
                            session_id,
                            result: Err(format!("{e}")),
                        });
                        // Keep going on errors — transient throttling
                        // shouldn't kill the session.
                    }
                }
            }
        });
        self.event_tail_task = Some(handle);
    }

    /// Key handling while the `:event-tail` overlay is open — the
    /// same surface as [`handle_log_tail_key`] minus the log-group
    /// picker (there's no group to switch; the tail is fleet-wide).
    pub(crate) fn handle_event_tail_key(&mut self, key: KeyEvent) {
        let Some(Overlay::EventTail { view, events, .. }) = self.current_overlay.as_mut() else {
            return;
        };
        let outcome = tail::handle_tail_key(view, key);
        // Same scroll ceiling as the log tail — `g`'s u16::MAX
        // sentinel must not leave `j` dead for thousands of presses.
        view.scroll = view.scroll.min(events.len() as u16);
        if outcome == tail::TailKeyOutcome::Close {
            // Reap so a late `EventTailOpened` from the aborted task
            // can't re-open the dismissed overlay.
            tail::reap_tail_task(&mut self.event_tail_task, &mut self.event_tail_session);
            self.current_overlay = None;
        }
    }
}
