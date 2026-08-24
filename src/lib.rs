//! ebman — k9s-style TUI for AWS Elastic Beanstalk.
//!
//! The crate is split lib + bin: this library holds the testable logic
//! (all the modules below), and `src/main.rs` is a thin binary entry
//! point that wires argv parsing, logging, the TUI lifecycle, and
//! dispatch into the lib. See `CLAUDE.md` for working rules and
//! `BACKLOG.md` for the milestone plan.
//!
//! # What is public, and why so little
//!
//! Only what `src/main.rs` needs. Concretely: `App` and its seven
//! methods (`new`, `new_demo`, `run`, `persist_state`, `set_read_only`,
//! `set_log_reload`, `reload_requested`), the ten `cli::*::run` entry
//! points, `config::load` with `Config`'s two icon accessors, one
//! function each from `audit` / `project` / `freeze` / `control` /
//! `splash` / `util`, `resolve_icons_setting`, and the `Tui` /
//! `LogReloadHandle` aliases. Everything else is `pub(crate)`.
//!
//! **Read the module list below as modules, not as surface.** An
//! earlier version of this note said "only the nine modules `main.rs`
//! needs are public" and left the impression that the API was about
//! nine things. It was 4565 items. `pub mod app` alone carried most of
//! them, because a public module re-exports everything `pub` inside it
//! — `App`'s 91 public fields, the mode types, `ViewState`, the lot.
//! Narrowing the *modules* had barely moved the number; narrowing the
//! items took it to 212. (Both figures are `cargo public-api` with no
//! flags; omitting auto-derived and blanket impls the surface is 67.)
//!
//! The cost of leaving it wide was not theoretical. Adding a field to
//! an internal struct was a semver event twice in two releases —
//! `Form.banner` in 0.31.0 and `WorkerQueues.dlq_origin` in 0.32.0 —
//! which made `cargo-semver-checks` a tax on ordinary refactoring
//! rather than a safety net on the API anyone actually uses. And 0.33.0
//! shipped a Breaking section enumerating **38** items that inherited a
//! type-identity change from a ratatui bump, none of which any consumer
//! could have wanted. After this narrowing that set is **two**: the
//! `Tui` alias and `ControlOp::Key(KeyEvent)`, both irreducible because
//! `main.rs` genuinely owns the alt-screen lifecycle and the control
//! channel.
//!
//! Keeping it narrow is enforced, not remembered: `unreachable_pub`
//! (below) catches a `pub` item inside a `pub(crate)` module, which is
//! the leak that made `App::theme`'s ratatui `Color` fields publicly
//! readable while `Theme` itself stayed unnameable.

// `unwrap_used` / `expect_used` are denied crate-wide (see
// Cargo.toml). Test code is exempt: a panic in a test IS the failure
// report, and `#[allow]` on every assertion would be noise. This does
// not weaken production checking: `cargo clippy --all-targets`
// compiles the lib WITHOUT cfg(test) as well, and that build still
// denies. Verified by planting an `unwrap` in production code and
// confirming clippy still errors — by the CI clippy job, not by a
// test, since a test cannot observe what clippy did.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]
// Catches the leak this narrowing was cleaning up: a `pub` item inside a
// `pub(crate)` module, which is invisible in the API listing but still
// widens it through any public signature that names it. `App::theme:
// Arc<Theme>` was exactly that — `Theme` is `pub` in a `pub(crate)`
// module, so its ratatui `Color` fields were publicly readable while the
// type stayed unnameable. Grep does not find that class; the compiler does.
#![warn(unreachable_pub)]

pub mod app;
pub mod audit;
pub(crate) mod aws;
pub mod cli;
pub(crate) mod commands;
pub mod config;
pub mod control;
pub(crate) mod cost_cache;
pub(crate) mod deploy_poll;
pub(crate) mod eb_cli;
pub(crate) mod form;
pub(crate) mod lint;
pub(crate) mod llm;
pub(crate) mod terraform;

// `font_probe` and `overlay` live in the shared `tui-common` crate so
// the sibling pgman repo can depend on the same code. Re-exported here
// so existing `crate::font_probe::*` / `crate::overlay::*` paths (and
// the `ebman::*` paths from the bin) keep working unchanged.
// Only the one function `main` calls, not the whole module. Re-exporting
// `font_probe` wholesale put `AutoResolved` and the two `detect_*` probes
// in ebman's public API to serve a single call site — and they are
// tb-tui-common's types, so a major bump there broke ebman's API for no
// consumer. Nothing inside this crate uses the module at all; the comment
// above claiming `crate::font_probe::*` paths depend on it was stale.
pub use tui_common::font_probe::resolve_icons_setting;
pub(crate) use tui_common::overlay;
pub(crate) mod demo_fixture;
pub mod freeze;
pub(crate) mod mode_action;
pub(crate) mod mode_detail;
pub(crate) mod mode_dlq;
pub(crate) mod plugins;
pub(crate) mod probe;
pub(crate) mod profiles;
pub mod project;
pub(crate) mod report_bug;
pub(crate) mod saved_config;
pub(crate) mod shell;
pub mod splash;
pub(crate) mod sso;
pub(crate) mod state;
pub(crate) mod theme;
pub(crate) mod ui;
pub(crate) mod update_check;
pub mod util;

use std::io::Stdout;

use ratatui::{backend::CrosstermBackend, Terminal};
use tracing_subscriber::{reload, EnvFilter};

/// Concrete `ratatui` terminal we drive through the alt-screen. Lives
/// in the lib so `app::App` can hold a mutable reference to it through
/// long-running operations (embedded shell, `$EDITOR` hand-off).
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Best-effort terminal restore: every step attempted regardless of
/// whether the previous one failed.
///
/// Shared because it was written twice with a `?` between the steps —
/// once in `main`'s `leave_tui`, once in the `$EDITOR` hand-off — and in
/// both a failure in `disable_raw_mode` meant the alternate screen was
/// never left. The operator is then looking at a dead screen with mouse
/// capture on, typing `reset` blind. Which step fails matters less than
/// the fact that a `?` between them stops the rest from running, and
/// there is no useful second move here: if the terminal will not
/// restore, trying the remaining steps anyway is strictly better than
/// stopping.
pub fn restore_terminal(terminal: &mut Tui) {
    use crossterm::terminal::{disable_raw_mode, LeaveAlternateScreen};
    let _ = disable_raw_mode();
    let _ = crossterm::execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        crossterm::event::DisableMouseCapture
    );
    let _ = terminal.show_cursor();
}

/// Handle for live-reloading the log filter from the running app.
/// Constructed by `main::init_logging` and threaded onto `App` so
/// `:loglevel` can mutate the active subscriber at runtime.
pub type LogReloadHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;
