//! Wait-for-Green polling state machine shared between the CLI
//! ([`crate::cli::action::run_deploy`] / `run_rollout`) and the TUI
//! (`App::spawn_rollout_dispatch`).
//!
//! Pure — no AWS, no clock, no I/O — so the exit-code matrix is
//! unit-testable. Caller drives the polling loop and feeds each
//! tick's `(status, health, elapsed_secs)` plus the configured
//! `--wait-for-green` and `--auto-rollback` deadlines; this returns
//! the next action (keep polling / success / emit timeout milestone
//! / dispatch rollback).
//!
//! Pre-0.16 this lived in `cli/mod.rs`; the TUI side
//! (`spawn_rollout_dispatch`) re-implemented the wait-for-green
//! case inline. Promoted here as a sibling lib module so both
//! paths share one implementation + one test matrix.

/// One tick's worth of decision for the deploy / rollout polling
/// loop.
#[derive(Debug, PartialEq, Eq)]
pub enum PollDecision {
    /// Env hasn't crossed Green and no deadline has elapsed yet.
    KeepPolling,
    /// Env reached Green/Ok with status=Ready — exit 0.
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

/// Pure poll-tick decision. Both status=Ready AND health=Green/Ok
/// required for Success — EB briefly leaves health Green while
/// status flips to Updating right after UpdateEnvironment, so a
/// check on health alone false-positives during the transition.
/// See [`crate::app::deploy_settled_green`].
pub fn decide_poll(
    status: &str,
    health: &str,
    elapsed_secs: u64,
    wait_for_green_secs: Option<u64>,
    auto_rollback_secs: Option<u64>,
    wait_for_green_timeout_emitted: bool,
) -> PollDecision {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn green_plus_ready_returns_success_regardless_of_deadlines() {
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
    fn green_during_updating_is_not_success() {
        // The transition window: status=Updating + health=Green
        // happens briefly after UpdateEnvironment. Must NOT
        // false-positive Success.
        assert_eq!(
            decide_poll("Updating", "Green", 5, Some(300), Some(600), false),
            PollDecision::KeepPolling
        );
    }

    #[test]
    fn keep_polling_before_any_deadline() {
        assert_eq!(
            decide_poll("Updating", "Red", 60, Some(300), Some(600), false),
            PollDecision::KeepPolling
        );
    }

    #[test]
    fn wait_for_green_only_emits_timeout_once() {
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
    fn rollback_wins_when_both_deadlines_passed() {
        // When both `--wait-for-green` and `--auto-rollback`
        // deadlines have elapsed, the rollback dispatch wins
        // (it's the more-aggressive action).
        assert_eq!(
            decide_poll("Updating", "Red", 600, Some(300), Some(600), true),
            PollDecision::DispatchRollback
        );
    }

    #[test]
    fn wait_then_rollback_sequence() {
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
    fn rollback_only_no_intermediate_emission() {
        // No `--wait-for-green`, only `--auto-rollback`. Should
        // emit DispatchRollback at the deadline with no
        // intermediate WaitForGreenTimeout.
        assert_eq!(
            decide_poll("Updating", "Red", 300, None, Some(300), false),
            PollDecision::DispatchRollback
        );
    }
}
