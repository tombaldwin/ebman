//! Configuration-template commands — `:config-save`, `:config-delete`,
//! `:config-apply`, `:config-inspect`. Each routes through either an
//! existing `spawn_config_*` helper (delete / apply / inspect) or
//! issues a direct `create_config_template` call with audit + pending
//! plumbing (save).
//!
//! Ninth slice of the `execute_command` split. Same parent-module
//! visibility pattern as the other `cmd_*` sub-modules.

use super::{flatten_err, Action, App, AppMsg};

impl App {
    pub(crate) fn cmd_config_save(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            None => {
                self.error_message =
                    Some("usage: :config-save <template-name>  (uses selected env)".into());
            }
            Some(template) => {
                let Some(env) = self.selected_env().cloned() else {
                    self.error_message = Some(
                        "no env selected — press 1-9, click a row, or type ' to jump by name"
                            .into(),
                    );
                    return;
                };
                if self.deny_write(&env.name, "config-save") {
                    return;
                }
                let Some(env_id) = env.id.clone() else {
                    self.error_message = Some("env has no internal ID — refresh and retry".into());
                    return;
                };
                let template = template.to_string();
                let app_name = env.application.clone();
                let aws = self.aws.clone();
                let tx = self.msg_tx.clone();
                let gen = self.generation;
                self.status_message = Some(format!(
                    "saving config from {} as template '{template}'…",
                    env.name
                ));
                let action = Action::ConfigSave;
                let display_env = env.name.clone();
                let template_for_msg = template.clone();
                let action_label = format!("{action:?}");
                crate::audit::append_action_dispatched(
                    self.context.account_id.as_deref(),
                    self.context.profile.as_deref(),
                    &self.context.region,
                    &action_label,
                    &display_env,
                    &[("template", template.as_str())],
                );
                self.push_pending(action.label(), display_env.clone());
                tokio::spawn(async move {
                    let result = aws
                        .create_config_template(&app_name, &template, &env_id)
                        .await
                        .map_err(|e| flatten_err("create_config_template", e));
                    let labelled = result
                        .map(|_| ())
                        .map_err(|e| format!("config-save '{template_for_msg}': {e}"));
                    let _ = tx.send(AppMsg::ActionResult {
                        gen,
                        action,
                        env_name: display_env,
                        result: labelled,
                    });
                });
            }
        }
    }

    pub(crate) fn cmd_config_delete(&mut self, rest: &[&str]) {
        match (rest.first().copied(), rest.get(1).copied()) {
            (Some(app_name), Some(_)) => {
                // Template names can contain spaces — join everything
                // after the app name so :config-delete app "Dev config
                // pre-redis" works as typed.
                let template = rest[1..].join(" ");
                self.spawn_config_delete_template(app_name.to_string(), template);
            }
            _ => {
                self.error_message =
                    Some("usage: :config-delete <application> <template-name>".into());
            }
        }
    }

    pub(crate) fn cmd_config_apply(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            None => {
                self.error_message = Some(
                    "usage: :config-apply <template-name>  (applies to selected env; template name may contain spaces)".into(),
                );
            }
            Some(_) => {
                // Join all rest tokens so multi-word template names work
                // as typed. The overlay's `a`/enter keys bypass this
                // parser and call spawn_config_apply_template directly.
                let template = rest.join(" ");
                let Some(env) = self.selected_env().cloned() else {
                    self.error_message = Some(
                        "no env selected — press 1-9, click a row, or type ' to jump by name"
                            .into(),
                    );
                    return;
                };
                self.spawn_config_apply_template(env.name.clone(), template);
            }
        }
    }

    /// Single-arg form: `:config-inspect TEMPLATE` (template name
    /// may contain spaces). Uses the selected env's application.
    /// Two-arg form with whitespace is ambiguous with multi-word
    /// template names, so the overlay's `i` keybind is the right
    /// path for cross-app inspection.
    pub(crate) fn cmd_config_inspect(&mut self, rest: &[&str]) {
        if rest.is_empty() {
            self.error_message = Some(
                "usage: :config-inspect TEMPLATE  (uses selected env's app; use `i` in :saved-configs for cross-app inspect)".into(),
            );
            return;
        }
        let template = rest.join(" ");
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        self.spawn_config_inspect_template(env.application, template);
    }
}
