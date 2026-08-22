//! Fetching the world and folding it back in.
//!
//! The `spawn_*` half launches AWS calls; the `apply_*` half merges
//! results into `App`. Every spawned task carries the `generation` it
//! was launched at and its handler drops the result if `App` has moved
//! on — that guard is what makes a mid-flight context switch safe.

use super::*;

impl App {
    pub(crate) fn manual_refresh(&mut self) {
        self.spawn_refresh();
        self.status_message = Some("refresh requested".into());
    }

    /// Spawn a Cost Explorer fetch in the background. Result lands
    /// via `AppMsg::CostsFetched`; on success the costs map updates
    /// AND the cache file is rewritten. Idempotent — multiple
    /// fetches in flight overwrite each other harmlessly (last
    /// write wins; the tag-grouped result is stable across calls).
    /// Spawn a background AWS call off the UI thread.
    ///
    /// `op` runs against a cloned `AwsClient`; on failure its `eyre::Report`
    /// is flattened to a user-facing string tagged with `op_name`. The
    /// `Result<T, String>` plus the generation captured at spawn time are
    /// handed to `into_msg`, whose `AppMsg` is sent back to the event loop.
    /// This is the boilerplate every simple single-call `spawn_*` helper
    /// shares; multi-call fan-outs (`spawn_worker_queue_check`,
    /// `spawn_app_latest_versions`) still build their tasks directly.
    pub(crate) fn spawn_aws<T, Fut, Op, Build>(
        &self,
        op_name: &'static str,
        op: Op,
        into_msg: Build,
    ) where
        T: Send + 'static,
        Fut: std::future::Future<Output = Result<T, color_eyre::eyre::Report>> + Send + 'static,
        Op: FnOnce(Arc<AwsClient>) -> Fut + Send + 'static,
        Build: FnOnce(u64, Result<T, String>) -> AppMsg + Send + 'static,
    {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = op(aws).await.map_err(|e| flatten_err(op_name, e));
            let _ = tx.send(into_msg(gen, result));
        });
    }

    /// `spawn_aws`, but against the region the row actually lives in.
    ///
    /// A resolution failure lands on the SAME `Err(String)` the
    /// operation itself would produce, so every existing handler
    /// already renders it — the alternative was ten new error paths.
    pub(crate) fn spawn_aws_in<T, Fut, Op, Build>(
        &self,
        client: RegionClient,
        op_name: &'static str,
        op: Op,
        into_msg: Build,
    ) where
        T: Send + 'static,
        Fut: std::future::Future<Output = Result<T, color_eyre::eyre::Report>> + Send + 'static,
        Op: FnOnce(Arc<AwsClient>) -> Fut + Send + 'static,
        Build: FnOnce(u64, Result<T, String>) -> AppMsg + Send + 'static,
    {
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        tokio::spawn(async move {
            let result = match client.resolve().await {
                Ok(aws) => op(aws).await.map_err(|e| flatten_err(op_name, e)),
                Err(e) => Err(flatten_err(op_name, e)),
            };
            let _ = tx.send(into_msg(gen, result));
        });
    }

    pub(crate) fn spawn_cost_fetch(&mut self) {
        let account = self.context.account_id.clone();
        let region = self.context.region.clone();
        self.spawn_aws(
            "fetch_env_costs",
            move |aws| async move { aws.fetch_env_costs().await },
            move |gen, result| AppMsg::CostsFetched {
                gen,
                account,
                region,
                result,
            },
        );
    }

    pub(crate) fn spawn_alarms_fetch(&mut self, env_name: String) {
        // The fetch's env name lives on the Overlay::Alarms variant so a late
        // result for a different env can be dropped at the handler. The body
        // is initially a placeholder until the result arrives.
        self.current_overlay = Some(Overlay::Alarms {
            env_name: env_name.clone(),
            body: format!("fetching alarms for {env_name}…"),
        });
        let name_for_msg = env_name.clone();
        let dims = self.cfg.alarm_dimensions.clone();
        self.spawn_aws(
            "list_alarms_for_env",
            move |aws| async move { aws.list_alarms_for_env(&env_name, &dims).await },
            move |gen, result| AppMsg::Alarms {
                gen,
                env_name: name_for_msg,
                result,
            },
        );
    }

    /// Recompute `tf_managed_envs` from the current `tf_state`.
    /// Called at startup (after `App::new`'s tfstate load), on
    /// `apply_rebuild` (context switch), and on the `:drift
    /// refresh` operator gesture. Cheap — set construction is
    /// O(n) over tf-managed env names; typically < 50.
    pub(crate) fn refresh_tf_managed_envs(&mut self) {
        self.tf_managed_envs = self
            .tf_state
            .as_ref()
            .map(|s| s.managed_names())
            .unwrap_or_default();
    }

    pub(crate) fn spawn_rebuild(&mut self) {
        self.load_state = LoadState::Loading;
        self.loading_since = Some(Instant::now());
        self.rebuild_epoch = self.rebuild_epoch.wrapping_add(1);
        let epoch = self.rebuild_epoch;
        let profile = self.override_profile.clone();
        let region = self.override_region.clone();
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = match AwsClient::with(profile, region).await {
                Ok(c) => Ok(Box::new(c)),
                Err(e) => Err(flatten_err("aws_client_with", e)),
            };
            let _ = tx.send(AppMsg::Rebuild { epoch, result });
        });
    }

    /// Background task variant of `spawn_rebuild` for the AssumeRole
    /// path. Calls `AwsClient::assume_role` with the operator's named
    /// account spec; same `AppMsg::Rebuild` arrival point so the rest
    /// of the swap (overlay tear-down, throttle reset, identity refresh)
    /// flows through the existing `apply_rebuild` handler.
    pub(crate) fn spawn_assume_role_switch(&mut self, account_name: String) {
        let Some(spec) = self.cfg.accounts.get(&account_name).cloned() else {
            self.error_message = Some(format!(
                "no `accounts.{account_name}` in config.toml — add `accounts.{account_name}.role_arn = …`"
            ));
            return;
        };
        self.load_state = LoadState::Loading;
        self.loading_since = Some(Instant::now());
        self.status_message = Some(format!("assuming role for account '{account_name}'…"));
        self.rebuild_epoch = self.rebuild_epoch.wrapping_add(1);
        let epoch = self.rebuild_epoch;
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = match AwsClient::assume_role(&account_name, &spec).await {
                Ok(c) => Ok(Box::new(c)),
                Err(e) => Err(flatten_err("aws_client_assume_role", e)),
            };
            let _ = tx.send(AppMsg::Rebuild { epoch, result });
        });
    }

    pub(crate) fn spawn_identity(&mut self) {
        self.spawn_aws(
            "verify_identity",
            move |aws| async move { aws.verify_identity().await },
            |gen, result| AppMsg::Identity { gen, result },
        );
    }

    pub(crate) fn spawn_update_check(&mut self) {
        // No outbound network in `--demo` mode — VHS captures shouldn't
        // pulse a "latest version available" toast partway through.
        if self.demo_mode {
            return;
        }
        let tx = self.msg_tx.clone();
        tokio::spawn(async move {
            let result = crate::update_check::check_async().await;
            let _ = tx.send(AppMsg::UpdateCheck(result));
        });
    }

    /// If the `loading…` indicator was visible during the current load (i.e.
    /// `loading_since` was set and crossed the display threshold), arm a
    /// linger window so the indicator stays on for at least
    /// [`LOADING_INDICATOR_LINGER`] after the load completes. Call this
    /// *before* clearing `loading_since` and flipping `load_state` back to
    /// Idle/Error in the AppMsg handler.
    fn arm_loading_linger(&mut self) {
        let now = Instant::now();
        if let Some(until) = compute_loading_linger_target(
            self.loading_since,
            LOADING_INDICATOR_THRESHOLD,
            LOADING_INDICATOR_LINGER,
            now,
        ) {
            self.loading_visible_until = Some(until);
        }
    }

    pub(crate) fn spawn_refresh(&mut self) {
        // `--demo` mode pins the fixture data in place — refresh would
        // call into the stub AwsClient, get empty results, and blank
        // the table. Skip entirely.
        if self.demo_mode {
            return;
        }
        if matches!(self.load_state, LoadState::Loading) {
            return;
        }
        self.load_state = LoadState::Loading;
        self.loading_since = Some(Instant::now());
        self.status_snapshot_at_refresh =
            Some((self.status_message.clone(), self.error_message.clone()));
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        if self.multi_regions.is_empty() {
            let aws = self.aws.clone();
            tokio::spawn(async move {
                let result = aws
                    .list_environments()
                    .await
                    .map_err(|e| flatten_err("list_environments", e));
                let _ = tx.send(AppMsg::Refresh {
                    gen,
                    result,
                    partial_errors: Vec::new(),
                });
            });
        } else {
            let regions = self.multi_regions.clone();
            let profile = self
                .override_profile
                .clone()
                .or_else(|| self.context.profile.clone());
            tokio::spawn(async move {
                use futures::future::join_all;
                let tasks = regions.into_iter().map(|r| {
                    let p = profile.clone();
                    async move { crate::aws::list_environments_in_region(p, r).await }
                });
                let results = join_all(tasks).await;
                let mut envs = Vec::new();
                let mut errs = Vec::new();
                for r in results {
                    match r {
                        Ok(v) => envs.extend(v),
                        // `{e:#}`, not `{e}`: the region is attached
                        // as eyre context by `list_environments_in_region`,
                        // and the bare Display shows only the outermost
                        // message — so the notice named no region at all.
                        Err(e) => errs.push(format!("{e:#}")),
                    }
                }
                // Every region failing is a hard error; some failing
                // while others returned rows is a PARTIAL result, and
                // has to be said out loud rather than silently dropping
                // those regions' environments from the table.
                let (result, partial_errors) = if envs.is_empty() && !errs.is_empty() {
                    (Err(errs.join("; ")), Vec::new())
                } else {
                    (Ok(envs), errs)
                };
                let _ = tx.send(AppMsg::Refresh {
                    gen,
                    result,
                    partial_errors,
                });
            });
        }
        if self.event_panel.visible {
            self.spawn_events();
        }
        self.spawn_applications();
        // Solution stacks change rarely (AWS releases platform versions
        // roughly monthly); fetch once per context and reuse. Cleared on a
        // context switch so a new account/region rebuilds it.
        if self.latest_stacks.is_empty() {
            self.spawn_solution_stacks();
        }
    }

    /// Fetch the region's solution-stack catalogue so the envs table can
    /// flag platforms with a newer version available. Best-effort: a failed
    /// fetch just leaves `latest_stacks` empty and no env is flagged.
    pub(crate) fn spawn_solution_stacks(&self) {
        self.spawn_aws(
            "list_solution_stacks",
            move |aws| async move { aws.list_solution_stacks().await },
            |gen, result| AppMsg::SolutionStacks { gen, result },
        );
    }

    fn spawn_applications(&self) {
        self.spawn_aws(
            "list_applications",
            move |aws| async move { aws.list_applications().await },
            |gen, result| AppMsg::Applications { gen, result },
        );
    }

    /// Fan out `DescribeApplicationVersions` per app to compute the LATEST
    /// column in the apps view. The AWS application-level `date_updated`
    /// only changes on metadata edits (description / templates / lifecycle),
    /// not on new version pushes — so operators expect this column to track
    /// version `date_created` instead. Errors on individual apps drop that
    /// row from the result rather than failing the batch.
    pub(crate) fn spawn_app_latest_versions(&self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let names: Vec<String> = self.applications.iter().map(|a| a.name.clone()).collect();
        if names.is_empty() {
            return;
        }
        tokio::spawn(async move {
            use futures::future::join_all;
            let futs = names.into_iter().map(|name| {
                let aws = aws.clone();
                async move {
                    let res = aws.list_application_versions(&name).await;
                    let head = res.ok().and_then(|mut v| v.drain(..).next());
                    (
                        name,
                        head.as_ref().map(|h| h.label.clone()),
                        head.and_then(|h| h.created),
                    )
                }
            });
            let results: Vec<(
                String,
                Option<String>,
                Option<chrono::DateTime<chrono::Utc>>,
            )> = join_all(futs).await;
            let _ = tx.send(AppMsg::AppLatestVersions { gen, results });
        });
    }

    /// Per-Worker-env DLQ depth fan-out. Fires once per refresh after
    /// `list_environments` lands. Skips Web envs (no DLQ). Each env's
    /// fetch is independent — a failure on one drops that entry from
    /// the result rather than failing the batch.
    fn spawn_worker_queue_check(&self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let workers: Vec<(String, String)> = self
            .environments
            .iter()
            .filter(|e| e.tier.eq_ignore_ascii_case("Worker"))
            .map(|e| (e.name.clone(), e.application.clone()))
            .collect();
        if workers.is_empty() {
            return;
        }
        tokio::spawn(async move {
            use futures::future::join_all;
            let futs = workers.into_iter().map(|(env, app)| {
                let aws = aws.clone();
                async move {
                    // Errors stay errors — a failed fetch must not be
                    // indistinguishable from "no DLQ" (the pre-0.27
                    // shape silently blinded red-alerting on
                    // AccessDenied/throttle).
                    let outcome = match aws.describe_worker_queues(&app, &env).await {
                        Ok(q) => Ok(q.dlq_stats.map(|s| s.visible)),
                        Err(e) => Err(flatten_err("describe_worker_queues", e)),
                    };
                    (env, outcome)
                }
            });
            let results: Vec<(String, Result<Option<i64>, String>)> =
                join_all(futs).await.into_iter().collect();
            let _ = tx.send(AppMsg::WorkerQueueCheck { gen, results });
        });
    }

    /// Fan `DescribeEnvironmentHealth` across every env on each refresh
    /// tick to populate the `INST` column. Skips Terminated / Terminating
    /// envs (EB returns AccessDenied-ish errors for them) and silently
    /// drops failures so a single env's API blip doesn't poison the
    /// whole batch. Same shape as `spawn_worker_queue_check`.
    pub(crate) fn spawn_env_instance_counts(&self) {
        let aws = self.aws.clone();
        let tx = self.msg_tx.clone();
        let gen = self.generation;
        let targets: Vec<String> = self
            .environments
            .iter()
            .filter(|e| {
                // EB rejects DescribeEnvironmentHealth for envs in
                // terminal lifecycle states — no instances to count.
                !matches!(
                    e.status.as_str(),
                    "Terminated" | "Terminating" | "Launching"
                )
            })
            .map(|e| e.name.clone())
            .collect();
        if targets.is_empty() {
            return;
        }
        tokio::spawn(async move {
            use futures::future::join_all;
            let futs = targets.into_iter().map(|env| {
                let aws = aws.clone();
                async move {
                    aws.fetch_env_instance_counts(&env)
                        .await
                        .ok()
                        .map(|counts| (env, counts))
                }
            });
            let results: Vec<(String, crate::aws::EnvInstanceCounts)> =
                join_all(futs).await.into_iter().flatten().collect();
            let _ = tx.send(AppMsg::EnvInstanceCountsCheck { gen, results });
        });
    }

    pub(crate) fn spawn_events(&mut self) {
        // Scope the events panel to the currently-selected env so it tells
        // the user about *this* env, not the entire account. Falls back to
        // the global event stream when no env is selected. The previously-
        // fetched env name is recorded so we can detect selection changes
        // and refetch without firing a request on every j/k.
        let selected = self.selected_env().map(|e| e.name.clone());
        self.event_panel.for_env = selected.clone();
        self.spawn_aws(
            "list_events",
            move |aws| async move {
                match selected {
                    Some(name) => aws.list_events_for_env(&name, 50).await,
                    None => aws.list_events(50).await,
                }
            },
            |gen, result| AppMsg::Events { gen, result },
        );
    }

    /// Refetch the events panel if the cursor has moved to a different env
    /// since the last fetch. Called from the main loop just before draw, so
    /// any keystroke / mouse click that changed selection picks up the new
    /// env's events on the next frame.
    pub(crate) fn refresh_events_if_selection_changed(&mut self) {
        if !self.event_panel.visible {
            return;
        }
        let selected = self.selected_env().map(|e| e.name.clone());
        if selected != self.event_panel.for_env {
            self.spawn_events();
        }
    }

    /// Apply a Detail-tab AppMsg payload. Handles the boilerplate every
    /// `Detail*` variant shares: drop when no Detail view is open, drop
    /// when the user switched to a different env mid-fetch. The stale-
    /// generation drop is handled upstream by `handle_msg`'s central guard.
    ///
    /// The closure runs against `&mut DetailState` + the raw
    /// `Result<T, String>` so the caller picks its own success / error
    /// behaviour — most clear `detail.error` on the Ok branch, but tags /
    /// env-vars use `tracing::warn!` instead since their failures
    /// shouldn't tint the whole tab red.
    pub(crate) fn apply_detail_msg<T, F>(
        &mut self,
        env_name: &str,
        result: Result<T, String>,
        apply: F,
    ) where
        F: FnOnce(&mut DetailState, Result<T, String>),
    {
        let Some(detail) = self.detail.as_mut() else {
            return;
        };
        if detail.env_name != env_name {
            return;
        }
        apply(detail, result);
    }

    pub(crate) fn apply_rebuild(&mut self, epoch: u64, result: Result<Box<AwsClient>, String>) {
        // Stale arrival: a NEWER switch was spawned after this one —
        // applying it would settle the app on an older choice.
        if epoch != self.rebuild_epoch {
            return;
        }
        match result {
            Ok(client) => {
                self.generation = self.generation.wrapping_add(1);
                // The operator just switched context, which is also
                // when they may have re-run `aws sso login` or edited
                // `~/.aws/config`. Drop the cached profile+region
                // clients so the next multi-region fan-out rebuilds
                // against whatever is on disk now.
                crate::aws::clear_client_cache();
                self.context = client.context.clone();
                self.aws = Arc::new(*client);
                self.maybe_apply_profile_theme();
                self.environments.clear();
                // Covers every view-cache input this block clears —
                // `environments` here and `latest_stacks` below.
                self.view.invalidate();
                self.event_panel.events.clear();
                self.event_panel.scroll = 0;
                self.history.clear();
                // Solution-stack catalogue is region-specific; drop it so the
                // new context's `spawn_refresh` rebuilds it.
                self.latest_stacks.clear();
                // Overlays show data from the previous context (describe dump,
                // alarms list, …); close them so the user doesn't act on stale info.
                self.current_overlay = None;
                // Tear down any long-running CW Logs poll that's mid-flight;
                // it would otherwise keep hitting the previous account's CW.
                // Also bump session id so any in-flight LogTailOpened from
                // the aborted task is dropped on arrival.
                tail::reap_tail_task(&mut self.log_tail_task, &mut self.log_tail_session);
                // Same teardown for the fleet event tail — it polls
                // DescribeEvents against the previous context.
                tail::reap_tail_task(&mut self.event_tail_task, &mut self.event_tail_session);
                // Reset throttle back-off across context switches — the new
                // account/region has its own rate limits.
                self.throttle_until = None;
                self.consecutive_throttles = 0;
                // Diff state is keyed by env name. Switching accounts/regions may
                // surface envs with overlapping names but unrelated history;
                // clearing here prevents spurious "newly red" / status-delta noise
                // on the first refresh in the new context.
                self.prev_health.clear();
                self.prev_status.clear();
                self.prev_alerts = 0;
                self.newly_red.clear();
                self.newly_added.clear();
                self.health_delta.clear();
                self.status_delta.clear();
                // Drop any auto-rollback watchdogs armed against the
                // previous context. The deadline tokio tasks survive
                // (no JoinHandle for cancellation), but their late
                // `AutoRollbackCheck` messages get dropped by the
                // generation-guard at msg.rs's entry. Clearing here
                // also prevents a same-name env in the new context
                // from being seen as "still armed" by apply_refresh.
                self.armed_watchdogs.clear();
                // Same reasoning applies to wait-for-green trackers:
                // env-name keyed, context-scoped — drop on rebuild.
                self.watching_deploys.clear();
                // Pre-deploy snapshots are env-name keyed; clearing on
                // context switch avoids :rollback in the new context
                // picking up a label that doesn't exist there.
                self.deploy_snapshots.clear();
                // Undo entries reference env names from the previous
                // context — meaningless after a switch. Drop them
                // alongside the other env-keyed state.
                self.undo_history.clear();
                // Promotion lineage is env-name keyed (and would point
                // at envs that don't exist in the new context). Clear
                // alongside undo_history.
                self.promotion_history.clear();
                // 0.21: lint-input caches are env-name keyed and
                // context-scoped (instance IDs from health, tag keys
                // are account/region-scoped). Same context-switch
                // semantics as worker_dlq_depths above.
                self.env_tag_cache.clear();
                self.env_health_cache.clear();
                // Pending actions reference env names from the
                // previous context. Their spawned tasks (if any) are
                // dropped at the generation guard in msg.rs, so the
                // matching `complete_pending` never runs and the
                // header `⏳ N` chip / `:pending` overlay would show
                // the previous-context op forever.
                self.pending_actions.clear();
                // Any pending arm-then-dispatch (`:rebuild` modal etc.)
                // is also context-scoped — drop it so a stray Enter
                // doesn't fire against the new context. Also clear the
                // associated status_message — `queue_action_dispatch`
                // set "X dispatches in 5s — press U to undo", and
                // without this the bar would lie about a dispatch that
                // we just cancelled.
                self.pending_dispatch = None;
                self.status_message = None;
                // Detail tab snapshot (env name + instances + tab
                // state) is context-scoped: instance IDs are EC2-IDs
                // in the OLD account. Without this, `:ssm-run` would
                // resolve env+instances from the stale snapshot and
                // dispatch against the NEW AwsClient — cross-account
                // silent dispatch if the new account has a same-named
                // env. Same reasoning for the cached log-tail and
                // pending shell target. Drop the mode back to Normal
                // so the UI doesn't render a Detail view with no
                // data underneath it.
                self.detail = None;
                self.pending_shell_target = None;
                // An open `:a` action modal / confirm modal references
                // an env name from the previous context. Drop it
                // alongside detail so the operator doesn't confirm
                // against stale data after a context switch.
                self.action_flow = None;
                // Picker overlays (profile / region / log-group / ssh
                // instance) all carry context-scoped state too.
                self.picker = None;
                // An open form (`:capacity`, `:subnets`, env-var
                // editor…) was built against the OLD context's env; a
                // `^S` after the switch would deny_write-check the new
                // context's config with the old env name and dispatch
                // against the new client — cross-account silent write
                // if the new account has a same-named env. Same class
                // as the detail/:ssm-run clear above.
                self.form = None;
                // Batch selections are env-name keyed; a stale set
                // would let `:batch-rebuild` fan out old names against
                // the new client.
                self.multi_selected.clear();
                self.apps_selected.clear();
                // DLQ state carries queue URLs from the old account
                // (and `:help`'s topic inference reads it).
                self.dlq = None;
                // Per-env caches from the old context — stale numbers
                // would render as current until the first refresh.
                self.worker_dlq_depths.clear();
                self.worker_dlq_stale.clear();
                self.env_instance_counts.clear();
                self.applications.clear();
                self.costs.clear();
                // The verdict belonged to the previous account.
                self.costs_complete = true;
                self.costs_fetched_at = None;
                // Help's stash (pre_mode / pre_overlay) points at
                // modes and overlays this switch just tore down —
                // closing help would restore e.g. Mode::Detail with
                // detail == None (a ghost state).
                self.help.pre_mode = None;
                self.help.pre_overlay = None;
                if self.mode == Mode::Detail
                    || self.mode == Mode::Dlq
                    || self.mode == Mode::Action
                    || self.mode == Mode::Picker
                    || self.mode == Mode::Form
                    || self.mode == Mode::Help
                {
                    self.mode = Mode::Normal;
                }
                // Re-read tfstate from cwd. The new context might
                // be a different repo (operator cd'd between
                // sessions of `ebman` left running, or switched
                // account / region within the same shell);
                // re-discovery ensures the tf-managed badge reflects
                // the current project.
                self.tf_state = crate::terraform::load_from_cwd();
                self.refresh_tf_managed_envs();
                self.rebuild_view();
                self.table_state.select(None);
                self.status_message = Some(format!(
                    "context: {} / {}",
                    self.context.profile.as_deref().unwrap_or("default"),
                    self.context.region
                ));
                self.error_message = None;
                self.arm_loading_linger();
                self.load_state = LoadState::Idle;
                self.persist_state();
                self.spawn_identity();
                self.spawn_refresh();
            }
            Err(msg) => {
                tracing::error!(error = %msg, "rebuild failed");
                self.arm_loading_linger();
                self.load_state = LoadState::Error;
                self.loading_since = None;
                self.error_message = Some(self.format_aws_error("context switch", &msg));
            }
        }
    }

    pub(crate) fn apply_refresh(
        &mut self,
        result: Result<Vec<Environment>, String>,
        partial_errors: Vec<String>,
    ) {
        match result {
            Ok(envs) => {
                // Track newly-Red transitions for the anomaly highlight.
                let is_red =
                    |h: &str| h.eq_ignore_ascii_case("Red") || h.eq_ignore_ascii_case("Severe");
                self.newly_red.clear();
                // Compute newly-added envs *before* swapping prev_health
                // below — once we overwrite it, "previously unseen" is no
                // longer derivable. Skip the first refresh (prev_health is
                // empty then) so every env doesn't get flagged on startup.
                self.newly_added.clear();
                if !self.prev_health.is_empty() {
                    for e in &envs {
                        if !self.prev_health.contains_key(&e.name) {
                            self.newly_added.insert(e.name.clone());
                        }
                    }
                }
                for e in &envs {
                    let prev_red = self
                        .prev_health
                        .get(&e.name)
                        .map(|h| is_red(h))
                        .unwrap_or(false);
                    if is_red(&e.health) && !prev_red {
                        self.newly_red.insert(e.name.clone());
                        // Surface the transition via tracing + the audit log
                        // so operators can wire their own notifier (Slack,
                        // pager, etc.) off the audit stream. The previous
                        // built-in `webhook_url` POST was trimmed — too rigid
                        // for real ops workflows.
                        tracing::warn!(
                            env = %e.name,
                            application = %e.application,
                            health = %e.health,
                            region = %self.context.region,
                            "env transitioned into Red",
                        );
                        crate::audit::append_raw(
                            self.context.account_id.as_deref(),
                            self.context.profile.as_deref(),
                            &self.context.region,
                            &format!(
                                "stage=event kind=red_transition env={} application={} health={}",
                                e.name, e.application, e.health
                            ),
                        );
                    }
                }
                // Compute health + status deltas before swapping prev maps.
                self.health_delta = bucket_delta(&self.prev_health, &envs, |e| e.health.clone());
                self.status_delta = bucket_delta(&self.prev_status, &envs, |e| e.status.clone());

                self.prev_health = envs
                    .iter()
                    .map(|e| (e.name.clone(), e.health.clone()))
                    .collect();
                self.prev_status = envs
                    .iter()
                    .map(|e| (e.name.clone(), e.status.clone()))
                    .collect();

                let new_alerts = compute_red_alerts(&envs, &self.worker_dlq_depths);
                if self.notify_bell && new_alerts > self.prev_alerts {
                    // BEL — write to stderr and flush so the terminal rings
                    // immediately even though we're in the alt screen.
                    use std::io::Write;
                    let mut err = std::io::stderr().lock();
                    let _ = err.write_all(b"\x07");
                    let _ = err.flush();
                }
                self.prev_alerts = new_alerts;
                self.alerts = new_alerts;

                self.environments = envs;
                self.view.invalidate();
                self.resort_envs();

                // Watchdog decision pass — single source of truth for
                // auto-rollback outcomes. Every armed watchdog gets
                // evaluated against the *freshly-applied* env list
                // (line above), eliminating the stale-cache race the
                // earlier "deadline handler dispatches inline" design
                // had: the deadline `tokio::spawn` now just sends an
                // `AutoRollbackCheck` message whose handler kicks a
                // manual refresh, so by the time we reach here the
                // health field is current.
                //
                // Three outcomes per armed env:
                //   1. Env is Green/Ok → drain, pin status.
                //   2. Env still non-Green AND deadline passed →
                //      dispatch the rollback redeploy.
                //   3. Else → keep armed; check again next refresh.
                let armed: Vec<(String, chrono::DateTime<chrono::Utc>)> = self
                    .armed_watchdogs
                    .iter()
                    .map(|(env, w)| (env.clone(), w.deadline_at))
                    .collect();
                let now = chrono::Utc::now();
                for (env_name, deadline_at) in armed {
                    let Some((status, health)) = self
                        .environments
                        .iter()
                        .find(|e| e.name == env_name)
                        .map(|e| (e.status.clone(), e.health.clone()))
                    else {
                        // Env left the fleet (terminated mid-watch) —
                        // disarm instead of dispatching a doomed
                        // redeploy at the deadline.
                        self.armed_watchdogs.remove(&env_name);
                        self.pin_status(format!(
                            "auto-rollback for {env_name}: env no longer in the fleet — watchdog disarmed"
                        ));
                        continue;
                    };
                    let healthy = deploy_settled_green(&status, &health);
                    if healthy {
                        self.armed_watchdogs.remove(&env_name);
                        // pin_status survives the same-tick auto-clear
                        // at the bottom of apply_refresh — without it
                        // the disarm message gets wiped before the
                        // operator ever sees it.
                        self.pin_status(format!(
                            "auto-rollback for {env_name}: env reached Green, watchdog disarmed"
                        ));
                    } else if now >= deadline_at {
                        // Deadline reached, env still bad. Dispatch
                        // the redeploy using the just-refreshed health
                        // so the audit line is accurate.
                        self.dispatch_auto_rollback(env_name, health);
                    }
                    // else: still armed, next refresh re-evaluates.
                }

                // Wait-for-green watcher decision pass. Same shape as
                // armed_watchdogs, but the resolution is purely
                // observational — no follow-on dispatch. Three outcomes:
                //   1. Env is Green/Ok → drain, pin success.
                //   2. Deadline passed and env still non-Green → drain,
                //      pin timeout error (operator decides next move).
                //   3. Else → keep watching, re-check next refresh.
                let watching: Vec<(String, chrono::DateTime<chrono::Utc>, String, u64)> = self
                    .watching_deploys
                    .iter()
                    .map(|(env, w)| {
                        let secs = (w.deadline_at - w.armed_at).num_seconds().max(0) as u64;
                        (env.clone(), w.deadline_at, w.target_label.clone(), secs)
                    })
                    .collect();
                for (env_name, deadline_at, target_label, total_secs) in watching {
                    let Some((status, health)) = self
                        .environments
                        .iter()
                        .find(|e| e.name == env_name)
                        .map(|e| (e.status.clone(), e.health.clone()))
                    else {
                        // Env left the fleet — stop watching with an
                        // honest note rather than timing out later.
                        self.watching_deploys.remove(&env_name);
                        self.pin_error(format!(
                            "deploy watch for {env_name}: env no longer in the fleet"
                        ));
                        continue;
                    };
                    let healthy = deploy_settled_green(&status, &health);
                    if healthy {
                        self.watching_deploys.remove(&env_name);
                        let label_hint = if target_label.is_empty() {
                            String::new()
                        } else {
                            format!(" ({target_label})")
                        };
                        self.pin_status(format!("✓ deploy reached Green: {env_name}{label_hint}"));
                    } else if now >= deadline_at {
                        self.watching_deploys.remove(&env_name);
                        let label_hint = if target_label.is_empty() {
                            String::new()
                        } else {
                            format!(" ({target_label})")
                        };
                        self.pin_error(format!(
                            "deploy did not reach Green within {total_secs}s: {env_name}{label_hint} — status={status} health={health}"
                        ));
                    }
                }

                let live: HashSet<String> =
                    self.environments.iter().map(|e| e.name.clone()).collect();
                for e in &self.environments {
                    let buf = self.history.entry(e.name.clone()).or_default();
                    buf.push_back(e.health.clone());
                    while buf.len() > HISTORY_CAP {
                        buf.pop_front();
                    }
                }
                self.history.retain(|k, _| live.contains(k));

                self.arm_loading_linger();
                self.load_state = LoadState::Idle;
                self.loading_since = None;
                self.last_refresh = Some(chrono::Utc::now());
                // A successful refresh resets the throttle back-off so the
                // next throttle (if any) starts again from the base interval.
                //
                // Unless the fan-out was only PARTIALLY successful and
                // the failures were throttles. Those now arrive here in
                // the `Ok` arm — some regions returned rows — and
                // resetting on them meant ebman never backed off from
                // the regions rate-limiting it, re-hammering them every
                // tick and deepening the throttle.
                let throttled_regions = partial_errors
                    .iter()
                    .filter(|e| is_throttling_error(e))
                    .count();
                if throttled_regions > 0 {
                    let backoff =
                        throttle_backoff(self.refresh_interval, self.consecutive_throttles);
                    self.consecutive_throttles = self.consecutive_throttles.saturating_add(1);
                    self.throttle_until = Some(Instant::now() + backoff);
                } else {
                    self.consecutive_throttles = 0;
                    self.throttle_until = None;
                }
                // Clear status/error only if the user hasn't replaced them
                // during the refresh round-trip. Otherwise their action message
                // (sort change, alias set, …) would get clobbered here.
                if let Some((prev_status, prev_error)) = self.status_snapshot_at_refresh.take() {
                    // Don't auto-clear user-pinned messages — those are
                    // results the operator just asked for and would lose
                    // every 15s otherwise.
                    if !self.status_message_pinned && self.status_message == prev_status {
                        self.status_message = None;
                    }
                    if self.error_message == prev_error {
                        self.error_message = None;
                    }
                } else if !self.status_message_pinned {
                    self.status_message = None;
                    self.error_message = None;
                }
                // AFTER the auto-clear above, not before it: a
                // successful refresh wipes `error_message`, so a
                // partial-failure notice set at the top of this
                // function is erased by the very refresh it describes.
                //
                // But only into a slot the auto-clear actually emptied.
                // Writing unconditionally overwrote a message the
                // operator set DURING the round trip — a failed
                // `:deploy`, say — which the guard above had just
                // deliberately preserved, and did it again every tick
                // with no way to dismiss it.
                if !partial_errors.is_empty() && self.error_message.is_none() {
                    self.error_message = Some(format!(
                        "some regions failed and their environments are NOT shown: {}",
                        partial_errors.join("; ")
                    ));
                }
                // Pin lasts one refresh cycle. After that the message
                // survives in the slot but the next ephemeral write (e.g.
                // a spawn helper's "fetching…") gets normal auto-clear
                // semantics again.
                self.status_message_pinned = false;
                self.restore_or_clamp_selection();
                // Fan out DLQ depth checks for Worker-tier envs. Result
                // lands as `AppMsg::WorkerQueueCheck` and updates the
                // alert count + the in-row `⚠ DLQ:N` chip on the next
                // draw.
                self.spawn_worker_queue_check();
                // Same fan-out shape for the INST column: per-env
                // `DescribeEnvironmentHealth` summarised down to
                // `(healthy, total)`. Cache rebuilt from results in
                // `handle_env_instance_counts`.
                self.spawn_env_instance_counts();
            }
            Err(msg) => {
                tracing::error!(error = %msg, "refresh failed");
                self.arm_loading_linger();
                self.load_state = LoadState::Error;
                self.loading_since = None;
                self.status_snapshot_at_refresh = None;
                if is_throttling_error(&msg) {
                    let backoff =
                        throttle_backoff(self.refresh_interval, self.consecutive_throttles);
                    self.consecutive_throttles = self.consecutive_throttles.saturating_add(1);
                    self.throttle_until = Some(Instant::now() + backoff);
                    self.error_message = Some(format!(
                        "rate-limited by AWS — backing off {}s (^R to force)",
                        backoff.as_secs().max(1)
                    ));
                } else {
                    self.error_message = Some(self.format_aws_error("refresh", &msg));
                }
            }
        }
    }
}
