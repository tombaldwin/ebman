//! Detail-view background fetches. Ten `spawn_detail_*` helpers, one per
//! lazily-loaded slice of the per-env Detail view: the Health tab's
//! alarms + recent-versions, the Logs tab's log-group discovery + log
//! tail, the Config tab's env-vars + tags, the Metrics tab series, the
//! Queue tab's queue stats, the Events tab, and the Instances tab.
//!
//! Each is fired when the operator opens the relevant tab (or on Detail
//! open for the eager ones), routes its result through a typed
//! `AppMsg::Detail*` variant, and the handler in `msg.rs` writes it into
//! the active `DetailState`. Several carry a `--demo` short-circuit that
//! injects `demo_fixture` data so VHS recordings show a populated view
//! instead of stub AWS errors.
//!
//! 0.21+ lift: cluster gathered out of `src/app.rs` (where the ten
//! methods were scattered across six locations) as the final step of the
//! `spawn_*` clusters refactor. Pure relocation; fn → pub(super) for the
//! app-module callers. No behaviour change.

use super::{collect_tail_logs, flatten_err, App, AppMsg, LogTailStage};

/// The Metrics tab's two fetches as one result: the series, and what
/// the custom-metric fetch said if it failed. Its failure used to be
/// dropped (`if let Ok`), so `:metric add`ed series vanished with
/// nothing on screen saying the fetch had failed; now the built-ins
/// still draw and the failure is shown beside them.
pub(crate) type MetricsResult = Result<(Vec<crate::aws::MetricSeries>, Option<String>), String>;

pub(crate) fn merge_metrics(
    builtin: Result<Vec<crate::aws::MetricSeries>, String>,
    custom: Result<Vec<crate::aws::MetricSeries>, String>,
) -> MetricsResult {
    let mut series = builtin?;
    match custom {
        Ok(extra) => {
            series.extend(extra);
            Ok((series, None))
        }
        Err(e) => Ok((series, Some(format!("custom metrics: {e}")))),
    }
}

impl App {
    /// Detail-Health-tab alarms fetch. Mirrors `spawn_why_red_alarms`
    /// but lands on `AppMsg::DetailAlarms` so the result populates the
    /// Detail view's `cw_alarms` field instead of the `:why` overlay
    /// state. The Health tab + `:why` now share the *same* underlying
    /// AWS call shape but each lands on its own typed result so a stale
    /// fetch from a closed overlay can't clobber the Detail view.
    pub(super) fn spawn_detail_alarms(&mut self, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            d.cw_alarms.begin();
        }
        // Demo-mode short-circuit (same pattern as
        // spawn_detail_instances / spawn_detail_events). Inject
        // fixture alarms so Detail/Health doesn't show an
        // ugly "error: DescribeAlarms failed" row.
        if self.demo_mode {
            let result = Ok(crate::demo_fixture::alarms_for_env(&env_name));
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::DetailAlarms {
                gen,
                env_name,
                result,
            });
            return;
        }
        let env_for_msg = env_name.clone();
        let matching = self.alarm_match_for(&env_name);
        self.spawn_aws_in(
            self.detail_client(),
            "list_alarms_for_env",
            move |aws| async move { aws.list_alarms_for_env(&env_name, &matching).await },
            move |gen, result| AppMsg::DetailAlarms {
                gen,
                env_name: env_for_msg,
                result,
            },
        );
    }

    /// Detail-Health-tab recent-versions fetch. Same shape as
    /// `spawn_why_red_deploys` but lands on `AppMsg::DetailRecentVersions`.
    pub(super) fn spawn_detail_recent_versions(&mut self, app_name: String, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            d.recent_versions.begin();
        }
        if self.demo_mode {
            let result = Ok(crate::demo_fixture::deploys_for_app(&app_name));
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::DetailRecentVersions {
                gen,
                env_name,
                result,
            });
            return;
        }
        self.spawn_aws_in(
            self.detail_client(),
            "list_application_versions",
            move |aws| async move { aws.list_application_versions(&app_name).await },
            move |gen, result| AppMsg::DetailRecentVersions {
                gen,
                env_name,
                result,
            },
        );
    }

    pub(super) fn spawn_detail_log_groups(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let env_name = d.env_name.clone();
        let client = self.detail_client();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            // We don't surface fetch errors here — failure just means we
            // can't tell whether CW Logs are configured, in which case the
            // Logs tab falls back to the generic "press ^R or s" hint.
            // A client that won't resolve is the same non-answer.
            let groups = match client.resolve().await {
                Ok(aws) => aws
                    .discover_env_log_groups(&env_name)
                    .await
                    .unwrap_or_default(),
                Err(_) => Vec::new(),
            };
            let _ = tx.send(AppMsg::DetailLogGroups {
                gen,
                env_name,
                groups,
            });
        });
    }

    pub(super) fn spawn_detail_env_vars(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let app_name = d.env_snapshot.application.clone();
        let env_name = d.env_name.clone();
        if let Some(d) = self.detail.as_mut() {
            d.loading_env_vars = true;
        }
        let env_for_msg = env_name.clone();
        self.spawn_aws_in(
            self.detail_client(),
            "fetch_env_vars",
            move |aws| async move { aws.fetch_env_vars(&app_name, &env_name).await },
            move |gen, result| AppMsg::DetailEnvVars {
                gen,
                env_name: env_for_msg,
                result,
            },
        );
    }

    pub(super) fn spawn_detail_tags(&mut self) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(arn) = d.env_snapshot.arn.clone() else {
            return;
        };
        let env_name = d.env_name.clone();
        if let Some(d) = self.detail.as_mut() {
            d.loading_tags = true;
        }
        self.spawn_aws_in(
            self.detail_client(),
            "list_tags",
            move |aws| async move { aws.list_tags(&arn).await },
            move |gen, result| AppMsg::DetailTags {
                gen,
                env_name,
                result,
            },
        );
    }

    pub(super) fn spawn_detail_logs(&mut self, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            // Re-entering an in-flight tail is a refresh; reset state. Existing
            // content is retained until the new fetch lands so the user keeps
            // seeing the previous tail rather than a blank screen.
            d.log_tail.stage = LogTailStage::Requesting;
            d.log_tail.poll_attempt = 0;
            d.log_tail.error = None;
        }
        let client = self.detail_client();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => collect_tail_logs(aws, env_name.clone(), tx.clone(), gen).await,
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            let _ = tx.send(AppMsg::DetailLogs {
                gen,
                env_name: env_for_msg,
                result,
            });
        });
    }

    pub(super) fn spawn_detail_metrics(&mut self, env_name: String) {
        let range = self
            .detail
            .as_ref()
            .map(|d| d.metrics_range_secs)
            .unwrap_or(3600);
        if let Some(d) = self.detail.as_mut() {
            d.loading_metrics = true;
            // Only its own: a sibling's failure stays on screen.
            d.clear_error(crate::mode_detail::DetailFetch::Metrics);
        }
        // Snapshot the custom-metrics spec list at spawn time so concurrent
        // `:metric add`s don't race with the in-flight fetch.
        let custom: Vec<crate::aws::CustomMetricQuery> = self
            .custom_metrics
            .iter()
            .map(|(label, spec)| {
                (
                    label.clone(),
                    spec.namespace.clone(),
                    spec.name.clone(),
                    spec.stat.clone(),
                    spec.dimensions.clone(),
                )
            })
            .collect();
        let client = self.detail_client();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let name = env_name.clone();
        tokio::spawn(async move {
            let aws = match client.resolve().await {
                Ok(aws) => aws,
                Err(e) => {
                    let _ = tx.send(AppMsg::DetailMetrics {
                        gen,
                        env_name,
                        range_secs: range,
                        result: Err(flatten_err("cached_client", e)),
                    });
                    return;
                }
            };
            // Fire both queries concurrently; combine into one ordered series
            // list. Built-ins come first, then user metrics in add-order so
            // the operator sees their additions appended to the familiar
            // four.
            let (builtin, user) = tokio::join!(
                aws.fetch_env_metrics(&name, range),
                aws.fetch_custom_env_metrics(&name, range, &custom),
            );
            let result = merge_metrics(
                builtin.map_err(|e| flatten_err("fetch_env_metrics", e)),
                user.map_err(|e| flatten_err("fetch_custom_env_metrics", e)),
            );
            let _ = tx.send(AppMsg::DetailMetrics {
                gen,
                env_name,
                range_secs: range,
                result,
            });
        });
    }

    /// The App tab's series. An env with no `app.ENV.*` config, or demo
    /// mode, settles at once with nothing to draw rather than calling
    /// AWS: the tab explains what to configure.
    pub(super) fn spawn_detail_app(&mut self, env_name: String) {
        let range = self
            .detail
            .as_ref()
            .map(|d| d.metrics_range_secs)
            .unwrap_or(3600);
        let ticket = self
            .detail
            .as_mut()
            .map(|d| d.app_metrics.begin())
            .unwrap_or_default();
        // No config: preview what discovery found evidence for, if any.
        let spec = self.app_tab_source(&env_name).spec();
        // In demo mode: the configured rows, empty, so the tab still
        // shows what is configured and says why there are no values.
        let demo_rows = spec
            .as_ref()
            .filter(|_| self.demo_mode)
            .map(crate::app_tab::demo_series);
        let Some(spec) = spec.filter(|_| demo_rows.is_none()) else {
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::DetailAppMetrics {
                gen,
                env_name,
                ticket,
                result: Ok(demo_rows.unwrap_or_default()),
            });
            return;
        };
        let env_for_msg = env_name.clone();
        self.spawn_aws_in(
            self.detail_client(),
            "fetch_app_metrics",
            move |aws| async move { aws.fetch_app_metrics(&env_name, range, &spec).await },
            move |gen, result| AppMsg::DetailAppMetrics {
                gen,
                env_name: env_for_msg,
                ticket,
                result,
            },
        );
    }

    /// The App tab's top errors: a Logs Insights query over the env's
    /// `app.ENV.log_group`. Opt-in by that key, because a query is billed
    /// per GB scanned; never stacked on one still running; never in
    /// demo mode.
    pub(super) fn spawn_detail_top_errors(&mut self, env_name: String) {
        let Some(group) = self
            .cfg
            .app_tabs
            .envs
            .get(&env_name)
            .and_then(|s| s.log_group.as_deref())
            .map(|g| crate::app_tab::top_errors_log_group(&env_name, g))
        else {
            return;
        };
        let Some(d) = self.detail.as_mut() else {
            return;
        };
        // In flight blocks a second query, unless it is older than any
        // answer could be (the query is bounded below): a task that
        // died without answering must not leave `^R` doing nothing.
        let stuck_after = crate::app_tab::TOP_ERRORS_BOUND + std::time::Duration::from_secs(30);
        let running = d.top_errors.in_flight()
            && d.top_errors_started
                .is_some_and(|t| t.elapsed() < stuck_after);
        if running || self.demo_mode {
            return;
        }
        let ticket = d.top_errors.begin();
        d.top_errors_started = Some(std::time::Instant::now());
        let fingerprint = self
            .cfg
            .app_tabs
            .envs
            .get(&env_name)
            .and_then(|s| s.error_fingerprint.clone());
        let env_for_msg = env_name.clone();
        self.spawn_aws_in(
            self.detail_client(),
            "run_insights_query",
            move |aws| async move {
                aws.fetch_top_errors(
                    group,
                    fingerprint.as_deref(),
                    crate::app_tab::TOP_ERRORS_BOUND,
                )
                .await
            },
            move |gen, result| AppMsg::DetailTopErrors {
                gen,
                env_name: env_for_msg,
                ticket,
                result,
            },
        );
    }

    pub(super) fn spawn_detail_queues(&mut self, application_name: String, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            d.loading_queues = true;
            // Only its own: a sibling's failure stays on screen.
            d.clear_error(crate::mode_detail::DetailFetch::Queues);
        }
        if self.demo_mode {
            let result = Ok(crate::demo_fixture::worker_queues_for_env(&env_name));
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::DetailQueues {
                gen,
                env_name,
                result,
            });
            return;
        }
        let env_for_msg = env_name.clone();
        self.spawn_aws_in(
            self.detail_client(),
            "describe_worker_queues",
            move |aws| async move {
                aws.describe_worker_queues(&application_name, &env_name)
                    .await
            },
            move |gen, result| AppMsg::DetailQueues {
                gen,
                env_name: env_for_msg,
                result,
            },
        );
    }

    pub(super) fn spawn_detail_events(&mut self, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            d.loading_events = true;
            // Only its own: a sibling's failure stays on screen.
            d.clear_error(crate::mode_detail::DetailFetch::Events);
        }
        // Demo-mode short-circuit: filter the fixture's fleet-wide
        // events down to this env, mirror what list_events_for_env
        // would have returned. Same channel + msg variant the live
        // path uses, so the rest of the rendering pipeline is
        // untouched.
        if self.demo_mode {
            let result = Ok(crate::demo_fixture::events_for_env(&env_name));
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::DetailEvents {
                gen,
                env_name,
                result,
            });
            return;
        }
        let env_for_msg = env_name.clone();
        self.spawn_aws_in(
            self.detail_client(),
            "list_events_for_env",
            move |aws| async move { aws.list_events_for_env(&env_name, 50).await },
            move |gen, result| AppMsg::DetailEvents {
                gen,
                env_name: env_for_msg,
                result,
            },
        );
    }

    pub(super) fn spawn_detail_instances(&mut self, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            d.loading_instances = true;
            // Only its own: a sibling's failure stays on screen.
            d.clear_error(crate::mode_detail::DetailFetch::Instances);
        }
        // Demo-mode short-circuit: synthesise the fixture's per-env
        // instance list and send the result message directly. Avoids
        // firing list_instances against the stub AwsClient, which
        // would error and leave Detail/Instances stuck on "loading…".
        if self.demo_mode {
            let result = Ok(crate::demo_fixture::instances_for(&env_name));
            let gen = self.generation;
            let _ = self.msg_tx.send(AppMsg::DetailInstances {
                gen,
                env_name,
                result,
            });
            return;
        }
        let env_for_msg = env_name.clone();
        self.spawn_aws_in(
            self.detail_client(),
            "list_instances",
            move |aws| async move { aws.list_instances(&env_name).await },
            move |gen, result| AppMsg::DetailInstances {
                gen,
                env_name: env_for_msg,
                result,
            },
        );
    }
}
