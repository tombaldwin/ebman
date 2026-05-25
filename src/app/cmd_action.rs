//! Lifecycle action commands — `:deploy`, `:upgrade`, `:clone`,
//! `:scale`, `:stop`, `:start`, `:abort`, `:rebuild`, `:restart`,
//! `:terminate`, `:swap`. Each opens the confirm modal (or skips it
//! when EB has no destructive impact) via `open_parameterised_action`
//! and routes through the existing audit + pending + toast plumbing.
//!
//! Fifth slice of the `execute_command` split. Same parent-module
//! visibility as the other `cmd_*` sub-modules.

use super::{parse_named_arg, parse_s3_url, Action, App, ParameterisedAction};

impl App {
    /// `:deploy LABEL [--preview]` ships an existing version, or
    /// `:deploy --from PATH | s3://BUCKET/KEY [--label L] [--describe D] [--no-deploy]`
    /// uploads a new bundle + creates a version + (by default)
    /// immediately deploys it. The two forms are disjoint — the first
    /// arg discriminates.
    pub(crate) fn cmd_deploy(&mut self, rest: &[&str]) {
        if rest.first().copied() == Some("--from") {
            let path = match rest.get(1).copied() {
                Some(p) => p.to_string(),
                None => {
                    self.error_message = Some(
                        "usage: :deploy --from PATH | s3://BUCKET/KEY [--label LABEL] [--describe DESC] [--no-deploy]".into(),
                    );
                    return;
                }
            };
            let label = parse_named_arg::<String>(rest, "--label");
            let description = parse_named_arg::<String>(rest, "--describe");
            let no_deploy = rest.contains(&"--no-deploy");
            if let Some((bucket, key)) = parse_s3_url(&path) {
                self.spawn_deploy_from_s3(bucket, key, label, description, !no_deploy);
            } else {
                self.spawn_deploy_from_local(path, label, description, !no_deploy);
            }
            return;
        }
        match rest.first().copied() {
            None => {
                self.error_message = Some(
                    "usage: :deploy LABEL [--preview] [--auto-rollback Nm]  (existing version) | :deploy --from PATH [--label L] [--describe D] [--no-deploy]".into(),
                );
            }
            Some(version) => {
                let preview = rest.contains(&"--preview");
                if preview {
                    let Some(env) = self.selected_env().cloned() else {
                        self.error_message = Some("no env selected".into());
                        return;
                    };
                    self.spawn_deploy_preview(env, version.to_string());
                    return;
                }
                // Optional `--auto-rollback Nm` — at deadline, the
                // watchdog checks env health and (if still non-Green)
                // redeploys the captured pre-deploy snapshot. Same
                // duration grammar as `parse_window_ms` / DLQ replay
                // so operators don't relearn it.
                let auto_rollback_secs = parse_named_arg::<String>(rest, "--auto-rollback")
                    .and_then(|s| {
                        let ms = crate::aws::parse_window_ms(&s)?;
                        Some((ms / 1000) as u64)
                    });
                if rest.contains(&"--auto-rollback") && auto_rollback_secs.is_none() {
                    self.error_message =
                        Some("--auto-rollback expects a duration like `5m` / `30m` / `1h`".into());
                    return;
                }
                self.open_parameterised_action(
                    Action::Deploy,
                    ParameterisedAction {
                        deploy_version: Some(version.to_string()),
                        auto_rollback_secs,
                        ..Default::default()
                    },
                );
            }
        }
    }

    /// `:upgrade [PLATFORM_ARN]` — no-arg form lists compatible
    /// platforms in an overlay; arg form opens the confirm modal.
    pub(crate) fn cmd_upgrade(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            None => {
                let Some(env) = self.selected_env().cloned() else {
                    self.error_message = Some("no env selected".into());
                    return;
                };
                self.spawn_list_compatible_platforms(env.name);
            }
            Some(arn) => {
                self.open_parameterised_action(
                    Action::UpgradePlatform,
                    ParameterisedAction {
                        upgrade_platform_arn: Some(arn.to_string()),
                        upgrade_platform_label: Some(arn.to_string()),
                        ..Default::default()
                    },
                );
            }
        }
    }

    pub(crate) fn cmd_clone(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            None => {
                self.error_message =
                    Some("usage: :clone <new-env-name>  (clones the selected env)".into());
            }
            Some(target) => {
                self.open_parameterised_action(
                    Action::Clone,
                    ParameterisedAction {
                        clone_target: Some(target.to_string()),
                        ..Default::default()
                    },
                );
            }
        }
    }

    pub(crate) fn cmd_scale(&mut self, rest: &[&str]) {
        match rest.first().copied().and_then(|s| s.parse::<i32>().ok()) {
            Some(n) if n >= 0 => self.open_parameterised_action(
                Action::Scale,
                ParameterisedAction {
                    scale_min: Some(n),
                    scale_max: Some(n),
                    ..Default::default()
                },
            ),
            _ => {
                self.error_message =
                    Some("usage: :scale N  (sets min=max=N; use :stop for 0)".into());
            }
        }
    }

    pub(crate) fn cmd_stop(&mut self) {
        self.open_parameterised_action(
            Action::Scale,
            ParameterisedAction {
                scale_min: Some(0),
                scale_max: Some(0),
                ..Default::default()
            },
        );
    }

    pub(crate) fn cmd_start(&mut self) {
        self.open_parameterised_action(
            Action::Scale,
            ParameterisedAction {
                scale_min: Some(1),
                scale_max: Some(1),
                ..Default::default()
            },
        );
    }

    pub(crate) fn cmd_abort(&mut self) {
        self.open_parameterised_action(Action::AbortUpdate, ParameterisedAction::default());
    }

    pub(crate) fn cmd_rebuild(&mut self) {
        self.open_parameterised_action(Action::Rebuild, ParameterisedAction::default());
    }

    pub(crate) fn cmd_restart(&mut self) {
        self.open_parameterised_action(Action::RestartAppServer, ParameterisedAction::default());
    }

    /// Terminate keeps its strict-typed-name guard via the action menu
    /// rather than the Y/N confirm `open_parameterised_action` uses.
    pub(crate) fn cmd_terminate(&mut self) {
        self.open_action_menu();
        self.advance_action_flow(Action::Terminate);
    }

    /// `:swap TARGET` — CNAME swap with another env in the same
    /// application. Pre-validates the target before opening the confirm
    /// modal so we fail fast on typos, then routes through
    /// `open_parameterised_action` so the preflight (impact preview +
    /// last-3 events) and read-only guard land the same way they would
    /// from the action menu.
    pub(crate) fn cmd_swap(&mut self, rest: &[&str]) {
        let Some(target) = rest.first().copied() else {
            self.error_message =
                Some("usage: :swap <target-env>  (must be in same application)".into());
            return;
        };
        let Some(env) = self.selected_env().cloned() else {
            self.error_message = Some("no env selected".into());
            return;
        };
        let target = target.to_string();
        let target_exists = self
            .environments
            .iter()
            .any(|e| e.name == target && e.application == env.application);
        if !target_exists {
            self.error_message = Some(format!(
                "swap target '{target}' not found in app '{}'",
                env.application
            ));
            return;
        }
        self.open_parameterised_action(
            Action::SwapCnames,
            ParameterisedAction {
                swap_with: Some(target),
                ..Default::default()
            },
        );
    }
}
