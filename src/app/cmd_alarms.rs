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
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, "alarm-create") {
            return;
        }
        let env_name = env.name.clone();
        let alarm_name = name.to_string();
        let metric_name = metric_name.to_string();
        let op_str = op.to_string();
        let stat_str = stat.to_string();
        let target = format!("{env_name}/{alarm_name}");
        crate::audit::append_raw(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &format!(
                "stage=dispatched action=AlarmCreate target={target} metric={metric_name} threshold={threshold} op={op_str}"
            ),
        );
        self.push_pending("Create alarm", target.clone());
        self.status_message = Some(format!(
            "creating alarm '{alarm_name}' on {env_name}/{metric_name} {op_str} {threshold}…"
        ));
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        let alarm_for_msg = alarm_name.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        tokio::spawn(async move {
            let result = aws
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
                .map_err(|e| flatten_err("put_metric_alarm", e));
            let outcome = match &result {
                Ok(()) => format!("stage=completed action=AlarmCreate target={target} ok"),
                Err(e) => format!(
                    "stage=completed action=AlarmCreate target={target} err=\"{}\"",
                    e.replace('"', "'")
                ),
            };
            crate::audit::append_raw(account.as_deref(), profile.as_deref(), &region, &outcome);
            let _ = tx.send(AppMsg::AlarmOp {
                gen,
                verb: "create",
                alarm_name: alarm_for_msg,
                env_name: env_for_msg,
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
                let env_name = self
                    .selected_env()
                    .map(|e| e.name.clone())
                    .unwrap_or_else(|| "?".into());
                if self.deny_write(&env_name, "alarm-delete") {
                    return;
                }
                let target = format!("{env_name}/{alarm_name}");
                crate::audit::append_raw(
                    self.context.account_id.as_deref(),
                    self.context.profile.as_deref(),
                    &self.context.region,
                    &format!("stage=dispatched action=AlarmDelete target={target}"),
                );
                self.push_pending("Delete alarm", target.clone());
                self.status_message = Some(format!("deleting alarm '{alarm_name}'…"));
                let aws = self.aws.clone();
                let tx = self.msg_tx.clone();
                let gen = self.generation;
                let alarm_for_msg = alarm_name.clone();
                let env_for_msg = env_name.clone();
                let account = self.context.account_id.clone();
                let profile = self.context.profile.clone();
                let region = self.context.region.clone();
                tokio::spawn(async move {
                    let result = aws
                        .delete_alarms(std::slice::from_ref(&alarm_for_msg))
                        .await
                        .map_err(|e| flatten_err("delete_alarms", e));
                    let outcome = match &result {
                        Ok(()) => {
                            format!("stage=completed action=AlarmDelete target={target} ok")
                        }
                        Err(e) => format!(
                            "stage=completed action=AlarmDelete target={target} err=\"{}\"",
                            e.replace('"', "'")
                        ),
                    };
                    crate::audit::append_raw(
                        account.as_deref(),
                        profile.as_deref(),
                        &region,
                        &outcome,
                    );
                    let _ = tx.send(AppMsg::AlarmOp {
                        gen,
                        verb: "delete",
                        alarm_name: alarm_for_msg,
                        env_name: env_for_msg,
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
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!("fetching alarm history for {name}…"));
        let name_for_title = name.clone();
        tokio::spawn(async move {
            let result = aws
                .fetch_alarm_history(&name, 50)
                .await
                .map_err(|e| flatten_err("fetch_alarm_history", e));
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
