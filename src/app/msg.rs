//! `AppMsg` dispatch.
//!
//! `handle_msg` is a thin router: it enforces the stale-result invariant
//! once, then delegates each variant to a dedicated `handle_*` method.
//! Splitting the bodies out keeps any single function from carrying the
//! whole async-result surface (it was previously one ~1,140-line `match`).

use super::*;

impl AppMsg {
    /// The context generation a result message was produced for, when it
    /// carries one. `handle_msg` drops the message if this no longer
    /// matches `App::generation` — the single enforcement point for the
    /// "results from a superseded context are discarded" invariant.
    /// `Rebuild` and `UpdateCheck` aren't tied to an AWS context, so they
    /// return `None` and are always delivered.
    fn generation(&self) -> Option<u64> {
        use AppMsg::*;
        match self {
            Rebuild(_) | UpdateCheck(_) => None,
            Refresh { gen, .. }
            | Identity { gen, .. }
            | Applications { gen, .. }
            | SolutionStacks { gen, .. }
            | AppLatestVersions { gen, .. }
            | WorkerQueueCheck { gen, .. }
            | Events { gen, .. }
            | DetailEvents { gen, .. }
            | ActionResult { gen, .. }
            | DetailInstances { gen, .. }
            | DetailMetrics { gen, .. }
            | DetailLogsProgress { gen, .. }
            | DetailLogs { gen, .. }
            | TextOverlay { gen, .. }
            | AppVersions { gen, .. }
            | DryRunResult { gen, .. }
            | Alarms { gen, .. }
            | WhyRedEvents { gen, .. }
            | WhyRedAlarms { gen, .. }
            | WhyRedInstances { gen, .. }
            | WhyRedDeploys { gen, .. }
            | WhyRedQueues { gen, .. }
            | WhyRedDlqMessages { gen, .. }
            | PreflightEvents { gen, .. }
            | RollbackTarget { gen, .. }
            | DetailTags { gen, .. }
            | DeployFromLocal { gen, .. }
            | LogTailOpened { gen, .. }
            | LogTailEvents { gen, .. }
            | DetailLogGroups { gen, .. }
            | DetailAlarms { gen, .. }
            | EnvVarsForEdit { gen, .. }
            | CostsFetched { gen, .. }
            | DetailRecentVersions { gen, .. }
            | FormPrefilled { gen, .. }
            | FormMultiSelectLoaded { gen, .. }
            | DetailEnvVars { gen, .. }
            | OptionSettingsUpdate { gen, .. }
            | AlarmOp { gen, .. }
            | DeleteAppVersion { gen, .. }
            | TagUpdate { gen, .. }
            | DetailQueues { gen, .. }
            | DlqMessages { gen, .. }
            | DlqActionResult { gen, .. } => Some(*gen),
        }
    }
}

impl App {
    pub(super) fn handle_msg(&mut self, msg: AppMsg) {
        // Single enforcement point for the stale-result invariant: any
        // context-bound message whose generation has been superseded by a
        // context switch is dropped here, so no individual handler needs
        // its own guard.
        if let Some(gen) = msg.generation() {
            if gen != self.generation {
                return;
            }
        }
        match msg {
            AppMsg::Refresh { result, .. } => self.apply_refresh(result),
            AppMsg::Rebuild(result) => self.apply_rebuild(result),
            AppMsg::Identity { result, .. } => self.handle_identity(result),
            AppMsg::Applications { result, .. } => self.handle_applications(result),
            AppMsg::SolutionStacks { result, .. } => self.handle_solution_stacks(result),
            AppMsg::AppLatestVersions { results, .. } => self.handle_app_latest_versions(results),
            AppMsg::WorkerQueueCheck { results, .. } => self.handle_worker_queue_check(results),
            AppMsg::Events { result, .. } => self.handle_events(result),
            AppMsg::DetailEvents {
                env_name, result, ..
            } => self.handle_detail_events(env_name, result),
            AppMsg::ActionResult {
                action,
                env_name,
                result,
                ..
            } => self.handle_action_result(action, env_name, result),
            AppMsg::DetailInstances {
                env_name, result, ..
            } => self.handle_detail_instances(env_name, result),
            AppMsg::DetailMetrics {
                env_name, result, ..
            } => self.handle_detail_metrics(env_name, result),
            AppMsg::DetailLogsProgress {
                env_name,
                stage,
                attempt,
                ..
            } => self.handle_detail_logs_progress(env_name, stage, attempt),
            AppMsg::DetailLogs {
                env_name, result, ..
            } => self.handle_detail_logs(env_name, result),
            AppMsg::TextOverlay { title, body, .. } => self.handle_text_overlay(title, body),
            AppMsg::AppVersions {
                application,
                deployed_label,
                result,
                ..
            } => self.handle_app_versions(application, deployed_label, result),
            AppMsg::UpdateCheck(latest) => self.handle_update_check(latest),
            AppMsg::DryRunResult {
                env_name, result, ..
            } => self.handle_dry_run_result(env_name, result),
            AppMsg::Alarms {
                env_name, result, ..
            } => self.handle_alarms(env_name, result),
            AppMsg::WhyRedEvents {
                session_id, result, ..
            } => self.handle_why_red_events(session_id, result),
            AppMsg::WhyRedAlarms {
                session_id, result, ..
            } => self.handle_why_red_alarms(session_id, result),
            AppMsg::WhyRedInstances {
                session_id, result, ..
            } => self.handle_why_red_instances(session_id, result),
            AppMsg::WhyRedDeploys {
                session_id, result, ..
            } => self.handle_why_red_deploys(session_id, result),
            AppMsg::WhyRedQueues {
                session_id, result, ..
            } => self.handle_why_red_queues(session_id, result),
            AppMsg::WhyRedDlqMessages {
                session_id, result, ..
            } => self.handle_why_red_dlq_messages(session_id, result),
            AppMsg::PreflightEvents {
                env_name, result, ..
            } => self.handle_preflight_events(env_name, result),
            AppMsg::RollbackTarget {
                env_name,
                current_version,
                result,
                ..
            } => self.handle_rollback_target(env_name, current_version, result),
            AppMsg::DetailTags {
                env_name, result, ..
            } => self.handle_detail_tags(env_name, result),
            AppMsg::DeployFromLocal {
                env_name,
                label,
                summary,
                result,
                ..
            } => self.handle_deploy_from_local(env_name, label, summary, result),
            AppMsg::LogTailOpened {
                session_id,
                env_name,
                log_group,
                since_ms,
                ..
            } => self.handle_log_tail_opened(session_id, env_name, log_group, since_ms),
            AppMsg::LogTailEvents {
                session_id,
                next_since_ms,
                result,
                ..
            } => self.handle_log_tail_events(session_id, next_since_ms, result),
            AppMsg::DetailLogGroups {
                env_name, groups, ..
            } => self.handle_detail_log_groups(env_name, groups),
            AppMsg::DetailAlarms {
                env_name, result, ..
            } => self.handle_detail_alarms(env_name, result),
            AppMsg::EnvVarsForEdit {
                env_name, result, ..
            } => self.handle_env_vars_for_edit(env_name, result),
            AppMsg::CostsFetched {
                account,
                region,
                result,
                ..
            } => self.handle_costs_fetched(account, region, result),
            AppMsg::DetailRecentVersions {
                env_name, result, ..
            } => self.handle_detail_recent_versions(env_name, result),
            AppMsg::FormPrefilled {
                env_name, settings, ..
            } => self.handle_form_prefilled(env_name, settings),
            AppMsg::FormMultiSelectLoaded {
                env_name,
                field_key,
                result,
                ..
            } => self.handle_form_multi_select_loaded(env_name, field_key, result),
            AppMsg::DetailEnvVars {
                env_name, result, ..
            } => self.handle_detail_env_vars(env_name, result),
            AppMsg::OptionSettingsUpdate {
                env_name,
                summary,
                result,
                ..
            } => self.handle_option_settings_update(env_name, summary, result),
            AppMsg::AlarmOp {
                verb,
                alarm_name,
                env_name,
                result,
                ..
            } => self.handle_alarm_op(verb, alarm_name, env_name, result),
            AppMsg::DeleteAppVersion {
                application,
                label,
                force,
                result,
                ..
            } => self.handle_delete_app_version(application, label, force, result),
            AppMsg::TagUpdate {
                env_name,
                summary,
                result,
                ..
            } => self.handle_tag_update(env_name, summary, result),
            AppMsg::DetailQueues {
                env_name, result, ..
            } => self.handle_detail_queues(env_name, result),
            AppMsg::DlqMessages {
                env_name, result, ..
            } => self.handle_dlq_messages(env_name, result),
            AppMsg::DlqActionResult {
                env_name, result, ..
            } => self.handle_dlq_action_result(env_name, result),
        }
    }

    fn handle_identity(&mut self, result: Result<Identity, String>) {
        match result {
            Ok(id) => {
                self.context.account_id = id.account_id;
                self.context.caller_arn = id.caller_arn;
            }
            Err(msg) => {
                tracing::warn!(error = %msg, "identity refresh failed");
            }
        }
    }

    fn handle_applications(&mut self, result: Result<Vec<Application>, String>) {
        match result {
            Ok(mut apps) => {
                apps.sort_by_key(|a| a.name.to_lowercase());
                // Preserve the previously-fetched LATEST values across
                // refreshes so the column doesn't flicker to "—" every
                // tick while the follow-up fan-out is in flight.
                merge_app_latest_versions(&self.applications, &mut apps);
                self.applications = apps;
                // Pinned-first sort runs every refresh so newly-arrived
                // apps don't shuffle the pinned ones off the top row.
                self.resort_applications();
                if self.applications.is_empty() {
                    self.app_table_state.select(None);
                } else if self
                    .app_table_state
                    .selected()
                    .map(|s| s >= self.applications.len())
                    .unwrap_or(true)
                {
                    self.app_table_state.select(Some(0));
                }
                // Fan out latest-version fetches only when the operator is
                // actually looking at the apps view. Otherwise we'd burn N
                // DescribeApplicationVersions calls on every refresh tick
                // for users who live in the envs view all day. Switching
                // scope to Apps (Tab / BackTab) kicks off the fetch on
                // demand; the periodic refresh then keeps it fresh.
                if self.scope == Scope::Apps {
                    self.spawn_app_latest_versions();
                }
            }
            Err(msg) => tracing::warn!(error = %msg, "applications fetch failed"),
        }
    }

    fn handle_solution_stacks(&mut self, result: Result<Vec<String>, String>) {
        match result {
            Ok(stacks) => {
                self.latest_stacks = crate::aws::latest_stack_versions(&stacks);
                // Refresh the derived stale-platform cache now that the
                // catalogue is available — the env list itself is unchanged.
                self.rebuild_view();
            }
            Err(msg) => tracing::warn!(error = %msg, "solution stacks fetch failed"),
        }
    }

    fn handle_app_latest_versions(
        &mut self,
        results: Vec<(
            String,
            Option<String>,
            Option<chrono::DateTime<chrono::Utc>>,
        )>,
    ) {
        let by_name: std::collections::HashMap<_, _> = results
            .into_iter()
            .map(|(name, label, created)| (name, (label, created)))
            .collect();
        for app in self.applications.iter_mut() {
            if let Some((label, created)) = by_name.get(&app.name) {
                app.latest_version_label = label.clone();
                app.latest_version_created = *created;
            }
        }
    }

    fn handle_worker_queue_check(&mut self, results: Vec<(String, i64)>) {
        // Rebuild the cache from scratch so workers whose DLQ drained back
        // to zero are reflected. Missing entries = "fetch failed this
        // tick"; we drop them so a transient SQS error doesn't blank the
        // chip for everyone.
        self.worker_dlq_depths.clear();
        for (env_name, depth) in results {
            self.worker_dlq_depths.insert(env_name, depth);
        }
        // Recompute alerts now that the cache is fresh — the count set
        // during apply_refresh used the *previous* tick's cache. Workers
        // newly above DLQ=0 join the alert pill on the next draw.
        self.alerts = compute_red_alerts(&self.environments, &self.worker_dlq_depths);
    }

    fn handle_events(&mut self, result: Result<Vec<EbEvent>, String>) {
        match result {
            // The API returns events in time-descending order already.
            Ok(events) => self.event_panel.events = events,
            Err(msg) => tracing::warn!(error = %msg, "event fetch failed"),
        }
    }

    fn handle_detail_events(&mut self, env_name: String, result: Result<Vec<EbEvent>, String>) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            d.loading_events = false;
            match r {
                Ok(events) => {
                    d.events = events;
                    d.error = None;
                }
                Err(msg) => d.error = Some(msg),
            }
        });
    }

    fn handle_action_result(
        &mut self,
        action: Action,
        env_name: String,
        result: Result<(), String>,
    ) {
        write_audit_outcome(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            action,
            &env_name,
            result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
        );
        // Stamp the matching pending-actions entry with the outcome so the
        // panel shows ✓ / ✗ instead of "in flight".
        self.complete_pending(
            action.label(),
            &env_name,
            result.as_ref().map(|_| ()).map_err(|e| e.clone()),
        );
        match result {
            Ok(()) => {
                self.close_action_flow();
                self.status_message = Some(format!("{} on {env_name} dispatched", action.label()));
                self.spawn_refresh();
            }
            Err(msg) => {
                // Keep the confirm modal open via a Running→error
                // transition; simpler: close flow, surface the error.
                self.close_action_flow();
                self.error_message =
                    Some(format!("{} on {env_name} failed: {msg}", action.label()));
            }
        }
    }

    fn handle_detail_instances(&mut self, env_name: String, result: Result<Vec<Instance>, String>) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            d.loading_instances = false;
            match r {
                Ok(instances) => {
                    d.instances = instances;
                    d.error = None;
                }
                Err(msg) => d.error = Some(msg),
            }
        });
    }

    fn handle_detail_metrics(
        &mut self,
        env_name: String,
        result: Result<Vec<MetricSeries>, String>,
    ) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            d.loading_metrics = false;
            match r {
                Ok(metrics) => {
                    d.metrics = metrics;
                    d.error = None;
                }
                Err(msg) => d.error = Some(msg),
            }
        });
    }

    fn handle_detail_logs_progress(&mut self, env_name: String, stage: LogTailStage, attempt: u32) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.env_name != env_name {
            return;
        }
        detail.log_tail.stage = stage;
        if matches!(stage, LogTailStage::Polling) {
            detail.log_tail.poll_attempt = attempt;
        }
    }

    fn handle_detail_logs(
        &mut self,
        env_name: String,
        result: Result<Vec<(String, String)>, String>,
    ) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.env_name != env_name {
            return;
        }
        match result {
            Ok(by_instance) => {
                detail.log_tail.by_instance = by_instance;
                detail.log_tail.stage = LogTailStage::Ready;
                detail.log_tail.error = None;
            }
            Err(msg) => {
                detail.log_tail.stage = LogTailStage::Ready;
                detail.log_tail.error = Some(msg);
            }
        }
    }

    fn handle_text_overlay(&mut self, title: String, body: String) {
        self.current_overlay = Some(Overlay::TextDump { title, body });
    }

    fn handle_app_versions(
        &mut self,
        application: String,
        deployed_label: Option<String>,
        result: Result<Vec<AppVersion>, String>,
    ) {
        match result {
            Ok(versions) if versions.is_empty() => {
                self.status_message = Some(format!("no application versions for {application}"));
            }
            Ok(versions) => {
                self.current_overlay = Some(Overlay::TextDump {
                    title: format!("application versions — {application}"),
                    body: format_app_versions(&versions, deployed_label.as_deref(), 20),
                });
            }
            Err(msg) => self.error_message = Some(msg),
        }
    }

    fn handle_update_check(&mut self, latest: Option<crate::update_check::LatestRelease>) {
        if let Some(release) = latest {
            tracing::info!(target: "ebman::update", current = env!("CARGO_PKG_VERSION"), latest = %release.version, "newer ebman released on crates.io");
            self.update_available = Some(release);
        }
    }

    fn handle_dry_run_result(&mut self, env_name: String, result: Result<Vec<Instance>, String>) {
        let Some(ActionFlow::Confirm(modal)) = self.action_flow.as_mut() else {
            return;
        };
        if modal.target_env != env_name {
            return;
        }
        modal.loading_dryrun = false;
        if let Ok(instances) = result {
            let azs: std::collections::HashSet<&str> = instances
                .iter()
                .map(|i| i.availability_zone.as_str())
                .filter(|az| !az.is_empty())
                .collect();
            modal.dryrun = Some(DryRunInfo {
                instance_count: instances.len(),
                az_count: azs.len(),
            });
        }
    }

    fn handle_alarms(&mut self, env_name: String, result: Result<Vec<CwAlarm>, String>) {
        // Drop stale results: the user may have closed the overlay or
        // requested alarms for a different env during the round-trip. The
        // overlay carries the env it was opened for; only replace its body
        // if that still matches the result we just received.
        match self.current_overlay.as_mut() {
            Some(Overlay::Alarms {
                env_name: requested,
                body,
            }) if requested == &env_name => {
                *body = format_alarms(result);
            }
            _ => (),
        }
    }

    fn handle_why_red_events(&mut self, session_id: u64, result: Result<Vec<EbEvent>, String>) {
        if let Some(Overlay::WhyRed {
            session_id: s,
            events,
            ..
        }) = self.current_overlay.as_mut()
        {
            if *s == session_id {
                *events = Some(result);
            }
        }
    }

    fn handle_why_red_alarms(&mut self, session_id: u64, result: Result<Vec<CwAlarm>, String>) {
        if let Some(Overlay::WhyRed {
            session_id: s,
            alarms,
            ..
        }) = self.current_overlay.as_mut()
        {
            if *s == session_id {
                *alarms = Some(result);
            }
        }
    }

    fn handle_why_red_instances(&mut self, session_id: u64, result: Result<Vec<Instance>, String>) {
        if let Some(Overlay::WhyRed {
            session_id: s,
            instances,
            ..
        }) = self.current_overlay.as_mut()
        {
            if *s == session_id {
                *instances = Some(result);
            }
        }
    }

    fn handle_why_red_deploys(&mut self, session_id: u64, result: Result<Vec<AppVersion>, String>) {
        if let Some(Overlay::WhyRed {
            session_id: s,
            deploys,
            ..
        }) = self.current_overlay.as_mut()
        {
            if *s == session_id {
                *deploys = Some(result);
            }
        }
    }

    fn handle_why_red_queues(&mut self, session_id: u64, result: Result<WorkerQueues, String>) {
        // Land the queues result, then kick the DLQ peek if the DLQ has
        // visible messages. The peek is a second-stage fetch — it only
        // fires when the first stage shows something worth peeking at,
        // avoiding pointless SQS calls on healthy workers.
        let mut dlq_url_to_peek: Option<String> = None;
        if let Some(Overlay::WhyRed {
            session_id: s,
            queues,
            dlq_messages,
            ..
        }) = self.current_overlay.as_mut()
        {
            if *s == session_id {
                if let Ok(ref qs) = result {
                    let dlq_visible = qs.dlq_stats.as_ref().map(|s| s.visible).unwrap_or(0);
                    if dlq_visible > 0 {
                        if let Some(url) = qs.dlq_url.clone() {
                            dlq_url_to_peek = Some(url);
                        }
                    } else {
                        // Mark dlq_messages as resolved-empty so the
                        // renderer doesn't show "loading…" forever for a
                        // clean DLQ.
                        *dlq_messages = Some(Ok(Vec::new()));
                    }
                }
                *queues = Some(result);
            }
        }
        if let Some(url) = dlq_url_to_peek {
            self.spawn_why_red_dlq_peek(url, session_id);
        }
    }

    fn handle_why_red_dlq_messages(
        &mut self,
        session_id: u64,
        result: Result<Vec<QueueMessage>, String>,
    ) {
        if let Some(Overlay::WhyRed {
            session_id: s,
            dlq_messages,
            ..
        }) = self.current_overlay.as_mut()
        {
            if *s == session_id {
                *dlq_messages = Some(result);
            }
        }
    }

    fn handle_preflight_events(&mut self, env_name: String, result: Result<Vec<EbEvent>, String>) {
        let Some(ActionFlow::Confirm(modal)) = self.action_flow.as_mut() else {
            return;
        };
        if modal.target_env != env_name {
            return;
        }
        modal.loading_events = false;
        if let Ok(events) = result {
            modal.recent_events = Some(events);
        }
    }

    fn handle_rollback_target(
        &mut self,
        env_name: String,
        current_version: String,
        result: Result<Vec<EbEvent>, String>,
    ) {
        match result {
            Err(e) => self.error_message = Some(format!("rollback: {e}")),
            Ok(events) => match previous_version_label(&events, &current_version) {
                None => {
                    self.error_message = Some(format!(
                        "rollback: no prior version in {env_name}'s recent events — use :versions then :deploy"
                    ));
                }
                // The deploy-confirm modal targets the *selected* env; if
                // the cursor moved off the env `:rollback` was issued for,
                // bail rather than roll back the wrong one.
                Some(_)
                    if self.selected_env().map(|e| e.name.as_str()) != Some(env_name.as_str()) =>
                {
                    self.error_message =
                        Some("rollback cancelled — selection moved off the target env".into());
                }
                Some(prev) => {
                    self.status_message = Some(format!(
                        "rollback {env_name}: redeploying previous version {prev}"
                    ));
                    self.open_parameterised_action(
                        Action::Deploy,
                        ParameterisedAction {
                            deploy_version: Some(prev),
                            ..Default::default()
                        },
                    );
                }
            },
        }
    }

    fn handle_detail_tags(
        &mut self,
        env_name: String,
        result: Result<Vec<(String, String)>, String>,
    ) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            d.loading_tags = false;
            match r {
                Ok(tags) => {
                    d.tags = tags;
                    // A delete may have shrunk the list / removed the row
                    // mid-edit.
                    d.clamp_config_cursor();
                    d.revalidate_config_edit();
                }
                Err(msg) => tracing::warn!(error = %msg, "tags fetch failed"),
            }
        });
    }

    fn handle_deploy_from_local(
        &mut self,
        env_name: String,
        label: String,
        summary: String,
        result: Result<(), String>,
    ) {
        self.complete_pending(
            &summary,
            &env_name,
            result.as_ref().map(|_| ()).map_err(|e| e.clone()),
        );
        match result {
            Ok(()) => {
                self.push_toast(
                    ToastKind::Info,
                    format!("{summary} → {env_name} (version {label})"),
                );
            }
            Err(msg) => {
                self.push_toast(
                    ToastKind::Error,
                    format!("{summary} on {env_name} failed: {msg}"),
                );
            }
        }
    }

    fn handle_log_tail_opened(
        &mut self,
        session_id: u64,
        env_name: String,
        log_group: String,
        since_ms: i64,
    ) {
        if session_id != self.log_tail_session {
            return;
        }
        self.current_overlay = Some(Overlay::LogTail {
            log_group,
            env_name,
            events: std::collections::VecDeque::with_capacity(LOG_TAIL_MAX_LINES),
            scroll: 0,
            following: true,
            since_ms,
            filter_input: String::new(),
            filter_active: false,
            filter_pattern: None,
            last_err: None,
            session_id,
        });
        self.status_message = None;
    }

    fn handle_log_tail_events(
        &mut self,
        session_id: u64,
        next_since_ms: i64,
        result: Result<Vec<crate::aws::LogEvent>, String>,
    ) {
        if session_id != self.log_tail_session {
            return;
        }
        // Route to whichever overlay slot currently holds the LogTail —
        // `current_overlay` normally, or `pre_help_overlay` if the user
        // pressed `?` mid-tail. Without the second slot, events arriving
        // during the help round-trip would be lost.
        let target = if matches!(
            self.current_overlay.as_ref(),
            Some(Overlay::LogTail { session_id: s, .. }) if *s == session_id
        ) {
            self.current_overlay.as_mut()
        } else if matches!(
            self.help.pre_overlay.as_ref(),
            Some(Overlay::LogTail { session_id: s, .. }) if *s == session_id
        ) {
            self.help.pre_overlay.as_mut()
        } else {
            return;
        };
        let Some(Overlay::LogTail {
            events,
            since_ms,
            last_err,
            ..
        }) = target
        else {
            return;
        };
        *since_ms = next_since_ms;
        match result {
            Ok(new_events) => {
                *last_err = None;
                for ev in new_events {
                    if events.len() >= LOG_TAIL_MAX_LINES {
                        events.pop_front();
                    }
                    events.push_back(ev);
                }
            }
            Err(msg) => {
                *last_err = Some(msg);
            }
        }
    }

    fn handle_detail_log_groups(&mut self, env_name: String, groups: Vec<String>) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.env_name != env_name {
            return;
        }
        detail.cw_log_groups = Some(groups);
    }

    fn handle_detail_alarms(
        &mut self,
        env_name: String,
        result: Result<Vec<crate::aws::CwAlarm>, String>,
    ) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.env_name != env_name {
            return;
        }
        detail.loading_cw_alarms = false;
        detail.cw_alarms = Some(result);
    }

    fn handle_env_vars_for_edit(
        &mut self,
        env_name: String,
        result: Result<Vec<(String, String)>, String>,
    ) {
        match result {
            Ok(vars) => {
                // Stash for the main loop to consume. The editor shell-out
                // has to happen there because that's where the `Tui`
                // handle is.
                self.pending_env_edit = Some((env_name, vars));
            }
            Err(msg) => {
                self.error_message = Some(format!("env-edit fetch: {msg}"));
            }
        }
    }

    fn handle_costs_fetched(
        &mut self,
        account: Option<String>,
        region: String,
        result: Result<Vec<crate::aws::EnvCost>, String>,
    ) {
        match result {
            Ok(rows) => {
                let now = chrono::Utc::now();
                self.costs.clear();
                for row in &rows {
                    self.costs.insert(row.env_name.clone(), row.cost_usd);
                }
                self.costs_fetched_at = Some(now);
                // Persist to ~/.cache/ebman/cost-{account}-{region}.toml so
                // subsequent sessions render immediately.
                let account_key = account.unwrap_or_else(|| "unknown".into());
                let cache = crate::cost_cache::CostCache {
                    fetched_at: Some(now),
                    costs: self.costs.clone(),
                };
                if let Err(e) = crate::cost_cache::save(&account_key, &region, &cache) {
                    tracing::warn!(
                        target: "ebman::cost",
                        error = %e,
                        "cost cache write failed (non-fatal)"
                    );
                }
                let n = rows.len();
                self.status_message = Some(format!("cost: refreshed {n} env(s)"));
            }
            Err(msg) => {
                self.error_message = Some(format!("cost fetch: {msg}"));
            }
        }
    }

    fn handle_detail_recent_versions(
        &mut self,
        env_name: String,
        result: Result<Vec<crate::aws::AppVersion>, String>,
    ) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.env_name != env_name {
            return;
        }
        detail.loading_recent_versions = false;
        detail.recent_versions = Some(result);
    }

    fn handle_form_prefilled(
        &mut self,
        env_name: String,
        settings: Result<Vec<(String, String, String)>, String>,
    ) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        if form.env_name != env_name {
            return;
        }
        match settings {
            Err(msg) => {
                // Surface the fetch failure on the form's first field as a
                // global error; operator can dismiss or fill values
                // manually.
                if let Some(first) = form.fields.first_mut() {
                    first.error = Some(format!("pre-fill failed: {msg}"));
                }
                form.state = crate::form::FormState::Ready;
            }
            Ok(rows) => {
                // Build a (ns, name) -> value lookup; populate the form's
                // fields using the mappings stored on submit.
                use std::collections::HashMap;
                let lookup: HashMap<(String, String), String> = rows
                    .into_iter()
                    .map(|(ns, name, value)| ((ns, name), value))
                    .collect();
                let mappings = match &form.submit {
                    crate::form::FormSubmit::OptionSettings { mappings } => mappings.clone(),
                    // LocalConfig forms skip the AWS pre-fill in `open_form`
                    // so the FormPrefilled msg never fires for them — drop
                    // the result if one arrives anyway (stale message after
                    // the user switched form types).
                    crate::form::FormSubmit::LocalConfig => return,
                };
                for (key, ns, opt) in mappings {
                    if let Some(value) = lookup.get(&(ns, opt)) {
                        if let Some(field) = form.fields.iter_mut().find(|f| f.key == key) {
                            field.value = value.clone();
                        }
                    }
                }
                form.state = crate::form::FormState::Ready;
            }
        }
    }

    fn handle_form_multi_select_loaded(
        &mut self,
        env_name: String,
        field_key: String,
        result: Result<MultiSelectOptions, String>,
    ) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        if form.env_name != env_name {
            return;
        }
        let Some(field) = form.fields.iter_mut().find(|f| f.key == field_key) else {
            return;
        };
        match result {
            Err(msg) => {
                field.error = Some(format!("load failed: {msg}"));
                form.state = crate::form::FormState::Ready;
            }
            Ok(opts) => {
                let initial_filtered: Vec<String> = opts
                    .initial
                    .iter()
                    .filter(|v| opts.options.iter().any(|o| o == *v))
                    .cloned()
                    .collect();
                field.value = initial_filtered.join(",");
                field.kind = crate::form::FieldKind::MultiSelect {
                    options: opts.options.clone(),
                };
                if opts.annotations.len() == opts.options.len() && !opts.annotations.is_empty() {
                    field.option_annotations = Some(opts.annotations);
                }
                field.option_cursor = 0;
                form.state = crate::form::FormState::Ready;
            }
        }
    }

    fn handle_detail_env_vars(
        &mut self,
        env_name: String,
        result: Result<Vec<(String, String)>, String>,
    ) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            d.loading_env_vars = false;
            match r {
                Ok(vars) => {
                    d.env_vars = vars;
                    // A delete may have shrunk the list / removed the row
                    // mid-edit.
                    d.clamp_config_cursor();
                    d.revalidate_config_edit();
                }
                Err(msg) => tracing::warn!(error = %msg, "env vars fetch failed"),
            }
        });
    }

    fn handle_option_settings_update(
        &mut self,
        env_name: String,
        summary: String,
        result: Result<(), String>,
    ) {
        self.complete_pending(
            &summary,
            &env_name,
            result.as_ref().map(|_| ()).map_err(|e| e.clone()),
        );
        match result {
            Ok(()) => {
                self.push_toast(ToastKind::Success, format!("{summary} → {env_name}"));
                // If it was an env-var set/unset/rename and the Detail view
                // is open on the same env, refresh the Config tab's env
                // vars so the change reflects without waiting for the next
                // 15s tick.
                if summary.starts_with("env set ")
                    || summary.starts_with("env unset ")
                    || summary.starts_with("env rename ")
                {
                    if let Some(d) = self.detail.as_ref() {
                        if d.env_name == env_name {
                            self.spawn_detail_env_vars();
                        }
                    }
                }
            }
            Err(msg) => {
                self.push_toast(
                    ToastKind::Error,
                    format!("{summary} on {env_name} failed: {msg}"),
                );
            }
        }
    }

    fn handle_alarm_op(
        &mut self,
        verb: &'static str,
        alarm_name: String,
        env_name: String,
        result: Result<(), String>,
    ) {
        let label = match verb {
            "create" => "Create alarm",
            "delete" => "Delete alarm",
            _ => "Alarm op",
        };
        let target = format!("{env_name}/{alarm_name}");
        self.complete_pending(
            label,
            &target,
            result.as_ref().map(|_| ()).map_err(|e| e.clone()),
        );
        match result {
            Ok(()) => {
                let past = if verb == "create" {
                    "created"
                } else {
                    "deleted"
                };
                self.push_toast(ToastKind::Success, format!("alarm '{alarm_name}' {past}"));
            }
            Err(msg) => {
                self.push_toast(
                    ToastKind::Error,
                    format!("alarm '{alarm_name}' {verb} failed: {msg}"),
                );
            }
        }
    }

    fn handle_delete_app_version(
        &mut self,
        application: String,
        label: String,
        force: bool,
        result: Result<(), String>,
    ) {
        let force_str = if force { " (+source bundle)" } else { "" };
        let pending_label = if force {
            "Delete app version (+source)"
        } else {
            "Delete app version"
        };
        let pending_target = format!("{application}/{label}");
        self.complete_pending(
            pending_label,
            &pending_target,
            result.as_ref().map(|_| ()).map_err(|e| e.clone()),
        );
        match result {
            Ok(()) => {
                self.push_toast(
                    ToastKind::Info,
                    format!("deleted {application}/{label}{force_str}"),
                );
                // If the user has the matching `:versions` overlay open,
                // re-fetch so the deleted entry disappears instead of
                // lingering as stale text.
                let want_title = format!("application versions — {application}");
                if matches!(
                    self.current_overlay.as_ref(),
                    Some(Overlay::TextDump { title, .. }) if title == &want_title
                ) {
                    let aws = self.aws.clone();
                    let tx = self.msg_tx.clone();
                    let gen = self.generation;
                    let app_name = application.clone();
                    // Look up the env's currently-deployed version to
                    // re-mark it after the refresh. Picks the first env in
                    // this application — single-env case is the norm;
                    // multi-env case is rare and the marker is best-effort
                    // anyway.
                    let deployed_label = self
                        .environments
                        .iter()
                        .find(|e| e.application == application)
                        .filter(|e| !e.version_label.is_empty())
                        .map(|e| e.version_label.clone());
                    tokio::spawn(async move {
                        let result = aws
                            .list_application_versions(&app_name)
                            .await
                            .map_err(|e| flatten_err("list_application_versions", e));
                        let _ = tx.send(AppMsg::AppVersions {
                            gen,
                            application: app_name,
                            deployed_label,
                            result,
                        });
                    });
                }
            }
            Err(msg) => {
                self.push_toast(
                    ToastKind::Error,
                    format!("delete {application}/{label}{force_str} failed: {msg}"),
                );
            }
        }
    }

    fn handle_tag_update(&mut self, env_name: String, summary: String, result: Result<(), String>) {
        self.complete_pending(
            &summary,
            &env_name,
            result.as_ref().map(|_| ()).map_err(|e| e.clone()),
        );
        match result {
            Ok(()) => {
                self.push_toast(ToastKind::Info, format!("{summary} on {env_name}"));
                if let Some(d) = self.detail.as_ref() {
                    if d.env_name == env_name {
                        self.spawn_detail_tags();
                    }
                }
            }
            Err(msg) => {
                self.push_toast(
                    ToastKind::Error,
                    format!("{summary} on {env_name} failed: {msg}"),
                );
            }
        }
    }

    fn handle_detail_queues(&mut self, env_name: String, result: Result<WorkerQueues, String>) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            d.loading_queues = false;
            match r {
                Ok(queues) => {
                    d.queues = queues;
                    d.error = None;
                }
                Err(msg) => d.error = Some(msg),
            }
        });
    }

    fn handle_dlq_messages(&mut self, env_name: String, result: Result<Vec<QueueMessage>, String>) {
        let Some(dlq) = self.dlq.as_mut() else { return };
        if dlq.env_name != env_name {
            return;
        }
        dlq.loading = false;
        match result {
            Ok(messages) => {
                dlq.messages = messages;
                let cur = dlq.list_state.selected().unwrap_or(0);
                if dlq.messages.is_empty() {
                    dlq.list_state.select(None);
                } else if cur >= dlq.messages.len() {
                    dlq.list_state.select(Some(0));
                }
                dlq.error = None;
            }
            Err(msg) => dlq.error = Some(msg),
        }
    }

    fn handle_dlq_action_result(&mut self, env_name: String, result: Result<DlqOp, String>) {
        let mut refetch = false;
        {
            let Some(dlq) = self.dlq.as_mut() else { return };
            if dlq.env_name != env_name {
                return;
            }
            match result {
                Ok(DlqOp::Resent { message_id }) => {
                    dlq.messages.retain(|m| m.id != message_id);
                    self.status_message = Some(format!("message {message_id} resent"));
                }
                Ok(DlqOp::Purged) => {
                    dlq.messages.clear();
                    self.status_message = Some("DLQ purged".into());
                }
                Ok(DlqOp::Replayed { count, failures }) => {
                    if failures == 0 {
                        self.status_message =
                            Some(format!("replayed {count} message(s) to the main queue"));
                    } else {
                        self.error_message =
                            Some(format!("replayed {count}, {failures} failed — see log"));
                    }
                    // The loaded page changed substantially — refetch rather
                    // than reconcile message ids one by one.
                    refetch = true;
                }
                Err(msg) => dlq.error = Some(msg),
            }
        }
        if refetch {
            self.spawn_dlq_fetch();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_none_for_context_independent_messages() {
        // `Rebuild` / `UpdateCheck` aren't tied to an AWS context — they
        // must always be delivered, never gen-dropped.
        assert_eq!(AppMsg::UpdateCheck(None).generation(), None);
        assert_eq!(AppMsg::Rebuild(Err("boom".into())).generation(), None);
    }

    #[test]
    fn generation_extracts_gen_from_context_bound_messages() {
        assert_eq!(
            AppMsg::Refresh {
                gen: 7,
                result: Ok(Vec::new()),
            }
            .generation(),
            Some(7)
        );
        assert_eq!(
            AppMsg::Events {
                gen: 42,
                result: Ok(Vec::new()),
            }
            .generation(),
            Some(42)
        );
    }
}
