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

/// One tick's worth of decision for the `action deploy` polling
/// loop. Pure — no AWS, no clock, no I/O — so the exit-code matrix
/// is unit-testable. Shared between [`action::run_deploy`] (the
/// `ebman action deploy` path) and the cross-region rollout loop in
/// [`action::run_rollout`].
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PollDecision {
    KeepPolling,
    Success,
    /// `--wait-for-green` deadline elapsed but rollback hasn't
    /// fired (either no rollback flag, or its deadline is later).
    WaitForGreenTimeout,
    /// `--auto-rollback` deadline elapsed and env still non-Green.
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
    // Both status=Ready AND health=Green/Ok required — EB briefly
    // leaves health Green while status flips to Updating right
    // after UpdateEnvironment, so a check on health alone
    // false-positives during the transition. See
    // `crate::app::deploy_settled_green`.
    if crate::app::deploy_settled_green(status, health) {
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

/// Re-exports of the canonical JSON helpers from `crate::util`. CLI
/// subcommand modules import these via `crate::cli::{json_string,
/// cli_esc}` so call-site rewrites are unnecessary; the actual
/// implementations live in `util.rs` and are shared across the
/// crate (lib + bin).
pub(crate) use crate::util::{json_escape as cli_esc, json_string};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decide_poll_green_plus_ready_returns_success_regardless_of_deadlines() {
        assert_eq!(
            decide_poll("Ready", "Green", 600, Some(300), Some(600), false),
            PollDecision::Success
        );
        assert_eq!(
            decide_poll("Ready", "Ok", 600, Some(300), Some(600), false),
            PollDecision::Success
        );
    }

    #[test]
    fn decide_poll_green_during_updating_is_not_success() {
        // The transition window: status=Updating + health=Green
        // happens briefly after UpdateEnvironment. Must NOT
        // false-positive Success.
        assert_eq!(
            decide_poll("Updating", "Green", 5, Some(300), Some(600), false),
            PollDecision::KeepPolling
        );
    }

    #[test]
    fn decide_poll_keep_polling_before_any_deadline() {
        assert_eq!(
            decide_poll("Updating", "Red", 60, Some(300), Some(600), false),
            PollDecision::KeepPolling
        );
    }

    #[test]
    fn decide_poll_wait_for_green_only_emits_timeout_once() {
        assert_eq!(
            decide_poll("Updating", "Red", 301, Some(300), None, false),
            PollDecision::WaitForGreenTimeout
        );
        // Second tick after the timeout was already emitted —
        // suppress so the caller doesn't double-log.
        assert_eq!(
            decide_poll("Updating", "Red", 302, Some(300), None, true),
            PollDecision::KeepPolling
        );
    }

    #[test]
    fn decide_poll_rollback_wins_when_both_deadlines_passed() {
        // When both `--wait-for-green` and `--auto-rollback`
        // deadlines have elapsed, the rollback dispatch wins
        // (it's the more-aggressive action).
        assert_eq!(
            decide_poll("Updating", "Red", 600, Some(300), Some(600), true),
            PollDecision::DispatchRollback
        );
    }

    #[test]
    fn decide_poll_wait_then_rollback_sequence() {
        // Typical sequence: tick at 301 emits the timeout, tick
        // at 600 dispatches the rollback.
        assert_eq!(
            decide_poll("Updating", "Red", 301, Some(300), Some(600), false),
            PollDecision::WaitForGreenTimeout
        );
        assert_eq!(
            decide_poll("Updating", "Red", 600, Some(300), Some(600), true),
            PollDecision::DispatchRollback
        );
    }

    #[test]
    fn decide_poll_rollback_only_no_intermediate_emission() {
        // No `--wait-for-green`, only `--auto-rollback`. Should
        // emit DispatchRollback at the deadline with no
        // intermediate WaitForGreenTimeout.
        assert_eq!(
            decide_poll("Updating", "Red", 300, None, Some(300), false),
            PollDecision::DispatchRollback
        );
    }

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
