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
    /// return `None` and are always delivered. `ClientRefreshed` also
    /// returns `None`, but for a different reason: it carries
    /// `rebuild_epoch` and `apply_client_refresh` checks that instead —
    /// a generation guard would be the wrong test, since the whole
    /// point is that the context has NOT changed.
    fn generation(&self) -> Option<u64> {
        use AppMsg::*;
        match self {
            Rebuild { .. } | ClientRefreshed { .. } | UpdateCheck(_) => None,
            Refresh { gen, .. }
            | Identity { gen, .. }
            | Applications { gen, .. }
            | SolutionStacks { gen, .. }
            | AppLatestVersions { gen, .. }
            | WorkerQueueCheck { gen, .. }
            | FiringAlarms { gen, .. }
            | EnvInstanceCountsCheck { gen, .. }
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
            | VersionPreview { gen, .. }
            | HealthCheckProbe { gen, .. }
            | UnavailabilityEstimate { gen, .. }
            | ConfirmModalLint { gen, .. }
            | LintInputsCached { gen, .. }
            | RolloutPreflight { gen, .. }
            | RolloutDispatched { gen, .. }
            | UndoCaptured { gen, .. }
            | RollbackTarget { gen, .. }
            | DetailTags { gen, .. }
            | DeployFromLocal { gen, .. }
            | LogTailOpened { gen, .. }
            | LogTailEvents { gen, .. }
            | EventTailOpened { gen, .. }
            | EventTailEvents { gen, .. }
            | DetailLogGroups { gen, .. }
            | DetailAlarms { gen, .. }
            | DetailAppMetrics { gen, .. }
            | DetailTopErrors { gen, .. }
            | AppTabDiscovered { gen, .. }
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
            | DlqActionResult { gen, .. }
            | AutoRollbackCheck { gen, .. } => Some(*gen),
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
            AppMsg::Refresh {
                fanout,
                result,
                partial_errors,
                ..
            } => self.apply_refresh(fanout, result, partial_errors),
            AppMsg::Rebuild { epoch, result } => self.apply_rebuild(epoch, result),
            AppMsg::ClientRefreshed { epoch, result } => self.apply_client_refresh(epoch, result),
            AppMsg::Identity { result, .. } => self.handle_identity(result),
            AppMsg::Applications { result, .. } => self.handle_applications(result),
            AppMsg::SolutionStacks { result, .. } => self.handle_solution_stacks(result),
            AppMsg::AppLatestVersions { results, .. } => self.handle_app_latest_versions(results),
            AppMsg::WorkerQueueCheck { round, results, .. } => {
                self.handle_worker_queue_check(round, results)
            }
            AppMsg::FiringAlarms { round, results, .. } => {
                self.handle_firing_alarms(round, results)
            }
            AppMsg::EnvInstanceCountsCheck { round, results, .. } => {
                self.handle_env_instance_counts(round, results)
            }
            AppMsg::Events { result, .. } => self.handle_events(result),
            AppMsg::DetailEvents {
                env_name,
                ticket,
                result,
                ..
            } => self.handle_detail_events(env_name, ticket, result),
            AppMsg::ActionResult {
                pending,
                action,
                env_name,
                result,
                ..
            } => self.handle_action_result(pending, action, env_name, result),
            AppMsg::DetailInstances {
                env_name,
                ticket,
                result,
                ..
            } => self.handle_detail_instances(env_name, ticket, result),
            AppMsg::DetailMetrics {
                env_name,
                ticket,
                result,
                ..
            } => self.handle_detail_metrics(env_name, ticket, result),
            AppMsg::DetailAppMetrics {
                env_name,
                ticket,
                result,
                ..
            } => self.apply_detail_msg(&env_name, result, |d, r| {
                d.app_tab.metrics.settle_ticket(ticket, r);
            }),
            AppMsg::AppTabDiscovered {
                key, ticket, found, ..
            } => self.handle_app_tab_discovered(key, ticket, *found),
            AppMsg::DetailTopErrors {
                env_name,
                ticket,
                result,
                ..
            } => self.apply_detail_msg(&env_name, result, |d, r| {
                d.app_tab.top_errors.settle_ticket(ticket, r);
            }),
            AppMsg::DetailLogsProgress {
                ticket,
                stage,
                attempt,
                ..
            } => self.handle_detail_logs_progress(ticket, stage, attempt),
            AppMsg::DetailLogs { ticket, result, .. } => self.handle_detail_logs(ticket, result),
            AppMsg::TextOverlay { title, body, .. } => self.handle_text_overlay(title, body),
            AppMsg::AppVersions {
                application,
                deployed_label,
                result,
                ..
            } => self.handle_app_versions(application, deployed_label, result),
            AppMsg::UpdateCheck(latest) => self.handle_update_check(latest),
            AppMsg::AutoRollbackCheck { env, .. } => self.handle_auto_rollback_check(env),
            AppMsg::DryRunResult {
                env_name,
                result,
                region,
                ..
            } if self.modal_takes(&region) => self.handle_dry_run_result(env_name, result),
            AppMsg::Alarms { env, result, .. } => self.handle_alarms(env, result),
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
                env_name,
                result,
                region,
                ..
            } if self.modal_takes(&region) => self.handle_preflight_events(env_name, result),
            AppMsg::VersionPreview {
                env_name,
                result,
                region,
                ..
            } if self.modal_takes(&region) => self.handle_version_preview(env_name, result),
            AppMsg::HealthCheckProbe {
                env_name,
                result,
                region,
                ..
            } if self.modal_takes(&region) => self.handle_health_check_probe(env_name, result),
            AppMsg::UnavailabilityEstimate {
                env_name,
                line,
                region,
                ..
            } if self.modal_takes(&region) => self.handle_unavailability_estimate(env_name, line),
            AppMsg::ConfirmModalLint {
                env_name,
                issues,
                not_run,
                region,
                ..
            } if self.modal_takes(&region) => {
                self.handle_confirm_modal_lint(env_name, issues, not_run)
            }
            AppMsg::LintInputsCached {
                key, tags, healthy, ..
            } => self.handle_lint_inputs_cached(key, tags, healthy),
            AppMsg::RolloutPreflight { region, result, .. } => {
                self.handle_rollout_preflight(region, result)
            }
            AppMsg::RolloutDispatched { region, result, .. } => {
                self.handle_rollout_dispatched(region, result)
            }
            AppMsg::UndoCaptured { entry, .. } => self.handle_undo_captured(entry),
            AppMsg::RollbackTarget {
                env_name,
                region,
                current_version,
                result,
                ..
            } => {
                self.handle_rollback_target(EnvKey::new(region, env_name), current_version, result)
            }
            AppMsg::DetailTags {
                env_name,
                ticket,
                result,
                ..
            } => self.handle_detail_tags(env_name, ticket, result),
            AppMsg::DeployFromLocal {
                pending,
                env_name,
                label,
                summary,
                result,
                ..
            } => self.handle_deploy_from_local(pending, env_name, label, summary, result),
            AppMsg::LogTailOpened {
                session_id,
                env_name,
                region,
                log_group,
                since_ms,
                ..
            } => self.handle_log_tail_opened(
                session_id,
                EnvKey::new(region, env_name),
                log_group,
                since_ms,
            ),
            AppMsg::LogTailEvents {
                session_id,
                next_since_ms,
                result,
                ..
            } => self.handle_log_tail_events(session_id, next_since_ms, result),
            AppMsg::EventTailOpened { session_id, .. } => self.handle_event_tail_opened(session_id),
            AppMsg::EventTailEvents {
                session_id, result, ..
            } => self.handle_event_tail_events(session_id, result),
            AppMsg::DetailLogGroups { key, groups, .. } => {
                self.handle_detail_log_groups(key, groups)
            }
            AppMsg::DetailAlarms {
                env_name,
                ticket,
                result,
                ..
            } => self.handle_detail_alarms(env_name, ticket, result),
            AppMsg::EnvVarsForEdit {
                env_name,
                region,
                result,
                ..
            } => self.handle_env_vars_for_edit(EnvKey::new(region, env_name), result),
            AppMsg::CostsFetched {
                account,
                region,
                result,
                ..
            } => self.handle_costs_fetched(account, region, result),
            AppMsg::DetailRecentVersions {
                env_name,
                ticket,
                result,
                ..
            } => self.handle_detail_recent_versions(env_name, ticket, result),
            AppMsg::FormPrefilled { key, settings, .. } => {
                self.handle_form_prefilled(key, settings)
            }
            AppMsg::FormMultiSelectLoaded {
                key,
                field_key,
                result,
                ..
            } => self.handle_form_multi_select_loaded(key, field_key, result),
            AppMsg::DetailEnvVars {
                env_name,
                ticket,
                result,
                ..
            } => self.handle_detail_env_vars(env_name, ticket, result),
            AppMsg::OptionSettingsUpdate {
                pending,
                env_name,
                summary,
                result,
                ..
            } => self.handle_option_settings_update(pending, env_name, summary, result),
            AppMsg::AlarmOp {
                pending,
                verb,
                alarm_name,
                result,
                ..
            } => self.handle_alarm_op(pending, verb, alarm_name, result),
            AppMsg::DeleteAppVersion {
                pending,
                application,
                region,
                label,
                force,
                result,
                ..
            } => self.handle_delete_app_version(pending, application, region, label, force, result),
            AppMsg::TagUpdate {
                pending,
                env_name,
                summary,
                result,
                ..
            } => self.handle_tag_update(pending, env_name, summary, result),
            AppMsg::DetailQueues {
                env_name,
                ticket,
                result,
                ..
            } => self.handle_detail_queues(env_name, ticket, result),
            // An answer for the modal's env name in another region: a
            // same-named env's modal, since closed.
            AppMsg::DryRunResult { .. }
            | AppMsg::PreflightEvents { .. }
            | AppMsg::VersionPreview { .. }
            | AppMsg::HealthCheckProbe { .. }
            | AppMsg::UnavailabilityEstimate { .. }
            | AppMsg::ConfirmModalLint { .. } => {}
            AppMsg::DlqMessages {
                env_name,
                queue_url,
                result,
                ..
            } => self.handle_dlq_messages(env_name, queue_url, result),
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
                merge_app_latest_versions(&self.caches.applications, &mut apps);
                self.caches.applications = apps;
                // Pinned-first sort runs every refresh so newly-arrived
                // apps don't shuffle the pinned ones off the top row.
                self.resort_applications();
                if self.caches.applications.is_empty() {
                    self.app_table_state.select(None);
                } else if self
                    .app_table_state
                    .selected()
                    .map(|s| s >= self.caches.applications.len())
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
                self.caches.latest_stacks = crate::aws::latest_stack_versions(&stacks);
                self.caches.latest_stacks_error = None;
                self.view.invalidate();
                // Refresh the derived stale-platform cache now that the
                // catalogue is available — the env list itself is unchanged.
                self.rebuild_view();
            }
            Err(msg) => {
                tracing::warn!(error = %msg, "solution stacks fetch failed");
                self.caches.latest_stacks_error = Some(msg);
            }
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
        for app in self.caches.applications.iter_mut() {
            if let Some((label, created)) = by_name.get(&app.name) {
                app.latest_version_label = label.clone();
                app.latest_version_created = *created;
            }
        }
    }

    fn handle_worker_queue_check(
        &mut self,
        round: u64,
        results: Vec<(EnvKey, Result<Option<i64>, String>)>,
    ) {
        if !self.caches.worker_dlq.accept_round(round) {
            return;
        }
        // Per-env reconcile (NOT a clear-and-rebuild — that dropped a
        // failed env's entry every tick, silently clearing its
        // `⚠ DLQ:N` chip and alert): a fetched depth updates, a genuine
        // "no DLQ" removes, and a fetch error keeps the previous depth
        // so alerts survive an AccessDenied/throttle blip.
        for (key, outcome) in results {
            // Logged when the check starts failing, not on every 15s
            // tick while it stays failed.
            if let Err(e) = &outcome {
                if !self.caches.worker_dlq.is_stale(&key) {
                    tracing::warn!(
                        env = %key.name,
                        region = %key.region,
                        error = %e,
                        "worker queue check failed — keeping previous DLQ depth (marked stale)"
                    );
                }
            }
            self.caches.worker_dlq.reconcile(key, outcome);
        }
        // Recompute alerts now that the cache is fresh — the count set
        // during apply_refresh used the *previous* tick's cache. Workers
        // newly above DLQ=0 join the alert pill on the next draw.
        self.alerts = compute_red_alerts(&self.environments, |e| self.dlq_depth(e));
    }

    fn handle_firing_alarms(
        &mut self,
        round: u64,
        results: Vec<(String, Result<Vec<crate::aws::FiringAlarm>, String>)>,
    ) {
        if !self.caches.firing_alarms.accept_round(round) {
            return;
        }
        for (region, outcome) in results {
            // This region's rows only: a same-named env elsewhere is
            // another key, answered by its own region.
            let envs: Vec<EnvKey> = self
                .environments
                .iter()
                .filter(|e| self.region_for(e) == region)
                .map(|e| self.env_key(e))
                .collect();
            match outcome {
                Ok(firing) => {
                    for key in envs {
                        let names = crate::alarms::firing_for(
                            &self.alarm_match_for(&key.name),
                            &key.name,
                            &firing,
                        );
                        self.caches
                            .firing_alarms
                            .reconcile::<()>(key, Ok((!names.is_empty()).then_some(names)));
                    }
                }
                // A failure keeps the last known alarms and marks the
                // count unknown. Logged when the region first fails,
                // not on every 15s tick while it stays failed.
                Err(e) => {
                    if envs.iter().any(|k| !self.caches.firing_alarms.is_stale(k)) {
                        tracing::warn!(
                            region = %region,
                            error = %e,
                            "firing-alarm check failed — keeping previous alarms (marked unknown)"
                        );
                    }
                    for key in envs {
                        self.caches.firing_alarms.reconcile(key, Err(()));
                    }
                }
            }
        }
        // They land after the refresh that sorted the table.
        if self.view.sort_key() == SortKey::Alarms {
            let follow = self.selected_key();
            self.resort_envs();
            self.rebuild_view();
            if let Some(key) = follow {
                self.select_key(&key);
            }
        }
    }

    fn handle_env_instance_counts(
        &mut self,
        round: u64,
        results: Vec<(
            EnvKey,
            Result<Option<crate::aws::EnvInstanceCounts>, String>,
        )>,
    ) {
        if !self.caches.env_instance_counts.accept_round(round) {
            return;
        }
        // Per env, as the DLQ check: one env's throttle keeps its last
        // counts (marked stale) instead of blanking its INST cell.
        for (key, outcome) in results {
            if let Err(e) = &outcome {
                tracing::debug!(env = %key.name, error = %e, "instance count check failed");
            }
            self.caches.env_instance_counts.reconcile(key, outcome);
        }
    }

    fn handle_events(&mut self, result: Result<Vec<EbEvent>, String>) {
        match result {
            // The API returns events in time-descending order already.
            Ok(events) => self.event_panel.events = events,
            Err(msg) => tracing::warn!(error = %msg, "event fetch failed"),
        }
    }

    fn handle_detail_events(
        &mut self,
        env_name: String,
        ticket: crate::app::Ticket,
        result: Result<Vec<EbEvent>, String>,
    ) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            if !d.events_request.settle_ticket(ticket, Ok(())) {
                return;
            }
            match r {
                Ok(events) => {
                    d.events = events;
                    d.clear_error(crate::mode_detail::DetailFetch::Events);
                }
                Err(msg) => d.set_error(crate::mode_detail::DetailFetch::Events, msg),
            }
        });
    }

    fn handle_action_result(
        &mut self,
        pending: PendingId,
        action: Action,
        env_name: String,
        result: Result<(), String>,
    ) {
        // No audit line here: `action_result` wrote it in the task, so a
        // result the generation guard drops after a context switch still
        // has one.
        // Stamp the matching pending-actions entry with the outcome so the
        // panel shows ✓ / ✗ instead of "in flight".
        self.complete_pending(pending, result.clone());
        // Leaves the action flow and the mode alone. The confirm modal
        // closed when the dispatch was queued, so whatever is open now
        // was opened since: an unattended result (an auto-rollback, a
        // batch member) closing it threw the operator out of a form or
        // an overlay mid-edit, leaving the form behind with no mode to
        // show it.
        match result {
            Ok(()) => {
                self.status_message = Some(format!("{} on {env_name} dispatched", action.label()));
                self.spawn_refresh();
            }
            Err(msg) => {
                self.error_message =
                    Some(format!("{} on {env_name} failed: {msg}", action.label()));
            }
        }
    }

    fn handle_detail_instances(
        &mut self,
        env_name: String,
        ticket: crate::app::Ticket,
        result: Result<Vec<Instance>, String>,
    ) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            if !d.instances_request.settle_ticket(ticket, Ok(())) {
                return;
            }
            match r {
                Ok(instances) => {
                    d.instances = instances;
                    d.clear_error(crate::mode_detail::DetailFetch::Instances);
                }
                Err(msg) => d.set_error(crate::mode_detail::DetailFetch::Instances, msg),
            }
        });
    }

    /// Only the latest request's answer lands: one for a range the
    /// operator has since moved off, or from before a `^R`, is dropped,
    /// and the newer request stays in flight, so the tab keeps loading
    /// until the right answer arrives rather than showing the wrong
    /// window under the right title.
    fn handle_detail_metrics(
        &mut self,
        env_name: String,
        ticket: crate::app::Ticket,
        result: super::spawn_detail::MetricsResult,
    ) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            if !d.metrics_request.settle_ticket(ticket, Ok(())) {
                return;
            }
            match r {
                Ok((metrics, partial)) => {
                    d.metrics = metrics;
                    match partial {
                        Some(msg) => d.set_error(crate::mode_detail::DetailFetch::Metrics, msg),
                        None => d.clear_error(crate::mode_detail::DetailFetch::Metrics),
                    }
                }
                Err(msg) => d.set_error(crate::mode_detail::DetailFetch::Metrics, msg),
            }
        });
    }

    fn handle_detail_logs_progress(&mut self, ticket: Ticket, stage: LogTailStage, attempt: u32) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if !detail.log_tail.request.is_latest(ticket) {
            return;
        }
        detail.log_tail.stage = stage;
        if matches!(stage, LogTailStage::Polling) {
            detail.log_tail.poll_attempt = attempt;
        }
    }

    /// Only the latest tail's answer lands: a `^R` mid-tail started
    /// another, and tickets are process-wide, so a same-named env's
    /// answer matches nothing here either.
    fn handle_detail_logs(
        &mut self,
        ticket: Ticket,
        result: Result<Vec<(String, String)>, String>,
    ) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if !detail.log_tail.request.settle_ticket(ticket, Ok(())) {
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
                    body: format_app_versions(
                        &versions,
                        deployed_label.as_deref(),
                        20,
                        matches!(self.theme.icons, crate::theme::IconStyle::Ascii),
                    ),
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

    /// Watchdog fired by `:deploy --auto-rollback Nm`. Reads the env's
    /// cached health (populated by the refresh loop) and either
    /// disarms with a status toast (env is Green) or dispatches a
    /// rollback redeploy back to the captured pre-deploy snapshot's
    /// previous version.
    /// Deadline tokio task fired — kick a manual refresh so
    /// `apply_refresh` (the single source of truth for watchdog
    /// outcomes) can evaluate against fresh health. We deliberately
    /// don't inline the dispatch here: that earlier shape read
    /// possibly-stale cached health and could trigger a spurious
    /// rollback when the next refresh would have shown Green. The
    /// trade-off is up to one refresh-roundtrip of extra latency
    /// past the deadline before dispatch, which is well below the
    /// human-noticeable threshold for an alert-and-rollback flow.
    fn handle_auto_rollback_check(&mut self, env: EnvKey) {
        // Slot may have been drained already (apply_refresh early-
        // disarm fired between the tokio task starting and this
        // message landing). If so, nothing to do.
        if !self.armed_watchdogs.contains_key(&env) {
            return;
        }
        // Kick the refresh. The watchdog stays armed in the meantime
        // so a second deadline tokio (e.g. operator re-armed) doesn't
        // get confused. Refresh result lands → apply_refresh →
        // watchdog decision pass.
        self.spawn_refresh();
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

    fn handle_alarms(&mut self, env: EnvKey, result: Result<Vec<CwAlarm>, String>) {
        // Drop stale results: the user may have closed the overlay or
        // requested alarms for a different env during the round-trip. The
        // overlay carries the env it was opened for; only replace its body
        // if that still matches the result we just received.
        match self.current_overlay.as_mut() {
            Some(Overlay::Alarms {
                env: requested,
                body,
            }) if requested == &env => {
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

    /// Stuff the rendered preview body into the modal slot — or
    /// drop it on the floor if the modal closed / switched envs
    /// while the fetch was in flight. A failed fetch is rendered
    /// as an inline error message rather than left blank so the
    /// operator knows the preview tried and failed.
    fn handle_version_preview(&mut self, env_name: String, result: Result<String, String>) {
        let Some(ActionFlow::Confirm(modal)) = self.action_flow.as_mut() else {
            return;
        };
        if modal.target_env != env_name {
            return;
        }
        modal.loading_version_preview = false;
        modal.version_preview = Some(match result {
            Ok(body) => body,
            Err(e) => format!("version preview unavailable: {e}"),
        });
    }

    /// Stuff the pre-deploy health-check probe outcome into the
    /// modal — clears the loading flag either way. Same drop-on-
    /// floor contract as `handle_version_preview` for cases where
    /// the modal closed mid-flight.
    fn handle_health_check_probe(&mut self, env_name: String, result: Result<(), String>) {
        let Some(ActionFlow::Confirm(modal)) = self.action_flow.as_mut() else {
            return;
        };
        if modal.target_env != env_name {
            return;
        }
        modal.loading_health_check = false;
        modal.health_check_probe = Some(result);
    }

    /// Stuff the pre-deploy unavailability estimate into the modal.
    /// `None` (fetch failed) clears the loading flag and leaves the
    /// line slot empty — UI silently omits the row.
    fn handle_unavailability_estimate(&mut self, env_name: String, line: Option<(String, bool)>) {
        let Some(ActionFlow::Confirm(modal)) = self.action_flow.as_mut() else {
            return;
        };
        if modal.target_env != env_name {
            return;
        }
        modal.loading_unavailability = false;
        modal.unavailability_line = line;
    }

    /// Stuff confirm-modal lint findings into the modal. Empty
    /// vec is fine — the UI renders nothing for that case
    /// (operator sees positive feedback by absence). Generation
    /// guard upstream means stale results from a previous
    /// modal-open won't land here.
    fn handle_confirm_modal_lint(
        &mut self,
        env_name: String,
        issues: Vec<crate::lint::Issue>,
        not_run: Vec<String>,
    ) {
        let Some(ActionFlow::Confirm(modal)) = self.action_flow.as_mut() else {
            return;
        };
        if modal.target_env != env_name {
            return;
        }
        modal.loading_lint = false;
        modal.lint_issues = Some(issues);
        modal.lint_not_run = not_run;
    }

    /// 0.21: write freshly-fetched lint inputs into the App caches
    /// so the next modal-open against the same env hits cache.
    /// `None` entries indicate the value came from cache (no need
    /// to re-store) or the fetch failed.
    fn handle_lint_inputs_cached(
        &mut self,
        key: EnvKey,
        tags: Option<Vec<String>>,
        healthy: Option<i64>,
    ) {
        let now = std::time::Instant::now();
        if let Some(t) = tags {
            self.caches.env_tag_cache.insert(key.clone(), (t, now));
        }
        if let Some(h) = healthy {
            self.caches.env_health_cache.insert(key, (h, now));
        }
    }

    /// Land one region's pre-flight result on the active
    /// rollout flow. Updates the per-region row; if all rows
    /// have landed, transitions the flow from Planning to
    /// AwaitingConfirm so the operator can press `y` to
    /// dispatch.
    fn handle_rollout_preflight(&mut self, region: String, result: Result<String, String>) {
        let Some(crate::mode_action::ActionFlow::Rollout(flow)) = self.action_flow.as_mut() else {
            // Operator escaped the flow mid-preflight; drop the
            // result. No need to surface — the pre-flight tokio
            // task already completed.
            return;
        };
        let Some(row) = flow.regions.iter_mut().find(|r| r.region == region) else {
            return;
        };
        match result {
            Ok(current) => {
                row.env_found = Some(true);
                row.current_version = Some(current);
            }
            Err(e) => {
                row.env_found = Some(false);
                row.preflight_error = Some(e);
            }
        }
        // If every region has landed, advance state. Operators
        // see the plan + can press y / n.
        let all_in = flow.regions.iter().all(|r| r.env_found.is_some());
        if all_in {
            flow.state = crate::mode_action::RolloutState::AwaitingConfirm;
            let n_ok = flow
                .regions
                .iter()
                .filter(|r| r.env_found == Some(true))
                .count();
            let n_total = flow.regions.len();
            self.status_message = Some(format!(
                "rollout: pre-flight done ({n_ok}/{n_total} regions ok) — press y to dispatch, esc to abort"
            ));
        }
    }

    /// Land one region's dispatch outcome on the active rollout
    /// flow. Advances `next_index`; if the outcome was Err,
    /// halts the rollout. Otherwise fires the next region's
    /// dispatch. Final outcome (success / partial-failure)
    /// surfaces as a `Done` state with a status toast.
    fn handle_rollout_dispatched(&mut self, region: String, result: Result<(), String>) {
        // Pull the values we need before re-borrowing for the
        // dispatch fire — borrow checker can't see we'd switch
        // to a different field of the same enum variant.
        let (
            should_continue,
            next_region,
            env_name,
            version_label,
            wait_for_green_secs,
            rollout_id,
        ) = {
            let Some(crate::mode_action::ActionFlow::Rollout(flow)) = self.action_flow.as_mut()
            else {
                return;
            };
            let Some(row) = flow.regions.iter_mut().find(|r| r.region == region) else {
                return;
            };
            row.outcome = Some(result.clone());
            // Mirror the CLI's per-region audit line — RolloutDispatched
            // arrival means the region's outcome is known.
            crate::audit::append_rollout(
                &flow.rollout_id,
                self.context.profile.as_deref(),
                &region,
                &flow.env_name,
                &flow.version_label,
                "completed",
                result.as_ref().err().map(String::as_str),
            );
            // Move the cursor. If `Err`, halt; otherwise advance to the
            // next region that PASSED pre-flight — failed ones keep
            // outcome None ("skipped"), matching the y-confirm gate's
            // promise (previously only leading failures were skipped;
            // a mid-sequence failed region got a doomed dispatch).
            let next_start = match &flow.state {
                crate::mode_action::RolloutState::Dispatching { next_index } => next_index + 1,
                _ => return,
            };
            let halt = result.is_err();
            let n = flow.regions.len();
            let next_eligible = (next_start..n).find(|&i| flow.regions[i].env_found == Some(true));
            // Halting, or running out of eligible regions, ends the
            // rollout; otherwise `next` is the region to dispatch. Both
            // facts come from the same `Option`, so neither branch has
            // to re-derive the other's precondition.
            let next_up = if halt { None } else { next_eligible };
            match next_up {
                Some(next) => {
                    flow.state = crate::mode_action::RolloutState::Dispatching { next_index: next };
                    let next_region = flow.regions[next].region.clone();
                    (
                        true,
                        Some(next_region),
                        flow.env_name.clone(),
                        flow.version_label.clone(),
                        flow.wait_for_green_secs,
                        flow.rollout_id.clone(),
                    )
                }
                None => {
                    flow.state = crate::mode_action::RolloutState::Done;
                    let n_ok = flow
                        .regions
                        .iter()
                        .filter(|r| matches!(r.outcome, Some(Ok(()))))
                        .count();
                    let n_err = flow
                        .regions
                        .iter()
                        .filter(|r| matches!(r.outcome, Some(Err(_))))
                        .count();
                    let n_skipped = flow.regions.iter().filter(|r| r.outcome.is_none()).count();
                    if halt {
                        self.error_message = Some(format!(
                            "rollout {}: halted ({n_ok} ok, {n_err} failed, {n_skipped} skipped)",
                            flow.rollout_id
                        ));
                    } else {
                        self.status_message = Some(format!(
                            "rollout {}: complete ({n_ok}/{n} regions ok)",
                            flow.rollout_id
                        ));
                    }
                    (
                        false,
                        None,
                        String::new(),
                        String::new(),
                        None,
                        String::new(),
                    )
                }
            }
        };
        if should_continue {
            let profile = self.context.profile.clone();
            if let Some(region) = next_region {
                self.spawn_rollout_dispatch(
                    rollout_id,
                    profile,
                    region,
                    env_name,
                    version_label,
                    wait_for_green_secs,
                );
            }
        }
    }

    /// Push a captured undo entry onto the history deque. Cap'd at
    /// `UNDO_HISTORY_CAP` — older entries fall off the front when
    /// the cap is hit. Generation guard upstream means a stale
    /// capture from a previous context won't land here.
    fn handle_undo_captured(&mut self, entry: crate::app::UndoEntry) {
        self.undo_history.push_back(entry);
        while self.undo_history.len() > crate::app::UNDO_HISTORY_CAP {
            self.undo_history.pop_front();
        }
    }

    fn handle_rollback_target(
        &mut self,
        target: EnvKey,
        current_version: String,
        result: Result<Vec<EbEvent>, String>,
    ) {
        let env_name = target.name.clone();
        match result {
            Err(e) => self.error_message = Some(format!("rollback: {e}")),
            Ok(events) => match previous_version_label(&events, &current_version) {
                None => {
                    self.error_message = Some(format!(
                        "rollback: no prior version in {env_name}'s recent events — use :versions then :deploy"
                    ));
                }
                // The deploy-confirm modal targets the env on screen
                // (Detail's, else the selected row); if that moved off the
                // env `:rollback` was issued for, bail rather than roll
                // back the wrong one. By key: the same name in another
                // region is another env.
                Some(_) if self.pointed_env().map(|e| self.env_key(&e)) != Some(target) => {
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
        ticket: crate::app::Ticket,
        result: Result<Vec<(String, String)>, String>,
    ) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            if !d.tags_request.settle_ticket(ticket, Ok(())) {
                return;
            }
            match r {
                Ok(tags) => {
                    d.tags = tags;
                    // A delete may have shrunk the list / removed the row
                    // mid-edit.
                    d.clamp_config_cursor();
                    d.revalidate_config_edit();
                    d.clear_error(crate::mode_detail::DetailFetch::Tags);
                }
                Err(msg) => {
                    tracing::warn!(error = %msg, "tags fetch failed");
                    d.set_error(crate::mode_detail::DetailFetch::Tags, msg);
                }
            }
        });
    }

    fn handle_deploy_from_local(
        &mut self,
        pending: PendingId,
        env_name: String,
        label: String,
        summary: String,
        result: Result<(), String>,
    ) {
        self.complete_pending(pending, result.clone());
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
        env: EnvKey,
        log_group: String,
        since_ms: i64,
    ) {
        if session_id != self.log_tail_session {
            return;
        }
        self.current_overlay = Some(Overlay::LogTail {
            log_group,
            env_name: env.name,
            region: env.region,
            events: std::collections::VecDeque::with_capacity(LOG_TAIL_MAX_LINES),
            since_ms,
            view: super::tail::TailView::new(),
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
            // The session is live but no overlay slot holds it — the
            // overlay was replaced by something else without going
            // through the close handler (e.g. the operator opened
            // `:why` mid-tail). Reap the polling task so it doesn't
            // keep hitting CW Logs invisibly; the session bump makes
            // any already-queued messages drop at the guard above.
            super::tail::reap_tail_task(&mut self.log_tail_task, &mut self.log_tail_session);
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

    fn handle_event_tail_opened(&mut self, session_id: u64) {
        if session_id != self.event_tail_session {
            return;
        }
        self.current_overlay = Some(Overlay::EventTail {
            events: std::collections::VecDeque::with_capacity(EVENT_TAIL_MAX_EVENTS),
            view: super::tail::TailView::new(),
            last_err: None,
            session_id,
            truncated_polls: 0,
        });
        self.status_message = None;
    }

    fn handle_event_tail_events(
        &mut self,
        session_id: u64,
        result: Result<Vec<crate::aws::Event>, String>,
    ) {
        if session_id != self.event_tail_session {
            return;
        }
        // Same two-slot routing as LogTail: events arriving while the
        // user is in `?` help land in the stashed overlay.
        let target = if matches!(
            self.current_overlay.as_ref(),
            Some(Overlay::EventTail { session_id: s, .. }) if *s == session_id
        ) {
            self.current_overlay.as_mut()
        } else if matches!(
            self.help.pre_overlay.as_ref(),
            Some(Overlay::EventTail { session_id: s, .. }) if *s == session_id
        ) {
            self.help.pre_overlay.as_mut()
        } else {
            // Overlay replaced without the close handler running —
            // reap the poller instead of letting it hit DescribeEvents
            // invisibly until context switch. Same shape as the
            // LogTail reap above.
            super::tail::reap_tail_task(&mut self.event_tail_task, &mut self.event_tail_session);
            return;
        };
        let Some(Overlay::EventTail {
            events,
            last_err,
            truncated_polls,
            ..
        }) = target
        else {
            return;
        };
        match result {
            Ok(new_events) => {
                *last_err = None;
                for ev in new_events {
                    // Count the gap BEFORE pushing: the marker itself
                    // can be evicted by this very batch, so the tally in
                    // the chrome is the part that has to survive.
                    if crate::app::is_event_tail_gap(&ev) {
                        *truncated_polls += 1;
                    }
                    if events.len() >= EVENT_TAIL_MAX_EVENTS {
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

    fn handle_detail_log_groups(&mut self, key: EnvKey, groups: Vec<String>) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.key != key {
            return;
        }
        detail.cw_log_groups = Some(groups);
    }

    fn handle_detail_alarms(
        &mut self,
        env_name: String,
        ticket: Ticket,
        result: Result<Vec<crate::aws::CwAlarm>, String>,
    ) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.env_name != env_name {
            return;
        }
        detail.cw_alarms.settle_ticket(ticket, result);
    }

    fn handle_env_vars_for_edit(
        &mut self,
        target: EnvKey,
        result: Result<Vec<(String, String)>, String>,
    ) {
        match result {
            Ok(vars) => {
                // Stash for the main loop to consume. The editor shell-out
                // has to happen there because that's where the `Tui`
                // handle is.
                self.pending_env_edit = Some((target, vars));
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
        result: Result<crate::aws::EnvCosts, String>,
    ) {
        match result {
            Ok(costs) => {
                let now = chrono::Utc::now();
                let n = costs.rows.len();
                if costs.truncated {
                    // A truncated walk is a failed refresh, not a
                    // smaller one. Replacing a good map with a partial
                    // one flips every env past the cap from a real
                    // number to `—`, which renders identically to
                    // "untagged" — so `:cost` and `:fleet-cost` would
                    // quietly under-report against numbers the operator
                    // was reading a moment ago. Keep what we have.
                    tracing::warn!(
                        target: "ebman::cost",
                        envs = n,
                        had_previous = !self.costs.is_empty(),
                        "Cost Explorer walk truncated"
                    );
                    // Replace only when what we hold is no better: an
                    // empty map, or one that was itself partial. A
                    // COMPLETE map is worth more than fresher partial
                    // data, and must survive.
                    if !self.costs.partial_would_lose_better_data() {
                        // Nothing better to preserve. `set_partial` is
                        // what makes "don't cache it, don't stamp a
                        // fetch time that would suppress the retry"
                        // structural rather than remembered.
                        self.costs
                            .set_partial(costs.rows.iter().map(|r| (r.key(), r.cost_usd)));
                        self.error_message = Some(format!(
                            "cost: INCOMPLETE — Cost Explorer returned more pages than ebman \
                             will walk. Showing {n} env(s); not cached — `:cost on` retries."
                        ));
                    } else {
                        self.error_message = Some(format!(
                            "cost: refresh INCOMPLETE (Cost Explorer page cap) — keeping the \
                             previous {} env(s) rather than replacing them with {n}. `:cost on` retries.",
                            self.costs.len()
                        ));
                    }
                    return;
                }
                self.costs
                    .set_complete(costs.rows.iter().map(|r| (r.key(), r.cost_usd)), now);
                {
                    // Persist to ~/.cache/ebman/cost-{account}-{region}.toml
                    // so subsequent sessions render immediately.
                    let account_key = account.unwrap_or_else(|| "unknown".into());
                    let cache = crate::cost_cache::CostCache {
                        fetched_at: Some(now),
                        costs: self.costs.to_cache_map(),
                    };
                    if let Err(e) = crate::cost_cache::save(&account_key, &region, &cache) {
                        tracing::warn!(
                            target: "ebman::cost",
                            error = %e,
                            "cost cache write failed (non-fatal)"
                        );
                    }
                    self.status_message = Some(format!("cost: refreshed {n} env(s)"));
                }
            }
            Err(msg) => {
                self.error_message = Some(format!("cost fetch: {msg}"));
            }
        }
    }

    fn handle_detail_recent_versions(
        &mut self,
        env_name: String,
        ticket: Ticket,
        result: Result<Vec<crate::aws::AppVersion>, String>,
    ) {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.env_name != env_name {
            return;
        }
        detail.recent_versions.settle_ticket(ticket, result);
    }

    fn handle_form_prefilled(
        &mut self,
        key: EnvKey,
        settings: Result<Vec<(String, String, String)>, String>,
    ) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        // By key: under `:region all` a form closed on one twin and
        // opened on the other would otherwise take the first one's
        // settings, and Submit would write them to the second.
        if form.key() != key {
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
                    // Pre-filled by discovery (`AppTabDiscovered`).
                    crate::form::FormSubmit::AppTab => return,
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
        key: EnvKey,
        field_key: String,
        result: Result<MultiSelectOptions, String>,
    ) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        // By key: another region's subnets, security groups or
        // certificates do not exist in this one's.
        if form.key() != key {
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
        ticket: crate::app::Ticket,
        result: Result<Vec<(String, String)>, String>,
    ) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            if !d.env_vars_request.settle_ticket(ticket, Ok(())) {
                return;
            }
            match r {
                Ok(vars) => {
                    d.env_vars = vars;
                    // A delete may have shrunk the list / removed the row
                    // mid-edit.
                    d.clamp_config_cursor();
                    d.revalidate_config_edit();
                    d.clear_error(crate::mode_detail::DetailFetch::EnvVars);
                }
                Err(msg) => {
                    tracing::warn!(error = %msg, "env vars fetch failed");
                    d.set_error(crate::mode_detail::DetailFetch::EnvVars, msg);
                }
            }
        });
    }

    fn handle_option_settings_update(
        &mut self,
        pending: PendingId,
        env_name: String,
        summary: String,
        result: Result<(), String>,
    ) {
        self.complete_pending(pending, result.clone());
        match result {
            Ok(()) => {
                // Append " · press U to undo" so operators discover
                // the undo ring. Every option-settings spawn captures
                // an UndoEntry before the write, so U is reliably
                // wired here. (Skipped silently if there's no undo
                // history yet — e.g. immediately after a context
                // switch when apply_rebuild clears the ring.)
                let toast = if self.undo_history.is_empty() {
                    format!("{summary} → {env_name}")
                } else {
                    format!("{summary} → {env_name} · press U to undo")
                };
                self.push_toast(ToastKind::Success, toast);
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
        pending: PendingId,
        verb: &'static str,
        alarm_name: String,
        result: Result<(), String>,
    ) {
        self.complete_pending(pending, result.clone());
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
        pending: PendingId,
        application: String,
        region: String,
        label: String,
        force: bool,
        result: Result<(), String>,
    ) {
        let force_str = if force { " (+source bundle)" } else { "" };
        self.complete_pending(pending, result.clone());
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
                    let client = self.client_for_region(&region);
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
                        .find(|e| e.application == application && self.region_for(e) == region)
                        .filter(|e| !e.version_label.is_empty())
                        .map(|e| e.version_label.clone());
                    tokio::spawn(async move {
                        let result = match client.resolve().await {
                            Ok(aws) => aws
                                .list_application_versions(&app_name)
                                .await
                                .map_err(|e| flatten_err("list_application_versions", e)),
                            Err(e) => Err(flatten_err("cached_client", e)),
                        };
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

    fn handle_tag_update(
        &mut self,
        pending: PendingId,
        env_name: String,
        summary: String,
        result: Result<(), String>,
    ) {
        self.complete_pending(pending, result.clone());
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

    fn handle_detail_queues(
        &mut self,
        env_name: String,
        ticket: crate::app::Ticket,
        result: Result<WorkerQueues, String>,
    ) {
        self.apply_detail_msg(&env_name, result, |d, r| {
            if !d.queues_request.settle_ticket(ticket, Ok(())) {
                return;
            }
            match r {
                Ok(queues) => {
                    d.queues = queues;
                    d.clear_error(crate::mode_detail::DetailFetch::Queues);
                }
                Err(msg) => d.set_error(crate::mode_detail::DetailFetch::Queues, msg),
            }
        });
    }

    fn handle_dlq_messages(
        &mut self,
        env_name: String,
        queue_url: String,
        result: Result<Vec<QueueMessage>, String>,
    ) {
        let Some(dlq) = self.dlq.as_mut() else { return };
        if dlq.env_name != env_name {
            return;
        }
        // Drop a peek that raced an `m`-toggle: the result belongs to
        // the queue that WAS loaded, not the one now viewed.
        let current_url = match dlq.viewing {
            QueueView::Dlq => &dlq.dlq_url,
            QueueView::Main => &dlq.main_queue_url,
        };
        if &queue_url != current_url {
            return;
        }
        dlq.loading = false;
        match result {
            Ok(messages) => {
                dlq.messages = messages;
                if dlq.messages.is_empty() {
                    dlq.list_state.select(None);
                } else {
                    // Select row 0 on a fresh load too — `unwrap_or(0)`
                    // used to mask the None case, leaving Enter/x/r
                    // inert and the first `j` skipping to row 1.
                    match dlq.list_state.selected() {
                        Some(cur) if cur < dlq.messages.len() => {}
                        _ => dlq.list_state.select(Some(0)),
                    }
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
                Ok(DlqOp::Deleted { message_id }) => {
                    dlq.messages.retain(|m| m.id != message_id);
                    self.status_message = Some(format!("message {message_id} deleted"));
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
        assert_eq!(
            AppMsg::Rebuild {
                epoch: 0,
                result: Err("boom".into())
            }
            .generation(),
            None
        );
    }

    #[test]
    fn generation_extracts_gen_from_context_bound_messages() {
        assert_eq!(
            AppMsg::Refresh {
                gen: 7,
                fanout: 0,
                result: Ok(Vec::new()),
                partial_errors: Vec::new(),
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
