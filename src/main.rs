mod app;
mod aws;
mod config;
mod plugins;
mod profiles;
mod state;
mod theme;
mod ui;
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
    for arg in std::env::args().skip(1) {
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
    draw_splash(&mut terminal)?;
    let mut app_inst = App::new(cfg).await?;
    app_inst.read_only = read_only;
    app_inst.log_reload = Some(log_handle);
    let result = app_inst.run(&mut terminal).await;
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
    -V, --version    Print version and exit.
    -h, --help       Print this help and exit.
        --read-only  Start with destructive actions disabled (also toggleable with :readonly).

CONFIG:
    ~/.config/ebman/config.toml   user configuration (see README)
    ~/.config/ebman/state.toml    persisted session state (managed by the app)
    ~/.cache/ebman/ebman.log      log output (filter with RUST_LOG)

KEYS:
    Once running, press '?' for the in-app help screen."
    );
}

fn draw_splash(terminal: &mut Tui) -> Result<()> {
    use ratatui::layout::{Alignment, Constraint, Direction, Layout};
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, BorderType, Borders, Paragraph};

    // Block-letter "ebman" — 5 rows tall, ~36 cols wide.
    const LOGO: &[&str] = &[
        "███████ ██████  ███    ███  █████  ███    ██",
        "██      ██   ██ ████  ████ ██   ██ ████   ██",
        "█████   ██████  ██ ████ ██ ███████ ██ ██  ██",
        "██      ██   ██ ██  ██  ██ ██   ██ ██  ██ ██",
        "███████ ██████  ██      ██ ██   ██ ██   ██ ██",
    ];

    terminal.draw(|f| {
        let area = f.area();
        let v = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Percentage(30),
                Constraint::Length(12),
                Constraint::Percentage(30),
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
        for row in LOGO {
            lines.push(
                Line::from(Span::styled(
                    (*row).to_string(),
                    Style::default()
                        .fg(Color::Rgb(120, 200, 240))
                        .add_modifier(Modifier::BOLD),
                ))
                .alignment(Alignment::Center),
            );
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
                "connecting to AWS…",
                Style::default()
                    .fg(Color::Rgb(255, 200, 120))
                    .add_modifier(Modifier::DIM),
            ))
            .alignment(Alignment::Center),
        );

        let block = Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(120, 200, 240)));
        f.render_widget(Paragraph::new(lines).block(block), h[1]);
    })?;
    Ok(())
}

fn enter_tui() -> Result<Tui> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    Ok(Terminal::new(CrosstermBackend::new(stdout))?)
}

fn leave_tui(terminal: &mut Tui) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen, DisableMouseCapture)?;
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
