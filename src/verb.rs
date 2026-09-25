//! One vocabulary for the write verbs several surfaces share.
//!
//! The TUI, the CLI (`ebman action`, `audit replay`, `lint --fix`) and
//! the MCP server each parse their own verb type, and each used to
//! carry its own spelling of the audit label — so one operation was
//! written under two names. MCP audited a restart as `Restart` where
//! every other surface wrote `RestartAppServer`; option writes were
//! `SetOption` from MCP, batch and `lint --fix` and
//! `UpdateOptionSettings` from TUI forms and deploy. `ebman audit
//! --action` matched one spelling and silently missed the other.
//!
//! The per-surface types stay — they are parse layers, each with its
//! own grammar — and map into [`Verb`], which owns the one spelling.
//! The maintainer's ruling (2026-09-25): `RestartAppServer`, the
//! majority and the AWS API's name, and `SetOption`, the verb rather
//! than the call. Logs already on disk keep matching through
//! `audit::ACTION_ALIASES`.
//!
//! Scoped to the verbs more than one surface writes. TUI-only actions
//! (`SsmRun`, `AlarmCreate`, …) keep their own labels: there is no
//! second spelling to drift from.

/// A write verb that more than one surface can dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verb {
    Deploy,
    RestartAppServer,
    Rebuild,
    Terminate,
    SetOption,
    DlqResend,
    DlqDelete,
    DlqPurge,
}

impl Verb {
    /// Every verb.
    pub(crate) const ALL: &'static [Verb] = &[
        Verb::Deploy,
        Verb::RestartAppServer,
        Verb::Rebuild,
        Verb::Terminate,
        Verb::SetOption,
        Verb::DlqResend,
        Verb::DlqDelete,
        Verb::DlqPurge,
    ];

    /// The `action=` label every surface writes for this verb. `const`
    /// so the TUI's refusal-label table can be built from it.
    pub(crate) const fn audit_label(self) -> &'static str {
        match self {
            Verb::Deploy => "Deploy",
            Verb::RestartAppServer => "RestartAppServer",
            Verb::Rebuild => "Rebuild",
            Verb::Terminate => "Terminate",
            Verb::SetOption => "SetOption",
            // The labels `spawn_dlq` has always audited under.
            Verb::DlqResend => "dlq-resend",
            Verb::DlqDelete => "sqs-delete",
            Verb::DlqPurge => "dlq-purge",
        }
    }

    /// The verb a label read back from the audit log names — under any
    /// spelling it has ever been written with (see [`ACTION_ALIASES`]).
    /// `None` for a label that is not a shared verb.
    pub(crate) fn from_audit_label(label: &str) -> Option<Verb> {
        let key = action_key(label);
        Verb::ALL.iter().copied().find(|v| v.audit_label() == key)
    }

    /// Does this verb destroy something? Rebuild recreates every
    /// resource; terminate ends the environment; delete and purge
    /// destroy messages. Restart, deploy, set-option and resend change
    /// state without destroying it — and flagging them would teach a
    /// client to ignore the flag. The MCP annotations table is checked
    /// against this, so the two cannot disagree.
    pub(crate) const fn destructive(self) -> bool {
        matches!(
            self,
            Verb::Rebuild | Verb::Terminate | Verb::DlqDelete | Verb::DlqPurge
        )
    }
}

/// Action labels that name the SAME operation, spelled differently by
/// different surfaces. First spelling is the comparison key.
///
/// MCP audits a restart as `Restart`; the TUI, CLI and replay write
/// `RestartAppServer`. Option writes are `SetOption` from MCP, batch and
/// `lint --fix`, and `UpdateOptionSettings` from TUI forms and deploy —
/// every one of them the same `update_env_option_settings` call. The
/// filter matched labels exactly, so `ebman audit --action
/// RestartAppServer` silently missed every MCP restart. `audit replay`
/// had already learned to accept both restart spellings; the filter had
/// not.
///
/// Reader-side, and kept after the writers converged on one spelling
/// (`Verb::audit_label`): logs already on disk still carry the old
/// ones, and they must keep matching. Lives here, beside the spellings
/// it reconciles, so the vocabulary is one file.
const ACTION_ALIASES: &[&[&str]] = &[
    &["RestartAppServer", "Restart"],
    &["SetOption", "UpdateOptionSettings"],
];

/// The key an action label is compared under: its alias group's first
/// spelling, or the label itself.
pub(crate) fn action_key(label: &str) -> &str {
    ACTION_ALIASES
        .iter()
        .find(|group| group.contains(&label))
        .map_or(label, |group| group[0])
}

#[cfg(test)]
mod tests {
    use super::Verb;

    #[test]
    fn every_verb_has_its_own_label() {
        let mut labels: Vec<&str> = Verb::ALL.iter().map(|v| v.audit_label()).collect();
        labels.sort_unstable();
        labels.dedup();
        assert_eq!(labels.len(), Verb::ALL.len(), "two verbs share a label");
    }

    /// The maintainer's ruling, pinned.
    #[test]
    fn the_ruled_spellings() {
        assert_eq!(Verb::RestartAppServer.audit_label(), "RestartAppServer");
        assert_eq!(Verb::SetOption.audit_label(), "SetOption");
    }
}

/// No production code spells a shared verb's audit label itself.
///
/// The drift this module exists to end: each surface kept its own
/// spelling, and one operation reached the log under two names. With
/// `Verb::audit_label` the one source, a label string reappearing
/// anywhere else is how the next split would start — a new audit site
/// typing `"UpdateOptionSettings"` or `"Restart"` by hand, compiling
/// fine, and quietly forking the verb again.
///
/// Scans every production source file, as string LITERALS (quote to
/// quote), for any shared label or old alias. `src/verb.rs` is the only
/// place allowed to spell them.
#[cfg(test)]
mod no_local_spellings {
    use super::{Verb, ACTION_ALIASES};

    #[test]
    fn no_surface_spells_a_shared_verb_itself() {
        let mut needles: Vec<String> = Verb::ALL
            .iter()
            .map(|v| format!("\"{}\"", v.audit_label()))
            .collect();
        for group in ACTION_ALIASES {
            for spelling in *group {
                needles.push(format!("\"{spelling}\""));
            }
        }
        needles.sort();
        needles.dedup();

        let mut offenders = Vec::new();
        let mut scanned = 0usize;
        for (path, src) in crate::app::tests::scan::source_files() {
            if crate::app::tests::scan::is_test_path(&path) || path.ends_with("src/verb.rs") {
                continue;
            }
            scanned += 1;
            let prod = crate::app::tests::scan::production_half(&src);
            for (n, line) in prod.lines().enumerate() {
                let code = crate::app::tests::scan::strip_line_comment(line);
                for needle in &needles {
                    if code.contains(needle.as_str()) {
                        offenders.push(format!("{path}:{}: {needle}", n + 1));
                    }
                }
            }
        }
        assert!(scanned > 50, "scanned only {scanned} files");
        assert!(
            offenders.is_empty(),
            "a shared verb's audit label spelled outside src/verb.rs — use \
             `Verb::audit_label()`, or the verb will fork in the log again: \
             {offenders:#?}"
        );
    }
}
