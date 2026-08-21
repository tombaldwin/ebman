//! The `:command` router.
//!
//! `execute_command` is pure one-liner routing — every arm body lives
//! in one of the `cmd_*` modules. `crate::commands::COMMANDS` is the
//! registry these arms are pinned against in CI.

use super::*;

impl App {
    pub(crate) fn execute_command(&mut self, raw: &str) {
        let line = raw.trim();
        if line.is_empty() {
            return;
        }
        // Expand user-defined aliases first — `alias.dp = "deploy
        // --auto-rollback 5m"` + `:dp build-900` becomes the line
        // `deploy --auto-rollback 5m build-900`. Single-level
        // expansion only so `alias.x = "x"` can't loop. The
        // expansion is owned (String) because the borrowed `raw`
        // doesn't outlive this scope.
        let expanded = expand_command_alias(line, &self.cfg.command_aliases);
        let line = expanded.as_str();
        let mut parts = line.split_whitespace();
        let Some(cmd) = parts.next() else { return };
        let rest: Vec<&str> = parts.collect();
        match cmd {
            "q" | "quit" => self.quit = true,
            "refresh" => self.manual_refresh(),
            "help" | "?" => {
                // Mirror the `?` keybind: scope help to the screen the user
                // was on before opening the command bar. The Command-mode
                // transition doesn't leave a breadcrumb, so we infer from
                // what's currently set (Detail view live, action flow open,
                // DLQ open, interactive overlay open).
                self.help.topic = if self.detail.is_some() {
                    HelpTopic::Detail
                } else if self.action_flow.is_some() {
                    HelpTopic::Action
                } else if self.dlq.is_some() {
                    HelpTopic::Dlq
                } else if matches!(
                    self.current_overlay,
                    Some(Overlay::SavedConfigsInteractive { .. })
                ) {
                    HelpTopic::SavedConfigs
                } else {
                    HelpTopic::Global
                };
                self.help.pre_mode = Some(self.mode);
                self.mode = Mode::Help;
            }
            "region" | "r" => self.cmd_region(&rest),
            "custom-platforms" | "platforms" => self.cmd_custom_platforms(),
            "accounts" => self.cmd_accounts(),
            "org-health" => self.cmd_org_health(),
            "find-env" => match rest.first().copied() {
                None => {
                    self.error_message = Some(
                        "usage: :find-env <name-substring>  (scans every AWS profile + AssumeRole account)"
                            .into(),
                    );
                }
                Some(needle) => self.cmd_find_env(needle),
            },
            "envs-by-version" => match rest.first().copied() {
                None => {
                    self.error_message = Some(
                        "usage: :envs-by-version <label>  (scans every AWS profile + AssumeRole account for envs running that exact version label)"
                            .into(),
                    );
                }
                Some(label) => self.cmd_envs_by_version(label),
            },
            "logs-insights" => {
                // Pass the whole remainder verbatim. `cmd_logs_insights`
                // parses the optional `--window WINDOW` prefix; everything
                // after is the Insights query (spacing + punctuation kept
                // exactly as the operator typed it).
                let args = rest.join(" ");
                self.cmd_logs_insights(&args);
            }
            "account" => self.cmd_account(&rest),
            "profile" | "p" => self.cmd_profile(&rest),
            "sort" => self.cmd_sort(&rest),
            "group" => self.cmd_group(&rest),
            "redact" => self.cmd_redact(&rest),
            "events" => {
                self.event_panel.visible =
                    parse_toggle(rest.first().copied(), self.event_panel.visible);
                if self.event_panel.visible && self.event_panel.events.is_empty() {
                    self.spawn_events();
                }
                self.status_message = Some(if self.event_panel.visible {
                    "events panel ON".into()
                } else {
                    "events panel off".into()
                });
            }
            "event-time" => self.cmd_event_time(&rest),
            "export" => self.export_tsv(),
            "json" => self.export_json(),
            "report" | "markdown" => self.export_markdown(),
            "readonly" => {
                self.read_only = parse_toggle(rest.first().copied(), self.read_only);
                self.status_message = Some(if self.read_only {
                    "read-only ON — destructive actions disabled".into()
                } else {
                    "read-only off".into()
                });
            }
            "pin" => self.toggle_pin_selected(),
            "alias" => match rest.first().copied() {
                Some(name) => {
                    let label = rest[1..].join(" ");
                    if label.is_empty() {
                        self.error_message = Some(
                            "usage: :alias <env-name> <label>  (label cannot be empty)".to_string(),
                        );
                    } else {
                        self.aliases.insert(name.to_string(), label.clone());
                        // Aliases are matched by the filter, so the visible
                        // rows may have just changed.
                        self.view.invalidate();
                        self.rebuild_view();
                        self.status_message = Some(format!("alias '{name}' → \"{label}\""));
                        self.persist_state();
                    }
                }
                None => {
                    if self.aliases.is_empty() {
                        self.status_message = Some("no aliases set".into());
                    } else {
                        let list: Vec<String> = self
                            .aliases
                            .iter()
                            .map(|(k, v)| format!("{k} → \"{v}\""))
                            .collect();
                        self.status_message = Some(format!("aliases: {}", list.join("  ")));
                    }
                }
            },
            "alias-drop" | "alias-rm" => match rest.first() {
                Some(name) => {
                    if self.aliases.remove(*name).is_some() {
                        self.view.invalidate();
                        self.rebuild_view();
                        self.status_message = Some(format!("alias '{name}' removed"));
                        self.persist_state();
                    } else {
                        self.error_message = Some(format!("no alias for '{name}'"));
                    }
                }
                None => self.error_message = Some("usage: :alias-drop <env-name>".into()),
            },
            "whatsnew" => self.open_whatsnew(),
            "about" | "credits" => self.open_about_overlay(),
            "apps-info" => self.open_apps_info_overlay(),
            "cost" => self.cmd_cost(&rest),
            "fleet-cost" => self.cmd_fleet_cost(),
            "promotions" => self.cmd_promotions(),
            "listeners" => self.cmd_listeners(),
            "listener-edit" => self.cmd_listener_edit(&rest),
            "rds" => self.cmd_rds(),
            "rds-attach" => self.cmd_rds_attach(),
            "rds-detach" => self.cmd_rds_detach(&rest),
            "options" => self.cmd_options(&rest),
            "config-diff" => self.cmd_config_diff(&rest),
            "config-diff-local" => self.cmd_config_diff_local(&rest),
            "explain" => self.cmd_explain(&rest),
            "env-edit" => self.cmd_env_edit(),
            "secrets" => self.cmd_secrets(&rest),
            "secret" => self.cmd_secret_view(&rest),
            "report-bug" => self.open_report_bug_overlay(),
            "settings" => {
                self.open_settings_form();
            }
            "capacity" => self.cmd_capacity(),
            "scaling-triggers" => self.cmd_scaling_triggers(),
            "subnets" => self.open_subnets_form(),
            "elb-subnets" => self.open_elb_subnets_form(),
            "security-groups" => self.open_security_groups_form(),
            "update" => {
                // Surface the upgrade command for whichever install channel
                // looks live. Doesn't actually upgrade — operators on
                // AWS-touching tools prefer conscious upgrades, and
                // self-replacing the binary across Cellar / cargo-bin /
                // tarball layouts has too many platform footguns.
                let channel = crate::update_check::detect_install_channel();
                let cmd = channel.upgrade_command();
                let current = env!("CARGO_PKG_VERSION");
                let msg = match self.update_available.as_ref() {
                    Some(release) => format!(
                        "update available: {current} → {}.  run: {cmd}",
                        release.version
                    ),
                    None => {
                        format!("already on the latest ({current}).  to force-reinstall: {cmd}")
                    }
                };
                // Best-effort yank to the clipboard so the operator can
                // paste the upgrade command directly. Silent if the
                // clipboard isn't reachable.
                if let Ok(mut cb) = arboard::Clipboard::new() {
                    let _ = cb.set_text(cmd.to_string());
                }
                self.pin_status(msg);
            }
            "history" => {
                self.current_overlay = Some(Overlay::History(self.format_message_log()));
            }
            "saved-configs" | "configs" => {
                let items = collect_saved_configs(&self.applications);
                if items.is_empty() {
                    self.current_overlay = Some(Overlay::SavedConfigs(format_saved_configs(
                        &self.applications,
                    )));
                } else {
                    self.current_overlay = Some(Overlay::SavedConfigsInteractive {
                        items,
                        cursor: 0,
                        confirm_delete: false,
                    });
                }
            }
            "plugins" => {
                if self.plugins.is_empty() {
                    self.status_message =
                        Some("no plugins — add ~/.config/ebman/commands.toml".into());
                } else {
                    let names: Vec<&str> = self.plugins.keys().map(String::as_str).collect();
                    self.status_message = Some(format!(":<plugin>  {}", names.join(", ")));
                }
            }
            "diff" => self.cmd_diff(&rest),
            "alarms" => {
                let env_opt = if let Some(d) = self.detail.as_ref() {
                    Some(d.env_name.clone())
                } else {
                    self.selected_env().map(|e| e.name.clone())
                };
                match env_opt {
                    Some(env_name) => self.spawn_alarms_fetch(env_name),
                    None => {
                        self.error_message = Some(
                            "no env selected — press 1-9, click a row, or type ' to jump by name"
                                .into(),
                        )
                    }
                }
            }
            "why" | "diagnose" => {
                let env_opt = if let Some(d) = self.detail.as_ref() {
                    Some((d.env_name.clone(), d.env_snapshot.application.clone()))
                } else {
                    self.selected_env()
                        .map(|e| (e.name.clone(), e.application.clone()))
                };
                match env_opt {
                    Some((env_name, app_name)) => self.open_why_red(env_name, app_name),
                    None => {
                        self.error_message = Some(
                            "no env selected — press 1-9, click a row, or type ' to jump by name"
                                .into(),
                        )
                    }
                }
            }
            "loglevel" => match rest.first() {
                None => {
                    self.status_message =
                        Some(format!("current log directive: {}", self.log_directive));
                }
                Some(level) => {
                    self.set_log_level(level);
                }
            },
            "cols" => self.cmd_cols(&rest),
            "save-view" => self.cmd_save_view(&rest),
            "view" => self.cmd_view(&rest),
            "views" => self.cmd_views(),
            "view-drop" => self.cmd_view_drop(&rest),
            "filter" | "f" => self.cmd_filter_load(&rest),
            "save" => self.cmd_save_filter(&rest),
            "drop" => self.cmd_drop_filter(&rest),
            "filters" => self.cmd_filters(),
            "batch-rebuild" => self.cmd_batch_action(Action::Rebuild),
            "batch-restart" => self.cmd_batch_action(Action::RestartAppServer),
            "batch-deploy" => self.cmd_batch_deploy(&rest),
            "batch-tag" => self.cmd_batch_tag_or_untag(true, &rest),
            "batch-untag" => self.cmd_batch_tag_or_untag(false, &rest),
            "batch-set-option" => self.cmd_batch_set_option(&rest),
            "versions" => self.cmd_versions(),
            "deploy" => self.cmd_deploy(&rest),
            "rollback" => self.cmd_rollback(&rest),
            "changes" => self.cmd_changes(),
            "lineage" => self.cmd_lineage(),
            "ssh" => self.cmd_ssh(&rest),
            "ssm-run" => self.cmd_ssm_run(&rest),
            "delete-version" => self.cmd_delete_version(&rest),
            "upgrade" => self.cmd_upgrade(&rest),
            "clone" => self.cmd_clone(&rest),
            "promote-env" => self.cmd_promote_env(&rest),
            "rollout" => self.cmd_rollout(&rest),
            "scale" => self.cmd_scale(&rest),
            "stop" => self.cmd_stop(),
            "start" => self.cmd_start(),
            "abort" => self.cmd_abort(),
            "pending" | "in-flight" | "inflight" => self.cmd_pending(),
            "rollbacks-armed" | "rb-armed" => self.cmd_rollbacks_armed(),
            "abort-rollback" => self.cmd_abort_rollback(&rest),
            "freeze-deploys" => self.cmd_freeze_deploys(&rest),
            "incident" => self.cmd_incident(&rest),
            "thaw-deploys" => self.cmd_thaw_deploys(),
            "undo" => self.cmd_undo(),
            "lint" => self.cmd_lint(&rest),
            "drift" => self.cmd_drift(&rest),
            "tag" => self.cmd_tag(&rest),
            "untag" => self.cmd_untag(&rest),
            "resources" | "res" => self.cmd_resources(),
            "rebuild" => self.cmd_rebuild(),
            "restart" => self.cmd_restart(),
            "terminate" => self.cmd_terminate(),
            "swap" => self.cmd_swap(&rest),
            "config-save" => self.cmd_config_save(&rest),
            "config-delete" => self.cmd_config_delete(&rest),
            "config-apply" => self.cmd_config_apply(&rest),
            "deployment-policy" => self.cmd_deployment_policy(&rest),
            "rolling-update" => self.cmd_rolling_update(&rest),
            "health-check-url" => self.cmd_health_check_url(&rest),
            "keypair" => self.cmd_keypair(&rest),
            "service-role" => self.cmd_service_role(&rest),
            "instance-profile" => self.cmd_instance_profile(&rest),
            "public-ip" => self.cmd_public_ip(&rest),
            "elb-scheme" => self.cmd_elb_scheme(&rest),
            "set-option" => self.cmd_set_option(&rest),
            "unset-option" => self.cmd_unset_option(&rest),
            "instance-type" => self.cmd_instance_type(&rest),
            "custom-platform-delete" => self.cmd_custom_platform_delete(&rest),
            "env" => self.cmd_env(&rest),
            "metric" => self.cmd_metric(&rest),
            "logs-tail" => {
                // `:logs-tail [LOG_GROUP]` — stream a CW Logs group for the
                // selected env. If no group given, discover groups for the
                // env and pick the most useful one (web.stdout.log if
                // present, else the first by name). The polling task is
                // tracked on App.log_tail_task so subsequent calls / close
                // can abort cleanly.
                let Some(env) = self.selected_env().cloned() else {
                    self.error_message = Some(
                        "no env selected — press 1-9, click a row, or type ' to jump by name"
                            .into(),
                    );
                    return;
                };
                let explicit_group = rest.first().map(|s| s.to_string());
                self.spawn_logs_tail(env.name.clone(), explicit_group);
            }
            "event-tail" | "tail-events" => {
                // `:event-tail` — cross-fleet EB event stream. No env
                // selection needed; the tail covers every env in the
                // current context. The polling task is tracked on
                // App.event_tail_task so re-issue / close abort cleanly.
                self.status_message = Some("event tail: fetching fleet events…".into());
                self.spawn_event_tail();
            }
            "logs-stream" => self.cmd_logs_stream(&rest),
            "notify" => self.cmd_notify(&rest),
            "managed-window" => self.cmd_managed_window(&rest),
            "alarm-create" => self.cmd_alarm_create(&rest),
            "alarm-delete" => self.cmd_alarm_delete(&rest),
            "alarm-history" => self.cmd_alarm_history(&rest),
            "config-inspect" => self.cmd_config_inspect(&rest),
            "deselect" | "select-clear" => {
                let n = self.multi_selected.len();
                self.multi_selected.clear();
                self.status_message = Some(format!("cleared {n} env selection(s)"));
            }
            other => {
                if let Some(plugin) = self.plugins.get(other).cloned() {
                    self.run_plugin_command(other, &plugin);
                    return;
                }
                // Did-you-mean: surface the closest registry name
                // within edit-distance 2. Catches everyday typos
                // like `:restrt` → `:restart`. Skips the suggestion
                // entirely when nothing's close enough — a wild
                // guess would mislead rather than help.
                let suggestion = suggest_command(other);
                let msg = match suggestion {
                    Some(name) => {
                        format!("unknown command: :{other} — did you mean :{name}? (try :help)")
                    }
                    None => format!("unknown command: :{other}  (try :help)"),
                };
                self.error_message = Some(msg);
            }
        }
    }

    fn run_plugin_command(&mut self, name: &str, plugin: &crate::plugins::Plugin) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(env) = env_opt else {
            self.error_message = Some(format!(":{name} — no env selected"));
            return;
        };
        let rendered = crate::plugins::render(
            &plugin.template,
            &env.name,
            &env.cname,
            &env.application,
            &env.tier,
            &self.context.region,
            self.override_profile
                .as_deref()
                .or(self.context.profile.as_deref()),
        );
        match yank(&rendered) {
            Ok(()) => {
                self.status_message = Some(format!(
                    "plugin :{name} → clipboard ({} chars)",
                    rendered.chars().count()
                ));
            }
            Err(e) => self.error_message = Some(format!("clipboard error: {e}")),
        }
    }
}
