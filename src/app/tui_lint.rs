//! The lint runs the TUI starts: `:lint`, `:explain`, and the pre-deploy
//! confirm lint.
//!
//! All three read the same App caches — the region's platform list
//! (EBL008) and the worker-queue poll (EBL011) — and each used to read
//! them its own way. A fix to one then missed the others: the cached-
//! input gaps landed in `:lint` and `:explain` and not in the confirm
//! modal, the surface where a missed check matters most. The snapshot is
//! taken in one place and the run finished in one place, and within
//! `src/app` only this module calls the lint engine (pinned by
//! `only_tui_lint_calls_the_lint_engine_in_app`).
//!
//! The cache readings live here, not in `lint::inputs`: nothing but the
//! TUI has a platform cache or a queue poll, and the shared module stays
//! neutral between surfaces.

use super::App;
use crate::aws::Environment;
use crate::lint::inputs::{EnvLintInputs, Platforms, ProbeOutcome};

/// EBL011's input, from the TUI's worker-queue poll.
pub(crate) enum WorkerDlq {
    Depth(i64),
    /// The last check found no DLQ configured: nothing to check.
    NoDlq,
    /// No usable answer, with why — the first poll has not landed, or
    /// the last one failed. A failed poll keeps the previous depth for
    /// the alert pill, but lint does not judge on it: the rule would
    /// fire, or pass, on a number nobody has seen since.
    Unknown(String),
}

impl WorkerDlq {
    pub(crate) fn depth(&self) -> Option<i64> {
        match self {
            Self::Depth(d) => Some(*d),
            _ => None,
        }
    }
}

/// The worker-queue poll's answer for one env, as lint may use it.
/// Stale wins: after a failed check the depth kept for the alert pill
/// — or an earlier "no DLQ" — is not something to judge a rule on.
pub(crate) fn worker_dlq(depth: Option<i64>, absent: bool, stale: bool) -> WorkerDlq {
    match (depth, absent, stale) {
        (_, _, true) => WorkerDlq::Unknown("the last worker-queue check failed".into()),
        (Some(d), _, false) => WorkerDlq::Depth(d),
        (None, true, false) => WorkerDlq::NoDlq,
        (None, false, false) => {
            WorkerDlq::Unknown("no worker-queue check has completed yet".into())
        }
    }
}

/// EBL011's input gap: a worker with no usable DLQ answer read the same
/// as a clean one — including every worker in the seconds before the
/// first poll lands, and one whose checks have been failing since an
/// earlier "no DLQ".
pub(crate) fn dlq_depth_gap(
    env: &Environment,
    disabled: &[String],
    dlq: &WorkerDlq,
) -> Option<String> {
    let WorkerDlq::Unknown(why) = dlq else {
        return None;
    };
    if !env.tier.eq_ignore_ascii_case("Worker") || disabled.iter().any(|d| d == "EBL011") {
        return None;
    }
    ProbeOutcome::Unknown(why.clone()).coverage_warning("EBL011", &env.name)
}

/// The TUI's platform cache, for one env. It is fetched for the HOME
/// region only; under a multi-region fan-out an env elsewhere would be
/// judged against another region's catalogue, so it is reported as not
/// available rather than claimed loaded. `failed` is the last fetch's
/// error: an empty list with none has not loaded yet (a region always
/// has platforms, so empty never means "none").
pub(crate) fn platforms_from_cache(
    latest: &std::collections::HashMap<String, String>,
    failed: Option<&str>,
    home_region: &str,
    env_region: &str,
) -> Platforms {
    if env_region != home_region {
        return Platforms::Unavailable(format!(
            "the platform-version list is loaded for {home_region} only, and this env is \
             in {env_region}"
        ));
    }
    match failed {
        Some(e) if latest.is_empty() => Platforms::Unavailable(e.to_string()),
        _ if latest.is_empty() => {
            Platforms::Unavailable("the platform-version list has not loaded yet".into())
        }
        _ => Platforms::Loaded(latest.clone()),
    }
}

/// Everything a lint run takes from App, captured before the spawn.
pub(crate) struct LintSnapshot {
    pub(crate) env: Environment,
    user_disables: Vec<String>,
    pub(crate) required_tags: Vec<String>,
    pub(crate) platforms: Platforms,
    pub(crate) dlq: WorkerDlq,
}

impl App {
    pub(crate) fn lint_snapshot(&self, env: &Environment) -> LintSnapshot {
        LintSnapshot {
            env: env.clone(),
            user_disables: self.cfg.lint_disable.clone(),
            required_tags: self.cfg.required_tags.clone(),
            platforms: platforms_from_cache(
                &self.latest_stacks,
                self.latest_stacks_error.as_deref(),
                &self.context.region,
                // The row's own region, not a lookup by name: under a
                // fan-out a same-named env in another region would
                // answer for it.
                &self.region_for(env),
            ),
            dlq: worker_dlq(
                self.worker_dlq_depths.get(&env.name).copied(),
                self.worker_dlq_absent.contains(&env.name),
                self.worker_dlq_stale.contains(&env.name),
            ),
        }
    }
}

impl LintSnapshot {
    /// User-level disables plus the project's, read fresh from cwd so a
    /// mid-session edit takes effect. Reads the filesystem: call it from
    /// the spawned task, not the UI thread.
    pub(crate) fn disabled(&self) -> Vec<String> {
        let mut disabled = self.user_disables.clone();
        disabled.extend(crate::project::load_lint_disables_from_cwd());
        disabled
    }

    /// Everything after the fetch, shared by every TUI lint run: fill in
    /// what came from App rather than a fetch — the DLQ depth, and its
    /// gap when there is none to use — then run the rules.
    pub(crate) fn finish(&self, mut inputs: EnvLintInputs, disabled: &[String]) -> LintRun {
        inputs.dlq_depth = self.dlq.depth();
        inputs
            .coverage_warnings
            .extend(dlq_depth_gap(&self.env, disabled, &self.dlq));
        let rules = crate::lint::default_rules(disabled);
        let issues =
            crate::lint::inputs::run_rules_for_env(&rules, &self.env, &inputs, &self.required_tags);
        LintRun {
            issues,
            coverage_warnings: inputs.coverage_warnings,
        }
    }

    /// The confirm modal's run: it fetches its own way (the lint-input
    /// cache first, no probes), then assembles and finishes exactly as
    /// the shared path does.
    pub(crate) fn finish_fetched(
        &self,
        options: Vec<(String, String, String)>,
        tags: Option<Result<Vec<String>, String>>,
        health: Result<i64, String>,
        disabled: &[String],
    ) -> LintRun {
        let inputs = crate::lint::inputs::assemble(
            &self.env,
            options,
            tags,
            health,
            &self.platforms,
            disabled,
            &self.required_tags,
        );
        self.finish(inputs, disabled)
    }
}

/// A finished run: what fired, and what could not be checked.
pub(crate) struct LintRun {
    pub(crate) issues: Vec<crate::lint::Issue>,
    pub(crate) coverage_warnings: Vec<String>,
}

/// `:lint` and `:explain`: the shared assembly, finished from the
/// snapshot. `Err` is the option-settings fetch, without which lint
/// cannot run at all.
pub(crate) async fn run_tui_lint(
    aws: &crate::aws::AwsClient,
    snap: &LintSnapshot,
) -> Result<LintRun, String> {
    let disabled = snap.disabled();
    let inputs = crate::lint::inputs::fetch_env_lint_inputs(
        aws,
        &snap.env,
        &snap.platforms,
        false,
        &disabled,
        &snap.required_tags,
    )
    .await?;
    Ok(snap.finish(inputs, &disabled))
}
