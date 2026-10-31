//! What a freeze declare must say, and the refusal it causes.

use super::*;

/// The one refusal sentence for an active fleet freeze.
///
/// Shared because it was written twice — once in the CLI's freeze
/// refusal (now the CLI gate, `cli::write_refusal_unaudited`) and once
/// in the MCP write gate — with the same body and different
/// framing, so the two surfaces disagreed in wording about the same
/// condition and either could drift on the next edit.
pub(crate) fn refusal_message(marker: &FreezeMarker) -> String {
    let reason = marker.reason_or_default();
    if marker.persistent {
        return format!(
            "fleet freeze active ({reason}) — {}; lift with `{}`",
            marker.holder(),
            marker.remedy()
        );
    }
    format!(
        "fleet freeze active ({reason}) — lift with `{}` in the owning TUI (pid {})",
        marker.remedy(),
        marker.pid
    )
}

/// Longest reason a freeze takes. It is shown wherever the freeze
/// refuses a write, a one-line status bar among them, and whoever lifts
/// it needs to read it at a glance.
pub(crate) const REASON_MAX: usize = 200;

/// Why a freeze reason was refused. Every declare checks it: `ebman
/// freeze`, `:freeze-deploys`, `:incident START` and `freeze_fleet`.
/// Only the last did, and the reason the rule exists (one line in a
/// status bar, read by whoever lifts it) is the same for all four.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BadReason {
    /// A control character, or a line break or direction override
    /// Unicode spells outside them: each can make the one line show
    /// something other than what was declared.
    BreaksTheLine,
    TooLong,
}

impl BadReason {
    /// What is wrong, after "the reason".
    pub(crate) fn what(self) -> String {
        match self {
            Self::BreaksTheLine => {
                "must be one line, with no control characters — it is shown in a one-line \
                 status bar"
                    .to_string()
            }
            Self::TooLong => format!("is over {REASON_MAX} characters"),
        }
    }
}

pub(crate) fn check_reason(reason: &str) -> Result<(), BadReason> {
    if reason.chars().any(crate::util::rewrites_the_line) {
        return Err(BadReason::BreaksTheLine);
    }
    if reason.chars().count() > REASON_MAX {
        return Err(BadReason::TooLong);
    }
    Ok(())
}

/// The audit label for declaring a freeze.
pub(crate) fn declare_label(incident: bool) -> &'static str {
    if incident {
        "IncidentStart"
    } else {
        "FreezeDeploys"
    }
}
