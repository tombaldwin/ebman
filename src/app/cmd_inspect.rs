//! Read-only `:commands` that build an overlay from AWS state:
//! secrets, lineage, changes, option settings, environment diffs,
//! RDS, listeners, and the LLM-backed `:explain`.

use super::*;

impl App {
    /// `:secrets [FILTER]` — list Secrets Manager secrets in the
    /// active region. Optional substring filter matches against
    /// secret name. Output: one section per secret with name +
    /// ARN + description + last-changed / last-rotated dates.
    /// Operator yanks the ARN to paste into `:env-edit` /
    /// `:env set ENV_VAR ARN` for downstream consumption.
    ///
    /// No secret *values* shown here — that's a separate explicit
    /// `:secret NAME` call so an accidentally-typed `:secrets`
    /// doesn't dump credentials to the screen.
    pub(crate) fn cmd_secrets(&mut self, rest: &[&str]) {
        let filter = rest.first().map(|s| s.to_string());
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let title_filter = filter.clone();
        self.status_message = Some(match filter.as_deref() {
            Some(f) => format!("listing secrets matching '{f}'…"),
            None => "listing secrets…".into(),
        });
        tokio::spawn(async move {
            let result = aws
                .list_secrets(filter.as_deref())
                .await
                .map_err(|e| flatten_err("list_secrets", e));
            let body = match result {
                Ok(rows) => render_secrets_overlay(&rows, title_filter.as_deref()),
                Err(e) => format!("secrets: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: "secrets".into(),
                body,
            });
        });
    }

    /// `:secret NAME` — fetch and reveal a single Secrets Manager
    /// secret's value. Requires an explicit name to make this an
    /// opt-in action (accidental `:secret` with no arg is an
    /// error, not a "dump every secret"). Audit-logs the read so
    /// the operator's CloudTrail-equivalent has a record.
    ///
    /// Output respects `app.view.redact` — when redact mode is on, the
    /// value is hashed instead of shown. The operator can flip
    /// `:redact off` first if they need to see it.
    pub(crate) fn cmd_secret_view(&mut self, rest: &[&str]) {
        let Some(name) = rest.first().map(|s| s.to_string()) else {
            self.error_message =
                Some("usage: :secret NAME  (NAME or full ARN; see :secrets to list)".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let redact = self.view.redact;
        crate::audit::append_action_dispatched(
            self.context.account_id.as_deref(),
            self.context.profile.as_deref(),
            &self.context.region,
            "GetSecretValue",
            &name,
            &[],
        );
        // Captured for the completion audit line written from the task.
        let account = self.context.account_id.clone();
        let profile = self.context.profile.clone();
        let region = self.context.region.clone();
        self.status_message = Some(format!("fetching secret '{name}'…"));
        tokio::spawn(async move {
            let result = aws
                .fetch_secret_value(&name)
                .await
                .map_err(|e| flatten_err("fetch_secret_value", e));
            // Audit the completion — `stage=completed`, matching the
            // dispatched/completed pairing of the write paths. (The
            // AWS-side CloudTrail event is the canonical record; this
            // is ebman's own breadcrumb.)
            crate::audit::append_action_completed(
                account.as_deref(),
                profile.as_deref(),
                &region,
                "GetSecretValue",
                &name,
                result.as_ref().map(|_| ()).map_err(|e| e.as_str()),
                &[],
            );
            let body = match result {
                Ok(value) => render_secret_value_overlay(&name, &value, redact),
                Err(e) => format!("secret: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("secret — {name}"),
                body,
            });
        });
    }

    /// `:lineage` — chronological deploy timeline for the selected env.
    /// Where `:changes` mixes deploy events with config-change events,
    /// `:lineage` filters to deploys only (events that carry a
    /// `version_label`), collapses consecutive same-label events into
    /// one row, and shows the inter-deploy gap (`Δ`) plus deploy span
    /// (`took`). Answers "what was deployed at HH:MM" during incident
    /// review — the cut that's currently a manual scan through
    /// `:changes` mixed output.
    pub(crate) fn cmd_lineage(&mut self) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_name.clone())
        } else {
            self.selected_env().map(|e| e.name.clone())
        };
        let Some(env_name) = env_opt else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!("fetching deploy lineage for {env_name}…"));
        tokio::spawn(async move {
            let result = aws
                .list_events_for_env(&env_name, 100)
                .await
                .map_err(|e| flatten_err("list_events_for_env", e));
            let body = match result {
                Ok(events) => format_lineage(&env_name, &events),
                Err(e) => format!("lineage: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("lineage — {env_name}"),
                body,
            });
        });
    }

    pub(crate) fn cmd_changes(&mut self) {
        let env_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_name.clone())
        } else {
            self.selected_env().map(|e| e.name.clone())
        };
        let Some(env_name) = env_opt else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        self.status_message = Some(format!("fetching change history for {env_name}…"));
        tokio::spawn(async move {
            let result = aws
                .list_events_for_env(&env_name, 100)
                .await
                .map_err(|e| flatten_err("list_events_for_env", e));
            let body = match result {
                Ok(events) => render_changes_overlay(&env_name, &events),
                Err(e) => format!("changes: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("changes — {env_name}"),
                body,
            });
        });
    }

    /// `:explain` — diagnose an IAM `AccessDenied` by calling
    /// `iam:SimulatePrincipalPolicy` against the principal + action
    /// the failed request named. Surfaces the policy decision
    /// (allowed / explicitDeny / implicitDeny), the matched
    /// statements, SCP / permission-boundary blockers, and a
    /// concrete JSON snippet the operator can paste into a policy.
    ///
    /// Two shapes:
    ///   - `:explain` (no args) walks the most recent error message
    ///     looking for the standard AWS AccessDenied shape; uses
    ///     [`parse_access_denied`] to extract principal + action.
    ///   - `:explain ARN ACTION [ACTION ...]` evaluates explicit
    ///     pairs. Useful for pre-flight ("can this role rebuild
    ///     this env?") even when no error has happened yet.
    ///
    /// Caller needs `iam:SimulatePrincipalPolicy` on the target
    /// principal — common gap on assumed-role sessions. We surface
    /// that as a clear error rather than a silent no-op.
    pub(crate) fn cmd_explain(&mut self, rest: &[&str]) {
        // New in 0.14: `:explain EBL###` routes to the LLM-backed
        // explainer for lint issues. Backward-compatible with the
        // existing IAM AccessDenied flow because rule IDs don't
        // start with `arn:aws:` and don't take a second positional.
        if let Some(first) = rest.first().copied() {
            if first.starts_with("EBL") {
                self.cmd_explain_issue(first);
                return;
            }
        }
        let (principal, actions): (String, Vec<String>) = match rest.first().copied() {
            // Args form: ARN + 1..N action names.
            Some(arn) if arn.starts_with("arn:aws:") && rest.len() >= 2 => {
                let actions: Vec<String> = rest[1..].iter().map(|s| s.to_string()).collect();
                (arn.to_string(), actions)
            }
            Some(_) => {
                self.error_message = Some(
                    "usage: :explain (IAM AccessDenied) | :explain ARN ACTION [...] | :explain EBL###"
                        .into(),
                );
                return;
            }
            None => {
                // Walk message_log for the latest error containing
                // "is not authorized to perform" — that's the
                // AWS AccessDenied shape `parse_access_denied`
                // understands.
                let latest = self.message_log.iter().rev().find(|(_, kind, text)| {
                    matches!(kind, MsgKind::Error) && text.contains("is not authorized to perform")
                });
                let Some((_, _, text)) = latest else {
                    self.error_message = Some(
                        "no recent AccessDenied to explain — :explain ARN ACTION to evaluate explicitly".into(),
                    );
                    return;
                };
                match parse_access_denied(text) {
                    Some((arn, action)) => (arn, vec![action]),
                    None => {
                        self.error_message = Some(format!(
                            "couldn't parse principal + action from last error: {text}"
                        ));
                        return;
                    }
                }
            }
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let principal_for_title = principal.clone();
        self.status_message = Some(format!(
            "diagnosing IAM perms for {} action(s) on {principal}…",
            actions.len()
        ));
        tokio::spawn(async move {
            let result = aws
                .simulate_principal_policy(&principal, &actions, &[])
                .await
                .map_err(|e| flatten_err("simulate_principal_policy", e));
            let body = match result {
                Ok(rows) => render_explain_overlay(&principal, &rows),
                Err(e) => format!(
                    "explain: {e}\n\n\
                     This usually means the caller lacks `iam:SimulatePrincipalPolicy`\n\
                     on the target role — common with assumed-role sessions that don't\n\
                     have IAM perms. Try from a profile with IAM access.\n\n\
                     esc / q to close"
                ),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("explain — {principal_for_title}"),
                body,
            });
        });
    }

    /// `:explain EBL###` — LLM-backed explanation of a lint issue.
    /// Runs the lint engine against the currently-selected env,
    /// finds the matching issue, builds the standard explain prompt
    /// via [`crate::llm::build_prompt`], and dispatches to the
    /// configured Provider. Result lands in a TextOverlay (same
    /// surface as the IAM AccessDenied explainer).
    ///
    /// Opt-in via `[explain] enabled = true` in `config.toml` plus
    /// the env-var holding the provider API key. Without consent
    /// the overlay just says so with a config-file pointer.
    fn cmd_explain_issue(&mut self, issue_id: &str) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let mut disabled = self.cfg.lint_disable.clone();
        disabled.extend(crate::project::load_lint_disables_from_cwd());
        let app_name = env.application.clone();
        let env_name_for_fetch = env.name.clone();
        let settings = self.cfg.explain_settings.clone();
        // Snapshot the lint-context inputs that aren't already
        // implied by `&env` + `&opts`. All four 0.18 wire-ups land
        // here too so `:explain` sees the same rule firing pattern
        // as `:lint` (EBL008 newer-stack, EBL010 required-tags,
        // EBL011 worker DLQ, EBL012 healthy-count).
        let newer_stack_owned =
            crate::aws::newer_stack_version(&env.solution_stack, &self.latest_stacks);
        let required_tags_owned = self.cfg.required_tags.clone();
        let dlq_depth_owned = if env.tier.eq_ignore_ascii_case("Worker") {
            self.worker_dlq_depths.get(&env.name).copied()
        } else {
            None
        };
        let env_arn_owned = env.arn.clone();
        let issue_id_owned = issue_id.to_string();
        let issue_id_title = issue_id.to_string();
        self.status_message = Some(format!("explain: building prompt for {issue_id}…"));
        tokio::spawn(async move {
            // Parallel fetch — see spawn_confirm_lint for the rationale.
            let opts_fut = aws.fetch_env_option_settings(&app_name, &env_name_for_fetch);
            let tags_fut = async {
                match env_arn_owned.as_deref() {
                    Some(arn) => aws.list_tags(arn).await.ok(),
                    None => None,
                }
            };
            let health_fut = aws.fetch_env_instance_counts(&env_name_for_fetch);
            let (opts_res, tags_opt, health_res) = tokio::join!(opts_fut, tags_fut, health_fut);
            let env_tag_keys_owned: Vec<String> = tags_opt
                .unwrap_or_default()
                .into_iter()
                .map(|(k, _)| k)
                .collect();
            let healthy_count_owned = health_res.ok().map(|c| c.healthy as i64);
            let body = match opts_res {
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
                    let issues = crate::lint::run_rules(&rules, &ctx);
                    match issues.iter().find(|i| i.rule_id == issue_id_owned) {
                        None => format!(
                            "explain: rule {issue_id_owned} doesn't fire on env {} — nothing to explain.\n\
                             Run :lint to see which issues do fire here.\n\nesc / q to close",
                            env.name
                        ),
                        Some(issue) => {
                            let prompt = crate::llm::build_prompt(issue);
                            // Cache first — operators running the
                            // same explain multiple times in a
                            // session don't burn API calls.
                            match crate::llm::read_cache(issue) {
                                Some(cached) => cached,
                                None => match crate::llm::dispatch(&settings, &prompt).await {
                                    Ok(r) => {
                                        crate::llm::write_cache(issue, &r);
                                        r
                                    }
                                    Err(e) => format!(
                                        "explain: {e}\n\n\
                                         Configure [explain] in {} or run from CLI with `ebman explain {issue_id_owned} --env {}`.\n\n\
                                         esc / q to close",
                                        crate::util::config_file("config.toml").display(),
                                        env.name,
                                    ),
                                },
                            }
                        }
                    }
                }
                Err(e) => format!("explain: fetch_env_option_settings: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("explain — {issue_id_title}"),
                body,
            });
        });
    }

    /// `:options [NAMESPACE]` — full settable-option vocabulary for
    /// the selected env's platform. Closes the biggest console-parity
    /// gap (config discoverability): the console has the canonical
    /// list of every settable EB option with metadata; ebman's
    /// `:set-option NAMESPACE NAME VALUE` requires the operator to
    /// already know the vocabulary.
    ///
    /// `:options` lists everything. `:options NAMESPACE` filters
    /// to one family (e.g. `:options aws:elbv2:listener`,
    /// `:options aws:autoscaling:asg`).
    pub(crate) fn cmd_options(&mut self, rest: &[&str]) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let filter_ns = rest.first().map(|s| s.to_string());
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        self.status_message = Some(format!(
            "fetching config vocabulary for {env_name}… (this can take a few seconds)"
        ));
        tokio::spawn(async move {
            let result = aws
                .fetch_env_configuration_options(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("fetch_env_configuration_options", e));
            let body = match result {
                Ok(rows) => render_options_overlay(&rows, filter_ns.as_deref(), &env_name),
                Err(e) => format!("options: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("options — {env_name}"),
                body,
            });
        });
    }

    /// `:config-diff-local [NAME]` — diff the deployed env's current
    /// option settings against a local EB CLI saved config (the YAML
    /// under `.elasticbeanstalk/saved_configs/<NAME>.cfg.yml`). With
    /// no arg, auto-picks the lone config if there's exactly one;
    /// with multiple, errors and lists names so the operator can
    /// pick. Bridges EB CLI users into ebman: answers "is what I
    /// committed still what's deployed" without rerunning
    /// `eb config get` and eyeballing the diff.
    pub(crate) fn cmd_config_diff_local(&mut self, rest: &[&str]) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let cwd = match std::env::current_dir() {
            Ok(p) => p,
            Err(e) => {
                self.error_message = Some(format!("can't read cwd: {e}"));
                return;
            }
        };
        let path = match rest.first().copied() {
            Some(name) => match crate::saved_config::resolve_saved_config(&cwd, name) {
                Ok(p) => p,
                Err(e) => {
                    self.error_message = Some(format!("config-diff-local: {e}"));
                    return;
                }
            },
            None => {
                let configs = match crate::saved_config::discover_saved_configs(&cwd) {
                    Ok(c) => c,
                    Err(e) => {
                        self.error_message = Some(format!("config-diff-local: {e}"));
                        return;
                    }
                };
                // Slice patterns rather than `match configs.len()`: the
                // one-config arm binds the config, so there's no arm that
                // knows an element exists without holding it.
                match configs.as_slice() {
                    [] => {
                        self.error_message = Some(format!(
                            "no .elasticbeanstalk/saved_configs/*.cfg.yml under {}",
                            cwd.display()
                        ));
                        return;
                    }
                    [only] => only.clone(),
                    many => {
                        let names: Vec<String> = many
                            .iter()
                            .map(|p| crate::saved_config::saved_config_name(p))
                            .collect();
                        self.error_message = Some(format!(
                            "multiple saved configs — pick one: :config-diff-local <{}>",
                            names.join(" | ")
                        ));
                        return;
                    }
                }
            }
        };
        let yaml = match std::fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                self.error_message = Some(format!("reading {}: {e}", path.display()));
                return;
            }
        };
        let local_opts = match crate::saved_config::parse_saved_config(&yaml) {
            Ok(o) => o,
            Err(e) => {
                self.error_message = Some(format!("parsing {}: {e}", path.display()));
                return;
            }
        };
        let local_name = crate::saved_config::saved_config_name(&path);
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let (app_name, env_name) = (env.application.clone(), env.name.clone());
        let env_name_for_title = env_name.clone();
        let local_name_for_title = local_name.clone();
        self.status_message = Some(format!(
            "comparing {env_name} ↔ saved config '{local_name}'…"
        ));
        tokio::spawn(async move {
            let result = aws
                .fetch_env_configuration_options(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("fetch_env_configuration_options", e));
            let body = match result {
                Ok(deployed) => {
                    let diffs = diff_config_options(&local_opts, &deployed);
                    let left_label = format!("local:{local_name}");
                    render_config_diff_overlay(&left_label, &env_name, &diffs)
                }
                Err(e) => format!("config-diff-local: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("config-diff-local — {env_name_for_title} ↔ {local_name_for_title}"),
                body,
            });
        });
    }

    /// `:diff ENV` / `:diff ENV-A ENV-B` — side-by-side env-metadata
    /// comparison (name/tier/status/health/platform/version/cname/
    /// updated). `--ignore-keys "k1,k2"` suppresses matching rows, the
    /// same flag shape `:config-diff` uses. Lives in its own helper (not
    /// inline in `execute_command`) so the `--ignore-keys` literal stays
    /// out of the dispatch-arm scanner's view — see
    /// `commands::tests::registry_covers_every_dispatch_arm`.
    pub(crate) fn cmd_diff(&mut self, rest: &[&str]) {
        // Parse out `--ignore-keys "k1,k2"`, leaving the positional env
        // name(s). Matches the metadata diff's field labels — `version`,
        // `updated`, `cname`, etc. — case-insensitively.
        let mut positionals: Vec<&str> = Vec::new();
        let mut ignore_csv: Option<String> = None;
        let mut iter = rest.iter().copied();
        let mut bad_arg: Option<String> = None;
        while let Some(arg) = iter.next() {
            match arg {
                "--ignore-keys" => {
                    let Some(value) = iter.next() else {
                        self.error_message = Some(
                            "--ignore-keys expects a comma-separated list (e.g. \"version,updated\")"
                                .into(),
                        );
                        return;
                    };
                    if value.starts_with("--") {
                        self.error_message = Some(format!(
                            "--ignore-keys expects a comma-separated list, got flag '{value}'"
                        ));
                        return;
                    }
                    ignore_csv = Some(value.to_string());
                }
                other if other.starts_with("--") => {
                    bad_arg = Some(other.to_string());
                    break;
                }
                other => positionals.push(other),
            }
        }
        if let Some(other) = bad_arg {
            self.error_message = Some(format!("unknown arg '{other}'"));
            return;
        }
        // Reject excess positionals rather than silently dropping them
        // (`:config-diff` does the same for its second positional).
        if positionals.len() > 2 {
            self.error_message = Some(
                "usage: :diff takes at most two env names (selected ↔ ENV, or ENV-A ENV-B)".into(),
            );
            return;
        }
        let ignore_keys: Vec<String> = parse_ignore_keys(ignore_csv.as_deref());
        match (positionals.first(), positionals.get(1)) {
            (None, _) => {
                self.error_message = Some(
                    "usage: :diff ENV  (selected ↔ ENV)  |  :diff ENV-A ENV-B  [--ignore-keys \"k1,k2\"]"
                        .into(),
                );
            }
            // Two-arg form: both envs named explicitly, so no implicit
            // selected-env side. Useful for picking env-A ↔ env-B from a
            // different scope than what's currently selected.
            (Some(a), Some(b)) => {
                if a == b {
                    self.error_message = Some("pick two different envs to compare".into());
                    return;
                }
                let Some(left) = self.environments.iter().find(|e| e.name == **a).cloned() else {
                    self.error_message = Some(format!("no env named '{a}' in current view"));
                    return;
                };
                let Some(right) = self.environments.iter().find(|e| e.name == **b).cloned() else {
                    self.error_message = Some(format!("no env named '{b}' in current view"));
                    return;
                };
                self.current_overlay = Some(Overlay::Diff(diff_envs(
                    &left,
                    &right,
                    self.view.redact,
                    &ignore_keys,
                )));
            }
            // Legacy single-arg form: selected (or detail-pane) env
            // compared against the named arg. Preserves the existing
            // behaviour every operator already knows.
            (Some(target), None) => {
                let left_opt = if let Some(d) = self.detail.as_ref() {
                    Some(d.env_snapshot.clone())
                } else {
                    self.selected_env().cloned()
                };
                let Some(left) = left_opt else {
                    self.error_message = Some(
                        "no env selected — press 1-9, click a row, or type ' to jump by name"
                            .into(),
                    );
                    return;
                };
                if left.name == **target {
                    self.error_message = Some("pick a different env to compare against".into());
                    return;
                }
                let right = self
                    .environments
                    .iter()
                    .find(|e| e.name == **target)
                    .cloned();
                match right {
                    None => {
                        self.error_message =
                            Some(format!("no env named '{target}' in current view"));
                    }
                    Some(right) => {
                        self.current_overlay = Some(Overlay::Diff(diff_envs(
                            &left,
                            &right,
                            self.view.redact,
                            &ignore_keys,
                        )));
                    }
                }
            }
        }
    }

    /// `:config-diff ENV` — compare the selected env's option-settings
    /// against `ENV`'s, showing every setting that differs. Answers
    /// "why does staging differ from prod?". Fetches both envs'
    /// configuration options in parallel and renders the diff.
    pub(crate) fn cmd_config_diff(&mut self, rest: &[&str]) {
        // Parse `:config-diff ENV [--ignore-keys "k1,k2,..."]`. Ignore-
        // keys is case-insensitive and matches the option `name` field
        // (e.g. "version_label", "EC2KeyName"). Operators can also use
        // `namespace:name` form for precise matches against a specific
        // namespace (e.g. "aws:autoscaling:asg:MinSize"). 0.19 addition.
        let mut env_name: Option<String> = None;
        let mut ignore_csv: Option<String> = None;
        let mut iter = rest.iter().copied();
        while let Some(arg) = iter.next() {
            match arg {
                "--ignore-keys" => {
                    let Some(value) = iter.next() else {
                        self.error_message = Some(
                            "--ignore-keys expects a comma-separated list (e.g. \"version_label,EC2KeyName\")"
                                .into(),
                        );
                        return;
                    };
                    // Guard against `:config-diff PROD --ignore-keys --json`
                    // consuming the next flag as the value (same pattern
                    // 0.17.0's --baseline parsing uses).
                    if value.starts_with("--") {
                        self.error_message = Some(format!(
                            "--ignore-keys expects a comma-separated list, got flag '{value}'"
                        ));
                        return;
                    }
                    ignore_csv = Some(value.to_string());
                }
                other if !other.starts_with("--") && env_name.is_none() => {
                    env_name = Some(other.to_string());
                }
                other => {
                    self.error_message = Some(format!("unknown arg '{other}'"));
                    return;
                }
            }
        }
        let Some(target) = env_name else {
            self.error_message = Some(
                "usage: :config-diff ENV [--ignore-keys \"k1,k2\"]  (compare the selected env's option-settings against ENV)"
                    .into(),
            );
            return;
        };
        let ignore_keys: Vec<String> = parse_ignore_keys(ignore_csv.as_deref());
        let left = if let Some(d) = self.detail.as_ref() {
            Some(d.env_snapshot.clone())
        } else {
            self.selected_env().cloned()
        };
        let Some(left) = left else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let Some(right) = self.environments.iter().find(|e| e.name == target).cloned() else {
            self.error_message = Some(format!("no env named '{target}' in the current view"));
            return;
        };
        if left.name == right.name {
            self.error_message = Some("pick a different env to compare against".into());
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let (la, ln) = (left.application.clone(), left.name.clone());
        let (ra, rn) = (right.application.clone(), right.name.clone());
        self.status_message = Some(format!("comparing config: {ln} ↔ {rn}…"));
        tokio::spawn(async move {
            let body = match tokio::try_join!(
                aws.fetch_env_configuration_options(&la, &ln),
                aws.fetch_env_configuration_options(&ra, &rn),
            ) {
                Ok((lopts, ropts)) => {
                    let diffs = diff_config_options(&lopts, &ropts);
                    let filtered = filter_config_diffs(diffs, &ignore_keys);
                    render_config_diff_overlay(&ln, &rn, &filtered)
                }
                Err(e) => format!(
                    "config-diff: {}\n\nesc / q to close",
                    flatten_err("fetch_env_configuration_options", e)
                ),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("config diff — {ln} ↔ {rn}"),
                body,
            });
        });
    }

    /// `:rds` — fetch the env's RDS dbinstance option settings and
    /// render them. Visibility-only first cut: attach (via
    /// `UpdateEnvironment(aws:rds:dbinstance.*)`) and detach (the
    /// decouple-via-snapshot workflow) are follow-ups — both need
    /// careful operator confirmation flows and the detach path is
    /// genuinely destructive.
    ///
    /// Empty result = no RDS attached. We surface that as an
    /// explicit message rather than "no config" so the operator
    /// isn't left wondering whether the fetch failed silently.
    pub(crate) fn cmd_rds(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        self.status_message = Some(format!("fetching RDS config for {env_name}…"));
        tokio::spawn(async move {
            let result = aws
                .fetch_env_rds_config(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("fetch_env_rds_config", e));
            let body = match result {
                Ok(rows) if rows.is_empty() => "No RDS instance attached to this env.\n\n\
                     EB-managed RDS is configured via `aws:rds:dbinstance.*`\n\
                     option settings. To attach a new one:\n\n  \
                     :set-option aws:rds:dbinstance DBEngine postgres\n  \
                     :set-option aws:rds:dbinstance DBInstanceClass db.t3.micro\n  \
                     :set-option aws:rds:dbinstance DBPassword <secret>\n\n\
                     (See the EB docs — there are 10+ required fields. A\n\
                     dedicated `:rds-attach` form is a planned follow-up.)\n\n\
                     esc / q to close"
                    .to_string(),
                Ok(rows) => {
                    let mut body = String::from("RDS dbinstance configuration:\n\n");
                    for (opt, value) in &rows {
                        // Redact the password field even when the
                        // operator hasn't toggled global redact mode —
                        // surfacing a DB password into an overlay is a
                        // worse default than hiding it.
                        let safe_value = if opt.eq_ignore_ascii_case("DBPassword") {
                            "(redacted)".to_string()
                        } else {
                            value.clone()
                        };
                        body.push_str(&format!("  {opt:<28}  {safe_value}\n"));
                    }
                    body.push_str(
                        "\nUse `:set-option aws:rds:dbinstance <KEY> <VALUE>` to change a setting.\n\
                         Note: most RDS option changes trigger instance modification (downtime risk).\n\
                         esc / q to close",
                    );
                    body
                }
                Err(e) => format!("rds: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("rds — {env_name}"),
                body,
            });
        });
    }

    /// `:listeners` — fetch the env's ALB listener config (per-port:
    /// protocol, attached cert ARN, SSL policy, default rule) and
    /// render it as a text overlay. Web-tier only — Worker envs
    /// don't have an ALB. Edit support (cert rotation, listener
    /// add/remove) is a follow-up; the generic
    /// `:set-option aws:elbv2:listener:<PORT> KEY VAL` already
    /// works for one-off updates.
    pub(crate) fn cmd_listeners(&mut self) {
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if env.tier.eq_ignore_ascii_case("Worker") {
            self.error_message = Some(format!(
                "env '{}' is Worker tier — no ALB to configure",
                env.name
            ));
            return;
        }
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let app_name = env.application.clone();
        let env_name = env.name.clone();
        self.status_message = Some(format!("fetching listeners for {env_name}…"));
        tokio::spawn(async move {
            let result = aws
                .fetch_env_listeners(&app_name, &env_name)
                .await
                .map_err(|e| flatten_err("fetch_env_listeners", e));
            let body = match result {
                Ok(rows) if rows.is_empty() => "No listener config found.\n\n\
                     The env may use a Classic ELB instead of an ALB, or no\n\
                     listener overrides have been set (EB uses account defaults).\n\
                     `:set-option aws:elbv2:listener:443 SSLCertificateArns ARN`\n\
                     to configure a listener from scratch.\n\nesc / q to close"
                    .to_string(),
                Ok(rows) => {
                    let mut body = String::from("Listener configuration:\n");
                    body.push_str("(one block per port; `default` = HTTP/80)\n\n");
                    let mut current_port: Option<String> = None;
                    for (port, opt, value) in &rows {
                        if current_port.as_deref() != Some(port.as_str()) {
                            if current_port.is_some() {
                                body.push('\n');
                            }
                            body.push_str(&format!("── aws:elbv2:listener:{port} ──\n"));
                            current_port = Some(port.clone());
                        }
                        body.push_str(&format!("  {opt:<32}  {value}\n"));
                    }
                    body.push_str(
                        "\n`:set-option aws:elbv2:listener:<PORT> <KEY> <VALUE>` to change a setting.\n\
                         esc / q to close",
                    );
                    body
                }
                Err(e) => format!("listeners: {e}\n\nesc / q to close"),
            };
            let _ = tx.send(AppMsg::TextOverlay {
                gen,
                title: format!("listeners — {env_name}"),
                body,
            });
        });
    }

    /// `:listener-edit PORT` — modal cert-rotation form for an ALB
    /// listener. Opens a single MultiSelect field whose options are the
    /// region's ISSUED ACM certificates (loaded async), pre-selected with
    /// the listener's current `SSLCertificateArns`. Submit writes the new
    /// cert set to `aws:elbv2:listener:<PORT>` via the option-settings
    /// path. `PORT` is `443` / a numeric port / `default` (HTTP/80).
    pub(crate) fn cmd_listener_edit(&mut self, rest: &[&str]) {
        use crate::form::{Form, FormField, FormSubmit};
        let Some(env) = self.selected_env().cloned() else {
            self.error_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if env.tier.eq_ignore_ascii_case("Worker") {
            self.error_message = Some(format!(
                "env '{}' is Worker tier — no ALB to configure",
                env.name
            ));
            return;
        }
        let Some(port) = rest.first().copied() else {
            self.error_message = Some(
                "usage: :listener-edit PORT  (e.g. :listener-edit 443; `default` = HTTP/80)".into(),
            );
            return;
        };
        let port = port.to_string();
        let ns = format!("aws:elbv2:listener:{port}");
        let placeholder = FormField::multi_select(
            "cert",
            "SSL certificate(s)",
            Vec::new(),
            Vec::new(),
            Some::<String>("space toggle · ↑↓ option cursor · loaded from ACM".into()),
        );
        let form = Form::loading(
            format!("listener {port} — {}", env.name),
            env.name.clone(),
            format!("listener {port} cert update"),
            vec![placeholder],
            FormSubmit::OptionSettings {
                mappings: vec![("cert".into(), ns, "SSLCertificateArns".into())],
            },
        );
        // Bypass open_form's DescribeConfigurationSettings pre-fill (it
        // wouldn't load ACM inventory) — stash the form and spawn the
        // cert-specific loader, mirroring the subnet / SG pickers.
        self.form = Some(form);
        self.mode = Mode::Form;
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let env_for_msg = env.name.clone();
        let app_name = env.application.clone();
        tokio::spawn(async move {
            let result = load_listener_certs(aws, &app_name, &env_for_msg, &port).await;
            let _ = tx.send(AppMsg::FormMultiSelectLoaded {
                gen,
                env_name: env_for_msg,
                field_key: "cert".to_string(),
                result,
            });
        });
    }
}
