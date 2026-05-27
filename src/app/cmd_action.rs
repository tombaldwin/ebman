//! Lifecycle action commands — `:deploy`, `:upgrade`, `:clone`,
//! `:scale`, `:stop`, `:start`, `:abort`, `:rebuild`, `:restart`,
//! `:terminate`, `:swap`. Each opens the confirm modal (or skips it
//! when EB has no destructive impact) via `open_parameterised_action`
//! and routes through the existing audit + pending + toast plumbing.
//!
//! Fifth slice of the `execute_command` split. Same parent-module
//! visibility as the other `cmd_*` sub-modules.

use super::{parse_named_arg, parse_s3_url, Action, App, Mode, ParameterisedAction};

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
                    "usage: :deploy LABEL [--preview] [--auto-rollback Nm] [--wait-for-green Nm]  (existing version) | :deploy --from PATH [--label L] [--describe D] [--no-deploy]".into(),
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
                // Optional `--wait-for-green Nm` — `apply_refresh` arms
                // a tracker at dispatch and pins success / timeout when
                // the env reaches Green or the deadline passes. Pure
                // observability — doesn't change the deploy itself.
                // Orthogonal to `--auto-rollback`; both flags can be
                // set on the same deploy.
                let wait_for_green_secs = parse_named_arg::<String>(rest, "--wait-for-green")
                    .and_then(|s| {
                        let ms = crate::aws::parse_window_ms(&s)?;
                        Some((ms / 1000) as u64)
                    });
                if rest.contains(&"--wait-for-green") && wait_for_green_secs.is_none() {
                    self.error_message =
                        Some("--wait-for-green expects a duration like `5m` / `30m` / `1h`".into());
                    return;
                }
                self.open_parameterised_action(
                    Action::Deploy,
                    ParameterisedAction {
                        deploy_version: Some(version.to_string()),
                        auto_rollback_secs,
                        wait_for_green_secs,
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

    /// `:promote-env SOURCE TARGET [--auto-rollback Nm] [--wait-for-green Nm]`
    /// — one-command "ship SOURCE's currently-deployed version to
    /// TARGET". The version-label promotion is the common case
    /// (staging is green, ship same build to prod); config-options
    /// promotion is a deeper follow-on with its own design surface
    /// (which settings to copy? operator-set only? defaults?) and
    /// is tracked separately. Composes with the same watchdog flags
    /// as `:deploy` so a daily "promote with safety net" gesture is
    /// one dispatch.
    ///
    /// Routes through `open_parameterised_action_on` with TARGET as
    /// the destination env, not the currently-selected one — so the
    /// operator can promote without first hunting for TARGET in the
    /// table.
    pub(crate) fn cmd_promote_env(&mut self, rest: &[&str]) {
        let usage = "usage: :promote-env SOURCE TARGET [--auto-rollback Nm] [--wait-for-green Nm]";
        let Some(source_name) = rest.first().copied() else {
            self.error_message = Some(usage.into());
            return;
        };
        let Some(target_name) = rest.get(1).copied() else {
            self.error_message = Some(usage.into());
            return;
        };
        if source_name == target_name {
            self.error_message = Some("source and target must be different envs".into());
            return;
        }
        let Some(source) = self
            .environments
            .iter()
            .find(|e| e.name == source_name)
            .cloned()
        else {
            self.error_message = Some(format!("no env named '{source_name}' in the current view"));
            return;
        };
        let Some(target) = self
            .environments
            .iter()
            .find(|e| e.name == target_name)
            .cloned()
        else {
            self.error_message = Some(format!("no env named '{target_name}' in the current view"));
            return;
        };
        if source.version_label.is_empty() {
            self.error_message = Some(format!(
                "source env '{source_name}' has no version deployed — nothing to promote"
            ));
            return;
        }
        if source.version_label == target.version_label {
            self.error_message = Some(format!(
                "'{}' is already deployed to {target_name} — nothing to promote",
                source.version_label
            ));
            return;
        }
        // Same flag parsing as `:deploy` so the duration grammar is
        // identical across the deploy story.
        let auto_rollback_secs = parse_named_arg::<String>(rest, "--auto-rollback").and_then(|s| {
            let ms = crate::aws::parse_window_ms(&s)?;
            Some((ms / 1000) as u64)
        });
        if rest.contains(&"--auto-rollback") && auto_rollback_secs.is_none() {
            self.error_message =
                Some("--auto-rollback expects a duration like `5m` / `30m` / `1h`".into());
            return;
        }
        let wait_for_green_secs =
            parse_named_arg::<String>(rest, "--wait-for-green").and_then(|s| {
                let ms = crate::aws::parse_window_ms(&s)?;
                Some((ms / 1000) as u64)
            });
        if rest.contains(&"--wait-for-green") && wait_for_green_secs.is_none() {
            self.error_message =
                Some("--wait-for-green expects a duration like `5m` / `30m` / `1h`".into());
            return;
        }
        let label = source.version_label.clone();
        self.open_parameterised_action_on(
            target,
            Action::Deploy,
            ParameterisedAction {
                deploy_version: Some(label.clone()),
                auto_rollback_secs,
                wait_for_green_secs,
                ..Default::default()
            },
        );
        self.status_message = Some(format!(
            "promote: {label} from {source_name} → {target_name}"
        ));
    }

    /// `:rollout LABEL --regions r1,r2,r3 [--wait-for-green Nm]`
    /// — cross-region sequential deploy. Same env-name across
    /// regions (defaults to the selected env's name; an explicit
    /// `--env NAME` overrides). Opens a Rollout flow overlay:
    /// pre-flights all regions, then awaits `y` to dispatch.
    ///
    /// Sequential dispatch only — stops on first failure. Matches
    /// the `ebman action rollout` CLI's semantics (single
    /// `rollout_id` for audit correlation, pre-flight halt if any
    /// region misses the env, etc.). The CLI is the more common
    /// path for CI; this TUI surface is for interactive
    /// "review-and-go" rollouts.
    pub(crate) fn cmd_rollout(&mut self, rest: &[&str]) {
        let usage = "usage: :rollout LABEL --regions r1,r2,r3 [--env NAME] [--wait-for-green Nm]";
        let Some(label) = rest.first().copied() else {
            self.error_message = Some(usage.into());
            return;
        };
        let regions_csv = match parse_named_arg::<String>(rest, "--regions") {
            Some(v) => v,
            None => {
                self.error_message = Some(
                    "rollout: --regions r1,r2,r3 is required (comma-separated, no spaces)".into(),
                );
                return;
            }
        };
        let regions: Vec<String> = regions_csv
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if regions.is_empty() {
            self.error_message = Some("rollout: --regions list is empty".into());
            return;
        }
        // Env: explicit --env NAME wins; else fall back to the
        // selected env's name. The CLI requires --env explicitly;
        // the TUI is more forgiving because the operator usually
        // has the target env selected when they fire :rollout.
        let env_name = match parse_named_arg::<String>(rest, "--env") {
            Some(name) => name,
            None => match self.selected_env() {
                Some(env) => env.name.clone(),
                None => {
                    self.error_message = Some(
                        "rollout: no env selected — pass --env NAME or select an env first".into(),
                    );
                    return;
                }
            },
        };
        let wait_for_green_secs =
            parse_named_arg::<String>(rest, "--wait-for-green").and_then(|s| {
                let ms = crate::aws::parse_window_ms(&s)?;
                Some((ms / 1000) as u64)
            });
        if rest.contains(&"--wait-for-green") && wait_for_green_secs.is_none() {
            self.error_message = Some(
                "rollout: --wait-for-green expects a duration like `5m` / `30m` / `1h`".into(),
            );
            return;
        }
        if self.demo_mode {
            // Demo synthetic fleet has fake CNAMEs that wouldn't
            // resolve; the per-region AwsClient construction
            // would actually try STS. Refuse with a clear hint.
            self.error_message = Some("rollout: not available in --demo mode".into());
            return;
        }

        // Build the initial RolloutFlow in Planning state with
        // one row per region (all placeholders) so the overlay
        // can render immediately. Pre-flight messages populate
        // each row as they land.
        let rollout_id = format!("rollout-{}", chrono::Utc::now().format("%Y%m%dT%H%M%SZ"));
        let flow = crate::mode_action::RolloutFlow {
            rollout_id: rollout_id.clone(),
            env_name: env_name.clone(),
            version_label: label.to_string(),
            wait_for_green_secs,
            regions: regions
                .iter()
                .map(|r| crate::mode_action::RolloutRegion {
                    region: r.clone(),
                    current_version: None,
                    env_found: None,
                    preflight_error: None,
                    outcome: None,
                })
                .collect(),
            state: crate::mode_action::RolloutState::Planning,
        };
        self.action_flow = Some(crate::mode_action::ActionFlow::Rollout(flow));
        self.mode = Mode::Action;
        self.status_message = Some(format!(
            "rollout {rollout_id}: pre-flighting {} region(s)…",
            regions.len()
        ));
        // Fan out the pre-flight: one spawn per region. Each
        // hits STS (via AwsClient::with) then list_environments,
        // emits AppMsg::RolloutPreflight back to App.
        let profile = self.context.profile.clone();
        for region in regions {
            self.spawn_rollout_preflight(profile.clone(), region, env_name.clone());
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
