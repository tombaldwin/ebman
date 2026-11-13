//! `:dlq-redrive` — move a worker env's dead-letter queue back to its
//! source queue with SQS's message move task: the messages never leave
//! AWS, however many there are (the DLQ viewer resends ten at a time,
//! through ebman). `status` shows the running and recent tasks, and
//! `cancel` stops the running one.

use super::*;

/// The worker env's two queues, as their ARNs: the dead-letter queue
/// the task drains, and the source queue it fills.
/// `starting` refuses an empty dead-letter queue, as MCP's plan does.
async fn redrive_arns(
    aws: &crate::aws::AwsClient,
    app_name: &str,
    env_name: &str,
    starting: bool,
) -> Result<(String, String), String> {
    let queues = aws
        .describe_worker_queues(app_name, env_name)
        .await
        .map_err(|e| flatten_err("describe_worker_queues", e))?;
    let (dlq, main) = crate::aws::redrive_queues(env_name, &queues, starting)?;
    let dlq_arn = aws
        .queue_arn(&dlq)
        .await
        .map_err(|e| flatten_err("queue_arn", e))?;
    let main_arn = aws
        .queue_arn(&main)
        .await
        .map_err(|e| flatten_err("queue_arn", e))?;
    Ok((dlq_arn, main_arn))
}

impl App {
    /// `:dlq-redrive [status|cancel]` on the selected worker env.
    pub(crate) fn cmd_dlq_redrive(&mut self, rest: &[&str]) {
        let Some(env) = self.pointed_env() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if let Err(e) = crate::aws::redrive_tier(&env) {
            self.error_message = Some(e);
            return;
        }
        match rest.first().copied() {
            None => self.start_redrive(&env),
            Some("status") => self.redrive_status(&env),
            Some("cancel") => self.cancel_redrive(&env),
            Some(other) => {
                self.error_message = Some(format!(
                    "unknown '{other}' — usage: :dlq-redrive [status|cancel]"
                ))
            }
        }
    }

    fn start_redrive(&mut self, env: &crate::aws::Environment) {
        let key = self.env_key(env);
        if self.deny_write(&key, "dlq-redrive") {
            return;
        }
        let label = crate::verb::Verb::DlqRedrive.audit_label();
        self.spawn_redrive_write(env, label, "Redrive DLQ", true, |aws, dlq_arn, main_arn| {
            Box::pin(async move {
                let handle = aws
                    .start_redrive(&dlq_arn, &main_arn)
                    .await
                    .map_err(|e| flatten_err("start_redrive", e))?;
                Ok((
                    handle,
                    "redrive started — every dead-lettered message goes back to the worker \
                     queue to be processed again; `:dlq-redrive status` follows it"
                        .to_string(),
                ))
            })
        });
    }

    fn cancel_redrive(&mut self, env: &crate::aws::Environment) {
        let key = self.env_key(env);
        if self.deny_write(&key, "dlq-redrive cancel") {
            return;
        }
        self.spawn_redrive_write(
            env,
            "dlq-redrive-cancel",
            "Cancel DLQ redrive",
            false,
            |aws, dlq_arn, _| {
                Box::pin(async move {
                    let tasks = aws
                        .list_redrives(&dlq_arn)
                        .await
                        .map_err(|e| flatten_err("list_redrives", e))?;
                    let Some(running) = tasks.into_iter().find(|t| t.in_progress()) else {
                        return Err("no redrive is running on this queue".to_string());
                    };
                    let moved = aws
                        .cancel_redrive(&running.handle)
                        .await
                        .map_err(|e| flatten_err("cancel_redrive", e))?;
                    Ok((
                        running.handle,
                        format!("redrive cancelled after about {moved} messages were moved"),
                    ))
                })
            },
        );
    }

    /// The shared shape of both redrive writes: audited dispatch and
    /// completion naming the task, a pending pill, and the result as a
    /// toast. `op` gets the client and the two queue ARNs and answers
    /// `(task handle, what to tell the operator)`.
    fn spawn_redrive_write(
        &mut self,
        env: &crate::aws::Environment,
        label: &'static str,
        pending_label: &'static str,
        starting: bool,
        op: impl FnOnce(
                std::sync::Arc<crate::aws::AwsClient>,
                String,
                String,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = Result<(String, String), String>> + Send>,
            > + Send
            + 'static,
    ) {
        let EnvKey {
            region,
            name: env_name,
        } = self.env_key(env);
        let app_name = env.application.clone();
        let pending = self.push_pending(pending_label, env_name.clone());
        let client = self.client_for_region(&region);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        tokio::spawn(async move {
            // The queues first: an env with none, or an empty dead-letter
            // queue to start from, is refused before anything is audited
            // as dispatched, as MCP refuses it at plan time.
            let checked = match client.resolve().await {
                Err(e) => Err(flatten_err("cached_client", e)),
                Ok(aws) => redrive_arns(&aws, &app_name, &env_name, starting)
                    .await
                    .map(|arns| (aws, arns)),
            };
            // SQS refuses a second task on a queue; saying which one is
            // running beats its bare error, and saying it before the
            // dispatched line keeps a refused start out of the log as MCP's
            // plan does.
            let checked = match checked {
                Ok((aws, arns)) if starting => match aws.list_redrives(&arns.0).await {
                    Err(e) => Err(flatten_err("list_redrives", e)),
                    Ok(tasks) => match tasks.iter().find(|t| t.in_progress()) {
                        Some(t) => Err(format!(
                            "a redrive is already {} ({} moved) — `:dlq-redrive cancel` stops it",
                            t.status.to_lowercase(),
                            t.moved
                        )),
                        None => Ok((aws, arns)),
                    },
                },
                other => other,
            };
            let (aws, (dlq_arn, main_arn)) = match checked {
                Ok(ready) => ready,
                Err(e) => {
                    let _ = tx.send(AppMsg::RedriveDone {
                        gen,
                        pending,
                        env_name,
                        result: Err(e),
                    });
                    return;
                }
            };
            crate::audit::append_action_dispatched(
                account.as_deref(),
                profile.as_deref(),
                &region,
                label,
                &env_name,
                &[],
            );
            let result = op(aws, dlq_arn, main_arn).await;
            let task = result.as_ref().map(|(h, _)| h.clone()).unwrap_or_default();
            let extras: Vec<(&str, &str)> = if task.is_empty() {
                Vec::new()
            } else {
                vec![("task", task.as_str())]
            };
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                label,
                &env_name,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &extras,
            );
            let _ = tx.send(AppMsg::RedriveDone {
                gen,
                pending,
                env_name,
                result: result.map(|(_, said)| said),
            });
        });
    }

    pub(super) fn handle_redrive_done(
        &mut self,
        pending: PendingId,
        env_name: String,
        result: Result<String, String>,
    ) {
        self.complete_pending(pending, result.as_ref().map(|_| ()).map_err(Clone::clone));
        match result {
            Ok(said) => self.push_toast(ToastKind::Info, format!("{env_name}: {said}")),
            Err(e) => self.push_toast(ToastKind::Error, format!("{env_name}: {e}")),
        }
    }

    /// `:dlq-redrive status` — the running and recent move tasks off the
    /// env's dead-letter queue, in a text overlay.
    fn redrive_status(&mut self, env: &crate::aws::Environment) {
        let env_name = env.name.clone();
        let app_name = env.application.clone();
        let title = format!("DLQ redrives — {env_name}");
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        if self.demo_mode {
            let body = format_redrives(&Ok(Vec::new()), chrono::Utc::now());
            let _ = tx.send(AppMsg::TextOverlay { gen, title, body });
            return;
        }
        let client = self.client_for(env);
        tokio::spawn(async move {
            let tasks = match client.resolve().await {
                Err(e) => Err(flatten_err("cached_client", e)),
                Ok(aws) => match redrive_arns(&aws, &app_name, &env_name, false).await {
                    Err(e) => Err(e),
                    Ok((dlq_arn, _)) => aws
                        .list_redrives(&dlq_arn)
                        .await
                        .map_err(|e| flatten_err("list_redrives", e)),
                },
            };
            let body = format_redrives(&tasks, chrono::Utc::now());
            let _ = tx.send(AppMsg::TextOverlay { gen, title, body });
        });
    }
}

/// `:dlq-redrive status`'s text. Pure. A failed read says so; none says
/// none.
pub(crate) fn format_redrives(
    tasks: &Result<Vec<crate::aws::RedriveTask>, String>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    match tasks {
        Err(e) => format!("could not read the redrives: {e}\n"),
        Ok(t) if t.is_empty() => "(no redrive has run on this dead-letter queue)\n".into(),
        Ok(tasks) => {
            let mut out = String::new();
            for t in tasks {
                let of = t.to_move.map(|n| format!(" of {n}")).unwrap_or_default();
                let started = t
                    .started
                    .map(|s| format!("{} ago", crate::ui::humanize_age(now - s)))
                    .unwrap_or_else(|| "—".into());
                out.push_str(&format!(
                    "{}  {}{of} moved  started {started}\n",
                    t.status, t.moved
                ));
                if let Some(f) = &t.failure {
                    out.push_str(&format!("  FAILED: {f}\n"));
                }
            }
            if tasks.iter().any(crate::aws::RedriveTask::in_progress) {
                out.push_str("`:dlq-redrive cancel` stops the running one\n");
            }
            out
        }
    }
}
