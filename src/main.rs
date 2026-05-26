use std::{io, panic};

use color_eyre::eyre::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tracing_subscriber::{layer::SubscriberExt, reload, util::SubscriberInitExt, EnvFilter};

use ebman::{app::App, aws, config, control, font_probe, splash, util, LogReloadHandle, Tui};

#[tokio::main]
async fn main() -> Result<()> {
    // Handle CLI flags before any TUI / logging setup so they print cleanly.
    let mut read_only = false;
    let mut demo = false;
    let mut control_socket: Option<std::path::PathBuf> = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Subcommand support: `ebman envs [--json]`, `ebman action ACTION --env NAME --yes`,
    // `ebman ctl <op> …`. Falls through to the TUI when no subcommand is present.
    if let Some(first) = args.first() {
        match first.as_str() {
            "envs" => return run_envs_cli(&args).await,
            "action" => return run_action_cli(&args).await,
            "ctl" => return run_ctl_cli(&args).await,
            "lint" => return run_lint_cli(&args).await,
            "drift" => return run_drift_cli(&args).await,
            _ => {}
        }
    }
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!(
                    "ebman {}\nby Tom Baldwin · Polymorphism Ltd · https://polymorphism.co.uk",
                    env!("CARGO_PKG_VERSION")
                );
                return Ok(());
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "--read-only" => read_only = true,
            "--demo" => demo = true,
            "--control-socket" => {
                control_socket = iter.next().map(std::path::PathBuf::from);
                if control_socket.is_none() {
                    eprintln!("ebman: --control-socket requires a path argument");
                    std::process::exit(2);
                }
            }
            other if other.starts_with('-') => {
                eprintln!("ebman: unknown flag {other}\n");
                print_help();
                std::process::exit(2);
            }
            _ => {}
        }
    }

    color_eyre::install()?;
    let log_handle = init_logging()?;
    install_panic_hook();

    let mut cfg = config::load();
    // Resolve `icons = "auto"` *before* we enter the alt-screen so the probe
    // glyph never reaches the user's scrollback. Any non-auto value is
    // passed through untouched.
    cfg.icons = font_probe::resolve_icons_setting(&cfg.icons);
    // Capture the resolved icons setting before `cfg` is consumed by
    // `App::new` — `draw_splash` needs it to pick between the plain-text
    // tagline and the Powerline rounded-cap pill variant.
    let splash_icons = cfg.icons.clone();
    let mut terminal = enter_tui()?;

    // Animate the splash while App::new resolves (config load + STS + first
    // SDK setup). Keep the splash visible for at least SPLASH_MIN_DURATION even
    // if App::new returns sooner — gives the user a chance to actually see it.
    const SPLASH_MIN_DURATION: std::time::Duration = std::time::Duration::from_secs(3);

    let mut app_inst = if demo {
        // `--demo` mode: no STS round-trip, no `state::load`. App
        // construction is synchronous and instant — but still run the
        // splash animation for SPLASH_MIN_DURATION so VHS / asciinema
        // captures get the brand animation rather than jumping
        // straight to the table.
        let app = App::new_demo(cfg);
        let splash_started = std::time::Instant::now();
        let mut splash_frame: u64 = 0;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(30));
        while splash_started.elapsed() < SPLASH_MIN_DURATION {
            interval.tick().await;
            draw_splash(&mut terminal, splash_frame, &splash_icons)?;
            splash_frame = splash_frame.wrapping_add(1);
        }
        app
    } else {
        let splash_started = std::time::Instant::now();
        let mut splash_frame: u64 = 0;
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(30));
        let mut new_app_fut = Box::pin(App::new(cfg));
        let mut app_ready: Option<App> = None;
        loop {
            tokio::select! {
                biased;
                res = &mut new_app_fut, if app_ready.is_none() => {
                    app_ready = Some(res?);
                }
                _ = interval.tick() => {
                    draw_splash(&mut terminal, splash_frame, &splash_icons)?;
                    splash_frame = splash_frame.wrapping_add(1);
                    if app_ready.is_some() && splash_started.elapsed() >= SPLASH_MIN_DURATION {
                        break app_ready
                            .take()
                            .expect("app_ready was Some, just checked above");
                    }
                }
            }
        }
    };
    app_inst.read_only = read_only;
    app_inst.log_reload = Some(log_handle);

    // Optional control socket. Spawn the listener *after* the splash so the
    // socket is guaranteed to exist by the time the user can issue commands.
    let control_rx = control_socket.map(|path| {
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
        control::spawn_listener(path, tx);
        rx
    });

    let result = app_inst.run(&mut terminal, control_rx).await;
    // Belt-and-braces: persist state regardless of how `run()` exited.
    // The internal call at the end of `run()` only fires on the Ok path,
    // so a `terminal.draw()?` error mid-shutdown (which can happen when
    // cargo-watch SIGTERM's the process and the TTY is flaky) would
    // otherwise drop the latest persistence. This second call is cheap
    // and idempotent; if run() succeeded it just over-writes its own
    // earlier write with the same values.
    app_inst.persist_state();
    leave_tui(&mut terminal)?;
    // Honour a reload request from the control socket: re-exec the same
    // binary with the original argv so the parent shell's terminal is
    // reused by the new process. Done AFTER `leave_tui` so the old TUI
    // state (raw mode, alt-screen, mouse capture) is fully torn down
    // before the new process sets it back up.
    if result.is_ok() && app_inst.reload_requested {
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            let exe = std::env::current_exe()?;
            let argv: Vec<String> = std::env::args().skip(1).collect();
            let err = std::process::Command::new(exe).args(argv).exec();
            // `exec` only returns on failure.
            return Err(color_eyre::eyre::eyre!("reload exec failed: {err}"));
        }
        #[cfg(not(unix))]
        {
            eprintln!("ebman: reload is unix-only");
        }
    }
    result
}

fn print_help() {
    let version = env!("CARGO_PKG_VERSION");
    println!(
        "\
ebman {version}
k9s-style TUI for AWS Elastic Beanstalk.

USAGE:
    ebman [FLAGS]

FLAGS:
    -V, --version           Print version and exit.
    -h, --help              Print this help and exit.
        --read-only         Start with destructive actions disabled (also toggleable with :readonly).
        --demo              Run with a hand-crafted synthetic fleet (no AWS calls, no disk reads).
                            Use for screenshots / VHS recordings / talk demos that shouldn't show
                            real account data. Drill-into-other-tabs may show stub errors — main
                            table + Detail/Health is the supported surface.
        --control-socket P  Open a Unix socket at P for remote control (off by default).
                            Pair with `ebman ctl <op>` to drive the running session.

SUBCOMMANDS:
    envs [--json]                                List environments in current profile / region.
    lint [--env NAME] [--json] [--severity LVL] [--rules ID1,ID2] [--quiet]
                                                  Run the diagnostic rule engine against one env
                                                  (or every env in the context) and emit findings
                                                  as text or JSON. Non-zero exit when issues found.
                                                  Useful for git hooks, CI gates, monitoring loops.
                                                  Exit codes: 0 clean, 1 aws err, 2 usage, 3 issues.
                                                  Operator disables via `lint.disable` in
                                                  config.toml and project-local .ebman/ebman.toml.
    drift [--env NAME] [--tfstate PATH] [--tfdir PATH] [--json] [--quiet]
                                                  Terraform drift report. Discovers tfstate via
                                                  walk-up from cwd (or --tfdir / --tfstate
                                                  overrides). Compares tf-declared option settings
                                                  + version_label against live EB state.
                                                  Exit codes: 0 no drift, 1 aws err, 2 usage,
                                                  3 drift detected. CI-friendly default exit code.
    action ACTION --env NAME [--yes]             Run an action (rebuild|restart|terminate|deploy) on an env.
                                                  Terminate requires --yes to confirm.
                                                  Deploy requires --version LABEL; supports
                                                  --wait-for-green Nm and --auto-rollback Nm.
                                                  Exit codes: 0 ok, 1 aws err, 2 usage, 4 wait-timeout, 5 rolled-back.
    ctl <screen|key|cmd|state|reload> [args]     Talk to a running ebman via --control-socket.
                                                  `reload` re-execs the binary (rebuild first via
                                                  `cargo build --release`). Use --socket PATH to
                                                  override the default location.

CONFIG:
    ~/.config/ebman/config.toml   user configuration (see README)
    ~/.config/ebman/state.toml    persisted session state (managed by the app)
    ~/.cache/ebman/ebman.log      log output (filter with RUST_LOG)

KEYS:
    Once running, press '?' for the in-app help screen."
    );
}

/// `ebman lint [--env NAME] [--json] [--severity warn] [--rules ID1,ID2] [--quiet]`
/// — scriptable surface for git hooks / CI gates / monitoring
/// tools. Shares the rule engine in `ebman::lint` with the
/// `:lint` TUI overlay; the only difference is the output
/// format and the exit-code surface.
///
/// Exit codes (per the 0.13 CLI charter):
/// - 0 clean (no issues at or above the filter severity)
/// - 1 AWS-layer error
/// - 2 usage error (unknown flag, bad severity, env not found)
/// - 3 issues found (CI gate non-zero)
///
/// `--quiet` suppresses text output (paired with `--json`, or
/// for "I just want the exit code" use cases). `--json` emits a
/// single `{"issues":[...]}` blob to stdout — same shape the
/// engine's `render_issues_json` produces.
/// `ebman drift [--env NAME] [--tfstate PATH] [--tfdir PATH] [--json] [--quiet]`
/// — terraform drift report for CI gates / git hooks. Detects
/// tfstate via the same walk-up logic the TUI uses, OR honors
/// `--tfstate PATH` (explicit file) / `--tfdir PATH` (walk-up
/// rooted at PATH instead of cwd). Compares tf-declared
/// option_settings + version_label against live EB state and
/// emits the per-env drift report.
///
/// Exit codes (per the 0.13 CLI charter):
/// - 0 no drift (or no tf-managed envs in scope)
/// - 1 AWS-layer error
/// - 2 usage error (unknown flag, env not found, tfstate
///   couldn't be parsed)
/// - 3 drift detected — non-zero by default so CI scripts can
///   gate `terraform plan` on a clean ebman state
async fn run_drift_cli(args: &[String]) -> Result<()> {
    let mut env_name: Option<String> = None;
    let mut tfstate_path: Option<std::path::PathBuf> = None;
    let mut tfdir: Option<std::path::PathBuf> = None;
    let mut json = false;
    let mut quiet = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = iter.next().cloned(),
            "--tfstate" => tfstate_path = iter.next().map(std::path::PathBuf::from),
            "--tfdir" => tfdir = iter.next().map(std::path::PathBuf::from),
            "--json" => json = true,
            "--quiet" => quiet = true,
            other => {
                eprintln!("ebman drift: unknown flag '{other}'");
                std::process::exit(2);
            }
        }
    }

    // Locate tfstate. Priority: --tfstate explicit path > --tfdir
    // walk-up > cwd walk-up.
    let (tf_state, used_path) = if let Some(path) = tfstate_path.as_ref() {
        let Some(state) = ebman::terraform::load_from_path(path) else {
            eprintln!(
                "ebman drift: could not read or parse tfstate at {}",
                path.display()
            );
            std::process::exit(2);
        };
        (state, Some(path.clone()))
    } else {
        let start = tfdir
            .as_deref()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf();
        let abs = start.canonicalize().unwrap_or(start);
        let Some(found) = ebman::terraform::find_tfstate(&abs) else {
            if !quiet {
                if json {
                    println!("{{\"tfstate\":null,\"envs\":[]}}");
                } else {
                    eprintln!(
                        "ebman drift: no terraform.tfstate found under {}",
                        abs.display()
                    );
                }
            }
            // No tfstate isn't an error — exit 0 (clean). Same
            // shape as `terraform plan` against a project with
            // no resources.
            return Ok(());
        };
        let Some(state) = ebman::terraform::load_from_path(&found) else {
            eprintln!(
                "ebman drift: could not parse tfstate at {}",
                found.display()
            );
            std::process::exit(2);
        };
        (state, Some(found))
    };

    let aws = aws::AwsClient::with(None, None).await?;
    let live_envs = aws
        .list_environments()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_environments: {e}"))?;

    // Build the work list. With --env, scope to that env (and
    // refuse if it's not tf-managed — operator typo'd a non-
    // managed name); without, run against every tf-managed env
    // in the live fleet.
    let targets: Vec<&ebman::aws::Environment> = match env_name.as_deref() {
        Some(name) => {
            let Some(env) = live_envs.iter().find(|e| e.name == name) else {
                eprintln!("ebman drift: env '{name}' not found in current context");
                std::process::exit(2);
            };
            vec![env]
        }
        None => live_envs
            .iter()
            .filter(|e| tf_state.env_by_name(&e.name).is_some())
            .collect(),
    };

    let mut reports: Vec<(String, bool, Vec<ebman::terraform::DriftField>)> = Vec::new();
    let mut any_drift = false;
    for env in targets {
        let tf_env = tf_state.env_by_name(&env.name);
        let tf_managed = tf_env.is_some();
        let drift = if let Some(tf) = tf_env {
            match aws
                .fetch_env_option_settings(&env.application, &env.name)
                .await
            {
                Ok(opts) => ebman::terraform::compute_drift(tf, env, &opts),
                Err(e) => {
                    if !quiet {
                        eprintln!(
                            "warning: skipping {} — fetch_env_option_settings: {e}",
                            env.name
                        );
                    }
                    Vec::new()
                }
            }
        } else {
            Vec::new()
        };
        if !drift.is_empty() {
            any_drift = true;
        }
        reports.push((env.name.clone(), tf_managed, drift));
    }

    // Output.
    if !quiet {
        if json {
            println!(
                "{}",
                ebman::terraform::render_drift_json(used_path.as_deref(), &reports)
            );
        } else {
            for (env, managed, drift) in &reports {
                if drift.is_empty() {
                    if *managed {
                        println!("{env}\t✓ no drift");
                    }
                    continue;
                }
                for d in drift {
                    let target = match (d.namespace.as_deref(), d.name.as_deref()) {
                        (Some(ns), Some(n)) => format!("{ns}/{n}"),
                        (_, Some(n)) => n.to_string(),
                        _ => d.kind.clone(),
                    };
                    println!(
                        "{env}\t{}\t{target}\ttf={}\tlive={}",
                        d.kind, d.tf_value, d.live_value
                    );
                }
            }
        }
    }

    if any_drift {
        std::process::exit(3);
    }
    Ok(())
}

async fn run_lint_cli(args: &[String]) -> Result<()> {
    let mut env_name: Option<String> = None;
    let mut json = false;
    let mut quiet = false;
    let mut severity_filter: Option<ebman::lint::Severity> = None;
    let mut rule_filter: Vec<String> = Vec::new();
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = iter.next().cloned(),
            "--json" => json = true,
            "--quiet" => quiet = true,
            "--severity" => {
                let Some(v) = iter.next() else {
                    eprintln!("ebman lint: --severity expects a value (info / warn / error)");
                    std::process::exit(2);
                };
                let Some(sev) = ebman::lint::Severity::parse(v) else {
                    eprintln!("ebman lint: unknown severity '{v}' (info / warn / error)");
                    std::process::exit(2);
                };
                severity_filter = Some(sev);
            }
            "--rules" => {
                let Some(v) = iter.next() else {
                    eprintln!("ebman lint: --rules expects a comma-separated rule id list");
                    std::process::exit(2);
                };
                rule_filter = v
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            other => {
                eprintln!("ebman lint: unknown flag '{other}'");
                std::process::exit(2);
            }
        }
    }

    let aws = aws::AwsClient::with(None, None).await?;
    let envs = aws
        .list_environments()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_environments: {e}"))?;

    // If --env is given, scope to that single env. Otherwise run
    // against every env in the current context — operators using
    // `ebman lint --json | jq` for fleet-wide CI gating get a
    // single combined report.
    let targets: Vec<&ebman::aws::Environment> = match env_name.as_deref() {
        Some(name) => {
            let Some(env) = envs.iter().find(|e| e.name == name) else {
                eprintln!("ebman lint: env '{name}' not found in current context");
                std::process::exit(2);
            };
            vec![env]
        }
        None => envs.iter().collect(),
    };

    // Operator-tunable disables — compose user-level
    // `lint.disable = "..."` from config.toml with project-local
    // `[lint].disable = [...]` from .ebman/ebman.toml. Project
    // entries extend the global set; nothing overrides (same
    // mental model as the runbooks merge).
    let mut disabled: Vec<String> = config::load_lint_disables();
    disabled.extend(ebman::project::load_lint_disables_from_cwd());
    let rules = ebman::lint::default_rules(&disabled);

    let mut all_issues: Vec<ebman::lint::Issue> = Vec::new();
    for env in targets {
        // Per-env option-settings fetch. Errors on a single env
        // get surfaced (eprintln to stderr) but don't abort the
        // full run — partial reports are better than no report
        // for CI use cases.
        let opts = match aws
            .fetch_env_option_settings(&env.application, &env.name)
            .await
        {
            Ok(opts) => opts,
            Err(e) => {
                if !quiet {
                    eprintln!(
                        "warning: skipping {} — fetch_env_option_settings: {e}",
                        env.name
                    );
                }
                continue;
            }
        };
        let ctx = ebman::lint::LintContext {
            env,
            options: &opts,
            events: &[],
            cost_usd_per_month: None,
            latest_stack_version: None,
        };
        let mut issues = ebman::lint::run_rules(&rules, &ctx);
        // Apply --severity floor.
        if let Some(min) = severity_filter {
            issues.retain(|i| i.severity >= min);
        }
        // Apply --rules whitelist (after severity so the two flags
        // compose as expected — show only these rules, and only at
        // this severity-or-higher).
        if !rule_filter.is_empty() {
            issues.retain(|i| rule_filter.contains(&i.rule_id));
        }
        all_issues.extend(issues);
    }

    // Output.
    if !quiet {
        if json {
            println!("{}", ebman::lint::render_issues_json(&all_issues));
        } else if all_issues.is_empty() {
            println!("✓ No issues found");
        } else {
            for issue in &all_issues {
                let sev = issue.severity.as_str();
                let env_str = issue.env_name.as_deref().unwrap_or("-");
                println!("{sev}\t{}\t{env_str}\t{}", issue.rule_id, issue.title);
                if let Some(s) = &issue.suggestion {
                    println!("\t→ {s}");
                }
            }
        }
    }

    // Non-zero exit if any issues survived the filters. CI scripts
    // get the natural `ebman lint && deploy` semantic.
    if all_issues.is_empty() {
        Ok(())
    } else {
        std::process::exit(3);
    }
}

async fn run_envs_cli(args: &[String]) -> Result<()> {
    let json = args.iter().any(|a| a == "--json");
    let aws = aws::AwsClient::with(None, None).await?;
    let envs = aws
        .list_environments()
        .await
        .map_err(|e| color_eyre::eyre::eyre!("list_environments: {e}"))?;
    if json {
        // Hand-rolled JSON to avoid pulling serde_json. Schema is flat and stable.
        let entries: Vec<String> = envs
            .iter()
            .map(|e| {
                format!(
                    "{{\"name\":\"{}\",\"application\":\"{}\",\"status\":\"{}\",\"health\":\"{}\",\"platform\":\"{}\",\"cname\":\"{}\",\"version_label\":\"{}\"}}",
                    cli_esc(&e.name),
                    cli_esc(&e.application),
                    cli_esc(&e.status),
                    cli_esc(&e.health),
                    cli_esc(&e.platform),
                    cli_esc(&e.cname),
                    cli_esc(&e.version_label),
                )
            })
            .collect();
        println!("[{}]", entries.join(","));
    } else {
        println!("NAME\tAPPLICATION\tSTATUS\tHEALTH\tPLATFORM\tCNAME\tVERSION");
        for e in &envs {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                e.name, e.application, e.status, e.health, e.platform, e.cname, e.version_label
            );
        }
    }
    Ok(())
}

/// One tick's worth of decision for the `action deploy` polling
/// loop. Pure — no AWS, no clock, no I/O — so the exit-code
/// matrix is unit-testable.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PollDecision {
    /// Env hasn't crossed Green and no deadline has elapsed yet.
    KeepPolling,
    /// Env reached Green/Ok — exit 0.
    Success,
    /// `--wait-for-green` deadline elapsed but rollback hasn't
    /// fired (either no rollback flag, or its deadline is later).
    /// Caller emits a milestone log and keeps polling if rollback
    /// is still pending; otherwise exits with the timeout code.
    WaitForGreenTimeout,
    /// `--auto-rollback` deadline elapsed and env still non-Green.
    /// Caller dispatches the snapshot redeploy + exits with the
    /// rollback code.
    DispatchRollback,
}

pub(crate) fn decide_poll(
    status: &str,
    health: &str,
    elapsed_secs: u64,
    wait_for_green_secs: Option<u64>,
    auto_rollback_secs: Option<u64>,
    wait_for_green_timeout_emitted: bool,
) -> PollDecision {
    // Both status=Ready AND health=Green/Ok required — EB
    // briefly leaves health Green while status flips to
    // Updating right after UpdateEnvironment, so a check on
    // health alone false-positives during the transition.
    // See `ebman::app::deploy_settled_green`.
    if ebman::app::deploy_settled_green(status, health) {
        return PollDecision::Success;
    }
    if let Some(d) = auto_rollback_secs {
        if elapsed_secs >= d {
            return PollDecision::DispatchRollback;
        }
    }
    if let Some(d) = wait_for_green_secs {
        if elapsed_secs >= d && !wait_for_green_timeout_emitted {
            return PollDecision::WaitForGreenTimeout;
        }
    }
    PollDecision::KeepPolling
}

async fn run_action_cli(args: &[String]) -> Result<()> {
    // Expected shape: ebman action ACTION --env NAME [--yes] [...flags]
    let action_name = args.get(1).map(|s| s.as_str()).unwrap_or("");
    if action_name.is_empty() || action_name.starts_with('-') {
        eprintln!(
            "usage: ebman action <rebuild|restart|terminate|deploy> --env NAME [--version LABEL] [--yes] [--wait-for-green Nm] [--auto-rollback Nm]"
        );
        std::process::exit(2);
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
        return run_action_deploy(&aws, &env, version, wait_for_green, auto_rollback).await;
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
/// typed-command `:deploy` path. Exit codes are deliberately
/// distinct so CI / shell wrappers can branch on outcome:
///   0  — deploy dispatched (and reached Green if asked)
///   1  — AWS-layer error (UpdateEnvironment, list_environments)
///   2  — usage error (missing flags, malformed duration)
///   4  — `--wait-for-green` deadline elapsed without Green
///   5  — `--auto-rollback` deadline elapsed; rollback dispatched
async fn run_action_deploy(
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
    // Same duration grammar as the TUI path so operators don't
    // relearn it. `parse_window_ms` returns ms; divide for seconds.
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

    // Capture pre-deploy snapshot — needed if --auto-rollback fires.
    // Look up the env first to verify it exists and grab the current
    // version_label.
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
    // If --auto-rollback is set but the env has no current version
    // (e.g. brand-new env with no prior deploy), there's nothing to
    // roll back to. Refuse upfront rather than letting the redeploy
    // fail at AWS with a confusing error.
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

    // No polling flags → done. Same as the existing rebuild / restart
    // path: dispatch and exit.
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
                // Rollback still pending — emit milestone, keep polling.
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

/// Subcommand: `ebman ctl <op> [args] [--socket PATH]`. Opens a one-shot Unix
/// socket connection to a running ebman process and prints the response.
async fn run_ctl_cli(args: &[String]) -> Result<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::UnixStream;
    let mut socket_path = control::default_socket_path();
    let mut rest: Vec<&str> = Vec::new();
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        if arg == "--socket" {
            if let Some(p) = iter.next() {
                socket_path = std::path::PathBuf::from(p);
            } else {
                eprintln!("ebman ctl: --socket requires a path");
                std::process::exit(2);
            }
        } else {
            rest.push(arg.as_str());
        }
    }
    if rest.is_empty() {
        eprintln!(
            "usage: ebman ctl <screen|key|cmd|state> [args]  [--socket PATH]\n\
             examples:\n  ebman ctl screen\n  ebman ctl key Down\n  ebman ctl key Ctrl+R\n  \
             ebman ctl cmd region eu-west-2\n  ebman ctl state"
        );
        std::process::exit(2);
    }
    let head = rest[0].to_ascii_uppercase();
    let body = rest[1..].join(" ");
    let request = if body.is_empty() {
        head
    } else {
        format!("{head} {body}")
    };
    let mut stream = UnixStream::connect(&socket_path).await.map_err(|e| {
        color_eyre::eyre::eyre!(
            "ebman ctl: connect to {} failed: {e}\n  hint: start ebman with `--control-socket {}`",
            socket_path.display(),
            socket_path.display()
        )
    })?;
    stream.write_all(request.as_bytes()).await?;
    stream.write_all(b"\n").await?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await?;
    print!("{response}");
    if !response.ends_with('\n') {
        println!();
    }
    if response.starts_with("ERR ") {
        std::process::exit(1);
    }
    Ok(())
}

/// Minimal JSON-string escape for CLI output. Quotes and backslashes only;
/// EB names don't contain control chars.
fn cli_esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

const SPLASH_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn draw_splash(terminal: &mut Tui, frame: u64, icons: &str) -> Result<()> {
    use ratatui::layout::{Alignment, Constraint, Direction, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

    terminal.draw(|f| {
        let area = f.area();
        let powerline = icons == "powerline";
        // Two tiers: a roomy terminal gets the pixel-art giant scene
        // above the text; a smaller one falls back to a compact
        // text-only card so the splash never overflows. The card is
        // sized to its content and centred.
        let show_scene = splash::splash_shows_scene(area.width, area.height);
        // Card carries 2 rows of slack over its content so a future
        // text tweak can't silently clip.
        let (card_w, card_h): (u16, u16) = if show_scene { (46, 30) } else { (52, 9) };
        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(card_h),
                Constraint::Min(0),
            ])
            .split(area);
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Min(0),
                Constraint::Length(card_w),
                Constraint::Min(0),
            ])
            .split(v[1]);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(""));
        // 8-bit angry-giant-eats-the-beanstalk scene above the text.
        if show_scene {
            lines.extend(splash::splash_scene_lines(frame));
            lines.push(Line::from(""));
        }
        // In Powerline mode (resolved by `font_probe` before this runs, so
        // the PUA glyphs are guaranteed to render here), wrap the tagline +
        // byline in rounded-cap pills and lead the tagline with a Nerd Font
        // cloud icon so the bottom half of the splash card carries the
        // same Powerline aesthetic as the rest of the app. Falls back to
        // plain text in Unicode / ASCII so users without a Nerd Font don't
        // get tofu on first launch.
        if powerline {
            let tag_bg = Color::Rgb(35, 45, 60);
            let tag_fg = Color::Rgb(170, 180, 200);
            let cloud = "\u{f0c2}"; // fa-cloud, stable across Nerd Font releases
            lines.push(
                Line::from(vec![
                    Span::styled("\u{e0b6}", Style::default().fg(tag_bg)),
                    Span::styled(
                        format!(" {cloud}  k9s-style TUI for AWS Elastic Beanstalk "),
                        Style::default().fg(tag_fg).bg(tag_bg),
                    ),
                    Span::styled("\u{e0b4}", Style::default().fg(tag_bg)),
                ])
                .alignment(Alignment::Center),
            );
            let by_bg = Color::Rgb(50, 40, 75);
            let by_fg = Color::Rgb(220, 195, 245);
            lines.push(
                Line::from(vec![
                    Span::styled("\u{e0b6}", Style::default().fg(by_bg)),
                    Span::styled(
                        " by Tom Baldwin · Polymorphism Ltd ",
                        Style::default().fg(by_fg).bg(by_bg),
                    ),
                    Span::styled("\u{e0b4}", Style::default().fg(by_bg)),
                ])
                .alignment(Alignment::Center),
            );
        } else {
            lines.push(
                Line::from(Span::styled(
                    "k9s-style TUI for AWS Elastic Beanstalk",
                    Style::default().fg(Color::Rgb(150, 155, 170)),
                ))
                .alignment(Alignment::Center),
            );
            lines.push(
                Line::from(Span::styled(
                    "by Tom Baldwin · Polymorphism Ltd",
                    Style::default().fg(Color::Rgb(180, 140, 230)),
                ))
                .alignment(Alignment::Center),
            );
        }
        lines.push(Line::from(""));
        // Spinner + dot-cycle so "connecting" feels alive even when the SDK is
        // taking its time.
        // Spinner: advance every 3 frames → ~10 fps spin at 30 ms ticks.
        // Dots: advance every 8 frames → ~240 ms per dot.
        let spinner = SPLASH_SPINNER[(frame as usize / 3) % SPLASH_SPINNER.len()];
        let dots = ".".repeat((frame as usize / 8) % 4);
        lines.push(
            Line::from(Span::styled(
                format!("{spinner} connecting to AWS{dots}"),
                Style::default()
                    .fg(Color::Rgb(255, 200, 120))
                    .add_modifier(Modifier::BOLD),
            ))
            .alignment(Alignment::Center),
        );

        // One-shot border glow: warms cyan → magenta → cyan over the
        // first ~1 s of the splash, then settles to cyan.
        const BORDER_SPOTLIGHT_FRAMES: f64 = 33.0;
        let border_phase = (frame as f64 / BORDER_SPOTLIGHT_FRAMES).clamp(0.0, 1.0);
        // Triangle: 0 → 1 → 0 across the pass.
        let border_glow = if border_phase < 0.5 {
            border_phase * 2.0
        } else {
            (1.0 - border_phase) * 2.0
        };
        let border_hue = 180.0 + border_glow * 120.0;
        let (br, bg, bb) = hsl_to_rgb(border_hue, 0.60, 0.65);
        let mut block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(br, bg, bb)));
        // In Powerline mode, embed a `v{VERSION}` pill on the top border so
        // the card reads as a labelled tab. Pill bg sits on top of the
        // border line; rounded caps complete the tab shape. Falls back to
        // no title in Unicode / ASCII to avoid PUA glyph tofu.
        if powerline {
            let tab_bg = Color::Rgb(60, 50, 80);
            let tab_fg = Color::Rgb(220, 200, 250);
            let version = format!(" v{} ", env!("CARGO_PKG_VERSION"));
            let title = Line::from(vec![
                Span::styled("\u{e0b6}", Style::default().fg(tab_bg)),
                Span::styled(
                    version,
                    Style::default()
                        .fg(tab_fg)
                        .bg(tab_bg)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled("\u{e0b4}", Style::default().fg(tab_bg)),
            ]);
            block = block.title(title).title_alignment(Alignment::Center);
        }
        f.render_widget(Paragraph::new(lines).block(block), h[1]);
    })?;
    Ok(())
}

/// Standard HSL → RGB. `h` in degrees 0-360, `s` and `l` in 0.0-1.0.
fn hsl_to_rgb(h: f64, s: f64, l: f64) -> (u8, u8, u8) {
    let h = h.rem_euclid(360.0);
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let h_prime = h / 60.0;
    let x = c * (1.0 - (h_prime.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = if h_prime < 1.0 {
        (c, x, 0.0)
    } else if h_prime < 2.0 {
        (x, c, 0.0)
    } else if h_prime < 3.0 {
        (0.0, c, x)
    } else if h_prime < 4.0 {
        (0.0, x, c)
    } else if h_prime < 5.0 {
        (x, 0.0, c)
    } else {
        (c, 0.0, x)
    };
    let m = l - c / 2.0;
    let to_u8 = |v: f64| ((v + m) * 255.0).round().clamp(0.0, 255.0) as u8;
    (to_u8(r1), to_u8(g1), to_u8(b1))
}

fn enter_tui() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    // Set the terminal window title via OSC 2. Most modern terminals
    // (xterm / iTerm2 / Terminal.app / Ghostty / Alacritty / WezTerm /
    // VS Code's terminal) honour this; ones that don't ignore the
    // sequence silently. Done after EnterAlternateScreen so the
    // shell's prompt-driven title is replaced cleanly; leave_tui
    // doesn't restore the prior title — the next shell prompt's
    // PS1-style title hook will overwrite anyway.
    execute!(stdout, crossterm::terminal::SetTitle("ebman"))?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn leave_tui(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn install_panic_hook() {
    let original = panic::take_hook();
    panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, DisableMouseCapture);
        write_crash_report(info);
        original(info);
    }));
}

fn write_crash_report(info: &panic::PanicHookInfo<'_>) {
    let dir = util::cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    prune_old_crash_reports(&dir, MAX_CRASH_REPORTS);
    let ts = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let path = dir.join(format!("crash-{ts}.log"));
    let location = info
        .location()
        .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
        .unwrap_or_else(|| "unknown".into());
    let payload = info
        .payload()
        .downcast_ref::<&str>()
        .map(|s| (*s).to_string())
        .or_else(|| info.payload().downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "<non-string panic payload>".into());
    let backtrace = std::backtrace::Backtrace::force_capture();
    let report = format!(
        "ebman {} crashed at {ts}\n\
         location: {location}\n\
         payload:  {payload}\n\
         \n--- backtrace ---\n{backtrace}\n",
        env!("CARGO_PKG_VERSION")
    );
    let _ = std::fs::write(&path, report);
    eprintln!("ebman: crash report written to {}", path.display());
}

/// Keep at most `keep` of the oldest `crash-*.log` files in `dir`. Anything
/// older is deleted. Best-effort; any I/O error is silently ignored so the
/// crash hook stays minimal.
const MAX_CRASH_REPORTS: usize = 10;
/// Crash reports older than this are deleted regardless of the count cap.
/// Old crash logs become unactionable quickly — keep a month's window so we
/// catch repeat-offender bugs but don't accumulate forever.
const CRASH_REPORT_MAX_AGE_DAYS: u64 = 30;

fn prune_old_crash_reports(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut crashes: Vec<std::path::PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .map(|n| n.starts_with("crash-") && n.ends_with(".log"))
                .unwrap_or(false)
        })
        .collect();
    // Age-based purge: drop anything older than CRASH_REPORT_MAX_AGE_DAYS
    // even if we're under the count cap. Old crashes are seldom useful.
    let age_cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(
            CRASH_REPORT_MAX_AGE_DAYS * 24 * 3600,
        ))
        .unwrap_or(std::time::UNIX_EPOCH);
    crashes.retain(|p| {
        let too_old = std::fs::metadata(p)
            .and_then(|m| m.modified())
            .map(|t| t < age_cutoff)
            .unwrap_or(false);
        if too_old {
            let _ = std::fs::remove_file(p);
        }
        !too_old
    });
    if crashes.len() < keep {
        return;
    }
    // Sort by filename — the timestamp is part of the name, so lexicographic
    // order matches chronological order. Drop everything before the tail.
    crashes.sort();
    let drop_count = crashes.len().saturating_sub(keep - 1);
    for p in crashes.into_iter().take(drop_count) {
        let _ = std::fs::remove_file(p);
    }
}

fn init_logging() -> Result<LogReloadHandle> {
    let log_dir = dirs_log_dir();
    std::fs::create_dir_all(&log_dir).ok();
    let file_appender = tracing_appender::rolling::never(log_dir, "ebman.log");

    let env_filter = EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| EnvFilter::new("info,aws=warn,hyper=warn"));
    let (filter_layer, handle) = reload::Layer::new(env_filter);

    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_writer(file_appender)
        .with_ansi(false);

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();

    Ok(handle)
}

fn dirs_log_dir() -> std::path::PathBuf {
    util::cache_dir()
}

#[cfg(test)]
mod tests {
    use super::{cli_esc, decide_poll, hsl_to_rgb, prune_old_crash_reports, PollDecision};

    #[test]
    fn decide_poll_green_plus_ready_returns_success_regardless_of_deadlines() {
        // Settled-green observation always wins — even if a
        // deadline has also elapsed, the deploy is done.
        assert_eq!(
            decide_poll("Ready", "Green", 600, Some(300), Some(600), false),
            PollDecision::Success
        );
        assert_eq!(
            decide_poll("Ready", "Ok", 0, None, None, false),
            PollDecision::Success
        );
        // Case-insensitive on both status + health.
        assert_eq!(
            decide_poll("ready", "ok", 0, Some(300), None, false),
            PollDecision::Success
        );
    }

    #[test]
    fn decide_poll_green_during_updating_is_not_success() {
        // EB leaves health=Green briefly while status flips to
        // Updating right after UpdateEnvironment. Must keep polling
        // — a Success here would false-positive disarm before the
        // deploy actually starts rolling.
        assert_eq!(
            decide_poll("Updating", "Green", 5, Some(300), Some(600), false),
            PollDecision::KeepPolling
        );
        // Same for Launching (initial env creation) and Terminating.
        assert_eq!(
            decide_poll("Launching", "Green", 5, Some(300), None, false),
            PollDecision::KeepPolling
        );
    }

    #[test]
    fn decide_poll_keep_polling_before_any_deadline() {
        // Env still non-Green and no deadline reached → keep going.
        assert_eq!(
            decide_poll("Ready", "Red", 30, Some(300), Some(600), false),
            PollDecision::KeepPolling
        );
        // No flags + non-Green also means keep polling — caller
        // should have skipped the loop entirely if no flags set.
        assert_eq!(
            decide_poll("Updating", "Yellow", 60, None, None, false),
            PollDecision::KeepPolling
        );
    }

    #[test]
    fn decide_poll_wait_for_green_only_emits_timeout_once() {
        // Only --wait-for-green set; deadline elapsed; emit timeout.
        assert_eq!(
            decide_poll("Ready", "Red", 301, Some(300), None, false),
            PollDecision::WaitForGreenTimeout
        );
        // After emission, don't fire again on subsequent ticks —
        // caller already exited if no rollback, or logged + moved on.
        assert_eq!(
            decide_poll("Ready", "Red", 350, Some(300), None, true),
            PollDecision::KeepPolling
        );
    }

    #[test]
    fn decide_poll_rollback_wins_when_both_deadlines_passed() {
        // Both deadlines passed → rollback (more aggressive remediation)
        // wins. Even if wait-for-green hasn't emitted its timeout yet.
        assert_eq!(
            decide_poll("Ready", "Red", 700, Some(300), Some(600), false),
            PollDecision::DispatchRollback
        );
    }

    #[test]
    fn decide_poll_wait_then_rollback_sequence() {
        // Typical "both flags, wait shorter" timeline:
        //   t=200 → KeepPolling
        //   t=350 → WaitForGreenTimeout (wait=300, rollback=600)
        //   t=500 → KeepPolling (timeout already emitted)
        //   t=601 → DispatchRollback
        assert_eq!(
            decide_poll("Updating", "Yellow", 200, Some(300), Some(600), false),
            PollDecision::KeepPolling
        );
        assert_eq!(
            decide_poll("Ready", "Red", 350, Some(300), Some(600), false),
            PollDecision::WaitForGreenTimeout
        );
        assert_eq!(
            decide_poll("Ready", "Red", 500, Some(300), Some(600), true),
            PollDecision::KeepPolling
        );
        assert_eq!(
            decide_poll("Ready", "Red", 601, Some(300), Some(600), true),
            PollDecision::DispatchRollback
        );
    }

    #[test]
    fn decide_poll_rollback_only_no_intermediate_emission() {
        // --auto-rollback alone → no wait-for-green emission ever;
        // straight from KeepPolling to DispatchRollback at deadline.
        assert_eq!(
            decide_poll("Updating", "Yellow", 100, None, Some(300), false),
            PollDecision::KeepPolling
        );
        assert_eq!(
            decide_poll("Ready", "Red", 301, None, Some(300), false),
            PollDecision::DispatchRollback
        );
    }

    #[test]
    fn cli_esc_escapes_quotes_and_backslashes() {
        assert_eq!(cli_esc("hello"), "hello");
        assert_eq!(cli_esc("a\"b"), "a\\\"b");
        assert_eq!(cli_esc("a\\b"), "a\\\\b");
    }

    #[test]
    fn hsl_to_rgb_red() {
        let (r, g, b) = hsl_to_rgb(0.0, 1.0, 0.5);
        assert_eq!((r, g, b), (255, 0, 0));
    }

    #[test]
    fn hsl_to_rgb_cyan_and_magenta() {
        let (r, g, b) = hsl_to_rgb(180.0, 1.0, 0.5);
        assert_eq!((r, g, b), (0, 255, 255));
        let (r, g, b) = hsl_to_rgb(300.0, 1.0, 0.5);
        assert_eq!((r, g, b), (255, 0, 255));
    }

    #[test]
    fn prune_old_crash_reports_keeps_newest() {
        let dir = std::env::temp_dir().join(format!("ebman-prune-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Names sort lexicographically the same as chronologically.
        let names = [
            "crash-20260101T000000Z.log",
            "crash-20260102T000000Z.log",
            "crash-20260103T000000Z.log",
            "crash-20260104T000000Z.log",
            "crash-20260105T000000Z.log",
        ];
        for n in names {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        // Also drop in an unrelated file — must not be touched.
        std::fs::write(dir.join("not-a-crash.log"), b"y").unwrap();
        // keep=3 means "after the about-to-be-written report, total ≤ 3".
        // So with 5 existing files, the 3 oldest are dropped to make room.
        prune_old_crash_reports(&dir, 3);
        assert!(!dir.join(names[0]).exists());
        assert!(!dir.join(names[1]).exists());
        assert!(!dir.join(names[2]).exists());
        assert!(dir.join(names[3]).exists());
        assert!(dir.join(names[4]).exists());
        assert!(dir.join("not-a-crash.log").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_old_crash_reports_under_limit_is_noop() {
        let dir = std::env::temp_dir().join(format!("ebman-prune-under-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("crash-2026.log"), b"x").unwrap();
        prune_old_crash_reports(&dir, 5);
        assert!(dir.join("crash-2026.log").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_old_crash_reports_drops_files_past_ttl() {
        let dir = std::env::temp_dir().join(format!("ebman-prune-ttl-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let fresh = dir.join("crash-fresh.log");
        let stale = dir.join("crash-stale.log");
        std::fs::write(&fresh, b"x").unwrap();
        std::fs::write(&stale, b"x").unwrap();
        // Backdate the "stale" file's mtime to 60 days ago — past the
        // 30-day TTL the pruner enforces.
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(60 * 24 * 3600);
        let file = std::fs::File::open(&stale).unwrap();
        file.set_modified(past).unwrap();
        drop(file);
        // Under count cap (10) so age is the only reason to prune.
        prune_old_crash_reports(&dir, 10);
        assert!(fresh.exists(), "fresh file should survive");
        assert!(!stale.exists(), "stale file should be deleted by TTL");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn hsl_to_rgb_clamps_to_valid_range() {
        // u8 enforces 0..=255 by type, so additionally assert that moderate-saturation
        // mid-lightness inputs produce visible (non-collapsed) outputs across the wheel,
        // and that hue is wrapped modulo 360 (h=-30 should equal h=330).
        for h in [-30.0, 0.0, 90.0, 180.0, 270.0, 360.0, 720.0] {
            let (r, g, b) = hsl_to_rgb(h, 0.7, 0.65);
            let max = r.max(g).max(b);
            let min = r.min(g).min(b);
            assert!(max > min, "hue {h} collapsed to greyscale");
        }
        assert_eq!(hsl_to_rgb(-30.0, 0.7, 0.65), hsl_to_rgb(330.0, 0.7, 0.65));
        assert_eq!(hsl_to_rgb(0.0, 0.7, 0.65), hsl_to_rgb(360.0, 0.7, 0.65));
        // Zero saturation collapses to greyscale at lightness * 255.
        let (r, g, b) = hsl_to_rgb(123.0, 0.0, 0.5);
        assert_eq!(r, g);
        assert_eq!(g, b);
    }
}
