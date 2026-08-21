//! The view layer over `App::environments` / `App::applications`:
//! filtering, sorting, grouping, pinning and cursor movement.
//!
//! [`App::rebuild_view`] lives here — the one thing that can install a
//! fresh view cache. Changing `environments` (or any other input
//! [`super::ViewState`] doesn't own) means `view.invalidate()` then
//! `rebuild_view()`; changing `filter` or `grouped` through `ViewState`
//! marks the cache stale on its own.

use super::*;

impl App {
    pub(crate) fn toggle_pin_selected(&mut self) {
        let name_opt = if let Some(d) = self.detail.as_ref() {
            Some(d.env_name.clone())
        } else {
            self.selected_env().map(|e| e.name.clone())
        };
        let Some(name) = name_opt else {
            self.status_message =
                Some("no env selected — press 1-9, click a row, or type ' to jump by name".into());
            return;
        };
        if self.pinned.remove(&name) {
            self.status_message = Some(format!("unpinned {name}"));
        } else {
            self.pinned.insert(name.clone());
            self.status_message = Some(format!("pinned {name}"));
        }
        self.resort_envs();
        self.persist_state();
    }

    /// Apps-scope counterpart to `toggle_pin_selected`. Pins / unpins
    /// the application under the apps-table cursor. Pinned apps sort
    /// to the top of the Apps table regardless of the sort key (the
    /// `applications` Vec gets re-sorted on every refresh; see
    /// `resort_applications`).
    pub(crate) fn toggle_pin_selected_app(&mut self) {
        let Some(idx) = self.app_table_state.selected() else {
            self.status_message = Some("no app selected".into());
            return;
        };
        let Some(name) = self.applications.get(idx).map(|a| a.name.clone()) else {
            return;
        };
        if self.pinned_apps.remove(&name) {
            self.status_message = Some(format!("unpinned app {name}"));
        } else {
            self.pinned_apps.insert(name.clone());
            self.status_message = Some(format!("pinned app {name}"));
        }
        self.resort_applications();
        self.persist_state();
    }

    /// Sort `self.applications` so pinned apps float to the top.
    /// Within each pinned / unpinned bucket, alphabetical by name to
    /// keep ordering stable.
    pub(crate) fn resort_applications(&mut self) {
        let pinned = self.pinned_apps.clone();
        self.applications.sort_by(|a, b| {
            let a_pin = pinned.contains(&a.name);
            let b_pin = pinned.contains(&b.name);
            if a_pin != b_pin {
                return if a_pin {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            a.name.cmp(&b.name)
        });
    }

    /// Cycle through saved-view chips above the env table.
    /// `delta = +1` → next chip; `-1` → previous; both wrap.
    /// "Active" is derived from comparing the current filter to
    /// each view's encoded `filter=` portion — matches the chip
    /// bar's own active-test, so cycling lands on the chip
    /// immediately to the right/left of whichever is currently
    /// applied. If no chip is active (operator typed a freeform
    /// filter or none at all), starts at index 0 / -1 depending
    /// on direction.
    ///
    /// Replaced the earlier `cycle_named_filter` (0.11 and prior)
    /// when saved-views unified into a single store in 0.12.
    /// Loading a view applies the full encoded snapshot via
    /// `apply_view`, so cycling can change sort / group / scope
    /// alongside the filter — the BACKLOG-promised "tab"
    /// behavior. Filter-only views (the legacy migration case)
    /// only change the filter, leaving sort/group/scope alone.
    pub(crate) fn cycle_saved_view(&mut self, delta: i32) {
        if self.saved_views.is_empty() {
            return;
        }
        // BTreeMap iteration is sorted by key, so the cycle order
        // matches the chip-bar render order. Keep them in sync.
        let names: Vec<String> = self.saved_views.keys().cloned().collect();
        let cur_idx = if self.view.filter().is_empty() {
            None
        } else {
            names.iter().position(|n| {
                self.saved_views
                    .get(n)
                    .map(|encoded| view_filter_value(encoded) == self.view.filter().text())
                    .unwrap_or(false)
            })
        };
        let next = match cur_idx {
            Some(i) => (i as i32 + delta).rem_euclid(names.len() as i32) as usize,
            None if delta >= 0 => 0,
            None => names.len() - 1,
        };
        let chosen = names[next].clone();
        if let Some(snap) = self.saved_views.get(&chosen).cloned() {
            apply_view(self, &snap);
            self.status_message = Some(format!("view: {chosen}"));
        }
    }

    pub(crate) fn open_profile_picker(&mut self) {
        let items = profiles::load_profiles();
        let current = self.context.profile.as_deref();
        self.picker = Some(Picker::new(PickerKind::Profile, items, current));
        self.mode = Mode::Picker;
    }

    pub(crate) fn open_region_picker(&mut self) {
        let mut items: Vec<String> = profiles::REGIONS.iter().map(|s| (*s).to_string()).collect();
        for r in &self.extra_regions {
            if !items.iter().any(|i| i == r) {
                items.push(r.clone());
            }
        }
        let current = Some(self.context.region.as_str());
        self.picker = Some(Picker::new(PickerKind::Region, items, current));
        self.mode = Mode::Picker;
    }

    /// Change the sort and re-apply it in one step.
    ///
    /// The single entry point for `sort_key` / `sort_desc`: `ViewState`
    /// keeps them private so they can't be set without the re-sort that
    /// makes them true of `environments`.
    pub(crate) fn set_sort(&mut self, key: SortKey, desc: bool) {
        self.view.set_sort(key, desc);
        self.resort_envs();
    }

    pub(crate) fn resort_envs(&mut self) {
        // Reordering `environments` renumbers every index the view cache
        // holds, so the caller must rebuild after this.
        self.view.invalidate();
        let key = self.view.sort_key();
        let desc = self.view.sort_desc();
        let pinned = self.pinned.clone();
        self.environments.sort_by(|a, b| {
            // Pinned envs always sort to the top regardless of key/direction.
            let a_pin = pinned.contains(&a.name);
            let b_pin = pinned.contains(&b.name);
            if a_pin != b_pin {
                return if a_pin {
                    std::cmp::Ordering::Less
                } else {
                    std::cmp::Ordering::Greater
                };
            }
            let ord = match key {
                SortKey::App => a
                    .application
                    .to_lowercase()
                    .cmp(&b.application.to_lowercase())
                    .then(a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortKey::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                SortKey::Status => a
                    .status
                    .to_lowercase()
                    .cmp(&b.status.to_lowercase())
                    .then(a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortKey::Health => health_rank(&a.health)
                    .cmp(&health_rank(&b.health))
                    .then(a.name.to_lowercase().cmp(&b.name.to_lowercase())),
                SortKey::Age => a.updated.cmp(&b.updated),
                SortKey::Version => a
                    .version_label
                    .to_lowercase()
                    .cmp(&b.version_label.to_lowercase()),
            };
            if desc {
                ord.reverse()
            } else {
                ord
            }
        });
        self.rebuild_view();
    }

    pub(crate) fn apply_picker_choice(&mut self, kind: PickerKind, value: String) {
        match kind {
            PickerKind::Profile => {
                tracing::info!(
                    target: "ebman::state",
                    new_profile = %value,
                    cleared_override_region = ?self.override_region,
                    "apply_picker_choice(Profile) clears override_region so SDK re-resolves from new profile config"
                );
                self.override_profile = Some(value.clone());
                self.override_region = None;
                self.status_message = Some(format!("switching to profile {value}…"));
                self.spawn_rebuild();
            }
            PickerKind::Region => {
                tracing::info!(
                    target: "ebman::state",
                    new_region = %value,
                    prior_override = ?self.override_region,
                    "apply_picker_choice(Region) sets override_region"
                );
                self.override_region = Some(value.clone());
                self.status_message = Some(format!("switching to region {value}…"));
                self.spawn_rebuild();
            }
            PickerKind::LogGroup => {
                // Swap the streaming overlay's tailed group. Read the env
                // from the currently-open LogTail overlay; `spawn_logs_tail`
                // aborts the existing poller and opens a fresh one against
                // the chosen group, replacing `current_overlay` via the
                // resulting `AppMsg::LogTailOpened`.
                let env = match self.current_overlay.as_ref() {
                    Some(Overlay::LogTail { env_name, .. }) => env_name.clone(),
                    _ => return,
                };
                self.spawn_logs_tail(env, Some(value));
            }
            PickerKind::SshInstance => {
                // Same flow as pressing `s` on Detail/Instances — the
                // main loop tick consumes `pending_shell_target` and
                // handles the TUI suspend/resume + alt-screen dance.
                crate::audit::append_action_dispatched(
                    self.context.account_id.as_deref(),
                    self.context.profile.as_deref(),
                    &self.context.region,
                    "SsmSession",
                    value.as_str(),
                    &[("via", "cmd_ssh_picker")],
                );
                self.pending_shell_target = Some(value.clone());
                self.status_message = Some(format!("opening SSM session to {value}…"));
            }
        }
    }

    /// Set the active scope. Triggers the lazy `spawn_app_latest_versions`
    /// fetch when transitioning to `Apps`, so the LATEST column populates
    /// on entry rather than waiting for the next periodic refresh tick.
    /// Idempotent — re-entering the same scope is a no-op.
    pub(crate) fn set_scope(&mut self, new: Scope) {
        let changed = self.scope != new;
        self.scope = new;
        if changed && new == Scope::Apps {
            self.spawn_app_latest_versions();
        }
    }

    pub(crate) fn move_scope_selection(&mut self, delta: i32) {
        match self.scope {
            Scope::Envs => self.move_selection(delta),
            Scope::Apps => {
                let n = self.applications.len();
                if n == 0 {
                    self.app_table_state.select(None);
                    return;
                }
                let cur = self.app_table_state.selected().unwrap_or(0) as i32;
                let next = (cur + delta).rem_euclid(n as i32) as usize;
                self.app_table_state.select(Some(next));
            }
        }
    }

    pub(crate) fn scope_select_first(&mut self) {
        match self.scope {
            Scope::Envs => self.select_first(),
            Scope::Apps => {
                if !self.applications.is_empty() {
                    self.app_table_state.select(Some(0));
                }
            }
        }
    }

    pub(crate) fn scope_select_last(&mut self) {
        match self.scope {
            Scope::Envs => self.select_last(),
            Scope::Apps => {
                if !self.applications.is_empty() {
                    self.app_table_state
                        .select(Some(self.applications.len() - 1));
                }
            }
        }
    }

    pub(crate) fn drill_into_app(&mut self) {
        let Some(idx) = self.app_table_state.selected() else {
            return;
        };
        let Some(name) = self.applications.get(idx).map(|a| a.name.clone()) else {
            return;
        };
        self.view.set_filter(name.clone());
        self.set_scope(Scope::Envs);
        self.rebuild_view();
        self.status_message = Some(format!("filtered envs to application '{name}'"));
    }

    fn select_first(&mut self) {
        let rows = self.display_rows();
        if let Some(pos) = rows.iter().position(|r| matches!(r, DisplayRow::Env(_))) {
            self.table_state.select(Some(pos));
        }
    }

    fn select_last(&mut self) {
        let rows = self.display_rows();
        if let Some(pos) = rows.iter().rposition(|r| matches!(r, DisplayRow::Env(_))) {
            self.table_state.select(Some(pos));
        }
    }

    pub(crate) fn move_selection(&mut self, delta: i32) {
        let rows = self.display_rows();
        if rows.is_empty() {
            self.table_state.select(None);
            return;
        }
        // Build a list of indexes that are selectable (Env rows only).
        let selectable: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter_map(|(i, r)| matches!(r, DisplayRow::Env(_)).then_some(i))
            .collect();
        if selectable.is_empty() {
            self.table_state.select(None);
            return;
        }
        let current = self.table_state.selected().unwrap_or(selectable[0]);
        let pos_in_selectable = selectable.iter().position(|i| *i == current).unwrap_or(0) as i32;
        let next = (pos_in_selectable + delta).rem_euclid(selectable.len() as i32) as usize;
        self.table_state.select(Some(selectable[next]));
    }

    pub fn display_rows(&self) -> &[DisplayRow] {
        self.view.display()
    }

    pub fn filtered_indexes(&self) -> &[usize] {
        self.view.filtered()
    }

    /// Recompute the cached filtered/display slices. Call after any change to
    /// filter, sort, grouping, or the env list.
    /// Recompute everything `ui` draws from `environments`.
    ///
    /// The only caller of `ViewState::store`, and so the only thing that
    /// can clear the stale flag. Call it after changing any input: the
    /// filter, the grouping, `environments` itself, `aliases`,
    /// `latest_stacks`, or the theme palette.
    pub fn rebuild_view(&mut self) {
        // Filtered indexes.
        let mut filtered: Vec<usize> = Vec::new();
        if self.view.filter().is_empty() {
            filtered.extend(0..self.environments.len());
        } else {
            let needle = self.view.filter().text().to_lowercase();
            for (i, e) in self.environments.iter().enumerate() {
                let alias_hit = self
                    .aliases
                    .get(&e.name)
                    .map(|a| a.to_lowercase().contains(&needle))
                    .unwrap_or(false);
                if e.name.to_lowercase().contains(&needle)
                    || alias_hit
                    || e.application.to_lowercase().contains(&needle)
                    || e.health.to_lowercase().contains(&needle)
                    || e.status.to_lowercase().contains(&needle)
                {
                    filtered.push(i);
                }
            }
        }

        // Display rows (with optional group separators).
        let mut display: Vec<DisplayRow> = Vec::new();
        let mut prev_app: Option<&str> = None;
        for i in &filtered {
            let e = &self.environments[*i];
            if self.view.grouped() && prev_app.is_some() && prev_app != Some(e.application.as_str())
            {
                display.push(DisplayRow::Separator);
            }
            display.push(DisplayRow::Env(*i));
            prev_app = Some(e.application.as_str());
        }

        // Per-application palette colour. Assigned by order of first
        // appearance in the filtered view; cached here so the render path
        // can do an O(1) lookup instead of building this map per frame.
        let app_colors = assign_app_colors(
            filtered
                .iter()
                .map(|i| self.environments[*i].application.as_str()),
            &self.theme.app_palette,
        );

        // Stale-platform lookup: parse each env's solution stack against the
        // available-versions catalogue once here, so the render path looks
        // up `env_name → newer version` instead of re-parsing per row per
        // frame. Empty while `latest_stacks` hasn't loaded yet.
        let mut stale_platforms: HashMap<String, String> = HashMap::new();
        if !self.latest_stacks.is_empty() {
            for e in &self.environments {
                if let Some(newer) =
                    crate::aws::newer_stack_version(&e.solution_stack, &self.latest_stacks)
                {
                    stale_platforms.insert(e.name.clone(), newer);
                }
            }
        }

        self.view
            .store(filtered, display, app_colors, stale_platforms);
    }

    pub(crate) fn restore_or_clamp_selection(&mut self) {
        if self.view.display().is_empty() {
            self.table_state.select(None);
            return;
        }
        let first_env_idx = self
            .view
            .display()
            .iter()
            .position(|r| matches!(r, DisplayRow::Env(_)))
            .unwrap_or(0);
        let pending = self.pending_select.take();
        if let Some(name) = pending {
            let pos = self.view.display().iter().position(|r| match r {
                DisplayRow::Env(i) => self.environments[*i].name == name,
                DisplayRow::Separator => false,
            });
            if let Some(p) = pos {
                self.table_state.select(Some(p));
                return;
            }
        }
        let valid = self
            .table_state
            .selected()
            .is_some_and(|s| matches!(self.view.display().get(s), Some(DisplayRow::Env(_))));
        if !valid {
            self.table_state.select(Some(first_env_idx));
        }
    }
}
