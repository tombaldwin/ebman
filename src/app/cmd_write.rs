//! Write-side command handlers — bulk operations over the multi-select
//! set (`:batch-rebuild`, `:batch-restart`, `:batch-deploy`,
//! `:batch-tag`, `:batch-untag`, `:batch-set-option`). Each shares the
//! same loop shape: validate read-only + non-empty selection + the
//! per-command arguments, then iterate `multi_selected` invoking the
//! corresponding `spawn_batch_*` helper which lives on App and routes
//! through the existing audit + pending + toast plumbing.
//!
//! Second slice of the `execute_command` split — follows the same
//! parent-module-visibility pattern as `app::cmd_overlay`.

use super::{parse_tag_args, Action, App, PendingDispatchKind};

impl App {
    /// `:batch-rebuild` / `:batch-restart`. Routes through the 5s
    /// cancel-window queue — operator can `U`-undo the whole fan-out
    /// before any AWS calls go out. Same one-at-a-time rule as
    /// single-env confirms.
    pub(crate) fn cmd_batch_action(&mut self, action: Action) {
        if self.multi_selected.is_empty() {
            self.error_message = Some("no envs selected — press space to mark envs first".into());
            return;
        }
        let env_names: Vec<String> = self.multi_selected.iter().cloned().collect();
        // Per-env safety gate: refuse if ANY selected env is pinned
        // read-only (global / demo / freeze / safety.envs / accounts).
        if self.deny_write_batch(&env_names, "batch action") {
            return;
        }
        let n = env_names.len();
        let label = format!("Batch {}", action.label().to_lowercase());
        let target = format!("{n} env(s)");
        self.queue_batch_dispatch(
            label,
            target,
            PendingDispatchKind::BatchAction { action, env_names },
        );
        self.multi_selected.clear();
    }

    /// `:batch-deploy LABEL`. Refuses when the selection spans more
    /// than one application — the label can't possibly resolve for all
    /// of them and we'd queue N failing requests for no gain. Once
    /// validated, queues through the cancel-window the same as
    /// `cmd_batch_action`.
    pub(crate) fn cmd_batch_deploy(&mut self, rest: &[&str]) {
        if self.multi_selected.is_empty() {
            self.error_message = Some("no envs selected — press space to mark envs first".into());
            return;
        }
        let Some(version_label) = rest.first().map(|s| s.to_string()) else {
            self.error_message = Some("usage: :batch-deploy LABEL".into());
            return;
        };
        let env_names: Vec<String> = self.multi_selected.iter().cloned().collect();
        // Per-env safety gate before the app-span check — a pinned env
        // should refuse regardless of whether the selection is valid.
        if self.deny_write_batch(&env_names, "batch-deploy") {
            return;
        }
        let apps: std::collections::BTreeSet<String> = env_names
            .iter()
            .filter_map(|n| {
                self.environments
                    .iter()
                    .find(|e| &e.name == n)
                    .map(|e| e.application.clone())
            })
            .collect();
        if apps.len() > 1 {
            self.error_message = Some(format!(
                "selected envs span {} applications ({}); :batch-deploy needs a single application",
                apps.len(),
                apps.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
            return;
        }
        let n = env_names.len();
        let target = format!("{n} env(s)");
        self.queue_batch_dispatch(
            format!("Batch deploy {version_label}"),
            target,
            PendingDispatchKind::BatchDeploy {
                env_names,
                version_label,
            },
        );
        self.multi_selected.clear();
    }

    /// `:batch-tag KEY VALUE` (`is_tag = true`) or `:batch-untag KEY`
    /// (`is_tag = false`). Envs whose ARN we don't have cached yet get
    /// skipped + reported so the operator can re-refresh and retry —
    /// rather than silently dispatching incomplete writes.
    pub(crate) fn cmd_batch_tag_or_untag(&mut self, is_tag: bool, rest: &[&str]) {
        if self.multi_selected.is_empty() {
            self.error_message = Some("no envs selected — press space to mark envs first".into());
            return;
        }
        {
            // Per-env safety gate before arg parsing + ARN resolution.
            let names: Vec<String> = self.multi_selected.iter().cloned().collect();
            if self.deny_write_batch(&names, "batch tag/untag") {
                return;
            }
        }
        let (key, value) = if is_tag {
            let Some((k, v)) = parse_tag_args(rest) else {
                self.error_message = Some("usage: :batch-tag KEY VALUE".into());
                return;
            };
            (k, Some(v))
        } else {
            let Some(k) = rest.first().map(|s| s.to_string()) else {
                self.error_message = Some("usage: :batch-untag KEY".into());
                return;
            };
            (k, None)
        };
        let names: Vec<String> = self.multi_selected.iter().cloned().collect();
        let mut envs_with_arns: Vec<(String, String)> = Vec::new();
        let mut missing_arn: Vec<String> = Vec::new();
        for name in &names {
            let arn = self
                .environments
                .iter()
                .find(|e| &e.name == name)
                .and_then(|e| e.arn.clone());
            match arn {
                Some(arn) => envs_with_arns.push((name.clone(), arn)),
                None => missing_arn.push(name.clone()),
            }
        }
        if envs_with_arns.is_empty() {
            self.error_message = Some(format!(
                "no env has a cached ARN yet ({} skipped) — refresh and retry",
                missing_arn.len()
            ));
            return;
        }
        let op = if is_tag { "tag" } else { "untag" };
        let count = envs_with_arns.len();
        let target = if missing_arn.is_empty() {
            format!("{count} env(s)")
        } else {
            format!("{count} env(s) (skipping {} — no ARN)", missing_arn.len())
        };
        self.queue_batch_dispatch(
            format!("Batch {op} {key}"),
            target,
            PendingDispatchKind::BatchTag {
                envs_with_arns,
                key,
                value,
            },
        );
        self.multi_selected.clear();
    }

    /// `:batch-set-option NAMESPACE NAME VALUE`. Value tokens after the
    /// first two args get joined with single spaces — matches `:tag`'s
    /// convention. Operators needing literal multi-space values set
    /// per-env via `:set-option` instead.
    pub(crate) fn cmd_batch_set_option(&mut self, rest: &[&str]) {
        if self.multi_selected.is_empty() {
            self.error_message = Some("no envs selected — press space to mark envs first".into());
            return;
        }
        if rest.len() < 3 {
            self.error_message = Some("usage: :batch-set-option NAMESPACE NAME VALUE".into());
            return;
        }
        let namespace = rest[0].to_string();
        let option_name = rest[1].to_string();
        let value = rest[2..].join(" ");
        let env_names: Vec<String> = self.multi_selected.iter().cloned().collect();
        // Per-env safety gate: refuse if any selected env is pinned.
        if self.deny_write_batch(&env_names, "batch-set-option") {
            return;
        }
        let n = env_names.len();
        self.queue_batch_dispatch(
            format!("Batch set-option {namespace}.{option_name}={value}"),
            format!("{n} env(s)"),
            PendingDispatchKind::BatchSetOption {
                env_names,
                namespace,
                option_name,
                value,
            },
        );
        self.multi_selected.clear();
    }
}
