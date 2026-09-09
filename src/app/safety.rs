//! The write-safety gate.
//!
//! Every mutating dispatch site — TUI, CLI and MCP alike — funnels
//! through `deny_write` / `deny_write_batch` before it touches AWS.
//! `--deny-write`, `safety.envs.NAME.read_only` and
//! `safety.accounts.NAME.read_only` are all resolved here, so there is
//! exactly one place to audit.

use super::*;

impl App {
    /// Resolve the effective read-only lock for a destructive action
    /// against `env_name`. Layered:
    ///
    /// 1. Global `--read-only` flag / `:readonly on` (master switch).
    /// 2. Per-env safety pin (`safety.envs.NAME.read_only = true` in
    ///    config.toml).
    /// 3. Per-account safety pin (`safety.accounts.NAME.read_only = true`)
    ///    matched against the active profile name.
    ///
    /// Any of these returning `true` blocks the action; the operator-
    /// facing error message can differentiate via `read_only_reason`.
    pub(crate) fn is_read_only_for(&self, env_name: &str) -> bool {
        // Deliberately delegating rather than repeating the chain.
        // These were two separate four-branch cascades kept in the same
        // order by a comment asking a human to do it — and the ONE
        // difference between them would have been invisible: a
        // predicate that says "allowed" while the reason function has
        // something to say is a write that slips through, and the
        // reverse is a refusal with no explanation. The allocation is
        // irrelevant here (this runs on a confirm, or once per env in a
        // batch of tens — never per frame).
        self.read_only_reason(env_name).is_some()
    }

    /// Enforce the read-only gate for a destructive action against
    /// `env_name`. Returns `true` (and sets `self.error_message` to a
    /// `"<reason> — <verb> disabled"` toast) when the env is locked;
    /// `false` (no side effects) otherwise. Designed to be the single
    /// guard at the top of every `spawn_*`-style destructive helper:
    ///
    /// ```ignore
    /// if self.deny_write(&env.name, "rollback") { return; }
    /// ```
    ///
    /// Saves duplicating the `is_read_only_for` + `read_only_reason`
    /// + `error_message` triplet at every call site (~25 of them).
    pub(crate) fn deny_write(&mut self, env_name: &str, verb: &str) -> bool {
        // `--demo` mode refuses writes outright (see spawn_action's
        // matching guard for the rationale — synthetic fleet, fake
        // AwsClient, real audit log).
        //
        // When BOTH demo_mode and a safety-pin / read-only lock apply,
        // mention both in the toast — operators using `--demo` to
        // validate their `safety.envs.*` / `safety.accounts.*` config
        // before going live shouldn't have to exit demo to confirm
        // the pin is wired correctly. (0.17.4 review)
        if self.demo_mode {
            let pin_reason = self.read_only_reason(env_name);
            let suffix = match pin_reason {
                Some(reason) => format!(" — would also refuse: {reason}"),
                None => String::new(),
            };
            self.error_message = Some(format!(
                "demo mode — {verb} not dispatched (writes are inert; press q to exit){suffix}"
            ));
            return true;
        }
        let Some(refusal) = self.refusal_for(env_name) else {
            return false;
        };
        // Record the attempt. Until this landed a blocked write left no
        // trace whatsoever — the dispatch never happened, so there was
        // no dispatched/completed pair, and repeated attempts on a
        // pinned env were indistinguishable from nobody trying.
        self.audit_refusal(env_name, verb, &refusal);
        let reason = self.render_refusal(&refusal);
        self.error_message = Some(format!("{reason} — {verb} disabled"));
        true
    }

    /// Read-only gate for a *batch* destructive op over `env_names`.
    /// Returns `true` (and sets `self.error_message`) when the op must
    /// be refused. Unlike single-env [`App::deny_write`], a batch is gated
    /// per-env: if ANY selected env is locked the whole batch is
    /// refused (refuse-all, not skip-some — a safety pin shouldn't be
    /// silently routed around for the unpinned remainder), with the
    /// locked env names named so the operator can deselect them.
    ///
    /// Catches the env-independent gates (`--demo`, global read-only,
    /// `:freeze-deploys`) first via a representative `is_read_only_for`
    /// probe so those produce their normal whole-fleet message, then
    /// scans for per-env / per-account pins. Mirrors the precedence in
    /// [`App::is_read_only_for`]. `verb` names the op for the toast.
    pub(crate) fn deny_write_batch(&mut self, env_names: &[String], verb: &str) -> bool {
        // Demo mode + global/freeze gates are env-independent: probe
        // with the first env (or "") so the existing single-env path
        // produces the familiar "demo mode …" / "read-only mode …" /
        // "deploys frozen …" toast rather than a per-env list.
        let probe = env_names.first().map(|s| s.as_str()).unwrap_or("");
        if self.demo_mode || self.read_only || self.deploy_freeze.is_some() {
            return self.deny_write(probe, verb);
        }
        let locked: Vec<String> = env_names
            .iter()
            .filter(|n| self.is_read_only_for(n))
            .cloned()
            .collect();
        if locked.is_empty() {
            return false;
        }
        // Use the first locked env's reason as the headline (per-env
        // and per-account pins read the same regardless of which env);
        // list the locked names so the operator knows what to deselect.
        let reason = self
            .read_only_reason(&locked[0])
            .unwrap_or_else(|| "read-only mode".into());
        // One line for the batch, naming every locked env: the refusal
        // was refuse-all, and filing it per-env would imply the
        // unlocked remainder went through, which is exactly the
        // misreading `deny_write_batch` exists to prevent.
        if let Some(refusal) = self.refusal_for(&locked[0]) {
            self.audit_refusal(&locked.join(","), verb, &refusal);
        }
        self.error_message = Some(format!(
            "{reason} — {verb} refused: {} of {} selected env(s) locked ({})",
            locked.len(),
            env_names.len(),
            locked.join(", ")
        ));
        true
    }

    /// Human-readable explanation of *why* an env is read-only, used
    /// in the toast / footer when a destructive action is blocked.
    /// Returns `None` when the env isn't locked (caller shouldn't have
    /// called this; defensive return). The three reasons are ordered
    /// to match `is_read_only_for`'s precedence.
    pub(crate) fn read_only_reason(&self, env_name: &str) -> Option<String> {
        Some(self.render_refusal(&self.refusal_for(env_name)?))
    }

    /// The typed decision for `env_name`, before any wording is applied.
    ///
    /// Split out from `read_only_reason` because a refusal now has to be
    /// *recorded* as well as shown, and the audit log needs the rule
    /// name — which is precisely what rendering throws away.
    pub(crate) fn refusal_for(&self, env_name: &str) -> Option<crate::write_gate::Refusal> {
        // The DECISION is `write_gate::decide`, shared with the CLI and
        // MCP paths. The WORDING below is not shared and should not be:
        // a toast can afford the freeze age and the `:incident END`
        // hint, and a CLI line cannot. Converging the messages too
        // would have been a visible regression for no benefit.
        crate::write_gate::decide(&crate::write_gate::WriteContext {
            env: env_name,
            profile: self.context.profile.as_deref(),
            global_read_only: self.read_only,
            frozen: self.deploy_freeze.is_some(),
            safety_envs: &self.cfg.safety_envs,
            safety_accounts: &self.cfg.safety_accounts,
        })
    }

    /// Render a refusal in the TUI's voice — the freeze age, the
    /// `:incident END` hint. See `write_gate`'s module docs for why this
    /// is deliberately not shared with the CLI.
    fn render_refusal(&self, refusal: &crate::write_gate::Refusal) -> String {
        match refusal {
            crate::write_gate::Refusal::GlobalReadOnly => "read-only mode (global toggle)".into(),
            crate::write_gate::Refusal::Frozen => {
                // `decide` only reports THAT a freeze applies; the
                // detail lives here because only this surface has room
                // for it.
                //
                // NOT `as_ref()?`. That was the first version, and `?`
                // in a function returning `Option<String>` propagates
                // `None` — which here means "not read-only", i.e. the
                // write proceeds. Unreachable today (both values come
                // from the same `Option` in the same function), but a
                // fail-OPEN shape inside a write gate is the one thing
                // this module exists to avoid. Refuse with less detail
                // instead.
                let Some(freeze) = self.deploy_freeze.as_ref() else {
                    return "deploys frozen".into();
                };
                let age = (chrono::Utc::now() - freeze.frozen_at).num_seconds().max(0);
                let age =
                    crate::app::humanize_short_age(std::time::Duration::from_secs(age as u64));
                // When the freeze came from `:incident START`, point the
                // operator at the gesture that actually closes it — a
                // bare :thaw-deploys would lift the lock but leave the
                // incident banner up, which is rarely what they meant.
                let unlock_hint = if self.incident.is_some() {
                    ":incident END to close"
                } else {
                    ":thaw-deploys to unfreeze"
                };
                if freeze.reason.is_empty() {
                    format!("deploys frozen ({age} ago) — {unlock_hint}")
                } else {
                    format!(
                        "deploys frozen ({age} ago): {} — {unlock_hint}",
                        freeze.reason
                    )
                }
            }
            crate::write_gate::Refusal::EnvPinned { env } => {
                format!("read-only mode (env pinned via safety.envs.{env})")
            }
            crate::write_gate::Refusal::AccountPinned { profile } => {
                format!("read-only mode (account pinned via safety.accounts.{profile})")
            }
        }
    }

    /// Record a refusal in the audit log.
    ///
    /// The region is the ROW's, not home: under a multi-region fan-out
    /// the selected env is usually elsewhere, and an audit line filed
    /// against the wrong region is worse than none.
    fn audit_refusal(&self, target: &str, verb: &str, refusal: &crate::write_gate::Refusal) {
        crate::audit::append_action_refused(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.region_for_name(target),
            verb,
            target,
            refusal.rule(),
            &refusal.remedy(),
        );
    }
}
