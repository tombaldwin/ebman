//! The lint runs the TUI starts: `:lint`, `:explain`, and the pre-deploy
//! confirm lint.
//!
//! All three read the same App caches — the region's platform list
//! (EBL008) and the worker-queue poll (EBL011) — and each used to read
//! them its own way. A fix to one then missed the others: the cached-
//! input gaps landed in `:lint` and `:explain` and not in the confirm
//! modal, the surface where a missed check matters most. The snapshot is
//! taken in one place, and the inputs are completed from it in one place.

use super::App;
use crate::aws::Environment;
use crate::lint::inputs::{EnvLintInputs, Platforms, WorkerDlq};

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
            platforms: Platforms::from_cache(
                &self.latest_stacks,
                self.latest_stacks_error.as_deref(),
            ),
            dlq: worker_dlq(
                self.worker_dlq_depths.get(&env.name).copied(),
                self.worker_dlq_absent.contains(&env.name),
                self.worker_dlq_stale.contains(&env.name),
            ),
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
            .extend(crate::lint::inputs::dlq_depth_gap(
                &self.env, disabled, &self.dlq,
            ));
        let rules = crate::lint::default_rules(disabled);
        let issues =
            crate::lint::inputs::run_rules_for_env(&rules, &self.env, &inputs, &self.required_tags);
        LintRun {
            issues,
            coverage_warnings: inputs.coverage_warnings,
        }
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
