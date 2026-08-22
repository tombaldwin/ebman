//! Operator `:commands` that reach into a running environment:
//! rollback, remote command execution, SSH, and the `$EDITOR`-backed
//! environment-variable edit.

use super::*;

impl App {
    /// `:rollback` — redeploy the env's previously-deployed version.
    /// Fetches the env's recent events, scans them for the version
    /// label that was current before this one (see
    /// [`previous_version_label`]), and opens the standard deploy
    /// confirm modal for it — so the operator sees + confirms the
    /// target, and the 5s undo window still applies.
    pub(crate) fn cmd_rollback(&mut self, rest: &[&str]) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, "rollback") {
            return;
        }
        // `--auto-rollback Nm` arms the same watchdog as
        // `:deploy LABEL --auto-rollback`. Composes with `--to LABEL`
        // so the operator can dispatch "roll back to build-820,
        // auto-roll-forward to build-823 if Green doesn't land
        // within Nm". Same duration grammar (`parse_window_ms`).
        let auto_rollback_secs = parse_named_arg::<String>(rest, "--auto-rollback").and_then(|s| {
            let ms = crate::aws::parse_window_ms(&s)?;
            Some((ms / 1000) as u64)
        });
        if rest.contains(&"--auto-rollback") && auto_rollback_secs.is_none() {
            self.error_message =
                Some("--auto-rollback expects a duration like `5m` / `30m` / `1h`".into());
            return;
        }

        // `:rollback --to LABEL` — operator picked the target
        // themselves. Skip snapshot detection + event-scan and
        // route straight to the deploy confirm with the named
        // label. EB will reject an unknown label downstream
        // with a clear error, so no pre-validation is needed.
        if let Some(target) = parse_named_arg::<String>(rest, "--to") {
            if target.is_empty() {
                self.error_message = Some("--to expects a version label".into());
                return;
            }
            if target == env.version_label {
                self.error_message = Some(format!("{target} is already the deployed version"));
                return;
            }
            self.open_parameterised_action(
                Action::Deploy,
                ParameterisedAction {
                    deploy_version: Some(target.clone()),
                    auto_rollback_secs,
                    ..Default::default()
                },
            );
            self.status_message = Some(format!("rollback target: {target} (operator-specified)"));
            return;
        }

        let env_name = env.name.clone();
        let current_version = env.version_label.clone();
        // Prefer the captured pre-deploy snapshot if one exists —
        // more reliable than scanning events (which can hit the
        // 100-event window cap on chatty envs and miss the actual
        // previous version). The snapshot was taken right before
        // the deploy we'd be rolling back from, so it's exactly
        // what the operator means.
        if let Some(snapshot) = self.deploy_snapshots.get(&env_name).cloned() {
            if snapshot.previous_version_label != current_version {
                self.open_parameterised_action(
                    Action::Deploy,
                    ParameterisedAction {
                        deploy_version: Some(snapshot.previous_version_label.clone()),
                        auto_rollback_secs,
                        ..Default::default()
                    },
                );
                let age = (chrono::Utc::now() - snapshot.taken_at).num_seconds();
                self.status_message = Some(format!(
                    "rollback target: {} (from snapshot taken {}s ago)",
                    snapshot.previous_version_label, age
                ));
                return;
            }
        }
        // Fallback: scan the env's recent event history for the
        // most-recent version_label that differs from current. The
        // RollbackTarget message handler opens the confirm modal.
        // The event-scan path doesn't currently thread `auto_rollback_secs`
        // through `AppMsg::RollbackTarget` — surface a friendly
        // refusal when the operator asked for it but we had to fall
        // back to the scan, so they don't think their flag was
        // honoured silently.
        if auto_rollback_secs.is_some() {
            self.error_message = Some(format!(
                "--auto-rollback needs an in-memory snapshot for {env_name} — none captured. \
                 Try `:rollback --to LABEL --auto-rollback Nm` to name the target explicitly."
            ));
            return;
        }
        let client = self.client_for_env(&env_name);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!("rollback: finding {env_name}'s previous version…"));
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => aws
                    .list_events_for_env(&env_name, 100)
                    .await
                    .map_err(|e| flatten_err("list_events_for_env", e)),
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            let _ = tx.send(AppMsg::RollbackTarget {
                gen,
                env_name,
                current_version,
                result,
            });
        });
    }

    /// `:changes` — config-change timeline for the selected env: the
    /// deploy + configuration-update events from `DescribeEvents`,
    /// newest-first, with routine health/scaling noise filtered out.
    /// `:ssm-run "<shell-command>"` — fan a shell command out across
    /// the selected env's instances via SSM Run Command, poll the
    /// per-instance results, and land them in a TextOverlay. Sources
    /// the target list from cached `Detail.instances` (same as `:ssh`'s
    /// no-arg form) — if Detail isn't open with the Instances tab
    /// loaded, surfaces a clear error. The command runs as the SSM
    /// agent's default user (root on most EB AMIs); operators should
    /// treat this as a write operation and prefer read-only probes
    /// (e.g. `:ssm-run "uptime"`, `:ssm-run "ls /var/log"`) over
    /// state-mutating shells. Hard-capped at 60s wall-clock per
    /// command to keep the overlay from hanging on a stuck instance.
    pub(crate) fn cmd_ssm_run(&mut self, rest: &[&str]) {
        // The shell command is everything after `:ssm-run`. Rejoin
        // tokens with single spaces — the operator can quote-wrap to
        // preserve internal whitespace if needed. EB CLI's
        // `eb ssh -c '...'` uses the same shape.
        //
        // Dispatch flow: parse + resolve target instances + read-only
        // gate fire fast (before the modal opens), then route through
        // `open_parameterised_action` so the operator gets the
        // standard Y/N confirm with the command + fan-out count
        // before anything reaches the SDK. spawn_action short-
        // circuits to `spawn_ssm_run_impl` for the dispatch tail.
        if rest.is_empty() {
            self.error_message = Some(
                "usage: :ssm-run \"<shell-command>\"  (fans the command out across the env's instances; quotes preserve whitespace)".into(),
            );
            return;
        }
        let command_str = rest.join(" ");
        let trimmed = command_str
            .trim_matches(|c: char| c == '"' || c == '\'')
            .to_string();
        if trimmed.is_empty() {
            self.error_message = Some("empty command — nothing to run".into());
            return;
        }
        let instances: Vec<String> = self
            .detail
            .as_ref()
            .map(|d| d.instances.iter().map(|i| i.id.clone()).collect())
            .unwrap_or_default();
        if instances.is_empty() {
            self.error_message = Some(
                "no cached instances — open the env's Detail/Instances tab first so :ssm-run knows what to target".into(),
            );
            return;
        }
        // The env name comes from the Detail tab (which is what
        // sourced the instance list). Falling back to selected_env()
        // would mismatch if the operator changed selection between
        // opening Detail and running :ssm-run.
        let env_name = self
            .detail
            .as_ref()
            .map(|d| d.env_name.clone())
            .unwrap_or_default();
        // Resolve the env from the cached fleet so
        // `open_parameterised_action_on` has an `Environment` to work
        // with. `selected_env()` could be the wrong env if the cursor
        // moved after Detail was opened, so look up by name.
        let Some(env) = self
            .environments
            .iter()
            .find(|e| e.name == env_name)
            .cloned()
        else {
            self.error_message = Some(format!(
                "ssm-run: env '{env_name}' not in cached fleet — refresh (Ctrl-R) and retry"
            ));
            return;
        };
        // `open_parameterised_action_on` calls `deny_write` for us —
        // it gates the read-only / safety-pin / demo-mode locks.
        self.open_parameterised_action_on(
            env,
            Action::SsmRun,
            ParameterisedAction {
                ssm_run_command: Some(trimmed),
                ssm_run_instances: Some(instances),
                ..Default::default()
            },
        );
    }

    /// `:ssh [INSTANCE-ID]` — open an SSM Session Manager session into
    /// one of the selected env's instances. With an arg (`:ssh i-abc`)
    /// the target is taken verbatim; with no arg, a picker opens over
    /// `Detail.instances` if the operator has the env's Detail view
    /// open with the Instances tab loaded (otherwise a clear error
    /// points them at the missing precondition). Either path routes
    /// to the existing `pending_shell_target → open_embedded_shell`
    /// machinery — same TUI-suspend/resume + alt-screen dance as
    /// pressing `s` on Detail/Instances. Requires the AWS CLI +
    /// `session-manager-plugin` on PATH (the SDK can't substitute —
    /// SSM start-session uses a binary side-channel).
    pub(crate) fn cmd_ssh(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            Some(id) => {
                if !id.starts_with("i-") {
                    self.error_message =
                        Some(format!("expected an EC2 instance ID (`i-…`), got '{id}'"));
                    return;
                }
                // Interactive shell = write surface (same gate as
                // `:ssm-run`). The env for pin purposes is the open
                // Detail env when there is one; global read-only /
                // freeze / incident apply regardless.
                let gate_env = self
                    .detail
                    .as_ref()
                    .map(|d| d.env_name.clone())
                    .unwrap_or_default();
                if self.deny_write(&gate_env, "ssm-session") {
                    return;
                }
                // Log the dispatch. Both this (typed-command) path
                // and the Detail/Instances `s` keybind end up
                // shell-ing out to `aws ssm start-session`; the audit
                // line is the operator-facing breadcrumb.
                crate::audit::append_action_dispatched(
                    self.context.account_id.as_deref(),
                    self.context.profile.as_deref(),
                    &self.context.region,
                    "SsmSession",
                    id,
                    &[("via", "cmd_ssh")],
                );
                self.pending_shell_target = Some(id.to_string());
                self.status_message = Some(format!("opening SSM session to {id}…"));
            }
            None => {
                // Picker path. Source from Detail.instances rather than
                // spawning a fresh DescribeInstancesHealth — keeps the
                // command boundary-free of new async machinery, and the
                // operator's typical journey already passes through
                // Detail/Instances on the way to a session.
                let instances: Vec<String> = self
                    .detail
                    .as_ref()
                    .map(|d| d.instances.iter().map(|i| i.id.clone()).collect())
                    .unwrap_or_default();
                if instances.is_empty() {
                    self.error_message = Some(
                        "no cached instances — open the env's Detail/Instances tab first, or pass an ID (`:ssh i-abc`)".into(),
                    );
                    return;
                }
                self.picker = Some(Picker::new(PickerKind::SshInstance, instances, None));
                self.mode = Mode::Picker;
            }
        }
    }

    /// `:event-time [utc|local|age]` — set how event timestamps render
    /// in the Events panel + Detail/Events tab. No argument cycles
    /// `Utc → Local → Age`. Persists to state.toml. UTC is the
    /// default because it matches the EB / CloudWatch API output the
    /// operator cross-references against.
    pub(crate) fn cmd_event_time(&mut self, rest: &[&str]) {
        let next = match rest.first().copied() {
            None => self.event_panel.time_format.next(),
            Some(arg) => match EventTimeFormat::parse(arg) {
                Some(f) => f,
                None => {
                    self.error_message = Some(format!(
                        "unknown event-time format '{arg}'  (use: utc | local | age)"
                    ));
                    return;
                }
            },
        };
        self.event_panel.time_format = next;
        self.persist_state();
        self.status_message = Some(match next {
            EventTimeFormat::Utc => "event timestamps: UTC (YYYY-MM-DD HH:MM:SSZ)".into(),
            EventTimeFormat::Local => "event timestamps: local time".into(),
            EventTimeFormat::Age => "event timestamps: relative age".into(),
        });
    }

    /// `:env-edit` — bulk env-var editor via `$EDITOR`. Two-stage:
    ///
    ///   1. Async fetch of the env's current env vars
    ///      (`spawn_env_vars_for_edit`).
    ///   2. Main-loop tick takes the result + shells out to
    ///      `$EDITOR` against a temp file, parses the result on
    ///      save, dispatches the diff via `spawn_option_settings_update`.
    ///
    /// Closes the bulk-edit gap that single-key `:env set` /
    /// `:env unset` doesn't. Operator can add / remove / rename
    /// multiple env vars in one update — and saving an unchanged
    /// file is a clean no-op.
    pub(crate) fn cmd_env_edit(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, ":env-edit") {
            return;
        }
        if self.pending_env_edit.is_some() {
            self.error_message =
                Some("another :env-edit is mid-flight — wait for the editor to close".into());
            return;
        }
        let client = self.client_for_env(&env.name);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        let env_name_for_msg = env_name.clone();
        self.status_message = Some(format!("fetching env vars for {env_name}…"));
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => aws
                    .fetch_env_vars(&app_name, &env_name)
                    .await
                    .map_err(|e| flatten_err("fetch_env_vars", e)),
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            let _ = tx.send(AppMsg::EnvVarsForEdit {
                gen,
                env_name: env_name_for_msg,
                result,
            });
        });
    }

    /// Fire `ec2:TerminateInstances` for the selected instance. ASG will
    /// re-launch a replacement automatically. Goes through the same
    /// `AppMsg::ActionResult` path so the status surface stays consistent.
    pub(crate) fn spawn_terminate_instance(&mut self, idx: usize) {
        let Some(d) = self.detail.as_ref() else {
            return;
        };
        let Some(inst) = d.instances.get(idx).cloned() else {
            return;
        };
        let env_name = d.env_name.clone();
        // Route through `deny_write` (not bare `is_read_only_for`) so the
        // gate picks up `--demo` mode alongside read-only / safety pins.
        // Pre-0.17.4 this dispatched real audit lines in --demo.
        if self.deny_write(&env_name, "terminate-instance") {
            return;
        }
        let id = inst.id.clone();
        let client = self.client_for_env(&env_name);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.region_for_name(&env_name),
            "TerminateInstance",
            env_name.as_str(),
            &[("instance", id.as_str())],
        );
        // Pending target carries env + instance id so the operator can tell
        // simultaneous terminations apart. Label must match
        // `Action::TerminateInstance.label()` exactly so the AppMsg handler's
        // `complete_pending` finds the row.
        let target = format!("{env_name}/{id}");
        self.push_pending(Action::TerminateInstance.label(), target.clone());
        // In-flight ack lives on the pending pill; completion toasts.
        let _ = id;
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => aws
                    .terminate_instance(&id)
                    .await
                    .map_err(|e| flatten_err("terminate_instance", e)),
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            let _ = tx.send(AppMsg::ActionResult {
                gen,
                action: Action::TerminateInstance,
                env_name: target,
                result,
            });
        });
    }

    /// Dispatch helper for `Action::SsmRun` — split from `spawn_action`
    /// because the SDK call returns per-instance rows that surface in a
    /// `TextOverlay`, not the `Result<(), String>` shape every other
    /// action variant uses. Carries the audit "dispatched" line + a
    /// status toast + the spawn + the audit "completed" line on
    /// arrival.
    ///
    /// **Deliberately bypasses `push_pending`** (the header `⏳ N` chip
    /// and `:pending` overlay). The 0.17.4 code-review flagged this as
    /// an invariant break — every other write-class dispatch shows in
    /// pending. SsmRun is the exception: it's a one-shot diagnostic
    /// probe with a 60s hard cap, the result lands in a TextOverlay
    /// (full-screen — operator can't miss it), and pending entries
    /// would mostly survive the SSM run as `…dispatching` because the
    /// overlay lands before the operator looks back at the bar.
    /// `:ssm-run` doesn't touch EB state so it doesn't need to
    /// participate in the "what writes are in-flight against the
    /// fleet" surface the pending pill exists to provide.
    pub(crate) fn spawn_ssm_run_impl(
        &mut self,
        env_name: String,
        command: String,
        instances: Vec<String>,
    ) {
        if command.is_empty() || instances.is_empty() {
            // Defensive — `cmd_ssm_run` should have refused before
            // opening the modal. If we ever land here with empty
            // payload, abort cleanly rather than dispatching a
            // pointless command.
            self.error_message = Some("ssm-run: empty command or no resolved instances".into());
            return;
        }
        // Audit-log the dispatch + the completion outcome. SSM
        // commands can mutate state; treating them as write-class
        // operations means an after-the-fact incident review can pin
        // down "who ran what, when, on which env" by tailing
        // ~/.cache/ebman/audit.log. The command string is escaped so
        // quotes don't break the line shape.
        let audit_cmd = command.replace('"', "'");
        let instances_str = instances.len().to_string();
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.region_for_name(&env_name),
            // Use the Debug-format name (`SsmRun`) for audit consistency
            // with cancel_pending_dispatch's UNDONE line and with every
            // other Action variant (`Rebuild`, `Terminate`, …). Pre-
            // 0.17.4 this was the literal "SsmRunCommand" which broke
            // grep-by-action correlation across dispatched/cancelled
            // stages. 0.17.3 audit consumers that grepped for
            // "SsmRunCommand" need to switch to "SsmRun".
            "SsmRun",
            env_name.as_str(),
            &[("instances", &instances_str), ("cmd", &audit_cmd)],
        );
        let client = self.client_for_env(&env_name);
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let command_for_render = command.clone();
        let n = instances.len();
        // Snapshot context for the completion-stage audit line.
        let audit_account = self.context.account_id.clone();
        let audit_profile = self.context.profile.clone();
        let audit_region = self.context.region.clone();
        let audit_env = env_name.clone();
        let audit_cmd_for_outcome = audit_cmd.clone();
        self.status_message = Some(format!("running `{command}` on {n} instance(s)…"));
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => aws
                    .run_shell_command(&instances, &command, 60)
                    .await
                    .map_err(|e| flatten_err("run_shell_command", e)),
                Err(e) => Err(flatten_err("cached_client", e)),
            };
            let ok_count_str = match &result {
                Ok(rows) => {
                    let oks = rows.iter().filter(|r| r.status == "Success").count();
                    format!("{oks}/{n}")
                }
                Err(_) => format!("0/{n}"),
            };
            crate::audit::append_action_completed(
                audit_account.as_deref(),
                audit_profile.as_deref(),
                &audit_region,
                "SsmRun", // canonical name — see dispatched-stage comment above
                &audit_env,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[("ok_count", &ok_count_str), ("cmd", &audit_cmd_for_outcome)],
            );
            let body = match result {
                Ok(rows) => format_ssm_results(&command_for_render, &rows),
                Err(e) => format!("ssm-run: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: "ssm-run".into(),
                body,
            });
        });
    }
}
