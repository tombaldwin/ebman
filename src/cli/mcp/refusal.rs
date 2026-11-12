//! Refusals of MCP writes, and the audit identity every line of a write
//! files under. Split from `writes.rs` (2026-11-12) so `role.rs`,
//! `undo.rs` and `freeze.rs` reach these without globbing `writes`.
//!
//! `mod audited` below is the privacy boundary for [`Audited`]: its
//! field is private there, so a `WriteError::Refused` can only come from
//! [`Audited::record`], which writes the `stage=refused` line.

use super::writes::{PendingWrite, WriteVerb};
use super::*;

/// A refusal you can only hold if it has been recorded.
///
/// The point of the private field: outside this module `Audited`
/// cannot be constructed, and inside it the only constructors are the
/// two below — one that writes the `stage=refused` line, and one that
/// accepts a line the shared CLI funnel already wrote. So a
/// `WriteError::Refused` is proof an audit line exists.
///
/// This does not make the bug impossible. A new gate can still return
/// `WriteError::Invalid` for something that is really a policy
/// refusal. What it does is convert an invisible OMISSION into a
/// visible MISCATEGORISATION: the author has to name which kind it is,
/// and the wrong choice is a word in the diff rather than the absence
/// of one. Every other invariant here works the same way —
/// `rebuild_view`, the generation guards, match-arm order — none are
/// impossible to violate, all are made visible.
mod audited {
    /// A recorded refusal. The message is for the agent; the audit
    /// line is already on disk.
    #[derive(Debug, Clone)]
    pub(in crate::cli::mcp) struct Audited(String);

    impl Audited {
        /// Record a refusal and render it. The ONLY way to make an
        /// `Audited` from nothing.
        ///
        /// A demo server's refusal is recorded nowhere: its constructor
        /// made the process hermetic (`audit::make_hermetic`), and a demo
        /// server reads the real cross-process freeze marker, so its
        /// verdict is genuine while its fleet is not.
        pub(in crate::cli::mcp) fn record(
            profile: Option<&str>,
            region: &str,
            refused: &crate::audit::Refused<'_>,
            message: String,
        ) -> Self {
            crate::audit::append_refused(None, profile, region, refused);
            Audited(message)
        }

        /// Add context to the message without losing the proof.
        ///
        /// Inside the module, so the private field stays private and a
        /// `Refused` still means a line was written.
        pub(in crate::cli::mcp) fn map_message(self, f: impl FnOnce(String) -> String) -> Self {
            Audited(f(self.0))
        }

        pub(in crate::cli::mcp) fn message(&self) -> &str {
            &self.0
        }

        pub(in crate::cli::mcp) fn into_message(self) -> String {
            self.0
        }
    }
}

pub(super) use audited::Audited;

/// Why a write call failed.
///
/// The distinction is the audit obligation. `Refused` means a policy
/// stopped this — a pin, a freeze, read-only, an out-of-scope verb, an
/// operator declining — and every one of those leaves a
/// `stage=refused` line, because a blocked write and nobody trying
/// look identical afterwards otherwise. `Invalid` means nothing was
/// refused: a missing argument, an unknown environment, a spent token,
/// another write already in flight. There was no attempt to record.
///
/// Replaces a bare `String`, where the two were indistinguishable and
/// a new gate could return `Err("nope".into())` and silently audit
/// nothing. That happened in 0.40 and again in 0.42 — the latter found
/// by classifying these sites to write this type.
#[derive(Debug)]
pub(super) enum WriteError {
    /// Nothing was refused by policy; no audit line.
    Invalid(String),
    /// A policy refused this. The payload is proof it was recorded.
    Refused(Audited),
}

impl std::fmt::Display for WriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WriteError::Invalid(m) => f.write_str(m),
            WriteError::Refused(a) => f.write_str(a.message()),
        }
    }
}

impl WriteError {
    /// Add context, preserving which kind it is.
    pub(super) fn with_context(self, f: impl FnOnce(String) -> String) -> Self {
        match self {
            WriteError::Invalid(m) => WriteError::Invalid(f(m)),
            WriteError::Refused(a) => WriteError::Refused(a.map_message(f)),
        }
    }

    pub(super) fn into_message(self) -> String {
        match self {
            WriteError::Invalid(m) => m,
            WriteError::Refused(a) => a.into_message(),
        }
    }
}

// `?` on the many `ok_or("'env' is required")` sites keeps working,
// and lands on Invalid — the right default, since a refusal now has to
// be written deliberately.
impl From<&str> for WriteError {
    fn from(m: &str) -> Self {
        WriteError::Invalid(m.to_string())
    }
}
impl From<String> for WriteError {
    fn from(m: String) -> Self {
        WriteError::Invalid(m)
    }
}

impl Server {
    /// Record a scope refusal, and render it.
    ///
    /// A refusal that leaves no `stage=refused` line is the pre-0.37
    /// blind spot — a blocked write and no attempt at all look
    /// identical in the log. This one is worth seeing more than most:
    /// an ungranted verb is not advertised, so a client reaching it is
    /// working from a stale tool list or probing the surface, and
    /// neither is visible any other way.
    ///
    /// Demo writes nothing real, matching `gate_refusal`: the refusal
    /// is genuine, the fleet is not.
    pub(super) async fn refused_out_of_scope(
        &self,
        verb: WriteVerb,
        env: Option<&str>,
        region: Option<&str>,
        profile: &Option<String>,
    ) -> WriteError {
        let name = verb.tool_name();
        let region = self.audit_region(profile, region).await;
        let audit_profile = self.audit_profile(profile);
        // On a `--read-only` server, "start it with --allow-writes" is
        // advice that produces a startup error: the two contradict and
        // are refused together. The control the operator actually has
        // is removing the flag they set, and a remedy naming the wrong
        // control is the defect remedies exist to avoid — it goes into
        // the audit line as well as the agent's error.
        if self
            .mcp_read_only
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return WriteError::Refused(Audited::record(
                audit_profile.as_deref(),
                &region,
                &crate::audit::Refused {
                    action_label: verb.label(),
                    target: env.unwrap_or("-"),
                    rule: crate::audit::RefusalRule::NotGranted,
                    remedy: "this server was started with --read-only; the operator must remove \
                 that flag and restart it",
                    extras: &[],
                },
                format!(
                    "'{name}' is unavailable: this server was started with --read-only, \
                     which refuses every write regardless of what your client can do. \
                     Do not ask for --allow-writes — the two are refused together at \
                     startup. Ask the operator to remove --read-only if they want \
                     writes here."
                ),
            ));
        }
        WriteError::Refused(Audited::record(
            audit_profile.as_deref(),
            &region,
            &crate::audit::Refused {
                action_label: verb.label(),
                target: env.unwrap_or("-"),
                rule: crate::audit::RefusalRule::NotGranted,
                remedy: &format!("restart the MCP server with --allow-writes={name}"),
                extras: &[],
            },
            format!(
                "'{name}' is not in this server's write scope — start it with \
                 --allow-writes, or --allow-writes={name} to grant just this one"
            ),
        ))
    }

    /// The write gate for both MCP phases.
    ///
    /// Demo goes through the pure half: a demo server still reads the
    /// REAL cross-process freeze marker, so `ebman mcp serve --demo
    /// --allow-writes` attempted during a live `:freeze-deploys` was
    /// appending a real line to the real audit log. This module's own
    /// docs promise demo writes none, and a refusal being genuine does
    /// not make the fleet genuine.
    pub(super) async fn gate_refusal(
        &self,
        env: &str,
        profile: &Option<String>,
        region: Option<&str>,
        action_label: &str,
        // What the role config says of the write's verb
        // (`Server::role_check`); `NotAsked` for `dlq_undo`, which goes
        // out under the delete's session (D4).
        role: &crate::config::RoleCheck,
    ) -> Option<Audited> {
        let freeze = self.freeze.active();
        // The UNAUDITED half deliberately, for both backends: the
        // recording happens in `Audited::record` so there is exactly
        // one place in this module that writes a refusal line. Going
        // through `cli::write_refusal` for the live case would audit
        // there instead, and a `Refused` would then exist that this
        // module had not recorded — which is the whole property the
        // type carries.
        let profile = self.audit_profile(profile);
        let (refusal, message, pin_profile) =
            crate::cli::write_refusal_unaudited(&self.safety_cfg, env, &profile, freeze, role)?;
        let region = self.audit_region(&profile, region).await;
        Some(Audited::record(
            pin_profile.as_deref(),
            &region,
            &crate::audit::Refused {
                action_label,
                target: env,
                rule: refusal.rule(),
                remedy: &refusal.remedy(),
                extras: &[],
            },
            message,
        ))
    }
}

impl Server {
    /// The profile a write's audit lines name: the call's, else the one
    /// the server runs under (`AWS_PROFILE`). Every line of one write
    /// names the same one; the ask and decline lines once wrote
    /// `profile=-` beside a dispatch that named it.
    pub(super) fn audit_profile(&self, profile: &Option<String>) -> Option<String> {
        profile.clone().or_else(|| self.ambient_profile.clone())
    }

    /// The region every line of a planned write files under: the one the
    /// plan's client found the env in, so the ask, any refusal and the
    /// session's dispatch agree, and `ebman audit` pairs them. Read by
    /// the SDK, instance metadata included, which `audit_region` cannot
    /// read: on an EC2 host it answered `-` while the dispatch named the
    /// region. `audit_region` only when the plan recorded none (demo).
    pub(super) async fn pending_region(&self, p: &PendingWrite) -> String {
        match &p.plan_region {
            Some(r) => r.clone(),
            None => self.audit_region(&p.profile, p.region.as_deref()).await,
        }
    }

    /// The region a refusal or ask line records: the one the write's
    /// client would have resolved, as its dispatched and completed lines
    /// do. `"-"` was written whenever the call named none, so a refusal
    /// and a dispatch for the same default-region write disagreed; and
    /// `"-"` still, never `unknown`, when nothing names one. Read from
    /// the environment and the profile file (`aws::resolve_region`):
    /// building a client to ask could probe instance metadata. A demo
    /// server writes no line, so it reads nothing.
    pub(super) async fn audit_region(
        &self,
        profile: &Option<String>,
        region: Option<&str>,
    ) -> String {
        if let Some(r) = region.filter(|r| !r.is_empty()) {
            return r.to_string();
        }
        if matches!(self.backend, Backend::Demo) {
            return "-".to_string();
        }
        // Under test the region comes from the injected client, as the
        // write's own lines' does: `resolve_region` reads nothing then.
        #[cfg(test)]
        if let Some(c) = &self.seams.client {
            return c.context.region.clone();
        }
        crate::aws::AwsClient::resolve_region(profile.as_deref())
            .await
            .unwrap_or_else(|| "-".to_string())
    }
}
