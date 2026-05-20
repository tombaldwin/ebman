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

use super::{parse_tag_args, Action, App};

impl App {
    /// `:batch-rebuild` / `:batch-restart`. Caller passes the resolved
    /// `Action` so the union arm in `execute_command` stays compact.
    pub(crate) fn cmd_batch_action(&mut self, action: Action) {
        if self.read_only {
            self.error_message = Some("read-only mode — batch actions disabled".into());
            return;
        }
        if self.multi_selected.is_empty() {
            self.error_message = Some("no envs selected — press space to mark envs first".into());
            return;
        }
        let names: Vec<String> = self.multi_selected.iter().cloned().collect();
        let n = names.len();
        for name in names {
            self.spawn_batch_action(action, name);
        }
        self.status_message = Some(format!(
            "dispatched {} to {n} env(s) — watch the events panel for outcomes",
            action.label()
        ));
        self.multi_selected.clear();
    }

    /// `:batch-deploy LABEL`. Refuses when the selection spans more
    /// than one application — the label can't possibly resolve for all
    /// of them and we'd queue N failing requests for no gain.
    pub(crate) fn cmd_batch_deploy(&mut self, rest: &[&str]) {
        if self.read_only {
            self.error_message = Some("read-only mode — batch-deploy disabled".into());
            return;
        }
        if self.multi_selected.is_empty() {
            self.error_message = Some("no envs selected — press space to mark envs first".into());
            return;
        }
        let Some(label) = rest.first().map(|s| s.to_string()) else {
            self.error_message = Some("usage: :batch-deploy LABEL".into());
            return;
        };
        let names: Vec<String> = self.multi_selected.iter().cloned().collect();
        let apps: std::collections::BTreeSet<String> = names
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
        let n = names.len();
        for name in names {
            self.spawn_batch_deploy(name, label.clone());
        }
        self.status_message = Some(format!(
            "dispatched Deploy({label}) to {n} env(s) — outcomes via toasts",
        ));
        self.multi_selected.clear();
    }

    /// `:batch-tag KEY VALUE` (`is_tag = true`) or `:batch-untag KEY`
    /// (`is_tag = false`). Envs whose ARN we don't have cached yet get
    /// skipped + reported so the operator can re-refresh and retry —
    /// rather than silently dispatching incomplete writes.
    pub(crate) fn cmd_batch_tag_or_untag(&mut self, is_tag: bool, rest: &[&str]) {
        if self.read_only {
            self.error_message = Some("read-only mode — batch tag/untag disabled".into());
            return;
        }
        if self.multi_selected.is_empty() {
            self.error_message = Some("no envs selected — press space to mark envs first".into());
            return;
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
        let mut dispatched = 0usize;
        let mut missing_arn: Vec<String> = Vec::new();
        for name in &names {
            let arn = self
                .environments
                .iter()
                .find(|e| &e.name == name)
                .and_then(|e| e.arn.clone());
            match arn {
                Some(arn) => {
                    self.spawn_batch_tag(name.clone(), arn, key.clone(), value.clone());
                    dispatched += 1;
                }
                None => missing_arn.push(name.clone()),
            }
        }
        let op = if is_tag { "tag" } else { "untag" };
        let mut msg = format!("dispatched {op} {key} to {dispatched} env(s)");
        if !missing_arn.is_empty() {
            msg.push_str(&format!(
                " — skipped {} (no ARN): {}",
                missing_arn.len(),
                missing_arn.join(", ")
            ));
        }
        self.status_message = Some(msg);
        self.multi_selected.clear();
    }

    /// `:batch-set-option NAMESPACE NAME VALUE`. Value tokens after the
    /// first two args get joined with single spaces — matches `:tag`'s
    /// convention. Operators needing literal multi-space values set
    /// per-env via `:set-option` instead.
    pub(crate) fn cmd_batch_set_option(&mut self, rest: &[&str]) {
        if self.read_only {
            self.error_message = Some("read-only mode — batch-set-option disabled".into());
            return;
        }
        if self.multi_selected.is_empty() {
            self.error_message = Some("no envs selected — press space to mark envs first".into());
            return;
        }
        if rest.len() < 3 {
            self.error_message = Some("usage: :batch-set-option NAMESPACE NAME VALUE".into());
            return;
        }
        let ns = rest[0].to_string();
        let name = rest[1].to_string();
        let value = rest[2..].join(" ");
        let names: Vec<String> = self.multi_selected.iter().cloned().collect();
        let n = names.len();
        for env_name in names {
            self.spawn_batch_set_option(env_name, ns.clone(), name.clone(), value.clone());
        }
        self.status_message = Some(format!(
            "dispatched set-option {ns}.{name}={value} to {n} env(s)"
        ));
        self.multi_selected.clear();
    }
}
