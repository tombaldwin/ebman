//! The undo buffer for MCP `dlq_delete`, and the `dlq_undo` tool that
//! reads it: a deleted dead-letter message is held in memory for
//! [`UNDO_WINDOW_SECS`] so "that was the wrong one" can put it back.
//! Moved out of `writes.rs`, which it was self-contained inside of.

use super::writes::*;
use super::*;

/// Does this verb destroy something that can be handed back?
///
/// One function because there are two dispatch paths — live and demo —
/// and they had the same condition written twice. A mutation to the
/// live copy was invisible to a demo-mode test, which is the shape
/// that hides a defect rather than the shape that finds one.
///
/// Delete only. A resend leaves the message existing on the main
/// queue, so "restoring" it would enqueue a second copy and call that
/// a recovery. A purge can be thousands, and a capped sample would put
/// back SOME of what it destroyed — worse than offering nothing.
pub(super) fn captures_for_undo(verb: WriteVerb) -> bool {
    matches!(verb, WriteVerb::DlqDelete)
}

/// How long a deleted message stays recoverable.
///
/// Long enough for "wait, that was the wrong one" — the mistake that
/// actually happens — and short enough that ebman is not a message
/// store. Memory only: the bodies are never written to disk, because
/// `mcp.peek_bodies` exists precisely because they can carry customer
/// data, and a durable copy would be worse than showing one to an
/// agent.
pub(super) const UNDO_WINDOW_SECS: u64 = 600;

/// How many. A cap, because an agent working through a bad deploy can
/// delete many in a row and memory is not free.
pub(super) const UNDO_CAPACITY: usize = 20;

/// A message ebman destroyed and can still put back.
#[derive(Debug, Clone)]
pub(super) struct DeletedMessage {
    pub env: String,
    pub queue_url: String,
    /// The `profile` / `region` the delete was dispatched with, so an
    /// undo rebuilds the SAME client. It used to take them from the
    /// undo call's own arguments, so a message deleted in a
    /// non-default region was restored against the default one.
    pub profile: Option<String>,
    pub region: Option<String>,
    pub original_id: String,
    pub task: Option<String>,
    pub body: String,
    pub attributes: Vec<(String, String, String)>,
    pub at: tokio::time::Instant,
    /// Set while an undo is restoring this message. The entry stays in
    /// the buffer until the restore SUCCEEDS, so nothing can lose it.
    pub restoring_since: Option<tokio::time::Instant>,
}

/// The task label for a dead-lettered message that carries no
/// `beanstalk.sqsd.task_name` — used by the plan, the delete's audit
/// line and the undo's, which must agree to correlate.
pub(super) const NOT_A_WORKER_TASK: &str = "(not an EB worker task)";

/// The error for a restore that did not go through. `held` is
/// [`Server::finish_restore`]'s answer: whether the message can still be
/// retried, or the window closed while the restore was running.
pub(super) fn restore_failed(why: &str, held: bool) -> String {
    if held {
        format!("restoring the message failed: {why} — it is still held and can be retried")
    } else {
        format!(
            "restoring the message failed: {why} — and its {UNDO_WINDOW_SECS}s window closed \
             while the restore was running, so it is no longer held"
        )
    }
}

/// How long an undo's claim on a held message stands: twice the tool
/// timeout, so a restore still running is never raced, and a claim left
/// behind by one whose call was DROPPED — the tool timeout firing
/// mid-send — lapses within a minute and the message is restorable
/// again.
pub(super) const RESTORE_CLAIM_SECS: u64 = super::TOOL_TIMEOUT_SECS * 2;

/// What an undo found when it tried to claim a held message.
#[derive(Debug)]
pub(super) enum Claim {
    Claimed(Box<DeletedMessage>),
    /// An undo is restoring it now — or one was, and its call was
    /// dropped; either way the claim lapses within `retry_after_secs`.
    InProgress {
        retry_after_secs: u64,
    },
    /// Not held: never deleted here, already restored, or expired.
    NotHeld,
}

impl Server {
    /// Hold a destroyed message for [`UNDO_WINDOW_SECS`], and say how
    /// long the caller has.
    ///
    /// Returns the window rather than nothing so the dispatch result
    /// can state it. An undo nobody is told about is not an undo.
    pub(super) async fn remember_deleted(
        &self,
        p: &PendingWrite,
        msg: crate::aws::QueueMessage,
    ) -> Option<u64> {
        let env = p.env.as_str();
        let queue_url = p.dlq_url.clone();
        // `None`, not `Some(0)`. A zero-second window is a claim that
        // the message was held and has already expired; not holding it
        // at all is a different fact, and the one an agent needs if it
        // is about to tell someone the delete is reversible.
        let queue_url = queue_url?;
        let mut buf = self.deleted.lock().await;
        buf.retain(|d: &DeletedMessage| d.at.elapsed().as_secs() < UNDO_WINDOW_SECS);
        if buf.len() >= UNDO_CAPACITY {
            // The oldest UNCLAIMED entry. Evicting a message mid-restore
            // meant that if the restore then failed, `finish_restore`
            // found nothing and the message was gone. Falls back to the
            // oldest overall only if every entry is claimed, so the
            // capacity bound always holds.
            let victim = buf
                .iter()
                .position(|d| d.restoring_since.is_none())
                .unwrap_or(0);
            buf.remove(victim);
        }
        buf.push(DeletedMessage {
            env: env.to_string(),
            queue_url,
            profile: p.profile.clone(),
            region: p.region.clone(),
            original_id: msg.id,
            task: msg.task.as_ref().and_then(|t| t.name.clone()),
            body: msg.body,
            attributes: msg.attributes,
            at: tokio::time::Instant::now(),
            restoring_since: None,
        });
        Some(UNDO_WINDOW_SECS)
    }

    /// Claim one held message for a restore, under the lock.
    ///
    /// A CLAIM, not a removal. The first cut of the take-once fix
    /// (90f93e2) removed the entry before restoring and put it back on
    /// the error paths — but `dlq_undo` runs under the 30 s tool
    /// timeout, and a timeout DROPS the future rather than returning an
    /// error: a slow credential load or send lost the only copy of the
    /// body, and the agent was told "timed out". Found by the re-review.
    /// Now the entry leaves the buffer only when the restore succeeds;
    /// a dropped restore leaves a claim that lapses (`RESTORE_CLAIM_SECS`).
    /// The worst case is a duplicate send, which in a worker queue is
    /// the recoverable failure — never a lost message.
    ///
    /// Still at most once: while a claim stands, a second undo is told
    /// `InProgress`, not the old "already restored" — which was false
    /// whenever the first one then failed.
    pub(super) async fn claim_for_restore(&self, id: &str) -> Claim {
        let mut buf = self.deleted.lock().await;
        buf.retain(|d| d.at.elapsed().as_secs() < UNDO_WINDOW_SECS);
        let Some(d) = buf.iter_mut().find(|d| d.original_id == id) else {
            return Claim::NotHeld;
        };
        if let Some(t) = d.restoring_since {
            let held_for = t.elapsed().as_secs();
            if held_for < RESTORE_CLAIM_SECS {
                return Claim::InProgress {
                    retry_after_secs: RESTORE_CLAIM_SECS - held_for,
                };
            }
        }
        d.restoring_since = Some(tokio::time::Instant::now());
        Claim::Claimed(Box::new(d.clone()))
    }

    /// End a claim: drop the message if it was restored, otherwise make
    /// it restorable again for the rest of its window. Returns whether
    /// it is still held: a restore claimed at 9:59 that fails after
    /// 10:00 releases a message the window has already closed on, and
    /// the failure must say so rather than imply a retry will work.
    pub(super) async fn finish_restore(&self, id: &str, restored: bool) -> bool {
        let mut buf = self.deleted.lock().await;
        if restored {
            buf.retain(|d| d.original_id != id);
            return false;
        }
        match buf.iter_mut().find(|d| d.original_id == id) {
            Some(d) => {
                d.restoring_since = None;
                d.at.elapsed().as_secs() < UNDO_WINDOW_SECS
            }
            None => false,
        }
    }

    /// What is still recoverable, newest first.
    pub(super) async fn recoverable(&self) -> Vec<DeletedMessage> {
        let mut buf = self.deleted.lock().await;
        buf.retain(|d| d.at.elapsed().as_secs() < UNDO_WINDOW_SECS);
        let mut out = buf.clone();
        out.reverse();
        out
    }
}

impl Server {
    /// Put back a message this server destroyed, or list what still
    /// can be.
    ///
    /// **Single-phase, deliberately.** Every other write plans and then
    /// confirms, because the cost of acting is high and the cost of
    /// pausing is low. Undo inverts both: it is the least destructive
    /// thing here — it restores what a previous write removed — and it
    /// is reached for under exactly the time pressure that makes a
    /// two-step protocol harmful. Requiring a plan to undo a mistake is
    /// backwards.
    ///
    /// The gates still apply. A freeze or a pin refuses this as it
    /// refuses anything, which is arguable — restoring a message during
    /// an incident is often what you want — but a gate with exceptions
    /// is one nobody can predict, and the operator can lift it.
    pub(super) async fn tool_dlq_undo(&self, args: &Value) -> Result<String, WriteError> {
        let held = self.recoverable().await;
        let Some(want) = arg_str(args, "message_id") else {
            // No id: report what is available rather than guessing.
            // Restoring "the last one" is the kind of convenience that
            // puts back the wrong message at 3am.
            let rows: Vec<String> = held
                .iter()
                .map(|d| {
                    format!(
                        "{{\"message_id\":{},\"env\":{},\"task\":{},\"expires_in_secs\":{},\"restoring\":{}}}",
                        util::json_string(&d.original_id),
                        util::json_string(&d.env),
                        util::json_string(d.task.as_deref().unwrap_or(NOT_A_WORKER_TASK)),
                        UNDO_WINDOW_SECS.saturating_sub(d.at.elapsed().as_secs()),
                        d.restoring_since
                            .is_some_and(|t| t.elapsed().as_secs() < RESTORE_CLAIM_SECS),
                    )
                })
                .collect();
            return Ok(format!(
                "{{\"recoverable\":[{}],\"window_secs\":{UNDO_WINDOW_SECS},\"note\":\"Held in \
                 memory by THIS server only: a restart loses them, and nothing deleted by \
                 another process or before this server started is here. Purges are never \
                 recoverable.\"}}",
                rows.join(",")
            ));
        };

        let d = match self.claim_for_restore(&want).await {
            Claim::Claimed(d) => *d,
            Claim::InProgress { retry_after_secs } => {
                // Not "another call right now": after a timeout there is
                // no other call, only a claim that has not lapsed yet.
                return Err(WriteError::Invalid(format!(
                    "'{want}' is mid-restore — by a call still running, or by one that timed \
                     out. Nothing is lost either way: it can be retried in at most \
                     {retry_after_secs}s if it is still held then."
                )));
            }
            Claim::NotHeld => {
                return Err(WriteError::Invalid(format!(
                    "'{want}' is not recoverable. Either it was never deleted by this server, \
                     it has already been restored, the {UNDO_WINDOW_SECS}s window has passed, \
                     or the server restarted. Call this tool with no arguments to see what IS \
                     recoverable."
                )));
            }
        };

        if let Some(refused) = self
            .gate_refusal(
                &d.env,
                &d.profile,
                d.region.as_deref(),
                "dlq-undo",
                &crate::config::RoleCheck::NotAsked,
            )
            .await
        {
            // A refusal is decided before anything is sent, well
            // inside the window: nothing to add to it.
            let _ = self.finish_restore(&d.original_id, false).await;
            return Err(WriteError::Refused(refused));
        }

        if !matches!(self.backend, Backend::Demo) {
            // The profile and region the DELETE used, not this call's.
            let client_args = json!({
                "profile": d.profile.clone().unwrap_or_default(),
                "region": d.region.clone().unwrap_or_default(),
            });
            let client = match self.client(&client_args).await {
                Ok(c) => c,
                Err(e) => {
                    let held = self.finish_restore(&d.original_id, false).await;
                    return Err(restore_failed(&e.to_string(), held).into());
                }
            };
            // Audited like every other write. A restore is a real
            // `SendMessage`, and it left no line at all — so the log
            // said a message was deleted and never that it came back.
            let client_name = self
                .client_name
                .lock()
                .map(|s| s.clone())
                .unwrap_or_else(|_| "unknown".into());
            let can_ask = self
                .client_supports_elicitation
                .load(std::sync::atomic::Ordering::Relaxed);
            // The label the DELETE's audit line used, so the two lines
            // correlate by task. `unwrap_or_default()` wrote `task=""`
            // here for a non-worker message, while the delete had
            // recorded `NOT_A_WORKER_TASK`.
            let target = DlqTarget {
                id: d.original_id.clone(),
                task: d.task.clone().unwrap_or_else(|| NOT_A_WORKER_TASK.into()),
            };
            let extras = dlq_audit_line(&client_name, can_ask, &target);
            let refs: Vec<(&str, &str)> = extras.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let audit_profile = d.profile.clone().or_else(|| self.ambient_profile.clone());
            let region = client.context.region.clone();
            crate::audit::append_action_dispatched(
                None,
                audit_profile.as_deref(),
                &region,
                "dlq-undo",
                &d.env,
                &refs,
            );
            let sent = client
                .send_message(&d.queue_url, &d.body, &d.attributes)
                .await
                .map_err(|e| e.to_string());
            crate::audit::append_action_completed(
                None,
                audit_profile.as_deref(),
                &region,
                "dlq-undo",
                &d.env,
                sent.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &refs,
            );
            let held = self.finish_restore(&d.original_id, sent.is_ok()).await;
            if let Err(e) = sent {
                return Err(restore_failed(&e, held).into());
            }
        } else {
            let _ = self.finish_restore(&d.original_id, true).await;
        }

        // Rule 6: say what could NOT be restored. The body and every
        // attribute come back verbatim, and three things cannot —
        // calling this an undo without saying so would be the
        // over-claim the foreclosure line exists to prevent.
        Ok(format!(
            "{{\"restored\":true,\"queue\":{},\"original_message_id\":{},\"task\":{},\
             \"not_restored\":[\"the message id: SQS assigns a new one on send\",\
             \"receive_count: resets to 0, so retry history is lost\",\
             \"sent_at: now, not the original enqueue time\"]}}",
            util::json_string(&d.queue_url),
            util::json_string(&d.original_id),
            util::json_string(d.task.as_deref().unwrap_or(NOT_A_WORKER_TASK)),
        ))
    }
}
