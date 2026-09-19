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

use crate::cli::lint::{
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
        matches!(self, AskOutcome::Declined | AskOutcome::Unanswered)
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
                 asks for it."
            }
            AskOutcome::Unanswered => {
                "The plan is spent. Nobody answered, which is NOT a refusal — the \
                 operator may have stepped away, or may never have been shown the \
                 dialog. Tell them it expired and let them decide; do not quietly \
                 re-plan it, and do not report this as a refusal."
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
            AskOutcome::Declined => "declined by the operator",
            AskOutcome::Unanswered => "no answer within the ask window",
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
        return AskOutcome::Declined;
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
    fn agent_summary(&self, standing_refusal: Option<&str>, can_ask: bool) -> String {
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

/// Appended to a granted-writes summary when the operator can be asked.
///
/// Kept whole rather than inlined twice: the two grant arms said the
/// same thing about confirmation and drifted apart once already.
const ASK_NOTE: &str = "\n\nEach confirmation is put to the OPERATOR, who sees the \
     action and answers it. Expect `confirm_action` to take as long as a person takes. \
     A decline is a final answer from a human — not an error, not a missing permission: \
     do not re-plan the same action, do not ask for the grant to be widened, and do not \
     report it as a fault. Say the operator declined, and stop.";

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

    pub(crate) async fn ask_operator(&self, summary: &str) -> AskOutcome {
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
            return AskOutcome::Unanswered;
        };

        let id = self
            .next_ask_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        if let Ok(mut map) = self.pending_asks.lock() {
            map.insert(id, reply_tx);
        }

        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "elicitation/create",
            "params": {
                "message": summary,
                // A confirmation, so the answer lives in `action` and
                // the schema carries nothing. An empty object rather
                // than an omitted field: the field is required, and a
                // client that validates it should get something valid.
                "requestedSchema": {"type": "object", "properties": {}}
            }
        });
        if tx.send(frame.to_string()).await.is_err() {
            self.forget_ask(id);
            // Same: the question could not be delivered to a client
            // that should have been able to answer it.
            return AskOutcome::Unanswered;
        }

        match tokio::time::timeout(std::time::Duration::from_secs(ASK_WAIT_SECS), reply_rx).await {
            Ok(Ok(reply)) => ask_outcome_from(&reply),
            // Sender dropped, or the budget expired. Both are "no
            // answer", and both deny.
            _ => {
                self.forget_ask(id);
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
                if elicits && !matches!(self.backend, Backend::Demo) {
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
mod tests {

    /// The `ask` question, answered with data rather than a guess.
    ///
    /// `docs/design/protection-levels.md` stage 3 turns on whether an
    /// MCP client can be asked something mid-request. Elicitation is
    /// the only mechanism, and it is a CLIENT capability — so the
    /// server can know, per connection, whether `ask` is expressible or
    /// must degrade to a refusal.
    ///
    /// Pinned in both directions because a detector that always says
    /// "no" would quietly make every client look unable, and the levels
    /// design would then be built around a limitation that is not real.
    #[tokio::test]
    async fn the_elicitation_capability_is_detected_per_client() {
        use std::sync::atomic::Ordering;

        async fn declares(caps: serde_json::Value) -> bool {
            let s = Server::with_scope(true, false, WriteScope::None);
            let _ = s
                .handle_request(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": caps,
                    "clientInfo": {"name": "probe", "version": "1"}
                    }
                }))
                .await;
            s.client_supports_elicitation.load(Ordering::Relaxed)
        }

        assert!(
            declares(json!({"elicitation": {}})).await,
            "a client declaring elicitation must be detected, or `ask` \
             degrades to a refusal for everyone"
        );
        for explicit_no in [json!({"elicitation": false}), json!({"elicitation": null})] {
            assert!(
                !declares(explicit_no.clone()).await,
                "{explicit_no} explicitly declines elicitation; treating \
                 mere presence as support would let `ask` believe a human \
                 is watching when none is"
            );
        }
        assert!(
            !declares(json!({})).await,
            "a client declaring nothing must NOT be treated as able to ask"
        );
        assert!(
            !declares(json!({"sampling": {}, "roots": {}})).await,
            "other capabilities are not elicitation"
        );
    }
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn demo_server() -> Server {
        Server::with_scope(true, false, WriteScope::None)
    }

    fn demo_writes_server() -> Server {
        Server::with_scope(true, false, WriteScope::All)
    }

    async fn rpc(server: &Server, frame: Value) -> Option<Value> {
        server.handle_request(&frame).await
    }

    /// tools/call a write tool and return the parsed result text.
    async fn call(server: &Server, name: &str, args: Value) -> (bool, Value) {
        let frame = json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}});
        let resp = server.handle_request(&frame).await.expect("response");
        let result = &resp["result"];
        let is_error = result["isError"].as_bool().unwrap_or(false);
        let text = result["content"][0]["text"].as_str().unwrap_or("");
        let parsed = serde_json::from_str(text).unwrap_or(Value::String(text.to_string()));
        (is_error, parsed)
    }

    #[tokio::test]
    async fn write_tools_appear_only_under_allow_writes() {
        let list = |s: &Server| {
            let arr = tool_table(&s.write_scope, true);
            arr.as_array()
                .unwrap()
                .iter()
                .map(|t| t["name"].as_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        let reads = list(&demo_server());
        assert!(!reads.contains(&"deploy".to_string()));
        assert!(!reads.contains(&"confirm_action".to_string()));
        let writes = list(&demo_writes_server());
        for t in [
            "deploy",
            "restart",
            "rebuild",
            "terminate",
            "set_option",
            "confirm_action",
        ] {
            assert!(writes.contains(&t.to_string()), "missing {t}");
        }
    }

    #[tokio::test]
    async fn two_phase_happy_path_demo() {
        let s = demo_writes_server();
        let envs = demo_fixture::envs();
        let env = &envs[0].name;
        let (err, plan) = call(&s, "restart", json!({"env": env})).await;
        assert!(!err);
        assert_eq!(plan["pending"], true);
        assert_eq!(plan["plan"]["action"], "Restart");
        let token = plan["confirm_token"].as_str().unwrap().to_string();
        let (err2, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
        assert!(!err2);
        assert_eq!(out["dispatched"], true);
        assert_eq!(out["demo"], true);
    }

    #[tokio::test]
    async fn confirm_token_single_use_and_unknown() {
        let s = demo_writes_server();
        let env = &demo_fixture::envs()[0].name;
        let (_, plan) = call(&s, "restart", json!({"env": env})).await;
        let token = plan["confirm_token"].as_str().unwrap().to_string();
        assert!(
            !call(
                &s,
                "confirm_action",
                json!({"confirm_token": token.clone()})
            )
            .await
            .0
        );
        // reused
        assert!(
            call(&s, "confirm_action", json!({"confirm_token": token}))
                .await
                .0
        );
        // unknown
        assert!(
            call(&s, "confirm_action", json!({"confirm_token": "deadbeef"}))
                .await
                .0
        );
        // no pending
        assert!(
            call(&s, "confirm_action", json!({"confirm_token": "x"}))
                .await
                .0
        );
    }

    #[tokio::test]
    async fn terminate_requires_matching_confirm_name_with_one_retry() {
        let s = demo_writes_server();
        let env = demo_fixture::envs()[0].name.clone();
        let (_, plan) = call(&s, "terminate", json!({"env": env})).await;
        let token = plan["confirm_token"].as_str().unwrap().to_string();
        // wrong once — token survives
        assert!(
            call(
                &s,
                "confirm_action",
                json!({"confirm_token": token.clone(), "confirm_name": "wrong"})
            )
            .await
            .0
        );
        // correct — dispatches
        let (err, out) = call(
            &s,
            "confirm_action",
            json!({"confirm_token": token, "confirm_name": env}),
        )
        .await;
        assert!(!err);
        assert_eq!(out["dispatched"], true);
    }

    #[tokio::test]
    async fn terminate_second_wrong_name_drops_the_plan() {
        let s = demo_writes_server();
        let env = demo_fixture::envs()[0].name.clone();
        let (_, plan) = call(&s, "terminate", json!({"env": env})).await;
        let token = plan["confirm_token"].as_str().unwrap().to_string();
        assert!(
            call(
                &s,
                "confirm_action",
                json!({"confirm_token": token.clone(), "confirm_name": "wrong"})
            )
            .await
            .0
        );
        assert!(
            call(
                &s,
                "confirm_action",
                json!({"confirm_token": token.clone(), "confirm_name": "wrong"})
            )
            .await
            .0
        );
        // even the RIGHT name now fails — plan is gone
        let env2 = demo_fixture::envs()[0].name.clone();
        assert!(
            call(
                &s,
                "confirm_action",
                json!({"confirm_token": token, "confirm_name": env2})
            )
            .await
            .0
        );
    }

    #[tokio::test]
    async fn deploy_rejects_unknown_version_and_plan_carries_versions() {
        let s = demo_writes_server();
        let envs = demo_fixture::envs();
        let env = &envs[0];
        let known = &demo_fixture::deploys_for_app(&env.application)[0].label;
        let (err, plan) = call(&s, "deploy", json!({"env": env.name, "version": known})).await;
        assert!(!err);
        assert_eq!(plan["plan"]["target_version"], known.as_str());
        assert!(plan["plan"]["current_version"].is_string());
        let (err2, _) = call(
            &s,
            "deploy",
            json!({"env": env.name, "version": "no-such-999"}),
        )
        .await;
        assert!(err2, "unknown version must refuse");
    }

    #[tokio::test]
    async fn set_option_caps_and_gates_namespaces_and_redacts_old() {
        let s = demo_writes_server();
        let env = &demo_fixture::envs()[0].name;
        // cap
        let big: Vec<Value> = (0..11)
            .map(|i| json!({"namespace": "aws:autoscaling:asg", "name": format!("n{i}"), "value": "1"}))
            .collect();
        assert!(
            call(&s, "set_option", json!({"env": env, "settings": big}))
                .await
                .0
        );
        // unknown namespace
        assert!(
            call(
                &s,
                "set_option",
                json!({"env": env, "settings": [{"namespace":"made:up","name":"X","value":"1"}]})
            )
            .await
            .0
        );
        // known namespace: plan present; if an env-var setting exists its OLD value is redacted
        let (err, plan) = call(
            &s,
            "set_option",
            json!({"env": env, "settings": [{"namespace":"aws:autoscaling:asg","name":"MinSize","value":"9"}]}),
        )
        .await;
        assert!(!err);
        assert_eq!(plan["plan"]["changes"][0]["new"], "9");
    }

    #[tokio::test]
    async fn dispatching_flag_clears_after_dispatch() {
        // C1 (0.28 pre-tag): the RAII guard must reset `dispatching`
        // after every dispatch (incl. cancellation/panic), or the
        // write surface wedges. Here: a completed demo dispatch leaves
        // the flag false and a second write goes through.
        let s = demo_writes_server();
        let env = &demo_fixture::envs()[0].name;
        let (_, p1) = call(&s, "restart", json!({"env": env})).await;
        let t1 = p1["confirm_token"].as_str().unwrap().to_string();
        assert!(
            !call(&s, "confirm_action", json!({"confirm_token": t1}))
                .await
                .0
        );
        assert!(
            !s.dispatching.load(std::sync::atomic::Ordering::SeqCst),
            "guard must clear dispatching after dispatch"
        );
        // second write not wedged
        let (_, p2) = call(&s, "restart", json!({"env": env})).await;
        let t2 = p2["confirm_token"].as_str().unwrap().to_string();
        assert!(
            !call(&s, "confirm_action", json!({"confirm_token": t2}))
                .await
                .0
        );
    }

    #[tokio::test]
    async fn write_serialization_blocks_second_dispatch_slot() {
        // A pending plan exists; injecting a dispatching=true state
        // makes confirm refuse. (Full concurrency is exercised live;
        // this pins the guard.)
        let s = demo_writes_server();
        let env = &demo_fixture::envs()[0].name;
        let (_, plan) = call(&s, "restart", json!({"env": env})).await;
        let token = plan["confirm_token"].as_str().unwrap().to_string();
        s.dispatching
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(
            call(&s, "confirm_action", json!({"confirm_token": token}))
                .await
                .0,
            "confirm must refuse while a dispatch is in flight"
        );
    }

    #[tokio::test]
    async fn write_pin_refusal() {
        let mut cfg = crate::config::Config::default();
        cfg.safety_envs
            .insert(demo_fixture::envs()[0].name.clone(), true);
        let s = Server::with_config(true, false, WriteScope::All, cfg);
        let env = demo_fixture::envs()[0].name.clone();
        let (err, plan) = call(&s, "restart", json!({"env": env})).await;
        assert!(err, "pinned env must refuse");
        assert!(
            plan.as_str().unwrap_or("").contains("pinned"),
            "refusal names the pin: {plan:?}"
        );
    }

    #[test]
    fn mcp_args_require_serve_and_reject_unknown_flags() {
        assert!(parse_mcp_args(&argv(&["mcp"])).is_err());
        assert!(parse_mcp_args(&argv(&["mcp", "listen"])).is_err());
        assert!(parse_mcp_args(&argv(&["mcp", "serve", "--port"])).is_err());
        let p = parse_mcp_args(&argv(&["mcp", "serve", "--demo", "--no-redact"])).unwrap();
        assert!(p.demo && p.no_redact);
        let p = parse_mcp_args(&argv(&["mcp", "serve"])).unwrap();
        assert!(!p.demo && !p.no_redact);
    }

    #[tokio::test]
    async fn golden_initialize_frame() {
        // Pins protocolVersion + capabilities + serverInfo shape. A
        // failure here means the protocol surface changed — bump
        // consciously, then update docs/headless.md.
        let resp = rpc(
            &demo_server(),
            json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                   "params":{"protocolVersion":"2025-06-18","capabilities":{}}}),
        )
        .await
        .expect("initialize answers");
        assert_eq!(resp["id"], 1);
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
        assert_eq!(resp["result"]["serverInfo"]["name"], "ebman");
        assert!(resp["result"]["capabilities"]["tools"].is_object());
        // An older client revision gets ours offered back.
        let resp = rpc(
            &demo_server(),
            json!({"jsonrpc":"2.0","id":2,"method":"initialize",
                   "params":{"protocolVersion":"2024-11-05"}}),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["protocolVersion"], PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn golden_tools_list_frame() {
        let resp = rpc(
            &demo_server(),
            json!({"jsonrpc":"2.0","id":3,"method":"tools/list"}),
        )
        .await
        .expect("tools/list answers");
        let tools = resp["result"]["tools"].as_array().expect("array");
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            vec![
                "list_environments",
                "worker_queues",
                "recent_logs",
                "why",
                "lint",
                "get_option_settings",
                "drift",
                "doctor",
                "audit_log",
                "recent_events",
                "list_versions",
                "fleet_cost",
            ],
            "tool registry changed — update docs/headless.md's table"
        );
        // Coverage caveats are part of the contract, not prose fluff.
        //
        // Looked up by NAME, not position: this indexed `tools[1]` and
        // broke when a tool was inserted ahead of `lint` — a failure
        // about tool ORDER dressed up as one about lint's caveats,
        // which is the wrong thing to be told.
        let lint_desc = tools
            .iter()
            .find(|t| t["name"] == "lint")
            .and_then(|t| t["description"].as_str())
            .expect("lint is advertised");
        assert!(lint_desc.contains("EBL011") && lint_desc.contains("EBL016"));
        // And the caveat must point somewhere reachable: EBL011 not
        // firing here is only actionable if it names what does.
        assert!(
            lint_desc.contains("worker_queues"),
            "the EBL011 caveat must name the tool that DOES see queues: {lint_desc}"
        );
        for t in tools {
            assert!(t["inputSchema"]["type"] == "object", "schema shape");
            assert!(
                !t["description"].as_str().unwrap().is_empty(),
                "empty description"
            );
        }
    }

    #[tokio::test]
    async fn notifications_and_unknown_methods_route_correctly() {
        let s = demo_server();
        assert!(rpc(
            &s,
            json!({"jsonrpc":"2.0","method":"notifications/initialized"})
        )
        .await
        .is_none());
        let resp = rpc(&s, json!({"jsonrpc":"2.0","id":4,"method":"ping"}))
            .await
            .unwrap();
        assert!(resp["result"].is_object());
        let resp = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":5,"method":"resources/list"}),
        )
        .await
        .unwrap();
        assert_eq!(resp["error"]["code"], -32601);
        // Unknown *notification* (no id) stays silent.
        assert!(
            rpc(&s, json!({"jsonrpc":"2.0","method":"resources/changed"}))
                .await
                .is_none()
        );
        // Unknown tool → -32602 at the JSON-RPC layer.
        let resp = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":6,"method":"tools/call",
                   "params":{"name":"terminate_env","arguments":{}}}),
        )
        .await
        .unwrap();
        assert_eq!(resp["error"]["code"], -32602);
    }

    #[tokio::test]
    async fn demo_e2e_list_environments_and_lint() {
        // The zero-AWS e2e: real frames through the real tool layer
        // over the synthetic fleet.
        let s = demo_server();
        let resp = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":7,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let body = resp["result"]["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(body).expect("tool body is valid JSON");
        assert!(
            !parsed.as_array().unwrap().is_empty(),
            "demo fleet non-empty"
        );
        assert!(parsed[0]["name"].is_string() && parsed[0]["health"].is_string());

        // Demo lint finds the planted EBL014 (NetworkOut trigger on a
        // scaling ASG) — a demo lint that finds nothing demonstrates
        // nothing.
        let resp = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":8,"method":"tools/call",
                   "params":{"name":"lint","arguments":{"rules":"EBL014"}}}),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], false);
        let body = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(body.contains("EBL014"), "planted finding surfaced: {body}");
    }

    #[tokio::test]
    async fn demo_e2e_option_settings_redacts_env_vars_by_default() {
        let s = demo_server();
        let env_name = demo_fixture::envs()[0].name.clone();
        let resp = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
                   "params":{"name":"get_option_settings","arguments":{"env": env_name}}}),
        )
        .await
        .unwrap();
        let body = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            !body.contains("hunter2"),
            "secret env-var value must not leak: {body}"
        );
        assert!(body.contains("DATABASE_URL"), "keys stay visible");
        assert!(body.contains("(redacted)"));
        assert!(body.contains("\"redacted\":true"));
        // --no-redact opt-out passes values through.
        let open = Server::with_scope(true, true, WriteScope::None);
        let resp = rpc(
            &open,
            json!({"jsonrpc":"2.0","id":10,"method":"tools/call",
                   "params":{"name":"get_option_settings",
                             "arguments":{"env": demo_fixture::envs()[0].name.clone()}}}),
        )
        .await
        .unwrap();
        let body = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(body.contains("hunter2") && body.contains("\"redacted\":false"));
    }

    #[tokio::test]
    async fn tool_errors_come_back_as_is_error_results_not_rpc_errors() {
        let s = demo_server();
        let resp = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":11,"method":"tools/call",
                   "params":{"name":"get_option_settings","arguments":{"env":"no-such-env"}}}),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("not found"), "got: {text}");
        // Missing required arg — same shape.
        let resp = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":12,"method":"tools/call",
                   "params":{"name":"list_versions","arguments":{}}}),
        )
        .await
        .unwrap();
        assert_eq!(resp["result"]["isError"], true);
    }

    #[test]
    fn drift_reports_redact_env_var_secrets() {
        // C1 (0.26 pre-tag review): the drift tool leaked the exact
        // values get_option_settings redacts.
        let mut reports = vec![(
            "prod".to_string(),
            true,
            vec![
                terraform::DriftField {
                    kind: "option_setting".into(),
                    namespace: Some("aws:elasticbeanstalk:application:environment".into()),
                    name: Some("DATABASE_URL".into()),
                    tf_value: "postgres://u:hunter2@old".into(),
                    live_value: "postgres://u:hunter2@new".into(),
                },
                terraform::DriftField {
                    kind: "option_setting".into(),
                    namespace: Some("aws:autoscaling:asg".into()),
                    name: Some("MaxSize".into()),
                    tf_value: "4".into(),
                    live_value: "6".into(),
                },
                terraform::DriftField {
                    kind: "version_label".into(),
                    namespace: None,
                    name: None,
                    tf_value: "v1".into(),
                    live_value: "v2".into(),
                },
            ],
        )];
        redact_drift_reports(&mut reports);
        let fields = &reports[0].2;
        assert_eq!(fields[0].tf_value, "(redacted)");
        assert_eq!(fields[0].live_value, "(redacted)");
        assert_eq!(fields[1].live_value, "6", "non-secret options untouched");
        assert_eq!(fields[2].tf_value, "v1", "non-option kinds untouched");
        let rendered = terraform::render_drift_json(None, None, &reports);
        assert!(!rendered.contains("hunter2"), "no secret in the payload");
    }

    #[test]
    fn skipped_envs_spliced_only_when_present() {
        let clean = append_skipped_envs("{\"issues\":[]}".to_string(), &[]);
        assert_eq!(clean, "{\"issues\":[]}", "common case byte-identical");
        let degraded = append_skipped_envs(
            "{\"issues\":[]}".to_string(),
            &["prod: fetch failed".to_string()],
        );
        assert_eq!(
            degraded,
            "{\"issues\":[],\"skipped_envs\":[\"prod: fetch failed\"]}"
        );
        serde_json::from_str::<Value>(&degraded).expect("valid JSON");
    }

    #[test]
    fn audit_jsonl_wraps_into_an_array() {
        assert_eq!(jsonl_to_array(""), "[]");
        assert_eq!(
            jsonl_to_array("{\"a\":1}\n{\"b\":2}\n"),
            "[{\"a\":1},{\"b\":2}]"
        );
        serde_json::from_str::<Value>(&jsonl_to_array("{\"a\":1}")).expect("valid JSON");
    }

    #[tokio::test]
    async fn id_less_requests_are_notifications_and_get_no_response() {
        let server = Server::with_scope(true, false, WriteScope::None);
        let req: Value = serde_json::from_str(
            r#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"nope"}}"#,
        )
        .unwrap();
        assert!(server.handle_request(&req).await.is_none());
        let ping: Value = serde_json::from_str(r#"{"jsonrpc":"2.0","method":"ping"}"#).unwrap();
        assert!(server.handle_request(&ping).await.is_none());
        // Explicit null id = notification too — answering it would
        // collide with the -32700 parse-error convention.
        let null_id: Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":null,"method":"ping"}"#).unwrap();
        assert!(server.handle_request(&null_id).await.is_none());
    }

    #[test]
    fn non_object_frames_get_invalid_request() {
        // Batch arrays and scalars answer -32600 (id null); a real
        // object passes through untouched.
        let arr: Value =
            serde_json::from_str(r#"[{"jsonrpc":"2.0","id":1,"method":"ping"}]"#).unwrap();
        let resp = invalid_request_response(&arr).expect("array is invalid");
        let parsed: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], -32600);
        assert!(parsed["id"].is_null());
        let scalar: Value = serde_json::from_str("42").unwrap();
        assert!(invalid_request_response(&scalar).is_some());
        let obj: Value =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":1,"method":"ping"}"#).unwrap();
        assert!(invalid_request_response(&obj).is_none());
    }

    #[test]
    fn empty_cost_cache_total_formats_positive_zero() {
        // f64's Sum impl folds from -0.0; without the `+ 0.0` normalise
        // an empty cost cache renders total_usd_month as "-0.00".
        let costs: std::collections::HashMap<String, f64> = Default::default();
        let total: f64 = costs.values().sum::<f64>() + 0.0;
        assert_eq!(format!("{total:.2}"), "0.00");
    }

    #[test]
    fn redaction_covers_env_vars_and_db_password_only() {
        let r = |ns, n, v| redact_option_value(ns, n, v, true);
        assert_eq!(
            r(
                "aws:elasticbeanstalk:application:environment",
                "API_KEY",
                "sk-123"
            ),
            "(redacted)"
        );
        assert_eq!(r("aws:rds:dbinstance", "DBPassword", "pw"), "(redacted)");
        assert_eq!(r("aws:autoscaling:asg", "MaxSize", "6"), "6");
        assert_eq!(
            redact_option_value(
                "aws:elasticbeanstalk:application:environment",
                "API_KEY",
                "sk-123",
                false
            ),
            "sk-123"
        );
    }

    #[tokio::test]
    async fn credential_errors_are_rewritten_actionably() {
        // Pure check on the shared rewrite path the tool errors use.
        let msg = tool_error(
            &Some("prod-admin".into()),
            "list_environments",
            "The security token included in the request is expired",
        );
        assert!(
            msg.contains("aws sso login --profile prod-admin"),
            "got: {msg}"
        );
        let msg = tool_error(&None, "op", "some unrelated failure");
        assert!(msg.contains("op failed"), "got: {msg}");
    }

    /// `initialize` must tell a client what ebman can do that this
    /// surface does not expose.
    ///
    /// An agent sees only the tool list, so a TUI-only capability is
    /// indistinguishable from one ebman lacks — and the reasonable
    /// conclusion is that the gap is absolute. On a real incident that
    /// sent the diagnosis out into raw `aws sqs` calls while ebman had
    /// had a DLQ peek all along.
    ///
    /// Pinned to the capabilities rather than the prose: this must fail
    /// when a gap CLOSES and the note goes stale, not merely when
    /// someone rewords it.
    #[tokio::test]
    async fn initialize_names_what_this_surface_cannot_do() {
        let s = Server::with_scope(true, false, WriteScope::None);
        let resp = s
            .handle_request(&json!({
                "jsonrpc": "2.0", "id": 1, "method": "initialize",
                "params": {"protocolVersion": PROTOCOL_VERSION, "capabilities": {},
                           "clientInfo": {"name": "probe", "version": "1"}}
            }))
            .await
            .expect("initialize responds");
        let instructions = resp["result"]["instructions"]
            .as_str()
            .expect("initialize must carry instructions")
            .to_string();

        // What is TUI-only TODAY. This list has shrunk three times as
        // tools shipped — queues, then `:why` and point-in-time logs,
        // now dead-letter management — and each time the guard failed
        // first and the block was corrected, which is the behaviour
        // wanted from it.
        // One entry today; it was three. Kept as a list because the
        // shape is "what is TUI-only", and the next capability to ship
        // should shorten this rather than restructure it.
        const TUI_ONLY: &[&str] = &["LIVE log tail"];
        for needle in TUI_ONLY {
            assert!(
                instructions.to_lowercase().contains(&needle.to_lowercase()),
                "the instructions must name `{needle}` as available elsewhere: {instructions}"
            );
        }
        assert!(
            instructions.contains("TUI"),
            "and must say where: {instructions}"
        );

        // The build version, IN the text. It is in `serverInfo` as well,
        // but a client need not surface that field and the agent reads
        // this — and the capability list above is a claim about a
        // specific build. A reader on a three-week-old binary spent two
        // days reporting capability gaps as facts, with no way to tell
        // from where it sat that it was two releases behind.
        assert!(
            instructions.contains(env!("CARGO_PKG_VERSION")),
            "the instructions must name the build they describe: {instructions}"
        );
        assert_eq!(
            resp["result"]["serverInfo"]["version"],
            env!("CARGO_PKG_VERSION"),
            "and must agree with serverInfo"
        );

        // If a queue TOOL ever ships, this note becomes a lie. Fail
        // here so it is updated with the tool rather than left behind.
        let tools = s
            .handle_request(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .await
            .expect("tools/list responds");
        let names: Vec<String> = tools["result"]["tools"]
            .as_array()
            .expect("a tool array")
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        // The staleness half, re-aimed. It fired for real when
        // `worker_queues` shipped — the block still said queues were
        // TUI-only — which is the whole point of pinning capabilities
        // rather than prose. Now it guards the claim that MANAGEMENT
        // (resend / delete / purge) stays TUI-only.
        assert!(
            !names
                .iter()
                .any(|n| n.contains("resend") || n.contains("purge")),
            "a dead-letter management tool exists now, so the instructions \
             claiming that is TUI-only are stale: {names:?}"
        );
    }

    /// The queue tool answers the question EB's health text does not.
    ///
    /// "1 message in Dead Letter Queue" names no task; the whole
    /// diagnosis of a real incident hinged on which one, and that took
    /// three raw `aws sqs` calls because this surface had no tool for
    /// it.
    #[tokio::test]
    async fn worker_queues_reports_depth_and_whether_it_looked() {
        let s = demo_server();
        let envs = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
        )
        .await
        .expect("envs");
        let listing = envs["result"]["content"][0]["text"].as_str().expect("text");
        let env_name = listing
            .split("\"name\":\"")
            .nth(1)
            .and_then(|r| r.split('"').next())
            .expect("an env in the demo fleet")
            .to_string();

        let resp = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                   "params":{"name":"worker_queues","arguments":{"env": env_name}}}),
        )
        .await
        .expect("worker_queues answers");
        let body = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text payload");
        let parsed: Value = serde_json::from_str(body).expect("valid JSON");

        assert!(parsed["main_queue"].is_object(), "{body}");
        assert!(parsed["dead_letter_queue"].is_object(), "{body}");
        // "we did not look" must be distinguishable from "nothing
        // there" — the same empty array otherwise, and they mean
        // opposite things during triage.
        assert_eq!(
            parsed["peeked"], false,
            "peek defaults off, and the response must say so: {body}"
        );
        assert!(parsed["messages"].is_array(), "{body}");
    }

    /// `env` is required — without it the tool would have to pick one.
    #[tokio::test]
    async fn worker_queues_requires_an_env() {
        let resp = rpc(
            &demo_server(),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"worker_queues","arguments":{}}}),
        )
        .await
        .expect("a response");
        let text = serde_json::to_string(&resp).expect("serialisable");
        assert!(
            text.contains("'env' is required"),
            "must refuse rather than guess an env: {text}"
        );
    }

    /// The counter caveat must be stated where an agent will read it.
    ///
    /// A peek increments `receive_count`, which counts every receive —
    /// an operator watching it climb across calls would conclude the
    /// task is still failing. That was measured on a live message
    /// (2 → 4 from three peeks), so the warning is fact, not caution.
    #[tokio::test]
    async fn the_queue_tool_warns_that_a_peek_inflates_the_receive_count() {
        let resp = rpc(
            &demo_server(),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .await
        .expect("tools/list");
        let desc = resp["result"]["tools"]
            .as_array()
            .expect("array")
            .iter()
            .find(|t| t["name"] == "worker_queues")
            .and_then(|t| t["description"].as_str())
            .expect("worker_queues is advertised")
            .to_string();
        assert!(
            desc.contains("receive_count"),
            "the caveat must name the field it is about: {desc}"
        );
        assert!(
            desc.contains("not a retry count") || desc.contains("NOT a retry count"),
            "and must say what it is not: {desc}"
        );
        assert!(
            // The field as EMITTED (`dead_letter_queue.origin`), not
            // the internal Rust field name — the description is what an
            // agent reads and then looks for in the JSON, and those two
            // disagreed.
            desc.contains("dead_letter_queue.origin"),
            "a derived DLQ url that returns nothing is ordinary; a reported \
             one that does is an anomaly — the consumer cannot tell without \
             this: {desc}"
        );
    }

    /// `recent_logs` must report whether it reached the newest lines.
    ///
    /// The failure this guards is a plausible wrong answer, not an
    /// error: `FilterLogEvents` returns matches oldest-first, so a
    /// truncated window hands back the OLDEST lines and answers "is
    /// this still running?" with evidence from hours ago. `complete`
    /// is the only thing that tells a reader which they are holding.
    #[tokio::test]
    async fn recent_logs_says_whether_it_reached_the_newest() {
        let resp = rpc(
            &demo_server(),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"recent_logs","arguments":{"env":"any"}}}),
        )
        .await
        .expect("recent_logs answers");
        let body = resp["result"]["content"][0]["text"]
            .as_str()
            .expect("text payload");
        let parsed: Value = serde_json::from_str(body).expect("valid JSON");
        assert!(
            parsed["complete"].is_boolean(),
            "every answer must say whether the window was fully read: {body}"
        );
        assert!(parsed["events"].is_array(), "{body}");
    }

    /// And the caveat must be where an agent reads it.
    #[tokio::test]
    async fn recent_logs_warns_about_oldest_first() {
        let resp = rpc(
            &demo_server(),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
        )
        .await
        .expect("tools/list");
        let desc = resp["result"]["tools"]
            .as_array()
            .expect("array")
            .iter()
            .find(|t| t["name"] == "recent_logs")
            .and_then(|t| t["description"].as_str())
            .expect("recent_logs is advertised")
            .to_string();
        assert!(
            desc.contains("oldest-first") || desc.contains("oldest first"),
            "the trap must be named: {desc}"
        );
        assert!(
            desc.contains("complete"),
            "and the field that tells you which you have: {desc}"
        );
        // Redaction is namespace-and-key based and cannot reach free
        // text, so this tool hands over whatever the application
        // logged. "Redaction-by-default" is a property a reader
        // attributes to the whole surface; the exception has to be
        // stated where it is acted on.
        assert!(
            desc.contains("NOT REDACTED"),
            "the one read tool that cannot be redacted must say so: {desc}"
        );
    }

    /// The `why` bundle puts the facts side by side in one call.
    ///
    /// Five sections assembled by hand across five calls during a real
    /// incident. The bundle is deliberately not a narrative: adjacent
    /// facts let a reader be wrong in their own name, where a confident
    /// generated sentence does not.
    #[tokio::test]
    async fn why_returns_every_section_in_one_call() {
        let s = demo_server();
        let envs = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
        )
        .await
        .expect("envs");
        let env_name = envs["result"]["content"][0]["text"]
            .as_str()
            .and_then(|t| t.split("\"name\":\"").nth(1))
            .and_then(|r| r.split('"').next())
            .expect("an env")
            .to_string();

        let resp = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                   "params":{"name":"why","arguments":{"env": env_name}}}),
        )
        .await
        .expect("why answers");
        let body = resp["result"]["content"][0]["text"].as_str().expect("text");
        let parsed: Value = serde_json::from_str(body).expect("valid JSON");

        for section in ["events", "alarms", "instances", "queues", "recent_versions"] {
            assert!(
                parsed.get(section).is_some(),
                "`{section}` must be present even when null — a missing key \
                 and an empty one read differently: {body}"
            );
        }
        assert!(
            parsed["errors"].is_array(),
            "a partial bundle must be visibly partial: {body}"
        );
    }

    /// A failed section is null WITH a reason, never an empty array.
    ///
    /// "We could not look" and "there is nothing there" are opposite
    /// conclusions during triage, and an empty array says the second
    /// while meaning the first.
    #[test]
    fn a_failed_why_section_is_null_with_its_reason() {
        let bundle = super::tools::render_why_json(
            "api-prod",
            "[]",
            "null",
            "[]",
            "null",
            "[]",
            &[
                ("alarms".to_string(), "AccessDenied".to_string()),
                ("queues".to_string(), "throttled".to_string()),
            ],
        );
        let parsed: Value = serde_json::from_str(&bundle).expect("valid JSON");
        assert!(parsed["alarms"].is_null(), "{bundle}");
        assert!(parsed["queues"].is_null(), "{bundle}");
        assert!(
            parsed["events"].is_array() && parsed["instances"].is_array(),
            "sections that succeeded must still be there: {bundle}"
        );
        let errs = parsed["errors"].as_array().expect("errors array");
        assert_eq!(errs.len(), 2, "{bundle}");
        assert!(
            errs.iter()
                .any(|e| e["section"] == "alarms" && e["error"] == "AccessDenied"),
            "the reason must survive, not just the fact of failure: {bundle}"
        );
    }

    /// A failed section must be `null` with its reason, not an empty
    /// array and not a silent success.
    #[test]
    fn a_failed_section_is_null_and_recorded() {
        let mut errors: Vec<(String, String)> = Vec::new();
        let ok = super::tools::section_or_error("events", Ok("[1,2]".into()), &mut errors);
        assert_eq!(ok, "[1,2]", "a good section passes through untouched");
        assert!(errors.is_empty(), "and records nothing");

        let bad = super::tools::section_or_error("alarms", Err("AccessDenied".into()), &mut errors);
        assert_eq!(
            bad, "null",
            "a failed section must be null — `[]` would read as \"no alarms\", \
             which is the opposite of \"we could not check\""
        );
        assert_eq!(
            errors,
            vec![("alarms".to_string(), "AccessDenied".to_string())],
            "and the reason must be kept, not just the fact of failure"
        );
    }

    /// A FAILED dead-letter peek must not report as an empty queue.
    ///
    /// Reading messages needs `sqs:ReceiveMessage`, a different
    /// permission from the attributes call that produced the depth — so
    /// "depth says 1, peek denied" is an ordinary IAM shape. Reported as
    /// `peeked: true, messages: []` it reads as "we looked, the queue is
    /// clean", which is the opposite of what happened and the exact
    /// distinction `peeked` exists to preserve.
    #[test]
    fn a_denied_dlq_peek_is_not_an_empty_queue() {
        use crate::aws::QueueMessage;
        let msg = || QueueMessage {
            id: "m-1".into(),
            attributes: Vec::new(),
            receipt_handle: String::new(),
            body: "b".into(),
            receive_count: 1,
            sent_at: None,
            task: None,
        };

        // Success: messages, and we looked.
        let mut errors = Vec::new();
        let (msgs, peeked) = super::tools::dlq_peek_outcome(Some(Ok(vec![msg()])), &mut errors);
        assert_eq!(msgs.len(), 1);
        assert!(peeked);
        assert!(errors.is_empty());

        // Failure: no messages, we did NOT look, and the reason survives.
        let mut errors = Vec::new();
        let (msgs, peeked) = super::tools::dlq_peek_outcome(
            Some(Err("AccessDenied: sqs:ReceiveMessage".into())),
            &mut errors,
        );
        assert!(msgs.is_empty());
        assert!(
            !peeked,
            "a denied peek must not claim we looked — that turns \
             'permission missing' into 'queue is clean'"
        );
        assert_eq!(
            errors,
            vec![(
                "dlq_peek".to_string(),
                "AccessDenied: sqs:ReceiveMessage".to_string()
            )],
            "and the reason must reach the caller, not just the fact"
        );

        // No dead-letter queue at all: nothing to look at, did not look,
        // and NOT an error — a web-tier env is not a failure.
        let mut errors = Vec::new();
        let (msgs, peeked) = super::tools::dlq_peek_outcome(None, &mut errors);
        assert!(msgs.is_empty() && !peeked);
        assert!(
            errors.is_empty(),
            "an env with no dead-letter queue is ordinary, not an error"
        );
    }

    /// Orchestration tests: the tool BODIES, driven against a mocked
    /// SDK.
    ///
    /// Everything under these was already covered — renderers, extracted
    /// decisions, the AWS-layer calls. What was not is which calls a
    /// tool makes and with what, and that is the layer the `tool_why`
    /// dead-letter peek bug lived in: it survived a full review because
    /// nothing could reach it.
    mod orchestration {
        use super::*;
        use aws_sdk_elasticbeanstalk::Client as EbClient;
        use aws_sdk_sqs::Client as SqsClient;

        fn client_with(eb: EbClient, sqs: SqsClient) -> crate::aws::AwsClient {
            let cfg = aws_config::SdkConfig::builder()
                .region(aws_config::Region::new("us-west-1"))
                .behavior_version(aws_config::BehaviorVersion::latest())
                .build();
            crate::aws::AwsClient::for_tests(
                eb,
                sqs,
                aws_sdk_cloudwatch::Client::new(&cfg),
                aws_sdk_cloudwatchlogs::Client::new(&cfg),
                aws_sdk_s3::Client::new(&cfg),
                aws_sdk_ec2::Client::new(&cfg),
            )
        }

        fn env_listing() -> aws_smithy_mocks::Rule {
            use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput;
            use aws_sdk_elasticbeanstalk::types::EnvironmentDescription;
            aws_smithy_mocks::mock!(EbClient::describe_environments).then_output(|| {
                DescribeEnvironmentsOutput::builder()
                    .environments(
                        EnvironmentDescription::builder()
                            .environment_name("poly-prod-wk")
                            .application_name("poly")
                            .status("Ready".into())
                            .health("Yellow".into())
                            .tier(
                                aws_sdk_elasticbeanstalk::types::EnvironmentTier::builder()
                                    .name("Worker")
                                    .build(),
                            )
                            .build(),
                    )
                    .build()
            })
        }

        /// `worker_queues` must resolve the env's queues and, with
        /// `peek`, read messages from the DEAD-LETTER url — not the
        /// main one.
        #[tokio::test]
        async fn worker_queues_peeks_the_dead_letter_queue() {
            use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
            use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
            use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
            use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
            use aws_sdk_sqs::types::{Message, MessageAttributeValue, QueueAttributeName};

            let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
                .then_output(|| {
                    DescribeEnvironmentResourcesOutput::builder()
                        .environment_resources(
                            EnvironmentResourceDescription::builder()
                                .queues(
                                    Queue::builder()
                                        .name("WorkerQueue")
                                        .url("https://sqs/main")
                                        .build(),
                                )
                                .queues(
                                    Queue::builder()
                                        .name("WorkerDeadLetterQueue")
                                        .url("https://sqs/main-dlq")
                                        .build(),
                                )
                                .build(),
                        )
                        .build()
                });
            let attrs =
                aws_smithy_mocks::mock!(SqsClient::get_queue_attributes).then_output(|| {
                    GetQueueAttributesOutput::builder()
                        .attributes(QueueAttributeName::ApproximateNumberOfMessages, "1")
                        .attributes(
                            QueueAttributeName::ApproximateNumberOfMessagesNotVisible,
                            "0",
                        )
                        .attributes(QueueAttributeName::ApproximateNumberOfMessagesDelayed, "0")
                        .build()
                });
            // Matches ONLY the dead-letter url. A body that peeked the
            // main queue would get no match and no task.
            let peek = aws_smithy_mocks::mock!(SqsClient::receive_message)
                .match_requests(|req| req.queue_url() == Some("https://sqs/main-dlq"))
                .then_output(|| {
                    let attr = |v: &str| {
                        MessageAttributeValue::builder()
                            .data_type("String")
                            .string_value(v)
                            .build()
                            .expect("valid")
                    };
                    ReceiveMessageOutput::builder()
                        .messages(
                            Message::builder()
                                .message_id("m-1")
                                .receipt_handle("rh-1")
                                .body("elasticbeanstalk scheduled job")
                                .message_attributes(
                                    "beanstalk.sqsd.task_name",
                                    attr("ORCHCANARY sweep"),
                                )
                                .build(),
                        )
                        .build()
                });

            let eb = aws_smithy_mocks::mock_client!(
                aws_sdk_elasticbeanstalk,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&env_listing(), &resources]
            );
            let sqs = aws_smithy_mocks::mock_client!(
                aws_sdk_sqs,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&attrs, &peek]
            );
            let s = Server::with_injected_client(
                WriteScope::None,
                crate::config::Config::default(),
                client_with(eb, sqs),
            );

            let out = s
                .call_tool(
                    "worker_queues",
                    &json!({"env": "poly-prod-wk", "peek": true}),
                )
                .await
                .expect("worker_queues answers");
            let v: Value = serde_json::from_str(&out).expect("valid JSON");

            assert_eq!(v["dead_letter_queue"]["stats"]["visible"], 1);
            assert_eq!(v["peeked"], true);
            assert_eq!(
                v["messages"][0]["task"]["name"], "ORCHCANARY sweep",
                "the peek must read the DEAD-LETTER queue and carry the \
                 task through: {out}"
            );
        }

        /// Without `peek` the tool must not call ReceiveMessage at all.
        ///
        /// The default path is documented as touching nothing, and a
        /// peek increments a counter an operator reads. The mock has no
        /// receive_message rule, so a body that peeked anyway fails.
        #[tokio::test]
        async fn worker_queues_does_not_peek_unless_asked() {
            use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
            use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
            use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
            use aws_sdk_sqs::types::QueueAttributeName;

            let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
                .then_output(|| {
                    DescribeEnvironmentResourcesOutput::builder()
                        .environment_resources(
                            EnvironmentResourceDescription::builder()
                                .queues(
                                    Queue::builder()
                                        .name("WorkerQueue")
                                        .url("https://sqs/main")
                                        .build(),
                                )
                                .build(),
                        )
                        .build()
                });
            let attrs =
                aws_smithy_mocks::mock!(SqsClient::get_queue_attributes).then_output(|| {
                    GetQueueAttributesOutput::builder()
                        .attributes(QueueAttributeName::ApproximateNumberOfMessages, "0")
                        .build()
                });
            // EB named no dead-letter queue, so `describe_worker_queues`
            // falls back to `aws:elasticbeanstalk:sqsd` option settings
            // looking for an explicit override. Discovered by this test
            // failing on an UNMATCHED call — which is exactly the
            // orchestration detail these tests exist to pin, and which
            // nothing below this layer could have shown.
            let settings = aws_smithy_mocks::mock!(EbClient::describe_configuration_settings)
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder().build()
                });
            let eb = aws_smithy_mocks::mock_client!(
                aws_sdk_elasticbeanstalk,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&env_listing(), &resources, &settings]
            );
            let sqs = aws_smithy_mocks::mock_client!(
                aws_sdk_sqs,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&attrs]
            );
            let s = Server::with_injected_client(
                WriteScope::None,
                crate::config::Config::default(),
                client_with(eb, sqs),
            );

            let out = s
                .call_tool("worker_queues", &json!({"env": "poly-prod-wk"}))
                .await
                .expect("depth-only must not need a receive_message rule");
            let v: Value = serde_json::from_str(&out).expect("valid JSON");
            assert_eq!(v["peeked"], false, "{out}");
            assert!(
                v["messages"].as_array().is_some_and(|m| m.is_empty()),
                "{out}"
            );
        }

        /// An env the fleet does not contain must be refused, not
        /// silently queried.
        #[tokio::test]
        async fn worker_queues_refuses_an_unknown_env() {
            let eb = aws_smithy_mocks::mock_client!(
                aws_sdk_elasticbeanstalk,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&env_listing()]
            );
            let cfg = aws_config::SdkConfig::builder()
                .region(aws_config::Region::new("us-west-1"))
                .behavior_version(aws_config::BehaviorVersion::latest())
                .build();
            let s = Server::with_injected_client(
                WriteScope::None,
                crate::config::Config::default(),
                client_with(eb, SqsClient::new(&cfg)),
            );
            let err = s
                .call_tool("worker_queues", &json!({"env": "no-such-env"}))
                .await
                .expect_err("an unknown env must be an error");
            assert!(err.contains("not found"), "{err}");
        }

        /// A derived dead-letter URL that names no real queue must not
        /// fail the call.
        ///
        /// `describe_worker_queues` treats NonExistentQueue on a DERIVED
        /// url as THE "this env has no dead-letter queue" shape — it
        /// swallows the error and leaves `dlq_stats: None` with
        /// `dlq_url: Some`. Peeking that url raised it again, and the
        /// `?` failed the whole tool call, discarding the depth answer
        /// we already had. On the case the tool's own description calls
        /// ordinary.
        ///
        /// The mock has NO receive_message rule: a body that still
        /// peeks gets an unmatched call and fails.
        #[tokio::test]
        async fn a_derived_dlq_that_does_not_exist_still_answers() {
            use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
            use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
            use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
            use aws_sdk_sqs::types::QueueAttributeName;

            // EB names only the main queue, so the DLQ url is derived.
            let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
                .then_output(|| {
                    DescribeEnvironmentResourcesOutput::builder()
                        .environment_resources(
                            EnvironmentResourceDescription::builder()
                                .queues(
                                    Queue::builder()
                                        .name("WorkerQueue")
                                        .url("https://sqs/main")
                                        .build(),
                                )
                                .build(),
                        )
                        .build()
                });
            let settings = aws_smithy_mocks::mock!(EbClient::describe_configuration_settings)
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder().build()
                });
            // The main queue answers; the derived `-dlq` does not exist.
            let main_attrs = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes)
                .match_requests(|req| req.queue_url() == Some("https://sqs/main"))
                .then_output(|| {
                    GetQueueAttributesOutput::builder()
                        .attributes(QueueAttributeName::ApproximateNumberOfMessages, "3")
                        .build()
                });
            let dlq_missing = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes)
                .match_requests(|req| req.queue_url() == Some("https://sqs/main-dlq"))
                .then_error(|| {
                    aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesError::generic(
                        aws_smithy_types::error::ErrorMetadata::builder()
                            .code("AWS.SimpleQueueService.NonExistentQueue")
                            .message("The specified queue does not exist")
                            .build(),
                    )
                });

            let eb = aws_smithy_mocks::mock_client!(
                aws_sdk_elasticbeanstalk,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&env_listing(), &resources, &settings]
            );
            let sqs = aws_smithy_mocks::mock_client!(
                aws_sdk_sqs,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&main_attrs, &dlq_missing]
            );
            let s = Server::with_injected_client(
                WriteScope::None,
                crate::config::Config::default(),
                client_with(eb, sqs),
            );

            let out = s
                .call_tool(
                    "worker_queues",
                    &json!({"env": "poly-prod-wk", "peek": true}),
                )
                .await
                .expect("a missing derived DLQ must not fail the call");
            let v: Value = serde_json::from_str(&out).expect("valid JSON");

            assert_eq!(
                v["main_queue"]["stats"]["visible"], 3,
                "the depth answer we already had must survive: {out}"
            );
            assert_eq!(
                v["peeked"], false,
                "there was nothing to peek, so we did not look — saying \
                 `true` here claims an empty queue that does not exist: {out}"
            );
        }

        /// `why` must not record a spurious "we could not look" when the
        /// env simply has no dead-letter queue.
        ///
        /// Sibling of `a_derived_dlq_that_does_not_exist_still_answers`,
        /// and it exists because that fix was applied to `why` as well
        /// and left UNTESTED — mutating the gate away came back green.
        /// `why` is the body the injected-client seam was built for, so
        /// an untested fix here is the gap the seam exists to close.
        #[tokio::test]
        async fn why_records_no_dlq_error_when_there_is_no_dlq() {
            use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
            use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
            use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
            use aws_sdk_sqs::types::QueueAttributeName;

            let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
                .then_output(|| {
                    DescribeEnvironmentResourcesOutput::builder()
                        .environment_resources(
                            EnvironmentResourceDescription::builder()
                                .queues(
                                    Queue::builder()
                                        .name("WorkerQueue")
                                        .url("https://sqs/main")
                                        .build(),
                                )
                                .build(),
                        )
                        .build()
                });
            let settings = aws_smithy_mocks::mock!(EbClient::describe_configuration_settings)
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_configuration_settings::DescribeConfigurationSettingsOutput::builder().build()
                });
            // `why` fans out to five more calls; every one errors here so
            // the bundle records them — which is fine, because the
            // assertion is specifically about `dlq_peek` NOT being among
            // them.
            let main_attrs = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes)
                .match_requests(|req| req.queue_url() == Some("https://sqs/main"))
                .then_output(|| {
                    GetQueueAttributesOutput::builder()
                        .attributes(QueueAttributeName::ApproximateNumberOfMessages, "0")
                        .build()
                });
            let dlq_missing = aws_smithy_mocks::mock!(SqsClient::get_queue_attributes)
                .match_requests(|req| req.queue_url() == Some("https://sqs/main-dlq"))
                .then_error(|| {
                    aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesError::generic(
                        aws_smithy_types::error::ErrorMetadata::builder()
                            .code("AWS.SimpleQueueService.NonExistentQueue")
                            .message("The specified queue does not exist")
                            .build(),
                    )
                });

            // `why` also fans out to events, instances and versions.
            // The mock HARD-FAILS on an unmatched call rather than
            // erroring the section, so each needs a rule even though
            // this test asserts about none of them.
            let events = aws_smithy_mocks::mock!(EbClient::describe_events).then_output(|| {
                aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput::builder(
                )
                .build()
            });
            let health = aws_smithy_mocks::mock!(EbClient::describe_instances_health)
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_instances_health::DescribeInstancesHealthOutput::builder().build()
                });
            let versions = aws_smithy_mocks::mock!(EbClient::describe_application_versions)
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::describe_application_versions::DescribeApplicationVersionsOutput::builder().build()
                });
            let eb = aws_smithy_mocks::mock_client!(
                aws_sdk_elasticbeanstalk,
                aws_smithy_mocks::RuleMode::MatchAny,
                [
                    &env_listing(),
                    &resources,
                    &settings,
                    &events,
                    &health,
                    &versions
                ]
            );
            let sqs = aws_smithy_mocks::mock_client!(
                aws_sdk_sqs,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&main_attrs, &dlq_missing]
            );
            let s = Server::with_injected_client(
                WriteScope::None,
                crate::config::Config::default(),
                client_with(eb, sqs),
            );

            let out = s
                .call_tool("why", &json!({"env": "poly-prod-wk"}))
                .await
                .expect("why answers");
            let v: Value = serde_json::from_str(&out).expect("valid JSON");

            let errors = v["errors"].as_array().expect("errors array");
            assert!(
                !errors.iter().any(|e| e["section"] == "dlq_peek"),
                "an env with no dead-letter queue is ordinary — recording \
                 `dlq_peek` as a failure is triage noise in the array that \
                 exists to make partial answers load-bearing: {out}"
            );
            assert_eq!(
                v["queues"]["peeked"], false,
                "and we did not look, because there was nothing to look at: {out}"
            );
        }

        /// Confirm must act on the message the PLAN named, or refuse.
        ///
        /// The dangerous implementation re-receives at confirm and acts
        /// on whatever comes back — deleting the head of the queue,
        /// which may not be what the plan described, with nothing in
        /// the output saying they differed. A silent target swap.
        ///
        /// Here the message is gone by confirm time, which is ordinary:
        /// something else consumed, redrove or removed it in the token
        /// window. The confirm must refuse and say nothing changed.
        #[tokio::test]
        async fn a_dlq_delete_refuses_when_the_planned_message_is_gone() {
            use aws_sdk_elasticbeanstalk::operation::describe_environment_resources::DescribeEnvironmentResourcesOutput;
            use aws_sdk_elasticbeanstalk::types::{EnvironmentResourceDescription, Queue};
            use aws_sdk_sqs::operation::get_queue_attributes::GetQueueAttributesOutput;
            use aws_sdk_sqs::operation::receive_message::ReceiveMessageOutput;
            use aws_sdk_sqs::types::{Message, QueueAttributeName};

            let resources = aws_smithy_mocks::mock!(EbClient::describe_environment_resources)
                .then_output(|| {
                    DescribeEnvironmentResourcesOutput::builder()
                        .environment_resources(
                            EnvironmentResourceDescription::builder()
                                .queues(
                                    Queue::builder()
                                        .name("WorkerQueue")
                                        .url("https://sqs/main")
                                        .build(),
                                )
                                .queues(
                                    Queue::builder()
                                        .name("WorkerDeadLetterQueue")
                                        .url("https://sqs/main-dlq")
                                        .build(),
                                )
                                .build(),
                        )
                        .build()
                });
            let attrs =
                aws_smithy_mocks::mock!(SqsClient::get_queue_attributes).then_output(|| {
                    GetQueueAttributesOutput::builder()
                        .attributes(QueueAttributeName::ApproximateNumberOfMessages, "2")
                        .build()
                });
            // Plan time: m-1 is present. Confirm time: only m-2 is —
            // the head of the queue is now a DIFFERENT message, which
            // is exactly the swap this must not perform.
            let seq = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let seq2 = std::sync::Arc::clone(&seq);
            let peek = aws_smithy_mocks::mock!(SqsClient::receive_message).then_output(move || {
                let first = seq2.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0;
                let id = if first { "m-1" } else { "m-2" };
                ReceiveMessageOutput::builder()
                    .messages(
                        Message::builder()
                            .message_id(id)
                            .receipt_handle(format!("rh-{id}"))
                            .body("payload")
                            .build(),
                    )
                    .build()
            });

            // The plan path also pulls recent events for its summary.
            let events = aws_smithy_mocks::mock!(EbClient::describe_events).then_output(|| {
                aws_sdk_elasticbeanstalk::operation::describe_events::DescribeEventsOutput::builder(
                )
                .build()
            });
            let eb = aws_smithy_mocks::mock_client!(
                aws_sdk_elasticbeanstalk,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&env_listing(), &resources, &events]
            );
            let sqs = aws_smithy_mocks::mock_client!(
                aws_sdk_sqs,
                aws_smithy_mocks::RuleMode::MatchAny,
                [&attrs, &peek]
            );
            let s = Server::with_injected_client(
                WriteScope::All,
                crate::config::Config::default(),
                client_with(eb, sqs),
            );

            let plan = s
                .call_tool(
                    "dlq_delete",
                    &json!({"env": "poly-prod-wk", "message_id": "m-1"}),
                )
                .await
                .expect("m-1 is present at plan time");
            let token = plan
                .split("\"confirm_token\":\"")
                .nth(1)
                .and_then(|r| r.split('"').next())
                .expect("a token")
                .to_string();
            assert!(plan.contains("m-1"), "the plan names the message: {plan}");

            let err = s
                .call_tool("confirm_action", &json!({"confirm_token": token}))
                .await
                .expect_err("m-1 is gone by confirm time — this must refuse");
            assert!(
                err.contains("not among the messages returned when the queue was re-read"),
                "it must say the planned message was not found, not delete m-2 \
                 quietly: {err}"
            );
            // Scoped to what was observed. This said "is no longer in
            // the dead-letter queue", which is a firmer claim than a
            // receive can support: SQS returns a sample, so absence
            // from one read is evidence, not proof. Same rule as
            // `empty_queue_reason` and the `peeked` flag — say what was
            // looked at, not what is the case.
            assert!(
                !err.contains("dispatched\":true"),
                "a batch where nothing succeeded must not report a dispatch: {err}"
            );
            assert!(
                !err.contains("m-2"),
                "and must not have touched the message that IS there: {err}"
            );
        }
    }

    /// The MCP drift tool must canonicalise before discovery.
    ///
    /// Source-scanned: the discovery path needs a real filesystem walk
    /// and an AWS client to reach. `Path::new(".")` there means
    /// discovery checks the server's own directory and nothing above
    /// it, which contradicts the tool's own description.
    #[test]
    fn mcp_drift_discovery_starts_from_an_absolute_path() {
        let src = std::fs::read_to_string("src/cli/mcp/tools.rs").expect("read source");
        let prod = src.split("\n#[cfg(test)]\nmod ").next().unwrap_or_default();
        assert!(
            prod.contains("std::env::current_dir()"),
            "drift discovery must start from the absolute cwd, or it \
             cannot walk up at all"
        );
        // Canary: prove the slice found real code.
        assert!(
            prod.contains("resolve_state_path("),
            "the production slice is not finding the resolution"
        );
    }

    /// Staleness is "the file changed", and unknown is never "changed".
    ///
    /// Verified empirically before this was built: replacing a binary
    /// underneath a running process changes its inode while
    /// `current_exe()` still resolves the path. That is the whole
    /// mechanism — a `brew upgrade` leaves the server on the old inode.
    #[test]
    fn the_stale_notice_says_whose_job_the_reconnect_is() {
        let n = stale_binary_notice("/usr/local/bin/ebman");
        assert!(
            n.contains("ASK YOUR OPERATOR"),
            "an agent reading `reconnect` imperatively will try it and fail — the \
             panel is a client surface it cannot reach: {n}"
        );
        assert!(
            n.contains("cannot do it yourself"),
            "and must be told why, or it will look for another way round: {n}"
        );
        assert!(
            n.contains("no MCP message a server can send"),
            "naming the missing primitive stops an agent hunting for a tool that \
             does not exist: {n}"
        );
        assert!(n.contains(env!("CARGO_PKG_VERSION")), "{n}");
        assert!(!n.contains("  "), "renders into one line: {n:?}");
    }

    #[test]
    fn a_replaced_binary_is_stale_and_an_unreadable_one_is_not() {
        assert!(
            super::staleness(Some(100), Some(200)),
            "a different inode is a different file — that is an upgrade"
        );
        assert!(
            !super::staleness(Some(100), Some(100)),
            "the same file is not stale, however often it is checked"
        );
        // Unknown on either side must NOT report stale. A server crying
        // staleness whenever it cannot stat itself is noise, and noise
        // is how a real warning gets ignored.
        assert!(!super::staleness(None, Some(200)), "unknown start");
        assert!(!super::staleness(Some(100), None), "unknown now");
        assert!(!super::staleness(None, None), "unknown both");
    }

    /// The notice must be actionable, not merely alarming.
    #[test]
    fn the_stale_notice_says_what_to_do_about_it() {
        let n = super::stale_binary_notice("/opt/homebrew/bin/ebman");
        assert!(
            n.contains(env!("CARGO_PKG_VERSION")),
            "it must name the version actually RUNNING — the cached \
             instructions block cannot be refreshed mid-connection, so \
             this is the only truthful version an agent sees: {n}"
        );
        assert!(n.contains("/opt/homebrew/bin/ebman"), "and where: {n}");
        assert!(
            n.contains("reconnect") || n.contains("Reconnect"),
            "an agent cannot act on \"you are stale\" — it can act on \
             \"reconnect\": {n}"
        );
    }

    /// A current server must say nothing.
    ///
    /// Without this the feature could prepend on every call and both
    /// tests above would still pass — a warning on every response is
    /// indistinguishable from no warning at all.
    #[tokio::test]
    async fn a_current_binary_adds_no_notice() {
        let s = demo_server();
        let resp = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
        )
        .await
        .expect("answers");
        let text = resp["result"]["content"][0]["text"].as_str().expect("text");
        assert!(
            !text.contains("is running, but a different build"),
            "this binary has not been replaced, so there is nothing to \
             report: {text}"
        );
        // And the payload is still the payload — the prepend must not
        // have eaten it.
        assert!(text.starts_with('['), "still JSON: {text}");
    }

    /// A stale server must SAY so, on a real response.
    ///
    /// The pure halves are tested above; this is the wiring. It points
    /// the check at a file the test controls, replaces it — which is
    /// what `brew upgrade` does — and drives a real tool call.
    #[tokio::test]
    async fn a_stale_server_prepends_the_notice_to_its_answer() {
        let dir = std::env::temp_dir().join(format!("ebman-exe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let fake = dir.join("ebman");
        std::fs::write(&fake, b"v1").expect("write");

        let s = Server::with_scope(true, false, WriteScope::None).watching_exe(&fake);

        // Unchanged: no notice.
        let quiet = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
        )
        .await
        .expect("answers");
        assert!(
            !quiet["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .contains("a different build"),
            "nothing has changed yet"
        );

        // Replace the file — a new inode, as an upgrade produces.
        let replacement = dir.join("ebman.new");
        std::fs::write(&replacement, b"v2").expect("write");
        std::fs::rename(&replacement, &fake).expect("rename");

        let loud = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call",
                   "params":{"name":"list_environments","arguments":{}}}),
        )
        .await
        .expect("answers");
        let text = loud["result"]["content"][0]["text"].as_str().expect("text");
        assert!(
            text.contains("a different build is now installed"),
            "the server must say the binary changed underneath it: {text}"
        );
        assert!(
            text.contains(env!("CARGO_PKG_VERSION")),
            "naming the version actually running, which is the only \
             truthful one an agent sees once instructions are cached: {text}"
        );
        assert!(
            text.contains('['),
            "and the payload must survive the prepend: {text}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A lint RESULT must name the rules that cannot fire.
    ///
    /// The description said so and that was not enough. Linting an
    /// environment that was Yellow because of a dead-lettered message
    /// returned two unrelated findings and nothing about the queue — so
    /// the reader sees findings, concludes lint has looked, and the
    /// actual cause of the health state is invisible. Observed against a
    /// live fleet.
    #[tokio::test]
    async fn a_lint_result_names_the_rules_it_could_not_check() {
        let resp = rpc(
            &demo_server(),
            json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
                   "params":{"name":"lint","arguments":{}}}),
        )
        .await
        .expect("lint answers");
        let body = resp["result"]["content"][0]["text"].as_str().expect("text");
        let v: Value = serde_json::from_str(body).expect("valid JSON");

        let not_checked = v["rules_not_checked"].as_array().unwrap_or_else(|| {
            panic!("every lint result must say what it could not check: {body}")
        });
        let joined = not_checked
            .iter()
            .filter_map(|x| x.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            joined.contains("EBL011"),
            "the worker-DLQ rule never fires here and the result must say \
             so, not only the description: {body}"
        );
        assert!(
            joined.contains("worker_queues"),
            "and must point at the tool that DOES see queues — a caveat \
             that points nowhere leaves the reader with \"lint says it is \
             fine\": {body}"
        );
        // The findings themselves must survive alongside it.
        assert!(v["issues"].is_array(), "{body}");
    }

    /// Rule 6: a result must carry its own negative space.
    ///
    /// Not a sweep — what "negative space" looks like differs per tool,
    /// so there is nothing generic to walk. This pins the five places
    /// the remedy already exists, each added after someone was misled
    /// rather than before, so removing one is a deliberate act rather
    /// than an oversight.
    ///
    /// The list is the point. Five separate features saying the same
    /// thing — *here is the edge of what I looked at* — and every one
    /// arrived as a bug report. ARCHITECTURE.md states the rule so the
    /// sixth is designed in.
    #[test]
    fn every_result_that_can_be_partial_says_so() {
        // PRODUCTION halves only. Reading whole files let this test's
        // own field names satisfy it — the guard was its own evidence,
        // and removing a field came back green. Caught by mutation, and
        // it is the second time in this file that a source scan has
        // needed protecting from itself.
        let prod = |path: &str| -> String {
            std::fs::read_to_string(path)
                .unwrap_or_else(|e| panic!("{path}: {e}"))
                .split("\n#[cfg(test)]\nmod ")
                .next()
                .unwrap_or_default()
                .to_string()
        };
        let src = format!(
            "{}{}",
            prod("src/cli/mcp/tools.rs"),
            prod("src/cli/mcp/mod.rs")
        );
        // Canary: the slices must be finding real code.
        assert!(
            src.contains("pub(super) fn append_cannot_fire"),
            "the production slice is not finding tools.rs"
        );

        for (field, what_it_bounds) in [
            ("rules_not_checked", "lint rules that cannot fire over MCP"),
            ("skipped_envs", "environments whose input fetch failed"),
            ("peeked", "whether a dead-letter queue was actually read"),
            ("complete", "whether a log window was fully consumed"),
            ("errors", "which sections of a `why` bundle failed"),
        ] {
            assert!(
                src.contains(field),
                "`{field}` is gone — it bounded {what_it_bounds}, and \
                 without it that absence reads as an answer. See rule 6 \
                 in ARCHITECTURE.md before removing it."
            );
        }
    }

    /// A narrowed grant advertises only what it named, and refuses the
    /// rest at dispatch too.
    ///
    /// The forcing case: a client wanted to delete one dead-lettered
    /// message, and the only grant available also handed it terminate
    /// across every environment the credentials reach.
    #[tokio::test]
    async fn a_scoped_grant_advertises_and_dispatches_only_its_verbs() {
        let scope = WriteScope::Only(vec!["dlq_delete".into(), "dlq_resend".into()]);
        let s = Server::with_config(true, false, scope, crate::config::Config::default());

        let resp = rpc(&s, json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
            .await
            .expect("tools/list");
        let names: Vec<String> = resp["result"]["tools"]
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();

        assert!(names.iter().any(|n| n == "dlq_delete"), "{names:?}");
        assert!(names.iter().any(|n| n == "dlq_resend"), "{names:?}");
        for ungranted in [
            "terminate",
            "deploy",
            "restart",
            "rebuild",
            "set_option",
            "dlq_purge",
        ] {
            assert!(
                !names.iter().any(|n| n == ungranted),
                "`{ungranted}` was not granted and must not be advertised — a \
                 client that cannot see a tool does not plan around it: {names:?}"
            );
        }
        // Reads are unaffected by the scope.
        assert!(names.iter().any(|n| n == "list_environments"), "{names:?}");
        // And the confirm half of the two-phase protocol rides along:
        // without it the grant could plan a delete and never dispatch
        // one, which is a broken server rather than a narrow one.
        assert!(
            names.iter().any(|n| n == "confirm_action"),
            "a narrow grant must still be able to confirm what it planned: {names:?}"
        );

        // The agent must be able to tell "not granted" from "ebman
        // can't do this" — a withheld tool is simply absent from
        // tools/list, and absent alone is ambiguous. tools/list reaches
        // the agent; the REASON only does if the server authors it.
        let init = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":0,"method":"initialize",
                   "params":{"protocolVersion":"2025-06-18","capabilities":{},
                             "clientInfo":{"name":"t","version":"0"}}}),
        )
        .await
        .expect("initialize");
        let instructions = init["result"]["instructions"]
            .as_str()
            .expect("instructions are the only version/scope signal an agent is guaranteed");
        assert!(
            instructions.contains("dlq_delete") && instructions.contains("dlq_resend"),
            "the instructions must name what WAS granted: {instructions}"
        );
        assert!(
            instructions.contains("NOT GRANTED"),
            "and say that the rest is withheld rather than missing: {instructions}"
        );

        // Calling an ungranted verb must say SO, not "unknown tool".
        // "Unknown" is a lie in the shape that matters: it teaches the
        // agent ebman lacks terminate, which directly contradicts the
        // scope line in `instructions` and is the reading that sends it
        // off to report a capability gap instead of asking.
        let call = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":9,"method":"tools/call",
                   "params":{"name":"terminate","arguments":{"env":"demo-prod"}}}),
        )
        .await
        .expect("tools/call");
        assert!(
            call.get("error").is_none(),
            "an ungranted verb is a tool refusal, not a protocol error: {call}"
        );
        let text = call["result"]["content"][0]["text"]
            .as_str()
            .expect("text content");
        assert!(
            text.contains("not in this server's write scope"),
            "the refusal must name the scope: {text}"
        );
        assert!(
            !text.contains("unknown tool"),
            "and must not claim the tool does not exist: {text}"
        );
        assert_eq!(call["result"]["isError"], json!(true));

        // A genuinely unknown name is still a protocol error.
        let bogus = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":10,"method":"tools/call",
                   "params":{"name":"no_such_tool","arguments":{}}}),
        )
        .await
        .expect("tools/call");
        assert_eq!(bogus["error"]["code"], json!(-32602), "{bogus}");

        // And the dispatch gate refuses it even if a client held a
        // cached list from a wider grant.
        let err = s
            .tool_write_plan_for_tests(writes::WriteVerb::Terminate, &json!({"env": "x"}))
            .await
            .expect_err("terminate is outside the scope");
        assert!(
            err.contains("not in this server's write scope"),
            "the refusal must name the scope, not read as a missing tool: {err}"
        );
        assert!(
            err.contains("--allow-writes=terminate"),
            "and say how to grant it: {err}"
        );

        // The positive half: a granted verb gets PAST the scope gate.
        // Without this the test would still pass if `allows` returned
        // false for everything, which is a grant that grants nothing.
        let granted = s
            .tool_write_plan_for_tests(writes::WriteVerb::DlqDelete, &json!({}))
            .await;
        if let Err(e) = &granted {
            assert!(
                !e.contains("not in this server's write scope"),
                "`dlq_delete` was granted and must clear the scope gate \
                 (any later complaint about arguments is fine): {e}"
            );
        }
    }

    /// An unknown verb in the flag is an error, never a silent skip.
    ///
    /// A typo'd `--allow-writes=dlq_delte` that quietly granted nothing
    /// looks exactly like a working narrow grant until the first write
    /// is refused; one that quietly granted everything would be worse.
    #[test]
    fn a_mistyped_write_verb_is_refused_at_startup() {
        let known = ["dlq_delete", "terminate"];
        let err = super::parse_write_scope(Some("dlq_delte"), &known)
            .expect_err("a typo must not be silently ignored");
        assert!(err.contains("dlq_delte"), "name the typo: {err}");
        assert!(err.contains("dlq_delete"), "and list what IS known: {err}");

        // Bare `--allow-writes` keeps meaning everything, so an
        // existing .mcp.json is unaffected.
        assert_eq!(
            super::parse_write_scope(None, &known).expect("bare is valid"),
            WriteScope::All
        );
        // `--allow-writes=` with nothing after it is a mistake, not an
        // empty grant that silently disables writes.
        assert!(super::parse_write_scope(Some(""), &known).is_err());
        assert!(super::parse_write_scope(Some("  , ,"), &known).is_err());

        assert_eq!(
            super::parse_write_scope(Some("terminate, dlq_delete ,terminate"), &known)
                .expect("valid"),
            WriteScope::Only(vec!["terminate".into(), "dlq_delete".into()]),
            "whitespace tolerated, duplicates collapsed, order kept"
        );
    }

    /// Every verb round-trips: the name `--allow-writes` accepts is
    /// the name the tool is advertised as is the name the dispatch
    /// gate checks.
    ///
    /// Three spellings of one thing, and two of them are hand-written.
    /// Drift between them does not fail loudly — it makes
    /// `--allow-writes=dlq_purge` advertise the tool and then refuse it
    /// at dispatch, which reads as a broken server rather than a typo.
    /// A mutation changing one arm of `tool_name` to `dlq-purge` walked
    /// straight past the first version of this guard.
    #[test]
    fn every_write_verb_round_trips_through_the_tool_table() {
        let nameable = writes::write_verb_names();
        assert_eq!(
            nameable.len(),
            writes::WriteVerb::ALL.len(),
            "a write verb exists in one table and not the other: nameable={nameable:?}"
        );

        let advertised: Vec<String> = tool_table(&WriteScope::All, true)
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();

        for verb in writes::WriteVerb::ALL {
            let name = verb.tool_name();
            assert!(
                nameable.iter().any(|n| n == name),
                "`{name}` is what the dispatch gate checks but not a name \
                 --allow-writes accepts: {nameable:?}"
            );
            assert!(
                advertised.contains(&name.to_string()),
                "`{name}` is checkable but never advertised: {advertised:?}"
            );

            // One discriminating case per verb: a grant of exactly this
            // verb admits it and nothing else. Sampling a single verb
            // would leave the other seven asserted by analogy.
            let only = WriteScope::Only(vec![name.to_string()]);
            assert!(only.allows(name), "a grant of `{name}` must allow it");
            for other in writes::WriteVerb::ALL {
                if other.tool_name() != name {
                    assert!(
                        !only.allows(other.tool_name()),
                        "a grant of `{name}` must not allow `{}`",
                        other.tool_name()
                    );
                }
            }
        }
    }

    /// A second `--allow-writes` is an error, not last-wins.
    ///
    /// `--allow-writes=dlq_delete --allow-writes` under last-wins
    /// silently widens a deliberately narrow grant to every verb —
    /// the fail-open this flag exists to remove, reachable by a stray
    /// duplicate in a hand-edited `.mcp.json` args array.
    #[test]
    fn a_repeated_write_flag_cannot_silently_widen_the_grant() {
        let args = |v: &[&str]| -> Vec<String> {
            std::iter::once("mcp")
                .chain(std::iter::once("serve"))
                .chain(v.iter().copied())
                .map(str::to_string)
                .collect()
        };

        let err = parse_mcp_args(&args(&["--allow-writes=dlq_delete", "--allow-writes"]))
            .expect_err("a second flag must not widen the first");
        assert!(
            err.contains("more than once"),
            "the error must name the duplication: {err}"
        );
        assert!(
            err.contains("--allow-writes=a,b"),
            "and say what to do instead: {err}"
        );
        // The rendered message, not the literal: a wrapped string
        // without a continuation embeds the newline and the next
        // line's indent, and this one goes to a one-line status bar.
        assert!(
            !err.contains("  "),
            "a wrapped literal left a gap in the rendered message: {err:?}"
        );

        // Narrowing order does not rescue it either.
        assert!(parse_mcp_args(&args(&["--allow-writes", "--allow-writes=dlq_delete"])).is_err());

        // One flag is still fine.
        let ok = parse_mcp_args(&args(&["--allow-writes=dlq_delete"])).expect("one flag is valid");
        assert_eq!(ok.write_scope, WriteScope::Only(vec!["dlq_delete".into()]));
    }

    /// A grant of nothing must read as nothing.
    ///
    /// The parser cannot build an empty `Only`, but a scope answering
    /// `any() == true` while `allows()` refused every verb would
    /// advertise `confirm_action` beside no verb to confirm — a server
    /// that looks write-capable and is not.
    #[test]
    fn an_empty_grant_is_not_a_grant() {
        let empty = WriteScope::Only(Vec::new());
        assert!(
            !empty.any(),
            "an empty grant must not read as write-capable"
        );
        let names: Vec<String> = tool_table(&empty, true)
            .as_array()
            .expect("array")
            .iter()
            .filter_map(|t| t["name"].as_str().map(str::to_string))
            .collect();
        assert!(
            !names.iter().any(|n| n == "confirm_action"),
            "nothing to confirm, so nothing should offer to confirm it: {names:?}"
        );
    }

    /// The audit-init decision, pinned outside `run`.
    ///
    /// A reads-only server must not read the config disk, and a demo
    /// server must not acquire a webhook. Both are decided on one line
    /// inside the process entry, where no lib test reaches — the
    /// mutation sweep found `&&` → `||` and a dropped `!` both survive
    /// there, which is why the decision moved out.
    #[test]
    fn only_a_live_write_capable_server_reads_the_audit_config() {
        let narrow = WriteScope::Only(vec!["dlq_delete".into()]);

        assert!(should_init_audit(&WriteScope::All, false));
        assert!(
            should_init_audit(&narrow, false),
            "a narrow grant still dispatches writes, so it still audits"
        );

        assert!(
            !should_init_audit(&WriteScope::None, false),
            "a reads-only server must not touch the config disk"
        );
        assert!(
            !should_init_audit(&WriteScope::All, true),
            "demo must not acquire a webhook — `||` here would give it one"
        );
        assert!(
            !should_init_audit(&WriteScope::None, true),
            "and neither half alone is enough — dropping the `!` inverts this"
        );
    }

    /// `serve` logs to a file; `setup` writes nothing.
    ///
    /// Both directions are load-bearing: `setup`'s module doc promises
    /// it writes no files, and `serve` is a daemon whose only voice is
    /// the log — it had none, so the elicitation measurement stage 5
    /// waits on recorded nothing at all.
    #[test]
    fn only_the_mcp_daemon_opens_a_log_file() {
        let a = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };

        assert!(wants_file_logging(&a(&["mcp", "serve"])));
        assert!(wants_file_logging(&a(&[
            "mcp",
            "serve",
            "--demo",
            "--allow-writes"
        ])));

        assert!(
            !wants_file_logging(&a(&["mcp", "setup"])),
            "setup is a pure printer that promises it writes no files"
        );
        assert!(!wants_file_logging(&a(&["mcp", "setup", "--allow-writes"])));
        assert!(!wants_file_logging(&a(&["mcp"])), "a bare usage error");
    }

    /// The instructions tell the agent that a grant is not its to make.
    ///
    /// A peer session on 0.40.0 went to scope its own server to
    /// `--allow-writes=dlq_delete` — the narrowest grant, the exact
    /// case the feature was built for — and its client refused the
    /// edit as self-modification. The refusal was correct; the wasted
    /// attempt was avoidable. Nothing on this surface said whose job
    /// the edit was, and `tools/list` showing no write tools reads as
    /// "go and enable them".
    #[test]
    fn the_instructions_say_a_grant_is_not_the_agents_to_make() {
        let read_only = WriteScope::None.agent_summary(None, false);
        assert!(
            read_only.contains("Do not edit the MCP config yourself"),
            "a read-only server must say whose job the grant is: {read_only}"
        );
        assert!(
            read_only.contains("self-modification"),
            "and warn that trying costs a denial: {read_only}"
        );

        // A narrow grant needs the other half: the verb it is missing
        // may have been granted already and not picked up.
        let narrow = WriteScope::Only(vec!["dlq_delete".into()]).agent_summary(None, false);
        assert!(
            narrow.contains("restart the client"),
            "a client that reconnects without re-reading its config shows the \
             verb as absent, which reads as a broken feature: {narrow}"
        );
        assert!(narrow.contains("Asking is your part"), "{narrow}");
    }

    /// A standing refusal outranks the grant in the instructions.
    ///
    /// The block is the one channel an agent is guaranteed to read. On
    /// a server with `safety.read_only` set it said "Writes are
    /// ENABLED for every verb" — true about the flag, false about the
    /// server, and the sentence an agent would plan an afternoon
    /// around. `doctor` reported it correctly, which is not a defence:
    /// a tool the agent has to think to call is not the same as the
    /// text it is handed.
    #[tokio::test]
    async fn a_standing_refusal_outranks_the_grant_in_the_instructions() {
        let cfg = crate::config::Config {
            safety_read_only: true,
            ..crate::config::Config::default()
        };
        let s = Server::with_config(true, false, WriteScope::All, cfg);
        let init = rpc(
            &s,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize",
                   "params":{"protocolVersion":"2025-06-18","capabilities":{},
                             "clientInfo":{"name":"t","version":"0"}}}),
        )
        .await
        .expect("initialize");
        let ins = init["result"]["instructions"]
            .as_str()
            .expect("instructions");

        assert!(ins.contains("Writes are REFUSED"), "{ins}");
        assert!(
            ins.contains("safety.read_only"),
            "and name the control: {ins}"
        );
        assert!(
            !ins.contains("Writes are ENABLED"),
            "the grant must not be described on a server that refuses every write: {ins}"
        );

        // An unreadable safety config fails closed the same way, and
        // says so rather than describing a grant it will not honour.
        let broken = crate::config::Config {
            safety_parse_errors: vec!["safety.envs.prod = true is missing .read_only".into()],
            ..crate::config::Config::default()
        };
        let b = Server::with_config(true, false, WriteScope::All, broken);
        assert!(
            b.write_scope
                .agent_summary(Some("the safety config could not be parsed."), false)
                .contains("REFUSED"),
            "a fail-closed parse refuses every write and the block must say so"
        );

        // The control: an unrestricted server still describes its grant.
        let open = Server::with_config(
            true,
            false,
            WriteScope::All,
            crate::config::Config::default(),
        );
        assert!(
            open.write_scope
                .agent_summary(None, false)
                .contains("ENABLED"),
            "without a standing refusal the grant is the right thing to describe"
        );
    }

    /// A call that can ask a human gets a human's budget.
    ///
    /// `TOOL_TIMEOUT_SECS` is 30 and bounds a hung AWS call, which is
    /// what it was written for. Applied to a person deciding whether
    /// to delete production data it turns the gate into a tool that
    /// times out under ordinary use — the failure a reviewer flagged
    /// as "the difference between a gate and a thing that breaks when
    /// used".
    #[test]
    fn only_the_call_that_asks_a_human_gets_a_humans_budget() {
        // The one tool every write dispatches through, on a connection
        // that can be asked.
        assert_eq!(
            call_timeout_secs(writes::CONFIRM_TOOL, true),
            ASK_TIMEOUT_SECS
        );
        // A connection that cannot be asked keeps the AWS bound —
        // nothing will stop to ask, so a long budget would only make a
        // hung dispatch take longer to fail.
        assert_eq!(
            call_timeout_secs(writes::CONFIRM_TOOL, false),
            TOOL_TIMEOUT_SECS
        );

        // Every other tool is AWS-bound whatever the client declared.
        for t in [
            "list_environments",
            "worker_queues",
            "deploy",
            "terminate",
            "doctor",
        ] {
            assert_eq!(
                call_timeout_secs(t, true),
                TOOL_TIMEOUT_SECS,
                "`{t}` cannot block on a person and must keep the AWS bound"
            );
        }
    }

    /// The frame loop uses the budget rather than the raw constant.
    ///
    /// `call_timeout_secs` is pure and tested, and that proves
    /// nothing about the one place it matters: a mutation swapping
    /// `budget` back for `TOOL_TIMEOUT_SECS` at the call site left the
    /// whole suite green. The loop is reachable only through stdio, so
    /// this is anchored in source — the same shape as
    /// `the_extracted_gates_are_wired_into_run`.
    #[test]
    fn the_frame_loop_times_calls_by_the_computed_budget() {
        let src = include_str!("mod.rs");
        let prod = crate::app::tests::scan::production_half(src);
        let call = prod
            .split("let outcome = tokio::time::timeout(")
            .nth(1)
            .and_then(|r| r.split(".await").next())
            .expect("the frame loop times the tool call here");

        assert!(
            call.contains("from_secs(budget)"),
            "the tool call must be bounded by the COMPUTED budget: a person deciding \
             whether to delete production data gets the AWS bound otherwise, and the \
             gate times out under ordinary use:\n{call}"
        );
        assert!(
            !call.contains("TOOL_TIMEOUT_SECS"),
            "and not by the raw constant, which is what it was before:\n{call}"
        );

        // Canary: the anchor must still find the loop. A scan that has
        // stopped seeing its subject passes silently, which is worse
        // than finding a defect.
        assert!(
            prod.contains("let budget = call_timeout_secs("),
            "the budget is no longer computed in this file — this guard has lost \
             its subject rather than being satisfied"
        );
    }

    /// A reply that is not a clear "accept" is not an approval.
    ///
    /// This is the one place where being generous to a malformed
    /// client converts a broken reply into a standing yes. Every
    /// shape that is not exactly `accept` denies.
    #[test]
    fn only_an_explicit_accept_approves() {
        let accept = json!({"jsonrpc":"2.0","id":1,"result":{"action":"accept"}});
        assert_eq!(ask_outcome_from(&accept), AskOutcome::Approved);

        for reply in [
            json!({"jsonrpc":"2.0","id":1,"result":{"action":"decline"}}),
            json!({"jsonrpc":"2.0","id":1,"result":{"action":"cancel"}}),
            // Malformed, missing, wrong type, an error response, a
            // result that is not an object — none of these is consent.
            json!({"jsonrpc":"2.0","id":1,"result":{"action":"ACCEPT"}}),
            json!({"jsonrpc":"2.0","id":1,"result":{"action":true}}),
            json!({"jsonrpc":"2.0","id":1,"result":{}}),
            json!({"jsonrpc":"2.0","id":1,"result":"accept"}),
            json!({"jsonrpc":"2.0","id":1,"error":{"code":-32601,"message":"no"}}),
            json!({"jsonrpc":"2.0","id":1}),
        ] {
            assert_eq!(
                ask_outcome_from(&reply),
                AskOutcome::Declined,
                "not an explicit accept, so not an approval: {reply}"
            );
        }
    }

    /// Not-asked and unanswered have opposite consequences.
    #[test]
    fn not_asked_is_not_the_same_as_unanswered() {
        assert!(
            AskOutcome::Unanswered.refuses(),
            "an ask nobody answered denies"
        );
        assert!(AskOutcome::Declined.refuses());
        assert!(
            !AskOutcome::NotAsked.refuses(),
            "no question was put, so there is no answer to respect — conflating \
             these denied every write on every client that cannot elicit, including \
             ones the operator had granted with the flag"
        );
        assert_ne!(
            AskOutcome::NotAsked,
            AskOutcome::Approved,
            "nor is it an approval"
        );
    }

    /// A client that cannot elicit is not asked, and is not refused.
    #[tokio::test]
    async fn a_client_without_elicitation_is_not_asked() {
        let s = Server::with_scope(true, false, WriteScope::All);
        assert_eq!(
            s.ask_operator("delete something").await,
            AskOutcome::NotAsked
        );
    }

    /// An ask with nobody listening denies rather than hanging.
    #[tokio::test(start_paused = true)]
    async fn an_unanswered_ask_denies_when_the_budget_expires() {
        let s = Server::with_scope(true, false, WriteScope::All);
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
        if let Ok(mut slot) = s.outbound.lock() {
            *slot = Some(tx);
        }

        let asked = tokio::spawn(async move { s.ask_operator("delete something").await });
        // The question goes out...
        let frame: Value = serde_json::from_str(&rx.recv().await.expect("a frame")).expect("json");
        assert_eq!(frame["method"], json!("elicitation/create"), "{frame}");
        assert!(
            frame["params"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("delete something")),
            "the question must carry what is being asked: {frame}"
        );

        // ...and nobody ever replies.
        tokio::time::advance(std::time::Duration::from_secs(ASK_TIMEOUT_SECS + 1)).await;
        // Past ASK_WAIT_SECS, which is the one that fires.
        let outcome = asked.await.expect("join");
        assert_eq!(
            outcome,
            AskOutcome::Unanswered,
            "an operator who walked away must produce a deny, not a call that \
             never returns"
        );
    }

    /// The answer reaches the waiting ask.
    ///
    /// A reply has an id and no method, which the frame loop would
    /// otherwise answer `-32601` while the ask sat waiting out its
    /// budget — denying a write the operator had just approved.
    #[tokio::test]
    async fn an_answer_is_routed_back_to_the_ask_that_is_waiting() {
        let s = std::sync::Arc::new(Server::with_scope(true, false, WriteScope::All));
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(4);
        if let Ok(mut slot) = s.outbound.lock() {
            *slot = Some(tx);
        }

        let asking = {
            let s = std::sync::Arc::clone(&s);
            tokio::spawn(async move { s.ask_operator("terminate prod").await })
        };
        let frame: Value = serde_json::from_str(&rx.recv().await.expect("frame")).expect("json");
        let id = frame["id"].clone();

        let reply = json!({"jsonrpc": "2.0", "id": id, "result": {"action": "accept"}});
        assert!(
            s.take_ask_reply(&reply),
            "a frame carrying an id we issued, with no method, is ours"
        );
        assert_eq!(asking.await.expect("join"), AskOutcome::Approved);

        // Someone else's frame is not ours, and must fall through to
        // the normal dispatch rather than being swallowed.
        assert!(!s.take_ask_reply(&json!({"jsonrpc":"2.0","id":7,"result":{}})));
        assert!(!s.take_ask_reply(&json!({"jsonrpc":"2.0","id":1,"method":"ping"})));
    }

    /// A demo server whose client can elicit, with a stand-in for the
    /// frame loop answering every ask with `action` and recording what
    /// it was asked.
    fn demo_answering(
        action: &'static str,
    ) -> (
        std::sync::Arc<Server>,
        std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    ) {
        let s = std::sync::Arc::new(demo_writes_server());
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
        if let Ok(mut slot) = s.outbound.lock() {
            *slot = Some(tx);
        }
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let seen = std::sync::Arc::clone(&asked);
        let srv = std::sync::Arc::clone(&s);
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                let frame: Value = serde_json::from_str(&line).expect("json");
                if let Some(m) = frame["params"]["message"].as_str() {
                    seen.lock().expect("lock").push(m.to_string());
                }
                // Exactly what the real frame loop does with a reply.
                let reply = json!({
                    "jsonrpc": "2.0",
                    "id": frame["id"].clone(),
                    "result": {"action": action},
                });
                assert!(srv.take_ask_reply(&reply), "the loop must route it back");
            }
        });
        (s, asked)
    }

    /// The gate is *wired*, not merely present.
    ///
    /// `ask_operator` having its own passing tests says nothing about
    /// whether `confirm_action` consults it — four separate defects
    /// this cycle were a tested function nothing called. So: decline,
    /// and assert the dispatch did not happen.
    #[tokio::test]
    async fn a_declined_ask_stops_the_write() {
        let (s, asked) = demo_answering("decline");
        let env = demo_fixture::envs()[0].name.clone();
        let (err, plan) = call(&s, "restart", json!({"env": &env})).await;
        assert!(!err, "planning is not gated: {plan}");
        let token = plan["confirm_token"].as_str().expect("token").to_string();

        let (err, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
        assert!(err, "a declined ask must refuse the write: {out}");
        let text = out.to_string();
        assert!(
            text.contains("declined"),
            "the refusal must say the operator declined, not blame a missing \
             flag or an expired token: {text}"
        );
        assert!(
            !text.contains("--allow-writes"),
            "and must not send the agent off to widen permissions it already \
             has — the answer was no: {text}"
        );

        let asked = asked.lock().expect("lock");
        assert_eq!(asked.len(), 1, "exactly one ask: {asked:?}");
        assert!(
            asked[0].contains(&env) && asked[0].to_lowercase().contains("restart"),
            "the operator must be told which verb on which environment, or the \
             question is unanswerable: {:?}",
            asked[0]
        );
    }

    /// And an approval lets it through — otherwise "it refuses" is
    /// satisfied by a gate that refuses everything.
    #[tokio::test]
    async fn an_approved_ask_lets_the_write_through() {
        let (s, asked) = demo_answering("accept");
        let env = demo_fixture::envs()[0].name.clone();
        let (_, plan) = call(&s, "restart", json!({"env": env})).await;
        let token = plan["confirm_token"].as_str().expect("token").to_string();

        let (err, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
        assert!(!err, "an approved write must dispatch: {out}");
        assert_eq!(asked.lock().expect("lock").len(), 1, "and must still ask");
    }

    /// The ask happens once per dispatch, not once per plan.
    ///
    /// A spent token must not re-ask: an agent replaying a used token
    /// would otherwise put the same question to the operator again,
    /// and repeated identical prompts are how consent gets clicked
    /// through.
    #[tokio::test]
    async fn a_spent_token_does_not_ask_again() {
        let (s, asked) = demo_answering("accept");
        let env = demo_fixture::envs()[0].name.clone();
        let (_, plan) = call(&s, "restart", json!({"env": env})).await;
        let token = plan["confirm_token"].as_str().expect("token").to_string();

        assert!(
            !call(&s, "confirm_action", json!({"confirm_token": &token}))
                .await
                .0
        );
        assert!(
            call(&s, "confirm_action", json!({"confirm_token": &token}))
                .await
                .0,
            "the token is single-use"
        );
        assert_eq!(
            asked.lock().expect("lock").len(),
            1,
            "the replay must be rejected before anyone is asked"
        );
    }

    /// The frame loop must claim an ask reply *before* it dispatches.
    ///
    /// The loop reads stdin and can't be called here, so this pins the
    /// two halves separately: that interception is load-bearing
    /// (below), and that the source has it in the right place
    /// (`take_ask_reply_precedes_the_dispatch`). Without it a reply is
    /// answered `-32601`, the ask waits out its full budget, and a
    /// write the operator *approved* is denied — indistinguishable
    /// from a timeout, so it would be diagnosed as a slow operator
    /// rather than a routing bug.
    ///
    /// Note it is `handle_request`, not `invalid_request_response`,
    /// that does the damage: that one only rejects non-objects, so a
    /// reply sails straight past it. Written the other way round first,
    /// and the test failed — the guard was aimed at a function that
    /// would never have fired.
    #[tokio::test]
    async fn an_unclaimed_reply_would_be_answered_as_a_bad_method() {
        let s = demo_writes_server();
        let reply = json!({"jsonrpc": "2.0", "id": 1_000_000, "result": {"action": "accept"}});
        assert!(
            invalid_request_response(&reply).is_none(),
            "the validity check passes it through — it only rejects non-objects"
        );
        let resp = s.handle_request(&reply).await.expect("a response");
        assert_eq!(
            resp["error"]["code"], -32601,
            "so an unclaimed reply gets method-not-found, and the approval is \
             lost: {resp}"
        );
    }

    /// ...and that the source actually intercepts before dispatching.
    #[test]
    fn take_ask_reply_precedes_the_dispatch() {
        let src = include_str!("mod.rs");
        let body = crate::app::tests::scan::production_half(src);
        let claim = body
            .find("if server.take_ask_reply(&req)")
            .expect("the frame loop must route ask replies");
        let dispatch = body
            .find("server.handle_request(&req).await")
            .expect("the frame loop must dispatch requests");
        assert!(
            claim < dispatch,
            "take_ask_reply must come first: a reply has an id and no method, \
             so the dispatch below answers it -32601 and the ask denies a write \
             the operator had approved"
        );
    }

    /// The agent is told the ask exists — and only when it does.
    ///
    /// Elicitation is negotiated in protocol metadata the *client*
    /// consumes; none of it reaches the agent reading `instructions`.
    /// So an agent on an ask-capable connection, not told, reads the
    /// operator's decline as a permission error and retries — which is
    /// the one response a decline must not produce.
    #[test]
    fn the_summary_says_whether_confirmations_are_put_to_a_person() {
        for scope in [
            WriteScope::All,
            WriteScope::Only(vec!["restart".into(), "dlq_delete".into()]),
        ] {
            let asked = scope.agent_summary(None, true);
            let silent = scope.agent_summary(None, false);
            assert!(
                asked.contains("OPERATOR"),
                "a granted scope on an ask-capable client must say the confirmation \
                 reaches a person: {asked}"
            );
            assert!(
                asked.contains("decline"),
                "and must say what a decline means, or it reads as an error: {asked}"
            );
            assert!(
                !silent.contains("OPERATOR"),
                "but must NOT promise an ask that cannot happen — on a client that \
                 can't elicit, nobody is reachable and the agent would wait for a \
                 human who is never shown anything: {silent}"
            );
        }
    }

    /// A standing refusal still outranks the ask note.
    ///
    /// Otherwise a server that refuses every write tells the agent to
    /// expect the operator to be asked — inviting it to plan a write
    /// and wait on a question that will never be put.
    #[test]
    fn a_standing_refusal_outranks_the_ask_note() {
        let t = WriteScope::All.agent_summary(Some("safety.read_only is set."), true);
        assert!(t.contains("REFUSED"), "{t}");
        assert!(
            !t.contains("OPERATOR"),
            "nothing will be put to anyone on a server that refuses every write: {t}"
        );
    }

    /// Parity by default: a client that can be asked gets the write
    /// surface with no flag at all.
    ///
    /// This is the design's whole usability claim. Without it the
    /// operator must stop, edit a config file and restart their client
    /// the first time they want a write — which is the point at which
    /// people stop bothering.
    #[tokio::test]
    async fn an_ask_capable_client_gets_writes_without_a_flag() {
        let s = Server::with_scope(true, false, WriteScope::None);
        assert_eq!(
            s.effective_scope(),
            WriteScope::None,
            "no flag and no ask is still read-only"
        );
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            s.effective_scope(),
            WriteScope::All,
            "but a client that can be asked gets parity with the TUI"
        );
    }

    /// An explicitly narrowed grant is NOT widened by elicitation.
    ///
    /// `--allow-writes=dlq_delete` is an operator saying "only this".
    /// Opening it to every verb because the client supports a dialog
    /// would override a restriction they typed deliberately — the one
    /// direction this design must never move on its own.
    #[tokio::test]
    async fn elicitation_does_not_widen_a_narrowed_grant() {
        let narrow = WriteScope::Only(vec!["dlq_delete".into()]);
        let s = Server::with_scope(true, false, narrow.clone());
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            s.effective_scope(),
            narrow,
            "the operator said only dlq_delete, and a dialog capability is not \
             their permission to widen it"
        );
        assert!(!s.effective_scope().allows("terminate"));
    }

    /// The advertised surface follows the connection, not argv.
    ///
    /// `tools/list` is where an agent learns what it may do. If the
    /// scope opened but the table did not, the write tools would exist
    /// and be invisible — and an agent that cannot see a tool will
    /// tell the operator ebman does not support it.
    #[tokio::test]
    async fn the_advertised_tools_follow_the_connection() {
        let s = demo_server(); // no flag
        let names = |s: &Server| -> Vec<String> {
            tool_table(&s.effective_scope(), true)
                .as_array()
                .expect("array")
                .iter()
                .filter_map(|t| t["name"].as_str().map(str::to_string))
                .collect()
        };
        let before = names(&s);
        assert!(
            !before.iter().any(|n| n == "confirm_action"),
            "read-only: {before:?}"
        );
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let after = names(&s);
        assert!(
            after.iter().any(|n| n == "confirm_action"),
            "an ask-capable client must be able to SEE the write surface, not \
             just be permitted it: {after:?}"
        );
        assert!(
            after.len() > before.len(),
            "and the read tools must not have been swapped out for them"
        );
    }

    /// Doctor reports the surface this connection actually has.
    ///
    /// It is the tool an agent runs when something is missing, so a
    /// doctor still reading argv would say "read-only" on a connection
    /// that had just been granted everything.
    #[tokio::test]
    async fn doctor_reports_the_effective_surface() {
        let s = demo_server();
        // Through the tool interface, not the private method: that is
        // how an agent reaches it, and it pins the wiring too.
        async fn doctor(s: &Server) -> String {
            call(s, "doctor", json!({})).await.1.to_string()
        }
        assert!(doctor(&s).await.contains("read-only"));
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let d = doctor(&s).await;
        assert!(
            !d.contains("read-only"),
            "doctor must not report a restriction this connection does not have: {d}"
        );
        assert!(d.contains("every verb"), "{d}");
    }

    /// The whole feature, end to end: no flag, no restart, one human
    /// answer, and the write goes.
    ///
    /// Each piece is tested above in isolation, and every one of them
    /// can pass while the path as a whole does not — which is the
    /// failure this cycle kept producing. So this drives the actual
    /// tools an agent calls, on a server started with no write flag.
    #[tokio::test]
    async fn no_flag_one_answer_and_the_write_dispatches() {
        let s = std::sync::Arc::new(demo_server());
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let (tx, mut rx) = tokio::sync::mpsc::channel::<String>(8);
        if let Ok(mut slot) = s.outbound.lock() {
            *slot = Some(tx);
        }
        let srv = std::sync::Arc::clone(&s);
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                let f: Value = serde_json::from_str(&line).expect("json");
                let reply = json!({"jsonrpc":"2.0","id":f["id"].clone(),
                                   "result":{"action":"accept"}});
                assert!(srv.take_ask_reply(&reply));
            }
        });

        let env = demo_fixture::envs()[0].name.clone();
        let (err, plan) = call(&s, "restart", json!({"env": env})).await;
        assert!(
            !err,
            "a write tool must be callable with no flag when the operator can be \
             asked — this is the restart-your-client problem the design exists to \
             remove: {plan}"
        );
        let token = plan["confirm_token"].as_str().expect("a token").to_string();
        let (err, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
        assert!(!err, "and it must dispatch once approved: {out}");
    }

    /// The ask must lose to nothing, and beat its own container.
    ///
    /// Found in self-review: both were `ASK_TIMEOUT_SECS`, so the outer
    /// call bound — whose clock starts earlier — would fire first, and
    /// the deny plus its `rule=not_approved` audit line would be lost
    /// with the dropped future. The suite was green: every ask test
    /// either answers or waits past both.
    ///
    /// The constants are compared at compile time above. What this adds
    /// is the WIRING: that `call_timeout_secs` actually hands
    /// `confirm_action` the larger budget on an ask-capable connection.
    /// A margin between two constants is worth nothing if the call that
    /// carries the ask is still bounded at 30 seconds.
    #[test]
    fn the_ask_resolves_inside_the_call_that_carries_it() {
        assert!(
            ASK_WAIT_SECS < call_timeout_secs(writes::CONFIRM_TOOL, true),
            "an ask given its container's whole budget never returns its own \
             answer — the outer timeout wins and nothing is audited"
        );
    }

    /// Two messages, ONE confirmation.
    ///
    /// The constraint this feature exists to satisfy, in the
    /// maintainer's words: *"if I ask to delete certain messages 1
    /// confirmation is fine, more than that and it's easier to do it
    /// myself"*. Every write asks, so without batching a ten-message
    /// clean-up is ten dialogs and the tool is worse than the console.
    ///
    /// Asserting the ask COUNT is the point. Everything else here
    /// could pass while the server quietly asked once per message.
    #[tokio::test]
    async fn a_batch_of_two_asks_once_and_reports_per_message() {
        let (s, asked) = demo_answering("accept");
        let ids: Vec<String> = crate::demo_fixture::dlq_messages_for_env("poly-batch")
            .into_iter()
            .map(|m| m.id)
            .collect();
        assert_eq!(ids.len(), 2, "the fixture must hold a real batch");

        let (err, plan) = call(
            &s,
            "dlq_delete",
            json!({"env": "poly-batch", "message_ids": ids.clone()}),
        )
        .await;
        assert!(!err, "a batch plan must be accepted: {plan}");
        let token = plan["confirm_token"].as_str().expect("token").to_string();
        let plan_text = plan.to_string();
        for id in &ids {
            assert!(
                plan_text.contains(id.as_str()),
                "the plan must name every message it covers: {plan_text}"
            );
        }

        let (err, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
        assert!(!err, "the batch must dispatch: {out}");

        let asked = asked.lock().expect("lock");
        assert_eq!(
            asked.len(),
            1,
            "ONE dialog for the whole batch — asking per message is the failure \
             this feature exists to remove: {asked:?}"
        );
        for id in &ids {
            assert!(
                asked[0].contains(id.as_str()),
                "and that one dialog must enumerate what it covers, or the operator \
                 approves a set they were not shown: {:?}",
                asked[0]
            );
        }

        let text = out.to_string();
        assert!(
            text.contains("\"succeeded\":2"),
            "the result must account for both: {text}"
        );
        assert!(
            text.contains("\"failed\":0"),
            "including the failures it did NOT have — an absent count reads as \
             unknown: {text}"
        );
    }

    /// A batch plan over the cap is refused before anyone is asked.
    #[tokio::test]
    async fn an_oversized_batch_never_reaches_the_operator() {
        let (s, asked) = demo_answering("accept");
        let too_many: Vec<String> = (0..=super::writes::DLQ_BATCH_CAP)
            .map(|i| format!("id-{i}"))
            .collect();
        let (err, out) = call(
            &s,
            "dlq_delete",
            json!({"env": "poly-batch", "message_ids": too_many}),
        )
        .await;
        assert!(err, "over the cap must refuse: {out}");
        assert_eq!(
            asked.lock().expect("lock").len(),
            0,
            "and must refuse at PLAN time — an unreadable list must never reach a \
             dialog, because the cap exists to keep the dialog readable"
        );
    }

    /// A dead ask channel DENIES; it does not fall through.
    ///
    /// Found by a release panel and confirmed: `NotAsked` meant three
    /// different things — no capability, no channel, failed send — and
    /// only the first has a flag to fall back to. On a connection
    /// widened purely by elicitation the ask IS the gate, so a client
    /// that crashed mid-confirm dispatched an unapproved terminate.
    #[tokio::test]
    async fn a_dead_ask_channel_denies_rather_than_falling_through() {
        let s = Server::with_scope(true, false, WriteScope::None);
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        // Capability declared, but no channel — the shutdown path
        // clears it, and a confirm already in flight sees this.
        assert_eq!(
            s.ask_operator("terminate poly-prod").await,
            AskOutcome::Unanswered,
            "a client that can be asked but cannot be reached must DENY — \
             `NotAsked` would fall back to a flag that was never given"
        );

        // A closed receiver is the same hazard by another route.
        let (tx, rx) = tokio::sync::mpsc::channel::<String>(1);
        drop(rx);
        if let Ok(mut slot) = s.outbound.lock() {
            *slot = Some(tx);
        }
        assert_eq!(
            s.ask_operator("terminate poly-prod").await,
            AskOutcome::Unanswered,
            "a send that cannot be delivered is an unanswered ask, not an absent one"
        );

        // And the one case that legitimately falls back is untouched.
        let flagged = Server::with_scope(true, false, WriteScope::All);
        assert_eq!(
            flagged.ask_operator("x").await,
            AskOutcome::NotAsked,
            "no capability is still NotAsked, or every flag-granted client is denied"
        );
    }

    /// The whole point, end to end: no flag, no channel, no dispatch.
    #[tokio::test]
    async fn a_write_that_only_the_ask_gated_cannot_dispatch_unasked() {
        let s = demo_server(); // no --allow-writes
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let env = demo_fixture::envs()[0].name.clone();
        let (err, plan) = call(&s, "restart", json!({"env": env})).await;
        assert!(!err, "planning is open: {plan}");
        let token = plan["confirm_token"].as_str().expect("token").to_string();

        // No outbound channel was ever installed — the client is gone.
        let (err, out) = call(&s, "confirm_action", json!({"confirm_token": token})).await;
        assert!(
            err,
            "with no flag and no reachable operator, nothing may dispatch: {out}"
        );
    }

    /// `--read-only` outranks the parity default.
    #[tokio::test]
    async fn read_only_outranks_the_elicitation_default() {
        let s = Server::with_scope(true, false, WriteScope::None);
        s.mcp_read_only
            .store(true, std::sync::atomic::Ordering::Relaxed);
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            s.effective_scope(),
            WriteScope::None,
            "the operator said no to writes on this server; a client that supports \
             dialogs is not their permission to change that"
        );
        assert!(
            !tool_table(&s.effective_scope(), true)
                .as_array()
                .expect("array")
                .iter()
                .any(|t| t["name"] == "confirm_action"),
            "and the write surface must not be advertised"
        );
    }

    /// The flag parses, refuses contradictions, and refuses repeats.
    #[test]
    fn read_only_is_parsed_and_cannot_contradict_a_grant() {
        let args = |v: &[&str]| -> Vec<String> {
            std::iter::once("mcp".to_string())
                .chain(std::iter::once("serve".to_string()))
                .chain(v.iter().map(|s| (*s).to_string()))
                .collect()
        };
        let ok = parse_mcp_args(&args(&["--read-only"])).expect("valid");
        assert_eq!(ok.write_scope, WriteScope::None);

        let err = parse_mcp_args(&args(&["--read-only", "--allow-writes"]))
            .expect_err("contradiction must be refused");
        assert!(
            err.contains("contradict"),
            "picking a winner silently hands the operator a posture they did not \
             choose, either way: {err}"
        );
        assert!(parse_mcp_args(&args(&["--allow-writes", "--read-only"])).is_err());
        assert!(parse_mcp_args(&args(&["--read-only", "--read-only"])).is_err());
    }

    /// A silence is not a refusal, and must not be answered as one.
    ///
    /// Both outcomes ended with the decline guard verbatim, which for
    /// a timeout names the operator as the only way forward — the very
    /// party who was not there. A peer session hit this live and had
    /// to invent the redirect itself.
    #[test]
    fn a_timeout_and_a_decline_tell_the_agent_different_things() {
        let declined = AskOutcome::Declined.guidance();
        let silent = AskOutcome::Unanswered.guidance();
        assert_ne!(declined, silent, "the two must not share wording");

        assert!(
            declined.contains("unless the operator asks for it"),
            "a decline keeps the flat prohibition with one named exception — a \
             rationale-shaped guard does not catch an agent under \
             task-completion pressure, which is a different pull from \
             permission-seeking: {declined}"
        );

        assert!(
            silent.contains("NOT a refusal"),
            "a silence must not be reported as a refusal — nobody refused: {silent}"
        );
        assert!(
            silent.contains("Tell them"),
            "and must name a legitimate next act, or the agent's only options are \
             invent one or go quiet: {silent}"
        );
        assert!(
            !silent.contains("unless the operator asks for it"),
            "naming the absent party as the only unblock leaves no move at all: {silent}"
        );

        // The two that never reach an agent say nothing.
        assert!(AskOutcome::Approved.guidance().is_empty());
        assert!(AskOutcome::NotAsked.guidance().is_empty());
    }

    /// An agent can tell a standing grant from a human gate.
    ///
    /// `doctor` reported `elicitation: true` and `writes: every verb`
    /// as adjacent facts, and a peer session confirmed it could not
    /// relate them. The two imply opposite things to tell a user: a
    /// flag means "I can do this"; elicitation means "I can propose
    /// this, and someone must approve it". The same string for both
    /// over-promises in one of them.
    #[tokio::test]
    async fn doctor_says_why_writes_are_available_not_just_how_wide() {
        async fn doctor(s: &Server) -> String {
            call(s, "doctor", json!({})).await.1.to_string()
        }

        // Granted by the flag: a standing grant.
        let flagged = Server::with_scope(true, false, WriteScope::All);
        let d = doctor(&flagged).await;
        assert!(d.contains("--allow-writes"), "{d}");
        assert!(
            !d.contains("client-elicitation"),
            "a flag-granted surface must not claim every write is put to a person — \
             on a client that cannot be asked, nobody is: {d}"
        );

        // Granted by the parity default: every write is asked.
        let asked = demo_server();
        asked
            .client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let d = doctor(&asked).await;
        assert!(
            d.contains("client-elicitation") && d.contains("may decline"),
            "an elicitation-gated surface must say so, or the agent tells its user \
             it can act when it can only propose: {d}"
        );

        // An explicit flag outranks the default even when both are true.
        let both = Server::with_scope(true, false, WriteScope::All);
        both.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(
            doctor(&both).await.contains("--allow-writes"),
            "the flag is the provenance when it was given"
        );

        // Read-only says neither.
        let ro = demo_server();
        let d = doctor(&ro).await;
        assert!(d.contains("no writes are available"), "{d}");
    }

    /// The plan tells the agent a person is asked.
    ///
    /// `next` read as a mechanical step. An agent that never timed out
    /// would model confirm_action as a formality and report a decline
    /// to its user as an error rather than as someone's decision.
    #[tokio::test]
    async fn the_plan_says_a_person_will_be_asked() {
        let s = demo_server();
        s.client_supports_elicitation
            .store(true, std::sync::atomic::Ordering::Relaxed);
        let env = demo_fixture::envs()[0].name.clone();
        let (_, plan) = call(&s, "restart", json!({"env": &env})).await;
        let next = plan["next"].as_str().expect("a next hint");
        assert!(
            next.contains("OPERATOR") && next.contains("decline"),
            "the plan must say the confirm is a request put to a person: {next}"
        );

        // And must NOT say it where nobody will be asked.
        let flagged = Server::with_scope(true, false, WriteScope::All);
        let (_, plan) = call(&flagged, "restart", json!({"env": env})).await;
        let next = plan["next"].as_str().expect("a next hint");
        assert!(
            !next.contains("OPERATOR"),
            "on a client that cannot be asked, promising an operator dialog is a \
             lie the agent would relay to its user: {next}"
        );
    }
}
