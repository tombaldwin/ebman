//! Per-env background dispatch for the multi-select batch commands
//! (`:batch-rebuild` / `:batch-restart` / `:batch-deploy` / `:batch-tag`
//! / `:batch-untag` / `:batch-set-option`). Each `spawn_batch_*` helper
//! is the parameterised, fire-against-one-env counterpart of a single-
//! env spawner; the `cmd_batch_*` entry points in `cmd_write.rs` fan
//! these across the selection (after the `deny_write_batch` safety gate).
//!
//! All four share the existing audit + pending-pill + `AppMsg`-result
//! plumbing so a batch row is indistinguishable from a single-env op in
//! the audit log and the `:pending` overlay.
//!
//! 0.21+ lift: cluster moved out of `src/app.rs` as part of the
//! `spawn_*` clusters refactor (deferred from 0.15–0.20). Mirrors the
//! `spawn_why_red.rs` extraction; pure relocation, no behaviour change.

use super::{build_undo_entry, flatten_err, write_audit_entry, Action, App, AppMsg};

impl App {
    /// Fire a single non-destructive action for batch mode. Unlike
    /// `spawn_action` this doesn't need a `ConfirmModal` — the user already
    /// opted in by typing `:batch-…`. Only Rebuild and RestartAppServer are
    /// allowed; destructive actions still require per-env strict confirm.
    pub(super) fn spawn_batch_action(&mut self, action: Action, env: String) {
        write_audit_entry(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            action,
            &env,
            None,
        );
        self.push_pending(action.label(), env.clone());
        let env_for_msg = env.clone();
        self.spawn_aws(
            "batch_action",
            move |aws| async move {
                match action {
                    Action::Rebuild => aws.rebuild_env(&env).await,
                    Action::RestartAppServer => aws.restart_app_server(&env).await,
                    _ => Err(color_eyre::eyre::eyre!(
                        "batch-mode only supports Rebuild / Restart"
                    )),
                }
            },
            move |gen, result| AppMsg::ActionResult {
                gen,
                action,
                env_name: env_for_msg,
                result,
            },
        );
    }

    /// Per-env deploy dispatch for `:batch-deploy`. Shares the pending
    /// pill + audit + `ActionResult` plumbing with the existing single-env
    /// `:deploy` path via `Action::Deploy`.
    pub(super) fn spawn_batch_deploy(&mut self, env: String, label: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "Deploy",
            env.as_str(),
            &[("version", label.as_str())],
        );
        self.push_pending(Action::Deploy.label(), env.clone());
        let env_for_msg = env.clone();
        tokio::spawn(async move {
            let result = aws
                .deploy_version(&env, &label)
                .await
                .map_err(|e| flatten_err("deploy_version", e));
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action: Action::Deploy,
                env_name: env_for_msg,
                result,
            });
        });
    }

    /// Per-env tag / untag dispatch for `:batch-tag` / `:batch-untag`.
    /// `value = Some(v)` adds the tag; `value = None` removes the key.
    /// Audit + pending entries label the op so a query against the audit
    /// log can distinguish tag from untag.
    pub(super) fn spawn_batch_tag(
        &mut self,
        env: String,
        arn: String,
        key: String,
        value: Option<String>,
    ) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let is_add = value.is_some();
        let op_label = if is_add { "tag" } else { "untag" };
        let detail = match &value {
            Some(v) => {
                format!("stage=dispatched action=Tag target={env} key={key} value={v}")
            }
            None => format!("stage=dispatched action=Untag target={env} key={key}"),
        };
        crate::audit::append_raw(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &detail,
        );
        let pending_label = format!("{op_label} {key}");
        self.push_pending(pending_label.clone(), env.clone());
        let env_for_msg = env.clone();
        tokio::spawn(async move {
            let to_add: Vec<(String, String)> = match &value {
                Some(v) => vec![(key.clone(), v.clone())],
                None => Vec::new(),
            };
            let to_remove: Vec<String> = if value.is_none() {
                vec![key.clone()]
            } else {
                Vec::new()
            };
            let result = aws
                .update_tags(&arn, &to_add, &to_remove)
                .await
                .map_err(|e| flatten_err("update_tags", e));
            let _ = tx.send(AppMsg::TagUpdate {
                gen,
                env_name: env_for_msg,
                summary: pending_label,
                result,
            });
        });
    }

    /// Per-env option-settings dispatch for `:batch-set-option`. Each env
    /// is its own `UpdateEnvironment(option_settings)` call; the existing
    /// `spawn_option_settings_update` is selected-env-only so this is a
    /// parameterised parallel.
    pub(super) fn spawn_batch_set_option(
        &mut self,
        env: String,
        namespace: String,
        name: String,
        value: String,
    ) {
        // Resolve the env's application name from the cached fleet
        // — needed for the option-settings read that backs undo
        // capture. If the env vanished mid-batch (context switch /
        // termination race), we skip the dispatch with an audit
        // line rather than firing a write against a stale name.
        let Some(app_name) = self
            .environments
            .iter()
            .find(|e| e.name == env)
            .map(|e| e.application.clone())
        else {
            crate::audit::append_raw(
                self.context.account_id.as_deref(),
                self.context.profile.as_deref(),
                &self.context.region,
                &format!(
                    "stage=skipped action=SetOption target={env} reason=\"env not in current view\""
                ),
            );
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let detail = format!(
            "stage=dispatched action=SetOption target={env} ns={namespace} name={name} value={value}"
        );
        crate::audit::append_raw(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &detail,
        );
        let pending_label = format!("set-option {namespace}.{name}");
        self.push_pending(pending_label.clone(), env.clone());
        let env_for_msg = env.clone();
        let env_for_undo = env.clone();
        let pending_label_for_undo = pending_label.clone();
        let to_set_for_undo: Vec<(String, String, String)> =
            vec![(namespace.clone(), name.clone(), value.clone())];
        tokio::spawn(async move {
            // Undo capture: read the env's current option-settings
            // BEFORE the write so :undo can reverse this batch entry
            // alongside any per-env writes. Read failure is non-
            // blocking — the write still proceeds, undo just isn't
            // captured for the affected env. Mirrors the safety-net
            // semantics of `spawn_option_settings_update`.
            let undo_entry = match aws
                .fetch_env_option_settings(&app_name, &env_for_undo)
                .await
            {
                Ok(opts) => Some(build_undo_entry(
                    &env_for_undo,
                    &pending_label_for_undo,
                    &to_set_for_undo,
                    &[],
                    &opts,
                )),
                Err(_) => None,
            };
            let settings = vec![(namespace, name, value)];
            let result = aws
                .update_env_option_settings(&env, &settings, &[])
                .await
                .map_err(|e| flatten_err("update_env_option_settings", e));
            // Only record undo on a successful write — otherwise
            // :undo would "revert" a write that never landed. Same
            // contract as the single-env spawn.
            if result.is_ok() {
                if let Some(entry) = undo_entry {
                    let _ = tx.send(AppMsg::UndoCaptured { gen, entry });
                }
            }
            let _ = tx.send(AppMsg::OptionSettingsUpdate {
                gen,
                env_name: env_for_msg,
                summary: pending_label,
                result,
            });
        });
    }
}
