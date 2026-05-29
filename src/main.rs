use std::{io, panic};

use color_eyre::eyre::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tracing_subscriber::{layer::SubscriberExt, reload, util::SubscriberInitExt, EnvFilter};

use ebman::{app::App, config, control, font_probe, splash, util, LogReloadHandle, Tui};

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
        // Each match arm calls `audit::init_from_config_disk()`
        // itself (rather than once before the match) so flag-only
        // invocations (`--read-only`, `--demo`, `--version`,
        // `--help`, `--control-socket`) don't pay the
        // `config::load` disk read. The two read-only subcommands
        // (envs, ctl) skip it too — they emit no audit lines.
        match first.as_str() {
            "envs" => return ebman::cli::envs::run(&args).await,
            "action" => {
                ebman::audit::init_from_config_disk();
                return ebman::cli::action::run(&args).await;
            }
            "ctl" => return ebman::cli::ctl::run(&args).await,
            "lint" => {
                ebman::audit::init_from_config_disk();
                return ebman::cli::lint::run(&args).await;
            }
            "drift" => return ebman::cli::drift::run(&args).await,
            "audit" => return ebman::cli::audit::run(&args).await,
            "explain" => return ebman::cli::explain::run(&args).await,
            "versions" => return ebman::cli::versions::run(&args).await,
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
            splash::draw_splash(&mut terminal, splash_frame, &splash_icons)?;
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
                    splash::draw_splash(&mut terminal, splash_frame, &splash_icons)?;
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
         [--fix (--yes | --dry-run)] [--watch [--interval 60s]]
         [--baseline FILE | --against-baseline FILE]
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
                                                  --watch loops at --interval (default 60s) until
                                                  Ctrl-C; exit code reflects the LAST cycle.
                                                  Canonical monitoring shape:
                                                  `ebman lint --watch --interval 5m --json > alerts.jsonl`.
                                                  --watch and --fix are mutually exclusive.
                                                  --baseline FILE snapshots current issues to JSON
                                                  (CI adoption: grandfather existing warnings).
                                                  --against-baseline FILE diffs vs the snapshot;
                                                  exits 3 only on NEW issues, cleared ones are
                                                  informational. Identity is (rule_id, env_name,
                                                  fields); title / detail drift doesn't churn.
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
                                                  --yes [--wait-for-green Nm] [--json] [--profile P]
                                                  [--parallel [--max-concurrency N]]
                                                  [--continue-on-fail] [--staggered Nm].
                                                  Default: sequential, halt on first failure.
                                                  --parallel fans out concurrently (implies
                                                  --continue-on-fail since in-flight regions
                                                  can't be cancelled). --staggered delays
                                                  between regions in sequential mode (requires
                                                  --wait-for-green). Single rollout_id
                                                  correlation across audit lines.
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
    explain EBL### [--env NAME] [--json] [--dry-run] [--no-cache]
                                                  LLM-backed explanation of a lint issue. Routes to
                                                  the configured Provider (Anthropic API or local
                                                  Ollama) and prints an operator-readable summary
                                                  of why the issue matters and what to do next.
                                                  Requires `[explain] enabled = true` in
                                                  config.toml + an exported ANTHROPIC_API_KEY
                                                  (Anthropic) or a running Ollama server. Responses
                                                  cached to ~/.cache/ebman/explain/; --no-cache
                                                  forces a fresh call. --dry-run prints the prompt
                                                  without sending. Exit codes: 0 ok, 1 provider err,
                                                  2 usage, 3 issue not found.
    versions --env NAME [--json]                 List application versions for the env's app,
                                                  newest-first. CLI mirror of the TUI `:versions`
                                                  overlay. Useful for CI scripts that want to
                                                  validate a candidate label exists before
                                                  `ebman action deploy`, or to surface the
                                                  candidate's description / age in a Slack notify.
                                                  --json emits one object per version with
                                                  {{label, deployed (bool), created (RFC3339),
                                                  description}}. Default text mode marks the
                                                  currently-deployed label with `*`. Exit codes:
                                                  0 ok, 1 aws err, 2 usage.

CONFIG:
    ~/.config/ebman/config.toml   user configuration (see README)
    ~/.config/ebman/state.toml    persisted session state (managed by the app)
    ~/.cache/ebman/ebman.log      log output (filter with RUST_LOG)

KEYS:
    Once running, press '?' for the in-app help screen."
    );
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
    use super::prune_old_crash_reports;

    // The original `decide_poll` + `cli_esc` tests moved to
    // `src/cli/mod.rs` (0.15 CLI-split). The original
    // `hsl_to_rgb_*` tests moved to `src/splash.rs` (0.16
    // draw_splash relocation). Look there for the matrix coverage.

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
}
