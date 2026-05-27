//! `ebman <verb>` non-interactive subcommands.
//!
//! Pre-0.15 every `run_*_cli` lived as an inline `async fn` in
//! `src/main.rs`, which ballooned to 2,600+ lines as the CLI surface
//! grew (audit/explain/lint --fix all landed in 0.14). The 0.14
//! architecture review's #1 finding was the resulting grab-bag.
//!
//! Each verb now lives in its own file under `src/cli/`, exposing
//! `pub async fn run(args: &[String]) -> Result<()>`. `main.rs`
//! dispatches by argv[1] and calls the matching `cli::<verb>::run`.
//! Shared CLI-only helpers (the `decide_poll` state machine, the
//! `--fix` dispatch-failure flag, the JSON-string escaper, the
//! cli-arg escaper) live here in `mod.rs`.
//!
//! Convention:
//! - Each module is named after the subcommand (`audit.rs`,
//!   `explain.rs`, ...) and exports exactly one public function:
//!   `pub async fn run(args: &[String]) -> Result<()>`. `args` is
//!   the full `std::env::args()` vector so callers can index from
//!   `args[1]` onwards uniformly.
//! - Exit codes follow the 0.13 CLI charter (locked in
//!   `BACKLOG.md`): 0 ok, 1 aws err, 2 usage err, 3 issues / drift,
//!   4 wait-for-green timeout, 5 auto-rollback fired.
//! - No `println!` inside the TUI alternate screen — these
//!   subcommands run before / outside TUI lifecycle, so plain
//!   stdout/stderr is fine.

pub mod action;
pub mod audit;
pub mod ctl;
pub mod drift;
pub mod envs;
pub mod explain;
pub mod lint;

/// Re-exports from the shared deploy-poll module. CLI subcommand
/// modules import via `crate::cli::{decide_poll, PollDecision}`;
/// the actual implementations live in `src/deploy_poll.rs` and are
/// shared with the TUI's `spawn_rollout_dispatch`.
pub(crate) use crate::deploy_poll::{decide_poll, PollDecision};

/// Re-exports of the canonical JSON helpers from `crate::util`. CLI
/// subcommand modules import these via `crate::cli::{json_string,
/// cli_esc}` so call-site rewrites are unnecessary; the actual
/// implementations live in `util.rs` and are shared across the
/// crate (lib + bin).
pub(crate) use crate::util::{json_escape as cli_esc, json_string};

#[cfg(test)]
mod tests {
    use super::*;

    // decide_poll matrix tests live in `src/deploy_poll.rs`
    // alongside the function itself (0.16 move).

    #[test]
    fn cli_esc_escapes_quotes_and_backslashes() {
        assert_eq!(cli_esc("hello"), "hello");
        assert_eq!(cli_esc("a\"b"), "a\\\"b");
        assert_eq!(cli_esc("a\\b"), "a\\\\b");
        // Newlines + tabs (added in 0.15) are also escaped so the
        // value can land in any JSON context safely.
        assert_eq!(cli_esc("a\nb"), "a\\nb");
        assert_eq!(cli_esc("a\tb"), "a\\tb");
    }

    #[test]
    fn json_string_wraps_in_quotes_and_escapes() {
        assert_eq!(json_string(""), "\"\"");
        assert_eq!(json_string("hello"), "\"hello\"");
        assert_eq!(json_string("a\"b"), "\"a\\\"b\"");
        // Round-trip via the YAML-superset parser.
        let s = "line1\nline2 \"with quotes\"";
        let escaped = json_string(s);
        let parsed: String =
            serde_yml::from_str(&escaped).expect("hand-rolled JSON should parse as YAML");
        assert_eq!(parsed, s);
    }
}
