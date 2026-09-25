//! `ebman mcp serve [--demo] [--no-redact]` — stdio MCP server
//! exposing ebman's read seams as tools, so Claude Code (or any MCP
//! client) can query fleet state first-class. Spec: BACKLOG.md "0.26
//! candidates". Registration: `claude mcp add ebman -- ebman mcp serve`.
//!
//! Two hard rules the implementation stands on:
//! - **stdout is protocol-only; stderr is diagnostics-only.** Tools
//!   call the underlying seams (`run_rules`, `parse_audit_line`,
//!   `aws::*`), never the `println!`-ing CLI `run()` wrappers.
//! - **Concurrent `tools/call`, responsive loop.** Every tool call is
//!   spawned and bounded at `TOOL_TIMEOUT_SECS`; `ping` never waits
//!   behind a slow AWS fan-out. `notifications/cancelled` is ignored
//!   in v1 (documented limitation).
//!
//! `--demo` serves the synthetic `demo_fixture` fleet through the
//! same tool layer (the demo AwsClient is a fail-loudly stub, so demo
//! data enters above the client) — this is the zero-AWS e2e harness.
//! `--no-redact` disables the `get_option_settings` env-var redaction.
//!
//! v1 is reads-only. Writes (`--allow-writes`) are a spec'd v2 with
//! their own safety review; nothing here dispatches.

use std::sync::Arc;

use color_eyre::eyre::Result;
use serde_json::{json, Value};

use crate::lint::inputs::{
    fetch_env_lint_inputs, fetch_stale_platform_issues, run_rules_for_env, EnvLintInputs,
};
use crate::{audit as audit_log, aws, cost_cache, demo_fixture, lint, terraform, util};

/// The MCP protocol revision this server claims. Clients offering a
/// different revision get this one back (echo-negotiate); the golden
/// frame test pins it so a bump is a conscious act.
mod annotations;
mod setup;
mod tools;
mod writes;
use tools::*;

pub(crate) const PROTOCOL_VERSION: &str = "2025-06-18";

/// Hard wall-clock bound on a single tool call so a hung AWS call
/// can't wedge the agent turn.
const TOOL_TIMEOUT_SECS: u64 = 30;

/// How long a call that will ASK a human may take.
///
/// 30 seconds bounds a hung AWS call, which is what it was written
/// for. It is nonsense as a bound on a person deciding whether to
/// delete production data: they will read the plan, think, and
/// routinely take longer. Applying the AWS bound to a human turns the
/// gate into a tool that times out under ordinary use.
///
/// Five minutes, not unbounded. An operator who has walked away must
/// eventually produce a DENY rather than a call that never returns —
/// an agent blocked forever on a dialog nobody will answer is its own
/// failure, and the design's rule is that an unanswerable ask degrades
/// to deny, never to allow.
const ASK_TIMEOUT_SECS: u64 = 300;

/// How long the ask itself waits — strictly less than the budget of
/// the call containing it.
///
/// These must not be equal. `call_timeout_secs` bounds the whole
/// `confirm_action` call at `ASK_TIMEOUT_SECS`, and that clock starts
/// before the question is even sent, so an ask given the same budget
/// loses the race with its own container. The visible effect is that
/// an unanswered ask returns a generic tool timeout instead of the
/// designed deny, and — worse — the `rule=not_approved` audit line
/// never gets written, because the code that writes it is on the far
/// side of a future that was dropped. "Deny on no-answer, audit the
/// decline" is the design's rule; equal budgets silently deliver
/// neither.
const ASK_WAIT_SECS: u64 = ASK_TIMEOUT_SECS - 20;

// The margin is the point, so it is checked at compile time rather
// than left to whoever next edits one of the two numbers.
const _: () = assert!(
    ASK_WAIT_SECS < ASK_TIMEOUT_SECS,
    "the ask must resolve inside the call that carries it, or the deny \
     and its audit line are both lost to the outer timeout"
);
const _: () = assert!(
    ASK_WAIT_SECS > TOOL_TIMEOUT_SECS,
    "and it must still be a human-sized wait, not an AWS-sized one"
);

// The relation between the two budgets, enforced at COMPILE time.
// Clippy caught these as constant assertions when they sat in a test,
// and it was right: two consts cannot disagree at runtime, so a
// runtime check is theatre. Here the build fails instead.
//
// Lower bound: a person reading a plan needs materially more than a
// hung socket does. Upper bound: an ask nobody will answer has to end,
// because the rule is deny on no-answer and a call blocked forever
// never reaches it.
const _: () = assert!(ASK_TIMEOUT_SECS > TOOL_TIMEOUT_SECS * 5);
const _: () = assert!(ASK_TIMEOUT_SECS <= 900);

/// What an operator said when asked.
///
/// Three outcomes, not two. "They said no" and "nobody answered" are
/// different facts and the audit line should not conflate them — but
/// they have the same EFFECT, because the design's rule is that an
/// unanswerable ask degrades to deny and never to allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AskOutcome {
    Approved,
    Declined,
    /// Asked, and nobody answered within the budget. Denies.
    Unanswered,
    /// The question never left ebman — no channel, or the send
    /// failed. Denies exactly as `Unanswered` does, and is separate
    /// only so the audit does not claim a question was put to somebody
    /// when none was sent. `stage=asked` means "this was asked"; a
    /// line saying `unanswered, elapsed_ms=0` for a frame that never
    /// reached a client is the over-claiming twin of the false record
    /// that stage exists to prevent.
    Undeliverable,
    /// The client answered with a JSON-RPC error: it could not put
    /// the question at all. Denies, like every other non-answer, but
    /// it is not a refusal and must not be reported as one — nobody
    /// was asked.
    Unsupported,
    /// The operator accepted, but the text they typed did not match
    /// what was required — or the client returned no text at all,
    /// which is what a client that cannot render an input field does.
    ///
    /// Distinct from `Declined` because nobody refused: this is a
    /// failed confirmation, and the log should not record a decline
    /// that did not happen. Denies either way.
    Unconfirmed,
    /// No ask was possible because this client never declared
    /// elicitation. ONLY that — a missing channel or a failed send on
    /// a client that CAN elicit is `Unanswered`, because there the ask
    /// was the gate and losing it must deny. Distinct from `Unanswered` because it has
    /// the opposite consequence: nothing was asked, so nothing was
    /// refused, and the write falls back to whatever gated it before
    /// (the `--allow-writes` opt-in). Conflating the two denied every
    /// write on every client that cannot elicit, including the ones
    /// an operator had explicitly granted with the flag.
    NotAsked,
}

impl AskOutcome {
    /// Does this outcome stop the write?
    ///
    /// `NotAsked` does not: no question was put, so there is no answer
    /// to respect, and the surface it reached was gated by the flag.
    pub(crate) fn refuses(self) -> bool {
        matches!(
            self,
            AskOutcome::Declined
                | AskOutcome::Unanswered
                | AskOutcome::Unconfirmed
                | AskOutcome::Unsupported
                | AskOutcome::Undeliverable
        )
    }

    /// The audit vocabulary for `stage=asked`.
    ///
    /// Separate from `reason()`, which is prose for the agent. This is
    /// a token a log reader filters on, so it is a fixed small set and
    /// stays stable across wording changes to the agent-facing text.
    pub(crate) fn answer_label(self) -> &'static str {
        match self {
            AskOutcome::Approved => "approved",
            AskOutcome::Declined => "declined",
            AskOutcome::Unanswered => "unanswered",
            AskOutcome::Unconfirmed => "unconfirmed",
            AskOutcome::Unsupported => "unsupported",
            AskOutcome::Undeliverable => "undeliverable",
            // Never reaches the audit — the caller skips `NotAsked`,
            // because no question was put and a line claiming one was
            // is exactly the false record this stage exists to avoid.
            AskOutcome::NotAsked => "not_asked",
        }
    }

    /// What the agent should do next, which is NOT the same for a
    /// decline and a silence.
    ///
    /// Both used to end with the decline guard verbatim — "do not
    /// re-plan unless the operator asks for it". For a timeout that
    /// leaves an agent with no legitimate move at all, because the one
    /// party who could unblock it is the party who demonstrably was
    /// not there. A peer session hit exactly this, reasoned its own
    /// way to "tell the human it expired", and reported that it had to
    /// invent the redirect.
    ///
    /// A decline is an answer and ends the line. A silence is the
    /// absence of one: the operator may have stepped away, or never
    /// been shown the dialog, and saying so is the correct next act.
    ///
    /// The retry permission is STATED rather than implied. An earlier
    /// version said "do not quietly re-plan it", and the same peer
    /// reported that as the one place it still had to reason: the
    /// adverb implies a non-quiet re-plan is allowed, which is an
    /// inference, not an instruction. It could not simply be given the
    /// decline's "unless the operator asks for it" clause — that names
    /// the absent party as the only route, which is the whole defect
    /// being fixed, and a test pins its absence here. "If they ask you
    /// to try again, plan it fresh" grants the exception without
    /// naming anyone as the unblock.
    ///
    /// The decline wording stays a FLAT prohibition with one named
    /// exception rather than a rationale about the operator's intent.
    /// The same peer noted that the pull it felt was task-completion
    /// pressure, not permission-seeking — a teammate had asked it for
    /// the decline string — and that a rationale-shaped guard
    /// ("respect the refusal") would not have caught that, because
    /// nobody had refused anything. The flat form did.
    pub(crate) fn guidance(self) -> &'static str {
        match self {
            AskOutcome::Declined => {
                "The plan is spent; do not re-plan the same action unless the operator \
                 asks for it. Say the confirmation was declined — do NOT tell your \
                 user a person refused, unless you independently know one was there. \
                 A non-interactive client (a `-p` run, a CI harness) declares the \
                 same capability and declines automatically with nobody present, and \
                 that is indistinguishable from here."
            }
            AskOutcome::Undeliverable => {
                "The plan is spent. The confirmation could not be delivered — your \
                 client is gone or its channel is closed, so NOBODY was asked. Not a \
                 decline. Say the connection dropped before the operator could be \
                 asked."
            }
            AskOutcome::Unsupported => {
                "The plan is spent. Your client could not present the confirmation — \
                 it answered with an error, so NOBODY was asked and nobody refused. \
                 Do not report this as a decline. Tell the operator their client \
                 cannot show ebman's confirmations, and that writes need either a \
                 client that can or `--allow-writes` on one that cannot be asked."
            }
            AskOutcome::Unconfirmed => {
                "The plan is spent. Nobody declined — the confirmation text did not \
                 match, which for this verb is required and is typed by the OPERATOR, \
                 not by you. If their client cannot show a text field, this verb \
                 cannot be confirmed there at all: say so and suggest the TUI, or \
                 `--allow-writes` on a client that can. If they simply mistyped, they \
                 can ask you to try again."
            }
            AskOutcome::Unanswered => {
                "The plan is spent. Nobody answered, which is NOT a refusal — the \
                 operator may have stepped away, or may never have been shown the \
                 dialog. Tell them it expired and let them decide. If they ask you \
                 to try again, plan it fresh; do not re-plan it on your own \
                 initiative, and do not report this as a refusal."
            }
            // Neither reaches an agent: `Approved` dispatches, and
            // `NotAsked` falls through to the flag that granted it.
            AskOutcome::Approved | AskOutcome::NotAsked => "",
        }
    }

    /// For the audit line and the agent-facing refusal.
    pub(crate) fn reason(self) -> &'static str {
        match self {
            AskOutcome::Approved => "approved",
            // NOT "declined by the operator". Measured 2026-09-20:
            // headless `claude -p` declares elicitation, auto-declines
            // in under a second, and there is no human in the session
            // at all. Asserting one made a decision is a false record,
            // and the agent relays it — the run that found this told
            // its user "the operator simply said no".
            AskOutcome::Declined => "the confirmation was declined",
            AskOutcome::Unanswered => "no answer within the ask window",
            AskOutcome::Unconfirmed => {
                "the typed confirmation did not match (or your client returned none)"
            }
            AskOutcome::Unsupported => {
                "your client could not present the confirmation and returned an error"
            }
            AskOutcome::Undeliverable => "the confirmation could not be delivered to your client",
            AskOutcome::NotAsked => "not asked",
        }
    }
}

/// Map a client's `elicitation/create` reply to an outcome.
///
/// Pure, because the interesting part is the mapping and the rest is
/// plumbing. Per MCP, the reply carries an `action` of `accept`,
/// `decline` or `cancel`. Anything else — a malformed reply, a missing
/// action, an error response — is NOT an approval: this is the one
/// place where being generous would convert a broken client into a
/// standing yes.
pub(crate) fn ask_outcome_from(reply: &Value) -> AskOutcome {
    if reply.get("error").is_some() {
        // NOT `Declined`. An error response means the client could not
        // present the question — an unsupported method, a schema it
        // cannot render, a transport fault. Nobody refused anything,
        // and reporting a decline puts a decision in the mouth of
        // whoever was not asked. That is the same false attribution
        // the decline wording was just rewritten to stop, and it was
        // still live on the adjacent branch: the changelog claimed a
        // client that cannot render a text field "returns no content",
        // which is one of at least two ways it can fail and the only
        // one that was handled.
        //
        // Still denies. Only the label changes.
        return AskOutcome::Unsupported;
    }
    match reply
        .get("result")
        .and_then(|r| r.get("action"))
        .and_then(Value::as_str)
    {
        Some("accept") => AskOutcome::Approved,
        Some("decline") | Some("cancel") => AskOutcome::Declined,
        _ => AskOutcome::Declined,
    }
}

/// How long this tool call may take.
///
/// Pure so the decision is testable without a client, a clock or a
/// dialog — the frame loop that consumes it is reachable only through
/// stdio, which is where the `wants_file_logging` decision had to go
/// for the same reason.
///
/// `confirm_action` is the only tool that can block on a person: it is
/// the single point every write dispatches through, and therefore the
/// single point the ask fires at. Everything else is AWS-bound and
/// keeps the AWS bound. A connection that cannot be asked keeps it
/// too, because nothing will stop to ask.
pub(crate) fn call_timeout_secs(tool: &str, client_can_elicit: bool) -> u64 {
    if tool == writes::CONFIRM_TOOL && client_can_elicit {
        ASK_TIMEOUT_SECS
    } else {
        TOOL_TIMEOUT_SECS
    }
}

#[derive(Debug, PartialEq, Eq)]
struct McpArgs {
    demo: bool,
    no_redact: bool,
    write_scope: WriteScope,
    /// The operator said no to writes on THIS server, whatever the
    /// client can do. Distinct from `write_scope == None`, which is
    /// "said nothing" and is what elicitation fills in.
    read_only: bool,
}

const MCP_USAGE: &str = "usage: ebman mcp <serve [--demo] [--no-redact] [--read-only] \
     [--allow-writes[=verb,verb]] | setup [--allow-writes[=verb,verb]]>";

fn parse_mcp_args(args: &[String]) -> Result<McpArgs, String> {
    // args[0] = "mcp"; the only sub-verb is "serve".
    if args.get(1).map(String::as_str) != Some("serve") {
        return Err(MCP_USAGE.into());
    }
    let mut demo = false;
    let mut no_redact = false;
    let mut write_scope = WriteScope::None;
    let mut saw_write_flag = false;
    let mut read_only = false;
    let known: Vec<String> = writes::write_verb_names();
    let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
    for arg in args.iter().skip(2) {
        // `--allow-writes` alone still means every verb, so an existing
        // `.mcp.json` keeps working. `--allow-writes=a,b` narrows it.
        if arg == "--read-only" {
            // The MCP-scoped standing no.
            //
            // `safety.read_only` already refuses every write, and it
            // refuses them EVERYWHERE — TUI and CLI included. Before
            // this flag, an operator who wanted their agent read-only
            // while keeping their own TUI usable had no way to say so:
            // writes became available by default to any client that
            // can be asked, and the only "no" was one that disabled
            // their own hands too.
            //
            // A flag rather than a config key, matching
            // `--allow-writes`: the write posture of an MCP server
            // stays visible in the process table and `.mcp.json`. It
            // only ever says NO, which is the direction config is
            // allowed to move in.
            if read_only {
                return Err(format!("ebman mcp: --read-only given twice — {MCP_USAGE}"));
            }
            read_only = true;
            continue;
        }
        if let Some(rest) = arg.strip_prefix("--allow-writes") {
            // Repeating it is an error, not last-wins. Last-wins is the
            // ordinary convention and wrong here: `--allow-writes=dlq_delete
            // --allow-writes` would silently widen a deliberately narrow
            // grant to every verb, which is the fail-open this flag exists
            // to remove. A `.mcp.json` args array is hand-edited often
            // enough for a stray duplicate to be plausible.
            if saw_write_flag {
                return Err(format!(
                    "ebman mcp: --allow-writes given more than once — a second one \
                     would silently widen the first. Name every verb in one flag: \
                     --allow-writes=a,b — {MCP_USAGE}"
                ));
            }
            saw_write_flag = true;
            let value = match rest {
                "" => None,
                v => Some(v.strip_prefix('=').ok_or_else(|| {
                    format!("ebman mcp: expected `--allow-writes=verbs` — {MCP_USAGE}")
                })?),
            };
            write_scope =
                parse_write_scope(value, &known_refs).map_err(|e| format!("ebman mcp: {e}"))?;
            continue;
        }
        match arg.as_str() {
            "--demo" => demo = true,
            "--no-redact" => no_redact = true,
            other => return Err(format!("ebman mcp: unknown flag '{other}' — {MCP_USAGE}")),
        }
    }
    if read_only && saw_write_flag {
        // Refused rather than resolved. Both flags together is an
        // operator who does not know what this server will do, and
        // picking a winner silently — either way — hands them a
        // posture they did not choose. Every other bad flag
        // combination here fails at startup for the same reason: a
        // registration that is wrong should break when you write it,
        // not the first time an agent tries to write.
        return Err(format!(
            "ebman mcp: --read-only and --allow-writes contradict each other — {MCP_USAGE}"
        ));
    }
    if read_only {
        write_scope = WriteScope::None;
    }
    Ok(McpArgs {
        demo,
        no_redact,
        write_scope,
        read_only,
    })
}

/// Which write verbs this server may dispatch.
///
/// `--allow-writes` was a single bool: enabling it to delete one
/// dead-lettered message also granted deploy, restart, rebuild,
/// terminate and set_option across every environment the credentials
/// reach. A client declined the grant on exactly those grounds, which
/// is both the right call and the evidence the flag was too coarse.
///
/// Scoping stays a SERVER FLAG rather than moving into config, so
/// `docs/design/protection-levels.md` Principle 5 holds trivially: the
/// ceiling is set outside the request and no request content can raise
/// it. It is also visible in the process table and in `.mcp.json`,
/// which is why write capability was flag-only in the first place.
///
/// Deliberately NOT per-environment. Differentiated ceremony — ask on
/// prod, proceed on demo — was considered and declined: on this fleet
/// the demo environment is client-facing, so it is not the low-stakes
/// tier that argument assumes. A gate costing the same everywhere is
/// also the one an operator keeps reading.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WriteScope {
    /// No write tool is advertised or dispatched.
    None,
    /// Every write verb — what a bare `--allow-writes` has always meant.
    All,
    /// Only these verbs. Others are not advertised AND refused at
    /// dispatch; unadvertised alone would leave a tool callable by a
    /// client that had cached an older list.
    Only(Vec<String>),
}

impl WriteScope {
    pub(crate) fn allows(&self, tool: &str) -> bool {
        match self {
            WriteScope::None => false,
            WriteScope::All => true,
            WriteScope::Only(v) => v.iter().any(|t| t == tool),
        }
    }

    pub(crate) fn any(&self) -> bool {
        match self {
            WriteScope::None => false,
            WriteScope::All => true,
            // An empty `Only` grants nothing, so it must READ as
            // nothing. The parser cannot produce one, but a scope that
            // answered `any() == true` while `allows()` refused every
            // verb would advertise `confirm_action` beside no verb to
            // confirm — a server that looks write-capable and is not.
            WriteScope::Only(v) => !v.is_empty(),
        }
    }

    /// How this grant reads to the AGENT, for the `instructions` block.
    ///
    /// A withheld tool is simply absent from `tools/list`, and absent
    /// is ambiguous: it reads as "ebman cannot do this" when it means
    /// "you were not granted this". That is the same confusion the
    /// version line above exists to prevent, and it has the same cost
    /// — an agent reporting a capability gap that is really a config
    /// choice, instead of asking the operator to widen the grant.
    /// `can_ask` is whether this connection's client declared
    /// elicitation. It changes what a confirm *means* — with the ask,
    /// a human sees the action and may say no — and an agent that does
    /// not know that reads a decline as a bug and retries it. This is
    /// authored text for exactly that reason: the capability is
    /// negotiated in protocol metadata the client sees, which never
    /// reaches the agent reading these instructions.
    fn agent_summary(
        &self,
        standing_refusal: Option<&str>,
        can_ask: bool,
        opened_by_ask: bool,
    ) -> String {
        // A standing refusal OUTRANKS the scope, so it is said first
        // and the scope is not said at all. Describing the grant on a
        // server that refuses every write told the agent "Writes are
        // ENABLED for every verb" while nothing could write — true
        // about the flag, false about the server, and wrong in the one
        // channel an agent is guaranteed to read.
        if let Some(why) = standing_refusal {
            return format!(
                "Writes are REFUSED on this server, regardless of any grant: {why} No plan \
                 will dispatch and no confirmation will lift it. Only the operator changing \
                 that control can. Do not plan writes and do not report this as a fault — \
                 `doctor` reports it too."
            );
        }
        match self {
            WriteScope::None => "This server is READ-ONLY: no write tool is available. ASK the \
                 operator to restart it with --allow-writes (optionally \
                 --allow-writes=verb,verb to grant only what you need) — asking \
                 is the whole of your part in it. Do not edit the MCP config \
                 yourself: a grant is not yours to make, and in at least one \
                 client that edit is refused as self-modification, so trying it \
                 costs a denial and teaches nothing."
                .to_string(),
            WriteScope::All => {
                let mut t = "Writes are ENABLED for every verb, via the two-phase \
                     plan-then-confirm protocol."
                    .to_string();
                if can_ask {
                    t.push_str(ASK_NOTE);
                }
                if opened_by_ask {
                    t.push_str(OPENED_BY_ASK_NOTE);
                }
                t
            }
            WriteScope::Only(v) => {
                format!(
                    "Writes are NARROWLY granted: {} only, via the two-phase plan-then-confirm \
                 protocol. Any other write verb is absent from this list because it was NOT \
                 GRANTED, not because ebman lacks it — say so and ask the operator to widen \
                 the grant rather than reporting it as unsupported. Asking is your part; \
                 the config edit is theirs. If they say they granted a verb and it is still \
                 missing here, the likeliest cause is a client that reconnected without \
                 re-reading its config — ask them to restart the client before either of you \
                 concludes it is broken.",
                    v.join(", ")
                ) + if can_ask { ASK_NOTE } else { "" }
            }
        }
    }
}

/// Appended when the write surface exists ONLY because this client can
/// be asked — no `--allow-writes` was given.
///
/// The operator never made a config edit to enable this. On 0.42 a
/// bare registration gains the write surface the moment the client
/// reconnects against a 0.42 binary, so the act that granted it was
/// pressing Reconnect — something operators do for unrelated reasons,
/// and did twice in one afternoon during this release's own testing.
///
/// ebman has no channel to the operator except a dialog, and raising
/// a dialog to announce a capability would be its own kind of rude.
/// The agent is the only path to them, so the agent is asked to say
/// it once. A peer session working on live infrastructure spotted
/// this and told its operator before touching anything, which is the
/// behaviour this text exists to make ordinary rather than
/// exceptional.
const OPENED_BY_ASK_NOTE: &str = "\n\nWORTH SAYING ONCE, EARLY: writes are \
     available here because YOUR CLIENT can put a question to the operator, not \
     because they passed a flag. They may not know the surface widened — on a bare \
     registration it widens when the client reconnects against this build. Tell them \
     plainly the first time it becomes relevant, before you plan a write rather than \
     after. Name the BREADTH, not just the fact: the default is every verb, \
     `terminate` included, so an operator who wanted one narrow thing got the rest \
     alongside it. Say that every action will be put to them and they can decline, \
     that `--allow-writes=verb,verb` narrows this permanently, and that \
     `--read-only` keeps the old posture if they would rather. Do not treat this as a \
     reason to avoid proposing work — it is a reason they should not be surprised by \
     it.";

/// Appended to a granted-writes summary when the operator can be asked.
///
/// Kept whole rather than inlined twice: the two grant arms said the
/// same thing about confirmation and drifted apart once already.
const ASK_NOTE: &str = "\n\nEach confirmation is sent to your CLIENT to put to the \
     operator. Expect `confirm_action` to take as long as a person takes — or to come \
     back at once, which is what a non-interactive client does when it answers for \
     itself. A decline is FINAL — not an error, not a missing permission: do not \
     re-plan the same action, do not ask for the grant to be widened, and do not \
     report it as a fault. Say the confirmation was declined, and stop. Do NOT tell \
     your user a person refused unless you independently know one was there: ebman \
     cannot tell an operator answering a dialog from a client answering for itself, \
     and saying otherwise puts a decision in someone's mouth.\n\nSURFACE THE PLAN; \
     DO NOT RESTATE THE CASE FOR IT. The confirmation already names the action, the \
     targets, the identity and what it forecloses, and the operator is about to read \
     it. Re-deriving the reasoning in your own message — especially reasoning you and \
     they settled days ago — turns a gate they approved of into a tax they resent, and \
     the tax is charged on every single write. Say what you are about to do in a line, \
     and let the dialog do the rest.";

/// The write verbs, for the docs-drift guard in `app::tests`.
///
/// `writes` is private to this module; this is the one seam out, kept
/// narrow deliberately rather than widening the module's visibility.
#[cfg(test)]
pub(crate) fn write_verb_names_for_docs() -> Vec<String> {
    writes::write_verb_names()
}

/// Does this `ebman mcp …` invocation want file logging switched on?
///
/// `serve` does; `setup` does not. The split matters in both
/// directions. `setup` is a pure printer whose module doc promises it
/// writes **no** files, and opening a log would make that false.
/// `serve` is a long-lived daemon nobody watches interactively, so a
/// log is the only way to see anything it says.
///
/// This exists because every subcommand returns from `main` BEFORE
/// `init_logging`, by a deliberate design that predates the MCP server
/// ("handle CLI flags before any TUI / logging setup so they print
/// cleanly"). Correct for `--version`; wrong for a daemon. The whole
/// subcommand surface contained exactly one `tracing::` call, and it
/// was the one recording whether a connecting MCP client supports
/// elicitation — the measurement `PLAN.md` stage 3 installed to turn
/// stage 5's stop condition from a guess into data. It had written
/// nothing, and could not: a probe declaring elicitation support moved
/// the log by zero bytes.
///
/// Logging here is FILE-ONLY and must stay that way. `serve` speaks
/// JSON-RPC on stdout; a stdout layer would interleave log lines into
/// the protocol and break every client.
pub fn wants_file_logging(args: &[String]) -> bool {
    args.get(1).map(String::as_str) == Some("serve")
}

/// Whether this server should read the audit config from disk.
///
/// Extracted from `run` because `run` is the process entry — it owns
/// stdin, stdout and the event loop, so no lib test reaches it and a
/// mutation sweep reports every decision inside it as uncovered. Both
/// halves of this one were: flipping `&&` to `||` and dropping the `!`
/// each left the suite green.
///
/// Neither is cosmetic. The first makes a reads-only server read the
/// config disk it is documented not to touch; the second inverts demo
/// and live, so the hermetic mode gains a webhook and the real one
/// loses it.
///
/// Same move as `no_state_output` for the same reason: a decision only
/// reachable through I/O is a decision no test can pin.
pub(crate) fn should_init_audit(scope: &WriteScope, demo: bool) -> bool {
    scope.any() && !demo
}

/// Parse the value half of `--allow-writes[=a,b]`.
///
/// An unknown verb is an ERROR, never a silent skip. A typo'd
/// `--allow-writes=dlq_delte` that quietly granted nothing would look
/// exactly like a working narrow grant until the first write was
/// refused; one that quietly granted everything would be worse. Same
/// fail-closed reasoning as the safety-pin parser.
pub(crate) fn parse_write_scope(value: Option<&str>, known: &[&str]) -> Result<WriteScope, String> {
    let Some(v) = value else {
        return Ok(WriteScope::All);
    };
    let mut out = Vec::new();
    for raw in v.split(',') {
        let name = raw.trim();
        if name.is_empty() {
            continue;
        }
        if !known.contains(&name) {
            let mut sorted: Vec<&str> = known.to_vec();
            sorted.sort_unstable();
            return Err(format!(
                "unknown write verb '{name}' — known verbs: {}",
                sorted.join(", ")
            ));
        }
        if !out.iter().any(|e| e == name) {
            out.push(name.to_string());
        }
    }
    if out.is_empty() {
        return Err("--allow-writes= was given with no verbs — omit the \
                    `=` to allow all, or name at least one"
            .into());
    }
    Ok(WriteScope::Only(out))
}

/// Non-object frames (batch arrays, bare scalars) are invalid
/// requests per JSON-RPC 2.0 — answer -32600 rather than silently
/// dropping them (a strict client would wait out its timeout). MCP
/// 2025-06-18 removed batching, so arrays are not-supported by spec.
/// Returns the response frame to send, `None` for a valid object.
fn invalid_request_response(req: &Value) -> Option<String> {
    if req.is_object() {
        return None;
    }
    Some(
        json!({
            "jsonrpc": "2.0",
            "id": null,
            "error": {"code": -32600, "message": "invalid request: expected a single JSON-RPC object"}
        })
        .to_string(),
    )
}

pub(crate) use crate::util::redact_option_value;

enum Backend {
    Aws,
    Demo,
}

/// Identity of the binary this server is running.
///
/// Captured once at startup so a later `stat` of the same path can tell
/// whether the file was replaced underneath the running process — which
/// is exactly what `brew upgrade` does, leaving the server on the old
/// inode with no way for a client to notice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExeIdentity {
    pub path: String,
    /// `None` when the binary could not be stat'ed. Kept rather than
    /// defaulted: "we could not look" must not become "unchanged".
    pub inode: Option<u64>,
}

impl ExeIdentity {
    fn of(path: &std::path::Path) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            path: path.display().to_string(),
            inode: std::fs::metadata(path).ok().map(|m| m.ino()),
        }
    }

    pub(crate) fn current() -> Option<Self> {
        std::env::current_exe().ok().map(|p| Self::of(&p))
    }

    /// Build an identity pointing at an arbitrary path.
    ///
    /// Test seam. Without it the prepend is only reachable by replacing
    /// the test binary underneath itself, so the WIRING would go
    /// untested while the pure halves passed — the gap this session has
    /// hit repeatedly.
    #[cfg(test)]
    pub(crate) fn for_tests(path: &std::path::Path) -> Self {
        Self::of(path)
    }

    /// Re-stat the same path and report whether it is now a different
    /// file.
    fn is_stale(&self) -> bool {
        staleness(self.inode, Self::of(std::path::Path::new(&self.path)).inode)
    }
}

/// Whether the binary on disk is a different file from the one running.
///
/// Pure so both directions are testable without replacing a binary
/// underneath a test process. Deliberately conservative: an unreadable
/// file is NOT reported stale. Verified empirically that replacing a
/// file changes its inode while `current_exe()` still resolves the
/// path — that is the whole mechanism.
///
/// Unknown on either side means unknown, never "changed": a server that
/// cried staleness whenever it could not stat itself would be noise,
/// and noise is how a real warning gets ignored.
pub(crate) fn staleness(at_start: Option<u64>, now: Option<u64>) -> bool {
    match (at_start, now) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

/// The line prepended to a tool response when the running binary is no
/// longer the one on disk.
///
/// Says what is running, what changed, and what to do. An agent cannot
/// act on "you are stale" — it can act on "reconnect".
pub(crate) fn stale_binary_notice(path: &str) -> String {
    format!(
        "[ebman {} is running, but a different build is now installed at {}. \
         This server keeps the old one until the connection is re-established. \
         ASK YOUR OPERATOR to reconnect (in Claude Code: /mcp, Reconnect) — you \
         cannot do it yourself: it is a client action, not a tool call, and \
         there is no MCP message a server can send to trigger one. Results \
         below are from the running build.]",
        env!("CARGO_PKG_VERSION"),
        path
    )
}

pub(crate) struct Server {
    backend: Backend,
    redact: bool,
    write_scope: WriteScope,
    /// Safety config loaded once at startup (pins). Demo servers get
    /// the default (hermetic).
    safety_cfg: crate::config::Config,
    /// Two-phase write state: the single pending-plan slot (spec:
    /// writes are serialized server-wide).
    writes: tokio::sync::Mutex<writes::WriteState>,
    /// Frames this server originates, for the writer task to drain.
    ///
    /// Until elicitation, every frame ebman sent was a RESPONSE to a
    /// request the client made, so the response could simply be
    /// returned up the call stack. An ask is the server originating a
    /// REQUEST, which needs a way out that is not a return value.
    outbound: std::sync::Mutex<Option<tokio::sync::mpsc::Sender<String>>>,
    /// Asks awaiting an answer, by request id.
    ///
    /// The frame loop treats every inbound frame as a request. A reply
    /// to one of ours is a frame with an id we issued and no `method`,
    /// which that loop would have answered `-32601`. This is how it
    /// tells them apart.
    pending_asks:
        std::sync::Mutex<std::collections::HashMap<i64, tokio::sync::oneshot::Sender<Value>>>,
    next_ask_id: std::sync::atomic::AtomicI64,

    /// Messages this server destroyed and can still put back.
    ///
    /// In memory, never on disk: a dead-lettered body can carry
    /// customer data — `mcp.peek_bodies` exists for that reason — and a
    /// durable copy would be worse than showing one to an agent. Dies
    /// with the process, by design.
    deleted: tokio::sync::Mutex<Vec<writes::DeletedMessage>>,
    /// Whether a write dispatch is in flight — separate AtomicBool so
    /// the confirm path's RAII guard can reset it on an unwind
    /// (pre-tag review I2).
    dispatching: std::sync::atomic::AtomicBool,
    /// Whether the client declared the `elicitation` capability at
    /// initialize.
    ///
    /// Captured because `ask` — the middle rung of the protection
    /// levels in `docs/design/protection-levels.md` — needs a way to
    /// put a question to a human, and over MCP that is elicitation. The
    /// design note could not say whether that is usable in practice,
    /// and the honest way to settle it is to record what real clients
    /// declare rather than to guess.
    ///
    /// Nothing branches on this yet. It is logged at initialize so the
    /// question has an answer before the levels work depends on it.
    client_supports_elicitation: std::sync::atomic::AtomicBool,
    /// `--read-only`: the operator said no to writes on this server,
    /// whatever the client can do.
    mcp_read_only: std::sync::atomic::AtomicBool,
    /// `clientInfo.name` from initialize — lands in audit extras so
    /// agent-dispatched writes are attributable.
    client_name: std::sync::Mutex<String>,
    /// The binary this server started from.
    ///
    /// `brew upgrade` replaces the file underneath a running server,
    /// which then keeps answering from the old build with nothing to
    /// tell a client. A reader spent two days reporting capability gaps
    /// against a stale binary for exactly this reason — the handshake
    /// carries the version, but the handshake already happened.
    exe: Option<ExeIdentity>,
    /// Test seam: an injected client, used in place of building one per
    /// call.
    ///
    /// Everything BELOW the tool bodies is now covered — renderers,
    /// extracted decisions, the AWS-layer calls via `aws_smithy_mocks`
    /// — but the orchestration itself was not, because `client()`
    /// builds a real `AwsClient` from ambient credentials. That is the
    /// layer the `tool_why` dead-letter peek bug lived in, and it
    /// survived a full review there.
    ///
    /// `App::for_tests` takes its client the same way, for the same
    /// reason.
    #[cfg(test)]
    injected_client: Option<std::sync::Arc<aws::AwsClient>>,
}

impl Server {
    /// The one constructor. It takes a scope rather than a bool
    /// because a bool could only ever mean "all or nothing", and
    /// spelling that as `true` at eight call sites is how the wider
    /// grant becomes the default nobody notices.
    pub(crate) fn with_scope(demo: bool, no_redact: bool, scope: WriteScope) -> Self {
        let safety_cfg = if demo {
            crate::config::Config::default()
        } else {
            crate::config::load()
        };
        Self::with_config(demo, no_redact, scope, safety_cfg)
    }

    /// Test seam: inject the safety config (pin tests must not read
    /// the operator's real config.toml).
    pub(crate) fn with_config(
        demo: bool,
        no_redact: bool,
        scope: WriteScope,
        safety_cfg: crate::config::Config,
    ) -> Self {
        Server {
            backend: if demo { Backend::Demo } else { Backend::Aws },
            redact: !no_redact,
            write_scope: scope,
            safety_cfg,
            writes: tokio::sync::Mutex::new(writes::WriteState::default()),
            outbound: std::sync::Mutex::new(None),
            pending_asks: std::sync::Mutex::new(std::collections::HashMap::new()),
            // Well clear of any id a client is likely to use for its
            // own requests; ids only need to be unique per direction,
            // but a collision would be maddening to diagnose.
            next_ask_id: std::sync::atomic::AtomicI64::new(1_000_000),
            deleted: tokio::sync::Mutex::new(Vec::new()),
            dispatching: std::sync::atomic::AtomicBool::new(false),
            client_name: std::sync::Mutex::new("unknown".to_string()),
            client_supports_elicitation: std::sync::atomic::AtomicBool::new(false),
            mcp_read_only: std::sync::atomic::AtomicBool::new(false),
            exe: ExeIdentity::current(),
            #[cfg(test)]
            injected_client: None,
        }
    }

    /// Build a server whose tool bodies talk to `client` instead of
    /// ambient AWS.
    ///
    /// `Backend::Aws`, deliberately: the demo backend short-circuits
    /// most tool bodies before they reach a client, which is precisely
    /// the orchestration this exists to exercise.
    /// Point the staleness check at a path a test controls.
    #[cfg(test)]
    pub(crate) fn watching_exe(mut self, path: &std::path::Path) -> Self {
        self.exe = Some(ExeIdentity::for_tests(path));
        self
    }

    #[cfg(test)]
    pub(crate) fn with_injected_client(
        scope: WriteScope,
        safety_cfg: crate::config::Config,
        client: aws::AwsClient,
    ) -> Self {
        let mut s = Self::with_config(false, false, scope, safety_cfg);
        s.injected_client = Some(std::sync::Arc::new(client));
        s
    }

    /// Put a question in front of the operator and wait for the answer.
    ///
    /// This is the gate. Until it existed the two-phase protocol was
    /// honour-system: an agent could plan and confirm without ever
    /// surfacing the plan, and nothing made it stop.
    ///
    /// Never degrades to approved. Distinguishes two non-answers that
    /// have opposite consequences: `NotAsked` when there was nobody to
    /// ask (no client capability, no channel) and the write falls back
    /// to whatever gated it before, versus `Unanswered` when the
    /// question went out and nobody replied in time — an operator who
    /// has walked away must produce a deny.
    /// The write surface for *this connection*.
    ///
    /// A client that can be asked gets parity with the TUI by default:
    /// the operator sees and answers every confirmation, so the flag
    /// is not carrying the safety — the human is. This is the whole
    /// usability claim of the design. Without it an operator has to
    /// stop, edit a config file and restart their client the first
    /// time they want a write, which is where people give up.
    ///
    /// An *explicitly narrowed* grant is still honoured. `--allow-writes=
    /// restart` is an operator saying "only this", and widening it back
    /// to everything because the client happens to support a dialog
    /// would override a restriction they typed on purpose. `None` is
    /// different in kind: it is the operator having said nothing at
    /// all, which is what the parity default is for.
    pub(crate) fn effective_scope(&self) -> WriteScope {
        // `--read-only` is a standing NO and outranks the parity
        // default: the operator asked for a server that cannot write,
        // and a client that happens to support dialogs is not their
        // permission to change that. Checked first so nothing below
        // can widen past it.
        if self
            .mcp_read_only
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return WriteScope::None;
        }
        if matches!(self.write_scope, WriteScope::None)
            && self
                .client_supports_elicitation
                .load(std::sync::atomic::Ordering::Relaxed)
        {
            return WriteScope::All;
        }
        self.write_scope.clone()
    }

    /// Withdraw a request we are no longer waiting on.
    ///
    /// Best-effort by design: it is a notification, so there is no
    /// reply to await, and a dead channel here means the client is
    /// already gone — which is the case where the dialog cannot be
    /// showing anyway.
    async fn notify_cancelled(&self, id: i64, reason: &str) {
        let Some(tx) = self.outbound.lock().ok().and_then(|g| g.clone()) else {
            return;
        };
        let frame = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": id, "reason": reason }
        });
        // `try_send`, never `send`. This runs between the ask deadline
        // and the deny that follows it, inside the outer call budget.
        // A client frozen with a full outbound queue — which is
        // exactly the client that just failed to answer a dialog —
        // would block an awaited send, the outer timeout would drop
        // the future, and the designed deny AND its `not_approved`
        // audit line would be lost to a generic tool timeout. That is
        // the precise loss `ASK_WAIT_SECS`'s margin exists to prevent,
        // and a courtesy notification must not reintroduce it.
        let _ = tx.try_send(frame.to_string());
    }

    /// `typed` demands the operator TYPE a string into the dialog, and
    /// requires it to match exactly.
    ///
    /// For `terminate` and `dlq_purge`, matching what the TUI already
    /// does — both are strict-typed-name confirms there, while the MCP
    /// side only ever checked a `confirm_name` the AGENT supplied, so
    /// the human's whole contribution to destroying an environment was
    /// one click. "Same bargain as the TUI" is the justification for
    /// writes-by-default, and for those two verbs it was not true.
    ///
    /// Fails CLOSED. A client that cannot render an input field
    /// returns no content, which does not match, which denies — so
    /// these two verbs become unusable there rather than quietly
    /// one-click. That is the right direction, and it is why the
    /// refusal says plainly what happened instead of leaving an
    /// operator wondering why terminate stopped working.
    pub(crate) async fn ask_operator(&self, summary: &str, typed: Option<&str>) -> AskOutcome {
        if !self
            .client_supports_elicitation
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return AskOutcome::NotAsked;
        }
        // No channel on a client that CAN elicit is a dead ask, not an
        // absent one — the client vanished mid-confirm, or the writer
        // task is gone. `NotAsked` would fall back to the flag, and on
        // a connection widened purely by elicitation there is no flag
        // to fall back to: the ask IS the gate. That path dispatched
        // an unapproved terminate. `Unanswered` denies.
        let Some(tx) = self.outbound.lock().ok().and_then(|g| g.clone()) else {
            return AskOutcome::Undeliverable;
        };

        let id = self
            .next_ask_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if let Ok(mut map) = self.pending_asks.lock() {
            map.insert(id, reply_tx);
        }

        // An empty object for an ordinary confirmation — the answer
        // lives in `action` and there is nothing to collect; an empty
        // object rather than an omitted field, because the field is
        // required and a client that validates it should get something
        // valid. A required string property where the operator must
        // type something back.
        let schema = match typed {
            None => json!({"type": "object", "properties": {}}),
            Some(_) => json!({
                "type": "object",
                "properties": {
                    "confirm": {
                        "type": "string",
                        "description": "Type the environment name exactly, to confirm",
                    }
                },
                "required": ["confirm"],
            }),
        };
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "elicitation/create",
            "params": {
                "message": summary,
                "requestedSchema": schema
            }
        });
        if tx.send(frame.to_string()).await.is_err() {
            self.forget_ask(id);
            // Same: the question could not be delivered to a client
            // that should have been able to answer it.
            return AskOutcome::Undeliverable;
        }

        match tokio::time::timeout(std::time::Duration::from_secs(ASK_WAIT_SECS), reply_rx).await {
            Ok(Ok(reply)) => match (ask_outcome_from(&reply), typed) {
                // Only an accept is checked against the typed value: a
                // decline is a decline whatever the field holds.
                (AskOutcome::Approved, Some(expected)) => {
                    let got = reply
                        .get("result")
                        .and_then(|r| r.get("content"))
                        .and_then(|c| c.get("confirm"))
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    // Exact, deliberately. Trimming or folding case is
                    // a convenience that erodes the only thing this
                    // step buys — that somebody read the name and
                    // reproduced it.
                    if got == expected {
                        AskOutcome::Approved
                    } else {
                        AskOutcome::Unconfirmed
                    }
                }
                (outcome, _) => outcome,
            },
            // Sender dropped, or the budget expired. Both are "no
            // answer", and both deny.
            _ => {
                self.forget_ask(id);
                // Tell the client to take the dialog down.
                //
                // Without this the server gives up and the operator is
                // left looking at a live-seeming prompt for an action
                // that can no longer happen — observed exactly that
                // way during release QA. Worse than untidy: the next
                // person to walk past that screen sees a pending
                // approval and has no way to know it is spent, and an
                // Accept on it is silently inert.
                //
                // `notifications/cancelled` is the protocol's own
                // answer and a notification, so there is nothing to
                // wait for and a client that ignores it is no worse
                // off than before.
                self.notify_cancelled(id, "the ask window expired with no answer")
                    .await;
                AskOutcome::Unanswered
            }
        }
    }

    fn forget_ask(&self, id: i64) {
        if let Ok(mut map) = self.pending_asks.lock() {
            map.remove(&id);
        }
    }

    /// Route a frame that is a reply to one of OUR requests.
    ///
    /// Returns true when it was consumed. The frame loop must call
    /// this before dispatching, because a reply has an id and no
    /// method, which that loop would otherwise answer `-32601` while
    /// the ask sat waiting out its budget.
    pub(crate) fn take_ask_reply(&self, frame: &Value) -> bool {
        if frame.get("method").is_some() {
            return false;
        }
        let Some(id) = frame.get("id").and_then(Value::as_i64) else {
            return false;
        };
        let waiting = self
            .pending_asks
            .lock()
            .ok()
            .and_then(|mut m| m.remove(&id));
        match waiting {
            Some(tx) => {
                let _ = tx.send(frame.clone());
                true
            }
            None => false,
        }
    }

    /// One JSON-RPC frame in, at most one out (`None` for
    /// notifications). Pure protocol layer — tool dispatch lives in
    /// [`Server::call_tool`] — so tests can drive full frames.
    pub(crate) async fn handle_request(&self, req: &Value) -> Option<Value> {
        let id = req.get("id").cloned();
        let method = req.get("method").and_then(Value::as_str).unwrap_or("");
        // Frames without an id — or with `"id": null` — are
        // notifications per JSON-RPC 2.0: never answered, whatever the
        // method (an id-less tools/call must not produce an
        // `"id": null` response, and answering an explicit null id
        // collides with the -32700 parse-error convention).
        match id {
            None | Some(Value::Null) => return None,
            Some(_) => {}
        }
        match method {
            "initialize" => {
                // Capture the client name for write-audit attribution.
                if let Some(name) = req
                    .get("params")
                    .and_then(|p| p.get("clientInfo"))
                    .and_then(|c| c.get("name"))
                    .and_then(Value::as_str)
                {
                    if let Ok(mut cn) = self.client_name.lock() {
                        *cn = name.to_string();
                    }
                }
                // Does the client support elicitation? That is the only
                // way an MCP server can ask a human a question
                // mid-request, so it decides whether `ask` is
                // expressible on this transport at all — see
                // `docs/design/protection-levels.md`. Recorded rather
                // than acted on: the point is to learn what clients
                // actually declare before the levels design commits to
                // it.
                // `is_object`, not `is_some`: the capability is typed as
                // an object, and a client that says `"elicitation":
                // false` or `null` means it CANNOT be asked. Presence
                // alone would read that as consent — fail-open in the
                // one direction that matters, since the whole point of
                // `ask` is that a human sees the question.
                let elicits = req
                    .get("params")
                    .and_then(|p| p.get("capabilities"))
                    .and_then(|c| c.get("elicitation"))
                    .is_some_and(Value::is_object);
                self.client_supports_elicitation
                    .store(elicits, std::sync::atomic::Ordering::Relaxed);
                // A connection that can be asked can write even with no
                // flag, so the audit wiring it needs cannot be decided
                // from argv alone. Deferred to here rather than made
                // unconditional at startup: a genuinely read-only
                // server is documented not to touch the config disk,
                // and `should_init_audit` exists to keep that true.
                // `effective_scope`, not `elicits`: a `--read-only`
                // server can never write, so it must not read the
                // config disk either — which the comment above
                // promises and `elicits` alone did not honour.
                if self.effective_scope().any() && !matches!(self.backend, Backend::Demo) {
                    crate::audit::init_from_config_disk();
                }
                tracing::info!(
                    target: "ebman::mcp",
                    client = %self.client_name.lock().map(|c| c.clone()).unwrap_or_default(),
                    elicitation = elicits,
                    "MCP client connected"
                );
                // Echo-negotiate: accept the client's revision when it
                // matches ours, otherwise offer ours.
                let client_version = req
                    .get("params")
                    .and_then(|p| p.get("protocolVersion"))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                let version = if client_version == PROTOCOL_VERSION {
                    client_version
                } else {
                    PROTOCOL_VERSION
                };
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "protocolVersion": version,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "ebman", "version": env!("CARGO_PKG_VERSION")},
                        // What this surface does NOT expose, and where
                        // it lives instead.
                        //
                        // An agent can only see the tool list, so a
                        // capability that exists in the TUI and not here
                        // is indistinguishable from one ebman does not
                        // have — and the agent reasonably concludes the
                        // gap is absolute and reaches for raw `aws`
                        // calls. That happened on a real incident: the
                        // whole diagnosis hinged on a DLQ peek this
                        // server had no tool for, while the TUI had had
                        // one all along.
                        //
                        // Deliberately short and specific. The tool
                        // CAVEATS are the most-read part of this surface
                        // precisely because they are not boilerplate;
                        // a discoverability block that grows into prose
                        // gets skimmed like a licence.
                        "instructions": format!(
                            "{}{}\n\n{}",
                            concat!(
                            "ebman ", env!("CARGO_PKG_VERSION"),
                            " — a fleet console for AWS Elastic Beanstalk. This surface exposes reads, ",
                            "plus two-phase writes. Whether writes are available to YOU is said below; ",
                            "it depends on this connection, not on the binary.\n\n"),
                            self.effective_scope().agent_summary(
                                // Parse errors FIRST, matching
                                // `write_gate::decide`'s precedence.
                                // Reversed, an operator with both set
                                // was told to clear `safety.read_only`
                                // while every actual refusal rendered
                                // as "safety config unreadable" — they
                                // clear it, are still refused, and have
                                // been sent to the wrong control by the
                                // text that exists to name the right
                                // one.
                                if !self.safety_cfg.safety_parse_errors.is_empty() {
                                    Some("the safety config could not be parsed, which \
                                          fails closed.")
                                } else if self.safety_cfg.safety_read_only {
                                    Some("safety.read_only is set in config.toml.")
                                } else {
                                    None
                                },
                                // Recorded from this same `initialize`
                                // request a few lines above, so it is
                                // already correct for this connection.
                                self.client_supports_elicitation
                                    .load(std::sync::atomic::Ordering::Relaxed),
                                // Opened by the ask alone: no flag was
                                // given, so `effective_scope` widened
                                // `None` on capability.
                                !self.write_scope.any()
                                    && self
                                        .client_supports_elicitation
                                        .load(std::sync::atomic::Ordering::Relaxed),
                            ),
                            concat!(
                            // NOT redundant with `serverInfo.version`.
                            // Confirmed, not assumed: an agent on Claude
                            // Code went looking for `serverInfo` and
                            // could not reach it — the client consumes
                            // the handshake and never passes server
                            // identity through. So this block is the
                            // only version signal an MCP client is
                            // GUARANTEED to see, because it is content
                            // the server authors rather than protocol
                            // metadata the client may drop.
                            //
                            // The general rule, worth keeping in mind
                            // for anything added here: "the client can
                            // see X in the handshake" and "the agent can
                            // see X" are different claims, and the gap
                            // between them is invisible from this side.
                            // Anything an agent MUST know goes in
                            // authored content. Annotations are the
                            // other case and are fine — they are aimed
                            // at the client, which does read them.
                            //
                            // It matters because the list below is a
                            // claim about what a specific build can do:
                            // a reader on an old binary spent two days
                            // reporting capability gaps as facts without
                            // knowing it was two releases behind, and
                            // had no way to tell from where it sat.
                            "Check this against the latest release before reporting a capability as missing — ",
                            "this list describes THIS build.\n\n",
                            "Capabilities ebman HAS that this surface does NOT expose — ask the operator to run them, ",
                            "or ask for them to be exposed here:\n",
                            "- Nothing queue-related: depth and peek are `worker_queues`, and resend / delete / ",
                            "purge are `dlq_resend` / `dlq_delete` / `dlq_purge` under --allow-writes.\n",
                            "- A LIVE log tail (streaming, follows new lines): TUI, Detail view, Logs tab. ",
                            "Point-in-time log queries ARE exposed here, as `recent_logs`.\n\n",
                            "Tool descriptions carry CAVEATS naming what each tool cannot see. They are accurate and ",
                            "worth reading: a clean result from a tool does not clear what that tool never checked.\n\n",
                            // The version line above answers "is this
                            // build capable?". This answers the three
                            // questions it does not: is this CLIENT
                            // capable, has the operator forbidden it,
                            // and is this server even real. All three
                            // otherwise present as "ebman cannot do
                            // this", which is the report that wastes a
                            // maintainer's afternoon.
                            "Before reporting ANY capability as missing, call `doctor`. It names this build, what ",
                            "your client declared, the write surface in force, and the operator's standing ",
                            "restrictions — which is how you tell \"ebman cannot\" from \"your client cannot\" from ",
                            "\"the operator said no\". Those three are indistinguishable from where you sit, and only ",
                            "the first is a bug worth reporting."
                        ))
                    }
                }))
            }
            // notifications/initialized + notifications/cancelled land
            // in the id-less early-return above, like all notifications.
            "ping" => Some(json!({"jsonrpc": "2.0", "id": id, "result": {}})),
            "tools/list" => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {"tools": tool_table(&self.effective_scope(), self.safety_cfg.mcp_peek_bodies)}
            })),
            "tools/call" => {
                let params = req.get("params").cloned().unwrap_or_else(|| json!({}));
                let name = params
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                let advertised =
                    tool_table(&self.effective_scope(), self.safety_cfg.mcp_peek_bodies)
                        .as_array()
                        .is_some_and(|t| t.iter().any(|d| d["name"] == name.as_str()));
                // A real verb this server was not granted falls through
                // to the scope gate rather than being answered here, so
                // it comes back as a refusal naming the flag, and gets
                // audited. `unknown tool` would be a lie in the shape
                // that matters: it teaches the agent that ebman LACKS
                // the verb, contradicting the scope line in
                // `instructions` and sending it off to report a
                // capability gap instead of asking.
                //
                // Evaluated second, and only when the name is not
                // advertised: it rebuilds the whole descriptor table,
                // and the overwhelmingly common case is an ordinary
                // read tool that is already advertised.
                if !advertised && !writes::write_verb_names().contains(&name) {
                    return Some(json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {"code": -32602, "message": format!("unknown tool '{name}'")}
                    }));
                }
                let budget = call_timeout_secs(
                    &name,
                    self.client_supports_elicitation
                        .load(std::sync::atomic::Ordering::Relaxed),
                );
                let outcome = tokio::time::timeout(
                    std::time::Duration::from_secs(budget),
                    self.call_tool(&name, &args),
                )
                .await
                .unwrap_or_else(|_| Err(format!("tool '{name}' timed out after {budget}s")));
                let (text, is_error) = match outcome {
                    Ok(body) => (body, false),
                    Err(msg) => (msg, true),
                };
                // Prepended rather than added as a field: every tool
                // returns its own JSON shape, and a client that renders
                // the text sees this whatever it does with structure.
                // One assembly point, so no tool can forget it.
                let text = match self.exe.as_ref().filter(|e| e.is_stale()) {
                    Some(e) => format!("{}\n{text}", stale_binary_notice(&e.path)),
                    None => text,
                };
                Some(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "content": [{"type": "text", "text": text}],
                        "isError": is_error
                    }
                }))
            }
            // Unknown request (id present — notifications returned
            // above): method-not-found per JSON-RPC.
            _ => Some(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": {"code": -32601, "message": format!("method '{method}' not found")}
            })),
        }
    }
}

/// `ebman mcp` — `serve` the stdio MCP server, or `setup` to print
/// local registration instructions.
///
/// `setup` is print-only and never edits a client's config: the
/// instructions come from the binary you already installed, so there
/// is no remote file to fetch or tamper with.
///
/// `serve` exits 0 when stdin closes, 2 on a usage error — including a
/// `--allow-writes` value naming a verb that does not exist, refused
/// at startup rather than at first use.
pub async fn run(args: &[String]) -> Result<()> {
    // Sub-verbs: `serve` (the stdio server) and `setup` (print the local
    // registration instructions — no network, no remote fetch). Anything
    // else is a usage error naming both.
    match args.get(1).map(String::as_str) {
        Some("setup") => return setup::run(args),
        Some("serve") => {}
        _ => {
            eprintln!("{MCP_USAGE}");
            std::process::exit(2);
        }
    }
    let McpArgs {
        demo,
        no_redact,
        write_scope,
        read_only,
    } = match parse_mcp_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };
    // Writes fan audit lines out to the configured webhook — the
    // reads-only server stays free of the config-disk read.
    if should_init_audit(&write_scope, demo) {
        crate::audit::init_from_config_disk();
    }
    let server = Arc::new(Server::with_scope(demo, no_redact, write_scope.clone()));
    server
        .mcp_read_only
        .store(read_only, std::sync::atomic::Ordering::Relaxed);
    // Frame-level tools/call concurrency cap (see the spawn site).
    let tool_slots = Arc::new(tokio::sync::Semaphore::new(16));

    // Single writer task: concurrent tool tasks send completed frames
    // through the channel so stdout writes can't interleave. Bounded:
    // a client that writes requests but stops reading stdout must
    // apply backpressure (senders park at `send().await`), not grow
    // an unbounded queue of completed frames.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(256);
    // The server originates frames now (an ask is a server→client
    // request), so it needs the writer's channel. Everything it sent
    // before was a response returned up the call stack.
    if let Ok(mut slot) = server.outbound.lock() {
        *slot = Some(out_tx.clone());
    }
    let writer = tokio::spawn(async move {
        use tokio::io::AsyncWriteExt;
        let mut stdout = tokio::io::stdout();
        while let Some(line) = out_rx.recv().await {
            // A write error means the client closed its read end —
            // keep-running would execute AWS-hitting tool calls whose
            // results nobody can ever receive. Stop draining; senders
            // then error out and the tasks unwind.
            if stdout.write_all(line.as_bytes()).await.is_err()
                || stdout.write_all(b"\n").await.is_err()
                || stdout.flush().await.is_err()
            {
                eprintln!("ebman mcp: stdout closed — dropping remaining frames");
                break;
            }
        }
    });

    use futures::StreamExt;
    use tokio_util::codec::{FramedRead, LinesCodec};
    // LinesCodec with a max length: a frame streamed without a newline
    // used to accumulate in the read buffer without bound. 16MB is far
    // beyond any real MCP frame.
    const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
    let mut lines = FramedRead::new(
        tokio::io::stdin(),
        LinesCodec::new_with_max_length(MAX_LINE_BYTES),
    );
    // Read errors (e.g. one invalid-UTF-8 byte → InvalidData) must not
    // masquerade as EOF: previously the server exited 0 silently. Skip
    // the bad line loudly; bail after a run of consecutive errors so a
    // permanently-broken stream can't spin.
    let mut consecutive_read_errors: u32 = 0;
    // FramedRead sets an internal errored flag on any decode error and
    // the NEXT poll returns None — then resets, so the stream is
    // resumable. A None right after an Err is therefore NOT EOF: treat
    // it as part of the error recovery and poll again (without this,
    // one oversized/invalid line killed the whole session as a silent
    // exit 0 — verified live before the fix).
    let mut last_was_error = false;
    loop {
        let line = match lines.next().await {
            Some(Ok(line)) => {
                consecutive_read_errors = 0;
                last_was_error = false;
                line
            }
            None => {
                if last_was_error {
                    last_was_error = false;
                    continue;
                }
                break;
            }
            Some(Err(e)) => {
                consecutive_read_errors += 1;
                last_was_error = true;
                eprintln!("ebman mcp: stdin read error (skipping line): {e}");
                if consecutive_read_errors >= 5 {
                    eprintln!(
                        "ebman mcp: {consecutive_read_errors} consecutive read errors — exiting"
                    );
                    break;
                }
                continue;
            }
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                let _ = out_tx
                    .send(
                        json!({
                            "jsonrpc": "2.0",
                            "id": null,
                            "error": {"code": -32700, "message": "parse error"}
                        })
                        .to_string(),
                    )
                    .await;
                continue;
            }
        };
        // A reply to an ask WE sent: an id we issued, no method. The
        // dispatch below would answer it `-32601` while the ask sat
        // waiting out its budget and then denied a write the operator
        // had just approved.
        if server.take_ask_reply(&req) {
            continue;
        }
        // An id with no method is a RESPONSE, and JSON-RPC says a
        // response is never answered. If `take_ask_reply` did not
        // claim it, it is a reply to an ask we have already given up
        // on — a late Accept on a dialog that timed out. Dropping it
        // is correct; the dispatch below would answer it `-32601`,
        // telling the client its perfectly well-formed reply named a
        // method that does not exist.
        // A response carries `result` or `error`. A frame with an id
        // and NEITHER, and no method, is a malformed request, and
        // JSON-RPC says that is an invalid request — so it must fall
        // through and be answered rather than vanish. (What it
        // actually gets is `-32601` from `handle_request`'s catch-all,
        // since the method extracts as "". Visible either way, which
        // is the property that matters here.) The first cut dropped on
        // "no method + has id" alone and would have swallowed it
        // silently, which is a behaviour change beyond the fix.
        if req.get("method").is_none()
            && req.get("id").is_some()
            && (req.get("result").is_some() || req.get("error").is_some())
        {
            tracing::debug!(
                target: "ebman::mcp",
                id = ?req.get("id"),
                "dropping a reply to an ask that is no longer pending"
            );
            continue;
        }
        if let Some(resp) = invalid_request_response(&req) {
            let _ = out_tx.send(resp).await;
            continue;
        }
        // tools/call may hit AWS for many seconds — spawn it so the
        // loop stays responsive to ping / further calls. Everything
        // else is answered inline (cheap + ordering-sensitive).
        // The semaphore caps frame-level concurrency: a flood of
        // one-line tools/call frames must not spawn unlimited tasks
        // each building an AwsClient (in-CALL fan-out is separately
        // bounded at FETCH_CONCURRENCY).
        if req.get("method").and_then(Value::as_str) == Some("tools/call") {
            let permit = Arc::clone(&tool_slots).acquire_owned().await;
            let server = Arc::clone(&server);
            let out_tx = out_tx.clone();
            tokio::spawn(async move {
                let _permit = permit;
                if let Some(resp) = server.handle_request(&req).await {
                    let _ = out_tx.send(resp.to_string()).await;
                }
            });
        } else if let Some(resp) = server.handle_request(&req).await {
            let _ = out_tx.send(resp.to_string()).await;
        }
    }
    // stdin closed: drop the sender. The writer keeps draining until
    // in-flight tool tasks (which hold out_tx clones) finish — bounded
    // by the per-call timeout — then exits.
    //
    // The ask channel holds a clone too, and it is not task-scoped:
    // it lives in the server for the whole connection, so dropping
    // only the local sender leaves one alive forever, the writer's
    // receiver never closes, and `writer.await` below never returns.
    // The process then hangs after stdin closes instead of exiting —
    // which two subprocess tests caught and no unit test could, since
    // the leak is in the shutdown of a loop they never run.
    if let Ok(mut slot) = server.outbound.lock() {
        *slot = None;
    }
    drop(out_tx);
    let _ = writer.await;
    // `effective_scope`, not `write_scope`: an elicit-capable client
    // writes with no flag, and draining on the flag alone would drop
    // the webhooks those writes queued.
    if server.effective_scope().any() {
        crate::audit::drain_webhooks(std::time::Duration::from_secs(12)).await;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
