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

use super::dialog::{ask_summary, forecloses};
use super::refusal::{Audited, WriteAudit, WriteError};
use super::role::*;
use super::undo::*;
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
pub(super) fn dlq_audit_line(
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

/// The `(key, value)` borrows the audit writers take.
pub(super) fn extras_refs<'a>(
    extras: &'a [(&'static str, String)],
) -> Vec<(&'static str, &'a str)> {
    extras.iter().map(|(k, v)| (*k, v.as_str())).collect()
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
    pub(super) fn write_extras(
        &self,
        client_name: &str,
        version: Option<&str>,
        settings_len: usize,
        dlq_message_id: Option<&str>,
        dlq_task: Option<&str>,
    ) -> Vec<(&'static str, String)> {
        write_extras_parts(
            client_name,
            self.can_ask(),
            version,
            settings_len,
            dlq_message_id,
            dlq_task,
        )
    }

    /// The client a confirmed write goes out on: its role session's
    /// when it has one (D3, and an undo reuses its delete's, D4),
    /// otherwise the base identity's for the profile and region the plan
    /// recorded, never the confirming call's.
    pub(super) async fn dispatch_client(
        &self,
        profile: &Option<String>,
        region: &Option<String>,
        session: Option<&RoleSession>,
    ) -> Result<std::sync::Arc<crate::aws::AwsClient>, String> {
        if let Some(s) = session {
            return Ok(std::sync::Arc::clone(&s.client));
        }
        self.client(&json!({
            "profile": profile.clone().unwrap_or_default(),
            "region": region.clone().unwrap_or_default(),
        }))
        .await
    }
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

/// A dispatch that AWS refused, as the agent reads it. Under a role
/// session the credentials came from STS for this write alone, so the
/// credential hint (`aws sso login --profile …`) names a profile that
/// would not fix it: the session is named instead.
fn dispatch_error(
    profile: &Option<String>,
    session: Option<&RoleSession>,
    op: &str,
    msg: &str,
) -> String {
    match session {
        Some(s) => format!("{op} failed under role session {}: {msg}", s.name),
        None => tool_error(profile, op, msg),
    }
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

/// How many retired tokens to remember. An agent re-planning more than
/// a handful of times inside one 60-second TTL is not a case worth
/// spending memory on.
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

    /// The shared verb this MCP verb dispatches.
    pub(super) fn verb(self) -> crate::verb::Verb {
        use crate::verb::Verb;
        match self {
            WriteVerb::Deploy => Verb::Deploy,
            WriteVerb::Restart => Verb::RestartAppServer,
            WriteVerb::Rebuild => Verb::Rebuild,
            WriteVerb::Terminate => Verb::Terminate,
            WriteVerb::SetOption => Verb::SetOption,
            WriteVerb::DlqResend => Verb::DlqResend,
            WriteVerb::DlqDelete => Verb::DlqDelete,
            WriteVerb::DlqPurge => Verb::DlqPurge,
        }
    }

    /// The audit label: the shared vocabulary's, never a local
    /// spelling. This was a local table, and it wrote a restart as
    /// `Restart` while every other surface wrote `RestartAppServer`.
    pub(super) fn label(self) -> &'static str {
        self.verb().audit_label()
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

/// The longest `set_option` namespace, name or value a plan may carry.
///
/// Not a limit on what Elastic Beanstalk accepts — a limit on what an
/// operator can be asked about. The dialog shows setting values WHOLE
/// (see `Shown::Whole`), because truncating one hides its operative
/// end: `https://payments.example/callback/<filler>@evil.example/x`
/// reads as a benign host and applies another, the `@` sitting past
/// the ellipsis. Refusing above a readable size is the same rule
/// `DLQ_BATCH_CAP` applies to a list, pointed at a single field.
///
/// 256 is generous for the things that legitimately appear here —
/// URLs, connection strings, ARNs — and still fits a dialog.
pub(super) const SET_OPTION_FIELD_MAX: usize = 256;

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
    /// What the role config said of the verb at plan time. Confirm
    /// re-gates on THIS, so plan and confirm agree by construction.
    pub role_check: crate::config::RoleCheck,
    /// The role the plan picked for the env's account, of those listing
    /// the verb: what the confirmation names and what slice 3 assumes,
    /// never picked again at confirm. `None` for a verb no role lists.
    pub role: Option<PlanRole>,
    /// Who and where every line of this write files under, resolved at
    /// plan time: the region is the one the plan's client found the env
    /// in, and a role session is assumed in it.
    pub audit: WriteAudit,
}

/// Token TTL — long enough for an agent round-trip, short enough
/// that a stale plan can't be confirmed against changed reality.
pub(super) const CONFIRM_TTL_SECS: u64 = 60;

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
    let confirm_note = format!("TWO-PHASE: this tool DISPATCHES NOTHING. It validates and returns {{pending:true, confirm_token, plan}}; you must surface the plan, then call confirm_action with the token ({CONFIRM_TTL_SECS}s TTL, single-use) to dispatch. Dispatch-only — poll the read tools for progress.");
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
            "description": format!("Put back a dead-lettered message THIS server deleted, within {undo_mins} minutes. Call with no arguments to list what is still recoverable. Single-phase — no plan/confirm — because it is the least destructive action here and is reached for under time pressure. CAVEATS: held in memory by this server only, so a restart loses them and nothing deleted by another process is here; purges are never recoverable; and the restore is a re-send, so the message id changes, receive_count resets to 0 and the enqueue time becomes now. Body and attributes come back verbatim. Each message can be restored ONCE, to the region and profile it was deleted from, and under the same role session if the delete had one; a failed or interrupted restore leaves it recoverable for the rest of its {undo_mins} minutes (the failure says if they have run out), and a second call while one is running is told so.", undo_mins = UNDO_WINDOW_SECS / 60),
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
            "description": format!("Phase 2 of every write tool: dispatch the pending plan identified by confirm_token (single-use, {CONFIRM_TTL_SECS}s TTL). terminate additionally requires confirm_name equal to the plan's env name. Writes are serialized — a confirm while another dispatch is in flight is refused."),
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
                            .unwrap_or_else(|| NOT_A_WORKER_TASK.into()),
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
        #[cfg(test)]
        if let Some(id) = &self.seams.identity {
            return id.clone();
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

    /// Phase 1 for every write verb: shared gates (verb in scope,
    /// writes enabled, not mid-dispatch, freeze, pins, env exists),
    /// verb-specific validation, then a pending plan + token.
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
            let audit = self
                .write_audit(
                    &arg_str(args, "profile"),
                    None,
                    arg_str(args, "region").as_deref(),
                )
                .await;
            return Err(self.refused_out_of_scope(verb, arg_str(args, "env").as_deref(), &audit));
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
        let role_check = self.role_check(verb);
        // Before the env is found or a client built, so from the call
        // and the profile file: a refusal here costs no AWS call.
        let early = self
            .write_audit(&profile, None, arg_str(args, "region").as_deref())
            .await;
        if let Some(refused) = self.gate_refusal(&env_name, &early, verb.label(), &role_check) {
            return Err(WriteError::Refused(refused));
        }

        let envs = self.fetch_envs(args).await?;
        let env = envs
            .iter()
            .find(|e| e.name == env_name)
            .ok_or_else(|| format!("env '{env_name}' not found"))?
            .clone();

        // Resolved before the plan is rendered so the operator sees
        // it in the confirmation, which is the only moment they can
        // act on it.
        let caller = self.caller_identity(args).await;

        // The plan's client, and the region it found the env in, which
        // every line of this write files under: read by the SDK,
        // instance metadata included.
        let plan_client = match self.backend {
            Backend::Demo => None,
            Backend::Aws => Some(self.client(args).await?),
        };
        let audit = self
            .write_audit(
                &profile,
                plan_client.as_ref().map(|c| c.context.region.as_str()),
                arg_str(args, "region").as_deref(),
            )
            .await;

        // The role the verb maps to, and whether it is in the env's
        // account. Before anything verb-specific is read: a DLQ plan
        // peeks the queue, which bumps each message's receive count,
        // and a refusal that depends only on config and the env must
        // not cost that.
        let role = match &role_check {
            crate::config::RoleCheck::Mapped { roles } => {
                match role_for_env(roles, &env, &caller, !matches!(self.backend, Backend::Demo)) {
                    Ok(r) => Some(r),
                    Err(refusal) => {
                        return Err(WriteError::Refused(Audited::record(
                            &audit,
                            &crate::audit::Refused {
                                action_label: verb.label(),
                                target: &env.name,
                                rule: refusal.rule(),
                                remedy: &refusal.remedy(),
                                extras: &[],
                            },
                            refusal.message(verb, &env.name, roles),
                        )));
                    }
                }
            }
            _ => None,
        };

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

        // Recent events give the plan operational context (3 max).
        let events_json = match &plan_client {
            None => String::new(),
            Some(client) => {
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
                role_check: role_check.clone(),
                role: role.clone(),
                audit: audit.clone(),
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
        let asked = self.can_ask();
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
            "{{\"pending\":true,\"confirm_token\":{},\"expires_in_secs\":{CONFIRM_TTL_SECS},\"plan\":{{\"action\":{},\"env\":{},\"application\":{},\"health\":{},\"status\":{},\"identity\":{},\"role\":{},\"forecloses\":{}{plan_extra}{events_json}}},\"next\":{}}}",
            util::json_string(&token),
            util::json_string(verb.label()),
            util::json_string(&env.name),
            util::json_string(&env.application),
            util::json_string(&env.health),
            util::json_string(&env.status),
            // Already JSON — an object, or `null` plus the reason.
            caller.json(),
            role.as_ref()
                .map(PlanRole::json)
                .unwrap_or_else(|| "null".to_string()),
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
            let audit = self
                .write_audit(
                    &arg_str(args, "profile"),
                    None,
                    arg_str(args, "region").as_deref(),
                )
                .await;
            return Err(WriteError::Refused(Audited::record(
                &audit,
                &crate::audit::Refused {
                    action_label: "confirm",
                    target: "-",
                    rule: crate::audit::RefusalRule::NotGranted,
                    remedy: "restart the MCP server with --allow-writes",
                    extras: &[],
                },
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
                let refused = self.refused_out_of_scope(p.verb, Some(&p.env), &p.audit);
                st.pending = None;
                return Err(refused.with_context(|m| format!("{m} — plan dropped")));
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
                self.gate_refusal(&p.env, &p.audit, p.verb.label(), &p.role_check)
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
                pending.audit.profile.as_deref(),
                pending.audit.region_label(),
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
                &pending.audit,
                &crate::audit::Refused {
                    action_label: pending.verb.label(),
                    target: &pending.env,
                    rule: crate::audit::RefusalRule::NotApproved,
                    remedy: reason,
                    extras: &[],
                },
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
            &pending.audit,
            pending.verb.label(),
            &pending.role_check,
        ) {
            return Err(WriteError::Refused(refused.map_message(|m| {
                format!("{m} (declared while the confirmation was open — nothing was dispatched)")
            })));
        }

        // The role, assumed now that the operator has said yes (D3):
        // a failure refuses, and is never sent as the base identity.
        let client_name = self.client_name();
        let session = self
            .assume_role_for(&pending, &client_name)
            .await
            .map_err(WriteError::Refused)?;
        self.dispatch_write(&pending, session.as_ref())
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
        session: Option<&RoleSession>,
    ) -> Result<String, String> {
        let verb_label = p.verb.label();
        let region = &client.context.region;
        let can_ask = self.can_ask();
        let lines: Vec<_> = dlq_audit_lines(client_name, can_ask, &p.dlq_targets)
            .into_iter()
            .map(|extras| with_session(extras, session))
            .collect();
        // One line per message BEFORE acting: a dispatched line with no
        // completed line is how a crash mid-batch stays visible.
        for extras in &lines {
            let refs = extras_refs(extras);
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
                    let refs = extras_refs(extras);
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
                return Err(dispatch_error(&p.profile, session, verb_label, &e));
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
            let extras = with_session(dlq_audit_line(client_name, can_ask, &o.target), session);
            let refs = extras_refs(&extras);
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
                        if let Some(until) = self.remember_deleted(p, msg, session).await {
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

    async fn dispatch_write(
        &self,
        p: &PendingWrite,
        session: Option<&RoleSession>,
    ) -> Result<String, String> {
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
                            // Demo assumes nothing (D8), so there is
                            // no session to hold.
                            if let Some(until) = self.remember_deleted(p, msg, None).await {
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
        let client = self.dispatch_client(&p.profile, &p.region, session).await?;
        let client_name = self.client_name();
        let audit_profile = p.audit.profile.clone();
        // Resend and delete take the batch path, which audits per
        // MESSAGE. The generic path below writes one dispatched line
        // and one completed line for the whole action, and for a batch
        // that records "five messages were deleted from poly-batch"
        // without naming one of them — the exact gap `message_id=`
        // exists to close, reopened by the plural.
        if matches!(p.verb, WriteVerb::DlqResend | WriteVerb::DlqDelete) {
            return self
                .dispatch_dlq_and_audit(&client, p, &client_name, audit_profile.as_deref(), session)
                .await;
        }
        let extras = with_session(
            self.write_extras(
                &client_name,
                p.version.as_deref(),
                p.settings.len(),
                None,
                None,
            ),
            session,
        );
        let extras_ref = extras_refs(&extras);
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
                        .remember_deleted(p, msg, session)
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
            Err(e) => Err(dispatch_error(&p.profile, session, verb_label, &e)),
        }
    }
}

#[cfg(test)]
#[path = "tests/writes.rs"]
mod tests;
