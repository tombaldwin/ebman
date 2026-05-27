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
            "audit" => return run_audit_cli(&args).await,
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
    lint [--env NAME] [--regions r1,r2,r3] [--json] [--severity LVL] [--rules ID1,ID2] [--quiet]
         [--fix (--yes | --dry-run)]
                                                  Run the diagnostic rule engine against one env
                                                  (or every env in the context) and emit findings
                                                  as text or JSON. Non-zero exit when issues found.
                                                  Useful for git hooks, CI gates, monitoring loops.
                                                  Exit codes: 0 clean, 1 aws err, 2 usage, 3 issues.
                                                  Operator disables via `lint.disable` in
                                                  config.toml and project-local .ebman/ebman.toml.
                                                  --regions fans out across regions; rows are
                                                  prefixed with the region.
                                                  --fix dispatches each rule's auto-remediation
                                                  (DeploymentPolicy → Rolling for EBL001, etc.).
                                                  Requires --yes to write; --dry-run prints the
                                                  plan without dispatching. Per-rule opt-out via
                                                  `lint.fix_disable`. Manual fixes printed as
                                                  instructions when the right answer is operator-
                                                  context-dependent.
    drift [--env NAME] [--regions r1,r2,r3] [--tfstate PATH] [--tfdir PATH] [--json] [--quiet]
                                                  Terraform drift report. Discovers tfstate via
                                                  walk-up from cwd (or --tfdir / --tfstate
                                                  overrides). Compares tf-declared option settings
                                                  + version_label against live EB state.
                                                  Exit codes: 0 no drift, 1 aws err, 2 usage,
                                                  3 drift detected. CI-friendly default exit code.
                                                  --regions fans out across regions against a
                                                  single tfstate (multi-region tf projects).
    action ACTION --env NAME [--yes]             Run an action (rebuild|restart|terminate|deploy|rollout) on an env.
                                                  Terminate requires --yes to confirm.
                                                  Deploy requires --version LABEL; supports
                                                  --wait-for-green Nm and --auto-rollback Nm.
                                                  Rollout: --version LABEL --regions r1,r2,r3 --env NAME
                                                  --yes [--wait-for-green Nm] [--json] [--profile P].
                                                  Sequential cross-region deploy with pre-flight
                                                  validation. Stops on first failure. Single
                                                  rollout_id correlation across audit lines.
                                                  Exit codes: 0 ok, 1 aws err, 2 usage, 3 partial
                                                  failure, 4 wait-timeout, 5 rolled-back.
    ctl <screen|key|cmd|state|reload> [args]     Talk to a running ebman via --control-socket.
                                                  `reload` re-execs the binary (rebuild first via
                                                  `cargo build --release`). Use --socket PATH to
                                                  override the default location.
    audit [--tail] [--since DUR] [--env NAME] [--rule ID] [--action NAME] [--json]
                                                  Read ~/.cache/ebman/audit.log — surface the local
                                                  audit trail for scripting / Slack-bot routing /
                                                  CI gating. Default text mode renders columns
                                                  (TS / REGION / STAGE / ACTION / TARGET / OUTCOME);
                                                  --json emits JSONL one entry per line. --tail
                                                  polls 1s for new entries (until Ctrl-C). --since
                                                  filters to entries within a duration (5m/1h/2d).
                                                  Exit codes: 0 ok, 1 io err, 2 usage.

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
    let mut regions_csv: Option<String> = None;
    let mut tfstate_path: Option<std::path::PathBuf> = None;
    let mut tfdir: Option<std::path::PathBuf> = None;
    let mut json = false;
    let mut quiet = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = iter.next().cloned(),
            "--regions" => regions_csv = iter.next().cloned(),
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
    // walk-up > cwd walk-up. ONE tfstate per invocation regardless
    // of --regions — common practice is a single state spanning
    // regions (terraform's `provider` blocks can target each
    // region in the same project). Operators with per-region
    // states run `ebman drift` separately per region.
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

    // Region fan-out — same shape as `ebman lint --regions`.
    // Without --regions, single iteration with the default
    // region (None → operator's AWS_REGION / profile default).
    let regions: Vec<Option<String>> = match regions_csv {
        Some(csv) => {
            let parsed: Vec<String> = csv
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if parsed.is_empty() {
                eprintln!("ebman drift: --regions list is empty");
                std::process::exit(2);
            }
            parsed.into_iter().map(Some).collect()
        }
        None => vec![None],
    };

    let multi_region = regions.len() > 1;
    // Reports are tuples of (region, env_name, tf_managed, drift)
    // so the output can group + label per region in multi-region
    // mode.
    let mut reports: Vec<(
        Option<String>,
        String,
        bool,
        Vec<ebman::terraform::DriftField>,
    )> = Vec::new();
    let mut any_drift = false;
    for region_opt in &regions {
        let aws = match aws::AwsClient::with(None, region_opt.clone()).await {
            Ok(c) => c,
            Err(e) => {
                if !quiet {
                    let region_label = region_opt.as_deref().unwrap_or("default");
                    eprintln!("warning: skipping region '{region_label}' — AwsClient::with: {e}");
                }
                continue;
            }
        };
        let live_envs = match aws.list_environments().await {
            Ok(envs) => envs,
            Err(e) => {
                if !quiet {
                    let region_label = region_opt.as_deref().unwrap_or("default");
                    eprintln!("warning: skipping region '{region_label}' — list_environments: {e}");
                }
                continue;
            }
        };

        // Build the per-region work list. With --env, scope to
        // that env in this region (skip with warning if not
        // present here in multi-region mode; hard-exit in single-
        // region mode). Without --env, run against every tf-
        // managed env in this region.
        let targets: Vec<&ebman::aws::Environment> = match env_name.as_deref() {
            Some(name) => match live_envs.iter().find(|e| e.name == name) {
                Some(env) => vec![env],
                None => {
                    if multi_region && !quiet {
                        let region_label = region_opt.as_deref().unwrap_or("default");
                        eprintln!(
                            "warning: env '{name}' not in region '{region_label}' — skipping"
                        );
                    } else if !multi_region {
                        eprintln!("ebman drift: env '{name}' not found in current context");
                        std::process::exit(2);
                    }
                    continue;
                }
            },
            None => live_envs
                .iter()
                .filter(|e| tf_state.env_by_name(&e.name).is_some())
                .collect(),
        };

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
            reports.push((region_opt.clone(), env.name.clone(), tf_managed, drift));
        }
    }

    // Output.
    if !quiet {
        if json {
            // The JSON renderer takes a `&[(String, bool, Vec<DriftField>)]`
            // shape (no region). For multi-region we tag each row
            // by re-using the env_name with `region/` prefix so
            // consumers can split on `/`. Single-region keeps
            // the existing shape.
            //
            // Future enhancement: a richer `render_drift_json_with_region`
            // that emits region as a structured field. Worth doing
            // if multi-region drift sees real usage.
            let shaped: Vec<(String, bool, Vec<ebman::terraform::DriftField>)> = reports
                .iter()
                .map(|(region, env, managed, drift)| {
                    let name = if multi_region {
                        if let Some(r) = region {
                            format!("{r}/{env}")
                        } else {
                            env.clone()
                        }
                    } else {
                        env.clone()
                    };
                    (name, *managed, drift.clone())
                })
                .collect();
            println!(
                "{}",
                ebman::terraform::render_drift_json(used_path.as_deref(), &shaped)
            );
        } else {
            for (region, env, managed, drift) in &reports {
                let prefix = if multi_region {
                    let r = region.as_deref().unwrap_or("default");
                    format!("{r}\t")
                } else {
                    String::new()
                };
                if drift.is_empty() {
                    if *managed {
                        println!("{prefix}{env}\t✓ no drift");
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
                        "{prefix}{env}\t{}\t{target}\ttf={}\tlive={}",
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
    let mut regions_csv: Option<String> = None;
    let mut json = false;
    let mut quiet = false;
    let mut severity_filter: Option<ebman::lint::Severity> = None;
    let mut rule_filter: Vec<String> = Vec::new();
    let mut fix = false;
    let mut dry_run = false;
    let mut yes = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = iter.next().cloned(),
            "--regions" => regions_csv = iter.next().cloned(),
            "--json" => json = true,
            "--quiet" => quiet = true,
            "--fix" => fix = true,
            "--dry-run" => dry_run = true,
            "--yes" => yes = true,
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

    // Operator-tunable disables — compose user-level
    // `lint.disable = "..."` from config.toml with project-local
    // `[lint].disable = [...]` from .ebman/ebman.toml. Project
    // entries extend the global set; nothing overrides (same
    // mental model as the runbooks merge).
    let mut disabled: Vec<String> = config::load_lint_disables();
    disabled.extend(ebman::project::load_lint_disables_from_cwd());
    let rules = ebman::lint::default_rules(&disabled);

    // Separate opt-out list for auto-fix dispatch. A rule can be
    // ENABLED for reporting (operator wants to know about
    // BatchSize > MaxSize) but DISABLED for fix (operator has a
    // deliberate non-standard BatchSize and doesn't want --fix
    // overwriting it). Same merge pattern as `lint.disable`.
    let mut fix_disabled: Vec<String> = config::load_lint_fix_disables();
    fix_disabled.extend(ebman::project::load_lint_fix_disables_from_cwd());

    // Safety guard: --fix without --yes (and without --dry-run)
    // refuses to dispatch. Operators in interactive sessions get
    // a plan + a clear "add --yes to apply" hint; CI scripts must
    // explicitly opt in.
    if fix && !yes && !dry_run {
        eprintln!("ebman lint --fix: requires --yes to dispatch writes (or --dry-run to preview)");
        std::process::exit(2);
    }
    if fix && yes && dry_run {
        eprintln!("ebman lint --fix: --yes and --dry-run are mutually exclusive");
        std::process::exit(2);
    }

    // Region fan-out. With --regions, iterate over each named
    // region (one AwsClient per). Without, single iteration with
    // the default region (None → operator's AWS_REGION / profile
    // default). Same per-region client pattern `ebman action
    // rollout` uses.
    let regions: Vec<Option<String>> = match regions_csv {
        Some(csv) => {
            let parsed: Vec<String> = csv
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if parsed.is_empty() {
                eprintln!("ebman lint: --regions list is empty");
                std::process::exit(2);
            }
            parsed.into_iter().map(Some).collect()
        }
        None => vec![None],
    };

    let multi_region = regions.len() > 1;
    let mut all_issues: Vec<ebman::lint::Issue> = Vec::new();
    for region_opt in &regions {
        let aws = match aws::AwsClient::with(None, region_opt.clone()).await {
            Ok(c) => c,
            Err(e) => {
                if !quiet {
                    let region_label = region_opt.as_deref().unwrap_or("default");
                    eprintln!("warning: skipping region '{region_label}' — AwsClient::with: {e}");
                }
                continue;
            }
        };
        let envs = match aws.list_environments().await {
            Ok(envs) => envs,
            Err(e) => {
                if !quiet {
                    let region_label = region_opt.as_deref().unwrap_or("default");
                    eprintln!("warning: skipping region '{region_label}' — list_environments: {e}");
                }
                continue;
            }
        };

        // If --env is given, scope to that single env in this
        // region. Skip (warning) if the env isn't present here —
        // operators with a name not in every region get a
        // partial report rather than a hard exit. Without --env,
        // run against every env in the region.
        let targets: Vec<&ebman::aws::Environment> = match env_name.as_deref() {
            Some(name) => match envs.iter().find(|e| e.name == name) {
                Some(env) => vec![env],
                None => {
                    if multi_region && !quiet {
                        let region_label = region_opt.as_deref().unwrap_or("default");
                        eprintln!(
                            "warning: env '{name}' not in region '{region_label}' — skipping"
                        );
                    } else if !multi_region {
                        eprintln!("ebman lint: env '{name}' not found in current context");
                        std::process::exit(2);
                    }
                    continue;
                }
            },
            None => envs.iter().collect(),
        };

        for env in targets {
            // Per-env option-settings fetch. Errors on a single
            // env get surfaced (stderr) but don't abort the run
            // — partial reports are better than nothing for CI.
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
            // Apply --rules whitelist (after severity so the two
            // flags compose: only these rules, only at this
            // severity-or-higher).
            if !rule_filter.is_empty() {
                issues.retain(|i| rule_filter.contains(&i.rule_id));
            }
            // Tag region into each issue's structured fields
            // when multi-region. JSON consumers see the region
            // alongside rule_id / severity / env / etc.; text
            // output uses the same field to prefix rows.
            if let Some(region) = region_opt {
                for issue in &mut issues {
                    issue.fields.insert("region".into(), region.clone());
                }
            }

            // --fix dispatch happens per-env, inline, so the
            // AwsClient borrow lifecycle stays simple. We only
            // call `fix()` on rules whose issue passed the
            // --severity / --rules filters — otherwise a `--rule
            // EBL001 --fix` invocation would still apply EBL004's
            // fix when both fire.
            if fix && !issues.is_empty() {
                let region_label = region_opt.as_deref().unwrap_or("default").to_string();
                let mut to_set: Vec<(String, String, String)> = Vec::new();
                let mut planned: Vec<(String, ebman::lint::FixAction)> = Vec::new();
                let mut planned_set_indices: Vec<usize> = Vec::new();
                for issue in &issues {
                    if fix_disabled.contains(&issue.rule_id) {
                        if !quiet {
                            println!("skip {} ({}): in lint.fix_disable", issue.rule_id, env.name);
                        }
                        continue;
                    }
                    let Some(rule) = rules.iter().find(|r| r.id() == issue.rule_id) else {
                        continue;
                    };
                    let Some(action) = rule.fix(&ctx) else {
                        if !quiet {
                            println!(
                                "no-fix {} ({}): rule has no auto-remediation",
                                issue.rule_id, env.name
                            );
                        }
                        continue;
                    };
                    if let ebman::lint::FixAction::SetOption {
                        namespace,
                        name,
                        value,
                        ..
                    } = &action
                    {
                        planned_set_indices.push(planned.len());
                        to_set.push((namespace.clone(), name.clone(), value.clone()));
                    }
                    planned.push((issue.rule_id.clone(), action));
                }
                for (rule_id, action) in &planned {
                    match action {
                        ebman::lint::FixAction::SetOption { description, .. } => {
                            println!("fix {rule_id} ({}): {description}", env.name);
                        }
                        ebman::lint::FixAction::Manual { instructions } => {
                            println!(
                                "fix {rule_id} ({}) MANUAL — operator action required:\n  {instructions}",
                                env.name
                            );
                        }
                    }
                }
                if !to_set.is_empty() && yes {
                    match aws
                        .update_env_option_settings(&env.name, &to_set, &[])
                        .await
                    {
                        Ok(()) => {
                            for &idx in &planned_set_indices {
                                let (rule_id, action) = &planned[idx];
                                if let ebman::lint::FixAction::SetOption {
                                    namespace,
                                    name,
                                    value,
                                    ..
                                } = action
                                {
                                    write_lint_fix_audit_line(
                                        &region_label,
                                        &env.name,
                                        rule_id,
                                        namespace,
                                        name,
                                        value,
                                        None,
                                    );
                                }
                            }
                            if !quiet {
                                println!(
                                    "ok ({}): applied {} fix(es)",
                                    env.name,
                                    planned_set_indices.len()
                                );
                            }
                        }
                        Err(e) => {
                            eprintln!(
                                "ebman lint --fix: dispatch failed for {} in {region_label}: {e}",
                                env.name
                            );
                            let err_str = e.to_string();
                            for &idx in &planned_set_indices {
                                let (rule_id, action) = &planned[idx];
                                if let ebman::lint::FixAction::SetOption {
                                    namespace,
                                    name,
                                    value,
                                    ..
                                } = action
                                {
                                    write_lint_fix_audit_line(
                                        &region_label,
                                        &env.name,
                                        rule_id,
                                        namespace,
                                        name,
                                        value,
                                        Some(&err_str),
                                    );
                                }
                            }
                            // Track so we exit 1 at the end of the
                            // run; don't abort here — other envs
                            // should still see their fixes applied.
                            FIX_DISPATCH_FAILED.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                }
            }

            all_issues.extend(issues);
        }
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
                // Multi-region runs prefix every row with the
                // region so the operator can scan by source.
                // Single-region runs keep the compact text shape
                // they had before.
                if multi_region {
                    let region = issue
                        .fields
                        .get("region")
                        .map(String::as_str)
                        .unwrap_or("-");
                    println!(
                        "{region}\t{sev}\t{}\t{env_str}\t{}",
                        issue.rule_id, issue.title
                    );
                } else {
                    println!("{sev}\t{}\t{env_str}\t{}", issue.rule_id, issue.title);
                }
                if let Some(s) = &issue.suggestion {
                    println!("\t→ {s}");
                }
            }
        }
    }

    // Exit-code regime depends on whether --fix was requested.
    // - Without --fix: issues found → 3 (CI gate semantic).
    // - With --fix:    fixes applied → 0; AWS dispatch failure → 1.
    //   We deliberately do NOT exit 3 in --fix mode; the operator's
    //   intent is "see issues then fix them", so a CI loop
    //   `ebman lint --fix --yes` should keep exit 0 after a clean
    //   apply, not flag itself failed because the lint surfaced
    //   something it then fixed.
    if fix {
        if FIX_DISPATCH_FAILED.load(std::sync::atomic::Ordering::Relaxed) {
            std::process::exit(1);
        }
        Ok(())
    } else if all_issues.is_empty() {
        Ok(())
    } else {
        std::process::exit(3);
    }
}

/// Tracks whether any `--fix` dispatch failed during the run. Single
/// run-wide flag is fine here because the CLI process exits after
/// `run_lint_cli` returns — no cross-process state. Used instead of
/// threading the boolean through every loop iteration's borrow
/// lifetime.
static FIX_DISPATCH_FAILED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Write one audit-log line tagged with the `stage=fix` shape +
/// `rule_id` correlation. Same `audit.log` path the App's
/// `write_audit_line` uses; duplicated here as a free fn because CLI
/// subcommands don't carry App state. Operators consuming the audit
/// log via `ebman audit --rule EBL001` get clean per-rule history.
fn write_lint_fix_audit_line(
    region: &str,
    env: &str,
    rule_id: &str,
    namespace: &str,
    name: &str,
    value: &str,
    err: Option<&str>,
) {
    let dir = ebman::util::cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("audit.log");
    let when = chrono::Utc::now().to_rfc3339();
    // Quote the value to keep the parser happy if it contains
    // whitespace (rare for option-settings, but cheap insurance).
    let q_value = value.replace('"', "'");
    let line = match err {
        None => format!(
            "{when}\tregion={region}\tstage=fix action=SetOption target={env} rule_id={rule_id} namespace={namespace} name={name} value=\"{q_value}\" outcome=ok\n"
        ),
        Some(e) => format!(
            "{when}\tregion={region}\tstage=fix action=SetOption target={env} rule_id={rule_id} namespace={namespace} name={name} value=\"{q_value}\" outcome=err err=\"{}\"\n",
            e.replace('"', "'")
        ),
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// `ebman audit [--tail] [--since DUR] [--env NAME] [--rule ID]
/// [--action NAME] [--json]` — operationalises the local audit log
/// for scripting / Slack-bot routing / CI gating.
///
/// `--tail` polls the file every 1s and emits new lines as they
/// arrive (Ctrl-C to exit). `--since DUR` accepts the same `5m / 1h
/// / 2d` grammar as `--wait-for-green` etc. Filtering composes:
/// `--since 1h --env prod-api --action Deploy` shows recent prod-api
/// deploys.
///
/// Exit codes (per the 0.13 CLI charter):
/// - 0 ok
/// - 1 io error (audit log unreadable)
/// - 2 usage error
async fn run_audit_cli(args: &[String]) -> Result<()> {
    let mut tail = false;
    let mut since_str: Option<String> = None;
    let mut env_filter: Option<String> = None;
    let mut rule_filter: Option<String> = None;
    let mut action_filter: Option<String> = None;
    let mut json = false;
    let mut iter = args.iter().skip(1);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--tail" => tail = true,
            "--since" => since_str = iter.next().cloned(),
            "--env" => env_filter = iter.next().cloned(),
            "--rule" => rule_filter = iter.next().cloned(),
            "--action" => action_filter = iter.next().cloned(),
            "--json" => json = true,
            other => {
                eprintln!("ebman audit: unknown flag '{other}'");
                std::process::exit(2);
            }
        }
    }

    let since_dt: Option<chrono::DateTime<chrono::Utc>> = match since_str.as_deref() {
        None => None,
        Some(s) => match aws::parse_window_ms(s) {
            Some(ms) => Some(chrono::Utc::now() - chrono::Duration::milliseconds(ms)),
            None => {
                eprintln!(
                    "ebman audit: --since expects a duration like `5m` / `30m` / `1h` / `2d`"
                );
                std::process::exit(2);
            }
        },
    };

    let filter = ebman::audit::AuditFilter {
        since: since_dt,
        env: env_filter.as_deref(),
        rule: rule_filter.as_deref(),
        action: action_filter.as_deref(),
    };

    let path = ebman::util::cache_dir().join("audit.log");
    if !path.exists() {
        // Not an error — the audit log only exists once something
        // has been written. Empty result for `ebman audit` is fine.
        if !json {
            println!("(no audit entries — log not yet created)");
        }
        return Ok(());
    }

    // Phase 1: read the existing file end-to-end, parse + filter +
    // render. Track the read offset so --tail can resume from EOF.
    let bytes = std::fs::read(&path)
        .map_err(|e| color_eyre::eyre::eyre!("read {}: {e}", path.display()))?;
    let initial_offset = bytes.len() as u64;
    let text = String::from_utf8_lossy(&bytes);
    let entries: Vec<ebman::audit::AuditEntry> = text
        .lines()
        .filter_map(ebman::audit::parse_audit_line)
        .filter(|e| filter.matches(e))
        .collect();
    if json {
        print!("{}", ebman::audit::render_audit_entries_json(&entries));
    } else {
        print!("{}", ebman::audit::render_audit_entries_text(&entries));
    }
    use std::io::Write;
    let _ = std::io::stdout().flush();

    // Phase 2: --tail. Poll for new bytes every second. We use a
    // simple offset-based read rather than inotify / FSEvents — keeps
    // the surface small and works the same on macOS/Linux. Rotation
    // (write_audit_line caps the file at 1 MiB and moves it to
    // audit.log.1) is detected by the file shrinking; we reset to 0
    // when that happens so the new rotated file's entries are
    // streamed.
    if tail {
        let mut offset = initial_offset;
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            let meta = match std::fs::metadata(&path) {
                Ok(m) => m,
                Err(_) => continue, // file vanished mid-rotate; try again
            };
            let len = meta.len();
            if len < offset {
                // Rotated. Re-read from the start.
                offset = 0;
            }
            if len == offset {
                continue;
            }
            // Read just the new bytes.
            use std::io::{Read, Seek, SeekFrom};
            let mut f = match std::fs::File::open(&path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            if f.seek(SeekFrom::Start(offset)).is_err() {
                continue;
            }
            let mut buf = Vec::with_capacity((len - offset) as usize);
            if f.read_to_end(&mut buf).is_err() {
                continue;
            }
            offset = len;
            let chunk = String::from_utf8_lossy(&buf);
            let new_entries: Vec<ebman::audit::AuditEntry> = chunk
                .lines()
                .filter_map(ebman::audit::parse_audit_line)
                .filter(|e| filter.matches(e))
                .collect();
            if new_entries.is_empty() {
                continue;
            }
            if json {
                print!("{}", ebman::audit::render_audit_entries_json(&new_entries));
            } else {
                // In tail mode we don't repeat the column header for
                // each new batch; render the data rows only.
                for e in &new_entries {
                    let outcome = match (e.outcome.as_deref(), e.err.as_deref()) {
                        (_, Some(err)) => format!("err=\"{err}\""),
                        (Some("ok"), _) => "ok".into(),
                        (Some(s), _) => s.into(),
                        _ => "-".into(),
                    };
                    println!(
                        "{}\t{}\t{}\t{}\t{}\t{}",
                        e.when,
                        e.region.as_deref().unwrap_or("-"),
                        e.stage.as_deref().unwrap_or("-"),
                        e.action.as_deref().unwrap_or("-"),
                        e.target.as_deref().unwrap_or("-"),
                        outcome,
                    );
                }
            }
            let _ = std::io::stdout().flush();
        }
    }
    Ok(())
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
            "usage: ebman action <rebuild|restart|terminate|deploy|rollout> --env NAME [--version LABEL] [--regions r1,r2,r3] [--yes] [--wait-for-green Nm] [--auto-rollback Nm]"
        );
        std::process::exit(2);
    }
    // `rollout` is the cross-region fan-out — separate parsing
    // path because it needs `--regions` and constructs N AwsClients
    // (one per region) rather than the single client every other
    // action uses.
    if action_name == "rollout" {
        return run_action_rollout(args).await;
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

/// `ebman action rollout --version LABEL --regions r1,r2,r3 --env NAME [--yes] [--json] [--quiet]`
/// — cross-region sequential deploy. Same-name env mapping
/// (operator names regions; ebman looks up the env with the
/// given `--env NAME` in each region). Pre-flight validates
/// every region has a matching env BEFORE dispatching to any
/// of them, so a typo doesn't leave the fleet half-deployed.
///
/// Value-add over `for r in us eu ap; do ebman action deploy
/// ...; done`:
/// - **Atomic correlation.** A single `rollout_id = uuid` is
///   written into every per-region audit line so post-mortems
///   can `grep rollout_id=XXX` to see the full sequence.
/// - **Pre-flight validation.** All regions checked for the
///   target env BEFORE any region is touched.
/// - **Stop on first failure.** If region 1 fails (UpdateEnv
///   error), regions 2 and 3 aren't dispatched. Operator
///   handles the failed region before continuing — safer
///   default than continuing blindly.
/// - **Aggregated JSON report.** `--json` emits a single
///   `{"rollout_id": "...", "regions": [...]}` payload.
///
/// Out of scope for the first cut (operators who want these
/// chain via shell): `--parallel`, `--continue-on-fail`, per-
/// region `--auto-rollback` watchdog. The `--wait-for-green`
/// flag does apply per-region (reuses `decide_poll` from
/// `run_action_deploy`).
///
/// Exit codes (per the 0.13 CLI charter):
/// - `0` all regions dispatched successfully
/// - `1` AWS-layer error before any region dispatched
/// - `2` usage error (missing --version / --regions / --env,
///   bad duration, env not found in some region)
/// - `3` one or more region dispatches failed (rollout halted)
async fn run_action_rollout(args: &[String]) -> Result<()> {
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

    // Pre-flight: build a per-region AwsClient + verify the
    // target env exists. Done sequentially rather than via
    // `join!` so error output stays orderly when one region's
    // STS hits a creds issue.
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

    // Confirm gate. Destructive enough that --yes is required
    // for non-interactive runs; matches the `:terminate` posture.
    if !yes {
        eprintln!(
            "ebman action rollout: would dispatch to {} region(s); re-run with --yes to confirm",
            regions.len()
        );
        std::process::exit(2);
    }

    // Audit-log correlation id. Single value written into every
    // per-region audit line + the JSON output so a post-mortem
    // can grep `rollout_id=` across the audit log to find the
    // full sequence.
    let rollout_id = format!("rollout-{}", chrono::Utc::now().format("%Y%m%dT%H%M%SZ"));

    // Sequential dispatch. Stop on first failure (the halt check
    // lives inside the loop, just below the per-region wait-for-
    // green block — examines the last outcome rather than a
    // separate halt flag).
    let mut outcomes: Vec<(String, Result<(), String>)> = Vec::new();
    for (region, client) in &per_region {
        if !quiet {
            eprintln!("rollout: dispatching to {region} (env={env}, version={version})");
        }
        match client.deploy_version(&env, &version).await {
            Ok(()) => {
                outcomes.push((region.clone(), Ok(())));
                if let Some(secs) = wait_for_green_secs {
                    // Reuse the same decide_poll loop
                    // `run_action_deploy` uses. Polling cadence
                    // 5s; deadline `secs` from now. Health
                    // observation per tick. Per-region wait;
                    // doesn't block subsequent regions.
                    let start = tokio::time::Instant::now();
                    // We don't toggle this — the WaitForGreenTimeout
                    // branch breaks the loop immediately, so there's
                    // no second tick where "already emitted" would
                    // matter (unlike `run_action_deploy` which keeps
                    // polling under auto-rollback after the timeout).
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
                                // wait_timeout_emitted and halted_after
                                // updates would normally happen here,
                                // but both writes are dead — we `break`
                                // the inner loop immediately, and the
                                // outer-loop halt check just below
                                // reads the per-iteration outcome
                                // (Err set on line above), so we don't
                                // need to mark halted_after explicitly.
                                let msg = format!(
                                    "did not reach Green within {secs}s (status={status}, health={health})"
                                );
                                eprintln!("rollout[{region}]: {msg}");
                                outcomes.last_mut().unwrap().1 = Err(msg);
                                break;
                            }
                            PollDecision::DispatchRollback => {
                                // --auto-rollback isn't wired into rollout
                                // yet; this branch is unreachable today
                                // because wait_for_green_secs is the
                                // only deadline. Defensive break in case
                                // a future change wires it.
                                break;
                            }
                        }
                    }
                    // Halt detection: if the wait-for-green loop
                    // recorded a failure for this region, halt the
                    // outer rollout before dispatching the next
                    // region. Cheaper than a separate halt-flag
                    // since outcomes already carries the truth.
                    if matches!(outcomes.last(), Some((_, Err(_)))) {
                        break;
                    }
                }
            }
            Err(e) => {
                let msg = format!("deploy_version: {e}");
                outcomes.push((region.clone(), Err(msg.clone())));
                eprintln!("rollout[{region}]: {msg}");
                break;
            }
        }
        write_rollout_audit_line(&rollout_id, region, &env, &version, "dispatched", None);
    }

    // Output.
    let any_failure = outcomes.iter().any(|(_, r)| r.is_err());
    if !quiet {
        if json {
            // Hand-rolled JSON, same style as the other CLI
            // subcommands. Shape:
            // {"rollout_id":"...","env":"...","version":"...",
            //  "regions":[{"region":"...","ok":true|false,"err":"..."}]}
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
            // Unreached regions (post-halt) reported separately
            // so JSON consumers know which regions weren't even
            // attempted.
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
            // Unreached regions
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

/// Write one audit-log line tagged with a rollout correlation
/// id. Same `audit.log` path the App's `write_audit_line` uses;
/// duplicated here as a free fn because CLI subcommands don't
/// carry App state.
fn write_rollout_audit_line(
    rollout_id: &str,
    region: &str,
    env: &str,
    version: &str,
    stage: &str,
    err: Option<&str>,
) {
    let dir = ebman::util::cache_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("audit.log");
    let when = chrono::Utc::now().to_rfc3339();
    let line = match err {
        None => format!(
            "{when}\trollout_id={rollout_id}\tregion={region}\tstage={stage} action=Rollout target={env} version={version}\n"
        ),
        Some(e) => format!(
            "{when}\trollout_id={rollout_id}\tregion={region}\tstage={stage} action=Rollout target={env} version={version} err=\"{}\"\n",
            e.replace('"', "'")
        ),
    };
    use std::io::Write;
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let _ = f.write_all(line.as_bytes());
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
