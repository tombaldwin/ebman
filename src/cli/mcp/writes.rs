//! MCP v2 write tools (`--allow-writes`, 0.28): `deploy`, `restart`,
//! `rebuild`, `terminate`, `set_option`, plus the `confirm_action`
//! second phase. Spec: BACKLOG.md "0.28 candidates" — decisions are
//! LOCKED there; the shape here implements them:
//!
//! - Every write is two-phase: the verb tool validates (env exists,
//!   pin, freeze, verb-specific checks) and returns a `pending` plan
//!   with a single-use 60s `confirm_token`; `confirm_action`
//!   dispatches. The plan is transcript-visible by construction.
//! - `terminate`'s phase 2 additionally requires `confirm_name` ==
//!   the env name (the MCP equivalent of the TUI's strict-typed
//!   confirm); one retry within the TTL on mismatch.
//! - Writes are serialized server-wide: one pending plan (a new plan
//!   replaces it — the agent re-planned), one in-flight dispatch.
//! - Dispatch-only semantics: no wait-for-green; the agent polls the
//!   read tools. Keeps every call inside the 30s tool bound.
//! - Audit parity with the CLI: dispatched/completed pairs tagged
//!   `via=mcp client=<clientInfo.name> can_ask=<bool>`, the last
//!   recording whether the client declared elicitation support — a
//!   per-connection fact that cannot be recovered after the fact.
//!   Demo mode writes NO audit
//!   lines and fires NO webhooks — synthetic success only.
//!
//! Tokens are single-use and short-lived; they force the round-trip,
//! they are not a cryptographic boundary (the agent that plans is the
//! agent that receives the token — the audience for the plan is the
//! HUMAN reading the agent's transcript).

use super::*;

/// Two-phase write state — the single pending-plan slot, guarded by
/// the server's mutex. `dispatching` (whether a write is in flight)
/// lives on `Server` as an `AtomicBool` so the RAII reset guard can
/// clear it synchronously even on an unwind (see `tool_confirm_action`).
#[derive(Default)]
pub(super) struct WriteState {
    pub pending: Option<PendingWrite>,
    /// Tokens minted for plans this session that are no longer live,
    /// newest last.
    ///
    /// Confirming one of these used to return "unknown confirm_token",
    /// which is what a TYPO returns — so an agent that re-planned and
    /// then confirmed the older token was told its token was garbage
    /// rather than superseded, and had no way to tell the two apart.
    /// The distinction changes what the agent should do next: re-read
    /// the newer plan, versus re-send the token it already has.
    ///
    /// Bounded, because it is only a diagnostic: past the cap the
    /// oldest are dropped and confirming one of those falls back to
    /// the unknown-token message, which is honest — we genuinely no
    /// longer know.
    pub retired: std::collections::VecDeque<String>,
}

impl WriteState {
    /// Install a freshly minted plan, retiring whatever it replaces.
    ///
    /// The retirement lives here rather than at the call site because
    /// the call site needs a running server to reach — so a test could
    /// pin the message branch but not the fact that anything ever
    /// reaches `retired`. That is the same shape as an earlier cache
    /// whose read path was tested while nothing wrote to it.
    pub(super) fn install(&mut self, plan: PendingWrite) {
        if let Some(old) = self.pending.take() {
            if self.retired.len() >= RETIRED_TOKEN_MEMORY {
                self.retired.pop_front();
            }
            self.retired.push_back(old.token);
        }
        self.pending = Some(plan);
    }
}

/// What to tell an agent whose `confirm_token` doesn't match the
/// pending plan.
///
/// Split out so the distinction is testable: `confirm` needs a live
/// server, and the branch — not the plumbing — is what was wrong.
fn mismatched_token_message(retired: &std::collections::VecDeque<String>, token: &str) -> String {
    if retired.iter().any(|t| t == token) {
        // The agent re-planned and then confirmed the older token.
        // Saying "unknown" here is what a TYPO gets, and the two want
        // different next moves: re-read the newer plan, versus re-send
        // the token you already hold.
        "confirm_token superseded by a newer plan — confirm that one, or re-plan".to_string()
    } else {
        "unknown confirm_token — re-plan required".to_string()
    }
}

/// How many retired tokens to remember. An agent re-planning more than
/// a handful of times inside one 60-second TTL is not a case worth
/// spending memory on.
/// Assemble the audit `extras` for an MCP-dispatched write.
///
/// Pure half of `Server::write_extras`, split out so the shape is
/// testable on its own. Callers go through the method — reading the
/// capability there rather than passing it in means there is no bool
/// at the call site to wire up wrongly.
fn write_extras_parts(
    client_name: &str,
    can_ask: bool,
    version: Option<&str>,
    settings_len: usize,
    dlq_message_id: Option<&str>,
    dlq_task: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut extras = vec![
        ("via", "mcp".to_string()),
        ("client", client_name.to_string()),
        ("can_ask", can_ask.to_string()),
    ];
    if let Some(v) = version {
        extras.push(("version", v.to_string()));
    }
    if settings_len > 0 {
        extras.push(("settings", settings_len.to_string()));
    }
    // WHICH message, and what it was. The target of a DLQ write is the
    // environment, so without these the log records that something was
    // deleted from `poly-batch` and never what — and a delete is the
    // one action where "what" cannot be recovered by looking.
    if let Some(id) = dlq_message_id {
        extras.push(("message_id", id.to_string()));
    }
    if let Some(t) = dlq_task {
        extras.push(("task", t.to_string()));
    }
    extras
}

/// One audit line's extras per message in a batch.
///
/// Extracted because the property that matters — N messages produce N
/// lines, each naming its own id and task — is otherwise only
/// observable through a live AWS dispatch, and so was not observable
/// at all. Collapsing a batch to a single line would record that five
/// messages were deleted from an environment while leaving the log
/// unable to say which, and for a delete the log is the only place
/// that answer can still exist.
fn dlq_audit_line(
    client_name: &str,
    can_ask: bool,
    target: &DlqTarget,
) -> Vec<(&'static str, String)> {
    write_extras_parts(
        client_name,
        can_ask,
        None,
        0,
        Some(&target.id),
        Some(&target.task),
    )
}

fn dlq_audit_lines(
    client_name: &str,
    can_ask: bool,
    targets: &[DlqTarget],
) -> Vec<Vec<(&'static str, String)>> {
    targets
        .iter()
        .map(|t| dlq_audit_line(client_name, can_ask, t))
        .collect()
}

impl Server {
    /// Audit extras for an MCP-dispatched write.
    ///
    /// `can_ask` — whether the client declared elicitation support — is
    /// read here rather than passed in, so the whole chain (initialize →
    /// capability → audit line) is reachable from a test without a live
    /// AWS dispatch. It belongs on the dispatch line rather than being
    /// inferred later: the capability is per-connection, and the
    /// connection is long gone by the time anyone reads the log.
    fn write_extras(
        &self,
        client_name: &str,
        version: Option<&str>,
        settings_len: usize,
        dlq_message_id: Option<&str>,
        dlq_task: Option<&str>,
    ) -> Vec<(&'static str, String)> {
        write_extras_parts(
            client_name,
            self.client_supports_elicitation
                .load(std::sync::atomic::Ordering::Relaxed),
            version,
            settings_len,
            dlq_message_id,
            dlq_task,
        )
    }
}

impl Server {
    /// Record a scope refusal, and render it.
    ///
    /// A refusal that leaves no `stage=refused` line is the pre-0.37
    /// blind spot — a blocked write and no attempt at all look
    /// identical in the log. This one is worth seeing more than most:
    /// an ungranted verb is not advertised, so a client reaching it is
    /// working from a stale tool list or probing the surface, and
    /// neither is visible any other way.
    ///
    /// Demo writes nothing real, matching `gate_refusal`: the refusal
    /// is genuine, the fleet is not.
    fn refused_out_of_scope(
        &self,
        verb: WriteVerb,
        env: Option<&str>,
        region: Option<&str>,
    ) -> WriteError {
        let name = verb.tool_name();
        // On a `--read-only` server, "start it with --allow-writes" is
        // advice that produces a startup error: the two contradict and
        // are refused together. The control the operator actually has
        // is removing the flag they set, and a remedy naming the wrong
        // control is the defect remedies exist to avoid — it goes into
        // the audit line as well as the agent's error.
        if self
            .mcp_read_only
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return WriteError::Refused(Audited::record(
                matches!(self.backend, Backend::Demo),
                None,
                region.unwrap_or("-"),
                verb.label(),
                env.unwrap_or("-"),
                "not_granted",
                "this server was started with --read-only; the operator must remove \
                 that flag and restart it",
                format!(
                    "'{name}' is unavailable: this server was started with --read-only, \
                     which refuses every write regardless of what your client can do. \
                     Do not ask for --allow-writes — the two are refused together at \
                     startup. Ask the operator to remove --read-only if they want \
                     writes here."
                ),
            ));
        }
        WriteError::Refused(Audited::record(
            matches!(self.backend, Backend::Demo),
            None,
            region.unwrap_or("-"),
            verb.label(),
            env.unwrap_or("-"),
            "not_granted",
            &format!("restart the MCP server with --allow-writes={name}"),
            format!(
                "'{name}' is not in this server's write scope — start it with \
                 --allow-writes, or --allow-writes={name} to grant just this one"
            ),
        ))
    }

    /// The write gate for both MCP phases.
    ///
    /// Named `gate_refusal` rather than `refuse_write` because
    /// `cli::refuse_write` is a different function with different
    /// behaviour — it loads the config, prints, and exits the process.
    /// Two same-named neighbours where one exits and one returns is a
    /// misreading waiting to happen.
    ///
    /// Demo goes through the pure half: a demo server still reads the
    /// REAL cross-process freeze marker, so `ebman mcp serve --demo
    /// --allow-writes` attempted during a live `:freeze-deploys` was
    /// appending a real line to the real audit log. This module's own
    /// docs promise demo writes none, and a refusal being genuine does
    /// not make the fleet genuine.
    fn gate_refusal(
        &self,
        env: &str,
        profile: &Option<String>,
        region: Option<&str>,
        action_label: &str,
    ) -> Option<Audited> {
        let freeze = crate::freeze::read_active();
        // The UNAUDITED half deliberately, for both backends: the
        // recording happens in `Audited::record` so there is exactly
        // one place in this module that writes a refusal line. Going
        // through `cli::write_refusal` for the live case would audit
        // there instead, and a `Refused` would then exist that this
        // module had not recorded — which is the whole property the
        // type carries.
        let (refusal, message, pin_profile) =
            crate::cli::write_refusal_unaudited(&self.safety_cfg, env, profile, freeze)?;
        Some(Audited::record(
            matches!(self.backend, Backend::Demo),
            pin_profile.as_deref(),
            region.unwrap_or("-"),
            action_label,
            env,
            refusal.rule(),
            &refusal.remedy(),
            message,
        ))
    }
}

/// Make a foreign string safe to show an operator.
///
/// Everything interpolated into the approval dialog that ebman did not
/// author is untrusted: a DLQ task name is whatever the application
/// POSTed to the queue (`beanstalk.sqsd.task_name`, arbitrary
/// sender-controlled UTF-8), a version label comes from AWS, and an
/// STS error is a remote string. The dialog is one sentence a human
/// reads to decide whether to destroy something, so a newline plus
/// `  • Nightly sweep (id)` forges a batch row, and a bidi override
/// reverses the meaning of the line around it.
///
/// This is prompt injection aimed at a PERSON rather than a model, and
/// the design note's "no agent-supplied prose in a plan" rule missed
/// it: these strings come from AWS, not from the agent.
///
/// Control characters and bidi/zero-width formatting become U+FFFD so
/// something visibly wrong is shown rather than nothing; length is
/// capped so one field cannot push the rest of the sentence out of a
/// dialog.
fn sanitize_for_ask(raw: &str) -> String {
    sanitize_capped(raw, 120)
}

/// The same stripping with no truncation.
///
/// ONLY for fields whose length is refused at plan time. Truncating a
/// value the operator is approving hides its operative end: a
/// `set_option` value of
/// `https://payments.example/callback/<90 filler>@evil.example/x`
/// shows as a benign-looking prefix and an ellipsis, because the `@`
/// that makes everything before it userinfo sits past the cut. The
/// operator approves one destination and another is applied. So a
/// value that cannot be shown whole is not shown at all — the plan is
/// refused instead, and this renders what survives that check.
fn sanitize_whole(raw: &str) -> String {
    sanitize_capped(raw, usize::MAX)
}

fn sanitize_capped(raw: &str, max: usize) -> String {
    let mut out = String::with_capacity(raw.len().min(max.min(4096)) + 1);
    for c in raw.chars() {
        if out.chars().count() >= max {
            out.push('…');
            break;
        }
        // Bidi overrides/embeddings, zero-width joiners and marks, and
        // the line/paragraph separators — none of which a task name
        // needs, all of which change what the sentence appears to say.
        let formatting = matches!(c,
            '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
            | '\u{2066}'..='\u{2069}' | '\u{2028}' | '\u{2029}' | '\u{FEFF}'
            // U+061C ARABIC LETTER MARK is a bidi control that
            // `is_control()` does not report, and it reorders visibly.
            | '\u{061C}'
            // The TAG block is invisible by design — text that renders
            // as nothing at all, beside text that does.
            | '\u{E0000}'..='\u{E007F}');
        if c.is_control() || formatting {
            out.push('\u{FFFD}');
        } else {
            out.push(c);
        }
    }
    out
}

/// The one sentence an operator is asked to approve.
///
/// Server-authored from the plan, never from anything the agent wrote
/// — a prompt composed by the party requesting permission is a
/// persuasion surface regardless of intent.
///
/// Carries what the action FORECLOSES as well as what it does. A plan
/// fully specified about mechanics and silent about stakes reads as
/// complete, and a prompt that looks complete discourages the pause in
/// which the operator remembers what it does not contain.
pub(super) fn ask_summary(p: &PendingWrite) -> String {
    let what = match p.dlq_targets.as_slice() {
        [] => match p.version.as_deref() {
            Some(v) => format!("{} to {}", p.verb.label(), sanitize_for_ask(v)),
            None => p.verb.label().to_string(),
        },
        [one] => format!(
            "{} — {} ({})",
            p.verb.label(),
            sanitize_for_ask(&one.task),
            sanitize_for_ask(&one.id)
        ),
        // Enumerated, never summarised. "5 messages" is a number to
        // agree with; a list is something to read. `DLQ_BATCH_CAP`
        // exists precisely so this stays readable.
        many => format!(
            "{} — {} messages:\n{}",
            p.verb.label(),
            many.len(),
            many.iter()
                .map(|t| format!(
                    "  • {} ({})",
                    sanitize_for_ask(&t.task),
                    sanitize_for_ask(&t.id)
                ))
                .collect::<Vec<_>>()
                .join("\n")
        ),
    };
    // The CHANGES themselves, not just the verb. A dialog reading
    // "SetOption on api-prod" asks the operator to approve an unknown
    // edit to an unknown key — and `set_option` can re-point an
    // environment variable at an attacker's endpoint. The plan JSON
    // has carried these all along; the plan JSON is read by the agent,
    // and the dialog exists precisely because the operator may not be
    // reading the agent's transcript.
    //
    // Values are shown. They are the substance of the approval, and a
    // redacted new value would make the prompt unanswerable; the
    // operator is the party already trusted with this environment.
    let detail = if p.settings.is_empty() {
        String::new()
    } else {
        format!(
            "\n{}",
            p.settings
                .iter()
                .map(|(ns, name, value)| format!(
                    "  • {}:{} = {}",
                    sanitize_whole(ns),
                    sanitize_whole(name),
                    // WHOLE, never truncated — see `sanitize_whole`.
                    // Bounded by `SET_OPTION_FIELD_MAX` at plan time.
                    sanitize_whole(value)
                ))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        "{what}{detail} on {}, {}.\n\n{}",
        sanitize_for_ask(&p.env),
        // Sanitised like every other foreign field. `line()` carries
        // an ARN from STS, and on the `Unknown` arm a raw SDK error
        // chain — remote prose that can contain newlines, and in one
        // traced path an agent-supplied profile name echoed back
        // through `tool_error`'s credential hint. The invariant this
        // file states is "every foreign field"; this was the one slot
        // that was not, and it sits in the sentence a human reads
        // before destroying something.
        sanitize_for_ask(&p.caller.line()),
        // `dlq_visible` is carried on the plan so a purge dialog can
        // say how much it destroys. Passing `None` here rendered
        // "every message in the queue" with no number, while the plan
        // JSON showed the agent `messages_now` — the operator got the
        // vaguer half of the same fact.
        forecloses(p.verb, p.dlq_visible, p.dlq_targets.len())
    )
}

/// Resend or delete the message the plan NAMED, or refuse.
///
/// The whole point is the id check. SQS deletes by receipt handle, and
/// a handle is only valid while the message is invisible: the peek that
/// issues one uses a 5-second visibility timeout while a confirm token
/// lives 60 seconds. So a handle captured at plan time is dead for 55
/// of the 60 seconds the plan stays confirmable — carrying it would
/// fail almost always.
///
/// The fix that looks obvious and is worse: re-receive at confirm and
/// act on whatever comes back. That deletes the head of the queue,
/// which may not be the message the plan described, and nothing in the
/// output would say they differed — a silent target swap. So:
/// re-receive to get FRESH handles, find the planned ids among what
/// came back, and refuse for any that is not there.
///
/// Refusing is the right failure. The message may have been consumed,
/// redriven or deleted by someone else in the interval, and every one
/// of those means the plan no longer describes that message.
///
/// **Per message, not per batch.** One id gone says nothing about the
/// other nine, and failing the whole plan would leave nine messages
/// the operator approved untouched and force a re-plan against a queue
/// that has moved again. So each is attempted and each reports, which
/// is ARCHITECTURE.md rule 6 applied across a set.
async fn dispatch_dlq_batch(
    client: &crate::aws::AwsClient,
    p: &PendingWrite,
) -> Result<Vec<DlqOutcome>, String> {
    let Some(url) = p.dlq_url.as_deref() else {
        return Err("plan carried no queue url".into());
    };
    if p.dlq_targets.is_empty() {
        return Err("plan named no messages".into());
    }
    // ONE fresh receive for the whole batch: the handles from plan
    // time are expired, and re-reading per message would both cost N
    // round trips and race itself — each peek makes the messages it
    // returns invisible for 5s, so the second call could miss what the
    // first was holding.
    let pool = client
        .peek_messages(url, (DLQ_BATCH_CAP as i32) * 3)
        .await
        .map_err(|e| format!("re-reading the queue failed: {e}"))?;

    let mut out = Vec::with_capacity(p.dlq_targets.len());
    for target in &p.dlq_targets {
        let Some(msg) = pool.iter().find(|m| m.id == target.id) else {
            out.push(DlqOutcome {
                target: target.clone(),
                // Scoped to what was actually observed. It was not
                // among the messages the re-read returned; SQS does not
                // let us say more than that, and "it is gone" would be
                // a firmer claim than the evidence supports.
                result: Err(
                    "not among the messages returned when the queue was re-read \
                             — it may have been consumed, redriven or removed since the \
                             plan was made. Nothing was changed for this one."
                        .to_string(),
                ),
            });
            continue;
        };
        out.push(DlqOutcome {
            target: target.clone(),
            result: dispatch_one_dlq_message(client, p.verb, url, p.dlq_main_url.as_deref(), msg)
                .await,
        });
    }
    Ok(out)
}

/// One message's line in the batch report.
///
/// ARCHITECTURE.md rule 6: a result carries its own negative space.
/// A failed item is present and says WHY, rather than being absent —
/// an agent that gets four results for a five-message plan has to
/// infer the fifth, and "it isn't in the list" is indistinguishable
/// from "the list was truncated".
pub(super) fn render_dlq_item(
    target: &DlqTarget,
    result: &Result<Option<crate::aws::QueueMessage>, String>,
) -> String {
    match result {
        Ok(_) => format!(
            "{{\"message_id\":{},\"task\":{},\"ok\":true}}",
            util::json_string(&target.id),
            util::json_string(&target.task)
        ),
        Err(e) => format!(
            "{{\"message_id\":{},\"task\":{},\"ok\":false,\"error\":{}}}",
            util::json_string(&target.id),
            util::json_string(&target.task),
            util::json_string(e)
        ),
    }
}

/// What happened to one message in a batch.
pub(super) struct DlqOutcome {
    pub target: DlqTarget,
    /// `Ok(Some(msg))` means it was destroyed and is briefly
    /// recoverable; `Ok(None)` means it was resent, so it still exists
    /// and there is nothing to recover.
    pub result: Result<Option<crate::aws::QueueMessage>, String>,
}

async fn dispatch_one_dlq_message(
    client: &crate::aws::AwsClient,
    verb: WriteVerb,
    url: &str,
    main_url: Option<&str>,
    msg: &crate::aws::QueueMessage,
) -> Result<Option<crate::aws::QueueMessage>, String> {
    if verb == WriteVerb::DlqResend {
        // The plan refuses a resend with no main queue, so this is the
        // backstop, not the gate: nothing is sent and nothing deleted.
        let Some(main_url) = main_url else {
            return Err(
                "the plan carried no main queue to resend to — nothing was sent or deleted"
                    .to_string(),
            );
        };
        // Send first, delete second. The other order can lose the
        // message outright if the send fails; this order can duplicate
        // it, and a duplicate in a worker queue is the recoverable
        // failure — sqsd tasks are retried by design.
        // WITH the attributes. Sending the body alone moved a husk:
        // for a cron-style task the body is the fixed literal
        // "elasticbeanstalk scheduled job" and every fact about which
        // task it was — `beanstalk.sqsd.task_name`, `.path`,
        // `.scheduled_time` — lives in the attributes. A resend that
        // dropped them delivered something the worker daemon has no
        // path to route to.
        client
            .send_message(main_url, &msg.body, &msg.attributes)
            .await
            .map_err(|e| format!("resend failed, message left in the dead-letter queue: {e}"))?;
    }
    client
        .delete_message(url, &msg.receipt_handle)
        .await
        .map_err(|e| {
            if verb == WriteVerb::DlqResend {
                // The send already succeeded. The message is NOW on
                // the main queue and the original is still in the
                // dead-letter queue, so this half-failure is not the
                // same as "nothing happened" — and the batch report
                // that calls it `ok: false` invites exactly the retry
                // that mints another copy, one per attempt.
                //
                // Found twice by the same reviewer, in two separate
                // reviews, because the bare error said none of this.
                format!(
                    "RESENT BUT NOT REMOVED: the copy reached the main queue and \
                     sqsd will pick it up, but deleting the original from the \
                     dead-letter queue failed ({e}). A duplicate now exists. DO NOT \
                     resend this id again — each attempt adds another copy. Delete \
                     it from the dead-letter queue instead, or leave it: the work \
                     itself is already on its way."
                )
            } else {
                e.to_string()
            }
        })?;
    // Handed back so the caller can hold it briefly. A resend returns
    // nothing: the message still exists, on the main queue, so there
    // is nothing to recover and offering one would be a lie.
    Ok(captures_for_undo(verb).then_some(msg.clone()))
}

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
const UNDO_CAPACITY: usize = 20;

/// A message ebman destroyed and can still put back.
#[derive(Debug, Clone)]
pub(super) struct DeletedMessage {
    pub env: String,
    pub queue_url: String,
    pub original_id: String,
    pub task: Option<String>,
    pub body: String,
    pub attributes: Vec<(String, String, String)>,
    pub at: tokio::time::Instant,
}

impl Server {
    /// Hold a destroyed message for [`UNDO_WINDOW_SECS`], and say how
    /// long the caller has.
    ///
    /// Returns the window rather than nothing so the dispatch result
    /// can state it. An undo nobody is told about is not an undo.
    pub(super) async fn remember_deleted(
        &self,
        env: &str,
        queue_url: Option<String>,
        msg: crate::aws::QueueMessage,
    ) -> Option<u64> {
        // `None`, not `Some(0)`. A zero-second window is a claim that
        // the message was held and has already expired; not holding it
        // at all is a different fact, and the one an agent needs if it
        // is about to tell someone the delete is reversible.
        let queue_url = queue_url?;
        let mut buf = self.deleted.lock().await;
        buf.retain(|d: &DeletedMessage| d.at.elapsed().as_secs() < UNDO_WINDOW_SECS);
        if buf.len() >= UNDO_CAPACITY {
            buf.remove(0);
        }
        buf.push(DeletedMessage {
            env: env.to_string(),
            queue_url,
            original_id: msg.id,
            task: msg.task.as_ref().and_then(|t| t.name.clone()),
            body: msg.body,
            attributes: msg.attributes,
            at: tokio::time::Instant::now(),
        });
        Some(UNDO_WINDOW_SECS)
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

const RETIRED_TOKEN_MEMORY: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum WriteVerb {
    Deploy,
    Restart,
    Rebuild,
    Terminate,
    SetOption,
    /// Move one dead-lettered message back to the main queue.
    DlqResend,
    /// Delete one dead-lettered message.
    DlqDelete,
    /// Empty the dead-letter queue.
    ///
    /// The bluntest write here, and arguably more destructive than
    /// `Terminate`: an environment can be rebuilt from its
    /// configuration, and a purged message is gone. It also takes
    /// anything that arrived AFTER the plan was made.
    DlqPurge,
}

/// What this action destroys that cannot be got back.
///
/// A plan describes the operation; the reason to refuse usually lives
/// outside it. That gap is not closed by more detail about the
/// mechanics — a prompt fully specified about what it does and silent
/// about what it costs reads as complete, and a prompt that looks
/// complete discourages the pause in which the operator remembers what
/// it does not contain.
///
/// The type specimen: a dead-lettered message whose plan named the id,
/// the task, the age and the receive count, all correct, and which was
/// the only live fixture for a feature shipped an hour earlier. No
/// amount of detail about the message would have surfaced that. "It is
/// destroyed, and ebman keeps no copy" might have.
///
/// This is ARCHITECTURE.md rule 6 — a result must carry its own
/// negative space — applied to a plan rather than a result.
///
/// The variance across verbs is deliberate and is half the value. An
/// operator who reads "nothing that cannot be redone" for `restart`
/// and "no undelete, and ebman keeps no copy" for `dlq_delete` learns
/// the difference between them without being told.
///
/// `dlq_visible` is SQS's `ApproximateNumberOfMessages`, and is
/// reported as approximate rather than as a count. Saying "it is the
/// only message in the queue" would be a firmer claim than the source
/// supports, which is the failure this function exists to avoid.
/// `named` is how many messages this plan acts on — 0 for every verb
/// that does not name messages individually. It is separate from
/// `dlq_visible` (how many are in the queue) because they answer
/// different questions, and a batch line that said only "the message
/// is destroyed" would understate a plan for nine of them in the one
/// sentence written to stop exactly that.
pub(super) fn forecloses(verb: WriteVerb, dlq_visible: Option<i64>, named: usize) -> String {
    let queue_note = || match dlq_visible {
        Some(1) => " SQS reports 1 message in the queue, approximately.".to_string(),
        Some(n) => format!(" SQS reports about {n} messages in the queue."),
        None => String::new(),
    };
    match verb {
        WriteVerb::DlqDelete if named > 1 => format!(
            "All {named} messages are destroyed in SQS, which has no undelete. ebman \
             holds copies in memory for {}s — `dlq_undo` can put each back, with a new \
             message id and a receive count reset to 0 — and after that nothing can.{}",
            UNDO_WINDOW_SECS,
            queue_note()
        ),
        WriteVerb::DlqDelete => format!(
            "The message is destroyed in SQS, which has no undelete. ebman holds \
             a copy in memory for {}s — `dlq_undo` can put it back, with a new \
             message id and a receive count reset to 0 — and after that nothing \
             can.{}",
            UNDO_WINDOW_SECS,
            queue_note()
        ),
        WriteVerb::DlqResend if named > 1 => format!(
            "All {named} messages leave the dead-letter queue. Any that fails again \
             dead-letters again, carrying its receive count forward — so this is \
             reversible only in the sense that the messages still exist.{}",
            queue_note()
        ),
        WriteVerb::DlqResend => format!(
            "The message leaves the dead-letter queue. If it fails again it \
             dead-letters again, carrying its receive count forward — so this \
             is reversible only in the sense that the message still exists.{}",
            queue_note()
        ),
        WriteVerb::DlqPurge => format!(
            "Every message in the queue is destroyed, including any that arrive \
             between now and your confirmation — those are not in this plan and \
             cannot be. None of them can be recovered.{}",
            queue_note()
        ),
        WriteVerb::Terminate => "The environment and its instances are destroyed. Its saved \
             configuration remains, so an environment can be rebuilt from \
             it — but anything written to instance-local storage, and this \
             environment's CNAME while it is gone, are not recoverable."
            .to_string(),
        WriteVerb::Rebuild => "Every instance is replaced. Anything written to instance-local \
             storage is not recoverable; the environment and its \
             configuration survive."
            .to_string(),
        WriteVerb::Deploy => "The running version stops serving. It remains an application \
             version and can be redeployed, so this is recoverable — the \
             cost is the time in between."
            .to_string(),
        WriteVerb::Restart => "In-flight requests on the instances are dropped. Nothing else — \
             no state is lost and nothing here needs undoing."
            .to_string(),
        WriteVerb::SetOption => "The previous values are replaced. ebman does not keep them: \
             read them back with `get_option_settings` before approving if you need \
             to restore them. A change that triggers an environment update will \
             bounce instances to apply it."
            .to_string(),
    }
}

impl WriteVerb {
    /// Every verb. Exhaustiveness is pinned by a guard, not by hope:
    /// `every_write_verb_round_trips_through_the_tool_table` compares
    /// this against the descriptor table, so a variant added to one
    /// and not the other fails the build.
    #[cfg(test)]
    pub(super) const ALL: [WriteVerb; 8] = [
        WriteVerb::Deploy,
        WriteVerb::Restart,
        WriteVerb::Rebuild,
        WriteVerb::Terminate,
        WriteVerb::SetOption,
        WriteVerb::DlqResend,
        WriteVerb::DlqDelete,
        WriteVerb::DlqPurge,
    ];

    /// The MCP tool name this verb is advertised as.
    ///
    /// Separate from `label()`, which is the AUDIT name: the audit
    /// vocabulary matches what the TUI dispatches under (`dlq-purge`)
    /// while the tool name is what a client calls (`dlq_purge`). Both
    /// are needed and conflating them would make either the scope flag
    /// or `ebman audit --action` wrong.
    pub(super) fn tool_name(self) -> &'static str {
        match self {
            WriteVerb::Deploy => "deploy",
            WriteVerb::Restart => "restart",
            WriteVerb::Rebuild => "rebuild",
            WriteVerb::Terminate => "terminate",
            WriteVerb::SetOption => "set_option",
            WriteVerb::DlqResend => "dlq_resend",
            WriteVerb::DlqDelete => "dlq_delete",
            WriteVerb::DlqPurge => "dlq_purge",
        }
    }

    fn label(self) -> &'static str {
        match self {
            WriteVerb::Deploy => "Deploy",
            WriteVerb::Restart => "Restart",
            WriteVerb::Rebuild => "Rebuild",
            WriteVerb::Terminate => "Terminate",
            WriteVerb::SetOption => "SetOption",
            // The labels `spawn_dlq` already audits under, so a TUI
            // purge and an MCP purge correlate under
            // `ebman audit --action dlq-purge`.
            WriteVerb::DlqResend => "dlq-resend",
            WriteVerb::DlqDelete => "sqs-delete",
            WriteVerb::DlqPurge => "dlq-purge",
        }
    }
}

/// The message ids a DLQ plan was asked for.
///
/// Accepts `message_id` (one) or `message_ids` (several) and refuses
/// every ambiguous shape rather than picking a reading. Each refusal
/// below is a case where guessing would act on a set the agent did not
/// ask for and the operator would approve without knowing:
///
/// - **Both keys.** No sane precedence exists. Taking `message_ids`
///   silently drops `message_id`; taking `message_id` silently drops a
///   list. Either way the confirmation names a set nobody requested.
/// - **Neither, or an empty list.** A DLQ write with no target is not
///   a no-op to approve — it is a malformed request, and answering it
///   with "dispatched: 0 messages" reads as success.
/// - **Duplicates.** The operator is told "5 messages"; four exist.
///   Deduplicating silently would be worse than refusing, because the
///   count in the dialog is the one thing they are being asked about.
/// - **Over the cap.** See `DLQ_BATCH_CAP`.
/// - **A non-string element.** An id that arrived as a number or null
///   is a client bug, and coercing it invents an id to go looking for.
///
/// The descriptor's schema is NOT a gate, so every check below is
/// load-bearing rather than belt-and-braces.
///
/// `message_ids` declares `maxItems: 10` and `items: {type: string}`,
/// and a live run through Claude Code sent both a bare integer and
/// eleven elements straight past them to this function. Client-side
/// validation of tool arguments is optional and that client does not
/// do it. Do not drop a check here as redundant with the schema.
fn requested_message_ids(args: &Value) -> Result<Vec<String>, String> {
    let one = arg_str(args, "message_id");
    let many = args.get("message_ids").filter(|v| !v.is_null());

    let ids: Vec<String> = match (one, many) {
        (Some(_), Some(_)) => {
            return Err(
                "give either 'message_id' or 'message_ids', not both — there is no \
                        order of precedence that would not silently drop one of them"
                    .into(),
            );
        }
        (Some(id), None) => vec![id],
        (None, Some(v)) => {
            let arr = v
                .as_array()
                .ok_or("'message_ids' must be an array of message ids")?;
            let mut out = Vec::with_capacity(arr.len());
            for (i, item) in arr.iter().enumerate() {
                let id = item.as_str().ok_or_else(|| {
                    format!("'message_ids[{i}]' is not a string — every id must be one")
                })?;
                out.push(id.to_string());
            }
            out
        }
        (None, None) => {
            return Err(
                "'message_id' (one) or 'message_ids' (several) is required, from \
                        `worker_queues` with peek"
                    .into(),
            );
        }
    };

    if ids.is_empty() {
        return Err("'message_ids' is empty — name at least one message".into());
    }
    if ids.len() > DLQ_BATCH_CAP {
        return Err(format!(
            "{} messages is more than one plan may name ({DLQ_BATCH_CAP}). The limit is \
             there so the operator can read the list before approving it. Name fewer, \
             or use `dlq_purge` if the intent is to empty the queue — that is one \
             deliberate action with an honest foreclosure line, not a long list nobody \
             reads.",
            ids.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for id in &ids {
        if !seen.insert(id.as_str()) {
            return Err(format!(
                "'{id}' is named twice. The confirmation would say {} messages and \
                 {} exist — refused rather than deduplicated, because that count is \
                 what the operator is being asked to approve.",
                ids.len(),
                seen.len()
            ));
        }
    }
    Ok(ids)
}

/// A refusal you can only hold if it has been recorded.
///
/// The point of the private field: outside this module `Audited`
/// cannot be constructed, and inside it the only constructors are the
/// two below — one that writes the `stage=refused` line, and one that
/// accepts a line the shared CLI funnel already wrote. So a
/// `WriteError::Refused` is proof an audit line exists.
///
/// This does not make the bug impossible. A new gate can still return
/// `WriteError::Invalid` for something that is really a policy
/// refusal. What it does is convert an invisible OMISSION into a
/// visible MISCATEGORISATION: the author has to name which kind it is,
/// and the wrong choice is a word in the diff rather than the absence
/// of one. Every other invariant here works the same way —
/// `rebuild_view`, the generation guards, match-arm order — none are
/// impossible to violate, all are made visible.
mod audited {
    /// A recorded refusal. The message is for the agent; the audit
    /// line is already on disk.
    #[derive(Debug, Clone)]
    pub(in crate::cli::mcp) struct Audited(String);

    impl Audited {
        /// Record a refusal and render it. The ONLY way to make an
        /// `Audited` from nothing.
        ///
        /// `demo` suppresses the write, not the refusal: a demo server
        /// reads the real cross-process freeze marker, so its verdict
        /// is genuine while its fleet is not, and it must leave no
        /// line in a real operator's log.
        #[allow(clippy::too_many_arguments)]
        pub(super) fn record(
            demo: bool,
            profile: Option<&str>,
            region: &str,
            action: &str,
            env: &str,
            rule: &str,
            remedy: &str,
            message: String,
        ) -> Self {
            if !demo {
                crate::audit::append_action_refused(
                    None, profile, region, action, env, rule, remedy,
                );
            }
            Audited(message)
        }

        /// Add context to the message without losing the proof.
        ///
        /// Inside the module, so the private field stays private and a
        /// `Refused` still means a line was written.
        pub(super) fn map_message(self, f: impl FnOnce(String) -> String) -> Self {
            Audited(f(self.0))
        }

        pub(super) fn message(&self) -> &str {
            &self.0
        }

        pub(super) fn into_message(self) -> String {
            self.0
        }
    }
}

use audited::Audited;

/// Why a write call failed.
///
/// The distinction is the audit obligation. `Refused` means a policy
/// stopped this — a pin, a freeze, read-only, an out-of-scope verb, an
/// operator declining — and every one of those leaves a
/// `stage=refused` line, because a blocked write and nobody trying
/// look identical afterwards otherwise. `Invalid` means nothing was
/// refused: a missing argument, an unknown environment, a spent token,
/// another write already in flight. There was no attempt to record.
///
/// Replaces a bare `String`, where the two were indistinguishable and
/// a new gate could return `Err("nope".into())` and silently audit
/// nothing. That happened in 0.40 and again in 0.42 — the latter found
/// by classifying these sites to write this type.
#[derive(Debug)]
pub(super) enum WriteError {
    /// Nothing was refused by policy; no audit line.
    Invalid(String),
    /// A policy refused this. The payload is proof it was recorded.
    Refused(Audited),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Invalid(m) => f.write_str(m),
            WriteError::Refused(a) => f.write_str(a.message()),
        }
    }
}

impl WriteError {
    /// Add context, preserving which kind it is.
    pub(super) fn with_context(self, f: impl FnOnce(String) -> String) -> Self {
        match self {
            WriteError::Invalid(m) => WriteError::Invalid(f(m)),
            WriteError::Refused(a) => WriteError::Refused(a.map_message(f)),
        }
    }

    pub(super) fn into_message(self) -> String {
        match self {
            WriteError::Invalid(m) => m,
            WriteError::Refused(a) => a.into_message(),
        }
    }
}

// `?` on the many `ok_or("'env' is required")` sites keeps working,
// and lands on Invalid — the right default, since a refusal now has to
// be written deliberately.
impl From<&str> for WriteError {
    fn from(m: &str) -> Self {
        WriteError::Invalid(m.to_string())
    }
}
impl From<String> for WriteError {
    fn from(m: String) -> Self {
        WriteError::Invalid(m)
    }
}

/// Whose credentials the write would go out under.
///
/// An enum rather than `Option<String>` so the failure has to be
/// rendered. A plan that simply omitted the identity when the lookup
/// failed would show the operator nothing where there should be
/// something, and "no identity to show" reads as "this is fine" —
/// ARCHITECTURE.md rule 6, pinned by the type rather than by everyone
/// remembering.
///
/// `sts:GetCallerIdentity` can be denied by policy while Elastic
/// Beanstalk works perfectly, so a failed lookup does NOT refuse the
/// plan. Same call as `ProbeOutcome` in `src/cli/lint.rs`: a denied
/// probe is "could not check", never a clean bill of health, and
/// never a reason to stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum CallerIdentity {
    Known { arn: String, account: String },
    Unknown { why: String },
}

impl CallerIdentity {
    /// The line the operator reads in the confirmation.
    ///
    /// Full ARN, not a shortened form. `assumed-role/Deploy/x` and
    /// `assumed-role/Admin/x` differ by one word in the middle, and
    /// the account number — the part that says WHICH account this is
    /// about to happen in — is only in the full form.
    pub(super) fn line(&self) -> String {
        match self {
            CallerIdentity::Known { arn, .. } => format!("as {arn}"),
            CallerIdentity::Unknown { why } => {
                format!("as an UNKNOWN identity — ebman could not determine it ({why})")
            }
        }
    }

    /// The plan's `identity` field.
    pub(super) fn json(&self) -> String {
        match self {
            CallerIdentity::Known { arn, account } => format!(
                "{{\"arn\":{},\"account\":{}}}",
                util::json_string(arn),
                util::json_string(account)
            ),
            // Null AND a reason. A bare null says "there is no
            // identity", which is never true of a call that is about
            // to be made with one.
            CallerIdentity::Unknown { why } => {
                format!("null,\"identity_error\":{}", util::json_string(why))
            }
        }
    }
}

/// The verb-dependent half of a plan.
///
/// A struct rather than a six-tuple: every field here is optional or
/// empty for most verbs, and positional returns of five `None`s and a
/// `String` are read wrong exactly once before someone swaps two of
/// them.
struct PlanDetails {
    version: Option<String>,
    settings: Vec<(String, String, String)>,
    /// Pre-rendered JSON fragment, spliced into the plan body.
    plan_extra: String,
    dlq_targets: Vec<DlqTarget>,
    dlq_url: Option<String>,
    /// The main queue this DLQ drains to, as EB reported it.
    dlq_main_url: Option<String>,
    /// SQS's `ApproximateNumberOfMessages`, for the foreclosure line.
    dlq_visible: Option<i64>,
}

/// How many dead-lettered messages one plan may name.
///
/// The number exists so the operator can *read* the list. Four
/// messages enumerate; two hundred do not, and a plan that summarises
/// — "200 messages matching X" — asks for approval of something
/// nobody has read. That is the appearance of control without the
/// substance, which this whole surface is built to avoid.
///
/// Ten, because that is also what one `worker_queues` peek shows: an
/// agent cannot name a message it has not seen, so a cap above the
/// peek would be unreachable by any honest path.
///
/// Above it the plan is REFUSED rather than truncated. Truncating
/// would dispatch a subset while reporting the whole, which is the
/// worst available outcome — the operator approves ten when twelve
/// were asked for. The refusal names the cap and points at
/// `dlq_purge`, which is one deliberate action carrying one honest
/// foreclosure line.
/// The longest `set_option` namespace, name or value a plan may carry.
///
/// Not a limit on what Elastic Beanstalk accepts — a limit on what an
/// operator can be asked about. The dialog shows setting values WHOLE
/// (see `sanitize_whole`), because truncating one hides its operative
/// end: `https://payments.example/callback/<filler>@evil.example/x`
/// reads as a benign host and applies another, the `@` sitting past
/// the ellipsis. Refusing above a readable size is the same rule
/// `DLQ_BATCH_CAP` applies to a list, pointed at a single field.
///
/// 256 is generous for the things that legitimately appear here —
/// URLs, connection strings, ARNs — and still fits a dialog.
pub(super) const SET_OPTION_FIELD_MAX: usize = 256;

pub(super) const DLQ_BATCH_CAP: usize = 10;

/// One dead-lettered message a plan names.
///
/// Id and task travel together because they answer different questions
/// and the audit needs both: the id says WHICH message, the task says
/// what it was. Keeping them as parallel `Vec`s invites the classic
/// mismatch where a filtered id list is paired with an unfiltered task
/// list and every line in the log names the wrong task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DlqTarget {
    /// The message id — deliberately NOT the receipt handle. See the
    /// note on `PendingWrite::dlq_targets`.
    pub id: String,
    pub task: String,
}

pub(super) struct PendingWrite {
    pub token: String,
    pub verb: WriteVerb,
    pub env: String,
    pub version: Option<String>,
    pub settings: Vec<(String, String, String)>,
    pub profile: Option<String>,
    pub region: Option<String>,
    pub expires_at: tokio::time::Instant,
    /// SQS's `ApproximateNumberOfMessages` at plan time, for the
    /// purge dialog's foreclosure line. Without it the operator is
    /// asked to approve destroying "every message in the queue" with
    /// no indication whether that is one or twelve thousand.
    pub dlq_visible: Option<i64>,
    /// Whose credentials this would go out under, resolved at plan
    /// time so the confirmation can name it. Resolved then rather than
    /// at dispatch because the confirmation is the only moment an
    /// operator can act on it.
    pub caller: CallerIdentity,
    /// Terminate only: one `confirm_name` mismatch keeps the token
    /// alive for a single retry; the second drops the plan.
    pub name_retry_used: bool,
    /// DLQ resend / delete: the messages this plan names.
    ///
    /// A `Vec` rather than an `Option`, because one and several differ
    /// only in length and a separate single-message path would be a
    /// second implementation of the same protocol — which is where the
    /// two would drift. Empty for every non-DLQ verb; a plan for
    /// resend or delete is refused before it is built if this would be
    /// empty.
    ///
    /// Each carries the task name for the audit line. A log that
    /// records "a message was deleted from poly-batch" and cannot say
    /// which one, or what it was, answers neither question an operator
    /// asks afterwards.
    ///
    /// Ids, deliberately — NOT receipt handles.
    ///
    /// The ID, deliberately — NOT the receipt handle. SQS deletes by
    /// handle, and a handle is only valid while the message is
    /// invisible: the peek that issues one uses a 5-second visibility
    /// timeout while a confirm token lives 60, so a handle captured at
    /// plan time is dead for 55 of the 60 seconds the plan stays
    /// confirmable. Carrying the handle would fail almost always; the
    /// alternative that "works" is re-receiving at confirm and deleting
    /// whatever is at the head of the queue now, which removes a
    /// different message than the plan named and says nothing about it.
    ///
    /// So confirm re-receives and finds THESE ids among what comes
    /// back, reporting per message rather than failing the batch: one
    /// message consumed in the interval says nothing about the other
    /// nine, and refusing all ten would force a re-plan against a
    /// queue that has moved again.
    pub dlq_targets: Vec<DlqTarget>,
    /// The dead-letter queue URL resolved at plan time.
    pub dlq_url: Option<String>,
    /// Resend only: the main queue the message goes back to, from the
    /// same `describe_worker_queues` answer as `dlq_url`.
    ///
    /// Taken from EB, never derived from the DLQ's name. This used to
    /// strip a `-dlq` suffix and otherwise return the DLQ url itself —
    /// so for a dead-letter queue EB reported under any other name, a
    /// resend put the message straight back into the DLQ, deleted the
    /// original, and reported success. The work never reached the
    /// worker. The TUI always used the reported main queue.
    pub dlq_main_url: Option<String>,
}

/// Token TTL — long enough for an agent round-trip, short enough
/// that a stale plan can't be confirmed against changed reality.
const CONFIRM_TTL_SECS: u64 = 60;

/// `set_option` per-call cap (spec-locked).
const SET_OPTION_MAX: usize = 10;

/// Single-use token: uniqueness is what matters (the agent receives
/// it; see module doc), sourced from pid + monotonic counter + nanos
/// through sha256.
fn mint_token() -> String {
    use sha2::{Digest, Sha256};
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut h = Sha256::new();
    h.update(std::process::id().to_le_bytes());
    h.update(n.to_le_bytes());
    h.update(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .subsec_nanos()
            .to_le_bytes(),
    );
    let digest = h.finalize();
    digest[..16].iter().map(|b| format!("{b:02x}")).collect()
}

/// The write verbs `--allow-writes` can name.
///
/// Derived from the descriptor table rather than a second list, so a
/// verb cannot be addable-but-unknown or known-but-unaddable. A guard
/// pins the two together.
pub(super) fn write_verb_names() -> Vec<String> {
    write_tool_descriptors()
        .iter()
        .filter_map(|d| d.get("name").and_then(|n| n.as_str()))
        .filter(|n| *n != CONFIRM_TOOL && *n != UNDO_TOOL)
        .map(str::to_string)
        .collect()
}

/// The second phase of every write, and not a verb.
///
/// It dispatches whatever a plan already authorised, so it is neither
/// separately grantable nor separately withholdable: a scope that
/// advertised `dlq_delete` without this would let an agent plan a
/// delete it could never confirm.
pub(super) const CONFIRM_TOOL: &str = "confirm_action";

/// Also not a verb, and for the same reason as `confirm_action`: it is
/// not an independently grantable capability. `dlq_undo` can only ever
/// put back something a delete already removed under a grant, so
/// granting it separately would mean nothing, and withholding it from
/// someone who holds `dlq_delete` would mean giving them the
/// destruction without the remedy.
pub(super) const UNDO_TOOL: &str = "dlq_undo";

/// Tool descriptors for the write surface — appended to tools/list
/// ONLY under the verbs `--allow-writes` granted (spec: the listing is
/// honest).
pub(super) fn write_tool_descriptors() -> Vec<Value> {
    let batch_note = format!("ONE CALL COVERS SEVERAL: pass `message_ids` (an array, up to {DLQ_BATCH_CAP}) to handle a set in one plan and ONE confirmation. Prefer it over calling this repeatedly — each plan asks the operator separately, and a person answering the same dialog eight times stops reading it. Over {DLQ_BATCH_CAP} is refused rather than truncated: the cap is what keeps the list readable, and `dlq_purge` is the action for emptying a queue. Give `message_id` or `message_ids`, not both.");
    let confirm_note = "TWO-PHASE: this tool DISPATCHES NOTHING. It validates and returns {pending:true, confirm_token, plan}; you must surface the plan, then call confirm_action with the token (60s TTL, single-use) to dispatch. Dispatch-only — poll the read tools for progress.";
    vec![
        json!({
            "name": "deploy",
            "description": format!("Deploy an existing application version to an environment. {confirm_note}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "version": {"type": "string", "description": "Existing application version label (required)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env", "version"]
            }
        }),
        json!({
            "name": "restart",
            "description": format!("Restart the app server on an environment's instances. {confirm_note}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env"]
            }
        }),
        json!({
            "name": "rebuild",
            "description": format!("Rebuild an environment (replaces its resources). {confirm_note}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env"]
            }
        }),
        json!({
            "name": "terminate",
            "description": format!("TERMINATE an environment — destructive and irreversible. {confirm_note} Additionally, confirm_action requires confirm_name equal to the env name (one retry per token) AND — on a client that can be asked — the OPERATOR must type the environment name into the confirmation dialog themselves. Two different checks: yours binds you to the planned environment, theirs binds a person to it. If their client cannot render a text field, this verb cannot be confirmed there at all."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env"]
            }
        }),
        json!({
            "name": "set_option",
            "description": format!("Update up to {SET_OPTION_MAX} option settings on one environment. Namespaces must already exist in the env's configuration (no cross-env blast). {confirm_note} The plan shows old -> new per setting; old env-var values are redacted per the standing contract."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "settings": {
                        "type": "array",
                        "description": "Settings to apply (max 10)",
                        "items": {
                            "type": "object",
                            "properties": {
                                "namespace": {"type": "string"},
                                "name": {"type": "string"},
                                "value": {"type": "string"}
                            },
                            "required": ["namespace", "name", "value"]
                        }
                    },
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env", "settings"]
            }
        }),
        json!({
            "name": "dlq_undo",
            "description": "Put back a dead-lettered message THIS server deleted, within 10 minutes. Call with no arguments to list what is still recoverable. Single-phase — no plan/confirm — because it is the least destructive action here and is reached for under time pressure. CAVEATS: held in memory by this server only, so a restart loses them and nothing deleted by another process is here; purges are never recoverable; and the restore is a re-send, so the message id changes, receive_count resets to 0 and the enqueue time becomes now. Body and attributes come back verbatim.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "message_id": {"type": "string", "description": "The id reported when it was deleted. Omit to list what is recoverable."},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                }
            }
        }),
        json!({
            "name": "confirm_action",
            "description": "Phase 2 of every write tool: dispatch the pending plan identified by confirm_token (single-use, 60s TTL). terminate additionally requires confirm_name equal to the plan's env name. Writes are serialized — a confirm while another dispatch is in flight is refused.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "confirm_token": {"type": "string", "description": "Token from the write tool's pending plan (required)"},
                    "confirm_name": {"type": "string", "description": "terminate only: must equal the env name"}
                },
                "required": ["confirm_token"]
            }
        }),
        json!({
            "name": "dlq_resend",
            "description": format!("Move dead-lettered messages back to the main worker queue, so sqsd retries the task. Identify them by id from `worker_queues` with peek. {batch_note} {confirm_note} The plan names each message id and task; confirm re-reads the queue and acts on THOSE ids, reporting per message and refusing any that is no longer there — a receipt handle cannot survive the token window, and acting on whatever is at the head of the queue instead would silently target a different message. Sends before deleting: the other order can lose the message if the send fails, while this order can duplicate it, and a duplicate in a worker queue is the recoverable failure."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "message_id": {"type": "string", "description": "One message, from `worker_queues` with peek. Give this OR message_ids."},
                    "message_ids": {"type": "array", "items": {"type": "string"}, "maxItems": DLQ_BATCH_CAP, "description": format!("Several messages in one plan and one confirmation, up to {DLQ_BATCH_CAP}. Give this OR message_id.")},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                // Only `env` is structurally required: exactly one of
                // the two id forms must be given, which JSON Schema
                // cannot express here and the tool enforces with a
                // message that says which shapes are wrong and why.
                "required": ["env"]
            }
        }),
        json!({
            "name": "dlq_delete",
            "description": format!("Delete dead-lettered messages. Identify them by id from `worker_queues` with peek. {batch_note} IRREVERSIBLE: unlike restarting or rebuilding an environment, a deleted message cannot be recovered — there is no configuration to rebuild it from. {confirm_note} The plan names each message id and task; confirm re-reads the queue and acts on THOSE ids, reporting per message and refusing any that is no longer there."),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "message_id": {"type": "string", "description": "One message, from `worker_queues` with peek. Give this OR message_ids."},
                    "message_ids": {"type": "array", "items": {"type": "string"}, "maxItems": DLQ_BATCH_CAP, "description": format!("Several messages in one plan and one confirmation, up to {DLQ_BATCH_CAP}. Give this OR message_id.")},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                // Only `env` is structurally required: exactly one of
                // the two id forms must be given, which JSON Schema
                // cannot express here and the tool enforces with a
                // message that says which shapes are wrong and why.
                "required": ["env"]
            }
        }),
        json!({
            "name": "dlq_purge",
            "description": format!("Empty the dead-letter queue. On a client that can be asked, the OPERATOR must type the environment name into the confirmation dialog — the same strict-typed confirm the TUI demands for a purge, and it fails closed if their client cannot render a text field. THE MOST DESTRUCTIVE TOOL HERE — arguably more so than `terminate`: an environment can be rebuilt from its configuration, and purged messages are gone. It also removes anything that arrived AFTER the plan was made, so the count in the plan is what was there then, not what will be deleted. Prefer `dlq_delete` when you know which message you mean. {confirm_note}"),
            "inputSchema": {
                "type": "object",
                "properties": {
                    "env": {"type": "string", "description": "Environment name (required)"},
                    "profile": {"type": "string"},
                    "region": {"type": "string"}
                },
                "required": ["env"]
            }
        }),
    ]
}

impl Server {
    /// Test seam: reach the plan gate without a tools/call frame.
    #[cfg(test)]
    pub(super) async fn tool_write_plan_for_tests(
        &self,
        verb: WriteVerb,
        args: &Value,
    ) -> Result<String, String> {
        self.tool_write_plan(verb, args)
            .await
            .map_err(WriteError::into_message)
    }

    /// Phase 1 for every write verb: shared gates (verb in scope,
    /// writes enabled, not mid-dispatch, freeze, pins, env exists),
    /// verb-specific validation, then a pending plan + token.
    /// The dead-letter arm of a plan: resolve the queue, and for
    /// resend/delete the individual messages.
    ///
    /// Its own function because it is the only arm that talks to a
    /// second AWS service, the only one that can name several targets,
    /// and the only one whose plan can be refused for the shape of the
    /// request rather than the state of the world. At 110 lines inside
    /// a match it was most of what `resolve_plan_details` appeared to
    /// be.
    async fn resolve_dlq_plan(
        &self,
        verb: WriteVerb,
        args: &Value,
        env: &crate::aws::Environment,
        profile: &Option<String>,
        out: &mut PlanDetails,
    ) -> Result<(), String> {
        let profile = profile.clone();
        let plan_extra;
        let mut dlq_targets: Vec<DlqTarget> = Vec::new();
        // Resolve the queue at plan time so the plan can say
        // WHICH queue, and so a web-tier env is refused here
        // rather than at confirm.
        // Demo resolves from the fixture and never builds a
        // client. It used to fall straight through to the AWS
        // calls below, against this module's own promise that
        // demo plans synthetically: `peek_messages` is not
        // side-effect-free — it increments `receive_count` on
        // every message it returns — so `--demo` could alter
        // metadata on a live queue.
        let queues = if matches!(self.backend, Backend::Demo) {
            demo_fixture::worker_queues_for_env(&env.name)
        } else {
            let client = self.client(args).await?;
            client
                .describe_worker_queues(&env.application, &env.name)
                .await
                .map_err(|e| tool_error(&profile, "describe_worker_queues", &e.to_string()))?
        };
        // The same predicate the peek paths use, not a fourth copy of
        // it: a url ebman guessed for a queue that never answered is
        // not a queue to plan against either.
        let url = super::tools::answered_dlq_url(&queues)
            .ok_or_else(|| format!("env '{}' has no dead-letter queue", env.name))?
            .to_string();
        // A resend needs somewhere to send TO, and that has to be the
        // queue EB reports — never one derived from the DLQ's name.
        if verb == WriteVerb::DlqResend && queues.main_url.is_none() {
            return Err(format!(
                "env '{}' has a dead-letter queue but its main queue could not be \
                 resolved, so a resend has nowhere to go. Nothing was planned.",
                env.name
            ));
        }

        let dlq_visible = queues.dlq_stats.as_ref().map(|s| s.visible);
        if verb == WriteVerb::DlqPurge {
            let visible = queues.dlq_stats.as_ref().map(|s| s.visible).unwrap_or(0);
            plan_extra = format!(
                ",\"queue\":{},\"messages_now\":{}",
                util::json_string(&url),
                visible
            );
        } else {
            // The messages must exist NOW, and the plan names
            // them by id. Confirm re-receives and matches those
            // ids — it cannot carry receipt handles, which
            // expire with the 5-second visibility timeout while
            // the token lives 60.
            let ids = requested_message_ids(args)?;
            // Peek wider than the batch: the ids came from a
            // peek of 10, but the queue has moved since and the
            // planned messages need not be in the first 10 this
            // time. Asking for more costs one more round trip
            // and is the difference between "not there" and
            // "not looked for".
            let pool = if matches!(self.backend, Backend::Demo) {
                demo_fixture::dlq_messages_for_env(&env.name)
            } else {
                self.client(args)
                    .await?
                    .peek_messages(&url, (DLQ_BATCH_CAP as i32) * 3)
                    .await
                    .map_err(|e| tool_error(&profile, "peek_messages", &e.to_string()))?
            };
            // Every id must resolve, or the plan does not
            // describe reality and the operator would be
            // approving a list partly made of things that are
            // not there. This is the one place a batch fails
            // whole: at DISPATCH a missing message is reported
            // per item, because by then the others have been
            // approved.
            let mut missing: Vec<String> = Vec::new();
            for id in &ids {
                match pool.iter().find(|m| &m.id == id) {
                    Some(msg) => dlq_targets.push(DlqTarget {
                        id: id.clone(),
                        task: msg
                            .task
                            .as_ref()
                            .and_then(|t| t.name.clone())
                            .unwrap_or_else(|| "(not an EB worker task)".into()),
                    }),
                    None => missing.push(id.clone()),
                }
            }
            if !missing.is_empty() {
                return Err(format!(
                    "not in the dead-letter queue right now: {} — re-run \
                         `worker_queues` with peek to see what is there. Nothing \
                         was planned; the other {} named {} not been acted on.",
                    missing.join(", "),
                    ids.len() - missing.len(),
                    if ids.len() - missing.len() == 1 {
                        "has"
                    } else {
                        "have"
                    }
                ));
            }
            plan_extra = format!(
                ",\"queue\":{},\"messages\":[{}],\"message_count\":{}",
                util::json_string(&url),
                dlq_targets
                    .iter()
                    .map(|t| format!(
                        "{{\"message_id\":{},\"task\":{}}}",
                        util::json_string(&t.id),
                        util::json_string(&t.task)
                    ))
                    .collect::<Vec<_>>()
                    .join(","),
                dlq_targets.len()
            );
        }
        out.dlq_url = Some(url);
        out.dlq_main_url = queues.main_url.clone();
        out.plan_extra = plan_extra;
        out.dlq_targets = dlq_targets;
        out.dlq_visible = dlq_visible;
        Ok(())
    }

    /// Who the write would go out as.
    ///
    /// Never fails: a denied `sts:GetCallerIdentity` becomes
    /// `Unknown` with the reason, because a policy can deny STS while
    /// Elastic Beanstalk works, and refusing the plan over a
    /// diagnostic would break exactly the scoped-IAM setups this is
    /// most useful to.
    async fn caller_identity(&self, args: &Value) -> CallerIdentity {
        if matches!(self.backend, Backend::Demo) {
            // A synthetic but well-formed ARN, so the demo renders the
            // same shape live does. A demo that showed nothing here
            // would be a demo you cannot use to check this.
            return CallerIdentity::Known {
                arn: "arn:aws:iam::123456789012:user/demo".into(),
                account: "123456789012".into(),
            };
        }
        let client = match self.client(args).await {
            Ok(c) => c,
            Err(e) => return CallerIdentity::Unknown { why: e },
        };
        match client.verify_identity().await {
            Ok(id) => match (id.caller_arn, id.account_id) {
                (Some(arn), Some(account)) => CallerIdentity::Known { arn, account },
                // STS answered without the fields. Reported rather
                // than papered over with an empty string, which would
                // render as `as ` and read as a rendering bug.
                _ => CallerIdentity::Unknown {
                    why: "sts:GetCallerIdentity returned no arn".into(),
                },
            },
            Err(e) => CallerIdentity::Unknown { why: e.to_string() },
        }
    }

    /// Resolve everything the plan needs that depends on the VERB.
    ///
    /// Split out of `tool_write_plan`, which was 346 lines of which
    /// this was 220 — the gates, the per-verb resolution and the
    /// rendering read as one function only because they happened to be
    /// adjacent. They have different reasons to change: a new verb
    /// touches this and nothing else, while a change to the gates or
    /// the token window touches the caller and none of this.
    ///
    /// Returns rather than mutating six locals across a 200-line
    /// match. The previous shape made "which arms set `dlq_url`?" a
    /// question you answered by reading all of them, and a arm that
    /// forgot one was indistinguishable from an arm that meant not to.
    async fn resolve_plan_details(
        &self,
        verb: WriteVerb,
        args: &Value,
        env: &crate::aws::Environment,
        profile: &Option<String>,
    ) -> Result<PlanDetails, String> {
        let profile = profile.clone();
        let mut version: Option<String> = None;
        let mut settings: Vec<(String, String, String)> = Vec::new();
        let mut plan_extra = String::new();
        let mut dlq_targets: Vec<DlqTarget> = Vec::new();
        let mut dlq_url: Option<String> = None;
        let mut dlq_main_url: Option<String> = None;
        // Captured for the foreclosure line, which needs to say how
        // much else is in the queue. Only the DLQ branch resolves it.
        let mut dlq_visible: Option<i64> = None;

        match verb {
            WriteVerb::Deploy => {
                let label = arg_str(args, "version").ok_or("'version' is required")?;
                let known = match self.backend {
                    Backend::Demo => demo_fixture::deploys_for_app(&env.application)
                        .iter()
                        .any(|v| v.label == label),
                    Backend::Aws => {
                        let client = self.client(args).await?;
                        client
                            .list_application_versions(&env.application)
                            .await
                            .map_err(|e| {
                                tool_error(&profile, "list_application_versions", &e.to_string())
                            })?
                            .iter()
                            .any(|v| v.label == label)
                    }
                };
                if !known {
                    return Err(format!(
                        "version '{label}' does not exist for application '{}'",
                        env.application
                    ));
                }
                plan_extra = format!(
                    ",\"current_version\":{},\"target_version\":{}",
                    util::json_string(&env.version_label),
                    util::json_string(&label),
                );
                version = Some(label);
            }
            WriteVerb::SetOption => {
                let raw = args
                    .get("settings")
                    .and_then(Value::as_array)
                    .ok_or("'settings' (array) is required")?;
                if raw.is_empty() {
                    return Err("'settings' is empty".into());
                }
                if raw.len() > SET_OPTION_MAX {
                    return Err(format!(
                        "set_option caps at {SET_OPTION_MAX} settings per call (got {})",
                        raw.len()
                    ));
                }
                for s in raw {
                    let ns = s.get("namespace").and_then(Value::as_str).unwrap_or("");
                    let name = s.get("name").and_then(Value::as_str).unwrap_or("");
                    let value = s.get("value").and_then(Value::as_str).unwrap_or("");
                    if ns.is_empty() || name.is_empty() {
                        return Err("each setting needs non-empty namespace and name".into());
                    }
                    // Refuse a field the dialog cannot show whole.
                    // Truncating in the dialog would let the operator
                    // approve a prefix while a different value
                    // dispatched — see `SET_OPTION_FIELD_MAX`.
                    if let Some((what, len)) = [("namespace", ns), ("name", name), ("value", value)]
                        .into_iter()
                        .map(|(w, v)| (w, v.chars().count()))
                        .find(|(_, len)| *len > SET_OPTION_FIELD_MAX)
                    {
                        return Err(format!(
                            "that {what} is {len} characters, more than the \
                             {SET_OPTION_FIELD_MAX} an operator can be shown in one \
                             line of a confirmation. The dialog shows setting values \
                             whole rather than truncated, because a shortened value \
                             can read as one thing and apply another — so a value \
                             too long to display is refused rather than abbreviated. \
                             Shorten it, or set it outside ebman."
                        ));
                    }
                    settings.push((ns.to_string(), name.to_string(), value.to_string()));
                }
                // Namespaces must already exist in the env's config —
                // the spec's no-cross-env-blast rule.
                let current: Vec<(String, String, String)> = match self.backend {
                    Backend::Demo => demo_fixture::option_settings_for(&env.name),
                    Backend::Aws => {
                        let client = self.client(args).await?;
                        client
                            .fetch_env_option_settings(&env.application, &env.name)
                            .await
                            .map_err(|e| {
                                tool_error(&profile, "fetch_env_option_settings", &e.to_string())
                            })?
                    }
                };
                let known_ns: std::collections::HashSet<&str> =
                    current.iter().map(|(ns, _, _)| ns.as_str()).collect();
                for (ns, _, _) in &settings {
                    if !known_ns.contains(ns.as_str()) {
                        return Err(format!(
                            "namespace '{ns}' is not present in {}'s configuration — refusing (set_option only touches existing namespaces)",
                            env.name
                        ));
                    }
                }
                // Plan rows: old -> new, old redacted per the
                // standing contract (the NEW value is echoed — the
                // agent supplied it).
                let rows: Vec<String> = settings
                    .iter()
                    .map(|(ns, name, new_v)| {
                        let old = current
                            .iter()
                            .find(|(cns, cn, _)| cns == ns && cn == name)
                            .map(|(_, _, v)| redact_option_value(ns, name, v, self.redact))
                            .unwrap_or_else(|| "(unset)".into());
                        format!(
                            "{{\"namespace\":{},\"name\":{},\"old\":{},\"new\":{}}}",
                            util::json_string(ns),
                            util::json_string(name),
                            util::json_string(&old),
                            util::json_string(new_v),
                        )
                    })
                    .collect();
                plan_extra = format!(",\"changes\":[{}]", rows.join(","));
            }
            WriteVerb::DlqResend | WriteVerb::DlqDelete | WriteVerb::DlqPurge => {
                let mut out = PlanDetails {
                    version: None,
                    settings: Vec::new(),
                    plan_extra: String::new(),
                    dlq_targets: Vec::new(),
                    dlq_url: None,
                    dlq_main_url: None,
                    dlq_visible: None,
                };
                self.resolve_dlq_plan(verb, args, env, &profile, &mut out)
                    .await?;
                plan_extra = out.plan_extra;
                dlq_targets = out.dlq_targets;
                dlq_url = out.dlq_url;
                dlq_main_url = out.dlq_main_url;
                dlq_visible = out.dlq_visible;
            }
            WriteVerb::Restart | WriteVerb::Rebuild | WriteVerb::Terminate => {}
        }

        Ok(PlanDetails {
            version,
            settings,
            plan_extra,
            dlq_targets,
            dlq_url,
            dlq_main_url,
            dlq_visible,
        })
    }

    pub(super) async fn tool_write_plan(
        &self,
        verb: WriteVerb,
        args: &Value,
    ) -> Result<String, WriteError> {
        // The VERB, not merely "writes are on". Unreachable via the
        // scoped table — an out-of-scope tool is not advertised — but a
        // client holding a cached list from a wider grant would
        // otherwise reach the body. Belt-and-braces, and the braces are
        // the ones that matter after a scope is narrowed.
        if !self.effective_scope().allows(verb.tool_name()) {
            return Err(self.refused_out_of_scope(
                verb,
                arg_str(args, "env").as_deref(),
                arg_str(args, "region").as_deref(),
            ));
        }
        if self.dispatching.load(std::sync::atomic::Ordering::SeqCst) {
            return Err("another write is in flight — wait for it to complete".into());
        }
        let env_name = arg_str(args, "env").ok_or("'env' is required")?;

        // Freeze + pin gate — run at plan time AND re-run at confirm,
        // because the 60s token window is long enough for an operator
        // to declare an incident between the two and the whole point of
        // the gates is to stop a write dispatching then.
        //
        // The FREEZE is what the re-run catches: it is re-read from
        // disk each time. `safety_cfg` is snapshotted at server
        // construction, so a *pin* added during a long-lived session is
        // not seen until restart. This comment used to claim it was.
        let profile = arg_str(args, "profile");
        if let Some(refused) = self.gate_refusal(
            &env_name,
            &profile,
            arg_str(args, "region").as_deref(),
            verb.label(),
        ) {
            return Err(WriteError::Refused(refused));
        }

        let envs = self.fetch_envs(args).await?;
        let env = envs
            .iter()
            .find(|e| e.name == env_name)
            .ok_or_else(|| format!("env '{env_name}' not found"))?
            .clone();

        let PlanDetails {
            version,
            settings,
            plan_extra,
            dlq_targets,
            dlq_url,
            dlq_main_url,
            dlq_visible,
        } = self
            .resolve_plan_details(verb, args, &env, &profile)
            .await?;

        // Resolved before the plan is rendered so the operator sees
        // it in the confirmation, which is the only moment they can
        // act on it.
        let caller = self.caller_identity(args).await;

        // Recent events give the plan operational context (3 max).
        let events_json = match self.backend {
            Backend::Demo => String::new(),
            Backend::Aws => {
                let client = self.client(args).await?;
                match client.list_events_for_env(&env.name, 3).await {
                    Ok(evs) => {
                        let rows: Vec<String> = evs
                            .iter()
                            .map(|e| {
                                format!(
                                    "{{\"severity\":{},\"message\":{}}}",
                                    util::json_string(&e.severity),
                                    util::json_string(&e.message),
                                )
                            })
                            .collect();
                        format!(",\"recent_events\":[{}]", rows.join(","))
                    }
                    // Context, not a gate — a failed event fetch
                    // doesn't block the plan.
                    Err(_) => String::new(),
                }
            }
        };

        let token = mint_token();
        {
            let mut st = self.writes.lock().await;
            if self.dispatching.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("another write is in flight — wait for it to complete".into());
            }
            // A new plan replaces any pending one: the agent
            // re-planned, and two live tokens would be ambiguous.
            // `install` remembers the token it retires so confirming
            // the old one can say what happened.
            st.install(PendingWrite {
                token: token.clone(),
                verb,
                env: env.name.clone(),
                version: version.clone(),
                settings: settings.clone(),
                profile: profile.clone(),
                region: arg_str(args, "region"),
                caller: caller.clone(),
                dlq_visible,
                expires_at: tokio::time::Instant::now()
                    + std::time::Duration::from_secs(CONFIRM_TTL_SECS),
                name_retry_used: false,
                dlq_targets: dlq_targets.clone(),
                dlq_url: dlq_url.clone(),
                dlq_main_url: dlq_main_url.clone(),
            });
        }

        // `next` is a human-readable string VALUE — build it plain,
        // then json_string it so any quotes (terminate's confirm_name
        // hint carries them) are escaped rather than breaking the frame.
        // `next` must say that a PERSON is asked, where one is.
        //
        // It read as a mechanical second step — "call confirm_action
        // with the confirm_token to dispatch" — and an agent that
        // never happened to hit a timeout would model the confirm as a
        // formality it performs. A peer session said exactly that: it
        // learned a human gate existed only by timing out, and noted
        // that an agent which never did would report a decline to its
        // user as an ERROR rather than as a person's decision. The
        // server instructions say this; the plan did not, and the plan
        // is what an agent reads at the moment it matters.
        let asked = self
            .client_supports_elicitation
            .load(std::sync::atomic::Ordering::Relaxed);
        let gate = if asked {
            " — the OPERATOR is asked to approve it and may decline or not answer, so \
             this is a request, not a formality"
        } else {
            ""
        };
        let next = if verb == WriteVerb::Terminate {
            format!(
                "call confirm_action with the confirm_token AND confirm_name={} to \
                 dispatch{gate}",
                env.name
            )
        } else {
            format!("call confirm_action with the confirm_token to dispatch{gate}")
        };
        Ok(format!(
            "{{\"pending\":true,\"confirm_token\":{},\"expires_in_secs\":{CONFIRM_TTL_SECS},\"plan\":{{\"action\":{},\"env\":{},\"application\":{},\"health\":{},\"status\":{},\"identity\":{},\"forecloses\":{}{plan_extra}{events_json}}},\"next\":{}}}",
            util::json_string(&token),
            util::json_string(verb.label()),
            util::json_string(&env.name),
            util::json_string(&env.application),
            util::json_string(&env.health),
            util::json_string(&env.status),
            // Already JSON — an object, or `null` plus the reason.
            caller.json(),
            util::json_string(&forecloses(verb, dlq_visible, dlq_targets.len())),
            util::json_string(&next),
        ))
    }

    /// Phase 2: dispatch the pending plan.
    pub(super) async fn tool_confirm_action(&self, args: &Value) -> Result<String, WriteError> {
        if !self.effective_scope().any() {
            // A REFUSAL, and it audited nothing until 0.42 — the
            // defect the backlog entry predicted, found by classifying
            // these sites rather than by any guard. Its sibling
            // `refused_out_of_scope` has always recorded `not_granted`
            // for the same reason: a client reaching a write tool that
            // is not advertised is working from a stale tool list or
            // probing, which is worth seeing and invisible any other
            // way. Reachable exactly as that one is — via a cached
            // tool list from a wider grant.
            return Err(WriteError::Refused(Audited::record(
                matches!(self.backend, Backend::Demo),
                arg_str(args, "profile").as_deref(),
                arg_str(args, "region").as_deref().unwrap_or("-"),
                "confirm",
                "-",
                "not_granted",
                "restart the MCP server with --allow-writes",
                "writes are disabled — start the server with --allow-writes".to_string(),
            )));
        }
        let token = arg_str(args, "confirm_token").ok_or("'confirm_token' is required")?;
        let pending = {
            let mut st = self.writes.lock().await;
            if self.dispatching.load(std::sync::atomic::Ordering::SeqCst) {
                return Err("another write is in flight — wait for it to complete".into());
            }
            let Some(p) = st.pending.as_mut() else {
                return Err("no pending write — call a write tool first".into());
            };
            if p.token != token {
                return Err(WriteError::Invalid(mismatched_token_message(
                    &st.retired,
                    &token,
                )));
            }
            if tokio::time::Instant::now() >= p.expires_at {
                st.pending = None;
                return Err("confirm_token expired — re-plan required".into());
            }
            // The plan already cleared the scope, so this can only
            // fire if the two gates disagree. That is exactly why it
            // is here: the plan gate and this one are the only things
            // standing between a cached tool list and a dispatch, and
            // a scope that held at plan time but not at confirm is a
            // bug worth failing on rather than dispatching through.
            if !self.effective_scope().allows(p.verb.tool_name()) {
                let (verb, env, region) = (p.verb, p.env.clone(), p.region.clone());
                st.pending = None;
                drop(st);
                return Err(self
                    .refused_out_of_scope(verb, Some(&env), region.as_deref())
                    .with_context(|m| format!("{m} — plan dropped")));
            }
            if p.verb == WriteVerb::Terminate {
                let supplied = arg_str(args, "confirm_name").unwrap_or_default();
                if supplied != p.env {
                    if p.name_retry_used {
                        st.pending = None;
                        return Err(
                            "confirm_name mismatch twice — plan dropped, re-plan required".into(),
                        );
                    }
                    p.name_retry_used = true;
                    return Err(WriteError::Invalid(format!(
                        "confirm_name must equal the env name ({}) — one retry remains on this token",
                        p.env
                    )));
                }
            }
            // Re-gate at CONFIRM time (R1, 0.28 panel): freeze/pin
            // were checked at plan time, but the token window is long
            // enough for an incident to be declared since. A refusal
            // here drops the plan — reality changed, re-plan required.
            if let Some(refused) =
                self.gate_refusal(&p.env, &p.profile, p.region.as_deref(), p.verb.label())
            {
                st.pending = None;
                return Err(WriteError::Refused(refused));
            }
            // Set BEFORE releasing the writes lock: a concurrent
            // plan/confirm checking `dispatching` must see it true.
            self.dispatching
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // Infallible: the `is_none()` check a few lines up runs
            // under this same `writes` lock, which is not released
            // between there and here, so nothing can take `pending` in
            // between. Restructuring to carry the value down from that
            // check would need the lock guard threaded through the
            // early-return arms for no safety gain.
            #[expect(clippy::expect_used)]
            {
                st.pending.take().expect("checked above under this lock")
            }
        };

        // RAII reset (0.28 pre-tag review I2): if `dispatch_write`
        // panics or unwinds, `dispatching` must still clear —
        // otherwise a single panicked task wedges the whole write
        // surface forever ("another write is in flight" on every
        // subsequent call). An AtomicBool store in Drop is
        // synchronous and runs on unwind; the plain set-false after
        // the await would be skipped.
        struct DispatchGuard<'a>(&'a std::sync::atomic::AtomicBool);
        impl Drop for DispatchGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, std::sync::atomic::Ordering::SeqCst);
            }
        }
        let _guard = DispatchGuard(&self.dispatching);

        // THE GATE. Everything above this point is the agent talking
        // to ebman; this is the operator being asked, once, about the
        // request they made. Placed after the plan is taken so the
        // question can carry it, and inside the dispatch guard so a
        // second write cannot start while a human is deciding.
        //
        // On a client that cannot elicit, and ONLY then, `ask_operator`
        // returns `NotAsked` — and the write proceeds, because there
        // the `--allow-writes` flag was the gate and still is. Every
        // other non-answer, including a channel that died mid-confirm,
        // is `Unanswered` and denies. The write surface is advertised
        // by default only where the ask exists; those two are one
        // change, deliberately, because advertising without the ask is
        // an open surface with no gate.
        let asked_at = std::time::Instant::now();
        // Terminate and purge demand the operator TYPE the env name,
        // matching the TUI's strict-typed-name confirms for both. The
        // agent-supplied `confirm_name` check above stays: that binds
        // the AGENT to the planned environment, and this binds a
        // PERSON to it. They defend different things and neither
        // replaces the other.
        let typed = match pending.verb {
            WriteVerb::Terminate | WriteVerb::DlqPurge => Some(pending.env.as_str()),
            _ => None,
        };
        let outcome = self.ask_operator(&ask_summary(&pending), typed).await;
        // Record the ASK itself, whatever it answered — including an
        // approval, which previously left no trace that a question had
        // been put at all. A dispatch line's `can_ask=true` says an
        // ask was possible; this says one happened and what came back.
        // `docs/design/protection-levels.md` asked for it: a client
        // that declares elicitation and answers its own dialogs
        // otherwise produces a cleaner record than an honest one.
        // Excludes every outcome where no question reached anybody:
        // `NotAsked` (client cannot elicit) and `Undeliverable` (no
        // channel, or the send failed). `stage=asked` asserts a
        // question was put; writing one for a frame that never left is
        // the same false record pointed the other way.
        let was_actually_asked =
            !matches!(outcome, AskOutcome::NotAsked | AskOutcome::Undeliverable);
        if was_actually_asked && !matches!(self.backend, Backend::Demo) {
            crate::audit::append_action_asked(
                None,
                pending.profile.as_deref(),
                pending.region.as_deref().unwrap_or("-"),
                pending.verb.label(),
                &pending.env,
                outcome.answer_label(),
                asked_at.elapsed().as_millis(),
            );
        }
        // Belt-and-braces, independent of `refuses()`. If this verb is
        // only reachable because the client declared elicitation —
        // `write_scope` does not grant it, `effective_scope` does —
        // then the ask is the ONLY gate this write ever had, and
        // anything short of an explicit approval must stop it. Without
        // this, any future value that is neither Approved nor
        // "refusing" dispatches unapproved, which is exactly how the
        // dead-channel `NotAsked` got through.
        let ask_is_the_only_gate = !self.write_scope.allows(pending.verb.tool_name());
        if outcome.refuses() || (ask_is_the_only_gate && outcome != AskOutcome::Approved) {
            let reason = outcome.reason();
            // Through `Audited::record` like every other refusal, so
            // the line is written by the same code that makes the
            // error. The hand-rolled `append_action_refused` this
            // replaces was correct, and was also exactly the shape
            // that made the 0.40 omission invisible — an audit call
            // that a future edit could drop without the type noticing.
            //
            // The remedy names no control, deliberately. The control
            // is a person who has just said no, and an agent that
            // retries a decline is the failure mode here.
            return Err(WriteError::Refused(Audited::record(
                matches!(self.backend, Backend::Demo),
                pending.profile.as_deref(),
                pending.region.as_deref().unwrap_or("-"),
                pending.verb.label(),
                &pending.env,
                "not_approved",
                reason,
                format!("not dispatched — {reason}. {}", outcome.guidance()),
            )));
        }
        // Re-gate AFTER the approval. The gates ran before the ask,
        // and the dialog can stay open for `ASK_WAIT_SECS` — far
        // longer than the 60s token window the original double-check
        // was written around. An operator who declares an incident
        // while a colleague is reading the dialog expects the freeze
        // to win, and `safety-and-privacy.md` promises exactly that.
        // The freeze is re-read from disk here, so this is the check
        // that catches it.
        if let Some(refused) = self.gate_refusal(
            &pending.env,
            &pending.profile,
            pending.region.as_deref(),
            pending.verb.label(),
        ) {
            return Err(WriteError::Refused(refused.map_message(|m| {
                format!("{m} (declared while the confirmation was open — nothing was dispatched)")
            })));
        }

        self.dispatch_write(&pending)
            .await
            .map_err(WriteError::Invalid)
    }

    /// Dispatch a DLQ batch, auditing each message separately.
    ///
    /// Separate from `dispatch_write`'s generic path because the audit
    /// granularity differs: there, one action produces one
    /// dispatched/completed pair; here, five messages produce five,
    /// each naming its own id and task. Collapsing them would record
    /// that five messages were deleted from an environment and leave
    /// the log unable to say which — and for a delete, the log is the
    /// only place that answer can still exist.
    ///
    /// The batch never fails whole once dispatched. A message that
    /// vanished between plan and confirm is one failed item among
    /// successes, because the others were approved and stopping at the
    /// second of five would leave three in an unknown state.
    async fn dispatch_dlq_and_audit(
        &self,
        client: &crate::aws::AwsClient,
        p: &PendingWrite,
        client_name: &str,
        audit_profile: Option<&str>,
    ) -> Result<String, String> {
        let verb_label = p.verb.label();
        let region = &client.context.region;
        let can_ask = self
            .client_supports_elicitation
            .load(std::sync::atomic::Ordering::Relaxed);
        let lines = dlq_audit_lines(client_name, can_ask, &p.dlq_targets);
        // One line per message BEFORE acting: a dispatched line with no
        // completed line is how a crash mid-batch stays visible.
        for extras in &lines {
            let refs: Vec<(&str, &str)> = extras.iter().map(|(k, v)| (*k, v.as_str())).collect();
            crate::audit::append_action_dispatched(
                None,
                audit_profile,
                region,
                verb_label,
                &p.env,
                &refs,
            );
        }

        // A whole-batch failure — no queue url, or the re-read itself
        // failed — is not a per-item outcome: nothing was attempted.
        // Each message still gets a completed line saying so, or the
        // dispatched lines above dangle forever.
        let outcomes = match dispatch_dlq_batch(client, p).await {
            Ok(o) => o,
            Err(e) => {
                for extras in &lines {
                    let refs: Vec<(&str, &str)> =
                        extras.iter().map(|(k, v)| (*k, v.as_str())).collect();
                    crate::audit::append_action_completed(
                        None,
                        audit_profile,
                        region,
                        verb_label,
                        &p.env,
                        Err(e.as_str()),
                        &refs,
                    );
                }
                return Err(tool_error(&p.profile, verb_label, &e));
            }
        };

        let mut items: Vec<String> = Vec::with_capacity(outcomes.len());
        let mut succeeded = 0usize;
        let mut failed = 0usize;
        let mut recoverable_until: Option<u64> = None;
        // Built from the OUTCOME's own target, not zipped against the
        // list above. A zip would pair by position and silently
        // mispair — attaching every completion line to the wrong
        // message, and truncating if the lengths ever diverged — while
        // reading as obviously correct. The outcome carries the target
        // it belongs to; using it removes the coupling rather than
        // documenting it.
        for o in outcomes {
            let extras = dlq_audit_line(client_name, can_ask, &o.target);
            let refs: Vec<(&str, &str)> = extras.iter().map(|(k, v)| (*k, v.as_str())).collect();
            crate::audit::append_action_completed(
                None,
                audit_profile,
                region,
                verb_label,
                &p.env,
                match &o.result {
                    Ok(_) => Ok(()),
                    Err(e) => Err(e.as_str()),
                },
                &refs,
            );
            match &o.result {
                Ok(destroyed) => {
                    succeeded += 1;
                    if let Some(msg) = destroyed.clone() {
                        if let Some(until) =
                            self.remember_deleted(&p.env, p.dlq_url.clone(), msg).await
                        {
                            // The window is per message but they are
                            // captured within milliseconds of each
                            // other, so the shortest is the honest one
                            // to quote for the batch.
                            recoverable_until =
                                Some(recoverable_until.map_or(until, |c: u64| c.min(until)));
                        }
                    }
                }
                Err(_) => failed += 1,
            }
            items.push(render_dlq_item(&o.target, &o.result));
        }

        let recoverable = recoverable_until
            .map(|u| format!(",\"recoverable_for_secs\":{u}"))
            .unwrap_or_default();
        let report = format!(
            "\"action\":{},\"env\":{},\"succeeded\":{succeeded},\"failed\":{failed},\
             \"results\":[{}]",
            util::json_string(verb_label),
            util::json_string(&p.env),
            items.join(",")
        );

        // Nothing succeeded → this is an ERROR, not a result with a
        // false flag in it.
        //
        // A partial batch is a success carrying its failures; a total
        // failure is a failure, and the difference matters because
        // `isError` is the field agents branch on. Returning
        // `{"dispatched": false}` with `isError` unset would tell an
        // agent that skims — which is all of them, sometimes — that a
        // delete happened when nothing was touched. That is the
        // `peeked: true, messages: []` shape again: a result reporting
        // the attempt while hiding that it achieved nothing.
        //
        // It also preserves the single-message behaviour exactly. One
        // target that has vanished is a batch where nothing succeeded,
        // so it errors as it always did.
        if succeeded == 0 {
            return Err(format!("{{\"dispatched\":false,{report}}}"));
        }
        Ok(format!("{{\"dispatched\":true,{report}{recoverable}}}"))
    }

    async fn dispatch_write(&self, p: &PendingWrite) -> Result<String, String> {
        let verb_label = p.verb.label();
        if matches!(self.backend, Backend::Demo) {
            // Synthetic success: no AWS, no audit, no webhook. But a
            // demo delete still fills the undo buffer from the
            // fixture, or the recovery path is unwalkable without
            // credentials — which is the one thing demo exists for.
            let mut shortest: Option<u64> = None;
            let mut results = String::new();
            if !p.dlq_targets.is_empty() {
                // The same per-item shape live dispatch produces. Demo
                // rendering its own simpler result is how the two
                // drift, and a demo that cannot show the batch report
                // cannot be used to check it.
                let mut items: Vec<String> = Vec::new();
                for t in &p.dlq_targets {
                    if captures_for_undo(p.verb) {
                        if let Some(msg) = demo_fixture::dlq_messages_for_env(&p.env)
                            .into_iter()
                            .find(|m| m.id == t.id)
                        {
                            if let Some(until) =
                                self.remember_deleted(&p.env, p.dlq_url.clone(), msg).await
                            {
                                // The SHORTEST window, as live does.
                                // Overwriting per message would quote
                                // the last one's, and a demo that
                                // renders a different number from live
                                // is a demo that hides the difference.
                                shortest = Some(shortest.map_or(until, |c: u64| c.min(until)));
                            }
                        }
                    }
                    items.push(render_dlq_item(t, &Ok(None)));
                }
                results = format!(
                    ",\"results\":[{}],\"succeeded\":{},\"failed\":0",
                    items.join(","),
                    p.dlq_targets.len()
                );
            }
            let recoverable = shortest
                .map(|u| format!(",\"recoverable_for_secs\":{u}"))
                .unwrap_or_default();
            return Ok(format!(
                "{{\"dispatched\":true,\"demo\":true,\"action\":{},\"env\":{}{recoverable}{results}}}",
                util::json_string(verb_label),
                util::json_string(&p.env),
            ));
        }
        let args = json!({
            "profile": p.profile.clone().unwrap_or_default(),
            "region": p.region.clone().unwrap_or_default(),
        });
        let client = self.client(&args).await?;
        let client_name = self
            .client_name
            .lock()
            .map(|s| s.clone())
            .unwrap_or_else(|_| "unknown".into());
        let audit_profile = p
            .profile
            .clone()
            .or_else(|| std::env::var("AWS_PROFILE").ok());
        // Resend and delete take the batch path, which audits per
        // MESSAGE. The generic path below writes one dispatched line
        // and one completed line for the whole action, and for a batch
        // that records "five messages were deleted from poly-batch"
        // without naming one of them — the exact gap `message_id=`
        // exists to close, reopened by the plural.
        if matches!(p.verb, WriteVerb::DlqResend | WriteVerb::DlqDelete) {
            return self
                .dispatch_dlq_and_audit(&client, p, &client_name, audit_profile.as_deref())
                .await;
        }
        let extras = self.write_extras(
            &client_name,
            p.version.as_deref(),
            p.settings.len(),
            None,
            None,
        );
        let extras_ref: Vec<(&str, &str)> = extras.iter().map(|(k, v)| (*k, v.as_str())).collect();
        crate::audit::append_action_dispatched(
            None,
            audit_profile.as_deref(),
            &client.context.region,
            verb_label,
            &p.env,
            &extras_ref,
        );
        // `Ok(Some(msg))` means a message was destroyed and is briefly
        // recoverable; `Ok(None)` means nothing was, which is every
        // other verb.
        let outcome: Result<Option<crate::aws::QueueMessage>, String> = match p.verb {
            WriteVerb::Deploy => client
                .deploy_version(&p.env, p.version.as_deref().unwrap_or_default())
                .await
                .map(|()| None)
                .map_err(|e| e.to_string()),
            WriteVerb::Restart => client
                .restart_app_server(&p.env)
                .await
                .map(|()| None)
                .map_err(|e| e.to_string()),
            WriteVerb::Rebuild => client
                .rebuild_env(&p.env)
                .await
                .map(|()| None)
                .map_err(|e| e.to_string()),
            WriteVerb::Terminate => client
                .terminate_env(&p.env)
                .await
                .map(|()| None)
                .map_err(|e| e.to_string()),
            WriteVerb::SetOption => client
                .update_env_option_settings(&p.env, &p.settings, &[])
                .await
                .map(|()| None)
                .map_err(|e| e.to_string()),
            WriteVerb::DlqPurge => match p.dlq_url.as_deref() {
                // Deliberately NOT captured. A purge can be thousands
                // of messages, and holding a capped sample would offer
                // an undo that silently restores some of what it
                // destroyed — worse than offering none.
                Some(url) => client
                    .purge_queue(url)
                    .await
                    .map(|()| None)
                    .map_err(|e| e.to_string()),
                None => Err("plan carried no queue url".into()),
            },
            // Handled above, per message.
            WriteVerb::DlqResend | WriteVerb::DlqDelete => {
                Err("unreachable: dlq resend/delete take the batch path".into())
            }
        };
        crate::audit::append_action_completed(
            None,
            audit_profile.as_deref(),
            &client.context.region,
            verb_label,
            &p.env,
            match &outcome {
                Ok(_) => Ok(()),
                Err(e) => Err(e.as_str()),
            },
            &extras_ref,
        );
        match outcome {
            Ok(destroyed) => {
                let recoverable = match destroyed {
                    Some(msg) => self
                        .remember_deleted(&p.env, p.dlq_url.clone(), msg)
                        .await
                        .map(|until| format!(",\"recoverable_for_secs\":{until}"))
                        .unwrap_or_default(),
                    None => String::new(),
                };
                Ok(format!(
                "{{\"dispatched\":true,\"action\":{},\"env\":{}{recoverable},\"note\":\"dispatch-only — poll list_environments / recent_events for progress\"}}",
                util::json_string(verb_label),
                util::json_string(&p.env),
            ))
            }
            Err(e) => Err(tool_error(&p.profile, verb_label, &e)),
        }
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
                        "{{\"message_id\":{},\"env\":{},\"task\":{},\"expires_in_secs\":{}}}",
                        util::json_string(&d.original_id),
                        util::json_string(&d.env),
                        util::json_string(d.task.as_deref().unwrap_or("(not an EB worker task)")),
                        UNDO_WINDOW_SECS.saturating_sub(d.at.elapsed().as_secs()),
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

        let Some(d) = held.into_iter().find(|d| d.original_id == want) else {
            return Err(WriteError::Invalid(format!(
                "'{want}' is not recoverable. Either it was never deleted by this server, \
                 or the {UNDO_WINDOW_SECS}s window has passed, or the server restarted. \
                 Call this tool with no arguments to see what IS recoverable."
            )));
        };

        if let Some(refused) =
            self.gate_refusal(&d.env, &arg_str(args, "profile"), None, "dlq-undo")
        {
            return Err(WriteError::Refused(refused));
        }

        if !matches!(self.backend, Backend::Demo) {
            self.client(args)
                .await?
                .send_message(&d.queue_url, &d.body, &d.attributes)
                .await
                .map_err(|e| format!("restoring the message failed: {e}"))?;
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
            util::json_string(d.task.as_deref().unwrap_or("(not an EB worker task)")),
        ))
    }
}

#[cfg(test)]
#[path = "tests/writes.rs"]
mod tests;
