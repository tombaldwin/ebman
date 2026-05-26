//! Miscellaneous commands — `:custom-platforms`, `:versions`,
//! `:delete-version`, `:pending`, `:resources`, `:custom-platform-delete`,
//! `:metric`. Pulled out as the final slice of the `execute_command`
//! split: cohesive enough as "read overlays + custom-metric admin" and
//! all that's left after the alarm / config / write / option / nav /
//! settings / view / overlay clusters were lifted.
//!
//! Tenth (and final) slice of the `execute_command` split. Same
//! parent-module visibility pattern as the other `cmd_*` sub-modules.

use std::time::Instant;

use super::{
    flatten_err, humanize_short_age, parse_metric_extra_args, write_audit_line, App, AppMsg,
    DetailTab, Overlay,
};

impl App {
    pub(crate) fn cmd_custom_platforms(&mut self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some("fetching custom platforms…".into());
        tokio::spawn(async move {
            let result = aws
                .list_custom_platforms()
                .await
                .map_err(|e| flatten_err("list_custom_platforms", e));
            let body = match result {
                Ok(platforms) if platforms.is_empty() => "Custom platforms: none\n\n\
                     This account hasn't built any custom EB platforms.\n\
                     `eb platform create` is the usual CLI entry.\n\nesc / q to close"
                    .to_string(),
                Ok(platforms) => {
                    let lines: Vec<String> = platforms
                        .iter()
                        .map(|p| {
                            format!(
                                "  ▸ {} v{}\n      branch: {}\n      status: {} / lifecycle: {}\n      {}",
                                if p.branch.is_empty() { "(unnamed)" } else { &p.branch },
                                p.version,
                                p.branch,
                                p.status,
                                p.lifecycle,
                                p.arn
                            )
                        })
                        .collect();
                    format!(
                        "Custom platforms ({})\n\
                         ─────────────────────\n\n\
                         {}\n\nesc / q to close",
                        platforms.len(),
                        lines.join("\n\n")
                    )
                }
                Err(e) => format!("custom platforms: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: "custom platforms".into(),
                body,
            });
        });
    }

    pub(crate) fn cmd_versions(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        let app_name = env.application.clone();
        // Capture the env's current label at dispatch time so the
        // resulting overlay can mark "this is what's deployed".
        let deployed_label = if env.version_label.is_empty() {
            None
        } else {
            Some(env.version_label.clone())
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!("fetching application versions for {app_name}…"));
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

    pub(crate) fn cmd_delete_version(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            None => {
                self.error_message = Some(
                    "usage: :delete-version <label> [--force]  (selected env's app; --force also removes the S3 source bundle)".into(),
                );
            }
            Some(label) => {
                let force = rest.iter().skip(1).any(|s| *s == "--force" || *s == "-f");
                self.spawn_delete_app_version(label.to_string(), force);
            }
        }
    }

    /// `:abort-rollback [ENV]` — explicit disarm. No arg drains
    /// every armed watchdog in the current context; with an env
    /// name, just that one. Audit-logged so a post-mortem can pin
    /// down "operator aborted the rollback at HH:MM" even if the
    /// auto-rollback never fired.
    ///
    /// The fire-and-forget tokio task that backs each watchdog
    /// survives the abort — no JoinHandle for cancellation — but
    /// `apply_refresh`'s decision pass will find the slot empty and
    /// no-op when the deadline message lands. So aborts are
    /// genuinely synchronous from the operator's perspective.
    ///
    /// Not gated by `deny_write`: aborting a rollback is a
    /// "clean up state I previously armed" action, not a write to
    /// AWS. Per-env safety pins added mid-window must not block the
    /// operator from clearing the watchdog they themselves armed.
    pub(crate) fn cmd_abort_rollback(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            Some(env_name) => {
                if self.armed_watchdogs.remove(env_name).is_some() {
                    write_audit_line(
                        self.context.account_id.as_deref(),
                        self.context.profile.as_deref(),
                        &self.context.region,
                        &format!("stage=dispatched action=AbortRollback target={env_name}"),
                    );
                    self.pin_status(format!("aborted auto-rollback for {env_name}"));
                } else {
                    self.error_message = Some(format!(
                        "no auto-rollback armed for '{env_name}' — try :rollbacks-armed"
                    ));
                }
            }
            None => {
                if self.armed_watchdogs.is_empty() {
                    self.pin_status("no auto-rollbacks armed to abort");
                    return;
                }
                let names: Vec<String> = self.armed_watchdogs.keys().cloned().collect();
                let n = names.len();
                for env_name in &names {
                    write_audit_line(
                        self.context.account_id.as_deref(),
                        self.context.profile.as_deref(),
                        &self.context.region,
                        &format!(
                            "stage=dispatched action=AbortRollback target={env_name} reason=batch"
                        ),
                    );
                }
                self.armed_watchdogs.clear();
                self.pin_status(format!(
                    "aborted {n} auto-rollback{}: {}",
                    if n == 1 { "" } else { "s" },
                    names.join(", ")
                ));
            }
        }
    }

    /// `:rollbacks-armed` (alias `:rb-armed`) — dump the table of
    /// currently-armed `--auto-rollback` watchdogs. Each row shows
    /// env / target_label / armed_at age / remaining-until-deadline.
    /// Updates every refresh tick because the overlay re-renders
    /// from `App.armed_watchdogs` every draw. Empty state yields a
    /// status toast rather than a thin overlay.
    pub(crate) fn cmd_rollbacks_armed(&mut self) {
        if self.armed_watchdogs.is_empty() {
            self.pin_status(
                "no auto-rollbacks armed — `:deploy LABEL --auto-rollback Nm` arms one",
            );
            return;
        }
        let body = super::format_armed_rollbacks(&self.armed_watchdogs, chrono::Utc::now());
        self.current_overlay = Some(Overlay::TextDump {
            title: format!("auto-rollbacks armed ({})", self.armed_watchdogs.len()),
            body,
        });
    }

    /// `:freeze-deploys [reason]` — set a session-scoped fleet-wide
    /// write-lock. Any destructive action against any env refuses
    /// while the lock is on, with the reason surfaced in the toast.
    /// Cleared by `:thaw-deploys` or by exiting ebman.
    ///
    /// Re-issuing the command while frozen replaces the reason
    /// (operators sometimes refine "rolling back" → "rolling back,
    /// PROD only" mid-incident — letting them update the message
    /// without thaw + refreeze is the obvious shape).
    pub(crate) fn cmd_freeze_deploys(&mut self, rest: &[&str]) {
        let reason = rest.join(" ");
        let trimmed = reason.trim();
        let reason_for_store = trimmed.to_string();
        let was_frozen = self.deploy_freeze.is_some();
        self.deploy_freeze = Some(crate::app::DeployFreeze {
            reason: reason_for_store.clone(),
            frozen_at: chrono::Utc::now(),
        });
        let audit_reason = if reason_for_store.is_empty() {
            "no-reason".to_string()
        } else {
            reason_for_store.clone()
        };
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!("stage=dispatched action=FreezeDeploys reason={audit_reason}"),
        );
        let verb = if was_frozen { "updated" } else { "set" };
        self.pin_status(if reason_for_store.is_empty() {
            format!("freeze {verb}: deploys + writes blocked until :thaw-deploys")
        } else {
            format!("freeze {verb}: deploys + writes blocked — reason: {reason_for_store}")
        });
    }

    /// `:thaw-deploys` — clear the session-scoped freeze. No-op
    /// (status toast) if no freeze was active. Audit-logged either
    /// way so the audit stream captures the lifecycle.
    pub(crate) fn cmd_thaw_deploys(&mut self) {
        let was_frozen = self.deploy_freeze.take().is_some();
        write_audit_line(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!("stage=dispatched action=ThawDeploys was_frozen={was_frozen}"),
        );
        if was_frozen {
            self.pin_status("freeze cleared — deploys + writes re-enabled");
        } else {
            self.pin_status("no freeze active — nothing to thaw");
        }
    }

    pub(crate) fn cmd_pending(&mut self) {
        if self.pending_actions.is_empty() {
            self.pin_status("no actions in flight or recently completed");
        } else {
            let now = Instant::now();
            let mut lines: Vec<String> = Vec::with_capacity(self.pending_actions.len() + 2);
            for entry in self.pending_actions.iter().rev() {
                let age = humanize_short_age(now.duration_since(entry.started));
                let status = match &entry.completed {
                    None => " ⏳ in flight".to_string(),
                    Some((c, Ok(()))) => {
                        format!(" ✓ ok ({} ago)", humanize_short_age(now.duration_since(*c)))
                    }
                    Some((c, Err(e))) => format!(
                        " ✗ err ({} ago): {}",
                        humanize_short_age(now.duration_since(*c)),
                        e.chars().take(80).collect::<String>()
                    ),
                };
                lines.push(format!(
                    "  {} → {}  ({} ago){}",
                    entry.label, entry.target, age, status
                ));
            }
            self.current_overlay = Some(Overlay::TextDump {
                title: "in-flight + recently-completed actions".into(),
                body: lines.join("\n"),
            });
        }
    }

    pub(crate) fn cmd_resources(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_name = env.name.clone();
        let tier = env.tier.clone();
        self.status_message = Some(format!("fetching env resources for {env_name}…"));
        let env_name_for_title = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .describe_env_resources(&env_name)
                .await
                .map_err(|e| flatten_err("describe_env_resources", e));
            let body = match result {
                Ok(res) => super::render_env_resources_tree(&res, &env_name, &tier),
                Err(e) => format!("resources: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("resources — {env_name_for_title}"),
                body,
            });
        });
    }

    pub(crate) fn cmd_custom_platform_delete(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            None => {
                self.error_message = Some(
                    "usage: :custom-platform-delete <platform-arn>  (fails if any env still uses it)".into(),
                );
            }
            Some(arn) => {
                // Custom platforms are account-scoped, not env-scoped —
                // an empty env name in deny_write fires the global /
                // account pin but doesn't match any per-env entry.
                if self.deny_write("", "custom-platform-delete") {
                    return;
                }
                let arn = arn.to_string();
                write_audit_line(
                    self.context.account_id.as_deref(),
                    self.context.profile.as_deref(),
                    &self.context.region,
                    &format!("stage=dispatched action=DeleteCustomPlatform target={arn}"),
                );
                self.push_pending("Delete custom platform", arn.clone());
                // In-flight ack lives on the pending pill.
                let aws = self.aws.clone();
                let tx = self.msg_tx.clone();
                let gen = self.generation;
                let arn_for_msg = arn.clone();
                let account = self.context.account_id.clone();
                let profile = self.context.profile.clone();
                let region = self.context.region.clone();
                tokio::spawn(async move {
                    let result = aws
                        .delete_custom_platform(&arn_for_msg)
                        .await
                        .map_err(|e| flatten_err("delete_custom_platform", e));
                    let outcome = match &result {
                        Ok(()) => format!(
                            "stage=completed action=DeleteCustomPlatform target={arn_for_msg} ok"
                        ),
                        Err(e) => format!(
                            "stage=completed action=DeleteCustomPlatform target={arn_for_msg} err=\"{}\"",
                            e.replace('"', "'")
                        ),
                    };
                    write_audit_line(account.as_deref(), profile.as_deref(), &region, &outcome);
                    // Reuse OptionSettingsUpdate's plumbing so the pending
                    // row is closed and a toast fires — the variant's
                    // shape (env_name + summary) maps cleanly to
                    // (target_arn + summary).
                    let _ = tx.send(AppMsg::OptionSettingsUpdate {
                        gen,
                        env_name: arn_for_msg,
                        summary: "Delete custom platform".into(),
                        result,
                    });
                });
            }
        }
    }

    /// `:metric add LABEL NAMESPACE NAME [STAT]` upserts a custom
    /// metric chart for the Metrics tab; `:metric remove LABEL`
    /// drops it; `:metric list` dumps the table. STAT defaults to
    /// Average. Persists to state.toml automatically via
    /// `persist_state`.
    pub(crate) fn cmd_metric(&mut self, rest: &[&str]) {
        let sub = rest.first().copied();
        match sub {
            Some("list") | Some("ls") | None => {
                if self.custom_metrics.is_empty() {
                    self.status_message = Some(
                        "no custom metrics — add with `:metric add LABEL NAMESPACE NAME [STAT]`"
                            .into(),
                    );
                } else {
                    let mut lines = String::new();
                    for (label, spec) in &self.custom_metrics {
                        lines.push_str(&format!(
                            "{label:<24}  {:<32}  {:<32}  {}\n",
                            spec.namespace, spec.name, spec.stat
                        ));
                    }
                    self.current_overlay = Some(Overlay::TextDump {
                        title: format!("custom metrics ({} total)", self.custom_metrics.len()),
                        body: lines,
                    });
                }
            }
            Some("add") => match (
                rest.get(1).copied(),
                rest.get(2).copied(),
                rest.get(3).copied(),
            ) {
                (Some(label), Some(namespace), Some(name)) => {
                    // Args after NAME are STAT and/or DIMS in any order.
                    // The token containing `=` is dims (e.g.
                    // `InstanceId=i-abc,Foo=bar`); the other is stat.
                    // STAT defaults to Average; DIMS defaults to the
                    // env-scoped dimension (resolved at fetch time).
                    let (stat, dimensions) = parse_metric_extra_args(&rest[4..]);
                    self.custom_metrics.insert(
                        label.to_string(),
                        crate::state::CustomMetricSpec {
                            namespace: namespace.to_string(),
                            name: name.to_string(),
                            stat,
                            dimensions,
                        },
                    );
                    self.persist_state();
                    self.status_message = Some(format!(
                        "custom metric '{label}' added — re-open Detail/Metrics to see"
                    ));
                    // If we're on the Metrics tab, refetch so the
                    // chart appears without the user toggling tabs.
                    if let Some(d) = self.detail.as_ref() {
                        if d.tab() == DetailTab::Metrics {
                            let env_name = d.env_name.clone();
                            self.spawn_detail_metrics(env_name);
                        }
                    }
                }
                _ => {
                    self.error_message = Some(
                        "usage: :metric add LABEL NAMESPACE NAME [STAT] [DIM=VAL,DIM=VAL]  (dimensions default to EnvironmentName=<env>; pass overrides for AWS/EC2 InstanceId, AWS/ApplicationELB LoadBalancer, etc.)".into(),
                    );
                }
            },
            Some("remove") | Some("rm") | Some("delete") => match rest.get(1).copied() {
                None => {
                    self.error_message = Some("usage: :metric remove LABEL".into());
                }
                Some(label) => {
                    if self.custom_metrics.remove(label).is_some() {
                        self.persist_state();
                        self.status_message = Some(format!("custom metric '{label}' removed"));
                        if let Some(d) = self.detail.as_ref() {
                            if d.tab() == DetailTab::Metrics {
                                let env_name = d.env_name.clone();
                                self.spawn_detail_metrics(env_name);
                            }
                        }
                    } else {
                        self.error_message = Some(format!("no custom metric named '{label}'"));
                    }
                }
            },
            Some(other) => {
                self.error_message = Some(format!(
                    "unknown subcommand '{other}'  (use: list | add LABEL NS NAME [STAT] | remove LABEL)"
                ));
            }
        }
    }
}
