//! The approval dialog: the one sentence an operator reads before a
//! write, and the cleaning every foreign string in it goes through.
//! Split from `writes.rs` (2026-11-12).

use super::role::PlanRole;
use super::undo::UNDO_WINDOW_SECS;
use super::writes::{PendingWrite, WriteVerb};
use super::*;

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
/// something visibly wrong is shown rather than nothing; how much of
/// the field is shown is the caller's [`Shown`], said at every call.
pub(super) fn sanitize(raw: &str, shown: Shown) -> String {
    let max = match shown {
        Shown::Capped(n) => n,
        Shown::Whole => usize::MAX,
    };
    let mut out = String::with_capacity(raw.len().min(max.min(4096)) + 1);
    for (n, c) in raw.chars().enumerate() {
        if n >= max {
            out.push('…');
            break;
        }
        // None of these is something a task name needs, and each
        // changes what the sentence appears to say.
        if util::rewrites_the_line(c) {
            out.push('\u{FFFD}');
        } else {
            out.push(c);
        }
    }
    out
}

/// How much of an untrusted field [`sanitize`] shows.
#[derive(Clone, Copy)]
pub(super) enum Shown {
    /// The first `n` characters and an ellipsis: so one field cannot
    /// push the rest of the sentence out of a dialog.
    Capped(usize),
    /// All of it. ONLY for fields whose length is refused at plan time.
    /// Truncating a value the operator is approving hides its operative
    /// end: a `set_option` value of
    /// `https://payments.example/callback/<90 filler>@evil.example/x`
    /// shows as a benign-looking prefix and an ellipsis, because the `@`
    /// that makes everything before it userinfo sits past the cut. The
    /// operator approves one destination and another is applied. So a
    /// value that cannot be shown whole is not shown at all — the plan
    /// is refused instead, and this renders what survives that check.
    Whole,
}

impl Shown {
    /// A field of the approval dialog.
    pub(super) const ASK: Shown = Shown::Capped(120);
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
        [] if p.swap_with.is_some() => format!(
            "{} — {} ⇄ {}",
            p.verb.label(),
            sanitize(&p.env, Shown::ASK),
            sanitize(p.swap_with.as_deref().unwrap_or_default(), Shown::ASK)
        ),
        [] if p.scale.is_some() => {
            let (min, max) = p.scale.unwrap_or_default();
            format!("{} to min {min}, max {max}", p.verb.label())
        }
        [] => match (p.version.as_deref(), &p.managed_action) {
            (Some(v), _) => format!("{} to {}", p.verb.label(), sanitize(v, Shown::ASK)),
            // EB's own description of what the update installs, beside
            // the id that dispatches: the id alone says nothing.
            (None, Some((id, description))) => format!(
                "{} {} — {}",
                p.verb.label(),
                sanitize(id, Shown::ASK),
                sanitize(description, Shown::ASK)
            ),
            (None, None) => p.verb.label().to_string(),
        },
        [one] => format!(
            "{} — {} ({})",
            p.verb.label(),
            sanitize(&one.task, Shown::ASK),
            sanitize(&one.id, Shown::ASK)
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
                    sanitize(&t.task, Shown::ASK),
                    sanitize(&t.id, Shown::ASK)
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
                    sanitize(ns, Shown::Whole),
                    sanitize(name, Shown::Whole),
                    // WHOLE, never truncated — see `Shown::Whole`.
                    // Bounded by `SET_OPTION_FIELD_MAX` at plan time.
                    sanitize(value, Shown::Whole)
                ))
                .collect::<Vec<_>>()
                .join("\n")
        )
    };
    format!(
        "{what}{detail} on {}, {}{}.\n\n{}",
        sanitize(&p.env, Shown::ASK),
        // Sanitised like every other foreign field. `line()` carries
        // an ARN from STS, and on the `Unknown` arm a raw SDK error
        // chain — remote prose that can contain newlines, and in one
        // traced path an agent-supplied profile name echoed back
        // through `tool_error`'s credential hint. The invariant this
        // file states is "every foreign field"; this was the one slot
        // that was not, and it sits in the sentence a human reads
        // before destroying something.
        sanitize(&p.caller.line(), Shown::ASK),
        // Its own slot: see `PlanRole::line`.
        p.role.as_ref().map(PlanRole::line).unwrap_or_default(),
        // `dlq_visible` is carried on the plan so a purge dialog can
        // say how much it destroys. Passing `None` here rendered
        // "every message in the queue" with no number, while the plan
        // JSON showed the agent `messages_now` — the operator got the
        // vaguer half of the same fact.
        forecloses(p.verb, p.dlq_visible, p.dlq_targets.len())
    )
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
        WriteVerb::Rollback => "The current version is replaced by the earlier one; the current \
             one stays deployable, so rolling forward is another deploy."
            .to_string(),
        WriteVerb::AbortUpdate => "The update in flight stops and EB rolls the environment back \
             to where it was. Nothing is destroyed; what the update was doing is undone, and \
             can be redone."
            .to_string(),
        WriteVerb::SwapCnames => "Each URL's traffic moves to the other environment at once. \
             Nothing is destroyed, and swapping again swaps back — but clients holding the old \
             DNS answer keep reaching the old target until it expires."
            .to_string(),
        WriteVerb::Scale => "The auto-scaling group's limits change, and EB applies it as an \
             environment update: a smaller max terminates instances above it, and min = max = 0 \
             stops the environment until scaled up again."
            .to_string(),
        WriteVerb::DlqRedrive => format!(
            "Every message leaves the dead-letter queue for the worker queue, to be \
             processed again — a poison message fails again and comes back. There is no \
             undo; `:dlq-redrive cancel` in the TUI stops a running move.{}",
            queue_note()
        ),
        WriteVerb::ApplyManagedAction => "The update runs now instead of in its maintenance \
             window, as EB deploys it: a platform update replaces every instance \
             (immutable), so anything written to instance-local storage is gone. There is \
             no undo; `:abort` in the TUI stops it while it is still running, and EB rolls \
             a failed one back."
            .to_string(),
        WriteVerb::SetOption => "The previous values are replaced. ebman does not keep them: \
             read them back with `get_option_settings` before approving if you need \
             to restore them. A change that triggers an environment update will \
             bounce instances to apply it."
            .to_string(),
    }
}
