//! Deploy-path async spawns: uploading and deploying a bundle,
//! previewing what a deploy would change, and the pre-flight checks
//! (lint, dry run, health probe, unavailability estimate) that gate it.

use super::*;

impl App {
    /// Dispatch an `UpdateEnvironment(option_settings)` call. Used by the
    /// three "tweak one or two settings" commands (`:logs-stream`, `:notify`,
    /// `:managed-window`); each pushes its own pending row + audit entry
    /// then funnels through here. `summary` is the human-readable label
    /// that ends up in the toast and the pending panel.
    pub(crate) fn spawn_option_settings_update(
        &mut self,
        summary: String,
        to_set: Vec<(String, String, String)>,
        to_remove: Vec<(String, String)>,
    ) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, &summary) {
            return;
        }
        if to_set.is_empty() && to_remove.is_empty() {
            self.error_message = Some(format!(
                "{summary}: nothing to do (no options to set or remove)"
            ));
            return;
        }
        let env_name = env.name.clone();
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "UpdateOptionSettings",
            env_name.as_str(),
            &[("summary", summary.as_str())],
        );
        self.push_pending(summary.clone(), env_name.clone());
        // No status_message ack here — the pending-actions pill in the
        // header (`⏳ N`) is the truth-source for in-flight work, and a
        // status_message ack would just race with whatever the operator
        // last set there. Completion fires a Success / Error toast.
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        let summary_for_msg = summary.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        // Capture inputs for the optional :undo round-trip — the
        // option-settings fetch happens BEFORE the write so we can
        // record the prior state and offer a clean reverse-action.
        let app_for_undo = env.application.clone();
        let env_for_undo = env_name.clone();
        let summary_for_undo = summary.clone();
        let to_set_for_undo = to_set.clone();
        let to_remove_for_undo = to_remove.clone();
        tokio::spawn(async move {
            // Fetch current option-settings for the affected keys
            // BEFORE the write so we can build the reverse-action.
            // Read failure doesn't block the write — undo is a
            // safety net, not a correctness invariant.
            let undo_entry = match aws
                .fetch_env_option_settings(&app_for_undo, &env_for_undo)
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
            // Only record undo on a successful write — otherwise
            // `:undo` would "revert" a write that never landed.
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
    }

    /// Register a new application version pointing at an existing S3
    /// object, and optionally deploy it. Skips the local-read +
    /// storage-location + put_object steps that `spawn_deploy_from_local`
    /// does. Useful when the bundle is already in S3 — most CI pipelines
    /// upload artifacts to S3 themselves.
    pub(crate) fn spawn_deploy_from_s3(
        &mut self,
        bucket: String,
        key: String,
        explicit_label: Option<String>,
        description: Option<String>,
        and_deploy: bool,
    ) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, "deploy-from-s3") {
            return;
        }
        // Derive label from the S3 key's basename if not pinned. Same
        // convention as the local-path flow so the audit log + version list
        // are consistent across the two sources.
        let label = explicit_label
            .unwrap_or_else(|| derive_version_label(&key, chrono::Utc::now().timestamp()));
        let env_name = env.name.clone();
        let app_name = env.application.clone();
        let summary = if and_deploy {
            format!("deploy-from-s3 {label}")
        } else {
            format!("create-version-from-s3 {label}")
        };
        let source_s3 = format!("s3://{bucket}/{key}");
        let and_deploy_str = and_deploy.to_string();
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "DeployFromS3",
            env_name.as_str(),
            &[
                ("label", label.as_str()),
                ("source", source_s3.as_str()),
                ("and_deploy", and_deploy_str.as_str()),
            ],
        );
        self.push_pending(summary.clone(), env_name.clone());
        // In-flight ack lives on the pending pill; completion toasts.
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        let label_for_msg = label.clone();
        let summary_for_msg = summary.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        let description_owned = description;
        tokio::spawn(async move {
            if let Err(e) = aws
                .create_app_version(
                    &app_name,
                    &label_for_msg,
                    description_owned.as_deref(),
                    &bucket,
                    &key,
                )
                .await
            {
                let err = format!("create-version: {}", flatten_err("create_app_version", e));
                finish_deploy_from_local(
                    &tx,
                    gen,
                    env_for_msg,
                    label_for_msg,
                    summary_for_msg,
                    account.as_deref(),
                    profile.as_deref(),
                    &region,
                    Err(err),
                );
                return;
            }
            if and_deploy {
                if let Err(e) = aws.deploy_version(&env_for_msg, &label_for_msg).await {
                    let err = format!("deploy: {}", flatten_err("deploy_version", e));
                    finish_deploy_from_local(
                        &tx,
                        gen,
                        env_for_msg,
                        label_for_msg,
                        summary_for_msg,
                        account.as_deref(),
                        profile.as_deref(),
                        &region,
                        Err(err),
                    );
                    return;
                }
            }
            finish_deploy_from_local(
                &tx,
                gen,
                env_for_msg,
                label_for_msg,
                summary_for_msg,
                account.as_deref(),
                profile.as_deref(),
                &region,
                Ok(()),
            );
        });
    }

    /// Upload a local bundle to EB's managed S3 storage, register a new
    /// application version pointing at it, and optionally deploy it to the
    /// selected env. The chain runs serially in one spawned task; failures
    /// at any stage surface as a single error toast with the stage name.
    /// Fetch the candidate version's metadata + the currently-deployed
    /// version's metadata for the env's app, render a preview text, and
    /// land it as a TextOverlay. EB application versions carry only a
    /// label + description + source-bundle S3 pointer + created date;
    /// there's no option-settings diff to surface (settings live on the
    /// env, not the version). So the preview is "informed deploy" —
    /// label, age, description, plus a warning when the candidate is
    /// older than what's currently deployed.
    pub(crate) fn spawn_deploy_preview(&self, env: crate::aws::Environment, label: String) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        let current_label = env.version_label.clone();
        tokio::spawn(async move {
            let body = match aws.list_application_versions(&app_name).await {
                Ok(versions) => format_deploy_preview(&env_name, &current_label, &label, &versions),
                Err(e) => format!(
                    "deploy preview — failed to fetch application versions:\n  {}\n",
                    flatten_err_to_string(&e)
                ),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("deploy preview — {env_name} ← {label}"),
                body,
            });
        });
    }

    pub(crate) fn spawn_deploy_from_local(
        &mut self,
        path: String,
        explicit_label: Option<String>,
        description: Option<String>,
        and_deploy: bool,
    ) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        // Route through `deny_write` (not bare `is_read_only_for`) so the
        // gate picks up the `--demo` refusal alongside read-only / safety
        // pins. Pre-0.17.4 this dispatched real audit lines in --demo.
        if self.deny_write(&env.name, "deploy-from-local") {
            return;
        }
        // Path resolution: ~ expansion + check file exists + size.
        // The bundle is streamed from disk by the AWS layer, not slurped
        // here — keeps RAM flat regardless of bundle size and lets the
        // multipart path handle anything above MULTIPART_THRESHOLD.
        let resolved = expand_tilde(&path);
        let resolved_path = std::path::PathBuf::from(&resolved);
        let size = match std::fs::metadata(&resolved_path) {
            Ok(m) => m.len(),
            Err(e) => {
                self.error_message = Some(format!("can't read {resolved}: {e}"));
                return;
            }
        };
        if size == 0 {
            self.error_message = Some(format!("{resolved} is empty"));
            return;
        }
        // Derive label if the operator didn't pin one. We use the filename
        // basename + a unix timestamp so re-deploys don't collide.
        let label = explicit_label
            .unwrap_or_else(|| derive_version_label(&resolved, chrono::Utc::now().timestamp()));
        let env_name = env.name.clone();
        let app_name = env.application.clone();
        let summary = if and_deploy {
            format!("deploy-from-local {label}")
        } else {
            format!("upload-version {label}")
        };
        let size_str = size.to_string();
        let and_deploy_str = and_deploy.to_string();
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "DeployFromLocal",
            env_name.as_str(),
            &[
                ("label", &label),
                ("bytes", &size_str),
                ("and_deploy", &and_deploy_str),
            ],
        );
        self.push_pending(summary.clone(), env_name.clone());
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env_name.clone();
        let label_for_msg = label.clone();
        let summary_for_msg = summary.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        let description_owned = description;
        tokio::spawn(async move {
            // Three (or four) stages: bucket → put → create version → (deploy).
            // We surface the stage name in any error so the operator knows
            // where it failed.
            let bucket = match aws.create_storage_location().await {
                Ok(b) => b,
                Err(e) => {
                    let err = format!(
                        "storage-location: {}",
                        flatten_err("create_storage_location", e)
                    );
                    finish_deploy_from_local(
                        &tx,
                        gen,
                        env_for_msg,
                        label_for_msg,
                        summary_for_msg,
                        account.as_deref(),
                        profile.as_deref(),
                        &region,
                        Err(err),
                    );
                    return;
                }
            };
            // Key: `applications/<app>/<label>` mirrors EB's own layout.
            let key = format!("applications/{app_name}/{label_for_msg}");
            if let Err(e) = aws.upload_bundle(&bucket, &key, &resolved_path).await {
                let err = format!("s3-put: {}", flatten_err("upload_bundle", e));
                finish_deploy_from_local(
                    &tx,
                    gen,
                    env_for_msg,
                    label_for_msg,
                    summary_for_msg,
                    account.as_deref(),
                    profile.as_deref(),
                    &region,
                    Err(err),
                );
                return;
            }
            if let Err(e) = aws
                .create_app_version(
                    &app_name,
                    &label_for_msg,
                    description_owned.as_deref(),
                    &bucket,
                    &key,
                )
                .await
            {
                let err = format!("create-version: {}", flatten_err("create_app_version", e));
                finish_deploy_from_local(
                    &tx,
                    gen,
                    env_for_msg,
                    label_for_msg,
                    summary_for_msg,
                    account.as_deref(),
                    profile.as_deref(),
                    &region,
                    Err(err),
                );
                return;
            }
            if and_deploy {
                if let Err(e) = aws.deploy_version(&env_for_msg, &label_for_msg).await {
                    let err = format!("deploy: {}", flatten_err("deploy_version", e));
                    finish_deploy_from_local(
                        &tx,
                        gen,
                        env_for_msg,
                        label_for_msg,
                        summary_for_msg,
                        account.as_deref(),
                        profile.as_deref(),
                        &region,
                        Err(err),
                    );
                    return;
                }
            }
            finish_deploy_from_local(
                &tx,
                gen,
                env_for_msg,
                label_for_msg,
                summary_for_msg,
                account.as_deref(),
                profile.as_deref(),
                &region,
                Ok(()),
            );
        });
    }

    /// Dispatch a `DeleteApplicationVersion` for the selected env's app.
    /// `force` also requests `DeleteSourceBundle=true` so the underlying
    /// `.zip` is removed from the env's storage bucket.
    pub(crate) fn spawn_delete_app_version(&mut self, label: String, force: bool) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, "delete-version") {
            return;
        }
        let application = env.application.clone();
        // Target carries the optional "(+source bundle)" suffix so a
        // tail of the audit log makes the force flag visible without
        // a separate extras field.
        let target_label = if force {
            format!("{application}/{label} (+source bundle)")
        } else {
            format!("{application}/{label}")
        };
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "DeleteAppVersion",
            &target_label,
            &[],
        );
        // In-flight ack lives on the pending pill; completion toasts.
        let pending_label = if force {
            "Delete app version (+source)"
        } else {
            "Delete app version"
        };
        let pending_target = format!("{application}/{label}");
        self.push_pending(pending_label, pending_target);
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        let app_for_msg = application.clone();
        let label_for_msg = label.clone();
        let target_label_for_outcome = target_label.clone();
        tokio::spawn(async move {
            let result = aws
                .delete_application_version(&application, &label, force)
                .await
                .map_err(|e| flatten_err("delete_application_version", e));
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                "DeleteAppVersion",
                &target_label_for_outcome,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[],
            );
            let _ = tx.send(AppMsg::DeleteAppVersion {
                gen,
                application: app_for_msg,
                label: label_for_msg,
                force,
                result,
            });
        });
    }

    /// Dispatch an `UpdateTagsForResource` for the selected env. `to_add`
    /// and `to_remove` follow EB semantics: the API allows both in a single
    /// call; we surface a summary toast either way.
    pub(crate) fn spawn_tag_update(
        &mut self,
        to_add: Vec<(String, String)>,
        to_remove: Vec<String>,
    ) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.deny_write(&env.name, "tag edits") {
            return;
        }
        let Some(arn) = env.arn.clone() else {
            self.error_message = Some(format!("env {} has no ARN — re-fetch and retry", env.name));
            return;
        };
        if to_add.is_empty() && to_remove.is_empty() {
            self.error_message =
                Some("nothing to do — provide tags to add or keys to remove".into());
            return;
        }
        let summary = if !to_add.is_empty() {
            let keys: Vec<String> = to_add.iter().map(|(k, _)| k.clone()).collect();
            format!("tag {}", keys.join(","))
        } else {
            format!("untag {}", to_remove.join(","))
        };
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "UpdateTags",
            &env.name,
            &[("summary", &summary)],
        );
        // Label intentionally carries the operation (`tag …` / `untag …`) so
        // the pending panel distinguishes simultaneous edits. The pending
        // pill in the header is the in-flight truth-source; no
        // status_message ack here (would race with the next operation).
        self.push_pending(summary.clone(), env.name.clone());
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_name = env.name.clone();
        let summary_for_msg = summary.clone();
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        tokio::spawn(async move {
            let result = aws
                .update_tags(&arn, &to_add, &to_remove)
                .await
                .map_err(|e| flatten_err("update_tags", e));
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                "UpdateTags",
                &env_name,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[("summary", &summary)],
            );
            let _ = tx.send(AppMsg::TagUpdate {
                gen,
                env_name,
                summary: summary_for_msg,
                result,
            });
        });
    }

    pub(crate) fn spawn_preflight_events(&mut self, env_name: String) {
        let env_for_msg = env_name.clone();
        self.spawn_aws_in(
            self.client_for_env(&env_for_msg),
            "preflight_events",
            move |aws| async move { aws.list_events_for_env(&env_name, 3).await },
            move |gen, result| AppMsg::PreflightEvents {
                gen,
                env_name: env_for_msg,
                result,
            },
        );
    }

    pub(crate) fn spawn_dry_run(&mut self, env_name: String) {
        let env_for_msg = env_name.clone();
        self.spawn_aws_in(
            self.client_for_env(&env_for_msg),
            "dry_run_list_instances",
            move |aws| async move { aws.list_instances(&env_name).await },
            move |gen, result| AppMsg::DryRunResult {
                gen,
                env_name: env_for_msg,
                result,
            },
        );
    }

    /// Kick off the version-list fetch for the Deploy confirm
    /// modal's inline preview. Builds the `format_deploy_preview`
    /// body off-thread so the spawn handler doesn't allocate; the
    /// handler just stuffs the rendered string into the modal.
    /// `current_label` may be empty for a brand-new env (first
    /// deploy) — the formatter handles that gracefully.
    pub(crate) fn spawn_version_preview(
        &mut self,
        app_name: String,
        env_name: String,
        current_label: String,
        candidate_label: String,
    ) {
        let env_for_msg = env_name.clone();
        let env_for_render = env_name.clone();
        let candidate_for_render = candidate_label.clone();
        self.spawn_aws_in(
            self.client_for_env(&env_for_msg),
            "version_preview",
            move |aws| async move { aws.list_application_versions(&app_name).await },
            move |gen, result| {
                let body = match result {
                    Ok(versions) => Ok(format_deploy_preview(
                        &env_for_render,
                        &current_label,
                        &candidate_for_render,
                        &versions,
                    )),
                    Err(e) => Err(e),
                };
                AppMsg::VersionPreview {
                    gen,
                    env_name: env_for_msg,
                    result: body,
                }
            },
        );
    }

    /// Pre-deploy health-check probe for the confirm modal. Reads
    /// the env's current `Application Healthcheck URL` option
    /// (defaults to `/` if unset), composes a probe URL against
    /// the env's CNAME, and HEADs it via curl with a 2s cap. The
    /// outcome (success / non-2xx / timeout / refusal) is just a
    /// warning surface; it doesn't block the deploy.
    ///
    /// Shells out to `curl` for the same reason `fetch_url_text`
    /// and `fire_audit_webhook` do — keeps ebman HTTP-client-dep
    /// free. Output is parsed to an HTTP status code (or classified
    /// as a transport error) so the warning text can surface what
    /// specifically failed.
    pub(crate) fn spawn_health_check_probe(
        &mut self,
        app_name: String,
        env_name: String,
        cname: String,
    ) {
        let env_for_msg = env_name.clone();
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            // Look up the configured health-check path. Missing or
            // empty setting means EB defaults to `/`, so we probe
            // the env root.
            let path = match aws.fetch_env_option_settings(&app_name, &env_name).await {
                Ok(opts) => opts
                    .into_iter()
                    .find(|(ns, name, _)| {
                        ns == "aws:elasticbeanstalk:application"
                            && name == "Application Healthcheck URL"
                    })
                    .map(|(_, _, v)| v)
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "/".into()),
                Err(_) => "/".into(),
            };
            let url = crate::probe::build_health_check_probe_url(&cname, &path);
            let result = crate::probe::run_health_check_probe(&url).await;
            let _ = tx.send(AppMsg::HealthCheckProbe {
                gen,
                env_name: env_for_msg,
                result,
            });
        });
    }

    /// Spawn the pre-deploy unavailability estimator. Fetches the
    /// env's option-settings, extracts deployment policy + batch +
    /// ASG max, formats a one-line summary, and emits
    /// `AppMsg::UnavailabilityEstimate`. Uses its own fetch rather
    /// than piggy-backing on the health-check probe — two parallel
    /// DescribeConfigurationSettings calls is fine for a one-shot
    /// modal open, and keeps the two features isolated so failure
    /// of one doesn't taint the other.
    pub(crate) fn spawn_unavailability_estimate(&mut self, app_name: String, env_name: String) {
        let env_for_msg = env_name.clone();
        self.spawn_aws_in(
            self.client_for_env(&env_for_msg),
            "unavailability_estimate",
            move |aws| async move { aws.fetch_env_option_settings(&app_name, &env_name).await },
            move |gen, result| {
                let line = result.ok().map(|opts| {
                    let (policy, batch, btype, asg_max) = extract_unavailability_inputs(&opts);
                    let count = compute_unavailability_count(&policy, batch, &btype, asg_max);
                    format_unavailability_line(&policy, count, asg_max)
                });
                AppMsg::UnavailabilityEstimate {
                    gen,
                    env_name: env_for_msg,
                    line,
                }
            },
        );
    }

    /// Run the lint engine against the env at confirm-modal-open
    /// time. Same rules `:lint` and `ebman lint` use; same
    /// operator-tunable disables. Issues at `>= Warn` render as
    /// modal warning lines so the operator sees rule-keyed risk
    /// before authorising the action.
    ///
    /// Uses the same option-settings fetch path as the
    /// unavailability estimate + health-check probe, but issues
    /// its own DescribeConfigurationSettings call to keep the
    /// three features isolated. Failure of the read is non-
    /// blocking — modal renders without lint when the fetch
    /// fails (the operator can still see the rule-output via
    /// `:lint` once the modal closes).
    pub(crate) fn spawn_confirm_lint(&mut self, env: crate::aws::Environment) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        // Snapshot operator-tunable disables — user-level (already
        // mirrored on App) + project-local (read fresh from cwd).
        let mut disabled = self.cfg.lint_disable.clone();
        disabled.extend(crate::project::load_lint_disables_from_cwd());
        let env_for_msg = env.name.clone();
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        // Plumb the live lint-context inputs. latest_stack enables
        // EBL008; required_tags + env_tag_keys (fetched parallel below)
        // enable EBL010; dlq_depth enables EBL011; healthy instance
        // count enables EBL012. All four wire-up tracks landed in 0.18.
        let newer_stack_owned =
            crate::aws::newer_stack_version(&env.solution_stack, &self.latest_stacks);
        let required_tags_owned = self.cfg.required_tags.clone();
        // EBL011 only fires for Worker envs and only when we have a
        // cached DLQ depth (populated by the Queue tab / worker poll).
        let dlq_depth_owned = if env.tier.eq_ignore_ascii_case("Worker") {
            self.worker_dlq_depths.get(&env.name).copied()
        } else {
            None
        };
        let env_arn_owned = env.arn.clone();
        // 0.21: opportunistic lint-input cache lookup. If tags or
        // health are fresh (< LINT_INPUT_CACHE_TTL), skip the
        // corresponding fetch — modal-open latency drops from
        // `max(t_opts, t_tags, t_health)` to `t_opts` when both
        // inputs are cached (typical for repeated modal-opens
        // against the same env).
        let now = std::time::Instant::now();
        let cached_tags: Option<Vec<String>> = self
            .env_tag_cache
            .get(&env.name)
            .and_then(|(v, t)| (now.duration_since(*t) < LINT_INPUT_CACHE_TTL).then(|| v.clone()));
        let cached_health: Option<i64> = self
            .env_health_cache
            .get(&env.name)
            .and_then(|(v, t)| (now.duration_since(*t) < LINT_INPUT_CACHE_TTL).then_some(*v));
        let cache_env_name = env.name.clone();
        tokio::spawn(async move {
            // Parallel fetch: option settings (always), tags + health
            // (only if not cached). The `tags_fut` / `health_fut`
            // async blocks resolve to the cached value when fresh —
            // tokio::join! still polls all three but the cached
            // branches return immediately.
            let opts_fut = aws.fetch_env_option_settings(&app_name, &env_name);
            let tags_was_cached = cached_tags.is_some();
            let tags_fut = async {
                if let Some(cached) = cached_tags {
                    Some(cached)
                } else {
                    match env_arn_owned.as_deref() {
                        Some(arn) => aws
                            .list_tags(arn)
                            .await
                            .ok()
                            .map(|kvs| kvs.into_iter().map(|(k, _)| k).collect()),
                        None => None,
                    }
                }
            };
            let health_was_cached = cached_health.is_some();
            let health_fut = async {
                if let Some(cached) = cached_health {
                    Some(cached)
                } else {
                    aws.fetch_env_instance_counts(&env_name)
                        .await
                        .ok()
                        .map(|c| c.healthy as i64)
                }
            };
            let (opts_res, tags_opt, health_opt) = tokio::join!(opts_fut, tags_fut, health_fut);
            // Send cache-update before lint computation so the cache
            // is fresh for the NEXT modal-open even if lint logic
            // changes shape.
            if !tags_was_cached || !health_was_cached {
                let _ = tx.send(AppMsg::LintInputsCached {
                    gen,
                    env_name: cache_env_name,
                    tags: if tags_was_cached {
                        None
                    } else {
                        tags_opt.clone()
                    },
                    healthy: if health_was_cached { None } else { health_opt },
                });
            }
            let env_tag_keys_owned: Vec<String> = tags_opt.unwrap_or_default();
            let healthy_count_owned = health_opt;
            let issues = match opts_res {
                Ok(opts) => {
                    let mut ctx = crate::lint::LintContext::for_env(&env, &opts)
                        .with_required_tags(&required_tags_owned)
                        .with_env_tag_keys(&env_tag_keys_owned);
                    if let Some(newer) = newer_stack_owned.as_deref() {
                        ctx = ctx.with_newer_stack_available(newer);
                    }
                    if let Some(depth) = dlq_depth_owned {
                        ctx = ctx.with_dlq_depth(depth);
                    }
                    if let Some(count) = healthy_count_owned {
                        ctx = ctx.with_healthy_count(count);
                    }
                    let rules = crate::lint::default_rules(&disabled);
                    crate::lint::run_rules(&rules, &ctx)
                }
                Err(_) => Vec::new(),
            };
            let _ = tx.send(AppMsg::ConfirmModalLint {
                gen,
                env_name: env_for_msg,
                issues,
            });
        });
    }
}
