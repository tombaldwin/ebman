//! `ebman action <verb> ...` — write-path subcommands. Three
//! shapes:
//!
//! - Single-env, instant-dispatch: rebuild / restart / terminate.
//!   `ebman action rebuild --env NAME [--yes]`.
//! - Single-env, polling: `ebman action deploy --env NAME --version
//!   LABEL [--wait-for-green Nm] [--auto-rollback Nm]`. Reuses the
//!   `decide_poll` state machine.
//! - Cross-region, fan-out: `ebman action rollout --version LABEL
//!   --regions r1,r2,r3 --env NAME --yes [--wait-for-green Nm]`.
//!   Pre-flight + sequential dispatch, halt on first failure,
//!   single `rollout_id` correlation across audit lines.

use color_eyre::eyre::Result;

use crate::audit;
use crate::aws;
use crate::cli::{cli_esc, decide_poll, PollDecision};

/// Parsed single-env `ebman action` invocation (rebuild / restart /
/// terminate / deploy — NOT rollout, which has its own parser). The
/// `action` verb is carried verbatim; whether it's a *known* verb is
/// decided later in [`run`]'s dispatch match, matching the original
/// ordering (unknown verbs reach the AWS client first).
#[derive(Debug, PartialEq, Eq)]
struct ActionArgs {
    action: String,
    env: String,
    /// Where the action goes, with what it needs: a deploy carries its
    /// validated flags, so there is no deploy without them to handle.
    route: Route,
    yes: bool,
}

/// A known action, resolved by the parser.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    Verb(CliVerb),
    Deploy(DeployFlags),
}

impl Route {
    fn audit_label(&self) -> &'static str {
        match self {
            Route::Verb(v) => CliAction::Verb(*v).audit_label(),
            Route::Deploy(_) => CliAction::Deploy.audit_label(),
        }
    }
}

/// A usage/gate failure carrying the exact exit code the CLI charter
/// assigns it — `2` for usage errors, `3` for the destructive-without-
/// `--yes` gate. Pulling this out of [`run`] lets the gate logic be
/// unit-tested without `std::process::exit` killing the test process.
#[derive(Debug, PartialEq, Eq)]
struct ActionArgError {
    msg: String,
    code: i32,
}

/// `take_value` adapted to the action parser's typed error (exit 2).
fn take_flag_value<'a, I: Iterator<Item = &'a String>>(
    iter: &mut I,
    flag: &str,
    what: &str,
) -> Result<String, ActionArgError> {
    crate::cli::take_value(iter, "ebman action", flag, what)
        .map_err(|msg| ActionArgError { msg, code: 2 })
}

/// The three verbs `ebman action` dispatches through its shared path.
/// (`deploy` and `rollout` have their own, richer paths and are
/// parsed separately.)
///
/// One row per verb rather than two parallel `match action_name`
/// blocks — the previous shape mapped the name to an audit label in
/// one place and to an AWS method in another, and the compiler checked
/// neither against the other. Adding a verb to one and forgetting the
/// other was a silent audit gap: dispatched under one name, completed
/// under another, or `unreachable!()` in a release binary.
/// What `ebman action <name>` routes to, once the name is known good.
///
/// `deploy` takes different arguments and has its own function, so it
/// is a routing variant rather than a `CliVerb`. Parsed BEFORE the
/// safety gates and the AWS client: a malformed command is malformed
/// whether or not the fleet is frozen, and building a client first
/// meant `ebman action nonsense` with no credentials reported a
/// credential failure instead of a usage error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliAction {
    Verb(CliVerb),
    Deploy,
}

impl CliAction {
    /// The label the DISPATCH would log, so a refusal and a dispatch of
    /// the same verb correlate under `ebman audit --action X`.
    /// `AuditFilter` matches exactly, so a refusal logged under the CLI
    /// token (`restart`) is invisible to a filter that finds the
    /// dispatch (`RestartAppServer`).
    fn audit_label(self) -> &'static str {
        match self {
            // Matches the `append_action_dispatched` sites in `deploy`.
            CliAction::Deploy => crate::verb::Verb::Deploy.audit_label(),
            CliAction::Verb(v) => v.audit_label(),
        }
    }

    fn parse(name: &str) -> Option<Self> {
        match name {
            "deploy" => Some(CliAction::Deploy),
            other => CliVerb::parse(other).map(CliAction::Verb),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CliVerb {
    Rebuild,
    Restart,
    Terminate,
}

impl CliVerb {
    /// Every variant, so the parser and the tests can't disagree about
    /// what exists.
    const ALL: &'static [(&'static str, CliVerb, crate::verb::Verb)] = &[
        ("rebuild", CliVerb::Rebuild, crate::verb::Verb::Rebuild),
        // "RestartAppServer", not "Restart": the TUI audits every
        // action under its Debug name, so a CLI-written "Restart"
        // split the same operation into two names in one log —
        // `ebman audit --action` matched half the history either way.
        (
            "restart",
            CliVerb::Restart,
            crate::verb::Verb::RestartAppServer,
        ),
        (
            "terminate",
            CliVerb::Terminate,
            crate::verb::Verb::Terminate,
        ),
    ];

    fn parse(name: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(n, ..)| *n == name)
            .map(|(_, v, _)| *v)
    }

    /// The `action=` field in the audit pair. Must match the TUI's
    /// `Action::label()` for the same verb so `ebman audit --action`
    /// correlates across surfaces.
    fn audit_label(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(_, v, _)| *v == self)
            .map(|(_, _, shared)| shared.audit_label())
            .unwrap_or("Unknown")
    }

    async fn dispatch(
        self,
        aws: &aws::AwsClient,
        env: &str,
    ) -> Result<(), color_eyre::eyre::Report> {
        match self {
            CliVerb::Rebuild => aws.rebuild_env(env).await,
            CliVerb::Restart => aws.restart_app_server(env).await,
            CliVerb::Terminate => aws.terminate_env(env).await,
        }
    }
}

const ACTION_USAGE: &str = "usage: ebman action <rebuild|restart|terminate|deploy|rollout> --env NAME [--version LABEL] [--regions r1,r2,r3] [--yes] [--wait-for-green Nm] [--auto-rollback Nm]";

/// Pure parser for the single-env action verbs. Mirrors the original
/// inline logic exactly: empty/dash-prefixed verb → usage error;
/// unknown flag → usage error; missing `--env` → usage error;
/// `terminate` without `--yes` → destructive gate (exit 3). `rollout`
/// is dispatched before this in [`run`] and never reaches here.
fn parse_action_args(args: &[String]) -> Result<ActionArgs, ActionArgError> {
    let action_name = args.get(1).map(|s| s.as_str()).unwrap_or("");
    if action_name.is_empty() || action_name.starts_with('-') {
        return Err(ActionArgError {
            msg: ACTION_USAGE.into(),
            code: 2,
        });
    }
    let mut env_name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut wait_for_green: Option<String> = None;
    let mut auto_rollback: Option<String> = None;
    let mut yes = false;
    let mut iter = args.iter().skip(2);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = Some(take_flag_value(&mut iter, "--env", "an env name")?),
            "--version" => {
                version = Some(take_flag_value(&mut iter, "--version", "a version label")?)
            }
            "--wait-for-green" => {
                wait_for_green = Some(take_flag_value(
                    &mut iter,
                    "--wait-for-green",
                    "a duration",
                )?)
            }
            "--auto-rollback" => {
                auto_rollback = Some(take_flag_value(&mut iter, "--auto-rollback", "a duration")?)
            }
            "--yes" => yes = true,
            other => {
                return Err(ActionArgError {
                    msg: format!("ebman action: unknown flag '{other}'"),
                    code: 2,
                })
            }
        }
    }
    let Some(env) = env_name else {
        return Err(ActionArgError {
            msg: "ebman action: --env NAME is required".into(),
            code: 2,
        });
    };
    // A deploy's own usage errors are decided here, with the rest, so
    // they beat the gate: a malformed deploy on a frozen fleet is a
    // usage error (2), not a refusal (3) with an audit line.
    let route = match CliAction::parse(action_name) {
        None => {
            return Err(ActionArgError {
                msg: format!("ebman action: unknown action '{action_name}'\n{ACTION_USAGE}"),
                code: 2,
            })
        }
        Some(CliAction::Verb(v)) => Route::Verb(v),
        Some(CliAction::Deploy) => {
            let usage = |msg: String| ActionArgError { msg, code: 2 };
            let version = version
                .ok_or_else(|| usage("ebman action deploy: --version LABEL is required".into()))?;
            let secs = |flag: &str, value: Option<String>| match value {
                Some(s) => aws::parse_window_ms(&s)
                    .map(|ms| Some((ms / 1000) as u64))
                    .ok_or_else(|| {
                        usage(format!(
                        "ebman action deploy: {flag} expects a duration like `5m` / `30m` / `1h`"
                    ))
                    }),
                None => Ok(None),
            };
            Route::Deploy(DeployFlags {
                version,
                wait_for_green_secs: secs("--wait-for-green", wait_for_green)?,
                auto_rollback_secs: secs("--auto-rollback", auto_rollback)?,
            })
        }
    };
    let destructive = matches!(action_name, "terminate");
    if destructive && !yes {
        return Err(ActionArgError {
            msg: format!(
                "ebman action: '{action_name}' is destructive; re-run with --yes to confirm"
            ),
            code: 3,
        });
    }
    Ok(ActionArgs {
        action: action_name.to_string(),
        env,
        route,
        yes,
    })
}

/// `ebman action` — dispatch one write, or a `rollout` across many.
///
/// Every dispatch goes through the same pins, freeze and read-only
/// checks as the TUI. Exit 2 on a usage error, 1 on a failed
/// dispatch.
pub async fn run(args: &[String]) -> Result<()> {
    let action_name = args.get(1).map(|s| s.as_str()).unwrap_or("");
    if action_name == "rollout" {
        return run_rollout(args).await;
    }
    let code = action_body(
        args,
        crate::cli::WriteEnv::real(),
        aws::AwsClient::with,
        &mut std::io::stdout().lock(),
        &mut std::io::stderr().lock(),
    )
    .await;
    // Every exit drains: a refusal's or a dispatch's audit line may
    // have a webhook POST in flight.
    if code != 0 {
        crate::cli::exit_after_drain(code).await;
    }
    crate::cli::drain_before_return().await;
    Ok(())
}

/// `ebman action <verb>`'s whole behaviour (not `rollout`) against any
/// safety decision, client factory (`profile, region`) and writers.
/// Returns the exit code. `ambient_profile` is `AWS_PROFILE`, which the
/// dispatch runs under and the pins are checked against.
pub(crate) async fn action_body<F, Fut>(
    args: &[String],
    write_env: crate::cli::WriteEnv<impl crate::cli::WriteGate>,
    client_for: F,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32
where
    F: FnOnce(Option<String>, Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<aws::AwsClient>>,
{
    let ActionArgs {
        action: action_name,
        env,
        route,
        ..
    } = match parse_action_args(args) {
        Ok(parsed) => parsed,
        Err(ActionArgError { msg, code }) => {
            let _ = writeln!(err, "{msg}");
            return code;
        }
    };
    let action_name = action_name.as_str();
    // Freeze BEFORE pin, matching the MCP write gate (inside the
    // refusal). The audit vocabulary, not the CLI token: a refusal
    // logging `action=restart` while the dispatch logs
    // `action=RestartAppServer` means `ebman audit --action
    // RestartAppServer` shows the writes that happened and hides the
    // ones that were stopped — which is the correlation this feature
    // exists for. No region: the gate runs before any AWS client is
    // built, and the rules it applies are region-independent.
    let profile = write_env.ambient_profile.as_deref();
    if let Some(reason) = write_env.refusal(&crate::cli::GateQuery {
        env: &env,
        profile,
        region: None,
        verb: route.audit_label(),
    }) {
        let _ = writeln!(err, "ebman action: {reason}");
        return 3;
    }
    let aws = match client_for(None, None).await {
        Ok(c) => c,
        Err(e) => {
            let _ = writeln!(err, "ebman action: {e}");
            return 1;
        }
    };

    let verb = match route {
        Route::Deploy(flags) => return deploy(&aws, &env, flags, profile, out, err).await,
        Route::Verb(v) => v,
    };
    let label = verb.audit_label();
    // Mirror the TUI's dispatched/completed audit pair — headless
    // dispatches were previously invisible to the audit log (and the
    // webhook fan-out), contradicting the safety docs.
    audit::append_action_dispatched(None, profile, &aws.context.region, label, &env, &[]);
    let result = verb.dispatch(&aws, &env).await;
    let err_text = result.as_ref().err().map(|e| e.to_string());
    audit::append_action_completed(
        None,
        profile,
        &aws.context.region,
        label,
        &env,
        match err_text.as_deref() {
            None => Ok(()),
            Some(e) => Err(e),
        },
        &[],
    );
    match err_text {
        None => {
            let _ = writeln!(out, "ok: {action_name} on {env} dispatched");
            0
        }
        Some(e) => {
            let _ = writeln!(err, "err: {e}");
            1
        }
    }
}

/// `ebman action deploy`'s flags, parsed and validated.
#[derive(Debug, PartialEq, Eq)]
struct DeployFlags {
    version: String,
    wait_for_green_secs: Option<u64>,
    auto_rollback_secs: Option<u64>,
}

/// `ebman action deploy --env X --version Y [--wait-for-green Nm]
/// [--auto-rollback Nm]` — non-interactive CLI parity with the
/// typed-command `:deploy` path. Exit codes:
///   0  — deploy dispatched (and reached Green if asked)
///   1  — AWS-layer error
///   2  — usage error
///   4  — `--wait-for-green` deadline elapsed without Green
///   5  — `--auto-rollback` deadline elapsed; rollback dispatched
async fn deploy(
    aws: &aws::AwsClient,
    env: &str,
    flags: DeployFlags,
    profile: Option<&str>,
    out: &mut dyn std::io::Write,
    err: &mut dyn std::io::Write,
) -> i32 {
    let DeployFlags {
        version,
        wait_for_green_secs,
        auto_rollback_secs,
    } = flags;

    let envs = match aws.list_environments().await {
        Ok(envs) => envs,
        Err(e) => {
            let _ = writeln!(err, "ebman action deploy: list_environments: {e}");
            return 1;
        }
    };
    let snapshot = envs
        .iter()
        .find(|e| e.name == env)
        .map(|e| e.version_label.clone());
    let Some(snapshot_label) = snapshot else {
        let _ = writeln!(err, "ebman action deploy: env '{env}' not found");
        return 2;
    };
    if auto_rollback_impossible(auto_rollback_secs, &snapshot_label) {
        let _ = writeln!(
            err,
            "ebman action deploy: --auto-rollback requested but env '{env}' has no prior version to roll back to"
        );
        return 2;
    }

    let _ = writeln!(out, "dispatching deploy: env={env} version={version}");
    let region = aws.context.region.as_str();
    let label = crate::verb::Verb::Deploy.audit_label();
    audit::append_action_dispatched(None, profile, region, label, env, &[("version", &version)]);
    if let Err(e) = aws.deploy_version(env, &version).await {
        let msg = format!("deploy_version: {e}");
        audit::append_action_completed(
            None,
            profile,
            region,
            label,
            env,
            Err(&msg),
            &[("version", &version)],
        );
        let _ = writeln!(err, "err: {msg}");
        return 1;
    }
    audit::append_action_completed(
        None,
        profile,
        region,
        label,
        env,
        Ok(()),
        &[("version", &version)],
    );

    if !deploy_needs_watching(wait_for_green_secs, auto_rollback_secs) {
        let _ = writeln!(out, "ok: deploy on {env} dispatched (version={version})");
        return 0;
    }

    let start = tokio::time::Instant::now();
    let poll_interval = std::time::Duration::from_secs(5);
    let mut wait_for_green_timeout_emitted = false;
    let _ = writeln!(
        out,
        "polling {env} every {}s for Green{}{}",
        poll_interval.as_secs(),
        wait_for_green_secs
            .map(|s| format!(", wait-for-green={s}s"))
            .unwrap_or_default(),
        auto_rollback_secs
            .map(|s| format!(", auto-rollback={s}s"))
            .unwrap_or_default(),
    );
    loop {
        tokio::time::sleep(poll_interval).await;
        let envs = match aws.list_environments().await {
            Ok(envs) => envs,
            Err(e) => {
                let _ = writeln!(err, "err: list_environments during poll: {e}");
                return 1;
            }
        };
        let (status, health) = envs
            .iter()
            .find(|e| e.name == env)
            .map(|e| (e.status.clone(), e.health.clone()))
            .unwrap_or_default();
        let elapsed = start.elapsed().as_secs();
        match decide_poll(
            &status,
            &health,
            elapsed,
            wait_for_green_secs,
            auto_rollback_secs,
            wait_for_green_timeout_emitted,
        ) {
            PollDecision::KeepPolling => {
                let _ = writeln!(out, "poll t={elapsed}s status={status} health={health}");
            }
            PollDecision::Success => {
                let _ = writeln!(
                    out,
                    "ok: deploy on {env} reached Green at t={elapsed}s (version={version})"
                );
                return 0;
            }
            PollDecision::WaitForGreenTimeout => {
                wait_for_green_timeout_emitted = true;
                if auto_rollback_secs.is_none() {
                    let _ = writeln!(
                        err,
                        "timeout: deploy on {env} did not reach Green within {}s (status={status}, health={health}, version={version})",
                        wait_for_green_secs.unwrap_or(0)
                    );
                    return 4;
                }
                let remaining = auto_rollback_secs.unwrap_or(0).saturating_sub(elapsed);
                let _ = writeln!(
                    out,
                    "wait-for-green timeout at t={elapsed}s (status={status}, health={health}); continuing under auto-rollback ({remaining}s remaining)"
                );
            }
            PollDecision::DispatchRollback => {
                let _ = writeln!(
                    err,
                    "auto-rollback firing on {env}: env still status={status} health={health} at t={elapsed}s; redeploying snapshot version={snapshot_label}"
                );
                audit::append_action_dispatched(
                    None,
                    profile,
                    region,
                    label,
                    env,
                    &[("version", &snapshot_label), ("auto_rollback_of", &version)],
                );
                let rolled_back = aws.deploy_version(env, &snapshot_label).await;
                // The pair, like every other dispatch: a `dispatched`
                // with no `completed` reads as a write that died mid-way.
                let failure = rolled_back
                    .as_ref()
                    .err()
                    .map(|e| format!("deploy_version: {e}"));
                audit::append_action_completed(
                    None,
                    profile,
                    region,
                    label,
                    env,
                    match failure.as_deref() {
                        None => Ok(()),
                        Some(e) => Err(e),
                    },
                    &[("version", &snapshot_label), ("auto_rollback_of", &version)],
                );
                if let Some(e) = failure {
                    let _ = writeln!(err, "err: rollback {e}");
                    return 1;
                }
                let _ = writeln!(
                    out,
                    "ok: rollback dispatched on {env} (version={snapshot_label})"
                );
                return 5;
            }
        }
    }
}

/// Pure: does this deploy need watching after dispatch?
///
/// `--wait-for-green` and `--auto-rollback` are independent opt-ins and
/// either one means we stay and poll. Extracted from `run_deploy` so the
/// condition can be tested — it lives inside an async fn that needs AWS,
/// and `cargo mutants` found the `&&` survivable. Flipped to `||`, a
/// deploy with `--auto-rollback 5m` returns immediately and never arms
/// the watchdog: the operator asked for a safety net and silently did
/// not get one.
fn deploy_needs_watching(
    wait_for_green_secs: Option<u64>,
    auto_rollback_secs: Option<u64>,
) -> bool {
    wait_for_green_secs.is_some() || auto_rollback_secs.is_some()
}

/// Pure: `--auto-rollback` needs a prior version to roll back TO.
///
/// An env deployed for the first time has an empty current-version
/// label, so there is nothing to restore. Refusing up front beats arming
/// a watchdog that can only fail. Same extraction rationale as
/// [`deploy_needs_watching`].
fn auto_rollback_impossible(auto_rollback_secs: Option<u64>, snapshot_label: &str) -> bool {
    auto_rollback_secs.is_some() && snapshot_label.is_empty()
}

/// Where a rollout's per-region progress goes: stderr in production,
/// shared by the tasks of a parallel rollout, which a borrowed writer
/// cannot reach.
pub(crate) type Progress = std::sync::Arc<dyn Fn(&str) + Send + Sync>;

/// What every region of one rollout dispatches: shared by its tasks.
struct Wave {
    env: String,
    version: String,
    wait_for_green_secs: Option<u64>,
    rollout_id: String,
    quiet: bool,
    progress: Progress,
}

/// Per-region dispatch helper shared by sequential + parallel paths.
/// Calls `deploy_version`; optionally polls until Green (or the
/// `--wait-for-green` deadline elapses). Emits the per-region
/// `stage=dispatched` and `stage=completed` audit-log lines. Returns
/// `Ok(())` on Green (or just dispatched if no wait); `Err(msg)`
/// when dispatch fails or the deadline elapses without Green.
async fn dispatch_one_region(
    client: &aws::AwsClient,
    region: &str,
    w: &Wave,
) -> Result<(), String> {
    let Wave {
        env,
        version,
        rollout_id,
        quiet,
        progress,
        ..
    } = w;
    if !quiet {
        progress(&format!(
            "rollout: dispatching to {region} (env={env}, version={version})"
        ));
    }
    audit::append_rollout(
        rollout_id,
        // From the CLIENT, not the `--profile` flag: this records the
        // profile the write actually went through, which is the question
        // the audit log is asked.
        client.context.profile.as_deref(),
        region,
        env,
        version,
        "dispatched",
        None,
    );
    let mut outcome: Result<(), String> = match client.deploy_version(env, version).await {
        Ok(()) => Ok(()),
        Err(e) => {
            let msg = format!("deploy_version: {e}");
            progress(&format!("rollout[{region}]: {msg}"));
            Err(msg)
        }
    };
    if outcome.is_ok() {
        if let Some(secs) = w.wait_for_green_secs {
            let start = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let envs = match client.list_environments().await {
                    Ok(envs) => envs,
                    Err(e) => {
                        progress(&format!(
                            "rollout[{region}]: list_environments during poll: {e}"
                        ));
                        outcome = Err(format!("poll: {e}"));
                        break;
                    }
                };
                let (status, health) = envs
                    .iter()
                    .find(|e| &e.name == env)
                    .map(|e| (e.status.clone(), e.health.clone()))
                    .unwrap_or_default();
                let elapsed = start.elapsed().as_secs();
                // `wait_for_green_timeout_emitted = false` is hard-
                // coded: rollout's WaitForGreenTimeout arm breaks
                // immediately (no per-tick suppression needed). A
                // future change wiring `--auto-rollback` per region
                // will need to thread the flag back in.
                match decide_poll(&status, &health, elapsed, Some(secs), None, false) {
                    PollDecision::KeepPolling => {
                        if !quiet {
                            progress(&format!(
                                "rollout[{region}]: t={elapsed}s status={status} health={health}"
                            ));
                        }
                    }
                    PollDecision::Success => {
                        if !quiet {
                            progress(&format!("rollout[{region}]: reached Green at t={elapsed}s"));
                        }
                        break;
                    }
                    PollDecision::WaitForGreenTimeout => {
                        let msg = format!(
                            "did not reach Green within {secs}s (status={status}, health={health})"
                        );
                        progress(&format!("rollout[{region}]: {msg}"));
                        outcome = Err(msg);
                        break;
                    }
                    PollDecision::DispatchRollback => break,
                }
            }
        }
    }
    audit::append_rollout(
        rollout_id,
        client.context.profile.as_deref(),
        region,
        env,
        version,
        "completed",
        outcome.as_ref().err().map(String::as_str),
    );
    outcome
}

/// `ebman action rollout`'s flags, parsed and validated.
#[derive(Debug)]
struct RolloutArgs {
    env: String,
    version: String,
    /// Deduplicated, in the operator's order.
    regions: Vec<String>,
    profile: Option<String>,
    wait_for_green_secs: Option<u64>,
    staggered_secs: Option<u64>,
    yes: bool,
    json: bool,
    quiet: bool,
    parallel: bool,
    max_concurrency: Option<usize>,
    /// Effective: `--parallel` implies it.
    continue_on_fail: bool,
}

/// Pure parser for `ebman action rollout`: every usage error (exit 2)
/// is decided here, before the pins are consulted or a client is built.
fn parse_rollout_args(args: &[String]) -> Result<RolloutArgs, String> {
    const PROG: &str = "ebman action rollout";
    let take = |iter: &mut std::iter::Skip<std::slice::Iter<'_, String>>,
                flag: &str,
                what: &str| { crate::cli::take_value(iter, PROG, flag, what) };
    let mut env_name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut regions_csv: Option<String> = None;
    let mut wait_for_green: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut yes = false;
    let mut json = false;
    let mut quiet = false;
    let mut parallel = false;
    let mut max_concurrency: Option<usize> = None;
    let mut continue_on_fail = false;
    let mut staggered: Option<String> = None;
    let mut iter = args.iter().skip(2);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = Some(take(&mut iter, "--env", "an env name")?),
            "--version" => version = Some(take(&mut iter, "--version", "a version label")?),
            "--regions" => regions_csv = Some(take(&mut iter, "--regions", "a region list")?),
            "--wait-for-green" => {
                wait_for_green = Some(take(&mut iter, "--wait-for-green", "a duration")?)
            }
            "--profile" => profile = Some(take(&mut iter, "--profile", "a profile name")?),
            "--yes" => yes = true,
            "--json" => json = true,
            "--quiet" => quiet = true,
            "--parallel" => parallel = true,
            "--max-concurrency" => {
                let Some(v) = iter.next() else {
                    return Err(format!("{PROG}: --max-concurrency expects an integer"));
                };
                let Ok(n) = v.parse::<usize>() else {
                    return Err(format!(
                        "{PROG}: --max-concurrency expects an integer, got '{v}'"
                    ));
                };
                if n == 0 {
                    return Err(format!("{PROG}: --max-concurrency must be > 0"));
                }
                max_concurrency = Some(n);
            }
            "--continue-on-fail" => continue_on_fail = true,
            "--staggered" => staggered = Some(take(&mut iter, "--staggered", "a duration")?),
            other => return Err(format!("{PROG}: unknown flag '{other}'")),
        }
    }
    let env = env_name.ok_or(format!("{PROG}: --env NAME is required"))?;
    let version = version.ok_or(format!("{PROG}: --version LABEL is required"))?;
    let regions_csv = regions_csv.ok_or(format!(
        "{PROG}: --regions r1,r2,r3 is required (comma-separated, no spaces)"
    ))?;
    let mut regions: Vec<String> = crate::util::split_csv(&regions_csv);
    // Dedupe preserving order — `r1,r1` used to dispatch twice, the
    // second racing the first ("already updating") and marking the
    // rollout partial for a self-inflicted reason.
    let mut seen = std::collections::HashSet::new();
    regions.retain(|r| seen.insert(r.clone()));
    if regions.is_empty() {
        return Err(format!("{PROG}: --regions list is empty"));
    }
    let secs = |flag: &str, value: Option<String>| match value {
        Some(s) => aws::parse_window_ms(&s)
            .map(|ms| Some((ms / 1000) as u64))
            .ok_or(format!(
                "{PROG}: {flag} expects a duration like `5m` / `30m` / `1h`"
            )),
        None => Ok(None),
    };
    let wait_for_green_secs = secs("--wait-for-green", wait_for_green)?;
    let staggered_secs = secs("--staggered", staggered)?;
    let continue_on_fail = validate_rollout_flags(
        parallel,
        staggered_secs,
        max_concurrency,
        wait_for_green_secs,
        continue_on_fail,
    )?;
    Ok(RolloutArgs {
        env,
        version,
        regions,
        profile,
        wait_for_green_secs,
        staggered_secs,
        yes,
        json,
        quiet,
        parallel,
        max_concurrency,
        continue_on_fail,
    })
}

/// Validate the mutually-exclusive rollout flags, and resolve the
/// effective `continue_on_fail`.
///
/// Extracted from `run_rollout`, where each check sat inline against an
/// `eprintln!` + `std::process::exit(2)` — so none was reachable from a
/// test, and the 2026-08-26 sweep reported every one of the conjunctions
/// as survivable. A rollout is the widest-blast-radius thing this binary
/// does; which flag combinations it refuses is worth being able to
/// assert.
///
/// `Err` is the operator-facing message; the caller prints it and exits 2.
pub(crate) fn validate_rollout_flags(
    parallel: bool,
    staggered_secs: Option<u64>,
    max_concurrency: Option<usize>,
    wait_for_green_secs: Option<u64>,
    continue_on_fail: bool,
) -> Result<bool, &'static str> {
    if parallel && staggered_secs.is_some() {
        return Err(
            "ebman action rollout: --parallel and --staggered are mutually exclusive (--staggered requires sequential ordering)",
        );
    }
    if !parallel && max_concurrency.is_some() {
        return Err("ebman action rollout: --max-concurrency only applies with --parallel");
    }
    if staggered_secs.is_some() && wait_for_green_secs.is_none() {
        return Err(
            "ebman action rollout: --staggered requires --wait-for-green (staggering is timed from each region's Green observation)",
        );
    }
    // --parallel implies --continue-on-fail. In-flight regions can't be
    // cancelled server-side, so "halt remaining" only makes sense for
    // un-started waves under --max-concurrency. For v1 simplicity,
    // --parallel always attempts all regions.
    Ok(continue_on_fail || parallel)
}

/// Regions that never got a dispatch attempt, in the operator's original
/// order — reported as `skipped (rollout halted)`.
///
/// Written out twice in `run_rollout`, once for the JSON renderer and
/// once for the text one, and both copies carried the same survivor. A
/// region silently vanishing from a rollout report is the class of bug
/// 0.14.1 shipped a fix for.
pub(crate) fn unattempted_regions<'a>(
    regions: &'a [String],
    outcomes: &[(String, Result<(), String>)],
) -> Vec<&'a str> {
    let attempted: std::collections::HashSet<&str> =
        outcomes.iter().map(|(r, _)| r.as_str()).collect();
    regions
        .iter()
        .map(String::as_str)
        .filter(|r| !attempted.contains(r))
        .collect()
}

/// `ebman action rollout --version LABEL --regions r1,r2,r3 --env NAME
/// --yes [...]` — cross-region deploy with pre-flight + per-region
/// dispatch + audit-log correlation. Sequential by default (halt on
/// first failure); `--parallel` fans out concurrently with optional
/// `--max-concurrency N` cap; `--continue-on-fail` attempts every
/// region in sequential mode; `--staggered Nm` waits N minutes
/// between regions in sequential mode (canary-style rollouts).
///
/// Exit codes:
/// - 0 all regions dispatched successfully
/// - 1 AWS-layer error before any region dispatched
/// - 2 usage error (mutually-exclusive flags, missing required args)
/// - 3 refused, or one or more region dispatches failed or were halted
async fn run_rollout(args: &[String]) -> Result<()> {
    let code = rollout_body(
        args,
        crate::cli::WriteEnv::real(),
        aws::AwsClient::with,
        // Unlocked handles: a parallel rollout's tasks print progress
        // from other worker threads, which a lock held for the whole
        // run would block for good.
        RolloutIo {
            out: &mut std::io::stdout(),
            err: &mut std::io::stderr(),
            progress: std::sync::Arc::new(|line: &str| eprintln!("{line}")),
        },
    )
    .await;
    // Every exit drains: a refusal's or a region's audit line may have
    // a webhook POST in flight.
    if code != 0 {
        crate::cli::exit_after_drain(code).await;
    }
    crate::cli::drain_before_return().await;
    Ok(())
}

/// Where a rollout writes: its report, its own messages, and the
/// per-region progress its tasks emit.
pub(crate) struct RolloutIo<'a> {
    pub out: &'a mut dyn std::io::Write,
    pub err: &'a mut dyn std::io::Write,
    pub progress: Progress,
}

/// `ebman action rollout`'s whole behaviour against any safety
/// decision, per-region client factory (`profile, region`), live freeze
/// check (re-read between regions) and writers. Returns the exit code.
/// `ambient_profile` is `AWS_PROFILE`, which `--profile` overrides for
/// both the dispatch and the pins.
pub(crate) async fn rollout_body<F, Fut>(
    args: &[String],
    write_env: crate::cli::WriteEnv<impl crate::cli::WriteGate>,
    client_for: F,
    io: RolloutIo<'_>,
) -> i32
where
    F: Fn(Option<String>, Option<String>) -> Fut,
    Fut: std::future::Future<Output = Result<aws::AwsClient>>,
{
    let RolloutIo { out, err, progress } = io;
    let RolloutArgs {
        env,
        version,
        regions,
        profile,
        wait_for_green_secs,
        staggered_secs,
        yes,
        json,
        quiet,
        parallel,
        max_concurrency,
        continue_on_fail,
    } = match parse_rollout_args(args) {
        Ok(parsed) => parsed,
        Err(msg) => {
            let _ = writeln!(err, "{msg}");
            return 2;
        }
    };
    // Same pin gate as the single-env verbs — a rollout is a deploy
    // fan-out of one env name across regions. Checked for the profile
    // this rollout will actually dispatch under — `--profile` if given,
    // else ambient. Gating on the ambient one while dispatching under
    // another checks the pin on the wrong account. No region: the rules
    // it applies are region-independent.
    let pin_profile = profile.as_deref().or(write_env.ambient_profile.as_deref());
    if let Some(reason) = write_env.refusal(&crate::cli::GateQuery {
        env: &env,
        profile: pin_profile,
        region: None,
        verb: "Rollout",
    }) {
        let _ = writeln!(err, "ebman action rollout: {reason}");
        return 3;
    }

    if !quiet {
        let _ = writeln!(
            err,
            "rollout: pre-flighting {} region(s) for env '{env}' version '{version}'",
            regions.len()
        );
    }
    let mut per_region: Vec<(String, aws::AwsClient)> = Vec::with_capacity(regions.len());
    for region in &regions {
        let client = match client_for(profile.clone(), Some(region.clone())).await {
            Ok(c) => c,
            Err(e) => {
                let _ = writeln!(
                    err,
                    "ebman action rollout: failed to construct client for region '{region}': {e}"
                );
                return 1;
            }
        };
        let envs = match client.list_environments().await {
            Ok(envs) => envs,
            Err(e) => {
                let _ = writeln!(
                    err,
                    "ebman action rollout: list_environments in '{region}' failed: {e}"
                );
                return 1;
            }
        };
        if !envs.iter().any(|e| e.name == env) {
            let _ = writeln!(
                err,
                "ebman action rollout: env '{env}' not found in region '{region}' — rollout halted before dispatching"
            );
            return 2;
        }
        per_region.push((region.clone(), client));
    }

    // After the pre-flight on purpose: the operator learns an env is
    // missing from a region BEFORE being asked to confirm.
    if !yes {
        let _ = writeln!(
            err,
            "ebman action rollout: would dispatch to {} region(s); re-run with --yes to confirm",
            regions.len()
        );
        return 2;
    }

    let wave = std::sync::Arc::new(Wave {
        env,
        version,
        wait_for_green_secs,
        rollout_id: format!("rollout-{}", chrono::Utc::now().format("%Y%m%dT%H%M%SZ")),
        quiet,
        progress,
    });
    // Arc-wrap clients so both sequential and parallel paths can
    // share them. Each AwsClient holds Arc'd SDK clients internally
    // (cheap clone), but the outer struct isn't Clone — wrap once
    // here so the parallel path's task closures get a moved Arc.
    let per_region: Vec<(String, std::sync::Arc<aws::AwsClient>)> = per_region
        .into_iter()
        .map(|(r, c)| (r, std::sync::Arc::new(c)))
        .collect();

    let mut outcomes: Vec<(String, Result<(), String>)> = Vec::new();
    // A freeze halt has no failed region, so `any_failure` below stays
    // false and the run would exit 0 — reporting success to CI while
    // regions sit un-dispatched. Tracked separately.
    let mut halted_by_freeze = false;
    if parallel {
        // Parallel dispatch — one task per region, all started
        // immediately (or capped at `max_concurrency` if set).
        // tokio::JoinSet awaits completions in arbitrary order;
        // outcomes therefore aren't sorted by region order — the
        // output renderer sorts by the input `regions` order when
        // emitting.
        if !quiet {
            let _ = writeln!(
                err,
                "rollout: dispatching {} region(s) in parallel{}",
                regions.len(),
                max_concurrency
                    .map(|n| format!(" (max-concurrency={n})"))
                    .unwrap_or_default(),
            );
        }
        let mut joinset: tokio::task::JoinSet<(String, Result<(), String>)> =
            tokio::task::JoinSet::new();
        // Tracks the region each spawned task was launched against, keyed
        // by JoinSet task id. When a JoinHandle fails (panic / cancellation)
        // we no longer get the region from inside the closure, so we look
        // it up here. Without this, `outcomes.push((String::new(), Err))`
        // would write an empty region key, which then matches no entry in
        // the `regions` HashSet at the bottom of this fn — so the real
        // region would be misreported as "skipped (rollout halted)".
        let mut id_to_region: std::collections::HashMap<tokio::task::Id, String> =
            std::collections::HashMap::new();
        let cap = max_concurrency.unwrap_or(per_region.len()).max(1);
        let mut queue: std::collections::VecDeque<(String, std::sync::Arc<aws::AwsClient>)> =
            per_region.into_iter().collect();
        // A freeze declared during the pre-flight stops the first wave,
        // as the sequential loop's check before its first region does.
        // Only the check after each join existed, so a freeze that
        // landed while regions were being listed reached all of them.
        if let Some(reason) = write_env.freeze_halt() {
            let _ = writeln!(err, "ebman action rollout: halting — {reason}");
            queue.clear();
            halted_by_freeze = true;
        }
        // Seed initial batch.
        for _ in 0..cap.min(queue.len()) {
            if let Some(next) = queue.pop_front() {
                spawn_region(&mut joinset, &mut id_to_region, &wave, next);
            }
        }
        // Drain + reseed as capacity frees up. `join_next_with_id` lets us
        // attribute join failures (panic/cancel) back to a region.
        while let Some(joined) = joinset.join_next_with_id().await {
            let (region, outcome) = match joined {
                Ok((id, (r, outcome))) => {
                    id_to_region.remove(&id);
                    (r, outcome)
                }
                Err(e) => {
                    let region = id_to_region.remove(&e.id()).unwrap_or_default();
                    (region, Err(format!("join: {e}")))
                }
            };
            outcomes.push((region, outcome));
            // A freeze declared mid-rollout stops un-started waves.
            // Regions already in flight cannot be cancelled server-side,
            // so they run to completion and report normally; draining
            // the queue leaves the rest to `unattempted_regions`, which
            // reports them as skipped.
            if !queue.is_empty() {
                if let Some(reason) = write_env.freeze_halt() {
                    let _ = writeln!(err, "ebman action rollout: halting — {reason}");
                    queue.clear();
                    halted_by_freeze = true;
                }
            }
            if let Some(next) = queue.pop_front() {
                spawn_region(&mut joinset, &mut id_to_region, &wave, next);
            }
        }
    } else {
        // Sequential dispatch — current shape, with --continue-on-fail
        // controlling whether a failed region halts subsequent ones
        // and --staggered controlling the inter-region delay.
        let mut first_region = true;
        for (region, client) in &per_region {
            if !first_region {
                if let Some(stagger) = staggered_secs {
                    if !quiet {
                        let _ = writeln!(err, "rollout: staggering {stagger}s before next region");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(stagger)).await;
                }
            }
            first_region = false;
            // A freeze declared since the last region halts the rest.
            if let Some(reason) = write_env.freeze_halt() {
                let _ = writeln!(err, "ebman action rollout: halting — {reason}");
                halted_by_freeze = true;
                break;
            }
            let outcome = dispatch_one_region(client, region, &wave).await;
            outcomes.push((region.clone(), outcome));
            if !continue_on_fail && matches!(outcomes.last(), Some((_, Err(_)))) {
                break;
            }
        }
    }

    // Re-sort outcomes by the input `regions` order so output is
    // deterministic regardless of dispatch mode. Sequential mode
    // already preserves order; --parallel populates outcomes via
    // JoinSet::join_next which yields in completion order. CI
    // consumers parsing the JSON output benefit from the ordering
    // contract.
    {
        let region_order: std::collections::HashMap<&str, usize> = regions
            .iter()
            .enumerate()
            .map(|(i, r)| (r.as_str(), i))
            .collect();
        outcomes
            .sort_by_key(|(region, _)| *region_order.get(region.as_str()).unwrap_or(&usize::MAX));
    }

    // The regions a freeze stopped were refused, and are audited so:
    // without a line the log showed the regions that ran and nothing
    // for the rest, and the webhook watching for stopped writes never
    // heard. A region a failure halted is not refused — no gate spoke.
    if halted_by_freeze {
        for region in unattempted_regions(&regions, &outcomes) {
            crate::cli::audit_frozen(
                pin_profile,
                region,
                "Rollout",
                &wave.env,
                &[("rollout_id", &wave.rollout_id)],
            );
        }
    }

    let any_failure = outcomes.iter().any(|(_, r)| r.is_err());
    if !quiet {
        let _ = writeln!(out, "{}", rollout_report(&wave, &regions, &outcomes, json));
    }

    // 3 covers both: regions failed, or regions were never attempted
    // because a freeze halted the run. Either way the rollout did not
    // do what was asked, and a CI gate must not read it as success.
    if any_failure || halted_by_freeze {
        return 3;
    }
    0
}

/// Start one region of a parallel rollout, remembering which region
/// the task is so a panicked or cancelled one is still reported by name.
fn spawn_region(
    joinset: &mut tokio::task::JoinSet<(String, Result<(), String>)>,
    id_to_region: &mut std::collections::HashMap<tokio::task::Id, String>,
    wave: &std::sync::Arc<Wave>,
    (region, client): (String, std::sync::Arc<aws::AwsClient>),
) {
    let wave = wave.clone();
    let inner = region.clone();
    let handle = joinset.spawn(async move {
        let outcome = dispatch_one_region(&client, &inner, &wave).await;
        (inner, outcome)
    });
    id_to_region.insert(handle.id(), region);
}

/// A rollout's report: one row per region in the operator's order, the
/// regions never attempted as `skipped (rollout halted)`.
fn rollout_report(
    wave: &Wave,
    regions: &[String],
    outcomes: &[(String, Result<(), String>)],
    json: bool,
) -> String {
    let Wave {
        rollout_id,
        env,
        version,
        ..
    } = wave;
    let skipped = unattempted_regions(regions, outcomes);
    if json {
        let mut out = String::from("{");
        out.push_str(&format!(
            "\"rollout_id\":\"{}\",\"env\":\"{}\",\"version\":\"{}\",\"regions\":[",
            cli_esc(rollout_id),
            cli_esc(env),
            cli_esc(version),
        ));
        let rows = outcomes
            .iter()
            .map(|(region, result)| match result {
                Ok(()) => format!("{{\"region\":\"{}\",\"ok\":true}}", cli_esc(region)),
                Err(e) => format!(
                    "{{\"region\":\"{}\",\"ok\":false,\"err\":\"{}\"}}",
                    cli_esc(region),
                    cli_esc(e),
                ),
            })
            .chain(skipped.iter().map(|region| {
                format!(
                    "{{\"region\":\"{}\",\"ok\":false,\"err\":\"skipped (rollout halted)\"}}",
                    cli_esc(region)
                )
            }))
            .collect::<Vec<_>>()
            .join(",");
        out.push_str(&rows);
        out.push_str("]}");
        out
    } else {
        let mut lines = vec![format!("rollout_id={rollout_id}")];
        for (region, result) in outcomes {
            lines.push(match result {
                Ok(()) => format!("{region}\tok"),
                Err(e) => format!("{region}\terr\t{e}"),
            });
        }
        for region in skipped {
            lines.push(format!("{region}\tskipped (rollout halted)"));
        }
        lines.join("\n")
    }
}

#[cfg(test)]
mod body {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// The fleet `api`: `version` before the deploy, then each listing
    /// after the first returning the next of `states` (the last
    /// repeating). `UpdateEnvironment` succeeds for `build-2` (the
    /// deploy) and `build-1` (the rollback): the rules, to count which.
    struct Fleet {
        client: aws::AwsClient,
        deployed: aws_smithy_mocks::Rule,
        rolled_back: aws_smithy_mocks::Rule,
        restarted: aws_smithy_mocks::Rule,
        rebuilt: aws_smithy_mocks::Rule,
        terminated: aws_smithy_mocks::Rule,
    }

    fn fleet(version: &'static str, states: &'static [(&'static str, &'static str)]) -> Fleet {
        fleet_named("api", version, states)
    }

    /// [`fleet`] with its one env named `name`.
    fn fleet_named(
        name: &'static str,
        version: &'static str,
        states: &'static [(&'static str, &'static str)],
    ) -> Fleet {
        use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput;
        use aws_sdk_elasticbeanstalk::types::EnvironmentDescription;
        let calls = Arc::new(AtomicUsize::new(0));
        let listing =
            aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::describe_environments)
                .then_output(move || {
                    let n = calls.fetch_add(1, Ordering::SeqCst);
                    let (status, health) = match n {
                        0 => ("Ready", "Green"),
                        n => states[(n - 1).min(states.len() - 1)],
                    };
                    DescribeEnvironmentsOutput::builder()
                        .environments(
                            EnvironmentDescription::builder()
                                .environment_name(name)
                                .application_name("shop")
                                .version_label(version)
                                .status(status.into())
                                .health(health.into())
                                .build(),
                        )
                        .build()
                });
        let update = |label: &'static str| {
            aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::update_environment)
                .match_requests(move |r| {
                    r.environment_name() == Some(name) && r.version_label() == Some(label)
                })
                .then_output(|| {
                    aws_sdk_elasticbeanstalk::operation::update_environment::UpdateEnvironmentOutput::builder().build()
                })
        };
        let (deployed, rolled_back) = (update("build-2"), update("build-1"));
        let restarted = aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::restart_app_server)
            .match_requests(move |r| r.environment_name() == Some(name))
            .then_output(|| {
                aws_sdk_elasticbeanstalk::operation::restart_app_server::RestartAppServerOutput::builder().build()
            });
        let rebuilt = aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::rebuild_environment)
            .match_requests(move |r| r.environment_name() == Some(name))
            .then_output(|| {
                aws_sdk_elasticbeanstalk::operation::rebuild_environment::RebuildEnvironmentOutput::builder().build()
            });
        let terminated = aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::terminate_environment)
            .match_requests(move |r| r.environment_name() == Some(name))
            .then_output(|| {
                aws_sdk_elasticbeanstalk::operation::terminate_environment::TerminateEnvironmentOutput::builder().build()
            });
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [
                &listing,
                &deployed,
                &rolled_back,
                &restarted,
                &rebuilt,
                &terminated
            ]
        );
        let cfg = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new("us-east-1"))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        let client = aws::AwsClient::for_tests(
            eb,
            aws_sdk_sqs::Client::new(&cfg),
            aws_sdk_cloudwatch::Client::new(&cfg),
            aws_sdk_cloudwatchlogs::Client::new(&cfg),
            aws_sdk_s3::Client::new(&cfg),
            aws_sdk_ec2::Client::new(&cfg),
        );
        Fleet {
            client,
            deployed,
            rolled_back,
            restarted,
            rebuilt,
            terminated,
        }
    }

    struct Outcome {
        code: i32,
        out: String,
        err: String,
        /// The profile the safety decision was asked about, if it was.
        asked: Option<Option<String>>,
        /// The verb label the gate was asked about, if it was.
        verb: Option<String>,
        client_built: bool,
    }

    async fn action(
        args: &[&str],
        refuse: Option<&str>,
        client: Option<aws::AwsClient>,
    ) -> Outcome {
        let argv: Vec<String> = std::iter::once("action")
            .chain(args.iter().copied())
            .map(String::from)
            .collect();
        let asked = Mutex::new(None);
        let verb_asked = Mutex::new(None);
        let built = Mutex::new(false);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = action_body(
            &argv,
            crate::cli::WriteEnv::for_tests(
                Some("ops"),
                |q: &crate::cli::GateQuery<'_>, _freeze| {
                    let (env, profile, verb) = (q.env, q.profile, q.verb);
                    *asked.lock().unwrap() = Some(profile.map(String::from));
                    *verb_asked.lock().unwrap() = Some(verb.to_string());
                    refuse.map(|why| format!("refusing {env} — {why}"))
                },
            ),
            |_, _| {
                *built.lock().unwrap() = true;
                async move { client.ok_or_else(|| color_eyre::eyre::eyre!("no credentials")) }
            },
            &mut out,
            &mut err,
        )
        .await;
        Outcome {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
            asked: asked.into_inner().unwrap(),
            verb: verb_asked.into_inner().unwrap(),
            client_built: built.into_inner().unwrap(),
        }
    }

    const STUCK: &[(&str, &str)] = &[("Updating", "Grey")];

    /// Every dispatch is audited as a `dispatched` / `completed` pair,
    /// the auto-rollback's included.
    #[tokio::test(start_paused = true)]
    async fn every_dispatch_is_audited_as_a_pair() {
        use crate::cli::audit_delta::{lines_for, mark};
        let before = mark();
        let f = fleet_named("audited-restart", "build-1", STUCK);
        let r = action(
            &["restart", "--env", "audited-restart"],
            None,
            Some(f.client),
        )
        .await;
        assert_eq!(r.code, 0, "{}", r.err);
        let lines = lines_for(&before, "audited-restart");
        assert_eq!(lines.len(), 2, "{lines:#?}");
        assert!(
            lines[0].contains("stage=dispatched") && lines[0].contains("action=RestartAppServer"),
            "{lines:#?}"
        );
        assert!(
            lines[1].contains("stage=completed") && lines[1].contains("outcome=ok"),
            "{lines:#?}"
        );

        let before = mark();
        let f = fleet_named("audited-rollback", "build-1", STUCK);
        let args = [
            "deploy",
            "--env",
            "audited-rollback",
            "--version",
            "build-2",
            "--auto-rollback",
            "2m",
        ];
        let r = action(&args, None, Some(f.client)).await;
        assert_eq!(r.code, 5, "{}", r.err);
        let lines = lines_for(&before, "audited-rollback");
        let rollback: Vec<&String> = lines
            .iter()
            .filter(|l| l.contains("auto_rollback_of=build-2"))
            .collect();
        assert_eq!(rollback.len(), 2, "the rollback is a pair too: {lines:#?}");
        assert!(
            rollback[0].contains("stage=dispatched") && rollback[0].contains("version=build-1"),
            "{lines:#?}"
        );
        assert!(
            rollback[1].contains("stage=completed") && rollback[1].contains("outcome=ok"),
            "{lines:#?}"
        );
    }

    /// Each verb reaches its own call, once, and no other: a swapped
    /// mapping would rebuild what was meant to restart, or terminate
    /// what was meant to rebuild.
    #[tokio::test]
    async fn each_verb_dispatches_its_own_call() {
        for verb in ["restart", "rebuild", "terminate"] {
            let f = fleet("build-1", STUCK);
            let r = action(&[verb, "--env", "api", "--yes"], None, Some(f.client)).await;
            assert_eq!(r.code, 0, "{verb}: {}", r.err);
            let calls = (
                f.restarted.num_calls(),
                f.rebuilt.num_calls(),
                f.terminated.num_calls(),
            );
            let want = match verb {
                "restart" => (1, 0, 0),
                "rebuild" => (0, 1, 0),
                _ => (0, 0, 1),
            };
            assert_eq!(calls, want, "{verb}");
        }
    }

    /// An unknown verb is a usage error before the gate or a client.
    #[tokio::test]
    async fn an_unknown_verb_asks_nothing() {
        let r = action(&["reboot", "--env", "api"], None, None).await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert!(r.asked.is_none() && !r.client_built);
    }

    #[tokio::test]
    async fn a_verb_is_dispatched_under_the_profile_its_pins_were_checked_for() {
        let f = fleet("build-1", STUCK);
        let r = action(&["restart", "--env", "api"], None, Some(f.client)).await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(f.restarted.num_calls(), 1);
        assert!(r.out.contains("ok: restart on api dispatched"), "{}", r.out);
        assert_eq!(r.asked, Some(Some("ops".into())));
        // The audit vocabulary, so a refusal and a dispatch correlate.
        assert_eq!(r.verb.as_deref(), Some("RestartAppServer"));

        let r = action(&["restart", "--env", "api"], Some("frozen"), None).await;
        assert_eq!(r.code, 3, "{}", r.err);
        assert!(!r.client_built, "a refusal reaches no client");
        assert!(r.err.contains("refusing api — frozen"), "{}", r.err);

        let r = action(&["restart", "--env", "api"], None, None).await;
        assert_eq!(r.code, 1, "no client");
    }

    #[tokio::test(start_paused = true)]
    async fn a_deploy_returns_at_once_or_watches_for_green() {
        let f = fleet("build-1", STUCK);
        let r = action(
            &["deploy", "--env", "api", "--version", "build-2"],
            None,
            Some(f.client),
        )
        .await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(f.deployed.num_calls(), 1);
        assert!(!r.out.contains("polling"), "no flag, no watch: {}", r.out);

        let f = fleet(
            "build-1",
            &[
                ("Updating", "Grey"),
                ("Updating", "Green"),
                ("Ready", "Green"),
            ],
        );
        let r = action(
            &[
                "deploy",
                "--env",
                "api",
                "--version",
                "build-2",
                "--wait-for-green",
                "5m",
            ],
            None,
            Some(f.client),
        )
        .await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert!(r.out.contains("reached Green at t=15s"), "{}", r.out);
    }

    #[tokio::test(start_paused = true)]
    async fn a_deploy_that_never_settles_times_out_4_or_rolls_back_5() {
        let f = fleet("build-1", STUCK);
        let r = action(
            &[
                "deploy",
                "--env",
                "api",
                "--version",
                "build-2",
                "--wait-for-green",
                "1m",
            ],
            None,
            Some(f.client),
        )
        .await;
        assert_eq!(r.code, 4, "{}", r.err);
        assert_eq!(f.rolled_back.num_calls(), 0);

        let f = fleet("build-1", STUCK);
        let r = action(
            &[
                "deploy",
                "--env",
                "api",
                "--version",
                "build-2",
                "--wait-for-green",
                "1m",
                "--auto-rollback",
                "2m",
            ],
            None,
            Some(f.client),
        )
        .await;
        assert_eq!(r.code, 5, "{}", r.err);
        assert_eq!((f.deployed.num_calls(), f.rolled_back.num_calls()), (1, 1));
        assert_eq!(
            r.out.matches("continuing under auto-rollback").count(),
            1,
            "said once, not every tick: {}",
            r.out
        );
    }

    /// A malformed deploy is a usage error before the gate: on a frozen
    /// fleet it exits 2 and writes no refusal, rather than 3.
    #[tokio::test]
    async fn a_malformed_deploy_is_usage_before_the_gate() {
        for args in [
            &["deploy", "--env", "api"][..],
            &[
                "deploy",
                "--env",
                "api",
                "--version",
                "build-2",
                "--wait-for-green",
                "soon",
            ][..],
            &[
                "deploy",
                "--env",
                "api",
                "--version",
                "build-2",
                "--auto-rollback",
                "soon",
            ][..],
        ] {
            let r = action(args, Some("deploys frozen"), None).await;
            assert_eq!(r.code, 2, "{args:?}: {}", r.err);
            assert!(r.asked.is_none() && !r.client_built, "{args:?}");
        }
    }

    #[tokio::test]
    async fn a_deploy_that_cannot_be_judged_is_not_sent() {
        for (args, version, why) in [
            (&["deploy", "--env", "api"][..], "build-1", "no --version"),
            (
                &[
                    "deploy",
                    "--env",
                    "api",
                    "--version",
                    "build-2",
                    "--wait-for-green",
                    "soon",
                ][..],
                "build-1",
                "bad duration",
            ),
            (
                &[
                    "deploy",
                    "--env",
                    "api",
                    "--version",
                    "build-2",
                    "--auto-rollback",
                    "soon",
                ][..],
                "build-1",
                "bad duration",
            ),
            (
                &["deploy", "--env", "nope", "--version", "build-2"][..],
                "build-1",
                "no such env",
            ),
            (
                &[
                    "deploy",
                    "--env",
                    "api",
                    "--version",
                    "build-2",
                    "--auto-rollback",
                    "5m",
                ][..],
                "",
                "nothing to roll back to",
            ),
        ] {
            let f = fleet(version, STUCK);
            let r = action(args, None, Some(f.client)).await;
            assert_eq!(r.code, 2, "{why}: {}", r.err);
            assert_eq!(f.deployed.num_calls(), 0, "{why}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rebuild_with_env_parses() {
        let p = parse_action_args(&argv(&["action", "rebuild", "--env", "prod"])).unwrap();
        assert_eq!(p.action, "rebuild");
        assert_eq!(p.env, "prod");
        assert!(!p.yes);
        assert_eq!(p.route, Route::Verb(CliVerb::Rebuild));
    }

    #[test]
    fn deploy_collects_version_and_durations() {
        let p = parse_action_args(&argv(&[
            "action",
            "deploy",
            "--env",
            "prod",
            "--version",
            "build-900",
            "--wait-for-green",
            "5m",
            "--auto-rollback",
            "10m",
        ]))
        .unwrap();
        assert_eq!(p.action, "deploy");
        assert_eq!(
            p.route,
            Route::Deploy(DeployFlags {
                version: "build-900".into(),
                wait_for_green_secs: Some(300),
                auto_rollback_secs: Some(600),
            })
        );
    }

    #[test]
    fn empty_verb_is_usage_error_code_2() {
        // No verb at all → args.get(1) is None → empty → usage error.
        let err = parse_action_args(&argv(&["action"])).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.msg.contains("usage:"), "got: {}", err.msg);
    }

    #[test]
    fn dash_prefixed_verb_is_usage_error_code_2() {
        // A flag where the verb should be (`ebman action --env x`) is
        // caught by the dash-prefix guard, not parsed as a verb.
        let err = parse_action_args(&argv(&["action", "--env", "prod"])).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.msg.contains("usage:"), "got: {}", err.msg);
    }

    #[test]
    fn missing_env_is_usage_error_code_2() {
        let err = parse_action_args(&argv(&["action", "rebuild"])).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(err.msg.contains("--env"), "got: {}", err.msg);
    }

    #[test]
    fn unknown_flag_is_usage_error_code_2_naming_the_flag() {
        let err =
            parse_action_args(&argv(&["action", "rebuild", "--env", "p", "--bogus"])).unwrap_err();
        assert_eq!(err.code, 2);
        assert!(
            err.msg.contains("unknown flag") && err.msg.contains("--bogus"),
            "got: {}",
            err.msg
        );
    }

    #[test]
    fn terminate_without_yes_is_destructive_gate_code_3() {
        // The one non-2 path: destructive verb missing --yes exits 3,
        // distinct from a usage error. Pinning the code so the gate
        // can't silently degrade to a usage (2) or success.
        let err = parse_action_args(&argv(&["action", "terminate", "--env", "prod"])).unwrap_err();
        assert_eq!(err.code, 3);
        assert!(err.msg.contains("destructive"), "got: {}", err.msg);
    }

    #[test]
    fn terminate_with_yes_parses() {
        let p =
            parse_action_args(&argv(&["action", "terminate", "--env", "prod", "--yes"])).unwrap();
        assert_eq!(p.action, "terminate");
        assert!(p.yes);
    }

    #[test]
    fn non_destructive_verbs_do_not_require_yes() {
        // rebuild / restart parse fine without --yes — only terminate gates.
        for verb in ["rebuild", "restart"] {
            let p = parse_action_args(&argv(&["action", verb, "--env", "prod"])).unwrap();
            assert!(!p.yes, "{verb} should parse without --yes");
        }
    }

    #[test]
    fn every_cli_verb_has_a_label_and_a_dispatch() {
        // The two used to be separate `match action_name` blocks and
        // the compiler checked neither against the other, so adding a
        // verb to one and forgetting the other was a silent audit gap:
        // dispatched under one name and completed under another, or an
        // `unreachable!()` in a release binary.
        for (name, verb, label) in CliVerb::ALL {
            assert_eq!(CliVerb::parse(name), Some(*verb), "{name} parses");
            assert_eq!(verb.audit_label(), label.audit_label(), "{name} labels");
        }
        assert_eq!(CliVerb::parse("rollout"), None, "has its own path");
        assert_eq!(CliVerb::parse("deploy"), None, "has its own path");
        assert_eq!(CliVerb::parse("nonsense"), None);

        // The audit label must match the TUI's for the same verb, or
        // `ebman audit --action Restart` misses half the fleet's
        // history depending on which surface dispatched it.
        for (name, verb, _) in CliVerb::ALL {
            let tui = match *name {
                "rebuild" => crate::mode_action::Action::Rebuild,
                "restart" => crate::mode_action::Action::RestartAppServer,
                "terminate" => crate::mode_action::Action::Terminate,
                other => panic!("unmapped CLI verb {other}"),
            };
            assert_eq!(
                verb.audit_label(),
                format!("{tui:?}"),
                "{name}: CLI and TUI must audit under the same action name"
            );
        }
    }
    #[test]
    fn an_unknown_verb_is_a_usage_error_before_anything_else() {
        // `ebman action nonsense` used to build an AWS client and run
        // the safety gates before noticing the verb was wrong — so with
        // no credentials it reported a credential failure, and on a
        // frozen fleet it reported the freeze (exit 3) rather than a
        // usage error (exit 2). A malformed command is malformed
        // whichever state the fleet is in.
        assert_eq!(
            CliAction::parse("rebuild"),
            Some(CliAction::Verb(CliVerb::Rebuild))
        );
        assert_eq!(
            CliAction::parse("restart"),
            Some(CliAction::Verb(CliVerb::Restart))
        );
        assert_eq!(
            CliAction::parse("terminate"),
            Some(CliAction::Verb(CliVerb::Terminate))
        );
        assert_eq!(CliAction::parse("deploy"), Some(CliAction::Deploy));

        assert_eq!(CliAction::parse("nonsense"), None);
        assert_eq!(CliAction::parse(""), None);
        // `rollout` is routed out of `run` before this point and must
        // not be reachable here — routing it as a plain verb would
        // dispatch a single-env action for a fan-out command.
        assert_eq!(CliAction::parse("rollout"), None);

        // Every name the usage line advertises is either routable here
        // or handled earlier, so the help can't advertise a verb that
        // errors as unknown.
        for name in ["rebuild", "restart", "terminate", "deploy", "rollout"] {
            assert!(
                ACTION_USAGE.contains(name),
                "{name} missing from the usage line"
            );
            assert!(
                CliAction::parse(name).is_some() || name == "rollout",
                "{name} is advertised but not routable"
            );
        }
    }
}

#[cfg(test)]
mod deploy_flag_tests {
    use super::{auto_rollback_impossible, deploy_needs_watching};

    /// Either opt-in means we stay and watch. `cargo mutants` found the
    /// original `&&` survivable, and the `||` form is the one that
    /// matters: with it flipped, `--auto-rollback 5m` alone returns
    /// immediately and the watchdog is never armed. The operator asked
    /// for a rollback net, the command said "ok: deploy dispatched",
    /// and nothing was watching.
    #[test]
    fn either_flag_alone_means_the_deploy_is_watched() {
        assert!(
            deploy_needs_watching(Some(300), None),
            "--wait-for-green alone must be watched"
        );
        assert!(
            deploy_needs_watching(None, Some(300)),
            "--auto-rollback alone must be watched — this is the case an \
             `&&` gets wrong"
        );
        assert!(
            deploy_needs_watching(Some(300), Some(300)),
            "both, obviously"
        );
        assert!(
            !deploy_needs_watching(None, None),
            "and a plain deploy returns immediately rather than polling \
             forever"
        );
    }

    /// `--auto-rollback` with nothing to roll back to is refused up
    /// front. Both halves matter: refusing without the flag would block
    /// ordinary first deploys, and accepting with an empty label arms a
    /// watchdog that can only fail.
    #[test]
    fn auto_rollback_is_refused_only_when_there_is_no_prior_version() {
        assert!(
            auto_rollback_impossible(Some(300), ""),
            "asked for auto-rollback with no prior version — refuse"
        );
        assert!(
            !auto_rollback_impossible(Some(300), "v1"),
            "asked for it WITH a prior version — allow"
        );
        assert!(
            !auto_rollback_impossible(None, ""),
            "a first deploy with no auto-rollback is perfectly ordinary and \
             must not be refused"
        );
        assert!(!auto_rollback_impossible(None, "v1"));
    }
}

#[cfg(test)]
mod rollout_flag_tests {
    use super::{unattempted_regions, validate_rollout_flags};

    // ── mutation-sweep triage, 2026-08-26 ────────────────────────────
    //
    // `run_rollout` held most of this file's 37 reachable survivors, all
    // of them inline against `eprintln!` + `exit(2)` and so unreachable
    // from a test. These cover what came out of it.

    /// Which flag combinations a rollout refuses.
    #[test]
    fn rollout_flags_refuse_the_combinations_that_cannot_work() {
        // The legal baselines first, so "refuse everything" can't pass.
        assert_eq!(
            validate_rollout_flags(false, None, None, None, false),
            Ok(false),
            "a plain sequential rollout is fine"
        );
        assert_eq!(
            validate_rollout_flags(true, None, Some(3), None, false),
            Ok(true),
            "--parallel with --max-concurrency is the point of the flag"
        );
        assert_eq!(
            validate_rollout_flags(false, Some(30), None, Some(300), false),
            Ok(false),
            "--staggered with --wait-for-green is fine"
        );

        // --staggered needs sequential ordering.
        assert!(
            validate_rollout_flags(true, Some(30), None, Some(300), false)
                .unwrap_err()
                .contains("mutually exclusive"),
            "--parallel + --staggered must be refused"
        );
        // --max-concurrency is meaningless without --parallel.
        assert!(validate_rollout_flags(false, None, Some(3), None, false)
            .unwrap_err()
            .contains("--max-concurrency only applies"),);
        // Staggering is timed from each region's Green observation, so
        // there has to be one.
        assert!(validate_rollout_flags(false, Some(30), None, None, false)
            .unwrap_err()
            .contains("--staggered requires --wait-for-green"),);
    }

    /// `--parallel` implies `--continue-on-fail`, because in-flight
    /// regions can't be cancelled server-side. The `||` matters in one
    /// direction only, so both are checked.
    #[test]
    fn parallel_implies_continue_on_fail() {
        assert_eq!(
            validate_rollout_flags(true, None, None, None, false),
            Ok(true),
            "--parallel alone still continues on failure"
        );
        assert_eq!(
            validate_rollout_flags(false, None, None, None, true),
            Ok(true),
            "and an explicit --continue-on-fail is honoured without it"
        );
        assert_eq!(
            validate_rollout_flags(false, None, None, None, false),
            Ok(false),
            "neither means neither — `&&` here would swallow both cases"
        );
    }

    /// A halted rollout still has to report the regions it never
    /// reached. Losing those lines is what 0.14.1 shipped a fix for.
    #[test]
    fn unattempted_regions_are_reported_in_operator_order() {
        let regions: Vec<String> = ["us-east-1", "eu-west-1", "ap-south-1"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        // Halted after the first region failed.
        let outcomes = vec![("us-east-1".to_string(), Err("boom".to_string()))];
        assert_eq!(
            unattempted_regions(&regions, &outcomes),
            vec!["eu-west-1", "ap-south-1"],
            "both un-reached regions, in the order the operator gave them"
        );

        // Everything attempted → nothing to report. Without this, a
        // function that returned every region would pass the case above.
        let all: Vec<(String, Result<(), String>)> =
            regions.iter().map(|r| (r.clone(), Ok(()))).collect();
        assert!(unattempted_regions(&regions, &all).is_empty());

        // Out-of-order completions (the parallel path) still resolve by
        // name, not by position.
        let jumbled = vec![
            ("ap-south-1".to_string(), Ok(())),
            ("us-east-1".to_string(), Ok(())),
        ];
        assert_eq!(unattempted_regions(&regions, &jumbled), vec!["eu-west-1"]);
    }
}

#[cfg(test)]
mod rollout {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    /// How one region of the test fleet answers.
    #[derive(Clone, Copy, PartialEq)]
    enum Region {
        Ok,
        /// `UpdateEnvironment` is refused.
        DeployFails,
        /// The env does not exist there.
        NoEnv,
        /// Still `Updating` on the first two polls after the dispatch.
        Slow,
    }

    /// Every successful dispatch across a test fleet, and the freeze an
    /// operator declares once `after` of them have gone.
    #[derive(Clone)]
    struct Dispatches {
        count: Arc<AtomicUsize>,
        freeze_after: Option<(usize, crate::freeze::FreezeStore)>,
    }

    impl Dispatches {
        fn record(&self) {
            let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
            if self
                .freeze_after
                .as_ref()
                .is_some_and(|(after, _)| n >= *after)
            {
                self.declare();
            }
        }

        fn declare(&self) {
            if let Some((_, store)) = &self.freeze_after {
                store
                    .declare_persistent("incident #4321", true)
                    .expect("declared");
            }
        }
    }

    /// A region's client: lists `api` (unless `NoEnv`), and answers
    /// `UpdateEnvironment` for `api` / `build-2`, recording into
    /// `dispatched` every one that succeeds.
    fn region_client(
        name: &'static str,
        region: &str,
        how: Region,
        dispatched: Dispatches,
    ) -> aws::AwsClient {
        use aws_sdk_elasticbeanstalk::operation::describe_environments::DescribeEnvironmentsOutput;
        use aws_sdk_elasticbeanstalk::operation::update_environment::{
            UpdateEnvironmentError, UpdateEnvironmentOutput,
        };
        use aws_sdk_elasticbeanstalk::types::EnvironmentDescription;
        let listings = Arc::new(AtomicUsize::new(0));
        let listing =
            aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::describe_environments)
                .then_output(move || {
                    // The pre-flight is listing 0, the first poll listing 1.
                    let n = listings.fetch_add(1, Ordering::SeqCst);
                    let (status, health) = match how {
                        Region::Slow if (1..=2).contains(&n) => ("Updating", "Grey"),
                        _ => ("Ready", "Green"),
                    };
                    let mut b = DescribeEnvironmentsOutput::builder();
                    if how != Region::NoEnv {
                        b = b.environments(
                            EnvironmentDescription::builder()
                                .environment_name(name)
                                .application_name("shop")
                                .version_label("build-1")
                                .status(status.into())
                                .health(health.into())
                                .build(),
                        );
                    }
                    b.build()
                });
        let deploy = aws_smithy_mocks::mock!(aws_sdk_elasticbeanstalk::Client::update_environment)
            .match_requests(|r| {
                r.environment_name() == Some(name) && r.version_label() == Some("build-2")
            });
        let deploy = if how == Region::DeployFails {
            deploy.then_error(|| {
                UpdateEnvironmentError::generic(
                    aws_smithy_types::error::ErrorMetadata::builder()
                        .code("InvalidParameterValue")
                        .message("already updating")
                        .build(),
                )
            })
        } else {
            deploy.then_output(move || {
                dispatched.record();
                UpdateEnvironmentOutput::builder().build()
            })
        };
        let eb = aws_smithy_mocks::mock_client!(
            aws_sdk_elasticbeanstalk,
            aws_smithy_mocks::RuleMode::MatchAny,
            [&listing, &deploy]
        );
        let cfg = aws_config::SdkConfig::builder()
            .region(aws_config::Region::new(region.to_string()))
            .behavior_version(aws_config::BehaviorVersion::latest())
            .build();
        aws::AwsClient::for_tests(
            eb,
            aws_sdk_sqs::Client::new(&cfg),
            aws_sdk_cloudwatch::Client::new(&cfg),
            aws_sdk_cloudwatchlogs::Client::new(&cfg),
            aws_sdk_s3::Client::new(&cfg),
            aws_sdk_ec2::Client::new(&cfg),
        )
    }

    struct Outcome {
        code: i32,
        out: String,
        err: String,
        /// Successful `UpdateEnvironment` calls, across every region.
        dispatched: usize,
        /// The `(profile, region)` of each client built, in order.
        built: Vec<(Option<String>, String)>,
        /// The profile the pins were checked for, if they were.
        asked: Option<Option<String>>,
        /// The verb label the gate was asked about, if it was.
        verb: Option<String>,
    }

    /// Run a rollout over `fleet`. `refuse` refuses it up front;
    /// `freeze_after` declares a freeze once that many regions have
    /// dispatched.
    async fn rollout(
        args: &[&str],
        fleet: &'static [(&'static str, Region)],
        refuse: Option<&str>,
        freeze_after: Option<usize>,
    ) -> Outcome {
        // The fleet's env is the one `--env` names (leaked: a test's
        // whole life), so a test can give it a name no other writes.
        let name: &'static str = args
            .iter()
            .position(|a| *a == "--env")
            .and_then(|i| args.get(i + 1))
            .map(|n| &*Box::leak(n.to_string().into_boxed_str()))
            .unwrap_or("api");
        let argv: Vec<String> = ["action", "rollout"]
            .iter()
            .chain(args)
            .map(|s| s.to_string())
            .collect();
        let built = Mutex::new(Vec::new());
        let asked = Mutex::new(None);
        let verb_asked = Mutex::new(None);
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let progress: Progress = Arc::new(|_: &str| {});
        let write_env = crate::cli::WriteEnv::for_tests(
            Some("ops"),
            |q: &crate::cli::GateQuery<'_>, _freeze| {
                let (env, profile, verb) = (q.env, q.profile, q.verb);
                *asked.lock().unwrap() = Some(profile.map(String::from));
                *verb_asked.lock().unwrap() = Some(verb.to_string());
                refuse.map(|why| format!("refusing {env} — {why}"))
            },
        );
        let dispatched = Dispatches {
            count: Arc::new(AtomicUsize::new(0)),
            freeze_after: freeze_after.map(|n| (n, write_env.freeze.clone())),
        };
        // After none: declared during the pre-flight, which the test gate
        // (blind to the freeze, as `refuse` alone decides it) lets past.
        if freeze_after == Some(0) {
            dispatched.declare();
        }
        let code = rollout_body(
            &argv,
            write_env,
            |profile: Option<String>, region: Option<String>| {
                let region = region.expect("a rollout names every region");
                built.lock().unwrap().push((profile, region.clone()));
                let how = fleet.iter().find(|(r, _)| *r == region).map(|(_, h)| *h);
                let dispatched = dispatched.clone();
                async move {
                    how.map(|h| region_client(name, &region, h, dispatched))
                        .ok_or_else(|| color_eyre::eyre::eyre!("no credentials for {region}"))
                }
            },
            RolloutIo {
                out: &mut out,
                err: &mut err,
                progress,
            },
        )
        .await;
        Outcome {
            code,
            out: String::from_utf8(out).unwrap(),
            err: String::from_utf8(err).unwrap(),
            dispatched: dispatched.count.load(Ordering::SeqCst),
            built: built.into_inner().unwrap(),
            asked: asked.into_inner().unwrap(),
            verb: verb_asked.into_inner().unwrap(),
        }
    }

    const THREE: &[(&str, Region)] = &[
        ("us-east-1", Region::Ok),
        ("eu-west-2", Region::Ok),
        ("ap-south-1", Region::Ok),
    ];
    const ARGS: &[&str] = &[
        "--env",
        "api",
        "--version",
        "build-2",
        "--regions",
        "us-east-1,eu-west-2,ap-south-1",
    ];

    fn with<'a>(extra: &[&'a str]) -> Vec<&'a str> {
        ARGS.iter().copied().chain(extra.iter().copied()).collect()
    }

    #[tokio::test]
    async fn every_region_is_dispatched_in_order() {
        let r = rollout(&with(&["--yes"]), THREE, None, None).await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(r.dispatched, 3);
        let rows: Vec<&str> = r.out.lines().skip(1).collect();
        assert_eq!(rows, ["us-east-1\tok", "eu-west-2\tok", "ap-south-1\tok"]);
    }

    /// In parallel, us-east-1 reaches Green last, so it completes last;
    /// the report still lists it first.
    #[tokio::test(start_paused = true)]
    async fn a_parallel_report_is_in_the_operators_order() {
        let fleet: &[(&str, Region)] = &[
            ("us-east-1", Region::Slow),
            ("eu-west-2", Region::Ok),
            ("ap-south-1", Region::Ok),
        ];
        let r = rollout(
            &with(&["--yes", "--parallel", "--json", "--wait-for-green", "5m"]),
            fleet,
            None,
            None,
        )
        .await;
        assert_eq!(r.code, 0, "{}", r.err);
        let v: serde_json::Value = serde_json::from_str(r.out.trim()).unwrap();
        let regions: Vec<&str> = v["regions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|x| x["region"].as_str().unwrap())
            .collect();
        assert_eq!(
            regions,
            ["us-east-1", "eu-west-2", "ap-south-1"],
            "the operator's order, not completion order"
        );
    }

    /// Without `--yes` the pre-flight runs (an env missing anywhere is
    /// said before the question) and nothing is dispatched: exit 2 with
    /// the real region count. With it, the dispatch goes ahead — so an
    /// inverted gate fails one half or the other.
    #[tokio::test]
    async fn the_yes_gate_stops_the_dispatch_after_the_preflight() {
        let r = rollout(ARGS, THREE, None, None).await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert_eq!(r.dispatched, 0, "nothing dispatched without --yes");
        assert_eq!(r.built.len(), 3, "every region pre-flighted first");
        assert!(r.err.contains("would dispatch to 3 region(s)"), "{}", r.err);

        let missing: &[(&str, Region)] = &[
            ("us-east-1", Region::Ok),
            ("eu-west-2", Region::NoEnv),
            ("ap-south-1", Region::Ok),
        ];
        let r = rollout(&with(&["--yes"]), missing, None, None).await;
        assert_eq!((r.code, r.dispatched), (2, 0), "{}", r.err);
        assert!(
            r.err.contains("not found in region 'eu-west-2'"),
            "{}",
            r.err
        );
    }

    /// `--profile` is the account the rollout writes to, so it is the
    /// account whose pins are checked — not the ambient `AWS_PROFILE`.
    #[tokio::test]
    async fn the_pins_are_checked_for_the_profile_that_dispatches() {
        let r = rollout(
            &with(&["--yes", "--profile", "prod-admin"]),
            THREE,
            None,
            None,
        )
        .await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(r.asked, Some(Some("prod-admin".into())));
        assert_eq!(r.verb.as_deref(), Some("Rollout"));
        assert!(
            r.built
                .iter()
                .all(|(p, _)| p.as_deref() == Some("prod-admin")),
            "{:?}",
            r.built
        );

        let r = rollout(&with(&["--yes"]), THREE, None, None).await;
        assert_eq!(
            r.asked,
            Some(Some("ops".into())),
            "no --profile: the ambient one"
        );
        assert!(r.built.iter().all(|(p, _)| p.is_none()), "{:?}", r.built);

        let r = rollout(&with(&["--yes"]), THREE, Some("pinned read-only"), None).await;
        assert_eq!(r.code, 3, "{}", r.err);
        assert!(r.built.is_empty(), "a refusal builds no client");
    }

    #[tokio::test]
    async fn a_failed_region_halts_the_rest_unless_told_to_carry_on() {
        let flaky: &[(&str, Region)] = &[
            ("us-east-1", Region::Ok),
            ("eu-west-2", Region::DeployFails),
            ("ap-south-1", Region::Ok),
        ];
        let r = rollout(&with(&["--yes"]), flaky, None, None).await;
        assert_eq!((r.code, r.dispatched), (3, 1), "{}", r.err);
        assert!(
            r.out.contains("eu-west-2\terr\t")
                && r.out.contains("ap-south-1\tskipped (rollout halted)"),
            "{}",
            r.out
        );

        let r = rollout(&with(&["--yes", "--continue-on-fail"]), flaky, None, None).await;
        assert_eq!((r.code, r.dispatched), (3, 2), "{}", r.err);
        assert!(r.out.contains("ap-south-1\tok"), "{}", r.out);
    }

    /// A freeze declared once the first region has gone stops the rest,
    /// in both dispatch loops, and the run is not a success: 3.
    #[tokio::test]
    async fn a_freeze_declared_mid_rollout_halts_both_loops() {
        for extra in [
            &["--yes"][..],
            &["--yes", "--parallel", "--max-concurrency", "1"][..],
        ] {
            let r = rollout(&with(extra), THREE, None, Some(1)).await;
            assert_eq!(r.code, 3, "{extra:?}: {}", r.err);
            assert_eq!(r.dispatched, 1, "{extra:?}");
            assert!(
                r.err
                    .contains("halting — fleet freeze active (incident #4321)"),
                "{extra:?}: {}",
                r.err
            );
            assert!(
                r.out.contains("eu-west-2\tskipped (rollout halted)")
                    && r.out.contains("ap-south-1\tskipped (rollout halted)"),
                "{extra:?}: {}",
                r.out
            );
        }
    }

    /// Each region's write is a pair under the run's one rollout id.
    #[tokio::test]
    async fn every_region_is_audited_under_one_rollout_id() {
        use crate::cli::audit_delta::{lines_for, mark};
        let before = mark();
        let args = [
            "--env",
            "audited-rollout",
            "--version",
            "build-2",
            "--regions",
            "us-east-1,eu-west-2,ap-south-1",
            "--yes",
        ];
        let r = rollout(&args, THREE, None, None).await;
        assert_eq!(r.code, 0, "{}", r.err);
        let lines = lines_for(&before, "audited-rollout");
        assert_eq!(lines.len(), 6, "{lines:#?}");
        let id = r
            .out
            .lines()
            .next()
            .unwrap()
            .trim_start_matches("rollout_id=");
        assert!(
            lines
                .iter()
                .all(|l| l.contains(id) && l.contains("action=Rollout")),
            "{id}: {lines:#?}"
        );
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("stage=completed") && l.contains("outcome=ok"))
                .count(),
            3
        );
    }

    /// The regions a freeze stopped are audited as refused, each under
    /// its own region, so the log and the webhook see what never ran.
    #[tokio::test]
    async fn the_regions_a_freeze_stopped_are_audited_as_refused() {
        use crate::cli::audit_delta::{lines_for, mark};
        let before = mark();
        let args = [
            "--env",
            "audited-halt",
            "--version",
            "build-2",
            "--regions",
            "us-east-1,eu-west-2,ap-south-1",
            "--yes",
        ];
        let r = rollout(&args, THREE, None, Some(1)).await;
        assert_eq!(r.code, 3, "{}", r.err);
        let refused: Vec<String> = lines_for(&before, "audited-halt")
            .into_iter()
            .filter(|l| l.contains("stage=refused"))
            .collect();
        assert_eq!(refused.len(), 2, "{refused:#?}");
        let id = r
            .out
            .lines()
            .next()
            .unwrap()
            .trim_start_matches("rollout_id=");
        for (line, region) in refused.iter().zip(["eu-west-2", "ap-south-1"]) {
            assert!(
                line.contains(&format!("region={region}")) && line.contains("action=Rollout"),
                "{line}"
            );
            assert!(
                line.contains(&format!("rollout_id={id}")),
                "correlates with the run: {line}"
            );
        }
    }

    /// A freeze before the first region: every row skipped, and the
    /// report still JSON. It was `"regions":[,{..}]`, each skipped row
    /// written with a leading comma.
    #[tokio::test]
    async fn a_rollout_halted_before_any_region_is_valid_json() {
        let r = rollout(&with(&["--yes", "--json"]), THREE, None, Some(0)).await;
        assert_eq!((r.code, r.dispatched), (3, 0), "{}", r.err);
        let v: serde_json::Value = serde_json::from_str(r.out.trim()).expect("valid JSON");
        let rows = v["regions"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert!(
            rows.iter().all(|r| r["err"] == "skipped (rollout halted)"),
            "{rows:?}"
        );
    }

    /// `--staggered` waits between regions (not before the first), and a
    /// region that misses `--wait-for-green` fails and halts the rest.
    #[tokio::test(start_paused = true)]
    async fn staggered_waits_and_a_slow_region_times_out() {
        let start = tokio::time::Instant::now();
        let r = rollout(
            &with(&["--yes", "--wait-for-green", "5m", "--staggered", "2m"]),
            THREE,
            None,
            None,
        )
        .await;
        assert_eq!(r.code, 0, "{}", r.err);
        assert_eq!(
            r.err.matches("staggering 120s before next region").count(),
            2,
            "{}",
            r.err
        );
        assert!(
            start.elapsed() >= std::time::Duration::from_secs(240),
            "{:?}",
            start.elapsed()
        );

        let slow_first: &[(&str, Region)] = &[
            ("us-east-1", Region::Slow),
            ("eu-west-2", Region::Ok),
            ("ap-south-1", Region::Ok),
        ];
        let r = rollout(
            &with(&["--yes", "--wait-for-green", "10s"]),
            slow_first,
            None,
            None,
        )
        .await;
        assert_eq!((r.code, r.dispatched), (3, 1), "{}", r.err);
        assert!(
            r.out
                .contains("us-east-1\terr\tdid not reach Green within 10s"),
            "{}",
            r.out
        );
        assert!(
            r.out.contains("eu-west-2\tskipped (rollout halted)"),
            "{}",
            r.out
        );
    }

    /// A freeze declared during the pre-flight stops the first wave too:
    /// the sequential loop checks before every region, and the parallel
    /// one must check before it seeds.
    #[tokio::test]
    async fn a_freeze_before_the_first_wave_dispatches_nothing() {
        for extra in [&["--yes"][..], &["--yes", "--parallel"][..]] {
            let r = rollout(&with(extra), THREE, None, Some(0)).await;
            assert_eq!((r.code, r.dispatched), (3, 0), "{extra:?}: {}", r.err);
        }
    }

    #[tokio::test]
    async fn usage_and_unreachable_regions() {
        let r = rollout(
            &with(&["--yes", "--parallel", "--staggered", "5m"]),
            THREE,
            None,
            None,
        )
        .await;
        assert_eq!(r.code, 2, "{}", r.err);
        assert!(
            r.asked.is_none(),
            "a usage error is decided before the pins"
        );
        let r = rollout(&["--env", "api", "--version", "build-2"], THREE, None, None).await;
        assert_eq!(r.code, 2, "no --regions");
        let two: &[(&str, Region)] = &[("us-east-1", Region::Ok)];
        let r = rollout(&with(&["--yes"]), two, None, None).await;
        assert_eq!(
            (r.code, r.dispatched),
            (1, 0),
            "eu-west-2 has no client: {}",
            r.err
        );
    }
}
