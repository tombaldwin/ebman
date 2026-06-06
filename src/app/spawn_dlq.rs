//! Dead-letter-queue background dispatch: peek / delete-one / resend /
//! purge / replay-batch. Each `spawn_dlq_*` helper reads the active
//! `DlqState` (set when the operator opens the DLQ viewer), fires the
//! matching SQS call, and routes the outcome through `AppMsg::DlqMessages`
//! (peek) or `AppMsg::DlqActionResult` (mutations) for the handler in
//! `msg.rs` to fold back into the viewer.
//!
//! Read-only safety: the three mutating spawners (resend / purge /
//! replay) gate through `deny_write` exactly as the single-env write
//! paths do. `spawn_dlq_delete_one` reuses the `DlqOp::Resent` result
//! variant (the handler drops the message by id, which is what a delete
//! needs).
//!
//! 0.21+ lift: cluster moved out of `src/app.rs` as part of the
//! `spawn_*` clusters refactor. Pure relocation; the `handle_dlq_key`
//! key handler that sat between these methods stays in `app.rs` (it's
//! not a spawner). No behaviour change.

use super::{flatten_err, App, AppMsg, DlqOp, QueueView};

impl App {
    pub(super) fn spawn_dlq_fetch(&mut self) {
        let Some(dlq) = self.dlq.as_mut() else { return };
        dlq.loading = true;
        dlq.error = None;
        let env_name = dlq.env_name.clone();
        let queue_url = match dlq.viewing {
            QueueView::Dlq => dlq.dlq_url.clone(),
            QueueView::Main => dlq.main_queue_url.clone(),
        };
        self.spawn_aws(
            "peek_messages",
            move |aws| async move { aws.peek_messages(&queue_url, 50).await },
            move |gen, result| AppMsg::DlqMessages {
                gen,
                env_name,
                result,
            },
        );
    }

    /// Delete a single message from whichever queue is currently loaded
    /// (`dlq.viewing`). The message's `receipt_handle` keeps it deletable
    /// even though our visibility timeout window is short — SQS treats the
    /// receipt handle as the canonical authorisation token for delete.
    pub(super) fn spawn_dlq_delete_one(&mut self, idx: usize) {
        let Some(dlq) = self.dlq.as_mut() else { return };
        let Some(msg) = dlq.messages.get(idx).cloned() else {
            return;
        };
        let queue_url = match dlq.viewing {
            QueueView::Dlq => dlq.dlq_url.clone(),
            QueueView::Main => dlq.main_queue_url.clone(),
        };
        if queue_url.is_empty() {
            self.error_message = Some("queue URL missing — cannot delete".into());
            return;
        }
        let env_name = dlq.env_name.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let queue_label = if matches!(dlq.viewing, QueueView::Main) {
            "MAIN"
        } else {
            "DLQ"
        };
        crate::audit::append_dlq_op(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "sqs-delete",
            &env_name,
            &[("queue", queue_label), ("msg_id", &msg.id)],
        );
        tokio::spawn(async move {
            let result = aws
                .delete_message(&queue_url, &msg.receipt_handle)
                .await
                .map(|_| DlqOp::Resent {
                    // Reuse the existing "Resent" variant — the handler
                    // already drops the message by id, which is exactly what
                    // delete should do.
                    message_id: msg.id.clone(),
                })
                .map_err(|e| flatten_err("delete_message", e));
            let _ = tx.send(AppMsg::DlqActionResult {
                gen,
                env_name,
                result,
            });
        });
    }

    pub(super) fn spawn_dlq_resend_selected(&mut self) {
        let env_name = match self.dlq.as_ref() {
            Some(d) => d.env_name.clone(),
            None => return,
        };
        if self.deny_write(&env_name, "resend") {
            return;
        }
        let Some(dlq) = self.dlq.as_mut() else { return };
        let Some(idx) = dlq.list_state.selected() else {
            return;
        };
        let Some(msg) = dlq.messages.get(idx).cloned() else {
            return;
        };
        if dlq.main_queue_url.is_empty() {
            dlq.error = Some("main queue URL unknown — cannot resend".into());
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_name = dlq.env_name.clone();
        let main_url = dlq.main_queue_url.clone();
        let dlq_url = dlq.dlq_url.clone();
        crate::audit::append_dlq_op(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "dlq-resend",
            &env_name,
            &[("msg_id", &msg.id)],
        );
        tokio::spawn(async move {
            let result = match aws.send_message(&main_url, &msg.body).await {
                Ok(()) => match aws.delete_message(&dlq_url, &msg.receipt_handle).await {
                    Ok(()) => Ok(DlqOp::Resent {
                        message_id: msg.id.clone(),
                    }),
                    Err(e) => {
                        tracing::error!(target: "ebman::aws", op = "dlq_delete_after_send", error = ?e, "aws call failed");
                        Err(format!("sent to main queue, but DLQ delete failed: {e}"))
                    }
                },
                Err(e) => {
                    tracing::error!(target: "ebman::aws", op = "dlq_send", error = ?e, "aws call failed");
                    Err(format!("send to main queue failed: {e}"))
                }
            };
            let _ = tx.send(AppMsg::DlqActionResult {
                gen,
                env_name,
                result,
            });
        });
    }

    pub(super) fn spawn_dlq_purge(&mut self, env_name: String, dlq_url: String) {
        if self.deny_write(&env_name, "purge") {
            return;
        }
        crate::audit::append_dlq_op(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "dlq-purge",
            &env_name,
            &[],
        );
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = aws
                .purge_queue(&dlq_url)
                .await
                .map(|_| DlqOp::Purged)
                .map_err(|e| flatten_err("purge_queue", e));
            let _ = tx.send(AppMsg::DlqActionResult {
                gen,
                env_name,
                result,
            });
        });
    }

    /// Batch DLQ replay: for each message, send the body to the main queue
    /// then delete it from the DLQ. A send failure (or a delete failure
    /// after a successful send) counts toward `failures` and is logged;
    /// the batch keeps going. Result lands as `DlqOp::Replayed`.
    pub(super) fn spawn_dlq_replay_batch(&mut self, messages: Vec<crate::aws::QueueMessage>) {
        let env_name = match self.dlq.as_ref() {
            Some(d) => d.env_name.clone(),
            None => return,
        };
        if self.deny_write(&env_name, "replay") {
            return;
        }
        let Some(dlq) = self.dlq.as_ref() else { return };
        if matches!(dlq.viewing, QueueView::Main) {
            self.error_message = Some("replay is only available in DLQ view".into());
            return;
        }
        if dlq.main_queue_url.is_empty() {
            self.error_message = Some("main queue URL unknown — cannot replay".into());
            return;
        }
        let main_url = dlq.main_queue_url.clone();
        let dlq_url = dlq.dlq_url.clone();
        let env_name = dlq.env_name.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let count = messages.len();
        crate::audit::append_dlq_op(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "dlq-replay",
            &env_name,
            &[("count", &count.to_string())],
        );
        self.status_message = Some(format!("replaying {count} message(s) to the main queue…"));
        tokio::spawn(async move {
            let mut failures = 0usize;
            for msg in &messages {
                match aws.send_message(&main_url, &msg.body).await {
                    Ok(()) => {
                        if let Err(e) = aws.delete_message(&dlq_url, &msg.receipt_handle).await {
                            tracing::error!(target: "ebman::aws", op = "dlq_replay_delete", error = ?e, msg_id = %msg.id, "DLQ delete after send failed");
                            failures += 1;
                        }
                    }
                    Err(e) => {
                        tracing::error!(target: "ebman::aws", op = "dlq_replay_send", error = ?e, msg_id = %msg.id, "send to main queue failed");
                        failures += 1;
                    }
                }
            }
            let result = Ok(DlqOp::Replayed {
                count: count - failures,
                failures,
            });
            let _ = tx.send(AppMsg::DlqActionResult {
                gen,
                env_name,
                result,
            });
        });
    }
}
