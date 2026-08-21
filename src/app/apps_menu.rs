//! The Applications scope: its action menu and the per-application
//! info overlay.

use super::*;

impl App {
    /// `:apps-info` — surface application metadata that doesn't fit
    /// in the apps-table columns: full description, creation date,
    /// last-updated date, saved-config templates, env count.
    /// Resolves the target via cursor position in either scope:
    /// Apps scope uses `app_table_state`; Envs scope walks
    /// `selected_env().application`.
    pub(crate) fn open_apps_info_overlay(&mut self) {
        let app_name_opt = match self.scope {
            Scope::Apps => self
                .app_table_state
                .selected()
                .and_then(|i| self.applications.get(i).map(|a| a.name.clone())),
            Scope::Envs => self.selected_env().map(|e| e.application.clone()),
        };
        let Some(app_name) = app_name_opt else {
            self.error_message = Some("no application selected".into());
            return;
        };
        let Some(app) = self.applications.iter().find(|a| a.name == app_name) else {
            self.error_message = Some(format!(
                "application '{app_name}' not in cache yet — refresh and retry"
            ));
            return;
        };
        // Walk env list for the rollup figures; mirrors the apps-table
        // columns so the operator can compare without bouncing.
        let rollup = app_rollup(&self.environments, &app.name, &self.worker_dlq_depths);
        let env_names: Vec<&str> = self
            .environments
            .iter()
            .filter(|e| e.application == app.name)
            .map(|e| e.name.as_str())
            .collect();
        let date_fmt = |dt: Option<chrono::DateTime<chrono::Utc>>| -> String {
            dt.map(|t| t.format("%Y-%m-%d %H:%M UTC").to_string())
                .unwrap_or_else(|| "—".into())
        };
        let templates_block = if app.templates.is_empty() {
            "  (none)".to_string()
        } else {
            app.templates
                .iter()
                .map(|t| format!("  ▸ {t}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let envs_block = if env_names.is_empty() {
            "  (none)".to_string()
        } else {
            env_names
                .iter()
                .map(|n| format!("  ▸ {n}"))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let description = if app.description.is_empty() {
            "(no description)".to_string()
        } else {
            app.description.clone()
        };
        let latest_line = match (
            app.latest_version_label.as_deref(),
            app.latest_version_created,
        ) {
            (Some(label), Some(created)) => format!("{label}  ({})", date_fmt(Some(created))),
            (Some(label), None) => label.to_string(),
            _ => "—".into(),
        };
        let body = format!(
            "Application: {}\n\
             Description: {description}\n\n\
             Created:     {created}\n\
             Updated:     {updated}\n\n\
             Versions:    {version_count} registered · latest: {latest_line}\n\
             Envs:        {env_count} total · {red_count} alerting · {updating_count} updating\n\n\
             Environments:\n{envs_block}\n\n\
             Saved configuration templates:\n{templates_block}\n\n\
             esc / q to close",
            app.name,
            created = date_fmt(app.date_created),
            updated = date_fmt(app.date_updated),
            version_count = app.version_count,
            env_count = rollup.env_count,
            red_count = rollup.red_count + rollup.worker_dlq_alerts,
            updating_count = rollup.updating_count,
        );
        self.current_overlay = Some(Overlay::TextDump {
            title: format!("info — {}", app.name),
            body,
        });
    }

    /// Open the apps-scope action overlay for the selected application.
    /// Captures the env list at open time so later refreshes (e.g. an
    /// env terminating mid-action) can't shift which envs the operator
    /// thought they were targeting. Closes silently when no app is
    /// selected or the application has no envs.
    pub(crate) fn open_apps_action_menu(&mut self) {
        let Some(idx) = self.app_table_state.selected() else {
            return;
        };
        let Some(app_name) = self.applications.get(idx).map(|a| a.name.clone()) else {
            return;
        };
        let env_names: Vec<String> = self
            .environments
            .iter()
            .filter(|e| e.application == app_name)
            .map(|e| e.name.clone())
            .collect();
        if env_names.is_empty() {
            self.status_message = Some(format!(
                "application '{app_name}' has no envs — nothing to act on"
            ));
            return;
        }
        self.current_overlay = Some(Overlay::AppsActionMenu {
            app_name,
            env_names,
            cursor: 0,
        });
    }

    /// Key handler for the apps-scope action overlay. j/k cycles the
    /// cursor; Enter dispatches the selected item; esc / q closes.
    /// Five items, dispatched via the matching `cmd_batch_*` helpers
    /// after seeding `multi_selected` with the captured env list.
    pub(crate) fn handle_apps_action_menu_key(&mut self, key: KeyEvent) {
        let n_items = APPS_ACTION_ITEMS.len() as i32;
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                self.current_overlay = None;
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(Overlay::AppsActionMenu { cursor, .. }) = self.current_overlay.as_mut()
                {
                    let cur = *cursor as i32;
                    *cursor = (cur + 1).rem_euclid(n_items) as usize;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(Overlay::AppsActionMenu { cursor, .. }) = self.current_overlay.as_mut()
                {
                    let cur = *cursor as i32;
                    *cursor = (cur - 1).rem_euclid(n_items) as usize;
                }
            }
            KeyCode::Enter => self.dispatch_apps_action_menu(),
            _ => {}
        }
    }

    pub(crate) fn dispatch_apps_action_menu(&mut self) {
        let Some(Overlay::AppsActionMenu {
            app_name,
            env_names,
            cursor,
        }) = self.current_overlay.as_ref().cloned()
        else {
            return;
        };
        // Close the overlay before dispatching so the resulting toast /
        // confirm modal renders on the bare apps table, not on top of
        // the menu.
        self.current_overlay = None;
        let item = match APPS_ACTION_ITEMS.get(cursor) {
            Some(it) => *it,
            None => return,
        };
        match item {
            AppsActionItem::Drill => {
                self.filter = app_name.clone().into();
                self.set_scope(Scope::Envs);
                self.rebuild_view();
                self.status_message = Some(format!("filtered envs to application '{app_name}'"));
            }
            AppsActionItem::BatchRebuild => {
                self.multi_selected = env_names.into_iter().collect();
                self.cmd_batch_action(Action::Rebuild);
            }
            AppsActionItem::BatchRestart => {
                self.multi_selected = env_names.into_iter().collect();
                self.cmd_batch_action(Action::RestartAppServer);
            }
            AppsActionItem::BatchDeploy => {
                // Seed the multi-select then drop into command mode
                // with `:batch-deploy ` so the operator types the
                // version label and Enter dispatches.
                self.multi_selected = env_names.into_iter().collect();
                self.mode = Mode::Command;
                self.command_input = "batch-deploy ".into();
                self.status_message = Some("type a version label and press enter".into());
            }
            AppsActionItem::OpenInConsole => {
                self.open_app_in_console();
            }
        }
    }
}
