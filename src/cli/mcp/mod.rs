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

#[derive(Debug, PartialEq, Eq)]
struct McpArgs {
    demo: bool,
    no_redact: bool,
    write_scope: WriteScope,
}

const MCP_USAGE: &str =
    "usage: ebman mcp <serve [--demo] [--no-redact] [--allow-writes[=verb,verb]] \
     | setup [--allow-writes[=verb,verb]]>";

fn parse_mcp_args(args: &[String]) -> Result<McpArgs, String> {
    // args[0] = "mcp"; the only sub-verb is "serve".
    if args.get(1).map(String::as_str) != Some("serve") {
        return Err(MCP_USAGE.into());
    }
    let mut demo = false;
    let mut no_redact = false;
    let mut write_scope = WriteScope::None;
    let mut saw_write_flag = false;
    let known: Vec<String> = writes::write_verb_names();
    let known_refs: Vec<&str> = known.iter().map(String::as_str).collect();
    for arg in args.iter().skip(2) {
        // `--allow-writes` alone still means every verb, so an existing
        // `.mcp.json` keeps working. `--allow-writes=a,b` narrows it.
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
    Ok(McpArgs {
        demo,
        no_redact,
        write_scope,
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
    fn agent_summary(&self) -> String {
        match self {
            WriteScope::None => "This server is READ-ONLY: no write tool is available. Ask the \
                 operator to restart it with --allow-writes (optionally \
                 --allow-writes=verb,verb to grant only what you need)."
                .to_string(),
            WriteScope::All => "Writes are ENABLED for every verb, via the two-phase \
                 plan-then-confirm protocol."
                .to_string(),
            WriteScope::Only(v) => format!(
                "Writes are NARROWLY granted: {} only, via the two-phase plan-then-confirm \
                 protocol. Any other write verb is absent from this list because it was NOT \
                 GRANTED, not because ebman lacks it — say so and ask the operator to widen \
                 the grant rather than reporting it as unsupported.",
                v.join(", ")
            ),
        }
    }
}

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
         This server keeps the old one until the connection is re-established \
         — reconnect (in Claude Code: /mcp, Reconnect) to pick it up. \
         Results below are from the running build.]",
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
            dispatching: std::sync::atomic::AtomicBool::new(false),
            client_name: std::sync::Mutex::new("unknown".to_string()),
            client_supports_elicitation: std::sync::atomic::AtomicBool::new(false),
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
                            "plus two-phase writes when the server was started with --allow-writes.\n\n"),
                            self.write_scope.agent_summary(),
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
                            "worth reading: a clean result from a tool does not clear what that tool never checked."
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
                "result": {"tools": tool_table(&self.write_scope, self.safety_cfg.mcp_peek_bodies)}
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
                let advertised = tool_table(&self.write_scope, self.safety_cfg.mcp_peek_bodies)
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
                let outcome = tokio::time::timeout(
                    std::time::Duration::from_secs(TOOL_TIMEOUT_SECS),
                    self.call_tool(&name, &args),
                )
                .await
                .unwrap_or_else(|_| {
                    Err(format!(
                        "tool '{name}' timed out after {TOOL_TIMEOUT_SECS}s"
                    ))
                });
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
    // Frame-level tools/call concurrency cap (see the spawn site).
    let tool_slots = Arc::new(tokio::sync::Semaphore::new(16));

    // Single writer task: concurrent tool tasks send completed frames
    // through the channel so stdout writes can't interleave. Bounded:
    // a client that writes requests but stops reading stdout must
    // apply backpressure (senders park at `send().await`), not grow
    // an unbounded queue of completed frames.
    let (out_tx, mut out_rx) = tokio::sync::mpsc::channel::<String>(256);
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
    drop(out_tx);
    let _ = writer.await;
    if write_scope.any() {
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
                err.contains("no longer in the dead-letter queue"),
                "it must say the planned message is gone, not delete m-2 \
                 quietly: {err}"
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
}
