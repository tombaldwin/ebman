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

impl App {
    /// Detail-Health-tab alarms fetch. Mirrors `spawn_why_red_alarms`
    /// but lands on `AppMsg::DetailAlarms` so the result populates the
    /// Detail view's `cw_alarms` field instead of the `:why` overlay
    /// state. The Health tab + `:why` now share the *same* underlying
    /// AWS call shape but each lands on its own typed result so a stale
    /// fetch from a closed overlay can't clobber the Detail view.
    pub(super) fn spawn_detail_alarms(&mut self, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            d.loading_cw_alarms = true;
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
        self.spawn_aws(
            "list_alarms_for_env",
            move |aws| async move { aws.list_alarms_for_env(&env_name).await },
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
            d.loading_recent_versions = true;
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
        self.spawn_aws(
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
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            // We don't surface fetch errors here — failure just means we
            // can't tell whether CW Logs are configured, in which case the
            // Logs tab falls back to the generic "press ^R or s" hint.
            let groups = aws
                .discover_env_log_groups(&env_name)
                .await
                .unwrap_or_default();
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
        self.spawn_aws(
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
        self.spawn_aws(
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
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = collect_tail_logs(aws, env_name.clone(), tx.clone(), gen).await;
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
            d.error = None;
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
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let name = env_name.clone();
        tokio::spawn(async move {
            // Fire both queries concurrently; combine into one ordered series
            // list. Built-ins come first, then user metrics in add-order so
            // the operator sees their additions appended to the familiar
            // four.
            let (builtin, user) = tokio::join!(
                aws.fetch_env_metrics(&name, range),
                aws.fetch_custom_env_metrics(&name, range, &custom),
            );
            let result = match builtin {
                Ok(mut series) => {
                    if let Ok(extra) = user {
                        series.extend(extra);
                    }
                    Ok(series)
                }
                Err(e) => Err(flatten_err("fetch_env_metrics", e)),
            };
            let _ = tx.send(AppMsg::DetailMetrics {
                gen,
                env_name,
                result,
            });
        });
    }

    pub(super) fn spawn_detail_queues(&mut self, application_name: String, env_name: String) {
        if let Some(d) = self.detail.as_mut() {
            d.loading_queues = true;
            d.error = None;
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
        self.spawn_aws(
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
            d.error = None;
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
        self.spawn_aws(
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
            d.error = None;
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
        self.spawn_aws(
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
