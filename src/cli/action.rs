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

/// `ebman action rollout --version LABEL --regions r1,r2,r3 --env NAME [--yes] [--json] [--quiet]`
/// — cross-region sequential deploy with pre-flight + halt-on-fail
/// + single `rollout_id` correlation across audit lines.
///
/// Exit codes:
/// - 0 all regions dispatched successfully
/// - 1 AWS-layer error before any region dispatched
/// - 2 usage error
/// - 3 one or more region dispatches failed (rollout halted)
async fn run_rollout(args: &[String]) -> Result<()> {
    let mut env_name: Option<String> = None;
    let mut version: Option<String> = None;
    let mut regions_csv: Option<String> = None;
    let mut wait_for_green: Option<String> = None;
    let mut profile: Option<String> = None;
    let mut yes = false;
    let mut json = false;
    let mut quiet = false;
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

    let mut outcomes: Vec<(String, Result<(), String>)> = Vec::new();
    for (region, client) in &per_region {
        if !quiet {
            eprintln!("rollout: dispatching to {region} (env={env}, version={version})");
        }
        audit::append_rollout(&rollout_id, region, &env, &version, "dispatched", None);
        match client.deploy_version(&env, &version).await {
            Ok(()) => {
                outcomes.push((region.clone(), Ok(())));
                if let Some(secs) = wait_for_green_secs {
                    let start = tokio::time::Instant::now();
                    let wait_timeout_emitted = false;
                    loop {
                        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                        let envs = match client.list_environments().await {
                            Ok(envs) => envs,
                            Err(e) => {
                                eprintln!("rollout[{region}]: list_environments during poll: {e}");
                                outcomes.last_mut().unwrap().1 = Err(format!("poll: {e}"));
                                break;
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
                            Some(secs),
                            None,
                            wait_timeout_emitted,
                        ) {
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
                                outcomes.last_mut().unwrap().1 = Err(msg);
                                break;
                            }
                            PollDecision::DispatchRollback => break,
                        }
                    }
                }
            }
            Err(e) => {
                let msg = format!("deploy_version: {e}");
                outcomes.push((region.clone(), Err(msg.clone())));
                eprintln!("rollout[{region}]: {msg}");
            }
        }
        let last_err = outcomes.last().and_then(|(_, r)| r.as_ref().err()).cloned();
        audit::append_rollout(
            &rollout_id,
            region,
            &env,
            &version,
            "completed",
            last_err.as_deref(),
        );
        if matches!(outcomes.last(), Some((_, Err(_)))) {
            break;
        }
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
