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
    version: Option<String>,
    wait_for_green: Option<String>,
    auto_rollback: Option<String>,
    yes: bool,
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
    const ALL: &'static [(&'static str, CliVerb, &'static str)] = &[
        ("rebuild", CliVerb::Rebuild, "Rebuild"),
        // "RestartAppServer", not "Restart": the TUI audits every
        // action under its Debug name, so a CLI-written "Restart"
        // split the same operation into two names in one log —
        // `ebman audit --action` matched half the history either way.
        ("restart", CliVerb::Restart, "RestartAppServer"),
        ("terminate", CliVerb::Terminate, "Terminate"),
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
            .map(|(_, _, l)| *l)
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
        version,
        wait_for_green,
        auto_rollback,
        yes,
    })
}

/// Safety-pin gate for every `ebman action` dispatch — the shared
/// `Config::pin_reason` check that `audit replay` and `lint --fix`
/// use, matched against the ambient `AWS_PROFILE`. Exit 3 (refused),
/// same as the replay refusal. Without this, the largest CLI write
/// surface bypassed `safety.envs.*` / `safety.accounts.*` entirely
/// (0.26 max-review C3 — the class the 0.14.1 patch fixed for
/// `lint --fix`).
/// The CLI's write gate: freeze first, then the per-env pin.
///
/// One function so the two call sites can't drift from each other,
/// and so the ORDER is defined once — it used to be two bare calls in
/// sequence, which is how it came to differ from the MCP gate's.
/// `explicit` is the profile the write will actually run under, when
/// the subcommand takes one. It is not optional politeness: the gate
/// resolves `safety.accounts.NAME.read_only` against the profile it is
/// given, so feeding it the ambient `AWS_PROFILE` while dispatching
/// under `--profile X` checks the pin on the wrong account entirely.
/// `rollout` did exactly that, and it is the biggest write the CLI has.
fn refuse_write(prog: &'static str, env: &str, explicit: Option<&str>) {
    let ambient = std::env::var("AWS_PROFILE").ok();
    let profile = explicit.or(ambient.as_deref());
    crate::cli::refuse_write(prog, env, env, profile);
}

pub async fn run(args: &[String]) -> Result<()> {
    let action_name = args.get(1).map(|s| s.as_str()).unwrap_or("");
    if action_name == "rollout" {
        return run_rollout(args).await;
    }
    let ActionArgs {
        action: action_name,
        env,
        version,
        wait_for_green,
        auto_rollback,
        ..
    } = match parse_action_args(args) {
        Ok(parsed) => parsed,
        Err(ActionArgError { msg, code }) => {
            eprintln!("{msg}");
            std::process::exit(code);
        }
    };
    let action_name = action_name.as_str();
    let Some(action) = CliAction::parse(action_name) else {
        eprintln!("ebman action: unknown action '{action_name}'");
        eprintln!("{ACTION_USAGE}");
        std::process::exit(2);
    };
    // Freeze BEFORE pin, matching the MCP write gate. The two
    // disagreed on order, so an env that was both pinned and frozen
    // got a different reason depending on which surface refused it —
    // and the freeze is the more urgent of the two: fleet-wide,
    // session-scoped, and the thing that just changed.
    refuse_write("ebman action", &env, None);
    let aws = aws::AwsClient::with(None, None).await?;

    let verb = match action {
        CliAction::Deploy => {
            return run_deploy(&aws, &env, version, wait_for_green, auto_rollback).await;
        }
        CliAction::Verb(v) => v,
    };
    let label = verb.audit_label();
    // Mirror the TUI's dispatched/completed audit pair — headless
    // dispatches were previously invisible to the audit log (and the
    // webhook fan-out), contradicting the safety docs.
    let cli_profile = std::env::var("AWS_PROFILE").ok();
    audit::append_action_dispatched(
        None,
        cli_profile.as_deref(),
        &aws.context.region,
        label,
        &env,
        &[],
    );
    let result = verb.dispatch(&aws, &env).await;
    let err_text = result.as_ref().err().map(|e| e.to_string());
    audit::append_action_completed(
        None,
        cli_profile.as_deref(),
        &aws.context.region,
        label,
        &env,
        match err_text.as_deref() {
            None => Ok(()),
            Some(e) => Err(e),
        },
        &[],
    );
    match result {
        Ok(()) => {
            println!("ok: {action_name} on {env} dispatched");
            crate::cli::drain_before_return().await;
            Ok(())
        }
        Err(e) => {
            eprintln!("err: {e}");
            crate::cli::exit_after_drain(1).await;
        }
    }
}

/// `ebman action deploy --env X --version Y [--wait-for-green Nm]
/// [--auto-rollback Nm]` — non-interactive CLI parity with the
/// typed-command `:deploy` path. Exit codes:
///   0  — deploy dispatched (and reached Green if asked)
///   1  — AWS-layer error
///   2  — usage error
///   4  — `--wait-for-green` deadline elapsed without Green
///   5  — `--auto-rollback` deadline elapsed; rollback dispatched
async fn run_deploy(
    aws: &aws::AwsClient,
    env: &str,
    version: Option<String>,
    wait_for_green: Option<String>,
    auto_rollback: Option<String>,
) -> Result<()> {
    let Some(version) = version else {
        eprintln!("ebman action deploy: --version LABEL is required");
        std::process::exit(2);
    };
    let wait_for_green_secs = match wait_for_green {
        Some(ref s) => match aws::parse_window_ms(s) {
            Some(ms) => Some((ms / 1000) as u64),
            None => {
                eprintln!(
                    "ebman action deploy: --wait-for-green expects a duration like `5m` / `30m` / `1h`"
                );
                std::process::exit(2);
            }
        },
        None => None,
    };
    let auto_rollback_secs = match auto_rollback {
        Some(ref s) => match aws::parse_window_ms(s) {
            Some(ms) => Some((ms / 1000) as u64),
            None => {
                eprintln!(
                    "ebman action deploy: --auto-rollback expects a duration like `5m` / `30m` / `1h`"
                );
                std::process::exit(2);
            }
        },
        None => None,
    };

    let envs = aws
        .list_environments()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_environments: {e}"))?;
    let snapshot = envs
        .iter()
        .find(|e| e.name == env)
        .map(|e| e.version_label.clone());
    let Some(snapshot_label) = snapshot else {
        eprintln!("ebman action deploy: env '{env}' not found");
        std::process::exit(2);
    };
    if auto_rollback_impossible(auto_rollback_secs, &snapshot_label) {
        eprintln!(
            "ebman action deploy: --auto-rollback requested but env '{env}' has no prior version to roll back to"
        );
        std::process::exit(2);
    }

    println!("dispatching deploy: env={env} version={version}");
    let cli_profile = std::env::var("AWS_PROFILE").ok();
    audit::append_action_dispatched(
        None,
        cli_profile.as_deref(),
        &aws.context.region,
        "Deploy",
        env,
        &[("version", &version)],
    );
    if let Err(e) = aws.deploy_version(env, &version).await {
        let msg = format!("deploy_version: {e}");
        audit::append_action_completed(
            None,
            cli_profile.as_deref(),
            &aws.context.region,
            "Deploy",
            env,
            Err(&msg),
            &[("version", &version)],
        );
        eprintln!("err: {msg}");
        crate::cli::exit_after_drain(1).await;
    }
    audit::append_action_completed(
        None,
        cli_profile.as_deref(),
        &aws.context.region,
        "Deploy",
        env,
        Ok(()),
        &[("version", &version)],
    );

    if !deploy_needs_watching(wait_for_green_secs, auto_rollback_secs) {
        println!("ok: deploy on {env} dispatched (version={version})");
        crate::cli::drain_before_return().await;
        return Ok(());
    }

    let start = tokio::time::Instant::now();
    let poll_interval = std::time::Duration::from_secs(5);
    let mut wait_for_green_timeout_emitted = false;
    println!(
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
                eprintln!("err: list_environments during poll: {e}");
                crate::cli::exit_after_drain(1).await;
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
                println!("poll t={elapsed}s status={status} health={health}");
            }
            PollDecision::Success => {
                println!("ok: deploy on {env} reached Green at t={elapsed}s (version={version})");
                crate::cli::drain_before_return().await;
                return Ok(());
            }
            PollDecision::WaitForGreenTimeout => {
                wait_for_green_timeout_emitted = true;
                if auto_rollback_secs.is_none() {
                    eprintln!(
                        "timeout: deploy on {env} did not reach Green within {}s (status={status}, health={health}, version={version})",
                        wait_for_green_secs.unwrap_or(0)
                    );
                    crate::cli::exit_after_drain(4).await;
                }
                let remaining = auto_rollback_secs.unwrap_or(0).saturating_sub(elapsed);
                println!(
                    "wait-for-green timeout at t={elapsed}s (status={status}, health={health}); continuing under auto-rollback ({remaining}s remaining)"
                );
            }
            PollDecision::DispatchRollback => {
                eprintln!(
                    "auto-rollback firing on {env}: env still status={status} health={health} at t={elapsed}s; redeploying snapshot version={snapshot_label}"
                );
                audit::append_action_dispatched(
                    None,
                    cli_profile.as_deref(),
                    &aws.context.region,
                    "Deploy",
                    env,
                    &[("version", &snapshot_label), ("auto_rollback_of", &version)],
                );
                if let Err(e) = aws.deploy_version(env, &snapshot_label).await {
                    eprintln!("err: rollback deploy_version: {e}");
                    crate::cli::exit_after_drain(1).await;
                }
                println!("ok: rollback dispatched on {env} (version={snapshot_label})");
                crate::cli::exit_after_drain(5).await;
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

/// Per-region dispatch helper shared by sequential + parallel paths.
/// Calls `deploy_version`; optionally polls until Green (or the
/// `--wait-for-green` deadline elapses). Emits the per-region
/// `stage=dispatched` and `stage=completed` audit-log lines. Returns
/// `Ok(())` on Green (or just dispatched if no wait); `Err(msg)`
/// when dispatch fails or the deadline elapses without Green.
async fn dispatch_one_region(
    client: &aws::AwsClient,
    env: &str,
    version: &str,
    wait_for_green_secs: Option<u64>,
    rollout_id: &str,
    region: &str,
    quiet: bool,
) -> Result<(), String> {
    if !quiet {
        eprintln!("rollout: dispatching to {region} (env={env}, version={version})");
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
            eprintln!("rollout[{region}]: {msg}");
            Err(msg)
        }
    };
    if outcome.is_ok() {
        if let Some(secs) = wait_for_green_secs {
            let start = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                let envs = match client.list_environments().await {
                    Ok(envs) => envs,
                    Err(e) => {
                        eprintln!("rollout[{region}]: list_environments during poll: {e}");
                        outcome = Err(format!("poll: {e}"));
                        break;
                    }
                };
                let (status, health) = envs
                    .iter()
                    .find(|e| e.name == env)
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
                            eprintln!(
                                "rollout[{region}]: t={elapsed}s status={status} health={health}"
                            );
                        }
                    }
                    PollDecision::Success => {
                        if !quiet {
                            eprintln!("rollout[{region}]: reached Green at t={elapsed}s");
                        }
                        break;
                    }
                    PollDecision::WaitForGreenTimeout => {
                        let msg = format!(
                            "did not reach Green within {secs}s (status={status}, health={health})"
                        );
                        eprintln!("rollout[{region}]: {msg}");
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

/// `ebman action rollout --version LABEL --regions r1,r2,r3 --env NAME --yes [...]`
/// `take_value` for the rollout parser, which exits directly (its
/// error paths all print + exit 2 inline).
fn rollout_value<'a, I: Iterator<Item = &'a String>>(
    iter: &mut I,
    flag: &str,
    what: &str,
) -> String {
    match crate::cli::take_value(iter, "ebman action rollout", flag, what) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    }
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

/// — cross-region deploy with pre-flight + per-region dispatch +
/// audit-log correlation. Sequential by default (halt on first
/// failure); `--parallel` fans out concurrently with optional
/// `--max-concurrency N` cap; `--continue-on-fail` attempts every
/// region in sequential mode; `--staggered Nm` waits N minutes
/// between regions in sequential mode (canary-style rollouts).
///
/// Exit codes:
/// - 0 all regions dispatched successfully
/// - 1 AWS-layer error before any region dispatched
/// - 2 usage error (mutually-exclusive flags, missing required args)
/// - 3 one or more region dispatches failed
async fn run_rollout(args: &[String]) -> Result<()> {
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
            "--env" => env_name = Some(rollout_value(&mut iter, "--env", "an env name")),
            "--version" => version = Some(rollout_value(&mut iter, "--version", "a version label")),
            "--regions" => {
                regions_csv = Some(rollout_value(&mut iter, "--regions", "a region list"))
            }
            "--wait-for-green" => {
                wait_for_green = Some(rollout_value(&mut iter, "--wait-for-green", "a duration"))
            }
            "--profile" => profile = Some(rollout_value(&mut iter, "--profile", "a profile name")),
            "--yes" => yes = true,
            "--json" => json = true,
            "--quiet" => quiet = true,
            "--parallel" => parallel = true,
            "--max-concurrency" => {
                let Some(v) = iter.next() else {
                    eprintln!("ebman action rollout: --max-concurrency expects an integer");
                    std::process::exit(2);
                };
                let Ok(n) = v.parse::<usize>() else {
                    eprintln!(
                        "ebman action rollout: --max-concurrency expects an integer, got '{v}'"
                    );
                    std::process::exit(2);
                };
                if n == 0 {
                    eprintln!("ebman action rollout: --max-concurrency must be > 0");
                    std::process::exit(2);
                }
                max_concurrency = Some(n);
            }
            "--continue-on-fail" => continue_on_fail = true,
            "--staggered" => {
                staggered = Some(rollout_value(&mut iter, "--staggered", "a duration"))
            }
            other => {
                eprintln!("ebman action rollout: unknown flag '{other}'");
                std::process::exit(2);
            }
        }
    }
    let Some(env) = env_name else {
        eprintln!("ebman action rollout: --env NAME is required");
        std::process::exit(2);
    };
    let Some(version) = version else {
        eprintln!("ebman action rollout: --version LABEL is required");
        std::process::exit(2);
    };
    let Some(regions_csv) = regions_csv else {
        eprintln!(
            "ebman action rollout: --regions r1,r2,r3 is required (comma-separated, no spaces)"
        );
        std::process::exit(2);
    };
    let mut regions: Vec<String> = crate::util::split_csv(&regions_csv);
    // Dedupe preserving order — `r1,r1` used to dispatch twice, the
    // second racing the first ("already updating") and marking the
    // rollout partial for a self-inflicted reason.
    let mut seen = std::collections::HashSet::new();
    regions.retain(|r| seen.insert(r.clone()));
    if regions.is_empty() {
        eprintln!("ebman action rollout: --regions list is empty");
        std::process::exit(2);
    }
    // Same pin gate as the single-env verbs — a rollout is a deploy
    // fan-out of one env name across regions.
    // The profile this rollout will actually dispatch under —
    // `--profile` if given, else ambient. Gating on the ambient
    // one while dispatching under another checks the pin on the
    // wrong account.
    refuse_write("ebman action rollout", &env, profile.as_deref());
    let wait_for_green_secs = match wait_for_green.as_deref() {
        Some(s) => match aws::parse_window_ms(s) {
            Some(ms) => Some((ms / 1000) as u64),
            None => {
                eprintln!(
                    "ebman action rollout: --wait-for-green expects a duration like `5m` / `30m` / `1h`"
                );
                std::process::exit(2);
            }
        },
        None => None,
    };
    let staggered_secs = match staggered.as_deref() {
        Some(s) => match aws::parse_window_ms(s) {
            Some(ms) => Some((ms / 1000) as u64),
            None => {
                eprintln!(
                    "ebman action rollout: --staggered expects a duration like `5m` / `30m` / `1h`"
                );
                std::process::exit(2);
            }
        },
        None => None,
    };

    // Flag combination validation.
    let continue_on_fail = match validate_rollout_flags(
        parallel,
        staggered_secs,
        max_concurrency,
        wait_for_green_secs,
        continue_on_fail,
    ) {
        Ok(v) => v,
        Err(msg) => {
            eprintln!("{msg}");
            std::process::exit(2);
        }
    };

    if !quiet {
        eprintln!(
            "rollout: pre-flighting {} region(s) for env '{env}' version '{version}'",
            regions.len()
        );
    }
    let mut per_region: Vec<(String, aws::AwsClient)> = Vec::with_capacity(regions.len());
    for region in &regions {
        let client = match aws::AwsClient::with(profile.clone(), Some(region.clone())).await {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "ebman action rollout: failed to construct client for region '{region}': {e}"
                );
                std::process::exit(1);
            }
        };
        let envs = match client.list_environments().await {
            Ok(envs) => envs,
            Err(e) => {
                eprintln!("ebman action rollout: list_environments in '{region}' failed: {e}");
                std::process::exit(1);
            }
        };
        if !envs.iter().any(|e| e.name == env) {
            eprintln!(
                "ebman action rollout: env '{env}' not found in region '{region}' — rollout halted before dispatching"
            );
            std::process::exit(2);
        }
        per_region.push((region.clone(), client));
    }

    if !yes {
        eprintln!(
            "ebman action rollout: would dispatch to {} region(s); re-run with --yes to confirm",
            regions.len()
        );
        std::process::exit(2);
    }

    let rollout_id = format!("rollout-{}", chrono::Utc::now().format("%Y%m%dT%H%M%SZ"));
    // Arc-wrap clients so both sequential and parallel paths can
    // share them. Each AwsClient holds Arc'd SDK clients internally
    // (cheap clone), but the outer struct isn't Clone — wrap once
    // here so the parallel path's task closures get a moved Arc.
    let per_region: Vec<(String, std::sync::Arc<aws::AwsClient>)> = per_region
        .into_iter()
        .map(|(r, c)| (r, std::sync::Arc::new(c)))
        .collect();

    let mut outcomes: Vec<(String, Result<(), String>)> = Vec::new();
    if parallel {
        // Parallel dispatch — one task per region, all started
        // immediately (or capped at `max_concurrency` if set).
        // tokio::JoinSet awaits completions in arbitrary order;
        // outcomes therefore aren't sorted by region order — the
        // output renderer sorts by the input `regions` order when
        // emitting.
        if !quiet {
            eprintln!(
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
        // Seed initial batch.
        for _ in 0..cap.min(queue.len()) {
            let Some((region, client)) = queue.pop_front() else {
                break;
            };
            let env_for = env.clone();
            let version_for = version.clone();
            let rollout_id_for = rollout_id.clone();
            let quiet_for = quiet;
            let region_for_inner = region.clone();
            let handle = joinset.spawn(async move {
                let outcome = dispatch_one_region(
                    &client,
                    &env_for,
                    &version_for,
                    wait_for_green_secs,
                    &rollout_id_for,
                    &region_for_inner,
                    quiet_for,
                )
                .await;
                (region_for_inner, outcome)
            });
            id_to_region.insert(handle.id(), region);
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
                    let id = e.id();
                    let region = id_to_region.remove(&id).unwrap_or_default();
                    (region, Err(format!("join: {e}")))
                }
            };
            outcomes.push((region, outcome));
            if let Some((next_region, next_client)) = queue.pop_front() {
                let env_for = env.clone();
                let version_for = version.clone();
                let rollout_id_for = rollout_id.clone();
                let quiet_for = quiet;
                let region_for_inner = next_region.clone();
                let handle = joinset.spawn(async move {
                    let outcome = dispatch_one_region(
                        &next_client,
                        &env_for,
                        &version_for,
                        wait_for_green_secs,
                        &rollout_id_for,
                        &region_for_inner,
                        quiet_for,
                    )
                    .await;
                    (region_for_inner, outcome)
                });
                id_to_region.insert(handle.id(), next_region);
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
                        eprintln!("rollout: staggering {stagger}s before next region");
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(stagger)).await;
                }
            }
            first_region = false;
            let outcome = dispatch_one_region(
                client,
                &env,
                &version,
                wait_for_green_secs,
                &rollout_id,
                region,
                quiet,
            )
            .await;
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

    let any_failure = outcomes.iter().any(|(_, r)| r.is_err());
    if !quiet {
        if json {
            let mut out = String::from("{");
            out.push_str(&format!(
                "\"rollout_id\":\"{}\",\"env\":\"{}\",\"version\":\"{}\",\"regions\":[",
                cli_esc(&rollout_id),
                cli_esc(&env),
                cli_esc(&version),
            ));
            for (i, (region, result)) in outcomes.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                match result {
                    Ok(()) => {
                        out.push_str(&format!(
                            "{{\"region\":\"{}\",\"ok\":true}}",
                            cli_esc(region)
                        ));
                    }
                    Err(e) => {
                        out.push_str(&format!(
                            "{{\"region\":\"{}\",\"ok\":false,\"err\":\"{}\"}}",
                            cli_esc(region),
                            cli_esc(e),
                        ));
                    }
                }
            }
            for region in unattempted_regions(&regions, &outcomes) {
                out.push_str(&format!(
                    ",{{\"region\":\"{}\",\"ok\":false,\"err\":\"skipped (rollout halted)\"}}",
                    cli_esc(region)
                ));
            }
            out.push_str("]}");
            println!("{}", out);
        } else {
            println!("rollout_id={rollout_id}");
            for (region, result) in &outcomes {
                match result {
                    Ok(()) => println!("{region}\tok"),
                    Err(e) => println!("{region}\terr\t{e}"),
                }
            }
            for region in unattempted_regions(&regions, &outcomes) {
                println!("{region}\tskipped (rollout halted)");
            }
        }
    }

    if any_failure {
        crate::cli::exit_after_drain(3).await;
    }
    crate::cli::drain_before_return().await;
    Ok(())
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
        assert!(!p.yes && p.version.is_none());
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
        assert_eq!(p.version.as_deref(), Some("build-900"));
        assert_eq!(p.wait_for_green.as_deref(), Some("5m"));
        assert_eq!(p.auto_rollback.as_deref(), Some("10m"));
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
            assert_eq!(verb.audit_label(), *label, "{name} labels");
            assert!(!label.is_empty());
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
