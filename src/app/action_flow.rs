//! The action menu and its confirm/dispatch state machine.
//!
//! `open_action_menu` -> `handle_action_key` -> `advance_action_flow`
//! walks an operator from picking an action to confirming it; the
//! `*_pending` / `*_dispatch` half then holds it in the undo window
//! before `spawn_action` actually fires.

use super::*;

impl App {
    /// Dispatch an auto-rollback redeploy for `env_name`. Single
    /// source of truth for the rollback dispatch — `apply_refresh`
    /// calls this when an armed watchdog's deadline has passed and
    /// the freshly-applied env is still non-Green. Earlier shape
    /// had this inline in `handle_auto_rollback_check`, which read
    /// possibly-stale cached health; making `apply_refresh` the
    /// decision point eliminates that race.
    ///
    /// Caller contracts: env is in the cached fleet, env is non-
    /// Green, watchdog slot exists. The "no snapshot" + read-only
    /// gating paths are handled here (drain the watchdog + surface
    /// an error / status) so caller logic stays simple.
    pub(crate) fn dispatch_auto_rollback(&mut self, env_name: String, health: String) {
        // Always drain a parallel wait-for-green watcher when the
        // rollback fires — otherwise the subsequent Green from the
        // rolled-back version would pin "✓ deploy reached Green:
        // ENV (build-900)" even though build-900 is the version we
        // just rolled away from. The auto-rollback's own pin is
        // the signal the operator should see.
        self.watching_deploys.remove(&env_name);
        let Some(snapshot) = self.deploy_snapshots.get(&env_name).cloned() else {
            // pin_error so the warning survives apply_refresh's auto-
            // clear — when this fires *from* apply_refresh (the
            // common case), the unpinned error_message would
            // otherwise be wiped on the same tick.
            self.pin_error(format!(
                "auto-rollback for {env_name}: no pre-deploy snapshot; manual rollback required"
            ));
            self.armed_watchdogs.remove(&env_name);
            return;
        };
        if self.deny_write(&env_name, "auto-rollback") {
            self.armed_watchdogs.remove(&env_name);
            return;
        }
        self.armed_watchdogs.remove(&env_name);
        let label = snapshot.previous_version_label.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        crate::audit::append_action_dispatched(
            account.as_deref(),
            profile.as_deref(),
            &region,
            "AutoRollback",
            env_name.as_str(),
            &[("version", label.as_str()), ("health", health.as_str())],
        );
        self.push_pending("Auto-rollback", env_name.clone());
        self.pin_status(format!(
            "auto-rollback for {env_name}: redeploying {label} (env was {health})"
        ));
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .deploy_version(&env_name, &label)
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

    fn target_env_for_action(&self) -> Option<Environment> {
        // Detail view targets the env it was opened on; Normal view targets selection.
        if let Some(d) = self.detail.as_ref() {
            return Some(d.env_snapshot.clone());
        }
        self.selected_env().cloned()
    }

    pub(crate) fn open_action_menu(&mut self) {
        let Some(target) = self.target_env_for_action() else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&target.name, "action menu") {
            return;
        }
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        self.action_flow = Some(ActionFlow::Menu { list_state });
        self.mode = Mode::Action;
    }

    pub(crate) fn close_action_flow(&mut self) {
        self.action_flow = None;
        if self.detail.is_some() {
            self.mode = Mode::Detail;
        } else {
            self.mode = Mode::Normal;
        }
    }

    pub(crate) fn handle_action_key(&mut self, key: KeyEvent) {
        let Some(flow) = self.action_flow.as_mut() else {
            self.mode = Mode::Normal;
            return;
        };
        match flow {
            ActionFlow::Menu { list_state } => match key.code {
                // Menu has j/k cursor + Enter to pick — no text input, so
                // `q` as close is unambiguous and matches every other
                // overlay's pattern.
                KeyCode::Esc | KeyCode::Char('q') => self.close_action_flow(),
                KeyCode::Char('j') | KeyCode::Down => {
                    let cur = list_state.selected().unwrap_or(0);
                    let next = (cur + 1) % ACTIONS.len();
                    list_state.select(Some(next));
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    let cur = list_state.selected().unwrap_or(0);
                    let next = (cur + ACTIONS.len() - 1) % ACTIONS.len();
                    list_state.select(Some(next));
                }
                KeyCode::Enter => {
                    let Some(idx) = list_state.selected() else {
                        return;
                    };
                    let action = ACTIONS[idx];
                    self.advance_action_flow(action);
                }
                _ => {}
            },
            ActionFlow::SwapTarget { picker, .. } => match key.code {
                KeyCode::Esc => self.close_action_flow(),
                KeyCode::Down | KeyCode::Char('j')
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    picker.move_selection(1);
                }
                KeyCode::Up | KeyCode::Char('k')
                    if !key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    picker.move_selection(-1);
                }
                KeyCode::Enter => {
                    let Some(target) = picker.selected_value() else {
                        return;
                    };
                    let source = match flow {
                        ActionFlow::SwapTarget { source, .. } => source.clone(),
                        _ => return,
                    };
                    let warning = self
                        .environments
                        .iter()
                        .find(|e| e.name == source)
                        .map(compute_traffic_warning)
                        .unwrap_or(None);
                    self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                        action: Action::SwapCnames,
                        target_env: source,
                        swap_with: Some(target),
                        typed: TextInput::new(),
                        kind: ConfirmKind::YesNo,
                        dryrun: None,
                        loading_dryrun: false,
                        recent_events: None,
                        loading_events: false,
                        traffic_warning: warning,
                        deploy_version: None,
                        upgrade_platform_arn: None,
                        upgrade_platform_label: None,
                        clone_target: None,
                        scale_min: None,
                        scale_max: None,
                        auto_rollback_secs: None,
                        wait_for_green_secs: None,
                        version_preview: None,
                        loading_version_preview: false,
                        health_check_probe: None,
                        loading_health_check: false,
                        unavailability_line: None,
                        loading_unavailability: false,
                        lint_issues: None,
                        loading_lint: false,
                        ssm_run_command: None,
                        ssm_run_instances: None,
                    }));
                }
                // TextInput consumes editing keys (incl. Backspace);
                // reselect a still-matching row after any accepted edit.
                _ => {
                    if picker.filter.handle_key(key) {
                        let filt = picker.filtered();
                        if !filt
                            .iter()
                            .any(|i| Some(*i) == picker.list_state.selected())
                        {
                            picker.list_state.select(filt.first().copied());
                        }
                    }
                }
            },
            ActionFlow::Confirm(modal) => match (key.code, modal.kind) {
                (KeyCode::Esc, _) => self.close_action_flow(),
                // `q` cancels Y/N confirms (n / esc are the others). TypeName
                // confirms intentionally don't bind q since the user is
                // typing the env name and `q` might be part of it.
                (KeyCode::Char('q'), ConfirmKind::YesNo) => self.close_action_flow(),
                (KeyCode::Char('y'), ConfirmKind::YesNo) | (KeyCode::Enter, ConfirmKind::YesNo) => {
                    // Queue with a 5s cancel window instead of dispatching
                    // immediately. The action flow closes (modal gone)
                    // and a countdown lands in `status_message`; `U` in
                    // Normal mode undoes it before the deadline.
                    let m = modal.clone();
                    self.close_action_flow();
                    self.queue_action_dispatch(m);
                }
                (KeyCode::Char('n'), ConfirmKind::YesNo) => self.close_action_flow(),
                (KeyCode::Enter, ConfirmKind::TypeName)
                    if modal.typed.text() == modal.target_env.as_str() =>
                {
                    // Same cancel-window treatment as Y-confirms. Terminate
                    // is the loudest example — the typed-name guard already
                    // prevents accidental dispatch, but the 5s window is a
                    // last-ditch "oh god no" rescue.
                    let m = modal.clone();
                    self.close_action_flow();
                    self.queue_action_dispatch(m);
                }
                // TextInput consumes editing keys for the type-to-confirm
                // field (cursor move / Ctrl-W included).
                (_, ConfirmKind::TypeName) => {
                    modal.typed.handle_key(key);
                }
                _ => {}
            },
            ActionFlow::Rollout(flow) => match (key.code, &flow.state) {
                // Esc / q close the rollout at any state — even
                // during Dispatching the operator can abort
                // further regions. The dispatched ones have
                // already fired (each region's UpdateEnvironment
                // is a non-reversible AWS write), but the loop
                // halts so regions queued behind the current
                // one don't fire.
                (KeyCode::Esc, _) | (KeyCode::Char('q'), _) => self.close_action_flow(),
                // `y` confirms a fully-pre-flighted plan. Only
                // valid in AwaitingConfirm. Switches state to
                // Dispatching and fires the first region.
                (KeyCode::Char('y'), crate::mode_action::RolloutState::AwaitingConfirm)
                | (KeyCode::Enter, crate::mode_action::RolloutState::AwaitingConfirm) => {
                    // Refuse to dispatch if no regions passed
                    // pre-flight — there's nothing safe to send.
                    let any_ok = flow.regions.iter().any(|r| r.env_found == Some(true));
                    if !any_ok {
                        self.error_message = Some(
                            "rollout: no regions passed pre-flight — fix or `esc` to abort".into(),
                        );
                        return;
                    }
                    // Find the first region that passed pre-
                    // flight. Failed regions are skipped (their
                    // outcome stays None — surfaced as
                    // "skipped" in the final report).
                    let Some((first_idx, _)) = flow
                        .regions
                        .iter()
                        .enumerate()
                        .find(|(_, r)| r.env_found == Some(true))
                    else {
                        return;
                    };
                    flow.state = crate::mode_action::RolloutState::Dispatching {
                        next_index: first_idx,
                    };
                    let region = flow.regions[first_idx].region.clone();
                    let env_name = flow.env_name.clone();
                    let version_label = flow.version_label.clone();
                    let wait_for_green_secs = flow.wait_for_green_secs;
                    let rollout_id = flow.rollout_id.clone();
                    let profile = self.context.profile.clone();
                    self.spawn_rollout_dispatch(
                        rollout_id,
                        profile,
                        region,
                        env_name,
                        version_label,
                        wait_for_green_secs,
                    );
                }
                // `n` aborts before any dispatch fires. Same as
                // esc but matches the y/n posture of the
                // standard ConfirmModal.
                (KeyCode::Char('n'), crate::mode_action::RolloutState::AwaitingConfirm) => {
                    self.close_action_flow();
                }
                _ => {}
            },
        }
    }

    pub(crate) fn advance_action_flow(&mut self, action: Action) {
        let Some(env) = self.target_env_for_action() else {
            self.close_action_flow();
            return;
        };
        match action {
            Action::SwapCnames => {
                // Build a list of envs in the same application (excluding the source).
                let candidates: Vec<String> = self
                    .environments
                    .iter()
                    .filter(|e| e.application == env.application && e.name != env.name)
                    .map(|e| e.name.clone())
                    .collect();
                if candidates.is_empty() {
                    self.action_flow = None;
                    self.mode = if self.detail.is_some() {
                        Mode::Detail
                    } else {
                        Mode::Normal
                    };
                    self.error_message = Some(format!(
                        "no swap candidates: app '{}' has only one env",
                        env.application
                    ));
                    return;
                }
                let picker = Picker::new(PickerKind::Region, candidates, None); // kind unused here
                self.action_flow = Some(ActionFlow::SwapTarget {
                    source: env.name.clone(),
                    picker,
                });
            }
            Action::Terminate => {
                // Terminate is the only Action that uses TypeName confirm;
                // every other entry routes through `open_parameterised_action`.
                // Preflight gating still flows from `Action::wants_preflight()`
                // so the rule lives in exactly one place.
                let wants_preflight = action.wants_preflight();
                self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                    action,
                    target_env: env.name.clone(),
                    swap_with: None,
                    typed: TextInput::new(),
                    kind: ConfirmKind::TypeName,
                    dryrun: None,
                    loading_dryrun: wants_preflight,
                    recent_events: None,
                    loading_events: wants_preflight,
                    traffic_warning: compute_traffic_warning(&env),
                    deploy_version: None,
                    upgrade_platform_arn: None,
                    upgrade_platform_label: None,
                    clone_target: None,
                    scale_min: None,
                    scale_max: None,
                    auto_rollback_secs: None,
                    wait_for_green_secs: None,
                    version_preview: None,
                    loading_version_preview: false,
                    health_check_probe: None,
                    loading_health_check: false,
                    unavailability_line: None,
                    loading_unavailability: false,
                    lint_issues: None,
                    loading_lint: false,
                    ssm_run_command: None,
                    ssm_run_instances: None,
                }));
                if wants_preflight {
                    self.spawn_dry_run(env.name.clone());
                    self.spawn_preflight_events(env.name.clone());
                }
            }
            Action::Rebuild => {
                self.open_parameterised_action(action, ParameterisedAction::default());
            }
            // Parameterised actions need user input before the confirm can
            // be built. The menu closes itself and pre-fills the command
            // bar so the user types `<arg>` and Enter, which routes through
            // the existing `:deploy` / `:upgrade` / `:clone` / `:scale`
            // handlers (all of which open a confirm modal).
            Action::Deploy => {
                self.close_action_flow();
                self.mode = Mode::Command;
                self.command_input = "deploy ".into();
                self.status_message = Some("type a version label and press enter".into());
            }
            Action::UpgradePlatform => {
                self.close_action_flow();
                self.spawn_list_compatible_platforms(env.name.clone());
                self.mode = Mode::Command;
                self.command_input = "upgrade ".into();
                self.status_message =
                    Some("listing platforms in overlay; paste an ARN and press enter".into());
            }
            Action::Clone => {
                self.close_action_flow();
                self.mode = Mode::Command;
                self.command_input = "clone ".into();
                self.status_message = Some("type a new env name and press enter".into());
            }
            Action::Scale => {
                self.close_action_flow();
                self.mode = Mode::Command;
                self.command_input = "scale ".into();
                self.status_message = Some(
                    "scale N (instances), or `scale min N` / `scale max N`; enter to apply".into(),
                );
            }
            Action::Capacity => {
                // `:capacity` opens a modal form pre-filled from
                // DescribeConfigurationSettings — no command-bar args
                // needed, so we close the menu and dispatch straight
                // to the form opener.
                self.close_action_flow();
                self.cmd_capacity();
            }
            Action::AbortUpdate => {
                self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                    action,
                    target_env: env.name.clone(),
                    swap_with: None,
                    typed: TextInput::new(),
                    kind: ConfirmKind::YesNo,
                    dryrun: None,
                    loading_dryrun: false,
                    recent_events: None,
                    loading_events: false,
                    traffic_warning: compute_traffic_warning(&env),
                    deploy_version: None,
                    upgrade_platform_arn: None,
                    upgrade_platform_label: None,
                    clone_target: None,
                    scale_min: None,
                    scale_max: None,
                    auto_rollback_secs: None,
                    wait_for_green_secs: None,
                    version_preview: None,
                    loading_version_preview: false,
                    health_check_probe: None,
                    loading_health_check: false,
                    unavailability_line: None,
                    loading_unavailability: false,
                    lint_issues: None,
                    loading_lint: false,
                    ssm_run_command: None,
                    ssm_run_instances: None,
                }));
            }
            _ => {
                self.action_flow = Some(ActionFlow::Confirm(ConfirmModal {
                    action,
                    target_env: env.name.clone(),
                    swap_with: None,
                    typed: TextInput::new(),
                    kind: ConfirmKind::YesNo,
                    dryrun: None,
                    loading_dryrun: false,
                    recent_events: None,
                    loading_events: false,
                    traffic_warning: compute_traffic_warning(&env),
                    deploy_version: None,
                    upgrade_platform_arn: None,
                    upgrade_platform_label: None,
                    clone_target: None,
                    scale_min: None,
                    scale_max: None,
                    auto_rollback_secs: None,
                    wait_for_green_secs: None,
                    version_preview: None,
                    loading_version_preview: false,
                    health_check_probe: None,
                    loading_health_check: false,
                    unavailability_line: None,
                    loading_unavailability: false,
                    lint_issues: None,
                    loading_lint: false,
                    ssm_run_command: None,
                    ssm_run_instances: None,
                }));
            }
        }
    }

    /// Add a row to the pending-actions panel before dispatching. Callers
    /// follow with a `tokio::spawn` that sends an `AppMsg::ActionResult`;
    /// the result handler finds the first matching unfinished row and
    /// stamps it with the outcome. Caps the list at `PENDING_CAP`.
    pub fn push_pending(&mut self, label: impl Into<String>, target: impl Into<String>) {
        if self.pending_actions.len() >= PENDING_CAP {
            self.pending_actions.pop_front();
        }
        self.pending_actions.push_back(PendingAction {
            label: label.into(),
            target: target.into(),
            started: Instant::now(),
            completed: None,
        });
    }

    /// Resolve a pending entry against an arriving `ActionResult`. Picks
    /// the oldest unfinished entry whose `(label, target)` match — the
    /// dispatch order is preserved so this is correct without IDs as long
    /// as we don't have two concurrent dispatches of the same action to the
    /// same target (a deliberate operator wouldn't do that).
    pub fn complete_pending(&mut self, label: &str, target: &str, result: Result<(), String>) {
        if let Some(entry) = self
            .pending_actions
            .iter_mut()
            .find(|e| e.completed.is_none() && e.label == label && e.target == target)
        {
            entry.completed = Some((Instant::now(), result));
        }
    }

    /// Drop completed entries older than `PENDING_COMPLETED_TTL`. Called
    /// from the run loop's per-frame housekeeping so the panel quietens
    /// after a minute of inactivity.
    pub fn expire_pending(&mut self) {
        let now = Instant::now();
        self.pending_actions.retain(|e| match e.completed {
            Some((c, _)) => now.duration_since(c) < PENDING_COMPLETED_TTL,
            None => true,
        });
    }

    pub(crate) fn open_parameterised_action(
        &mut self,
        action: Action,
        params: ParameterisedAction,
    ) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        self.open_parameterised_action_on(env, action, params);
    }

    /// Variant of `open_parameterised_action` that targets an
    /// explicit env rather than the currently-selected one. Used by
    /// commands like `:promote-env SOURCE TARGET` where the
    /// destination is named in the command itself, not implied by
    /// the table cursor. The `selected_env()`-based wrapper is the
    /// common path; this is the cursor-independent escape hatch.
    pub(crate) fn open_parameterised_action_on(
        &mut self,
        env: crate::aws::Environment,
        action: Action,
        params: ParameterisedAction,
    ) {
        if self.deny_write(&env.name, action.label()) {
            return;
        }
        // The preflight (impact preview + last-3 events) is gated by
        // `Action::wants_preflight()` — single source of truth, see
        // `mode_action.rs`. Every ConfirmModal construction site must
        // route through here so the rule can't drift.
        let wants_preflight = action.wants_preflight();
        // For Deploy with a candidate label, pull the version
        // metadata (label / age / description) and inline the
        // existing `:deploy --preview` body in the modal. Saves
        // the operator the separate `:deploy LABEL --preview`
        // round-trip.
        let wants_version_preview = action == Action::Deploy && params.deploy_version.is_some();
        let modal = ConfirmModal {
            action,
            target_env: env.name.clone(),
            swap_with: params.swap_with,
            typed: TextInput::new(),
            kind: ConfirmKind::YesNo,
            dryrun: None,
            loading_dryrun: wants_preflight,
            recent_events: None,
            loading_events: wants_preflight,
            // Skip traffic_warning for SsmRun — operators frequently
            // run diagnostic shells on Red envs (it's how they diagnose
            // Red), so showing "env is currently Red" in the modal
            // would suggest they shouldn't dispatch the very command
            // they're using to investigate. Other write actions get
            // the warning because Red+write is genuinely risky.
            // (0.17.4 review)
            traffic_warning: if action == Action::SsmRun {
                None
            } else {
                compute_traffic_warning(&env)
            },
            deploy_version: params.deploy_version.clone(),
            upgrade_platform_arn: params.upgrade_platform_arn,
            upgrade_platform_label: params.upgrade_platform_label,
            clone_target: params.clone_target,
            scale_min: params.scale_min,
            scale_max: params.scale_max,
            auto_rollback_secs: params.auto_rollback_secs,
            wait_for_green_secs: params.wait_for_green_secs,
            version_preview: None,
            loading_version_preview: wants_version_preview,
            health_check_probe: None,
            // Pre-deploy health-check probe only runs for Deploy
            // confirms — we want to warn the operator if the env's
            // current health-check-url is dead BEFORE they ship a
            // new build over it. For non-Deploy actions, the probe
            // is meaningless. Skipped in `--demo` mode because the
            // synthetic CNAMEs would always fail DNS and pollute
            // screencasts with a fake-warning red herring.
            loading_health_check: wants_version_preview && !env.cname.is_empty() && !self.demo_mode,
            unavailability_line: None,
            // Same gate as the health-check probe — only useful for
            // Deploy confirms; --demo mode skips the AWS call.
            loading_unavailability: wants_version_preview && !self.demo_mode,
            lint_issues: None,
            // Lint at confirm time runs against every confirm modal,
            // not just Deploy. The health-check probe + unavailability
            // pill specialise on deploys; lint is universal — operator
            // sees AllAtOnce / health-check-empty / cooldown-low etc.
            // before confirming any destructive action. Skipped in
            // demo mode (same gate as the other probes — no AWS
            // round-trip in demo).
            //
            // SsmRun also skips lint — running an ad-hoc shell command
            // isn't gated by EB-config-health rules; firing them here
            // would be noise.
            loading_lint: !self.demo_mode && action != Action::SsmRun,
            ssm_run_command: params.ssm_run_command,
            ssm_run_instances: params.ssm_run_instances,
        };
        let needs_health_check_probe = modal.loading_health_check;
        let needs_unavailability = modal.loading_unavailability;
        let needs_lint = modal.loading_lint;
        self.action_flow = Some(ActionFlow::Confirm(modal));
        self.mode = Mode::Action;
        if wants_preflight {
            self.spawn_dry_run(env.name.clone());
            self.spawn_preflight_events(env.name.clone());
        }
        if wants_version_preview {
            if let Some(label) = params.deploy_version {
                self.spawn_version_preview(
                    env.application.clone(),
                    env.name.clone(),
                    env.version_label.clone(),
                    label,
                );
            }
        }
        if needs_health_check_probe {
            self.spawn_health_check_probe(
                env.application.clone(),
                env.name.clone(),
                env.cname.clone(),
            );
        }
        if needs_unavailability {
            self.spawn_unavailability_estimate(env.application.clone(), env.name.clone());
        }
        if needs_lint {
            self.spawn_confirm_lint(env.clone());
        }
    }

    /// Fetch `list_compatible_platforms` for `env` and surface them in an
    /// overlay so the user can copy the desired ARN into `:upgrade <arn>`.
    pub(crate) fn spawn_list_compatible_platforms(&mut self, env_name: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!(
            "fetching compatible platform versions for {env_name}…"
        ));
        let env_for_msg = env_name.clone();
        tokio::spawn(async move {
            let result = aws
                .list_compatible_platforms(&env_name)
                .await
                .map_err(|e| flatten_err("list_compatible_platforms", e));
            let body = match result {
                Ok(p) if p.is_empty() => {
                    format!("No compatible platform versions found for {env_for_msg}.\n\nesc / q to close")
                }
                Ok(platforms) => {
                    let mut lines: Vec<String> = vec![
                        format!("Compatible platform versions for {env_for_msg}"),
                        "─────────────────────────────────────────────".into(),
                        String::new(),
                    ];
                    for p in platforms.iter().take(20) {
                        lines.push(format!(
                            "  v{}  {}  ({}, {})",
                            p.version, p.branch, p.status, p.lifecycle
                        ));
                        lines.push(format!("      {}", p.arn));
                    }
                    lines.push(String::new());
                    lines.push(
                        "Copy an ARN and run `:upgrade <ARN>` to migrate. esc / q to close".into(),
                    );
                    lines.join("\n")
                }
                Err(e) => format!("upgrade list failed: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("compatible platforms — {env_for_msg}"),
                body,
            });
        });
    }

    /// Queue a single-env action with the cancel window. Called from
    /// the Y / TypeName-confirm paths in `handle_action_key`.
    pub(crate) fn queue_action_dispatch(&mut self, modal: ConfirmModal) {
        // SsmRun bypasses the 5s cancel-window — operators using it for
        // diagnostic probes (`:ssm-run "uptime"`) expect immediate
        // dispatch; the 5s wait for the undo window was a 0.17.3
        // regression. The Y press itself is the gate; the modal already
        // surfaces command + fan-out count + env before dispatch.
        // Other destructive actions keep the cancel window for the
        // explicit "I just pressed Y, oh no" rescue path.
        if modal.action == Action::SsmRun {
            self.spawn_action(modal);
            return;
        }
        if self.pending_dispatch.is_some() {
            self.error_message = Some(
                "another action is mid-dispatch — wait for it to land or press U to undo".into(),
            );
            return;
        }
        let label = modal.action.label().to_string();
        let target = modal.target_env.clone();
        let deadline = Instant::now() + UNDO_WINDOW;
        self.pending_dispatch = Some(PendingDispatch {
            deadline,
            label: label.clone(),
            target: target.clone(),
            kind: PendingDispatchKind::Single { modal },
        });
        self.status_message = Some(format!(
            "{} → {} dispatches in {}s — press U to undo",
            label,
            target,
            UNDO_WINDOW.as_secs()
        ));
    }

    /// Queue a batch dispatch with the same cancel window. Caller
    /// resolves the kind + display labels (e.g. `"Batch rebuild"` /
    /// `"5 envs"`) before invoking. One-at-a-time rule shared with
    /// `queue_action_dispatch`.
    pub(crate) fn queue_batch_dispatch(
        &mut self,
        label: String,
        target: String,
        kind: PendingDispatchKind,
    ) {
        if self.pending_dispatch.is_some() {
            self.error_message = Some(
                "another dispatch is mid-window — wait for it to land or press U to undo".into(),
            );
            return;
        }
        let deadline = Instant::now() + UNDO_WINDOW;
        let status = format!(
            "{} → {} dispatches in {}s — press U to undo",
            label,
            target,
            UNDO_WINDOW.as_secs()
        );
        self.pending_dispatch = Some(PendingDispatch {
            deadline,
            label,
            target,
            kind,
        });
        self.status_message = Some(status);
    }

    /// Per-tick check called from the main loop. Fires whatever
    /// dispatch is queued when its cancel window expires. The
    /// per-variant dispatch re-uses the same helpers the immediate
    /// path used to call (`spawn_action`, `spawn_batch_*`), so audit
    /// log + pending pill + toast plumbing carry over unchanged.
    pub(crate) fn tick_pending_dispatch(&mut self) {
        let now = Instant::now();
        let Some(pd) = self.pending_dispatch.as_ref() else {
            return;
        };
        if now < pd.deadline {
            return;
        }
        let kind = pd.kind.clone();
        self.pending_dispatch = None;
        // `--demo` mode refuses ANY dispatch from the pending queue —
        // covers the batch variants that bypass `spawn_action`'s own
        // demo gate (Single dispatches still hit that guard too, so
        // both paths land on the same refusal toast). See spawn_action
        // for the rationale.
        if self.demo_mode {
            self.error_message = Some("demo mode — writes are inert; press q to exit".into());
            return;
        }
        match kind {
            PendingDispatchKind::Single { modal } => self.spawn_action(modal),
            PendingDispatchKind::BatchAction { action, env_names } => {
                for env in env_names {
                    self.spawn_batch_action(action, env);
                }
            }
            PendingDispatchKind::BatchDeploy {
                env_names,
                version_label,
            } => {
                for env in env_names {
                    self.spawn_batch_deploy(env, version_label.clone());
                }
            }
            PendingDispatchKind::BatchTag {
                envs_with_arns,
                key,
                value,
            } => {
                for (env, arn) in envs_with_arns {
                    self.spawn_batch_tag(env, arn, key.clone(), value.clone());
                }
            }
            PendingDispatchKind::BatchSetOption {
                env_names,
                namespace,
                option_name,
                value,
            } => {
                for env in env_names {
                    self.spawn_batch_set_option(
                        env,
                        namespace.clone(),
                        option_name.clone(),
                        value.clone(),
                    );
                }
            }
        }
    }

    /// Cancel the pending dispatch (bound to `U` in Normal mode).
    /// Audit-logs the cancel + emits a status toast. Silent abort
    /// would feel like a missed keypress.
    pub(crate) fn cancel_pending_dispatch(&mut self) {
        let Some(pd) = self.pending_dispatch.take() else {
            return;
        };
        let msg = format!("undone — {} → {} not dispatched", pd.label, pd.target);
        let action_for_audit = match &pd.kind {
            PendingDispatchKind::Single { modal } => format!("{:?}", modal.action),
            PendingDispatchKind::BatchAction { action, .. } => format!("Batch{action:?}"),
            PendingDispatchKind::BatchDeploy { .. } => "BatchDeploy".into(),
            PendingDispatchKind::BatchTag { value, .. } => {
                if value.is_some() {
                    "BatchTag".into()
                } else {
                    "BatchUntag".into()
                }
            }
            PendingDispatchKind::BatchSetOption { .. } => "BatchSetOption".into(),
        };
        crate::audit::append_action_undone(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            &action_for_audit,
            &pd.target,
        );
        self.status_message = Some(msg);
    }

    pub(crate) fn spawn_action(&mut self, modal: ConfirmModal) {
        // `--demo` mode runs against a synthetic fleet on a fake
        // AwsClient. Destructive dispatch against that client would
        // fail at the SDK layer but still write `stage=dispatched`
        // audit lines to the real audit log. Refuse outright so a
        // fat-fingered keypress during a demo / screencast doesn't
        // touch operator state.
        if self.demo_mode {
            self.error_message = Some(format!(
                "demo mode — {} not dispatched (writes are inert; press q to exit)",
                modal.action.label()
            ));
            return;
        }
        // Per-env / per-account read-only locks short-circuit the
        // dispatch before any AWS call. `read_only_reason` returns
        // the specific cause (global toggle vs. config-pinned env vs.
        // pinned account) so the toast tells the operator exactly
        // which knob is keeping them safe.
        if self.is_read_only_for(&modal.target_env) {
            let reason = self
                .read_only_reason(&modal.target_env)
                .unwrap_or_else(|| "read-only mode".into());
            self.error_message = Some(format!("{reason} — {} disabled", modal.action.label()));
            return;
        }
        // For Deploy actions: snapshot the env's pre-deploy version
        // label before dispatching, so :rollback-deploy and the
        // optional `--auto-rollback Nm` watchdog know what to roll
        // back TO. Skip if we don't have the env in our cached
        // fleet (e.g. assume-role race where the modal opened
        // before the refresh landed) — the existing :rollback can
        // scan events as a fallback.
        if modal.action == Action::Deploy {
            if let Some(env) = self
                .environments
                .iter()
                .find(|e| e.name == modal.target_env)
            {
                if !env.version_label.is_empty() {
                    self.deploy_snapshots.insert(
                        env.name.clone(),
                        DeploySnapshot {
                            env_name: env.name.clone(),
                            previous_version_label: env.version_label.clone(),
                            taken_at: chrono::Utc::now(),
                        },
                    );
                }
            }
            // Arm the auto-rollback watchdog if requested. Two signals
            // can fire: the env reaching Green on the next refresh
            // tick (early disarm via apply_refresh — most common
            // outcome) or the deadline timer firing AutoRollbackCheck.
            // `armed_watchdogs` carries the in-flight state for both
            // surfaces.
            if let Some(secs) = modal.auto_rollback_secs {
                let tx = self.msg_tx.clone();
                let env_name = modal.target_env.clone();
                let gen = self.generation;
                // Snapshot the rollback target now so the watchdog
                // doesn't have to re-look it up later. The pre-deploy
                // snapshot we just inserted is the source of truth.
                let target_label = self
                    .deploy_snapshots
                    .get(&modal.target_env)
                    .map(|s| s.previous_version_label.clone())
                    .unwrap_or_default();
                let armed_at = chrono::Utc::now();
                let deadline_at = armed_at + chrono::Duration::seconds(secs as i64);
                self.armed_watchdogs.insert(
                    modal.target_env.clone(),
                    ArmedWatchdog {
                        env_name: modal.target_env.clone(),
                        target_label,
                        armed_at,
                        deadline_at,
                    },
                );
                self.status_message = Some(format!(
                    "auto-rollback armed: {secs}s to reach Green or revert"
                ));
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_secs(secs)).await;
                    let _ = tx.send(AppMsg::AutoRollbackCheck { gen, env_name });
                });
            }
            // Arm the wait-for-green tracker if requested. Pure
            // observability — `apply_refresh` watches `watching_deploys`
            // and pins the outcome (success on Green, error on timeout).
            // No tokio task needed: apply_refresh runs on every refresh
            // tick anyway and checks deadlines there. Orthogonal to
            // auto-rollback so both flags can coexist.
            if let Some(secs) = modal.wait_for_green_secs {
                let target_label = modal.deploy_version.clone().unwrap_or_default();
                let armed_at = chrono::Utc::now();
                let deadline_at = armed_at + chrono::Duration::seconds(secs as i64);
                self.watching_deploys.insert(
                    modal.target_env.clone(),
                    WatchingDeploy {
                        env_name: modal.target_env.clone(),
                        target_label,
                        armed_at,
                        deadline_at,
                    },
                );
                // Don't clobber a "auto-rollback armed" message if
                // both flags were set — append instead.
                if let Some(existing) = self.status_message.as_mut() {
                    existing.push_str(&format!("; watching for Green ({secs}s)"));
                } else {
                    self.status_message = Some(format!(
                        "watching deploy: {secs}s to reach Green or report timeout"
                    ));
                }
            }
        }
        // SsmRun's dispatch shape doesn't fit the standard
        // `Result<(), _>` + `ActionResult` pipeline below — the SDK
        // call returns per-instance rows that surface in a
        // TextOverlay. Short-circuit to a dedicated helper that
        // carries the audit / pending / spawn dance with the right
        // payload. Everything ABOVE this point (read-only check, demo
        // guard, deploy-only auto-rollback / wait-for-green arming —
        // none of which fires for SsmRun) still runs uniformly.
        if modal.action == Action::SsmRun {
            let command = modal.ssm_run_command.clone().unwrap_or_default();
            let instances = modal.ssm_run_instances.clone().unwrap_or_default();
            self.spawn_ssm_run_impl(modal.target_env.clone(), command, instances);
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let action = modal.action;
        let env = modal.target_env.clone();
        let swap_with = modal.swap_with.clone();
        let deploy_version = modal.deploy_version.clone();
        let upgrade_arn = modal.upgrade_platform_arn.clone();
        let clone_target = modal.clone_target.clone();
        let scale_min = modal.scale_min;
        let scale_max = modal.scale_max;
        write_audit_entry(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            action,
            &env,
            swap_with.as_deref(),
        );
        self.push_pending(action.label(), env.clone());
        tokio::spawn(async move {
            let result = match action {
                Action::Rebuild => aws.rebuild_env(&env).await,
                Action::RestartAppServer => aws.restart_app_server(&env).await,
                Action::Terminate => aws.terminate_env(&env).await,
                Action::SwapCnames => match swap_with {
                    Some(dest) => aws.swap_cnames(&env, &dest).await,
                    None => Err(color_eyre::eyre::eyre!("swap target missing")),
                },
                Action::Deploy => match deploy_version {
                    Some(ver) => aws.deploy_version(&env, &ver).await,
                    None => Err(color_eyre::eyre::eyre!("deploy version missing")),
                },
                Action::UpgradePlatform => match upgrade_arn {
                    Some(arn) => aws.upgrade_platform(&env, &arn).await,
                    None => Err(color_eyre::eyre::eyre!("upgrade platform ARN missing")),
                },
                Action::Clone => match clone_target {
                    Some(target) => aws.clone_env(&env, &target).await,
                    None => Err(color_eyre::eyre::eyre!("clone target name missing")),
                },
                Action::Scale => match (scale_min, scale_max) {
                    (Some(mn), Some(mx)) => aws.scale_env(&env, mn, mx).await,
                    _ => Err(color_eyre::eyre::eyre!("scale min/max missing")),
                },
                Action::AbortUpdate => aws.abort_environment_update(&env).await,
                // Capacity opens a modal form (cmd_capacity) and dispatches
                // via spawn_option_settings_update — it never reaches
                // spawn_action's ConfirmModal path. Same for Config* and
                // TerminateInstance which have dedicated spawn paths.
                // SsmRun is short-circuited above via spawn_ssm_run_impl
                // and never reaches this match — included here for
                // exhaustiveness with a defensive error in case the
                // short-circuit is ever removed.
                Action::Capacity
                | Action::ConfigSave
                | Action::ConfigDelete
                | Action::ConfigApply
                | Action::TerminateInstance
                | Action::SsmRun => Err(color_eyre::eyre::eyre!(
                    "internal: {} dispatched through spawn_action path",
                    action.label()
                )),
            }
            .map_err(|e| flatten_err("action", e));
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action,
                env_name: env,
                result,
            });
        });
    }
}
