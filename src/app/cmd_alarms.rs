//! Alarm CRUD commands — `:alarm-create` and `:alarm-delete`. Both
//! route through `AwsClient::put_env_metric_alarm` /
//! `AwsClient::delete_alarms` and reuse the `AppMsg::AlarmOp` plumbing
//! so the pending pill closes + a toast fires on completion.
//!
//! Eighth slice of the `execute_command` split. Same parent-module
//! visibility pattern as the other `cmd_*` sub-modules.
//!
//! Read-side `:alarm-history NAME` lives here too — same alarm-namespace
//! and uses the same `cw` client, so co-locating the dispatch beats a
//! third file.

use super::{alarm_kind_to_metric, flatten_err, App, AppMsg};

impl App {
    /// `:alarm-create NAME KIND THRESHOLD [op]` — KIND maps to one
    /// of the env-scoped metrics we already chart (health / 4xx
    /// / 5xx / latency). Operator can override the comparison
    /// operator as a 4th arg; period defaults to 300s and 1
    /// evaluation period for a 5-min trigger.
    pub(crate) fn cmd_alarm_create(&mut self, rest: &[&str]) {
        let (name, kind, threshold_raw, op_override) = match (
            rest.first().copied(),
            rest.get(1).copied(),
            rest.get(2).copied(),
            rest.get(3).copied(),
        ) {
            (Some(n), Some(k), Some(t), op) => (n, k, t, op),
            _ => {
                self.error_message = Some(
                    "usage: :alarm-create NAME KIND THRESHOLD [OP]  (KIND: health|4xx|5xx|latency; OP defaults match the kind; no SNS action wired)".into(),
                );
                return;
            }
        };
        let Some((metric_name, default_op, stat)) = alarm_kind_to_metric(kind) else {
            self.error_message = Some(format!(
                "unknown KIND '{kind}'  (valid: health, 4xx, 5xx, latency)"
            ));
            return;
        };
        let Ok(threshold) = threshold_raw.parse::<f64>() else {
            self.error_message = Some(format!("threshold '{threshold_raw}' is not a number"));
            return;
        };
        let op = op_override.unwrap_or(default_op);
        let Some(env) = self.pointed_env() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&self.env_key(&env), "alarm-create") {
            return;
        }
        let env_name = env.name.clone();
        let region = self.region_for(&env);
        let alarm_name = name.to_string();
        let metric_name = metric_name.to_string();
        let op_str = op.to_string();
        let stat_str = stat.to_string();
        let target = format!("{env_name}/{alarm_name}");
        let threshold_str = threshold.to_string();
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &region,
            "AlarmCreate",
            &target,
            &[
                ("metric", metric_name.as_str()),
                ("threshold", threshold_str.as_str()),
                ("op", op_str.as_str()),
            ],
        );
        let pending = self.push_pending("Create alarm", target.clone());
        self.status_message = Some(format!(
            "creating alarm '{alarm_name}' on {env_name}/{metric_name} {op_str} {threshold}…"
        ));
        let client = self.client_for(&env);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        let alarm_for_msg = alarm_name.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => aws
                    .put_env_metric_alarm(
                        &alarm_for_msg,
                        &env_for_msg,
                        &metric_name,
                        threshold,
                        &op_str,
                        300,
                        1,
                        &stat_str,
                    )
                    .await
                    .map_err(|e| flatten_err("put_metric_alarm", e)),
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                "AlarmCreate",
                &target,
                result.as_ref().map(|_| ()).map_err(String::as_str),
                &[],
            );
            let _ = tx.send(AppMsg::AlarmOp {
                gen,
                pending,
                verb: "create",
                alarm_name: alarm_for_msg,
                result,
            });
        });
    }

    pub(crate) fn cmd_alarm_delete(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            None => {
                self.error_message = Some("usage: :alarm-delete NAME".into());
            }
            Some(name) => {
                let alarm_name = name.to_string();
                // Alarms are regional: the selected row's region, or
                // home with nothing selected.
                let env = self.pointed_env();
                let env_name = env
                    .as_ref()
                    .map(|e| e.name.clone())
                    .unwrap_or_else(|| "?".into());
                let region = match env.as_ref() {
                    Some(e) => self.region_for(e),
                    None => self.context.region.clone(),
                };
                let target = super::EnvKey::new(region.clone(), env_name.clone());
                if self.deny_write(&target, "alarm-delete") {
                    return;
                }
                let target = format!("{env_name}/{alarm_name}");
                crate::audit::append_action_dispatched(
                    self.context.account_id.as_deref(),
                    self.context.profile.as_deref(),
                    &region,
                    "AlarmDelete",
                    &target,
                    &[],
                );
                let pending = self.push_pending("Delete alarm", target.clone());
                self.status_message = Some(format!("deleting alarm '{alarm_name}'…"));
                let client = self.client_for_region(&region);
                let tx = self.msg_tx.clone();
                let gen = self.generation;
                let alarm_for_msg = alarm_name.clone();
                let account = self.context.account_id.clone();
                let profile = self.context.profile.clone();
                tokio::spawn(async move {
                    let result = match client.resolve().await {
                        Ok(aws) => aws
                            .delete_alarms(std::slice::from_ref(&alarm_for_msg))
                            .await
                            .map_err(|e| flatten_err("delete_alarms", e)),
                        Err(e) => Err(flatten_err("cached_client", e)),
                    };
                    crate::audit::append_action_completed(
                        account.as_deref(),
                        profile.as_deref(),
                        &region,
                        "AlarmDelete",
                        &target,
                        result.as_ref().map(|_| ()).map_err(String::as_str),
                        &[],
                    );
                    let _ = tx.send(AppMsg::AlarmOp {
                        gen,
                        pending,
                        verb: "delete",
                        alarm_name: alarm_for_msg,
                        result,
                    });
                });
            }
        }
    }

    /// `:alarm-history NAME` — recent transition timeline for one
    /// alarm. Fetches up to 50 history items via `DescribeAlarmHistory`,
    /// newest-first, and lands them as a TextOverlay. Read-only — no
    /// confirms / write gates needed. Operator typically gets the
    /// alarm name from `:alarms` first.
    pub(crate) fn cmd_alarm_history(&mut self, rest: &[&str]) {
        let Some(name) = rest.first().copied() else {
            self.error_message = Some(
                "usage: :alarm-history NAME  (alarm name — see :alarms for the env's alarms)"
                    .into(),
            );
            return;
        };
        let name = name.to_string();
        let client = self.current_env_client();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!("fetching alarm history for {name}…"));
        let name_for_title = name.clone();
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => aws
                    .fetch_alarm_history(&name, 50)
                    .await
                    .map_err(|e| flatten_err("fetch_alarm_history", e)),
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            let body = match result {
                Ok(items) => super::format_alarm_history(&name, &items),
                Err(e) => format!("alarm-history: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("alarm history — {name_for_title}"),
                body,
            });
        });
    }
}
