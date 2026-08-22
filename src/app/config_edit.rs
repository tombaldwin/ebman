//! Editing option settings from the Detail view's Config tab, plus
//! the saved-configuration-template CRUD (`apply` / `delete` /
//! `inspect`) reachable from the same surface.
//!
//! Edits are staged locally and only leave for AWS on commit, so an
//! abandoned edit costs nothing.

use super::*;

impl App {
    /// Open the in-place value editor for the Config-tab row under the
    /// cursor. No-op if the cursor isn't on an editable row (empty
    /// list). Refuses in read-only mode so the operator isn't left
    /// typing a value that can't be dispatched.
    pub(crate) fn start_config_edit(&mut self) {
        let env_name = match self.detail.as_ref() {
            Some(d) => d.env_name.clone(),
            None => return,
        };
        if self.deny_write(&env_name, "config editing") {
            return;
        }
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let items = crate::app::config_editable_items(detail);
        let Some(item) = items.get(detail.config_cursor) else {
            self.error_message = Some("no editable config rows".into());
            return;
        };
        let key = item.key.clone();
        // TextInput seeds the caret at the end of the value so the
        // operator can append immediately, or arrow left to edit.
        detail.config_edit = Some(ConfigEdit {
            kind: item.kind,
            key: item.key.clone(),
            original: item.value.clone(),
            input: item.value.clone().into(),
            mode: ConfigEditMode::Value,
        });
        self.status_message = Some(format!("editing {key} — enter saves, esc cancels"));
    }

    /// Key handling while the Config-tab in-place editor is open.
    /// Esc cancels, Enter commits, Backspace / printable chars edit
    /// the value buffer. Mirrors `handle_detail_search_key`.
    pub(crate) fn handle_config_edit_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc => {
                if let Some(d) = self.detail.as_mut() {
                    d.config_edit = None;
                }
                self.status_message = Some("config edit cancelled".into());
            }
            KeyCode::Enter => self.commit_config_edit(),
            KeyCode::Backspace => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.backspace();
                }
            }
            KeyCode::Delete => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.delete();
                }
            }
            KeyCode::Left => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.move_left();
                }
            }
            KeyCode::Right => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.move_right();
                }
            }
            KeyCode::Home => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.move_home();
                }
            }
            KeyCode::End => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.move_end();
                }
            }
            KeyCode::Char(c) if is_text_input(&key) => {
                if let Some(e) = self.detail.as_mut().and_then(|d| d.config_edit.as_mut()) {
                    e.insert(c);
                }
            }
            _ => {}
        }
    }

    /// Commit the open Config-tab edit. All three modes dispatch via
    /// the same `UpdateOptionSettings` (env var) / `UpdateTags` (tag)
    /// paths `:env set` / `:tag` use. `Value` sets the row's new
    /// value (unchanged → no-op); `NewRow` parses the `KEY=VALUE`
    /// buffer and sets the new row; `RenameKey` sets the new key +
    /// removes the old in one call, carrying the row's value across.
    /// Clears the editor either way.
    fn commit_config_edit(&mut self) {
        let Some(edit) = self.detail.as_mut().and_then(|d| d.config_edit.take()) else {
            return;
        };
        let ns = "aws:elasticbeanstalk:application:environment";
        match edit.mode {
            ConfigEditMode::Value => {
                if edit.input.text() == edit.original.as_str() {
                    self.status_message = Some(format!("{} unchanged", edit.key));
                    return;
                }
                match edit.kind {
                    ConfigItemKind::EnvVar => self.spawn_option_settings_update(
                        format!("env set {}", edit.key),
                        vec![(ns.into(), edit.key.clone(), edit.input.text().to_string())],
                        vec![],
                    ),
                    ConfigItemKind::Tag => self.spawn_tag_update(
                        vec![(edit.key.clone(), edit.input.text().to_string())],
                        vec![],
                    ),
                }
            }
            ConfigEditMode::NewRow => {
                let Some((k, v)) = crate::mode_detail::parse_new_config_row(edit.input.text())
                else {
                    self.error_message = Some("new row needs KEY=VALUE (non-empty key)".into());
                    return;
                };
                match edit.kind {
                    ConfigItemKind::EnvVar => self.spawn_option_settings_update(
                        format!("env set {k}"),
                        vec![(ns.into(), k, v)],
                        vec![],
                    ),
                    ConfigItemKind::Tag => self.spawn_tag_update(vec![(k, v)], vec![]),
                }
            }
            ConfigEditMode::RenameKey => {
                let new_key = edit.input.trimmed().to_string();
                if new_key.is_empty() {
                    self.error_message = Some("rename: the new key can't be empty".into());
                    return;
                }
                if new_key == edit.original {
                    self.status_message = Some(format!("{} unchanged", edit.key));
                    return;
                }
                // Carry the row's current value across to the new key.
                let value = self.detail.as_ref().and_then(|d| {
                    config_editable_items(d)
                        .into_iter()
                        .find(|it| it.kind == edit.kind && it.key == edit.key)
                        .map(|it| it.value)
                });
                let Some(value) = value else {
                    self.error_message = Some("rename: the row no longer exists".into());
                    return;
                };
                let old = edit.key.clone();
                match edit.kind {
                    ConfigItemKind::EnvVar => self.spawn_option_settings_update(
                        format!("env rename {old} -> {new_key}"),
                        vec![(ns.into(), new_key, value)],
                        vec![(ns.into(), old)],
                    ),
                    ConfigItemKind::Tag => self.spawn_tag_update(vec![(new_key, value)], vec![old]),
                }
            }
        }
    }

    /// `n` on the Config tab — open the add-a-new-row editor. The new
    /// row's kind (tag vs env var) is taken from the section the
    /// cursor currently sits in; an empty editable list defaults to
    /// an env var (the more common edit target). The buffer is typed
    /// as `KEY=VALUE`.
    pub(crate) fn start_config_add(&mut self) {
        let env_name = match self.detail.as_ref() {
            Some(d) => d.env_name.clone(),
            None => return,
        };
        if self.deny_write(&env_name, "config editing") {
            return;
        }
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let items = crate::app::config_editable_items(detail);
        let kind = items
            .get(detail.config_cursor)
            .map(|i| i.kind)
            .unwrap_or(ConfigItemKind::EnvVar);
        detail.config_edit = Some(ConfigEdit {
            kind,
            key: String::new(),
            original: String::new(),
            input: TextInput::new(),
            mode: ConfigEditMode::NewRow,
        });
        let what = match kind {
            ConfigItemKind::EnvVar => "env var",
            ConfigItemKind::Tag => "tag",
        };
        self.status_message = Some(format!(
            "new {what} — type KEY=VALUE, enter saves, esc cancels"
        ));
    }

    /// `r` on the Config tab — open the key-rename editor for the row
    /// under the cursor. `input` is seeded with the current key;
    /// commit dispatches a remove-old + set-new (keeping the value)
    /// as one `UpdateOptionSettings` / `UpdateTags` call.
    pub(crate) fn start_config_rename(&mut self) {
        let env_name = match self.detail.as_ref() {
            Some(d) => d.env_name.clone(),
            None => return,
        };
        if self.deny_write(&env_name, "config editing") {
            return;
        }
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let items = crate::app::config_editable_items(detail);
        let Some(item) = items.get(detail.config_cursor) else {
            self.error_message = Some("no editable config rows".into());
            return;
        };
        let key = item.key.clone();
        detail.config_edit = Some(ConfigEdit {
            kind: item.kind,
            key: item.key.clone(),
            original: item.key.clone(),
            input: item.key.clone().into(),
            mode: ConfigEditMode::RenameKey,
        });
        self.status_message = Some(format!(
            "renaming {key} — type the new key, enter saves, esc cancels"
        ));
    }

    /// `x` on the Config tab — arm a delete of the row under the
    /// cursor. The actual `UpdateTags` / `UpdateOptionSettings`
    /// removal waits for the `y` confirmation (see the
    /// `config_delete_confirm` interception in the key handler).
    pub(crate) fn arm_config_delete(&mut self) {
        let env_name = match self.detail.as_ref() {
            Some(d) => d.env_name.clone(),
            None => return,
        };
        if self.deny_write(&env_name, "config editing") {
            return;
        }
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        let items = crate::app::config_editable_items(detail);
        let Some(item) = items.get(detail.config_cursor) else {
            self.error_message = Some("no editable config rows".into());
            return;
        };
        let key = item.key.clone();
        detail.config_delete_confirm = Some(detail.config_cursor);
        self.status_message = Some(format!("delete {key}? — y confirms, any other key cancels"));
    }

    /// Confirmed delete of the armed Config-tab row — dispatches the
    /// removal (`UpdateTags` remove / `UpdateOptionSettings` remove).
    pub(crate) fn commit_config_delete(&mut self) {
        let Some(idx) = self
            .detail
            .as_mut()
            .and_then(|d| d.config_delete_confirm.take())
        else {
            return;
        };
        let Some(detail) = self.detail.as_ref() else {
            return;
        };
        let items = crate::app::config_editable_items(detail);
        let Some(item) = items.get(idx) else {
            self.error_message = Some("config row no longer exists".into());
            return;
        };
        let kind = item.kind;
        let key = item.key.clone();
        match kind {
            ConfigItemKind::EnvVar => {
                let ns = "aws:elasticbeanstalk:application:environment";
                self.spawn_option_settings_update(
                    format!("env unset {key}"),
                    vec![],
                    vec![(ns.into(), key)],
                );
            }
            ConfigItemKind::Tag => {
                self.spawn_tag_update(vec![], vec![key]);
            }
        }
    }

    /// Dispatch `UpdateEnvironment(template_name)`. Used by both the typed
    /// `:config-apply TEMPLATE` command and the `a`/enter key in the
    /// interactive saved-configs overlay. Reads template + env directly
    /// so callers can pass strings with embedded spaces (the typed-command
    /// parser joins rest with single spaces; the overlay passes the raw
    /// template name).
    pub(crate) fn spawn_config_apply_template(&mut self, env_name: String, template: String) {
        if self.deny_write(&env_name, "config-apply") {
            return;
        }
        let client = self.client_for_env(&env_name);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        // In-flight ack lives on the pending pill; completion toasts.
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.region_for_name(&env_name),
            "ConfigApply",
            env_name.as_str(),
            &[("template", template.as_str())],
        );
        self.push_pending(Action::ConfigApply.label(), env_name.clone());
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => aws
                    .apply_config_template(&env_for_msg, &template)
                    .await
                    .map_err(|e| flatten_err("apply_config_template", e)),
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action: Action::ConfigApply,
                env_name: env_for_msg,
                result,
            });
        });
    }

    /// Dispatch `DeleteConfigurationTemplate`. Same shape as
    /// `spawn_config_apply_template`; bypasses the typed-command parser so
    /// the overlay can pass template names with embedded spaces.
    pub(crate) fn spawn_config_delete_template(&mut self, app_name: String, template: String) {
        // config-delete is app-scoped, not env-scoped — the template
        // lives at the application level. Per-account safety still
        // applies; per-env doesn't. The global / account-pin gate fires
        // via `deny_write` with an empty env name (which never matches
        // any `safety_envs` key).
        if self.deny_write("", "config-delete") {
            return;
        }
        let client = self.client_for_app(&app_name);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let target = format!("{app_name}/{template}");
        self.status_message = Some(format!(
            "deleting template '{template}' from app '{app_name}'…"
        ));
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.region_for_app(&app_name),
            "ConfigDelete",
            &target,
            &[],
        );
        self.push_pending(Action::ConfigDelete.label(), target.clone());
        let template_for_msg = template.clone();
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => aws
                    .delete_config_template(&app_name, &template)
                    .await
                    .map_err(|e| flatten_err("delete_config_template", e)),
                Err(e) => Err(flatten_err("cached_client", e)),
            }
            .map_err(|e| format!("config-delete '{template_for_msg}': {e}"));
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action: Action::ConfigDelete,
                env_name: target,
                result,
            });
        });
    }

    /// Fetch a template's option settings and surface them as a TextOverlay.
    /// Read-only — no read-only-mode gate. Called by `:config-inspect` and
    /// by the `i` keybind in the interactive saved-configs overlay.
    pub(crate) fn spawn_config_inspect_template(&mut self, app_name: String, template: String) {
        let client = self.client_for_app(&app_name);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let title = format!("template — {app_name}/{template}");
        // In-flight ack: pending pill. Inspect result lands as a TextOverlay.
        tokio::spawn(async move {
            let aws = match client.resolve().await {
                Ok(aws) => aws,
                Err(e) => {
                    let _ = tx.send(AppMsg::TextOverlay {
                        gen,
                        title,
                        body: format!("config-inspect: {}", flatten_err("cached_client", e)),
                    });
                    return;
                }
            };
            let body = match aws.describe_template_settings(&app_name, &template).await {
                Ok(settings) if settings.is_empty() => {
                    "(template has no option settings)".to_string()
                }
                Ok(settings) => format_template_settings(&settings),
                Err(e) => format!("error: {}", flatten_err("describe_template_settings", e)),
            };
            let _ = tx.send(AppMsg::TextOverlay { gen, title, body });
        });
    }

    /// Key handler for the interactive saved-configs overlay. Cursor moves
    /// with j/k/arrows/g/G; `a` applies the selected template to the current
    /// env (via `apply_config_template`); `x` deletes it; `c` closes the
    /// overlay and prefills `:config-save ` so the user can type a name; `?`
    /// stashes the overlay and surfaces the SavedConfigs help topic — closing
    /// help restores the overlay.
    pub(crate) fn handle_saved_configs_interactive_key(&mut self, key: KeyEvent) {
        // Mutate cursor in-place for navigation keys, then return early; for
        // dispatch keys (a/x/c) extract the selected pair, clear the overlay,
        // and re-enter the existing command path so we inherit read-only
        // gating + audit trail + ActionResult plumbing.
        {
            let Some(Overlay::SavedConfigsInteractive {
                items,
                cursor,
                confirm_delete,
            }) = self.current_overlay.as_mut()
            else {
                return;
            };
            if items.is_empty() {
                self.current_overlay = None;
                return;
            }
            let len = items.len();
            // When the delete confirm is armed, only y/Y/enter and n/N/esc do
            // anything — navigation keys are inert so a stray j/k doesn't
            // discard the confirm state and reset the cursor.
            if *confirm_delete {
                match key.code {
                    KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => {
                        *confirm_delete = false;
                        return;
                    }
                    KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter => {
                        // Fall through to the dispatch block below.
                    }
                    _ => return,
                }
            } else {
                match key.code {
                    KeyCode::Char('j') | KeyCode::Down => {
                        *cursor = (*cursor + 1).min(len.saturating_sub(1));
                        return;
                    }
                    KeyCode::Char('k') | KeyCode::Up => {
                        *cursor = cursor.saturating_sub(1);
                        return;
                    }
                    KeyCode::Char('g') | KeyCode::Home => {
                        *cursor = 0;
                        return;
                    }
                    KeyCode::Char('G') | KeyCode::End => {
                        *cursor = len.saturating_sub(1);
                        return;
                    }
                    KeyCode::Char('x') => {
                        *confirm_delete = true;
                        return;
                    }
                    _ => {}
                }
            }
        }
        let Some(Overlay::SavedConfigsInteractive {
            items,
            cursor,
            confirm_delete,
        }) = self.current_overlay.as_ref()
        else {
            return;
        };
        let cursor = *cursor;
        let confirm_delete = *confirm_delete;
        let selected = items.get(cursor).cloned();
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.current_overlay = None;
            }
            KeyCode::Char('a') | KeyCode::Enter if !confirm_delete => {
                if let Some((_app, template)) = selected {
                    self.current_overlay = None;
                    let Some(env) = self.selected_env().cloned() else {
                        self.error_message = Some(
                            "no env selected — press 1-9, click a row, or type ' to jump by name"
                                .into(),
                        );
                        return;
                    };
                    // Direct call bypasses execute_command's whitespace
                    // split so template names with spaces work.
                    self.spawn_config_apply_template(env.name, template);
                }
            }
            // y/Y/enter under armed-confirm dispatches the delete.
            KeyCode::Char('y') | KeyCode::Char('Y') | KeyCode::Enter if confirm_delete => {
                if let Some((app_name, template)) = selected {
                    self.current_overlay = None;
                    self.spawn_config_delete_template(app_name, template);
                }
            }
            KeyCode::Char('c') => {
                self.current_overlay = None;
                self.command_input = "config-save ".into();
                self.mode = Mode::Command;
            }
            KeyCode::Char('i') => {
                // Inspect: close the interactive overlay and dispatch
                // config-inspect directly. Template name may contain spaces
                // (e.g. "Dev config pre-redis") — direct method call avoids
                // execute_command's whitespace-split parser.
                if let Some((app_name, template)) = selected {
                    self.current_overlay = None;
                    self.spawn_config_inspect_template(app_name, template);
                }
            }
            KeyCode::Char('?') => {
                self.help.pre_overlay = self.current_overlay.take();
                self.help.pre_mode = Some(self.mode);
                self.help.topic = HelpTopic::SavedConfigs;
                self.mode = Mode::Help;
            }
            _ => {}
        }
    }
}
