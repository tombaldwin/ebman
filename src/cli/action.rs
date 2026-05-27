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

pub async fn run(args: &[String]) -> Result<()> {
    let action_name = args.get(1).map(|s| s.as_str()).unwrap_or("");
    if action_name.is_empty() || action_name.starts_with('-') {
        eprintln!(
            "usage: ebman action <rebuild|restart|terminate|deploy|rollout> --env NAME [--version LABEL] [--regions r1,r2,r3] [--yes] [--wait-for-green Nm] [--auto-rollback Nm]"
        );
        std::process::exit(2);
    }
    if action_name == "rollout" {
        return run_rollout(args).await;
    }
    let mut env_name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut wait_for_green: Option<String> = None;
    let mut auto_rollback: Option<String> = None;
    let mut yes = false;
    let mut iter = args.iter().skip(2);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = iter.next().cloned(),
            "--version" => version = iter.next().cloned(),
            "--wait-for-green" => wait_for_green = iter.next().cloned(),
            "--auto-rollback" => auto_rollback = iter.next().cloned(),
            "--yes" => yes = true,
            other => {
                eprintln!("ebman action: unknown flag '{other}'");
                std::process::exit(2);
            }
        }
    }
    let Some(env) = env_name else {
        eprintln!("ebman action: --env NAME is required");
        std::process::exit(2);
    };
    let destructive = matches!(action_name, "terminate");
    if destructive && !yes {
        eprintln!("ebman action: '{action_name}' is destructive; re-run with --yes to confirm");
        std::process::exit(3);
    }
    let aws = aws::AwsClient::with(None, None).await?;

    if action_name == "deploy" {
        return run_deploy(&aws, &env, version, wait_for_green, auto_rollback).await;
    }

    let result = match action_name {
        "rebuild" => aws.rebuild_env(&env).await,
        "restart" => aws.restart_app_server(&env).await,
        "terminate" => aws.terminate_env(&env).await,
        other => {
            eprintln!("ebman action: unknown action '{other}'");
            std::process::exit(2);
        }
    };
    match result {
        Ok(()) => {
            println!("ok: {action_name} on {env} dispatched");
            Ok(())
        }
        Err(e) => {
            eprintln!("err: {e}");
            std::process::exit(1);
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
    if auto_rollback_secs.is_some() && snapshot_label.is_empty() {
        eprintln!(
            "ebman action deploy: --auto-rollback requested but env '{env}' has no prior version to roll back to"
        );
        std::process::exit(2);
    }

    println!("dispatching deploy: env={env} version={version}");
    if let Err(e) = aws.deploy_version(env, &version).await {
        eprintln!("err: deploy_version: {e}");
        std::process::exit(1);
    }

    if wait_for_green_secs.is_none() && auto_rollback_secs.is_none() {
        println!("ok: deploy on {env} dispatched (version={version})");
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
                std::process::exit(1);
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
                return Ok(());
            }
            PollDecision::WaitForGreenTimeout => {
                wait_for_green_timeout_emitted = true;
                if auto_rollback_secs.is_none() {
                    eprintln!(
                        "timeout: deploy on {env} did not reach Green within {}s (status={status}, health={health}, version={version})",
                        wait_for_green_secs.unwrap_or(0)
                    );
                    std::process::exit(4);
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
                if let Err(e) = aws.deploy_version(env, &snapshot_label).await {
                    eprintln!("err: rollback deploy_version: {e}");
                    std::process::exit(1);
                }
                println!("ok: rollback dispatched on {env} (version={snapshot_label})");
                std::process::exit(5);
            }
        }
    }
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
    audit::append_rollout(rollout_id, region, env, version, "dispatched", None);
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
        region,
        env,
        version,
        "completed",
        outcome.as_ref().err().map(String::as_str),
    );
    outcome
}

/// `ebman action rollout --version LABEL --regions r1,r2,r3 --env NAME --yes [...]`
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
            "--env" => env_name = iter.next().cloned(),
            "--version" => version = iter.next().cloned(),
            "--regions" => regions_csv = iter.next().cloned(),
            "--wait-for-green" => wait_for_green = iter.next().cloned(),
            "--profile" => profile = iter.next().cloned(),
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
            "--staggered" => staggered = iter.next().cloned(),
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
    let regions: Vec<String> = regions_csv
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if regions.is_empty() {
        eprintln!("ebman action rollout: --regions list is empty");
        std::process::exit(2);
    }
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
    if parallel && staggered_secs.is_some() {
        eprintln!(
            "ebman action rollout: --parallel and --staggered are mutually exclusive (--staggered requires sequential ordering)"
        );
        std::process::exit(2);
    }
    if !parallel && max_concurrency.is_some() {
        eprintln!("ebman action rollout: --max-concurrency only applies with --parallel");
        std::process::exit(2);
    }
    if staggered_secs.is_some() && wait_for_green_secs.is_none() {
        eprintln!(
            "ebman action rollout: --staggered requires --wait-for-green (staggering is timed from each region's Green observation)"
        );
        std::process::exit(2);
    }
    // --parallel implies --continue-on-fail. In-flight regions can't
    // be cancelled server-side, so "halt remaining" only makes sense
    // for un-started waves under --max-concurrency. For v1
    // simplicity, --parallel always attempts all regions.
    let continue_on_fail = continue_on_fail || parallel;

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
        let cap = max_concurrency.unwrap_or(per_region.len()).max(1);
        let mut queue: std::collections::VecDeque<(String, std::sync::Arc<aws::AwsClient>)> =
            per_region.into_iter().collect();
        // Seed initial batch.
        for _ in 0..cap.min(queue.len()) {
            let (region, client) = queue.pop_front().unwrap();
            let env_for = env.clone();
            let version_for = version.clone();
            let rollout_id_for = rollout_id.clone();
            let quiet_for = quiet;
            joinset.spawn(async move {
                let outcome = dispatch_one_region(
                    &client,
                    &env_for,
                    &version_for,
                    wait_for_green_secs,
                    &rollout_id_for,
                    &region,
                    quiet_for,
                )
                .await;
                (region, outcome)
            });
        }
        // Drain + reseed as capacity frees up.
        while let Some(joined) = joinset.join_next().await {
            let (region, outcome) =
                joined.unwrap_or_else(|e| (String::new(), Err(format!("join: {e}"))));
            outcomes.push((region, outcome));
            if let Some((next_region, next_client)) = queue.pop_front() {
                let env_for = env.clone();
                let version_for = version.clone();
                let rollout_id_for = rollout_id.clone();
                let quiet_for = quiet;
                joinset.spawn(async move {
                    let outcome = dispatch_one_region(
                        &next_client,
                        &env_for,
                        &version_for,
                        wait_for_green_secs,
                        &rollout_id_for,
                        &next_region,
                        quiet_for,
                    )
                    .await;
                    (next_region, outcome)
                });
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
            let attempted: std::collections::HashSet<&str> =
                outcomes.iter().map(|(r, _)| r.as_str()).collect();
            for region in &regions {
                if !attempted.contains(region.as_str()) {
                    out.push_str(&format!(
                        ",{{\"region\":\"{}\",\"ok\":false,\"err\":\"skipped (rollout halted)\"}}",
                        cli_esc(region)
                    ));
                }
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
            let attempted: std::collections::HashSet<&str> =
                outcomes.iter().map(|(r, _)| r.as_str()).collect();
            for region in &regions {
                if !attempted.contains(region.as_str()) {
                    println!("{region}\tskipped (rollout halted)");
                }
            }
        }
    }

    if any_failure {
        std::process::exit(3);
    }
    Ok(())
}
