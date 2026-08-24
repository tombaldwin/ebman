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
//! Only what `src/main.rs` needs: `app`, `audit`, `cli`, `config`,
//! `control`, `freeze`, `project`, `splash`, `util`, plus the
//! `font_probe` re-export and the `Tui` / `LogReloadHandle` aliases.
//! Everything else is `pub(crate)`.
//!
//! It used to be all 33 modules — about 500 public items, 107 public
//! structs, 94% of them with every field `pub` — serving a `main.rs`
//! that touches twelve. The original rationale (integration tests, and
//! a sibling crate sharing types with `pgman`) went stale: the tests
//! are inline `#[cfg(test)]` and need `pub(crate)`, and pgman consumes
//! the extracted `tb-tui-common` crate instead.
//!
//! The cost of leaving it wide was not theoretical. Adding a field to
//! an internal struct was a semver event twice in two releases —
//! `Form.banner` in 0.31.0 and `WorkerQueues.dlq_origin` in 0.32.0 —
//! which made `cargo-semver-checks` a tax on ordinary refactoring
//! rather than a safety net on the API anyone actually uses.

// `unwrap_used` / `expect_used` are denied crate-wide (see
// Cargo.toml). Test code is exempt: a panic in a test IS the failure
// report, and `#[allow]` on every assertion would be noise. This does
// not weaken production checking: `cargo clippy --all-targets`
// compiles the lib WITHOUT cfg(test) as well, and that build still
// denies. Verified by planting an `unwrap` in production code and
// confirming clippy still errors — by the CI clippy job, not by a
// test, since a test cannot observe what clippy did.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

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
pub use tui_common::font_probe;
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
