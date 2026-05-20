//! Navigation / view-state commands — `:region`, `:profile`,
//! `:account`, `:sort`, `:group`, `:redact`. These manipulate App's
//! navigation state (active region, profile, sort key) and either
//! trigger a `spawn_refresh` / `spawn_rebuild` (region / profile /
//! account) or just rebuild the in-memory view (sort / group).
//!
//! Sixth slice of the `execute_command` split. Same parent-module
//! visibility as the other `cmd_*` sub-modules.

use super::{parse_toggle, App, PickerKind, SortKey};

impl App {
    /// `:region <name> | all | off | r <name>`.
    ///   - `all`  → multi-region fan-out across `extra_regions ∪ current`.
    ///   - `off` / `single` → return to single-region mode.
    ///   - `<name>` → switch to that region (rebuilds the AWS client).
    pub(crate) fn cmd_region(&mut self, rest: &[&str]) {
        match rest.first().copied() {
            Some("all") => {
                let mut regions = self.extra_regions.clone();
                if !regions.iter().any(|r| r == &self.context.region) {
                    regions.push(self.context.region.clone());
                }
                if regions.is_empty() {
                    self.error_message =
                        Some("no regions configured — set extra_regions in config.toml".into());
                    return;
                }
                regions.sort();
                regions.dedup();
                self.multi_regions = regions.clone();
                self.status_message = Some(format!(
                    "multi-region: fanning across {} regions ({})",
                    regions.len(),
                    regions.join(", ")
                ));
                self.spawn_refresh();
            }
            Some("off") | Some("single") => {
                self.multi_regions.clear();
                self.status_message = Some("multi-region off".into());
                self.spawn_refresh();
            }
            Some(r) => self.apply_picker_choice(PickerKind::Region, r.to_string()),
            None => self.error_message = Some("usage: :region <name> | all | off".into()),
        }
    }

    /// `:account NAME`. Two paths:
    ///   1. `accounts.NAME.role_arn = …` configured in config.toml →
    ///      AssumeRole flow via `spawn_assume_role_switch`.
    ///   2. Otherwise legacy aliasing to `:profile NAME` (operators
    ///      who use one profile per account via the `role_arn` chain
    ///      in `~/.aws/config`).
    pub(crate) fn cmd_account(&mut self, rest: &[&str]) {
        match rest.first() {
            Some(name) => {
                let name = (*name).to_string();
                if self.accounts.contains_key(&name) {
                    self.spawn_assume_role_switch(name);
                } else {
                    self.apply_picker_choice(PickerKind::Profile, name);
                }
            }
            None => {
                self.error_message = Some(
                    "usage: :account <name>  (resolves accounts.NAME from config.toml, falls back to :profile)"
                        .into(),
                );
            }
        }
    }

    pub(crate) fn cmd_profile(&mut self, rest: &[&str]) {
        match rest.first() {
            Some(p) => self.apply_picker_choice(PickerKind::Profile, (*p).to_string()),
            None => self.error_message = Some("usage: :profile <name>".into()),
        }
    }

    /// `:sort <key> [asc|desc]` — keys: name app status health version age.
    pub(crate) fn cmd_sort(&mut self, rest: &[&str]) {
        let Some(key) = rest.first() else {
            self.error_message = Some(
                "usage: :sort <key> [asc|desc]  — keys: name app status health version age".into(),
            );
            return;
        };
        match SortKey::parse(key) {
            Some(k) => {
                self.sort_key = k;
                self.sort_desc = matches!(rest.get(1), Some(&"desc"));
                self.resort_envs();
                self.status_message = Some(format!(
                    "sort: {} ({})",
                    self.sort_key.label(),
                    if self.sort_desc { "desc" } else { "asc" }
                ));
            }
            None => self.error_message = Some(format!("unknown sort key: {key}")),
        }
    }

    pub(crate) fn cmd_group(&mut self, rest: &[&str]) {
        self.grouped = parse_toggle(rest.first().copied(), self.grouped);
        self.rebuild_view();
        self.status_message = Some(if self.grouped {
            "grouped by application".into()
        } else {
            "ungrouped".into()
        });
    }

    pub(crate) fn cmd_redact(&mut self, rest: &[&str]) {
        self.redact = parse_toggle(rest.first().copied(), self.redact);
        self.status_message = Some(if self.redact {
            "redact mode ON".into()
        } else {
            "redact mode off".into()
        });
    }
}
