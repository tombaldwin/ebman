//! ebman — k9s-style TUI for AWS Elastic Beanstalk.
//!
//! The crate is split lib + bin: this library holds the testable logic
//! (all the modules below), and `src/main.rs` is a thin binary entry
//! point that wires argv parsing, logging, the TUI lifecycle, and
//! dispatch into the lib. See `CLAUDE.md` for working rules and
//! `BACKLOG.md` for the milestone plan.
//!
//! Splitting lib + bin lets integration tests and a sibling crate
//! (`tui-common` planned for a workspace with `pgman`) `use ebman::…`
//! the internal types instead of re-implementing them.

pub mod app;
pub mod aws;
pub mod commands;
pub mod config;
pub mod control;
pub mod cost_cache;
pub mod eb_cli;
pub mod form;
pub mod lint;

// `font_probe` and `overlay` live in the shared `tui-common` crate so
// the sibling pgman repo can depend on the same code. Re-exported here
// so existing `crate::font_probe::*` / `crate::overlay::*` paths (and
// the `ebman::*` paths from the bin) keep working unchanged.
pub use tui_common::font_probe;
pub use tui_common::overlay;
pub mod demo_fixture;
pub mod mode_action;
pub mod mode_detail;
pub mod mode_dlq;
pub mod plugins;
pub mod profiles;
pub mod project;
pub mod report_bug;
pub mod saved_config;
pub mod shell;
pub mod splash;
pub mod sso;
pub mod state;
pub mod theme;
pub mod ui;
pub mod update_check;
pub mod util;

use std::io::Stdout;

use ratatui::{backend::CrosstermBackend, Terminal};
use tracing_subscriber::{reload, EnvFilter};

/// Concrete `ratatui` terminal we drive through the alt-screen. Lives
/// in the lib so `app::App` can hold a mutable reference to it through
/// long-running operations (embedded shell, `$EDITOR` hand-off).
pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Handle for live-reloading the log filter from the running app.
/// Constructed by `main::init_logging` and threaded onto `App` so
/// `:loglevel` can mutate the active subscriber at runtime.
pub type LogReloadHandle = reload::Handle<EnvFilter, tracing_subscriber::Registry>;
