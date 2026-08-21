//! Opening the read-only informational overlays — why-red, what's
//! new, about, report-a-bug, instance info, the DLQ queue viewer —
//! and the small toggles that sit alongside them.

use super::*;

impl App {
    /// `:why` / `:diagnose` — open the unified diagnostic overlay for the
    /// given env. Installs an empty `Overlay::WhyRed` immediately so the
    /// user sees "fetching…" placeholders, then fans out four parallel
    /// fetchers (events, alarms, instances, deploys). Each lands as its
    /// own `AppMsg::WhyRed*` variant gated on `session_id`.
    pub(crate) fn open_why_red(&mut self, env_name: String, app_name: String) {
        self.why_red_session = self.why_red_session.wrapping_add(1);
        let session_id = self.why_red_session;
        // Tier captured up front so the renderer can hide the queue
        // section for Web envs without consulting `self.environments`
        // (which may have refreshed under us by the time the overlay
        // renders).
        let tier = self
            .environments
            .iter()
            .find(|e| e.name == env_name)
            .map(|e| e.tier.clone())
            .unwrap_or_default();
        let is_worker = tier.eq_ignore_ascii_case("Worker");
        self.current_overlay = Some(Overlay::WhyRed {
            env_name: env_name.clone(),
            tier,
            events: None,
            alarms: None,
            instances: None,
            deploys: None,
            // Web envs never get a queues entry — keep it None so the
            // renderer omits the section entirely. Worker envs start at
            // None and fill in via WhyRedQueues.
            queues: None,
            dlq_messages: None,
            session_id,
            cursor: 0,
        });
        self.spawn_why_red_events(env_name.clone(), session_id);
        self.spawn_why_red_alarms(env_name.clone(), session_id);
        self.spawn_why_red_instances(env_name.clone(), session_id);
        self.spawn_why_red_deploys(app_name.clone(), session_id);
        if is_worker {
            self.spawn_why_red_queues(app_name, env_name, session_id);
        }
    }

    pub(crate) fn set_log_level(&mut self, level: &str) {
        // Treat a bare level as a directive applied to the root, but keep the
        // AWS/hyper crates capped at warn unless the user explicitly opts in.
        let directive = match level.to_lowercase().as_str() {
            "trace" | "debug" | "info" | "warn" | "error" => {
                format!("{level},aws=warn,hyper=warn")
            }
            other => other.to_string(),
        };
        let new_filter = match tracing_subscriber::EnvFilter::try_new(&directive) {
            Ok(f) => f,
            Err(e) => {
                self.error_message = Some(format!("invalid log directive '{level}': {e}"));
                return;
            }
        };
        let Some(handle) = self.log_reload.as_ref() else {
            self.error_message = Some("log reload handle missing".into());
            return;
        };
        match handle.modify(|f| *f = new_filter) {
            Ok(()) => {
                self.log_directive = directive.clone();
                self.status_message = Some(format!("log level → {directive}"));
            }
            Err(e) => self.error_message = Some(format!("log reload failed: {e}")),
        }
    }

    pub(crate) fn open_whatsnew(&mut self) {
        // Embedded changelog text. Keep this short — full release notes live in
        // git history / GitHub releases. Update on every release.
        self.current_overlay = Some(Overlay::Whatsnew(WHATSNEW.into()));
    }

    /// `:about` / `:credits` — author + license + repo info. Discoverable
    /// via the command palette but never pushed at the operator;
    /// existence justifies removing the splash byline if anyone ever
    /// objects to the 3-second introduction.
    /// `:report-bug` — build a scrubbed bug-report payload from
    /// current app state + ~/.cache/ebman/ebman.log tail + latest
    /// crash log (if any), and show it in the `Overlay::ReportBug`.
    /// Operator chooses `y` (copy to clipboard) or `b` (open
    /// GitHub issue in browser). See `report_bug` module for the
    /// scrubbing rules.
    pub(crate) fn open_report_bug_overlay(&mut self) {
        let cnames: std::collections::BTreeSet<String> = self
            .environments
            .iter()
            .filter(|e| !e.cname.is_empty())
            .map(|e| e.cname.clone())
            .collect();
        let env_names: std::collections::BTreeSet<String> =
            self.environments.iter().map(|e| e.name.clone()).collect();
        let app_names: std::collections::BTreeSet<String> =
            self.applications.iter().map(|a| a.name.clone()).collect();
        // message_log entries are (timestamp, kind, text) tuples;
        // pull the text + a single-char severity prefix so the
        // operator can see whether each line was a status or an
        // error without the structured tracing noise.
        let recent_messages: Vec<String> = self
            .message_log
            .iter()
            .rev()
            .take(10)
            .map(|(ts, kind, text)| {
                let sev = match kind {
                    MsgKind::Info => "[i]",
                    MsgKind::Error => "[!]",
                };
                let when = ts.format("%H:%M:%S");
                format!("{when}  {sev}  {text}")
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let icons = format!("{:?}", self.theme.icons).to_lowercase();
        let input = crate::report_bug::ReportInput {
            ebman_version: env!("CARGO_PKG_VERSION"),
            os: std::env::consts::OS,
            os_release: std::env::consts::ARCH,
            icons: &icons,
            theme: self.theme.name,
            refresh_interval_secs: self.refresh_interval.as_secs(),
            recent_log_lines: crate::report_bug::tail_ebman_log(30),
            recent_messages,
            recent_crash: crate::report_bug::latest_crash_log(),
            env_count: self.environments.len(),
            app_count: self.applications.len(),
            multi_regions_count: self.multi_regions.len(),
            multi_account_enabled: !self.cfg.accounts.is_empty(),
        };
        let ctx = crate::report_bug::ScrubContext {
            account_id: self.context.account_id.clone(),
            profile: self.context.profile.clone(),
            region: Some(self.context.region.clone()),
            env_names,
            app_names,
            cnames,
        };
        let body = crate::report_bug::build_report(&input, &ctx);
        self.current_overlay = Some(Overlay::ReportBug { body });
    }

    /// Key handler for the `:report-bug` overlay. `y` copies the
    /// scrubbed payload to clipboard; `b` opens a pre-filled
    /// GitHub issue in the browser; `esc` / `q` closes. Same shape
    /// as the other interactive overlays.
    pub(crate) fn handle_report_bug_key(&mut self, key: KeyEvent) {
        let body = match self.current_overlay.as_ref() {
            Some(Overlay::ReportBug { body }) => body.clone(),
            _ => return,
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.current_overlay = None;
            }
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                match yank(&body) {
                    Ok(()) => {
                        self.status_message = Some(format!(
                            "bug report copied to clipboard ({} chars) — paste at https://github.com/tombaldwin/ebman/issues/new",
                            body.chars().count()
                        ));
                    }
                    Err(e) => {
                        self.error_message = Some(format!("clipboard error: {e}"));
                    }
                }
                self.current_overlay = None;
            }
            KeyCode::Char('b') | KeyCode::Char('B') => {
                let url = crate::report_bug::github_issue_url(
                    "tombaldwin/ebman",
                    "Bug report from ebman",
                    &body,
                );
                match open_url(&url) {
                    Ok(()) => {
                        self.status_message = Some("opened GitHub issue draft in browser".into());
                    }
                    Err(e) => {
                        self.error_message = Some(format!("couldn't open browser: {e}"));
                    }
                }
                self.current_overlay = None;
            }
            _ => {}
        }
    }

    pub(crate) fn open_about_overlay(&mut self) {
        // The card content is built by `draw_about`; the overlay just
        // carries the open time so the giant scene can animate.
        self.current_overlay = Some(Overlay::About(Instant::now()));
    }

    pub(crate) fn cycle_metrics_range(&mut self, delta: i32) {
        const RANGES: &[i64] = &[900, 3600, 21_600, 86_400]; // 15m / 1h / 6h / 24h
        let Some(d) = self.detail.as_mut() else {
            return;
        };
        let cur = RANGES
            .iter()
            .position(|r| *r == d.metrics_range_secs)
            .unwrap_or(1) as i32;
        let next = (cur + delta).rem_euclid(RANGES.len() as i32) as usize;
        d.metrics_range_secs = RANGES[next];
        let env_name = d.env_name.clone();
        self.spawn_detail_metrics(env_name);
    }

    /// Open the worker-queue viewer for the env in Detail mode, defaulting
    /// to whichever queue the caller asked for. `open_dlq` is the legacy
    /// shortcut that always opens the DLQ.
    pub(crate) fn open_queue_viewer(&mut self, viewing: QueueView) {
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        if detail.tab() != DetailTab::Queue {
            return;
        }
        let main_url = detail.queues.main_url.clone().unwrap_or_default();
        let dlq_url = detail.queues.dlq_url.clone().unwrap_or_default();
        let target_url = match viewing {
            QueueView::Main => main_url.clone(),
            QueueView::Dlq => dlq_url.clone(),
        };
        if target_url.is_empty() {
            self.status_message = Some(match viewing {
                QueueView::Main => "no main queue URL known".into(),
                QueueView::Dlq => "no DLQ for this env".into(),
            });
            return;
        }
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
            viewing,
            confirm_delete_id: None,
            replay_input: None,
        };
        self.dlq = Some(dlq);
        self.mode = Mode::Dlq;
        self.spawn_dlq_fetch();
    }

    /// Open a confirm modal for an action that carries parameters (deploy
    /// version, clone target, scale min/max, …). Uses the same Y/N path as
    /// the existing Rebuild / Restart / Swap confirms so the operator sees
    /// the impact summary before authorising.
    /// Surface the selected instance's details as an `Overlay::TextDump`.
    /// Non-intrusive alternative to opening the EC2 console — operators
    /// can scan id / type / AZ / health / causes / launch age without
    /// leaving the TUI. `b` still opens the browser when needed.
    pub(crate) fn open_instance_info_overlay(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(inst) = d.instances.get(d.instances_cursor) else {
            self.status_message = Some("no instance selected".into());
            return;
        };
        let mut body = String::new();
        body.push_str(&format!("Instance ID       {}\n", inst.id));
        body.push_str(&format!("Type              {}\n", inst.instance_type));
        body.push_str(&format!("Availability zone {}\n", inst.availability_zone));
        body.push_str(&format!(
            "Health            {} ({})\n",
            inst.health, inst.color
        ));
        if let Some(t) = inst.launched_at {
            let age = chrono::Utc::now().signed_duration_since(t);
            body.push_str(&format!(
                "Launched          {}  (up {})\n",
                t.format("%Y-%m-%d %H:%M UTC"),
                humanize_short_age(age.to_std().unwrap_or_default())
            ));
        }
        if !inst.causes.is_empty() {
            body.push_str("\nCauses:\n");
            for c in &inst.causes {
                body.push_str(&format!("  • {c}\n"));
            }
        }
        body.push_str(
            "\nKeys: b → open in EC2 console · s → SSM shell · y → yank id · x → terminate",
        );
        self.current_overlay = Some(Overlay::TextDump {
            title: format!("instance — {}", inst.id),
            body,
        });
    }
}
