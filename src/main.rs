mod app;
mod aws;
mod commands;
mod config;
mod control;
mod cost_cache;
mod font_probe;
mod form;
mod mode_action;
mod mode_detail;
mod mode_dlq;
mod plugins;
mod profiles;
mod report_bug;
mod shell;
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
                draw_splash(&mut terminal, splash_frame, &splash_icons)?;
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
        --control-socket P  Open a Unix socket at P for remote control (off by default).
                            Pair with `ebman ctl <op>` to drive the running session.

SUBCOMMANDS:
    envs [--json]                                List environments in current profile / region.
    action ACTION --env NAME [--yes]             Run an action (rebuild|restart|terminate) on an env.
                                                  Terminate requires --yes to confirm.
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

/// Minimal JSON-string escape for CLI output. Quotes and backslashes only;
/// EB names don't contain control chars.
fn cli_esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

const SPLASH_SPINNER: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

// 8-bit pixel-art splash scene — a beanstalk growing out of its pot
// (the `ebman` = Elastic *Beanstalk* gag: watch the thing sprout).
//
// Each glyph in a frame is a palette key, not a literal. The
// renderer ([`splash_scene_lines`]) paints every non-`.` key as a
// **two-cell** `██` block coloured via [`splash_pixel`] — two cells
// wide so each logical pixel is roughly square (terminal cells are
// ~1:2). The `.` key is transparent (rendered as two blank cells).
//
// Eight frames take the env from bare pot to full bloom: empty pot
// → first leaf → stem rising → side leaves → upper stem → upper
// leaf cluster → second-tier branches → bud crowning the top. The
// pot stays rooted across every frame so the growing motion reads
// against a fixed anchor.
//
// Palette keys: `#` outline (dark green) · `G` leaf · `L` leaf
// highlight · `F` bud · `P` pot · `T` soil. All frames are 20×20.

// Frame index → growth stage. The 8 keyframes from the JSON design
// source are at indices 0, 1, 3, 5, 7, 9, 11, 13; the 6 in-between
// frames (indices 2, 4, 6, 8, 10, 12) smooth the largest visual jumps
// (stem extending, leaf clusters forming, bud emerging). Visual cycle:
// empty pot → sprout pixel → stem rises → first leaves bud and fill
// → upper sprout rises → upper leaves bud and fill → bud crowns the top.

const SPLASH_FRAME_0: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_1: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: sprout extends to a 2-pixel stem before the wings appear.
const SPLASH_FRAME_2: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_3: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "........G#G.........",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: wings shed, stem grows another two rows.
const SPLASH_FRAME_4: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_5: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: small leaf bud forming at the top of the stem before
// the full cluster fills in.
const SPLASH_FRAME_6: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "........###.........",
    ".......#GGG#........",
    "........###.........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_7: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: upper sprout begins to climb above the lower cluster.
const SPLASH_FRAME_8: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_9: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: small upper bud forms before the branched upper cluster.
const SPLASH_FRAME_10: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    "........###.........",
    ".......#GGG#........",
    "........###.........",
    ".........#..........",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_11: &[&str] = &[
    "....................",
    "....................",
    "....................",
    "....................",
    ".........G..........",
    "......##.#.##.......",
    ".....#GG#G#GG#......",
    ".....#GLGGGLG#......",
    "......##.#.##.......",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

// In-between: stem extends one row above the upper cluster before
// the bud forms its cap.
const SPLASH_FRAME_12: &[&str] = &[
    "....................",
    "....................",
    "....................",
    ".........G..........",
    ".........#..........",
    "......##.#.##.......",
    ".....#GG#G#GG#......",
    ".....#GLGGGLG#......",
    "......##.#.##.......",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

const SPLASH_FRAME_13: &[&str] = &[
    "....................",
    ".........##.........",
    "........#FF#........",
    ".........##.........",
    ".........#..........",
    "......##.#.##.......",
    ".....#GG#G#GG#......",
    ".....#GLGGGLG#......",
    "......##.#.##.......",
    ".........#..........",
    ".......#######......",
    "......#GGGGGGG#.....",
    ".......#GGLGG#......",
    "........#####.......",
    ".........#..........",
    ".........#..........",
    "....############....",
    "....#TTTTTTTTTT#....",
    "....#PPPPPPPPPP#....",
    ".....##########.....",
];

/// Map a pixel-art palette key to an RGB colour. `None` = a
/// transparent cell (rendered as two blank cells).
fn splash_pixel(key: char) -> Option<(u8, u8, u8)> {
    Some(match key {
        '#' => (28, 107, 46),   // outline (dark green)
        'G' => (76, 192, 106),  // leaf body
        'L' => (139, 224, 156), // leaf highlight
        'F' => (255, 208, 36),  // bud (yellow)
        'P' => (184, 115, 46),  // pot (brown)
        'T' => (107, 67, 33),   // soil (dark brown)
        _ => return None,
    })
}

/// Number of art rows in the splash scene (every frame is the same
/// height). Used by callers to size the splash card / about popup.
pub(crate) const SPLASH_SCENE_ROWS: usize = SPLASH_FRAME_0.len();

/// Number of art columns per frame. Every frame is exactly this
/// wide; the renderer relies on that to avoid per-frame jitter when
/// alignment recentres. Doubled (×2) at render time so logical
/// pixels are roughly square in the terminal cell grid.
pub(crate) const SPLASH_SCENE_COLS: usize = 20;

/// Build the coloured lines for the splash scene at `frame`. Each
/// non-transparent pixel is a **two-cell** `██` block in its palette
/// colour, so the logical pixels are roughly square. Used by the
/// boot splash and the `:about` overlay.
///
/// 14 frames cycle at ≈180 ms each (6 30 ms ticks); a full grow
/// cycle takes ~2.5 s so the whole empty-pot-to-bloom animation
/// fits inside the boot splash's 3 s minimum duration with the
/// final bud frame holding for ~500 ms before the table appears.
/// The 8 JSON keyframes (indices 0, 1, 3, 5, 7, 9, 11, 13) are
/// interleaved with 6 hand-drawn in-betweens (2, 4, 6, 8, 10, 12)
/// to smooth the largest visual jumps — stem extending, leaf
/// clusters filling in, bud cap forming.
pub(crate) fn splash_scene_lines(frame: u64) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::layout::Alignment;
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    const FRAMES: [&[&str]; 14] = [
        SPLASH_FRAME_0,
        SPLASH_FRAME_1,
        SPLASH_FRAME_2,
        SPLASH_FRAME_3,
        SPLASH_FRAME_4,
        SPLASH_FRAME_5,
        SPLASH_FRAME_6,
        SPLASH_FRAME_7,
        SPLASH_FRAME_8,
        SPLASH_FRAME_9,
        SPLASH_FRAME_10,
        SPLASH_FRAME_11,
        SPLASH_FRAME_12,
        SPLASH_FRAME_13,
    ];
    // 14 frames × 6 ticks × 30 ms = 2520 ms. Cycle completes inside
    // the 3 s SPLASH_MIN_DURATION so the final bud frame lands before
    // the splash dismisses. The `splash_animation_completes_within_min_duration`
    // test pins the relationship so a future bump fails loud.
    const TICKS_PER_FRAME: usize = 6;
    let scene = FRAMES[(frame as usize / TICKS_PER_FRAME) % FRAMES.len()];
    scene
        .iter()
        .map(|row| {
            let chars: Vec<char> = row.chars().collect();
            let spans: Vec<Span> = (0..SPLASH_SCENE_COLS)
                .map(|col| {
                    let key = chars.get(col).copied().unwrap_or('.');
                    match splash_pixel(key) {
                        // Two cells per pixel → square logical pixels.
                        Some((r, g, b)) => {
                            Span::styled("██", Style::default().fg(Color::Rgb(r, g, b)))
                        }
                        None => Span::raw("  "),
                    }
                })
                .collect();
            Line::from(spans).alignment(Alignment::Center)
        })
        .collect()
}

/// Pure: whether the boot splash has room for the pixel-art scene.
/// Below this it falls back to the compact text-only card. Scene is
/// 40 cells wide (20 px × 2) and 20 rows tall; the threshold is the
/// card chrome budget (+ borders / padding) on top.
pub(crate) fn splash_shows_scene(w: u16, h: u16) -> bool {
    w >= 48 && h >= 30
}

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
        let show_scene = splash_shows_scene(area.width, area.height);
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
            lines.extend(splash_scene_lines(frame));
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
    use super::{
        cli_esc, hsl_to_rgb, prune_old_crash_reports, splash_pixel, splash_scene_lines,
        splash_shows_scene, SPLASH_FRAME_0, SPLASH_FRAME_1, SPLASH_FRAME_10, SPLASH_FRAME_11,
        SPLASH_FRAME_12, SPLASH_FRAME_13, SPLASH_FRAME_2, SPLASH_FRAME_3, SPLASH_FRAME_4,
        SPLASH_FRAME_5, SPLASH_FRAME_6, SPLASH_FRAME_7, SPLASH_FRAME_8, SPLASH_FRAME_9,
    };

    const ALL_SPLASH_FRAMES: [&[&str]; 14] = [
        SPLASH_FRAME_0,
        SPLASH_FRAME_1,
        SPLASH_FRAME_2,
        SPLASH_FRAME_3,
        SPLASH_FRAME_4,
        SPLASH_FRAME_5,
        SPLASH_FRAME_6,
        SPLASH_FRAME_7,
        SPLASH_FRAME_8,
        SPLASH_FRAME_9,
        SPLASH_FRAME_10,
        SPLASH_FRAME_11,
        SPLASH_FRAME_12,
        SPLASH_FRAME_13,
    ];

    #[test]
    fn splash_shows_scene_gates_on_terminal_size() {
        // Roomy → scene.
        assert!(splash_shows_scene(120, 50));
        assert!(splash_shows_scene(48, 30)); // exact threshold
                                             // Too narrow / too short → text-only fallback.
        assert!(!splash_shows_scene(47, 50));
        assert!(!splash_shows_scene(120, 29));
        assert!(!splash_shows_scene(40, 20));
    }

    #[test]
    fn cli_esc_escapes_quotes_and_backslashes() {
        assert_eq!(cli_esc("hello"), "hello");
        assert_eq!(cli_esc("a\"b"), "a\\\"b");
        assert_eq!(cli_esc("a\\b"), "a\\\\b");
    }

    #[test]
    fn splash_pixel_maps_keys_and_treats_dot_as_transparent() {
        // Every key used in the art frames must resolve to a colour
        // (apart from `.`, which is transparent by design).
        for frame in ALL_SPLASH_FRAMES {
            for row in frame {
                for ch in row.chars() {
                    if ch != '.' {
                        assert!(
                            splash_pixel(ch).is_some(),
                            "art key '{ch}' has no palette entry"
                        );
                    }
                }
            }
        }
        assert_eq!(splash_pixel('.'), None);
    }

    #[test]
    fn all_splash_frames_have_identical_dimensions() {
        // Frames must agree on row count *and* column count — the
        // pot anchor is meant to stay rooted while only the plant
        // above it changes, so any frame-to-frame size shift would
        // re-centre and read as a jitter.
        let h = SPLASH_FRAME_0.len();
        for f in ALL_SPLASH_FRAMES {
            assert_eq!(f.len(), h, "frame height mismatch");
            for row in f {
                assert_eq!(
                    row.chars().count(),
                    super::SPLASH_SCENE_COLS,
                    "row width mismatch in frame: {row:?}"
                );
            }
        }
    }

    /// Mirrors `splash_scene_lines`'s internal constant so the cycle
    /// probes match the actual frame stride.
    const TICKS_PER_FRAME: u64 = 6;

    /// Total number of frames in the animation cycle. Mirrors the
    /// FRAMES array length in `splash_scene_lines`.
    const FRAME_COUNT: u64 = 14;

    #[test]
    fn splash_scene_frames_render_identical_width() {
        // Every frame must render to the same total cell width, or
        // centre-alignment shifts the sprite sideways between frames
        // (a visible jitter).
        let line_w = |l: &ratatui::text::Line| -> usize {
            l.spans.iter().map(|s| s.content.chars().count()).sum()
        };
        // Probe one frame from each cycle slot; the sampled tick
        // offsets span the full N-frame cycle.
        let widths: Vec<usize> = (0..FRAME_COUNT)
            .map(|i| {
                let ls = splash_scene_lines(i * TICKS_PER_FRAME);
                line_w(&ls[0])
            })
            .collect();
        assert!(
            widths.iter().all(|&w| w == widths[0]),
            "frames render at differing widths: {widths:?}"
        );
    }

    #[test]
    fn splash_scene_lines_renders_full_scene_and_animates() {
        // Include the foreground colour as well as the glyph — the
        // animation is mostly composition (more pixels appear as the
        // beanstalk grows), so a glyph-only compare may miss subtle
        // recolours; a (color, glyph) compare catches both.
        let render = |ls: &[ratatui::text::Line]| {
            ls.iter()
                .map(|l| {
                    l.spans
                        .iter()
                        .map(|s| format!("{:?}{}", s.style.fg, s.content))
                        .collect::<String>()
                })
                .collect::<Vec<_>>()
        };
        // Every frame renders every art row.
        let f0 = splash_scene_lines(0);
        assert_eq!(f0.len(), SPLASH_FRAME_0.len());
        // Probe several cycle slots — at least one pair must differ.
        let r0 = render(&f0);
        let r5 = render(&splash_scene_lines(5 * TICKS_PER_FRAME));
        let r13 = render(&splash_scene_lines(13 * TICKS_PER_FRAME));
        assert!(
            r0 != r5 || r0 != r13,
            "frames should differ — scene is not animating"
        );
        // The cycle wraps back to frame 0 after FRAME_COUNT × TICKS_PER_FRAME.
        assert_eq!(
            render(&splash_scene_lines(FRAME_COUNT * TICKS_PER_FRAME)),
            r0
        );
        // Square pixels: a painted cell is the two-block "██" (sampled
        // from the last frame which has the most pixels lit).
        assert!(splash_scene_lines(13 * TICKS_PER_FRAME)
            .iter()
            .flat_map(|l| l.spans.iter())
            .any(|s| s.content.as_ref() == "██"));
    }

    #[test]
    fn splash_animation_completes_within_min_duration() {
        // Pin the speed-up: one full cycle should finish in less than
        // the splash's 3 s minimum duration, so the boot splash always
        // lands on the final-bloom frame before the table replaces it.
        let cycle_ms = FRAME_COUNT * TICKS_PER_FRAME * 30;
        assert!(
            cycle_ms < 3000,
            "cycle is {cycle_ms} ms — exceeds 3 s splash duration; bump TICKS_PER_FRAME down"
        );
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
