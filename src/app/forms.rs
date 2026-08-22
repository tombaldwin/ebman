//! Modal form flows: building a form for an action, routing keys
//! into it, and turning a submitted form into AWS calls.
//!
//! Every `open_*_form` seeds `App::form`; `handle_form_key` owns the
//! editing keymap; `submit_form` is the single exit point that turns
//! collected field values into a spawned write.

use super::*;

impl App {
    /// Open a modal form. Captures the env at open-time (so later main-table
    /// cursor moves don't redirect the submit), spawns a
    /// `DescribeConfigurationSettings` fetch to pre-fill values, and flips
    /// to `Mode::Form`. The form stays in `FormState::Loading` until the
    /// `FormPrefilled` AppMsg lands.
    pub(crate) fn open_form(&mut self, mut form: crate::form::Form) {
        // LocalConfig forms don't need an AWS pre-fill — the caller has
        // already populated the field values from the live `App` state.
        // Skip the DescribeConfigurationSettings round-trip and go straight
        // to Ready so the user can type immediately.
        if matches!(form.submit, crate::form::FormSubmit::LocalConfig) {
            form.state = crate::form::FormState::Ready;
            self.form = Some(form);
            self.mode = Mode::Form;
            return;
        }
        let env_name = form.env_name.clone();
        // Look up the env's application from the live env list. We need it
        // for DescribeConfigurationSettings; the form itself only knows the
        // env name.
        let app_name = match self.environments.iter().find(|e| e.name == env_name) {
            Some(e) => e.application.clone(),
            None => {
                self.error_message = Some(format!("env '{env_name}' not in current list"));
                return;
            }
        };
        self.form = Some(form);
        self.mode = Mode::Form;
        let client = self.client_for_env(&env_name);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let settings = match client.resolve().await {
                Ok(aws) => aws
                    .fetch_env_option_settings(&app_name, &env_for_msg)
                    .await
                    .map_err(|e| flatten_err("fetch_env_option_settings", e)),
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            let _ = tx.send(AppMsg::FormPrefilled {
                gen,
                env_name: env_for_msg,
                settings,
            });
        });
    }

    /// Key handler for `Mode::Form`. Loading-state forms ignore input
    /// (operator waits for the pre-fill); Ready forms route through Tab /
    /// arrow nav + per-field input; Submitting forms ignore input (waiting
    /// for the AppMsg::OptionSettingsUpdate that lands the result).
    pub(crate) fn handle_form_key(&mut self, key: KeyEvent) {
        use crate::form::{FieldKind, FormState};
        // Resolve current state before borrowing the form mutably so the
        // submit branch can dispatch through self.
        let state = self.form.as_ref().map(|f| f.state.clone());
        let cursor_kind = self
            .form
            .as_ref()
            .and_then(|f| f.current_field().map(|fld| fld.kind.clone()));
        match state {
            None => return,
            Some(FormState::Loading) | Some(FormState::Submitting) => {
                if matches!(key.code, KeyCode::Esc) {
                    self.form = None;
                    self.mode = Mode::Normal;
                }
                return;
            }
            Some(FormState::Ready) => {}
        }
        // Submit shortcut works regardless of focused-field kind.
        if matches!(key.code, KeyCode::Char('s')) && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.submit_form();
            return;
        }
        if matches!(key.code, KeyCode::Esc) {
            self.form = None;
            self.mode = Mode::Normal;
            return;
        }
        // Field navigation that's always available: Tab, Shift-Tab, Up, Down.
        // Up/Down would conflict with vim-style j/k inside text input — we
        // don't bind j/k for nav inside the form. Exception: when the
        // focused field is a MultiSelect, Up/Down (and j/k) move the
        // *option cursor* within the field rather than between fields;
        // Tab/Shift-Tab still leave the field.
        let is_multi = matches!(cursor_kind.as_ref(), Some(FieldKind::MultiSelect { .. }));
        let between_fields = match key.code {
            KeyCode::Tab => Some(1),
            KeyCode::BackTab => Some(-1),
            KeyCode::Up | KeyCode::Down if !is_multi => {
                if matches!(key.code, KeyCode::Up) {
                    Some(-1)
                } else {
                    Some(1)
                }
            }
            _ => None,
        };
        if let Some(delta) = between_fields {
            if let Some(form) = self.form.as_mut() {
                form.move_cursor(delta);
            }
            return;
        }
        // In-field option-cursor movement for MultiSelect fields. Wraps
        // around the option list both ways.
        if is_multi
            && matches!(
                key.code,
                KeyCode::Up | KeyCode::Down | KeyCode::Char('j') | KeyCode::Char('k')
            )
        {
            if let Some(form) = self.form.as_mut() {
                if let Some(field) = form.current_field_mut() {
                    if let FieldKind::MultiSelect { options } = &field.kind {
                        let n = options.len();
                        if n > 0 {
                            let delta: isize =
                                matches!(key.code, KeyCode::Down | KeyCode::Char('j')) as isize * 2
                                    - 1;
                            let cur = field.option_cursor as isize;
                            let next = ((cur + delta) % n as isize + n as isize) % n as isize;
                            field.option_cursor = next as usize;
                        }
                    }
                }
            }
            return;
        }
        // Per-kind editing on the focused field.
        let Some(form) = self.form.as_mut() else {
            return;
        };
        let Some(field) = form.current_field_mut() else {
            return;
        };
        // Live-revalidate after every edit so the inline error clears as the
        // operator fixes it.
        match (cursor_kind.unwrap_or(FieldKind::Text), key.code) {
            (FieldKind::Text, KeyCode::Backspace) => {
                field.value.pop();
            }
            (FieldKind::Text, KeyCode::Char(c)) if is_text_input(&key) => {
                field.value.push(c);
            }
            (FieldKind::Integer { .. }, KeyCode::Backspace) => {
                field.value.pop();
            }
            (FieldKind::Integer { .. }, KeyCode::Char(c))
                if c.is_ascii_digit() || (c == '-' && field.value.is_empty()) =>
            {
                field.value.push(c);
            }
            (FieldKind::Boolean, KeyCode::Char(' ')) => {
                field.value = if field.value == "true" {
                    "false".into()
                } else {
                    "true".into()
                };
            }
            (FieldKind::Boolean, KeyCode::Char('t')) => {
                field.value = "true".into();
            }
            (FieldKind::Boolean, KeyCode::Char('f')) => {
                field.value = "false".into();
            }
            (FieldKind::Select { options }, KeyCode::Left)
            | (FieldKind::Select { options }, KeyCode::Char('h')) => {
                let i = options.iter().position(|o| o == &field.value).unwrap_or(0);
                let next = (i + options.len() - 1) % options.len();
                field.value = options[next].clone();
            }
            (FieldKind::Select { options }, KeyCode::Right)
            | (FieldKind::Select { options }, KeyCode::Char('l')) => {
                let i = options.iter().position(|o| o == &field.value).unwrap_or(0);
                let next = (i + 1) % options.len();
                field.value = options[next].clone();
            }
            (FieldKind::MultiSelect { options }, KeyCode::Char(' ')) => {
                if let Some(opt) = options.get(field.option_cursor) {
                    field.value = crate::form::toggle_multi(&field.value, opt);
                }
            }
            _ => {}
        }
        // Clear stale error on this field after any edit.
        let _ = crate::form::validate_field(&field.value, &field.kind).map(|_| field.error = None);
    }

    /// Validate the form; if good, dispatch via the existing option-settings
    /// helper and switch to Submitting. Failures keep the form open with
    /// per-field error messages.
    fn submit_form(&mut self) {
        let Some(form) = self.form.as_mut() else {
            return;
        };
        if let Err(failing) = form.validate() {
            form.cursor = failing[0];
            return;
        }
        // LocalConfig submits write `config.toml` and apply changes live to
        // the running App. No AWS round-trip, so close out immediately.
        if matches!(form.submit, crate::form::FormSubmit::LocalConfig) {
            self.submit_local_config();
            return;
        }
        let env_name = form.env_name.clone();
        let summary = form.summary.clone();
        let (to_set, to_remove) = form.to_option_settings();
        form.state = crate::form::FormState::Submitting;
        // We can't reuse spawn_option_settings_update directly because it
        // reads self.selected_env() for the env_name; the form captured its
        // env at open time so we dispatch by-value here. Inlining keeps the
        // form's env binding authoritative.
        if self.deny_write(&env_name, "form submit") {
            self.form = None;
            self.mode = Mode::Normal;
            return;
        }
        if to_set.is_empty() && to_remove.is_empty() {
            self.status_message = Some("no changes to apply".into());
            self.form = None;
            self.mode = Mode::Normal;
            return;
        }
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.region_for_name(&env_name),
            "UpdateOptionSettings",
            env_name.as_str(),
            &[("summary", summary.as_str())],
        );
        self.push_pending(summary.clone(), env_name.clone());
        // No status_message ack here — the pending-actions pill in the
        // header (`⏳ N`) is the truth-source for in-flight work, and a
        // status_message ack would just race with whatever the operator
        // last set there. Completion fires a Success / Error toast.
        let client = self.client_for_env(&env_name);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        let summary_for_msg = summary.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.region_for_name(&env_name);
        // Undo capture — same shape as `spawn_option_settings_update`.
        // The form path lost the env's application name when it
        // stashed only `env_name`; recover it by looking up the
        // env in the cached fleet. Race with context switch leaves
        // `app_for_undo` as None and we silently skip capture.
        let app_for_undo = self
            .environments
            .iter()
            .find(|e| e.name == env_name)
            .map(|e| e.application.clone());
        let env_for_undo = env_name.clone();
        let summary_for_undo = summary.clone();
        let to_set_for_undo = to_set.clone();
        let to_remove_for_undo = to_remove.clone();
        tokio::spawn(async move {
            // One resolve for the undo read and the write, so :undo
            // can't offer the home region's settings for a row in
            // another one.
            let aws = match client.resolve().await {
                Ok(aws) => aws,
                Err(e) => {
                    let _ = tx.send(AppMsg::OptionSettingsUpdate {
                        gen,
                        env_name: env_for_msg,
                        summary: summary_for_msg,
                        result: Err(flatten_err("cached_client", e)),
                    });
                    return;
                }
            };
            let undo_entry = if let Some(app_name) = app_for_undo {
                match aws
                    .fetch_env_option_settings(&app_name, &env_for_undo)
                    .await
                {
                    Ok(opts) => Some(build_undo_entry(
                        &env_for_undo,
                        &summary_for_undo,
                        &to_set_for_undo,
                        &to_remove_for_undo,
                        &opts,
                    )),
                    Err(_) => None,
                }
            } else {
                None
            };
            let result = aws
                .update_env_option_settings(&env_for_msg, &to_set, &to_remove)
                .await
                .map_err(|e| flatten_err("update_env_option_settings", e));
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                "UpdateOptionSettings",
                &env_for_msg,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[("summary", &summary_for_msg)],
            );
            if result.is_ok() {
                if let Some(entry) = undo_entry {
                    let _ = tx.send(AppMsg::UndoCaptured { gen, entry });
                }
            }
            let _ = tx.send(AppMsg::OptionSettingsUpdate {
                gen,
                env_name: env_for_msg,
                summary: summary_for_msg,
                result,
            });
        });
        // Close the form so the user returns to wherever they were.
        // OptionSettingsUpdate handler will fire a toast on completion.
        self.form = None;
        self.mode = Mode::Normal;
    }

    /// Apply a [`crate::form::FormSubmit::LocalConfig`] submit: render the
    /// form values back into a [`Config`], write it to disk, and update the
    /// live `App` state so theme / icons / refresh interval changes take
    /// effect immediately. Other fields (notify_bell, required_tags,
    /// redact, grouped, extra_regions) are updated in place but
    /// only take effect on the next refresh / restart depending on what
    /// reads them — see the field docs.
    fn submit_local_config(&mut self) {
        let Some(form) = self.form.as_ref() else {
            return;
        };
        let snapshot = self.current_config_snapshot();
        let updated = form.apply_to_config(&snapshot);
        match crate::config::save(&updated) {
            Ok(()) => {
                let path = crate::config::config_path();
                self.apply_config_live(&updated);
                self.pin_status(format!("settings saved → {}", path.display()));
            }
            Err(e) => {
                self.error_message = Some(format!("settings save failed: {e}"));
            }
        }
        self.form = None;
        self.mode = Mode::Normal;
    }

    /// Build the `:settings` form pre-filled from the live App state and
    /// Open the `:subnets` MultiSelect form: lists subnets in the env's
    /// VPC via `DescribeSubnets`, pre-fills with the env's current
    /// `aws:ec2:vpc.Subnets` selection, submits via the shared
    /// option-settings update path. Bound to the env table cursor —
    /// reports an error and bails if no env is selected.
    pub(crate) fn open_subnets_form(&mut self) {
        self.open_multi_select_form(MultiSelectFlavour::Subnets);
    }

    /// Open the `:elb-subnets` MultiSelect form. Same EC2 list call as
    /// `:subnets` but targets `aws:ec2:vpc.ELBSubnets` — the option
    /// setting that controls which subnets the env's ELB attaches to.
    /// Web-tier only; worker-tier envs leave this empty.
    pub(crate) fn open_elb_subnets_form(&mut self) {
        self.open_multi_select_form(MultiSelectFlavour::ElbSubnets);
    }

    /// Open the `:security-groups` MultiSelect form. Same shape as
    /// `:subnets` but lists security groups in the env's VPC and
    /// targets `aws:autoscaling:launchconfiguration.SecurityGroups`.
    pub(crate) fn open_security_groups_form(&mut self) {
        self.open_multi_select_form(MultiSelectFlavour::SecurityGroups);
    }

    /// Shared open + async-load path for the two MultiSelect pickers.
    /// Opens the form in `Loading` state with an empty option list,
    /// then spawns a tokio task that fans out to fetch the VPC context
    /// (via DescribeConfigurationSettings) and the EC2 listing
    /// (DescribeSubnets / DescribeSecurityGroups). The result lands as
    /// `AppMsg::FormMultiSelectLoaded` which the handler matches by
    /// `field_key` to populate the form.
    pub(crate) fn open_multi_select_form(&mut self, flavour: MultiSelectFlavour) {
        use crate::form::{Form, FormField, FormSubmit};
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let (title_prefix, summary, field_key, label, ns, opt_name) = match flavour {
            MultiSelectFlavour::Subnets => (
                "subnets",
                "subnets update",
                "subnets",
                "Subnets",
                "aws:ec2:vpc",
                "Subnets",
            ),
            MultiSelectFlavour::ElbSubnets => (
                "elb-subnets",
                "elb-subnets update",
                "elb_subnets",
                "ELB subnets",
                "aws:ec2:vpc",
                "ELBSubnets",
            ),
            MultiSelectFlavour::SecurityGroups => (
                "security-groups",
                "security-groups update",
                "security_groups",
                "Security groups",
                "aws:autoscaling:launchconfiguration",
                "SecurityGroups",
            ),
        };
        let placeholder = FormField::multi_select(
            field_key,
            label,
            Vec::new(),
            Vec::new(),
            Some::<String>("space toggle · ↑↓ option cursor · tab field".into()),
        );
        let form = Form::loading(
            format!("{title_prefix} — {}", env.name),
            env.name.clone(),
            summary.to_string(),
            vec![placeholder],
            FormSubmit::OptionSettings {
                mappings: vec![(field_key.into(), ns.into(), opt_name.into())],
            },
        );
        // open_form would dispatch the default DescribeConfigurationSettings
        // pre-fill, which doesn't load EC2 inventory. Bypass it: stash the
        // form ourselves and spawn the multi-select-specific loader.
        self.form = Some(form);
        self.mode = Mode::Form;
        let client = self.client_for_env(&env.name);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env.name.clone();
        let app_name = env.application.clone();
        let field_key_for_msg = field_key.to_string();
        tokio::spawn(async move {
            // Subnets and security groups are region-scoped: the home
            // region's VPC inventory for another region's env would
            // offer IDs that don't exist there, and the form writes
            // them straight into the env's option settings.
            let result = match client.resolve().await {
                Ok(aws) => load_multi_select(aws, &app_name, &env_for_msg, flavour).await,
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            let _ = tx.send(AppMsg::FormMultiSelectLoaded {
                gen,
                env_name: env_for_msg,
                field_key: field_key_for_msg,
                result,
            });
        });
    }

    /// Open the `:settings` form pre-filled from the live App state and
    /// open it. Submit writes `config.toml` and live-applies any field
    /// that can change at runtime (see [`App::apply_config_live`]).
    pub(crate) fn open_settings_form(&mut self) {
        use crate::form::{Form, FormField, FormSubmit};
        let snapshot = self.current_config_snapshot();
        let bool_select = vec!["true".to_string(), "false".to_string()];
        let triple_select = vec!["auto".to_string(), "true".to_string(), "false".to_string()];
        let mut fields: Vec<FormField> = Vec::new();
        // Theme — present the known names as a select; user can still
        // type-edit via the value field if they prefer a wider list later.
        let theme_options = vec![
            "dark".to_string(),
            "light".to_string(),
            "high-contrast".to_string(),
        ];
        let mut theme_field = FormField::select(
            "theme",
            "Theme",
            theme_options.clone(),
            Some::<String>("dark / light / high-contrast".into()),
        );
        // Pre-fill from current Config. Theme name is always one of the
        // known options at this point — App::new normalises unknown names
        // back to `dark`. Fall back to the first option defensively in
        // case a future theme is added without updating this list.
        theme_field.value = if theme_options.iter().any(|o| o == &snapshot.theme) {
            snapshot.theme.clone()
        } else {
            theme_options[0].clone()
        };
        fields.push(theme_field);

        let icons_options = vec![
            "unicode".to_string(),
            "ascii".to_string(),
            "powerline".to_string(),
            "auto".to_string(),
        ];
        let mut icons_field = FormField::select(
            "icons",
            "Icons",
            icons_options.clone(),
            Some::<String>("auto = probe the terminal at startup".into()),
        );
        icons_field.value = if icons_options
            .iter()
            .any(|o| o.eq_ignore_ascii_case(&snapshot.icons))
        {
            snapshot.icons.to_ascii_lowercase()
        } else {
            "unicode".to_string()
        };
        fields.push(icons_field);

        let mut refresh_field = FormField::integer(
            "refresh_interval_secs",
            "Refresh interval (s)",
            Some("How often the env list reloads from AWS"),
            Some(5),
            Some(600),
            false,
        );
        refresh_field.value = snapshot.refresh_interval.as_secs().to_string();
        fields.push(refresh_field);

        // redact_default and grouped_default are Option<bool> → use a
        // three-way select.
        let mut redact_field = FormField::select(
            "redact_default",
            "Redact by default",
            triple_select.clone(),
            Some::<String>("auto leaves the toggle to per-session state".into()),
        );
        redact_field.value = match snapshot.redact_default {
            None => "auto".into(),
            Some(true) => "true".into(),
            Some(false) => "false".into(),
        };
        fields.push(redact_field);

        let mut grouped_field = FormField::select(
            "grouped_default",
            "Group by app by default",
            triple_select,
            Some::<String>("auto leaves the toggle to per-session state".into()),
        );
        grouped_field.value = match snapshot.grouped_default {
            None => "auto".into(),
            Some(true) => "true".into(),
            Some(false) => "false".into(),
        };
        fields.push(grouped_field);

        let mut notify_field = FormField::select(
            "notify_bell",
            "Bell on new Red",
            bool_select,
            Some::<String>("ring BEL when an env transitions into Red".into()),
        );
        notify_field.value = if snapshot.notify_bell {
            "true".into()
        } else {
            "false".into()
        };
        fields.push(notify_field);

        let mut tags_field = FormField::text(
            "required_tags",
            "Required tags",
            Some::<String>("comma-separated; surfaced in :report".into()),
        );
        tags_field.value = snapshot.required_tags.join(",");
        fields.push(tags_field);

        let mut regions_field = FormField::text(
            "extra_regions",
            "Extra regions",
            Some::<String>("comma-separated; appended to :region picker".into()),
        );
        regions_field.value = snapshot.extra_regions.join(",");
        fields.push(regions_field);

        let form = Form::loading(
            "settings",
            String::new(),
            "settings".to_string(),
            fields,
            FormSubmit::LocalConfig,
        );
        self.open_form(form);
    }

    /// Build a [`Config`] from the App's current state. Used by the
    /// `:settings` form for pre-fill and as the base the form's edited
    /// fields are merged onto before writing back to disk.
    pub(crate) fn current_config_snapshot(&self) -> Config {
        let mut snapshot = Config {
            refresh_interval: self.refresh_interval,
            extra_regions: self.extra_regions.clone(),
            redact_default: Some(self.view.redact),
            grouped_default: Some(self.view.grouped()),
            // Snapshot the BASE theme name, not the currently-applied one;
            // otherwise a profile-overridden theme would persist as the
            // new default and erase the operator's per-profile mapping.
            theme: self.cfg.base_theme_name.clone(),
            icons: self.cfg.cfg_icons_raw.clone(),
            notify_bell: self.notify_bell,
            required_tags: self.cfg.required_tags.clone(),
            alarm_dimensions: self.cfg.alarm_dimensions.clone(),
            // Carried through the `:settings` round trip so a save
            // doesn't drop lines the model doesn't model.
            passthrough: self.cfg.passthrough.clone(),
            profile_themes: self.cfg.profile_themes.clone(),
            // Accounts live in config.toml only — :settings doesn't
            // surface them in the form (the assume-role schema would
            // need its own editor), so the snapshot just preserves
            // whatever was loaded.
            accounts: self.cfg.accounts.clone(),
            runbooks: self.cfg.runbooks.clone(),
            safety_envs: self.cfg.safety_envs.clone(),
            safety_accounts: self.cfg.safety_accounts.clone(),
            notify_webhook: self.cfg.notify_webhook.clone(),
            command_aliases: self.cfg.command_aliases.clone(),
            lint_disable: self.cfg.lint_disable.clone(),
            // `lint.fix_disable` is a CLI-only knob (no TUI surface
            // consumes it; `ebman lint --fix` reads via
            // `config::load_lint_fix_disables` directly). We re-read
            // from disk on snapshot so `:settings save` doesn't
            // silently drop the existing line.
            lint_fix_disable: crate::config::load_lint_fix_disables(),
            explain_enabled: false,
            explain_provider: String::new(),
            explain_model: String::new(),
            explain_api_key_env: String::new(),
            explain_ollama_url: String::new(),
            explain_max_tokens: 0,
        };
        // Single source of truth for the `[explain]` block: the
        // resolved `Settings` on App. `write_to_config` fills the
        // Config struct's discrete fields and uses empty-string
        // sentinels for defaults so the serialiser only emits the
        // lines the operator has actually configured.
        self.cfg.explain_settings.write_to_config(&mut snapshot);
        snapshot
    }
}
