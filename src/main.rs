mod app;
mod aws;
mod config;
mod control;
mod keys;
mod plugins;
mod profiles;
mod sso;
mod state;
mod theme;
mod ui;
mod update_check;
mod util;

use std::{
    io::{self, Stdout},
    panic,
};

use color_eyre::eyre::Result;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use tracing_subscriber::{layer::SubscriberExt, reload, util::SubscriberInitExt, EnvFilter};

/// Handle for live-reloading the log filter from the running app.
pub type LogReloadHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;

use crate::app::App;

type Tui = Terminal<CrosstermBackend<Stdout>>;

#[tokio::main]
async fn main() -> Result<()> {
    // Handle CLI flags before any TUI / logging setup so they print cleanly.
    let mut read_only = false;
    let mut control_socket: Option<std::path::PathBuf> = None;
    let args: Vec<String> = std::env::args().skip(1).collect();
    // Subcommand support: `ebman envs [--json]`, `ebman action ACTION --env NAME --yes`,
    // `ebman ctl <op> …`. Falls through to the TUI when no subcommand is present.
    if let Some(first) = args.first() {
        match first.as_str() {
            "envs" => return run_envs_cli(&args).await,
            "action" => return run_action_cli(&args).await,
            "ctl" => return run_ctl_cli(&args).await,
            _ => {}
        }
    }
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--version" | "-V" => {
                println!("ebman {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            "--help" | "-h" => {
                print_help();
                return Ok(());
            }
            "--read-only" => read_only = true,
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

    let cfg = config::load();
    let mut terminal = enter_tui()?;

    // Animate the splash while App::new resolves (config load + STS + first
    // SDK setup). Keep the splash visible for at least SPLASH_MIN_DURATION even
    // if App::new returns sooner — gives the user a chance to actually see it.
    const SPLASH_MIN_DURATION: std::time::Duration = std::time::Duration::from_secs(1);

    let splash_started = std::time::Instant::now();
    let mut splash_frame: u64 = 0;
    let mut interval = tokio::time::interval(std::time::Duration::from_millis(30));
    let mut new_app_fut = Box::pin(App::new(cfg));
    let mut app_ready: Option<App> = None;
    let mut app_inst = loop {
        tokio::select! {
            biased;
            res = &mut new_app_fut, if app_ready.is_none() => {
                app_ready = Some(res?);
            }
            _ = interval.tick() => {
                draw_splash(&mut terminal, splash_frame)?;
                splash_frame = splash_frame.wrapping_add(1);
                if app_ready.is_some() && splash_started.elapsed() >= SPLASH_MIN_DURATION {
                    break app_ready
                        .take()
                        .expect("app_ready was Some, just checked above");
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
    leave_tui(&mut terminal)?;
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
        --control-socket P  Open a Unix socket at P for remote control (off by default).
                            Pair with `ebman ctl <op>` to drive the running session.

SUBCOMMANDS:
    envs [--json]                                List environments in current profile / region.
    action ACTION --env NAME [--yes]             Run an action (rebuild|restart|terminate) on an env.
                                                  Terminate requires --yes to confirm.
    ctl <screen|key|cmd|state> [args]            Talk to a running ebman via --control-socket.
                                                  Use --socket PATH to override the default location.

CONFIG:
    ~/.config/ebman/config.toml   user configuration (see README)
    ~/.config/ebman/state.toml    persisted session state (managed by the app)
    ~/.cache/ebman/ebman.log      log output (filter with RUST_LOG)

KEYS:
    Once running, press '?' for the in-app help screen."
    );
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

async fn run_action_cli(args: &[String]) -> Result<()> {
    // Expected shape: ebman action ACTION --env NAME [--yes]
    let action_name = args.get(1).map(|s| s.as_str()).unwrap_or("");
    if action_name.is_empty() || action_name.starts_with('-') {
        eprintln!("usage: ebman action <rebuild|restart|terminate> --env NAME [--yes]");
        std::process::exit(2);
    }
    let mut env_name: Option<String> = None;
    let mut yes = false;
    let mut iter = args.iter().skip(2);
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--env" => env_name = iter.next().cloned(),
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

/// Minimal JSON-string escape for CLI output. Mirrors the webhook payload
/// escape — quotes and backslashes only; EB names don't contain control chars.
fn cli_esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

const SPLASH_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// Block-letter "ebman" — 5 rows tall, ~44 cols wide.
const SPLASH_LOGO: &[&str] = &[
    "███████ ██████  ███    ███  █████  ███    ██",
    "██      ██   ██ ████  ████ ██   ██ ████   ██",
    "█████   ██████  ██ ████ ██ ███████ ██ ██  ██",
    "██      ██   ██ ██  ██  ██ ██   ██ ██  ██ ██",
    "███████ ██████  ██      ██ ██   ██ ██   ██ ██",
];

fn draw_splash(terminal: &mut Tui, frame: u64) -> Result<()> {
    use ratatui::layout::{Alignment, Constraint, Direction, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

    terminal.draw(|f| {
        let area = f.area();
        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(28),
                Constraint::Length(14),
                Constraint::Percentage(28),
            ])
            .split(area);
        let h = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Percentage(15),
                Constraint::Percentage(70),
                Constraint::Percentage(15),
            ])
            .split(v[1]);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::from(""));
        // Per-character hue-shift gives a horizontal gradient that scrolls
        // through cool tones (cyan → blue → purple → magenta) as `frame` advances.
        for row in SPLASH_LOGO {
            let spans: Vec<Span> = row
                .chars()
                .enumerate()
                .map(|(col, ch)| {
                    if ch == ' ' {
                        return Span::raw(" ".to_string());
                    }
                    // One-shot spotlight: a soft magenta wave crosses the logo
                    // left-to-right over ~1 s, then the letters settle to cyan
                    // and stay there for the rest of the splash.
                    const SPOTLIGHT_FRAMES: f64 = 33.0; // ~1 s at 30 ms ticks
                    const SPOTLIGHT_WIDTH: f64 = 9.0; // column-wise softness
                    const LOGO_TRAVEL: f64 = 56.0; // includes off-screen exit
                    let pos = (frame as f64 / SPOTLIGHT_FRAMES) * LOGO_TRAVEL;
                    let dist = (col as f64 - pos).abs();
                    let glow = (1.0 - dist / SPOTLIGHT_WIDTH).clamp(0.0, 1.0);
                    let h_deg = 180.0 + glow * 120.0; // cyan → magenta at peak
                    let l = 0.55 + glow * 0.10;
                    let (r, g, b) = hsl_to_rgb(h_deg, 0.70, l);
                    Span::styled(
                        ch.to_string(),
                        Style::default()
                            .fg(Color::Rgb(r, g, b))
                            .add_modifier(Modifier::BOLD),
                    )
                })
                .collect();
            lines.push(Line::from(spans).alignment(Alignment::Center));
        }
        lines.push(Line::from(""));
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

        // Card border tracks the spotlight: warms to magenta during the pass,
        // cools back to cyan once the spotlight has exited.
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
        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(br, bg, bb)));
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
    use super::{cli_esc, hsl_to_rgb, prune_old_crash_reports};

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
